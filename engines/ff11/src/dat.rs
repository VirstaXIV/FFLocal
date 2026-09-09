//! FFXI install location, the VTABLE/FTABLE file index, and the DAT section container.
//!
//! Format facts (from GalkaReeve's DatLoader / Muzisoft's TDW viewer, verified against the
//! Steam install): `VTABLE*.DAT` holds one byte per file id (the ROM number the file lives in,
//! 0 = absent) and `FTABLE*.DAT` one `u16` per id (`folder = v >> 7`, `file = v & 0x7F`), giving
//! `ROM{n}/{folder}/{file}.DAT`. Later ROMs (`ROM9`..`ROM2`) override earlier ones. A DAT is a
//! chain of sections with a 16-byte header: 4 id bytes, then a `u32` whose low 7 bits are the
//! section type and bits 7..26 the section length in 16-byte units.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::sound::SoundKind;

pub const SECTION_MZB: u8 = 0x1c;
pub const SECTION_IMG: u8 = 0x20;
pub const SECTION_MMB: u8 = 0x2e;
/// `SeSep`: a zone's sound-effect reference (se id at payload + 8).
pub const SECTION_SESEP: u8 = 0x3D;

/// Sound directories under the install root, highest (newest) first: a file in `sound9`
/// overrides the same id in `sound`.
pub const SOUND_DIRS: [&str; 7] = ["sound9", "sound6", "sound5", "sound4", "sound3", "sound2", "sound"];

pub struct Install {
    pub root: PathBuf,
    /// (rom number, vtable bytes, ftable bytes), highest ROM first.
    tables: Vec<(u8, Vec<u8>, Vec<u8>)>,
    /// `(rom, folder, file)` → file id, built on first use.
    by_path: std::sync::OnceLock<std::collections::HashMap<(u8, u16, u16), u32>>,
}

impl Install {
    pub fn open(root: &Path) -> Result<Self> {
        if !root.join("VTABLE.DAT").exists() {
            bail!("{} has no VTABLE.DAT", root.display());
        }
        let mut tables = Vec::new();
        for n in (1..=9u8).rev() {
            let (v, f) = if n == 1 {
                (root.join("VTABLE.DAT"), root.join("FTABLE.DAT"))
            } else {
                let dir = root.join(format!("ROM{n}"));
                (dir.join(format!("VTABLE{n}.DAT")), dir.join(format!("FTABLE{n}.DAT")))
            };
            if v.exists() && f.exists() {
                tables.push((n, fs::read(&v)?, fs::read(&f)?));
            }
        }
        if tables.is_empty() {
            bail!("no VTABLE/FTABLE pairs under {}", root.display());
        }
        Ok(Self {
            root: root.to_path_buf(),
            tables,
            by_path: std::sync::OnceLock::new(),
        })
    }

    /// Number of file ids in the index.
    pub fn file_count(&self) -> usize {
        self.tables.iter().map(|(_, v, _)| v.len()).max().unwrap_or(0)
    }

    /// Path of a file id, if the index has it.
    pub fn file_path(&self, id: u32) -> Option<PathBuf> {
        let id = id as usize;
        for (_, vt, ft) in &self.tables {
            if id < vt.len() && vt[id] != 0 && id * 2 + 1 < ft.len() {
                let rom = vt[id];
                let v = u16::from_le_bytes([ft[id * 2], ft[id * 2 + 1]]);
                let dir = if rom == 1 { "ROM".to_string() } else { format!("ROM{rom}") };
                return Some(self.root.join(dir).join((v >> 7).to_string()).join(format!("{}.DAT", v & 0x7F)));
            }
        }
        None
    }

    /// File id of `ROM<rom>/<folder>/<file>.DAT` (`rom` 1 = `ROM`), if the index maps it.
    pub fn id_of(&self, rom: u8, folder: u16, file: u16) -> Option<u32> {
        let map = self.by_path.get_or_init(|| {
            let mut m = std::collections::HashMap::new();
            for (_, vt, ft) in self.tables.iter().rev() {
                for id in 0..vt.len() {
                    if vt[id] != 0 && id * 2 + 1 < ft.len() {
                        let v = u16::from_le_bytes([ft[id * 2], ft[id * 2 + 1]]);
                        m.insert((vt[id], v >> 7, v & 0x7F), id as u32);
                    }
                }
            }
            m
        });
        map.get(&(rom, folder, file)).copied()
    }

