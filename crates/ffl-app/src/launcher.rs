//! Launcher: the hub views (`ffl_hub::ui`) drawn with bevy_egui, plus applying graphics settings
//! to the Bevy runtime. Everything the menus *decide* lives in `ffl_hub`; this file only maps
//! its events onto Bevy state.

use bevy::light::DirectionalLightShadowMap;
use bevy::prelude::*;
use bevy::render::view::Msaa;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use ffl_hub::HubEvent;

use crate::app::{AppState, Audio, EngineFactories, EngineFactory, Engines, Graphics, HubState, RuntimeOptions, WorldRequest};
use crate::camera::MainCamera;

pub struct LauncherPlugin;

impl Plugin for LauncherPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Launcher), build_hub)
            .add_systems(Update, ((auto_reenter, dev_edit_set, dev_set_game).run_if(in_state(AppState::Launcher)), apply_graphics, apply_audio))
            .add_systems(EguiPrimaryContextPass, launcher_ui.run_if(in_state(AppState::Launcher)));
    }
}

fn build_hub(engines: Res<Engines>, mut hub: ResMut<HubState>) {
    hub.0.build(&engines.0);
    // Verification aid: open the character editor on a preset (by name or id) at startup.
    if let Some(wanted) = std::env::var_os("FFL_EDIT_PRESET").map(|v| v.to_string_lossy().to_string())
        && hub.0.editor.is_none()
        && let Some(p) = hub.0.library.presets.iter().find(|p| p.name == wanted || p.id == wanted).cloned()
    {
        hub.0.start_edit(p);
        // ... and optionally open the lookup popup of one field (`FFL_EDIT_LOOKUP=item.body`).
        if let Some(key) = std::env::var_os("FFL_EDIT_LOOKUP").map(|v| v.to_string_lossy().to_string())
            && let Some((catalog, none)) = hub.0.editor.as_ref().and_then(|e| {
                e.fields.iter().find(|f| f.key == key).and_then(|f| match &f.kind {
                    ffl_core::FieldKind::Lookup { catalog, none_label } => Some((catalog.clone(), none_label.clone())),
                    _ => None,
                })
            })
        {
            hub.0.editor_open_lookup(&key, &catalog, &none);
        }
    }
}

fn launcher_ui(
    mut contexts: EguiContexts,
    mut hub: ResMut<HubState>,
    mut graphics: ResMut<Graphics>,
    mut audio: ResMut<Audio>,
    mut request: ResMut<WorldRequest>,
    mut next: ResMut<NextState<AppState>>,
    mut ui_focus: ResMut<crate::app::UiFocus>,
    mut exit: MessageWriter<AppExit>,
    mut engines: ResMut<Engines>,
    factories: Res<EngineFactories>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    for event in ffl_hub::ui::hub_ui(ctx, &mut hub.0) {
        let event = match event {
            HubEvent::SetGamePath { engine, path } => {
                set_game_path(&mut hub.0, &mut engines, &factories, &engine, &path);
                continue;
            }
            HubEvent::SetGameEnabled { engine, enabled } => {
                set_game_enabled(&mut hub.0, &mut engines, &factories, &engine, enabled);
                continue;
            }
            HubEvent::SetPluginsPath { engine, path } => {
                if let Some(factory) = factories.get(&engine) {
                    hub.0.settings.games.entry(engine.clone()).or_default().plugins_path = path.trim().to_string();
                    reopen_engine(&mut hub.0, &mut engines, factory);
                    hub.0.plugins_edit.insert(engine, path.trim().to_string());
                }
                continue;
            }
            HubEvent::ScanGame(engine) => {
                if let Some(f) = factories.get(&engine) {
                    let found = f.scan();
                    info!("{engine}: scan found {} install(s)", found.len());
                    hub.0.game_candidates.insert(engine, found);
                }
                continue;
            }
            HubEvent::BrowseGame(engine) => {
                let name = factories.get(&engine).map(|f| f.name().to_string()).unwrap_or(engine.clone());
                if let Some(dir) = rfd::FileDialog::new().set_title(format!("{name} install folder")).pick_folder() {
                    let path = dir.display().to_string();
                    hub.0.game_edit.insert(engine.clone(), path.clone());
                    set_game_path(&mut hub.0, &mut engines, &factories, &engine, &path);
                }
                continue;
            }
            other => other,
        };
        if handle_event(event, &mut graphics, &mut audio, &mut request, &mut next) {
            info!("quit from the hub");
            exit.write(AppExit::Success);
        }
    }
    // The preview takes drags and the wheel only in the free centre, and not under a
    // floating window (the root panels sit on egui's background layer, which the
    // "over egui" queries do not count).
    let over_ui = match (ctx.pointer_latest_pos(), hub.0.free_rect) {
        (Some(p), Some(r)) => {
            let inside = p.x >= r[0] && p.x <= r[2] && p.y >= r[1] && p.y <= r[3];
            !inside || ctx.layer_id_at(p).is_some_and(|l| l.order != egui::Order::Background)
        }
        (Some(_), None) => true,
        (None, _) => false,
    };
    *ui_focus = crate::app::UiFocus {
        pointer: over_ui || ctx.egui_wants_pointer_input(),
        keyboard: ctx.egui_wants_keyboard_input(),
    };
    Ok(())
}

