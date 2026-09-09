//! MZB collision geometry (the "header 2" blocks) → world-space triangle meshes.
//!
//! Layout (GalkaReeve's DatLoader `SMZBHeader2`/`SMZBBlock92`/`SMZBBlock16`, verified on
//! zones 100 and 230): the MZB header's `u32` at +8 points at a 32-byte second header:
//! `u24` piece count (+ 8 flag bits), piece offset, entry count, entry offset, grid offset,
//! transform offset, transform count, unknown. All offsets are relative to the MZB payload.
//!
//! - Pieces (`SMZBBlock92`, variable length, consecutive): `u32 r1, r2, r3` — vertices at
//!   `r1..r2` and normals at `r2..r3` (`float[3]` each, `r1` = record start + 16), then a
//!   `u32` whose low 16 bits are the face count and high 16 bits flags (0 or 1 seen), the
//!   vertices, the normals and the faces: four `u16` = three vertex indices and one normal
//!   index, with flag bits above the index (`0x8000` on most; [`limit`] strips them as the
//!   reference does). The walk ends exactly at the transform block.
//! - Transforms: 192-byte records, one per MZB placement record in order: a row-vector 4×4
//!   forward matrix (`world = v * m`, translation in the last row), its inverse, and 64
//!   unexplained bytes. The reference names them `SMZBBlock112` and reads only the first 64
//!   bytes; entries address them by absolute offset, so the record size only matters for
//!   mapping a transform back to its instance (`(offset - base) / stride`).
//! - Entries (`SMZBBlock16`): `u32 unk, transform offset, piece offset, next`; a non-zero
//!   `next` is the first transform offset of a list of `(transform, piece)` offset pairs
//!   that ends when a transform slot reads 0. The walk ends exactly at the grid offset. The
//!   grid after it indexes the entries spatially and is not needed here.
//!
//! Coordinates are Y-down like the render meshes: rotate 180° about X (negate Y and Z).

use std::collections::HashMap;

use anyhow::{Result, bail};

#[derive(Debug, Clone, Default)]
pub struct Piece {
    /// Offset of the record in the MZB payload (the key entries use).
    pub offset: u32,
    pub flags: u16,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Vertex indices 0..3 and the normal index at 3, flag bits stripped.
    pub faces: Vec<[u16; 4]>,
}

#[derive(Debug, Clone)]
pub struct Placement {
    /// Index of the MZB placement record (instance) whose transform this is.
    pub instance: usize,
    /// Row-vector 4×4 (D3D): `world = v * m`, translation at 12..15.
    pub matrix: [f32; 16],
    /// Indices into [`ZoneCollision::pieces`].
    pub pieces: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ZoneCollision {
    pub pieces: Vec<Piece>,
    /// Sorted by instance index.
    pub placements: Vec<Placement>,
    pub entries: usize,
    /// (transform, piece) pairs listed by the entries, before grouping.
    pub pairs: usize,
    pub transform_count: usize,
    pub transform_stride: usize,
    pub notes: Vec<String>,
}

/// A world-space (Y-up) triangle list.
#[derive(Debug, Clone, Default)]
pub struct TriMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub triangles: usize,
    pub vertices: usize,
    pub bounds: Option<([f32; 3], [f32; 3])>,
    /// (piece flags, piece count).
    pub flag_counts: Vec<(u16, usize)>,
    /// Pieces not referenced by any entry.
    pub unused_pieces: usize,
}

/// FFXI is Y-down; rotate 180° about X so Y is up (no mirroring, so winding survives).
pub fn y_up(p: [f32; 3]) -> [f32; 3] {
    [p[0], -p[1], -p[2]]
}

fn rd_u32(p: &[u8], o: usize) -> Result<u32> {
    match p.get(o..o + 4) {
        Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        None => bail!("read past the end of the MZB at {o:#x} ({} bytes)", p.len()),
    }
}

fn rd_u16(p: &[u8], o: usize) -> Result<u16> {
    match p.get(o..o + 2) {
        Some(b) => Ok(u16::from_le_bytes([b[0], b[1]])),
        None => bail!("read past the end of the MZB at {o:#x} ({} bytes)", p.len()),
    }
}

fn rd_f32(p: &[u8], o: usize) -> Result<f32> {
    Ok(f32::from_bits(rd_u32(p, o)?))
}

