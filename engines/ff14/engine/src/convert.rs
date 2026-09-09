//! Physis → `ffl_core` conversion: models, textures and materials.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ffl_core::{SurfaceKind, MaterialDesc, MeshData, ModelData, TextureData, TextureFormat, TextureRef};

use crate::shaders;
use ffl_ff14_assets::AssetSource;
use ffl_ff14_assets::loaders::load_tex;
use physis::model::{MDL, PartType};
use ffl_ff14_chara::meta::stm::DyeTemplates;
use physis::mtrl::{ColorDyeTable, ColorTable, Material};
use physis::tex::Texture;

// Sampler usage CRCs. Background shaders (xivModdingFramework ShaderHelpers):
pub const SAMPLER_COLOR_MAP0: u32 = 0x1E6F_EF9C;
pub const SAMPLER_NORMAL_MAP0: u32 = 0xAAB4_D9E9;
pub const SAMPLER_SPECULAR_MAP0: u32 = 0x1BBC_2F12;
pub const SAMPLER_COLOR_MAP1: u32 = 0x6968_DF0A;
pub const SAMPLER_NORMAL_MAP1: u32 = 0xDDB3_E97F;
pub const SAMPLER_SPECULAR_MAP1: u32 = 0x6CBB_1F84;
// Character shaders (Penumbra ShpkFile.cs):
pub const SAMPLER_NORMAL: u32 = 0x0C5E_C1F1;
pub const SAMPLER_INDEX: u32 = 0x565F_8FD8;
pub const SAMPLER_SPECULAR: u32 = 0x2B99_E025;
pub const SAMPLER_DIFFUSE: u32 = 0x1153_06BE;
pub const SAMPLER_MASK: u32 = 0x8A4E_82B6;
/// `g_SamplerCatchlight`: the scrolling effect texture of characterscroll.shpk.
pub const SAMPLER_CATCHLIGHT: u32 = 0xFEA0_F3D2;

/// Cache key of a game path under a source: mod replacements get their pack appended so
/// they never collide with the game's own file of the same path.
pub fn keyed(source: &dyn AssetSource, path: &str) -> String {
    match source.origin(path) {
        ffl_core::Provenance::Mod(pack) => format!("{path}@{pack}"),
        _ => path.to_string(),
    }
}

/// Texture cache shared by all loads. Block-compressed game textures keep their block data and
/// mip chain; `get_rg` decodes a normal map to a two-component RG8 chain for the PBR base.
#[derive(Default)]
pub struct TextureCache {
    entries: Mutex<HashMap<String, Option<TextureRef>>>,
}

impl TextureCache {
    /// Fetch a texture. `srgb` only affects the hint stored on the data.
    pub fn get(&self, source: &dyn AssetSource, path: &str, srgb: bool) -> Option<TextureRef> {
        let key = keyed(source, path);
        if let Some(e) = self.entries.lock().unwrap().get(&key) {
            return e.clone();
        }
        let decoded = match load_tex(source, path) {
            Ok(t) => prepare_texture(&key, &t, srgb).map(Arc::new),
            Err(err) => {
                tracing::warn!("{err:#}");
                None
            }
        };
        self.entries.lock().unwrap().insert(key, decoded.clone());
        decoded
    }

    /// Fetch a normal map as an RG8 chain (BC5 sources are already two-component and are
    /// returned as-is).
    pub fn get_rg(&self, source: &dyn AssetSource, path: &str) -> Option<TextureRef> {
        let key = format!("{}#rg", keyed(source, path));
        if let Some(e) = self.entries.lock().unwrap().get(&key) {
            return e.clone();
        }
        let decoded = match load_tex(source, path) {
            Ok(t) if matches!(t.format, physis::tex::TextureFormat::BC5_UNORM) => prepare_texture(&key, &t, false).map(Arc::new),
            Ok(t) => t.to_rgba().map(|rgba| {
                let expected = t.width as usize * t.height as usize * 4;
                Arc::new(TextureData::rg8_from_rgba(&key, t.width as u32, t.height as u32, &rgba[..expected.min(rgba.len())]))
            }),
            Err(err) => {
                tracing::warn!("{err:#}");
                None
            }
        };
        self.entries.lock().unwrap().insert(key, decoded.clone());
        decoded
    }
}

/// Record a material and its textures in a content report.
pub fn report_material(report: &mut ffl_core::ContentReport, source: &dyn AssetSource, mtrl_path: &str, m: &Material) {
    report.push(mtrl_path, source.origin(mtrl_path));
    for t in &m.texture_paths {
        if source.exists(t) {
            report.push(t, source.origin(t));
        }
    }
}

fn block_format(f: physis::tex::TextureFormat) -> Option<TextureFormat> {
    use physis::tex::TextureFormat as P;
    Some(match f {
        P::BC1_UNORM => TextureFormat::Bc1,
        P::BC2_UNORM => TextureFormat::Bc2,
        P::BC3_UNORM => TextureFormat::Bc3,
        P::BC4_UNORM => TextureFormat::Bc4,
        P::BC5_UNORM => TextureFormat::Bc5,
        P::BC7_UNORM => TextureFormat::Bc7,
        _ => return None,
    })
}