/// Apply a hub event to the runtime. `Quit` is returned to the caller, which owns the exit
/// writer.
/// Save an engine's install path (empty = auto-detect), reopen the engine from it and
/// rebuild the hub's lists. Using a path switches the game on.
fn set_game_path(hub: &mut ffl_hub::Hub, engines: &mut Engines, factories: &EngineFactories, engine_id: &str, path: &str) {
    let Some(factory) = factories.get(engine_id) else {
        hub.status = format!("no engine {engine_id}");
        return;
    };
    let entry = hub.settings.games.entry(engine_id.to_string()).or_default();
    entry.path = path.trim().to_string();
    entry.enabled = true;
    reopen_engine(hub, engines, factory);
    let info = engines.get(engine_id).map(|e| e.info());
    let shown = if path.trim().is_empty() { info.and_then(|i| i.path).unwrap_or_default() } else { path.trim().to_string() };
    hub.game_edit.insert(engine_id.to_string(), shown);
}

/// Switch a game off (its engine is replaced by a disabled stand-in: characters, worlds and
/// sounds of that game are gone at once) or on again. Only reachable from the hub, where no
/// world holds the engine's data.
fn set_game_enabled(hub: &mut ffl_hub::Hub, engines: &mut Engines, factories: &EngineFactories, engine_id: &str, enabled: bool) {
    let Some(factory) = factories.get(engine_id) else {
        hub.status = format!("no engine {engine_id}");
        return;
    };
    hub.settings.games.entry(engine_id.to_string()).or_default().enabled = enabled;
    reopen_engine(hub, engines, factory);
    if let Some(p) = hub.selected_preset().filter(|p| p.engine == engine_id && !enabled).map(|p| p.id.clone()) {
        info!("{engine_id}: switched off, deselecting {p}");
        hub.selected_preset = hub.library.presets.iter().find(|p| hub.settings.game_enabled(&p.engine)).map(|p| p.id.clone());
        if hub.editor.as_ref().is_some_and(|e| e.preset.engine == engine_id) {
            hub.editor = None;
        }
    }
}

/// Save the settings, open the engine as they say and rebuild the hub.
fn reopen_engine(hub: &mut ffl_hub::Hub, engines: &mut Engines, factory: &dyn EngineFactory) {
    if let Err(err) = hub.settings.save() {
        hub.status = format!("saving settings: {err:#}");
    }
    let path = hub.settings.game_path(factory.id());
    let engine = crate::app::open_engine(factory, &hub.settings, path.as_deref());
    let info = engine.info();
    info!("{}: {} ({})", info.name, if info.available { "opened" } else { "unavailable" }, info.detail);
    engines.replace(engine);
    hub.rebuild(&engines.0);
}

/// Verification aid: `FFL_SET_GAME="ff11=/path"` applies an install path through the Games
/// flow 3 s after start (the engine must reopen and the worlds list refill); `ff11=off` /
/// `ff11=on` flip the game's switch the same way.
fn dev_set_game(time: Res<Time>, mut hub: ResMut<HubState>, mut engines: ResMut<Engines>, factories: Res<EngineFactories>, mut done: Local<bool>) {
    if *done || time.elapsed_secs() < 3.0 {
        return;
    }
    *done = true;
    let Some(spec) = std::env::var_os("FFL_SET_GAME").map(|v| v.to_string_lossy().to_string()) else {
        return;
    };
    if let Some((engine, path)) = spec.split_once('=') {
        info!("FFL_SET_GAME: {engine} = {path}");
        match path {
            // `ff11=off` / `ff11=on` drive the switch instead of the path.
            "off" | "on" => set_game_enabled(&mut hub.0, &mut engines, &factories, engine, path == "on"),
            _ => {
                hub.0.game_edit.insert(engine.to_string(), path.to_string());
                set_game_path(&mut hub.0, &mut engines, &factories, engine, path);
            }
        }
    }
}

