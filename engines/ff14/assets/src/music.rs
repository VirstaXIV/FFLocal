//! Zone background music: `TerritoryType.BGM` → (`BGMSwitch` →) `BGMSituation` → `BGM.File`.
//!
//! The `BGM` value is range-tagged (verified on game 2026.08.11): `< 1000` is a `BGM` row,
//! `1000..50000` a `BGMSituation` row (`Daytime`, `Night`, `Battle`, `Daybreak`, `Twilight`,
//! each a `BGM` row) and `>= 50000` a `BGMSwitch` row whose subrows are quest-gated overrides;
//! subrow 0 is the default and may itself point at a situation. Mist (339) = 1083 →
//! Daytime 186 `music/ffxiv/BGM_Field_Housing_Day.scd`, Night 187, Battle 33.

use anyhow::{Result, anyhow};
use ffl_core::MusicSet;

use crate::excel::ExcelCache;
use crate::source::{AssetSource, keyed_path};

#[derive(Debug, Clone, PartialEq)]
pub struct BgmRow {
    pub row: u32,
    pub file: String,
    pub priority: u8,
    pub disable_restart: bool,
    pub special_mode: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainKind {
    None,
    Bgm,
    Situation,
    Switch,
}

#[derive(Debug, Clone, Default)]
pub struct MusicChain {
    pub territory: u32,
    /// Raw `TerritoryType.BGM` value.
    pub value: u64,
    pub switch_row: Option<u32>,
    /// The value the switch's default subrow resolved to.
    pub switch_value: Option<u64>,
    pub situation_row: Option<u32>,
    pub day: Option<BgmRow>,
    pub night: Option<BgmRow>,
    pub battle: Option<BgmRow>,
    pub daybreak: Option<BgmRow>,
    pub twilight: Option<BgmRow>,
}

impl MusicChain {
    pub fn kind(&self) -> ChainKind {
        if self.value == 0 {
            ChainKind::None
        } else if self.value >= 50_000 {
            ChainKind::Switch
        } else if self.value >= 1000 {
            ChainKind::Situation
        } else {
            ChainKind::Bgm
        }
    }

    pub fn rows(&self) -> impl Iterator<Item = (&'static str, &BgmRow)> {
        [("day", &self.day), ("night", &self.night), ("battle", &self.battle), ("daybreak", &self.daybreak), ("twilight", &self.twilight)]
            .into_iter()
            .filter_map(|(k, r)| r.as_ref().map(|r| (k, r)))
    }
}

/// One `BGM` row; `None` for id 0 or a row without a file.
pub fn bgm_row(excel: &ExcelCache, id: u64) -> Result<Option<BgmRow>> {
    if id == 0 {
        return Ok(None);
    }
    let sheet = excel.sheet("BGM")?;
    let row = id as u32;
    if sheet.row(row).is_none() {
        return Err(anyhow!("BGM has no row {row}"));
    }
    let file = sheet.string(row, "File")?;
    // Row 1 is `music/ffxiv/BGM_Null.scd`: the game's explicit silence.
    if file.is_empty() || file.ends_with("BGM_Null.scd") {
        return Ok(None);
    }
    Ok(Some(BgmRow {
        row,
        file,
        priority: sheet.integer(row, "Priority").unwrap_or(0) as u8,
        disable_restart: sheet.integer(row, "DisableRestart").unwrap_or(0) != 0,
        special_mode: sheet.integer(row, "SpecialMode").unwrap_or(0) as u8,
    }))
}

/// Resolve the whole chain for a territory.
pub fn resolve_chain(excel: &ExcelCache, territory: u32) -> Result<MusicChain> {
    let tt = excel.sheet("TerritoryType")?;
    let value = tt.integer(territory, "BGM")?;
    resolve_value(excel, territory, value)
}

/// Resolve a `TerritoryType.BGM`-style value (also carried by `MapRange` layer objects).
pub fn resolve_value(excel: &ExcelCache, territory: u32, value: u64) -> Result<MusicChain> {
    let mut chain = MusicChain { territory, value, ..Default::default() };
    let mut v = value;
    if v >= 50_000 {
        let switch = excel.sheet("BGMSwitch")?;
        chain.switch_row = Some(v as u32);
        // Physis returns subrow 0 for a subrow sheet: the default entry (no quest condition).
        v = switch.integer(v as u32, "BGM")?;
        chain.switch_value = Some(v);
    }
    if v >= 1000 && v < 50_000 {
        let situation = excel.sheet("BGMSituation")?;
        let row = v as u32;
        chain.situation_row = Some(row);
        chain.day = bgm_row(excel, situation.integer(row, "DaytimeID")?)?;
        chain.night = bgm_row(excel, situation.integer(row, "NightID")?)?;
        chain.battle = bgm_row(excel, situation.integer(row, "BattleID")?)?;
        chain.daybreak = bgm_row(excel, situation.integer(row, "DaybreakID")?)?;
        chain.twilight = bgm_row(excel, situation.integer(row, "TwilightID")?)?;
    } else if v > 0 {
        chain.day = bgm_row(excel, v)?;
    }
    Ok(chain)
}

/// Sound key of an SCD entry.
pub fn scd_key(source: &dyn AssetSource, path: &str, entry: usize) -> String {
    format!("ff14:scd:{}#{entry}", keyed_path(source, path))
}

/// The music set of a territory (`None` when it plays nothing).
pub fn music_set(excel: &ExcelCache, source: &dyn AssetSource, territory: u32) -> Result<Option<MusicSet>> {
    let chain = resolve_chain(excel, territory)?;
    chain_to_set(&chain, source)
}

/// The music set of a raw BGM value (sub-area music).
pub fn music_set_for_value(excel: &ExcelCache, source: &dyn AssetSource, territory: u32, value: u64) -> Result<Option<MusicSet>> {
    let chain = resolve_value(excel, territory, value)?;
    chain_to_set(&chain, source)
}

fn chain_to_set(chain: &MusicChain, source: &dyn AssetSource) -> Result<Option<MusicSet>> {
    let mut set = MusicSet { fade_in: 1.5, fade_out: 2.0, restart_on_return: true, ..Default::default() };
    let mut key_of = |row: &Option<BgmRow>| -> Option<String> {
        let r = row.as_ref()?;
        if source.exists(&r.file) {
            Some(scd_key(source, &r.file, 0))
        } else {
            set.notes.push(format!("BGM {} file missing: {}", r.row, r.file));
            None
        }
    };
    set.day = key_of(&chain.day);
    set.night = key_of(&chain.night);
    if chain.kind() == ChainKind::Bgm {
        // A plain BGM row plays regardless of the time of day.
        set.night = set.day.clone();
    }
    set.battle = key_of(&chain.battle);
    set.daybreak = key_of(&chain.daybreak);
    if let Some(k) = key_of(&chain.twilight) {
        set.extra.push(("twilight".into(), k));
    }
    if let Some(r) = chain.rows().next().map(|(_, r)| r) {
        set.restart_on_return = !r.disable_restart;
    }
    set.notes.insert(0, format!("TerritoryType {} BGM {} ({:?})", chain.territory, chain.value, chain.kind()));
    Ok(if set.is_empty() { None } else { Some(set) })
}
