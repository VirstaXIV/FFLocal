//! Plain mesh and model data.

/// One drawable mesh: positions/normals/uv in model space, triangle list indices.
#[derive(Debug, Clone, Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uv0: Vec<[f32; 2]>,
    pub uv1: Vec<[f32; 2]>,
    /// Per-vertex colour (used by some shaders as blend weights, not as tint).
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    /// Index into [`ModelData::materials`].
    pub material_index: usize,
    /// Per-vertex joint indices (into `bone_table`) and normalized weights, when skinned.
    pub joints: Option<(Vec<[u16; 4]>, Vec<[f32; 4]>)>,
    /// Mesh-local joint index → index into [`ModelData::bone_names`].
    pub bone_table: Vec<u16>,
    /// Water/transparent surface hint from the source engine.
    pub water: bool,
    /// Level of detail this mesh belongs to (0 = nearest); see [`ModelData::lod_ranges`].
    pub lod: u8,
}

/// A collision-only triangle mesh with the ground material it reports to footsteps.
#[derive(Debug, Clone, Default)]
pub struct CollisionMesh {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub surface: crate::sound::SurfaceKind,
}

/// A model: meshes plus the materials they reference and the bone names they use.
#[derive(Debug, Clone, Default)]
pub struct ModelData {
    /// Engine-specific key (e.g. a game path). Stable for caching.
    pub key: String,
    pub meshes: Vec<MeshData>,
    pub materials: Vec<crate::material::MaterialDesc>,
    pub bone_names: Vec<String>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    /// The engine's own collision meshes (model space). When present the runtime collides
    /// with these instead of the render meshes; empty = collide with the render meshes.
    pub collision: Vec<CollisionMesh>,
    /// Distance (metres) at which each level of detail hands over to the next: LOD `i` shows
    /// from `lod_ranges[i - 1]` to `lod_ranges[i]`; the last LOD has no entry and shows until
    /// the runtime culls the model. Empty when the model has a single LOD.
    pub lod_ranges: Vec<f32>,
}

impl ModelData {
    pub fn is_skinned(&self) -> bool {
        self.meshes.iter().any(|m| m.joints.is_some() && !m.bone_table.is_empty())
    }
}
