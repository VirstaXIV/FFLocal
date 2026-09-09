//! Scene loading: the engine builds a `Scene` on a task, models stream in through the compute
//! pool, instances spawn as their assets become ready.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::Result;
use avian3d::prelude::{Collider, CollisionLayers, LayerMask};
use bevy::asset::RenderAssetUsages;
use bevy::camera::Exposure;
use bevy::camera::visibility::VisibilityRange;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::atmosphere::ScatteringMedium;
use bevy::light::{Atmosphere, AtmosphereEnvironmentMapLight, NotShadowCaster, light_consts::lux};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{AtmosphereSettings, DistanceFog, FogFalloff};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::Face;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};
use ffl_core::{CollisionHint, Engine, ModelData, Node, NodeKind, Scene};

use crate::app::{AppState, Engines, Graphics, RuntimeOptions, WorldEntity, WorldRequest};
use crate::camera::{FlyCamera, MainCamera};
use crate::convert::{build_mesh, parse_cull, trs_to_transform};
use crate::world::materials::{MaterialAssets, MaterialCache, RuntimeMaterial};
use crate::world::shaders::{EngineMaterial, ShaderRegistry};
use crate::world::sound::Surface;

#[derive(Resource, Default)]
pub struct ZoneStatus {
    pub phase: String,
    pub models_total: usize,
    pub models_ready: usize,
    pub instances_spawned: usize,
    /// Meshes that carry a distance cull range.
    pub ranged_meshes: usize,
    pub errors: Vec<String>,
    pub finished_at: Option<f64>,
    pub screenshot_taken: bool,
    pub center: Option<Vec3>,
    pub spawn_candidates: Vec<Vec3>,
    pub scene_notes: Vec<String>,
    /// Content report summary of the scene.
    pub scene_content: String,
    pub scene_shareable: bool,
}

#[derive(Resource)]
pub struct CullState(pub Option<Face>);

#[derive(Component)]
pub struct ZoneRoot;

/// Collision layer of water surfaces: ground probes and the camera skip it, the swim probe
/// looks only for it.
pub const WATER_LAYER: LayerMask = LayerMask(1 << 1);
/// Everything the player walks on and bumps into.
pub const SOLID_LAYER: LayerMask = LayerMask::DEFAULT;

/// A water surface mesh (its collider is on [`WATER_LAYER`]).
#[derive(Component)]
pub struct WaterSurface;

/// The sea ring drawn out to the horizon around a large flat water surface.
#[derive(Component)]
pub struct HorizonSea;

struct LoadedModel {
    key: String,
    model: Result<ModelData>,
    /// Per render mesh (empty when the model brings its own collision meshes).
    colliders: Vec<Option<Collider>>,
    /// Per render mesh: the collider of a water surface (`None` for solid meshes).
    water_colliders: Vec<Option<Collider>>,
    /// The engine's collision meshes with their ground surface.
    collision: Vec<(Collider, ffl_core::SurfaceKind)>,
}

struct PartAssets {
    mesh: Handle<Mesh>,
    material: RuntimeMaterial,
    /// Solid collider, or the water collider of a water part.
    collider: Option<Collider>,
    surface: ffl_core::SurfaceKind,
    /// Level of detail (see `ModelAssets::lod_ranges`).
    lod: u8,
    water: bool,
    /// Model-space bounds (water parts only, for the horizon sea).
    bounds: Option<(Vec3, Vec3)>,
}

struct ModelAssets {
    parts: Vec<PartAssets>,
    /// Collision-only colliders (from the engine's collision meshes), model space.
    collision: Vec<(Collider, ffl_core::SurfaceKind)>,
    /// Distance at which each LOD hands over to the next (`ffl_core::ModelData::lod_ranges`).
    lod_ranges: Vec<f32>,
    bounds_min: Vec3,
    bounds_max: Vec3,
}

/// Visible distance window of a part at `lod` for a model culled at `cull` metres; the
/// game's switch distances are scaled by `scale` (`GraphicsSettings::lod_distance`).
fn lod_window(ranges: &[f32], lod: u8, cull: f32, scale: f32) -> (f32, f32) {
    let lod = lod as usize;
    let start = if lod == 0 { 0.0 } else { ranges.get(lod - 1).copied().unwrap_or(0.0) * scale };
    let end = ranges.get(lod).map(|r| (r * scale).min(cull)).unwrap_or(cull);
    (start, end)
}

