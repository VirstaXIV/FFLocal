//! FFXI entity DATs (player characters, NPCs, weapons): the sections a rigged model is made
//! of. Layouts follow xi-model-viewer's `dat.js` (itself a port of xim's Kotlin parsers, the
//! reference that renders retail DATs) and were verified against the Steam install.
//!
//! - `0x29` skeleton: `u8` joint count at +2, then 30-byte joints at +4 (`u8` parent — equal
//!   to the joint's own index for the root —, pad, quaternion x y z w, translation), then a
//!   `u16` joint-reference count and 26-byte references (`u16` joint, 12 unknown bytes,
//!   offset). References 126/127 are the left/right hand attach points.
//! - `0x2A` skinned mesh: a header of half-word offsets (see [`parse_mesh`]), a joint array
//!   (local → skeleton joint), vertices with one or two joints (two-joint positions are
//!   pre-weighted), an optional mirrored copy for symmetric meshes, and an instruction
//!   stream of textured/untextured triangle strips and lists with per-corner UVs.
//! - `0x2B` animation: `u16` joint count, `u16` frame count, `f32` key-frame duration, then
//!   per joint `i32` joint index and three channel groups (rotation 4, translation 3, scale
//!   3) of `i32` offsets (0 = constant, > 0 = per-frame floats at `base + offset × 4`, < 0 =
//!   reset the joint to its bind pose) followed by the constants.
//! - `0x07` routine (schedule): the command list at `body + (i32@0x14 − 16)`; op `0x05`
//!   entries play a clip named at +8 (a `?` matches the body-region slot digit).
//! - `0x45` `info`: weapon animation type at byte 3, grip joint reference at byte 6 (0xFF =
//!   none), body waist variant at byte 9.

use anyhow::{Result, bail};

pub const SECTION_ROUTINE: u8 = 0x07;
pub const SECTION_SKELETON: u8 = 0x29;
pub const SECTION_MESH: u8 = 0x2A;
pub const SECTION_ANIMATION: u8 = 0x2B;
pub const SECTION_INFO: u8 = 0x45;

fn u16_at(p: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([p[o], p[o + 1]])
}
fn i32_at(p: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}
fn u32_at(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}
fn f32_at(p: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}
fn vec3_at(p: &[u8], o: usize) -> [f32; 3] {
    [f32_at(p, o), f32_at(p, o + 4), f32_at(p, o + 8)]
}

/// 4-char section id as text.
pub fn id_str(id: &[u8; 4]) -> String {
    String::from_utf8_lossy(id).trim_end_matches(['\0', ' ']).to_string()
}

fn name_str(n: &[u8]) -> String {
    let end = n.iter().position(|&b| b == 0).unwrap_or(n.len());
    String::from_utf8_lossy(&n[..end]).trim_end().to_string()
}

// ---- skeleton ----

#[derive(Debug, Clone)]
pub struct Joint {
    /// `None` for the root.
    pub parent: Option<usize>,
    /// Quaternion x, y, z, w.
    pub rotation: [f32; 4],
    pub translation: [f32; 3],
}

#[derive(Debug, Clone)]
pub struct JointReference {
    pub joint: u16,
    pub offset: [f32; 3],
}

#[derive(Debug, Clone, Default)]
pub struct Skeleton {
    pub id: String,
    pub joints: Vec<Joint>,
    pub references: Vec<JointReference>,
}

pub fn parse_skeleton(id: &[u8; 4], p: &[u8]) -> Result<Skeleton> {
    if p.len() < 4 {
        bail!("skeleton section too short");
    }
    let n = p[2] as usize;
    if p.len() < 4 + n * 30 {
        bail!("skeleton section truncated ({} joints)", n);
    }
    let mut joints = Vec::with_capacity(n);
    for i in 0..n {
        let o = 4 + i * 30;
        let parent = p[o] as usize;
        joints.push(Joint {
            parent: (parent != i && parent < n).then_some(parent),
            rotation: [f32_at(p, o + 2), f32_at(p, o + 6), f32_at(p, o + 10), f32_at(p, o + 14)],
            translation: vec3_at(p, o + 18),
        });
    }
    let mut references = Vec::new();
    let mut o = 4 + n * 30;
    if o + 4 <= p.len() {
        let count = u16_at(p, o) as usize;
        o += 4;
        for _ in 0..count {
            if o + 26 > p.len() {
                break;
            }
            references.push(JointReference {
                joint: u16_at(p, o),
                offset: vec3_at(p, o + 14),
            });
            o += 26;
        }
    }
    Ok(Skeleton {
        id: id_str(id),
        joints,
        references,
    })
}

