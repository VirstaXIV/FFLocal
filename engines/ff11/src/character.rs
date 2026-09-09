//! FFXI player characters as `ffl_core::CharacterModel`: the race's skeleton and motions
//! (base DAT + upper-body and waist packs), the face DAT and one gear DAT per slot, all
//! rigged to the same skeleton.
//!
//! Skinning: a vertex sits in the local space of one or two joints (two-joint positions are
//! pre-weighted), world = Σ rot(q_i, s_i·p_i) + w_i·t_i. The meshes are converted to bind
//! model space with the bind-pose joint transforms and skinned by the runtime with the
//! joints' inverse bind matrices, which gives the same result when both joints agree on the
//! bind position (they do: the file stores one point twice).
//!
//! Animations key rotations relative to the bind rotation (`q = anim × bind`), translations
//! as offsets from the bind translation, and the root's translation is swizzled
//! `(x, y, z) → (−z, y, x)`. Clips are pre-composed into absolute local poses here; the
//! runtime samples them like any other clip. A clip's body regions are separate 4-char
//! animations (`idl0` lower body, `idl1` upper body from the base+1 pack, `idl2` waist from
//! the base+3/+4 pack) merged into one clip named `idl`.
//!
//! Y-down → Y-up: entity DATs are drawn through a 180° turn about X (xi-model-viewer
//! `ENTITY_ROT`), plus a −90° turn about Y so the character faces +Z like FF14 rigs; both
//! are folded into the root joints' local poses (bind and every frame).
//!
//! Weapons are skinned to grip joints (4/5 on Hume ♂) that sit at the skeleton root in the
//! bind pose and are carried to the hip/back by the waist motion pack (`idl2`, `wlk2`, ...):
//! that is the sheathed look. The battle DAT of the weapon's animation type (`info` byte 3)
//! holds the draw (`in 0` routine, clip `inb?`/`ind?`/...: the waist part carries the grip
//! joint from the hip into the hand by the clip's end), the sheathe (`out0`: the grip leaves
//! the hand at 0.6 s), the battle idle `btl` and the engaged upper-body walk/run parts; the
//! waist parts live in the skirt pack `MotionBFileNo + num`. While drawn, the runtime
//! re-parents the grip joint onto the hand joint (`Attach::Reparent`, references 127 main /
//! 126 sub) so the weapon follows the hand through clips that do not key the grip.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, anyhow};
use ffl_core::{
    ActionCategory, ActionDef, AddonDef, AddonKind, AddonPlacement, AddonTransition, Attach, BoneData, CharacterModel, CharacterPart, Clip, ClipEvent, ClipEventKind, ClipPose,
    ContentReport, Foot, Locomotion, MaterialDesc, MeshData, ModelData, Provenance, SkeletonData, SoundCue, TextureRef, Trs,
};
use glam::{Quat, Vec3};

use crate::dat::Install;
use crate::entity::{self, Animation, EntityDat, Piece, Routine, SkinVertex, SkinnedMesh};
use crate::pc::{Look, Race, Slot};

/// Joint reference indices of the hands (weapon attach points).
const REF_LEFT_HAND: usize = 126;
const REF_RIGHT_HAND: usize = 127;

/// The clip that plays in each locomotion state.
const IDLE: &str = "idl";
const WALK: &str = "wlk";
const RUN: &str = "run";
const JUMP: &str = "jmp";

/// English names of the emote clips (4-char ids minus the body-region digit).
pub fn emote_name(clip: &str) -> Option<&'static str> {
    Some(match clip {
        "bow" => "Bow",
        "poi" => "Point",
        "sl1" => "Salute",
        "sl2" => "Salute 2",
        "sl3" => "Salute 3",
        "lau" => "Laugh",
        "wee" => "Cry",
        "kne" => "Kneel",
        "den" => "No",
        "nod" => "Yes",
        "wav" => "Wave",
        "wel" => "Welcome",
        "gla" => "Joy",
        "clp" => "Clap",
        "che" => "Cheer",
        "pok" => "Poke",
        "sta" => "Stagger",
        "sur" => "Surprised",
        "cmf" => "Comfort",
        "sig" => "Sigh",
        "why" => "Huh",
        "blu" => "Blush",
        "ang" => "Angry",
        "ups" => "Upset",
        "pan" => "Panic",
        "thk" => "Think",
        "gut" => "Doubt",
        "fum" => "Fume",
        "rs0" => "Farewell",
        "xe0" => "Job emote 1",
        "xe1" => "Job emote 2",
        "xe2" => "Job emote 3",
        "xe3" => "Job emote 4",
        "xe4" => "Job emote 5",
        "xe5" => "Job emote 6",
        "xe6" => "Job emote 7",
        _ => return None,
    })
}

/// DAT space → rig space: the 180° turn about X that makes entity DATs Y-up (xi-model-viewer
/// `ENTITY_ROT`), then a −90° turn about Y so the character faces +Z like every other rig.
fn flip_quat() -> Quat {
    Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2) * Quat::from_rotation_x(std::f32::consts::PI)
}

fn bone_name(i: usize) -> String {
    format!("j{i}")
}

/// A joint's absolute local pose (bind pose or one animation frame).
#[derive(Clone, Copy)]
struct LocalPose {
    rotation: Quat,
    translation: Vec3,
    scale: Vec3,
}