/// A flat polar grid in the XZ plane (normals up) out to `outer` metres, leaving out every
/// cell whose centre lies inside one of `holes` (world XZ rectangles relative to the mesh
/// origin): the sea out to the horizon, minus the game's own sea sheets (drawing a second
/// translucent sheet over them doubled the water). Cells grow geometrically with the
/// radius, so the edge around the sheets is fine and the far ring cheap.
fn horizon_mesh(outer: f32, holes: &[(Vec2, Vec2)]) -> Mesh {
    const SEGMENTS: usize = 128;
    const FIRST_RADIUS: f32 = 40.0;
    const GROWTH: f32 = 1.05;
    let mut radii = vec![0.0, FIRST_RADIUS];
    while *radii.last().unwrap() < outer {
        radii.push(radii.last().unwrap() * GROWTH);
    }
    let rings = radii.len();
    let mut positions = Vec::with_capacity(rings * SEGMENTS);
    let mut normals = Vec::with_capacity(rings * SEGMENTS);
    let mut uvs = Vec::with_capacity(rings * SEGMENTS);
    for &r in &radii {
        for i in 0..SEGMENTS {
            let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            positions.push([c * r, 0.0, s * r]);
            normals.push([0.0, 1.0, 0.0]);
            uvs.push([c * r * 0.01, s * r * 0.01]);
        }
    }
    let inside = |p: Vec2| holes.iter().any(|(lo, hi)| p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y);
    let mut indices = Vec::new();
    for ri in 0..rings - 1 {
        let (r0, r1) = (radii[ri], radii[ri + 1]);
        for i in 0..SEGMENTS {
            let j = (i + 1) % SEGMENTS;
            let a = (i as f32 + 0.5) / SEGMENTS as f32 * std::f32::consts::TAU;
            let mid = (r0 + r1) * 0.5;
            if inside(Vec2::new(a.cos() * mid, a.sin() * mid)) {
                continue;
            }
            let (i0, i1) = ((ri * SEGMENTS + i) as u32, (ri * SEGMENTS + j) as u32);
            let (o0, o1) = (((ri + 1) * SEGMENTS + i) as u32, ((ri + 1) * SEGMENTS + j) as u32);
            indices.extend_from_slice(&[i0, o1, o0, i0, i1, o1]);
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// World-space bounds of a model-space box under a transform.
fn world_bounds(min: Vec3, max: Vec3, transform: &Transform) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for i in 0..8 {
        let c = Vec3::new(if i & 1 == 0 { min.x } else { max.x }, if i & 2 == 0 { min.y } else { max.y }, if i & 4 == 0 { min.z } else { max.z });
        let w = transform.transform_point(c);
        lo = lo.min(w);
        hi = hi.max(w);
    }
    (lo, hi)
}

/// Half-size (metres) a flat water surface must reach before the sea ring is drawn beyond it.
const HORIZON_SEA_MIN_EXTENT: f32 = 300.0;
/// Outer radius of the sea ring (metres); the fog hides its edge long before.
const HORIZON_SEA_RADIUS: f32 = 60_000.0;
/// Depth of the opaque sea floor under the whole sea: where the zone's terrain ends, the
/// translucent surface shows this deep-water colour instead of the sky's ground.
const HORIZON_SEA_FLOOR_DEPTH: f32 = 25.0;

fn trimesh_collider(positions: &[[f32; 3]], indices: &[u32]) -> Option<Collider> {
    if indices.len() < 3 {
        return None;
    }
    let verts: Vec<Vec3> = positions.iter().map(|p| Vec3::from(*p)).collect();
    let tris: Vec<[u32; 3]> = indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    Some(Collider::trimesh(verts, tris))
}

#[derive(Resource, Default)]
pub struct ZoneLoader {
    engine: Option<Arc<dyn Engine>>,
    scene_task: Option<Task<Result<Scene>>>,
    scene: Option<Arc<Scene>>,
    root: Option<Entity>,
    node_entities: Vec<Option<Entity>>,
    queue: VecDeque<String>,
    in_flight: Vec<Task<LoadedModel>>,
    model_assets: HashMap<String, Arc<ModelAssets>>,
    instances_by_key: HashMap<String, Vec<usize>>,
    finished: bool,
    camera_placed: bool,
    environment_applied: bool,
    /// The horizon sea has been spawned (once per world).
    horizon_spawned: bool,
}

const MAX_IN_FLIGHT: usize = 8;

impl ZoneLoader {
    /// The loaded scene (music, emitters, footsteps, notes), once the engine built it.
    pub fn scene(&self) -> Option<Arc<Scene>> {
        self.scene.clone()
    }
}

/// Scattering medium asset kept alive across world entries (see `apply_environment`).
#[derive(Resource, Default)]
pub struct SharedMedium(Option<Handle<ScatteringMedium>>);


pub struct ZonePlugin;

impl Plugin for ZonePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ZoneStatus>()
            .init_resource::<ZoneLoader>()
            .init_resource::<MaterialCache>()
            .insert_resource(CullState(None))
            .init_resource::<SharedMedium>()
            .add_systems(OnEnter(AppState::World), enter_world)
            .add_systems(OnExit(AppState::World), exit_world)
            .add_systems(
                Update,
                (poll_scene, apply_environment, pump_models, finish_loading, hotkeys, auto_screenshot)
                    .chain()
                    .run_if(in_state(AppState::World)),
            );
    }
}

