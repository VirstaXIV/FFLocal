//! The engine trait: one implementation per game/content source.

use anyhow::Result;

use crate::character::{CharacterModel, Clip};
use crate::material::ShaderDef;
use crate::mesh::ModelData;
use crate::preset::CharacterPreset;
use crate::scene::Scene;
use crate::sound::{ClockSpec, MusicSet, SoundData};

/// Whether an engine's content may be sent to other players. Game assets read from a licensed
/// install stay local; mod files or original content may be shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentPolicy {
    LocalOnly,
    Shareable,
}

#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub available: bool,
    pub detail: String,
    /// The install directory in use (when available).
    pub path: Option<String>,
    /// Companion tooling the engine reads (mod managers, plugin data): one line, empty when
    /// the engine has none.
    pub plugins: String,
}

/// A place an engine's game might be installed, found by scanning the machine.
#[derive(Debug, Clone)]
pub struct InstallCandidate {
    pub path: std::path::PathBuf,
    /// Where the hint came from ("Steam library", "XIVLauncher", ...).
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct MapInfo {
    pub engine: String,
    pub id: String,
    pub name: String,
    pub category: String,
    pub detail: String,
}

/// An editable preset field an engine exposes in the character editor.
#[derive(Debug, Clone)]
pub struct PresetField {
    pub key: String,
    pub label: String,
    /// Section the editor groups the field under ("Race", "Face", "Gear", ...).
    pub group: String,
    pub kind: FieldKind,
    /// Short help shown next to the field (may be empty).
    pub hint: String,
}

impl PresetField {
    pub fn new(key: &str, label: &str, group: &str, kind: FieldKind) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            group: group.into(),
            kind,
            hint: String::new(),
        }
    }

    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = hint.into();
        self
    }
}

#[derive(Debug, Clone)]
pub enum FieldKind {
    /// Pick one of (value, label).
    Choice(Vec<(String, String)>),
    Text,
    Integer { min: i64, max: i64 },
    Bool,
    /// Pick a colour by index into the palette (sRGB).
    Palette(Vec<[u8; 3]>),
    /// Pick an entry of an engine catalog (see [`Engine::catalog`]); the value is the entry id,
    /// an empty value means none.
    Lookup { catalog: String, none_label: String },
    /// Any number of (value, label); the value is `;`-separated.
    MultiChoice(Vec<(String, String)>),
}

/// Something a character can be imported from ("the game's character-creator save slot 1").
#[derive(Debug, Clone)]
pub struct ImportSource {
    pub id: String,
    pub label: String,
    pub detail: String,
}

/// One searchable catalog entry (an item, a hairstyle, ...).
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub id: String,
    pub label: String,
    pub detail: String,
}

/// A mod pack the engine found (replacement files for game assets).
#[derive(Debug, Clone)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Number of game files it replaces.
    pub files: usize,
}

/// A named selection of mods maintained outside FFLocal (a Penumbra collection): enabling
/// its id enables every mod it selects, with the options it chose.
#[derive(Debug, Clone)]
pub struct ModProfile {
    pub id: String,
    pub name: String,
    pub detail: String,
    /// Number of mods the profile enables.
    pub mods: usize,
}

/// What [`Engine::archive_character`] wrote.
#[derive(Debug, Clone, Default)]
pub struct ArchiveReport {
    pub dir: std::path::PathBuf,
    /// Modded files copied into the archive.
    pub files: usize,
    pub bytes: u64,
    /// Files that still come from the game (not copied; the archive needs the game for them).
    pub vanilla: usize,
    /// Mod packs the copied files came from.
    pub packs: Vec<String>,
}

pub trait Engine: Send + Sync {
    fn info(&self) -> EngineInfo;
    fn content_policy(&self) -> ContentPolicy;

    /// Shaders this engine's materials use. Registered once at startup by the runtime.
    fn shaders(&self) -> Vec<ShaderDef> {
        Vec::new()
    }

    /// Maps this engine can load.
    fn list_maps(&self) -> Result<Vec<MapInfo>>;
    /// Build the scene graph for a map id.
    fn load_scene(&self, map_id: &str) -> Result<Scene>;
    /// Load a model referenced by a scene node.
    fn load_model(&self, key: &str) -> Result<ModelData>;