/// Core texture from a game `.tex`: block formats keep their data and mips (the alpha flag is
/// still computed from a decode of mip 0 for colour textures), everything else is decoded to
/// RGBA8 with a generated chain.
pub fn prepare_texture(path: &str, tex: &Texture, srgb: bool) -> Option<TextureData> {
    let (w, h) = (tex.width as u32, tex.height as u32);
    if let Some(format) = block_format(tex.format) {
        let mut t = TextureData::block(path, w, h, format, &tex.data, tex.mip_levels as u32, srgb);
        if srgb && format.has_alpha_channel() {
            let expected = w as usize * h as usize * 4;
            t.has_alpha = tex.to_rgba().map(|rgba| rgba[..expected.min(rgba.len())].chunks_exact(4).any(|p| p[3] < 250)).unwrap_or(false);
        }
        return Some(t);
    }
    let rgba = tex.to_rgba()?;
    let expected = w as usize * h as usize * 4;
    if rgba.len() < expected {
        tracing::warn!("{path}: decoded {} bytes, expected {expected}", rgba.len());
        return None;
    }
    Some(TextureData::rgba8(path, w, h, rgba[..expected].to_vec(), srgb))
}

/// Geometry of one LOD of a model as core meshes (materials left empty; callers fill them).
pub fn prepare_model(key: &str, mdl: &MDL, lod: usize) -> ModelData {
    let lod_index = lod.min(mdl.lods.len().saturating_sub(1));
    prepare_model_lods(key, mdl, &[lod_index])
}

/// [`prepare_model`] with shape keys applied: every named shape's vertex deltas (Physis
/// `Shape::morphed_vertices`, position differences) are added to the part's vertices.
/// Face models carry the character-creator options as shapes (`shp_eye_c`, `shp_mth_a`, ...).
pub fn prepare_model_shaped(key: &str, mdl: &MDL, lod: usize, shapes: &[String]) -> ModelData {
    let mut model = prepare_model(key, mdl, lod);
    if shapes.is_empty() {
        return model;
    }
    let lod_index = lod.min(mdl.lods.len().saturating_sub(1));
    let Some(lod) = mdl.lods.get(lod_index) else {
        return model;
    };
    let mut mi = 0;
    for part in &lod.parts {
        if part.vertices.is_empty() || part.indices.len() < 3 {
            continue;
        }
        if let Some(mesh) = model.meshes.get_mut(mi) {
            for shape in part.shapes.iter().filter(|s| shapes.iter().any(|w| w == &s.name)) {
                for (v, d) in mesh.positions.iter_mut().zip(&shape.morphed_vertices) {
                    v[0] += d.position[0];
                    v[1] += d.position[1];
                    v[2] += d.position[2];
                }
            }
        }
        mi += 1;
    }
    model
}

/// Every LOD of a model: meshes tagged with their level, `lod_ranges` from the file's
/// per-LOD ranges (stored as squared metres: a 3-LOD rock is `[64², 128², 0]`, a single-LOD
/// tower carries the format's filler values, which are ignored because the last LOD has no
/// range).
pub fn prepare_model_all_lods(key: &str, mdl: &MDL) -> ModelData {
    let lods: Vec<usize> = (0..mdl.lods.len()).collect();
    let mut model = prepare_model_lods(key, mdl, &lods);
    if mdl.lods.len() > 1 {
        let ranges = mdl.lod_ranges();
        model.lod_ranges = (0..mdl.lods.len() - 1).map(|i| ranges.get(i).map(|r| r.0.max(0.0).sqrt()).unwrap_or(0.0)).collect();
        // Ranges must increase; a zero or shrinking entry ends that LOD immediately.
        for i in 1..model.lod_ranges.len() {
            if model.lod_ranges[i] < model.lod_ranges[i - 1] {
                model.lod_ranges[i] = model.lod_ranges[i - 1];
            }
        }
    }
    model
}

