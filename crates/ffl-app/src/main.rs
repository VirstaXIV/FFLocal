//! FFLocal: launcher + runtime. Engines provide scenes and characters; the runtime renders them.
//!
//! Double-clickable: no console window in Windows release builds, the log always goes to
//! `<data dir>/fflocal.log` as well as to the terminal when there is one, the Games window
//! sets up the installs, `config.toml` is read from the working directory or next to the
//! executable.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod camera;
mod convert;
mod debug_ui;
mod launcher;
mod preview;
mod world;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use avian3d::prelude::*;
use bevy::prelude::*;
use bevy::window::WindowResolution;
use bevy_egui::EguiPlugin;
use clap::Parser;
use ffl_core::{Engine, Library};
use ffl_hub::{Hub, Settings};

use crate::app::{AppState, Audio, EngineFactories, EngineFactory, Engines, Graphics, HubState, MissingEngine, RuntimeOptions, WorldRequest};

#[derive(Parser, Debug)]
#[command(name = "ffl-app", version, about)]
struct Args {
    /// FFXIV install root (or its game/ directory). Overrides env/config/launcher.ini.
    #[arg(long)]
    game_path: Option<PathBuf>,
    /// FFXI install root (the folder holding VTABLE.DAT). Overrides env/config/Steam search.
    #[arg(long)]
    ff11_path: Option<PathBuf>,
    /// Skip the launcher and load this map: "<engine>:<id>" (e.g. ff14:339).
    #[arg(long)]
    map: Option<String>,
    /// Shortcut for --map ff14:<territory>.
    #[arg(long)]
    zone: Option<u32>,
    /// Preset name or id to use with --map/--zone (default: last used or the first FF14 preset).
    #[arg(long)]
    preset: Option<String>,
    /// Skip the launcher and show a single FF14 model.
    #[arg(long)]
    mdl: Option<String>,
    /// Skip colliders and the player.
    #[arg(long)]
    no_collision: bool,
    /// Draw physics colliders.
    #[arg(long)]
    physics_debug: bool,
    /// Take a screenshot once loading settles, write it here, then exit.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    #[arg(long, default_value_t = 3.0)]
    settle: f32,
    /// Take the --screenshot this many seconds after start regardless of state (launcher shots).
    #[arg(long)]
    screenshot_at: Option<f32>,
    /// With --screenshot-at: capture this many frames (`<stem>-<n>.png`).
    #[arg(long, default_value_t = 1)]
    screenshot_burst: u32,
    /// Seconds between burst frames (0 = consecutive frames).
    #[arg(long, default_value_t = 0.0)]
    screenshot_interval: f32,
    /// Initial camera position "x,y,z".
    #[arg(long)]
    camera: Option<String>,
    /// Player spawn anchor "x,y,z".
    #[arg(long)]
    spawn: Option<String>,
    /// Initial orbit camera "yaw_deg,pitch_deg,distance".
    #[arg(long)]
    orbit: Option<String>,
    /// Debug: walk forward for N seconds after spawning.
    #[arg(long, default_value_t = 0.0)]
    autowalk: f32,
    /// Jump once this many seconds after spawning (verification aid).
    #[arg(long)]
    autojump: Option<f32>,
    /// Spawn this many metres above the ground and fall (verification aid).
    #[arg(long)]
    drop: Option<f32>,
    /// Sprint while auto-walking (verification aid).
    #[arg(long)]
    sprint: bool,
    /// Start with weapons drawn; the default is sheathed like the game (Z toggles).
    #[arg(long)]
    drawn: bool,
    /// Press Z once this many seconds after spawning (verification aid).
    #[arg(long)]
    toggle_sheathe_at: Option<f32>,
    /// Return to the launcher this many seconds after the zone loaded (verification aid).
    #[arg(long)]
    menu_at: Option<f32>,
    /// Start in fly mode (F toggles it in the world).
    #[arg(long)]
    fly: bool,
    /// Fly with this constant velocity `x,y,z` (verification aid).
    #[arg(long)]
    fly_velocity: Option<String>,
    /// Re-enter the same world this many seconds after returning to the launcher (verification aid).
    #[arg(long)]
    reenter_at: Option<f32>,
    /// Debug: force an animation clip by name.
    #[arg(long)]
    anim: Option<String>,
    /// Play an action (emote/idle pose id or name) once the character is loaded.
    #[arg(long)]
    action: Option<String>,
    /// Addon override, e.g. `--addon weapon:main=off` (repeatable).
    #[arg(long = "addon")]
    addons: Vec<String>,
    /// Debug: freeze the animation at this time (seconds).
    #[arg(long)]
    anim_time: Option<f32>,
    #[arg(long)]
    no_normal_maps: bool,
    /// Initial face culling: none, back or front.
    #[arg(long, default_value = "back")]
    cull: String,
    /// Start with all audio muted (the saved setting is left alone).
    #[arg(long)]
    mute: bool,
    /// Start the game clock (day/night music) at this time of day, e.g. 17:59.
    #[arg(long)]
    time: Option<String>,
    /// Wet ground: footsteps use the rain banks (no weather system exists yet).
    #[arg(long)]
    wet: bool,
    /// Write every decoded sound once as a WAV file into this directory (verification aid).
    #[arg(long)]
    audio_dump: Option<PathBuf>,
}