fn rd_vec3(p: &[u8], o: usize) -> Result<[f32; 3]> {
    Ok([rd_f32(p, o)?, rd_f32(p, o + 4)?, rd_f32(p, o + 8)?])
}

/// Strip the flag bits of a face index (reference `CDatLoader::limit`): clear high bits
/// until the value is a valid index.
pub fn limit(count: usize, mut val: u16) -> u16 {
    if (val as usize) < count {
        return val;
    }
    let mut mask: u16 = 0x7FFF;
    while mask != 0 {
        val &= mask;
        mask >>= 1;
        if (val as usize) < count {
            return val;
        }
    }
    0
}

/// Parse the collision blocks of a decrypted MZB payload. `Ok(None)` when the MZB has no
/// second header.
pub fn parse(p: &[u8]) -> Result<Option<ZoneCollision>> {
    if p.len() < 32 {
        bail!("MZB too short");
    }
    let off_h2 = rd_u32(p, 8)? as usize;
    if off_h2 == 0 {
        return Ok(None);
    }
    if off_h2 + 32 > p.len() {
        bail!("collision header at {off_h2:#x} is past the end ({} bytes)", p.len());
    }
    let tot92 = (rd_u32(p, off_h2)? & 0xFF_FFFF) as usize;
    let off92 = rd_u32(p, off_h2 + 4)? as usize;
    let tot16 = rd_u32(p, off_h2 + 8)? as usize;
    let off16 = rd_u32(p, off_h2 + 12)? as usize;
    let off_grid = rd_u32(p, off_h2 + 16)? as usize;
    let off112 = rd_u32(p, off_h2 + 20)? as usize;
    let tot112 = rd_u32(p, off_h2 + 24)? as usize;
    if tot92 == 0 || tot16 == 0 || tot112 == 0 {
        return Ok(None);
    }
    for (name, o) in [("pieces", off92), ("entries", off16), ("grid", off_grid), ("transforms", off112)] {
        if o < 32 || o > p.len() {
            bail!("collision {name} offset {o:#x} out of range ({} bytes)", p.len());
        }
    }
    let mut notes = Vec::new();

    // Pieces.
    let mut pieces = Vec::with_capacity(tot92);
    let mut by_offset: HashMap<u32, usize> = HashMap::with_capacity(tot92);
    let mut o = off92;
    for i in 0..tot92 {
        let start = o;
        let r1 = rd_u32(p, o)? as usize;
        let r2 = rd_u32(p, o + 4)? as usize;
        let r3 = rd_u32(p, o + 8)? as usize;
        let sb = rd_u32(p, o + 12)?;
        if r1 != start + 16 || r2 < r1 || r3 < r2 || (r2 - r1) % 12 != 0 || (r3 - r2) % 12 != 0 {
            bail!("piece {i} at {start:#x}: ranges {r1:#x} {r2:#x} {r3:#x} do not follow the record");
        }
        let nv = (r2 - r1) / 12;
        let nn = (r3 - r2) / 12;
        let nf = (sb & 0xFFFF) as usize;
        let flags = (sb >> 16) as u16;
        let end = r3 + nf * 8;
        if end > p.len() {
            bail!("piece {i} at {start:#x}: {nv} vertices, {nn} normals, {nf} faces run past the end");
        }
        let mut piece = Piece {
            offset: start as u32,
            flags,
            positions: Vec::with_capacity(nv),
            normals: Vec::with_capacity(nn),
            faces: Vec::with_capacity(nf),
        };
        for k in 0..nv {
            piece.positions.push(rd_vec3(p, r1 + k * 12)?);
        }
        for k in 0..nn {
            piece.normals.push(rd_vec3(p, r2 + k * 12)?);
        }
        for k in 0..nf {
            let b = r3 + k * 8;
            piece.faces.push([
                limit(nv, rd_u16(p, b)?),
                limit(nv, rd_u16(p, b + 2)?),
                limit(nv, rd_u16(p, b + 4)?),
                limit(nn, rd_u16(p, b + 6)?),
            ]);
        }
        by_offset.insert(start as u32, pieces.len());
        pieces.push(piece);
        o = end;
    }
    if o != off112 {
        notes.push(format!("piece walk ended at {o:#x}, transforms start at {off112:#x}"));
    }

    // Transform record size: the transforms fill the space up to the entries.
    let stride = if off16 > off112 && (off16 - off112) % tot112 == 0 { (off16 - off112) / tot112 } else { 192 };
    if stride < 64 || off112 + tot112 * stride > p.len() {
        bail!("transform block: {tot112} records of {stride} bytes at {off112:#x} do not fit");
    }
    if stride != 192 {
        notes.push(format!("transform records are {stride} bytes (192 expected)"));
    }

    // Entries → (transform offset, piece offset) pairs.
    let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(tot16);
    let mut o = off16;
    for _ in 0..tot16 {
        let t = rd_u32(p, o + 4)?;
        let pc = rd_u32(p, o + 8)?;
        let next = rd_u32(p, o + 12)?;
        o += 16;
        pairs.push((t, pc));
        if next != 0 {
            let mut pending = Some(next);
            loop {
                let r = rd_u32(p, o)?;
                o += 4;
                match pending.take() {
                    Some(t) => pairs.push((t, r)),
                    None => pending = Some(r),
                }
                if r == 0 {
                    break;
                }
            }
        }
    }
    if o != off_grid {
        notes.push(format!("entry walk ended at {o:#x}, grid starts at {off_grid:#x}"));
    }

    // Group by transform (= MZB instance).
    let mut placements: Vec<Placement> = Vec::new();
    let mut by_transform: HashMap<u32, usize> = HashMap::new();
    let (mut bad_piece, mut bad_transform, mut dup) = (0usize, 0usize, 0usize);
    for &(t, pc) in &pairs {
        let Some(&pi) = by_offset.get(&pc) else {
            bad_piece += 1;
            continue;
        };
        let t = t as usize;
        if t < off112 || (t - off112) % stride != 0 || (t - off112) / stride >= tot112 {
            bad_transform += 1;
            continue;
        }
        let instance = (t - off112) / stride;
        let idx = *by_transform.entry(t as u32).or_insert_with(|| {
            let mut matrix = [0f32; 16];
            for (k, m) in matrix.iter_mut().enumerate() {
                *m = f32::from_bits(u32::from_le_bytes([p[t + k * 4], p[t + k * 4 + 1], p[t + k * 4 + 2], p[t + k * 4 + 3]]));
            }
            placements.push(Placement { instance, matrix, pieces: Vec::new() });
            placements.len() - 1
        });
        if placements[idx].pieces.contains(&pi) {
            dup += 1;
        } else {
            placements[idx].pieces.push(pi);
        }
    }
    if bad_piece > 0 || bad_transform > 0 || dup > 0 {
        notes.push(format!("{bad_piece} entries point at no piece, {bad_transform} at no transform, {dup} duplicates"));
    }
    placements.sort_by_key(|pl| pl.instance);
    Ok(Some(ZoneCollision {
        pieces,
        placements,
        entries: tot16,
        pairs: pairs.len(),
        transform_count: tot112,
        transform_stride: stride,
        notes,
    }))
}

