//! Staining templates (`chara/base_material/stainingtemplate.stm` for legacy materials,
//! `stainingtemplate_gud.stm` for Dawntrail ones): what a dye does to a colour-table row.
//!
//! Layout (verified on game 2026.08.11): u16 magic 0x534D, u16 version (0x101 legacy, 0x201
//! Dawntrail), u16 template count, u16 unknown; u32 template ids; u32 entry offsets (in u16
//! units from the end of the offset table). An entry is `u16 ends[N]` (cumulative sizes in
//! u16 units, N = 5 legacy arrays: diffuse, specular, emissive, gloss, specular power; 12
//! Dawntrail: diffuse, specular, emissive, scalar3, metalness, roughness, sheen rate, sheen
//! tint, sheen aperture, anisotropy, sphere-map index, sphere-map mask) followed by the
//! arrays. Colours are three halves, scalars one half. An array is empty (nothing for any
//! stain), one value (every stain), 128 values (one per stain id) or compressed: `n` values,
//! a 0xFF byte and 253 u8 indices for stains 1..253 (0 = no value).

use std::collections::HashMap;

use anyhow::{Result, bail};
use half::f16;

pub const LEGACY_PATH: &str = "chara/base_material/stainingtemplate.stm";
pub const DAWNTRAIL_PATH: &str = "chara/base_material/stainingtemplate_gud.stm";

const STAINS: usize = 254;

/// One dye's effect: colours and scalars a row takes over when its dye flags say so.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Dye {
    pub diffuse: Option<[f32; 3]>,
    pub specular: Option<[f32; 3]>,
    pub emissive: Option<[f32; 3]>,
    /// Legacy: gloss strength; Dawntrail: the third scalar (unknown use).
    pub scalar3: Option<f32>,
    /// Legacy: specular power; Dawntrail: metalness.
    pub scalar4: Option<f32>,
    pub roughness: Option<f32>,
    pub sheen_rate: Option<f32>,
    pub sheen_tint: Option<f32>,
    pub sheen_aperture: Option<f32>,
    pub anisotropy: Option<f32>,
    pub sphere_index: Option<u16>,
    pub sphere_mask: Option<f32>,
}

#[derive(Debug, Clone)]
enum Column {
    Colors(Vec<Option<[f32; 3]>>),
    Scalars(Vec<Option<f32>>),
    Raw(Vec<Option<u16>>),
}

#[derive(Debug, Clone)]
pub struct Template {
    columns: Vec<Column>,
}

#[derive(Debug, Clone)]
pub struct StmFile {
    pub version: u16,
    pub templates: HashMap<u32, Template>,
}

fn half(b: &[u8]) -> f32 {
    f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32()
}

/// Expand one array to a value per stain id (index 0 = no dye).
fn read_column(data: &[u8], size: usize, elem: usize, raw: bool) -> Result<Column> {
    let read = |k: usize| -> Option<(f32, f32, f32, u16)> {
        let b = data.get(k * elem..k * elem + elem)?;
        Some(match elem {
            6 => (half(&b[0..2]), half(&b[2..4]), half(&b[4..6]), 0),
            _ => (half(&b[0..2]), 0.0, 0.0, u16::from_le_bytes([b[0], b[1]])),
        })
    };
    let count = size / elem;
    let mut picks: Vec<Option<usize>> = vec![None; STAINS];
    if count == 1 {
        picks.iter_mut().skip(1).for_each(|p| *p = Some(0));
    } else if count == 128 {
        for (s, p) in picks.iter_mut().enumerate().take(128) {
            *p = Some(s);
        }
    } else if count > 1 {
        if size < STAINS || (size - STAINS) % elem != 0 {
            bail!("staining template array of {size} bytes is not {elem}-byte values plus an index table");
        }
        let n = (size - STAINS) / elem;
        let table = &data[n * elem..];
        if table.first() != Some(&0xFF) {
            bail!("staining template index table without its marker");
        }
        for (s, idx) in table[1..STAINS].iter().enumerate() {
            let i = *idx as usize;
            if i > 0 && i <= n {
                picks[s + 1] = Some(i - 1);
            }
        }
    }
    Ok(match (elem, raw) {
        (6, _) => Column::Colors(picks.iter().map(|p| p.and_then(read).map(|(r, g, b, _)| [r, g, b])).collect()),
        (_, true) => Column::Raw(picks.iter().map(|p| p.and_then(read).map(|(_, _, _, u)| u)).collect()),
        _ => Column::Scalars(picks.iter().map(|p| p.and_then(read).map(|(x, _, _, _)| x)).collect()),
    })
}

