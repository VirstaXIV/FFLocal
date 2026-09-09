//! Launcher character preview: the selected preset (or the one being edited) is loaded
//! through its engine and shown on a small stage behind the hub panels, idling, and reloads
//! whenever the preset changes. Drag turns it, the wheel zooms.

use std::sync::Arc;

use anyhow::Result;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};
use ffl_core::{CharacterModel, CharacterPreset, Engine};

use crate::app::{AppState, Engines, Graphics, HubState, RuntimeOptions, UiFocus};
use crate::camera::MainCamera;
use crate::world::character::{RigAssets, spawn_rig};
use crate::world::materials::MaterialCache;
use crate::world::shaders::{EngineMaterial, ShaderRegistry};

/// Everything spawned for the preview (stage, lights, the rig); despawned on leaving the launcher.
#[derive(Component)]
pub struct PreviewEntity;

/// The preview's character rig (animated by `world::character::animate_character`).
#[derive(Component)]
pub struct PreviewRig;

/// Seconds a preset change must settle before the preview reloads (typing, sliders).
const DEBOUNCE: f32 = 0.35;

#[derive(Resource)]
pub struct Preview {
    /// A full description of the last load, handed to the hub's log once.
    pub detail: Option<String>,
    /// Key of the preset the preview should show and the preset itself.
    wanted: Option<(String, CharacterPreset)>,
    wanted_at: f64,
    loaded: Option<String>,
    task: Option<(String, Task<Result<CharacterModel>>)>,
    rig: Option<Entity>,
    pub status: String,
    /// Standing height of the shown character (frames the camera).
    height: f32,
    yaw: f32,
    pitch: f32,
    distance: f32,
    stage_ready: bool,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            detail: None,
            wanted: None,
            wanted_at: 0.0,
            loaded: None,
            task: None,
            rig: None,
            status: String::new(),
            height: 1.7,
            yaw: 0.35,
            pitch: -0.12,
            distance: 3.2,
            stage_ready: false,
        }
    }
}

pub struct PreviewPlugin;

impl Plugin for PreviewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Preview>()
            .init_resource::<UiFocus>()
            .add_systems(OnEnter(AppState::Launcher), enter_preview)
            .add_systems(OnExit(AppState::Launcher), exit_preview)
            .add_systems(Update, (sync_preview, poll_preview, orbit_preview).chain().run_if(in_state(AppState::Launcher)));
    }
}

fn preset_key(p: &CharacterPreset, generation: u64) -> String {
    format!("{generation}|{}|{:?}", p.engine, p.settings)
}

fn enter_preview(
    mut commands: Commands,
    mut preview: ResMut<Preview>,
    mut cache: ResMut<MaterialCache>,
    opts: Res<RuntimeOptions>,
    graphics: Res<Graphics>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    *preview = Preview::default();
    preview.stage_ready = true;
    cache.normal_maps = opts.normal_maps;
    cache.anisotropy = graphics.current.anisotropy as u16;
    commands.insert_resource(ClearColor(Color::srgb(0.10, 0.11, 0.14)));
    // Stage: a floor disc and a key light; the camera's ambient light fills the rest.
    commands.spawn((
        Name::new("PreviewFloor"),
        PreviewEntity,
        Mesh3d(meshes.add(Circle::new(2.4))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.16, 0.17, 0.2),
            perceptual_roughness: 0.95,
            ..default()
        })),
        Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
    ));
    commands.spawn((
        Name::new("PreviewKeyLight"),
        PreviewEntity,
        DirectionalLight {
            illuminance: 2600.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.6, -0.9, 0.0)),
    ));
    commands.spawn((
        Name::new("PreviewFillLight"),
        PreviewEntity,
        DirectionalLight {
            illuminance: 900.0,
            color: Color::srgb(0.8, 0.85, 1.0),
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, -2.4, -0.5, 0.0)),
    ));
}

fn exit_preview(mut commands: Commands, entities: Query<Entity, With<PreviewEntity>>, mut preview: ResMut<Preview>) {
    for e in &entities {
        commands.entity(e).despawn();
    }
    *preview = Preview::default();
}