    /// File id of a motion "file number" (`folder × 1000 + file`, files past 127 spill into
    /// the next folder), the encoding the client's motion tables use.
    pub fn id_of_motion_number(&self, n: u32) -> Option<u32> {
        let (mut folder, mut file) = (n / 1000, n % 1000);
        if file > 127 {
            folder += 1;
            file -= 128;
        }
        self.id_of(1, folder as u16, file as u16)
    }

    pub fn exists(&self, id: u32) -> bool {
        self.file_path(id).is_some_and(|p| p.exists())
    }

    pub fn read(&self, id: u32) -> Result<Vec<u8>> {
        let path = self.file_path(id).ok_or_else(|| anyhow!("file id {id} is not in the DAT index"))?;
        fs::read(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// Path of a music (`bgw`) or effect (`spw`) file, searching [`SOUND_DIRS`] in order.
    pub fn sound_path(&self, kind: SoundKind, id: u32) -> Option<PathBuf> {
        let rel = kind.relative_path(id);
        SOUND_DIRS.iter().map(|d| self.root.join(d).join(&rel)).find(|p| p.exists())
    }

    pub fn read_sound(&self, kind: SoundKind, id: u32) -> Result<Vec<u8>> {
        let path = self.sound_path(kind, id).ok_or_else(|| anyhow!("no {} {id} under any sound directory", kind.label()))?;
        fs::read(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// The first 0x30 bytes (see `sound::header`) without reading the whole stream.
    pub fn sound_header(&self, kind: SoundKind, id: u32) -> Result<[u8; crate::sound::HEADER_LEN]> {
        use std::io::Read;
        let path = self.sound_path(kind, id).ok_or_else(|| anyhow!("no {} {id} under any sound directory", kind.label()))?;
        let mut f = fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let mut head = [0u8; crate::sound::HEADER_LEN];
        f.read_exact(&mut head).with_context(|| format!("reading the header of {}", path.display()))?;
        Ok(head)
    }
}

/// Every place the install might be, in precedence order: `FFLOCAL_FF11_PATH`, `config.toml`
/// (`ff11_path`), the Steam libraries of this machine (`FFXINA`, the JP/EU Steam ids and the
/// plain folder name), the PlayOnline default folders on Windows.
pub fn candidates() -> Vec<ffl_core::InstallCandidate> {
    let c = |path: PathBuf, source: &str| ffl_core::InstallCandidate { path, source: source.to_string() };
    let mut out = Vec::new();
    if let Ok(env) = std::env::var("FFLOCAL_FF11_PATH")
        && !env.is_empty()
    {
        out.push(c(PathBuf::from(env), "FFLOCAL_FF11_PATH"));
    }
    for cfg in ["config.toml"].iter().map(PathBuf::from) {
        if let Ok(text) = fs::read_to_string(&cfg) {
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("ff11_path") {
                    let value = rest.trim_start().trim_start_matches('=').trim().trim_matches('"');
                    if !value.is_empty() {
                        out.push(c(PathBuf::from(value), "config.toml"));
                    }
                }
            }
        }
    }
    for lib in ffl_ff14_assets_free::steam_libraries() {
        for folder in ["FFXINA", "FFXIEU", "FFXIJP", "FINAL FANTASY XI"] {
            out.push(c(lib.join("steamapps/common").join(folder).join("SquareEnix/FINAL FANTASY XI"), "Steam library"));
        }
    }
    for pf in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(dir) = std::env::var_os(pf) {
            out.push(c(PathBuf::from(dir).join("PlayOnline/SquareEnix/FINAL FANTASY XI"), "PlayOnline default folder"));
        }
    }
    out.push(c(PathBuf::from("C:\\Program Files (x86)\\PlayOnline\\SquareEnix\\FINAL FANTASY XI"), "PlayOnline default folder"));
    out
}

/// Steam library folders (the same search the FF14 locator uses, kept here so this crate
/// stays free of the FF14 crates).
mod ffl_ff14_assets_free {
    use std::path::PathBuf;

    pub fn steam_libraries() -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(h) = std::env::var_os("HOME").map(PathBuf::from).or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from)) {
            roots.push(h.join(".steam/steam"));
            roots.push(h.join(".local/share/Steam"));
            roots.push(h.join("Library/Application Support/Steam"));
        }
        if let Some(pf) = std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles")) {
            roots.push(PathBuf::from(pf).join("Steam"));
        }
        roots.push(PathBuf::from("C:\\Program Files (x86)\\Steam"));
        let mut out = Vec::new();
        for root in roots {
            let Ok(text) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else {
                continue;
            };
            if !out.contains(&root) {
                out.push(root.clone());
            }
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("\"path\"") {
                    let p = PathBuf::from(rest.trim().trim_matches('"').replace("\\\\", "\\"));
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}

/// The install root a candidate path denotes (the folder holding `VTABLE.DAT`, or a parent
/// of it two levels up), if any.
pub fn install_root(path: &Path) -> Option<PathBuf> {
    [path.to_path_buf(), path.join("SquareEnix/FINAL FANTASY XI"), path.join("FINAL FANTASY XI")]
        .into_iter()
        .find(|root| root.join("VTABLE.DAT").exists())
}

/// The candidates that really hold the game, duplicates dropped: the hub's "Scan".
pub fn scan() -> Vec<ffl_core::InstallCandidate> {
    let mut out: Vec<ffl_core::InstallCandidate> = Vec::new();
    for c in candidates() {
        let Some(root) = install_root(&c.path) else {
            continue;
        };
        if !out.iter().any(|o| o.path == root) {
            out.push(ffl_core::InstallCandidate { path: root, source: c.source });
        }
    }
    out
}

/// Locate the install: an explicit path (CLI, the hub) must be the game itself; otherwise
/// the first candidate that holds it wins.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return install_root(p).ok_or_else(|| anyhow!("{} is not an FFXI installation (no VTABLE.DAT)", p.display()));
    }
    let list = candidates();
    let tried = list.len();
    for c in list {
        if let Some(root) = install_root(&c.path) {
            return Ok(root);
        }
    }
    Err(anyhow!("could not find an FFXI installation; pick it in Games, pass --ff11-path, set FFLOCAL_FF11_PATH, or add ff11_path to config.toml (tried {tried} candidates)"))
}