    /// Fields a character preset for this engine has. Choices may depend on the preset being
    /// edited (hairstyles per race, ...), hence the optional context.
    fn preset_fields(&self, preset: Option<&CharacterPreset>) -> Result<Vec<PresetField>>;
    /// Sensible starting presets (e.g. one per save slot found, imported).
    fn default_presets(&self) -> Result<Vec<CharacterPreset>>;
    /// A fresh preset with every field at a sensible default.
    fn new_preset(&self, name: &str) -> Result<CharacterPreset> {
        let mut settings = std::collections::BTreeMap::new();
        for f in self.preset_fields(None)? {
            let v = match &f.kind {
                FieldKind::Choice(c) => c.first().map(|c| c.0.clone()).unwrap_or_default(),
                FieldKind::Bool => "false".into(),
                FieldKind::Integer { min, .. } => min.to_string(),
                FieldKind::Palette(_) => "0".into(),
                FieldKind::Text | FieldKind::Lookup { .. } | FieldKind::MultiChoice(_) => String::new(),
            };
            settings.insert(f.key.clone(), v);
        }
        Ok(CharacterPreset {
            id: crate::preset::Library::new_id(),
            name: name.to_string(),
            engine: self.info().id,
            settings,
        })
    }
    /// Called when a preset is opened in the editor: engines may upgrade old preset formats
    /// (e.g. presets that only pointed at game files) so the fields reflect the character.
    fn prepare_for_edit(&self, preset: &mut CharacterPreset) -> Result<Option<String>> {
        let _ = preset;
        Ok(None)
    }
    /// Where a preset can be imported from (the game's own character data).
    fn import_sources(&self) -> Vec<ImportSource> {
        Vec::new()
    }
    /// Merge the data of an import source into `preset`; returns a note about what was taken.
    fn import_preset(&self, preset: &mut CharacterPreset, source_id: &str) -> Result<String> {
        Err(anyhow::anyhow!("{} cannot import {source_id} into {}", self.info().name, preset.name))
    }
    /// Search a catalog used by [`FieldKind::Lookup`] fields.
    fn catalog(&self, catalog: &str, query: &str, limit: usize) -> Result<Vec<CatalogEntry>> {
        let _ = (catalog, query, limit);
        Ok(Vec::new())
    }
    /// Display label of one catalog entry.
    fn catalog_label(&self, catalog: &str, id: &str) -> Option<String> {
        let _ = (catalog, id);
        None
    }
    /// Mod packs this engine can overlay on the game files.
    fn list_mods(&self) -> Vec<ModInfo> {
        Vec::new()
    }
    /// Mod profiles (collections of an external mod manager) selectable like packs.
    fn mod_profiles(&self) -> Vec<ModProfile> {
        Vec::new()
    }
    /// Globally enabled mod packs and profiles (worlds and characters without their own list).
    fn set_enabled_mods(&self, ids: &[String]) {
        let _ = ids;
    }
    /// Copy every modded file a character uses into `dir` as a mirror of the engine's paths
    /// plus a manifest, so the character can be loaded from that copy (and, later, shared)
    /// without the mod manager; vanilla files are not copied.
    fn archive_character(&self, preset: &CharacterPreset, dir: &std::path::Path) -> Result<ArchiveReport> {
        let _ = dir;
        Err(anyhow::anyhow!("{} cannot archive {}", self.info().name, preset.name))
    }
    /// Load a character from a preset of this engine.
    fn load_character(&self, preset: &CharacterPreset) -> Result<CharacterModel>;

    /// Load the clip for one of the character's actions (see `CharacterModel::actions`).
    fn load_action(&self, preset: &CharacterPreset, action_id: &str) -> Result<Clip> {
        Err(anyhow::anyhow!("engine has no action {action_id} for {}", preset.name))
    }

    /// Background music of a map (also delivered as `Scene::music` by `load_scene`).
    fn music_for_map(&self, map_id: &str) -> Result<Option<MusicSet>> {
        let _ = map_id;
        Ok(None)
    }

    /// Decode one sound by key (keys come from `MusicSet`, `AmbientEmitter`, `SoundCue`).
    fn load_sound(&self, key: &str) -> Result<SoundData> {
        Err(anyhow::anyhow!("engine cannot load sound {key}"))
    }

    /// The engine's game clock, used to pick day/night music. `None` = no day/night cycle.
    fn clock(&self) -> Option<ClockSpec> {
        None
    }
}
