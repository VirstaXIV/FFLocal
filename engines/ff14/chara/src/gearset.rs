//! `GEARSET.DAT` parser (the per-character gear set file in `FFXIV_CHR*/`).
//!
//! Layout (game 2026.08.11, verified on real files):
//! - 17-byte header (u16 type=5, u16 version, u32 max_size, u32 content_size, 4 pad, u8 0xFF)
//! - everything after is XOR `0x73`: u8, u8 current set, u16, then 100 × 452-byte gear sets:
//!   u8 index, 47-byte name, u64, 14 slots × 28 bytes (u32 item id [+1_000_000 = HQ],
//!   u32 glamour id, u32 dye, 4 × u32), u32 facewear.

use anyhow::{Result, anyhow, bail};

pub const SLOT_COUNT: usize = 14;
pub const GEARSET_COUNT: usize = 100;
const HEADER_SIZE: usize = 17;
const XOR_KEY: u8 = 0x73;
const NAME_SIZE: usize = 47;
const SLOT_SIZE: usize = 28;
const SET_SIZE: usize = 1 + NAME_SIZE + 8 + SLOT_COUNT * SLOT_SIZE + 4;
const HQ_FLAG: u32 = 1_000_000;

/// Slot order inside a gear set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GearsetSlot {
    MainHand = 0,
    OffHand,
    Head,
    Body,
    Hands,
    Waist,
    Legs,
    Feet,
    Ears,
    Neck,
    Wrists,
    RingLeft,
    RingRight,
    SoulCrystal,
}

impl GearsetSlot {
    pub const ALL: [GearsetSlot; SLOT_COUNT] = [
        GearsetSlot::MainHand,
        GearsetSlot::OffHand,
        GearsetSlot::Head,
        GearsetSlot::Body,
        GearsetSlot::Hands,
        GearsetSlot::Waist,
        GearsetSlot::Legs,
        GearsetSlot::Feet,
        GearsetSlot::Ears,
        GearsetSlot::Neck,
        GearsetSlot::Wrists,
        GearsetSlot::RingLeft,
        GearsetSlot::RingRight,
        GearsetSlot::SoulCrystal,
    ];
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GearsetItem {
    pub item_id: u32,
    pub hq: bool,
    pub glamour_id: u32,
    /// Packed stains: byte 0 = first dye, byte 1 = second dye (Stain sheet rows), the rest
    /// flags (0x10 / 0x0F seen on live sets).
    pub dye: u32,
}

impl GearsetItem {
    /// The two dye channels' stain ids (0 = undyed).
    pub fn stains(&self) -> [u8; 2] {
        [(self.dye & 0xFF) as u8, ((self.dye >> 8) & 0xFF) as u8]
    }

    pub fn pack_stains(stains: [u8; 2]) -> u32 {
        stains[0] as u32 | (stains[1] as u32) << 8
    }
}

#[derive(Debug, Clone)]
pub struct Gearset {
    pub index: u8,
    pub name: String,
    pub class_job: u8,
    pub slots: [GearsetItem; SLOT_COUNT],
    pub facewear: u32,
}

impl Gearset {
    pub fn item(&self, slot: GearsetSlot) -> Option<GearsetItem> {
        let it = self.slots[slot as usize];
        (it.item_id != 0).then_some(it)
    }
}

#[derive(Debug, Clone)]
pub struct Gearsets {
    pub current: u8,
    pub sets: Vec<Gearset>,
}

impl Gearsets {
    pub fn parse(bytes: &[u8]) -> Result<Gearsets> {
        if bytes.len() < HEADER_SIZE + 4 {
            bail!("file too small ({} bytes)", bytes.len());
        }
        let file_type = u16::from_le_bytes([bytes[0], bytes[1]]);
        if file_type != 5 {
            bail!("not a GEARSET.DAT (type {file_type})");
        }
        let content_size = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        let end = (HEADER_SIZE + content_size.saturating_sub(1)).min(bytes.len());
        let body: Vec<u8> = bytes[HEADER_SIZE..end].iter().map(|b| b ^ XOR_KEY).collect();
        if body.len() < 4 {
            bail!("gear set body too small");
        }
        let current = body[1];
        let mut sets = Vec::new();
        let mut off = 4;
        for _ in 0..GEARSET_COUNT {
            if off + SET_SIZE > body.len() {
                break;
            }
            let rec = &body[off..off + SET_SIZE];
            let index = rec[0];
            let name_bytes = &rec[1..1 + NAME_SIZE];
            let name_len = name_bytes.iter().position(|b| *b == 0).unwrap_or(NAME_SIZE);
            let name = String::from_utf8_lossy(&name_bytes[..name_len]).to_string();
            let class_job = rec[1 + NAME_SIZE + 1];
            let mut slots = [GearsetItem::default(); SLOT_COUNT];
            let base = 1 + NAME_SIZE + 8;
            for (i, slot) in slots.iter_mut().enumerate() {
                let s = &rec[base + i * SLOT_SIZE..base + (i + 1) * SLOT_SIZE];
                let raw = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                let glamour = u32::from_le_bytes([s[4], s[5], s[6], s[7]]);
                let dye = u32::from_le_bytes([s[8], s[9], s[10], s[11]]);
                let (item_id, hq) = if raw >= HQ_FLAG { (raw - HQ_FLAG, true) } else { (raw, false) };
                *slot = GearsetItem {
                    item_id,
                    hq,
                    glamour_id: if glamour >= HQ_FLAG { glamour - HQ_FLAG } else { glamour },
                    dye,
                };
            }
            let fw = &rec[SET_SIZE - 4..];
            let facewear = u32::from_le_bytes([fw[0], fw[1], fw[2], fw[3]]);
            if !name.is_empty() {
                sets.push(Gearset {
                    index,
                    name,
                    class_job,
                    slots,
                    facewear,
                });
            }
            off += SET_SIZE;
        }
        if sets.is_empty() {
            return Err(anyhow!("no gear sets found"));
        }
        Ok(Gearsets { current, sets })
    }

    pub fn by_index(&self, index: u8) -> Option<&Gearset> {
        self.sets.iter().find(|s| s.index == index)
    }
}
