//! Character rig: `CharacterModel` → bone entities, skinned/attached meshes, clip playback.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};
use ffl_core::{Attach, CharacterModel, Clip, ClipPose, Engine};

use crate::app::{AppState, Engines, RuntimeOptions, WorldRequest};
use crate::convert::{build_mesh, trs_to_transform};
use crate::world::materials::{MaterialAssets, MaterialCache, RuntimeMaterial};
use crate::world::shaders::{EngineMaterial, ShaderRegistry};
use crate::world::player::{CharacterRig, PlaceholderVisual, Player};

/// The character chain (`poll_character_load`, `attach_character`, `animate_character`);
/// the sound systems run after it so they see this frame's clip time.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CharacterSystems;

/// Footstep and voice banks of the attached character (`CharacterModel::footsteps/voice`),
/// read by `world::sound::clip_events`.
#[derive(Component, Default)]
pub struct CharacterSounds {
    pub footsteps: Option<ffl_core::FootstepSet>,
    pub voice: Option<ffl_core::VoiceSet>,
}

#[derive(Resource, Default)]
pub struct CharacterLoad {
    task: Option<Task<Result<CharacterModel>>>,
    loaded: Option<Arc<Mutex<Option<CharacterModel>>>>,
    attached: bool,
}

#[derive(Resource, Default)]
pub struct CharacterStatus {
    pub phase: String,
    pub errors: Vec<String>,
    /// Content report summary (protected game assets vs shareable mods).
    pub content: String,
    pub shareable: bool,
}

