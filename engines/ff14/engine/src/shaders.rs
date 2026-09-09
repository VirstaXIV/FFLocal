//! The FFXIV shader set registered with the runtime (see `ffl_core::ShaderDef`).

use ffl_core::{ShaderDef, TextureSlotDef};

pub const BG: &str = "ff14/bg";
pub const CHARACTER: &str = "ff14/character";
pub const SKIN: &str = "ff14/skin";
pub const HAIR: &str = "ff14/hair";
pub const IRIS: &str = "ff14/iris";
pub const WATER: &str = "ff14/water";

fn slot(name: &str, srgb: bool) -> TextureSlotDef {
    TextureSlotDef {
        name: name.to_string(),
        srgb,
    }
}

pub fn shader_defs() -> Vec<ShaderDef> {
    vec![
        ShaderDef {
            id: BG.into(),
            entry: "ff14_bg".into(),
            source: include_str!("../shaders/bg.wgsl").into(),
            slots: vec![slot("diffuse2", true), slot("specular", false), slot("height0", false), slot("height1", false)],
        },
        ShaderDef {
            id: CHARACTER.into(),
            entry: "ff14_character".into(),
            source: include_str!("../shaders/character.wgsl").into(),
            slots: vec![slot("index", false), slot("mask", false), slot("normal_rgba", false), slot("effect", false)],
        },
        ShaderDef {
            id: SKIN.into(),
            entry: "ff14_skin".into(),
            source: include_str!("../shaders/skin.wgsl").into(),
            slots: vec![slot("normal_rgba", false), slot("mask", false), slot("decal", false)],
        },
        ShaderDef {
            id: HAIR.into(),
            entry: "ff14_hair".into(),
            source: include_str!("../shaders/hair.wgsl").into(),
            slots: vec![slot("normal_rgba", false), slot("mask", false)],
        },
        ShaderDef {
            id: IRIS.into(),
            entry: "ff14_iris".into(),
            source: include_str!("../shaders/iris.wgsl").into(),
            slots: vec![slot("mask", false)],
        },
        ShaderDef {
            id: WATER.into(),
            entry: "ff14_water".into(),
            source: include_str!("../shaders/water.wgsl").into(),
            slots: vec![slot("wave_a", false), slot("wave_b", false), slot("whitecap", false), slot("wavelet", false)],
        },
    ]
}
