//! FFXI engine: zones from the installed game's `ROM*/…/*.DAT` files, read in place.
//!
//! Zone `n` lives in DAT file id `100 + n` (verified for all 293 named zones). The DAT holds
//! the placement list (MZB), the meshes (MMB, keyed by 16-byte name) and the textures (IMG).
//! Player characters (`character`, `pc`, `entity`): race skeleton + motion packs, face and gear
//! DATs by the client's model-id tables; emotes from the race's emote DATs.
//! Sound: zone music from a LandSandBoat-derived table (`zone_music`), ambience beds and a
//! provisional footstep bank from the zone's SeSep references (`sound`).

pub mod character;
pub mod collision;
pub mod dat;
pub mod entity;
pub mod gear_names;
pub mod pc;
pub mod sound;
pub mod tex;
pub mod zone;
pub mod zone_music;
pub mod zones;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use ffl_core::{
    AmbientEmitter, CatalogEntry, CharacterModel, CharacterPreset, Clip, ClockSpec, CollisionHint, ContentPolicy, Engine, EngineInfo, FieldKind, MapInfo, MaterialDesc, MeshData, ModelData,
    MusicSet, Node, NodeKind, PresetField, Provenance, Scene, ShaderDef, SoundData, SurfaceKind, TextureSlotDef, Trs,
};
use pc::{Look, Race, Slot};
use sound::SoundKind;

/// Alpha-test threshold of `_`-prefixed models (texture alpha × vertex alpha, 0x80 = 1.0).
const FF11_ALPHA_TEST: f32 = 0.375;

pub const ENGINE_ID: &str = "ff11";
pub const SHADER_ZONE: &str = "ff11/zone";

pub struct Ff11Engine {
    install: Option<dat::Install>,
    error: Option<String>,
    zones: Mutex<HashMap<u16, Arc<zone::ZoneDat>>>,
}

impl Ff11Engine {
    /// Open the install (see [`dat::locate`]); an engine that failed to open still registers,
    /// reporting itself unavailable.
    pub fn open(explicit: Option<&Path>) -> Self {
        match dat::locate(explicit).and_then(|root| dat::Install::open(&root)) {
            Ok(install) => {
                tracing::info!("FFXI at {}", install.root.display());
                Self {
                    install: Some(install),
                    error: None,
                    zones: Mutex::new(HashMap::new()),
                }
            }
            Err(err) => Self {
                install: None,
                error: Some(format!("{err:#}")),
                zones: Mutex::new(HashMap::new()),
            },
        }
    }

    /// The engine's shaders (the same with or without an install).
    pub fn shader_defs() -> Vec<ShaderDef> {
        vec![ShaderDef {
            id: SHADER_ZONE.into(),
            entry: "ff11_zone".into(),
            source: include_str!("../shaders/zone.wgsl").into(),
            slots: Vec::<TextureSlotDef>::new(),
        }]
    }

    /// The opened install (file index and sound directories).
    pub fn install(&self) -> Result<&dat::Install> {
        self.install.as_ref().ok_or_else(|| anyhow!("FFXI engine unavailable: {}", self.error.clone().unwrap_or_default()))
    }

    /// Music key for a track id, or `None` with a note when the track is silent (0), missing
    /// or in a codec we cannot decode (ATRAC3).
    fn music_key(&self, id: u16, notes: &mut Vec<String>) -> Option<String> {
        if id == 0 {
            return None;
        }
        let name = zone_music::music_name(id).unwrap_or("?");
        let install = self.install().ok()?;
        match install.sound_header(SoundKind::Bgw, id as u32).and_then(|h| sound::header(&h)) {
            Ok(h) if h.codec == sound::CODEC_ATRAC3 => {
                notes.push(format!("music {id:03} ({name}) is ATRAC3 (unsupported)"));
                None
            }
            Ok(h) if !h.decodable() => {
                notes.push(format!("music {id:03} ({name}) has unknown codec {}", h.codec));
                None
            }
            Ok(_) => Some(sound::sound_key(SoundKind::Bgw, id as u32)),
            Err(e) => {
                notes.push(format!("music {id:03} ({name}): {e:#}"));
                None
            }
        }
    }

