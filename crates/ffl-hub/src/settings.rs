//! User settings persisted next to the preset library (`settings.toml`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use ffl_core::SoundCategory;
use serde::{Deserialize, Serialize};

/// Graphics options every runtime is expected to honour (a runtime may ignore what it cannot
/// do; the hub only stores and edits them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    /// Borderless fullscreen on the current monitor instead of a window.
    pub fullscreen: bool,
    pub vsync: bool,
    /// Multisample count: 1, 2, 4 or 8.
    pub msaa: u8,
    pub shadows: bool,
    /// Shadow map size per cascade (1024, 2048, 4096).
    pub shadow_resolution: u32,
    /// Nothing renders beyond this distance (metres).
    pub render_distance: f32,
    /// Small-object culling: an object stays visible up to `bounding radius × this` metres.
    pub detail_distance: f32,
    /// Multiplier on the games' own level-of-detail switch distances (1 = as the game data
    /// says; FFXIV swaps a rock to its coarse mesh at 64 m, which reads as popping).
    #[serde(default = "default_lod_distance")]
    pub lod_distance: f32,
    /// Anisotropic filtering samples (1 = off, up to 16).
    pub anisotropy: u8,
    /// UI zoom factor.
    pub ui_scale: f32,
}

fn default_lod_distance() -> f32 {
    2.5
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            fullscreen: false,
            vsync: true,
            msaa: 4,
            shadows: true,
            shadow_resolution: 2048,
            render_distance: 1500.0,
            detail_distance: 150.0,
            lod_distance: default_lod_distance(),
            anisotropy: 8,
            ui_scale: 1.0,
        }
    }
}

impl GraphicsSettings {
    /// Clamp to values a runtime can use.
    pub fn sanitized(mut self) -> Self {
        self.msaa = match self.msaa {
            0 | 1 => 1,
            2 => 2,
            3..=5 => 4,
            _ => 8,
        };
        self.shadow_resolution = match self.shadow_resolution {
            0..=1024 => 1024,
            1025..=2048 => 2048,
            _ => 4096,
        };
        self.render_distance = self.render_distance.clamp(100.0, 10_000.0);
        self.detail_distance = self.detail_distance.clamp(20.0, 2000.0);
        self.lod_distance = if self.lod_distance.is_finite() { self.lod_distance.clamp(0.5, 8.0) } else { default_lod_distance() };
        self.anisotropy = self.anisotropy.clamp(1, 16);
        self.ui_scale = if self.ui_scale.is_finite() { self.ui_scale.clamp(0.5, 3.0) } else { 1.0 };
        self
    }
}

/// Mixer volumes per [`SoundCategory`] plus the two switches every runtime honours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Applied on top of every category (0..=1).
    pub master: f32,
    pub music: f32,
    pub ambience: f32,
    pub footsteps: f32,
    pub voice: f32,
    pub effects: f32,
    /// Everything silent (the `M` hotkey); the volumes are kept.
    pub mute: bool,
    /// False pauses the background music instead of silencing it.
    pub music_enabled: bool,
    /// Show the in-world list of playing sounds (top right).
    pub show_monitor: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            master: 1.0,
            music: 1.0,
            ambience: 1.0,
            footsteps: 1.0,
            voice: 1.0,
            effects: 1.0,
            mute: false,
            music_enabled: true,
            show_monitor: false,
        }
    }
}

impl AudioSettings {
    fn clamp01(v: f32) -> f32 {
        if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 }
    }

    /// Clamp every volume into 0..=1.
    pub fn sanitized(mut self) -> Self {
        self.master = Self::clamp01(self.master);
        self.music = Self::clamp01(self.music);
        self.ambience = Self::clamp01(self.ambience);
        self.footsteps = Self::clamp01(self.footsteps);
        self.voice = Self::clamp01(self.voice);
        self.effects = Self::clamp01(self.effects);
        self
    }

    /// The category volume (without master or mute).
    pub fn volume(&self, category: SoundCategory) -> f32 {
        match category {
            SoundCategory::Music => self.music,
            SoundCategory::Ambience => self.ambience,
            SoundCategory::Footsteps => self.footsteps,
            SoundCategory::Voice => self.voice,
            SoundCategory::Effects => self.effects,
        }
    }

    /// Mutable access to a category volume (for the settings sliders).
    pub fn volume_mut(&mut self, category: SoundCategory) -> &mut f32 {
        match category {
            SoundCategory::Music => &mut self.music,
            SoundCategory::Ambience => &mut self.ambience,
            SoundCategory::Footsteps => &mut self.footsteps,
            SoundCategory::Voice => &mut self.voice,
            SoundCategory::Effects => &mut self.effects,
        }
    }

    /// The linear gain a sound of `category` plays at: category × master, 0 while muted.
    pub fn gain(&self, category: SoundCategory) -> f32 {
        if self.mute { 0.0 } else { self.volume(category) * self.master }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub graphics: GraphicsSettings,
    pub audio: AudioSettings,
    pub ui: UiSettings,
    /// Globally enabled mod packs per engine id.
    pub mods: std::collections::BTreeMap<String, Vec<String>>,
    /// Game installs per engine id (the Games window); an empty path means auto-detect.
    pub games: std::collections::BTreeMap<String, GameSettings>,
}

