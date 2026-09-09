//! The FFXIV engine: implements `ffl_core::Engine` on top of the installed game.
//!
//! Modding: a Penumbra-style overlay belongs here (an `AssetSource` that resolves paths through
//! mod collections before the sqpack). Shared worlds would then be restricted to
//! `ContentPolicy::Shareable` sources (mod files), never the game archives.

pub mod character;
pub mod collision;
pub mod convert;
pub mod scene;
pub mod shaders;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow};
use ffl_core::{ArchiveReport, CatalogEntry, CharacterModel, CharacterPreset, Clip, ClockSpec, ContentPolicy, Engine, EngineInfo, FieldKind, ImportSource, MapInfo, ModInfo, ModProfile, ModelData, MusicSet, PresetField, Scene, ShaderDef, SoundData};
use ffl_ff14_assets::dalamud::{self, Dalamud};
use ffl_ff14_assets::mods::{LayeredSource, ModLibrary, ModPack};
use ffl_ff14_assets::{AssetSource, GameData};
use ffl_ff14_assets::loaders::{load_mdl, load_mtrl, load_pcb, load_scd};
use ffl_ff14_assets::zone::{build_zone, territory_bg};
use ffl_ff14_chara::appearance::{default_chara_dat_paths, default_gearset_paths};
use ffl_ff14_chara::meta::CmpFile;
use ffl_ff14_chara::preset::{ALL_RACES, ITEM_KEYS, customize_from_settings, customize_to_settings, items_to_settings, race_name, tribe_name, tribes_of};
use ffl_ff14_chara::race::{BodyPart, paths};
use ffl_ff14_chara::resolver::slot_category;
use ffl_ff14_chara::{CharaDatSource, GearsetSource, RaceCode};
use physis::race::{Gender, Race};
use physis::savedata::chardat::CustomizeData;

use crate::character::{CharacterRequest, JOB_KEY, MODS_KEY, load_character};
use crate::convert::{TextureCache, background_material, keyed, prepare_model, prepare_model_all_lods};
use crate::scene::zone_to_scene;

pub const ENGINE_ID: &str = "ff14";

/// Catalogs of [`FieldKind::Lookup`] gear fields, in bit order of [`ItemEntry::slots`].
const ITEM_CATALOGS: [&str; 11] = [
    "item:mainhand",
    "item:offhand",
    "item:head",
    "item:body",
    "item:hands",
    "item:legs",
    "item:feet",
    "item:ears",
    "item:neck",
    "item:wrists",
    "item:ring",
];

struct ItemEntry {
    id: u32,
    name: String,
    /// Bit per catalog in [`ITEM_CATALOGS`].
    slots: u16,
}

pub struct Ff14Engine {
    pub game: Arc<GameData>,
    textures: TextureCache,
    lod: usize,
    pub mods: ModLibrary,
    /// Dalamud plugin data (Penumbra collections, Glamourer designs), when found.
    pub dalamud: Option<Arc<Dalamud>>,
    items: OnceLock<Vec<ItemEntry>>,
    cmp: OnceLock<Option<CmpFile>>,
}

/// Preset key: "1" keeps a modded copy of the character (see `archive_character`).
pub const ARCHIVE_KEY: &str = "archive";
/// Pack id prefix of a character's own archive.
pub const ARCHIVE_PACK: &str = "archive:";

/// Where a preset's modded copy lives: `<data dir>/characters/ff14/<preset id>/`.
pub fn archive_dir(preset_id: &str) -> PathBuf {
    ffl_core::Library::path().with_file_name("characters").join(ENGINE_ID).join(preset_id)
}

