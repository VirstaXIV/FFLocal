//! Mod packs: directories of replacement files addressed by game path, layered over the
//! sqpack. Two layouts are understood:
//!
//! - Penumbra mods: `meta.json` + `default_mod.json` (`Files` map, `FileSwaps`) +
//!   `group_*.json` option groups (`Single` groups pick the option with index
//!   `DefaultSettings`, `Multi` groups treat it as a bit mask; a Penumbra collection's
//!   settings override the defaults per group name; `Imc`/`Combining` groups carry no files).
//! - Plain mirrors: the directory tree mirrors game paths (`chara/equipment/e0408/...`).
//!
//! A Penumbra collection (`penumbra:<id>`, see [`crate::dalamud`]) is a *profile*: its
//! enabled mods, resolved with their option settings, highest priority first.
//! Nothing here writes; packs are read in place.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use ffl_core::{ModProfile, Provenance};
use physis::Platform;

use crate::dalamud::Dalamud;
use crate::source::{AssetSource, SqPackSource};

/// Prefix of profile ids that name a Penumbra collection.
pub const PENUMBRA_PROFILE: &str = "penumbra:";

/// A Penumbra metadata edit (`Manipulations` in a mod's option): the game tables it
/// overrides for one entry. Only the kinds the character loader consults are kept.
#[derive(Debug, Clone, PartialEq)]
pub enum Manipulation {
    /// EQDP: race code, gear set, slot name (`Body`, `Feet`, `Ears`, ...), 2-bit entry
    /// (1 = own material, 2 = own model).
    Eqdp { race: u16, set: u16, slot: String, entry: u8 },
    /// IMC: `Equipment`/`Accessory`/... primary id (set), secondary id, variant, slot name,
    /// material id, attribute mask.
    Imc { object: String, primary: u16, secondary: u16, variant: u16, slot: String, material_id: u8, attribute_mask: u16 },
    /// EST: race code, gear/face/hair set id, slot (`Face`, `Hair`, `Head`, `Body`), skeleton id.
    Est { race: u16, set: u16, slot: String, skeleton: u16 },
    /// A named submesh attribute switched on or off globally (`atrx_*` on modded bodies).
    Atr { attribute: String, enabled: bool },
}

/// Penumbra race + gender names → the race code (`c0801` = 801).
pub fn race_code(race: &str, gender: &str) -> Option<u16> {
    let base = match race {
        "Midlander" => 101,
        "Highlander" => 301,
        "Elezen" => 501,
        "Miqote" | "Miqo'te" => 701,
        "Roegadyn" => 901,
        "Lalafell" => 1101,
        "AuRa" | "Au Ra" => 1301,
        "Hrothgar" => 1501,
        "Viera" => 1701,
        _ => return None,
    };
    Some(if gender == "Female" { base + 100 } else { base })
}

fn parse_manipulations(container: &serde_json::Value, out: &mut Vec<Manipulation>) {
    let Some(list) = container.get("Manipulations").and_then(|m| m.as_array()) else {
        return;
    };
    let num = |v: Option<&serde_json::Value>| -> u64 { v.and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0) };
    let text = |v: Option<&serde_json::Value>| -> String { v.and_then(|x| x.as_str()).unwrap_or("").to_string() };
    for item in list {
        let kind = text(item.get("Type"));
        let Some(m) = item.get("Manipulation") else {
            continue;
        };
        match kind.as_str() {
            "Eqdp" => {
                if let Some(race) = race_code(&text(m.get("Race")), &text(m.get("Gender"))) {
                    out.push(Manipulation::Eqdp { race, set: num(m.get("SetId")) as u16, slot: text(m.get("Slot")), entry: num(m.get("Entry")) as u8 });
                }
            }
            "Imc" => {
                let e = m.get("Entry");
                out.push(Manipulation::Imc {
                    object: text(m.get("ObjectType")),
                    primary: num(m.get("PrimaryId")) as u16,
                    secondary: num(m.get("SecondaryId")) as u16,
                    variant: num(m.get("Variant")) as u16,
                    slot: text(m.get("EquipSlot")),
                    material_id: num(e.and_then(|e| e.get("MaterialId"))) as u8,
                    attribute_mask: num(e.and_then(|e| e.get("AttributeMask"))) as u16,
                });
            }
            "Est" => {
                if let Some(race) = race_code(&text(m.get("Race")), &text(m.get("Gender"))) {
                    out.push(Manipulation::Est { race, set: num(m.get("SetId")) as u16, slot: text(m.get("Slot")), skeleton: num(m.get("Entry")) as u16 });
                }
            }
            "Atr" => out.push(Manipulation::Atr { attribute: text(m.get("Attribute")), enabled: m.get("Entry").and_then(|e| e.as_bool()).unwrap_or(true) }),
            _ => {}
        }
    }
}

