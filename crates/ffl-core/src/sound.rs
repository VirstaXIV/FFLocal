//! Sound contracts: decoded audio handed over by engines, music sets, ambient emitters,
//! footstep and voice banks, and the game clock that drives day/night music.
//!
//! Engines decode their own formats (FF14 SCD, FF11 BGW/SPW) and hand the runtime plain PCM;
//! the runtime never sees a game codec. Sound *keys* are opaque engine strings
//! (`ff14:scd:<path>#<entry>`, `ff11:bgw:<id>`) resolved lazily through
//! [`crate::Engine::load_sound`], like action clips.

use crate::content::Provenance;

/// Mixer category; the settings hold one volume per category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundCategory {
    Music,
    Ambience,
    Footsteps,
    Voice,
    Effects,
}

impl SoundCategory {
    pub const ALL: [SoundCategory; 5] = [
        SoundCategory::Music,
        SoundCategory::Ambience,
        SoundCategory::Footsteps,
        SoundCategory::Voice,
        SoundCategory::Effects,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SoundCategory::Music => "Music",
            SoundCategory::Ambience => "Ambience",
            SoundCategory::Footsteps => "Footsteps",
            SoundCategory::Voice => "Voice",
            SoundCategory::Effects => "Effects",
        }
    }
}

/// Audio payload of a [`SoundData`].
#[derive(Clone)]
pub enum SoundEncoding {
    /// Interleaved signed 16-bit PCM, `channels` samples per frame.
    Pcm16(Vec<i16>),
    /// A complete Ogg Vorbis file; the runtime decodes it. Reserved for engines that cannot
    /// decode themselves; no bundled engine emits it.
    OggVorbis(Vec<u8>),
}

impl std::fmt::Debug for SoundEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoundEncoding::Pcm16(s) => write!(f, "Pcm16({} samples)", s.len()),
            SoundEncoding::OggVorbis(b) => write!(f, "OggVorbis({} bytes)", b.len()),
        }
    }
}

/// One decoded sound.
#[derive(Debug, Clone)]
pub struct SoundData {
    pub key: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub encoding: SoundEncoding,
    /// Total sample frames.
    pub frames: u64,
    /// Loop region in sample frames `[start, end)`; `None` = play once.
    pub loop_range: Option<(u64, u64)>,
    /// The game's own playback volume for this sound (FF14 sound programs carry 0.3–0.85);
    /// the runtime multiplies it into the mix so game sounds keep their relative levels.
    pub gain: f32,
    pub provenance: Provenance,
}

impl SoundData {
    pub fn pcm16(key: &str, sample_rate: u32, channels: u16, samples: Vec<i16>, loop_range: Option<(u64, u64)>, provenance: Provenance) -> Self {
        let frames = samples.len() as u64 / channels.max(1) as u64;
        let loop_range = loop_range.filter(|(s, e)| e > s && *s < frames).map(|(s, e)| (s, e.min(frames)));
        Self { key: key.to_string(), sample_rate, channels, encoding: SoundEncoding::Pcm16(samples), frames, loop_range, gain: 1.0, provenance }
    }

    pub fn duration_secs(&self) -> f32 {
        self.frames as f32 / self.sample_rate.max(1) as f32
    }

    /// RIFF/WAVE bytes (PCM payloads only) for CLI dumps and tests.
    pub fn wav_bytes(&self) -> Option<Vec<u8>> {
        match &self.encoding {
            SoundEncoding::Pcm16(s) => Some(wav_bytes(self.sample_rate, self.channels, s)),
            SoundEncoding::OggVorbis(_) => None,
        }
    }
}

/// 16-bit PCM WAV file bytes (44-byte header + samples).
pub fn wav_bytes(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let block_align = channels * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// A one-shot bank: the runtime plays one of `variations` (sound keys) picked at random.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SoundCue {
    pub variations: Vec<String>,
    /// Linear gain applied on top of the category volume.
    pub gain: f32,
}

impl SoundCue {
    pub fn new(variations: Vec<String>) -> Self {
        Self { variations, gain: 1.0 }
    }
}

