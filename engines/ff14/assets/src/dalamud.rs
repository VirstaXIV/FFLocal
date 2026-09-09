//! Dalamud plugin data: where XIVLauncher keeps the plugin configurations, Penumbra's mod
//! directory and collections, Glamourer's designs. Everything is read in place; nothing is
//! written into the launcher's or the plugins' directories.
//!
//! Layout (XIVLauncher.Core on Linux: `~/.xlcore/pluginConfigs/`; Windows:
//! `%APPDATA%\XIVLauncher\pluginConfigs\`):
//! - `Penumbra.json`: `ModDirectory` (a Wine path such as `Z:\media\...` on Linux).
//! - `Penumbra/collections/<id>.json`: `{Version, Id, Name, Settings: {<mod dir>:
//!   {Enabled, Priority, Settings: {<group>: value}}}, Inheritance: [<ids>]}`; a collection
//!   inherits the settings of mods it has no entry for from its inheritance list.
//! - `Penumbra/active_collections.json`: `Default`, `Interface`, `Current`, `Yourself`,
//!   `Individuals` (player name/world → collection).
//! - `Glamourer/designs/<id>.json`: `Name`, `FileSystemFolder`, `Customize: {<field>:
//!   {Value, Apply}}` (raw customize bytes), `Equipment: {<slot>: {ItemId, Apply, Stain}}`
//!   (item ids; ids at 0xFFFFFF00.. mean "nothing").
//! Files are UTF-8 with a BOM.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

/// Read a JSON file that may start with a UTF-8 BOM.
pub fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str(text).with_context(|| format!("parsing {}", path.display()))
}

/// Candidate plugin-config directories on this machine, best first.
pub fn candidates() -> Vec<ffl_core::InstallCandidate> {
    let mut out = Vec::new();
    let mut push = |p: PathBuf, source: &str| {
        if p.is_dir() {
            out.push(ffl_core::InstallCandidate { path: p, source: source.to_string() });
        }
    };
    if let Some(d) = super::locate::load_config().0.dalamud_path {
        push(PathBuf::from(d), "config.toml dalamud_path");
    }
    if let Some(home) = dirs::home_dir() {
        push(home.join(".xlcore/pluginConfigs"), "XIVLauncher.Core");
        push(home.join(".local/share/dev.goats.xivlauncher/pluginConfigs"), "XIVLauncher.Core (flatpak)");
        push(home.join("Library/Application Support/XIV on Mac/pluginConfigs"), "XIV on Mac");
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        push(PathBuf::from(appdata).join("XIVLauncher/pluginConfigs"), "XIVLauncher");
    }
    out
}

/// The plugin configuration root and what it holds.
#[derive(Debug)]
pub struct Dalamud {
    pub root: PathBuf,
    pub source: String,
    pub penumbra: Option<Penumbra>,
    pub glamourer: Option<Glamourer>,
}

impl Dalamud {
    /// Open an explicit plugin-config directory, or the first candidate found.
    pub fn locate(explicit: Option<&Path>) -> Option<Dalamud> {
        let (root, source) = match explicit {
            Some(p) => (p.to_path_buf(), "chosen".to_string()),
            None => {
                let c = candidates().into_iter().next()?;
                (c.path, c.source)
            }
        };
        if !root.is_dir() {
            return None;
        }
        let penumbra = match Penumbra::open(&root) {
            Ok(p) => Some(p),
            Err(err) => {
                tracing::info!("no Penumbra data under {}: {err:#}", root.display());
                None
            }
        };
        let glamourer = Glamourer::open(&root);
        Some(Dalamud { root, source, penumbra, glamourer })
    }

    /// One-line status for the Games window.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match &self.penumbra {
            Some(p) => parts.push(format!(
                "Penumbra: {} mods in {}, {} collections{}",
                p.mod_count(),
                p.mod_directory.display(),
                p.collections.len(),
                p.current.as_ref().and_then(|id| p.collection(id)).map(|c| format!(" (current: {})", c.name)).unwrap_or_default()
            )),
            None => parts.push("Penumbra: not found".into()),
        }
        match &self.glamourer {
            Some(g) => parts.push(format!("Glamourer: {} designs", g.designs.len())),
            None => parts.push("Glamourer: not found".into()),
        }
        format!("{} ({}): {}", self.root.display(), self.source, parts.join("; "))
    }
}