fn prepare_model_lods(key: &str, mdl: &MDL, lods: &[usize]) -> ModelData {
    let mut meshes = Vec::new();
    let mut bounds_min = [f32::MAX; 3];
    let mut bounds_max = [f32::MIN; 3];
    for (level, &lod_index) in lods.iter().enumerate() {
        let Some(lod) = mdl.lods.get(lod_index) else {
            continue;
        };
        for part in &lod.parts {
            if part.vertices.is_empty() || part.indices.len() < 3 {
                continue;
            }
            let skinned = !part.bone_table.is_empty();
            let mut mesh = MeshData {
                material_index: part.material_index as usize,
                water: matches!(part.part_type, PartType::Water),
                bone_table: part.bone_table.clone(),
                lod: level as u8,
                ..Default::default()
            };
            let mut ji = Vec::new();
            let mut jw = Vec::new();
            for v in &part.vertices {
                mesh.positions.push(v.position);
                let n = glam::Vec3::from(v.normal);
                mesh.normals.push(if n.length_squared() > 1e-8 { n.normalize().to_array() } else { [0.0, 1.0, 0.0] });
                mesh.uv0.push(v.uv0);
                mesh.uv1.push(v.uv1);
                mesh.colors.push(v.color);
                // Bounds from the nearest LOD only (the far ones are simplified).
                if level == 0 {
                    for k in 0..3 {
                        bounds_min[k] = bounds_min[k].min(v.position[k]);
                        bounds_max[k] = bounds_max[k].max(v.position[k]);
                    }
                }
                if skinned {
                    // Up to eight influences (Dawntrail faces and bodies); the renderer takes
                    // four per vertex, so keep the strongest four and renormalise.
                    let mut inf: Vec<(u16, f32)> = (0..4).map(|k| (v.bone_id[k] as u16, v.bone_weight[k])).chain((0..4).map(|k| (v.bone_id2[k] as u16, v.bone_weight2[k]))).filter(|(_, w)| *w > 0.0).collect();
                    inf.sort_by(|a, b| b.1.total_cmp(&a.1));
                    inf.truncate(4);
                    let sum: f32 = inf.iter().map(|(_, w)| w).sum();
                    let mut w = [0.0f32; 4];
                    let mut id = [0u16; 4];
                    if sum > 0.0 {
                        for (k, (i, x)) in inf.iter().enumerate() {
                            id[k] = *i;
                            w[k] = x / sum;
                        }
                    } else {
                        w = [1.0, 0.0, 0.0, 0.0];
                    }
                    jw.push(w);
                    ji.push(id);
                }
            }
            mesh.indices = part.indices.iter().map(|i| *i as u32).collect();
            if skinned {
                mesh.joints = Some((ji, jw));
            }
            meshes.push(mesh);
        }
    }
    if meshes.is_empty() || bounds_min[0] == f32::MAX {
        bounds_min = [0.0; 3];
        bounds_max = [0.0; 3];
    }
    ModelData {
        key: key.to_string(),
        meshes,
        materials: Vec::new(),
        bone_names: mdl.affected_bone_names.clone(),
        bounds_min,
        bounds_max,
        collision: Vec::new(),
        lod_ranges: Vec::new(),
    }
}

/// Texture path for the first sampler whose usage is in `usages`.
fn sampler_texture<'a>(m: &'a Material, usages: &[u32]) -> Option<&'a str> {
    m.samplers
        .iter()
        .find(|s| usages.contains(&s.texture_usage))
        .and_then(|s| m.texture_paths.get(s.texture_index as usize))
        .map(String::as_str)
}

/// Suffix-based fallback for character textures whose sampler CRCs we do not know.
fn suffix_texture<'a>(m: &'a Material, suffixes: &[&str]) -> Option<&'a str> {
    m.texture_paths.iter().map(String::as_str).find(|p| {
        let stem = p.rsplit('/').next().unwrap_or("").trim_end_matches(".tex");
        suffixes.iter().any(|s| stem.ends_with(s))
    })
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_rgb(c: [f32; 3], w: f32) -> [f32; 4] {
    [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2]), w]
}

/// Material constant ids (Meddle `MaterialConstant`).
/// `character.wgsl` reads its flag vector here: 2 + 3 × 32 colour rows.
const FLAGS_PARAM: usize = 98;
const CONST_ALPHA_THRESHOLD: u32 = 0x29AC0223;
const CONST_DIFFUSE_COLOR: u32 = 0x2C2A34DD;
/// characterscroll.shpk effect parameter set B: tiling U/V, scroll U/V (per second).
const CONST_EFFECT_TILING_U: u32 = 0xDA3D022F;
const CONST_EFFECT_TILING_V: u32 = 0xD87BBC76;
const CONST_EFFECT_SCROLL_U: u32 = 0xEA8375A6;
const CONST_EFFECT_SCROLL_V: u32 = 0xE8C5CBFF;

fn constant(m: &Material, id: u32) -> Option<&[f32]> {
    m.constants.iter().find(|c| c.id == id).map(|c| &c.values[..(c.num_values.min(4) as usize)])
}

fn constant1(m: &Material, id: u32, default: f32) -> f32 {
    constant(m, id).and_then(|v| v.first().copied()).unwrap_or(default)
}

fn constant3(m: &Material, id: u32, default: [f32; 3]) -> [f32; 3] {
    match constant(m, id) {
        Some(v) if v.len() >= 3 => [v[0], v[1], v[2]],
        _ => default,
    }
}