/// One section of a DAT: type and payload range (after the 16-byte header).
#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub kind: u8,
    pub id: [u8; 4],
    pub start: usize,
    pub end: usize,
}

pub fn sections(data: &[u8]) -> Vec<Section> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 16 <= data.len() {
        let bits = u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        let kind = (bits & 0x7F) as u8;
        let next = ((bits >> 7) & 0x7FFFF) as usize * 16;
        let end = (off + next).min(data.len());
        if next >= 16 {
            out.push(Section {
                kind,
                id: [data[off], data[off + 1], data[off + 2], data[off + 3]],
                start: off + 16,
                end,
            });
        }
        if next == 0 {
            break;
        }
        off += next;
    }
    out
}

const KEY_TABLE: [u8; 256] = [
    0xE2, 0xE5, 0x06, 0xA9, 0xED, 0x26, 0xF4, 0x42, 0x15, 0xF4, 0x81, 0x7F, 0xDE, 0x9A, 0xDE, 0xD0, 0x1A, 0x98, 0x20, 0x91, 0x39, 0x49, 0x48, 0xA4, 0x0A, 0x9F, 0x40, 0x69, 0xEC, 0xBD, 0x81, 0x81,
    0x8D, 0xAD, 0x10, 0xB8, 0xC1, 0x88, 0x15, 0x05, 0x11, 0xB1, 0xAA, 0xF0, 0x0F, 0x1E, 0x34, 0xE6, 0x81, 0xAA, 0xCD, 0xAC, 0x02, 0x84, 0x33, 0x0A, 0x19, 0x38, 0x9E, 0xE6, 0x73, 0x4A, 0x11, 0x5D,
    0xBF, 0x85, 0x77, 0x08, 0xCD, 0xD9, 0x96, 0x0D, 0x79, 0x78, 0xCC, 0x35, 0x06, 0x8E, 0xF9, 0xFE, 0x66, 0xB9, 0x21, 0x03, 0x20, 0x29, 0x1E, 0x27, 0xCA, 0x86, 0x82, 0xE6, 0x45, 0x07, 0xDD, 0xA9,
    0xB6, 0xD5, 0xA2, 0x03, 0xEC, 0xAD, 0x62, 0x45, 0x2D, 0xCE, 0x79, 0xBD, 0x8F, 0x2D, 0x10, 0x18, 0xE6, 0x0A, 0x6F, 0xAA, 0x6F, 0x46, 0x84, 0x32, 0x9F, 0x29, 0x2C, 0xC2, 0xF0, 0xEB, 0x18, 0x6F,
    0xF2, 0x3A, 0xDC, 0xEA, 0x7B, 0x0C, 0x81, 0x2D, 0xCC, 0xEB, 0xA1, 0x51, 0x77, 0x2C, 0xFB, 0x49, 0xE8, 0x90, 0xF7, 0x90, 0xCE, 0x5C, 0x01, 0xF3, 0x5C, 0xF4, 0x41, 0xAB, 0x04, 0xE7, 0x16, 0xCC,
    0x3A, 0x05, 0x54, 0x55, 0xDC, 0xED, 0xA4, 0xD6, 0xBF, 0x3F, 0x9E, 0x08, 0x93, 0xB5, 0x63, 0x38, 0x90, 0xF7, 0x5A, 0xF0, 0xA2, 0x5F, 0x56, 0xC8, 0x08, 0x70, 0xCB, 0x24, 0x16, 0xDD, 0xD2, 0x74,
    0x95, 0x3A, 0x1A, 0x2A, 0x74, 0xC4, 0x9D, 0xEB, 0xAF, 0x69, 0xAA, 0x51, 0x39, 0x65, 0x94, 0xA2, 0x4B, 0x1F, 0x1A, 0x60, 0x52, 0x39, 0xE8, 0x23, 0xEE, 0x58, 0x39, 0x06, 0x3D, 0x22, 0x6A, 0x2D,
    0xD2, 0x91, 0x25, 0xA5, 0x2E, 0x71, 0x62, 0xA5, 0x0B, 0xC1, 0xE5, 0x6E, 0x43, 0x49, 0x7C, 0x58, 0x46, 0x19, 0x9F, 0x45, 0x49, 0xC6, 0x40, 0x09, 0xA2, 0x99, 0x5B, 0x7B, 0x98, 0x7F, 0xA0, 0xD0,
];

