//! Kinematic player controller and third-person orbit camera.
//!
//! Horizontal motion uses Avian's move-and-slide against walls; the vertical axis is handled with
//! explicit shape casts and ground snapping (move-and-slide ignores contacts it already touches).

use avian3d::prelude::*;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;

use crate::app::{AppState, RuntimeOptions, WorldEntity};
use crate::camera::{CameraMode, MainCamera, OrbitCamera};
use crate::world::zone::{SOLID_LAYER, WATER_LAYER, ZoneStatus};

const CAPSULE_RADIUS: f32 = 0.3;
const CAPSULE_LENGTH: f32 = 1.1;
const RUN_SPEED: f32 = 6.0;
/// Sprint: the game's sprint is a ~30% speed boost over running.
const SPRINT_SPEED: f32 = 7.8;
const FLY_SPEED: f32 = 14.0;
const WALK_SPEED: f32 = 2.5;
// ~1 m high, ~0.65 s in the air (running jumps cover ~4 m), close to the game's.
const JUMP_SPEED: f32 = 6.2;
const GRAVITY: f32 = 19.0;
const STEP_HEIGHT: f32 = 0.45;
/// Horizontal speed of sliding off surfaces steeper than the walkable limit.
const SLIDE_SPEED: f32 = 3.0;
// Walkable up to ~60°; steeper surfaces slide.
const MIN_GROUND_NORMAL_Y: f32 = 0.5;
/// Swimming (the game's sea swimming): the character starts swimming once the water above
/// the feet is chest deep, wades back out when it is shallower, and floats with the feet
/// this far under the surface.
const SWIM_ENTER_DEPTH: f32 = 1.35;
const SWIM_EXIT_DEPTH: f32 = 1.05;
const SWIM_FLOAT_DEPTH: f32 = 1.2;
const SWIM_SPEED: f32 = 3.6;
const SWIM_SPRINT_SPEED: f32 = 4.8;
/// Vertical swim speed (Space up, Ctrl/C down) and the rate the body bobs back to the surface.
const DIVE_SPEED: f32 = 2.6;
/// Feet this far under the surface put the head under water (a diving character).
const SUBMERGED_DEPTH: f32 = 1.75;

/// The player chain (`spawn_player`, `move_player`, `orbit_camera`, `trace_player`).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerSystems;

#[derive(Component)]
pub struct Player {
    pub velocity: Vec3,
    pub grounded: bool,
    pub facing: f32,
    /// Yaw the rig is turned to; follows `facing` at `TURN_RATE` (the game turns the body over
    /// a few frames rather than snapping it).
    pub rig_yaw: f32,
    pub speed: f32,
    pub moving: bool,
    /// Walk toggle (`/`).
    pub walking: bool,
    /// Sprint held (Shift).
    pub sprinting: bool,
    pub age: f32,
    /// Normal of the ground under the player from the last snap (Y when airborne).
    pub ground_normal: Vec3,
    /// Free flight (F): no gravity, Space/Ctrl for height, collisions still apply.
    pub flying: bool,
    /// The mesh entity under the feet from the last ground probe (`None` while airborne);
    /// footsteps read its `Surface`.
    pub ground_entity: Option<Entity>,
    /// In water deep enough to swim (no gravity, Space/Ctrl for depth).
    pub swimming: bool,
    /// Swimming with the head under the surface.
    pub submerged: bool,
    /// Metres of water above the feet (0 when not in water).
    pub water_depth: f32,
}

/// Visual root under the player; the character plugin populates it.
#[derive(Component)]
pub struct CharacterRig;

#[derive(Component)]
pub struct PlaceholderVisual;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TraceTimer>().add_systems(
            Update,
            (spawn_player, move_player, orbit_camera, trace_player)
                .chain()
                .in_set(PlayerSystems)
                .run_if(in_state(AppState::World)),
        );
    }
}

fn player_collider() -> Collider {
    Collider::capsule(CAPSULE_RADIUS, CAPSULE_LENGTH)
}