/// `HH:MM` → hours of the day.
fn parse_time(s: &Option<String>) -> Result<Option<f32>> {
    let Some(s) = s else {
        return Ok(None);
    };
    let (h, m) = s.split_once(':').ok_or_else(|| anyhow::anyhow!("--time expects HH:MM"))?;
    let h: f32 = h.trim().parse().map_err(|_| anyhow::anyhow!("--time expects HH:MM"))?;
    let m: f32 = m.trim().parse().map_err(|_| anyhow::anyhow!("--time expects HH:MM"))?;
    Ok(Some((h + m / 60.0).rem_euclid(24.0)))
}

fn parse_vec3(name: &str, s: &Option<String>) -> Result<Option<Vec3>> {
    match s {
        Some(s) => {
            let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() != 3 {
                anyhow::bail!("--{name} expects x,y,z");
            }
            Ok(Some(Vec3::new(v[0], v[1], v[2])))
        }
        None => Ok(None),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let opts = RuntimeOptions {
        collision: !args.no_collision && args.mdl.is_none(),
        normal_maps: !args.no_normal_maps,
        screenshot: args.screenshot.clone(),
        settle_seconds: args.settle,
        camera_pos: parse_vec3("camera", &args.camera)?,
        spawn: parse_vec3("spawn", &args.spawn)?,
        orbit: parse_vec3("orbit", &args.orbit)?,
        autowalk: args.autowalk,
        autojump: args.autojump,
        drop: args.drop,
        sprint: args.sprint,
        drawn: args.drawn,
        toggle_sheathe_at: args.toggle_sheathe_at,
        menu_at: args.menu_at,
        fly: args.fly,
        fly_velocity: parse_vec3("fly-velocity", &args.fly_velocity)?,
        reenter_at: args.reenter_at,
        anim: args.anim.clone(),
        action: args.action.clone(),
        addons: args
            .addons
            .iter()
            .filter_map(|a| {
                let (id, v) = a.split_once('=').unwrap_or((a.as_str(), "on"));
                Some((id.to_string(), !matches!(v.trim().to_lowercase().as_str(), "off" | "0" | "false" | "no")))
            })
            .collect(),
        anim_time: args.anim_time,
        cull: args.cull.clone(),
        single_model: args.mdl.clone(),
        screenshot_at: args.screenshot_at,
        screenshot_burst: args.screenshot_burst.max(1),
        screenshot_interval: args.screenshot_interval.max(0.0),
        mute: args.mute,
        time: parse_time(&args.time)?,
        wet: args.wet,
        audio_dump: args.audio_dump.clone(),
    };

    let library = Library::load();
    let settings = Settings::load();

    // Engines: a CLI path wins (and opens the game even when it is switched off), then the
    // install saved in Games or the launcher, then a search. A game that is missing still
    // registers, so the hub can point at it; a switched-off one registers as disabled.
    let factories: Vec<Box<dyn EngineFactory>> = vec![Box::new(Ff14Factory), Box::new(Ff11Factory)];
    let mut engines: Vec<Arc<dyn Engine>> = Vec::new();
    for f in &factories {
        let cli = match f.id() {
            ffl_ff14::ENGINE_ID => args.game_path.clone(),
            ffl_ff11::ENGINE_ID => args.ff11_path.clone(),
            _ => None,
        };
        let engine = match cli {
            Some(path) => {
                let e = f.open(Some(&path), settings.plugins_path(f.id()).as_deref());
                if let Some(ids) = settings.mods.get(f.id()) {
                    e.set_enabled_mods(ids);
                }
                e
            }
            None => crate::app::open_engine(f.as_ref(), &settings, settings.game_path(f.id()).as_deref()),
        };
        let info = engine.info();
        if !info.available {
            tracing::warn!("{} unavailable: {}", info.name, info.detail);
        }
        engines.push(engine);
    }
    let graphics = Graphics {
        current: settings.graphics.clone(),
        dirty: true,
    };
    let audio = Audio {
        current: {
            let mut a = settings.audio.clone();
            a.mute |= args.mute;
            a
        },
        dirty: true,
    };
    let direct_map = args
        .map
        .clone()
        .or_else(|| args.zone.map(|z| format!("ff14:{z}")))
        .or_else(|| args.mdl.as_ref().map(|_| "ff14:0".to_string()));
    let mut request = WorldRequest::default();
    let start_state = if let Some(spec) = direct_map {
        let (engine, map) = spec.split_once(':').unwrap_or(("ff14", spec.as_str()));
        request.engine = engine.to_string();
        request.map = map.to_string();
        request.map_name = map.to_string();
        let mut presets = library.presets.clone();
        // Engines without a saved preset offer their defaults (as the hub does).
        for e in &engines {
            if presets.iter().all(|p| p.engine != e.info().id) {
                presets.extend(e.default_presets().unwrap_or_default());
            }
        }
        request.character = match &args.preset {
            Some(p) => presets.iter().find(|x| &x.name == p || &x.id == p).cloned(),
            None => library
                .last_preset
                .as_ref()
                .and_then(|id| presets.iter().find(|x| &x.id == id).cloned())
                .or_else(|| presets.iter().find(|x| x.engine == engine).cloned()),
        };
        AppState::World
    } else {
        AppState::Launcher
    };

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "FFLocal".into(),
                    resolution: window_size(),
                    ..default()
                }),
                ..default()
            })
            .set(bevy::log::LogPlugin {
                custom_layer: log_file_layer,
                ..default()
            }),
    )
    .insert_resource(Engines(engines))
    .insert_resource(EngineFactories(factories))
    .insert_resource(HubState({
        let mut hub = Hub::new(library, settings);
        // Verification aid: open the settings window at startup.
        hub.show_settings = std::env::var_os("FFL_SHOW_SETTINGS").is_some();
        hub.show_log = std::env::var_os("FFL_SHOW_LOG").is_some();
        hub.show_games = std::env::var_os("FFL_SHOW_GAMES").is_some();
        hub
    }))
    .insert_resource(graphics)
    .insert_resource(audio)
    .insert_resource(request)
    .insert_resource(opts)
    .insert_state(start_state)
    .add_plugins(PhysicsPlugins::default())
    .add_plugins(EguiPlugin::default())
    .add_plugins((
        camera::CameraPlugin,
        world::shaders::ShadersPlugin,
        launcher::LauncherPlugin,
        preview::PreviewPlugin,
        world::zone::ZonePlugin,
        world::player::PlayerPlugin,
        world::character::CharacterPlugin,
        world::actions::ActionsPlugin,
        world::sound::SoundPlugin,
        debug_ui::DebugUiPlugin,
    ));
    if args.physics_debug {
        app.add_plugins(PhysicsDebugPlugin::default());
    }
    app.add_systems(Update, timed_screenshot);
    if std::env::var_os("FFL_TRACE_CAMERA").is_some() {
        app.add_systems(Last, trace_camera_main);
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render_app.add_systems(bevy::render::Render, trace_camera_render);
        }
    }
    app.run();
    Ok(())
}