impl StmFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 || u16::from_le_bytes([bytes[0], bytes[1]]) != 0x534D {
            bail!("not a staining template");
        }
        let version = u16::from_le_bytes([bytes[2], bytes[3]]);
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        let arrays = if version == 0x101 { 5 } else { 12 };
        let base = 8 + count * 8;
        if bytes.len() < base {
            bail!("truncated staining template");
        }
        let mut templates = HashMap::with_capacity(count);
        for i in 0..count {
            let id = u32::from_le_bytes(bytes[8 + i * 4..12 + i * 4].try_into().unwrap());
            let offset = u32::from_le_bytes(bytes[8 + count * 4 + i * 4..12 + count * 4 + i * 4].try_into().unwrap()) as usize;
            let entry = base + offset * 2;
            if bytes.len() < entry + arrays * 2 {
                bail!("template {id} past the end of the file");
            }
            let ends: Vec<usize> = (0..arrays).map(|a| u16::from_le_bytes([bytes[entry + a * 2], bytes[entry + a * 2 + 1]]) as usize).collect();
            let start = entry + arrays * 2;
            let mut columns = Vec::with_capacity(arrays);
            let mut prev = 0;
            for (a, end) in ends.iter().enumerate() {
                let size = end.saturating_sub(prev) * 2;
                let at = start + prev * 2;
                prev = *end;
                let elem = if a < 3 { 6 } else { 2 };
                let raw = version != 0x101 && a == 10;
                let data = bytes.get(at..at + size).unwrap_or(&[]);
                columns.push(read_column(data, size, elem, raw)?);
            }
            templates.insert(id, Template { columns });
        }
        Ok(Self { version, templates })
    }

    /// The dye `stain` (Stain sheet row) gives under `template`; `None` when the template
    /// is unknown, all fields `None` when the stain has no entry.
    pub fn dye(&self, template: u32, stain: u8) -> Option<Dye> {
        let t = self.templates.get(&template)?;
        let s = stain as usize;
        let color = |i: usize| match t.columns.get(i) {
            Some(Column::Colors(v)) => v.get(s).copied().flatten(),
            _ => None,
        };
        let scalar = |i: usize| match t.columns.get(i) {
            Some(Column::Scalars(v)) => v.get(s).copied().flatten(),
            _ => None,
        };
        let raw = |i: usize| match t.columns.get(i) {
            Some(Column::Raw(v)) => v.get(s).copied().flatten(),
            _ => None,
        };
        Some(Dye {
            diffuse: color(0),
            specular: color(1),
            emissive: color(2),
            scalar3: scalar(3),
            scalar4: scalar(4),
            roughness: scalar(5),
            sheen_rate: scalar(6),
            sheen_tint: scalar(7),
            sheen_aperture: scalar(8),
            anisotropy: scalar(9),
            sphere_index: raw(10),
            sphere_mask: scalar(11),
        })
    }
}

/// Both templates a character needs, either possibly absent.
#[derive(Debug, Default, Clone)]
pub struct DyeTemplates {
    pub legacy: Option<StmFile>,
    pub dawntrail: Option<StmFile>,
}

impl DyeTemplates {
    pub fn is_empty(&self) -> bool {
        self.legacy.is_none() && self.dawntrail.is_none()
    }
}
