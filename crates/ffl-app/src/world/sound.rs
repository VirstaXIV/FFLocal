//! Sound: zone BGM with day/night crossfades, placed ambient loops, footsteps, voice lines and
//! emote sounds.
//!
//! Engines hand over decoded PCM (`ffl_core::SoundData`, fetched lazily by key through
//! `Engine::load_sound`); this file turns it into [`PcmClip`] assets and Bevy audio entities
//! and mixes them by [`SoundCategory`] from the hub's `AudioSettings`. Nothing here names an
//! engine or a game format: the clock, the music slots, the surfaces and the cues all come
//! through `ffl_core`.
//!
//! Verification is log driven (`FFL_TRACE_AUDIO=1` prints every play/stop/fade; `bgm` and
//! `sound loaded` lines always print) plus the overlay lines in `AudioTrace`.

use std::collections::{HashMap, HashSet};
use std::num::NonZero;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bevy::audio::{AddAudioSource, AudioPlayer, AudioSink, AudioSinkPlayback, ChannelCount, Decodable, PlaybackSettings, Sample, SampleRate, Source, Volume};
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};
use ffl_core::{ClipEventKind, ClockSpec, Engine, Foot, Gait, GroundCondition, MusicRegion, MusicSet, MusicSlot, SoundCategory, SoundCue, SoundData, SoundEncoding, SurfaceKind};
use ffl_hub::{SoundActivity, SoundSource};

use crate::app::{AppState, Audio, Engines, HubState, RuntimeOptions, WorldEntity, WorldRequest};
use crate::camera::MainCamera;
use crate::world::character::{CharacterAnimator, CharacterSounds, CharacterSystems};
use crate::world::player::{CharacterRig, Player, PlayerSystems};
use crate::world::zone::ZoneLoader;

/// `FFL_TRACE_AUDIO=1`: log every play, stop and fade.
fn trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FFL_TRACE_AUDIO").is_some_and(|v| v != "0"))
}

macro_rules! trace_audio {
    ($($arg:tt)*) => {
        if trace_enabled() {
            info!($($arg)*);
        }
    };
}

// ---------------------------------------------------------------------------------------------
// PCM clip asset
// ---------------------------------------------------------------------------------------------

/// Decoded audio as a Bevy asset. The loop is applied sample-exactly inside the decoder, so
/// clips always play with `PlaybackMode::Once` (`Loop` would `repeat_infinite`, which buffers
/// a second copy of a 30 MB track).
#[derive(Asset, TypePath)]
pub struct PcmClip {
    pub rate: u32,
    pub channels: u16,
    /// Interleaved samples, shared between the plain and the looped variant.
    pub samples: Arc<[i16]>,
    /// Loop region in sample frames `[start, end)`; `None` loops the whole clip.
    pub loop_range: Option<(u64, u64)>,
    pub looped: bool,
}

impl PcmClip {
    pub fn frames(&self) -> u64 {
        self.samples.len() as u64 / self.channels.max(1) as u64
    }

    pub fn duration_secs(&self) -> f32 {
        self.frames() as f32 / self.rate.max(1) as f32
    }
}

/// Iterator over a [`PcmClip`] yielding interleaved `f32` samples (rodio's sample type).
pub struct PcmDecoder {
    samples: Arc<[i16]>,
    channels: ChannelCount,
    rate: SampleRate,
    pos: usize,
    /// Sample index playback restarts at when the loop end is reached.
    loop_start: usize,
    /// Exclusive end (of the loop region while looping, of the clip otherwise).
    end: usize,
    looped: bool,
    duration: Option<Duration>,
}

impl Iterator for PcmDecoder {
    type Item = Sample;

    #[inline]
    fn next(&mut self) -> Option<Sample> {
        if self.pos >= self.end {
            if !self.looped || self.loop_start >= self.end {
                return None;
            }
            self.pos = self.loop_start;
        }
        let s = self.samples[self.pos];
        self.pos += 1;
        Some(s as f32 / 32768.0)
    }
}

impl Source for PcmDecoder {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }
}

impl Decodable for PcmClip {
    type Decoder = PcmDecoder;

