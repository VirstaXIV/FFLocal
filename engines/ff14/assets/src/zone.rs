//! Zone graph: everything needed to place a territory's static geometry, as plain data.
//!
//! Pipeline: `TerritoryType.Bg` → `bg/<Bg>.lvb` → its LGB list → BG parts and
//! (recursively) shared groups → plus terrain plates from `bgplate/terrain.tera`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use physis::layer::{Layer, LayerEntryData, ModelCollisionType, Transformation};
use physis::sgb::Sgb;

use crate::excel::ExcelCache;
use crate::loaders::{load_lgb, load_lvb, load_sgb, load_tera};
use crate::source::AssetSource;

/// Translation, XYZ Euler rotation in radians (applied as Rz·Ry·Rx like the game), scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

impl Placement {
    pub const IDENTITY: Placement = Placement {
        translation: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0; 3],
    };

    fn from_transformation(t: &Transformation) -> Self {
        Self {
            translation: t.translation,
            rotation: t.rotation,
            scale: t.scale,
        }
    }
}

/// How a BG part collides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Collision {
    /// Game default: the model's own collision mesh, if it has one.
    Default,
    /// No collision at all.
    None,
    /// A separate `.pcb` file replaces the model's collision.
    Replace(String),
    /// Axis-aligned box collision.
    Box,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlacedKind {
    /// A `BG` layer entry.
    BgPart,
    /// A terrain plate from `terrain.tera`.
    Terrain,
}

/// One model instance in the zone.
#[derive(Debug, Clone)]
pub struct PlacedModel {
    pub kind: PlacedKind,
    /// Full game path to the `.mdl`.
    pub mdl_path: String,
    /// Local placement relative to `parent` (or the zone origin).
    pub placement: Placement,
    /// Index into [`ZoneGraph::groups`] when nested in a shared group.
    pub parent: Option<usize>,
    pub collision: Collision,
    pub clip_range: f32,
    pub instance_id: u32,
    pub layer_name: String,
}