/// Compose the bind pose or an animation sample into the joint's local pose the runtime
/// expects (the root swizzle and the Y flip included).
fn compose_local(joint: &entity::Joint, index: usize, anim: Option<(Quat, Vec3, Vec3)>) -> LocalPose {
    let bind_q = Quat::from_xyzw(joint.rotation[0], joint.rotation[1], joint.rotation[2], joint.rotation[3]).normalize();
    let bind_t = Vec3::from(joint.translation);
    let (mut q, mut t, mut s) = match anim {
        Some((aq, at, asc)) => (aq * bind_q, bind_t + at, if index == 0 { Vec3::ONE } else { asc }),
        None => (bind_q, bind_t, Vec3::ONE),
    };
    if joint.parent.is_none() {
        if index == 0 {
            t = Vec3::new(-t.z, t.y, t.x);
        }
        q = flip_quat() * q;
        t = flip_quat() * t;
        s = Vec3::ONE;
    }
    LocalPose {
        rotation: q.normalize(),
        translation: t,
        scale: s,
    }
}

/// World (model-space) bind transforms per joint: rotation, translation, scale.
fn bind_world(skeleton: &entity::Skeleton) -> Vec<(Quat, Vec3, Vec3)> {
    let n = skeleton.joints.len();
    let mut out: Vec<Option<(Quat, Vec3, Vec3)>> = vec![None; n];
    fn resolve(i: usize, skeleton: &entity::Skeleton, out: &mut Vec<Option<(Quat, Vec3, Vec3)>>, depth: usize) -> (Quat, Vec3, Vec3) {
        if let Some(v) = out[i] {
            return v;
        }
        let local = compose_local(&skeleton.joints[i], i, None);
        let v = match skeleton.joints[i].parent {
            Some(p) if depth < 256 => {
                let (pq, pt, ps) = resolve(p, skeleton, out, depth + 1);
                (pq * local.rotation, pt + pq * (ps * local.translation), ps * local.scale)
            }
            _ => (local.rotation, local.translation, local.scale),
        };
        out[i] = Some(v);
        v
    }
    (0..n).map(|i| resolve(i, skeleton, &mut out, 0)).collect()
}

fn skeleton_data(skeleton: &entity::Skeleton) -> SkeletonData {
    SkeletonData {
        bones: skeleton
            .joints
            .iter()
            .enumerate()
            .map(|(i, j)| {
                let l = compose_local(j, i, None);
                BoneData {
                    name: bone_name(i),
                    parent: j.parent,
                    translation: l.translation.to_array(),
                    rotation: l.rotation.to_array(),
                    scale: l.scale.to_array(),
                }
            })
            .collect(),
    }
}

/// Model-space position and normal of a skinned vertex in the bind pose.
fn skin_vertex(v: &SkinVertex, world: &[(Quat, Vec3, Vec3)]) -> (Vec3, Vec3) {
    let clamp = |j: u16| (j as usize).min(world.len().saturating_sub(1));
    let (q0, t0, s0) = world[clamp(v.joint0)];
    let mut p = q0 * (s0 * Vec3::from(v.p0));
    let mut n = q0 * Vec3::from(v.n0);
    match v.joint1 {
        Some(j1) => {
            let (q1, t1, s1) = world[clamp(j1)];
            p += v.w0 * t0 + q1 * (s1 * Vec3::from(v.p1)) + v.w1 * t1;
            n += q1 * Vec3::from(v.n1);
        }
        None => p += t0,
    }
    (p, n.normalize_or_zero())
}

/// Whether a piece is hidden by the occlusion types the worn gear declares (a helmet hides
/// hair, sleeves hide wrists, ...); xim `ActorModel.isOccluded`.
fn occluded(display_type: u8, occl: &[u8]) -> bool {
    let has = |t: u8| occl.contains(&t);
    match display_type {
        1 => has(0x02) || has(0x03) || has(0x04) || has(0x05) || has(0x06),
        2 | 3 => has(0x04) || has(0x05) || has(0x06),
        4 => has(0x05),
        5 => has(0x12),
        6 => has(0x32),
        7 => has(0x22),
        _ => false,
    }
}

struct PartBuilder<'a> {
    key: String,
    textures: &'a HashMap<String, TextureRef>,
    world: &'a [(Quat, Vec3, Vec3)],
    joint_count: usize,
    model: ModelData,
    /// (texture, display type, colour) → material index.
    materials: HashMap<(String, u8, u32), usize>,
}