// ---- skinned mesh ----

/// One vertex of a skinned mesh: up to two joints. Positions and normals of two-joint
/// vertices are pre-weighted in the file (`p_i = w_i × local_i`).
#[derive(Debug, Clone, Copy)]
pub struct SkinVertex {
    pub p0: [f32; 3],
    pub p1: [f32; 3],
    pub n0: [f32; 3],
    pub n1: [f32; 3],
    pub w0: f32,
    pub w1: f32,
    /// Skeleton joint index.
    pub joint0: u16,
    /// Skeleton joint index of the second joint, `None` for single-joint vertices.
    pub joint1: Option<u16>,
}

#[derive(Debug, Clone, Copy)]
pub struct Corner {
    /// Index into the vertex pool.
    pub vertex: u16,
    pub uv: [f32; 2],
    /// BGRA colour (0x80 = 1.0).
    pub color: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RenderProps {
    pub specular_enabled: bool,
    pub specular_power: f32,
    /// What this piece is (1/2/3 hair, 4 face, 5 wrist, 6 pants, 7 shins) for occlusion.
    pub display_type: u8,
    pub ambient_multiplier: f32,
}

#[derive(Debug, Clone)]
pub struct Piece {
    pub strip: bool,
    pub corners: Vec<Corner>,
    /// Texture name (empty = untextured, vertex colour only).
    pub texture: String,
    pub props: RenderProps,
    /// Uses the mirrored vertex pool (symmetric meshes).
    pub mirrored: bool,
}

#[derive(Debug, Clone)]
pub struct SkinnedMesh {
    pub id: String,
    pub vertices: Vec<SkinVertex>,
    /// Mirrored vertex pool of symmetric meshes (same length as `vertices`).
    pub mirrored: Option<Vec<SkinVertex>>,
    pub pieces: Vec<Piece>,
    pub has_normals: bool,
    /// What this mesh hides on other pieces (0x04 a helmet hides hair, 0x12 wrists, ...).
    pub occlude_type: u8,
}

struct JointRef {
    index: u16,
    flipped_index: u16,
    flip_axis: u8,
}

fn joint_ref(v: u16) -> JointRef {
    JointRef {
        index: v & 0x7F,
        flipped_index: (v >> 7) & 0x7F,
        flip_axis: ((v >> 14) & 0x3) as u8,
    }
}

fn flip(v: [f32; 3], axis: u8) -> [f32; 3] {
    let mut out = v;
    if (1..=3).contains(&axis) {
        out[axis as usize - 1] = -out[axis as usize - 1];
    }
    out
}

fn default_props() -> RenderProps {
    RenderProps {
        specular_enabled: false,
        specular_power: 0.0,
        display_type: 0,
        ambient_multiplier: 1.0,
    }
}

pub fn parse_mesh(id: &[u8; 4], p: &[u8]) -> Result<SkinnedMesh> {
    if p.len() < 42 {
        bail!("mesh section too short");
    }
    let flags3 = p[2];
    let cloth = flags3 & 0x01 != 0;
    let use_joint_array = flags3 & 0x80 != 0;
    let has_normals = !cloth;
    let occlude_type = p[3];
    let symmetric = p[4] == 1;
    let instruction_offset = 2 * i32_at(p, 6) as usize;
    let joint_array_offset = 2 * i32_at(p, 12) as usize;
    let num_joints = u16_at(p, 16) as usize;
    let vertex_counts_offset = 2 * i32_at(p, 18) as usize;
    let num_vertex_counts = u16_at(p, 22);
    let joint_mapping_offset = 2 * i32_at(p, 24) as usize;
    let vertex_data_offset = 2 * i32_at(p, 30) as usize;

    if joint_array_offset + num_joints * 2 > p.len() || vertex_counts_offset + 4 > p.len() {
        bail!("mesh header offsets out of range");
    }
    let joint_array: Vec<u16> = (0..num_joints).map(|i| u16_at(p, joint_array_offset + i * 2)).collect();
    let map_joint = |i: u16| -> u16 {
        if use_joint_array { joint_array.get(i as usize).copied().unwrap_or(0) } else { i }
    };
    if num_vertex_counts != 2 {
        bail!("expected 2 vertex counts, got {num_vertex_counts}");
    }
    let single = u16_at(p, vertex_counts_offset) as usize;
    let double = u16_at(p, vertex_counts_offset + 2) as usize;
    let total = single + double;
    if joint_mapping_offset + total * 4 > p.len() {
        bail!("mesh joint mapping out of range");
    }
    let mut refs0 = Vec::with_capacity(total);
    let mut refs1 = Vec::with_capacity(total);
    for i in 0..total {
        refs0.push(joint_ref(u16_at(p, joint_mapping_offset + i * 4)));
        refs1.push(joint_ref(u16_at(p, joint_mapping_offset + i * 4 + 2)));
    }
    let single_stride = if has_normals { 24 } else { 12 };
    let double_stride = if has_normals { 56 } else { 32 };
    if vertex_data_offset + single * single_stride + double * double_stride > p.len() {
        bail!("mesh vertex data out of range");
    }
    let mut vertices = Vec::with_capacity(total);
    let mut o = vertex_data_offset;
    for i in 0..single {
        let p0 = vec3_at(p, o);
        let n0 = if has_normals { vec3_at(p, o + 12) } else { [0.0; 3] };
        o += single_stride;
        vertices.push(SkinVertex {
            p0,
            p1: [0.0; 3],
            n0,
            n1: [0.0; 3],
            w0: 1.0,
            w1: 0.0,
            joint0: map_joint(refs0[i].index),
            joint1: None,
        });
    }
    for i in single..total {
        let f = |k: usize| f32_at(p, o + k * 4);
        let p0 = [f(0), f(2), f(4)];
        let p1 = [f(1), f(3), f(5)];
        let (w0, w1) = (f(6), f(7));
        let (n0, n1) = if has_normals { ([f(8), f(10), f(12)], [f(9), f(11), f(13)]) } else { ([0.0; 3], [0.0; 3]) };
        o += double_stride;
        vertices.push(SkinVertex {
            p0,
            p1,
            n0,
            n1,
            w0,
            w1,
            joint0: map_joint(refs0[i].index),
            joint1: Some(map_joint(refs1[i].index)),
        });
    }
    let mirrored = symmetric.then(|| {
        vertices
            .iter()
            .enumerate()
            .map(|(i, v)| SkinVertex {
                p0: flip(v.p0, refs0[i].flip_axis),
                p1: flip(v.p1, refs1[i].flip_axis),
                n0: flip(v.n0, refs0[i].flip_axis),
                n1: flip(v.n1, refs1[i].flip_axis),
                w0: v.w0,
                w1: v.w1,
                joint0: map_joint(refs0[i].flipped_index),
                joint1: v.joint1.map(|_| map_joint(refs1[i].flipped_index)),
            })
            .collect()
    });

    // Instruction stream.
    let mut pieces = Vec::new();
    let mut texture = String::new();
    let mut props = default_props();
    let mut o = instruction_offset;
    let mut add = |strip: bool, corners: Vec<Corner>, texture: &str, props: RenderProps| {
        pieces.push(Piece {
            strip,
            corners: corners.clone(),
            texture: texture.to_string(),
            props,
            mirrored: false,
        });
        if symmetric {
            pieces.push(Piece {
                strip,
                corners,
                texture: texture.to_string(),
                props,
                mirrored: true,
            });
        }
    };
    loop {
        if o + 2 > p.len() {
            bail!("mesh instruction stream runs past the section");
        }
        let op = u16_at(p, o);
        o += 2;
        match op {
            0xFFFF => break,
            0x8000 => {
                if o + 16 > p.len() {
                    bail!("texture name past the section");
                }
                texture = name_str(&p[o..o + 16]);
                o += 16;
            }
            0x8010 => {
                if o + 44 > p.len() {
                    bail!("render props past the section");
                }
                props = RenderProps {
                    display_type: p[o + 13],
                    ambient_multiplier: f32_at(p, o + 16),
                    specular_power: f32_at(p, o + 36),
                    specular_enabled: f32_at(p, o + 40) == 1.0,
                };
                o += 44;
            }
            0x5453 | 0x4353 => {
                // Textured / untextured triangle strip.
                let textured = op == 0x5453;
                let n = u16_at(p, o) as usize;
                o += 2;
                let mut corners = Vec::with_capacity(n + 2);
                if textured {
                    if o + 30 > p.len() {
                        bail!("strip past the section");
                    }
                    for k in 0..3 {
                        corners.push(Corner {
                            vertex: u16_at(p, o + k * 2),
                            uv: [f32_at(p, o + 6 + k * 8), f32_at(p, o + 10 + k * 8)],
                            color: 0x8080_8080,
                        });
                    }
                    o += 30;
                    for _ in 1..n {
                        if o + 10 > p.len() {
                            bail!("strip past the section");
                        }
                        corners.push(Corner {
                            vertex: u16_at(p, o),
                            uv: [f32_at(p, o + 2), f32_at(p, o + 6)],
                            color: 0x8080_8080,
                        });
                        o += 10;
                    }
                } else {
                    if o + 10 > p.len() {
                        bail!("strip past the section");
                    }
                    let color = u32_at(p, o + 6);
                    for k in 0..3 {
                        corners.push(Corner {
                            vertex: u16_at(p, o + k * 2),
                            uv: [0.0; 2],
                            color,
                        });
                    }
                    o += 10;
                    for _ in 1..n {
                        if o + 2 > p.len() {
                            bail!("strip past the section");
                        }
                        corners.push(Corner {
                            vertex: u16_at(p, o),
                            uv: [0.0; 2],
                            color,
                        });
                        o += 2;
                    }
                }
                add(true, corners, if textured { &texture } else { "" }, props);
            }
            0x0054 | 0x0043 => {
                // Textured / untextured triangle list.
                let textured = op == 0x0054;
                let n = u16_at(p, o) as usize;
                o += 2;
                let mut corners = Vec::with_capacity(n * 3);
                for _ in 0..n {
                    if textured {
                        if o + 30 > p.len() {
                            bail!("triangle list past the section");
                        }
                        for k in 0..3 {
                            corners.push(Corner {
                                vertex: u16_at(p, o + k * 2),
                                uv: [f32_at(p, o + 6 + k * 8), f32_at(p, o + 10 + k * 8)],
                                color: 0x8080_8080,
                            });
                        }
                        o += 30;
                    } else {
                        if o + 10 > p.len() {
                            bail!("triangle list past the section");
                        }
                        let color = u32_at(p, o + 6);
                        for k in 0..3 {
                            corners.push(Corner {
                                vertex: u16_at(p, o + k * 2),
                                uv: [0.0; 2],
                                color,
                            });
                        }
                        o += 10;
                    }
                }
                add(false, corners, if textured { &texture } else { "" }, props);
            }
            other => bail!("unknown mesh opcode {other:#06x} at {:#x}", o - 2),
        }
    }
    Ok(SkinnedMesh {
        id: id_str(id),
        vertices,
        mirrored,
        pieces,
        has_normals,
        occlude_type,
    })
}

impl Piece {
    /// Triangle-list corner indices (into `corners`), strips unrolled and degenerate
    /// triangles dropped. Strip triangle `i` keeps a consistent winding by swapping every
    /// other one.
    pub fn triangles(&self) -> Vec<[usize; 3]> {
        let mut out = Vec::new();
        if self.strip {
            for i in 0..self.corners.len().saturating_sub(2) {
                let (a, b, c) = if i % 2 == 0 { (i, i + 1, i + 2) } else { (i + 1, i, i + 2) };
                let (va, vb, vc) = (self.corners[a].vertex, self.corners[b].vertex, self.corners[c].vertex);
                if va == vb || vb == vc || va == vc {
                    continue;
                }
                out.push([a, b, c]);
            }
        } else {
            for t in 0..self.corners.len() / 3 {
                out.push([t * 3, t * 3 + 1, t * 3 + 2]);
            }
        }
        out
    }
}

// ---- animation ----

/// One joint's samples: `frames` entries each (constant channels repeated).
#[derive(Debug, Clone)]
pub struct Track {
    pub joint: usize,
    pub rotations: Vec<[f32; 4]>,
    pub translations: Vec<[f32; 3]>,
    pub scales: Vec<[f32; 3]>,
    /// A negative channel offset in the file: the joint is pinned to its bind pose.
    pub reset: bool,
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub id: String,
    pub frames: usize,
    /// Key-frame duration in 1/30 s units: the clip lasts `(frames − 1) / key_frame_duration`
    /// frames at 30 fps.
    pub key_frame_duration: f32,
    pub tracks: Vec<Track>,
}

impl Animation {
    /// Length in seconds.
    pub fn duration(&self) -> f32 {
        let kfd = if self.key_frame_duration > 0.0 { self.key_frame_duration } else { 1.0 };
        (self.frames.saturating_sub(1)).max(1) as f32 / kfd / 30.0
    }
}

fn read_channels<const N: usize>(p: &[u8], o: usize, frames: usize, base: usize) -> Result<Option<Vec<[f32; N]>>> {
    let mut offsets = [0i32; N];
    let mut consts = [0f32; N];
    for k in 0..N {
        offsets[k] = i32_at(p, o + k * 4);
        consts[k] = f32_at(p, o + N * 4 + k * 4) % 10000.0;
    }
    if offsets.iter().any(|&x| x < 0) {
        return Ok(None);
    }
    let mut out = vec![[0f32; N]; frames];
    for k in 0..N {
        if offsets[k] == 0 {
            for f in out.iter_mut() {
                f[k] = consts[k];
            }
        } else {
            let start = base + offsets[k] as usize * 4;
            if start + frames * 4 > p.len() {
                bail!("animation channel data out of range");
            }
            for (f, v) in out.iter_mut().enumerate() {
                v[k] = f32_at(p, start + f * 4);
            }
        }
    }
    Ok(Some(out))
}

pub fn parse_animation(id: &[u8; 4], p: &[u8]) -> Result<Animation> {
    if p.len() < 10 {
        bail!("animation section too short");
    }
    let joints = u16_at(p, 2) as usize;
    let frames = u16_at(p, 4) as usize;
    let key_frame_duration = f32_at(p, 6);
    let base = 10;
    let mut tracks = Vec::with_capacity(joints);
    let mut o = base;
    for _ in 0..joints {
        if o + 84 > p.len() {
            bail!("animation joint table out of range");
        }
        let joint = i32_at(p, o);
        let rot = read_channels::<4>(p, o + 4, frames, base)?;
        let trans = read_channels::<3>(p, o + 36, frames, base)?;
        let scale = read_channels::<3>(p, o + 60, frames, base)?;
        o += 84;
        // The client masks the sign bit off the joint index of reset tracks.
        let joint = (joint & 0x7FFF_FFFF) as usize;
        match (rot, trans, scale) {
            (Some(r), Some(t), Some(s)) => tracks.push(Track {
                joint,
                rotations: r,
                translations: t,
                scales: s,
                reset: false,
            }),
            _ => tracks.push(Track {
                joint,
                rotations: vec![[0.0, 0.0, 0.0, 1.0]],
                translations: vec![[0.0; 3]],
                scales: vec![[1.0; 3]],
                reset: true,
            }),
        }
    }
    Ok(Animation {
        id: id_str(id),
        frames: frames.max(1),
        key_frame_duration,
        tracks,
    })
}

// ---- routines ----

#[derive(Debug, Clone)]
pub struct RoutineCommand {
    /// Clip reference; `?` matches any body-region slot digit.
    pub clip: String,
    /// Start delay in 1/60 s ticks (relative delays summed).
    pub delay: u32,
    pub duration: u16,
    pub max_loops: u16,
}

#[derive(Debug, Clone)]
pub struct Routine {
    pub id: String,
    pub commands: Vec<RoutineCommand>,
    /// Other routines this one calls (their commands play too).
    pub calls: Vec<String>,
    /// Sound effects: (sound pointer section id of the same DAT, start in 1/60 s ticks).
    pub sounds: Vec<(String, u32)>,
}

pub fn parse_routine(id: &[u8; 4], p: &[u8]) -> Option<Routine> {
    if p.len() < 0x20 {
        return None;
    }
    let sec2 = i32_at(p, 0x14);
    let start = sec2.checked_sub(16)?;
    if start < 0 {
        return None;
    }
    let mut o = start as usize;
    let mut commands = Vec::new();
    let mut calls = Vec::new();
    let mut sounds = Vec::new();
    let mut clock = 0u32;
    for _ in 0..128 {
        if o + 8 > p.len() {
            break;
        }
        let op = p[o];
        let n = (u16_at(p, o + 1) & 0x1F) as usize;
        let len = n.max(1) * 4;
        let at = clock;
        if op != 0 {
            clock += u16_at(p, o + 4) as u32;
        }
        if op == 0x05 && o + 32 <= p.len() {
            let r = &p[o + 8..o + 12];
            if r.iter().all(|b| (0x20..=0x7E).contains(b)) {
                commands.push(RoutineCommand {
                    clip: String::from_utf8_lossy(r).trim_end().to_string(),
                    delay: at,
                    duration: u16_at(p, o + 6),
                    max_loops: u16_at(p, o + 30),
                });
            }
        }
        if matches!(op, 0x03 | 0x09 | 0x3B | 0x3C | 0x57) && o + 12 <= p.len() {
            let r = name_str(&p[o + 8..o + 12]);
            if !r.is_empty() && r.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
                calls.push(r.trim_end().to_string());
            }
        }
        // Sound ops (xi-model-viewer `effect.js` SOUND_OPS): the ref is a `SeSep` section id.
        if matches!(op, 0x0A | 0x0B | 0x4A | 0x53 | 0x60) && o + 12 <= p.len() {
            let r = name_str(&p[o + 8..o + 12]);
            if !r.is_empty() && r.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
                sounds.push((r.trim_end().to_string(), at));
            }
        }
        if op == 0 {
            break;
        }
        o += len;
    }
    Some(Routine {
        id: id_str(id),
        commands,
        calls,
        sounds,
    })
}