/// A second log sink, `<data dir>/fflocal.log`, truncated per run: the record when the app
/// was started from a desktop entry and has no terminal.
fn log_file_layer(_app: &mut App) -> Option<bevy::log::BoxedLayer> {
    use tracing_subscriber::Layer;
    let dir = Library::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("fflocal.log");
    let file = match std::fs::File::create(&path) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("no log file at {}: {err}", path.display());
            return None;
        }
    };
    let layer = tracing_subscriber::fmt::layer().with_ansi(false).with_target(true).with_writer(std::sync::Mutex::new(file));
    Some(layer.boxed())
}

/// Initial window size: `FFL_WINDOW=WxH` (layout checks on small windows), else 1600×900.
fn window_size() -> WindowResolution {
    std::env::var("FFL_WINDOW")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))))
        .map(|(w, h)| WindowResolution::new(w.max(320), h.max(240)))
        .unwrap_or_else(|| WindowResolution::new(1600, 900))
}

/// The FFXIV engine, opened from an install (or found).
struct Ff14Factory;

impl EngineFactory for Ff14Factory {
    fn id(&self) -> &str {
        ffl_ff14::ENGINE_ID
    }
    fn name(&self) -> &str {
        "Final Fantasy XIV"
    }
    fn shaders(&self) -> Vec<ffl_core::ShaderDef> {
        ffl_ff14::shaders::shader_defs()
    }
    fn scan(&self) -> Vec<ffl_core::InstallCandidate> {
        ffl_ff14_assets::locate::scan()
    }
    fn open(&self, path: Option<&std::path::Path>, plugins: Option<&std::path::Path>) -> Arc<dyn Engine> {
        match ffl_ff14::Ff14Engine::open(path, plugins) {
            Ok(e) => Arc::new(e),
            Err(err) => Arc::new(MissingEngine {
                id: self.id().into(),
                name: self.name().into(),
                error: format!("{err:#}"),
                shaders: self.shaders(),
            }),
        }
    }
}

