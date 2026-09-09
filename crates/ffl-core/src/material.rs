//! Data-driven materials. Engines register shaders ([`ShaderDef`]: WGSL source + texture slot
//! layout); each material references a shader id and carries a generic parameter block and
//! named texture slots. The runtime hosts every registered shader side by side, so characters
//! and maps from different engines keep their own game's shading in one world.

use std::sync::Arc;

/// GPU texture format of a [`TextureData`]. Block formats are uploaded as-is (the games ship
/// them that way); uncompressed data gets a generated mip chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// 4 bytes per texel.
    Rgba8,
    /// 2 bytes per texel (two-component normal maps).
    Rg8,
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
}

impl TextureFormat {
    pub fn is_block(self) -> bool {
        !matches!(self, TextureFormat::Rgba8 | TextureFormat::Rg8)
    }

    /// Bytes of one 4x4 block (block formats) or one texel.
    pub fn unit_bytes(self) -> usize {
        match self {
            TextureFormat::Rgba8 => 4,
            TextureFormat::Rg8 => 2,
            TextureFormat::Bc1 | TextureFormat::Bc4 => 8,
            TextureFormat::Bc2 | TextureFormat::Bc3 | TextureFormat::Bc5 | TextureFormat::Bc7 => 16,
        }
    }

    /// Byte size of one mip level of `w` x `h` texels.
    pub fn level_bytes(self, w: u32, h: u32) -> usize {
        if self.is_block() {
            (w.max(1).div_ceil(4) as usize) * (h.max(1).div_ceil(4) as usize) * self.unit_bytes()
        } else {
            w.max(1) as usize * h.max(1) as usize * self.unit_bytes()
        }
    }

    pub fn has_alpha_channel(self) -> bool {
        matches!(self, TextureFormat::Rgba8 | TextureFormat::Bc1 | TextureFormat::Bc2 | TextureFormat::Bc3 | TextureFormat::Bc7)
    }
}

/// CPU-side texture: a mip chain in one [`TextureFormat`].
#[derive(Debug, Clone)]
pub struct TextureData {
    /// Stable key for caching (path plus any generation suffix).
    pub key: String,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    /// Mip levels, largest first, each tightly packed in `format`. Never empty.
    pub mips: Vec<Vec<u8>>,
    /// Any texel with alpha noticeably below 1 (false when unknown).
    pub has_alpha: bool,
    /// Whether the data is colour (sRGB) or linear data (normals, masks).
    pub srgb: bool,
}

impl TextureData {
    /// RGBA8 texture; the mip chain is generated here.
    pub fn rgba8(key: &str, width: u32, height: u32, rgba: Vec<u8>, srgb: bool) -> TextureData {
        let has_alpha = rgba.chunks_exact(4).any(|p| p[3] < 250);
        let mut t = TextureData {
            key: key.to_string(),
            width,
            height,
            format: TextureFormat::Rgba8,
            mips: vec![rgba],
            has_alpha,
            srgb,
        };
        t.generate_mips();
        t
    }