    fn zone(&self, zone_id: u16) -> Result<Arc<zone::ZoneDat>> {
        if let Some(z) = self.zones.lock().unwrap().get(&zone_id) {
            return Ok(z.clone());
        }
        let install = self.install()?;
        let mut data = install.read(100 + zone_id as u32).with_context(|| format!("zone {zone_id}"))?;
        let parsed = Arc::new(zone::parse_zone(zone_id, &mut data)?);
        for n in &parsed.notes {
            tracing::warn!("zone {zone_id}: {n}");
        }
        self.zones.lock().unwrap().insert(zone_id, parsed.clone());
        Ok(parsed)
    }
}

/// FFXI is Y-down; rotate 180° about X so Y is up (no mirroring, so winding survives).
fn convert_point(p: [f32; 3]) -> [f32; 3] {
    collision::y_up(p)
}

fn instance_trs(i: &zone::Instance) -> Trs {
    // D3DX order: scale, rotate X, Y, Z, translate. Conjugated by the Y/Z flip the Y and Z
    // angles change sign.
    let q = glam::Quat::from_rotation_z(-i.rotation[2]) * glam::Quat::from_rotation_y(-i.rotation[1]) * glam::Quat::from_rotation_x(i.rotation[0]);
    Trs {
        translation: convert_point(i.translation),
        rotation: q.to_array(),
        scale: i.scale,
    }
}

fn model_key(zone_id: u16, name: &[u8; 16]) -> String {
    format!("ff11:z{zone_id}:{}", zone::name_str(name))
}

/// World-space (Y-up) bounds of the placed render meshes.
pub fn render_bounds(z: &zone::ZoneDat) -> Option<([f32; 3], [f32; 3])> {
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    let mut any = false;
    for inst in &z.instances {
        let Some(meshes) = z.models.get(&inst.name) else {
            continue;
        };
        let trs = instance_trs(inst);
        let affine = glam::Affine3A::from_scale_rotation_translation(trs.scale.into(), glam::Quat::from_array(trs.rotation), trs.translation.into());
        for m in meshes {
            for p in &m.positions {
                let w = affine.transform_point3(convert_point(*p).into());
                for k in 0..3 {
                    min[k] = min[k].min(w[k]);
                    max[k] = max[k].max(w[k]);
                }
                any = true;
            }
        }
    }
    any.then_some((min, max))
}

/// Ground surface of an instance for footsteps: the surface named by the texture that
/// covers most of its render mesh (the collision blocks carry no material).
fn instance_surface(z: &zone::ZoneDat, inst: &zone::Instance) -> SurfaceKind {
    let mut counts: Vec<(SurfaceKind, usize)> = Vec::new();
    for m in z.models.get(&inst.name).map(|v| v.as_slice()).unwrap_or(&[]) {
        let Some(s) = SurfaceKind::from_name(&zone::name_str(&m.texture)) else {
            continue;
        };
        match counts.iter_mut().find(|(k, _)| *k == s) {
            Some(e) => e.1 += m.indices.len() / 3,
            None => counts.push((s, m.indices.len() / 3)),
        }
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(s, _)| s).unwrap_or_default()
}

/// Size of the cells the collision meshes are grouped by (world units).
const COLLISION_CELL: f32 = 64.0;