/// Background (zone) material.
pub fn background_material(source: &dyn AssetSource, cache: &TextureCache, key: &str, m: &Material) -> MaterialDesc {
    let shader = m.shader_package_name.as_str();
    if shader.starts_with("water") || shader.starts_with("river") {
        return water_material(source, cache, key, m, shader.starts_with("river"));
    }
    let diffuse = sampler_texture(m, &[SAMPLER_COLOR_MAP0, SAMPLER_DIFFUSE]).and_then(|p| cache.get(source, p, true));
    let normal = sampler_texture(m, &[SAMPLER_NORMAL_MAP0, SAMPLER_NORMAL]).and_then(|p| cache.get_rg(source, p));
    let specular = sampler_texture(m, &[SAMPLER_SPECULAR_MAP0, SAMPLER_SPECULAR]).and_then(|p| cache.get(source, p, false));
    // Terrain plates split two-layer materials into `_bgs0`/`_bgs1` single-layer variants
    // whose second layer is `bgcommon/texture/dummy_*.tex` (a flat 4×4 grey): blending it in
    // by vertex alpha painted the plaster triangles of Mist's plot walls as flat pale patches.
    let real = |p: &str| !p.contains("/dummy_");
    let diffuse2 = sampler_texture(m, &[SAMPLER_COLOR_MAP1]).filter(|p| real(p)).and_then(|p| cache.get(source, p, true));
    // Layer heights for the two-layer blend: the bg normal maps keep a height in their blue
    // channel (means 70-75 on Mist's stone/stucco, never a unit-normal Z).
    let height0 = diffuse2.as_ref().and(sampler_texture(m, &[SAMPLER_NORMAL_MAP0])).filter(|p| real(p)).and_then(|p| cache.get(source, p, false));
    let height1 = diffuse2.as_ref().and(sampler_texture(m, &[SAMPLER_NORMAL_MAP1])).filter(|p| real(p)).and_then(|p| cache.get(source, p, false));
    let mut desc = MaterialDesc::new(key, shaders::BG);
    desc.alpha_mask = diffuse.as_ref().map(|d| d.has_alpha).unwrap_or(false);
    desc.double_sided = true;
    desc.roughness = 0.9;
    desc.params = vec![[0.0; 4], [
        if diffuse2.is_some() { 1.0 } else { 0.0 },
        if specular.is_some() { 1.0 } else { 0.0 },
        if diffuse.is_some() { 1.0 } else { 0.0 },
        if height0.is_some() && height1.is_some() { 1.0 } else { 0.0 },
    ]];
    if let Some(d) = diffuse2 {
        desc.slots.push(("diffuse2".into(), d));
    }
    if let (Some(h0), Some(h1)) = (height0, height1) {
        desc.slots.push(("height0".into(), h0));
        desc.slots.push(("height1".into(), h1));
    }
    if let Some(s) = specular {
        desc.slots.push(("specular".into(), s));
    }
    desc.diffuse = diffuse;
    desc.normal = normal;
    desc.surface = surface_of(m.texture_paths.iter().map(String::as_str).chain(std::iter::once(key)));
    desc
}

/// Water colour (`water.shpk` / `river.shpk`) constant ids: the surface colour, the colour
/// of deep water and the tint/alpha vector (read from Mist's sea and river materials).
const CONST_WATER_COLOR: u32 = 0xBA163700;
const CONST_WATER_DEEP_COLOR: u32 = 0xD315E728;
const CONST_WATER_TINT: u32 = 0x29FA2AC1;

/// Water surface (`water.shpk` for seas and lakes, `river.shpk` for flowing water): the two
/// scrolling wave normal maps, the whitecap mask and the wavelet detail map go to the
/// `ff14/water` shader; the colour texture (L8, foam/colour lookup) is not decoded. Seas tile
/// their waves over world space (their uv0 is zero), rivers along their uv.
fn water_material(source: &dyn AssetSource, cache: &TextureCache, key: &str, m: &Material, river: bool) -> MaterialDesc {
    let mut desc = MaterialDesc::new(key, shaders::WATER);
    let mut slot = |name: &str, suffixes: &[&str]| {
        if let Some(t) = suffix_texture_contains(m, suffixes).and_then(|p| cache.get(source, p, false)) {
            desc.slots.push((name.into(), t));
            true
        } else {
            false
        }
    };
    let has_wave_a = slot("wave_a", &["_n_wave_0", "_n_wave_1"]);
    let has_wave_b = slot("wave_b", &["_n_wave_2", "_n_wave_3"]) || has_wave_a && slot("wave_b", &["_n_wave_0"]);
    let has_cap = slot("whitecap", &["whitecap"]);
    let _ = slot("wavelet", &["wavelet"]);
    let colour = constant3(m, CONST_WATER_COLOR, [0.42, 0.56, 0.56]);
    let deep = constant3(m, CONST_WATER_DEEP_COLOR, [0.005, 0.065, 0.09]);
    let tint = constant(m, CONST_WATER_TINT).filter(|v| v.len() >= 4).map(|v| [v[0], v[1], v[2], v[3]]).unwrap_or([0.64, 0.64, 0.64, 0.8]);
    desc.alpha_blend = true;
    desc.double_sided = true;
    desc.roughness = 0.08;
    desc.metallic = 0.0;
    desc.base_color = [1.0, 1.0, 1.0, 1.0];
    desc.surface = SurfaceKind::Water;
    // params[1] = surface colour (linear) + alpha, params[2] = deep colour + river flag,
    // params[3] = (wave tile metres, scroll speed, normal strength, foam amount).
    // The tint's alpha (0.8 on Mist's sea) reads too thin over a sandy bed without the game's
    // depth-based colouring: keep a little more of the surface colour.
    let c = linear_rgb([colour[0] * tint[0], colour[1] * tint[1], colour[2] * tint[2]], tint[3].clamp(0.4, 0.95).max(0.85));
    let d = linear_rgb(deep, if river { 1.0 } else { 0.0 });
    let normal_strength = if has_wave_a { 0.35 } else { 0.0 };
    desc.params = vec![[0.0; 4], c, d, [if river { 3.0 } else { 9.0 }, if river { 2.5 } else { 1.0 }, normal_strength, if has_cap { 0.6 } else { 0.0 }]];
    let _ = has_wave_b;
    desc
}

