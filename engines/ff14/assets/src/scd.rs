//! FF14 `.scd` sound containers: the header/table layout, the per-entry codec sub-headers and
//! decoding to PCM. Verified against retail files (`music/ffxiv/BGM_Field_Housing_Day.scd`,
//! `sound/foot/foot/fs_grass_m_f_shoes.scd`); the vendored Physis `scd` struct does not match
//! these files, so this is a plain byte parser. Layout reference: VFXEditor `ScdFile`,
//! `ScdAudioEntry`, `ScdVorbis`, `ScdAdpcm`, `ScdUtils`.
//!
//! - Header: `SEDBSSCF`, u16 tables offset at 0x0E (0x30), u32 file size at 0x10.
//! - Tables: u16 sound-entry count at +0x04, u32 offset of the entry offset table at +0x0C.
//! - Entry (0x20 bytes): stream size, channels, sample rate, codec, loop start, loop end,
//!   sub-info size, flags; then the optional `MARK` chunk (flags bit 0), the codec sub-header,
//!   then `stream size` bytes of audio.
//! - Codec 6 = Ogg Vorbis with a scrambled header (loop points are byte offsets into the
//!   stream, on page boundaries); 12 = MS-ADPCM with a `WAVEFORMATEX` (loop points in
//!   samples); 0 = PCM16; 26 = HCA (unsupported).

use std::io::Cursor;

use anyhow::{Result, anyhow, bail};

use crate::adpcm::MsAdpcmFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScdCodec {
    Pcm,
    OggVorbis,
    MsAdpcm,
    Hca,
    Other(u32),
}

impl ScdCodec {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => ScdCodec::Pcm,
            6 => ScdCodec::OggVorbis,
            12 => ScdCodec::MsAdpcm,
            26 => ScdCodec::Hca,
            other => ScdCodec::Other(other),
        }
    }

    pub fn label(self) -> String {
        match self {
            ScdCodec::Pcm => "pcm16".into(),
            ScdCodec::OggVorbis => "ogg-vorbis".into(),
            ScdCodec::MsAdpcm => "ms-adpcm".into(),
            ScdCodec::Hca => "hca".into(),
            ScdCodec::Other(v) => format!("codec {v}"),
        }
    }
}

/// The optional `MARK` chunk: loop points and cue markers in samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScdMarker {
    pub loop_start: i32,
    pub loop_end: i32,
    pub markers: Vec<i32>,
}

#[derive(Debug, Clone)]
pub struct ScdEntry {
    /// Slot index in the file.
    pub index: usize,
    pub stream_size: u32,
    pub channels: u16,
    pub sample_rate: u32,
    pub codec: ScdCodec,
    /// Raw loop values: bytes into the stream for Ogg, samples for ADPCM/PCM.
    pub loop_start: u32,
    pub loop_end: u32,
    pub flags: u32,
    pub marker: Option<ScdMarker>,
    /// Codec sub-header (MARK chunk stripped).
    pub extradata: Vec<u8>,
    pub stream: Vec<u8>,
}

/// A sound program (VFXEditor `ScdSoundEntry`): what the game plays when it asks for sound
/// index N of the file — one audio entry, or a random pick over several.
#[derive(Debug, Clone, PartialEq)]
pub struct ScdSound {
    /// VFXEditor `SoundType`: 1 normal, 2 random, 4 cycle, 12 group random, 13 group order.
    pub kind: u8,
    pub volume: f32,
    /// Audio entry indices, in track order.
    pub audio: Vec<usize>,
    /// Cumulative pick weights of the random kinds (e.g. 33, 66, 100).
    pub weights: Vec<i16>,
}

#[derive(Debug, Clone)]
pub struct ScdFile {
    /// Audio entries, one slot per table entry; `None` for empty slots.
    pub entries: Vec<Option<ScdEntry>>,
    /// Sound programs; `C063`/`C042` sound ids index this table.
    pub sounds: Vec<ScdSound>,
}