/// The zone's MZB collision as static world-space meshes: the placed instances' pieces,
/// grouped by ground surface and 64 m cell so the runtime gets a few hundred colliders
/// rather than one per instance or one giant mesh.
pub fn scene_collision(z: &zone::ZoneDat) -> Vec<ffl_core::CollisionMesh> {
    let Some(c) = z.collision.as_ref() else {
        return Vec::new();
    };
    let mut groups: Vec<((SurfaceKind, i32, i32), ffl_core::CollisionMesh)> = Vec::new();
    for pl in &c.placements {
        let mesh = c.placement_mesh(pl);
        if mesh.indices.len() < 3 {
            continue;
        }
        let Some(inst) = z.instances.get(pl.instance) else {
            continue;
        };
        let surface = instance_surface(z, inst);
        let t = convert_point(inst.translation);
        let key = (surface, (t[0] / COLLISION_CELL).floor() as i32, (t[2] / COLLISION_CELL).floor() as i32);
        let group = match groups.iter_mut().position(|(k, _)| *k == key) {
            Some(i) => &mut groups[i].1,
            None => {
                groups.push((key, ffl_core::CollisionMesh { surface, ..Default::default() }));
                &mut groups.last_mut().unwrap().1
            }
        };
        let base = group.positions.len() as u32;
        group.positions.extend(mesh.positions);
        group.indices.extend(mesh.indices.iter().map(|i| i + base));
    }
    groups.into_iter().map(|(_, m)| m).collect()
}