const KEY_TABLE2: [u8; 256] = [
    0xB8, 0xC5, 0xF7, 0x84, 0xE4, 0x5A, 0x23, 0x7B, 0xC8, 0x90, 0x1D, 0xF6, 0x5D, 0x09, 0x51, 0xC1, 0x07, 0x24, 0xEF, 0x5B, 0x1D, 0x73, 0x90, 0x08, 0xA5, 0x70, 0x1C, 0x22, 0x5F, 0x6B, 0xEB, 0xB0,
    0x06, 0xC7, 0x2A, 0x3A, 0xD2, 0x66, 0x81, 0xDB, 0x41, 0x62, 0xF2, 0x97, 0x17, 0xFE, 0x05, 0xEF, 0xA3, 0xDC, 0x22, 0xB3, 0x45, 0x70, 0x3E, 0x18, 0x2D, 0xB4, 0xBA, 0x0A, 0x65, 0x1D, 0x87, 0xC3,
    0x12, 0xCE, 0x8F, 0x9D, 0xF7, 0x0D, 0x50, 0x24, 0x3A, 0xF3, 0xCA, 0x70, 0x6B, 0x67, 0x9C, 0xB2, 0xC2, 0x4D, 0x6A, 0x0C, 0xA8, 0xFA, 0x81, 0xA6, 0x79, 0xEB, 0xBE, 0xFE, 0x89, 0xB7, 0xAC, 0x7F,
    0x65, 0x43, 0xEC, 0x56, 0x5B, 0x35, 0xDA, 0x81, 0x3C, 0xAB, 0x6D, 0x28, 0x60, 0x2C, 0x5F, 0x31, 0xEB, 0xDF, 0x8E, 0x0F, 0x4F, 0xFA, 0xA3, 0xDA, 0x12, 0x7E, 0xF1, 0xA5, 0xD2, 0x22, 0xA0, 0x0C,
    0x86, 0x8C, 0x0A, 0x0C, 0x06, 0xC7, 0x65, 0x18, 0xCE, 0xF2, 0xA3, 0x68, 0xFE, 0x35, 0x96, 0x95, 0xA6, 0xFA, 0x58, 0x63, 0x41, 0x59, 0xEA, 0xDD, 0x7F, 0xD3, 0x1B, 0xA8, 0x48, 0x44, 0xAB, 0x91,
    0xFD, 0x13, 0xB1, 0x68, 0x01, 0xAC, 0x3A, 0x11, 0x78, 0x30, 0x33, 0xD8, 0x4E, 0x6A, 0x89, 0x05, 0x7B, 0x06, 0x8E, 0xB0, 0x86, 0xFD, 0x9F, 0xD7, 0x48, 0x54, 0x04, 0xAE, 0xF3, 0x06, 0x17, 0x36,
    0x53, 0x3F, 0xA8, 0x11, 0x53, 0xCA, 0xA1, 0x95, 0xC2, 0xCD, 0xE6, 0x1F, 0x57, 0xB4, 0x7F, 0xAA, 0xF3, 0x6B, 0xF9, 0xA0, 0x27, 0xD0, 0x09, 0xEF, 0xF6, 0x68, 0x73, 0x60, 0xDC, 0x50, 0x2A, 0x25,
    0x0F, 0x77, 0xB9, 0xB0, 0x04, 0x0B, 0xE1, 0xCC, 0x35, 0x31, 0x84, 0xE6, 0x22, 0xF9, 0xC2, 0xAB, 0x95, 0x91, 0x61, 0xD9, 0x2B, 0xB9, 0x72, 0x4E, 0x10, 0x76, 0x31, 0x66, 0x0A, 0x0B, 0x2E, 0x83,
];

