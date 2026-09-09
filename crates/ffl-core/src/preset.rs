//! Character presets and the on-disk library.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CharacterPreset {
    pub id: String,
    pub name: String,
    pub engine: String,
    /// Engine-defined settings (see [`crate::Engine::preset_fields`]).
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
}

impl CharacterPreset {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.settings.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub presets: Vec<CharacterPreset>,
    /// Last used engine/map/preset for the launcher.
    #[serde(default)]
    pub last_map: Option<(String, String)>,
    #[serde(default)]
    pub last_preset: Option<String>,
    /// Recently entered worlds, newest first (engine, map id).
    #[serde(default)]
    pub recent_maps: Vec<(String, String)>,
}

impl Library {
    /// `<data dir>/fflocal/presets.toml`: `XDG_DATA_HOME` when set (on every OS, so tests can
    /// redirect it), else the platform data directory (`~/.local/share`, `%APPDATA%`,
    /// `~/Library/Application Support`).
    pub fn path() -> PathBuf {
        Self::data_dir().join("presets.toml")
    }

    /// The directory holding presets, settings and mod packs.
    pub fn data_dir() -> PathBuf {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::data_dir)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("fflocal")
    }

    pub fn load() -> Library {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!("ignoring {}: {err}", path.display());
                Library::default()
            }),
            Err(_) => Library::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn upsert(&mut self, preset: CharacterPreset) {
        if let Some(existing) = self.presets.iter_mut().find(|p| p.id == preset.id) {
            *existing = preset;
        } else {
            self.presets.push(preset);
        }
    }

    /// Record a world as the most recently entered one.
    pub fn remember_map(&mut self, engine: &str, map: &str) {
        let key = (engine.to_string(), map.to_string());
        self.recent_maps.retain(|m| *m != key);
        self.recent_maps.insert(0, key.clone());
        self.recent_maps.truncate(8);
        self.last_map = Some(key);
    }

    pub fn new_id() -> String {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("p{t:x}")
    }
}