impl Engine for Ff11Engine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            plugins: String::new(),
            id: ENGINE_ID.into(),
            name: "Final Fantasy XI".into(),
            version: String::new(),
            available: self.install.is_some(),
            detail: match &self.install {
                Some(i) => i.root.display().to_string(),
                None => self.error.clone().unwrap_or_default(),
            },
            path: self.install.as_ref().map(|i| i.root.display().to_string()),
        }
    }

    fn content_policy(&self) -> ContentPolicy {
        ContentPolicy::LocalOnly
    }

    fn shaders(&self) -> Vec<ShaderDef> {
        Self::shader_defs()
    }

    fn list_maps(&self) -> Result<Vec<MapInfo>> {
        let install = self.install()?;
        Ok(zones::ZONES
            .iter()
            .filter(|(id, _)| install.exists(100 + *id as u32))
            .map(|(id, name)| MapInfo {
                engine: ENGINE_ID.into(),
                id: id.to_string(),
                name: (*name).to_string(),
                category: "Zone".into(),
                detail: format!("zone {id}, DAT {}", 100 + id),
            })
            .collect())
    }

    fn load_scene(&self, map_id: &str) -> Result<Scene> {
        let zone_id: u16 = map_id.parse().with_context(|| format!("map id {map_id} is not a zone id"))?;
        let z = self.zone(zone_id)?;
        let name = zones::zone_name(zone_id).unwrap_or("FFXI zone").to_string();
        let mut nodes = Vec::with_capacity(z.instances.len() + 1);
        let mut missing = 0;
        // With MZB collision the render meshes carry no colliders (foliage, roofs and
        // decorations are not solid in the game); without it every placed model is solid.
        let collision = z.collision.as_ref().filter(|c| !c.placements.is_empty());
        let render_collision = if collision.is_some() { CollisionHint::None } else { CollisionHint::Mesh };
        for inst in &z.instances {
            if !z.models.contains_key(&inst.name) {
                missing += 1;
                continue;
            }
            nodes.push(Node {
                name: zone::name_str(&inst.name),
                parent: None,
                transform: instance_trs(inst),
                kind: NodeKind::Model {
                    model: model_key(zone_id, &inst.name),
                    collision: render_collision,
                },
            });
        }
        // Spawn hints: median of instance positions, then the instances nearest to it.
        let mut xs: Vec<f32> = nodes.iter().map(|n| n.transform.translation[0]).collect();
        let mut ys: Vec<f32> = nodes.iter().map(|n| n.transform.translation[1]).collect();
        let mut zs: Vec<f32> = nodes.iter().map(|n| n.transform.translation[2]).collect();
        let median = |v: &mut Vec<f32>| {
            if v.is_empty() {
                return 0.0;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v[v.len() / 2]
        };
        let center = [median(&mut xs), median(&mut ys), median(&mut zs)];
        let mut candidates: Vec<[f32; 3]> = nodes.iter().map(|n| n.transform.translation).collect();
        candidates.sort_by(|a, b| {
            let da = (a[0] - center[0]).powi(2) + (a[2] - center[2]).powi(2);
            let db = (b[0] - center[0]).powi(2) + (b[2] - center[2]).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(40);
        let mut notes = vec![format!(
            "{} instances ({missing} without a mesh), {} meshes, {} textures",
            z.instances.len(),
            z.models.len(),
            z.textures.len()
        )];
        let scene_collision = scene_collision(&z);
        match collision {
            Some(c) => {
                let s = c.stats();
                notes.push(format!(
                    "collision: {} placements ({} instances), {} pieces, {} triangles in {} meshes",
                    c.placements.len(),
                    c.transform_count,
                    c.pieces.len(),
                    s.triangles,
                    scene_collision.len()
                ));
            }
            None => notes.push("no MZB collision: render meshes are solid".into()),
        }
        notes.extend(z.notes.iter().cloned());
        // The whole zone comes from one game DAT file; music and ambience add their files.
        let mut content = ffl_core::ContentReport::default();
        content.push(&format!("ff11:dat:{}", 100 + zone_id as u32), Provenance::Game);
        let music = match self.music_for_map(map_id) {
            Ok(m) => m,
            Err(e) => {
                notes.push(format!("music: {e:#}"));
                None
            }
        };
        if let Some(m) = &music {
            for key in m.day.iter().chain(&m.night).chain(&m.battle).chain(&m.daybreak).chain(m.extra.iter().map(|(_, k)| k)) {
                content.push(key, Provenance::Game);
            }
            notes.extend(m.notes.iter().cloned());
        }
        // SeSep groups 1 (stereo loops) and 2 (mono loops) are the zone's ambience beds; their
        // placement is not decoded, so they play zone-wide from the centre. Group 2 also lists
        // one-shot transients (bird calls, ~1 s, no loop flag): those need the byte-code's
        // placement and timing, so only looping files become beds.
        let mut emitters = Vec::new();
        let mut one_shots = 0;
        for id in &z.sounds {
            let gain = match id / 1000 {
                1 => 1.0,
                2 => 0.5,
                _ => continue,
            };
            let looped = self
                .install()
                .ok()
                .and_then(|i| i.sound_header(SoundKind::Spw, *id).ok())
                .and_then(|h| sound::header(&h).ok())
                .is_some_and(|h| h.loop_start > 0);
            if !looped {
                one_shots += 1;
                continue;
            }
            let key = sound::sound_key(SoundKind::Spw, *id);
            content.push(&key, Provenance::Game);
            emitters.push(AmbientEmitter {
                name: format!("se{id:06}"),
                key,
                position: center,
                positional: false,
                inner_radius: 0.0,
                max_distance: 0.0,
                height: 0.0,
                gain,
                spots: Vec::new(),
                spot_interval: (0.0, 0.0),
            });
        }
        if !emitters.is_empty() || one_shots > 0 {
            notes.push(format!("{} ambience beds, {one_shots} one-shot ambience sounds skipped (placement undecoded)", emitters.len()));
        }
        // Footsteps for characters that bring none: the Hume ♂ entries of the shared bank.
        let footsteps = sound::footstep_set(pc::Race::HumeMale.footstep_entry(), |id| self.install().ok().and_then(|i| i.sound_path(SoundKind::Spw, id)).is_some());
        if let Some(f) = &footsteps {
            notes.push(format!("footstep bank: {} cues (surface letters matched by ear)", f.cues.len()));
        }
        Ok(Scene {
            engine: ENGINE_ID.into(),
            map: map_id.to_string(),
            name,
            nodes,
            spawn_candidates: candidates,
            center,
            environment: ffl_core::Environment {
                outdoors: true,
                ambient: 1.0,
                ..Default::default()
            },
            music,
            music_regions: Vec::new(),
            emitters,
            footsteps,
            collision: scene_collision,
            notes,
            content,
        })
    }

    fn music_for_map(&self, map_id: &str) -> Result<Option<MusicSet>> {
        let zone_id: u16 = map_id.parse().with_context(|| format!("map id {map_id} is not a zone id"))?;
        let Some((day, night, solo, party)) = zone_music::zone_music(zone_id) else {
            return Ok(None);
        };
        let mut notes = Vec::new();
        let mut set = MusicSet {
            day: self.music_key(day, &mut notes),
            night: self.music_key(night, &mut notes),
            battle: self.music_key(solo, &mut notes),
            daybreak: None,
            extra: Vec::new(),
            fade_in: 1.5,
            fade_out: 2.0,
            restart_on_return: true,
            notes: Vec::new(),
        };
        if let Some(key) = self.music_key(party, &mut notes) {
            set.extra.push(("battle_party".into(), key));
        }
        if day == 0 && night == 0 {
            notes.push("zone has no field music".into());
        }
        set.notes = notes;
        Ok(Some(set))
    }

    fn load_sound(&self, key: &str) -> Result<SoundData> {
        let (kind, id) = sound::parse_key(key).ok_or_else(|| anyhow!("not an FF11 sound key: {key}"))?;
        let bytes = self.install()?.read_sound(kind, id)?;
        sound::parse(key, &bytes)
    }

    /// Vana'diel time: 25× real time, the epoch (00:00 of day 0) is 2001-12-31 15:00 UTC.
    fn clock(&self) -> Option<ClockSpec> {
        Some(ClockSpec { name: "Vana'diel".into(), epoch_unix: 1_009_810_800, rate: 25.0, day: (6.0, 18.0), daybreak: None })
    }

    fn load_model(&self, key: &str) -> Result<ModelData> {
        let rest = key.strip_prefix("ff11:z").ok_or_else(|| anyhow!("not an FF11 model key: {key}"))?;
        let (zone_str, name) = rest.split_once(':').ok_or_else(|| anyhow!("bad model key {key}"))?;
        let zone_id: u16 = zone_str.parse()?;
        let z = self.zone(zone_id)?;
        let meshes = z
            .models
            .iter()
            .find(|(n, _)| zone::name_str(n) == name)
            .map(|(_, m)| m)
            .ok_or_else(|| anyhow!("zone {zone_id} has no mesh {name}"))?;
        let mut model = ModelData {
            key: key.to_string(),
            bounds_min: [f32::MAX; 3],
            bounds_max: [f32::MIN; 3],
            collision: Vec::new(),
            ..Default::default()
        };
        let mut material_index: HashMap<([u8; 16], u8), usize> = HashMap::new();
        // The client's rules (xi-model-viewer `zoneModel.js`, from the xim shader): models
        // whose NAME starts with `_` (foliage, grates, overlay structures) are alpha tested at
        // 0.375 of texture × vertex alpha, whatever their blend flags; every other model keeps
        // its texture alpha for blending only (ground atlases carry the overlay mask there,
        // cutting with it punched holes in the terrain). 0x8000 = translucent, 0x2000 = draw
        // both faces. Meshes without a texture name (hit walls, effect anchors) are invisible.
        let cutout_model = name.starts_with('_');
        for mm in meshes {
            let blended = mm.translucent();
            let cutout = cutout_model;
            let tex_name = zone::name_str(&mm.texture);
            let mode = (if tex_name.is_empty() { 8 } else { 0 }) | (blended as u8) | ((cutout as u8) << 1) | ((mm.two_sided() as u8) << 2);
            let mi = *material_index.entry((mm.texture, mode)).or_insert_with(|| {
                if tex_name.is_empty() {
                    model.materials.push(MaterialDesc::new(&format!("{key}/#hidden"), ffl_core::builtin::SKIP));
                    return model.materials.len() - 1;
                }
                let mut d = MaterialDesc::new(&format!("{key}/{tex_name}#{mode}"), SHADER_ZONE);
                match z.textures.get(&mm.texture) {
                    Some(t) => d.diffuse = Some(t.clone()),
                    None => d.base_color = [0.6, 0.6, 0.6, 1.0],
                }
                d.alpha_blend = blended;
                // Translucent cutouts keep blending; the shader discards below the threshold
                // (params[1].x) for them.
                d.alpha_mask = cutout && !blended;
                d.alpha_cutoff = FF11_ALPHA_TEST;
                d.params = vec![[0.0; 4], [if cutout && blended { FF11_ALPHA_TEST } else { 0.0 }, 0.0, 0.0, 0.0]];
                // Back-face culling would need the strip winding to be right for every mesh;
                // until that is verified per zone, everything draws both faces like before.
                d.double_sided = true;
                d.roughness = 0.9;
                // Interim: MZB collision materials are not read, so the texture name decides.
                d.surface = SurfaceKind::from_name(&tex_name).unwrap_or_default();
                model.materials.push(d);
                model.materials.len() - 1
            });
            let positions: Vec<[f32; 3]> = mm.positions.iter().map(|p| convert_point(*p)).collect();
            for p in &positions {
                for k in 0..3 {
                    model.bounds_min[k] = model.bounds_min[k].min(p[k]);
                    model.bounds_max[k] = model.bounds_max[k].max(p[k]);
                }
            }
            model.meshes.push(MeshData {
                positions,
                normals: mm.normals.iter().map(|n| convert_point(*n)).collect(),
                uv0: mm.uvs.clone(),
                uv1: Vec::new(),
                colors: mm.colors.clone(),
                indices: mm.indices.clone(),
                material_index: mi,
                joints: None,
                bone_table: Vec::new(),
                water: false,
                lod: 0,
            });
        }
        Ok(model)
    }

    fn preset_fields(&self, preset: Option<&CharacterPreset>) -> Result<Vec<PresetField>> {
        let look = preset.map(Look::from_preset).unwrap_or_default();
        let install = self.install().ok();
        let mut f = Vec::new();
        f.push(PresetField::new(
            pc::RACE_KEY,
            "Race",
            "Race",
            FieldKind::Choice(Race::ALL.iter().map(|r| ((*r as u8).to_string(), r.name().to_string())).collect()),
        ));
        // Faces: the face DATs that exist for the race (each is one face with one hairstyle).
        let faces: Vec<(String, String)> = pc::slot_models(look.race, Slot::Face)
            .into_iter()
            .filter(|(_, id)| install.is_some_and(|i| i.exists(*id)))
            .map(|(m, _)| (m.to_string(), gear_names::gear_name("face", look.race as u8, m).map(str::to_string).unwrap_or_else(|| format!("Face {m}"))))
            .collect();
        f.push(PresetField::new(pc::FACE_KEY, "Face and hair", "Race", FieldKind::Choice(faces)));
        f.push(PresetField::new(
            pc::SIZE_KEY,
            "Size",
            "Race",
            FieldKind::Choice(vec![("0".into(), "Small".into()), ("1".into(), "Medium".into()), ("2".into(), "Large".into())]),
        )
        .with_hint("not rendered yet"));
        for slot in Slot::GEAR {
            let none = if slot.is_weapon() { "none" } else { "none (bare)" };
            f.push(PresetField::new(&pc::gear_key(slot), slot.label(), "Gear", FieldKind::Lookup {
                catalog: format!("gear:{}", slot.key()),
                none_label: none.into(),
            }));
        }
        Ok(f)
    }

    fn new_preset(&self, name: &str) -> Result<CharacterPreset> {
        let mut settings = std::collections::BTreeMap::new();
        settings.insert(pc::RACE_KEY.into(), (Race::HumeMale as u8).to_string());
        settings.insert(pc::FACE_KEY.into(), "0".into());
        settings.insert(pc::SIZE_KEY.into(), "1".into());
        for slot in Slot::GEAR {
            settings.insert(pc::gear_key(slot), String::new());
        }
        Ok(CharacterPreset {
            id: ffl_core::Library::new_id(),
            name: name.to_string(),
            engine: ENGINE_ID.into(),
            settings,
        })
    }

    fn default_presets(&self) -> Result<Vec<CharacterPreset>> {
        if self.install.is_none() {
            return Ok(Vec::new());
        }
        let mut p = self.new_preset("Adventurer")?;
        p.id = "ff11-adventurer".into();
        // The starter outfit (model 8 in every armour slot is the race's initial gear).
        for slot in [Slot::Body, Slot::Hands, Slot::Legs, Slot::Feet] {
            p.settings.insert(pc::gear_key(slot), "8".into());
        }
        Ok(vec![p])
    }

    /// `gear:<slot>` catalogs list the model ids whose DAT the install has, named from the
    /// community tables (the game's item files carry no model ids).
    fn catalog(&self, catalog: &str, query: &str, limit: usize) -> Result<Vec<CatalogEntry>> {
        let Some(slot) = catalog.strip_prefix("gear:").and_then(Slot::from_key) else {
            return Ok(Vec::new());
        };
        let install = self.install()?;
        let q = query.trim().to_lowercase();
        let id_query = q.parse::<u16>().ok();
        let mut out = Vec::new();
        // Model ids are shared across races; names too apart from the starter gear.
        let race = Race::HumeMale;
        for (model, file) in pc::slot_models(race, slot) {
            if !install.exists(file) {
                continue;
            }
            let name = gear_names::gear_name(slot.key(), race as u8, model).map(str::to_string).unwrap_or_else(|| format!("model {model}"));
            if !q.is_empty() && !name.to_lowercase().contains(&q) && id_query != Some(model) {
                continue;
            }
            out.push(CatalogEntry {
                id: model.to_string(),
                label: name,
                detail: format!("model {model}"),
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn catalog_label(&self, catalog: &str, id: &str) -> Option<String> {
        let slot = catalog.strip_prefix("gear:").and_then(Slot::from_key)?;
        let model: u16 = id.trim().parse().ok()?;
        Some(gear_names::gear_name(slot.key(), Race::HumeMale as u8, model).map(str::to_string).unwrap_or_else(|| format!("model {model}")))
    }

    fn load_character(&self, preset: &CharacterPreset) -> Result<CharacterModel> {
        if preset.engine != ENGINE_ID {
            return Err(anyhow!("preset {} belongs to engine {}", preset.name, preset.engine));
        }
        let install = self.install()?;
        let look = Look::from_preset(preset);
        character::load_character(install, &look, &preset.name)
    }

    fn load_action(&self, preset: &CharacterPreset, action_id: &str) -> Result<Clip> {
        let install = self.install()?;
        character::load_emote(install, &Look::from_preset(preset), action_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs FFLOCAL_FF11_PATH"]
    fn west_ronfaure_scene_sound() {
        let engine = Ff11Engine::open(None);
        if engine.install.is_none() {
            return;
        }
        let scene = engine.load_scene("100").unwrap();
        let music = scene.music.as_ref().expect("music set");
        assert_eq!(music.day.as_deref(), Some("ff11:bgw:109"));
        assert_eq!(music.battle.as_deref(), Some("ff11:bgw:101"));
        assert_eq!(music.extra, vec![("battle_party".to_string(), "ff11:bgw:103".to_string())]);
        assert!(scene.emitters.iter().all(|e| !e.positional && e.key.starts_with("ff11:spw:")));
        assert!(scene.emitters.iter().any(|e| e.key == "ff11:spw:1005"));
        let steps = scene.footsteps.as_ref().expect("footsteps");
        assert!(steps.cue(SurfaceKind::Grass, ffl_core::Gait::Run, ffl_core::GroundCondition::Dry).unwrap().variations.contains(&"ff11:spw:101001".to_string()));
        assert!(scene.content.summary().contains("protected"));
        let snd = engine.load_sound("ff11:spw:100001").unwrap();
        assert_eq!((snd.sample_rate, snd.channels, snd.frames), (48_000, 1, 48_128));
        assert!(engine.load_sound("ff11:bgw:40").is_err());
        assert_eq!(engine.clock().unwrap().name, "Vana'diel");
        for n in &scene.notes {
            eprintln!("note: {n}");
        }
    }
}