/// A shared-group (`.sgb`) instance; children reference it by index.
#[derive(Debug, Clone)]
pub struct GroupNode {
    pub sgb_path: String,
    pub placement: Placement,
    pub parent: Option<usize>,
    pub instance_id: u32,
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct SpawnRange {
    pub placement: Placement,
    pub parent: Option<usize>,
    pub layer: String,
}

#[derive(Debug, Clone)]
pub struct PlacedLight {
    pub placement: Placement,
    pub parent: Option<usize>,
    pub light_type: String,
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: f32,
    pub cone_degrees: f32,
}

/// A placed ambient sound (`Sound` layer entry). The object's scale is the emitter shape:
/// (inner radius, height, max distance) — verified on Mist's `sound.lgb`.
#[derive(Debug, Clone)]
pub struct PlacedSound {
    pub placement: Placement,
    pub parent: Option<usize>,
    pub scd_path: String,
    pub sound_effect_param: i32,
    pub instance_id: u32,
    pub layer_name: String,
}

/// An environment volume (`EnvSet`) with its sound scape (`.essb`) — not used yet.
#[derive(Debug, Clone)]
pub struct PlacedEnvSet {
    pub placement: Placement,
    pub parent: Option<usize>,
    pub shape: String,
    pub priority: u8,
    pub effective_range: f32,
    pub reverb: f32,
    pub filter: f32,
    pub sound_asset_path: String,
    pub layer_name: String,
}

/// A `MapRange` trigger box: sub-area name, and sub-area music when `bgm_enabled`.
#[derive(Debug, Clone)]
pub struct PlacedMapRange {
    pub placement: Placement,
    pub parent: Option<usize>,
    pub shape: String,
    pub priority: i16,
    pub place_name_spot: u32,
    /// A `TerritoryType.BGM`-style value (0 = none).
    pub bgm: u32,
    pub bgm_enabled: bool,
    pub bgm_play_zone_in_only: bool,
    pub instance_id: u32,
    pub layer_name: String,
}

#[derive(Debug, Default, Clone)]
pub struct ZoneStats {
    /// Layer entries skipped, keyed by their `LayerEntryData` variant name.
    pub skipped_by_type: BTreeMap<String, usize>,
    pub layers_skipped_festival: usize,
    /// BG parts whose `is_visible` byte was 0 (informational).
    pub bg_flagged_invisible: usize,
    pub unique_models: usize,
    pub unique_groups: usize,
    pub max_group_depth: u32,
    pub terrain_plates: usize,
    pub missing_sgb: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ZoneGraph {
    pub territory_id: u32,
    pub bg: String,
    pub lvb_path: String,
    /// Base directory for terrain and level files, e.g. `bg/ffxiv/sea_s1/hou/s1h1`.
    pub base_dir: String,
    pub lgb_paths: Vec<String>,
    pub groups: Vec<GroupNode>,
    pub models: Vec<PlacedModel>,
    pub lights: Vec<PlacedLight>,
    /// Player spawn ranges from the level (`PopRange` with `PopType::PC`), local to `parent`.
    pub spawn_ranges: Vec<SpawnRange>,
    /// Ambient sound emitters (empty-path obstruction probes are skipped).
    pub sounds: Vec<PlacedSound>,
    pub env_sets: Vec<PlacedEnvSet>,
    pub map_ranges: Vec<PlacedMapRange>,
    pub stats: ZoneStats,
}

impl ZoneGraph {
    /// Distinct model paths, in first-seen order.
    pub fn unique_model_paths(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for m in &self.models {
            if seen.insert(m.mdl_path.as_str()) {
                out.push(m.mdl_path.as_str());
            }
        }
        out
    }
}

const MAX_GROUP_DEPTH: u32 = 8;

struct Builder<'a> {
    source: &'a dyn AssetSource,
    graph: ZoneGraph,
    sgb_cache: HashMap<String, Option<Arc<Sgb>>>,
}

impl<'a> Builder<'a> {
    fn entry_kind(data: &LayerEntryData) -> String {
        let dbg = format!("{data:?}");
        dbg.split(['(', ' ', '{']).next().unwrap_or("?").to_string()
    }