#[derive(Component)]
pub struct Bone {
    pub name: String,
    pub index: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct BonePose {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl From<&ClipPose> for BonePose {
    fn from(p: &ClipPose) -> Self {
        Self {
            translation: Vec3::from(p.translation),
            rotation: Quat::from_xyzw(p.rotation[0], p.rotation[1], p.rotation[2], p.rotation[3]),
            scale: Vec3::from(p.scale),
        }
    }
}

/// Sample a clip at `time`. Looping clips wrap (the last frame blends into the first); one-shot
/// clips clamp at their last frame instead (wrapping there made the end of a draw animation
/// swing the arm back toward the clip's start pose within a single frame).
fn sample_clip(clip: &Clip, time: f32, looping: bool, out: &mut Vec<BonePose>) {
    out.clear();
    if clip.frames.is_empty() {
        return;
    }
    let last = clip.frames.len() - 1;
    let (i0, i1, a) = if looping {
        let t = if clip.duration > 0.0 { time.rem_euclid(clip.duration) } else { 0.0 };
        let f = t * clip.fps;
        let i0 = (f.floor() as usize).min(last);
        (i0, (i0 + 1) % clip.frames.len(), f - i0 as f32)
    } else {
        let f = (time.max(0.0) * clip.fps).min(last as f32);
        let i0 = (f.floor() as usize).min(last);
        (i0, (i0 + 1).min(last), f - i0 as f32)
    };
    for (pa, pb) in clip.frames[i0].iter().zip(clip.frames[i1].iter()) {
        let (pa, pb) = (BonePose::from(pa), BonePose::from(pb));
        out.push(BonePose {
            translation: pa.translation.lerp(pb.translation, a),
            rotation: pa.rotation.slerp(pb.rotation, a),
            scale: pa.scale.lerp(pb.scale, a),
        });
    }
}

#[derive(Component)]
pub struct CharacterSkeleton {
    pub entities: Vec<Entity>,
    pub reference: Vec<BonePose>,
    pub names: Vec<String>,
}

#[derive(Component)]
pub struct CharacterAnimator {
    pub clips: Arc<HashMap<String, Clip>>,
    pub locomotion: ffl_core::Locomotion,
    /// Battle-stance locomotion used while `armed` (weapon drawn).
    pub armed_locomotion: Option<ffl_core::Locomotion>,
    /// Set by the addon system when a weapon is drawn.
    pub armed: bool,
    pub current: Option<String>,
    /// The current clip plays once (emote, draw/sheathe): it clamps at its last frame and the
    /// way out of it blends longer. Kept here because the emote state is cleared before the
    /// idle is requested.
    pub current_one_shot: bool,
    pub time: f32,
    /// Clip being crossfaded out: (name, time offset, looping). It keeps advancing during the
    /// blend so locomotion cycles fade into each other instead of freezing.
    pub previous: Option<(String, f32, bool)>,
    /// The pose that was on screen when the current transition started: the blend source for
    /// every bone the previous clip does not animate, and for the whole body when a
    /// transition interrupts another one (the previous clip is dropped then).
    pub blend_from: Vec<BonePose>,
    /// The pose written last frame.
    pub last_pose: Vec<BonePose>,
    /// Blend length for the current transition (longer after one-shot clips).
    pub blend_seconds: f32,
    pub blend: f32,
    /// Clip that replaces the locomotion idle (an idle-pose action).
    pub idle_override: Option<String>,
    /// Emote in progress: (clip name, loops). One-shot emotes end at the clip's duration.
    pub emote: Option<(String, bool)>,
    /// Seconds the player has been airborne (spawn settling must not cancel emotes).
    air_time: f32,
    /// Airborne last frame (to detect take-off and landing).
    was_airborne: bool,
    /// Remaining seconds of the landing clip.
    land_timer: f32,
    /// Bones the addon system re-parented (drawn FFXI weapons): their clip tracks are
    /// ignored and their transform left as set (identity under the new parent).
    pub pinned: Vec<usize>,
    /// Clips applied over the locomotion every frame for their own bones (a face's resting
    /// expression), looping on the character's clock.
    pub overlays: Vec<String>,
    /// One-shot played over the overlays every few seconds (a blink): clip, time left until
    /// the next one, seconds into the current one (`None` = not blinking).
    pub blink: Option<(String, f32, Option<f32>)>,
    scratch_a: Vec<BonePose>,
    scratch_b: Vec<BonePose>,
}

/// Crossfade length between looping clips (idle, walk, run); one-shots ease out longer.
const BLEND_SECONDS: f32 = 0.22;
const ONE_SHOT_BLEND_SECONDS: f32 = 0.3;

impl CharacterAnimator {
    /// Whether the current clip loops (everything except one-shot emotes/transitions).
    fn current_loops(&self) -> bool {
        !self.current_one_shot
    }

    /// Whether a clip is a locomotion cycle (walk/run/sprint of either locomotion set).
    fn is_cycle(&self, name: &str) -> bool {
        [Some(&self.locomotion), self.armed_locomotion.as_ref()]
            .into_iter()
            .flatten()
            .any(|l| [&l.walk, &l.run, &l.sprint].into_iter().flatten().any(|c| c == name))
    }

    fn play(&mut self, name: &str) {
        self.play_with(name, false);
    }

    fn play_with(&mut self, name: &str, one_shot: bool) {
        if self.current.as_deref() == Some(name) || !self.clips.contains_key(name) {
            return;
        }
        let was_one_shot = self.current_one_shot;
        let mut start_time = 0.0;
        if let Some(cur) = self.current.take() {
            // Interrupting a blend: fade from the pose on screen instead of chaining clips.
            let mid_blend = self.previous.is_some() && self.blend < 1.0;
            self.previous = (!mid_blend).then(|| (cur.clone(), self.time, !was_one_shot));
            self.blend_from = self.last_pose.clone();
            self.blend_seconds = if was_one_shot { ONE_SHOT_BLEND_SECONDS } else { BLEND_SECONDS };
            self.blend = 0.0;
            // Locomotion cycles hand their phase over so the feet keep their rhythm.
            if self.is_cycle(&cur) && self.is_cycle(name)
                && let (Some(from), Some(to)) = (self.clips.get(&cur), self.clips.get(name))
                && from.duration > 0.0
            {
                start_time = (self.time / from.duration).rem_euclid(1.0) * to.duration;
            }
        }
        self.current = Some(name.to_string());
        self.current_one_shot = one_shot;
        self.time = start_time;
    }

    /// Start (or restart) an emote clip.
    pub fn start_emote(&mut self, clip: String, looped: bool) {
        if !self.clips.contains_key(&clip) {
            return;
        }
        if self.current.as_deref() == Some(clip.as_str()) {
            // Restart from the beginning, blending from the pose on screen.
            self.previous = None;
            self.blend_from = self.last_pose.clone();
            self.blend_seconds = ONE_SHOT_BLEND_SECONDS;
            self.blend = 0.0;
            self.time = 0.0;
            self.current_one_shot = !looped;
        } else {
            self.play_with(&clip, !looped);
        }
        self.emote = Some((clip, looped));
    }
}

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CharacterLoad>()
            .init_resource::<CharacterStatus>()
            .add_systems(OnEnter(AppState::World), start_character_load)
            .add_systems(
                Update,
                (poll_character_load, attach_character, animate_character)
                    .chain()
                    .in_set(CharacterSystems)
                    .run_if(in_state(AppState::World)),
            )
            .add_systems(Update, animate_character.run_if(in_state(AppState::Launcher)));
    }
}

