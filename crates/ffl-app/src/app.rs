//! App-wide state: registered engines, runtime options, the current world request.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use ffl_core::{CharacterPreset, Engine};
use ffl_hub::{AudioSettings, GraphicsSettings, Hub};

/// Top-level application state.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    #[default]
    Launcher,
    World,
}

/// All registered engines (available or not).
#[derive(Resource, Clone)]
pub struct Engines(pub Vec<Arc<dyn Engine>>);

impl Engines {
    pub fn get(&self, id: &str) -> Option<Arc<dyn Engine>> {
        self.0.iter().find(|e| e.info().id == id).cloned()
    }

    /// Replace the engine with `id` (or append it).
    pub fn replace(&mut self, engine: Arc<dyn Engine>) {
        let id = engine.info().id;
        match self.0.iter().position(|e| e.info().id == id) {
            Some(i) => self.0[i] = engine,
            None => self.0.push(engine),
        }
    }
}

/// Opens an engine from a game install (the Games window reopens engines at runtime).
/// `main.rs` implements one per engine crate; the rest of the runtime only sees this.
pub trait EngineFactory: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    /// Shaders the engine's materials use (registered at startup, install or not).
    fn shaders(&self) -> Vec<ffl_core::ShaderDef>;
    /// Installs found in the usual places of this machine.
    fn scan(&self) -> Vec<ffl_core::InstallCandidate>;
    /// Open from an install directory, or search when `None`, reading companion tooling
    /// (plugin data) from `plugins` or the usual places. A failure still yields an engine —
    /// one that reports itself unavailable with the reason.
    fn open(&self, path: Option<&std::path::Path>, plugins: Option<&std::path::Path>) -> Arc<dyn Engine>;
}

#[derive(Resource)]
pub struct EngineFactories(pub Vec<Box<dyn EngineFactory>>);

impl EngineFactories {
    pub fn get(&self, id: &str) -> Option<&dyn EngineFactory> {
        self.0.iter().find(|f| f.id() == id).map(|f| f.as_ref())
    }
}

/// The stand-in for a game switched off in Games (or the launcher): listed, loads nothing.
pub fn disabled_engine(factory: &dyn EngineFactory) -> Arc<dyn Engine> {
    Arc::new(MissingEngine {
        id: factory.id().into(),
        name: factory.name().into(),
        error: "switched off in Games".into(),
        shaders: factory.shaders(),
    })
}

/// Open an engine the way the app and the hub do: a switched-off game becomes a
/// [`disabled_engine`], otherwise the factory opens `path` (or searches) and the engine gets
/// its globally enabled mods.
pub fn open_engine(factory: &dyn EngineFactory, settings: &ffl_hub::Settings, path: Option<&std::path::Path>) -> Arc<dyn Engine> {
    if !settings.game_enabled(factory.id()) {
        return disabled_engine(factory);
    }
    let plugins = settings.plugins_path(factory.id());
    let engine = factory.open(path, plugins.as_deref());
    if let Some(ids) = settings.mods.get(factory.id()) {
        engine.set_enabled_mods(ids);
    }
    engine
}

/// An engine whose game could not be opened: listed (so the Games window can fix it), fails
/// everything else.
pub struct MissingEngine {
    pub id: String,
    pub name: String,
    pub error: String,
    pub shaders: Vec<ffl_core::ShaderDef>,
}

impl Engine for MissingEngine {
    fn info(&self) -> ffl_core::EngineInfo {
        ffl_core::EngineInfo {
            plugins: String::new(),
            id: self.id.clone(),
            name: self.name.clone(),
            version: String::new(),
            available: false,
            detail: self.error.clone(),
            path: None,
        }
    }
    fn content_policy(&self) -> ffl_core::ContentPolicy {
        ffl_core::ContentPolicy::LocalOnly
    }
    fn shaders(&self) -> Vec<ffl_core::ShaderDef> {
        self.shaders.clone()
    }
    fn list_maps(&self) -> anyhow::Result<Vec<ffl_core::MapInfo>> {
        Ok(Vec::new())
    }
    fn load_scene(&self, _map_id: &str) -> anyhow::Result<ffl_core::Scene> {
        anyhow::bail!("{}: {}", self.name, self.error)
    }
    fn load_model(&self, _key: &str) -> anyhow::Result<ffl_core::ModelData> {
        anyhow::bail!("{}: {}", self.name, self.error)
    }
    fn preset_fields(&self, _preset: Option<&CharacterPreset>) -> anyhow::Result<Vec<ffl_core::PresetField>> {
        Ok(Vec::new())
    }
    fn default_presets(&self) -> anyhow::Result<Vec<CharacterPreset>> {
        Ok(Vec::new())
    }
    fn load_character(&self, _preset: &CharacterPreset) -> anyhow::Result<ffl_core::CharacterModel> {
        anyhow::bail!("{}: {}", self.name, self.error)
    }
}