impl PartBuilder<'_> {
    fn material(&mut self, piece: &Piece) -> usize {
        let key = (piece.texture.clone(), piece.props.display_type, if piece.texture.is_empty() { piece.corners.first().map(|c| c.color).unwrap_or(0x8080_8080) } else { 0 });
        if let Some(&i) = self.materials.get(&key) {
            return i;
        }
        let mut d = MaterialDesc::new(&format!("{}/{}#{}#{:08x}", self.key, key.0, key.1, key.2), crate::SHADER_ZONE);
        d.double_sided = true;
        d.roughness = 0.85;
        if piece.texture.is_empty() {
            let c = key.2;
            let f = |v: u32| ((v & 0xFF) as f32 / 128.0).min(1.0);
            d.base_color = [f(c >> 16), f(c >> 8), f(c), 1.0];
        } else {
            match self.textures.get(&piece.texture) {
                Some(t) => {
                    d.diffuse = Some(t.clone());
                    if t.has_alpha {
                        d.alpha_mask = true;
                        d.alpha_cutoff = 0.5;
                    }
                }
                None => d.base_color = [0.7, 0.7, 0.7, 1.0],
            }
        }
        self.model.materials.push(d);
        let i = self.model.materials.len() - 1;
        self.materials.insert(key, i);
        i
    }

    fn add_mesh(&mut self, mesh: &SkinnedMesh, occl: &[u8]) {
        // One MeshData per material: corners are split per (vertex, uv).
        let mut by_material: BTreeMap<usize, MeshData> = BTreeMap::new();
        let mut vertex_index: HashMap<(usize, usize, u16, u32, u32), u32> = HashMap::new();
        for piece in &mesh.pieces {
            if occluded(piece.props.display_type, occl) {
                continue;
            }
            let pool: &[SkinVertex] = match (piece.mirrored, &mesh.mirrored) {
                (true, Some(m)) => m,
                (true, None) => continue,
                (false, _) => &mesh.vertices,
            };
            if pool.is_empty() {
                continue;
            }
            let mi = self.material(piece);
            let world = self.world;
            let joint_count = self.joint_count;
            let out = by_material.entry(mi).or_insert_with(|| MeshData {
                material_index: mi,
                joints: Some((Vec::new(), Vec::new())),
                ..Default::default()
            });
            let mut corner_vertex = |c: &entity::Corner, pool_id: usize, out: &mut MeshData| -> u32 {
                let key = (mi, pool_id, c.vertex, c.uv[0].to_bits(), c.uv[1].to_bits());
                if let Some(&i) = vertex_index.get(&key) {
                    return i;
                }
                let v = &pool[(c.vertex as usize).min(pool.len() - 1)];
                let (p, n) = skin_vertex(v, world);
                out.positions.push(p.to_array());
                out.normals.push(n.to_array());
                out.uv0.push(c.uv);
                let j0 = (v.joint0 as usize).min(joint_count.saturating_sub(1)) as u16;
                let (j1, w0, w1) = match v.joint1 {
                    Some(j) => ((j as usize).min(joint_count.saturating_sub(1)) as u16, v.w0, v.w1),
                    None => (j0, 1.0, 0.0),
                };
                let sum = w0 + w1;
                let (w0, w1) = if sum > 1e-6 { (w0 / sum, w1 / sum) } else { (1.0, 0.0) };
                let (ji, jw) = out.joints.as_mut().unwrap();
                ji.push([j0, j1, 0, 0]);
                jw.push([w0, w1, 0.0, 0.0]);
                let i = out.positions.len() as u32 - 1;
                vertex_index.insert(key, i);
                i
            };
            let pool_id = if piece.mirrored { 1 } else { 0 };
            let tris = piece.triangles();
            // Winding: agree with the stored normals (mirrored pools flip it).
            let mut agreement = 0.0f32;
            let mut ids = Vec::with_capacity(tris.len());
            for t in &tris {
                let idx = [
                    corner_vertex(&piece.corners[t[0]], pool_id, out),
                    corner_vertex(&piece.corners[t[1]], pool_id, out),
                    corner_vertex(&piece.corners[t[2]], pool_id, out),
                ];
                if mesh.has_normals {
                    let p = |i: u32| Vec3::from(out.positions[i as usize]);
                    let n = |i: u32| Vec3::from(out.normals[i as usize]);
                    let g = (p(idx[1]) - p(idx[0])).cross(p(idx[2]) - p(idx[0]));
                    agreement += g.dot(n(idx[0]) + n(idx[1]) + n(idx[2]));
                }
                ids.push(idx);
            }
            let flip = agreement < 0.0;
            for idx in ids {
                if flip {
                    out.indices.extend([idx[0], idx[2], idx[1]]);
                } else {
                    out.indices.extend(idx);
                }
            }
        }
        for (_, mut m) in by_material {
            if m.indices.is_empty() {
                continue;
            }
            // Joint indices are skeleton indices; the bone table maps them 1:1.
            let mut table: Vec<u16> = Vec::new();
            let mut remap: HashMap<u16, u16> = HashMap::new();
            if let Some((ji, _)) = m.joints.as_mut() {
                for j in ji.iter_mut() {
                    for k in 0..2 {
                        let local = *remap.entry(j[k]).or_insert_with(|| {
                            table.push(j[k]);
                            (table.len() - 1) as u16
                        });
                        j[k] = local;
                    }
                }
            }
            m.bone_table = table;
            for p in &m.positions {
                for k in 0..3 {
                    self.model.bounds_min[k] = self.model.bounds_min[k].min(p[k]);
                    self.model.bounds_max[k] = self.model.bounds_max[k].max(p[k]);
                }
            }
            self.model.meshes.push(m);
        }
    }
}