fn start_character_load(
    engines: Res<Engines>,
    request: Res<WorldRequest>,
    opts: Res<RuntimeOptions>,
    mut load: ResMut<CharacterLoad>,
    mut status: ResMut<CharacterStatus>,
) {
    *load = CharacterLoad::default();
    *status = CharacterStatus::default();
    let Some(preset) = request.character.clone() else {
        status.phase = "no character selected".into();
        return;
    };
    if opts.single_model.is_some() {
        return;
    }
    let Some(engine) = engines.get(&preset.engine) else {
        status.phase = format!("unknown engine {}", preset.engine);
        return;
    };
    status.phase = format!("loading {} ({})", preset.name, preset.engine);
    let engine: Arc<dyn Engine> = engine;
    load.task = Some(AsyncComputeTaskPool::get().spawn(async move { engine.load_character(&preset) }));
}

fn poll_character_load(mut load: ResMut<CharacterLoad>, mut status: ResMut<CharacterStatus>) {
    let Some(task) = load.task.as_mut() else {
        return;
    };
    let Some(result) = block_on(poll_once(task)) else {
        return;
    };
    load.task = None;
    match result {
        Ok(c) => {
            status.phase = format!("{} loaded ({} parts, {} bones, {} clips)", c.name, c.parts.len(), c.skeleton.bones.len(), c.clips.len());
            status.content = c.content.summary();
            status.shareable = c.content.shareable();
            info!("{}  content: {}", status.phase, status.content);
            for n in &c.notes {
                debug!("character: {n}");
            }
            load.loaded = Some(Arc::new(Mutex::new(Some(c))));
        }
        Err(err) => {
            error!("character load failed: {err:#}");
            status.phase = format!("character failed: {err:#}");
            status.errors.push(format!("{err:#}"));
        }
    }
}

fn bone_transform(b: &ffl_core::BoneData) -> Transform {
    Transform {
        translation: Vec3::from(b.translation),
        rotation: Quat::from_xyzw(b.rotation[0], b.rotation[1], b.rotation[2], b.rotation[3]).normalize(),
        scale: Vec3::from(b.scale),
    }
}

/// The asset stores a rig spawn needs.
pub struct RigAssets<'a> {
    pub meshes: &'a mut Assets<Mesh>,
    pub images: &'a mut Assets<Image>,
    pub standard: &'a mut Assets<StandardMaterial>,
    pub engine: &'a mut Assets<EngineMaterial>,
    pub registry: &'a ShaderRegistry,
    pub inverse_bindposes: &'a mut Assets<SkinnedMeshInverseBindposes>,
    pub cache: &'a mut MaterialCache,
}