/// Every metadata edit the active packs make, highest priority applied last (it wins).
#[derive(Debug, Clone, Default)]
pub struct MetaOverrides {
    /// (race code, set, slot name) → 2-bit EQDP entry.
    pub eqdp: HashMap<(u16, u16, String), u8>,
    /// (object type, primary id, variant, slot name) → (material id, attribute mask).
    pub imc: HashMap<(String, u16, u16, String), (u8, u16)>,
    /// (race code, slot name, set) → skeleton id.
    pub est: HashMap<(u16, String, u16), u16>,
    /// Attribute name → visible.
    pub atr: HashMap<String, bool>,
}

impl MetaOverrides {
    pub fn is_empty(&self) -> bool {
        self.eqdp.is_empty() && self.imc.is_empty() && self.est.is_empty() && self.atr.is_empty()
    }

    fn apply(&mut self, m: &Manipulation) {
        match m {
            Manipulation::Eqdp { race, set, slot, entry } => {
                self.eqdp.insert((*race, *set, slot.clone()), *entry);
            }
            Manipulation::Imc { object, primary, variant, slot, material_id, attribute_mask, .. } => {
                self.imc.insert((object.clone(), *primary, *variant, slot.clone()), (*material_id, *attribute_mask));
            }
            Manipulation::Est { race, set, slot, skeleton } => {
                self.est.insert((*race, slot.clone(), *set), *skeleton);
            }
            Manipulation::Atr { attribute, enabled } => {
                self.atr.insert(attribute.clone(), *enabled);
            }
        }
    }
}

#[derive(Debug)]
pub struct ModPack {
    /// Directory name; stable id.
    pub id: String,
    pub name: String,
    pub description: String,
    pub root: PathBuf,
    /// Lower-case game path → file on disk.
    pub files: HashMap<String, PathBuf>,
    /// Lower-case game path → another game path that stands in for it (Penumbra `FileSwaps`).
    pub swaps: HashMap<String, String>,
    /// Metadata edits of the selected options (Penumbra `Manipulations`).
    pub manipulations: Vec<Manipulation>,
}

impl ModPack {
    pub fn file(&self, game_path: &str) -> Option<&Path> {
        self.files.get(&game_path.to_ascii_lowercase()).map(PathBuf::as_path)
    }

    pub fn swap(&self, game_path: &str) -> Option<&str> {
        self.swaps.get(&game_path.to_ascii_lowercase()).map(String::as_str)
    }

    /// A plain mirror directory (game paths under `root`) as a pack.
    pub fn load_mirror_dir(root: &Path, id: &str) -> anyhow::Result<ModPack> {
        if !root.is_dir() {
            anyhow::bail!("{} is not a directory", root.display());
        }
        let mut pack = ModPack {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            root: root.to_path_buf(),
            files: HashMap::new(),
            swaps: HashMap::new(),
            manipulations: Vec::new(),
        };
        load_mirror(&mut pack, root, "");
        Ok(pack)
    }

    /// A Penumbra mod directory with the given option selections (group name → value);
    /// missing groups use the mod's defaults.
    pub fn load_penumbra_dir(root: &Path, id: &str, options: Option<&HashMap<String, u64>>) -> anyhow::Result<ModPack> {
        let mut pack = ModPack {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            root: root.to_path_buf(),
            files: HashMap::new(),
            swaps: HashMap::new(),
            manipulations: Vec::new(),
        };
        load_penumbra(&mut pack, options)?;
        Ok(pack)
    }
}

