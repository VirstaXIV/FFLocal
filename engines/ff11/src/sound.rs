//! FFXI sound files: BGW music streams (`sound*/win/music/data/music{id:03}.bgw`) and SPW
//! effects (`sound*/win/se/se{id/1000:03}/se{id:06}.spw`), decoded to PCM for
//! [`ffl_core::SoundData`].
//!
//! Layout (verified on the Steam install against vgmstream's `bgw.c`):
//!
//! | field            | BGW (`"BGMStream"`) | SPW (`"SeWave"`) |
//! |------------------|---------------------|------------------|
//! | codec u32        | 0x0C                | 0x0C             |
//! | file size u32    | 0x10                | 0x08             |
//! | id u32           | 0x14                | 0x10             |
//! | block count u32  | 0x18                | 0x14             |
//! | loop start i32   | 0x1C                | 0x18             |
//! | sample rate      | (u32@0x20 + u32@0x24) & 0x7FFFFFFF | (u32@0x1C + u32@0x20) & 0x7FFFFFFF |
//! | data offset u32  | 0x28                | 0x24             |
//! | channels u8      | 0x2E                | 0x2A             |
//! | block align u8   | 0x2F                | 0x2C             |
//!
//! Codec 0 is PS-ADPCM: one frame per channel per block, `block_align / 2 + 1` bytes (a
//! shift/predictor byte, then `block_align` nibbles, low nibble first), channels interleaved
//! frame by frame; `samples = block_count × block_align`. Codec 1 (SPW only) is 16-bit PCM
//! with `block_count` sample frames. Codec 3 is ATRAC3 (XOR-scrambled), which we cannot
//! decode: 81 music tracks (ids 040-074) and ~1000 effects stay silent. A loop starts at block
//! `loop_start` (1-based; `-1`/`0` = none) and always runs to the end.

use anyhow::{Result, bail};
use ffl_core::{FootstepSet, Gait, GroundCondition, Provenance, SoundCue, SoundData, SurfaceKind};

/// Which of the two containers a key or path refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundKind {
    /// Music stream (`music{id:03}.bgw`).
    Bgw,
    /// Sound effect (`se{id:06}.spw`).
    Spw,
}

impl SoundKind {
    pub fn label(self) -> &'static str {
        match self {
            SoundKind::Bgw => "bgw",
            SoundKind::Spw => "spw",
        }
    }

    pub fn from_label(s: &str) -> Option<SoundKind> {
        match s.to_ascii_lowercase().as_str() {
            "bgw" | "music" | "bgm" => Some(SoundKind::Bgw),
            "spw" | "se" => Some(SoundKind::Spw),
            _ => None,
        }
    }

    /// Path relative to a `sound*` directory.
    pub fn relative_path(self, id: u32) -> String {
        match self {
            SoundKind::Bgw => format!("win/music/data/music{id:03}.bgw"),
            SoundKind::Spw => format!("win/se/se{:03}/se{id:06}.spw", id / 1000),
        }
    }
}

/// Sound key for [`ffl_core::Engine::load_sound`]: `ff11:bgw:<id>` / `ff11:spw:<id>`.
pub fn sound_key(kind: SoundKind, id: u32) -> String {
    format!("ff11:{}:{id}", kind.label())
}

pub fn parse_key(key: &str) -> Option<(SoundKind, u32)> {
    let rest = key.strip_prefix("ff11:")?;
    let (kind, id) = rest.split_once(':')?;
    Some((SoundKind::from_label(kind)?, id.parse().ok()?))
}

pub const CODEC_PS_ADPCM: u32 = 0;
pub const CODEC_PCM16: u32 = 1;
pub const CODEC_ATRAC3: u32 = 3;

pub fn codec_name(codec: u32) -> &'static str {
    match codec {
        CODEC_PS_ADPCM => "PS-ADPCM",
        CODEC_PCM16 => "PCM16",
        CODEC_ATRAC3 => "ATRAC3",
        _ => "unknown",
    }
}

/// The first 0x30 bytes of a BGW/SPW file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundHeader {
    pub kind: SoundKind,
    pub codec: u32,
    pub file_size: u32,
    pub id: u32,
    pub block_count: u32,
    /// 1-based loop start block (PCM: sample frame); `<= 0` = no loop.
    pub loop_start: i32,
    pub sample_rate: u32,
    pub data_offset: u32,
    pub channels: u8,
    /// Samples per PS-ADPCM frame (0 for ATRAC3, 1 or 0 for PCM).
    pub block_align: u8,
}

pub const HEADER_LEN: usize = 0x30;