/// Sun, sky and ambient from the scene's environment description (once per world).
fn apply_environment(
    mut commands: Commands,
    mut loader: ResMut<ZoneLoader>,
    mut mediums: ResMut<Assets<ScatteringMedium>>,
    mut shared_medium: ResMut<SharedMedium>,
    graphics: Res<Graphics>,
    camera: Query<Entity, With<MainCamera>>,
) {
    if loader.environment_applied {
        return;
    }
    let Some(scene) = loader.scene.clone() else {
        return;
    };
    loader.environment_applied = true;
    let env = &scene.environment;
    let dir = Vec3::from(env.sun_direction).normalize_or_zero();
    let sun_rotation = Transform::default().looking_to(dir, Vec3::Y).rotation;
    commands.spawn((
        Name::new("Sun"),
        WorldEntity,
        DirectionalLight {
            color: Color::srgb(env.sun_color[0], env.sun_color[1], env.sun_color[2]),
            illuminance: if env.outdoors { lux::RAW_SUNLIGHT } else { lux::FULL_DAYLIGHT } * env.sun_intensity,
            shadow_maps_enabled: graphics.current.shadows,
            ..default()
        },
        Transform::from_rotation(sun_rotation),
    ));
    commands.insert_resource(ClearColor(Color::srgb(0.55, 0.72, 0.9)));
    let Ok(camera) = camera.single() else {
        return;
    };
    if env.outdoors {
        // The medium asset must outlive the world: the render world still references it for a
        // frame after the atmosphere entity is despawned (dropping it panicked on exit).
        let medium = shared_medium.0.get_or_insert_with(|| mediums.add(ScatteringMedium::earth(256, 256))).clone();
        // A pale ground under the horizon: the default dark albedo showed as a black band
        // below the fogged terrain edge.
        commands.spawn((
            Name::new("Atmosphere"),
            WorldEntity,
            Atmosphere {
                ground_albedo: Vec3::new(0.5, 0.55, 0.62),
                ..Atmosphere::earth(medium)
            },
        ));
        // Distance haze the way the game fades its zones into the sky: the edge of the loaded
        // map, the far LODs and the sea ring all dissolve into the horizon colour instead of
        // stopping against the void.
        let visibility = graphics.current.render_distance.max(500.0) * 3.0;
        let fog = match env.fog {
            Some((c, start, end)) => DistanceFog {
                color: Color::srgb(c[0], c[1], c[2]),
                falloff: FogFalloff::Linear { start, end },
                ..default()
            },
            None => DistanceFog {
                color: Color::srgb(0.66, 0.74, 0.86),
                directional_light_color: Color::srgba(1.0, 0.95, 0.85, 0.5),
                directional_light_exponent: 30.0,
                falloff: FogFalloff::from_visibility_colors(
                    visibility,
                    Color::srgb(0.35, 0.5, 0.66),
                    Color::srgb(0.8, 0.844, 1.0),
                ),
            },
        };
        commands.entity(camera).insert((
            fog,
            // The aerial-perspective LUT reaches as far as the fog: past its default 32 km the
            // sea ring picked up the LUT's darkest slice and drew a dark band at the horizon.
            AtmosphereSettings {
                aerial_view_lut_max_distance: visibility,
                ..default()
            },
            AtmosphereEnvironmentMapLight {
                intensity: env.ambient,
                ..default()
            },
            Exposure { ev100: 13.0 },
            Tonemapping::AcesFitted,
            Bloom::NATURAL,
            AmbientLight {
                color: Color::WHITE,
                brightness: 0.0,
                affects_lightmapped_meshes: true,
            },
        ));
    } else {
        commands.entity(camera).insert((
            Exposure { ev100: 11.0 },
            Tonemapping::AcesFitted,
            Bloom::NATURAL,
            AmbientLight {
                color: Color::srgb(0.8, 0.85, 1.0),
                brightness: 2500.0 * env.ambient,
                affects_lightmapped_meshes: true,
            },
        ));
    }
}

