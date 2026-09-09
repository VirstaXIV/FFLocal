//! Hosts engine shaders. Every registered `ShaderDef` is assembled into one WGSL program with a
//! per-material dispatch, so materials from different engines render side by side with their
//! own game's shading. One Bevy material type (`EngineMaterial`) carries a generic parameter
//! block and six texture slots.

use std::collections::HashMap;
use std::sync::OnceLock;

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::{Shader, ShaderRef};
use ffl_core::{MAX_PARAMS, ShaderDef, TextureSlotDef};

use crate::app::EngineFactories;

pub type EngineMaterial = ExtendedMaterial<StandardMaterial, EngineExt>;

static ENGINE_SHADER: OnceLock<Handle<Shader>> = OnceLock::new();

#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct EngineExt {
    /// Slot 0: x = shader id. Slots 1.. belong to the engine shader.
    #[uniform(100)]
    pub params: [Vec4; MAX_PARAMS],
    #[texture(101)]
    #[sampler(102)]
    pub tex0: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    pub tex1: Option<Handle<Image>>,
    #[texture(105)]
    #[sampler(106)]
    pub tex2: Option<Handle<Image>>,
    #[texture(107)]
    #[sampler(108)]
    pub tex3: Option<Handle<Image>>,
    #[texture(109)]
    #[sampler(110)]
    pub tex4: Option<Handle<Image>>,
    #[texture(111)]
    #[sampler(112)]
    pub tex5: Option<Handle<Image>>,
}

impl Default for EngineExt {
    fn default() -> Self {
        Self {
            params: [Vec4::ZERO; MAX_PARAMS],
            tex0: None,
            tex1: None,
            tex2: None,
            tex3: None,
            tex4: None,
            tex5: None,
        }
    }
}

impl EngineExt {
    pub fn set_slot(&mut self, index: usize, handle: Handle<Image>) {
        match index {
            0 => self.tex0 = Some(handle),
            1 => self.tex1 = Some(handle),
            2 => self.tex2 = Some(handle),
            3 => self.tex3 = Some(handle),
            4 => self.tex4 = Some(handle),
            5 => self.tex5 = Some(handle),
            _ => {}
        }
    }
}

impl MaterialExtension for EngineExt {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(ENGINE_SHADER.get().expect("engine shaders not registered").clone())
    }
    fn deferred_fragment_shader() -> ShaderRef {
        ShaderRef::Handle(ENGINE_SHADER.get().expect("engine shaders not registered").clone())
    }
}

/// Registered engine shaders: id → numeric id and slot layout.
#[derive(Resource, Default)]
pub struct ShaderRegistry {
    pub ids: HashMap<String, u32>,
    pub slots: HashMap<String, Vec<TextureSlotDef>>,
}

impl ShaderRegistry {
    pub fn slot_index(&self, shader: &str, slot_name: &str) -> Option<(usize, bool)> {
        self.slots
            .get(shader)?
            .iter()
            .position(|s| s.name == slot_name)
            .map(|i| (i, self.slots[shader][i].srgb))
    }
}

const PREAMBLE: &str = r#"
#import bevy_pbr::{
    pbr_bindings,
    mesh_view_bindings::globals,
    pbr_types::PbrInput,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
}
#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
#endif

struct EngineParams {
    data: array<vec4<f32>, 128>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> params: EngineParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var tex0: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var samp0: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var tex1: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var samp1: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var tex2: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var samp2: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var tex3: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var samp3: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(109) var tex4: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var samp4: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(111) var tex5: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(112) var samp5: sampler;
"#;

/// Concatenate the registered shaders into one program with a dispatch on `params.data[0].x`.
pub fn assemble(defs: &[ShaderDef]) -> (String, HashMap<String, u32>) {
    let mut ids = HashMap::new();
    let mut src = String::from(PREAMBLE);
    let mut dispatch = String::new();
    for (i, d) in defs.iter().enumerate() {
        let id = i as u32 + 1;
        ids.insert(d.id.clone(), id);
        src.push_str(&format!("\n// ---- {} ----\n", d.id));
        src.push_str(&d.source);
        src.push('\n');
        dispatch.push_str(&format!(
            "        case {id}u: {{ base = {}(in, &pbr_input, base); }}\n",
            d.entry
        ));
    }
    src.push_str(&format!(
        r#"
@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {{
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    var base = pbr_input.material.base_color;
    let shader_id = u32(params.data[0].x);
    switch shader_id {{
{dispatch}        default: {{ }}
    }}
    pbr_input.material.base_color = alpha_discard(pbr_input.material, base);
#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif
    return out;
}}
"#
    ));
    (src, ids)
}

pub struct ShadersPlugin;

impl Plugin for ShadersPlugin {
    fn build(&self, app: &mut App) {
        // Assemble before any pipeline can ask for the shader. The factories provide the
        // lists, so an engine whose game is picked later in the Games window has its shaders.
        let defs: Vec<ShaderDef> = app
            .world()
            .resource::<EngineFactories>()
            .0
            .iter()
            .flat_map(|f| f.shaders())
            .collect();
        let (source, ids) = assemble(&defs);
        let handle = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .add(Shader::from_wgsl(source, "ffl://engine_shaders.wgsl"));
        let _ = ENGINE_SHADER.set(handle);
        let slots = defs.iter().map(|d| (d.id.clone(), d.slots.clone())).collect();
        info!("registered {} engine shaders: {}", defs.len(), defs.iter().map(|d| d.id.as_str()).collect::<Vec<_>>().join(" "));
        app.insert_resource(ShaderRegistry { ids, slots })
            .add_plugins(MaterialPlugin::<EngineMaterial>::default());
    }
}