fn transform_point(m: &[f32; 16], v: [f32; 3]) -> [f32; 3] {
    y_up([
        m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12],
        m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13],
        m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14],
    ])
}

fn transform_dir(m: &[f32; 16], v: [f32; 3]) -> [f32; 3] {
    y_up([
        m[0] * v[0] + m[4] * v[1] + m[8] * v[2],
        m[1] * v[0] + m[5] * v[1] + m[9] * v[2],
        m[2] * v[0] + m[6] * v[1] + m[10] * v[2],
    ])
}

impl ZoneCollision {
    /// World-space (Y-up) triangle list of one placement: its pieces transformed by the
    /// placement matrix, triangles wound to agree with the stored face normals (the matrix
    /// may mirror), per-vertex normals accumulated from the face normals.
    pub fn placement_mesh(&self, pl: &Placement) -> TriMesh {
        let mut out = TriMesh::default();
        for &pi in &pl.pieces {
            let piece = &self.pieces[pi];
            let base = out.positions.len() as u32;
            let world: Vec<glam::Vec3> = piece.positions.iter().map(|&v| glam::Vec3::from(transform_point(&pl.matrix, v))).collect();
            let mut acc = vec![glam::Vec3::ZERO; world.len()];
            for f in &piece.faces {
                let (a, b, c) = (f[0] as usize, f[1] as usize, f[2] as usize);
                if a >= world.len() || b >= world.len() || c >= world.len() || a == b || b == c || a == c {
                    continue;
                }
                let stored = piece.normals.get(f[3] as usize).map(|&n| glam::Vec3::from(transform_dir(&pl.matrix, n)));
                let face = (world[b] - world[a]).cross(world[c] - world[a]);
                let n = stored.filter(|n| n.length_squared() > 0.0).unwrap_or(face);
                let flip = face.dot(n) < 0.0;
                let (b, c) = if flip { (c, b) } else { (b, c) };
                out.indices.extend_from_slice(&[base + a as u32, base + b as u32, base + c as u32]);
                for i in [a, b, c] {
                    acc[i] += n.normalize_or_zero();
                }
            }
            out.positions.extend(world.iter().map(|v| v.to_array()));
            out.normals.extend(acc.iter().map(|n| n.try_normalize().unwrap_or(glam::Vec3::Y).to_array()));
        }
        out
    }

    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        let mut used = vec![false; self.pieces.len()];
        let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
        for pl in &self.placements {
            for &pi in &pl.pieces {
                used[pi] = true;
                let piece = &self.pieces[pi];
                s.triangles += piece.faces.len();
                s.vertices += piece.positions.len();
                for &v in &piece.positions {
                    let w = transform_point(&pl.matrix, v);
                    for k in 0..3 {
                        min[k] = min[k].min(w[k]);
                        max[k] = max[k].max(w[k]);
                    }
                }
            }
        }
        if s.vertices > 0 {
            s.bounds = Some((min, max));
        }
        s.unused_pieces = used.iter().filter(|u| !**u).count();
        let mut flags: HashMap<u16, usize> = HashMap::new();
        for piece in &self.pieces {
            *flags.entry(piece.flags).or_default() += 1;
        }
        s.flag_counts = flags.into_iter().collect();
        s.flag_counts.sort();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_strips_flag_bits() {
        assert_eq!(limit(4, 3), 3);
        assert_eq!(limit(4, 0x8003), 3);
        assert_eq!(limit(4, 0xC002), 2);
        assert_eq!(limit(5, 0x4001), 1);
    }