/// What the runtime should load when entering [`AppState::World`].
#[derive(Resource, Clone, Debug, Default)]
pub struct WorldRequest {
    pub engine: String,
    pub map: String,
    pub map_name: String,
    pub character: Option<CharacterPreset>,
}

impl From<ffl_hub::WorldRequest> for WorldRequest {
    fn from(r: ffl_hub::WorldRequest) -> Self {
        WorldRequest {
            engine: r.engine,
            map: r.map,
            map_name: r.map_name,
            character: r.character,
        }
    }
}

/// The hub model (presets, worlds, settings); the launcher and settings windows are its views.
#[derive(Resource)]
pub struct HubState(pub Hub);

/// Graphics settings as last applied to the runtime (see `launcher::apply_graphics`).
#[derive(Resource, Clone, Debug)]
pub struct Graphics {
    pub current: GraphicsSettings,
    /// Set when `current` changed and the runtime has not applied it yet.
    pub dirty: bool,
}

impl Graphics {
    /// Distance at which an object of `radius` metres stops rendering: proportional to its size
    /// (detail distance), never beyond the render distance.
    pub fn cull_distance(&self, radius: f32) -> f32 {
        let by_size = (radius.max(0.05) * self.current.detail_distance).max(30.0);
        by_size.min(self.current.render_distance)
    }
}

/// Audio settings as last applied to the runtime (see `launcher::apply_audio`). The mixer
/// (`world::sound::mix_sinks`) reads `current` every frame, so most changes need no explicit
/// apply step; `dirty` is for the few that do (pausing the music).
#[derive(Resource, Clone, Debug)]
pub struct Audio {
    pub current: AudioSettings,
    pub dirty: bool,
}

impl Audio {
    /// Linear gain for a sound of `category` with its own `gain`: gain × category × master,
    /// zero while muted.
    pub fn mix(&self, category: ffl_core::SoundCategory, gain: f32) -> f32 {
        (gain * self.current.gain(category)).clamp(0.0, 4.0)
    }
}

/// Command-line / debug options that affect the runtime.
#[derive(Resource, Clone, Debug)]
pub struct RuntimeOptions {
    pub collision: bool,
    pub normal_maps: bool,
    pub screenshot: Option<PathBuf>,
    pub settle_seconds: f32,
    pub camera_pos: Option<Vec3>,
    pub spawn: Option<Vec3>,
    pub orbit: Option<Vec3>,
    pub autowalk: f32,
    /// Debug: jump once when the player reaches this age in seconds.
    pub autojump: Option<f32>,
    /// Debug: spawn this many metres above the ground and fall.
    pub drop: Option<f32>,
    /// Debug: `--autowalk` sprints.
    pub sprint: bool,
    /// Start with weapons drawn (the default is sheathed, as in the game).
    pub drawn: bool,
    /// Debug: press Z once when the player reaches this age in seconds.
    pub toggle_sheathe_at: Option<f32>,
    /// Debug: return to the launcher this many seconds after the zone finished loading.
    pub menu_at: Option<f32>,
    /// Start in fly mode.
    pub fly: bool,
    /// Debug: constant flight velocity while flying (m/s).
    pub fly_velocity: Option<Vec3>,
    /// Debug: re-enter the same world this many seconds after reaching the launcher.
    pub reenter_at: Option<f32>,
    pub anim: Option<String>,
    /// Play this action (id or name) as soon as the character is attached (verification aid).
    pub action: Option<String>,
    /// Addon overrides: `(id, enabled)` (from `--addon id=off`).
    pub addons: Vec<(String, bool)>,
    /// Debug: freeze the animator at this clip time.
    pub anim_time: Option<f32>,
    pub cull: String,
    /// Show a single model instead of a scene (verification aid).
    pub single_model: Option<String>,
    /// Take a screenshot this many seconds after start regardless of state, then exit.
    pub screenshot_at: Option<f32>,
    /// Number of frames captured from `screenshot_at`, `screenshot_interval` seconds apart.
    pub screenshot_burst: u32,
    pub screenshot_interval: f32,
    /// Start muted (`--mute`; the setting itself is not changed).
    pub mute: bool,
    /// Start the game clock at this hour of the day (`--time HH:MM`) instead of the wall clock.
    pub time: Option<f32>,
    /// Wet ground (`--wet`): footsteps use the rain banks.
    pub wet: bool,
    /// Write every decoded sound once as `<dir>/<key>.wav` (`--audio-dump`).
    pub audio_dump: Option<PathBuf>,
}

/// What egui claimed last frame: the pointer (over a panel or window — no camera zoom or
/// preview drag then) and the keyboard (a text field has focus — no movement keys then).
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct UiFocus {
    pub pointer: bool,
    pub keyboard: bool,
}

/// Marker for everything spawned while in a world; despawned on exit.
#[derive(Component)]
pub struct WorldEntity;