/// Every pack under `dir` (one sub-directory each), sorted by id.
pub fn discover(dir: &Path) -> Vec<ModPack> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let root = e.path();
        if !root.is_dir() {
            continue;
        }
        match load_pack(&root) {
            Ok(p) if !p.files.is_empty() => out.push(p),
            Ok(p) => tracing::info!("mod {}: no files, skipped", p.id),
            Err(err) => tracing::warn!("mod {}: {err:#}", root.display()),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn load_pack(root: &Path) -> anyhow::Result<ModPack> {
    let id = root.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    let mut pack = ModPack {
        id: id.clone(),
        name: id,
        description: String::new(),
        root: root.to_path_buf(),
        files: HashMap::new(),
        swaps: HashMap::new(),
        manipulations: Vec::new(),
    };
    if root.join("default_mod.json").is_file() || root.join("meta.json").is_file() {
        load_penumbra(&mut pack, None)?;
    } else {
        load_mirror(&mut pack, root, "");
    }
    Ok(pack)
}

fn load_mirror(pack: &mut ModPack, dir: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let game_path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if path.is_dir() {
            load_mirror(pack, &path, &game_path);
        } else if !name.ends_with(".json") && !name.ends_with(".txt") && !name.ends_with(".md") && !name.ends_with(".toml") {
            pack.files.insert(game_path.to_ascii_lowercase(), path);
        }
    }
}

/// Parse JSON that may start with a UTF-8 BOM (Penumbra writes one).
fn json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(v) => Some(v),
        Err(err) => {
            tracing::warn!("{}: {err}", path.display());
            None
        }
    }
}

fn load_penumbra(pack: &mut ModPack, options: Option<&HashMap<String, u64>>) -> anyhow::Result<()> {
    let root = pack.root.clone();
    if let Some(meta) = json(&root.join("meta.json")) {
        if let Some(n) = meta.get("Name").and_then(|v| v.as_str()) {
            pack.name = n.to_string();
        }
        if let Some(d) = meta.get("Description").and_then(|v| v.as_str()) {
            pack.description = d.to_string();
        }
    }
    let mut add_files = |container: &serde_json::Value| {
        if let Some(map) = container.get("Files").and_then(|f| f.as_object()) {
            for (game_path, rel) in map {
                if let Some(rel) = rel.as_str() {
                    let on_disk = root.join(rel.replace('\\', "/"));
                    pack.files.insert(game_path.to_ascii_lowercase(), on_disk);
                }
            }
        }
        if let Some(map) = container.get("FileSwaps").and_then(|f| f.as_object()) {
            for (game_path, target) in map {
                if let Some(target) = target.as_str() {
                    pack.swaps.insert(game_path.to_ascii_lowercase(), target.to_ascii_lowercase());
                }
            }
        }
        parse_manipulations(container, &mut pack.manipulations);
    };
    if let Some(v) = json(&root.join("default_mod.json")) {
        add_files(&v);
    }
    let mut groups: Vec<PathBuf> = std::fs::read_dir(&root)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("group_") && n.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    groups.sort();
    for g in groups {
        let Some(v) = json(&g) else {
            continue;
        };
        let kind = v.get("Type").and_then(|t| t.as_str()).unwrap_or("Single");
        if kind != "Single" && kind != "Multi" {
            // `Imc` and `Combining` groups edit metadata, not files.
            continue;
        }
        let name = v.get("Name").and_then(|n| n.as_str()).unwrap_or("");
        let default = v.get("DefaultSettings").and_then(|d| d.as_u64()).unwrap_or(0);
        let value = options.and_then(|o| o.get(name).copied()).unwrap_or(default);
        let Some(list) = v.get("Options").and_then(|o| o.as_array()) else {
            continue;
        };
        for (i, opt) in list.iter().enumerate() {
            let selected = match kind {
                "Multi" => i < 64 && value & (1 << i) != 0,
                _ => i as u64 == value,
            };
            if selected {
                add_files(opt);
            }
        }
    }
    Ok(())
}