/// Ground material under a foot. Engines classify their own surfaces into these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SurfaceKind {
    #[default]
    Unknown,
    Dirt,
    Grass,
    Sand,
    Stone,
    Wood,
    Metal,
    Gravel,
    Leaf,
    Powder,
    Carpet,
    Snow,
    Water,
    /// Grating / netting (FF14 `fs_mesh_ex`).
    Mesh,
}

impl SurfaceKind {
    pub const ALL: [SurfaceKind; 14] = [
        SurfaceKind::Unknown,
        SurfaceKind::Dirt,
        SurfaceKind::Grass,
        SurfaceKind::Sand,
        SurfaceKind::Stone,
        SurfaceKind::Wood,
        SurfaceKind::Metal,
        SurfaceKind::Gravel,
        SurfaceKind::Leaf,
        SurfaceKind::Powder,
        SurfaceKind::Carpet,
        SurfaceKind::Snow,
        SurfaceKind::Water,
        SurfaceKind::Mesh,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SurfaceKind::Unknown => "unknown",
            SurfaceKind::Dirt => "dirt",
            SurfaceKind::Grass => "grass",
            SurfaceKind::Sand => "sand",
            SurfaceKind::Stone => "stone",
            SurfaceKind::Wood => "wood",
            SurfaceKind::Metal => "metal",
            SurfaceKind::Gravel => "gravel",
            SurfaceKind::Leaf => "leaf",
            SurfaceKind::Powder => "powder",
            SurfaceKind::Carpet => "carpet",
            SurfaceKind::Snow => "snow",
            SurfaceKind::Water => "water",
            SurfaceKind::Mesh => "mesh",
        }
    }

    /// Keyword heuristic over a material, texture or model name. Interim until engines expose
    /// their real collision materials; shared so both engines classify the same way.
    pub fn from_name(name: &str) -> Option<SurfaceKind> {
        const TABLE: &[(&str, SurfaceKind)] = &[
            ("gravel", SurfaceKind::Gravel),
            ("pebble", SurfaceKind::Gravel),
            ("grass", SurfaceKind::Grass),
            ("gras", SurfaceKind::Grass),
            ("lawn", SurfaceKind::Grass),
            ("moss", SurfaceKind::Grass),
            ("sand", SurfaceKind::Sand),
            ("beach", SurfaceKind::Sand),
            ("dune", SurfaceKind::Sand),
            ("cobble", SurfaceKind::Stone),
            ("stone", SurfaceKind::Stone),
            ("rock", SurfaceKind::Stone),
            ("brick", SurfaceKind::Stone),
            ("pave", SurfaceKind::Stone),
            ("tile", SurfaceKind::Stone),
            ("marble", SurfaceKind::Stone),
            ("cliff", SurfaceKind::Stone),
            ("plank", SurfaceKind::Wood),
            ("wood", SurfaceKind::Wood),
            ("board", SurfaceKind::Wood),
            ("timber", SurfaceKind::Wood),
            ("deck", SurfaceKind::Wood),
            ("log", SurfaceKind::Wood),
            ("metal", SurfaceKind::Metal),
            ("iron", SurfaceKind::Metal),
            ("steel", SurfaceKind::Metal),
            ("grate", SurfaceKind::Metal),
            ("leaf", SurfaceKind::Leaf),
            ("leaves", SurfaceKind::Leaf),
            ("snow", SurfaceKind::Snow),
            ("ice", SurfaceKind::Snow),
            ("carpet", SurfaceKind::Carpet),
            ("rug", SurfaceKind::Carpet),
            ("cloth", SurfaceKind::Carpet),
            ("water", SurfaceKind::Water),
            ("river", SurfaceKind::Water),
            ("sea", SurfaceKind::Water),
            ("powder", SurfaceKind::Powder),
            ("ash", SurfaceKind::Powder),
            ("dust", SurfaceKind::Powder),
            ("mesh", SurfaceKind::Mesh),
            ("net", SurfaceKind::Mesh),
            ("dirt", SurfaceKind::Dirt),
            ("soil", SurfaceKind::Dirt),
            ("mud", SurfaceKind::Dirt),
            ("ground", SurfaceKind::Dirt),
            ("earth", SurfaceKind::Dirt),
            ("road", SurfaceKind::Dirt),
            // Romanised Japanese, as FFXI names its textures (jimen = ground, shiba = lawn,
            // kusa = grass, suna = sand, ishi/iwa = stone, renga = brick, ki/moku = wood,
            // tetsu = iron, mizu/umi/kawa/nami = water, yuki = snow, tsuchi/doro = soil).
            ("jimen", SurfaceKind::Dirt),
            ("_jim", SurfaceKind::Dirt),
            ("tsuchi", SurfaceKind::Dirt),
            ("doro", SurfaceKind::Dirt),
            ("michi", SurfaceKind::Dirt),
            ("shiba", SurfaceKind::Grass),
            ("siba", SurfaceKind::Grass),
            ("kusa", SurfaceKind::Grass),
            ("suna", SurfaceKind::Sand),
            ("ishi", SurfaceKind::Stone),
            ("isi", SurfaceKind::Stone),
            ("iwa", SurfaceKind::Stone),
            ("renga", SurfaceKind::Stone),
            ("yama", SurfaceKind::Stone),
            ("gake", SurfaceKind::Stone),
            ("seki", SurfaceKind::Stone),
            ("moku", SurfaceKind::Wood),
            ("hashi", SurfaceKind::Wood),
            ("tetsu", SurfaceKind::Metal),
            ("mizu", SurfaceKind::Water),
            ("umi", SurfaceKind::Water),
            ("kawa", SurfaceKind::Water),
            ("nami", SurfaceKind::Water),
            ("ike", SurfaceKind::Water),
            ("yuki", SurfaceKind::Snow),
            ("koori", SurfaceKind::Snow),
        ];
        let lower = name.to_ascii_lowercase();
        // Longest keyword that occurs wins, so "gravel" beats "grave"/"rock" fragments.
        TABLE
            .iter()
            .filter(|(k, _)| lower.contains(k))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, s)| *s)
    }
}

