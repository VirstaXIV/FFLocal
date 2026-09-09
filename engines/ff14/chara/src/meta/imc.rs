//! Item model configuration: material variant, decal, attribute mask per part/variant.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImcEntry {
    pub material_id: u8,
    pub decal_id: u8,
    pub attribute_mask: u16,
    pub sound_id: u8,
    pub vfx_id: u8,
    pub material_animation_id: u8,
}

pub struct ImcFile {
    count: u16,
    part_mask: u16,
    num_parts: usize,
    entries: Vec<ImcEntry>,
}

impl ImcFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            bail!("imc too small");
        }
        let count = u16::from_le_bytes([bytes[0], bytes[1]]);
        let part_mask = u16::from_le_bytes([bytes[2], bytes[3]]);
        let num_parts = part_mask.count_ones() as usize;
        let mut entries = Vec::new();
        let mut off = 4;
        while off + 6 <= bytes.len() {
            let b = &bytes[off..off + 6];
            let attr_sound = u16::from_le_bytes([b[2], b[3]]);
            entries.push(ImcEntry {
                material_id: b[0],
                decal_id: b[1],
                attribute_mask: attr_sound & 0x3FF,
                sound_id: (attr_sound >> 10) as u8,
                vfx_id: b[4],
                material_animation_id: b[5],
            });
            off += 6;
        }
        Ok(Self {
            count,
            part_mask,
            num_parts,
            entries,
        })
    }

    pub fn variant_count(&self) -> u16 {
        self.count
    }

    /// Entry for a part index and variant (variant 0 is the default row).
    pub fn entry(&self, part: usize, variant: u16) -> Option<ImcEntry> {
        if self.part_mask & (1 << part) == 0 || variant > self.count || self.num_parts == 0 {
            return None;
        }
        let part_slot = (0..part).filter(|p| self.part_mask & (1 << p) != 0).count();
        self.entries
            .get(variant as usize * self.num_parts + part_slot)
            .copied()
    }
}