fn enter_world(
    engines: Res<Engines>,
    request: Res<WorldRequest>,
    opts: Res<RuntimeOptions>,
    graphics: Res<Graphics>,
    mut loader: ResMut<ZoneLoader>,
    mut status: ResMut<ZoneStatus>,
    mut cache: ResMut<MaterialCache>,
    mut cull: ResMut<CullState>,
) {
    *loader = ZoneLoader::default();
    *status = ZoneStatus::default();
    *cache = MaterialCache::default();
    cull.0 = parse_cull(&opts.cull);
    cache.cull = cull.0;
    cache.normal_maps = opts.normal_maps;
    cache.anisotropy = graphics.current.anisotropy as u16;

    let Some(engine) = engines.get(&request.engine) else {
        status.phase = format!("unknown engine {}", request.engine);
        loader.finished = true;
        return;
    };
    loader.engine = Some(engine.clone());
    let pool = AsyncComputeTaskPool::get();
    if let Some(model) = opts.single_model.clone() {
        status.phase = format!("loading model {model}");
        let scene = Scene {
            engine: request.engine.clone(),
            map: String::new(),
            name: model.clone(),
            nodes: vec![Node {
                name: model.clone(),
                parent: None,
                transform: ffl_core::Trs::IDENTITY,
                kind: NodeKind::Model {
                    model,
                    collision: CollisionHint::Mesh,
                },
            }],
            ..Default::default()
        };
        loader.scene_task = Some(pool.spawn(async move { Ok(scene) }));
    } else {
        let map = request.map.clone();
        status.phase = format!("building scene for {} map {map}", request.engine);
        loader.scene_task = Some(pool.spawn(async move { engine.load_scene(&map) }));
    }
}

fn exit_world(mut commands: Commands, world_entities: Query<Entity, With<WorldEntity>>, mut loader: ResMut<ZoneLoader>) {
    // The camera is replaced by `camera::respawn_camera` (its world-only components go with it).
    for e in &world_entities {
        commands.entity(e).despawn();
    }
    *loader = ZoneLoader::default();
}