/// Decoded PCM.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
    /// Loop in sample frames.
    pub loop_range: Option<(u64, u64)>,
}

impl Decoded {
    pub fn frames(&self) -> u64 {
        self.samples.len() as u64 / self.channels.max(1) as u64
    }
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| anyhow!("scd truncated at {o:#x}"))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| anyhow!("scd truncated at {o:#x}"))
}
fn i32_at(d: &[u8], o: usize) -> Result<i32> {
    u32_at(d, o).map(|v| v as i32)
}

impl ScdFile {
    pub fn parse(d: &[u8]) -> Result<ScdFile> {
        if d.len() < 0x30 || &d[0..8] != b"SEDBSSCF" {
            bail!("not an SCD file ({} bytes)", d.len());
        }
        let tables = u16_at(d, 0x0E)? as usize;
        let program_count = u16_at(d, tables)? as usize;
        let audio_count = u16_at(d, tables + 0x04)? as usize;
        let entry_table = u32_at(d, tables + 0x0C)? as usize;
        let mut entries = Vec::with_capacity(audio_count);
        for i in 0..audio_count {
            let off = u32_at(d, entry_table + 4 * i)? as usize;
            if off == 0 {
                entries.push(None);
                continue;
            }
            entries.push(Self::parse_entry(d, i, off)?);
        }
        // The sound-program offsets follow the 0x20-byte table header directly.
        let mut sounds = Vec::with_capacity(program_count);
        for i in 0..program_count {
            let off = u32_at(d, tables + 0x20 + 4 * i)? as usize;
            match Self::parse_sound(d, off) {
                Some(s) => sounds.push(s),
                None => sounds.push(ScdSound { kind: 0, volume: 1.0, audio: Vec::new(), weights: Vec::new() }),
            }
        }
        Ok(ScdFile { entries, sounds })
    }

    /// VFXEditor `ScdSoundEntry.Read`: 16-byte header, optional attribute blocks, then the
    /// track list. Only the `Extra_Desc` block (attribute 0x2000, self-sized) is understood;
    /// programs with other attribute blocks are left empty.
    fn parse_sound(d: &[u8], off: usize) -> Option<ScdSound> {
        if off == 0 || off + 16 > d.len() {
            return None;
        }
        let track_count = d[off] as usize;
        let kind = d[off + 3];
        let attrs = u32_at(d, off + 4).ok()?;
        let volume = f32::from_le_bytes([d[off + 8], d[off + 9], d[off + 10], d[off + 11]]);
        let mut p = off + 16;
        const EXTRA: u32 = 0x2000;
        const UNKNOWN_BLOCKS: u32 = 0x0040 | 0x0100 | 0x0400 | 0x0800 | 0x8000;
        if attrs & UNKNOWN_BLOCKS != 0 {
            return None;
        }
        if attrs & EXTRA != 0 {
            let size = u16_at(d, p + 2).ok()? as usize;
            p += if size == 0 { 16 } else { size };
        }
        let random = matches!(kind, 2 | 4 | 12 | 13);
        let mut audio = Vec::with_capacity(track_count);
        let mut weights = Vec::new();
        for _ in 0..track_count {
            let a = u16_at(d, p + 2).ok()? as usize;
            audio.push(a);
            if random {
                weights.push(u16_at(d, p + 4).ok()? as i16);
                p += 8;
            } else {
                p += 4;
            }
        }
        Some(ScdSound { kind, volume, audio, weights })
    }

    /// Audio entries a sound program plays, as present slots (empty when unknown).
    pub fn program_audio(&self, sound_id: usize) -> Vec<&ScdEntry> {
        self.sounds
            .get(sound_id)
            .map(|s| s.audio.iter().filter_map(|&i| self.entries.get(i).and_then(|e| e.as_ref())).collect())
            .unwrap_or_default()
    }

