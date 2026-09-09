//! Asset sources: where game files come from.
//!
//! The engine never copies or modifies game files; a source reads them in place.

use std::sync::Mutex;

use physis::Platform;
use physis::resource::{Resource, SqPackResource};

/// A read-only provider of game files addressed by their in-game path
/// (e.g. `chara/human/c0801/obj/body/b0001/model/c0801b0001_top.mdl`).
///
/// Implementations must be cheap to share between threads; loaders parse
/// the returned bytes outside of any internal lock.
pub trait AssetSource: Send + Sync + 'static {
    /// Read the whole file at `path`, or `None` if it does not exist.
    fn read(&self, path: &str) -> Option<Vec<u8>>;

    /// Whether `path` exists in this source.
    fn exists(&self, path: &str) -> bool;

    /// Platform the data was built for (affects endianness of some formats).
    fn platform(&self) -> Platform {
        Platform::Win32
    }

    /// Human readable description for logs.
    fn describe(&self) -> String;

    /// Where `path` would be read from (game archive or a mod pack).
    fn origin(&self, path: &str) -> ffl_core::Provenance {
        let _ = path;
        ffl_core::Provenance::Game
    }

    /// The game's own file at `path`, ignoring every mod (the fallback when a modded file
    /// cannot be read).
    fn read_vanilla(&self, path: &str) -> Option<Vec<u8>> {
        self.read(path)
    }

    /// Metadata edits the active mods make (EQDP, IMC, EST, attributes); empty for the game.
    fn meta_overrides(&self) -> crate::mods::MetaOverrides {
        crate::mods::MetaOverrides::default()
    }
}

/// The retail game's `sqpack` archives, read in place through Physis.
pub struct SqPackSource {
    inner: Mutex<SqPackResource>,
    game_dir: String,
}

impl SqPackSource {
    /// `game_dir` is the directory that contains `sqpack/` and `ffxivgame.ver`
    /// (i.e. `<install>/game`).
    pub fn new(game_dir: &str) -> Self {
        let resource = SqPackResource::from_existing(game_dir);
        Self {
            inner: Mutex::new(resource),
            game_dir: game_dir.to_string(),
        }
    }

    /// The `<install>/game` directory this source reads from.
    pub fn game_dir(&self) -> &str {
        &self.game_dir
    }

    /// Repository names and versions (`ffxiv`, `ex1`, ...).
    pub fn repositories(&self) -> Vec<(String, Option<String>)> {
        let guard = self.inner.lock().expect("sqpack mutex poisoned");
        guard
            .repositories
            .iter()
            .map(|r| (r.name.clone(), r.version.clone()))
            .collect()
    }

    /// Run a closure with exclusive access to the underlying Physis resource.
    ///
    /// Used by the Excel layer, which needs Physis' own sheet readers.
    pub fn with_resource<T>(&self, f: impl FnOnce(&mut SqPackResource) -> T) -> T {
        let mut guard = self.inner.lock().expect("sqpack mutex poisoned");
        f(&mut guard)
    }
}

impl AssetSource for SqPackSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let mut guard = self.inner.lock().expect("sqpack mutex poisoned");
        guard.read(path)
    }

    fn exists(&self, path: &str) -> bool {
        let mut guard = self.inner.lock().expect("sqpack mutex poisoned");
        guard.exists(path)
    }

    fn platform(&self) -> Platform {
        let guard = self.inner.lock().expect("sqpack mutex poisoned");
        guard.platform()
    }

    fn describe(&self) -> String {
        format!("sqpack at {}", self.game_dir)
    }
}

/// Cache/sound key for a path: the path itself, or `path@pack` when a mod pack replaces it,
/// so a modded file never shares a cache slot with the game's file of the same path.
pub fn keyed_path(source: &dyn AssetSource, path: &str) -> String {
    match source.origin(path) {
        ffl_core::Provenance::Mod(pack) => format!("{path}@{pack}"),
        _ => path.to_string(),
    }
}