fn poll_scene(mut commands: Commands, opts: Res<RuntimeOptions>, mut loader: ResMut<ZoneLoader>, mut status: ResMut<ZoneStatus>) {
    let Some(task) = loader.scene_task.as_mut() else {
        return;
    };
    let Some(result) = block_on(poll_once(task)) else {
        return;
    };
    loader.scene_task = None;
    let scene = match result {
        Ok(s) => Arc::new(s),
        Err(err) => {
            error!("scene failed: {err:#}");
            status.phase = format!("scene failed: {err:#}");
            status.errors.push(format!("{err:#}"));
            loader.finished = true;
            return;
        }
    };
    info!("scene {}: {} nodes, {} model instances", scene.name, scene.nodes.len(), scene.model_instance_count());
    status.scene_notes = scene.notes.clone();
    status.scene_content = scene.content.summary();
    status.scene_shareable = scene.content.shareable();
    status.center = Some(Vec3::from(scene.center));
    status.spawn_candidates = scene.spawn_candidates.iter().map(|c| Vec3::from(*c)).collect();

    let root = commands
        .spawn((Name::new("ZoneRoot"), ZoneRoot, WorldEntity, Transform::IDENTITY, Visibility::default()))
        .id();
    loader.root = Some(root);

    // Static world-space collision the engine provides (terrain pieces with ground materials).
    if opts.collision {
        let mut polys = 0;
        for c in &scene.collision {
            if let Some(col) = trimesh_collider(&c.positions, &c.indices) {
                polys += c.indices.len() / 3;
                commands.spawn((Name::new(format!("terrain collision {}", c.surface.label())), Transform::IDENTITY, ChildOf(root), Surface(c.surface), col));
            }
        }
        if polys > 0 {
            info!("scene collision: {} meshes, {polys} polygons", scene.collision.len());
        }
    }

    // Spawn group and light nodes now; model nodes spawn when their assets arrive.
    let mut node_entities: Vec<Option<Entity>> = Vec::with_capacity(scene.nodes.len());
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        let parent = node.parent.and_then(|p| node_entities[p]).unwrap_or(root);
        let entity = match &node.kind {
            NodeKind::Empty => Some(
                commands
                    .spawn((Name::new(node.name.clone()), trs_to_transform(&node.transform), Visibility::default(), ChildOf(parent)))
                    .id(),
            ),
            NodeKind::Light(l) => Some(
                commands
                    .spawn((
                        Name::new("light"),
                        PointLight {
                            color: Color::srgb(l.color[0], l.color[1], l.color[2]),
                            intensity: (l.intensity.max(0.1)) * 20_000.0,
                            range: l.range.clamp(2.0, 40.0),
                            shadow_maps_enabled: false,
                            ..default()
                        },
                        trs_to_transform(&node.transform),
                        Visibility::default(),
                        ChildOf(parent),
                    ))
                    .id(),
            ),
            NodeKind::Model { model, .. } => {
                by_key.entry(model.clone()).or_default().push(i);
                None
            }
        };
        node_entities.push(entity);
    }
    loader.node_entities = node_entities;
    let mut keys: Vec<(String, usize)> = by_key.iter().map(|(k, v)| (k.clone(), v.len())).collect();
    keys.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    loader.queue = keys.into_iter().map(|(k, _)| k).collect();
    status.models_total = loader.queue.len();
    loader.instances_by_key = by_key;
    loader.scene = Some(scene);
    status.phase = "loading models".into();
}

fn spawn_model_task(engine: Arc<dyn Engine>, key: String, with_colliders: bool) -> Task<LoadedModel> {
    AsyncComputeTaskPool::get().spawn(async move {
        let model = engine.load_model(&key);
        // The engine's own collision meshes win (real ground materials); otherwise every
        // render mesh is solid.
        let collision: Vec<(Collider, ffl_core::SurfaceKind)> = match &model {
            Ok(m) if with_colliders => m.collision.iter().filter_map(|c| trimesh_collider(&c.positions, &c.indices).map(|col| (col, c.surface))).collect(),
            _ => Vec::new(),
        };
        let colliders = match &model {
            Ok(m) if with_colliders && collision.is_empty() => m
                .meshes
                .iter()
                .map(|mesh| if mesh.water { None } else { trimesh_collider(&mesh.positions, &mesh.indices) })
                .collect(),
            _ => Vec::new(),
        };
        // Water surfaces are never solid; their colliders answer the swim probe only.
        let water_colliders = match &model {
            Ok(m) if with_colliders => m
                .meshes
                .iter()
                .map(|mesh| if mesh.water && mesh.lod == 0 { trimesh_collider(&mesh.positions, &mesh.indices) } else { None })
                .collect(),
            _ => Vec::new(),
        };
        LoadedModel { key, model, colliders, water_colliders, collision }
    })
}