    fn parse_entry(d: &[u8], index: usize, off: usize) -> Result<Option<ScdEntry>> {
        let stream_size = u32_at(d, off)?;
        let channels = u32_at(d, off + 4)? as u16;
        let sample_rate = u32_at(d, off + 8)?;
        let codec_raw = u32_at(d, off + 12)?;
        let loop_start = u32_at(d, off + 16)?;
        let loop_end = u32_at(d, off + 20)?;
        let subinfo = u32_at(d, off + 24)? as usize;
        let flags = u32_at(d, off + 28)?;
        if codec_raw == 0xFFFF_FFFF || stream_size == 0 {
            return Ok(None);
        }
        let mut p = off + 0x20;
        let subinfo_end = p + subinfo;
        let mut marker = None;
        if flags & 1 != 0 && d.get(p..p + 4) == Some(b"MARK") {
            let size = u32_at(d, p + 4)? as usize;
            let ls = i32_at(d, p + 8)?;
            let le = i32_at(d, p + 12)?;
            let n = u32_at(d, p + 16)? as usize;
            let mut markers = Vec::with_capacity(n.min(1024));
            for k in 0..n.min(1024) {
                markers.push(i32_at(d, p + 20 + 4 * k)?);
            }
            marker = Some(ScdMarker { loop_start: ls, loop_end: le, markers });
            let padded = (20 + 4 * n).div_ceil(16) * 16;
            p += if size >= 20 { size } else { padded };
        }
        if p > subinfo_end || subinfo_end > d.len() {
            bail!("scd entry {index}: sub-info runs past the file");
        }
        let extradata = d[p..subinfo_end].to_vec();
        let stream_end = subinfo_end + stream_size as usize;
        if stream_end > d.len() {
            bail!("scd entry {index}: stream ({stream_size} bytes at {subinfo_end:#x}) runs past the file ({} bytes)", d.len());
        }
        let stream = d[subinfo_end..stream_end].to_vec();
        Ok(Some(ScdEntry {
            index,
            stream_size,
            channels,
            sample_rate,
            codec: ScdCodec::from_u32(codec_raw),
            loop_start,
            loop_end,
            flags,
            marker,
            extradata,
            stream,
        }))
    }

    /// Entries that hold audio, in slot order.
    pub fn present(&self) -> impl Iterator<Item = &ScdEntry> {
        self.entries.iter().flatten()
    }
}

/// Ogg Vorbis sub-header of an SCD entry.
#[derive(Debug, Clone)]
pub struct VorbisInfo {
    pub encode_mode: u16,
    pub encode_byte: u8,
    pub seek_step: f32,
    pub seek_table: Vec<u32>,
    pub header_size: usize,
}

impl ScdEntry {
    pub fn vorbis_info(&self) -> Result<VorbisInfo> {
        let e = &self.extradata;
        if e.len() < 0x20 {
            bail!("vorbis sub-header too short ({} bytes)", e.len());
        }
        if &e[16..19] == b"vor" || e.get(16..20).is_some_and(|b| b.ends_with(b"vor")) {
            bail!("legacy vorbis SCD layout is not supported");
        }
        let encode_mode = u16_at(e, 0)?;
        let encode_byte = u16_at(e, 2)? as u8;
        let seek_step = f32::from_le_bytes([e[12], e[13], e[14], e[15]]);
        let seek_table_size = u32_at(e, 16)? as usize;
        let header_size = u32_at(e, 20)? as usize;
        if 0x20 + seek_table_size + header_size > e.len() {
            bail!("vorbis sub-header claims {seek_table_size}+{header_size} bytes, has {}", e.len() - 0x20);
        }
        let seek_table = (0..seek_table_size / 4).map(|i| u32_at(e, 0x20 + 4 * i)).collect::<Result<Vec<_>>>()?;
        Ok(VorbisInfo { encode_mode, encode_byte, seek_step, seek_table, header_size })
    }

