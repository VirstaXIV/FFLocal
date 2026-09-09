//! Typed loaders: read bytes from an [`AssetSource`] and parse them with Physis.

use anyhow::{Context, Result, anyhow};
use physis::ReadableFile;
use physis::lgb::Lgb;
use physis::lvb::Lvb;
use physis::model::MDL;
use physis::mtrl::Material;
use physis::pap::Pap;
use physis::pbd::PreBoneDeformer;
use physis::pcb::Pcb;
use physis::sgb::Sgb;
use physis::skeleton::Skeleton;
use physis::tera::Terrain;
use physis::tex::Texture;

use crate::source::AssetSource;

/// Run a Physis parser without letting one of its `panic!`s (unimplemented Havok versions,
/// vertex formats mod tools write) take the program down: a panic is a parse failure.
pub fn guarded<T>(what: &str, parse: impl FnOnce() -> Option<T>) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(parse)) {
        Ok(v) => v,
        Err(payload) => {
            let msg = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            tracing::warn!("{what}: parser panicked: {}", msg.lines().next().unwrap_or(""));
            None
        }
    }
}

/// Read and parse any Physis [`ReadableFile`] from a source. A modded file the parser
/// cannot read (mod tools write formats retail never does) falls back to the game's own
/// file, with a warning, so one bad mod file never blocks a character.
pub fn load<F: ReadableFile>(source: &dyn AssetSource, path: &str) -> Result<F> {
    let bytes = source
        .read(path)
        .ok_or_else(|| anyhow!("file not found: {path}"))?;
    if let Some(v) = guarded(path, || F::from_existing(source.platform(), &bytes)) {
        return Ok(v);
    }
    if !matches!(source.origin(path), ffl_core::Provenance::Game)
        && let Some(vanilla) = source.read_vanilla(path)
        && let Some(v) = guarded(path, || F::from_existing(source.platform(), &vanilla))
    {
        tracing::warn!("{path}: the modded file could not be read; using the game's ({} bytes)", vanilla.len());
        return Ok(v);
    }
    Err(anyhow!("failed to parse {path} ({} bytes)", bytes.len()))
}

pub fn load_mdl(source: &dyn AssetSource, path: &str) -> Result<MDL> {
    load(source, path).context("loading model")
}

pub fn load_mtrl(source: &dyn AssetSource, path: &str) -> Result<Material> {
    load(source, path).context("loading material")
}

pub fn load_tex(source: &dyn AssetSource, path: &str) -> Result<Texture> {
    load(source, path).context("loading texture")
}

pub fn load_lgb(source: &dyn AssetSource, path: &str) -> Result<Lgb> {
    load(source, path).context("loading layer group")
}

pub fn load_lvb(source: &dyn AssetSource, path: &str) -> Result<Lvb> {
    load(source, path).context("loading level")
}

pub fn load_sgb(source: &dyn AssetSource, path: &str) -> Result<Sgb> {
    load(source, path).context("loading shared group")
}

pub fn load_tera(source: &dyn AssetSource, path: &str) -> Result<Terrain> {
    load(source, path).context("loading terrain")
}

pub fn load_pcb(source: &dyn AssetSource, path: &str) -> Result<Pcb> {
    load(source, path).context("loading collision")
}

pub fn load_sklb(source: &dyn AssetSource, path: &str) -> Result<Skeleton> {
    load(source, path).context("loading skeleton")
}

pub fn load_pbd(source: &dyn AssetSource, path: &str) -> Result<PreBoneDeformer> {
    load(source, path).context("loading bone deformer")
}

pub fn load_pap(source: &dyn AssetSource, path: &str) -> Result<Pap> {
    load(source, path).context("loading animation")
}

/// Parse a sound container (`.scd`); see [`crate::scd`].
pub fn load_scd(source: &dyn AssetSource, path: &str) -> Result<crate::scd::ScdFile> {
    let bytes = source.read(path).ok_or_else(|| anyhow!("file not found: {path}"))?;
    crate::scd::ScdFile::parse(&bytes).map_err(|e| anyhow!("{path}: {e}"))
}
