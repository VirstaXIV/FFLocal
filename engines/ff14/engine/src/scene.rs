//! Zone graph → `ffl_core::Scene`.

use ffl_core::{AmbientEmitter, CollisionHint, LightDesc, MusicRegion, Node, NodeKind, Scene, Trs};
use ffl_ff14_assets::AssetSource;
use ffl_ff14_assets::music::{music_set_for_value, scd_key};
use ffl_ff14_assets::zone::{Placement, ZoneGraph};

/// FFXIV layer rotations are XYZ Euler radians applied as Rz·Ry·Rx (matches Novus/GLM).
pub fn placement_to_trs(p: &Placement) -> Trs {
    let q = glam::Quat::from_euler(glam::EulerRot::ZYX, p.rotation[2], p.rotation[1], p.rotation[0]);
    Trs {
        translation: p.translation,
        rotation: q.to_array(),
        scale: p.scale,
    }
}

/// World position of a placement nested in the graph's groups.
fn world_position(graph: &ZoneGraph, parent: Option<usize>, local: [f32; 3]) -> [f32; 3] {
    let mut pos = glam::Vec3::from(local);
    let mut p = parent;
    while let Some(i) = p {
        let g = &graph.groups[i];
        let q = glam::Quat::from_euler(glam::EulerRot::ZYX, g.placement.rotation[2], g.placement.rotation[1], g.placement.rotation[0]);
        pos = q * (pos * glam::Vec3::from(g.placement.scale)) + glam::Vec3::from(g.placement.translation);
        p = g.parent;
    }
    pos.to_array()
}

/// World rotation of a placement nested in the graph's groups.
fn world_rotation(graph: &ZoneGraph, parent: Option<usize>, local: &Placement) -> glam::Quat {
    let mut q = glam::Quat::from_euler(glam::EulerRot::ZYX, local.rotation[2], local.rotation[1], local.rotation[0]);
    let mut p = parent;
    while let Some(i) = p {
        let g = &graph.groups[i];
        q = glam::Quat::from_euler(glam::EulerRot::ZYX, g.placement.rotation[2], g.placement.rotation[1], g.placement.rotation[0]) * q;
        p = g.parent;
    }
    q
}

/// How a placed sound file plays: its sound program 0 names a looping bed (an entry with loop
/// points) and/or one-shots (entries without). A normal program with a single non-looping
/// entry is an interaction cue (summoning bells) and is not placed as ambience.
struct AmbientProgram {
    bed: Option<usize>,
    spots: Vec<usize>,
}

fn ambient_program(source: &dyn AssetSource, path: &str) -> Option<AmbientProgram> {
    let scd = ffl_ff14_assets::loaders::load_scd(source, path).ok()?;
    let program = scd.sounds.first();
    let entries: Vec<&ffl_ff14_assets::scd::ScdEntry> = match program {
        Some(p) if !p.audio.is_empty() => {
            let mut seen = Vec::new();
            for &i in &p.audio {
                if !seen.contains(&i) {
                    seen.push(i);
                }
            }
            seen.iter().filter_map(|&i| scd.entries.get(i).and_then(|e| e.as_ref())).collect()
        }
        _ => scd.present().collect(),
    };
    let loops = |e: &ffl_ff14_assets::scd::ScdEntry| e.marker.as_ref().map(|m| m.loop_end > 0).unwrap_or(e.loop_end > 0);
    let bed = entries.iter().find(|e| loops(e)).map(|e| e.index);
    let spots: Vec<usize> = entries.iter().filter(|e| !loops(e)).map(|e| e.index).collect();
    let kind = program.map(|p| p.kind).unwrap_or(1);
    // Kind 1 (normal) plays once when triggered: without a loop it is not ambience.
    if bed.is_none() && kind == 1 {
        return None;
    }
    Some(AmbientProgram { bed, spots })
}