/// Where an engine's game is installed, as chosen in the Games window or the launcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    /// Install directory; empty = scan the usual places.
    pub path: String,
    /// False = the game is not opened at all: its characters, worlds, catalogs and sounds are
    /// off, as if it were not installed. Changed in the hub or the launcher (never in a world).
    pub enabled: bool,
    /// Companion tooling directory (FFXIV: XIVLauncher's `pluginConfigs` with the Penumbra and
    /// Glamourer data); empty = search the usual places.
    pub plugins_path: String,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self { path: String::new(), enabled: true, plugins_path: String::new() }
    }
}

impl Settings {
    /// The saved install path of an engine, `None` when empty (= auto-detect).
    pub fn game_path(&self, engine: &str) -> Option<PathBuf> {
        self.games.get(engine).map(|g| g.path.trim()).filter(|p| !p.is_empty()).map(PathBuf::from)
    }

    /// Whether the engine's game is enabled (games without an entry are).
    pub fn game_enabled(&self, engine: &str) -> bool {
        self.games.get(engine).is_none_or(|g| g.enabled)
    }

    /// The saved plugin-data path of an engine, `None` when empty (= auto-detect).
    pub fn plugins_path(&self, engine: &str) -> Option<PathBuf> {
        self.games.get(engine).map(|g| g.plugins_path.trim()).filter(|p| !p.is_empty()).map(PathBuf::from)
    }
}

/// Which panels are shown (the "UI" menu in the hub and in a world).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// In-world diagnostics window (load state, clip, player, camera).
    pub diagnostics: bool,
    /// In-world actions panel (weapons, stances, emotes).
    pub actions: bool,
    /// Hub: the characters panel on the left.
    pub hub_characters: bool,
    /// Hub: the worlds panel on the right (the editor shows regardless).
    pub hub_worlds: bool,
    /// Hub: the hint under the preview.
    pub hub_hint: bool,
    /// In-world key hints line.
    pub key_hints: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            diagnostics: false,
            actions: true,
            hub_characters: true,
            hub_worlds: true,
            hub_hint: true,
            key_hints: true,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        ffl_core::Library::path().with_file_name("settings.toml")
    }

    pub fn load() -> Settings {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(s) => Settings {
                    graphics: s.graphics.sanitized(),
                    audio: s.audio.sanitized(),
                    ui: s.ui,
                    mods: s.mods,
                    games: s.games,
                },
                Err(err) => {
                    tracing::warn!("ignoring {}: {err}", path.display());
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_round_trips_through_toml() {
        let mut s = Settings::default();
        s.audio.music = 0.25;
        s.audio.mute = true;
        let text = toml::to_string_pretty(&s).unwrap();
        assert!(text.contains("[audio]"), "{text}");
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.audio, s.audio);
        // Old files without the section load with the defaults.
        let old: Settings = toml::from_str("[graphics]\nmsaa = 2\n").unwrap();
        assert_eq!(old.audio, AudioSettings::default());
        assert_eq!(old.graphics.msaa, 2);
    }

    #[test]
    fn games_default_to_enabled() {
        let s: Settings = toml::from_str("[games.ff11]\npath = \"/games/ffxi\"\n").unwrap();
        assert!(s.game_enabled("ff11"));
        assert!(s.game_enabled("ff14"));
        assert_eq!(s.game_path("ff11").unwrap().to_str(), Some("/games/ffxi"));
        assert_eq!(s.game_path("ff14"), None);
        let off: Settings = toml::from_str("[games.ff11]\nenabled = false\n").unwrap();
        assert!(!off.game_enabled("ff11"));
    }

    #[test]
    fn audio_sanitized_and_gains() {
        let a = AudioSettings { master: 2.0, music: -1.0, voice: f32::NAN, ..Default::default() }.sanitized();
        assert_eq!((a.master, a.music, a.voice), (1.0, 0.0, 1.0));
        assert_eq!(a.gain(SoundCategory::Ambience), 1.0);
        let muted = AudioSettings { mute: true, ..Default::default() };
        assert_eq!(muted.gain(SoundCategory::Music), 0.0);
    }
}