fn nlerp(a: [f32; 4], b: [f32; 4], t: f32) -> Quat {
    let qa = Quat::from_array(a);
    let qb = Quat::from_array(b);
    let qb = if qa.dot(qb) < 0.0 { -qb } else { qb };
    (qa * (1.0 - t) + qb * t).normalize()
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> Vec3 {
    Vec3::from(a).lerp(Vec3::from(b), t)
}

/// Sample a track at a phase in `0..=1`.
fn sample_track(track: &entity::Track, phase: f32) -> (Quat, Vec3, Vec3) {
    let frames = track.rotations.len();
    if frames <= 1 || phase >= 1.0 {
        let f = frames.saturating_sub(1);
        return (Quat::from_array(track.rotations[f]).normalize(), Vec3::from(track.translations[f]), Vec3::from(track.scales[f]));
    }
    let pos = phase.max(0.0) * (frames - 1) as f32;
    let lower = (pos.floor() as usize).min(frames - 2);
    let t = pos - lower as f32;
    (
        nlerp(track.rotations[lower], track.rotations[lower + 1], t),
        lerp3(track.translations[lower], track.translations[lower + 1], t),
        lerp3(track.scales[lower], track.scales[lower + 1], t),
    )
}

/// Body-region slot digit of an animation id (`idl0` → 0), if it has one.
fn body_slot(id: &str) -> Option<u8> {
    let b = id.as_bytes();
    if b.len() == 4 && (b'0'..=b'2').contains(&b[3]) { Some(b[3] - b'0') } else { None }
}

/// Clip name of an animation id (`idl0` → `idl`, `mou4` → `mou4`).
pub fn clip_name(id: &str) -> &str {
    if body_slot(id).is_some() { &id[..3] } else { id }
}

/// Merge the body-region parts of one clip into an `ffl_core::Clip` of absolute local poses.
/// Higher slots win joints they share with lower ones; parts of different lengths are
/// sampled by phase. Joints in `skip` are left to the reference pose (re-parented grips).
pub fn build_clip(name: &str, parts: &[&Animation], skeleton: &entity::Skeleton, skip: &[usize]) -> Option<Clip> {
    if parts.is_empty() {
        return None;
    }
    let mut ordered: Vec<&Animation> = parts.to_vec();
    ordered.sort_by_key(|a| body_slot(&a.id).unwrap_or(0));
    let duration = ordered.iter().map(|a| a.duration()).fold(0.0f32, f32::max).max(1.0 / 30.0);
    let frames = ordered.iter().map(|a| a.frames).max().unwrap_or(1).max(2);
    // joint → track (last writer wins = higher slot).
    let mut tracks: BTreeMap<usize, &entity::Track> = BTreeMap::new();
    for a in &ordered {
        for t in &a.tracks {
            if t.joint < skeleton.joints.len() && !skip.contains(&t.joint) {
                tracks.insert(t.joint, t);
            }
        }
    }
    if tracks.is_empty() {
        return None;
    }
    let track_to_bone: Vec<usize> = tracks.keys().copied().collect();
    let mut out_frames = Vec::with_capacity(frames);
    for f in 0..frames {
        let phase = f as f32 / (frames - 1) as f32;
        let mut poses = Vec::with_capacity(track_to_bone.len());
        for (&joint, track) in &tracks {
            let j = &skeleton.joints[joint];
            let local = if track.reset {
                compose_local(j, joint, None)
            } else {
                let (q, t, s) = sample_track(track, phase);
                compose_local(j, joint, Some((q.normalize(), t, s)))
            };
            poses.push(ClipPose {
                translation: local.translation.to_array(),
                rotation: local.rotation.to_array(),
                scale: local.scale.to_array(),
            });
        }
        out_frames.push(poses);
    }
    Some(Clip {
        additive: false,
        name: name.to_string(),
        duration,
        fps: (frames - 1) as f32 / duration,
        frames: out_frames,
        track_to_bone,
        events: Vec::new(),
    })
}

/// Group animations by clip name and build every clip.
fn build_clips(animations: &[Animation], skeleton: &entity::Skeleton, skip: &[usize]) -> HashMap<String, Clip> {
    let mut groups: BTreeMap<String, Vec<&Animation>> = BTreeMap::new();
    for a in animations {
        groups.entry(clip_name(&a.id).to_string()).or_default().push(a);
    }
    groups.into_iter().filter_map(|(name, parts)| build_clip(&name, &parts, skeleton, skip).map(|c| (name, c))).collect()
}

/// The DATs of a look and what each contributes.
pub struct Loaded {
    pub skeleton: entity::Skeleton,
    pub animations: Vec<Animation>,
    /// (slot, file id, DAT) of the face and every worn gear piece.
    pub parts: Vec<(Slot, u32, EntityDat)>,
    /// Battle motions of the main weapon's animation type (battle DAT + skirt pack).
    pub battle: Vec<Animation>,
    pub battle_routines: Vec<Routine>,
    pub notes: Vec<String>,
    pub content: ContentReport,
}

/// Motion "file number" (`folder × 1000 + file`) of a file id, from its path.
fn motion_number(install: &Install, id: u32) -> Option<u32> {
    let path = install.file_path(id)?;
    let file: u32 = path.file_stem()?.to_str()?.parse().ok()?;
    let folder: u32 = path.parent()?.file_name()?.to_str()?.parse().ok()?;
    Some(folder * 1000 + file)
}

fn read_entity(install: &Install, id: u32, content: &mut ContentReport) -> Result<EntityDat> {
    let data = install.read(id).with_context(|| format!("DAT {id}"))?;
    content.push(&format!("ff11:dat:{id}"), Provenance::Game);
    Ok(entity::parse_entity(&format!("ff11:dat:{id}"), &data))
}

/// Read the skeleton, motion packs, face and gear DATs of a look.
pub fn load_look(install: &Install, look: &Look) -> Result<Loaded> {
    let mut content = ContentReport::default();
    let mut notes = Vec::new();
    let base_id = look.race.base_file();
    let base = read_entity(install, base_id, &mut content)?;
    let skeleton = base.skeleton.clone().ok_or_else(|| anyhow!("DAT {base_id} ({}) has no skeleton", look.race.name()))?;
    let mut animations = base.animations;
    notes.extend(base.notes);

    let mut parts = Vec::new();
    let face_id = crate::pc::model_file(look.race, Slot::Face, look.face);
    match face_id {
        Some(id) if install.exists(id) => match read_entity(install, id, &mut content) {
            Ok(d) => parts.push((Slot::Face, id, d)),
            Err(e) => notes.push(format!("face: {e:#}")),
        },
        _ => notes.push(format!("face {} has no DAT for {}", look.face, look.race.name())),
    }
    let mut waist_variant = 0u8;
    for slot in Slot::GEAR {
        let model = look.model(slot);
        if slot.is_weapon() && model == 0 {
            continue;
        }
        let Some(id) = crate::pc::model_file(look.race, slot, model) else {
            notes.push(format!("{}: model {model} is not in the table", slot.key()));
            continue;
        };
        if !install.exists(id) {
            notes.push(format!("{}: model {model} (DAT {id}) is not installed", slot.key()));
            continue;
        }
        match read_entity(install, id, &mut content) {
            Ok(d) => {
                if slot == Slot::Body {
                    waist_variant = d.info.map(|i| i.waist_variant).unwrap_or(0);
                }
                notes.extend(d.notes.iter().map(|n| format!("{}: {n}", slot.key())));
                parts.push((slot, id, d));
            }
            Err(e) => notes.push(format!("{}: {e:#}", slot.key())),
        }
    }
    // Upper body (+1) and waist (+3, or +4 for bodies that ask for the second block).
    for extra in [1, if waist_variant == 2 { 4 } else { 3 }] {
        let id = base_id + extra;
        if !install.exists(id) {
            continue;
        }
        match read_entity(install, id, &mut content) {
            Ok(d) => animations.extend(d.animations),
            Err(e) => notes.push(format!("motion pack {id}: {e:#}")),
        }
    }
    // Battle motions for the weapon in hand (the main weapon's type, else the off hand's).
    let mut battle = Vec::new();
    let mut battle_routines = Vec::new();
    let weapon_type = [Slot::Main, Slot::Sub]
        .into_iter()
        .find_map(|slot| parts.iter().find(|(s, _, _)| *s == slot).and_then(|(_, _, d)| d.info.map(|i| i.weapon_animation_type)));
    if let Some(wt) = weapon_type {
        let id = look.race.battle_file(wt);
        match read_entity(install, id, &mut content) {
            Ok(d) => {
                battle.extend(d.animations);
                battle_routines.extend(d.routines);
                let skirt = motion_number(install, id)
                    .and_then(|n| look.race.battle_skirt_number(n, waist_variant))
                    .and_then(|n| install.id_of_motion_number(n));
                match skirt {
                    Some(sid) if install.exists(sid) => match read_entity(install, sid, &mut content) {
                        Ok(d) => battle.extend(d.animations),
                        Err(e) => notes.push(format!("battle skirt pack {sid}: {e:#}")),
                    },
                    _ => notes.push(format!("no skirt pack for battle DAT {id}")),
                }
                notes.push(format!("battle motions: weapon type {wt}, DAT {id}, {} clips", battle.len()));
            }
            Err(e) => notes.push(format!("battle DAT {id}: {e:#}")),
        }
    }
    Ok(Loaded {
        skeleton,
        animations,
        parts,
        battle,
        battle_routines,
        notes,
        content,
    })
}

/// Emote actions: eight routines per emote DAT.
pub fn list_emotes(install: &Install, race: Race) -> Vec<ActionDef> {
    let mut out = Vec::new();
    for n in 0..Race::EMOTE_FILES {
        let id = race.emote_file() + n;
        let Ok(data) = install.read(id) else {
            continue;
        };
        let dat = entity::parse_entity("", &data);
        let ids: Vec<&str> = dat.animations.iter().map(|a| a.id.as_str()).collect();
        for r in &dat.routines {
            let Some(cmd) = r.commands.first() else {
                continue;
            };
            let Some(clip) = ids.iter().find(|i| entity::clip_matches(&cmd.clip, i)) else {
                continue;
            };
            let name = clip_name(clip);
            out.push(ActionDef {
                id: format!("emote:{id}:{}", r.id),
                name: emote_name(name).map(str::to_string).unwrap_or_else(|| format!("Emote {name}")),
                category: ActionCategory::Emote,
                looped: false,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    // Some emotes ship twice (a second variant in a later DAT): number the repeats.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for a in &mut out {
        let n = seen.entry(a.name.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            a.name = format!("{} {}", a.name, n);
        }
    }
    out
}

/// Load an emote routine's clip (body regions merged, the waist pack six files on).
pub fn load_emote(install: &Install, look: &Look, action_id: &str) -> Result<Clip> {
    let rest = action_id.strip_prefix("emote:").ok_or_else(|| anyhow!("not an emote action: {action_id}"))?;
    let (file, routine) = rest.split_once(':').ok_or_else(|| anyhow!("bad emote id {action_id}"))?;
    let file: u32 = file.parse()?;
    let dat = entity::parse_entity("", &install.read(file)?);
    let r = dat.routines.iter().find(|r| r.id == routine).ok_or_else(|| anyhow!("DAT {file} has no routine {routine}"))?;
    let mut animations = dat.animations.clone();
    if let Ok(waist) = install.read(file + 6) {
        animations.extend(entity::parse_entity("", &waist).animations);
    }
    let base = entity::parse_entity("", &install.read(look.race.base_file())?);
    let skeleton = base.skeleton.ok_or_else(|| anyhow!("no skeleton"))?;
    let skip: Vec<usize> = Vec::new();
    let mut parts: Vec<&Animation> = Vec::new();
    for cmd in &r.commands {
        for a in &animations {
            if entity::clip_matches(&cmd.clip, &a.id) && !parts.iter().any(|p| p.id == a.id) {
                parts.push(a);
            }
        }
    }
    let mut clip = build_clip(action_id, &parts, &skeleton, &skip).ok_or_else(|| anyhow!("routine {routine} of DAT {file} plays no clip"))?;
    // The routine's sound effects (a clap, a logging axe): the emote's motion starts at its
    // first command, the sounds at their own tick.
    let clip_start = r.commands.first().map(|c| c.delay).unwrap_or(0);
    for (pointer, at) in &r.sounds {
        let Some((_, se)) = dat.sounds.iter().find(|(id, _)| id == pointer) else {
            continue;
        };
        let time = (*at as f32 - clip_start as f32).max(0.0) / 60.0;
        clip.events.push(ClipEvent {
            time,
            kind: ClipEventKind::Sound { cue: SoundCue::new(vec![crate::sound::sound_key(crate::sound::SoundKind::Spw, *se)]), stop_at_end: true },
        });
    }
    clip.events.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    Ok(clip)
}

/// (grip joint, hand joint) for a weapon's info, when both references resolve.
fn grip_override(skeleton: &entity::Skeleton, info: Option<&entity::Info>, hand_ref: usize) -> Option<(usize, usize)> {
    let grip_ref = info?.grip_reference? as usize;
    let grip = skeleton.references.get(grip_ref)?.joint as usize;
    let hand = skeleton.references.get(hand_ref)?.joint as usize;
    (grip != hand && grip < skeleton.joints.len() && hand < skeleton.joints.len()).then_some((grip, hand))
}

/// Build the character model of a look.
pub fn load_character(install: &Install, look: &Look, name: &str) -> Result<CharacterModel> {
    let mut loaded = load_look(install, look)?;
    let skeleton = std::mem::take(&mut loaded.skeleton);
    let mut notes = std::mem::take(&mut loaded.notes);
    let mut content = std::mem::take(&mut loaded.content);
    let skip: Vec<usize> = Vec::new();
    let world = bind_world(&skeleton);
    let skeleton_data = skeleton_data(&skeleton);

    // Textures from every DAT (later ones override).
    let mut textures: HashMap<String, TextureRef> = HashMap::new();
    for (_, _, d) in &loaded.parts {
        for t in &d.textures {
            textures.insert(crate::zone::name_str(&t.name), t.texture.clone());
        }
    }
    let occl: Vec<u8> = loaded.parts.iter().flat_map(|(_, _, d)| d.meshes.iter().map(|m| m.occlude_type)).filter(|t| *t != 0).collect();

    let mut parts = Vec::new();
    let mut addons = Vec::new();
    let (mut bmin, mut bmax) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for (slot, id, dat) in &loaded.parts {
        let mut b = PartBuilder {
            key: format!("ff11:dat:{id}"),
            textures: &textures,
            world: &world,
            joint_count: skeleton.joints.len(),
            model: ModelData {
                key: format!("ff11:dat:{id}"),
                bone_names: (0..skeleton.joints.len()).map(bone_name).collect(),
                bounds_min: [f32::MAX; 3],
                bounds_max: [f32::MIN; 3],
                ..Default::default()
            },
            materials: HashMap::new(),
        };
        for m in &dat.meshes {
            b.add_mesh(m, &occl);
        }
        if b.model.meshes.is_empty() {
            continue;
        }
        bmin = bmin.min(Vec3::from(b.model.bounds_min));
        bmax = bmax.max(Vec3::from(b.model.bounds_max));
        let addon = slot.is_weapon().then(|| format!("weapon:{}", slot.key()));
        if let Some(a) = &addon {
            // Sheathed = as skinned (the waist pack carries the grip joint to the hip);
            // drawn = the grip joint hangs off the hand joint.
            let hand_ref = if *slot == Slot::Sub { REF_LEFT_HAND } else { REF_RIGHT_HAND };
            let grip = grip_override(&skeleton, dat.info.as_ref(), hand_ref);
            let mut placements = vec![AddonPlacement {
                id: "sheathed".into(),
                name: "Sheathed".into(),
                attach: Attach::Skinned,
                offset: Trs::IDENTITY,
            }];
            if let Some((g, h)) = grip {
                placements.push(AddonPlacement {
                    id: "drawn".into(),
                    name: "In hand".into(),
                    attach: Attach::Reparent { bone: bone_name(g), onto: bone_name(h) },
                    offset: Trs::IDENTITY,
                });
            }
            addons.push(AddonDef {
                id: a.clone(),
                name: format!("{} ({})", slot.label(), crate::gear_names::gear_name(slot.key(), look.race as u8, look.model(*slot)).unwrap_or("weapon")),
                kind: AddonKind::Weapon,
                enabled: true,
                rules: Vec::new(),
                placements,
                active: grip.map(|_| "drawn".to_string()),
                stowed: Some("sheathed".into()),
                active_actions: Vec::new(),
                activate: None,
                stow: None,
            });
        }
        parts.push(CharacterPart {
            name: slot.key().to_string(),
            model: b.model,
            attach: Attach::Skinned,
            addon,
        });
    }
    if parts.is_empty() {
        return Err(anyhow!("no meshes for {} face {}", look.race.name(), look.face));
    }
    let mut clips = build_clips(&loaded.animations, &skeleton, &skip);
    let has = |clips: &HashMap<String, Clip>, n: &str| clips.contains_key(n).then(|| n.to_string());
    let locomotion = Locomotion {
        idle: has(&clips, IDLE),
        walk: has(&clips, WALK),
        run: has(&clips, RUN),
        sprint: None,
        fall: None,
        jump: has(&clips, JUMP),
        land: None,
        ..Default::default()
    };
    if locomotion.idle.is_none() {
        notes.push(format!("no idle clip among {} clips", clips.len()));
    }

    // Battle set: the engaged idle and the engaged walk/run (base lower body + waist, battle
    // upper body), plus the draw/sheathe transitions named by the battle routines.
    let mut armed_locomotion = None;
    if !loaded.battle.is_empty() {
        let mut combined: Vec<Animation> = loaded.animations.iter().filter(|a| matches!(clip_name(&a.id), "wlk" | "run")).cloned().collect();
        combined.extend(loaded.battle.iter().cloned());
        let by_name = |name: &str| -> Vec<&Animation> {
            // Later parts (battle, skirt) replace base parts of the same id.
            let mut out: Vec<&Animation> = Vec::new();
            for a in combined.iter().filter(|a| clip_name(&a.id) == name) {
                out.retain(|o| o.id != a.id);
                out.push(a);
            }
            out
        };
        for (clip, source) in [("btl", "btl"), ("btl:wlk", "wlk"), ("btl:run", "run")] {
            if let Some(c) = build_clip(clip, &by_name(source), &skeleton, &skip) {
                clips.insert(clip.to_string(), c);
            }
        }
        // Draw (`in 0`) and sheathe (`out0`): the clip the routine's first command names.
        for (clip, routine) in [("draw", "in 0"), ("sheathe", "out0")] {
            let Some(r) = loaded.battle_routines.iter().find(|r| r.id == routine) else {
                notes.push(format!("battle DAT has no {routine} routine"));
                continue;
            };
            let Some(cmd) = r.commands.first() else {
                continue;
            };
            let parts: Vec<&Animation> = combined.iter().filter(|a| entity::clip_matches(&cmd.clip, &a.id)).collect();
            if let Some(c) = build_clip(clip, &parts, &skeleton, &skip) {
                clips.insert(clip.to_string(), c);
            } else {
                notes.push(format!("{routine}: no clip matches {}", cmd.clip));
            }
        }
        // The routines fire the weapon's own draw/sheathe sound (`sinr` / `sotr` sound
        // pointers of the weapon DAT) 36 ticks (0.6 s) in.
        for (clip, pointer) in [("draw", "sinr"), ("sheathe", "sotr")] {
            let keys: Vec<String> = loaded
                .parts
                .iter()
                .filter(|(s, _, _)| s.is_weapon())
                .flat_map(|(_, _, d)| d.sounds.iter().filter(|(id, _)| id == pointer).map(|(_, se)| crate::sound::sound_key(crate::sound::SoundKind::Spw, *se)))
                .collect();
            if let (Some(c), false) = (clips.get_mut(clip), keys.is_empty()) {
                c.events.push(ClipEvent {
                    time: 0.6,
                    kind: ClipEventKind::Sound { cue: SoundCue::new(keys), stop_at_end: false },
                });
            }
        }
        if let Some(draw) = clips.get("draw").map(|c| c.duration) {
            // The grip joint reaches the hand at the draw's end and leaves it as the sheathe
            // starts; the runtime re-parents at those moments.
            for a in &mut addons {
                if a.active.is_some() {
                    a.activate = Some(AddonTransition { clip: "draw".into(), switch_at: (draw - 0.05).max(0.0) });
                    a.stow = Some(AddonTransition { clip: "sheathe".into(), switch_at: 0.0 });
                }
            }
        }
        armed_locomotion = clips.contains_key("btl").then(|| Locomotion {
            idle: Some("btl".into()),
            walk: has(&clips, "btl:wlk"),
            run: has(&clips, "btl:run"),
            sprint: None,
            fall: None,
            jump: has(&clips, JUMP),
            land: None,
            ..Default::default()
        });
    }

    // Footsteps: contact events from the feet's own motion (FFXI keeps no step timeline).
    let feet = feet_joints(&skeleton_data, &skeleton.references);
    let mut step_notes = Vec::new();
    if let Some(feet) = feet {
        for name in [WALK, RUN, "btl:wlk", "btl:run"] {
            if let Some(c) = clips.get_mut(name) {
                c.events = footstep_events(c, &skeleton_data, feet);
                step_notes.push(format!("{name} {}", c.events.iter().map(|e| format!("{:.2}{}", e.time, match e.kind { ClipEventKind::Footstep { foot: Foot::Left, .. } => "L", _ => "R" })).collect::<Vec<_>>().join("/")));
            }
        }
    }
    notes.push(format!("footstep events (feet {:?}): {}", feet, step_notes.join(", ")));
    let footsteps = crate::sound::footstep_set(look.race.footstep_entry(), |id| install.sound_path(crate::sound::SoundKind::Spw, id).is_some());
    for key in footsteps.iter().flat_map(|f| f.cues.iter().flat_map(|c| c.cue.variations.iter())) {
        content.push(key, Provenance::Game);
    }
    let height = if bmax.y > bmin.y && (bmax.y - bmin.y) > 0.3 { bmax.y - bmin.y } else { look.race.height() };
    let actions = list_emotes(install, look.race);
    notes.push(format!(
        "{} {}: {} joints, {} parts, {} clips, {} emotes, height {height:.2}",
        look.race.name(),
        look.face,
        skeleton.joints.len(),
        parts.len(),
        clips.len(),
        actions.len()
    ));
    Ok(CharacterModel {
        overlays: Vec::new(),
        blink: None,
        engine: crate::ENGINE_ID.into(),
        name: name.to_string(),
        skeleton: skeleton_data,
        parts,
        clips,
        locomotion,
        armed_locomotion,
        actions,
        addons,
        scale: 1.0,
        height,
        notes,
        footsteps,
        voice: None,
        content,
    })
}

/// The feet: joint references 8 and 9 name them on every race (the client's effect attach
/// points; verified on Hume ♂/♀, Mithra, Tarutaru and Galka). Ordered (left, right) by the
/// rig-space X of the bind pose (facing +Z with Y up, the character's left is +X).
fn feet_joints(skeleton: &SkeletonData, references: &[entity::JointReference]) -> Option<(usize, usize)> {
    let a = references.get(8)?.joint as usize;
    let b = references.get(9)?.joint as usize;
    if a >= skeleton.bones.len() || b >= skeleton.bones.len() {
        return None;
    }
    let bind = bind_positions(skeleton, None);
    Some(if bind[a].x >= bind[b].x { (a, b) } else { (b, a) })
}

/// World positions of every bone for the bind pose, or for one clip frame (bones the clip
/// does not key stay at bind).
fn bind_positions(skeleton: &SkeletonData, frame: Option<(&Clip, usize)>) -> Vec<Vec3> {
    let n = skeleton.bones.len();
    let mut local: Vec<(Quat, Vec3, Vec3)> = skeleton
        .bones
        .iter()
        .map(|b| (Quat::from_array(b.rotation), Vec3::from(b.translation), Vec3::from(b.scale)))
        .collect();
    if let Some((clip, f)) = frame
        && let Some(poses) = clip.frames.get(f)
    {
        for (track, pose) in poses.iter().enumerate() {
            if let Some(&bone) = clip.track_to_bone.get(track)
                && bone < n
            {
                local[bone] = (Quat::from_array(pose.rotation), Vec3::from(pose.translation), local[bone].2);
            }
        }
    }
    let mut world: Vec<Option<(Quat, Vec3, Vec3)>> = vec![None; n];
    fn resolve(i: usize, bones: &[BoneData], local: &[(Quat, Vec3, Vec3)], world: &mut Vec<Option<(Quat, Vec3, Vec3)>>, depth: usize) -> (Quat, Vec3, Vec3) {
        if let Some(w) = world[i] {
            return w;
        }
        let (q, t, s) = local[i];
        let w = match bones[i].parent {
            Some(p) if depth < 256 => {
                let (pq, pt, ps) = resolve(p, bones, local, world, depth + 1);
                (pq * q, pt + pq * (ps * t), ps * s)
            }
            _ => (q, t, s),
        };
        world[i] = Some(w);
        w
    }
    (0..n).map(|i| resolve(i, &skeleton.bones, &local, &mut world, 0).1).collect()
}

/// Footstep events of a locomotion cycle: one per foot, at the frame where that foot is
/// lowest (the cycle runs in place, so the plant is the foot's low point). Feet that barely
/// move (a shuffle) get none.
fn footstep_events(clip: &Clip, skeleton: &SkeletonData, feet: (usize, usize)) -> Vec<ClipEvent> {
    let frames = clip.frames.len();
    if frames < 2 {
        return Vec::new();
    }
    let heights: Vec<(f32, f32)> = (0..frames)
        .map(|f| {
            let w = bind_positions(skeleton, Some((clip, f)));
            (w.get(feet.0).map(|p| p.y).unwrap_or(0.0), w.get(feet.1).map(|p| p.y).unwrap_or(0.0))
        })
        .collect();
    let mut events = Vec::new();
    for (side, foot) in [(0usize, Foot::Left), (1, Foot::Right)] {
        let h = |f: usize| if side == 0 { heights[f].0 } else { heights[f].1 };
        let (mut lowest, mut min, mut max) = (0usize, f32::MAX, f32::MIN);
        // The last frame repeats the first in a cycle; leave it out so the minimum is unique.
        for f in 0..frames - 1 {
            if h(f) < min {
                min = h(f);
                lowest = f;
            }
            max = max.max(h(f));
        }
        if max - min < 0.02 {
            continue;
        }
        events.push(ClipEvent {
            time: lowest as f32 / clip.fps,
            kind: ClipEventKind::Footstep { foot, variant: 0 },
        });
    }
    events.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    events
}