    fn process_layer(&mut self, layer: &Layer, parent: Option<usize>, depth: u32, stack: &mut Vec<String>) {
        if layer.header.festival_id != 0 {
            self.graph.stats.layers_skipped_festival += 1;
            return;
        }
        let layer_name = layer.header.name.value.clone();
        for object in &layer.objects {
            let placement = Placement::from_transformation(&object.transform);
            match &object.data {
                LayerEntryData::BG(bg) => {
                    // `is_visible` is 0 for practically every part in retail data, so it is
                    // not a render gate; count it for diagnostics only.
                    if !bg.is_visible {
                        self.graph.stats.bg_flagged_invisible += 1;
                    }
                    let asset = bg.asset_path.value.clone();
                    if asset.is_empty() {
                        continue;
                    }
                    let collision = match bg.collision_type {
                        ModelCollisionType::None => Collision::None,
                        ModelCollisionType::Replace => {
                            Collision::Replace(bg.collision_asset_path.value.clone())
                        }
                        ModelCollisionType::Box => Collision::Box,
                        #[allow(unreachable_patterns)]
                        _ => Collision::Default,
                    };
                    self.graph.models.push(PlacedModel {
                        kind: PlacedKind::BgPart,
                        mdl_path: asset,
                        placement,
                        parent,
                        collision,
                        clip_range: bg.render_model_clip_range,
                        instance_id: object.instance_id,
                        layer_name: layer_name.clone(),
                    });
                }
                LayerEntryData::SharedGroup(sg) => {
                    let path = sg.asset_path.value.clone();
                    if path.is_empty() {
                        continue;
                    }
                    if depth >= MAX_GROUP_DEPTH || stack.contains(&path) {
                        tracing::warn!("shared group recursion limit/cycle at {path}");
                        continue;
                    }
                    let group_index = self.graph.groups.len();
                    self.graph.groups.push(GroupNode {
                        sgb_path: path.clone(),
                        placement,
                        parent,
                        instance_id: object.instance_id,
                        depth: depth + 1,
                    });
                    self.graph.stats.max_group_depth =
                        self.graph.stats.max_group_depth.max(depth + 1);
                    let sgb = self.sgb(&path);
                    let Some(sgb) = sgb else {
                        continue;
                    };
                    stack.push(path);
                    if let Some(section) = sgb.sections.first() {
                        for lg in &section.layer_groups {
                            for child in &lg.layers {
                                self.process_layer(child, Some(group_index), depth + 1, stack);
                            }
                        }
                    }
                    stack.pop();
                }
                LayerEntryData::LayLight(light) => {
                    let c = &light.diffuse_color_hdri;
                    self.graph.lights.push(PlacedLight {
                        placement,
                        parent,
                        light_type: format!("{:?}", light.light_type),
                        color: [
                            c.red as f32 / 255.0,
                            c.green as f32 / 255.0,
                            c.blue as f32 / 255.0,
                        ],
                        intensity: c.intensity,
                        range: light.range_rate,
                        cone_degrees: light.cone_degree,
                    });
                }
                LayerEntryData::PopRange(pop) if pop.pop_type == physis::layer::PopType::PC => {
                    self.graph.spawn_ranges.push(SpawnRange {
                        placement,
                        parent,
                        layer: layer_name.clone(),
                    });
                }
                LayerEntryData::Sound(sound) => {
                    let path = sound.asset_path.value.clone();
                    if path.is_empty() {
                        // Obstruction probes: no asset, unit scale.
                        continue;
                    }
                    self.graph.sounds.push(PlacedSound {
                        placement,
                        parent,
                        scd_path: path,
                        sound_effect_param: sound.sound_effect_param,
                        instance_id: object.instance_id,
                        layer_name: layer_name.clone(),
                    });
                }
                LayerEntryData::EnvSet(env) => {
                    self.graph.env_sets.push(PlacedEnvSet {
                        placement,
                        parent,
                        shape: format!("{:?}", env.shape),
                        priority: env.priority,
                        effective_range: env.effective_range,
                        reverb: env.reverb,
                        filter: env.filter,
                        sound_asset_path: env.sound_asset_path.value.clone(),
                        layer_name: layer_name.clone(),
                    });
                }
                LayerEntryData::MapRange(range) => {
                    self.graph.map_ranges.push(PlacedMapRange {
                        placement,
                        parent,
                        shape: format!("{:?}", range.parent_data.trigger_box_shape),
                        priority: range.parent_data.priority,
                        place_name_spot: range.place_name_spot,
                        bgm: range.bgm,
                        bgm_enabled: range.bgm_enabled,
                        bgm_play_zone_in_only: range.bgm_play_zone_in_only,
                        instance_id: object.instance_id,
                        layer_name: layer_name.clone(),
                    });
                }
                other => {
                    *self
                        .graph
                        .stats
                        .skipped_by_type
                        .entry(Self::entry_kind(other))
                        .or_default() += 1;
                }
            }
        }
    }

    fn sgb(&mut self, path: &str) -> Option<Arc<Sgb>> {
        if let Some(cached) = self.sgb_cache.get(path) {
            return cached.clone();
        }
        let loaded = match load_sgb(self.source, path) {
            Ok(s) => Some(Arc::new(s)),
            Err(err) => {
                tracing::warn!("{err:#}");
                self.graph.stats.missing_sgb.push(path.to_string());
                None
            }
        };
        self.sgb_cache.insert(path.to_string(), loaded.clone());
        loaded
    }