fn payload_len(p: &[u8]) -> usize {
    (p[0] as usize) | ((p[1] as usize) << 8) | ((p[2] as usize) << 16)
}

/// Decrypt an MMB payload in place (byte XOR stream, then the 8-byte block swap).
pub fn decode_mmb(p: &mut [u8]) {
    if p.len() < 8 {
        return;
    }
    if p[3] >= 5 {
        let len = payload_len(p).min(p.len());
        let mut key = KEY_TABLE[(p[5] ^ 0xF0) as usize] as u32;
        let mut counter: u32 = 0;
        for pos in 8..len {
            let x = ((key & 0xFF) << 8) | (key & 0xFF);
            counter += 1;
            key = key.wrapping_add(counter);
            p[pos] ^= (x >> (key & 7)) as u8;
            counter += 1;
            key = key.wrapping_add(counter);
        }
    }
    if p[6] == 0xFF && p[7] == 0xFF {
        let len = payload_len(p).min(p.len());
        let mut key1 = (p[5] ^ 0xF0) as u32;
        let mut key2 = KEY_TABLE2[key1 as usize] as u32;
        let count = ((len.saturating_sub(8)) & !0xF) / 2;
        let (mut a, mut b) = (8usize, 8 + count);
        let mut pos = 0;
        while pos < count && b + 8 <= p.len() {
            if key2 & 1 == 1 {
                for k in 0..8 {
                    p.swap(a + k, b + k);
                }
            }
            key1 = key1.wrapping_add(9);
            key2 = key2.wrapping_add(key1);
            a += 8;
            b += 8;
            pos += 8;
        }
    }
}

/// Decrypt an MZB payload in place (16..23-byte XOR runs). Returns whether it was encrypted.
/// Record names are XORed with 0x55 as well; `zone.rs` undoes that once the record size is known.
pub fn decode_mzb(p: &mut [u8]) -> bool {
    if p.len() < 32 || p[3] < 0x1B {
        return false;
    }
    let len = payload_len(p).min(p.len());
    let mut key = KEY_TABLE[(p[7] ^ 0xFF) as usize] as u32;
    let mut counter: u32 = 0;
    let mut pos = 8usize;
    while pos < len {
        let run = (((key >> 4) & 7) + 16) as usize;
        if key & 1 == 1 && pos + run < len {
            for b in &mut p[pos..pos + run] {
                *b ^= 0xFF;
            }
        }
        counter += 1;
        key = key.wrapping_add(counter);
        pos += run;
    }
    true
}
