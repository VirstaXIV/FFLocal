//! Scene graph for a map: placed models, groups and lights, engine-agnostic.

/// Translation, rotation as a quaternion (x, y, z, w), scale. Y up, metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trs {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl Trs {
    pub const IDENTITY: Trs = Trs {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionHint {
    /// Build collision from the render mesh.
    Mesh,
    /// No collision.
    None,
}

#[derive(Debug, Clone)]
pub struct LightDesc {
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: f32,
    pub spot_angle_degrees: Option<f32>,
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    /// Transform-only node (group).
    Empty,
    /// A model instance; `model` is a key resolved through [`crate::Engine::load_model`].
    Model { model: String, collision: CollisionHint },
    Light(LightDesc),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    /// Index into [`Scene::nodes`]; parents always precede children.
    pub parent: Option<usize>,
    pub transform: Trs,
    pub kind: NodeKind,
}

/// Lighting environment for a scene. Engines fill what they know; the runtime picks defaults
/// for the rest.
#[derive(Debug, Clone)]
pub struct Environment {
    /// Direction the sun light travels (normalized), e.g. pointing down and a bit sideways.
    pub sun_direction: [f32; 3],
    /// Sun colour (linear RGB multiplier).
    pub sun_color: [f32; 3],
    /// Relative sun strength (1.0 = full daylight).
    pub sun_intensity: f32,
    /// Whether the scene is outdoors (draw sky, use sky ambient).
    pub outdoors: bool,
    /// Ambient light strength relative to the runtime default.
    pub ambient: f32,
    /// Distance fog: colour, start and end distance in metres.
    pub fog: Option<([f32; 3], f32, f32)>,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            sun_direction: [-0.4, -0.8, -0.45],
            sun_color: [1.0, 0.97, 0.92],
            sun_intensity: 1.0,
            outdoors: true,
            ambient: 1.0,
            fog: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub engine: String,
    pub map: String,
    pub name: String,
    pub nodes: Vec<Node>,
    /// Suggested spawn anchors, best first (the runtime still probes for ground).
    pub spawn_candidates: Vec<[f32; 3]>,
    /// Rough centre of the playable area.
    pub center: [f32; 3],
    pub environment: Environment,
    /// Background music of the map.
    pub music: Option<crate::sound::MusicSet>,
    /// Sub-areas with their own music; they override `music` while the listener is inside.
    pub music_regions: Vec<crate::sound::MusicRegion>,
    /// Placed ambient loops (emitters are not nodes: they are neither streamed nor culled).
    pub emitters: Vec<crate::sound::AmbientEmitter>,
    /// Footstep bank the map provides for characters that bring none of their own.
    pub footsteps: Option<crate::sound::FootstepSet>,
    /// Static world-space collision (terrain) the engine provides; nodes whose collision hint
    /// is `None` rely on it.
    pub collision: Vec<crate::mesh::CollisionMesh>,
    pub notes: Vec<String>,
    /// Where the scene's assets came from (see [`crate::content`]).
    pub content: crate::content::ContentReport,
}

impl Scene {
    pub fn unique_model_keys(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for n in &self.nodes {
            if let NodeKind::Model { model, .. } = &n.kind
                && seen.insert(model.as_str())
            {
                out.push(model.as_str());
            }
        }
        out
    }

    pub fn model_instance_count(&self) -> usize {
        self.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Model { .. })).count()
    }
}