/// Follow the hub: the edited preset wins over the selected one; a change starts the
/// debounce, its end starts the load (one at a time, the newest key wins).
fn sync_preview(time: Res<Time>, engines: Res<Engines>, mut hub: ResMut<HubState>, mut preview: ResMut<Preview>) {
    let now = time.elapsed_secs_f64();
    let wanted = hub.0.preview_preset().cloned();
    let generation = hub.0.engine_generation;
    let key = wanted.as_ref().map(|p| preset_key(p, generation));
    if preview.wanted.as_ref().map(|(k, _)| k) != key.as_ref() {
        preview.wanted = key.clone().zip(wanted);
        preview.wanted_at = now;
        if key.is_some() {
            preview.status = "preview: updating…".into();
        }
    }
    if preview.task.is_none() && preview.wanted.as_ref().map(|(k, _)| k) != preview.loaded.as_ref() && now - preview.wanted_at >= DEBOUNCE as f64 {
        match preview.wanted.clone() {
            Some((k, preset)) => match engines.get(&preset.engine) {
                Some(engine) => {
                    let engine: Arc<dyn Engine> = engine;
                    preview.status = format!("preview: loading {}…", preset.name);
                    preview.task = Some((k, AsyncComputeTaskPool::get().spawn(async move { engine.load_character(&preset) })));
                }
                None => {
                    preview.status = format!("preview: engine {} unavailable", preset.engine);
                    preview.loaded = Some(k);
                }
            },
            None => {
                preview.loaded = None;
                preview.status.clear();
            }
        }
    }
    hub.0.preview_status = preview.status.clone();
    if let Some(detail) = preview.detail.take() {
        hub.0.log(detail);
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_preview(
    mut commands: Commands,
    opts: Res<RuntimeOptions>,
    mut preview: ResMut<Preview>,
    mut cache: ResMut<MaterialCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut engine_materials: ResMut<Assets<EngineMaterial>>,
    registry: Res<ShaderRegistry>,
    mut inverse_bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let Some((_, task)) = preview.task.as_mut() else {
        return;
    };
    let Some(result) = block_on(poll_once(task)) else {
        return;
    };
    let (key, _) = preview.task.take().unwrap();
    preview.loaded = Some(key);
    match result {
        Ok(character) => {
            if let Some(old) = preview.rig.take() {
                commands.entity(old).despawn();
            }
            let rig = commands
                .spawn((Name::new("PreviewRig"), PreviewEntity, PreviewRig, Transform::IDENTITY, Visibility::default()))
                .id();
            let mut assets = RigAssets {
                meshes: &mut meshes,
                images: &mut images,
                standard: &mut materials,
                engine: &mut engine_materials,
                registry: &registry,
                inverse_bindposes: &mut inverse_bindposes,
                cache: &mut cache,
            };
            let spawned = spawn_rig(&mut commands, rig, &character, &mut assets, Transform::IDENTITY, opts.normal_maps, false);
            preview.rig = Some(rig);
            preview.height = if character.height > 0.3 { character.height * character.scale } else { 1.7 };
            preview.status = format!("preview: {} · {spawned} meshes · {}", character.name, character.content.short_summary());
            preview.detail = Some(format!("preview: {} loaded ({spawned} meshes): {}", character.name, character.content.summary()));
            info!("preview: {} loaded, {spawned} meshes", character.name);
        }
        Err(err) => {
            warn!("preview load failed: {err:#}");
            preview.status = format!("preview failed: {err:#}");
        }
    }
    // A newer preset arrived meanwhile: `sync_preview` starts its load on the next frame.
}

/// Orbit the launcher camera around the character: left drag turns, wheel zooms (only when
/// the pointer is not over the UI).
fn orbit_preview(
    ui_pointer: Res<UiFocus>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut preview: ResMut<Preview>,
    mut camera: Query<&mut Transform, With<MainCamera>>,
    mut dragging: Local<bool>,
) {
    if mouse.just_pressed(MouseButton::Left) && !ui_pointer.pointer {
        *dragging = true;
    }
    if !mouse.pressed(MouseButton::Left) {
        *dragging = false;
    }
    if *dragging {
        preview.yaw -= motion.delta.x * 0.008;
        preview.pitch = (preview.pitch - motion.delta.y * 0.005).clamp(-1.2, 0.6);
    }
    if !ui_pointer.pointer && scroll.delta.y.abs() > 0.0 {
        preview.distance = (preview.distance * (1.0 - 0.1 * scroll.delta.y.signum())).clamp(0.25, 8.0);
    }
    let Ok(mut transform) = camera.single_mut() else {
        return;
    };
    let h = preview.height;
    // Close in, the pivot climbs from the chest to the face so zooming lands on it.
    let close = ((2.0 - preview.distance) / 1.5).clamp(0.0, 1.0);
    let target = Vec3::new(0.0, h * (0.52 + 0.38 * close), 0.0);
    let distance = preview.distance * (h / 1.7).max(0.6);
    let rot = Quat::from_euler(EulerRot::YXZ, preview.yaw, preview.pitch, 0.0);
    let eye = target + rot * Vec3::new(0.0, 0.0, distance);
    *transform = Transform::from_translation(eye).looking_at(target, Vec3::Y);
}