impl SoundHeader {
    pub fn codec_name(&self) -> &'static str {
        codec_name(self.codec)
    }

    pub fn decodable(&self) -> bool {
        matches!(self.codec, CODEC_PS_ADPCM | CODEC_PCM16)
    }

    /// PS-ADPCM bytes per channel per block.
    pub fn frame_bytes(&self) -> usize {
        self.block_align as usize / 2 + 1
    }

    /// Sample frames the header announces (0 when the codec is not decodable).
    pub fn frames(&self) -> u64 {
        match self.codec {
            CODEC_PS_ADPCM => self.block_count as u64 * self.block_align as u64,
            CODEC_PCM16 => self.block_count as u64,
            _ => 0,
        }
    }

    pub fn duration_secs(&self) -> f32 {
        self.frames() as f32 / self.sample_rate.max(1) as f32
    }

    /// Loop region in sample frames, `[start, total)`.
    pub fn loop_frames(&self) -> Option<(u64, u64)> {
        if self.loop_start <= 0 {
            return None;
        }
        let start = match self.codec {
            CODEC_PS_ADPCM => (self.loop_start as u64 - 1) * self.block_align as u64,
            CODEC_PCM16 => self.loop_start as u64 - 1,
            _ => return None,
        };
        Some((start, self.frames()))
    }
}

fn u32_at(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}

/// Parse the header; the kind is detected from the magic.
pub fn header(bytes: &[u8]) -> Result<SoundHeader> {
    if bytes.len() < HEADER_LEN {
        bail!("sound file too short ({} bytes)", bytes.len());
    }
    let rate = |a: usize, b: usize| u32_at(bytes, a).wrapping_add(u32_at(bytes, b)) & 0x7FFF_FFFF;
    if bytes.starts_with(b"BGMStream\0") {
        Ok(SoundHeader {
            kind: SoundKind::Bgw,
            codec: u32_at(bytes, 0x0C),
            file_size: u32_at(bytes, 0x10),
            id: u32_at(bytes, 0x14),
            block_count: u32_at(bytes, 0x18),
            loop_start: u32_at(bytes, 0x1C) as i32,
            sample_rate: rate(0x20, 0x24),
            data_offset: u32_at(bytes, 0x28),
            channels: bytes[0x2E],
            block_align: bytes[0x2F],
        })
    } else if bytes.starts_with(b"SeWave\0") {
        Ok(SoundHeader {
            kind: SoundKind::Spw,
            codec: u32_at(bytes, 0x0C),
            file_size: u32_at(bytes, 0x08),
            id: u32_at(bytes, 0x10),
            block_count: u32_at(bytes, 0x14),
            loop_start: u32_at(bytes, 0x18) as i32,
            sample_rate: rate(0x1C, 0x20),
            data_offset: u32_at(bytes, 0x24),
            channels: bytes[0x2A],
            block_align: bytes[0x2C],
        })
    } else {
        bail!("not a BGW/SPW file (magic {:?})", String::from_utf8_lossy(&bytes[..8]))
    }
}

/// PS-ADPCM (VAG) predictor coefficients, ×64.
const VAG_COEFS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// Decode interleaved PS-ADPCM frames: `block_count` blocks of `channels` frames of
/// `block_align / 2 + 1` bytes. Returns interleaved 16-bit samples; a truncated payload
/// yields the blocks that are complete.
pub fn decode_ps_adpcm(data: &[u8], channels: usize, block_align: usize, block_count: usize) -> Vec<i16> {
    let channels = channels.max(1);
    let frame_bytes = block_align / 2 + 1;
    if block_align == 0 {
        return Vec::new();
    }
    let blocks = block_count.min(data.len() / (frame_bytes * channels));
    let mut out = vec![0i16; blocks * block_align * channels];
    let mut hist = vec![(0i32, 0i32); channels];
    for block in 0..blocks {
        for ch in 0..channels {
            let frame = &data[(block * channels + ch) * frame_bytes..][..frame_bytes];
            let mut shift = (frame[0] & 0x0F) as u32;
            if shift > 12 {
                shift = 9;
            }
            let predictor = ((frame[0] >> 4) as usize).min(4);
            let (c0, c1) = VAG_COEFS[predictor];
            let (mut h1, mut h2) = hist[ch];
            for i in 0..block_align {
                let byte = frame[1 + i / 2];
                let nibble = if i & 1 == 1 { byte >> 4 } else { byte & 0x0F } as i32;
                // Sign-extend the nibble into the top of a 16-bit word, then scale.
                let base = (((nibble << 12) as i16) >> shift) as i32;
                let sample = (base + ((h1 * c0 + h2 * c1 + 32) >> 6)).clamp(-32768, 32767);
                out[(block * block_align + i) * channels + ch] = sample as i16;
                h2 = h1;
                h1 = sample;
            }
            hist[ch] = (h1, h2);
        }
    }
    out
}

