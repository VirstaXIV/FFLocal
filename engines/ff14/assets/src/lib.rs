//! FFLocal asset layer: reads FFXIV game data in place and turns it into plain data
//! structures. This crate has no engine dependency.

pub mod adpcm;
pub mod dalamud;
pub mod excel;
pub mod loaders;
pub mod mods;
pub mod music;
pub mod locate;
pub mod schema_data;
pub mod scd;
pub mod source;
pub mod zone;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

pub use excel::{ExcelCache, LoadedSheet, SchemaMap};
pub use locate::{GameInstall, GamePathOrigin, locate_game};
pub use source::{AssetSource, SqPackSource};

/// Everything needed to read one installed game: its location, a sqpack source and an Excel cache.
pub struct GameData {
    pub install: GameInstall,
    pub source: Arc<SqPackSource>,
    pub excel: ExcelCache,
}

impl GameData {
    /// Locate the game (see [`locate_game`]) and open it.
    pub fn open(explicit: Option<&Path>) -> Result<Self> {
        let install = locate_game(explicit)?;
        let game_dir = install.game_dir.to_string_lossy().to_string();
        tracing::info!(
            "using FFXIV {} at {} ({:?})",
            install.version,
            game_dir,
            install.origin
        );
        let source = Arc::new(SqPackSource::new(&game_dir));
        let excel = ExcelCache::new(source.clone());
        Ok(Self {
            install,
            source,
            excel,
        })
    }

    pub fn source(&self) -> &dyn AssetSource {
        self.source.as_ref()
    }
}