    /// Two-component (RG) texture from RGBA8 input, mips generated.
    pub fn rg8_from_rgba(key: &str, width: u32, height: u32, rgba: &[u8]) -> TextureData {
        let rg: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1]]).collect();
        let mut t = TextureData {
            key: key.to_string(),
            width,
            height,
            format: TextureFormat::Rg8,
            mips: vec![rg],
            has_alpha: false,
            srgb: false,
        };
        t.generate_mips();
        t
    }

    /// Block-compressed texture whose mip chain is stored contiguously in `data` (largest
    /// first, `mip_levels` levels). Levels the data does not cover are dropped.
    pub fn block(key: &str, width: u32, height: u32, format: TextureFormat, data: &[u8], mip_levels: u32, srgb: bool) -> TextureData {
        let mut mips = Vec::new();
        let mut offset = 0;
        for level in 0..mip_levels.max(1) {
            let (w, h) = (width >> level, height >> level);
            if (w == 0 && h == 0) || level > 0 && (w < 1 || h < 1) {
                break;
            }
            let size = format.level_bytes(w, h);
            if offset + size > data.len() {
                break;
            }
            mips.push(data[offset..offset + size].to_vec());
            offset += size;
        }
        if mips.is_empty() {
            mips.push(data[..data.len().min(format.level_bytes(width, height))].to_vec());
        }
        TextureData {
            key: key.to_string(),
            width,
            height,
            format,
            mips,
            has_alpha: false,
            srgb,
        }
    }

    pub fn mip_count(&self) -> u32 {
        self.mips.len() as u32
    }

    pub fn total_bytes(&self) -> usize {
        self.mips.iter().map(Vec::len).sum()
    }

    /// Full mip chain down to 1x1 by 2x2 box filtering (uncompressed formats only; a no-op for
    /// block formats, which arrive with their own chain).
    pub fn generate_mips(&mut self) {
        if self.format.is_block() || self.mips.len() != 1 {
            return;
        }
        let bpp = self.format.unit_bytes();
        let (mut w, mut h) = (self.width.max(1), self.height.max(1));
        while w > 1 || h > 1 {
            let src = self.mips.last().unwrap();
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let mut dst = vec![0u8; nw as usize * nh as usize * bpp];
            for y in 0..nh as usize {
                let y0 = (y * 2).min(h as usize - 1);
                let y1 = (y * 2 + 1).min(h as usize - 1);
                for x in 0..nw as usize {
                    let x0 = (x * 2).min(w as usize - 1);
                    let x1 = (x * 2 + 1).min(w as usize - 1);
                    for c in 0..bpp {
                        let sum = src[(y0 * w as usize + x0) * bpp + c] as u32
                            + src[(y0 * w as usize + x1) * bpp + c] as u32
                            + src[(y1 * w as usize + x0) * bpp + c] as u32
                            + src[(y1 * w as usize + x1) * bpp + c] as u32;
                        dst[(y * nw as usize + x) * bpp + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
            self.mips.push(dst);
            w = nw;
            h = nh;
        }
    }
}

pub type TextureRef = Arc<TextureData>;

/// Maximum generic parameter block size (vec4 slots) a shader may use.
pub const MAX_PARAMS: usize = 128;
/// Number of generic texture slots a shader may use (`tex0`..`tex5`).
pub const MAX_TEXTURE_SLOTS: usize = 6;

/// One generic texture slot of a shader.
#[derive(Debug, Clone)]
pub struct TextureSlotDef {
    /// Name materials use to fill the slot (e.g. `index`, `mask`).
    pub name: String,
    /// Upload as sRGB colour (true) or linear data (false).
    pub srgb: bool,
}

/// A shader an engine contributes. The WGSL `source` must define a function
///
/// ```wgsl
/// fn <entry>(in: VertexOutput, pbr: ptr<function, PbrInput>, base: vec4<f32>) -> vec4<f32>
/// ```
///
/// that returns the base colour (rgba) and may modify the PBR inputs (roughness, metallic,
/// reflectance, emissive). It can read `params.data[1..]` (vec4 parameter block; slot 0 is
/// reserved) and sample `tex0..tex5` with `samp0..samp5` according to its slot layout.
/// `in.uv`, `in.uv_b` (behind `VERTEX_UVS_B`) and `in.color` (behind `VERTEX_COLORS`) are
/// available like in any Bevy PBR shader.
#[derive(Debug, Clone)]
pub struct ShaderDef {
    /// Globally unique id, conventionally `<engine>/<name>`.
    pub id: String,
    pub entry: String,
    pub source: String,
    pub slots: Vec<TextureSlotDef>,
}

/// Built-in shader ids the runtime always provides.
pub mod builtin {
    /// Plain PBR from the base settings only.
    pub const PBR: &str = "core/pbr";
    /// Unlit base colour.
    pub const UNLIT: &str = "core/unlit";
    /// Translucent water plane.
    pub const WATER: &str = "core/water";
    /// Do not render.
    pub const SKIP: &str = "core/skip";
}

#[derive(Debug, Clone)]
pub struct MaterialDesc {
    /// Stable key for caching.
    pub key: String,
    /// Shader id: a builtin or one registered by an engine.
    pub shader: String,
    /// Base colour factor (rgba, sRGB-ish 0..1).
    pub base_color: [f32; 4],
    /// PBR base colour texture (sRGB).
    pub diffuse: Option<TextureRef>,
    /// Tangent-space normal map (RG used).
    pub normal: Option<TextureRef>,
    /// Generic parameter block for the engine shader (slot 0 is reserved by the runtime).
    pub params: Vec<[f32; 4]>,
    /// Engine shader texture slots by slot name.
    pub slots: Vec<(String, TextureRef)>,
    pub alpha_mask: bool,
    /// Alpha-mask cutoff (only used when `alpha_mask`).
    pub alpha_cutoff: f32,
    pub alpha_blend: bool,
    /// Multiply the colour over what is already drawn (shadow overlays).
    pub multiply: bool,
    pub double_sided: bool,
    pub roughness: f32,
    pub metallic: f32,
    /// Ground material for footsteps when a character stands on this surface.
    pub surface: crate::sound::SurfaceKind,
}

impl MaterialDesc {
    pub fn new(key: &str, shader: &str) -> Self {
        Self {
            key: key.to_string(),
            shader: shader.to_string(),
            base_color: [1.0; 4],
            diffuse: None,
            normal: None,
            params: Vec::new(),
            slots: Vec::new(),
            alpha_mask: false,
            alpha_cutoff: 0.5,
            alpha_blend: false,
            multiply: false,
            double_sided: false,
            roughness: 0.8,
            metallic: 0.0,
            surface: crate::sound::SurfaceKind::Unknown,
        }
    }

    pub fn unlit(key: &str, color: [f32; 4]) -> Self {
        let mut m = Self::new(key, builtin::UNLIT);
        m.base_color = color;
        m.double_sided = true;
        m
    }

    pub fn water(key: &str) -> Self {
        let mut m = Self::new(key, builtin::WATER);
        m.base_color = [0.25, 0.5, 0.75, 0.6];
        m.alpha_blend = true;
        m.double_sided = true;
        m.surface = crate::sound::SurfaceKind::Water;
        m
    }

    pub fn slot(&self, name: &str) -> Option<&TextureRef> {
        self.slots.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    pub fn is_skip(&self) -> bool {
        self.shader == builtin::SKIP
    }
}