/// First texture path containing one of the fragments (case-insensitive).
fn suffix_texture_contains<'a>(m: &'a Material, fragments: &[&str]) -> Option<&'a str> {
    m.texture_paths.iter().map(String::as_str).find(|p| {
        let stem = p.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        fragments.iter().any(|f| stem.contains(f))
    })
}

/// Ground material of a zone material from its texture and material names. Interim heuristic
/// until PCB collision materials are read: FF14 texture stems abbreviate the material
/// (`_grs`, `_snd`, `_stn`, ...); the shared keyword table covers the spelled-out names.
pub fn surface_of<'a>(names: impl Iterator<Item = &'a str>) -> SurfaceKind {
    const ABBREVIATIONS: &[(&str, SurfaceKind)] = &[
        ("grs", SurfaceKind::Grass),
        ("gras", SurfaceKind::Grass),
        ("mos", SurfaceKind::Grass),
        ("snd", SurfaceKind::Sand),
        ("sand", SurfaceKind::Sand),
        ("bch", SurfaceKind::Sand),
        ("stn", SurfaceKind::Stone),
        ("ston", SurfaceKind::Stone),
        ("rck", SurfaceKind::Stone),
        ("rock", SurfaceKind::Stone),
        ("brk", SurfaceKind::Stone),
        ("bric", SurfaceKind::Stone),
        ("tile", SurfaceKind::Stone),
        ("til", SurfaceKind::Stone),
        ("pav", SurfaceKind::Stone),
        ("cob", SurfaceKind::Stone),
        ("mbl", SurfaceKind::Stone),
        ("wal", SurfaceKind::Stone),
        ("wod", SurfaceKind::Wood),
        ("wd", SurfaceKind::Wood),
        ("wood", SurfaceKind::Wood),
        ("plk", SurfaceKind::Wood),
        ("brd", SurfaceKind::Wood),
        ("mtl", SurfaceKind::Metal),
        ("metal", SurfaceKind::Metal),
        ("irn", SurfaceKind::Metal),
        ("grv", SurfaceKind::Gravel),
        ("grav", SurfaceKind::Gravel),
        ("lef", SurfaceKind::Leaf),
        ("leaf", SurfaceKind::Leaf),
        ("crp", SurfaceKind::Carpet),
        ("carp", SurfaceKind::Carpet),
        ("rug", SurfaceKind::Carpet),
        ("clt", SurfaceKind::Carpet),
        ("snw", SurfaceKind::Snow),
        ("snow", SurfaceKind::Snow),
        ("ice", SurfaceKind::Snow),
        ("wtr", SurfaceKind::Water),
        ("water", SurfaceKind::Water),
        ("soil", SurfaceKind::Dirt),
        ("sol", SurfaceKind::Dirt),
        ("grd", SurfaceKind::Dirt),
        ("gnd", SurfaceKind::Dirt),
        ("mud", SurfaceKind::Dirt),
        ("drt", SurfaceKind::Dirt),
        ("dirt", SurfaceKind::Dirt),
        ("road", SurfaceKind::Dirt),
        ("rod", SurfaceKind::Dirt),
        // Mist (s1h1) stems: ground, stucco, wall, stone pavement, block, ground variation,
        // rush mat (komo), timber (moku).
        ("grnd", SurfaceKind::Dirt),
        ("stuc", SurfaceKind::Stone),
        ("wall", SurfaceKind::Stone),
        ("stpv", SurfaceKind::Stone),
        ("blok", SurfaceKind::Stone),
        ("roof", SurfaceKind::Stone),
        ("gvar", SurfaceKind::Gravel),
        ("komo", SurfaceKind::Carpet),
        ("moku", SurfaceKind::Wood),
        ("plk", SurfaceKind::Wood),
        // Central Shroud (f1f1): metal, board (ita), wood decks, brick, pillar, ruin, door.
        ("metl", SurfaceKind::Metal),
        ("ita", SurfaceKind::Wood),
        ("wdx", SurfaceKind::Wood),
        ("dor", SurfaceKind::Wood),
        ("bri", SurfaceKind::Stone),
        ("pill", SurfaceKind::Stone),
        ("ruin", SurfaceKind::Stone),
    ];
    for name in names {
        let stem = name.rsplit('/').next().unwrap_or(name).trim_end_matches(".tex").trim_end_matches(".mtrl").to_ascii_lowercase();
        // Texture stems are `<zone>_<area>_<material><nn>[_<suffix>]`; look at the underscore
        // fields after the zone/area prefix, longest keyword wins per field.
        let mut best: Option<(usize, SurfaceKind)> = None;
        for field in stem.split('_').skip(2) {
            // `wall1a` → `wall`, `stpv1` → `stpv`.
            let field: String = field.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            for (k, s) in ABBREVIATIONS {
                if (field == *k || (k.len() >= 4 && field.contains(k))) && best.is_none_or(|(l, _)| k.len() > l) {
                    best = Some((k.len(), *s));
                }
            }
        }
        if let Some((_, s)) = best {
            return s;
        }
        if let Some(s) = SurfaceKind::from_name(&stem) {
            return s;
        }
    }
    SurfaceKind::Unknown
}