/// Build a character under `rig`: bone entities, skinned and rigidly attached meshes, the
/// animator. `root` is the rig's own transform (feet at the rig origin; the world places the
/// feet below the player capsule's centre). Returns the number of meshes spawned. Shared by
/// the world (player rig) and the launcher preview.
pub fn spawn_rig(commands: &mut Commands, rig: Entity, character: &CharacterModel, assets: &mut RigAssets, root: Transform, normal_maps: bool, drawn: bool) -> usize {
    let bones = &character.skeleton.bones;
    let mut world: Vec<Mat4> = vec![Mat4::IDENTITY; bones.len()];
    let mut entities: Vec<Entity> = Vec::with_capacity(bones.len());
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (i, b) in bones.iter().enumerate() {
        let local = bone_transform(b);
        let parent_world = b.parent.map(|p| world[p]).unwrap_or(Mat4::IDENTITY);
        world[i] = parent_world * local.to_matrix();
        let parent_entity = b.parent.map(|p| entities[p]).unwrap_or(rig);
        let e = commands
            .spawn((
                Name::new(b.name.clone()),
                Bone {
                    name: b.name.clone(),
                    index: i,
                },
                local,
                Visibility::default(),
                ChildOf(parent_entity),
            ))
            .id();
        entities.push(e);
        by_name.entry(b.name.clone()).or_insert(i);
    }
    let inverse_world: Vec<Mat4> = world.iter().map(|m| m.inverse()).collect();
    for probe in ["j_kao", "j_f_face", "j_f_ulip_01_l", "j_f_eye_l", "j_kami_a"] {
        if let Some(&i) = by_name.get(probe) {
            let p = world[i].to_scale_rotation_translation();
            debug!("bone {probe}: world pos {:?} local {:?} parent {:?}", p.2, bones[i].translation, bones[i].parent.map(|p| bones[p].name.as_str()));
        }
    }
    for part in &character.parts {
        if part.name == "face" {
            let m = &part.model;
            debug!("face model bounds {:?}..{:?}, {} meshes, bone table names {:?}", m.bounds_min, m.bounds_max, m.meshes.len(), m.meshes.first().map(|x| x.bone_table.iter().take(6).map(|b| m.bone_names[*b as usize].clone()).collect::<Vec<_>>()));
        }
    }
    let reference: Vec<BonePose> = bones
        .iter()
        .map(|b| {
            let t = bone_transform(b);
            BonePose {
                translation: t.translation,
                rotation: t.rotation,
                scale: t.scale,
            }
        })
        .collect();

    commands.entity(rig).insert((
        root.with_scale(root.scale * character.scale),
        CharacterSkeleton {
            names: character.skeleton.bones.iter().map(|b| b.name.clone()).collect(),
            entities: entities.clone(),
            reference,
        },
        CharacterAnimator {
            clips: Arc::new(character.clips.clone()),
            locomotion: character.locomotion.clone(),
            armed_locomotion: character.armed_locomotion.clone(),
            armed: false,
            current: None,
            current_one_shot: false,
            time: 0.0,
            previous: None,
            blend_from: Vec::new(),
            last_pose: Vec::new(),
            blend_seconds: BLEND_SECONDS,
            blend: 1.0,
            idle_override: None,
            emote: None,
            air_time: 0.0,
            was_airborne: false,
            land_timer: 0.0,
            pinned: Vec::new(),
            overlays: character.overlays.clone(),
            blink: character.blink.clone().map(|c| (c, 3.0, None)),
            scratch_a: Vec::new(),
            scratch_b: Vec::new(),
        },
        CharacterSounds {
            footsteps: character.footsteps.clone(),
            voice: character.voice.clone(),
        },
    ));

    let mut spawned = 0;
    let mut mat_assets = MaterialAssets {
        images: assets.images,
        standard: assets.standard,
        engine: assets.engine,
        registry: assets.registry,
    };
    for part in &character.parts {
        let mat_handles: Vec<RuntimeMaterial> = part
            .model
            .materials
            .iter()
            .map(|d| assets.cache.material(&mut mat_assets, d))
            .collect();
        let skip: Vec<bool> = part.model.materials.iter().map(|d| d.is_skip()).collect();
        // Addon parts start stowed like in the game (`--drawn` starts them in hand).
        let placement = part.addon.as_deref().and_then(|a| {
            let def = character.addons.iter().find(|d| d.id == a)?;
            let id = if drawn { def.active.as_deref() } else { def.stowed.as_deref().or(def.active.as_deref()) }?;
            def.placement(id).cloned()
        });
        let (attach, offset) = match &placement {
            Some(p) => (&p.attach, trs_to_transform(&p.offset)),
            None => (&part.attach, Transform::IDENTITY),
        };
        let attach_bone = match attach {
            Attach::Bone(name) => Some(by_name.get(name).map(|&i| entities[i]).unwrap_or(rig)),
            Attach::Skinned | Attach::Reparent { .. } => None,
        };
        for mesh in &part.model.meshes {
            if mesh.indices.is_empty() || skip.get(mesh.material_index).copied().unwrap_or(false) {
                continue;
            }
            let material = mat_handles
                .get(mesh.material_index)
                .cloned()
                .unwrap_or_else(|| RuntimeMaterial::Standard(mat_assets.standard.add(StandardMaterial::from(Color::srgb(1.0, 0.0, 1.0)))));
            let name = Name::new(format!("{} {}", part.name, part.model.key.rsplit('/').next().unwrap_or("")));
            if let Some(bone_entity) = attach_bone {
                let mut e = commands.spawn((
                    name,
                    Mesh3d(assets.meshes.add(build_mesh(mesh, normal_maps, false, false))),
                    offset,
                    Visibility::default(),
                    ChildOf(bone_entity),
                ));
                material.insert(&mut e);
                if let Some(a) = &part.addon {
                    e.insert(crate::world::actions::AddonMember {
                        addon: a.clone(),
                        placement: placement.as_ref().map(|p| p.id.clone()),
                    });
                }
                spawned += 1;
                continue;
            }
            let skinned = mesh.joints.is_some() && !mesh.bone_table.is_empty();
            let mut entity = commands.spawn((
                name,
                Mesh3d(assets.meshes.add(build_mesh(mesh, normal_maps, skinned, false))),
                Transform::IDENTITY,
                Visibility::default(),
                ChildOf(rig),
            ));
            material.insert(&mut entity);
            if let Some(a) = &part.addon {
                entity.insert(crate::world::actions::AddonMember {
                    addon: a.clone(),
                    placement: None,
                });
            }
            if skinned {
                let mut joints = Vec::with_capacity(mesh.bone_table.len());
                let mut ibp = Vec::with_capacity(mesh.bone_table.len());
                for &bi in &mesh.bone_table {
                    let bone_name = part.model.bone_names.get(bi as usize).map(String::as_str).unwrap_or("");
                    let idx = by_name.get(bone_name).copied().unwrap_or_else(|| {
                        let fallback = if part.name == "hair" || part.name == "face" || part.name == "head" {
                            by_name.get("j_kao").copied().unwrap_or(0)
                        } else {
                            0
                        };
                        debug!("bone {bone_name} not in skeleton; using {}", bones[fallback].name);
                        fallback
                    });
                    joints.push(entities[idx]);
                    ibp.push(inverse_world[idx]);
                }
                entity.insert(SkinnedMesh {
                    inverse_bindposes: assets.inverse_bindposes.add(SkinnedMeshInverseBindposes::from(ibp)),
                    joints,
                });
            }
            spawned += 1;
        }
    }
    spawned
}

