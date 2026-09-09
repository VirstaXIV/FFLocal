//! Zone DAT → instances (MZB), meshes (MMB) and textures (IMG).
//!
//! MZB (after decryption): 32-byte header (`u24` record count at +4, `offsetEndRecord100` at
//! +20) then records of 84, 92 or 100 bytes: 16-byte name (XOR 0x55), translation, XYZ Euler
//! rotation (radians), scale. MMB: 16-byte head, 16-byte name, `pieces` at +32, block offset
//! at +60; each block is a 32-byte header (`numModel` first) followed by models of 16-byte
//! texture name, `u16` vertex count, `u16` blend flags, 36-byte vertices (pos, normal, BGRA
//! colour with 0x80 = 1.0, uv; 48 bytes with a wind-sway target after the position when bit
//! 1 of the section's config byte at +4 is set), `u32` index count and a `u16` triangle
//! strip, padded to 4. Blend flags (xi-model-viewer `zone.js`): 0x8000 = translucent,
//! 0x2000 = no back-face culling; alpha testing is decided by the MODEL name instead: models
//! whose name starts with `_` cut out below 0.375 of texture × vertex alpha.
//! SeSep (type 0x3D): `"SeSep  \0"`, `u32` se id at +8 (folder `id / 1000`, file
//! `se{id:06}.spw`), `u32` size at +12, `u32` 2 at +16, then an undecoded byte-code.

use std::collections::HashMap;

use anyhow::{Result, bail};
use ffl_core::TextureRef;

use crate::collision;
use crate::dat::{self, SECTION_IMG, SECTION_MMB, SECTION_MZB, SECTION_SESEP};
use crate::tex;

#[derive(Debug, Clone)]
pub struct Instance {
    pub name: [u8; 16],
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, Default)]
pub struct MmbMesh {
    pub texture: [u8; 16],
    pub blend: u16,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex wind-sway offset (the client draws `pos + wind × sway`); empty when the
    /// section has no sway stream.
    pub sway: Vec<[f32; 3]>,
    /// Triangle list (strips already unrolled, degenerates dropped, winding made consistent
    /// with the stored normals).
    pub indices: Vec<u32>,
}

impl MmbMesh {
    pub fn translucent(&self) -> bool {
        self.blend & 0x8000 != 0
    }

    pub fn two_sided(&self) -> bool {
        self.blend & 0x2000 != 0
    }
}

pub struct ZoneDat {
    pub instances: Vec<Instance>,
    pub models: HashMap<[u8; 16], Vec<MmbMesh>>,
    pub textures: HashMap<[u8; 16], TextureRef>,
    /// The MZB's collision blocks (see `collision`), when the zone has them.
    pub collision: Option<collision::ZoneCollision>,
    /// Distinct sound-effect ids referenced by the zone's SeSep sections, in file order.
    pub sounds: Vec<u32>,
    pub notes: Vec<String>,
}

fn u32_at(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}
fn f32_at(p: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}

pub fn name_str(n: &[u8; 16]) -> String {
    String::from_utf8_lossy(n).trim_end_matches(['\0', ' ']).to_string()
}

pub fn parse_zone(zone: u16, data: &mut [u8]) -> Result<ZoneDat> {
    let secs = dat::sections(data);
    let mut instances = Vec::new();
    let mut models = HashMap::new();
    let mut textures = HashMap::new();
    let mut collision = None;
    let mut sounds: Vec<u32> = Vec::new();
    let mut sound_refs = 0usize;
    let mut notes = Vec::new();
    let mut bad_mmb = 0;
    for s in &secs {
        let payload = &mut data[s.start..s.end];
        match s.kind {
            SECTION_MZB => {
                let encrypted = dat::decode_mzb(payload);
                match parse_mzb(payload, encrypted) {
                    Ok(v) => instances.extend(v),
                    Err(e) => notes.push(format!("mzb: {e:#}")),
                }
                match collision::parse(payload) {
                    Ok(Some(c)) => collision = Some(c),
                    Ok(None) => notes.push("mzb has no collision blocks".into()),
                    Err(e) => notes.push(format!("mzb collision: {e:#}")),
                }
            }
            SECTION_MMB => {
                dat::decode_mmb(payload);
                if payload.len() < 96 {
                    continue;
                }
                match parse_mmb(payload) {
                    Ok((name, meshes)) => {
                        models.insert(name, meshes);
                    }
                    Err(_) => bad_mmb += 1,
                }
            }
            SECTION_IMG => {
                if let Some(img) = tex::decode_img(&format!("ff11/z{zone}"), payload) {
                    textures.insert(img.name, img.texture);
                }
            }
            SECTION_SESEP => {
                if payload.len() >= 12 && payload.starts_with(b"SeSep") {
                    sound_refs += 1;
                    let id = u32_at(payload, 8);
                    if !sounds.contains(&id) {
                        sounds.push(id);
                    }
                }
            }
            _ => {}
        }
    }
    if bad_mmb > 0 {
        notes.push(format!("{bad_mmb} MMB sections failed to parse"));
    }
    if sound_refs > 0 {
        notes.push(format!("{sound_refs} sound references ({} distinct)", sounds.len()));
    }
    if instances.is_empty() {
        bail!("no MZB instances in zone {zone}");
    }
    Ok(ZoneDat {
        instances,
        models,
        textures,
        collision,
        sounds,
        notes,
    })
}

