//! Equipment deformer parameters: which races have their own model/material for a gear set.

use anyhow::{Result, bail};

use crate::race::GearSlot;

pub struct EqdpFile {
    block_size: u16,
    block_offsets: Vec<u16>,
    data: Vec<u8>,
    data_offset: usize,
}

impl EqdpFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            bail!("eqdp too small");
        }
        let block_size = u16::from_le_bytes([bytes[2], bytes[3]]);
        let block_count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        let mut block_offsets = Vec::with_capacity(block_count);
        for i in 0..block_count {
            let o = 6 + i * 2;
            if o + 2 > bytes.len() {
                bail!("eqdp block table truncated");
            }
            block_offsets.push(u16::from_le_bytes([bytes[o], bytes[o + 1]]));
        }
        Ok(Self {
            block_size,
            block_offsets,
            data_offset: 6 + block_count * 2,
            data: bytes.to_vec(),
        })
    }

    /// Raw 10-bit entry for a set id (0 when absent).
    pub fn entry(&self, set: u16) -> u16 {
        if self.block_size == 0 {
            return 0;
        }
        let block = (set / self.block_size) as usize;
        let Some(&off) = self.block_offsets.get(block) else {
            return 0;
        };
        if off == 0xFFFF {
            return 0;
        }
        let idx = self.data_offset + (off as usize + (set % self.block_size) as usize) * 2;
        if idx + 2 > self.data.len() {
            return 0;
        }
        u16::from_le_bytes([self.data[idx], self.data[idx + 1]])
    }

    /// (bit 1, bit 2) for the slot: material and model presence flags.
    pub fn bits(&self, set: u16, slot: GearSlot) -> (bool, bool) {
        let e = self.entry(set);
        let o = slot.eqdp_offset();
        (e & (1 << o) != 0, e & (2 << o) != 0)
    }

    /// Whether this race has its own model for the set/slot.
    pub fn has_model(&self, set: u16, slot: GearSlot) -> bool {
        self.bits(set, slot).1
    }

    /// Whether this race has its own material for the set/slot.
    pub fn has_material(&self, set: u16, slot: GearSlot) -> bool {
        self.bits(set, slot).0
    }
}
