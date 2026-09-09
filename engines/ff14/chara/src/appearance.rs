//! Where a character's look comes from. Vanilla sources only: the character-creator save
//! (`FFXIV_CHARA_xx.dat`) for the customize data and `GEARSET.DAT` for equipment.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use physis::savedata::chardat::{CharacterData, CustomizeData};

use crate::gearset::{GearsetItem, GearsetSlot, Gearsets};

/// A fully specified appearance: customize bytes plus equipped items.
#[derive(Debug, Clone)]
pub struct Appearance {
    pub customize: CustomizeData,
    /// Items by gear set slot (weapons, armour, accessories, soul crystal).
    pub items: [Option<GearsetItem>; crate::gearset::SLOT_COUNT],
    /// Where the data came from, for diagnostics.
    pub notes: Vec<String>,
}

impl Appearance {
    pub fn item(&self, slot: GearsetSlot) -> Option<GearsetItem> {
        self.items[slot as usize]
    }

    /// The item id to draw for a slot: glamour if set, else the equipped item.
    pub fn visible_item_id(&self, slot: GearsetSlot) -> Option<u32> {
        let it = self.item(slot)?;
        Some(if it.glamour_id != 0 { it.glamour_id } else { it.item_id })
    }
}

pub trait AppearanceSource {
    fn load(&self) -> Result<Appearance>;
}

/// Character-creator save: customize only.
pub struct CharaDatSource {
    pub path: PathBuf,
}

impl CharaDatSource {
    pub fn read(path: &Path) -> Result<CharacterData> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        CharacterData::from_existing(&bytes)
            .ok_or_else(|| anyhow!("{} is not a FFXIV_CHARA save", path.display()))
    }
}

impl AppearanceSource for CharaDatSource {
    fn load(&self) -> Result<Appearance> {
        let data = Self::read(&self.path)?;
        Ok(Appearance {
            customize: data.customize,
            items: Default::default(),
            notes: vec![format!(
                "customize from {} (comment {:?})",
                self.path.display(),
                data.comment
            )],
        })
    }
}

/// Gear set file: equipment only. `index` selects a set (default: the current one).
pub struct GearsetSource {
    pub path: PathBuf,
    pub index: Option<u8>,
}

impl GearsetSource {
    pub fn read(path: &Path) -> Result<Gearsets> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Gearsets::parse(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    /// `ClassJob` row of the selected gear set.
    pub fn class_job(&self) -> Result<u8> {
        let sets = Self::read(&self.path)?;
        let index = self.index.unwrap_or(sets.current);
        let set = sets
            .by_index(index)
            .or_else(|| sets.sets.first())
            .ok_or_else(|| anyhow!("no gear set {index} in {}", self.path.display()))?;
        Ok(set.class_job)
    }

    pub fn items(&self) -> Result<([Option<GearsetItem>; crate::gearset::SLOT_COUNT], String)> {
        let sets = Self::read(&self.path)?;
        let index = self.index.unwrap_or(sets.current);
        let set = sets
            .by_index(index)
            .or_else(|| sets.sets.first())
            .ok_or_else(|| anyhow!("no gear set {index} in {}", self.path.display()))?;
        let mut items: [Option<GearsetItem>; crate::gearset::SLOT_COUNT] = Default::default();
        for (i, it) in set.slots.iter().enumerate() {
            if it.item_id != 0 {
                items[i] = Some(*it);
            }
        }
        Ok((items, format!("gear set {} {:?} from {}", set.index, set.name, self.path.display())))
    }
}

/// Customize from a character-creator save plus gear from a gear set file.
pub struct VanillaSource {
    pub chara: CharaDatSource,
    pub gearset: Option<GearsetSource>,
}

impl AppearanceSource for VanillaSource {
    fn load(&self) -> Result<Appearance> {
        let mut app = self.chara.load()?;
        if let Some(g) = &self.gearset {
            let (items, note) = g.items()?;
            app.items = items;
            app.notes.push(note);
        }
        Ok(app)
    }
}

/// `FFXIV_CHARA_*.dat` files in the XIVLauncher config directory, sorted by name.
pub fn default_chara_dat_paths() -> Vec<PathBuf> {
    let Some(dir) = ffl_ff14_assets::locate::xivlauncher_config_dir() else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("FFXIV_CHARA_") && n.ends_with(".dat"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// `FFXIV_CHR*/GEARSET.DAT` files, most recently modified first.
pub fn default_gearset_paths() -> Vec<PathBuf> {
    let Some(dir) = ffl_ff14_assets::locate::xivlauncher_config_dir() else {
        return Vec::new();
    };
    let mut out: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("GEARSET.DAT"))
        .filter(|p| p.is_file())
        .filter_map(|p| {
            let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((t, p))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, p)| p).collect()
}
