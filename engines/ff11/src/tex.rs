//! FFXI `IMG` sections → RGBA8. Layout (IMGINFO in the TDW viewer): flag byte, 16-byte name,
//! a BITMAPINFOHEADER-like block (size 40, width, height, planes|bpp, ...), the palette
//! entry width, then either a 256-entry palette + indexed pixels (bottom-up rows), raw 32-bit
//! pixels, or a `DXT1`/`DXT3` block stream. Alpha is stored 0..0x80 (0x80 = opaque).

use std::sync::Arc;

use ffl_core::{TextureData, TextureRef};

#[derive(Debug, Clone)]
pub struct Img {
    pub name: [u8; 16],
    pub texture: TextureRef,
}

fn u32_at(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}

fn alpha7(a: u32) -> u8 {
    (a.min(0x80) * 255 / 0x80) as u8
}

/// Palette dword 0xAARRGGBB (alpha 0..0x80) → RGBA8.
fn pal_rgba(v: u32) -> [u8; 4] {
    [((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8, alpha7(v >> 24)]
}

fn rgb565(c: u16) -> [u8; 4] {
    let r = ((c >> 11) & 0x1F) as u32;
    let g = ((c >> 5) & 0x3F) as u32;
    let b = (c & 0x1F) as u32;
    [((r * 255 + 15) / 31) as u8, ((g * 255 + 31) / 63) as u8, ((b * 255 + 15) / 31) as u8, 255]
}

fn decode_bc1_block(block: &[u8], out: &mut [[u8; 4]; 16]) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));
    let mix = |x: [u8; 4], y: [u8; 4], wx: u32, wy: u32| -> [u8; 4] {
        let f = |i: usize| ((x[i] as u32 * wx + y[i] as u32 * wy) / (wx + wy)) as u8;
        [f(0), f(1), f(2), 255]
    };
    let palette = if c0 > c1 {
        [a, b, mix(a, b, 2, 1), mix(a, b, 1, 2)]
    } else {
        [a, b, mix(a, b, 1, 1), [0, 0, 0, 0]]
    };
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    for i in 0..16 {
        out[i] = palette[((bits >> (2 * i)) & 3) as usize];
    }
}

fn decode_dxt(data: &[u8], width: usize, height: usize, dxt3: bool) -> Option<Vec<u8>> {
    let bw = width.div_ceil(4);
    let bh = height.div_ceil(4);
    let block_size = if dxt3 { 16 } else { 8 };
    if data.len() < bw * bh * block_size {
        return None;
    }
    let mut rgba = vec![0u8; width * height * 4];
    let mut px = [[0u8; 4]; 16];
    for by in 0..bh {
        for bx in 0..bw {
            let block = &data[(by * bw + bx) * block_size..];
            let color = if dxt3 { &block[8..16] } else { &block[0..8] };
            decode_bc1_block(color, &mut px);
            if dxt3 {
                for i in 0..16 {
                    // FFXI alpha is half-range everywhere: 0x8 of 0xF is opaque.
                    let nib = ((block[i / 2] >> ((i % 2) * 4)) & 0xF) as u32;
                    px[i][3] = (nib * 34).min(255) as u8;
                }
            }
            for i in 0..16 {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x < width && y < height {
                    let o = (y * width + x) * 4;
                    rgba[o..o + 4].copy_from_slice(&px[i]);
                }
            }
        }
    }
    Some(rgba)
}

/// Decode one IMG section payload. Returns `None` for sections that are not images.
pub fn decode_img(key_prefix: &str, p: &[u8]) -> Option<Img> {
    if p.len() < 1081 {
        return None;
    }
    let flag = p[0];
    let mut name = [0u8; 16];
    name.copy_from_slice(&p[1..17]);
    if u32_at(p, 17) != 40 {
        return None;
    }
    let width = u32_at(p, 21) as i32;
    let height = u32_at(p, 25) as i32;
    if !(4..=4096).contains(&width) || !(4..=4096).contains(&height) {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let planes_bpp = u32_at(p, 29);
    if !matches!(planes_bpp, 0x80001 | 0x40001 | 0x200001) {
        return None;
    }
    let pal_width = u32_at(p, 53);
    let pal_off = 57usize;
    let fourcc = &p[pal_off..pal_off + 4];
    let (rgba, has_alpha) = if fourcc == b"3TXD" || fourcc == b"1TXD" {
        // Multi-char constants 'DXT3' / 'DXT1' stored little-endian.
        let dxt3 = fourcc == b"3TXD";
        let data = &p[pal_off + 12..];
        let rgba = decode_dxt(data, w, h, dxt3)?;
        (rgba, true)
    } else {
        let (pal, pixels) = if flag == 0xB1 { (pal_off + 4, pal_off + 4 + 1024) } else { (pal_off, pal_off + 1024) };
        let mut rgba = vec![0u8; w * h * 4];
        let mut any_alpha = false;
        for row in 0..h {
            let dst_row = h - 1 - row; // bottom-up rows
            for x in 0..w {
                let px = match planes_bpp {
                    0x200001 => {
                        let o = pal_off + (row * w + x) * 4;
                        if o + 4 > p.len() {
                            return None;
                        }
                        pal_rgba(u32_at(p, o))
                    }
                    0x40001 => {
                        let o = pixels + (row * w + x) / 2;
                        if o >= p.len() {
                            return None;
                        }
                        let idx = if x % 2 == 0 { p[o] >> 4 } else { p[o] & 0xF } as usize;
                        palette_entry(p, pal, pal_width, idx)
                    }
                    _ => {
                        let o = pixels + row * w + x;
                        if o >= p.len() {
                            return None;
                        }
                        palette_entry(p, pal, pal_width, p[o] as usize)
                    }
                };
                if px[3] < 250 {
                    any_alpha = true;
                }
                let o = (dst_row * w + x) * 4;
                rgba[o..o + 4].copy_from_slice(&px);
            }
        }
        (rgba, any_alpha)
    };
    let has_alpha = has_alpha && rgba.chunks_exact(4).any(|c| c[3] < 250);
    let name_str = String::from_utf8_lossy(&name).trim_end_matches(['\0', ' ']).to_string();
    Some(Img {
        name,
        texture: Arc::new({
            let mut t = TextureData::rgba8(&format!("{key_prefix}/{name_str}"), w as u32, h as u32, rgba, true);
            t.has_alpha = has_alpha;
            t
        }),
    })
}

fn palette_entry(p: &[u8], pal: usize, pal_width: u32, idx: usize) -> [u8; 4] {
    if pal_width == 16 {
        let o = pal + idx * 2;
        let v = u16::from_le_bytes([p[o], p[o + 1]]) as u32;
        let r = ((v & 0x1F) << 3) as u8;
        let g = (((v >> 5) & 0x1F) << 3) as u8;
        let b = (((v >> 10) & 0x1F) << 3) as u8;
        let a = if v & 0x8000 != 0 { 255 } else { 0 };
        [r, g, b, a]
    } else {
        pal_rgba(u32_at(p, pal + idx * 4))
    }
}
