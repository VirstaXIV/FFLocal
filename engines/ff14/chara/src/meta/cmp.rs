//! `human.cmp`: character-maker colour tables and racial scaling.
//!
//! Layout follows Penumbra.GameData `CmpData.cs` (all colours RGBA8).

use anyhow::{Result, bail};
use physis::race::{Gender, Tribe};

pub const FILE_SIZE: usize = 0x2D980;
const COLOR_PARAMS_SIZE: usize = (256 * 7 + 128 * 4) * 4; // 9216
const RACE_BLOCK_SIZE: usize = (256 + 256 * 2 + 256 + 256) * 4; // 5120
const RACES_OFFSET: usize = COLOR_PARAMS_SIZE * 2;
const SCALES_OFFSET: usize = RACES_OFFSET + 32 * RACE_BLOCK_SIZE; // 0x2C800
const SCALE_SIZE: usize = 14 * 4;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rgba8(pub [u8; 4]);

impl Rgba8 {
    pub fn to_f32(self) -> [f32; 4] {
        let c = self.0;
        [
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
            c[3] as f32 / 255.0,
        ]
    }

    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0[0], self.0[1], self.0[2])
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RacialScale {
    pub male_height: [f32; 2],
    pub male_tail: [f32; 2],
    pub female_height: [f32; 2],
    pub female_tail: [f32; 2],
    pub bust_min: [f32; 3],
    pub bust_max: [f32; 3],
}

pub struct CmpFile {
    data: Vec<u8>,
}

impl CmpFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < FILE_SIZE {
            bail!("human.cmp is {} bytes, expected at least {FILE_SIZE}", bytes.len());
        }
        Ok(Self {
            data: bytes.to_vec(),
        })
    }

    fn color_at(&self, byte_offset: usize) -> Rgba8 {
        let b = &self.data[byte_offset..byte_offset + 4];
        Rgba8([b[0], b[1], b[2], b[3]])
    }

    /// Index of the race block for a tribe/gender.
    fn race_index(tribe: Tribe, gender: &Gender) -> usize {
        let t = tribe as usize - 1;
        t * 2 + usize::from(*gender == Gender::Female)
    }

    pub fn eye_color(&self, idx: u8) -> Rgba8 {
        self.color_at(idx as usize * 4)
    }

    pub fn hair_highlight_color(&self, idx: u8) -> Rgba8 {
        self.color_at(256 * 4 + idx as usize * 4)
    }

    /// Lip colour: `idx & 0x7F` selects the row; the 0x80 flag selects the light table.
    pub fn lip_color(&self, idx: u8) -> Rgba8 {
        let light = idx & 0x80 != 0;
        let i = (idx & 0x7F) as usize;
        let base = if light {
            (256 + 256 + 128 + 128 + 256) * 4
        } else {
            (256 + 256) * 4
        };
        self.color_at(base + i * 4)
    }

    /// Facial feature / tattoo / limbal ring colour.
    pub fn feature_color(&self, idx: u8) -> Rgba8 {
        self.color_at((256 + 256 + 128 + 128) * 4 + idx as usize * 4)
    }

    pub fn face_paint_color(&self, idx: u8) -> Rgba8 {
        let light = idx & 0x80 != 0;
        let i = (idx & 0x7F) as usize;
        let base = if light {
            (256 + 256 + 128 + 128 + 256 + 128) * 4
        } else {
            (256 + 256 + 128) * 4
        };
        self.color_at(base + i * 4)
    }

    pub fn skin_color(&self, tribe: Tribe, gender: &Gender, idx: u8) -> Rgba8 {
        let base = RACES_OFFSET + Self::race_index(tribe, gender) * RACE_BLOCK_SIZE;
        self.color_at(base + idx as usize * 4)
    }

    pub fn hair_color(&self, tribe: Tribe, gender: &Gender, idx: u8) -> Rgba8 {
        let base = RACES_OFFSET + Self::race_index(tribe, gender) * RACE_BLOCK_SIZE + 256 * 4;
        self.color_at(base + idx as usize * 8)
    }

    pub fn scale(&self, tribe: Tribe) -> RacialScale {
        let t = tribe as usize - 1;
        let off = SCALES_OFFSET + ((t >> 1) * 10 + (t & 1)) * SCALE_SIZE;
        let f = |i: usize| -> f32 {
            let b = &self.data[off + i * 4..off + i * 4 + 4];
            f32::from_le_bytes([b[0], b[1], b[2], b[3]])
        };
        RacialScale {
            male_height: [f(0), f(1)],
            male_tail: [f(2), f(3)],
            female_height: [f(4), f(5)],
            female_tail: [f(6), f(7)],
            bust_min: [f(8), f(9), f(10)],
            bust_max: [f(11), f(12), f(13)],
        }
    }

    /// Uniform character scale for a height slider value (0..100).
    pub fn height_scale(&self, tribe: Tribe, gender: &Gender, height: u8) -> f32 {
        let s = self.scale(tribe);
        let [lo, hi] = if *gender == Gender::Female {
            s.female_height
        } else {
            s.male_height
        };
        let t = (height as f32 / 100.0).clamp(0.0, 1.0);
        if !(0.1..=10.0).contains(&lo) || !(0.1..=10.0).contains(&hi) {
            return 1.0;
        }
        lo + (hi - lo) * t
    }
}
