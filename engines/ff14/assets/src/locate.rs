//! Finding the installed game.
//!
//! An explicit path (CLI flag, the hub's Games window) is used as given; otherwise the
//! candidates in order: `FFLOCAL_GAME_PATH` → `config.toml` → XIVLauncher (Linux/macOS
//! `launcher.ini`, Windows `launcherConfigV3.json`) → Steam libraries → the platform's
//! default install folders → Physis' own search.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Contents of `config.toml` (all keys optional).
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// The FFXIV install root (the directory containing `game/` and `boot/`),
    /// or the `game/` directory itself.
    pub game_path: Option<String>,
    /// Override for the XIVLauncher config directory holding `FFXIV_CHARA_*.dat`
    /// and `FFXIV_CHR*/`.
    pub ffxiv_config_dir: Option<String>,
    /// Directory of FFXIV mod packs (default `~/.local/share/fflocal/mods/ff14`).
    pub ff14_mods: Option<String>,
    /// Override for the Dalamud plugin-configuration directory (`pluginConfigs`).
    pub dalamud_path: Option<String>,
}

/// Where a game path came from, for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GamePathOrigin {
    Explicit,
    Environment,
    ConfigFile(PathBuf),
    XivLauncherIni(PathBuf),
    PhysisSearch,
}