    fn decoder(&self) -> PcmDecoder {
        let ch = self.channels.max(1) as usize;
        let len = self.samples.len() - self.samples.len() % ch;
        let (loop_start, end) = match (self.looped, self.loop_range) {
            (true, Some((s, e))) => ((s as usize * ch).min(len), (e as usize * ch).min(len)),
            _ => (0, len),
        };
        PcmDecoder {
            samples: self.samples.clone(),
            channels: NonZero::new(self.channels.max(1)).unwrap(),
            rate: NonZero::new(self.rate.max(1)).unwrap(),
            pos: 0,
            loop_start,
            end,
            looped: self.looped,
            duration: (!self.looped).then(|| Duration::from_secs_f32(self.duration_secs())),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Library: key → clip, loaded through the engine that owns the key
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct LoadedClip {
    pub plain: Handle<PcmClip>,
    pub looped: Handle<PcmClip>,
    pub duration: f32,
    /// The game's own volume for this sound (`SoundData::gain`).
    pub gain: f32,
}

/// The game's volume of the clip a sink plays; multiplied into the mix with [`Gain`].
#[derive(Component, Clone, Copy)]
pub struct ClipGain(pub f32);

/// Every sound the world has asked for, by key. Loads run on the compute pool; the engine is
/// picked from the key prefix (`ff14:`, `ff11:`), so a FF14 character in a FF11 world loads
/// footsteps from one engine and music from the other.
#[derive(Resource, Default)]
pub struct SoundLibrary {
    clips: HashMap<String, LoadedClip>,
    pending: HashMap<String, Task<anyhow::Result<SoundData>>>,
    failed: HashSet<String>,
    /// PCM bytes held by the loaded clips.
    pub bytes: usize,
    /// `--audio-dump`: write each decoded clip once as WAV here.
    dump_dir: Option<PathBuf>,
    dumped: HashSet<String>,
}

impl SoundLibrary {
    pub fn get(&self, key: &str) -> Option<&LoadedClip> {
        self.clips.get(key)
    }

    pub fn is_failed(&self, key: &str) -> bool {
        self.failed.contains(key)
    }

    pub fn loaded_count(&self) -> usize {
        self.clips.len()
    }

    /// Start loading `key` unless it is loaded, loading or failed. Returns true when loaded.
    pub fn request(&mut self, engines: &Engines, key: &str) -> bool {
        if self.clips.contains_key(key) {
            return true;
        }
        if self.pending.contains_key(key) || self.failed.contains(key) {
            return false;
        }
        let prefix = key.split_once(':').map(|(p, _)| p).unwrap_or("");
        let Some(engine) = engines.get(prefix) else {
            warn!("sound {key}: no engine for prefix {prefix:?}");
            self.failed.insert(key.to_string());
            return false;
        };
        let engine: Arc<dyn Engine> = engine;
        let task_key = key.to_string();
        trace_audio!("sound load {key}");
        self.pending.insert(key.to_string(), AsyncComputeTaskPool::get().spawn(async move { engine.load_sound(&task_key) }));
        false
    }

    fn dump(&mut self, sound: &SoundData) {
        let Some(dir) = self.dump_dir.clone() else {
            return;
        };
        if !self.dumped.insert(sound.key.clone()) {
            return;
        }
        let Some(bytes) = sound.wav_bytes() else {
            return;
        };
        let name: String = sound
            .key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
            .collect();
        let path = dir.join(format!("{name}.wav"));
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &bytes)) {
            Ok(()) => info!("sound dump {} ({} bytes)", path.display(), bytes.len()),
            Err(err) => warn!("sound dump {}: {err}", path.display()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Components and resources
// ---------------------------------------------------------------------------------------------

/// Mixer category of an audio entity.
#[derive(Component, Clone, Copy)]
pub struct Bus(pub SoundCategory);

/// Per-entity linear gain (cue gain, distance attenuation, fade), before the category volume.
#[derive(Component, Clone, Copy)]
pub struct Gain(pub f32);

/// A sound that plays once. Bevy's `Despawn` mode only fires when a sink exists; without an
/// audio device sinks never appear, so the entity also expires after `ttl` seconds.
#[derive(Component)]
pub struct OneShot {
    pub ttl: f32,
}

/// Marks voice lines (despawned when the emote that started them ends).
#[derive(Component)]
pub struct VoiceLine;

/// A placed ambient loop (one entity per `ffl_core::AmbientEmitter`).
#[derive(Component)]
pub struct Emitter {
    pub name: String,
    pub key: String,
    pub inner: f32,
    pub max: f32,
    pub height: f32,
    pub positional: bool,
    pub gain: f32,
    /// A player component is attached (the sink follows a frame later when a device exists).
    pub started: bool,
    /// Intermittent one-shots and the seconds until the next one.
    pub spots: Vec<String>,
    pub spot_interval: (f32, f32),
    /// Surf mode: the program's short looping entry was one of several variations, so the
    /// variations play back to back (each for its own length) instead of one of them looping
    /// like a metronome under the rest.
    pub continuous: bool,
    pub next_spot: f32,
    pub rng: u32,
}

impl Emitter {
    fn next_random(&mut self) -> u32 {
        let mut x = self.rng.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    fn schedule_spot(&mut self) {
        let (lo, hi) = self.spot_interval;
        let t = (self.next_random() % 1000) as f32 / 1000.0;
        self.next_spot = lo + (hi - lo).max(0.0) * t;
    }
}

/// Ground material of a zone mesh (from its `MaterialDesc::surface`), read through the
/// player's `ground_entity` when a foot lands.
#[derive(Component, Clone, Copy)]
pub struct Surface(pub SurfaceKind);

/// What a looping sound entity (music, ambience) is, for the sound monitor. One-shots are
/// recorded in [`SoundMonitorState`] when they start instead.
#[derive(Component, Clone)]
pub struct SoundLabel {
    pub source: SoundSource,
    pub name: String,
    pub detail: String,
}

/// The world's weather as far as sound is concerned. Nothing sets it yet besides `--wet`;
/// a weather system will. Worlds do not carry it: zones only tag their surfaces.
#[derive(Resource, Default, Clone, Copy)]
pub struct Weather {
    pub wet: bool,
}

impl Weather {
    pub fn ground(&self) -> GroundCondition {
        if self.wet { GroundCondition::Wet } else { GroundCondition::Dry }
    }
}

/// Feeds the hub's sound monitor: live loops come from [`SoundLabel`] entities each frame,
/// one-shots are remembered here from their start until shortly after they end.
#[derive(Resource, Default)]
pub struct SoundMonitorState {
    pub view: ffl_hub::SoundMonitor,
    /// (entry, app time the clip ends).
    shots: Vec<(SoundActivity, f32)>,
}

/// How long an ended one-shot stays listed (dimmed).
const MONITOR_LINGER: f32 = 1.5;

impl SoundMonitorState {
    fn record_shot(&mut self, now: f32, duration: f32, entry: SoundActivity) {
        self.shots.push((entry, now + duration));
    }
}

/// Engine prefix of a sound key (`ff14:scd:...` → `ff14`).
fn key_engine(key: &str) -> &str {
    key.split_once(':').map(|(p, _)| p).unwrap_or("?")
}

/// Short display name of a sound key: the file stem and entry (`fs_stone_f_f_boots #4`,
/// `bgw 109`).
pub fn key_name(key: &str) -> String {
    let body = key.splitn(3, ':').nth(2).unwrap_or(key);
    let (path, entry) = body.split_once('#').unwrap_or((body, ""));
    let (path, pack) = path.split_once('@').unwrap_or((path, ""));
    let stem = path.rsplit('/').next().unwrap_or(path);
    let stem = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem);
    let mut name = match key.splitn(3, ':').nth(1) {
        Some(kind) if !stem.chars().any(|c| c.is_ascii_alphabetic()) => format!("{kind} {stem}"),
        _ => stem.to_string(),
    };
    if !entry.is_empty() {
        name.push_str(" #");
        name.push_str(entry);
    }
    if !pack.is_empty() {
        name.push_str(" @");
        name.push_str(pack);
    }
    name
}

struct Fade {
    from: f32,
    to: f32,
    elapsed: f32,
    duration: f32,
    despawn_at_end: bool,
}

impl Fade {
    fn value(&self) -> f32 {
        let t = if self.duration > 0.0 { (self.elapsed / self.duration).clamp(0.0, 1.0) } else { 1.0 };
        self.from + (self.to - self.from) * t
    }

    fn done(&self) -> bool {
        self.elapsed >= self.duration
    }
}

/// One background music entity.
#[derive(Component)]
struct BgmTrack {
    key: String,
    fade: Fade,
}

/// Per-animator bookkeeping for [`clip_events`].
#[derive(Component)]
pub struct EventTracker {
    clip: Option<String>,
    last_time: f32,
    rng: u32,
    air_time: f32,
    was_grounded: bool,
    /// Last voice line and when it started (app seconds), to ignore the immediate re-fire when
    /// an emote is interrupted and restarted within a few frames (spawn-time ground flicker).
    last_voice: Option<(u32, f32)>,
}

impl Default for EventTracker {
    fn default() -> Self {
        Self { clip: None, last_time: 0.0, rng: 0x9E37_79B9, air_time: 0.0, was_grounded: true, last_voice: None }
    }
}

/// A voice line restarted within this many seconds is the same emote being re-triggered.
const VOICE_DEBOUNCE_SECONDS: f32 = 0.5;

impl EventTracker {
    fn next_random(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }
}

/// Background music state of the current world.
#[derive(Resource, Default)]
pub struct Bgm {
    /// The map-wide set (`Scene::music`).
    set: Option<MusicSet>,
    /// Sub-area sets; the highest-priority one containing the listener wins over `set`.
    regions: Vec<MusicRegion>,
    /// Index into `regions` of the one the listener is in.
    region: Option<usize>,
    /// The music set was looked up (it may be `None`).
    resolved: bool,
    /// Entity and key of the track playing (or fading in).
    current: Option<(Entity, String)>,
    slot: Option<MusicSlot>,
}

/// The world engine's game clock, advanced by the runtime.
#[derive(Resource, Default)]
pub struct WorldClock {
    pub spec: Option<ClockSpec>,
    /// Game hour of the day (0..24).
    pub hours: f32,
    /// Seeded from `--time` instead of the wall clock (it still advances at the game rate).
    pub fixed: bool,
}

impl WorldClock {
    pub fn slot(&self) -> Option<MusicSlot> {
        self.spec.as_ref().map(|s| s.slot(self.hours))
    }

    pub fn hhmm(&self) -> String {
        let h = self.hours.rem_euclid(24.0);
        let hh = h.floor();
        let mm = ((h - hh) * 60.0).floor();
        format!("{hh:02.0}:{mm:02.0}")
    }
}

/// Overlay lines (see `debug_ui`).
#[derive(Resource, Default)]
pub struct AudioTrace {
    pub bgm: String,
    pub ambience: String,
    pub step: String,
    pub voice: String,
    /// Sinks alive this frame.
    pub sinks: usize,
}

/// One-shots whose clip is still loading (played as soon as it arrives, dropped after the
/// deadline: a footstep half a second late is worse than none).
#[derive(Resource, Default)]
struct PendingPlays(Vec<PendingPlay>);

struct PendingPlay {
    key: String,
    bus: SoundCategory,
    gain: f32,
    voice: bool,
    label: String,
    source: SoundSource,
    detail: String,
    deadline: f32,
}

#[derive(Resource, Default)]
struct Ambience {
    spawned: bool,
    total: usize,
    active: usize,
}

/// Placed ambient loops kept playing at once.
const MAX_AMBIENT_SINKS: usize = 16;
/// Emitters keep playing until this far beyond their max distance (hysteresis).
const AMBIENT_HYSTERESIS: f32 = 10.0;
const AMBIENT_START_GAIN: f32 = 0.005;
/// A looping ambience entry shorter than this, in a program that also has one-shot
/// variations, is played as one of the variations (see `Emitter::continuous`).
const SURF_LOOP_MAX_SECONDS: f32 = 8.0;
const MIN_FADE: f32 = 0.5;
/// Airborne time after which touching down counts as a landing.
const LAND_AIR_TIME: f32 = 0.15;
const PENDING_PLAY_WINDOW: f32 = 1.5;

fn slot_label(slot: MusicSlot) -> &'static str {
    match slot {
        MusicSlot::Day => "day",
        MusicSlot::Night => "night",
        MusicSlot::Daybreak => "daybreak",
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------------------------

pub struct SoundPlugin;

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.add_audio_source::<PcmClip>()
            .init_resource::<SoundLibrary>()
            .init_resource::<Bgm>()
            .init_resource::<WorldClock>()
            .init_resource::<AudioTrace>()
            .init_resource::<PendingPlays>()
            .init_resource::<Ambience>()
            .init_resource::<Weather>()
            .init_resource::<SoundMonitorState>()
            .add_systems(OnEnter(AppState::World), enter_world)
            .add_systems(OnExit(AppState::World), exit_world)
            .add_systems(
                Update,
                (
                    poll_sound_loads,
                    tick_clock,
                    bgm_controller,
                    spawn_emitters,
                    update_emitters,
                    clip_events,
                    voice_cleanup,
                    expire_one_shots,
                    mute_hotkey,
                    mix_sinks,
                    update_monitor,
                )
                    .chain()
                    .after(CharacterSystems)
                    .after(PlayerSystems)
                    .run_if(in_state(AppState::World)),
            );
    }
}

fn enter_world(
    engines: Res<Engines>,
    request: Res<WorldRequest>,
    opts: Res<RuntimeOptions>,
    mut library: ResMut<SoundLibrary>,
    mut bgm: ResMut<Bgm>,
    mut clock: ResMut<WorldClock>,
    mut trace: ResMut<AudioTrace>,
    mut pending: ResMut<PendingPlays>,
    mut ambience: ResMut<Ambience>,
    mut weather: ResMut<Weather>,
    mut monitor: ResMut<SoundMonitorState>,
) {
    *library = SoundLibrary { dump_dir: opts.audio_dump.clone(), ..Default::default() };
    *bgm = Bgm::default();
    *trace = AudioTrace::default();
    *pending = PendingPlays::default();
    *ambience = Ambience::default();
    *weather = Weather { wet: opts.wet };
    *monitor = SoundMonitorState::default();
    if opts.wet {
        info!("weather: wet ground (--wet), footsteps use the rain banks");
    }
    let spec = engines.get(&request.engine).and_then(|e| e.clock());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let hours = match (&spec, opts.time) {
        (_, Some(t)) => t,
        (Some(s), None) => s.hours(now),
        (None, None) => 12.0,
    };
    *clock = WorldClock { spec, hours, fixed: opts.time.is_some() };
    if opts.mute {
        info!("audio starts muted (--mute)");
    }
    match &clock.spec {
        Some(s) => info!("clock {} {} ({} game s per real s, day {:?}, daybreak {:?}){}", s.name, clock.hhmm(), s.rate, s.day, s.daybreak, if clock.fixed { " from --time" } else { "" }),
        None => info!("clock: {} has no game clock; music stays on the day slot", request.engine),
    }
}

fn exit_world(tracks: Query<&BgmTrack>, emitters: Query<&Emitter>, mut library: ResMut<SoundLibrary>, mut bgm: ResMut<Bgm>, mut pending: ResMut<PendingPlays>) {
    // The entities themselves are `WorldEntity`s: `zone::exit_world` despawns them, which drops
    // their sinks and stops playback.
    for t in &tracks {
        info!("bgm stop {} (leaving the world)", t.key);
    }
    let playing = emitters.iter().filter(|e| e.started).count();
    if playing > 0 {
        trace_audio!("emitter stop all ({playing} playing, leaving the world)");
    }
    *library = SoundLibrary::default();
    *bgm = Bgm::default();
    *pending = PendingPlays::default();
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

fn poll_sound_loads(
    mut commands: Commands,
    time: Res<Time>,
    audio: Res<Audio>,
    mut library: ResMut<SoundLibrary>,
    mut clips: ResMut<Assets<PcmClip>>,
    mut pending: ResMut<PendingPlays>,
    mut monitor: ResMut<SoundMonitorState>,
) {
    let mut done = Vec::new();
    for (key, task) in library.pending.iter_mut() {
        if let Some(result) = block_on(poll_once(task)) {
            done.push((key.clone(), result));
        }
    }
    for (key, result) in done {
        library.pending.remove(&key);
        match result {
            Ok(sound) => {
                library.dump(&sound);
                let samples: Vec<i16> = match sound.encoding {
                    SoundEncoding::Pcm16(s) => s,
                    SoundEncoding::OggVorbis(_) => {
                        warn!("sound {key}: Ogg Vorbis payloads are not decoded by the runtime (engines hand over PCM)");
                        library.failed.insert(key);
                        continue;
                    }
                };
                let bytes = samples.len() * 2;
                let samples: Arc<[i16]> = Arc::from(samples);
                let base = PcmClip { rate: sound.sample_rate, channels: sound.channels, samples: samples.clone(), loop_range: sound.loop_range, looped: false };
                let duration = base.duration_secs();
                let looped = PcmClip { samples, looped: true, ..base };
                let plain = clips.add(base);
                let looped = clips.add(looped);
                library.bytes += bytes;
                info!(
                    "sound loaded {key}: {duration:.2}s {}ch {}Hz{} ({:.1} MB)",
                    sound.channels,
                    sound.sample_rate,
                    sound.loop_range.map(|(s, e)| format!(" loop {s}..{e}")).unwrap_or_default(),
                    bytes as f64 / 1e6
                );
                library.clips.insert(key, LoadedClip { plain, looped, duration, gain: sound.gain.clamp(0.0, 1.0) });
            }
            Err(err) => {
                warn!("sound {key}: {err:#}");
                library.failed.insert(key);
            }
        }
    }
    // One-shots that waited for their clip.
    let now = time.elapsed_secs();
    let mut waiting = std::mem::take(&mut pending.0);
    waiting.retain(|p| {
        if let Some(clip) = library.get(&p.key).cloned() {
            spawn_one_shot(&mut commands, &audio, &mut monitor, now, &clip, &p.key, p.bus, p.gain, p.voice, &p.label, p.source, &p.detail);
            false
        } else if library.is_failed(&p.key) || now > p.deadline {
            trace_audio!("{} dropped: {} {}", p.label, p.key, if library.is_failed(&p.key) { "failed" } else { "loaded too late" });
            false
        } else {
            true
        }
    });
    pending.0 = waiting;
}

#[allow(clippy::too_many_arguments)]
fn spawn_one_shot(
    commands: &mut Commands,
    audio: &Audio,
    monitor: &mut SoundMonitorState,
    now: f32,
    clip: &LoadedClip,
    key: &str,
    bus: SoundCategory,
    gain: f32,
    voice: bool,
    label: &str,
    source: SoundSource,
    detail: &str,
) {
    let mix = audio.mix(bus, gain * clip.gain);
    trace_audio!("{label} {key} gain={gain:.2} clip={:.2} mix={mix:.2} {:.2}s", clip.gain, clip.duration);
    monitor.record_shot(
        now,
        clip.duration,
        SoundActivity { source, engine: key_engine(key).to_string(), category: bus, name: key_name(key), detail: detail.to_string(), level: mix, ended_for: 0.0 },
    );
    let mut e = commands.spawn((
        Name::new(format!("sound {key}")),
        WorldEntity,
        Bus(bus),
        Gain(gain),
        ClipGain(clip.gain),
        OneShot { ttl: clip.duration + 0.5 },
        AudioPlayer(clip.plain.clone()),
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(mix)),
    ));
    if voice {
        e.insert(VoiceLine);
    }
}

/// Play one variation of a cue now, or as soon as its clip is loaded.
#[allow(clippy::too_many_arguments)]
fn play_cue(
    commands: &mut Commands,
    engines: &Engines,
    library: &mut SoundLibrary,
    audio: &Audio,
    pending: &mut PendingPlays,
    monitor: &mut SoundMonitorState,
    now: f32,
    cue: &SoundCue,
    pick: u32,
    bus: SoundCategory,
    voice: bool,
    label: &str,
    source: SoundSource,
    detail: &str,
) -> Option<String> {
    if cue.variations.is_empty() {
        return None;
    }
    let key = &cue.variations[pick as usize % cue.variations.len()];
    if library.request(engines, key) {
        let clip = library.get(key).cloned()?;
        spawn_one_shot(commands, audio, monitor, now, &clip, key, bus, cue.gain, voice, label, source, detail);
    } else if !library.is_failed(key) {
        trace_audio!("{label} {key} (loading)");
        pending.0.push(PendingPlay {
            key: key.clone(),
            bus,
            gain: cue.gain,
            voice,
            label: label.to_string(),
            source,
            detail: detail.to_string(),
            deadline: now + PENDING_PLAY_WINDOW,
        });
    }
    Some(key.clone())
}

// ---------------------------------------------------------------------------------------------
// Clock and BGM
// ---------------------------------------------------------------------------------------------

fn tick_clock(time: Res<Time>, mut clock: ResMut<WorldClock>) {
    if let Some(spec) = &clock.spec {
        let rate = spec.rate;
        clock.hours = (clock.hours + time.delta_secs() * rate / 3600.0).rem_euclid(24.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn bgm_controller(
    mut commands: Commands,
    time: Res<Time>,
    engines: Res<Engines>,
    request: Res<WorldRequest>,
    loader: Res<ZoneLoader>,
    audio: Res<Audio>,
    clock: Res<WorldClock>,
    mut library: ResMut<SoundLibrary>,
    mut bgm: ResMut<Bgm>,
    mut trace: ResMut<AudioTrace>,
    mut tracks: Query<(Entity, &mut BgmTrack, &mut Gain, Option<&AudioSink>)>,
    players: Query<&Transform, With<Player>>,
    camera: Query<&GlobalTransform, With<MainCamera>>,
) {
    if !bgm.resolved {
        let Some(scene) = loader.scene() else {
            trace.bgm = "bgm: waiting for the scene".into();
            return;
        };
        bgm.resolved = true;
        bgm.regions = scene.music_regions.clone();
        if !bgm.regions.is_empty() {
            info!("bgm regions: {} sub-area music volumes", bgm.regions.len());
        }
        let mut set = scene.music.clone();
        if set.is_none() {
            match engines.get(&request.engine).map(|e| e.music_for_map(&request.map)) {
                Some(Ok(s)) => set = s,
                Some(Err(err)) => warn!("bgm: {err:#}"),
                None => {}
            }
        }
        match &set {
            Some(s) if !s.is_empty() => {
                info!(
                    "bgm set: day={} night={} daybreak={} battle={} extra={:?} fade in {:.1}s out {:.1}s",
                    s.day.as_deref().unwrap_or("-"),
                    s.night.as_deref().unwrap_or("-"),
                    s.daybreak.as_deref().unwrap_or("-"),
                    s.battle.as_deref().unwrap_or("-"),
                    s.extra,
                    s.fade_in,
                    s.fade_out
                );
                for n in &s.notes {
                    info!("bgm note: {n}");
                }
            }
            _ => info!("bgm none for {}:{}", request.engine, request.map),
        }
        bgm.set = set.filter(|s| !s.is_empty());
    }

    // Fades.
    let dt = time.delta_secs();
    for (entity, mut track, mut gain, _) in &mut tracks {
        if track.fade.done() {
            continue;
        }
        track.fade.elapsed += dt;
        gain.0 = track.fade.value();
        if track.fade.done() {
            if track.fade.despawn_at_end {
                info!("bgm stop {} (faded out)", track.key);
                commands.entity(entity).despawn();
            } else {
                trace_audio!("bgm {} fade done gain={:.2}", track.key, gain.0);
            }
        }
    }
    // Music switch: pause/resume the sinks (the volume stays where the mixer puts it).
    let enabled = audio.current.music_enabled;
    for (_, track, _, sink) in &mut tracks {
        if let Some(sink) = sink {
            if enabled && sink.is_paused() {
                info!("bgm resume {}", track.key);
                sink.play();
            } else if !enabled && !sink.is_paused() {
                info!("bgm pause {}", track.key);
                sink.pause();
            }
        }
    }

    // Sub-area music: the highest-priority region around the listener (the player when there
    // is one, else the camera) replaces the map-wide set while inside it.
    let listener = players
        .single()
        .map(|t| t.translation)
        .ok()
        .or_else(|| camera.single().ok().map(|c| c.translation()))
        .unwrap_or(Vec3::ZERO);
    let region = bgm
        .regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains(listener.to_array()))
        .max_by_key(|(_, r)| r.priority)
        .map(|(i, _)| i);
    if region != bgm.region {
        match region {
            Some(i) => {
                let r = &bgm.regions[i];
                info!(
                    "bgm region {} (priority {}): day={} night={}",
                    r.name,
                    r.priority,
                    r.music.day.as_deref().unwrap_or("-"),
                    r.music.night.as_deref().unwrap_or("-")
                );
            }
            None => info!("bgm region none (map music)"),
        }
        bgm.region = region;
    }
    let region_name = region.map(|i| bgm.regions[i].name.clone());
    let set = match region {
        Some(i) => Some(bgm.regions[i].music.clone()),
        None => bgm.set.clone(),
    };
    let Some(set) = set else {
        if let Some((old, old_key)) = bgm.current.take()
            && let Ok((_, mut track, gain, _)) = tracks.get_mut(old)
        {
            info!("bgm fade {old_key} -> silence (no music here)");
            track.fade = Fade { from: gain.0, to: 0.0, elapsed: 0.0, duration: MIN_FADE * 2.0, despawn_at_end: true };
        }
        trace.bgm = "bgm none".into();
        return;
    };
    let slot = clock.slot().unwrap_or(MusicSlot::Day);
    let wanted = set.for_slot(slot).map(str::to_string);
    let current_key = bgm.current.as_ref().map(|(_, k)| k.clone());
    let mut state = "playing";
    if wanted != current_key {
        if let Some(key) = &wanted {
            if library.request(&engines, key) {
                let clip = library.get(key).cloned().unwrap();
                let fade_in = set.fade_in.max(MIN_FADE);
                let fade_out = set.fade_out.max(MIN_FADE);
                if let Some((old, old_key)) = bgm.current.take() {
                    if let Ok((_, mut track, gain, _)) = tracks.get_mut(old) {
                        info!("bgm fade {old_key} -> {key} ({}: out {fade_out:.1}s, in {fade_in:.1}s)", slot_label(slot));
                        track.fade = Fade { from: gain.0, to: 0.0, elapsed: 0.0, duration: fade_out, despawn_at_end: true };
                    }
                }
                let mix = audio.mix(SoundCategory::Music, 0.0);
                info!("bgm start {key} slot {} clock {} fade in {fade_in:.1}s mix={:.2}", slot_label(slot), clock.hhmm(), audio.mix(SoundCategory::Music, 1.0));
                let entity = commands
                    .spawn((
                        Name::new(format!("bgm {key}")),
                        WorldEntity,
                        Bus(SoundCategory::Music),
                        Gain(0.0),
                        SoundLabel { source: SoundSource::World, name: key_name(key), detail: format!("{} slot", slot_label(slot)) },
                        BgmTrack { key: key.clone(), fade: Fade { from: 0.0, to: 1.0, elapsed: 0.0, duration: fade_in, despawn_at_end: false } },
                        ClipGain(clip.gain),
                        AudioPlayer(clip.looped.clone()),
                        {
                            let mut s = PlaybackSettings::ONCE.with_volume(Volume::Linear(mix));
                            s.paused = !enabled;
                            s
                        },
                    ))
                    .id();
                bgm.current = Some((entity, key.clone()));
                bgm.slot = Some(slot);
            } else if library.is_failed(key) {
                state = "failed";
            } else {
                state = "loading";
            }
        } else if let Some((old, old_key)) = bgm.current.take() {
            // The slot has no track at all: fade the current one out.
            if let Ok((_, mut track, gain, _)) = tracks.get_mut(old) {
                info!("bgm fade {old_key} -> silence ({}: no track for this slot)", slot_label(slot));
                track.fade = Fade { from: gain.0, to: 0.0, elapsed: 0.0, duration: set.fade_out.max(MIN_FADE), despawn_at_end: true };
            }
        }
    }
    let fading = tracks.iter().any(|(_, t, _, _)| !t.fade.done());
    let state = if !enabled {
        "paused"
    } else if state == "playing" && fading {
        "fading"
    } else {
        state
    };
    let clock_text = clock.spec.as_ref().map(|s| format!(" {} {}", s.name, clock.hhmm())).unwrap_or_default();
    let region_text = region_name.map(|n| format!(" in {n}")).unwrap_or_default();
    // The overlay shows the file name; the log lines carry the full key.
    let short = wanted.as_deref().map(|k| k.rsplit('/').next().unwrap_or(k)).unwrap_or("silence");
    trace.bgm = format!("bgm {short} [{state}]{clock_text} ({}){region_text}", slot_label(slot));
}

// ---------------------------------------------------------------------------------------------
// Ambience
// ---------------------------------------------------------------------------------------------

fn spawn_emitters(mut commands: Commands, loader: Res<ZoneLoader>, mut ambience: ResMut<Ambience>) {
    if ambience.spawned {
        return;
    }
    let Some(scene) = loader.scene() else {
        return;
    };
    ambience.spawned = true;
    ambience.total = scene.emitters.len();
    for (i, e) in scene.emitters.iter().enumerate() {
        debug!(
            "emitter {} {} at ({:.1}, {:.1}, {:.1}) inner {:.0} max {:.0} height {:.0} gain {:.2}{}",
            e.name,
            e.key,
            e.position[0],
            e.position[1],
            e.position[2],
            e.inner_radius,
            e.max_distance,
            e.height,
            e.gain,
            if e.positional { "" } else { " (bed)" }
        );
        commands.spawn((
            Name::new(format!("emitter {}", e.name)),
            WorldEntity,
            Transform::from_translation(Vec3::from(e.position)),
            Bus(SoundCategory::Ambience),
            Gain(0.0),
            {
                let mut em = Emitter {
                    name: e.name.clone(),
                    key: e.key.clone(),
                    inner: e.inner_radius,
                    max: e.max_distance,
                    height: e.height,
                    positional: e.positional,
                    gain: e.gain,
                    started: false,
                    spots: e.spots.clone(),
                    spot_interval: e.spot_interval,
                    next_spot: 0.0,
                    rng: 0x2545_F491 ^ (i as u32).wrapping_mul(0x9E37_79B9),
                    continuous: false,
                };
                em.schedule_spot();
                em
            },
        ));
    }
    if ambience.total > 0 {
        info!("ambience: {} emitters placed (up to {MAX_AMBIENT_SINKS} playing at once)", ambience.total);
    }
}

#[allow(clippy::too_many_arguments)]
fn update_emitters(
    mut commands: Commands,
    time: Res<Time>,
    engines: Res<Engines>,
    audio: Res<Audio>,
    mut library: ResMut<SoundLibrary>,
    mut ambience: ResMut<Ambience>,
    mut trace: ResMut<AudioTrace>,
    mut monitor: ResMut<SoundMonitorState>,
    listener: Query<&GlobalTransform, With<MainCamera>>,
    mut emitters: Query<(Entity, &mut Emitter, &Transform, &mut Gain)>,
) {
    let dt = time.delta_secs();
    let now = time.elapsed_secs();
    let Ok(listener) = listener.single() else {
        return;
    };
    let at = listener.translation();
    // (entity, gain, distance) for every emitter in reach, loudest first.
    let mut candidates: Vec<(Entity, f32, f32)> = Vec::new();
    for (entity, emitter, transform, mut gain) in &mut emitters {
        let (g, d) = if emitter.positional {
            let p = transform.translation;
            let d_xz = Vec2::new(p.x - at.x, p.z - at.z).length();
            let dy = (p.y - at.y).abs();
            let h = emitter.height.max(0.5);
            // Quadratic falloff from the inner radius: the smoothstep kept a 180 m beach wave
            // near full level over a third of its range, drowning the terrace above the beach.
            let inner = emitter.inner.min(emitter.max);
            let t = ((d_xz - inner) / (emitter.max - inner).max(0.1)).clamp(0.0, 1.0);
            // Vertically the sound reaches as far as horizontally: the 5 m "height" every
            // placed sound carries silenced the beach waves from a terrace 15 m up.
            let g = emitter.gain * (1.0 - t) * (1.0 - t) * (1.0 - smoothstep(h, emitter.max.max(2.0 * h), dy));
            (g, d_xz)
        } else {
            (emitter.gain, 0.0)
        };
        gain.0 = g;
        let in_reach = g > AMBIENT_START_GAIN || (emitter.started && emitter.positional && d <= emitter.max + AMBIENT_HYSTERESIS) || (emitter.started && !emitter.positional);
        if in_reach {
            candidates.push((entity, g, d));
        }
    }
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let keep: HashSet<Entity> = candidates.iter().take(MAX_AMBIENT_SINKS).map(|c| c.0).collect();
    let mut active = 0;
    for (entity, mut emitter, _, gain) in &mut emitters {
        let wanted = keep.contains(&entity);
        if wanted && !emitter.started {
            if emitter.key.is_empty() {
                // Spots only (boat creaks): nothing loops, the timer below plays them.
                emitter.started = true;
            } else if library.request(&engines, &emitter.key) {
                let clip = library.get(&emitter.key).cloned().unwrap();
                if !emitter.spots.is_empty() && clip.duration < SURF_LOOP_MAX_SECONDS {
                    // A short loop among one-shot variations (beach waves: a 3.7 s crash next
                    // to 6 s and 4 s ones) is a variation itself, not a bed.
                    trace_audio!("emitter {} {}: {:.1}s loop joins its {} variations (surf mode)", emitter.name, emitter.key, clip.duration, emitter.spots.len());
                    let key = std::mem::take(&mut emitter.key);
                    emitter.spots.push(key);
                    emitter.continuous = true;
                    emitter.next_spot = 0.0;
                    emitter.started = true;
                    continue;
                }
                let mix = audio.mix(SoundCategory::Ambience, gain.0);
                trace_audio!("emitter start {} {} gain={:.2} mix={mix:.2}", emitter.name, emitter.key, gain.0);
                commands.entity(entity).insert((
                    ClipGain(clip.gain),
                    SoundLabel { source: SoundSource::World, name: key_name(&emitter.key), detail: format!("ambience {}", emitter.name) },
                    AudioPlayer(clip.looped.clone()),
                    PlaybackSettings::ONCE.with_volume(Volume::Linear(mix)),
                ));
                emitter.started = true;
            }
        } else if !wanted && emitter.started {
            if !emitter.key.is_empty() {
                trace_audio!("emitter stop {} {}", emitter.name, emitter.key);
                commands.entity(entity).remove::<(AudioPlayer<PcmClip>, AudioSink, PlaybackSettings)>();
            }
            emitter.started = false;
        }
        if emitter.started {
            active += 1;
            // Intermittent one-shots at the emitter's current gain.
            if !emitter.spots.is_empty() {
                emitter.next_spot -= dt;
                if emitter.next_spot <= 0.0 {
                    let pick = emitter.next_random() as usize % emitter.spots.len();
                    let key = emitter.spots[pick].clone();
                    if library.request(&engines, &key) {
                        let clip = library.get(&key).cloned().unwrap();
                        let label = format!("spot {}", emitter.name);
                        let detail = format!("spot {}", emitter.name);
                        spawn_one_shot(&mut commands, &audio, &mut monitor, now, &clip, &key, SoundCategory::Ambience, gain.0, false, &label, SoundSource::World, &detail);
                        if emitter.continuous {
                            // The next variation follows this one, with a little slack.
                            let t = (emitter.next_random() % 1000) as f32 / 1000.0;
                            emitter.next_spot = (clip.duration * (0.85 + 0.35 * t)).max(0.5);
                        } else {
                            emitter.schedule_spot();
                        }
                    } else {
                        emitter.next_spot = 0.5;
                    }
                }
            }
        }
    }
    ambience.active = active;
    trace.ambience = format!("ambience {active}/{} sinks, clips {} ({:.1} MB)", ambience.total, library.loaded_count(), library.bytes as f64 / 1e6);
}

// ---------------------------------------------------------------------------------------------
// Clip events: footsteps, voice lines, emote sounds
// ---------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn clip_events(
    mut commands: Commands,
    time: Res<Time>,
    engines: Res<Engines>,
    audio: Res<Audio>,
    loader: Res<ZoneLoader>,
    mut library: ResMut<SoundLibrary>,
    mut pending: ResMut<PendingPlays>,
    mut trace: ResMut<AudioTrace>,
    mut monitor: ResMut<SoundMonitorState>,
    weather: Res<Weather>,
    players: Query<&Player>,
    surfaces: Query<(&Surface, Option<&Name>)>,
    lines: Query<Entity, With<VoiceLine>>,
    mut rigs: Query<(Entity, &CharacterAnimator, &CharacterSounds, Option<&mut EventTracker>), With<CharacterRig>>,
) {
    let Ok((rig, animator, sounds, tracker)) = rigs.single_mut() else {
        return;
    };
    let scene = loader.scene();
    let footsteps = sounds.footsteps.as_ref().or(scene.as_ref().and_then(|s| s.footsteps.as_ref()));
    let condition = weather.ground();
    let Some(mut tracker) = tracker else {
        // First sight of the character: pre-warm the footstep banks of the current weather.
        let mut n = 0;
        if let Some(f) = footsteps {
            for key in f.keys_for(condition) {
                library.request(&engines, key);
                n += 1;
            }
        }
        info!(
            "sound events: footsteps {} ({n} {} keys{}), voice {}",
            footsteps.map(|f| f.summary()).unwrap_or_else(|| "none".into()),
            condition.label(),
            if sounds.footsteps.is_none() && footsteps.is_some() { ", from the scene" } else { "" },
            sounds.voice.as_ref().map(|v| format!("{} ({} lines)", v.name, v.lines.len())).unwrap_or_else(|| "none".into())
        );
        commands.entity(rig).insert(EventTracker::default());
        return;
    };
    let now = time.elapsed_secs();
    let dt = time.delta_secs();
    let player = players.single().ok();
    let ground = player.and_then(|p| p.ground_entity).and_then(|e| surfaces.get(e).ok());
    let surface = ground.map(|(s, _)| s.0).unwrap_or(SurfaceKind::Unknown);
    let ground_name = ground.and_then(|(_, n)| n.map(|n| n.as_str())).unwrap_or("-");
    let gait = match player {
        Some(p) if p.moving && !p.walking => Gait::Run,
        _ => Gait::Walk,
    };

    // Landing: once, on the grounded rising edge after a real fall.
    if let Some(p) = player {
        if p.grounded {
            if !tracker.was_grounded && tracker.air_time > LAND_AIR_TIME && !p.flying {
                if let Some(cue) = footsteps.and_then(|f| f.cue(surface, Gait::Land, condition)) {
                    let pick = tracker.next_random();
                    let label = format!("land {}/{}", surface.label(), Gait::label(Gait::Land));
                    let detail = format!("{} / land / {}", surface.label(), condition.label());
                    if let Some(key) = play_cue(&mut commands, &engines, &mut library, &audio, &mut pending, &mut monitor, now, cue, pick, SoundCategory::Footsteps, false, &label, SoundSource::Character, &detail) {
                        trace.step = format!("step {}/land {key}", surface.label());
                    }
                } else {
                    trace_audio!("land {}: no cue (air {:.2}s)", surface.label(), tracker.air_time);
                }
            }
            tracker.air_time = 0.0;
        } else {
            tracker.air_time += dt;
        }
        tracker.was_grounded = p.grounded;
    }

    let Some(current) = animator.current.as_deref() else {
        tracker.clip = None;
        return;
    };
    let Some(clip) = animator.clips.get(current) else {
        return;
    };
    let t = animator.time;
    let changed = tracker.clip.as_deref() != Some(current);
    let looping = !animator.current_one_shot;
    // A one-shot whose time went backwards was restarted (`start_emote` on the same clip).
    let restarted = !changed && !looping && t < tracker.last_time;
    // Window of clip time covered this frame. On a clip change the window starts where the
    // new clip started (the phase hand-over of `play_with`), inclusive so events at 0 fire.
    let (last, inclusive) = if changed || restarted { ((t - dt).max(0.0), true) } else { (tracker.last_time, false) };
    if changed {
        debug!("sound events: clip {:?} -> {current} t={t:.3} (one-shot {})", tracker.clip, !looping);
    } else if restarted {
        debug!("sound events: {current} restarted t={t:.3} (was {:.3})", tracker.last_time);
    }
    tracker.clip = Some(current.to_string());
    tracker.last_time = t;
    if clip.events.is_empty() || clip.duration <= 0.0 {
        return;
    }
    let is_land_clip = [Some(&animator.locomotion), animator.armed_locomotion.as_ref()]
        .into_iter()
        .flatten()
        .any(|l| l.land.as_deref() == Some(current));
    let after_last = |e: f32| if inclusive { e >= last } else { e > last };
    let fired: Vec<&ffl_core::ClipEvent> = if looping {
        let d = clip.duration;
        let t_mod = t.rem_euclid(d);
        let last_mod = last.rem_euclid(d);
        if t - last >= d {
            clip.events.iter().collect()
        } else if t_mod >= last_mod {
            clip.events.iter().filter(|e| after_last_mod(e.time, last_mod, inclusive) && e.time <= t_mod).collect()
        } else {
            clip.events.iter().filter(|e| after_last_mod(e.time, last_mod, inclusive) || e.time <= t_mod).collect()
        }
    } else {
        let t_c = t.min(clip.duration);
        clip.events.iter().filter(|e| after_last(e.time) && e.time <= t_c).collect()
    };
    for event in fired {
        debug!("sound events: {current} fires t={:.3} (window {last:.3}..{t:.3}{}, dt {dt:.3})", event.time, if inclusive { " incl" } else { "" });
        match &event.kind {
            ClipEventKind::Footstep { foot, variant } => {
                if is_land_clip {
                    continue;
                }
                let Some(cue) = footsteps.and_then(|f| f.cue(surface, gait, condition)) else {
                    trace_audio!("step {}/{}/{}: no cue ({current} t={:.2})", surface.label(), gait.label(), condition.label(), event.time);
                    continue;
                };
                let pick = tracker.next_random();
                let side = if *foot == Foot::Left { "L" } else { "R" };
                let label = format!("step {}/{}/{} {side} v{variant} {current} t={:.2} on {ground_name}", surface.label(), gait.label(), condition.label(), event.time);
                let detail = format!("{} / {} / {} {side}", surface.label(), gait.label(), condition.label());
                if let Some(key) = play_cue(&mut commands, &engines, &mut library, &audio, &mut pending, &mut monitor, now, cue, pick, SoundCategory::Footsteps, false, &label, SoundSource::Character, &detail) {
                    trace.step = format!("step {}/{}/{} {key} on {ground_name}", surface.label(), gait.label(), condition.label());
                }
            }
            ClipEventKind::Voice { line } => {
                let Some(cue) = sounds.voice.as_ref().and_then(|v| v.line(*line)) else {
                    trace_audio!("voice line {line}: no cue ({current} t={:.2})", event.time);
                    trace.voice = format!("voice line {line}: none");
                    continue;
                };
                if tracker.last_voice.is_some_and(|(l, at)| l == *line && now - at < VOICE_DEBOUNCE_SECONDS) {
                    trace_audio!("voice line {line} re-fired within {VOICE_DEBOUNCE_SECONDS}s, kept playing");
                    continue;
                }
                tracker.last_voice = Some((*line, now));
                // One voice at a time: a restarted emote replaces the line still playing.
                for e in &lines {
                    trace_audio!("voice stop (replaced by line {line})");
                    commands.entity(e).despawn();
                }
                let pick = tracker.next_random();
                let label = format!("voice line {line} {current} t={:.2}", event.time);
                let detail = format!("line {line} ({current})");
                if let Some(key) = play_cue(&mut commands, &engines, &mut library, &audio, &mut pending, &mut monitor, now, cue, pick, SoundCategory::Voice, true, &label, SoundSource::Character, &detail) {
                    trace.voice = format!("voice {key}");
                }
            }
            ClipEventKind::Sound { cue, stop_at_end } => {
                let pick = tracker.next_random();
                let label = format!("sound {current} t={:.2}{}", event.time, if *stop_at_end { " (stops with the clip)" } else { "" });
                // Sounds that stop with the clip share the voice line lifetime (emote end).
                let detail = format!("emote sound ({current})");
                play_cue(&mut commands, &engines, &mut library, &audio, &mut pending, &mut monitor, now, cue, pick, SoundCategory::Effects, *stop_at_end, &label, SoundSource::Character, &detail);
            }
        }
    }
}

fn after_last_mod(e: f32, last_mod: f32, inclusive: bool) -> bool {
    if inclusive { e >= last_mod } else { e > last_mod }
}

/// When the emote that started voice lines ends (movement cancel, Stop, natural end), the
/// lines stop with it.
fn voice_cleanup(mut commands: Commands, animators: Query<&CharacterAnimator, With<CharacterRig>>, lines: Query<Entity, With<VoiceLine>>, mut had_emote: Local<bool>) {
    let has = animators.single().is_ok_and(|a| a.emote.is_some());
    if *had_emote && !has && !lines.is_empty() {
        trace_audio!("voice stop ({} lines, emote ended)", lines.iter().count());
        for e in &lines {
            commands.entity(e).despawn();
        }
    }
    *had_emote = has;
}

fn expire_one_shots(mut commands: Commands, time: Res<Time>, mut shots: Query<(Entity, &mut OneShot)>) {
    for (entity, mut shot) in &mut shots {
        shot.ttl -= time.delta_secs();
        if shot.ttl <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

/// `M` toggles "mute all" through the hub (so it is persisted like the settings window).
fn mute_hotkey(keys: Res<ButtonInput<KeyCode>>, ui_focus: Res<crate::app::UiFocus>, mut hub: ResMut<HubState>, mut audio: ResMut<Audio>) {
    if ui_focus.keyboard || !keys.just_pressed(KeyCode::KeyM) {
        return;
    }
    if let ffl_hub::HubEvent::AudioChanged(a) = hub.0.toggle_mute() {
        info!("audio {}", if a.mute { "muted (M)" } else { "unmuted (M)" });
        audio.current = a;
        audio.dirty = true;
    }
}

/// Push gain × category volume × master (× mute) into every live sink, each frame.
fn mix_sinks(audio: Res<Audio>, mut trace: ResMut<AudioTrace>, mut sinks: Query<(&Bus, &Gain, Option<&ClipGain>, &mut AudioSink)>) {
    let mut n = 0;
    for (bus, gain, clip_gain, mut sink) in &mut sinks {
        let v = audio.mix(bus.0, gain.0 * clip_gain.map(|c| c.0).unwrap_or(1.0));
        if (sink.volume().to_linear() - v).abs() > 1e-4 {
            sink.set_volume(Volume::Linear(v));
        }
        n += 1;
    }
    trace.sinks = n;
}

/// Fill the hub's sound monitor: live loops (labelled sinks) first, then one-shots from the
/// last [`MONITOR_LINGER`] seconds, character before world.
fn update_monitor(
    time: Res<Time>,
    audio: Res<Audio>,
    weather: Res<Weather>,
    request: Res<WorldRequest>,
    mut monitor: ResMut<SoundMonitorState>,
    sinks: Query<(&Bus, &Gain, Option<&ClipGain>, &SoundLabel, &AudioSink)>,
) {
    let now = time.elapsed_secs();
    let mut entries: Vec<SoundActivity> = Vec::new();
    for (bus, gain, clip_gain, label, sink) in &sinks {
        if sink.is_paused() {
            continue;
        }
        let level = audio.mix(bus.0, gain.0 * clip_gain.map(|c| c.0).unwrap_or(1.0));
        // Emitters idling within their hysteresis band are inaudible: leave them out.
        if level < 0.01 && !audio.current.mute {
            continue;
        }
        entries.push(SoundActivity {
            source: label.source,
            engine: request.engine.clone(),
            category: bus.0,
            name: label.name.clone(),
            detail: label.detail.clone(),
            level,
            ended_for: 0.0,
        });
    }
    monitor.shots.retain(|(_, ends)| now - *ends < MONITOR_LINGER);
    for (shot, ends) in monitor.shots.iter().rev() {
        let mut e = shot.clone();
        e.ended_for = (now - *ends).max(0.0);
        e.level = if e.ended_for > 0.0 { 0.0 } else { audio.mix(e.category, 1.0).min(1.0) * shot.level.min(1.0) };
        entries.push(e);
    }
    let rank = |e: &SoundActivity| (e.ended_for > 0.0, e.source != SoundSource::Character);
    entries.sort_by(|a, b| rank(a).cmp(&rank(b)));
    // Emitters can outnumber the space: the loudest first.
    entries.truncate(14);
    let view = &mut monitor.view;
    view.entries = entries;
    view.ground = weather.ground().label().to_string();
    view.world_engine = request.engine.clone();
    view.character_engine = request.character.as_ref().map(|c| c.engine.clone()).unwrap_or_else(|| "-".into());
    view.muted = audio.current.mute;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_name("ff14:scd:sound/foot/foot/fs_stone_f_f_boots.scd#4"), "fs_stone_f_f_boots #4");
        assert_eq!(key_name("ff14:scd:music/ffxiv/BGM_Field_Housing_Day.scd@pack#0"), "BGM_Field_Housing_Day #0 @pack");
        assert_eq!(key_name("ff11:bgw:109"), "bgw 109");
        assert_eq!(key_engine("ff11:bgw:109"), "ff11");
    }

    fn clip(samples: &[i16], channels: u16, loop_range: Option<(u64, u64)>, looped: bool) -> PcmClip {
        PcmClip { rate: 8, channels, samples: Arc::from(samples), loop_range, looped }
    }

    #[test]
    fn decoder_plays_once() {
        let d: Vec<f32> = clip(&[0, 16384, -32768, 32767], 1, None, false).decoder().collect();
        assert_eq!(d.len(), 4);
        assert!((d[1] - 0.5).abs() < 1e-6);
        assert!((d[2] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn decoder_loops_sample_exactly() {
        // Frames: 0 1 2 3 4 5, loop [2, 5): plays 0 1 2 3 4 then 2 3 4 2 3 4 ...
        let c = clip(&[0, 1, 2, 3, 4, 5], 1, Some((2, 5)), true);
        let d: Vec<i16> = c.decoder().take(11).map(|s| (s * 32768.0).round() as i16).collect();
        assert_eq!(d, vec![0, 1, 2, 3, 4, 2, 3, 4, 2, 3, 4]);
        assert_eq!(c.decoder().total_duration(), None);
    }

    #[test]
    fn decoder_loops_stereo_frames() {
        let c = clip(&[0, 0, 1, 1, 2, 2, 3, 3], 2, Some((1, 3)), true);
        let d: Vec<i16> = c.decoder().take(10).map(|s| (s * 32768.0).round() as i16).collect();
        assert_eq!(d, vec![0, 0, 1, 1, 2, 2, 1, 1, 2, 2]);
        assert_eq!(c.decoder().channels().get(), 2);
    }

    #[test]
    fn falloff() {
        assert_eq!(smoothstep(10.0, 20.0, 5.0), 0.0);
        assert_eq!(smoothstep(10.0, 20.0, 25.0), 1.0);
        assert!((smoothstep(10.0, 20.0, 15.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn clock_text() {
        let c = WorldClock { spec: None, hours: 17.983, fixed: true };
        assert_eq!(c.hhmm(), "17:58");
    }
}