#[allow(clippy::too_many_arguments)]
fn pump_models(
    mut commands: Commands,
    opts: Res<RuntimeOptions>,
    graphics: Res<Graphics>,
    mut loader: ResMut<ZoneLoader>,
    mut status: ResMut<ZoneStatus>,
    mut cache: ResMut<MaterialCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut engine_materials: ResMut<Assets<EngineMaterial>>,
    registry: Res<ShaderRegistry>,
) {
    let mut assets = MaterialAssets {
        images: &mut images,
        standard: &mut materials,
        engine: &mut engine_materials,
        registry: &registry,
    };
    let Some(engine) = loader.engine.clone() else {
        return;
    };
    if loader.scene.is_none() {
        return;
    }
    while loader.in_flight.len() < MAX_IN_FLIGHT {
        let Some(key) = loader.queue.pop_front() else {
            break;
        };
        loader.in_flight.push(spawn_model_task(engine.clone(), key, opts.collision));
    }
    let mut done = Vec::new();
    for (i, task) in loader.in_flight.iter_mut().enumerate() {
        if let Some(result) = block_on(poll_once(task)) {
            done.push((i, result));
        }
    }
    for (i, _) in done.iter().rev() {
        let _ = loader.in_flight.remove(*i);
    }
    for (_, loaded) in done {
        let model = match loaded.model {
            Ok(m) => m,
            Err(err) => {
                warn!("{}: {err:#}", loaded.key);
                status.errors.push(format!("{}: {err:#}", loaded.key));
                status.models_ready += 1;
                continue;
            }
        };
        let mut parts = Vec::new();
        for (mi, mesh) in model.meshes.iter().enumerate() {
            if mesh.indices.is_empty() {
                continue;
            }
            let desc = model.materials.get(mesh.material_index);
            if desc.map(|d| d.is_skip()).unwrap_or(false) {
                continue;
            }
            let (mat, surface) = match desc {
                Some(d) => (cache.material(&mut assets, d), if mesh.water { ffl_core::SurfaceKind::Water } else { d.surface }),
                None => (cache.material(&mut assets, &ffl_core::MaterialDesc::water("<water>")), ffl_core::SurfaceKind::Water),
            };
            let mesh_handle = meshes.add(build_mesh(mesh, opts.normal_maps, false, true));
            let bounds = mesh.water.then(|| {
                let mut lo = Vec3::splat(f32::MAX);
                let mut hi = Vec3::splat(f32::MIN);
                for p in &mesh.positions {
                    lo = lo.min(Vec3::from(*p));
                    hi = hi.max(Vec3::from(*p));
                }
                (lo, hi)
            });
            parts.push(PartAssets {
                mesh: mesh_handle,
                material: mat,
                collider: if mesh.water { loaded.water_colliders.get(mi).cloned().flatten() } else { loaded.colliders.get(mi).cloned().flatten() },
                surface,
                lod: mesh.lod,
                water: mesh.water,
                bounds,
            });
        }
        let model_assets = Arc::new(ModelAssets {
            parts,
            collision: loaded.collision.clone(),
            lod_ranges: model.lod_ranges.clone(),
            bounds_min: Vec3::from(model.bounds_min),
            bounds_max: Vec3::from(model.bounds_max),
        });
        status.models_ready += 1;

        let scene = loader.scene.clone().unwrap();
        let root = loader.root.unwrap();
        // Small-object culling: the bounding radius decides how far away an instance still
        // renders (the game data carries no clip ranges for placed models).
        let radius = (model_assets.bounds_max - model_assets.bounds_min).length() * 0.5;
        // A large flat water surface is a sea: continue it to the horizon, once per world,
        // around every placed sheet of this model (Mist places its sea twice).
        if !loader.horizon_spawned
            && let Some(indices) = loader.instances_by_key.get(&loaded.key)
        {
            let mut sheets: Vec<(Vec3, Vec3)> = Vec::new();
            let mut material = None;
            for &idx in indices {
                let transform = trs_to_transform(&scene.nodes[idx].transform);
                for part in model_assets.parts.iter().filter(|p| p.water) {
                    if let Some((min, max)) = part.bounds {
                        let (lo, hi) = world_bounds(min, max, &transform);
                        if (hi.x - lo.x).min(hi.z - lo.z) * 0.5 >= HORIZON_SEA_MIN_EXTENT && hi.y - lo.y <= 1.5 {
                            sheets.push((lo, hi));
                            material.get_or_insert_with(|| part.material.clone());
                        }
                    }
                }
            }
            if let Some(material) = material {
                loader.horizon_spawned = true;
                let level = sheets.iter().map(|(lo, hi)| (lo.y + hi.y) * 0.5).sum::<f32>() / sheets.len() as f32;
                let centre_xz = sheets.iter().map(|(lo, hi)| (lo.xz() + hi.xz()) * 0.5).sum::<Vec2>() / sheets.len() as f32;
                let centre = Vec3::new(centre_xz.x, level - 0.05, centre_xz.y);
                let holes: Vec<(Vec2, Vec2)> = sheets.iter().map(|(lo, hi)| (lo.xz() - centre_xz, hi.xz() - centre_xz)).collect();
                let far = VisibilityRange {
                    start_margin: 0.0..0.0,
                    end_margin: HORIZON_SEA_RADIUS..HORIZON_SEA_RADIUS,
                    use_aabb: false,
                };
                let mut sea = commands.spawn((
                    Name::new("horizon sea"),
                    HorizonSea,
                    NotShadowCaster,
                    Mesh3d(meshes.add(horizon_mesh(HORIZON_SEA_RADIUS, &holes))),
                    Transform::from_translation(centre),
                    Visibility::default(),
                    ChildOf(root),
                    far.clone(),
                ));
                material.insert(&mut sea);
                let floor_material = assets.standard.add(StandardMaterial {
                    base_color: Color::srgb(0.02, 0.09, 0.12),
                    perceptual_roughness: 1.0,
                    ..default()
                });
                commands.spawn((
                    Name::new("horizon sea floor"),
                    HorizonSea,
                    Mesh3d(meshes.add(horizon_mesh(HORIZON_SEA_RADIUS, &[]))),
                    MeshMaterial3d(floor_material),
                    Transform::from_translation(centre - Vec3::Y * HORIZON_SEA_FLOOR_DEPTH),
                    Visibility::default(),
                    ChildOf(root),
                    far,
                ));
                info!("horizon sea at y = {level:.2} around {} sheet(s) of {}", sheets.len(), loaded.key);
            }
        }
        if let Some(indices) = loader.instances_by_key.get(&loaded.key) {
            for &idx in indices {
                let node = &scene.nodes[idx];
                let parent = node.parent.and_then(|p| loader.node_entities[p]).unwrap_or(root);
                let transform = trs_to_transform(&node.transform);
                let collision = matches!(node.kind, NodeKind::Model { collision: CollisionHint::Mesh, .. });
                let scale = transform.scale.abs().max_element().max(0.01);
                let d = graphics.cull_distance(radius * scale);
                if collision {
                    for (c, surface) in &model_assets.collision {
                        commands.spawn((Name::new(format!("collision {}", node.name)), transform, ChildOf(parent), Surface(*surface), c.clone()));
                    }
                }
                for part in &model_assets.parts {
                    // Each LOD shows in its own distance window; the last one until the cull.
                    let (start, end) = lod_window(&model_assets.lod_ranges, part.lod, d, graphics.current.lod_distance);
                    if start >= end {
                        continue;
                    }
                    let range = VisibilityRange {
                        start_margin: start..start,
                        end_margin: end..end,
                        use_aabb: false,
                    };
                    let mut e = commands.spawn((
                        Name::new(node.name.clone()),
                        Mesh3d(part.mesh.clone()),
                        transform,
                        Visibility::default(),
                        ChildOf(parent),
                        Surface(part.surface),
                    ));
                    e.insert(range);
                    if end < graphics.current.render_distance {
                        status.ranged_meshes += 1;
                    }
                    part.material.insert(&mut e);
                    if part.water {
                        // A translucent sheet must not shadow its own bed: the sea darkened
                        // the seabed out to the shadow cascade's end (100 m) and the water
                        // turned pale beyond it in a straight line.
                        e.insert(NotShadowCaster);
                        if let Some(c) = &part.collider {
                            e.insert((c.clone(), CollisionLayers::new(WATER_LAYER, LayerMask::NONE), WaterSurface));
                        }
                    } else if let Some(c) = &part.collider
                        && collision
                    {
                        e.insert(c.clone());
                    }
                }
                status.instances_spawned += 1;
            }
        }
        loader.model_assets.insert(loaded.key.clone(), model_assets);
    }
}