    /// The entry as a plain Ogg Vorbis file (descrambled header + stream).
    pub fn ogg_bytes(&self) -> Result<Vec<u8>> {
        if self.codec != ScdCodec::OggVorbis {
            bail!("entry {} is {}, not ogg vorbis", self.index, self.codec.label());
        }
        let info = self.vorbis_info()?;
        let start = 0x20 + info.seek_table.len() * 4;
        let mut header = self.extradata[start..start + info.header_size].to_vec();
        if info.encode_mode == 0x2002 && info.encode_byte != 0 {
            for b in &mut header {
                *b ^= info.encode_byte;
            }
        }
        let mut out = header;
        out.extend_from_slice(&self.stream);
        if info.encode_mode == 0x2003 {
            xor_decode_table(&mut out, self.stream.len());
        }
        if out.get(0..4) != Some(b"OggS") {
            bail!("entry {}: descrambled header does not start with OggS (mode {:#x})", self.index, info.encode_mode);
        }
        Ok(out)
    }

    /// Decode to PCM. Loop points come out in sample frames.
    pub fn decode(&self) -> Result<Decoded> {
        match self.codec {
            ScdCodec::OggVorbis => self.decode_vorbis(),
            ScdCodec::MsAdpcm => {
                let format = MsAdpcmFormat::parse(&self.extradata)?;
                let samples = format.decode(&self.stream);
                let mut out = Decoded { sample_rate: format.sample_rate, channels: format.channels, samples, loop_range: None };
                out.loop_range = self.sample_loop(out.frames());
                Ok(out)
            }
            ScdCodec::Pcm => {
                let samples = self.stream.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                let mut out = Decoded { sample_rate: self.sample_rate, channels: self.channels.max(1), samples, loop_range: None };
                out.loop_range = self.sample_loop(out.frames());
                Ok(out)
            }
            other => bail!("entry {}: {} is not supported", self.index, other.label()),
        }
    }

    /// Loop range for codecs whose loop values are samples.
    fn sample_loop(&self, frames: u64) -> Option<(u64, u64)> {
        let (s, e) = match &self.marker {
            Some(m) if m.loop_end > 0 => (m.loop_start.max(0) as u64, m.loop_end as u64),
            _ if self.loop_end > 0 => (self.loop_start as u64, self.loop_end as u64),
            _ => return None,
        };
        (e > s && s < frames).then(|| (s, e.min(frames)))
    }

    fn decode_vorbis(&self) -> Result<Decoded> {
        let ogg = self.ogg_bytes()?;
        let pages = ogg_pages(&ogg);
        let mut rdr = lewton::inside_ogg::OggStreamReader::new(Cursor::new(&ogg)).map_err(|e| anyhow!("vorbis: {e:?}"))?;
        let channels = rdr.ident_hdr.audio_channels as u16;
        let sample_rate = rdr.ident_hdr.audio_sample_rate;
        let mut samples: Vec<i16> = Vec::with_capacity(self.stream.len() * 8);
        while let Some(pck) = rdr.read_dec_packet_itl().map_err(|e| anyhow!("vorbis: {e:?}"))? {
            samples.extend_from_slice(&pck);
        }
        let mut out = Decoded { sample_rate, channels: channels.max(1), samples, loop_range: None };
        let frames = out.frames();
        let loop_range = match &self.marker {
            Some(m) if m.loop_end > 0 => Some((m.loop_start.max(0) as u64, m.loop_end as u64)),
            _ if self.loop_end > 0 => {
                // Byte offsets into the stream, which starts after the header bytes.
                let header_len = ogg.len() - self.stream.len();
                let to_frames = |byte: u32| -> u64 {
                    let abs = header_len + byte as usize;
                    if byte as usize >= self.stream.len() {
                        return frames;
                    }
                    // Samples decoded before the page that starts at (or contains) `abs`.
                    let idx = pages.iter().rposition(|(o, _)| *o <= abs).unwrap_or(0);
                    if idx == 0 { 0 } else { pages[idx - 1].1.max(0) as u64 }
                };
                Some((to_frames(self.loop_start), to_frames(self.loop_end)))
            }
            _ => None,
        };
        out.loop_range = loop_range.filter(|(s, e)| e > s && *s < frames).map(|(s, e)| (s, e.min(frames)));
        Ok(out)
    }
}