#[allow(clippy::too_many_arguments)]
fn spawn_player(
    mut commands: Commands,
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    status: Res<ZoneStatus>,
    existing: Query<(), With<Player>>,
    spatial: SpatialQuery,
    mut mode: ResMut<CameraMode>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut camera: Query<(&mut OrbitCamera, &Transform), With<MainCamera>>,
) {
    if !existing.is_empty() || opts.single_model.is_some() || !opts.collision {
        return;
    }
    let Some(finished_at) = status.finished_at else {
        return;
    };
    if time.elapsed_secs_f64() - finished_at < 0.3 {
        return;
    }
    // Ground only: the sea surface must not count as a place to stand.
    let filter = SpatialQueryFilter::from_mask(SOLID_LAYER);
    let mut candidates: Vec<Vec3> = Vec::new();
    if let Some(s) = opts.spawn {
        candidates.push(s);
    } else {
        candidates.extend(status.center);
        candidates.extend(status.spawn_candidates.iter().copied());
    }
    let reference_y = status.center.map(|c| c.y);
    let mut found: Option<Vec3> = None;
    let mut best_score = f32::MAX;
    for anchor in candidates.iter().take(if opts.spawn.is_some() { 1 } else { 25 }) {
        let origin = Vec3::new(anchor.x, anchor.y + 300.0, anchor.z);
        if let Some(hit) = spatial.cast_ray(origin, Dir3::NEG_Y, 1000.0, true, &filter) {
            let ground = origin + Vec3::NEG_Y * hit.distance;
            if hit.normal.y < 0.85 && opts.spawn.is_none() {
                continue;
            }
            let headroom = spatial
                .cast_ray(ground + Vec3::Y * 0.5, Dir3::Y, 3.0, true, &filter)
                .map(|h| h.distance)
                .unwrap_or(3.0);
            let score = reference_y.map(|y| (ground.y - y).abs()).unwrap_or(0.0) + (1.0 - hit.normal.y) * 100.0 + (3.0 - headroom) * 20.0;
            if score < best_score {
                best_score = score;
                found = Some(ground + Vec3::Y * (CAPSULE_LENGTH * 0.5 + CAPSULE_RADIUS + 0.2));
            }
        }
    }
    let pos = match found {
        Some(p) => p,
        None => {
            if time.elapsed_secs_f64() - finished_at < 4.0 {
                return;
            }
            let anchor = candidates.first().copied().unwrap_or(Vec3::ZERO);
            warn!("no ground below any spawn candidate; spawning in the air at {anchor:?}");
            anchor + Vec3::Y * 5.0
        }
    };
    // `--drop N`: start N metres up and fall (jump/fall/land verification).
    let pos = pos + Vec3::Y * opts.drop.unwrap_or(0.0);
    info!("spawning player at {pos:?}");
    let capsule = meshes.add(Capsule3d::new(CAPSULE_RADIUS, CAPSULE_LENGTH));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.9, 0.5, 0.2),
        ..default()
    });
    commands
        .spawn((
            Name::new("Player"),
            WorldEntity,
            Player {
                velocity: Vec3::ZERO,
                grounded: false,
                facing: 0.0,
                rig_yaw: 0.0,
                speed: RUN_SPEED,
                moving: false,
                walking: false,
                sprinting: false,
                age: 0.0,
                ground_normal: Vec3::Y,
                flying: opts.fly,
                ground_entity: None,
                swimming: false,
                submerged: false,
                water_depth: 0.0,
            },
            Transform::from_translation(pos),
            Visibility::default(),
        ))
        .with_children(|parent| {
            parent
                .spawn((Name::new("CharacterRig"), CharacterRig, Transform::IDENTITY, Visibility::default()))
                .with_children(|rig| {
                    rig.spawn((
                        Name::new("Placeholder"),
                        PlaceholderVisual,
                        Mesh3d(capsule),
                        MeshMaterial3d(material),
                        Transform::IDENTITY,
                    ));
                });
        });
    if let Ok((mut orbit, cam_tf)) = camera.single_mut() {
        let (yaw, _, _) = cam_tf.rotation.to_euler(EulerRot::YXZ);
        orbit.yaw = yaw;
        if let Some(o) = opts.orbit {
            orbit.yaw = o.x.to_radians();
            orbit.pitch = o.y.to_radians();
            orbit.distance = o.z;
        }
    }
    *mode = CameraMode::Orbit;
}

