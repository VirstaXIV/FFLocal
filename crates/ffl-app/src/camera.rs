//! Fly camera (F1 toggles to the orbit camera when a player exists).

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use bevy_egui::{EguiContext, PrimaryEguiContext};

use crate::app::AppState;

#[derive(Component)]
pub struct FlyCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
    pub needs_sync: bool,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: -0.4,
            speed: 20.0,
            needs_sync: true,
        }
    }
}

#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraMode {
    #[default]
    Fly,
    Orbit,
}

#[derive(Component)]
pub struct MainCamera;

#[derive(Component)]
pub struct OrbitCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: -0.3,
            distance: 6.0,
        }
    }
}

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraMode>()
            .add_systems(Startup, spawn_camera)
            .add_systems(OnExit(AppState::World), (release_cursor, respawn_camera))
            .add_systems(Update, (toggle_mode, fly_camera, mouse_grab).run_if(in_state(AppState::World)));
    }
}

fn spawn_camera(mut commands: Commands) {
    // bevy_egui attaches its primary context to the first camera on its own.
    spawn_main_camera(&mut commands);
}

fn spawn_main_camera(commands: &mut Commands) -> Entity {
    commands
        .spawn((
            Name::new("MainCamera"),
            MainCamera,
            Camera3d::default(),
            Projection::Perspective(PerspectiveProjection {
                fov: 60f32.to_radians(),
                near: 0.1,
                // Far enough for the sea ring; Bevy's reverse-Z projection keeps the precision.
                far: 120_000.0,
                ..default()
            }),
            Transform::from_xyz(0.0, 40.0, 60.0).looking_at(Vec3::ZERO, Vec3::Y),
            FlyCamera::default(),
            OrbitCamera::default(),
            AmbientLight {
                color: Color::WHITE,
                brightness: 400.0,
                affects_lightmapped_meshes: true,
            },
        ))
        .id()
}

/// Replace the camera when leaving a world. A world adds HDR, tonemapping, bloom and atmosphere
/// components; removing them one by one left stale render-world state behind (the sky pass kept
/// its pipeline, uniforms and bind groups on the camera's render entity and flickered over the
/// launcher, or failed validation once the target went back to LDR). A fresh entity gets a
/// fresh render twin. bevy_egui only auto-attaches its context to the first camera, so the
/// primary context is re-created here.
fn respawn_camera(mut commands: Commands, cameras: Query<Entity, With<MainCamera>>, mut graphics: ResMut<crate::app::Graphics>) {
    for cam in &cameras {
        commands.entity(cam).despawn();
    }
    let cam = spawn_main_camera(&mut commands);
    commands.entity(cam).insert((EguiContext::default(), PrimaryEguiContext));
    // Re-apply MSAA and the rest to the fresh camera.
    graphics.dirty = true;
}

fn release_cursor(mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>, mut mode: ResMut<CameraMode>) {
    cursor.grab_mode = CursorGrabMode::None;
    cursor.visible = true;
    *mode = CameraMode::Fly;
}

fn toggle_mode(keys: Res<ButtonInput<KeyCode>>, mut mode: ResMut<CameraMode>, mut fly: Query<&mut FlyCamera>) {
    if keys.just_pressed(KeyCode::F1) {
        *mode = match *mode {
            CameraMode::Fly => CameraMode::Orbit,
            CameraMode::Orbit => CameraMode::Fly,
        };
        info!("camera mode: {:?}", *mode);
        if *mode == CameraMode::Fly {
            for mut cam in &mut fly {
                cam.needs_sync = true;
            }
        }
    }
}

fn mouse_grab(mouse: Res<ButtonInput<MouseButton>>, mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>) {
    if mouse.just_pressed(MouseButton::Right) {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    }
    if mouse.just_released(MouseButton::Right) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

fn fly_camera(
    mode: Res<CameraMode>,
    ui_focus: Res<crate::app::UiFocus>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    mut query: Query<(&mut Transform, &mut FlyCamera), With<MainCamera>>,
) {
    if *mode != CameraMode::Fly {
        return;
    }
    let Ok((mut transform, mut cam)) = query.single_mut() else {
        return;
    };
    if cam.needs_sync {
        let (yaw, pitch, _) = transform.rotation.to_euler(EulerRot::YXZ);
        cam.yaw = yaw;
        cam.pitch = pitch;
        cam.needs_sync = false;
    }
    if mouse.pressed(MouseButton::Right) {
        cam.yaw -= motion.delta.x * 0.0025;
        cam.pitch = (cam.pitch - motion.delta.y * 0.0025).clamp(-1.54, 1.54);
    }
    if !ui_focus.pointer && scroll.delta.y.abs() > 0.0 {
        cam.speed = (cam.speed * (1.0 + 0.15 * scroll.delta.y.signum())).clamp(1.0, 500.0);
    }
    let keys_free = !ui_focus.keyboard;
    let key_down = |k: KeyCode| keys_free && keys.pressed(k);
    transform.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    let forward = transform.forward().as_vec3();
    let right = transform.right().as_vec3();
    let mut dir = Vec3::ZERO;
    if key_down(KeyCode::KeyW) {
        dir += forward;
    }
    if key_down(KeyCode::KeyS) {
        dir -= forward;
    }
    if key_down(KeyCode::KeyD) {
        dir += right;
    }
    if key_down(KeyCode::KeyA) {
        dir -= right;
    }
    if key_down(KeyCode::KeyE) || key_down(KeyCode::Space) {
        dir += Vec3::Y;
    }
    if key_down(KeyCode::KeyQ) || key_down(KeyCode::ControlLeft) {
        dir -= Vec3::Y;
    }
    let mut speed = cam.speed;
    if key_down(KeyCode::ShiftLeft) {
        speed *= 5.0;
    }
    if dir.length_squared() > 0.0 {
        transform.translation += dir.normalize() * speed * time.delta_secs();
    }
}