/// Turn a path written by a Windows program (Penumbra under Wine) into a local path: `Z:\`
/// is the Wine root, `C:\` the prefix's `drive_c` next to the plugin directory's launcher
/// root (`~/.xlcore/wineprefix/drive_c`). Native Windows paths pass through.
pub fn wine_path(raw: &str, plugin_root: &Path) -> PathBuf {
    if cfg!(windows) {
        return PathBuf::from(raw);
    }
    let unix = raw.replace('\\', "/");
    let bytes = unix.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = unix[2..].trim_start_matches('/');
        return match drive {
            'z' => PathBuf::from("/").join(rest),
            _ => plugin_root
                .parent()
                .map(|launcher| launcher.join("wineprefix").join(format!("drive_{drive}")).join(rest))
                .unwrap_or_else(|| PathBuf::from(rest)),
        };
    }
    PathBuf::from(unix)
}

/// A mod's settings inside a collection.
#[derive(Debug, Clone, Default)]
pub struct ModSetting {
    pub enabled: bool,
    pub priority: i64,
    /// Option group name → selected value (index for `Single`, bit mask for `Multi`).
    pub options: HashMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub settings: HashMap<String, ModSetting>,
    pub inherits: Vec<String>,
}

/// A mod selected by a collection, after inheritance: directory name, priority, options.
#[derive(Debug, Clone)]
pub struct SelectedMod {
    pub dir: String,
    pub priority: i64,
    pub options: HashMap<String, u64>,
}

#[derive(Debug)]
pub struct Penumbra {
    pub config_path: PathBuf,
    pub mod_directory: PathBuf,
    pub collections: Vec<Collection>,
    /// `active_collections.json`: the collection Penumbra applies to the player ("Yourself"),
    /// the one selected in its window ("Current") and the default.
    pub yourself: Option<String>,
    pub current: Option<String>,
    pub default: Option<String>,
    /// Individual assignments: display name → collection id.
    pub individuals: Vec<(String, String)>,
}