    /// Prints the collision statistics of West Ronfaure (100) and Southern San d'Oria (230)
    /// and checks that the collision bounds overlap the render meshes' bounds.
    #[test]
    #[ignore = "needs an FFXI install (FFLOCAL_FF11_PATH or Steam)"]
    fn zone_collision_stats() {
        let Ok(root) = crate::dat::locate(None) else {
            return;
        };
        let install = crate::dat::Install::open(&root).unwrap();
        for zone in [100u16, 230] {
            let mut data = install.read(100 + zone as u32).unwrap();
            let z = crate::zone::parse_zone(zone, &mut data).unwrap();
            let c = z.collision.as_ref().expect("collision blocks");
            let s = c.stats();
            eprintln!(
                "zone {zone}: {} pieces, {} entries, {} pairs, {} transforms (stride {}), {} placements, {} triangles, {} vertices, flags {:?}, unused pieces {}, bounds {:?}, notes {:?}",
                c.pieces.len(),
                c.entries,
                c.pairs,
                c.transform_count,
                c.transform_stride,
                c.placements.len(),
                s.triangles,
                s.vertices,
                s.flag_counts,
                s.unused_pieces,
                s.bounds,
                c.notes
            );
            let render = crate::render_bounds(&z).expect("render bounds");
            eprintln!("zone {zone}: render bounds {render:?}");
            assert_eq!(c.transform_stride, 192);
            assert_eq!(c.transform_count, z.instances.len());
            assert!(s.triangles > 10_000);
            assert!(c.notes.iter().all(|n| !n.contains("walk ended")), "{:?}", c.notes);
            let (cmin, cmax) = s.bounds.unwrap();
            for k in 0..3 {
                assert!(cmin[k] < render.1[k] && cmax[k] > render.0[k], "axis {k}: collision {cmin:?}..{cmax:?} vs render {render:?}");
            }
            let mesh_tris: usize = c.placements.iter().map(|pl| c.placement_mesh(pl).indices.len() / 3).sum();
            assert_eq!(mesh_tris, s.triangles);
        }
    }
}