/// Colours a character's materials need, from human.cmp (sRGB 0..1).
#[derive(Debug, Clone, Copy)]
pub struct CharacterColors {
    pub skin: [f32; 3],
    pub lip: [f32; 3],
    /// Lip colour opacity from the CMP alpha channel.
    pub lip_strength: f32,
    pub hair: [f32; 3],
    pub highlight: [f32; 3],
    pub highlights_enabled: bool,
    pub eye_left: [f32; 3],
    pub eye_right: [f32; 3],
    pub feature: [f32; 3],
    /// Face paint colour and opacity.
    pub decal: [f32; 4],
    /// Face paint UV scale and offset (the game's `FacePaintUvMultiplier` / `Offset`).
    pub decal_uv: [f32; 2],
}

/// The colour table with the item's dyes applied: every row whose dye entry names a template
/// takes the staining template's values for the fields its flags select, from the stain of
/// the row's dye channel (`dyes[0]` = first dye, `dyes[1]` = second).
pub fn apply_dyes(table: &ColorTable, dye_table: Option<&ColorDyeTable>, dyes: [u8; 2], templates: &DyeTemplates) -> ColorTable {
    let mut out = table.clone();
    if dyes == [0, 0] || templates.is_empty() {
        return out;
    }
    match (&mut out, dye_table) {
        (ColorTable::DawntrailColorTable(t), Some(ColorDyeTable::DawntrailColorDyeTable(d))) => {
            let Some(stm) = &templates.dawntrail else {
                return out;
            };
            for (row, dye_row) in t.rows.iter_mut().zip(&d.rows) {
                let stain = match dye_row.channel {
                    0 => dyes[0],
                    1 => dyes[1],
                    _ => 0,
                };
                if stain == 0 || dye_row.template == 0 {
                    continue;
                }
                let Some(dye) = stm.dye(dye_row.template as u32, stain) else {
                    continue;
                };
                if dye_row.diffuse && let Some(c) = dye.diffuse {
                    row.diffuse_color = c;
                }
                if dye_row.specular && let Some(c) = dye.specular {
                    row.specular_color = c;
                }
                if dye_row.emissive && let Some(c) = dye.emissive {
                    row.emissive_color = c;
                }
                if dye_row.scalar3 && let Some(v) = dye.scalar3 {
                    row.unknown3 = v;
                }
                if dye_row.metalness && let Some(v) = dye.scalar4 {
                    row.metalness = v;
                }
                if dye_row.roughness && let Some(v) = dye.roughness {
                    row.roughness = v;
                }
                if dye_row.sheen_rate && let Some(v) = dye.sheen_rate {
                    row.sheen_rate = v;
                }
                if dye_row.sheen_tint_rate && let Some(v) = dye.sheen_tint {
                    row.sheen_tint = v;
                }
                if dye_row.sheen_aperture && let Some(v) = dye.sheen_aperture {
                    row.sheen_aperture = v;
                }
                if dye_row.anisotropy && let Some(v) = dye.anisotropy {
                    row.anisotropy = v;
                }
                if dye_row.sphere_map_index && let Some(v) = dye.sphere_index {
                    row.sphere_index = v;
                }
                if dye_row.sphere_map_mask && let Some(v) = dye.sphere_mask {
                    row.sphere_mask = v;
                }
            }
        }
        (ColorTable::LegacyColorTable(t), Some(ColorDyeTable::LegacyColorDyeTable(d))) => {
            let Some(stm) = &templates.legacy else {
                return out;
            };
            for (row, dye_row) in t.rows.iter_mut().zip(&d.rows) {
                let stain = dyes[0];
                if stain == 0 || dye_row.template == 0 {
                    continue;
                }
                let Some(dye) = stm.dye(dye_row.template as u32, stain) else {
                    continue;
                };
                if dye_row.diffuse && let Some(c) = dye.diffuse {
                    row.diffuse_color = c;
                }
                if dye_row.specular && let Some(c) = dye.specular {
                    row.specular_color = c;
                }
                if dye_row.emissive && let Some(c) = dye.emissive {
                    row.emissive_color = c;
                }
                if dye_row.gloss && let Some(v) = dye.scalar3 {
                    row.gloss_strength = v;
                }
                if dye_row.specular_strength && let Some(v) = dye.scalar4 {
                    row.specular_strength = v;
                }
            }
        }
        _ => {}
    }
    out
}