/// The tracks a map plays. Keys are sound keys for [`crate::Engine::load_sound`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MusicSet {
    pub day: Option<String>,
    pub night: Option<String>,
    pub battle: Option<String>,
    pub daybreak: Option<String>,
    /// Engine-specific extra slots as (label, key), e.g. FF11 `battle_party`.
    pub extra: Vec<(String, String)>,
    /// Crossfade timings in seconds.
    pub fade_in: f32,
    pub fade_out: f32,
    /// False when the game resumes the track where it stopped after a short absence.
    pub restart_on_return: bool,
    /// Human-readable notes (unsupported codecs, silence, ...).
    pub notes: Vec<String>,
}

impl MusicSet {
    pub fn is_empty(&self) -> bool {
        self.day.is_none() && self.night.is_none() && self.battle.is_none() && self.daybreak.is_none() && self.extra.is_empty()
    }

    /// The key for a clock slot. `None` means silence: engines fill every slot they know
    /// (FF14 field zones really are silent at night), so there is no day/night fallback; only
    /// daybreak falls back to the night track.
    pub fn for_slot(&self, slot: MusicSlot) -> Option<&str> {
        match slot {
            MusicSlot::Day => self.day.as_deref(),
            MusicSlot::Night => self.night.as_deref(),
            MusicSlot::Daybreak => self.daybreak.as_deref().or(self.night.as_deref()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegionShape {
    #[default]
    Box,
    /// Vertical cylinder: `half_extents[0]` is the radius, `half_extents[1]` the half height.
    Cylinder,
    /// `half_extents[0]` is the radius.
    Sphere,
}

/// A sub-area of a map with its own music (FF14 `MapRange` volumes). The runtime plays the
/// region containing the listener (highest priority wins) instead of the map's music.
#[derive(Debug, Clone, PartialEq)]
pub struct MusicRegion {
    pub name: String,
    pub shape: RegionShape,
    /// Centre, rotation (quaternion x, y, z, w) and half extents, in world space.
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub half_extents: [f32; 3],
    pub priority: i32,
    pub music: MusicSet,
}

impl MusicRegion {
    /// Whether a world point lies inside the (oriented) volume.
    pub fn contains(&self, p: [f32; 3]) -> bool {
        let d = [p[0] - self.position[0], p[1] - self.position[1], p[2] - self.position[2]];
        // Rotate the offset by the inverse quaternion.
        let [x, y, z, w] = self.rotation;
        let (qx, qy, qz, qw) = (-x, -y, -z, w);
        // v' = q * v * q^-1 with q = conjugate
        let ix = qw * d[0] + qy * d[2] - qz * d[1];
        let iy = qw * d[1] + qz * d[0] - qx * d[2];
        let iz = qw * d[2] + qx * d[1] - qy * d[0];
        let iw = -qx * d[0] - qy * d[1] - qz * d[2];
        let lx = ix * qw + iw * -qx + iy * -qz - iz * -qy;
        let ly = iy * qw + iw * -qy + iz * -qx - ix * -qz;
        let lz = iz * qw + iw * -qz + ix * -qy - iy * -qx;
        let [hx, hy, hz] = self.half_extents;
        match self.shape {
            RegionShape::Box => lx.abs() <= hx && ly.abs() <= hy && lz.abs() <= hz,
            RegionShape::Cylinder => ly.abs() <= hy && lx * lx + lz * lz <= hx * hx,
            RegionShape::Sphere => lx * lx + ly * ly + lz * lz <= hx * hx,
        }
    }
}

/// A placed ambient sound: a looping bed (`key`, may be empty) and/or intermittent one-shots
/// (`spots`, played every `spot_interval` seconds, picked at random).
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientEmitter {
    pub name: String,
    /// Looping bed sound key; empty when the emitter only plays spots.
    pub key: String,
    pub spots: Vec<String>,
    /// Seconds between spots, (min, max).
    pub spot_interval: (f32, f32),
    pub position: [f32; 3],
    /// False = zone-wide bed with no distance attenuation.
    pub positional: bool,
    /// Full volume inside this radius (horizontal), silent beyond `max_distance`.
    pub inner_radius: f32,
    pub max_distance: f32,
    /// Vertical half-extent of the emitter volume.
    pub height: f32,
    pub gain: f32,
}

/// How the character moves when a foot lands. FF14 carries no sprint distinction in its
/// data (sprinting steps are run steps); `Land` is the touchdown after a jump or fall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gait {
    Walk,
    Run,
    Land,
}

impl Gait {
    pub fn label(self) -> &'static str {
        match self {
            Gait::Walk => "walk",
            Gait::Run => "run",
            Gait::Land => "land",
        }
    }
}

/// State of the ground under the character: dry, or wet from rain. Decided by the world's
/// weather (the runtime's, never a zone's: zones only tag their surfaces), `Dry` when a world
/// has no weather.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GroundCondition {
    #[default]
    Dry,
    Wet,
}

impl GroundCondition {
    pub fn label(self) -> &'static str {
        match self {
            GroundCondition::Dry => "dry",
            GroundCondition::Wet => "wet",
        }
    }
}