/// Walk Ogg pages: (byte offset, granule position) per page.
pub fn ogg_pages(d: &[u8]) -> Vec<(usize, i64)> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while o + 27 <= d.len() && &d[o..o + 4] == b"OggS" {
        let granule = i64::from_le_bytes(d[o + 6..o + 14].try_into().unwrap());
        let nsegs = d[o + 26] as usize;
        if o + 27 + nsegs > d.len() {
            break;
        }
        let body: usize = d[o + 27..o + 27 + nsegs].iter().map(|&s| s as usize).sum();
        out.push((o, granule));
        o += 27 + nsegs + body;
    }
    out
}

/// VFXEditor `ScdUtils.XorDecodeFromTableVorbis`: the whole-file scramble of encode mode 0x2003.
fn xor_decode_table(data: &mut [u8], data_length: usize) {
    let byte1 = (data_length & 0x7F) as u8;
    let byte2 = byte1 & 0x3F;
    for (i, b) in data.iter_mut().enumerate() {
        *b = XOR_TABLE[(byte2 as usize + i) & 0xFF] ^ *b ^ byte1;
    }
}

const XOR_TABLE: [u8; 256] = [
    0x3A, 0x32, 0x32, 0x32, 0x03, 0x7E, 0x12, 0xF7, 0xB2, 0xE2, 0xA2, 0x67, 0x32, 0x32, 0x22, 0x32, 0x32, 0x52, 0x16, 0x1B, 0x3C, 0xA1, 0x54, 0x7B, 0x1B, 0x97, 0xA6, 0x93, 0x1A, 0x4B, 0xAA, 0xA6, 0x7A, 0x7B, 0x1B, 0x97, 0xA6, 0xF7, 0x02, 0xBB, 0xAA, 0xA6, 0xBB, 0xF7, 0x2A, 0x51, 0xBE, 0x03, 0xF4, 0x2A, 0x51, 0xBE, 0x03, 0xF4, 0x2A, 0x51,
    0xBE, 0x12, 0x06, 0x56, 0x27, 0x32, 0x32, 0x36, 0x32, 0xB2, 0x1A, 0x3B, 0xBC, 0x91, 0xD4, 0x7B, 0x58, 0xFC, 0x0B, 0x55, 0x2A, 0x15, 0xBC, 0x40, 0x92, 0x0B, 0x5B, 0x7C, 0x0A, 0x95, 0x12, 0x35, 0xB8, 0x63, 0xD2, 0x0B, 0x3B, 0xF0, 0xC7, 0x14, 0x51, 0x5C, 0x94, 0x86, 0x94, 0x59, 0x5C, 0xFC, 0x1B, 0x17, 0x3A, 0x3F, 0x6B, 0x37, 0x32, 0x32,
    0x30, 0x32, 0x72, 0x7A, 0x13, 0xB7, 0x26, 0x60, 0x7A, 0x13, 0xB7, 0x26, 0x50, 0xBA, 0x13, 0xB4, 0x2A, 0x50, 0xBA, 0x13, 0xB5, 0x2E, 0x40, 0xFA, 0x13, 0x95, 0xAE, 0x40, 0x38, 0x18, 0x9A, 0x92, 0xB0, 0x38, 0x00, 0xFA, 0x12, 0xB1, 0x7E, 0x00, 0xDB, 0x96, 0xA1, 0x7C, 0x08, 0xDB, 0x9A, 0x91, 0xBC, 0x08, 0xD8, 0x1A, 0x86, 0xE2, 0x70, 0x39,
    0x1F, 0x86, 0xE0, 0x78, 0x7E, 0x03, 0xE7, 0x64, 0x51, 0x9C, 0x8F, 0x34, 0x6F, 0x4E, 0x41, 0xFC, 0x0B, 0xD5, 0xAE, 0x41, 0xFC, 0x0B, 0xD5, 0xAE, 0x41, 0xFC, 0x3B, 0x70, 0x71, 0x64, 0x33, 0x32, 0x12, 0x32, 0x32, 0x36, 0x70, 0x34, 0x2B, 0x56, 0x22, 0x70, 0x3A, 0x13, 0xB7, 0x26, 0x60, 0xBA, 0x1B, 0x94, 0xAA, 0x40, 0x38, 0x00, 0xFA, 0xB2,
    0xE2, 0xA2, 0x67, 0x32, 0x32, 0x12, 0x32, 0xB2, 0x32, 0x32, 0x32, 0x32, 0x75, 0xA3, 0x26, 0x7B, 0x83, 0x26, 0xF9, 0x83, 0x2E, 0xFF, 0xE3, 0x16, 0x7D, 0xC0, 0x1E, 0x63, 0x21, 0x07, 0xE3, 0x01,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_table_has_256_entries() {
        assert_eq!(XOR_TABLE.len(), 256);
        assert_eq!(XOR_TABLE[255], 0x01);
        assert_eq!(XOR_TABLE[5], 0x7E);
    }

    #[test]
    fn walks_ogg_pages() {
        let mut d = Vec::new();
        for (granule, body) in [(0i64, 3u8), (1024, 5)] {
            d.extend_from_slice(b"OggS");
            d.push(0);
            d.push(0);
            d.extend_from_slice(&granule.to_le_bytes());
            d.extend_from_slice(&[0; 12]);
            d.push(1);
            d.push(body);
            d.extend(std::iter::repeat_n(0u8, body as usize));
        }
        let pages = ogg_pages(&d);
        assert_eq!(pages, vec![(0, 0), (27 + 1 + 3, 1024)]);
    }

    fn scratch_scd(name: &str) -> Option<Vec<u8>> {
        let dir = std::env::var("FFL_TEST_SCD_DIR").ok()?;
        std::fs::read(format!("{dir}/{name}")).ok()
    }

    #[test]
    #[ignore = "needs FFL_TEST_SCD_DIR with extracted game files"]
    fn housing_day_loops_at_eight_seconds() {
        let Some(bytes) = scratch_scd("housing_day.scd") else { return };
        let scd = ScdFile::parse(&bytes).unwrap();
        let e = scd.present().next().unwrap();
        assert_eq!(e.codec, ScdCodec::OggVorbis);
        assert_eq!(e.channels, 2);
        let d = e.decode().unwrap();
        let (s, en) = d.loop_range.unwrap();
        let start = s as f32 / d.sample_rate as f32;
        assert!((start - 8.41).abs() < 0.05, "loop start {start}");
        assert!(en > s && en <= d.frames());
    }

    #[test]
    #[ignore = "needs FFL_TEST_SCD_DIR with extracted game files"]
    fn footstep_bank_has_eight_variations() {
        let Some(bytes) = scratch_scd("fs_grass_m_f_shoes.scd") else { return };
        let scd = ScdFile::parse(&bytes).unwrap();
        let present: Vec<_> = scd.present().collect();
        assert_eq!(present.len(), 8);
        assert_eq!(scd.sounds.len(), 4);
        assert_eq!(scd.sounds[0].kind, 12);
        assert_eq!(scd.sounds[0].audio, vec![12, 13, 14]);
        assert_eq!(scd.sounds[0].weights, vec![33, 66, 100]);
        assert!((scd.sounds[0].volume - 0.28).abs() < 0.01);
        assert_eq!(scd.sounds[2].audio, vec![0]);
        assert_eq!(scd.program_audio(0).len(), 3);
        for e in present {
            assert_eq!(e.codec, ScdCodec::MsAdpcm);
            let d = e.decode().unwrap();
            assert_eq!(d.channels, 1);
            assert!((43900..44300).contains(&d.sample_rate));
            assert!(d.frames() > 1000);
        }
    }
}