fn parse_mzb(p: &mut [u8], encrypted: bool) -> Result<Vec<Instance>> {
    if p.len() < 32 {
        bail!("MZB too short");
    }
    let count = (u32_at(p, 4) & 0xFF_FFFF) as usize;
    let end = u32_at(p, 20) as usize;
    let size = [84usize, 92, 100].into_iter().find(|s| 32 + count * s == end);
    let Some(size) = size else {
        bail!("MZB record size not recognised (count {count}, end {end})");
    };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let o = 32 + i * size;
        if o + size > p.len() {
            break;
        }
        let mut name = [0u8; 16];
        name.copy_from_slice(&p[o..o + 16]);
        if encrypted {
            for b in &mut name {
                *b ^= 0x55;
            }
        }
        let f = |k: usize| f32_at(p, o + 16 + k * 4);
        out.push(Instance {
            name,
            translation: [f(0), f(1), f(2)],
            rotation: [f(3), f(4), f(5)],
            scale: [f(6), f(7), f(8)],
        });
    }
    Ok(out)
}

fn parse_mmb(m: &[u8]) -> Result<([u8; 16], Vec<MmbMesh>)> {
    let mut name = [0u8; 16];
    name.copy_from_slice(&m[16..32]);
    // Config byte: bit 0 = strips, bit 1 = vertices carry a wind-sway target (48 bytes).
    let config = u32_at(m, 4) & 0xFF;
    let stride = if config & 0x2 != 0 { 48 } else { 36 };
    let pieces = u32_at(m, 32) as i32;
    if pieces <= 0 || pieces > 4096 {
        bail!("bad piece count {pieces}");
    }
    let mut o = u32_at(m, 60) as usize;
    if o == 0 || o >= m.len() {
        o = if pieces == 1 { 64 } else if pieces <= 16 { 96 } else { 32 + pieces as usize * 4 };
    }
    let mut meshes = Vec::new();
    for _ in 0..pieces {
        if o + 32 > m.len() {
            bail!("block header past end");
        }
        let nmodel = u32_at(m, o) as i32;
        if !(0..=4096).contains(&nmodel) {
            bail!("bad model count {nmodel}");
        }
        o += 32;
        for _ in 0..nmodel {
            if o + 20 > m.len() {
                bail!("model header past end");
            }
            let mut texture = [0u8; 16];
            texture.copy_from_slice(&m[o..o + 16]);
            let nver = u16::from_le_bytes([m[o + 16], m[o + 17]]) as usize;
            let blend = u16::from_le_bytes([m[o + 18], m[o + 19]]);
            o += 20;
            if o + nver * stride + 4 > m.len() {
                bail!("vertices past end");
            }
            let mut mesh = MmbMesh {
                texture,
                blend,
                ..Default::default()
            };
            for v in 0..nver {
                let b = o + v * stride;
                mesh.positions.push([f32_at(m, b), f32_at(m, b + 4), f32_at(m, b + 8)]);
                let n = if stride == 48 {
                    mesh.sway.push([f32_at(m, b + 12), f32_at(m, b + 16), f32_at(m, b + 20)]);
                    b + 24
                } else {
                    b + 12
                };
                mesh.normals.push([f32_at(m, n), f32_at(m, n + 4), f32_at(m, n + 8)]);
                // D3DCOLOR: bytes B, G, R, A; 0x80 is 1.0.
                let c = &m[n + 12..n + 16];
                mesh.colors.push([c[2] as f32 / 128.0, c[1] as f32 / 128.0, c[0] as f32 / 128.0, (c[3] as f32 / 128.0).min(1.0)]);
                mesh.uvs.push([f32_at(m, n + 16), f32_at(m, n + 20)]);
            }
            o += nver * stride;
            let nidx = u32_at(m, o) as usize;
            o += 4;
            if nidx > 0x10000 || o + nidx * 2 > m.len() {
                bail!("indices past end");
            }
            let strip: Vec<u16> = (0..nidx).map(|i| u16::from_le_bytes([m[o + i * 2], m[o + i * 2 + 1]])).collect();
            o += nidx * 2;
            o = (o + 3) & !3;
            if strip.iter().any(|&i| i as usize >= nver) {
                bail!("index out of range");
            }
            mesh.indices = strip_to_list(&strip, &mesh.positions, &mesh.normals);
            if !mesh.indices.is_empty() {
                meshes.push(mesh);
            }
        }
    }
    Ok((name, meshes))
}

/// Unroll a triangle strip, drop degenerate triangles and pick the winding that agrees with the
/// stored vertex normals (D3D front faces are clockwise; Bevy's are counter-clockwise).
fn strip_to_list(strip: &[u16], pos: &[[f32; 3]], nrm: &[[f32; 3]]) -> Vec<u32> {
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for i in 0..strip.len().saturating_sub(2) {
        let (a, b, c) = (strip[i] as u32, strip[i + 1] as u32, strip[i + 2] as u32);
        if a == b || b == c || a == c {
            continue;
        }
        tris.push(if i % 2 == 0 { [a, b, c] } else { [b, a, c] });
    }
    let mut agree = 0.0f32;
    for t in &tris {
        let p0 = glam::Vec3::from(pos[t[0] as usize]);
        let p1 = glam::Vec3::from(pos[t[1] as usize]);
        let p2 = glam::Vec3::from(pos[t[2] as usize]);
        let face = (p1 - p0).cross(p2 - p0);
        let n = glam::Vec3::from(nrm[t[0] as usize]) + glam::Vec3::from(nrm[t[1] as usize]) + glam::Vec3::from(nrm[t[2] as usize]);
        agree += face.dot(n);
    }
    let mut out = Vec::with_capacity(tris.len() * 3);
    for t in tris {
        if agree < 0.0 {
            out.extend_from_slice(&[t[0], t[2], t[1]]);
        } else {
            out.extend_from_slice(&t);
        }
    }
    out
}
