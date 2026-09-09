//! `ffl_core` data → Bevy assets.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, Face, TextureDimension, TextureFormat};
use ffl_core::{MeshData, TextureData};

/// Build a Bevy mesh. `skinned` adds joint attributes (only for meshes that get a `SkinnedMesh`).
pub fn build_mesh(m: &MeshData, tangents: bool, skinned: bool, colors: bool) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, m.uv0.clone());
    // A second UV set only when the mesh actually has one (engines hand over zeros
    // otherwise); shaders key their second-layer mapping on its presence.
    if m.uv1.len() == m.positions.len() && m.uv1.iter().any(|uv| *uv != [0.0, 0.0]) {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, m.uv1.clone());
    }
    if colors && m.colors.len() == m.positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, m.colors.clone());
    }
    if skinned && let Some((ji, jw)) = &m.joints {
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, bevy::mesh::VertexAttributeValues::Uint16x4(ji.clone()));
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, jw.clone());
    }
    mesh.insert_indices(Indices::U32(m.indices.clone()));
    if tangents && let Err(err) = mesh.generate_tangents() {
        debug!("tangent generation failed: {err:?}");
    }
    mesh
}

fn sampler(anisotropy: u16) -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: anisotropy.max(1),
        ..default()
    })
}

fn gpu_format(f: ffl_core::TextureFormat, srgb: bool) -> TextureFormat {
    use ffl_core::TextureFormat as F;
    match (f, srgb) {
        (F::Rgba8, true) => TextureFormat::Rgba8UnormSrgb,
        (F::Rgba8, false) => TextureFormat::Rgba8Unorm,
        (F::Rg8, _) => TextureFormat::Rg8Unorm,
        (F::Bc1, true) => TextureFormat::Bc1RgbaUnormSrgb,
        (F::Bc1, false) => TextureFormat::Bc1RgbaUnorm,
        (F::Bc2, true) => TextureFormat::Bc2RgbaUnormSrgb,
        (F::Bc2, false) => TextureFormat::Bc2RgbaUnorm,
        (F::Bc3, true) => TextureFormat::Bc3RgbaUnormSrgb,
        (F::Bc3, false) => TextureFormat::Bc3RgbaUnorm,
        (F::Bc4, _) => TextureFormat::Bc4RUnorm,
        (F::Bc5, _) => TextureFormat::Bc5RgUnorm,
        (F::Bc7, true) => TextureFormat::Bc7RgbaUnormSrgb,
        (F::Bc7, false) => TextureFormat::Bc7RgbaUnorm,
    }
}

/// GPU image with the texture's full mip chain, uploaded in its own format (block-compressed
/// game textures stay compressed). `srgb` selects the colour-space view for colour formats.
pub fn build_image(t: &TextureData, srgb: bool, anisotropy: u16) -> Image {
    let format = gpu_format(t.format, srgb);
    let size = Extent3d {
        width: t.width.max(1),
        height: t.height.max(1),
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_uninit(size, TextureDimension::D2, format, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.size = size.physical_size(format);
    image.texture_descriptor.mip_level_count = t.mip_count().max(1);
    image.data = Some(t.mips.concat());
    image.sampler = sampler(anisotropy);
    image
}

/// Normal map for the PBR base: RG8 and BC5 chains upload as two-component maps; RGBA8 input
/// is reduced to RG here. Other block formats cannot be reduced without a decoder and are
/// uploaded as they are (engines should hand over RG data instead).
pub fn build_normal_image(t: &TextureData, anisotropy: u16) -> Image {
    match t.format {
        ffl_core::TextureFormat::Rgba8 => {
            let mut rg = TextureData::rg8_from_rgba(&t.key, t.width, t.height, &t.mips[0]);
            rg.generate_mips();
            build_image(&rg, false, anisotropy)
        }
        _ => build_image(t, false, anisotropy),
    }
}

/// 1x1 magenta placeholder.
pub fn placeholder_image() -> Image {
    Image::new_fill(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[255, 0, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

pub fn parse_cull(s: &str) -> Option<Face> {
    match s {
        "back" => Some(Face::Back),
        "front" => Some(Face::Front),
        _ => None,
    }
}

pub fn cull_name(f: Option<Face>) -> &'static str {
    match f {
        None => "none",
        Some(Face::Back) => "back",
        Some(Face::Front) => "front",
    }
}

pub fn trs_to_transform(t: &ffl_core::Trs) -> Transform {
    Transform {
        translation: Vec3::from(t.translation),
        rotation: Quat::from_xyzw(t.rotation[0], t.rotation[1], t.rotation[2], t.rotation[3]).normalize(),
        scale: Vec3::from(t.scale),
    }
}