/// The FFXI engine, opened from an install (or found).
struct Ff11Factory;

impl EngineFactory for Ff11Factory {
    fn id(&self) -> &str {
        ffl_ff11::ENGINE_ID
    }
    fn name(&self) -> &str {
        "Final Fantasy XI"
    }
    fn shaders(&self) -> Vec<ffl_core::ShaderDef> {
        ffl_ff11::Ff11Engine::shader_defs()
    }
    fn scan(&self) -> Vec<ffl_core::InstallCandidate> {
        ffl_ff11::dat::scan()
    }
    fn open(&self, path: Option<&std::path::Path>, _plugins: Option<&std::path::Path>) -> Arc<dyn Engine> {
        Arc::new(ffl_ff11::Ff11Engine::open(path))
    }
}

/// `--screenshot-at`: capture in any state (`--screenshot-burst` consecutive frames), then exit
/// two seconds later.
fn timed_screenshot(
    mut commands: Commands,
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    mut taken: Local<u32>,
    mut exit: MessageWriter<AppExit>,
) {
    let (Some(at), Some(path)) = (opts.screenshot_at, opts.screenshot.as_ref()) else {
        return;
    };
    let now = time.elapsed_secs();
    if *taken < opts.screenshot_burst && now >= at + *taken as f32 * opts.screenshot_interval {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let target = if opts.screenshot_burst > 1 {
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            path.with_file_name(format!("{stem}-{}.png", *taken))
        } else {
            path.clone()
        };
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(target));
        *taken += 1;
    } else if *taken >= opts.screenshot_burst && now >= at + opts.screenshot_burst as f32 * opts.screenshot_interval + 2.0 {
        exit.write(AppExit::Success);
    }
}

/// `FFL_TRACE_CAMERA`: per-frame component census of every camera (main world).
fn trace_camera_main(
    cams: Query<(
        Entity,
        &Camera,
        Has<Camera3d>,
        Has<bevy::pbr::AtmosphereSettings>,
        Has<bevy::light::AtmosphereEnvironmentMapLight>,
        Has<bevy::post_process::bloom::Bloom>,
        Has<bevy::camera::Hdr>,
        Option<&bevy::core_pipeline::tonemapping::Tonemapping>,
        Option<&bevy::render::sync_world::RenderEntity>,
    )>,
    atmos: Query<Entity, With<bevy::light::Atmosphere>>,
    frames: Res<bevy::diagnostic::FrameCount>,
) {
    for (e, cam, c3, settings, env, bloom, hdr, tone, re) in &cams {
        tracing::info!(
            "frame {} main cam {e} order {} 3d={c3} atmo_settings={settings} env_light={env} bloom={bloom} hdr={hdr} tone={tone:?} render={:?} atmospheres={}",
            frames.0,
            cam.order,
            re.map(|r| r.id()),
            atmos.iter().count()
        );
    }
}

/// `FFL_TRACE_CAMERA`: render-world twin of [`trace_camera_main`].
fn trace_camera_render(
    views: Query<(
        Entity,
        Has<Camera3d>,
        Has<bevy::pbr::ExtractedAtmosphere>,
        Has<bevy::pbr::GpuAtmosphereSettings>,
        Has<bevy::render::view::ViewTarget>,
        Option<&bevy::render::sync_world::MainEntity>,
    ), With<bevy::render::camera::ExtractedCamera>>,
) {
    for (e, c3, ex, gpu, vt, me) in &views {
        tracing::info!("render view {e} main={:?} 3d={c3} extracted_atmo={ex} gpu_settings={gpu} view_target={vt}", me.map(|m| m.id()));
    }
}
