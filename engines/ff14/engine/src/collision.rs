//! Player collision (`.pcb`) → collision meshes with ground materials.
//!
//! Placed models keep their collision in `<zone>/collision/<model stem>.pcb` (the layer's
//! `Replace` path always matches the stem in Mist), terrain in `<zone>/collision/trNNNN.pcb`
//! (world space; `list.pcb` lists the pieces). Every polygon carries a `material` word whose low
//! byte is the surface id of the executable's 24-entry footstep table (Mist: sea plane 0x0d =
//! water, plaza 0x04 = stone, planks 0x05 = wood, ground 0x02 = grass); the high bits are flags.

use ffl_core::{CollisionMesh, SurfaceKind};
use physis::pcb::{Pcb, ResourceNode};

/// The executable's surface id → footstep material table (`sound/foot/foot/fs_%s_...`).
pub const SURFACE_TABLE: [SurfaceKind; 24] = [
    SurfaceKind::Unknown, // 0 None
    SurfaceKind::Dirt,    // 1 dart
    SurfaceKind::Grass,   // 2
    SurfaceKind::Sand,    // 3
    SurfaceKind::Stone,   // 4
    SurfaceKind::Wood,    // 5
    SurfaceKind::Metal,   // 6
    SurfaceKind::Gravel,  // 7
    SurfaceKind::Leaf,    // 8
    SurfaceKind::Powder,  // 9
    SurfaceKind::Carpet,  // 10
    SurfaceKind::Snow,    // 11
    SurfaceKind::Water,   // 12
    SurfaceKind::Water,   // 13
    SurfaceKind::Dirt,    // 14 soil
    SurfaceKind::Dirt,    // 15 soil
    SurfaceKind::Dirt,    // 16 soil
    SurfaceKind::Dirt,    // 17 soil
    SurfaceKind::Dirt,    // 18 soil
    SurfaceKind::Dirt,    // 19 soil
    SurfaceKind::Dirt,    // 20 soil
    SurfaceKind::Water,   // 21
    SurfaceKind::Grass,   // 22
    SurfaceKind::Metal,   // 23
];

pub fn surface_of_material(material: u64) -> SurfaceKind {
    SURFACE_TABLE.get((material & 0xFF) as usize).copied().unwrap_or(SurfaceKind::Unknown)
}

/// One collision mesh per surface kind present in the file.
pub fn collision_meshes(pcb: &Pcb) -> Vec<CollisionMesh> {
    let mut by_surface: Vec<(SurfaceKind, CollisionMesh)> = Vec::new();
    fn walk(node: &ResourceNode, out: &mut Vec<(SurfaceKind, CollisionMesh)>) {
        for poly in &node.polygons {
            let surface = surface_of_material(poly.material);
            let entry = match out.iter_mut().position(|(s, _)| *s == surface) {
                Some(i) => &mut out[i].1,
                None => {
                    out.push((surface, CollisionMesh { surface, ..Default::default() }));
                    &mut out.last_mut().unwrap().1
                }
            };
            let base = entry.positions.len() as u32;
            let mut ok = true;
            for &vi in &poly.vertex_indices {
                match node.vertices.get(vi as usize) {
                    Some(v) => entry.positions.push(*v),
                    None => ok = false,
                }
            }
            if ok {
                entry.indices.extend_from_slice(&[base, base + 1, base + 2]);
            } else {
                entry.positions.truncate(base as usize);
            }
        }
        for c in &node.children {
            walk(c, out);
        }
    }
    walk(&pcb.root_node, &mut by_surface);
    by_surface.into_iter().map(|(_, m)| m).filter(|m| m.indices.len() >= 3).collect()
}

/// `bg/.../bgparts/x.mdl` → `bg/.../collision/x.pcb`.
pub fn model_collision_path(mdl_path: &str) -> Option<String> {
    let (dir, file) = mdl_path.rsplit_once('/')?;
    let (zone_dir, _) = dir.rsplit_once('/')?;
    let stem = file.strip_suffix(".mdl")?;
    Some(format!("{zone_dir}/collision/{stem}.pcb"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_materials() {
        assert_eq!(model_collision_path("bg/ffxiv/sea_s1/hou/s1h1/bgparts/s1h0_a0_roc02.mdl").as_deref(), Some("bg/ffxiv/sea_s1/hou/s1h1/collision/s1h0_a0_roc02.pcb"));
        assert_eq!(surface_of_material(0xb80d), SurfaceKind::Water);
        assert_eq!(surface_of_material(0x7004), SurfaceKind::Stone);
        assert_eq!(surface_of_material(0x7002), SurfaceKind::Grass);
        assert_eq!(surface_of_material(0x606400), SurfaceKind::Unknown);
    }
}