    fn add_terrain(&mut self) -> Result<()> {
        let tera_path = format!("{}/bgplate/terrain.tera", self.graph.base_dir);
        if !self.source.exists(&tera_path) {
            tracing::info!("no terrain at {tera_path}");
            return Ok(());
        }
        let terrain = load_tera(self.source, &tera_path)?;
        for (i, plate) in terrain.plates.iter().enumerate() {
            let pos = terrain.plate_position(plate);
            let mdl_path = format!(
                "{}/bgplate/{}",
                self.graph.base_dir,
                physis::tera::Terrain::mdl_filename(i)
            );
            self.graph.models.push(PlacedModel {
                kind: PlacedKind::Terrain,
                mdl_path,
                placement: Placement {
                    translation: [pos[0], 0.0, pos[1]],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                parent: None,
                collision: Collision::Default,
                clip_range: terrain.clip_distance,
                instance_id: i as u32,
                layer_name: "terrain".into(),
            });
        }
        self.graph.stats.terrain_plates = terrain.plates.len();
        Ok(())
    }
}

/// Resolve `TerritoryType.Bg` for a territory row.
pub fn territory_bg(excel: &ExcelCache, territory_id: u32) -> Result<String> {
    let bg = excel.string("TerritoryType", territory_id, "Bg")?;
    if bg.is_empty() {
        return Err(anyhow!("TerritoryType {territory_id} has an empty Bg path"));
    }
    Ok(bg)
}

/// Build the zone graph for a territory.
pub fn build_zone(
    source: &dyn AssetSource,
    excel: &ExcelCache,
    territory_id: u32,
) -> Result<ZoneGraph> {
    let bg = territory_bg(excel, territory_id)?;
    build_zone_from_bg(source, territory_id, &bg)
}

/// Build the zone graph from a raw `TerritoryType.Bg` value (e.g. `ffxiv/sea_s1/hou/s1h1/level/s1h1`).
pub fn build_zone_from_bg(source: &dyn AssetSource, territory_id: u32, bg: &str) -> Result<ZoneGraph> {
    let lvb_path = format!("bg/{bg}.lvb");
    let lvb = load_lvb(source, &lvb_path).with_context(|| format!("loading {lvb_path}"))?;
    let section = lvb
        .sections
        .first()
        .ok_or_else(|| anyhow!("{lvb_path} has no sections"))?;

    let mut base_dir = section.general.bg_path.value.trim_end_matches('/').to_string();
    if base_dir.is_empty() {
        // Fall back to the level directory's parent.
        base_dir = format!("bg/{bg}")
            .rsplitn(3, '/')
            .last()
            .unwrap_or("")
            .to_string();
    }

    let mut builder = Builder {
        source,
        graph: ZoneGraph {
            territory_id,
            bg: bg.to_string(),
            lvb_path: lvb_path.clone(),
            base_dir,
            lgb_paths: section.lgb_paths.clone(),
            groups: Vec::new(),
            models: Vec::new(),
            lights: Vec::new(),
            spawn_ranges: Vec::new(),
            sounds: Vec::new(),
            env_sets: Vec::new(),
            map_ranges: Vec::new(),
            stats: ZoneStats::default(),
        },
        sgb_cache: HashMap::new(),
    };

    for lgb_path in &section.lgb_paths {
        let lgb = match load_lgb(source, lgb_path) {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!("skipping {lgb_path}: {err:#}");
                continue;
            }
        };
        let mut stack = Vec::new();
        for chunk in &lgb.chunks {
            for layer in &chunk.layers {
                builder.process_layer(layer, None, 0, &mut stack);
            }
        }
    }

    builder.add_terrain()?;

    let unique_models = builder.graph.unique_model_paths().len();
    builder.graph.stats.unique_models = unique_models;
    builder.graph.stats.unique_groups = builder.sgb_cache.len();
    Ok(builder.graph)
}