/// Remove archived files under `dir` that are not in `keep` (lower-case game paths relative to
/// `root`), then empty directories.
fn prune_archive(root: &Path, dir: &Path, keep: &std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            prune_archive(root, &p, keep);
            let _ = std::fs::remove_dir(&p);
        } else if let Ok(rel) = p.strip_prefix(root) {
            let rel = rel.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
            if rel != "manifest.toml" && !keep.contains(&rel) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Where mod packs live: `config.toml` `ff14_mods`, else `<data dir>/mods/ff14`.
pub fn mods_dir() -> PathBuf {
    let (config, _) = ffl_ff14_assets::locate::load_config();
    match config.ff14_mods {
        Some(p) => PathBuf::from(p),
        None => ffl_core::Library::path().with_file_name("mods").join("ff14"),
    }
}

impl Ff14Engine {
    /// Open the game at `explicit_path` (or search), reading Dalamud plugin data from
    /// `plugins` (or the usual places).
    pub fn open(explicit_path: Option<&Path>, plugins: Option<&Path>) -> Result<Self> {
        let game = GameData::open(explicit_path)?;
        Ok(Self::from_game(Arc::new(game), plugins))
    }

    pub fn from_game(game: Arc<GameData>, plugins: Option<&Path>) -> Self {
        let dalamud = Dalamud::locate(plugins).map(Arc::new);
        match &dalamud {
            Some(d) => tracing::info!("Dalamud plugin data: {}", d.summary()),
            None => tracing::info!("no Dalamud plugin data found"),
        }
        Self {
            game,
            textures: TextureCache::default(),
            lod: 0,
            mods: ModLibrary::open(&mods_dir(), dalamud.clone()),
            dalamud,
            items: OnceLock::new(),
            cmp: OnceLock::new(),
        }
    }

    /// The game plus the globally enabled mod packs, for tools that inspect what the engine
    /// would read (`ffl-cli --collection`).
    pub fn mod_source(&self) -> LayeredSource {
        self.world_source()
    }

    /// The game plus the globally enabled mod packs (worlds, scene models).
    fn world_source(&self) -> LayeredSource {
        LayeredSource {
            base: self.game.source.clone(),
            packs: self.mods.select(&self.mods.enabled()),
        }
    }

    /// The game plus the global packs plus a preset's own packs (preset packs win). A preset
    /// that keeps a modded copy reads that copy first.
    fn character_source(&self, preset: &CharacterPreset, preset_mods: &[String], use_archive: bool) -> LayeredSource {
        let mut ids: Vec<String> = preset_mods.to_vec();
        for id in self.mods.enabled() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let mut packs = Vec::new();
        if use_archive && preset.settings.get(ARCHIVE_KEY).map(|v| v.trim()) == Some("1") {
            let dir = archive_dir(&preset.id);
            if dir.join("manifest.toml").is_file() {
                match ModPack::load_mirror_dir(&dir, &format!("{ARCHIVE_PACK}{}", preset.id)) {
                    Ok(p) => packs.push(Arc::new(p)),
                    Err(err) => tracing::warn!("archive of {}: {err:#}", preset.name),
                }
            }
        }
        packs.extend(self.mods.select(&ids));
        LayeredSource {
            base: self.game.source.clone(),
            packs,
        }
    }

    fn cmp(&self) -> Option<&CmpFile> {
        self.cmp
            .get_or_init(|| self.game.source().read(paths::CMP).and_then(|b| CmpFile::parse(&b).ok()))
            .as_ref()
    }

    fn items(&self) -> &Vec<ItemEntry> {
        self.items.get_or_init(|| {
            let mut out = Vec::new();
            let Ok(items) = self.game.excel.sheet("Item") else {
                return out;
            };
            let mut categories: HashMap<u32, u16> = HashMap::new();
            for page in &items.sheet.pages {
                for entry in &page.entries {
                    let row = entry.id;
                    let Ok(name) = items.string(row, "Name") else {
                        continue;
                    };
                    if name.is_empty() {
                        continue;
                    }
                    let category = items.integer(row, "EquipSlotCategory").unwrap_or(0) as u32;
                    if category == 0 || items.integer(row, "ModelMain").unwrap_or(0) == 0 {
                        continue;
                    }
                    let slots = *categories.entry(category).or_insert_with(|| {
                        let Ok(c) = slot_category(&self.game.excel, category) else {
                            return 0;
                        };
                        use ffl_ff14_chara::GearSlot as G;
                        let mut mask = 0u16;
                        if c.main_hand {
                            mask |= 1 << 0;
                        }
                        if c.off_hand {
                            mask |= 1 << 1;
                        }
                        for s in &c.occupies {
                            mask |= match s {
                                G::Head => 1 << 2,
                                G::Body => 1 << 3,
                                G::Hands => 1 << 4,
                                G::Legs => 1 << 5,
                                G::Feet => 1 << 6,
                                G::Ears => 1 << 7,
                                G::Neck => 1 << 8,
                                G::Wrists => 1 << 9,
                                G::RingLeft | G::RingRight => 1 << 10,
                            };
                        }
                        mask
                    });
                    if slots != 0 {
                        out.push(ItemEntry { id: row, name, slots });
                    }
                }
            }
            tracing::info!("item catalog: {} equippable items", out.len());
            out
        })
    }

    fn territory_name(&self, row: u32) -> String {
        let Ok(tt) = self.game.excel.sheet("TerritoryType") else {
            return String::new();
        };
        let place = tt.integer(row, "PlaceName").unwrap_or(0) as u32;
        self.game
            .excel
            .string("PlaceName", place, "Name")
            .unwrap_or_default()
    }

    fn intended_use_label(v: u64) -> &'static str {
        match v {
            0 => "Town",
            1 => "Open World",
            2 => "Inn",
            3 => "Dungeon",
            4 => "Variant Dungeon",
            6 => "Gaol",
            7 => "Opening",
            8 => "Alliance Raid",
            10 => "Trial",
            13 => "Housing District",
            14 => "Housing Interior",
            15 => "Solo Instance",
            16 => "Raid",
            17 => "Raid",
            19 => "Chocobo Square",
            20 => "Chocobo Race",
            21 => "Firmament",
            22 => "Wondrous Tails",
            23 => "Gold Saucer",
            26 => "Diadem",
            29 => "Barracks",
            31 => "Deep Dungeon",
            41 => "Eureka",
            45 => "Masked Carnivale",
            47 => "Ocean Fishing",
            48 => "Bozja",
            49 => "Island Sanctuary",
            _ => "Other",
        }
    }

    /// Ids of body-part models that exist on disk for a race (`f0001`.. faces, `h0001`.. hair).
    fn existing_parts(&self, race: RaceCode, part: BodyPart, max: u16) -> Vec<u16> {
        let source = self.game.source();
        (1..=max).filter(|id| source.exists(&paths::body_part_mdl(race, part, *id))).collect()
    }

    fn palette(&self, n: usize, f: impl Fn(&CmpFile, u8) -> [u8; 4]) -> Vec<[u8; 3]> {
        let Some(cmp) = self.cmp() else {
            return Vec::new();
        };
        let mut out: Vec<[u8; 3]> = (0..n.min(256)).map(|i| {
            let c = f(cmp, i as u8);
            [c[0], c[1], c[2]]
        }).collect();
        while out.len() > 1 && out.last() == Some(&[0, 0, 0]) {
            out.pop();
        }
        out
    }

    /// The customize data a preset currently describes (defaults for a new preset).
    fn customize_of(preset: Option<&CharacterPreset>) -> CustomizeData {
        preset
            .and_then(|p| customize_from_settings(&p.settings).ok().flatten())
            .unwrap_or_default()
    }
}