#[allow(clippy::too_many_arguments)]
fn attach_character(
    mut commands: Commands,
    opts: Res<RuntimeOptions>,
    mut load: ResMut<CharacterLoad>,
    mut status: ResMut<CharacterStatus>,
    mut cache: ResMut<MaterialCache>,
    rigs: Query<Entity, With<CharacterRig>>,
    placeholders: Query<Entity, With<PlaceholderVisual>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut engine_materials: ResMut<Assets<EngineMaterial>>,
    registry: Res<ShaderRegistry>,
    mut inverse_bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut actions: ResMut<crate::world::actions::ActionRuntime>,
    mut addons: ResMut<crate::world::actions::AddonState>,
    request: Res<WorldRequest>,
) {
    if load.attached {
        return;
    }
    let Some(slot) = load.loaded.clone() else {
        return;
    };
    let Ok(rig) = rigs.single() else {
        return;
    };
    let Some(character) = slot.lock().unwrap().take() else {
        return;
    };
    load.attached = true;
    actions.reset(character.actions.clone(), request.character.clone());
    addons.reset(&character.addons, &opts.addons, !opts.drawn);
    for e in &placeholders {
        commands.entity(e).despawn();
    }
    let mut assets = RigAssets {
        meshes: &mut meshes,
        images: &mut images,
        standard: &mut materials,
        engine: &mut engine_materials,
        registry: &registry,
        inverse_bindposes: &mut inverse_bindposes,
        cache: &mut cache,
    };
    // The player entity is the capsule's centre; the rig puts the feet at its bottom.
    let root = Transform::from_translation(Vec3::new(0.0, -0.85, 0.0));
    let spawned = spawn_rig(&mut commands, rig, &character, &mut assets, root, opts.normal_maps, opts.drawn);
    status.phase = format!("{} attached: {spawned} meshes, {} bones", character.name, character.skeleton.bones.len());
    info!("{}", status.phase);
}

