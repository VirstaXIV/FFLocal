//! `MaterialDesc` → Bevy materials: builtins on `StandardMaterial`, engine shaders on
//! `EngineMaterial` (see `shaders.rs`).

use std::collections::HashMap;

use bevy::ecs::system::EntityCommands;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::render::render_resource::Face;
use ffl_core::{MAX_PARAMS, MaterialDesc, TextureRef, builtin};

use crate::convert::{build_image, build_normal_image, placeholder_image};
use crate::world::shaders::{EngineExt, EngineMaterial, ShaderRegistry};

#[derive(Clone, Debug)]
pub enum RuntimeMaterial {
    Standard(Handle<StandardMaterial>),
    Engine(Handle<EngineMaterial>),
}

impl RuntimeMaterial {
    pub fn insert(&self, entity: &mut EntityCommands) {
        match self {
            RuntimeMaterial::Standard(h) => {
                entity.insert(MeshMaterial3d(h.clone()));
            }
            RuntimeMaterial::Engine(h) => {
                entity.insert(MeshMaterial3d(h.clone()));
            }
        }
    }
}

pub struct MaterialAssets<'a> {
    pub images: &'a mut Assets<Image>,
    pub standard: &'a mut Assets<StandardMaterial>,
    pub engine: &'a mut Assets<EngineMaterial>,
    pub registry: &'a ShaderRegistry,
}

#[derive(Resource, Default)]
pub struct MaterialCache {
    pub images: HashMap<String, Handle<Image>>,
    pub materials: HashMap<String, RuntimeMaterial>,
    pub placeholder: Option<Handle<Image>>,
    pub cull: Option<Face>,
    pub normal_maps: bool,
    /// Sampler anisotropy for every uploaded texture (graphics setting).
    pub anisotropy: u16,
    /// Bytes of texture data handed to the GPU so far (statistics).
    pub texture_bytes: usize,
    /// What the same textures would take as mip-less RGBA8 (statistics).
    pub texture_raw_bytes: usize,
}

impl MaterialCache {
    fn image(&mut self, images: &mut Assets<Image>, t: &TextureRef, srgb: bool) -> Handle<Image> {
        let key = format!("{}#{}", t.key, if srgb { "srgb" } else { "lin" });
        let aniso = self.anisotropy;
        let (bytes, raw) = (&mut self.texture_bytes, &mut self.texture_raw_bytes);
        self.images
            .entry(key)
            .or_insert_with(|| {
                *bytes += t.total_bytes();
                *raw += t.width as usize * t.height as usize * 4;
                images.add(build_image(t, srgb, aniso))
            })
            .clone()
    }

    fn normal(&mut self, images: &mut Assets<Image>, t: &TextureRef) -> Handle<Image> {
        let key = format!("{}#normal", t.key);
        let aniso = self.anisotropy;
        let (bytes, raw) = (&mut self.texture_bytes, &mut self.texture_raw_bytes);
        self.images
            .entry(key)
            .or_insert_with(|| {
                *bytes += t.total_bytes();
                *raw += t.width as usize * t.height as usize * 4;
                images.add(build_normal_image(t, aniso))
            })
            .clone()
    }

    fn placeholder(&mut self, images: &mut Assets<Image>) -> Handle<Image> {
        self.placeholder.get_or_insert_with(|| images.add(placeholder_image())).clone()
    }

    /// The PBR base every material starts from.
    fn base(&mut self, images: &mut Assets<Image>, desc: &MaterialDesc, engine: bool) -> StandardMaterial {
        let c = desc.base_color;
        let alpha_mode = if desc.multiply {
            AlphaMode::Multiply
        } else if desc.alpha_blend {
            AlphaMode::Blend
        } else if desc.alpha_mask {
            AlphaMode::Mask(desc.alpha_cutoff.clamp(0.01, 0.99))
        } else {
            AlphaMode::Opaque
        };
        StandardMaterial {
            base_color: Color::srgba(c[0], c[1], c[2], c[3]),
            base_color_texture: match &desc.diffuse {
                Some(d) => Some(self.image(images, d, d.srgb)),
                None if engine && desc.shader == "ff14/bg" => Some(self.placeholder(images)),
                None => None,
            },
            normal_map_texture: desc.normal.as_ref().filter(|_| self.normal_maps).map(|n| self.normal(images, n)),
            flip_normal_map_y: true,
            perceptual_roughness: desc.roughness,
            metallic: desc.metallic,
            reflectance: 0.25,
            double_sided: desc.double_sided,
            cull_mode: if desc.double_sided { None } else { self.cull.or(Some(Face::Back)) },
            alpha_mode,
            unlit: desc.shader == builtin::UNLIT || desc.multiply,
            ..default()
        }
    }

    /// Resolve (or build) the Bevy material for a description.
    pub fn material(&mut self, assets: &mut MaterialAssets, desc: &MaterialDesc) -> RuntimeMaterial {
        if let Some(h) = self.materials.get(&desc.key) {
            return h.clone();
        }
        let handle = match assets.registry.ids.get(&desc.shader).copied() {
            Some(id) => {
                let base = self.base(assets.images, desc, true);
                let mut ext = EngineExt::default();
                // FFL_SHADER_DEBUG=<n> is passed to engine shaders as params[0].y (debug views).
                let debug = std::env::var("FFL_SHADER_DEBUG").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                ext.params[0] = Vec4::new(id as f32, debug, 0.0, 0.0);
                for (i, p) in desc.params.iter().enumerate().skip(1).take(MAX_PARAMS - 1) {
                    ext.params[i] = Vec4::from(*p);
                }
                for (name, tex) in &desc.slots {
                    match assets.registry.slot_index(&desc.shader, name) {
                        Some((index, srgb)) => {
                            let h = self.image(assets.images, tex, srgb);
                            ext.set_slot(index, h);
                        }
                        None => warn!("{}: shader {} has no slot {name}", desc.key, desc.shader),
                    }
                }
                RuntimeMaterial::Engine(assets.engine.add(ExtendedMaterial { base, extension: ext }))
            }
            None => {
                if !matches!(desc.shader.as_str(), builtin::PBR | builtin::UNLIT | builtin::WATER | builtin::SKIP) {
                    warn!("{}: unknown shader {}; using PBR", desc.key, desc.shader);
                }
                let mut m = self.base(assets.images, desc, false);
                if desc.shader == builtin::WATER {
                    m.perceptual_roughness = 0.15;
                    m.reflectance = 0.5;
                    m.cull_mode = None;
                }
                RuntimeMaterial::Standard(assets.standard.add(m))
            }
        };
        self.materials.insert(desc.key.clone(), handle.clone());
        handle
    }
}