/// One footstep bank: the sounds for a surface, a gait and a ground condition.
#[derive(Debug, Clone, PartialEq)]
pub struct FootstepCue {
    pub surface: SurfaceKind,
    pub gait: Gait,
    pub condition: GroundCondition,
    pub cue: SoundCue,
}

/// The footstep map of a character: surface × gait × ground condition → cue. The character's
/// engine builds it once from the character (gender, footwear, ...); the world only supplies
/// the surface (from its collision or materials) and the condition (weather).
#[derive(Debug, Clone, Default)]
pub struct FootstepSet {
    pub cues: Vec<FootstepCue>,
    /// Used when the surface is `Unknown` or has no cue.
    pub fallback_surface: SurfaceKind,
}

impl FootstepSet {
    pub fn push(&mut self, surface: SurfaceKind, gait: Gait, condition: GroundCondition, cue: SoundCue) {
        self.cues.push(FootstepCue { surface, gait, condition, cue });
    }

    /// Exact (surface, gait, condition); a wet request falls back to the dry bank of the same
    /// surface and gait; then the fallback surface (same gait, wet then dry); then any gait of
    /// the surface; then any gait of the fallback surface.
    pub fn cue(&self, surface: SurfaceKind, gait: Gait, condition: GroundCondition) -> Option<&SoundCue> {
        let exact = |s: SurfaceKind, g: Gait, c: GroundCondition| self.cues.iter().find(|f| f.surface == s && f.gait == g && f.condition == c).map(|f| &f.cue);
        let dry = GroundCondition::Dry;
        let fb = self.fallback_surface;
        exact(surface, gait, condition)
            .or_else(|| (condition != dry).then(|| exact(surface, gait, dry)).flatten())
            .or_else(|| exact(fb, gait, condition))
            .or_else(|| (condition != dry).then(|| exact(fb, gait, dry)).flatten())
            .or_else(|| self.cues.iter().find(|f| f.surface == surface && f.condition == condition).map(|f| &f.cue))
            .or_else(|| self.cues.iter().find(|f| f.surface == surface).map(|f| &f.cue))
            .or_else(|| self.cues.iter().find(|f| f.surface == fb).map(|f| &f.cue))
    }