#[allow(clippy::too_many_arguments)]
fn move_player(
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    keys: Res<ButtonInput<KeyCode>>,
    ui_focus: Res<crate::app::UiFocus>,
    mode: Res<CameraMode>,
    move_and_slide: MoveAndSlide,
    spatial: SpatialQuery,
    mut players: Query<(Entity, &mut Transform, &mut Player)>,
    orbit: Query<&OrbitCamera, With<MainCamera>>,
    mut rigs: Query<&mut Transform, (With<CharacterRig>, Without<Player>)>,
) {
    let Ok((entity, mut transform, mut player)) = players.single_mut() else {
        return;
    };
    // A focused text field (the emote search) keeps the movement keys.
    let keys_free = !ui_focus.keyboard;
    let key_down = |k: KeyCode| keys_free && keys.pressed(k);
    let key_hit = |k: KeyCode| keys_free && keys.just_pressed(k);
    player.age += time.delta_secs();
    let auto_forward = player.age < opts.autowalk;
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let collider = player_collider();
    // Solid geometry only: water surfaces live on their own layer.
    let filter = SpatialQueryFilter::from_excluded_entities([entity]).with_mask(SOLID_LAYER);
    let yaw = orbit.single().map(|o| o.yaw).unwrap_or(0.0);

    let mut dir = Vec3::ZERO;
    if *mode == CameraMode::Orbit {
        let forward = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
        let right = Quat::from_rotation_y(yaw) * Vec3::X;
        if key_down(KeyCode::KeyW) || auto_forward {
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
    }
    let dir = dir.normalize_or_zero();
    if *mode == CameraMode::Orbit && key_hit(KeyCode::Slash) {
        player.walking = !player.walking;
    }
    player.sprinting = (*mode == CameraMode::Orbit && (key_down(KeyCode::ShiftLeft) || key_down(KeyCode::ShiftRight))) || (auto_forward && opts.sprint);
    player.moving = dir.length_squared() > 0.0;
    // Water above the feet: a ray up from the feet finds the surface (water colliders sit on
    // their own layer, so nothing else answers).
    let feet_offset = CAPSULE_LENGTH * 0.5 + CAPSULE_RADIUS;
    let feet = transform.translation - Vec3::Y * feet_offset;
    let water_filter = SpatialQueryFilter::from_mask(WATER_LAYER);
    // Cast from below the feet so a surface at ankle height still counts.
    player.water_depth = spatial.cast_ray(feet - Vec3::Y * 0.5, Dir3::Y, 200.0, true, &water_filter).map(|h| (h.distance - 0.5).max(0.0)).unwrap_or(0.0);
    if !player.flying {
        if !player.swimming && player.water_depth >= SWIM_ENTER_DEPTH {
            player.swimming = true;
            player.grounded = false;
            player.ground_entity = None;
            player.ground_normal = Vec3::Y;
            player.velocity = Vec3::ZERO;
            info!("swimming (water {:.2} m deep over the feet)", player.water_depth);
        } else if player.swimming && player.water_depth < SWIM_EXIT_DEPTH {
            player.swimming = false;
            player.submerged = false;
            player.velocity = Vec3::ZERO;
            info!("out of the water");
        }
    } else {
        player.swimming = false;
        player.submerged = false;
    }
    player.speed = if player.swimming {
        if player.sprinting { SWIM_SPRINT_SPEED } else { SWIM_SPEED }
    } else if player.sprinting {
        SPRINT_SPEED
    } else if player.walking {
        WALK_SPEED
    } else {
        RUN_SPEED
    };
    if *mode == CameraMode::Orbit && key_hit(KeyCode::KeyF) {
        player.flying = !player.flying;
        player.grounded = false;
        player.velocity = Vec3::ZERO;
        info!("fly mode {}", if player.flying { "on" } else { "off" });
    }
    if player.flying {
        player.ground_entity = None;
        let mut fly = dir * FLY_SPEED;
        if *mode == CameraMode::Orbit {
            if key_down(KeyCode::Space) {
                fly.y += FLY_SPEED * 0.7;
            }
            if key_down(KeyCode::ControlLeft) || key_down(KeyCode::KeyC) {
                fly.y -= FLY_SPEED * 0.7;
            }
        }
        if let Some(v) = opts.fly_velocity {
            fly = v;
        }
        player.moving = fly.length_squared() > 0.0;
        player.grounded = false;
        player.velocity = Vec3::ZERO;
        if player.moving {
            let out = move_and_slide.move_and_slide(
                &player_collider(),
                transform.translation,
                Quat::IDENTITY,
                fly,
                time.delta(),
                &MoveAndSlideConfig::default(),
                &SpatialQueryFilter::from_excluded_entities([entity]),
                |_| MoveAndSlideHitResponse::Accept,
            );
            transform.translation = out.position;
            if dir.length_squared() > 0.0 {
                player.facing = dir.x.atan2(dir.z);
            }
            turn_rig(&mut player, time.delta_secs(), &mut rigs);
        }
        return;
    }

    if player.swimming {
        // Surface swimming: horizontal at swim speed, the body floats with the feet
        // `SWIM_FLOAT_DEPTH` under the surface; Space swims up, Ctrl/C dives. Under water the
        // same keys move up and down freely; letting go drifts back up to the surface.
        let surface = feet.y + player.water_depth;
        let mut velocity = dir * player.speed;
        let up = *mode == CameraMode::Orbit && key_down(KeyCode::Space);
        let down = *mode == CameraMode::Orbit && (key_down(KeyCode::ControlLeft) || key_down(KeyCode::KeyC));
        let target_feet = surface - SWIM_FLOAT_DEPTH;
        if down {
            velocity.y = -DIVE_SPEED;
        } else if up {
            // Never above the float line.
            velocity.y = if feet.y < target_feet { DIVE_SPEED } else { 0.0 };
        } else if feet.y < target_feet - 0.02 {
            velocity.y = ((target_feet - feet.y) * 2.0).min(DIVE_SPEED);
        } else if feet.y > target_feet + 0.02 {
            velocity.y = ((target_feet - feet.y) * 2.0).max(-DIVE_SPEED);
        }
        player.velocity = velocity;
        player.moving = velocity.xz().length_squared() > 0.0;
        if velocity.length_squared() > 0.0 {
            let out = move_and_slide.move_and_slide(&collider, transform.translation, Quat::IDENTITY, velocity, time.delta(), &MoveAndSlideConfig::default(), &filter, |_| MoveAndSlideHitResponse::Accept);
            transform.translation = out.position;
        }
        let feet_now = transform.translation.y - feet_offset;
        player.submerged = surface - feet_now > SUBMERGED_DEPTH;
        if dir.length_squared() > 0.0 {
            player.facing = dir.x.atan2(dir.z);
        }
        turn_rig(&mut player, dt, &mut rigs);
        return;
    }

    let verbose = std::env::var("FFL_TRACE_MOVE").is_ok();
    let start_pos = transform.translation;
    let mut pos = start_pos;
    let config = MoveAndSlideConfig::default();
    let horizontal = dir * player.speed;
    if horizontal.length_squared() > 0.0 {
        // While grounded, move along the ground plane (full speed up slopes, like the game) with
        // the capsule lifted by the step height so steps and slope surfaces never act as walls;
        // the ground snap below brings it back down.
        let (lift, velocity) = if player.grounded {
            // Keep the horizontal speed constant on walkable ground: trimesh terrain returns
            // slightly tilted normals even on flat-looking grass, and scaling the whole vector
            // to `speed` lost 10-15% of the pace there.
            let n = player.ground_normal;
            let along = horizontal - n * horizontal.dot(n);
            let flat = along.xz().length();
            let along = if flat > 1e-3 { along * (player.speed / flat) } else { horizontal };
            (STEP_HEIGHT, along)
        } else {
            (0.0, Vec3::new(horizontal.x, 0.0, horizontal.z))
        };
        let out = move_and_slide.move_and_slide(
            &collider,
            pos + Vec3::Y * lift,
            Quat::IDENTITY,
            velocity,
            time.delta(),
            &config,
            &filter,
            |_| MoveAndSlideHitResponse::Accept,
        );
        if verbose {
            debug!("slide: from {:?} vel {:?} -> {:?} (lift {lift})", pos + Vec3::Y * lift, velocity, out.position);
        }
        pos = Vec3::new(out.position.x, start_pos.y, out.position.z);
    }

    let probe_up = STEP_HEIGHT;
    // Casts ignore geometry the capsule already penetrates at its start: a zero-distance hit
    // there placed the player at the probe origin and let it climb walls a step per frame.
    let cast_config = |max: f32| ShapeCastConfig {
        ignore_origin_penetration: true,
        ..ShapeCastConfig::from_max_distance(max)
    };
    // Ground probe: a capsule cast, but when that only touches a steep face (a stair riser, a
    // wall beside the path) fall back to a ray from the capsule centre so the tread below is
    // what counts as ground. The ray hits the ground `CAPSULE_LENGTH/2 + CAPSULE_RADIUS`
    // below the centre when the capsule rests on it.
    // Returns (distance, normal, entity hit).
    let cast_down = |from: Vec3, max: f32| -> Option<(f32, Vec3, Entity)> {
        let shape = spatial
            .cast_shape(&collider, from, Quat::IDENTITY, Dir3::NEG_Y, &cast_config(max), &filter)
            .map(|hit| (hit.distance, hit.normal1, hit.entity));
        match shape {
            Some((_, n, _)) if n.y < MIN_GROUND_NORMAL_Y => {
                let bottom = CAPSULE_LENGTH * 0.5 + CAPSULE_RADIUS;
                spatial
                    .cast_ray(from, Dir3::NEG_Y, max + bottom, true, &filter)
                    .filter(|hit| hit.normal.y >= MIN_GROUND_NORMAL_Y && hit.distance >= bottom)
                    .map(|hit| (hit.distance - bottom, hit.normal, hit.entity))
                    .or(shape)
            }
            other => other,
        }
    };
    // "Inside geometry" is tested with a slightly smaller capsule: resting against a stair
    // riser or a wall is a legitimate touching contact, being deep inside a mesh is not.
    let probe = Collider::capsule((CAPSULE_RADIUS - 0.08).max(0.05), (CAPSULE_LENGTH - 0.1).max(0.1));
    let free = |at: Vec3| spatial.shape_intersections(&probe, at, Quat::IDENTITY, &filter).is_empty();
    let auto_jump = opts.autojump.is_some_and(|t| player.age - dt < t && player.age >= t);
    let jump = (*mode == CameraMode::Orbit && key_hit(KeyCode::Space)) || auto_jump;
    if player.grounded && jump {
        player.velocity.y = JUMP_SPEED;
        player.grounded = false;
    }
    if player.grounded {
        let from = pos + Vec3::Y * probe_up;
        let hit = cast_down(from, probe_up + STEP_HEIGHT);
        if verbose {
            debug!("snap: from {from:?} -> {hit:?}");
        }
        match hit {
            Some((d, normal, ground)) if normal.y >= MIN_GROUND_NORMAL_Y || d >= probe_up => {
                pos = from - Vec3::Y * d;
                player.velocity.y = 0.0;
                player.ground_normal = if normal.y >= MIN_GROUND_NORMAL_Y { normal } else { Vec3::Y };
                player.ground_entity = Some(ground);
            }
            Some((_, steep, _)) => {
                // Too steep ahead: keep the part of the motion that runs along the steep face
                // instead of cancelling the whole step (that made rough ground feel sticky).
                let delta = pos - start_pos;
                let n = Vec3::new(steep.x, 0.0, steep.z).normalize_or_zero();
                let slid = start_pos + (delta - n * delta.dot(n).max(0.0) * if n.length_squared() > 0.0 { 1.0 } else { 0.0 });
                let try_from = slid + Vec3::Y * probe_up;
                pos = match cast_down(try_from, probe_up + STEP_HEIGHT) {
                    Some((d, normal, ground)) if normal.y >= MIN_GROUND_NORMAL_Y => {
                        player.ground_normal = normal;
                        player.ground_entity = Some(ground);
                        try_from - Vec3::Y * d
                    }
                    _ => {
                        let from = start_pos + Vec3::Y * probe_up;
                        match cast_down(from, probe_up + STEP_HEIGHT) {
                            Some((d, _, ground)) => {
                                player.ground_entity = Some(ground);
                                from - Vec3::Y * d
                            }
                            None => start_pos,
                        }
                    }
                };
                player.velocity.y = 0.0;
            }
            None => {
                player.grounded = false;
                player.ground_normal = Vec3::Y;
                player.ground_entity = None;
                player.velocity.y = 0.0;
            }
        }
    } else {
        player.velocity.y -= GRAVITY * dt;
        let dy = player.velocity.y * dt;
        if dy < 0.0 {
            let from = pos + Vec3::Y * 0.05;
            match cast_down(from, 0.05 - dy) {
                Some((d, normal, ground)) => {
                    pos = from - Vec3::Y * d;
                    if normal.y >= MIN_GROUND_NORMAL_Y {
                        player.velocity.y = 0.0;
                        player.grounded = true;
                        player.ground_normal = normal;
                        player.ground_entity = Some(ground);
                    } else {
                        // Too steep to stand on: slide down along the surface.
                        player.velocity.y = player.velocity.y.max(-4.0);
                        let downhill = Vec3::new(normal.x, 0.0, normal.z).normalize_or_zero();
                        pos += downhill * SLIDE_SPEED * dt;
                    }
                }
                None => pos.y += dy,
            }
        } else {
            let up = spatial.cast_shape(&collider, pos, Quat::IDENTITY, Dir3::Y, &cast_config(dy), &filter);
            match up {
                Some(hit) => {
                    pos.y += hit.distance;
                    player.velocity.y = 0.0;
                }
                None => pos.y += dy,
            }
        }
    }
    // Never move into geometry: fall back to where we started, unless we were already stuck
    // there (then any motion, e.g. move_and_slide's depenetration, is the way out).
    if pos != start_pos && !free(pos) && free(start_pos) {
        pos = start_pos;
        player.velocity.y = player.velocity.y.min(0.0);
    }
    transform.translation = pos;

    if dir.length_squared() > 0.0 {
        player.facing = dir.x.atan2(dir.z);
    }
    turn_rig(&mut player, dt, &mut rigs);
}

/// Radians per second the body turns toward the movement direction (a half turn in ~0.2 s).
const TURN_RATE: f32 = 16.0;

fn turn_rig(player: &mut Player, dt: f32, rigs: &mut Query<&mut Transform, (With<CharacterRig>, Without<Player>)>) {
    let delta = (player.facing - player.rig_yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    let step = delta.abs().min(TURN_RATE * dt);
    player.rig_yaw = (player.rig_yaw + step.copysign(delta)).rem_euclid(std::f32::consts::TAU);
    for mut rig in rigs.iter_mut() {
        rig.rotation = Quat::from_rotation_y(player.rig_yaw);
    }
}

#[allow(clippy::too_many_arguments)]
fn orbit_camera(
    mode: Res<CameraMode>,
    ui_focus: Res<crate::app::UiFocus>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    spatial: SpatialQuery,
    players: Query<(Entity, &Transform), With<Player>>,
    mut camera: Query<(&mut Transform, &mut OrbitCamera), (With<MainCamera>, Without<Player>)>,
) {
    if *mode != CameraMode::Orbit {
        return;
    }
    let Ok((player_entity, player_tf)) = players.single() else {
        return;
    };
    let Ok((mut cam_tf, mut orbit)) = camera.single_mut() else {
        return;
    };
    if mouse.pressed(MouseButton::Right) {
        orbit.yaw -= motion.delta.x * 0.0035;
        orbit.pitch = (orbit.pitch - motion.delta.y * 0.0035).clamp(-1.4, 1.2);
    }
    // The wheel over a panel scrolls the panel, not the camera.
    if !ui_focus.pointer && scroll.delta.y.abs() > 0.0 {
        orbit.distance = (orbit.distance * (1.0 - 0.1 * scroll.delta.y.signum())).clamp(0.4, 25.0);
    }
    // Close in, the pivot climbs from the chest to the face so zooming lands on it.
    let close = ((2.5 - orbit.distance) / 2.0).clamp(0.0, 1.0);
    let pivot = player_tf.translation + Vec3::Y * (0.35 + 0.35 * close);
    let rotation = Quat::from_euler(EulerRot::YXZ, orbit.yaw, orbit.pitch, 0.0);
    let offset = rotation * Vec3::Z;
    let mut distance = orbit.distance;
    let filter = SpatialQueryFilter::from_excluded_entities([player_entity]).with_mask(SOLID_LAYER);
    if let Ok(dir) = Dir3::new(offset)
        && let Some(hit) = spatial.cast_ray(pivot, dir, distance, true, &filter)
    {
        distance = (hit.distance - 0.3).max(0.5);
    }
    cam_tf.translation = pivot + offset * distance;
    cam_tf.look_at(pivot, Vec3::Y);
}

#[derive(Resource, Default)]
struct TraceTimer(f32);

/// Once a second at debug level: where the player is and what it is doing.
fn trace_player(
    time: Res<Time>,
    mut timer: ResMut<TraceTimer>,
    players: Query<(&Transform, &Player)>,
    animators: Query<&crate::world::character::CharacterAnimator>,
) {
    timer.0 += time.delta_secs();
    // FFL_TRACE_INTERVAL=0 traces every frame.
    let interval = std::env::var("FFL_TRACE_INTERVAL").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.5);
    if timer.0 < interval {
        return;
    }
    timer.0 = 0.0;
    if let Ok((t, p)) = players.single() {
        let (clip, at, blend) = animators
            .single()
            .ok()
            .map(|a| {
                let from = a.previous.as_ref().map(|(n, _, _)| n.as_str()).unwrap_or("pose");
                (a.current.clone().unwrap_or_else(|| "-".into()), a.time, if a.blend < 1.0 { format!(" blend {:.2} from {from}", a.blend) } else { String::new() })
            })
            .unwrap_or_else(|| ("-".into(), 0.0, String::new()));
        let water = if p.swimming {
            format!(" swimming{} depth={:.2}", if p.submerged { " (under)" } else { "" }, p.water_depth)
        } else if p.water_depth > 0.0 {
            format!(" wading depth={:.2}", p.water_depth)
        } else {
            String::new()
        };
        debug!(
            "player pos=({:.2}, {:.2}, {:.2}) grounded={} vy={:.2} moving={} clip={clip} t={at:.2}{blend}{water}",
            t.translation.x, t.translation.y, t.translation.z, p.grounded, p.velocity.y, p.moving
        );
    }
}