/// Whether a routine clip reference names an animation id (`at0?` matches `at00`).
pub fn clip_matches(reference: &str, id: &str) -> bool {
    match reference.find('?') {
        Some(q) => id.starts_with(&reference[..q]),
        None => id == reference || id.strip_prefix(reference).is_some_and(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit()),
    }
}

// ---- info ----

#[derive(Debug, Clone, Copy, Default)]
pub struct Info {
    pub weapon_animation_type: u8,
    pub weapon_animation_sub_type: u8,
    /// Joint reference of the weapon's grip joint (re-parented onto the hand when drawn).
    pub grip_reference: Option<u8>,
    /// Which waist motion pack a body pairs with (2 = the second block).
    pub waist_variant: u8,
}

pub fn parse_info(p: &[u8]) -> Option<Info> {
    if p.len() < 16 {
        return None;
    }
    Some(Info {
        weapon_animation_type: p[3],
        weapon_animation_sub_type: p[4],
        grip_reference: (p[6] != 0xFF).then_some(p[6]),
        waist_variant: if p[9] == 0xFF { 0 } else { p[9] },
    })
}

// ---- a whole DAT ----

/// Everything an entity DAT contributes.
#[derive(Debug, Clone, Default)]
pub struct EntityDat {
    pub skeleton: Option<Skeleton>,
    pub meshes: Vec<SkinnedMesh>,
    pub animations: Vec<Animation>,
    pub routines: Vec<Routine>,
    pub info: Option<Info>,
    /// Textures by name.
    pub textures: Vec<crate::tex::Img>,
    /// Sound pointers (`SeSep` sections): section id → `se` id (weapon draw `sotr`, sheathe
    /// `sinr`, swing `skaz`, hit `shit`; face voices `atk1`, `dam1`, ...).
    pub sounds: Vec<(String, u32)>,
    pub notes: Vec<String>,
}