fn finish_loading(
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    cache: Res<MaterialCache>,
    mut loader: ResMut<ZoneLoader>,
    mut status: ResMut<ZoneStatus>,
    mut camera: Query<(&mut Transform, &mut FlyCamera), With<MainCamera>>,
) {
    let Some(scene) = loader.scene.clone() else {
        return;
    };
    if !loader.camera_placed
        && let Ok((mut transform, mut fly)) = camera.single_mut()
    {
        let single = opts.single_model.is_some();
        if single && loader.model_assets.is_empty() && opts.camera_pos.is_none() {
            return;
        }
        let (eye, target) = if let Some(pos) = opts.camera_pos {
            (pos, pos + Vec3::new(0.0, -0.5, -1.0))
        } else if single {
            let (min, max) = loader
                .model_assets
                .values()
                .next()
                .map(|a| (a.bounds_min, a.bounds_max))
                .unwrap_or((Vec3::splat(-1.0), Vec3::splat(1.0)));
            let center = (min + max) * 0.5;
            let extent = (max - min).length().max(1.0);
            (center + Vec3::new(0.6, 0.5, 1.0).normalize() * extent * 1.2, center)
        } else {
            let center = Vec3::from(scene.center);
            (center + Vec3::new(0.0, 60.0, 110.0), center)
        };
        *transform = Transform::from_translation(eye).looking_at(target, Vec3::Y);
        fly.needs_sync = true;
        fly.speed = if single { 5.0 } else { 30.0 };
        loader.camera_placed = true;
    }
    if !loader.finished && loader.queue.is_empty() && loader.in_flight.is_empty() {
        loader.finished = true;
        status.finished_at = Some(time.elapsed_secs_f64());
        status.phase = format!("{} loaded ({} errors)", scene.name, status.errors.len());
        info!(
            "{}  textures: {} images, {:.1} MB uploaded ({:.1} MB as uncompressed RGBA8 without mips)  size-culled meshes: {}",
            status.phase,
            cache.images.len(),
            cache.texture_bytes as f64 / 1e6,
            cache.texture_raw_bytes as f64 / 1e6,
            status.ranged_meshes
        );
    }
}

