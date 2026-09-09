//! In-world egui overlay with load progress and controls.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use egui::Color32;

use crate::app::{AppState, Audio, Graphics, HubState, UiFocus, WorldRequest};
use crate::camera::{CameraMode, MainCamera};
use crate::convert::cull_name;
use crate::world::character::{CharacterAnimator, CharacterStatus};
use crate::world::player::Player;
use crate::world::sound::{AudioTrace, SoundMonitorState};
use crate::world::zone::{CullState, ZoneStatus};

/// The runtime state the overlay's buttons act on (bundled: Bevy systems take 16 params).
#[derive(bevy::ecs::system::SystemParam)]
pub struct Host<'w> {
    pub next: ResMut<'w, NextState<AppState>>,
    pub hub: ResMut<'w, HubState>,
    pub graphics: ResMut<'w, Graphics>,
    pub audio: ResMut<'w, Audio>,
    pub exit: MessageWriter<'w, AppExit>,
}

pub struct DebugUiPlugin;

impl Plugin for DebugUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FrameTimeDiagnosticsPlugin::default())
            .add_systems(EguiPrimaryContextPass, (overlay, sync_ui_focus).chain().run_if(in_state(AppState::World)));
    }
}

#[allow(clippy::too_many_arguments)]
fn overlay(
    mut contexts: EguiContexts,
    request: Res<WorldRequest>,
    status: Res<ZoneStatus>,
    chara: Res<CharacterStatus>,
    cull: Res<CullState>,
    mode: Res<CameraMode>,
    diagnostics: Res<DiagnosticsStore>,
    camera: Query<&Transform, With<MainCamera>>,
    animators: Query<&CharacterAnimator>,
    players: Query<&Player>,
    trace: Res<AudioTrace>,
    monitor: Res<SoundMonitorState>,
    mut host: Host,
) -> Result {
    let Host { next, hub, graphics, audio, exit } = &mut host;
    let ctx = contexts.ctx_mut()?;
    let (bar_events, menu) = ffl_hub::ui::world_bar(ctx, &mut hub.0, &request.engine, &request.map_name);
    if menu {
        next.set(AppState::Launcher);
    }
    for event in bar_events {
        let mut unused = WorldRequest::default();
        if crate::launcher::handle_event(event, graphics, audio, &mut unused, next) {
            info!("quit from the world bar");
            exit.write(AppExit::Success);
        }
    }
    if audio.current.show_monitor {
        ffl_hub::ui::sound_monitor(ctx, &monitor.view);
    }
    let fps = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    let show_diagnostics = hub.0.settings.ui.diagnostics;
    let key_hints = hub.0.settings.ui.key_hints;
    egui::Window::new("Diagnostics").default_pos([10.0, 10.0]).resizable(false).open(&mut hub.0.settings.ui.diagnostics).show(ctx, |ui| {
        if !show_diagnostics {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(format!("{fps:.0} fps   {} / {}", request.engine, request.map_name));
            if audio.current.mute {
                ui.colored_label(egui::Color32::YELLOW, "MUTED (M)");
            }
        });
        ui.label(&status.phase);
        ui.label(format!("models {}/{}  instances {}", status.models_ready, status.models_total, status.instances_spawned));
        if !status.scene_content.is_empty() {
            let color = if status.scene_shareable { Color32::LIGHT_GREEN } else { Color32::from_rgb(255, 180, 120) };
            ui.colored_label(color, format!("world content: {}", status.scene_content));
        }
        ui.label(&chara.phase);
        if !chara.content.is_empty() {
            let color = if chara.shareable { Color32::LIGHT_GREEN } else { Color32::from_rgb(255, 180, 120) };
            ui.colored_label(color, format!("character content: {}", chara.content));
        }
        if let Ok(a) = animators.single() {
            ui.label(format!(
                "clip {}  t={:.2}s  ({} clips loaded)",
                a.current.as_deref().unwrap_or("-"),
                a.time,
                a.clips.len()
            ));
        }
        if let Ok(p) = players.single() {
            ui.label(format!(
                "player grounded={} moving={} vy={:.1}{}{}{}",
                p.grounded,
                p.moving,
                p.velocity.y,
                if p.sprinting { "  SPRINT" } else { "" },
                if p.walking { "  WALK" } else { "" },
                if p.flying { "  FLYING" } else { "" }
            ));
        }
        if !trace.bgm.is_empty() {
            ui.label(&trace.bgm);
        }
        if !trace.ambience.is_empty() {
            ui.label(format!("{}, {} sinks live", trace.ambience, trace.sinks));
        }
        if !trace.step.is_empty() {
            ui.label(&trace.step);
        }
        if !trace.voice.is_empty() {
            ui.label(&trace.voice);
        }
        if !chara.errors.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("{} character errors (see log)", chara.errors.len()));
        }
        if !status.errors.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("{} load errors (see log)", status.errors.len()));
        }
        if let Ok(t) = camera.single() {
            let p = t.translation;
            ui.label(format!("camera {:.1} {:.1} {:.1}", p.x, p.y, p.z));
        }
        if *mode == CameraMode::Fly {
            ui.colored_label(egui::Color32::YELLOW, "FLY CAMERA: WASD moves the camera. Press F1 to control the character.");
        } else {
            ui.label(format!("camera {:?} (F1)   cull {} (F3)   Esc: launcher", *mode, cull_name(cull.0)));
        }
        if key_hints {
            ui.label("RMB look, WASD move, Shift sprint, / walk, Space jump, Z sheathe, F fly (Space/Ctrl up/down), M mute, F12 screenshot");
        }
    });
    if show_diagnostics != hub.0.settings.ui.diagnostics {
        hub.0.commit_ui();
    }
    for event in ffl_hub::ui::settings_window(ctx, &mut hub.0) {
        let mut unused = WorldRequest::default();
        crate::launcher::handle_event(event, graphics, audio, &mut unused, next);
    }
    Ok(())
}

/// Wheel and keys go to the camera and the player only when egui does not want them.
fn sync_ui_focus(mut contexts: EguiContexts, mut ui_focus: ResMut<UiFocus>) -> Result {
    let ctx = contexts.ctx_mut()?;
    *ui_focus = UiFocus {
        pointer: ctx.is_pointer_over_egui() || ctx.egui_wants_pointer_input(),
        keyboard: ctx.egui_wants_keyboard_input(),
    };
    Ok(())
}