/// Colour table rows → the character shader's parameter block (see `shaders/character.wgsl`).
fn color_table_params(table: &ColorTable, legacy: bool, has_index: bool, has_mask: bool) -> Vec<[f32; 4]> {
    let mut rows_a = vec![[1.0, 1.0, 1.0, 0.7]; 32];
    let mut rows_b = vec![[0.0, 0.0, 0.0, 0.0]; 32];
    let mut rows_c = vec![[0.0, 0.0, 0.0, 1.0]; 32];
    let mut count = 0usize;
    match table {
        ColorTable::DawntrailColorTable(t) => {
            for (i, r) in t.rows.iter().take(32).enumerate() {
                if legacy {
                    // Legacy-shader tables stored in the Dawntrail layout: no roughness/metalness;
                    // `unknown1` is the specular power (Phong-like exponent, 3..20 on retail gear)
                    // and `unknown2` the gloss strength (Meddle: SpecularStrength/GlossStrength).
                    rows_a[i] = [r.diffuse_color[0], r.diffuse_color[1], r.diffuse_color[2], r.unknown2];
                    rows_b[i] = [r.specular_color[0], r.specular_color[1], r.specular_color[2], 0.0];
                    rows_c[i] = [r.emissive_color[0], r.emissive_color[1], r.emissive_color[2], r.unknown1];
                } else {
                    rows_a[i] = [r.diffuse_color[0], r.diffuse_color[1], r.diffuse_color[2], r.roughness];
                    rows_b[i] = [r.specular_color[0], r.specular_color[1], r.specular_color[2], r.metalness];
                    rows_c[i] = [r.emissive_color[0], r.emissive_color[1], r.emissive_color[2], 1.0];
                }
                count += 1;
            }
        }
        ColorTable::LegacyColorTable(t) => {
            for (i, r) in t.rows.iter().take(32).enumerate() {
                rows_a[i] = [r.diffuse_color[0], r.diffuse_color[1], r.diffuse_color[2], r.gloss_strength];
                rows_b[i] = [r.specular_color[0], r.specular_color[1], r.specular_color[2], 0.0];
                rows_c[i] = [r.emissive_color[0], r.emissive_color[1], r.emissive_color[2], r.specular_strength];
                count += 1;
            }
        }
        _ => {}
    }
    let mut params = Vec::with_capacity(98);
    params.push([0.0; 4]);
    params.push([
        count as f32,
        if has_index { 1.0 } else { 0.0 },
        if has_mask { 1.0 } else { 0.0 },
        if legacy { 1.0 } else { 0.0 },
    ]);
    params.extend(rows_a);
    params.extend(rows_b);
    params.extend(rows_c);
    params
}