pub fn handle_event(event: HubEvent, graphics: &mut Graphics, audio: &mut Audio, request: &mut WorldRequest, next: &mut NextState<AppState>) -> bool {
    match event {
        HubEvent::Quit => return true,
        // Handled by the launcher itself (they need the engine factories).
        HubEvent::SetGamePath { .. } | HubEvent::SetPluginsPath { .. } | HubEvent::SetGameEnabled { .. } | HubEvent::ScanGame(_) | HubEvent::BrowseGame(_) => {}
        HubEvent::EnterWorld(r) => {
            *request = r.into();
            next.set(AppState::World);
        }
        HubEvent::GraphicsChanged(g) => {
            graphics.current = g;
            graphics.dirty = true;
        }
        // The hub already pushed the list into the engine; worlds pick it up when (re)entered.
        HubEvent::ModsChanged(engine, ids) => info!("{engine}: enabled mods {ids:?}"),
        HubEvent::AudioChanged(a) => {
            audio.current = a;
            audio.dirty = true;
        }
    }
    false
}

/// Audio settings changed: the mixer reads them every frame, so this only logs the new state
/// (the music pause/resume is handled by `world::sound::bgm_controller` on `dirty`).
fn apply_audio(audio: Res<Audio>) {
    if !audio.is_changed() {
        return;
    }
    let a = &audio.current;
    info!(
        "audio settings: master {:.2} music {:.2} ambience {:.2} footsteps {:.2} voice {:.2} effects {:.2}{}{}",
        a.master,
        a.music,
        a.ambience,
        a.footsteps,
        a.voice,
        a.effects,
        if a.mute { " MUTED" } else { "" },
        if a.music_enabled { "" } else { " music off" }
    );
}

/// Push the graphics settings into the window, camera, shadow map and UI scale. Runs on
/// change (and once at startup). Render/detail distance and anisotropy are read where meshes
/// and textures are created.
fn apply_graphics(
    mut graphics: ResMut<Graphics>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    mut cameras: Query<(Entity, &Msaa), With<MainCamera>>,
    mut commands: Commands,
    mut shadow_map: ResMut<DirectionalLightShadowMap>,
    mut suns: Query<&mut DirectionalLight>,
    mut contexts: EguiContexts,
) {
    if !graphics.dirty {
        return;
    }
    graphics.dirty = false;
    let g = graphics.current.clone();
    if let Ok(mut w) = window.single_mut() {
        let mode = if g.fullscreen { WindowMode::BorderlessFullscreen(MonitorSelection::Current) } else { WindowMode::Windowed };
        if w.mode != mode {
            w.mode = mode;
        }
        let present = if g.vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync };
        if w.present_mode != present {
            w.present_mode = present;
        }
    }
    let msaa = match g.msaa {
        1 => Msaa::Off,
        2 => Msaa::Sample2,
        8 => Msaa::Sample8,
        _ => Msaa::Sample4,
    };
    for (cam, current) in &mut cameras {
        if *current != msaa {
            commands.entity(cam).insert(msaa);
        }
    }
    shadow_map.size = g.shadow_resolution as usize;
    for mut sun in &mut suns {
        sun.shadow_maps_enabled = g.shadows;
    }
    if let Ok(ctx) = contexts.ctx_mut() {
        ctx.set_zoom_factor(g.ui_scale);
    }
}

/// Verification aid: `FFL_EDIT_SET="key=value[;key=value]"` writes these fields into the open
/// editor 3 s after start (the preview must reload), as if they had been edited by hand.
fn dev_edit_set(time: Res<Time>, mut hub: ResMut<HubState>, mut done: Local<bool>) {
    if *done || time.elapsed_secs() < 3.0 {
        return;
    }
    let Some(spec) = std::env::var_os("FFL_EDIT_SET").map(|v| v.to_string_lossy().to_string()) else {
        *done = true;
        return;
    };
    let Some(editor) = hub.0.editor.as_mut() else {
        return;
    };
    *done = true;
    for kv in spec.split(';') {
        if let Some((k, v)) = kv.split_once('=') {
            editor.preset.settings.insert(k.trim().to_string(), v.trim().to_string());
            info!("FFL_EDIT_SET: {} = {}", k.trim(), v.trim());
        }
    }
    hub.0.refresh_editor();
}

/// `--reenter-at`: go back into the same world after a delay (round-trip verification).
fn auto_reenter(
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    request: Res<WorldRequest>,
    mut elapsed: Local<f32>,
    mut fired: Local<bool>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(t) = opts.reenter_at else {
        return;
    };
    if *fired || request.map.is_empty() {
        return;
    }
    *elapsed += time.delta_secs();
    if *elapsed >= t {
        *fired = true;
        info!("re-entering {}:{}", request.engine, request.map);
        next.set(AppState::World);
    }
}