/// A located, validated game installation.
#[derive(Debug, Clone)]
pub struct GameInstall {
    /// `<install>/game`: contains `sqpack/` and `ffxivgame.ver`.
    pub game_dir: PathBuf,
    /// Contents of `ffxivgame.ver`, trimmed.
    pub version: String,
    pub origin: GamePathOrigin,
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Steam library folders of this machine (from every `libraryfolders.vdf` we know), plus
/// the Steam root itself.
pub fn steam_libraries() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(h) = home() {
        roots.push(h.join(".steam/steam"));
        roots.push(h.join(".local/share/Steam"));
        roots.push(h.join("Library/Application Support/Steam"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles")) {
        roots.push(PathBuf::from(pf).join("Steam"));
    }
    roots.push(PathBuf::from("C:\\Program Files (x86)\\Steam"));
    let mut out = Vec::new();
    for root in roots {
        let vdf = root.join("steamapps/libraryfolders.vdf");
        let Ok(text) = std::fs::read_to_string(&vdf) else {
            continue;
        };
        if !out.contains(&root) {
            out.push(root.clone());
        }
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("\"path\"") {
                let lib = rest.trim().trim_matches('"').replace("\\\\", "\\");
                let p = PathBuf::from(lib);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Every place the game might be, in precedence order, validated or not.
pub fn candidates() -> Vec<ffl_core::InstallCandidate> {
    let c = |path: PathBuf, source: &str| ffl_core::InstallCandidate { path, source: source.to_string() };
    let mut out = Vec::new();
    if let Ok(env) = std::env::var("FFLOCAL_GAME_PATH")
        && !env.is_empty()
    {
        out.push(c(PathBuf::from(env), "FFLOCAL_GAME_PATH"));
    }
    let (config, config_path) = load_config();
    if let (Some(p), Some(cp)) = (config.game_path.as_ref(), config_path) {
        out.push(c(PathBuf::from(p), &format!("{}", cp.display())));
    }
    if let Some((p, ini)) = xivlauncher_game_path() {
        out.push(c(p, &format!("XIVLauncher ({})", ini.display())));
    }
    for lib in steam_libraries() {
        out.push(c(lib.join("steamapps/common/FINAL FANTASY XIV Online"), "Steam library"));
        out.push(c(lib.join("steamapps/common/FINAL FANTASY XIV - A Realm Reborn"), "Steam library"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles")) {
        out.push(c(PathBuf::from(pf).join("SquareEnix/FINAL FANTASY XIV - A Realm Reborn"), "default install folder"));
    }
    out.push(c(PathBuf::from("C:\\Program Files (x86)\\SquareEnix\\FINAL FANTASY XIV - A Realm Reborn"), "default install folder"));
    if let Some(h) = home() {
        out.push(c(h.join("Library/Application Support/XIV on Mac/ffxiv"), "XIV on Mac"));
        out.push(c(h.join(".xlcore/ffxiv"), "XIVLauncher.Core default"));
    }
    for dir in physis::existing_dirs::find_existing_game_dirs() {
        out.push(c(PathBuf::from(dir.path), "Physis search"));
    }
    out
}

/// The candidates that really hold the game (`game/ffxivgame.ver` + `sqpack/`), duplicates
/// dropped: what the hub's "Scan" shows.
pub fn scan() -> Vec<ffl_core::InstallCandidate> {
    let mut out: Vec<ffl_core::InstallCandidate> = Vec::new();
    for c in candidates() {
        let Some(game) = normalize_game_dir(&c.path) else {
            continue;
        };
        if !game.join("sqpack").is_dir() || out.iter().any(|o| o.path == game) {
            continue;
        }
        out.push(ffl_core::InstallCandidate { path: game, source: c.source });
    }
    out
}

/// Load `config.toml` from the working directory or next to the executable, if present.
pub fn load_config() -> (Config, Option<PathBuf>) {
    let mut candidates = vec![PathBuf::from("config.toml")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("config.toml"));
    }
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            match toml::from_str::<Config>(&text) {
                Ok(cfg) => return (cfg, Some(path)),
                Err(err) => tracing::warn!("ignoring {}: {err}", path.display()),
            }
        }
    }
    (Config::default(), None)
}

/// Accepts either the install root or its `game/` directory and returns `game/`.
fn normalize_game_dir(path: &Path) -> Option<PathBuf> {
    if path.join("ffxivgame.ver").is_file() {
        return Some(path.to_path_buf());
    }
    let game = path.join("game");
    if game.join("ffxivgame.ver").is_file() {
        return Some(game);
    }
    None
}

/// The game path XIVLauncher knows: `GamePath=` in XIVLauncher.Core's `launcher.ini`
/// (Linux/macOS), `"GamePath": "..."` in the Windows launcher's `launcherConfigV3.json`.
pub fn xivlauncher_game_path() -> Option<(PathBuf, PathBuf)> {
    let home = home()?;
    let mut candidates = vec![
        home.join(".xlcore/launcher.ini"),
        home.join(".local/share/dev.goats.xivlauncher/launcher.ini"),
        home.join("Library/Application Support/XIV on Mac/launcher.ini"),
    ];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("XIVLauncher/launcherConfigV3.json"));
    }
    for ini in candidates {
        let Ok(text) = std::fs::read_to_string(&ini) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            let value = if let Some(v) = line.strip_prefix("GamePath=") {
                v.trim().to_string()
            } else if let Some(rest) = line.strip_prefix("\"GamePath\"") {
                rest.trim_start_matches([':', ' ']).trim_end_matches(',').trim_matches('"').replace("\\\\", "\\")
            } else {
                continue;
            };
            if !value.is_empty() {
                return Some((PathBuf::from(value), ini));
            }
        }
    }
    None
}

/// The XIVLauncher.Core FFXIV config directory (`My Games/FINAL FANTASY XIV - A Realm Reborn` equivalent).
pub fn xivlauncher_config_dir() -> Option<PathBuf> {
    let home = home()?;
    let candidates = [
        home.join(".xlcore/ffxivConfig"),
        home.join(".local/share/dev.goats.xivlauncher/ffxivConfig"),
    ];
    candidates.into_iter().find(|p| p.is_dir())
}

/// Locate the game. An explicit path (CLI, the hub) must be the game itself; otherwise the
/// first candidate that holds it wins.
pub fn locate_game(explicit: Option<&Path>) -> Result<GameInstall> {
    let attempts: Vec<(PathBuf, GamePathOrigin)> = match explicit {
        Some(p) => vec![(p.to_path_buf(), GamePathOrigin::Explicit)],
        None => candidates()
            .into_iter()
            .map(|c| {
                let origin = match c.source.as_str() {
                    "FFLOCAL_GAME_PATH" => GamePathOrigin::Environment,
                    "Physis search" => GamePathOrigin::PhysisSearch,
                    s if s.starts_with("XIVLauncher") => GamePathOrigin::XivLauncherIni(PathBuf::from(s.trim_start_matches("XIVLauncher (").trim_end_matches(')'))),
                    s if s.ends_with("config.toml") => GamePathOrigin::ConfigFile(PathBuf::from(s)),
                    _ => GamePathOrigin::PhysisSearch,
                };
                (c.path, origin)
            })
            .collect(),
    };
    let tried = attempts.len();

    for (path, origin) in &attempts {
        if let Some(game_dir) = normalize_game_dir(path) {
            let version = physis::read_version(&game_dir.join("ffxivgame.ver"))
                .context("reading ffxivgame.ver")?
                .trim()
                .to_string();
            if !game_dir.join("sqpack").is_dir() {
                tracing::warn!("{} has no sqpack/ directory, skipping", game_dir.display());
                continue;
            }
            return Ok(GameInstall {
                game_dir,
                version,
                origin: origin.clone(),
            });
        }
        tracing::debug!("candidate {} ({origin:?}) is not a game directory", path.display());
    }

    match explicit {
        Some(p) => bail!("{} is not an FFXIV installation (no game/ffxivgame.ver and sqpack/)", p.display()),
        None => bail!(
            "could not find an FFXIV installation; pick it in Games, pass --game-path, set FFLOCAL_GAME_PATH, \
             or add game_path to config.toml (tried {tried} candidates)"
        ),
    }
}