/// The packs found on disk, the Penumbra collections (profiles) and which of them are
/// enabled globally.
pub struct ModLibrary {
    pub dir: PathBuf,
    pub packs: Vec<Arc<ModPack>>,
    pub dalamud: Option<Arc<Dalamud>>,
    enabled: RwLock<Vec<String>>,
    /// Resolved collections by profile id (a collection's mods are read once).
    collections: RwLock<HashMap<String, Arc<Vec<Arc<ModPack>>>>>,
}

impl ModLibrary {
    pub fn open(dir: &Path, dalamud: Option<Arc<Dalamud>>) -> Self {
        let packs = discover(dir).into_iter().map(Arc::new).collect::<Vec<_>>();
        if !packs.is_empty() {
            tracing::info!("{} mod pack(s) under {}", packs.len(), dir.display());
        }
        Self {
            dir: dir.to_path_buf(),
            packs,
            dalamud,
            enabled: RwLock::new(Vec::new()),
            collections: RwLock::new(HashMap::new()),
        }
    }

    /// Penumbra collections as selectable profiles.
    pub fn profiles(&self) -> Vec<ModProfile> {
        let Some(p) = self.dalamud.as_ref().and_then(|d| d.penumbra.as_ref()) else {
            return Vec::new();
        };
        p.collections
            .iter()
            .map(|c| {
                let enabled = p.resolve(&c.id).map(|m| m.len()).unwrap_or(0);
                let own = c.settings.values().filter(|s| s.enabled).count();
                let mut roles = Vec::new();
                if p.yourself.as_deref() == Some(&c.id) {
                    roles.push("your character");
                }
                if p.current.as_deref() == Some(&c.id) {
                    roles.push("selected in Penumbra");
                }
                if p.default.as_deref() == Some(&c.id) {
                    roles.push("default");
                }
                for (who, id) in &p.individuals {
                    if id == &c.id {
                        roles.push(who);
                    }
                }
                // The designs Glamourer applies to the characters this collection is assigned to.
                let mut automation: Vec<String> = Vec::new();
                if let Some(g) = self.dalamud.as_ref().and_then(|d| d.glamourer.as_ref()) {
                    for (who, id) in &p.individuals {
                        if id != &c.id {
                            continue;
                        }
                        let player = who.split(" (").next().unwrap_or(who);
                        for a in g.automation_for(player) {
                            for d in a.designs.iter().filter_map(|d| g.design(d)) {
                                let line = format!("Glamourer applies design {:?} to {player}", d.name);
                                if !automation.contains(&line) {
                                    automation.push(line);
                                }
                            }
                        }
                    }
                }
                let automation = if automation.is_empty() { String::new() } else { format!(" — {}", automation.join("; ")) };
                ModProfile {
                    id: format!("{PENUMBRA_PROFILE}{}", c.id),
                    name: c.name.clone(),
                    detail: format!(
                        "Penumbra collection: {enabled} mods enabled ({own} of its own{}){}",
                        if enabled > own { format!(", {} inherited from {}", enabled - own, c.inherits.iter().filter_map(|i| p.collection(i)).map(|c| c.name.as_str()).collect::<Vec<_>>().join(" + ")) } else { String::new() },
                        if roles.is_empty() { automation.clone() } else { format!(" — {}{automation}", roles.join(", ")) }
                    ),
                    mods: enabled,
                }
            })
            .collect()
    }

    /// The mods of a Penumbra collection as packs, highest priority first.
    fn collection_packs(&self, profile: &str) -> Arc<Vec<Arc<ModPack>>> {
        if let Some(c) = self.collections.read().unwrap().get(profile) {
            return c.clone();
        }
        let mut packs = Vec::new();
        if let Some(p) = self.dalamud.as_ref().and_then(|d| d.penumbra.as_ref())
            && let Some(id) = profile.strip_prefix(PENUMBRA_PROFILE)
        {
            match p.resolve(id) {
                Ok(mods) => {
                    let mut files = 0;
                    for m in mods.iter().rev() {
                        let root = p.mod_directory.join(&m.dir);
                        if !root.is_dir() {
                            tracing::debug!("collection {id}: mod {} is not on disk", m.dir);
                            continue;
                        }
                        match ModPack::load_penumbra_dir(&root, &m.dir, Some(&m.options)) {
                            Ok(pack) => {
                                files += pack.files.len() + pack.swaps.len();
                                packs.push(Arc::new(pack));
                            }
                            Err(err) => tracing::warn!("collection {id}: mod {}: {err:#}", m.dir),
                        }
                    }
                    tracing::info!("Penumbra collection {}: {} mods, {files} replacements", p.collection(id).map(|c| c.name.as_str()).unwrap_or(id), packs.len());
                }
                Err(err) => tracing::warn!("{profile}: {err:#}"),
            }
        }
        let packs = Arc::new(packs);
        self.collections.write().unwrap().insert(profile.to_string(), packs.clone());
        packs
    }