pub fn parse_entity(key_prefix: &str, data: &[u8]) -> EntityDat {
    let mut out = EntityDat::default();
    for s in crate::dat::sections(data) {
        let payload = &data[s.start..s.end];
        match s.kind {
            SECTION_SKELETON => match parse_skeleton(&s.id, payload) {
                Ok(sk) if out.skeleton.is_none() => out.skeleton = Some(sk),
                Ok(_) => {}
                Err(e) => out.notes.push(format!("{}: {e:#}", id_str(&s.id))),
            },
            SECTION_MESH => match parse_mesh(&s.id, payload) {
                Ok(m) => out.meshes.push(m),
                Err(e) => out.notes.push(format!("{}: {e:#}", id_str(&s.id))),
            },
            SECTION_ANIMATION => match parse_animation(&s.id, payload) {
                Ok(a) => out.animations.push(a),
                Err(e) => out.notes.push(format!("{}: {e:#}", id_str(&s.id))),
            },
            SECTION_ROUTINE => {
                if let Some(r) = parse_routine(&s.id, payload) {
                    out.routines.push(r);
                }
            }
            SECTION_INFO => {
                if &s.id == b"info" && out.info.is_none() {
                    out.info = parse_info(payload);
                }
            }
            crate::dat::SECTION_IMG => {
                if let Some(img) = crate::tex::decode_img(key_prefix, payload) {
                    out.textures.push(img);
                }
            }
            crate::dat::SECTION_SESEP => {
                if payload.len() >= 12 && payload.starts_with(b"SeSep") {
                    out.sounds.push((id_str(&s.id), u32_at(payload, 8)));
                }
            }
            _ => {}
        }
    }
    out
}