fn hotkeys(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    opts: Res<RuntimeOptions>,
    time: Res<Time>,
    status: Res<ZoneStatus>,
    mut cull: ResMut<CullState>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut engine_materials: ResMut<Assets<EngineMaterial>>,
    mut next: ResMut<NextState<AppState>>,
    mut menu_fired: Local<bool>,
) {
    let menu_due = match (opts.menu_at, status.finished_at) {
        (Some(t), Some(f)) if !*menu_fired => time.elapsed_secs_f64() - f >= t as f64,
        _ => false,
    };
    if menu_due {
        *menu_fired = true;
    }
    if keys.just_pressed(KeyCode::Escape) || menu_due {
        next.set(AppState::Launcher);
    }
    if keys.just_pressed(KeyCode::F3) {
        cull.0 = match cull.0 {
            None => Some(Face::Back),
            Some(Face::Back) => Some(Face::Front),
            Some(Face::Front) => None,
        };
        for (_, m) in materials.iter_mut() {
            if m.alpha_mode != AlphaMode::Blend {
                m.cull_mode = cull.0;
            }
        }
        for (_, m) in engine_materials.iter_mut() {
            if m.base.alpha_mode != AlphaMode::Blend && !m.base.double_sided {
                m.base.cull_mode = cull.0;
            }
        }
    }
    if keys.just_pressed(KeyCode::F12) {
        let path = format!(
            "screenshots/shot-{}.png",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
        );
        let _ = std::fs::create_dir_all("screenshots");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
}

fn auto_screenshot(
    mut commands: Commands,
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    mut status: ResMut<ZoneStatus>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(path) = &opts.screenshot else {
        return;
    };
    let Some(finished_at) = status.finished_at else {
        return;
    };
    let now = time.elapsed_secs_f64();
    if !status.screenshot_taken && now - finished_at >= opts.settle_seconds as f64 {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        info!("auto screenshot -> {}", path.display());
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
        status.screenshot_taken = true;
    } else if status.screenshot_taken && now - finished_at >= opts.settle_seconds as f64 + 2.0 {
        exit.write(AppExit::Success);
    }
}