fn choice_u8<I: IntoIterator<Item = (u8, String)>>(items: I) -> FieldKind {
    FieldKind::Choice(items.into_iter().map(|(v, l)| (v.to_string(), l)).collect())
}

impl Engine for Ff14Engine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: ENGINE_ID.into(),
            name: "Final Fantasy XIV".into(),
            version: self.game.install.version.clone(),
            available: true,
            detail: self.game.install.game_dir.display().to_string(),
            path: Some(self.game.install.game_dir.display().to_string()),
            plugins: self.dalamud.as_ref().map(|d| d.summary()).unwrap_or_else(|| "Dalamud plugin data: not found (XIVLauncher's pluginConfigs folder)".into()),
        }
    }

    fn content_policy(&self) -> ContentPolicy {
        ContentPolicy::LocalOnly
    }

    fn shaders(&self) -> Vec<ShaderDef> {
        crate::shaders::shader_defs()
    }

    fn list_maps(&self) -> Result<Vec<MapInfo>> {
        let tt = self.game.excel.sheet("TerritoryType")?;
        let mut out = Vec::new();
        for page in &tt.sheet.pages {
            for entry in &page.entries {
                let row = entry.id;
                let Ok(bg) = tt.string(row, "Bg") else {
                    continue;
                };
                if bg.is_empty() {
                    continue;
                }
                let name = self.territory_name(row);
                let use_id = tt.integer(row, "TerritoryIntendedUse").unwrap_or(999);
                let category = Self::intended_use_label(use_id).to_string();
                let code = tt.string(row, "Name").unwrap_or_default();
                out.push(MapInfo {
                    engine: ENGINE_ID.into(),
                    id: row.to_string(),
                    name: if name.is_empty() { code.clone() } else { name },
                    category,
                    detail: format!("{code}  {bg}"),
                });
            }
        }
        Ok(out)
    }

    fn load_scene(&self, map_id: &str) -> Result<Scene> {
        let territory: u32 = map_id.parse().with_context(|| format!("map id {map_id} is not a TerritoryType row"))?;
        let _ = territory_bg(&self.game.excel, territory)?;
        let source = self.world_source();
        let graph = build_zone(&source, &self.game.excel, territory)?;
        let name = self.territory_name(territory);
        let spawns = crate::scene::aetheryte_positions(&self.game.excel, territory);
        let mut scene = zone_to_scene(&graph, map_id, &name, &spawns);
        for path in graph.unique_model_paths() {
            scene.content.push(path, source.origin(path));
        }
        match ffl_ff14_assets::music::music_set(&self.game.excel, &source, territory) {
            Ok(music) => {
                if let Some(m) = &music {
                    scene.notes.extend(m.notes.iter().cloned());
                }
                scene.music = music;
            }
            Err(err) => scene.notes.push(format!("music: {err:#}")),
        }
        // Terrain collision pieces (world space) replace the terrain plates' render-mesh colliders.
        let mut terrain_polys = 0usize;
        let mut terrain_files = 0usize;
        let pieces: Vec<u32> = (0..100u32)
            .chain(load_pcb_list(&source, &format!("{}/collision/list.pcb", graph.base_dir)).unwrap_or_default())
            .collect();
        for id in pieces {
            let path = format!("{}/collision/tr{id:04}.pcb", graph.base_dir);
            if !source.exists(&path) {
                continue;
            }
            match load_pcb(&source, &path) {
                Ok(pcb) => {
                    let meshes = crate::collision::collision_meshes(&pcb);
                    terrain_polys += meshes.iter().map(|m| m.indices.len() / 3).sum::<usize>();
                    terrain_files += 1;
                    scene.content.push(&path, source.origin(&path));
                    scene.collision.extend(meshes);
                }
                Err(err) => scene.notes.push(format!("{path}: {err:#}")),
            }
        }
        if terrain_files > 0 {
            for node in &mut scene.nodes {
                if let ffl_core::NodeKind::Model { model, collision } = &mut node.kind
                    && (model.contains("/bgplate/") || model.contains("/terrain/"))
                {
                    *collision = ffl_core::CollisionHint::None;
                }
            }
        }
        scene.notes.push(format!("terrain collision: {terrain_files} pcb pieces, {terrain_polys} polygons"));
        scene.music_regions = crate::scene::music_regions(&graph, &self.game.excel, &source, territory, &mut scene.notes);
        scene.emitters = crate::scene::sound_emitters(&graph, &source, &mut scene.notes);
        scene.notes.push(format!(
            "{} ambient emitters ({} placed sounds, {} env sets), {} music regions of {} map ranges",
            scene.emitters.len(),
            graph.sounds.len(),
            graph.env_sets.len(),
            scene.music_regions.len(),
            graph.map_ranges.len()
        ));
        let music_keys = scene
            .music
            .iter()
            .chain(scene.music_regions.iter().map(|r| &r.music))
            .flat_map(|m| m.day.iter().chain(&m.night).chain(&m.battle).chain(&m.daybreak).chain(m.extra.iter().map(|(_, k)| k)))
            .chain(scene.emitters.iter().map(|e| &e.key))
            .cloned()
            .collect::<Vec<_>>();
        for key in music_keys {
            if let Some(path) = scd_key_path(&key) {
                scene.content.push(path, source.origin(path));
            }
        }
        scene.content.notes.push("materials and textures are attributed as models load".into());
        scene.notes.push(scene.content.summary());
        Ok(scene)
    }

    fn load_model(&self, key: &str) -> Result<ModelData> {
        let source = self.world_source();
        let mdl = load_mdl(&source, key)?;
        // Placed models bring every LOD; `lod` forces one level for debugging.
        // `FFL_FORCE_LOD=n` renders every placed model at that level (LOD verification).
        let forced = std::env::var("FFL_FORCE_LOD").ok().and_then(|v| v.parse::<usize>().ok());
        let mut model = match forced.or((self.lod > 0).then_some(self.lod)) {
            Some(l) => prepare_model(&keyed(&source, key), &mdl, l),
            None => prepare_model_all_lods(&keyed(&source, key), &mdl),
        };
        model.materials = mdl
            .material_names
            .iter()
            .map(|name| match load_mtrl(&source, name) {
                Ok(m) => background_material(&source, &self.textures, &keyed(&source, name), &m),
                Err(err) => {
                    tracing::warn!("{err:#}");
                    ffl_core::MaterialDesc::unlit(name, [1.0, 0.0, 1.0, 1.0])
                }
            })
            .collect();
        // The model's own collision file, with the game's ground materials.
        if let Some(pcb_path) = crate::collision::model_collision_path(key)
            && source.exists(&pcb_path)
        {
            match load_pcb(&source, &pcb_path) {
                Ok(pcb) => model.collision = crate::collision::collision_meshes(&pcb),
                Err(err) => tracing::warn!("{pcb_path}: {err:#}"),
            }
        }
        Ok(model)
    }

    fn preset_fields(&self, preset: Option<&CharacterPreset>) -> Result<Vec<PresetField>> {
        let c = Self::customize_of(preset);
        let race_code = RaceCode::from_customize(c.race, c.tribe, &c.gender).unwrap_or(RaceCode::MIDLANDER_MALE);
        let female = c.gender == Gender::Female;
        let mut f = Vec::new();

        // Job: animation set (stances, victory poses) and later job-specific behaviour.
        let mut jobs: Vec<(String, String)> = vec![(String::new(), "by weapon".into())];
        if let Ok(sheet) = self.game.excel.sheet("ClassJob") {
            for row in 1..sheet.row_count() {
                if crate::character::job_animation_set(row).is_none() {
                    continue;
                }
                let name = sheet.string(row, "NameEnglish").unwrap_or_default();
                let abbr = sheet.string(row, "Abbreviation").unwrap_or_default();
                if !name.is_empty() {
                    jobs.push((row.to_string(), format!("{name} ({abbr})")));
                }
            }
        }
        f.push(PresetField::new(JOB_KEY, "Job", "Job", FieldKind::Choice(jobs)).with_hint("picks the stance and victory animations"));

        // Race
        f.push(PresetField::new("race", "Race", "Race", choice_u8(ALL_RACES.iter().map(|r| (*r as u8, race_name(*r).to_string())))));
        f.push(PresetField::new("tribe", "Clan", "Race", choice_u8(tribes_of(c.race).iter().map(|t| (*t as u8, tribe_name(*t).to_string())))));
        f.push(PresetField::new("gender", "Gender", "Race", choice_u8([(0u8, "Male".to_string()), (1, "Female".to_string())])));
        match ffl_ff14_chara::voice::chara_make_voices(&self.game.excel, c.tribe, &c.gender) {
            Some(voices) => f.push(
                PresetField::new("voice", "Voice", "Race", choice_u8(voices.iter().enumerate().map(|(i, v)| (*v, format!("Voice {} (id {v})", i + 1)))))
                    .with_hint("emote voice lines (laugh, cheer, ...)"),
            ),
            None => f.push(PresetField::new("voice", "Voice", "Race", FieldKind::Integer { min: 0, max: 255 }).with_hint("voice id from the game save")),
        }

        // Body
        f.push(PresetField::new("height", "Height", "Body", FieldKind::Integer { min: 0, max: 100 }));
        if female {
            f.push(PresetField::new("bust", "Bust", "Body", FieldKind::Integer { min: 0, max: 100 }).with_hint("not rendered yet"));
        }
        let feature_part = match c.race {
            Race::Miqote | Race::AuRa | Race::Hrothgar => Some((BodyPart::Tail, "Tail")),
            Race::Viera => Some((BodyPart::Ears, "Ears")),
            _ => None,
        };
        if let Some((part, label)) = feature_part {
            let ids = self.existing_parts(race_code, part, 8);
            f.push(PresetField::new("race_feature_type", label, "Body", choice_u8(ids.iter().map(|i| (*i as u8, format!("{label} {i}"))))));
            f.push(PresetField::new("race_feature_size", &format!("{label} size"), "Body", FieldKind::Integer { min: 0, max: 100 }).with_hint("not rendered yet"));
        } else {
            f.push(PresetField::new("race_feature_size", "Muscle / ear size", "Body", FieldKind::Integer { min: 0, max: 100 }).with_hint("not rendered yet"));
        }

        // Face and hair
        let faces = self.existing_parts(race_code, BodyPart::Face, 250);
        f.push(PresetField::new("face", "Face", "Face", choice_u8(faces.iter().map(|i| (*i as u8, format!("Face {i}"))))));
        let hairs = self.existing_parts(race_code, BodyPart::Hair, 250);
        f.push(PresetField::new("hair", "Hairstyle", "Hair", choice_u8(hairs.iter().map(|i| (*i as u8, format!("Hair {i}"))))));
        f.push(PresetField::new("highlights", "Highlights", "Hair", FieldKind::Bool));

        // Colours (palettes from human.cmp)
        let (tribe, gender) = (c.tribe, c.gender.clone());
        f.push(PresetField::new("skin_tone", "Skin", "Colours", FieldKind::Palette(self.palette(192, |m, i| m.skin_color(tribe, &gender, i).0))));
        f.push(PresetField::new("hair_tone", "Hair", "Colours", FieldKind::Palette(self.palette(192, |m, i| m.hair_color(tribe, &gender, i).0))));
        f.push(PresetField::new("highlight_tone", "Highlights", "Colours", FieldKind::Palette(self.palette(192, |m, i| m.hair_highlight_color(i).0))));
        f.push(PresetField::new("left_eye_color", "Left eye", "Colours", FieldKind::Palette(self.palette(192, |m, i| m.eye_color(i).0))));
        f.push(PresetField::new("right_eye_color", "Right eye", "Colours", FieldKind::Palette(self.palette(192, |m, i| m.eye_color(i).0))));
        f.push(PresetField::new("lips_tone", "Lips", "Colours", FieldKind::Palette(self.palette(256, |m, i| m.lip_color(i).0))).with_hint("second half: light"));
        f.push(PresetField::new("facial_feature_color", "Facial features", "Colours", FieldKind::Palette(self.palette(192, |m, i| m.feature_color(i).0))).with_hint("tattoos, limbal rings; not rendered yet"));

        // Face details: shape keys (eyebrows, eyes, nose, jaw, mouth), feature marks, the
        // face paint decal and its colour row.
        for (key, label) in [("eyebrows", "Eyebrows"), ("eyes", "Eyes"), ("nose", "Nose"), ("jaw", "Jaw"), ("mouth", "Mouth"), ("facial_features", "Facial features"), ("face_paint", "Face paint"), ("face_paint_color", "Face paint colour")] {
            f.push(PresetField::new(key, label, "Face details", FieldKind::Integer { min: 0, max: 255 }));
        }
        // Advanced colours: Glamourer's parameters, `r g b [a]` in 0..1 (palette domain);
        // empty = the palette colour of the customize value above.
        for (field, key, n) in ffl_ff14_assets::dalamud::PARAMETER_MAP {
            let label = match *field {
                "SkinDiffuse" => "Skin",
                "HairDiffuse" => "Hair",
                "HairHighlight" => "Highlights",
                "LeftEye" => "Left eye",
                "RightEye" => "Right eye",
                "FeatureColor" => "Facial features",
                "LipDiffuse" => "Lips",
                "DecalColor" => "Face paint",
                "FacePaintUvMultiplier" => "Face paint UV scale",
                "FacePaintUvOffset" => "Face paint UV offset",
                other => other,
            };
            let hint = match *n {
                4 => "r g b a in 0..1, empty = palette",
                3 => "r g b in 0..1, empty = palette",
                _ => "one number, empty = the game's default",
            };
            f.push(PresetField::new(key, label, "Advanced colours (Glamourer)", FieldKind::Text).with_hint(hint));
        }

        // Gear
        let labels = [
            ("item.mainhand", "Main hand", 0),
            ("item.offhand", "Off hand", 1),
            ("item.head", "Head", 2),
            ("item.body", "Body", 3),
            ("item.hands", "Hands", 4),
            ("item.legs", "Legs", 5),
            ("item.feet", "Feet", 6),
            ("item.ears", "Earrings", 7),
            ("item.neck", "Necklace", 8),
            ("item.wrists", "Bracelets", 9),
            ("item.ring_left", "Left ring", 10),
            ("item.ring_right", "Right ring", 10),
        ];
        // Dyes: the Stain sheet's rows (column 5 = name), two channels per item.
        let mut stains: Vec<(String, String)> = vec![(String::new(), "undyed".into())];
        if let Ok(sheet) = self.game.excel.sheet("Stain") {
            for row in 1..sheet.row_count() {
                let name = match sheet.row(row).and_then(|r| r.columns.get(5)) {
                    Some(physis::excel::Field::String(n)) => n.clone(),
                    _ => continue,
                };
                if !name.is_empty() {
                    stains.push((row.to_string(), name));
                }
            }
        }
        for (key, label, cat) in labels {
            let none = if matches!(cat, 3 | 4 | 5 | 6) { "none (smallclothes)" } else { "none" };
            f.push(PresetField::new(key, label, "Gear", FieldKind::Lookup {
                catalog: ITEM_CATALOGS[cat].into(),
                none_label: none.into(),
            }));
            if cat <= 6 {
                let (k1, k2) = ffl_ff14_chara::preset::dye_keys(key);
                f.push(PresetField::new(&k1, &format!("{label} dye"), "Gear", FieldKind::Choice(stains.clone())));
                f.push(PresetField::new(&k2, &format!("{label} dye 2"), "Gear", FieldKind::Choice(stains.clone())));
            }
        }

        // Mods: Penumbra collections first, then FFLocal's own packs.
        let mut packs: Vec<(String, String)> = self.mods.profiles().into_iter().map(|p| (p.id, format!("{} (Penumbra collection, {} mods)", p.name, p.mods))).collect();
        packs.extend(self.mods.packs.iter().map(|p| (p.id.clone(), format!("{} ({} files)", p.name, p.files.len()))));
        if !packs.is_empty() {
            f.push(PresetField::new(MODS_KEY, "Mods for this character", "Mods", FieldKind::MultiChoice(packs)).with_hint("applied on top of the globally enabled mods"));
        }
        f.push(PresetField::new(ARCHIVE_KEY, "Keep a modded copy", "Mods", FieldKind::Bool).with_hint("copies the character's modded files into FFLocal's data folder (vanilla files stay in the game); rebuilt on save"));
        Ok(f)
    }

    fn default_presets(&self) -> Result<Vec<CharacterPreset>> {
        let mut out = Vec::new();
        for (i, p) in default_chara_dat_paths().into_iter().enumerate() {
            let name = CharaDatSource::read(&p)
                .map(|d| if d.comment.is_empty() { format!("Slot {}", i + 1) } else { d.comment })
                .unwrap_or_else(|_| format!("Slot {}", i + 1));
            let mut preset = self.new_preset(&name)?;
            preset.id = format!("ff14-slot-{}", i + 1);
            if let Err(err) = self.import_preset(&mut preset, &format!("chara:{}", p.display())) {
                tracing::warn!("{}: {err:#}", p.display());
                continue;
            }
            if let Some(g) = default_gearset_paths().into_iter().next()
                && let Err(err) = self.import_preset(&mut preset, &format!("gear:{}:current", g.display()))
            {
                tracing::warn!("{}: {err:#}", g.display());
            }
            out.push(preset);
        }
        Ok(out)
    }

    /// Presets from before settings-based presets only pointed at `FFXIV_CHARA_*.dat` and
    /// `GEARSET.DAT`; import them so the editor shows the real character.
    fn prepare_for_edit(&self, preset: &mut CharacterPreset) -> Result<Option<String>> {
        if ffl_ff14_chara::preset::has_customize(&preset.settings) {
            return Ok(None);
        }
        let req = CharacterRequest::from_preset(preset)?;
        let crate::character::AppearanceSpec::Files { chara, gearset, set } = &req.appearance else {
            return Ok(None);
        };
        let mut note = self.import_preset(preset, &format!("chara:{}", chara.display()))?;
        if let Some(g) = gearset {
            let idx = set.map(|s| s.to_string()).unwrap_or_else(|| "current".into());
            match self.import_preset(preset, &format!("gear:{}:{idx}", g.display())) {
                Ok(n) => note = format!("{note}; {n}"),
                Err(err) => note = format!("{note}; gear import failed: {err:#}"),
            }
        }
        for key in ["chara", "gear", "gearset", "set"] {
            preset.settings.remove(key);
        }
        Ok(Some(format!("upgraded to a self-contained preset: {note}")))
    }

    fn import_sources(&self) -> Vec<ImportSource> {
        let mut out = Vec::new();
        if let Some(g) = self.dalamud.as_ref().and_then(|d| d.glamourer.as_ref()) {
            // What the game applies by itself: the automation sets, one entry per design.
            for a in g.automation.iter().filter(|a| a.enabled) {
                for id in &a.designs {
                    let Some(d) = g.design(id) else {
                        continue;
                    };
                    out.push(ImportSource {
                        id: format!("glamourer:{}", d.id),
                        label: format!("Glamourer automation: {} → {}", a.player, d.name),
                        detail: format!("set \"{}\": the design Glamourer applies to {} in the game", a.name, a.player),
                    });
                }
            }
            for d in &g.designs {
                let customize = d.customize.iter().filter(|c| c.2).count();
                let gear = d.equipment.iter().filter(|e| e.2).count();
                let colors = d.parameters.iter().filter(|p| p.2).count();
                out.push(ImportSource {
                    id: format!("glamourer:{}", d.id),
                    label: format!("Glamourer design: {}{}", if d.folder.is_empty() { String::new() } else { format!("{}/", d.folder) }, d.name),
                    detail: format!("{customize} appearance fields, {gear} gear slots, {colors} advanced colours"),
                });
            }
        }
        for p in default_chara_dat_paths() {
            let file = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            let (label, detail) = match CharaDatSource::read(&p) {
                Ok(d) => {
                    let c = &d.customize;
                    (
                        format!("Appearance: {}", if d.comment.is_empty() { file.clone() } else { d.comment.clone() }),
                        format!("{file}: {} {} {:?}", race_name(c.race), tribe_name(c.tribe), c.gender),
                    )
                }
                Err(_) => (format!("Appearance: {file}"), "unreadable".into()),
            };
            out.push(ImportSource {
                id: format!("chara:{}", p.display()),
                label,
                detail,
            });
        }
        for (n, p) in default_gearset_paths().into_iter().enumerate() {
            let dir = p.parent().and_then(|d| d.file_name()).map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            let Ok(g) = GearsetSource::read(&p) else {
                continue;
            };
            out.push(ImportSource {
                id: format!("gear:{}:current", p.display()),
                label: format!("Gear: current set of {dir}"),
                detail: p.display().to_string(),
            });
            if n == 0 {
                for set in &g.sets {
                    out.push(ImportSource {
                        id: format!("gear:{}:{}", p.display(), set.index),
                        label: format!("Gear: {} ({})", set.name, set.index),
                        detail: dir.clone(),
                    });
                }
            }
        }
        out
    }

    fn import_preset(&self, preset: &mut CharacterPreset, source_id: &str) -> Result<String> {
        if let Some(id) = source_id.strip_prefix("glamourer:") {
            let design = self
                .dalamud
                .as_ref()
                .and_then(|d| d.glamourer.as_ref())
                .and_then(|g| g.design(id))
                .ok_or_else(|| anyhow!("no Glamourer design {id}"))?;
            let (customize, gear, colors) = dalamud::apply_design(design, &mut preset.settings);
            return Ok(format!("Glamourer design {}: {customize} appearance fields, {gear} gear slots and {colors} advanced colours applied", design.name));
        }
        if let Some(path) = source_id.strip_prefix("chara:") {
            let data = CharaDatSource::read(Path::new(path))?;
            customize_to_settings(&data.customize, &mut preset.settings);
            let c = &data.customize;
            return Ok(format!("appearance imported from {path} ({} {} {:?})", race_name(c.race), tribe_name(c.tribe), c.gender));
        }
        if let Some(rest) = source_id.strip_prefix("gear:") {
            let (path, index) = rest.rsplit_once(':').ok_or_else(|| anyhow!("bad gear source {source_id}"))?;
            let index = if index == "current" { None } else { Some(index.parse::<u8>()?) };
            let source = GearsetSource {
                path: PathBuf::from(path),
                index,
            };
            let (items, note) = source.items()?;
            items_to_settings(&items, &mut preset.settings);
            if let Ok(job) = source.class_job()
                && job != 0
            {
                preset.settings.insert(JOB_KEY.into(), job.to_string());
            }
            return Ok(note);
        }
        Err(anyhow!("unknown import source {source_id}"))
    }

    fn catalog(&self, catalog: &str, query: &str, limit: usize) -> Result<Vec<CatalogEntry>> {
        let Some(bit) = ITEM_CATALOGS.iter().position(|c| *c == catalog) else {
            return Ok(Vec::new());
        };
        let q = query.trim().to_lowercase();
        let id_query = q.parse::<u32>().ok();
        let mut out = Vec::new();
        for it in self.items() {
            if it.slots & (1 << bit) == 0 {
                continue;
            }
            let hit = q.is_empty() || it.name.to_lowercase().contains(&q) || id_query == Some(it.id);
            if hit {
                out.push(CatalogEntry {
                    id: it.id.to_string(),
                    label: it.name.clone(),
                    detail: format!("item {}", it.id),
                });
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    fn catalog_label(&self, catalog: &str, id: &str) -> Option<String> {
        if !catalog.starts_with("item:") {
            return None;
        }
        let id: u32 = id.trim().parse().ok()?;
        self.items().iter().find(|it| it.id == id).map(|it| it.name.clone())
    }

    fn list_mods(&self) -> Vec<ModInfo> {
        self.mods
            .packs
            .iter()
            .map(|p| ModInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                description: p.description.clone(),
                files: p.files.len(),
            })
            .collect()
    }

    fn mod_profiles(&self) -> Vec<ModProfile> {
        self.mods.profiles()
    }

    fn set_enabled_mods(&self, ids: &[String]) {
        self.mods.set_enabled(ids);
    }

    fn archive_character(&self, preset: &CharacterPreset, dir: &Path) -> Result<ArchiveReport> {
        if preset.engine != ENGINE_ID {
            return Err(anyhow!("preset {} belongs to engine {}", preset.name, preset.engine));
        }
        let req = CharacterRequest::from_preset(preset)?;
        // Resolve from the live mods, not from an older copy.
        let source = self.character_source(preset, &req.mods, false);
        let model = load_character(&self.game, &source, &self.textures, &req, &preset.name)?;
        let mut report = ArchiveReport { dir: dir.to_path_buf(), ..Default::default() };
        let mut entries = Vec::new();
        for asset in &model.content.assets {
            let pack = match &asset.provenance {
                ffl_core::Provenance::Mod(p) => p.clone(),
                ffl_core::Provenance::Game => {
                    report.vanilla += 1;
                    continue;
                }
                ffl_core::Provenance::Original => continue,
            };
            let Some(bytes) = source.read(&asset.path) else {
                continue;
            };
            let target = dir.join(&asset.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&target, &bytes).with_context(|| format!("writing {}", target.display()))?;
            report.files += 1;
            report.bytes += bytes.len() as u64;
            if !report.packs.contains(&pack) {
                report.packs.push(pack.clone());
            }
            entries.push(format!("[[file]]\npath = {:?}\npack = {:?}\nbytes = {}\n", asset.path, pack, bytes.len()));
        }
        // Files of an earlier copy that this gear no longer uses are removed: one copy per
        // character, matching what it wears now.
        let keep: std::collections::HashSet<String> = model.content.assets.iter().map(|a| a.path.to_ascii_lowercase()).collect();
        prune_archive(dir, dir, &keep);
        std::fs::create_dir_all(dir)?;
        let manifest = format!(
            "# FFLocal character archive: the modded files of one character, mirrored by game path.\n# Vanilla files are not included; they are read from the game.\nformat = 1\nengine = {:?}\npreset_id = {:?}\nname = {:?}\nfiles = {}\nbytes = {}\nvanilla = {}\npacks = {:?}\n\n{}",
            ENGINE_ID,
            preset.id,
            preset.name,
            report.files,
            report.bytes,
            report.vanilla,
            report.packs,
            entries.join("\n")
        );
        std::fs::write(dir.join("manifest.toml"), manifest)?;
        tracing::info!("archived {}: {} modded files ({:.1} MB) from {:?}, {} vanilla files stay in the game", preset.name, report.files, report.bytes as f64 / 1e6, report.packs, report.vanilla);
        Ok(report)
    }

    fn load_character(&self, preset: &CharacterPreset) -> Result<CharacterModel> {
        if preset.engine != ENGINE_ID {
            return Err(anyhow!("preset {} belongs to engine {}", preset.name, preset.engine));
        }
        let req = CharacterRequest::from_preset(preset)?;
        let source = self.character_source(preset, &req.mods, true);
        load_character(&self.game, &source, &self.textures, &req, &preset.name)
    }

    fn music_for_map(&self, map_id: &str) -> Result<Option<MusicSet>> {
        let territory: u32 = map_id.parse().with_context(|| format!("map id {map_id} is not a TerritoryType row"))?;
        ffl_ff14_assets::music::music_set(&self.game.excel, &self.world_source(), territory)
    }

    /// Keys are `ff14:scd:<path>[@<pack>]#<entry>`; the file is read through the mod layer.
    fn load_sound(&self, key: &str) -> Result<SoundData> {
        let rest = key.strip_prefix("ff14:scd:").ok_or_else(|| anyhow!("not an FF14 sound key: {key}"))?;
        let (path_keyed, entry) = rest.rsplit_once('#').ok_or_else(|| anyhow!("sound key without an entry index: {key}"))?;
        let entry: usize = entry.parse().with_context(|| format!("bad entry index in {key}"))?;
        let path = path_keyed.split('@').next().unwrap_or(path_keyed);
        let source = self.world_source();
        let scd = load_scd(&source, path)?;
        let e = scd
            .entries
            .get(entry)
            .and_then(|e| e.as_ref())
            .ok_or_else(|| anyhow!("{path}: slot {entry} holds no audio ({} slots)", scd.entries.len()))?;
        let mut d = e.decode().with_context(|| format!("decoding {path}#{entry}"))?;
        if d.channels > 2 {
            // Quad ambience (`_4ch`): fold the rear channels into the front pair.
            let ch = d.channels as usize;
            let mut stereo = Vec::with_capacity(d.samples.len() / ch * 2);
            for frame in d.samples.chunks_exact(ch) {
                let l: i32 = frame.iter().step_by(2).map(|s| *s as i32).sum::<i32>() / ((ch as i32 + 1) / 2);
                let r: i32 = frame.iter().skip(1).step_by(2).map(|s| *s as i32).sum::<i32>() / (ch as i32 / 2).max(1);
                stereo.push(l.clamp(-32768, 32767) as i16);
                stereo.push(r.clamp(-32768, 32767) as i16);
            }
            d.samples = stereo;
            d.channels = 2;
        }
        let mut data = SoundData::pcm16(key, d.sample_rate, d.channels, d.samples, d.loop_range, source.origin(path));
        // The volume of the sound program that plays this entry (program 0 when none names it).
        if let Some(p) = scd.sounds.iter().find(|s| s.audio.contains(&entry)).or(scd.sounds.first()) {
            data.gain = p.volume.clamp(0.05, 1.0);
        }
        Ok(data)
    }

    /// Eorzea time: 1 real second = 3600/175 Eorzea seconds, counted from the Unix epoch.
    fn clock(&self) -> Option<ClockSpec> {
        Some(ClockSpec { name: "Eorzea".into(), epoch_unix: 0, rate: 3600.0 / 175.0, day: (6.0, 18.0), daybreak: Some((5.0, 6.0)) })
    }

    fn load_action(&self, preset: &CharacterPreset, action_id: &str) -> Result<Clip> {
        let req = CharacterRequest::from_preset(preset)?;
        let appearance = req.appearance()?;
        let c = &appearance.customize;
        let race = RaceCode::from_customize(c.race, c.tribe, &c.gender).ok_or_else(|| anyhow!("unsupported race"))?;
        let folder = crate::character::preset_animation_set(&self.game, req.job, &appearance).map(|s| s.stance);
        crate::character::load_action(&self.game, race, folder, action_id)
    }
}

/// Mesh ids listed by a zone's `collision/list.pcb`.
fn load_pcb_list(source: &dyn AssetSource, path: &str) -> Option<Vec<u32>> {
    let list = ffl_ff14_assets::loaders::load::<physis::pcblist::PcbList>(source, path).ok()?;
    Some(list.entries.iter().map(|e| e.mesh_id).collect())
}

/// The game path inside an `ff14:scd:` sound key.
pub fn scd_key_path(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("ff14:scd:")?;
    let (path_keyed, _) = rest.rsplit_once('#')?;
    Some(path_keyed.split('@').next().unwrap_or(path_keyed))
}

/// Item keys a preset stores (re-exported for the CLI).
pub fn item_keys() -> &'static [(ffl_ff14_chara::gearset::GearsetSlot, &'static str)] {
    &ITEM_KEYS
}