/// Character (skin/hair/iris/gear) material.
pub fn character_material(
    source: &dyn AssetSource,
    cache: &TextureCache,
    key: &str,
    m: &Material,
    colors: &CharacterColors,
    is_face: bool,
    decal: Option<TextureRef>,
    dyes: [u8; 2],
    templates: &DyeTemplates,
) -> MaterialDesc {
    let shader = m.shader_package_name.as_str();
    // Dawntrail skin `_base` textures are UNORM data multiplied by the (squared) skin colour in
    // linear space: decoding them as sRGB is what made skin come out far too dark.
    let diffuse_srgb = shader != "skin.shpk";
    let diffuse = sampler_texture(m, &[SAMPLER_DIFFUSE])
        .or_else(|| suffix_texture(m, &["_base", "_d"]))
        .and_then(|p| cache.get(source, p, diffuse_srgb));
    let normal_path = sampler_texture(m, &[SAMPLER_NORMAL]).or_else(|| suffix_texture(m, &["_norm", "_n"]));
    // Full RGBA for the engine shaders (B/A carry opacity and masks), RG chain for the PBR base.
    let normal = normal_path.and_then(|p| cache.get(source, p, false));
    let normal_rg = normal_path.and_then(|p| cache.get_rg(source, p));
    let mask = sampler_texture(m, &[SAMPLER_MASK, SAMPLER_SPECULAR])
        .or_else(|| suffix_texture(m, &["_mask", "_m", "_s"]))
        .and_then(|p| cache.get(source, p, false));
    let index = sampler_texture(m, &[SAMPLER_INDEX])
        .or_else(|| suffix_texture(m, &["_id"]))
        .and_then(|p| cache.get(source, p, false));
    match shader {
        "skin.shpk" => {
            let mut d = MaterialDesc::new(key, shaders::SKIN);
            d.params = vec![
                [0.0; 4],
                linear_rgb(colors.skin, if mask.is_some() { 1.0 } else { 0.0 }),
                linear_rgb(colors.lip, if is_face { colors.lip_strength } else { 0.0 }),
                linear_rgb(colors.hair, if normal.is_some() { 1.0 } else { 0.0 }),
                {
                    let dc = constant3(m, CONST_DIFFUSE_COLOR, [1.0; 3]);
                    [dc[0], dc[1], dc[2], 0.0]
                },
                linear_rgb([colors.decal[0], colors.decal[1], colors.decal[2]], colors.decal[3]),
                [colors.decal_uv[0], colors.decal_uv[1], if decal.is_some() { 1.0 } else { 0.0 }, 0.0],
            ];
            if let Some(n) = &normal {
                d.slots.push(("normal_rgba".into(), n.clone()));
            }
            if let Some(mk) = mask {
                d.slots.push(("mask".into(), mk));
            }
            if let Some(dc) = decal {
                d.slots.push(("decal".into(), dc));
            }
            d.diffuse = diffuse;
            d.normal = normal_rg.clone();
            d.roughness = 0.55;
            d
        }
        "hair.shpk" => {
            let mut d = MaterialDesc::new(key, shaders::HAIR);
            d.params = vec![
                [0.0; 4],
                linear_rgb(colors.hair, if colors.highlights_enabled { 1.0 } else { 0.0 }),
                linear_rgb(colors.highlight, if mask.is_some() { 1.0 } else { 0.0 }),
                {
                    let dc = constant3(m, CONST_DIFFUSE_COLOR, [1.0; 3]);
                    [dc[0], dc[1], dc[2], 0.0]
                },
            ];
            d.alpha_cutoff = constant1(m, CONST_ALPHA_THRESHOLD, 0.5);
            if let Some(n) = &normal {
                d.slots.push(("normal_rgba".into(), n.clone()));
            }
            if let Some(mk) = mask {
                d.slots.push(("mask".into(), mk));
            }
            d.normal = normal_rg.clone();
            d.alpha_mask = true;
            d.double_sided = true;
            d
        }
        "iris.shpk" => {
            let mut d = MaterialDesc::new(key, shaders::IRIS);
            d.params = vec![
                [0.0; 4],
                linear_rgb(colors.eye_left, if mask.is_some() { 1.0 } else { 0.0 }),
                linear_rgb(colors.eye_right, 0.0),
            ];
            if let Some(mk) = mask {
                d.slots.push(("mask".into(), mk));
            }
            d.diffuse = diffuse;
            d.normal = normal_rg.clone();
            d.roughness = 0.2;
            d
        }
        "charactertattoo.shpk" => MaterialDesc::new(key, ffl_core::builtin::SKIP),
        // The eyelid shadow: a grey texture (bound as the normal sampler) multiplied over
        // whatever it covers.
        "characterocclusion.shpk" => {
            let mut d = MaterialDesc::new(key, ffl_core::builtin::UNLIT);
            d.diffuse = normal_path.and_then(|p| cache.get(source, p, false));
            d.multiply = true;
            d.double_sided = true;
            if std::env::var_os("FFL_OCCLUSION_DEBUG").is_some() {
                // Verification aid: draw the overlay solid red instead of multiplying.
                d.diffuse = None;
                d.multiply = false;
                d.base_color = [1.0, 0.0, 0.0, 1.0];
            }
            d
        }
        s if s.starts_with("character") => {
            let legacy = s == "characterlegacy.shpk";
            let mut d = MaterialDesc::new(key, shaders::CHARACTER);
            let dyed = m.color_table.as_ref().map(|t| apply_dyes(t, m.color_dye_table.as_ref(), dyes, templates));
            let table = dyed.as_ref();
            d.params = match table {
                Some(t) => color_table_params(t, legacy, index.is_some(), mask.is_some()),
                None => vec![[0.0; 4], [0.0, 0.0, 0.0, 0.0]],
            };
            if let Some(i) = index {
                d.slots.push(("index".into(), i));
            }
            if let Some(mk) = mask {
                d.slots.push(("mask".into(), mk));
            }
            // characterscroll.shpk: an effect texture (bound as the catchlight sampler)
            // scrolls over the UVs and lights the colour rows' emissive where it is bright
            // (DTM "The Animated Effect"): uv × tiling + time × scroll, sample squared.
            let effect = sampler_texture(m, &[SAMPLER_CATCHLIGHT]).and_then(|p| cache.get(source, p, false));
            let has_effect = effect.is_some();
            if let Some(e) = effect {
                d.slots.push(("effect".into(), e));
            }
            // g_AlphaThreshold > 0 means the normal's blue channel is a cutout mask.
            let threshold = constant1(m, CONST_ALPHA_THRESHOLD, 0.0);
            let alpha_from_normal = threshold > 0.0 || s == "charactertransparency.shpk" || s == "characterglass.shpk" || is_face;
            if let Some(n) = &normal {
                d.slots.push(("normal_rgba".into(), n.clone()));
            }
            // Flags live after the 3×32 rows (index 98): index 4 is colour row 2!
            while d.params.len() < FLAGS_PARAM + 1 {
                d.params.push([0.0; 4]);
            }
            d.params[FLAGS_PARAM] = [if normal.is_some() { 1.0 } else { 0.0 }, if alpha_from_normal { 1.0 } else { 0.0 }, if has_effect { 1.0 } else { 0.0 }, 0.0];
            if has_effect {
                // Parameter set B (the one the rows select); DTM writes both sets alike.
                d.params.push([
                    constant1(m, CONST_EFFECT_TILING_U, 1.0),
                    constant1(m, CONST_EFFECT_TILING_V, 1.0),
                    constant1(m, CONST_EFFECT_SCROLL_U, 0.0),
                    constant1(m, CONST_EFFECT_SCROLL_V, 0.0),
                ]);
            }
            d.alpha_mask = alpha_from_normal;
            d.alpha_cutoff = if threshold > 0.0 { threshold } else { 0.5 };
            d.double_sided = is_face;
            d.diffuse = diffuse;
            d.normal = normal_rg.clone();
            d.roughness = 0.7;
            d
        }
        other => {
            tracing::warn!("{key}: unhandled shader {other}");
            let mut d = MaterialDesc::new(key, ffl_core::builtin::PBR);
            d.base_color = [0.8, 0.8, 0.8, 1.0];
            d.diffuse = diffuse;
            d.normal = normal_rg.clone();
            d.double_sided = true;
            d
        }
    }
}