    pub fn set_enabled(&self, ids: &[String]) {
        *self.enabled.write().unwrap() = ids.to_vec();
    }

    pub fn enabled(&self) -> Vec<String> {
        self.enabled.read().unwrap().clone()
    }

    /// Packs by id, in the given order, skipping unknown ids; a profile id expands to the
    /// collection's mods.
    pub fn select(&self, ids: &[String]) -> Vec<Arc<ModPack>> {
        let mut out = Vec::new();
        for id in ids {
            if id.starts_with(PENUMBRA_PROFILE) {
                out.extend(self.collection_packs(id).iter().cloned());
            } else if let Some(p) = self.packs.iter().find(|p| &p.id == id) {
                out.push(p.clone());
            }
        }
        out
    }
}

/// A source that answers from the selected packs first (in order), then the game.
pub struct LayeredSource {
    pub base: Arc<SqPackSource>,
    pub packs: Vec<Arc<ModPack>>,
}

impl LayeredSource {
    /// First pack that has  as an existing file (a listed but missing file falls
    /// through to the game and is reported as such).
    fn find(&self, path: &str) -> Option<(&ModPack, &Path)> {
        self.packs.iter().find_map(|p| p.file(path).filter(|f| f.is_file()).map(|f| (p.as_ref(), f)))
    }
}

impl LayeredSource {
    /// First pack that redirects  to another game path (a file replacement in an
    /// earlier pack wins over a swap in a later one).
    fn swap_for(&self, path: &str) -> Option<&str> {
        for p in &self.packs {
            if p.file(path).is_some_and(|f| f.is_file()) {
                return None;
            }
            if let Some(target) = p.swap(path) {
                return Some(target);
            }
        }
        None
    }
}

impl AssetSource for LayeredSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        if let Some((pack, file)) = self.find(path) {
            match std::fs::read(file) {
                Ok(bytes) => return Some(bytes),
                Err(err) => tracing::warn!("mod {}: {}: {err}", pack.id, file.display()),
            }
        }
        if let Some(target) = self.swap_for(path)
            && target != path
        {
            // One hop: the swapped-in path may itself be replaced by a file.
            if let Some((_, file)) = self.find(target)
                && let Ok(bytes) = std::fs::read(file)
            {
                return Some(bytes);
            }
            return self.base.read(target);
        }
        self.base.read(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.find(path).is_some() || self.swap_for(path).is_some_and(|t| self.find(t).is_some() || self.base.exists(t)) || self.base.exists(path)
    }

    fn platform(&self) -> Platform {
        self.base.platform()
    }

    fn read_vanilla(&self, path: &str) -> Option<Vec<u8>> {
        self.base.read(path)
    }

    fn meta_overrides(&self) -> MetaOverrides {
        let mut out = MetaOverrides::default();
        // First pack wins for files; for metadata apply it last so it overrides the rest.
        for pack in self.packs.iter().rev() {
            for m in &pack.manipulations {
                out.apply(m);
            }
        }
        out
    }

    fn describe(&self) -> String {
        let packs: Vec<&str> = self.packs.iter().map(|p| p.id.as_str()).collect();
        format!("{} + mods [{}]", self.base.describe(), packs.join(", "))
    }

    fn origin(&self, path: &str) -> Provenance {
        match self.find(path) {
            Some((pack, _)) => Provenance::Mod(pack.id.clone()),
            None => match self.swap_for(path).and_then(|t| self.find(t)) {
                Some((pack, _)) => Provenance::Mod(pack.id.clone()),
                None => Provenance::Game,
            },
        }
    }
}