/// Advance every rig's animator: the player's rig follows the player state (locomotion,
/// jumps), any other rig (the launcher preview) idles or plays its emote.
fn animate_character(
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    players: Query<&Player>,
    mut rigs: Query<(&CharacterSkeleton, &mut CharacterAnimator, Has<CharacterRig>)>,
    mut bones: Query<&mut Transform, With<Bone>>,
) {
    for (skeleton, mut animator, is_player) in &mut rigs {
        let player = if is_player { players.single().ok() } else { None };
        animate_rig(&time, &opts, player, skeleton, &mut animator, &mut bones);
    }
}

fn animate_rig(time: &Time, opts: &RuntimeOptions, player: Option<&Player>, skeleton: &CharacterSkeleton, animator: &mut CharacterAnimator, bones: &mut Query<&mut Transform, With<Bone>>) {
    // Battle stance while armed, per clip falling back to the normal set.
    let loco = match (&animator.armed_locomotion, animator.armed) {
        (Some(a), true) => ffl_core::Locomotion {
            idle: a.idle.clone().or(animator.locomotion.idle.clone()),
            walk: a.walk.clone().or(animator.locomotion.walk.clone()),
            run: a.run.clone().or(animator.locomotion.run.clone()),
            sprint: a.sprint.clone().or(animator.locomotion.sprint.clone()),
            fall: a.fall.clone().or(animator.locomotion.fall.clone()),
            jump: a.jump.clone().or(animator.locomotion.jump.clone()),
            land: a.land.clone().or(animator.locomotion.land.clone()),
            swim_idle: a.swim_idle.clone().or(animator.locomotion.swim_idle.clone()),
            swim_move: a.swim_move.clone().or(animator.locomotion.swim_move.clone()),
            swim_sprint: a.swim_sprint.clone().or(animator.locomotion.swim_sprint.clone()),
            dive_idle: a.dive_idle.clone().or(animator.locomotion.dive_idle.clone()),
            dive_move: a.dive_move.clone().or(animator.locomotion.dive_move.clone()),
        },
        _ => animator.locomotion.clone(),
    };
    let dt = time.delta_secs();
    let air_time_before = animator.air_time;
    match player {
        Some(p) if !p.grounded && !p.swimming => animator.air_time += dt,
        _ => animator.air_time = 0.0,
    }
    let busy = player.is_some_and(|p| p.moving) || animator.air_time > 0.3;
    // Movement cancels emotes; one-shot emotes end when their clip does.
    if busy {
        animator.emote = None;
    } else if let Some((clip, looped)) = animator.emote.clone()
        && !looped
        && animator.current.as_deref() == Some(clip.as_str())
        && animator.clips.get(&clip).is_some_and(|c| animator.time >= c.duration)
    {
        animator.emote = None;
    }
    let idle = animator.idle_override.clone().or(loco.idle.clone());
    let emote = animator.emote.as_ref().map(|(c, _)| c.clone());
    // Jump phases: take-off clip once, then the airborne loop, then the landing clip. The
    // grounded flag flickers on uneven ground; without a grace period that restarted the
    // take-off clip every frame (a stutter that reads as a looping landing). A real jump has
    // upward velocity and starts at once.
    let airborne = player.is_some_and(|p| !p.grounded && !p.swimming && (animator.air_time > 0.12 || p.velocity.y > 1.0));
    let mut jump_clip = None;
    if airborne {
        if !animator.was_airborne {
            animator.was_airborne = true;
            animator.land_timer = 0.0;
            if let Some(j) = &loco.jump {
                animator.play(j);
            }
        }
        let takeoff_running = loco.jump.as_deref().is_some_and(|j| {
            animator.current.as_deref() == Some(j) && animator.clips.get(j).is_some_and(|c| animator.time < c.duration)
        });
        jump_clip = if takeoff_running { loco.jump.clone() } else { loco.fall.clone().or(loco.jump.clone()) };
    } else if animator.was_airborne {
        animator.was_airborne = false;
        if air_time_before > 0.15
            && let Some(l) = &loco.land
            && let Some(c) = animator.clips.get(l)
        {
            animator.land_timer = c.duration;
        }
    }
    let landing = if animator.land_timer > 0.0 && !player.is_some_and(|p| p.moving) {
        animator.land_timer -= dt;
        loco.land.clone()
    } else {
        animator.land_timer = 0.0;
        None
    };
    let wanted = match player {
        Some(p) if p.flying => loco.fall.clone().or(loco.jump.clone()).or(idle.clone()),
        // Swimming: the game's swim cycles when the engine found them, else the walk cycle
        // and the idle stand in (FFXIV's swim packs are not located yet).
        Some(p) if p.swimming && p.submerged && p.moving => loco.dive_move.clone().or(loco.swim_move.clone()).or(loco.walk.clone()).or(loco.run.clone()),
        Some(p) if p.swimming && p.submerged => loco.dive_idle.clone().or(loco.swim_idle.clone()).or(idle.clone()),
        Some(p) if p.swimming && p.moving && p.sprinting => loco.swim_sprint.clone().or(loco.swim_move.clone()).or(loco.walk.clone()).or(loco.run.clone()),
        Some(p) if p.swimming && p.moving => loco.swim_move.clone().or(loco.walk.clone()).or(loco.run.clone()),
        Some(p) if p.swimming => loco.swim_idle.clone().or(idle.clone()),
        Some(_) if jump_clip.is_some() => jump_clip,
        Some(_) if landing.is_some() => landing,
        Some(p) if p.moving && p.sprinting => loco.sprint.clone().or(loco.run.clone()).or(loco.walk.clone()),
        Some(p) if p.moving && p.walking => loco.walk.clone().or(loco.run.clone()),
        Some(p) if p.moving => loco.run.clone().or(loco.walk.clone()),
        _ => emote.or(idle),
    };
    let wanted = if player.is_some() { opts.anim.clone().or(wanted) } else { wanted };
    if let Some(w) = wanted {
        // Resuming a one-shot emote after an interruption (a landing, a draw) must keep it a
        // one-shot: as a loop it wrapped to its first frame before the idle took over and
        // replayed its start events.
        let one_shot = animator.emote.as_ref().is_some_and(|(c, looped)| *c == w && !*looped);
        animator.play_with(&w, one_shot);
    }
    animator.time += dt;
    if let Some(t) = opts.anim_time.filter(|_| player.is_some()) {
        animator.time = t;
        animator.previous = None;
        animator.blend = 1.0;
    }
    if animator.blend < 1.0 {
        animator.blend = (animator.blend + dt / animator.blend_seconds).min(1.0);
        if animator.blend >= 1.0 {
            animator.previous = None;
            animator.blend_from.clear();
        }
    }
    let Some(current) = animator.current.clone() else {
        return;
    };
    let clips = animator.clips.clone();
    let Some(clip) = clips.get(&current) else {
        return;
    };
    let mut pose_a = std::mem::take(&mut animator.scratch_a);
    let mut pose_b = std::mem::take(&mut animator.scratch_b);
    let looping = animator.current_loops();
    sample_clip(clip, animator.time, looping, &mut pose_a);
    let mut final_pose: Vec<BonePose> = skeleton.reference.clone();
    for (track, pose) in pose_a.iter().enumerate() {
        if let Some(&bone) = clip.track_to_bone.get(track)
            && bone < final_pose.len()
        {
            final_pose[bone] = *pose;
        }
    }
    if animator.blend < 1.0 && animator.blend_from.len() == final_pose.len() {
        // Blend source: the previous clip where it animates a bone (still advancing, so a
        // run keeps cycling while the idle fades in), the frozen pose elsewhere.
        let mut from = std::mem::take(&mut animator.blend_from);
        if let Some((prev_name, prev_time, prev_loops)) = animator.previous.clone()
            && let Some(prev) = clips.get(&prev_name)
        {
            sample_clip(prev, prev_time + animator.time, prev_loops, &mut pose_b);
            for (track, pose) in pose_b.iter().enumerate() {
                if let Some(&bone) = prev.track_to_bone.get(track)
                    && bone < from.len()
                {
                    from[bone] = *pose;
                }
            }
        }
        // Ease in and out: a linear fade shows the switch on the feet.
        let b = animator.blend;
        let w = b * b * (3.0 - 2.0 * b);
        for (bone, cur) in final_pose.iter_mut().enumerate() {
            let src = from[bone];
            *cur = BonePose {
                translation: src.translation.lerp(cur.translation, w),
                rotation: src.rotation.slerp(cur.rotation, w),
                scale: cur.scale,
            };
        }
        animator.blend_from = from;
    }
    // Overlays own their bones (the body clips never touch the face); additive ones (the
    // face expressions) are deltas on the reference pose.
    let apply_layer = |clip: &Clip, sampled: &[BonePose], final_pose: &mut Vec<BonePose>, reference: &[BonePose]| {
        for (track, pose) in sampled.iter().enumerate() {
            if let Some(&bone) = clip.track_to_bone.get(track)
                && bone < final_pose.len()
            {
                final_pose[bone] = if clip.additive {
                    let r = reference[bone];
                    BonePose {
                        translation: r.translation + pose.translation,
                        rotation: (r.rotation * pose.rotation).normalize(),
                        scale: r.scale * pose.scale,
                    }
                } else {
                    *pose
                };
            }
        }
    };
    for name in animator.overlays.clone() {
        if let Some(clip) = clips.get(&name) {
            sample_clip(clip, animator.time, true, &mut pose_b);
            apply_layer(clip, &pose_b, &mut final_pose, &skeleton.reference);
        }
    }
    if let Some((name, until_next, running)) = animator.blink.as_mut()
        && let Some(clip) = clips.get(name.as_str())
    {
        match running {
            Some(t) => {
                *t += dt;
                if *t >= clip.duration {
                    *running = None;
                    *until_next = 2.5 + (animator.time * 7.31).fract() * 4.0;
                } else {
                    sample_clip(clip, *t, false, &mut pose_b);
                    apply_layer(clip, &pose_b, &mut final_pose, &skeleton.reference);
                }
            }
            None => {
                *until_next -= dt;
                if *until_next <= 0.0 {
                    *running = Some(0.0);
                }
            }
        }
    }
    if let Ok(v) = std::env::var("FFL_JAW_TEST")
        && let (bone_name, v) = v.split_once(':').map(|(b, r)| (b.to_string(), r.to_string())).unwrap_or(("j_ago".into(), v.clone()))
        && let Some(i) = skeleton.names.iter().position(|n| *n == bone_name)
    {
        // Verification aid `FFL_JAW_TEST=[<bone>:]<axis>,<degrees>`: rotate the jaw (or any
        // bone) about one of its local axes on top of its pose.
        let (axis, deg) = v.split_once(',').map(|(a, d)| (a.to_string(), d.parse::<f32>().unwrap_or(-15.0))).unwrap_or(("z".into(), v.parse().unwrap_or(-15.0)));
        let q = match axis.as_str() {
            "x" => Quat::from_rotation_x(deg.to_radians()),
            "y" => Quat::from_rotation_y(deg.to_radians()),
            _ => Quat::from_rotation_z(deg.to_radians()),
        };
        final_pose[i].rotation = (final_pose[i].rotation * q).normalize();
    }
    animator.last_pose.clone_from(&final_pose);
    for (i, entity) in skeleton.entities.iter().enumerate() {
        if animator.pinned.contains(&i) {
            continue;
        }
        if let Ok(mut t) = bones.get_mut(*entity) {
            let p = final_pose[i];
            t.translation = p.translation;
            t.rotation = p.rotation;
            // Havok bone scale is not inherited; Bevy's hierarchy would propagate it. Keep rest scale.
            t.scale = skeleton.reference[i].scale;
        }
    }
    animator.scratch_a = pose_a;
    animator.scratch_b = pose_b;
}