    /// Every sound key in the set (for pre-loading).
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.cues.iter().flat_map(|f| f.cue.variations.iter().map(String::as_str))
    }

    /// The sound keys of one ground condition (what a world in that weather will play).
    pub fn keys_for(&self, condition: GroundCondition) -> impl Iterator<Item = &str> {
        self.cues.iter().filter(move |f| f.condition == condition).flat_map(|f| f.cue.variations.iter().map(String::as_str))
    }

    /// (surfaces, gaits, conditions) covered, for status lines.
    pub fn summary(&self) -> String {
        let mut surfaces: Vec<&str> = self.cues.iter().map(|f| f.surface.label()).collect();
        surfaces.dedup();
        let wet = self.cues.iter().filter(|f| f.condition == GroundCondition::Wet).count();
        format!("{} banks over {} surfaces{}", self.cues.len(), surfaces.len(), if wet > 0 { format!(", {wet} wet") } else { String::new() })
    }
}

/// Voice lines of a character (emote laughs, cheers, ...), keyed by the engine's line id.
#[derive(Debug, Clone, Default)]
pub struct VoiceSet {
    pub name: String,
    pub lines: Vec<(u32, SoundCue)>,
}

impl VoiceSet {
    pub fn line(&self, id: u32) -> Option<&SoundCue> {
        self.lines.iter().find(|(l, _)| *l == id).map(|(_, c)| c)
    }
}

/// Which music slot a time of day selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicSlot {
    Day,
    Night,
    Daybreak,
}

/// A game clock. Each engine defines its own (Eorzea time, Vana'diel time, ...); the runtime
/// only advances it and asks which slot applies.
#[derive(Debug, Clone, PartialEq)]
pub struct ClockSpec {
    /// Display name, e.g. "Eorzea".
    pub name: String,
    /// Unix seconds at which the game clock reads 00:00.
    pub epoch_unix: i64,
    /// Game seconds per real second.
    pub rate: f32,
    /// Daytime range in game hours `[start, end)`.
    pub day: (f32, f32),
    /// Daybreak range in game hours, when the game has a separate track for it.
    pub daybreak: Option<(f32, f32)>,
}

impl ClockSpec {
    /// Game hour of the day (0..24) at a wall-clock instant.
    pub fn hours(&self, unix_secs: f64) -> f32 {
        let game_secs = (unix_secs - self.epoch_unix as f64) * self.rate as f64;
        let day = game_secs.rem_euclid(86_400.0);
        (day / 3600.0) as f32
    }