/// Decode a whole BGW/SPW file.
pub fn parse(key: &str, bytes: &[u8]) -> Result<SoundData> {
    let h = header(bytes)?;
    let data = &bytes[(h.data_offset as usize).min(bytes.len())..];
    let channels = h.channels.max(1) as u16;
    let samples = match h.codec {
        CODEC_PS_ADPCM => {
            if h.block_align == 0 || h.block_align % 2 != 0 {
                bail!("{key}: PS-ADPCM block align {} is not usable", h.block_align);
            }
            decode_ps_adpcm(data, channels as usize, h.block_align as usize, h.block_count as usize)
        }
        CODEC_PCM16 => {
            let frames = (h.block_count as usize).min(data.len() / (2 * channels as usize));
            data[..frames * 2 * channels as usize].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
        }
        CODEC_ATRAC3 => bail!("{key}: ATRAC3 not supported"),
        other => bail!("{key}: unknown codec {other}"),
    };
    Ok(SoundData::pcm16(key, h.sample_rate, channels, samples, h.loop_frames(), Provenance::Game))
}

/// The shared footstep bank `se100..se140`, decoded from the zone DATs' `SeSep` sections,
/// which are named `<race><surface><condition>`: `0111` = race 01 (Hume ♂), surface `1`,
/// condition 1 → `se100`, `0112` → `se101`, `0113` → `se102`, `0131` → `se103`, ... Each
/// race's section points at its own entry of the group (`Race::footstep_entry`, plus the
/// alternate ten entries higher). The last digit is a ground condition, not a gait (FFXI
/// plays one step sound for walking and running, and has no jump): 1 = dry, 2 = wet — the
/// owner heard the `2` groups as wet steps — and 3 something heavier that is not used
/// (2 s low thuds on most surfaces). Surfaces `m`, `q`, `r` only exist as `2` and `p` as
/// `3`; `6`, `b`, `c`, `k`, `n` only as `1`.
///
/// Which surface letter a ground polygon carries is not decoded (neither the MZB collision
/// faces nor the placement records hold a material), so the letters are matched to
/// [`SurfaceKind`] by their audio character (spectral centroid, decay, low-frequency share of
/// the Hume entries): a best effort to be corrected by ear, see `tools/` notes in CLAUDE.md.
pub const SURFACE_LETTERS: &[(SurfaceKind, char)] = &[
    (SurfaceKind::Dirt, '1'),
    (SurfaceKind::Grass, '3'),
    (SurfaceKind::Gravel, '2'),
    (SurfaceKind::Stone, '9'),
    (SurfaceKind::Wood, '8'),
    (SurfaceKind::Metal, 'a'),
    (SurfaceKind::Leaf, '5'),
    (SurfaceKind::Sand, '4'),
    (SurfaceKind::Powder, '7'),
    (SurfaceKind::Snow, 'l'),
    (SurfaceKind::Carpet, 'k'),
    (SurfaceKind::Water, 'o'),
    (SurfaceKind::Mesh, 'a'),
];

/// `se` group of (surface letter, condition 1..3): the bank lists the surfaces in the order
/// `1 3 2 7 8 9 a 5 4` with three conditions each, then the single-condition ones (which
/// answer for any condition).
pub fn footstep_group(letter: char, variant: u8) -> Option<u32> {
    const THREE: &[(char, u32)] = &[('1', 100), ('3', 103), ('2', 106), ('7', 109), ('8', 112), ('9', 115), ('a', 118), ('5', 121), ('4', 124), ('l', 131)];
    const ONE: &[(char, u32)] = &[('6', 127), ('b', 128), ('c', 129), ('k', 130), ('m', 134), ('n', 135), ('p', 138), ('q', 139), ('r', 140)];
    if let Some((_, g)) = THREE.iter().find(|(c, _)| *c == letter) {
        return (1..=3).contains(&variant).then(|| g + variant as u32 - 1);
    }
    if letter == 'o' {
        // `o1` is se137 and `o2` se136.
        return Some(if variant == 2 { 136 } else { 137 });
    }
    ONE.iter().find(|(c, _)| *c == letter).map(|(_, g)| *g)
}