/// Placed `Sound` objects as ambient emitters. The object scale is (inner radius, height,
/// max distance); `x == 0` means a point source.
pub fn sound_emitters(graph: &ZoneGraph, source: &dyn AssetSource, notes: &mut Vec<String>) -> Vec<AmbientEmitter> {
    let mut programs: std::collections::HashMap<&str, Option<AmbientProgram>> = std::collections::HashMap::new();
    let mut skipped: std::collections::BTreeMap<String, usize> = Default::default();
    let mut out = Vec::new();
    for s in graph.sounds.iter().filter(|s| source.exists(&s.scd_path)) {
        let program = programs.entry(s.scd_path.as_str()).or_insert_with(|| ambient_program(source, &s.scd_path));
        let name = s.scd_path.rsplit('/').next().unwrap_or("").trim_end_matches(".scd").to_string();
        let Some(program) = program else {
            *skipped.entry(name).or_default() += 1;
            continue;
        };
        let [inner, height, max] = s.placement.scale;
        out.push(AmbientEmitter {
            name,
            key: program.bed.map(|i| scd_key(source, &s.scd_path, i)).unwrap_or_default(),
            spots: program.spots.iter().map(|&i| scd_key(source, &s.scd_path, i)).collect(),
            // The interval lives in the SCD track commands, which are not decoded: a spread
            // that keeps creaks and splashes occasional.
            spot_interval: (6.0, 16.0),
            position: world_position(graph, s.parent, s.placement.translation),
            positional: true,
            inner_radius: inner.max(0.0),
            max_distance: max.max(inner + 1.0),
            height: height.max(1.0),
            gain: 1.0,
        });
    }
    for (name, n) in skipped {
        notes.push(format!("{n} placed {name} sounds are interaction cues, not ambience"));
    }
    out
}

/// `MapRange` boxes with their own music.
pub fn music_regions(graph: &ZoneGraph, excel: &ffl_ff14_assets::ExcelCache, source: &dyn AssetSource, territory: u32, notes: &mut Vec<String>) -> Vec<MusicRegion> {
    let names = excel.sheet("PlaceName").ok();
    let mut out = Vec::new();
    for r in graph.map_ranges.iter().filter(|r| r.bgm_enabled && r.bgm != 0) {
        let music = match music_set_for_value(excel, source, territory, r.bgm as u64) {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(err) => {
                notes.push(format!("map range {}: bgm {}: {err:#}", r.instance_id, r.bgm));
                continue;
            }
        };
        let name = names
            .as_ref()
            .and_then(|n| n.string(r.place_name_spot, "Name").ok())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("range {}", r.instance_id));
        let shape = match r.shape.as_str() {
            "Box" => ffl_core::RegionShape::Box,
            "Cylinder" => ffl_core::RegionShape::Cylinder,
            "Sphere" => ffl_core::RegionShape::Sphere,
            other => {
                notes.push(format!("map range {name}: {other} shape treated as a box"));
                ffl_core::RegionShape::Box
            }
        };
        out.push(MusicRegion {
            name,
            shape,
            position: world_position(graph, r.parent, r.placement.translation),
            rotation: world_rotation(graph, r.parent, &r.placement).to_array(),
            half_extents: [r.placement.scale[0].abs(), r.placement.scale[1].abs(), r.placement.scale[2].abs()],
            priority: r.priority as i32,
            music,
        });
    }
    out
}

/// Aetheryte and aethernet shard positions of a territory from the `Aetheryte` → `Level` sheets.
pub fn aetheryte_positions(excel: &ffl_ff14_assets::ExcelCache, territory: u32) -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    let (Ok(aetherytes), Ok(levels)) = (excel.sheet("Aetheryte"), excel.sheet("Level")) else {
        return out;
    };
    let float = |row: u32, name: &str| -> Option<f32> {
        match levels.field(row, name).ok()? {
            physis::excel::Field::Float32(v) => Some(*v),
            _ => None,
        }
    };
    for page in &aetherytes.sheet.pages {
        for entry in &page.entries {
            let row = entry.id;
            if aetherytes.integer(row, "Territory").ok() != Some(territory as u64) {
                continue;
            }
            // The main aetheryte first (IsAetheryte), shards after.
            let main = aetherytes.integer(row, "IsAetheryte").unwrap_or(0) != 0;
            for i in 0..4 {
                let Ok(level) = aetherytes.integer(row, &format!("Level[{i}]")) else {
                    continue;
                };
                if level == 0 {
                    continue;
                }
                if let (Some(x), Some(y), Some(z)) = (float(level as u32, "X"), float(level as u32, "Y"), float(level as u32, "Z")) {
                    if main {
                        out.insert(0, [x, y, z]);
                    } else {
                        out.push([x, y, z]);
                    }
                }
            }
        }
    }
    out
}