    pub fn slot(&self, hours: f32) -> MusicSlot {
        let h = hours.rem_euclid(24.0);
        if let Some((s, e)) = self.daybreak
            && s <= h
            && h < e
        {
            return MusicSlot::Daybreak;
        }
        if self.day.0 <= h && h < self.day.1 { MusicSlot::Day } else { MusicSlot::Night }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_keywords() {
        assert_eq!(SurfaceKind::from_name("s1h1_a1_grass01"), Some(SurfaceKind::Grass));
        assert_eq!(SurfaceKind::from_name("bg/sea_s1/GRAVEL_road"), Some(SurfaceKind::Gravel));
        assert_eq!(SurfaceKind::from_name("wood_plank"), Some(SurfaceKind::Wood));
        assert_eq!(SurfaceKind::from_name("s1h1_a1_flr01"), None);
    }

    #[test]
    fn wav_header() {
        let w = wav_bytes(44100, 2, &[0, 1, -1, 2]);
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 44100);
        assert_eq!(u16::from_le_bytes(w[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 8);
    }

    #[test]
    fn footstep_fallbacks() {
        use GroundCondition::{Dry, Wet};
        let cue = |k: &str| SoundCue::new(vec![k.into()]);
        let mut set = FootstepSet { cues: Vec::new(), fallback_surface: SurfaceKind::Dirt };
        set.push(SurfaceKind::Grass, Gait::Walk, Dry, cue("gw"));
        set.push(SurfaceKind::Grass, Gait::Run, Dry, cue("gr"));
        set.push(SurfaceKind::Grass, Gait::Run, Wet, cue("grw"));
        set.push(SurfaceKind::Dirt, Gait::Walk, Dry, cue("dw"));
        set.push(SurfaceKind::Dirt, Gait::Walk, Wet, cue("dww"));
        set.push(SurfaceKind::Stone, Gait::Land, Dry, cue("sl"));
        let pick = |s, g, c| set.cue(s, g, c).unwrap().variations[0].clone();
        assert_eq!(pick(SurfaceKind::Grass, Gait::Run, Dry), "gr");
        assert_eq!(pick(SurfaceKind::Grass, Gait::Run, Wet), "grw");
        // Wet without a wet bank: the dry bank of the same surface and gait.
        assert_eq!(pick(SurfaceKind::Grass, Gait::Walk, Wet), "gw");
        // Unknown surface: the fallback surface, honouring the condition.
        assert_eq!(pick(SurfaceKind::Sand, Gait::Walk, Dry), "dw");
        assert_eq!(pick(SurfaceKind::Sand, Gait::Walk, Wet), "dww");
        assert_eq!(pick(SurfaceKind::Stone, Gait::Walk, Dry), "dw");
        assert_eq!(pick(SurfaceKind::Stone, Gait::Land, Dry), "sl");
        // No landing bank for grass: any grass bank before the fallback surface.
        assert_eq!(pick(SurfaceKind::Grass, Gait::Land, Dry), "gw");
        assert_eq!(set.keys_for(Wet).count(), 2);
        assert_eq!(set.summary(), "6 banks over 3 surfaces, 2 wet");
    }

    #[test]
    fn region_contains_rotated_box() {
        // 90° about Y: local X maps to world -Z.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let r = MusicRegion { name: "r".into(), shape: RegionShape::Box, position: [10.0, 0.0, 0.0], rotation: [0.0, s, 0.0, s], half_extents: [5.0, 1.0, 1.0], priority: 0, music: MusicSet::default() };
        assert!(r.contains([10.0, 0.0, -4.0]));
        assert!(r.contains([10.0, 0.0, 4.0]));
        assert!(!r.contains([14.0, 0.0, 0.0]));
        assert!(!r.contains([10.0, 2.0, 0.0]));
        let c = MusicRegion { shape: RegionShape::Cylinder, rotation: [0.0, 0.0, 0.0, 1.0], half_extents: [5.0, 1.0, 5.0], ..r.clone() };
        assert!(c.contains([13.0, 0.5, 3.0]));
        assert!(!c.contains([14.0, 0.0, 4.0]));
    }

    #[test]
    fn slots_do_not_fall_back_to_day() {
        let set = MusicSet { day: Some("d".into()), ..Default::default() };
        assert_eq!(set.for_slot(MusicSlot::Day), Some("d"));
        assert_eq!(set.for_slot(MusicSlot::Night), None);
        let set = MusicSet { night: Some("n".into()), ..Default::default() };
        assert_eq!(set.for_slot(MusicSlot::Daybreak), Some("n"));
    }

    #[test]
    fn clock_slots() {
        let spec = ClockSpec { name: "Eorzea".into(), epoch_unix: 0, rate: 3600.0 / 175.0, day: (6.0, 18.0), daybreak: Some((5.0, 6.0)) };
        assert_eq!(spec.slot(12.0), MusicSlot::Day);
        assert_eq!(spec.slot(18.0), MusicSlot::Night);
        assert_eq!(spec.slot(5.5), MusicSlot::Daybreak);
        // 175 real seconds = one Eorzea hour.
        assert!((spec.hours(175.0) - 1.0).abs() < 1e-4);
    }
}