/// The footstep set of a race: per surface the dry step (condition 1) for walking, running
/// and landing alike — the game has one step sound per surface — and the wet step
/// (condition 2) for rain; each a pick between the race's entry and its alternate when the
/// install has them.
pub fn footstep_set(entry: u32, exists: impl Fn(u32) -> bool) -> Option<FootstepSet> {
    let mut set = FootstepSet { cues: Vec::new(), fallback_surface: SurfaceKind::Dirt };
    for (surface, letter) in SURFACE_LETTERS {
        for (condition, digit) in [(GroundCondition::Dry, 1u8), (GroundCondition::Wet, 2)] {
            let Some(group) = footstep_group(*letter, digit) else {
                continue;
            };
            let keys: Vec<String> = [entry, entry + 10].into_iter().map(|e| group * 1000 + e).filter(|id| exists(*id)).map(|id| sound_key(SoundKind::Spw, id)).collect();
            if keys.is_empty() {
                continue;
            }
            for gait in [Gait::Walk, Gait::Run, Gait::Land] {
                let mut cue = SoundCue::new(keys.clone());
                cue.gain = 0.6;
                set.push(*surface, gait, condition, cue);
            }
        }
    }
    (!set.cues.is_empty()).then_some(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spw_header(codec: u32, channels: u8, block_align: u8, block_count: u32, loop_start: i32, rate: u32) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        h[..7].copy_from_slice(b"SeWave\0");
        h[0x0C..0x10].copy_from_slice(&codec.to_le_bytes());
        h[0x10..0x14].copy_from_slice(&1005u32.to_le_bytes());
        h[0x14..0x18].copy_from_slice(&block_count.to_le_bytes());
        h[0x18..0x1C].copy_from_slice(&(loop_start as u32).to_le_bytes());
        // Obfuscated rate: two halves whose wrapping sum has bit 31 set.
        let a: u32 = 0xB000_1234;
        let b = (rate | 0x8000_0000).wrapping_sub(a);
        h[0x1C..0x20].copy_from_slice(&a.to_le_bytes());
        h[0x20..0x24].copy_from_slice(&b.to_le_bytes());
        h[0x24..0x28].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        h[0x2A] = channels;
        h[0x2C] = block_align;
        h
    }

    #[test]
    fn adpcm_zero_frame_is_silent_and_scaled_nibbles_decode() {
        // Two stereo blocks of 16 samples: 9 bytes per channel per block.
        let mut data = vec![0u8; 9 * 2 * 2];
        // Second block, right channel: shift 12 → nibble 1 = 0x1000 >> 12 = 1, predictor 0.
        let frame = &mut data[9 * 3..9 * 4];
        frame[0] = 0x0C;
        frame[1] = 0x21; // samples 0 = 1, 1 = 2
        let pcm = decode_ps_adpcm(&data, 2, 16, 2);
        assert_eq!(pcm.len(), 2 * 16 * 2);
        assert!(pcm[..32].iter().all(|s| *s == 0));
        assert_eq!(pcm[32 + 1], 1); // block 1, sample 0, right
        assert_eq!(pcm[32 + 3], 2);
        assert_eq!(pcm[32], 0); // left stays silent
        // Predictor 1 (0.9375): sample = nibble + 60 × prev / 64.
        let mut data = vec![0u8; 9];
        data[0] = 0x1C;
        data[1] = 0x00;
        data[2] = 0x02; // sample 2 = 2 (hist 0), sample 3 = 0 + 2 × 60 / 64 → (120 + 32) >> 6 = 2
        let pcm = decode_ps_adpcm(&data, 1, 16, 1);
        assert_eq!(&pcm[..5], &[0, 0, 2, 2, 2]);
        // Truncated payload keeps the complete blocks only.
        assert_eq!(decode_ps_adpcm(&[0u8; 13], 1, 16, 2).len(), 16);
    }

    #[test]
    fn parse_pcm16_spw() {
        let mut bytes = spw_header(CODEC_PCM16, 2, 1, 4, 3, 48_000);
        for s in [1i16, -1, 2, -2, 3, -3, 4, -4] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let h = header(&bytes).unwrap();
        assert_eq!(h.kind, SoundKind::Spw);
        assert_eq!(h.sample_rate, 48_000);
        assert_eq!(h.id, 1005);
        assert_eq!(h.frames(), 4);
        let s = parse("ff11:spw:1005", &bytes).unwrap();
        assert_eq!(s.channels, 2);
        assert_eq!(s.frames, 4);
        assert_eq!(s.loop_range, Some((2, 4)));
        match s.encoding {
            ffl_core::SoundEncoding::Pcm16(v) => assert_eq!(v, vec![1, -1, 2, -2, 3, -3, 4, -4]),
            _ => panic!("not pcm"),
        }
    }

    #[test]
    fn parse_adpcm_spw_and_loop() {
        let mut bytes = spw_header(CODEC_PS_ADPCM, 1, 16, 3, 2, 48_000);
        bytes.extend_from_slice(&[0u8; 9 * 3]);
        let s = parse("ff11:spw:1", &bytes).unwrap();
        assert_eq!(s.frames, 48);
        assert_eq!(s.loop_range, Some((16, 48)));
        let none = spw_header(CODEC_PS_ADPCM, 1, 16, 1, -1, 48_000);
        assert_eq!(header(&none).unwrap().loop_frames(), None);
        let atrac = spw_header(CODEC_ATRAC3, 2, 0, 100, 0, 44_100);
        let err = parse("ff11:spw:2", &atrac).unwrap_err().to_string();
        assert!(err.contains("ATRAC3"), "{err}");
    }

    #[test]
    fn keys_and_footsteps() {
        assert_eq!(parse_key("ff11:bgw:109"), Some((SoundKind::Bgw, 109)));
        assert_eq!(parse_key("ff11:spw:100001"), Some((SoundKind::Spw, 100_001)));
        assert_eq!(parse_key("ff14:scd:x"), None);
        assert_eq!(SoundKind::Bgw.relative_path(9), "win/music/data/music009.bgw");
        assert_eq!(SoundKind::Spw.relative_path(100_001), "win/se/se100/se100001.spw");
        // Bank layout: surface `1` = se100/101/102 (walk/run/land), `3` = se103.., `o1` = se137.
        assert_eq!(footstep_group('1', 2), Some(101));
        assert_eq!(footstep_group('3', 3), Some(105));
        assert_eq!(footstep_group('o', 1), Some(137));
        assert_eq!(footstep_group('6', 2), Some(127));
        // Mithra (entry 2) with every file present: dirt dry = se100002 + se100012 for every
        // gait, wet = se101xxx; landing is the same step.
        let set = footstep_set(2, |_| true).unwrap();
        assert_eq!(set.cue(SurfaceKind::Dirt, Gait::Run, GroundCondition::Dry).unwrap().variations, vec!["ff11:spw:100002", "ff11:spw:100012"]);
        assert_eq!(set.cue(SurfaceKind::Dirt, Gait::Land, GroundCondition::Dry).unwrap().variations[0], "ff11:spw:100002");
        assert_eq!(set.cue(SurfaceKind::Grass, Gait::Walk, GroundCondition::Wet).unwrap().variations[0], "ff11:spw:104002");
        // An unknown surface falls back to dirt; a missing bank yields no set.
        assert_eq!(set.cue(SurfaceKind::Unknown, Gait::Walk, GroundCondition::Dry).unwrap().variations[0], "ff11:spw:100002");
        assert!(footstep_set(1, |_| false).is_none());
    }

    fn install() -> Option<crate::dat::Install> {
        let root = std::env::var_os("FFLOCAL_FF11_PATH")?;
        crate::dat::Install::open(std::path::Path::new(&root)).ok()
    }

    #[test]
    #[ignore = "needs FFLOCAL_FF11_PATH"]
    fn real_music109_decodes() {
        let Some(install) = install() else { return };
        let bytes = install.read_sound(SoundKind::Bgw, 109).unwrap();
        let h = header(&bytes).unwrap();
        assert_eq!((h.codec, h.channels, h.block_align, h.sample_rate), (CODEC_PS_ADPCM, 2, 16, 44_100));
        assert_eq!(h.file_size as usize, bytes.len());
        let s = parse("ff11:bgw:109", &bytes).unwrap();
        assert_eq!(s.frames, h.frames());
        assert!(s.loop_range.is_some());
        let ffl_core::SoundEncoding::Pcm16(pcm) = &s.encoding else { panic!() };
        let peak = pcm.iter().map(|s| (*s as i32).abs()).max().unwrap();
        assert!(peak > 2000 && peak <= 32767, "peak {peak}");
    }

    #[test]
    #[ignore = "needs FFLOCAL_FF11_PATH"]
    fn real_music040_is_atrac3() {
        let Some(install) = install() else { return };
        let bytes = install.read_sound(SoundKind::Bgw, 40).unwrap();
        assert_eq!(header(&bytes).unwrap().codec, CODEC_ATRAC3);
        assert!(parse("ff11:bgw:40", &bytes).is_err());
    }
}