pub fn zone_to_scene(graph: &ZoneGraph, map_id: &str, name: &str, preferred_spawns: &[[f32; 3]]) -> Scene {
    let mut nodes = Vec::with_capacity(graph.groups.len() + graph.models.len() + graph.lights.len());
    // Groups first (their parents always precede them), remembering their node indices.
    let mut group_nodes = Vec::with_capacity(graph.groups.len());
    for g in &graph.groups {
        let parent = g.parent.map(|p| group_nodes[p]);
        nodes.push(Node {
            name: g.sgb_path.rsplit('/').next().unwrap_or("").to_string(),
            parent,
            transform: placement_to_trs(&g.placement),
            kind: NodeKind::Empty,
        });
        group_nodes.push(nodes.len() - 1);
    }
    for m in &graph.models {
        nodes.push(Node {
            name: m.mdl_path.rsplit('/').next().unwrap_or("").to_string(),
            parent: m.parent.map(|p| group_nodes[p]),
            transform: placement_to_trs(&m.placement),
            // `ModelCollisionType::None` in the layer means "no replacement collision file", not
            // "no collision": floors and walls carry it. Until the PCB files are used, every
            // placed model is solid.
            kind: NodeKind::Model {
                model: m.mdl_path.clone(),
                collision: CollisionHint::Mesh,
            },
        });
    }
    for l in &graph.lights {
        nodes.push(Node {
            name: "light".into(),
            parent: l.parent.map(|p| group_nodes[p]),
            transform: placement_to_trs(&l.placement),
            kind: NodeKind::Light(LightDesc {
                color: l.color,
                intensity: l.intensity,
                range: l.range,
                spot_angle_degrees: (l.cone_degrees > 0.0).then_some(l.cone_degrees),
            }),
        });
    }

    // Spawn hints: median of top-level object positions, then nearest real objects.
    let tops: Vec<[f32; 3]> = graph
        .models
        .iter()
        .filter(|m| m.parent.is_none())
        .map(|m| m.placement.translation)
        .chain(graph.groups.iter().filter(|g| g.parent.is_none()).map(|g| g.placement.translation))
        .collect();
    let median = |axis: usize| -> f32 {
        let mut v: Vec<f32> = tops.iter().map(|t| t[axis]).collect();
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    let mut fallback = tops.clone();
    let median_center = [median(0), median(1), median(2)];
    fallback.sort_by(|a, b| {
        let da = (a[0] - median_center[0]).powi(2) + (a[2] - median_center[2]).powi(2);
        let db = (b[0] - median_center[0]).powi(2) + (b[2] - median_center[2]).powi(2);
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    });
    fallback.truncate(40);
    // Real spawn points first: aetherytes/shards (preferred), then the level's player pop
    // ranges, then the geometric fallback.
    let mut candidates: Vec<[f32; 3]> = preferred_spawns.to_vec();
    candidates.extend(graph.spawn_ranges.iter().map(|r| world_position(graph, r.parent, r.placement.translation)));
    let center = candidates.first().copied().unwrap_or(median_center);
    candidates.extend(fallback);

    let mut notes = vec![format!(
        "{} models, {} groups, {} lights, {} terrain plates, {} aetheryte + {} pop-range spawns",
        graph.models.len(),
        graph.groups.len(),
        graph.lights.len(),
        graph.stats.terrain_plates,
        preferred_spawns.len(),
        graph.spawn_ranges.len()
    )];
    for (k, n) in &graph.stats.skipped_by_type {
        notes.push(format!("skipped {k}: {n}"));
    }
    let outdoors = graph.stats.terrain_plates > 0;
    Scene {
        engine: "ff14".into(),
        map: map_id.to_string(),
        name: name.to_string(),
        nodes,
        spawn_candidates: candidates,
        center,
        environment: ffl_core::Environment {
            outdoors,
            ambient: if outdoors { 1.0 } else { 0.6 },
            ..Default::default()
        },
        music: None,
        music_regions: Vec::new(),
        emitters: Vec::new(),
        footsteps: None,
        collision: Vec::new(),
        notes,
        content: Default::default(),
    }
}
