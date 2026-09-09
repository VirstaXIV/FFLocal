//! Extra skeleton tables: which gear sets / hairstyles bring their own skeleton.

use anyhow::{Result, bail};

use crate::race::RaceCode;

pub struct EstFile {
    entries: Vec<(u16, u16, u16)>, // (race code, set id, skeleton id)
}

impl EstFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            bail!("est too small");
        }
        let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let desc_end = 4 + count * 4;
        if bytes.len() < desc_end + count * 2 {
            bail!("est truncated");
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let d = &bytes[4 + i * 4..8 + i * 4];
            let set = u16::from_le_bytes([d[0], d[1]]);
            let race = u16::from_le_bytes([d[2], d[3]]);
            let s = &bytes[desc_end + i * 2..desc_end + i * 2 + 2];
            entries.push((race, set, u16::from_le_bytes([s[0], s[1]])));
        }
        Ok(Self { entries })
    }

    /// Skeleton id for a race/set, if any.
    pub fn skeleton(&self, race: RaceCode, set: u16) -> Option<u16> {
        self.entries
            .iter()
            .find(|(r, s, _)| *r == race.0 && *s == set)
            .map(|(_, _, id)| *id)
            .filter(|id| *id != 0)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
