//! Microsoft ADPCM (WAVE_FORMAT_ADPCM, format tag 2) decoder, the codec of FF14's sound
//! effect SCD entries. The `WAVEFORMATEX` sub-header of the entry carries the block size,
//! samples per block and the coefficient table.

use anyhow::{Result, anyhow, bail};

/// The parts of a `WAVEFORMATEX` + ADPCM extension we need.
#[derive(Debug, Clone)]
pub struct MsAdpcmFormat {
    pub channels: u16,
    pub sample_rate: u32,
    pub block_align: u16,
    pub samples_per_block: u16,
    pub coefficients: Vec<(i32, i32)>,
}

const ADAPTATION: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];
const DEFAULT_COEFFICIENTS: [(i32, i32); 7] = [(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| anyhow!("WAVEFORMATEX truncated at {o}"))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| anyhow!("WAVEFORMATEX truncated at {o}"))
}

impl MsAdpcmFormat {
    /// Parse a `WAVEFORMATEX` (18 bytes) followed by the ADPCM extension.
    pub fn parse(d: &[u8]) -> Result<Self> {
        let tag = u16_at(d, 0)?;
        if tag != 2 {
            bail!("not an MS-ADPCM WAVEFORMATEX (format tag {tag})");
        }
        let channels = u16_at(d, 2)?;
        let sample_rate = u32_at(d, 4)?;
        let block_align = u16_at(d, 12)?;
        let bits = u16_at(d, 14)?;
        if bits != 4 {
            bail!("MS-ADPCM with {bits} bits per sample");
        }
        let cb_size = u16_at(d, 16)? as usize;
        let (samples_per_block, coefficients) = if cb_size >= 4 {
            let spb = u16_at(d, 18)?;
            let n = u16_at(d, 20)? as usize;
            let mut coefs = Vec::with_capacity(n);
            for i in 0..n {
                let c1 = u16_at(d, 22 + i * 4)? as i16 as i32;
                let c2 = u16_at(d, 24 + i * 4)? as i16 as i32;
                coefs.push((c1, c2));
            }
            (spb, coefs)
        } else {
            let spb = ((block_align as usize - 7 * channels as usize) * 2 / channels as usize + 2) as u16;
            (spb, DEFAULT_COEFFICIENTS.to_vec())
        };
        if channels == 0 || channels > 2 || block_align == 0 || coefficients.is_empty() {
            bail!("unsupported MS-ADPCM layout: {channels} ch, block {block_align}, {} coefs", coefficients.len());
        }
        Ok(Self { channels, sample_rate, block_align, samples_per_block, coefficients })
    }

    /// Decode a whole stream to interleaved 16-bit PCM.
    pub fn decode(&self, data: &[u8]) -> Vec<i16> {
        let ch = self.channels as usize;
        let block = self.block_align as usize;
        let mut out = Vec::with_capacity(data.len() * 2);
        for chunk in data.chunks(block) {
            let header = 7 * ch;
            if chunk.len() < header {
                break;
            }
            let mut predictor = [0usize; 2];
            let mut delta = [0i32; 2];
            let mut s1 = [0i32; 2];
            let mut s2 = [0i32; 2];
            for c in 0..ch {
                predictor[c] = (chunk[c] as usize).min(self.coefficients.len() - 1);
            }
            let mut o = ch;
            for c in 0..ch {
                delta[c] = i16::from_le_bytes([chunk[o], chunk[o + 1]]) as i32;
                o += 2;
            }
            for c in 0..ch {
                s1[c] = i16::from_le_bytes([chunk[o], chunk[o + 1]]) as i32;
                o += 2;
            }
            for c in 0..ch {
                s2[c] = i16::from_le_bytes([chunk[o], chunk[o + 1]]) as i32;
                o += 2;
            }
            let mut written = 0usize;
            let limit = self.samples_per_block as usize;
            for c in 0..ch {
                out.push(s2[c] as i16);
            }
            for c in 0..ch {
                out.push(s1[c] as i16);
            }
            written += 2;
            let mut channel = 0usize;
            'nibbles: for &byte in &chunk[header..] {
                for nibble in [byte >> 4, byte & 0x0F] {
                    if written >= limit {
                        break 'nibbles;
                    }
                    let c = channel;
                    let (c1, c2) = self.coefficients[predictor[c]];
                    let pred = (s1[c] * c1 + s2[c] * c2) >> 8;
                    let signed = if nibble & 8 != 0 { nibble as i32 - 16 } else { nibble as i32 };
                    let sample = (pred + signed * delta[c]).clamp(-32768, 32767);
                    out.push(sample as i16);
                    s2[c] = s1[c];
                    s1[c] = sample;
                    delta[c] = ((ADAPTATION[nibble as usize] * delta[c]) >> 8).max(16);
                    channel += 1;
                    if channel == ch {
                        channel = 0;
                        written += 1;
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(channels: u16, block_align: u16, spb: u16) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&channels.to_le_bytes());
        d.extend_from_slice(&44100u32.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&block_align.to_le_bytes());
        d.extend_from_slice(&4u16.to_le_bytes());
        d.extend_from_slice(&32u16.to_le_bytes());
        d.extend_from_slice(&spb.to_le_bytes());
        d.extend_from_slice(&7u16.to_le_bytes());
        for (c1, c2) in DEFAULT_COEFFICIENTS {
            d.extend_from_slice(&(c1 as i16).to_le_bytes());
            d.extend_from_slice(&(c2 as i16).to_le_bytes());
        }
        d
    }

    #[test]
    fn parses_wave_format() {
        let f = MsAdpcmFormat::parse(&format(1, 70, 128)).unwrap();
        assert_eq!(f.channels, 1);
        assert_eq!(f.block_align, 70);
        assert_eq!(f.samples_per_block, 128);
        assert_eq!(f.coefficients.len(), 7);
    }

    #[test]
    fn decodes_constant_block() {
        // Predictor 0 (c1 = 256, c2 = 0): the prediction is the previous sample, so zero nibbles
        // hold the value; nibble 1 adds one delta step.
        let f = MsAdpcmFormat::parse(&format(1, 11, 10)).unwrap();
        let mut block = vec![0u8]; // predictor
        block.extend_from_slice(&16i16.to_le_bytes()); // delta
        block.extend_from_slice(&100i16.to_le_bytes()); // sample1
        block.extend_from_slice(&50i16.to_le_bytes()); // sample2
        block.extend_from_slice(&[0x00, 0x10, 0x00, 0x00]); // nibbles 0,0,1,0,0,0,0,0
        let out = f.decode(&block);
        assert_eq!(out, vec![50, 100, 100, 100, 116, 116, 116, 116, 116, 116]);
    }
}