impl Penumbra {
    pub fn open(root: &Path) -> Result<Penumbra> {
        let config_path = root.join("Penumbra.json");
        let config = read_json(&config_path)?;
        let raw_dir = config.get("ModDirectory").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("Penumbra.json has no ModDirectory"))?;
        let mod_directory = wine_path(raw_dir, root);
        let mut collections = Vec::new();
        let dir = root.join("Penumbra/collections");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
            files.sort();
            for f in files {
                match read_json(&f).and_then(|v| parse_collection(&v)) {
                    Ok(c) => collections.push(c),
                    Err(err) => tracing::warn!("{}: {err:#}", f.display()),
                }
            }
        }
        let mut out = Penumbra {
            config_path,
            mod_directory,
            collections,
            yourself: None,
            current: None,
            default: None,
            individuals: Vec::new(),
        };
        if let Ok(active) = read_json(&root.join("Penumbra/active_collections.json")) {
            let id = |k: &str| active.get(k).and_then(|v| v.as_str()).map(str::to_string);
            out.yourself = id("Yourself");
            out.current = id("Current");
            out.default = id("Default");
            if let Some(list) = active.get("Individuals").and_then(|v| v.as_array()) {
                for i in list {
                    if let (Some(c), Some(d)) = (i.get("Collection").and_then(|v| v.as_str()), i.get("Display").and_then(|v| v.as_str())) {
                        out.individuals.push((d.to_string(), c.to_string()));
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn collection(&self, id: &str) -> Option<&Collection> {
        self.collections.iter().find(|c| c.id == id)
    }

    /// Mod directories under the mod directory (each Penumbra mod is one).
    pub fn mod_count(&self) -> usize {
        std::fs::read_dir(&self.mod_directory).map(|d| d.flatten().filter(|e| e.path().is_dir()).count()).unwrap_or(0)
    }

    /// The mods a collection enables, own settings first, then inherited ones for mods it
    /// has no entry for (depth first, the way Penumbra resolves inheritance). Sorted by
    /// priority ascending, so a later mod in the list overrides an earlier one.
    pub fn resolve(&self, id: &str) -> Result<Vec<SelectedMod>> {
        let mut settings: BTreeMap<String, ModSetting> = BTreeMap::new();
        let mut visited = Vec::new();
        self.collect(id, &mut settings, &mut visited)?;
        let mut out: Vec<SelectedMod> = settings
            .into_iter()
            .filter(|(_, s)| s.enabled)
            .map(|(dir, s)| SelectedMod { dir, priority: s.priority, options: s.options })
            .collect();
        out.sort_by_key(|m| m.priority);
        Ok(out)
    }

    fn collect(&self, id: &str, into: &mut BTreeMap<String, ModSetting>, visited: &mut Vec<String>) -> Result<()> {
        if visited.iter().any(|v| v == id) {
            return Ok(());
        }
        visited.push(id.to_string());
        let c = self.collection(id).ok_or_else(|| anyhow!("no collection {id}"))?;
        for (dir, s) in &c.settings {
            into.entry(dir.clone()).or_insert_with(|| s.clone());
        }
        for parent in &c.inherits {
            self.collect(parent, into, visited)?;
        }
        Ok(())
    }
}

fn parse_collection(v: &Value) -> Result<Collection> {
    let id = v.get("Id").and_then(|i| i.as_str()).ok_or_else(|| anyhow!("collection without Id"))?.to_string();
    let name = v.get("Name").and_then(|n| n.as_str()).unwrap_or(&id).to_string();
    let mut settings = HashMap::new();
    if let Some(map) = v.get("Settings").and_then(|s| s.as_object()) {
        for (dir, s) in map {
            let mut setting = ModSetting {
                enabled: s.get("Enabled").and_then(|e| e.as_bool()).unwrap_or(false),
                priority: s.get("Priority").and_then(|p| p.as_i64()).unwrap_or(0),
                options: HashMap::new(),
            };
            if let Some(opts) = s.get("Settings").and_then(|o| o.as_object()) {
                for (group, value) in opts {
                    if let Some(n) = value.as_u64() {
                        setting.options.insert(group.clone(), n);
                    }
                }
            }
            settings.insert(dir.clone(), setting);
        }
    }
    let inherits = v
        .get("Inheritance")
        .and_then(|i| i.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(Collection { id, name, settings, inherits })
}

// ---------------------------------------------------------------------------------------------
// Glamourer
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Design {
    pub id: String,
    pub name: String,
    pub folder: String,
    /// Glamourer customize field → (raw value, apply).
    pub customize: Vec<(String, u64, bool)>,
    /// Glamourer slot name → (item id, apply). Ids of `NOTHING_ID..` mean the slot is emptied.
    pub equipment: Vec<(String, u64, bool)>,
    /// Glamourer slot name → (stain, stain 2, apply stain).
    pub stains: Vec<(String, u8, u8, bool)>,
    /// Glamourer "advanced customization" parameters → (rgba or scalar in x, apply): the
    /// colours the game's shaders receive (palette domain, like `human.cmp`), which never
    /// touch the customize bytes.
    pub parameters: Vec<(String, [f32; 4], bool)>,
}

/// One Glamourer automation set: the designs applied to a character by name.
#[derive(Debug, Clone)]
pub struct Automation {
    pub name: String,
    /// The character (player name) the set applies to.
    pub player: String,
    pub enabled: bool,
    /// Design ids in application order (Glamourer's own `//` pseudo designs are skipped).
    pub designs: Vec<String>,
}

/// Glamourer's "nothing" item ids start here (`uint.MaxValue - 128 - slot`).
pub const NOTHING_ID: u64 = 0xFFFF_FF00;

#[derive(Debug)]
pub struct Glamourer {
    pub designs: Vec<Design>,
    pub automation: Vec<Automation>,
}

impl Glamourer {
    pub fn open(root: &Path) -> Option<Glamourer> {
        let dir = root.join("Glamourer/designs");
        let entries = std::fs::read_dir(&dir).ok()?;
        let mut designs = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            match read_json(&p).and_then(|v| parse_design(&v)) {
                Ok(d) => designs.push(d),
                Err(err) => tracing::warn!("{}: {err:#}", p.display()),
            }
        }
        designs.sort_by(|a, b| (a.folder.to_lowercase(), a.name.to_lowercase()).cmp(&(b.folder.to_lowercase(), b.name.to_lowercase())));
        let automation = read_json(&root.join("Glamourer/automation.json"))
            .map(|v| parse_automation(&v))
            .unwrap_or_default();
        Some(Glamourer { designs, automation })
    }

    pub fn design(&self, id: &str) -> Option<&Design> {
        self.designs.iter().find(|d| d.id == id)
    }

    /// The enabled automation sets of a character (by player name, case-insensitive).
    pub fn automation_for(&self, player: &str) -> Vec<&Automation> {
        let player = player.trim().to_lowercase();
        self.automation.iter().filter(|a| a.enabled && a.player.to_lowercase() == player && !a.designs.is_empty()).collect()
    }
}

/// `Glamourer/automation.json`: `Data: [{Name, Identifier: {PlayerName}, Enabled, Designs: [{Design}]}]`.
fn parse_automation(v: &Value) -> Vec<Automation> {
    let Some(sets) = v.get("Data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    sets.iter()
        .filter_map(|set| {
            let player = set.get("Identifier")?.get("PlayerName")?.as_str()?.to_string();
            let designs = set
                .get("Designs")
                .and_then(|d| d.as_array())
                .map(|ds| ds.iter().filter_map(|d| d.get("Design")?.as_str()).filter(|d| !d.starts_with("//")).map(str::to_string).collect())
                .unwrap_or_default();
            Some(Automation {
                name: set.get("Name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                player,
                enabled: set.get("Enabled").and_then(|e| e.as_bool()).unwrap_or(true),
                designs,
            })
        })
        .collect()
}

fn parse_design(v: &Value) -> Result<Design> {
    let id = v.get("Identifier").and_then(|i| i.as_str()).ok_or_else(|| anyhow!("design without Identifier"))?.to_string();
    let name = v.get("Name").and_then(|n| n.as_str()).unwrap_or(&id).to_string();
    let folder = v.get("FileSystemFolder").and_then(|n| n.as_str()).unwrap_or("").to_string();
    let mut customize = Vec::new();
    if let Some(map) = v.get("Customize").and_then(|c| c.as_object()) {
        for (k, e) in map {
            if let Some(value) = e.get("Value").and_then(|x| x.as_u64()) {
                customize.push((k.clone(), value, e.get("Apply").and_then(|a| a.as_bool()).unwrap_or(false)));
            }
        }
    }
    let mut equipment = Vec::new();
    let mut stains = Vec::new();
    if let Some(map) = v.get("Equipment").and_then(|c| c.as_object()) {
        for (k, e) in map {
            if let Some(item) = e.get("ItemId").and_then(|x| x.as_u64()) {
                equipment.push((k.clone(), item, e.get("Apply").and_then(|a| a.as_bool()).unwrap_or(false)));
            }
            let stain = |name: &str| e.get(name).and_then(|x| x.as_u64()).unwrap_or(0) as u8;
            stains.push((k.clone(), stain("Stain"), stain("Stain2"), e.get("ApplyStain").and_then(|a| a.as_bool()).unwrap_or(false)));
        }
    }
    let mut parameters = Vec::new();
    if let Some(map) = v.get("Parameters").and_then(|c| c.as_object()) {
        for (k, e) in map {
            let f = |n: &str| e.get(n).and_then(|x| x.as_f64()).map(|x| x as f32);
            let value = match (f("Red"), f("Green"), f("Blue")) {
                (Some(r), Some(g), Some(b)) => [r, g, b, f("Alpha").unwrap_or(1.0)],
                _ => match f("Value").or_else(|| f("Percentage")) {
                    Some(x) => [x, 0.0, 0.0, 0.0],
                    None => continue,
                },
            };
            parameters.push((k.clone(), value, e.get("Apply").and_then(|a| a.as_bool()).unwrap_or(false)));
        }
    }
    Ok(Design { id, name, folder, customize, equipment, stains, parameters })
}

/// FFLocal preset keys a Glamourer parameter writes: (field, key, components). Colours are
/// stored as `r g b [a]` in 0..1 (palette domain); the face-paint UV values as one number.
pub const PARAMETER_MAP: &[(&str, &str, usize)] = &[
    ("SkinDiffuse", "color.skin", 3),
    ("HairDiffuse", "color.hair", 3),
    ("HairHighlight", "color.highlight", 3),
    ("LeftEye", "color.eye_left", 3),
    ("RightEye", "color.eye_right", 3),
    ("FeatureColor", "color.feature", 3),
    ("LipDiffuse", "color.lip", 4),
    ("DecalColor", "color.decal", 4),
    ("FacePaintUvMultiplier", "decal_uv_scale", 1),
    ("FacePaintUvOffset", "decal_uv_offset", 1),
];

/// Parse a `color.*` / `decal_uv_*` preset value written by [`apply_design`].
pub fn parse_parameter(value: &str) -> Option<[f32; 4]> {
    let mut out = [0.0, 0.0, 0.0, 1.0];
    let mut n = 0;
    for (i, part) in value.split_whitespace().take(4).enumerate() {
        out[i] = part.parse::<f32>().ok()?;
        n += 1;
    }
    (n > 0).then_some(out)
}

/// FFLocal preset keys a Glamourer customize field writes: (field, key, flag bit). Fields
/// with a flag bit OR it into the key's byte instead of replacing it.
pub const CUSTOMIZE_MAP: &[(&str, &str, u8)] = &[
    ("Race", "race", 0),
    ("Gender", "gender", 0),
    ("BodyType", "age", 0),
    ("Height", "height", 0),
    ("Clan", "tribe", 0),
    ("Face", "face", 0),
    ("Hairstyle", "hair", 0),
    ("Highlights", "highlights", 0x80),
    ("SkinColor", "skin_tone", 0),
    ("EyeColorRight", "right_eye_color", 0),
    ("HairColor", "hair_tone", 0),
    ("HighlightsColor", "highlight_tone", 0),
    ("FacialFeature1", "facial_features", 0x01),
    ("FacialFeature2", "facial_features", 0x02),
    ("FacialFeature3", "facial_features", 0x04),
    ("FacialFeature4", "facial_features", 0x08),
    ("FacialFeature5", "facial_features", 0x10),
    ("FacialFeature6", "facial_features", 0x20),
    ("FacialFeature7", "facial_features", 0x40),
    ("LegacyTattoo", "facial_features", 0x80),
    ("TattooColor", "facial_feature_color", 0),
    ("Eyebrows", "eyebrows", 0),
    ("EyeColorLeft", "left_eye_color", 0),
    ("EyeShape", "eyes", 0),
    ("SmallIris", "eyes", 0x80),
    ("Nose", "nose", 0),
    ("Jaw", "jaw", 0),
    ("Mouth", "mouth", 0),
    ("Lipstick", "mouth", 0x80),
    ("LipColor", "lips_tone", 0),
    ("MuscleMass", "race_feature_size", 0),
    ("TailShape", "race_feature_type", 0),
    ("BustSize", "bust", 0),
    ("FacePaint", "face_paint", 0),
    ("FacePaintReversed", "face_paint", 0x80),
    ("FacePaintColor", "face_paint_color", 0),
];

/// Glamourer equipment slot → FFLocal item key.
pub const EQUIPMENT_MAP: &[(&str, &str)] = &[
    ("MainHand", "item.mainhand"),
    ("OffHand", "item.offhand"),
    ("Head", "item.head"),
    ("Body", "item.body"),
    ("Hands", "item.hands"),
    ("Legs", "item.legs"),
    ("Feet", "item.feet"),
    ("Ears", "item.ears"),
    ("Neck", "item.neck"),
    ("Wrists", "item.wrists"),
    ("RFinger", "item.ring_right"),
    ("LFinger", "item.ring_left"),
];

/// Write the applied parts of a design into preset settings; returns (customize fields,
/// gear slots) written.
pub fn apply_design(design: &Design, settings: &mut BTreeMap<String, String>) -> (usize, usize, usize) {
    // Flag fields OR into a byte the plain field of the same key sets first.
    let mut customized = 0;
    let mut bytes: BTreeMap<&str, u8> = BTreeMap::new();
    for (field, key, flag) in CUSTOMIZE_MAP {
        let Some((_, value, apply)) = design.customize.iter().find(|(f, _, _)| f == field) else {
            continue;
        };
        if !*apply {
            continue;
        }
        customized += 1;
        let current = bytes.get(key).copied().unwrap_or_else(|| settings.get(*key).and_then(|v| v.parse::<u8>().ok()).unwrap_or(0));
        let value = *value as u8;
        let next = if *flag == 0 {
            // A plain field keeps the flag bits its flag siblings may set later.
            value
        } else if value != 0 {
            current | flag
        } else {
            current & !flag
        };
        bytes.insert(key, next);
    }
    for (key, value) in bytes {
        settings.insert(key.to_string(), value.to_string());
    }
    let mut geared = 0;
    for (slot, key) in EQUIPMENT_MAP {
        let Some((_, item, apply)) = design.equipment.iter().find(|(s, _, _)| s == slot) else {
            continue;
        };
        if !*apply {
            continue;
        }
        geared += 1;
        settings.insert(key.to_string(), if *item >= NOTHING_ID || *item == 0 { String::new() } else { item.to_string() });
    }
    for (slot, key) in EQUIPMENT_MAP {
        let Some((_, s1, s2, apply)) = design.stains.iter().find(|(s, _, _, _)| s == slot) else {
            continue;
        };
        if !*apply {
            continue;
        }
        let stem = key.trim_start_matches("item.");
        settings.insert(format!("dye.{stem}"), if *s1 == 0 { String::new() } else { s1.to_string() });
        settings.insert(format!("dye2.{stem}"), if *s2 == 0 { String::new() } else { s2.to_string() });
    }
    // Advanced colours: an applied parameter overrides the palette colour, one the design
    // does not apply clears an earlier override so the palette shows through again.
    let mut colored = 0;
    for (field, key, n) in PARAMETER_MAP {
        let Some((_, value, apply)) = design.parameters.iter().find(|(f, _, _)| f == field) else {
            continue;
        };
        if *apply {
            colored += 1;
            let text = value.iter().take(*n).map(|x| format!("{x:.4}")).collect::<Vec<_>>().join(" ");
            settings.insert(key.to_string(), text);
        } else {
            settings.remove(*key);
        }
    }
    (customized, geared, colored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wine_paths() {
        let root = Path::new("/tmp/x/.xlcore/pluginConfigs");
        assert_eq!(wine_path(r"Z:\games\Mods", root), PathBuf::from("/games/Mods"));
        assert_eq!(wine_path(r"C:\users\x\Mods", root), PathBuf::from("/tmp/x/.xlcore/wineprefix/drive_c/users/x/Mods"));
        assert_eq!(wine_path("/already/unix", root), PathBuf::from("/already/unix"));
    }

    #[test]
    fn automation_sets() {
        let v: Value = serde_json::from_str(r#"{"Version":1,"Data":[{"Name":"Main","Identifier":{"Type":"Player","PlayerName":"Some One","HomeWorld":65535},"Enabled":true,"Designs":[{"Design":"abc","Type":31},{"Design":"//QuickSelection","Type":31}]},{"Name":"Off","Identifier":{"Type":"Player","PlayerName":"Some One"},"Enabled":false,"Designs":[{"Design":"def"}]}]}"#).unwrap();
        let sets = parse_automation(&v);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].player, "Some One");
        assert_eq!(sets[0].designs, vec!["abc".to_string()]);
        assert!(!sets[1].enabled);
        let g = Glamourer { designs: Vec::new(), automation: sets };
        assert_eq!(g.automation_for("some one").len(), 1);
        assert!(g.automation_for("nobody").is_empty());
    }

    #[test]
    fn design_flags() {
        let d = Design {
            id: "d".into(),
            name: "d".into(),
            folder: String::new(),
            customize: vec![("EyeShape".into(), 3, true), ("SmallIris".into(), 128, true), ("Highlights".into(), 128, true), ("Race".into(), 4, false)],
            equipment: vec![("Body".into(), 41581, true), ("Head".into(), 4294967164, true), ("Legs".into(), 5, false)],
            stains: vec![("Body".into(), 102, 79, true), ("Legs".into(), 5, 0, false)],
            parameters: vec![("SkinDiffuse".into(), [0.5, 0.25, 0.75, 1.0], true), ("LipDiffuse".into(), [0.0, 0.0, 0.0, 0.7], false)],
        };
        let mut s = BTreeMap::new();
        s.insert("race".to_string(), "1".to_string());
        let (c, g, _) = apply_design(&d, &mut s);
        assert_eq!((c, g), (3, 2));
        assert_eq!(s["eyes"], "131");
        assert_eq!(s["highlights"], "128");
        assert_eq!(s["race"], "1");
        assert_eq!(s["item.body"], "41581");
        assert_eq!(s["item.head"], "");
        assert!(!s.contains_key("item.legs"));
        assert_eq!(s["dye.body"], "102");
        assert_eq!(s["dye2.body"], "79");
        assert!(!s.contains_key("dye.legs"));
        assert_eq!(s["color.skin"], "0.5000 0.2500 0.7500");
        assert!(!s.contains_key("color.lip"));
        assert_eq!(parse_parameter(&s["color.skin"]), Some([0.5, 0.25, 0.75, 1.0]));
    }
}
