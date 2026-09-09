//! Character voice: the customize `voice` slot and the emote voice files.
//!
//! Emote paps carry a `C053` voice-line entry (see `meta::tmb`); the client turns its sound id
//! into `sound/voice/vo_emote/%04d%03d.scd` (template in the executable). 5520 such files exist:
//! groups 7301..7548 with 16 or 22 files each plus 173xx..175xx blocks. `7300 + line` matches the
//! observed lines (laugh 47 → 7347, joy 52 → 7352). The `%03d` part is a voice *bank* (0..15 or
//! 0..21); how the client maps a character's voice id (1..168, `CharaMakeType.Voice`) to that
//! bank is not known. PROVISIONAL rule: the bank is the index of the voice id within the
//! character's 12 selectable voices. `FFL_VOICE_BANK=<n>` overrides it for listening tests.

use ffl_ff14_assets::ExcelCache;
use ffl_ff14_assets::excel::field_as_u64;
use physis::race::{Gender, Tribe};

/// Preset key holding the customize voice id.
pub const VOICE_KEY: &str = "voice";

/// Number of selectable voices per race/tribe/gender.
pub const VOICES_PER_ROW: usize = 12;

/// Row byte offset of `CharaMakeType.Voice[0]` (game 2026.08.11; the 12 voices follow as u8).
const VOICE_COLUMN_OFFSET: u16 = 12656;

pub fn emote_voice_path(line: u32, bank: u16) -> String {
    format!("sound/voice/vo_emote/{:04}{:03}.scd", 7300 + line, bank)
}

/// `CharaMakeType` row of a tribe/gender: rows are tribe-major (32 rows = 16 tribes × 2 genders).
pub fn chara_make_row(tribe: Tribe, gender: &Gender) -> u32 {
    let tribe = tribe as u32;
    let gender = match gender {
        Gender::Male => 0,
        Gender::Female => 1,
    };
    (tribe.saturating_sub(1)) * 2 + gender
}

/// The 12 voice ids a character of this tribe/gender can pick.
pub fn chara_make_voices(excel: &ExcelCache, tribe: Tribe, gender: &Gender) -> Option<[u8; VOICES_PER_ROW]> {
    let sheet = excel.sheet("CharaMakeType").ok()?;
    let row = sheet.row(chara_make_row(tribe, gender))?;
    let first = sheet.sheet.exh.column_definitions.iter().position(|c| c.offset == VOICE_COLUMN_OFFSET)?;
    let mut out = [0u8; VOICES_PER_ROW];
    for (i, slot) in out.iter_mut().enumerate() {
        let col = sheet.sheet.exh.column_definitions.get(first + i)?;
        if col.offset != VOICE_COLUMN_OFFSET + i as u16 {
            return None;
        }
        *slot = field_as_u64(row.columns.get(first + i)?)? as u8;
    }
    Some(out)
}

/// Voice bank of a voice id (see the module docs). Unknown ids map to bank 0.
pub fn voice_bank(voice_id: u8, row_voices: &[u8; VOICES_PER_ROW]) -> u16 {
    if let Some(b) = voice_bank_override() {
        return b;
    }
    row_voices.iter().position(|v| *v == voice_id).unwrap_or(0) as u16
}

pub fn voice_bank_override() -> Option<u16> {
    std::env::var("FFL_VOICE_BANK").ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_tribe_major() {
        assert_eq!(chara_make_row(Tribe::Midlander, &Gender::Male), 0);
        assert_eq!(chara_make_row(Tribe::Midlander, &Gender::Female), 1);
        assert_eq!(chara_make_row(Tribe::Highlander, &Gender::Male), 2);
        assert_eq!(chara_make_row(Tribe::Keeper, &Gender::Female), 15);
    }

    #[test]
    fn paths() {
        assert_eq!(emote_voice_path(47, 5), "sound/voice/vo_emote/7347005.scd");
    }
}
