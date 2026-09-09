//! `chara/xls/attachOffset/c{race}.atch`: where weapons and props attach, per attach type
//! (a 3-letter code such as `2sw`, `stf`, `sld`) and state. Layout (Penumbra `AtchFile.cs`):
//! `u16 points, u16 entries_per_point`, `points × u32 type` (ASCII code packed big-endian),
//! a 32-byte bit field (accessory flags), then for every point `entries × { u32 absolute
//! offset of the bone name, f32 scale, f32 offset xyz, f32 rotation xyz }` and a string pool.
//! Entry 0 is the in-hand placement, entry 1 the sheathed one (verified on c0801: `2sw`
//! → `n_buki_r` then `j_buki_sebo_r` with a back offset; `stf` → `n_buki_r` then
//! `j_buki_kosi_l`).

use std::collections::HashMap;

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub struct AtchEntry {
    pub bone: String,
    pub scale: f32,
    pub offset: [f32; 3],
    /// Euler angles in radians.
    pub rotation: [f32; 3],
}

#[derive(Debug, Clone, Default)]
pub struct AtchFile {
    /// Attach type code → entries (index 0 in hand, 1 sheathed, ...).
    pub points: HashMap<String, Vec<AtchEntry>>,
}

impl AtchFile {
    pub fn parse(d: &[u8]) -> Result<Self> {
        let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
        let f32_at = |o: usize| f32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
        if d.len() < 4 {
            bail!("atch too short");
        }
        let n_points = u16_at(0) as usize;
        let n_entries = u16_at(2) as usize;
        let mut off = 4;
        if d.len() < off + n_points * 4 + 32 + n_points * n_entries * 32 {
            bail!("atch truncated ({} points × {} entries, {} bytes)", n_points, n_entries, d.len());
        }
        let mut types = Vec::with_capacity(n_points);
        for _ in 0..n_points {
            let v = u32_at(off);
            off += 4;
            let code: String = [(v >> 16) as u8, (v >> 8) as u8, v as u8].iter().map(|b| *b as char).collect();
            types.push(code);
        }
        off += 32;
        let cstr = |o: usize| -> String {
            let end = d[o..].iter().position(|b| *b == 0).map(|e| o + e).unwrap_or(d.len());
            String::from_utf8_lossy(&d[o..end]).to_string()
        };
        let mut points = HashMap::new();
        for code in types {
            let mut entries = Vec::with_capacity(n_entries);
            for _ in 0..n_entries {
                let name_off = u32_at(off) as usize;
                let bone = if name_off < d.len() { cstr(name_off) } else { String::new() };
                entries.push(AtchEntry {
                    bone,
                    scale: f32_at(off + 4),
                    offset: [f32_at(off + 8), f32_at(off + 12), f32_at(off + 16)],
                    rotation: [f32_at(off + 20), f32_at(off + 24), f32_at(off + 28)],
                });
                off += 32;
            }
            points.insert(code, entries);
        }
        Ok(Self { points })
    }

    pub fn entry(&self, code: &str, index: usize) -> Option<&AtchEntry> {
        self.points.get(code)?.get(index)
    }
}

/// Attach type of a main-hand weapon by its `ItemUICategory` row.
pub fn main_hand_type(ui_category: u32) -> Option<&'static str> {
    Some(match ui_category {
        1 => "clw",  // Pugilist's Arm
        2 => "swd",  // Gladiator's Arm
        3 => "2ax",  // Marauder's Arm
        4 => "2bw",  // Archer's Arm
        5 => "2sp",  // Lancer's Arm
        6 | 8 => "rod", // one-handed Thaumaturge / Conjurer (hip)
        7 | 9 => "2st", // two-handed Thaumaturge / Conjurer (greatstaff: on the back)
        10 | 98 => "2bk", // Arcanist's Grimoire, Scholar's Arm
        84 => "dgr",  // Rogue's Arm
        87 => "2sw",  // Dark Knight's Arm
        88 => "2gn",  // Machinist's Arm
        89 => "2gl",  // Astrologian's Arm
        96 => "2kt",  // Samurai's Arm
        97 => "2rp",  // Red Mage's Arm
        105 => "rod", // Blue Mage's Arm
        106 => "2gb", // Gunbreaker's Arm
        107 => "chk", // Dancer's Arm
        108 => "2km", // Reaper's Arm
        109 => "2ff", // Sage's Arm
        110 => "bld", // Viper's Arm (main)
        111 => "brs", // Pictomancer's Arm
        _ => return None,
    })
}

/// Attach type of an off-hand item (shield, Viper's second blade, ...).
pub fn off_hand_type(ui_category: u32) -> Option<&'static str> {
    Some(match ui_category {
        11 => "sld",
        110 => "bl2",
        _ => return None,
    })
}

/// Attach type of a two-handed weapon's secondary model (quiver, cardholder, ...).
pub fn sub_model_type(main: &str) -> Option<&'static str> {
    Some(match main {
        "2bw" => "qvr",
        "2gl" => "crd",
        "2gn" => "bag",
        "2kt" => "ksh",
        _ => return None,
    })
}
