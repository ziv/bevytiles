//! The tile store, ECS-native: one entity per resident tile; bookkeeping in
//! resources; frame steps as ordered systems (reconcile → promote →
//! update_desired → status). Bevy's visibility system replaces the frustum
//! engine (each tile carries a custom Aabb covering the displaced column).

use crate::config::*;
use crate::height::HeightGrids;
use crate::lod::{self, LodOptions, TileKey};
use crate::material::{TerrainMaterial, TerrainParams};
use crate::source::{TileDrop, TilePayload, TileRequest, TileSource};
use bevy::prelude::*;
use bevy::render::mesh::PlaneMeshBuilder;
use bevy::render::primitives::Aabb;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Component)]
pub struct Tile {
    pub key: TileKey,
    pub size: f32,
    /// Absolute tile center (f64 — precision far from the anchor).
    pub abs_x: f64,
    pub abs_z: f64,
}

#[derive(Resource, Default)]
pub struct TileIndex {
    pub resident: HashMap<TileKey, Entity>,
    pub desired: HashSet<TileKey>,
    pub loading: HashSet<TileKey>,
}

#[derive(Resource)]
pub struct ZoomMeshes(pub Vec<Handle<Mesh>>);

#[derive(Resource)]
pub struct SourceRes(pub TileSource);

#[derive(Resource, Default)]
pub struct PendingPayloads(pub VecDeque<TilePayload>);

#[derive(Resource)]
pub struct LastDesiredPos(pub Vec3);

impl Default for LastDesiredPos {
    fn default() -> Self {
        Self(Vec3::splat(-9_999_999.9))
    }
}

/// Coverage relations only change on promotion / desired rebuilds; the
/// (comparatively expensive) covered-by eviction checks are skipped while
/// this is clear. Exact, not a heuristic: evictions only remove cover.
#[derive(Resource)]
pub struct CoverageDirty(pub bool);

impl Default for CoverageDirty {
    fn default() -> Self {
        Self(true)
    }
}

pub fn zoom_size(world: &WorldConfig, zoom: u8) -> f64 {
    f64::from(world.tile_size) / f64::from(1u32 << (zoom - world.base_zoom))
}

// ---------------------------------------------------------------------------
// startup

pub fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, world: Res<WorldConfig>, net: Res<NetworkConfig>) {
    assert!(world.base_zoom >= MIN_ZOOM && world.max_zoom <= MAX_ZOOM && world.max_zoom >= world.base_zoom);

    // shared per-zoom grid meshes: resolution doubles 4 → 256 with zoom;
    // UVs span [0,1] (displacement samples by UV)
    let mut handles = Vec::new();
    let mut res = 4u32;
    for zoom in world.base_zoom..=world.max_zoom {
        let idx = (zoom - world.base_zoom) as usize;
        let size = zoom_size(&world, zoom) as f32 * world.skirt_overlap[idx];
        let mesh: Mesh = PlaneMeshBuilder::from_length(size).subdivisions(res - 1).build();
        handles.push(meshes.add(mesh));
        res = (res * 2).min(256);
    }
    commands.insert_resource(ZoomMeshes(handles));
    commands.insert_resource(SourceRes(TileSource::new(&net)));
}

// ---------------------------------------------------------------------------
// per-frame systems (chained: reconcile → promote → update_desired → status)

pub fn reconcile(
    mut commands: Commands,
    mut index: ResMut<TileIndex>,
    mut grids: ResMut<HeightGrids>,
    mut coverage_dirty: ResMut<CoverageDirty>,
    world: Res<WorldConfig>,
    anchor: Res<TerrainAnchor>,
    cams: Query<&Transform, With<TerrainCamera>>,
    vis: Query<&ViewVisibility, With<Tile>>,
) {
    let Ok(cam) = cams.single() else { return };
    let abs = cam.translation - anchor.world_offset;

    let mut evict = Vec::new();
    for (&key, &entity) in index.resident.iter() {
        if index.desired.contains(&key) {
            continue;
        }
        let remove = if key.zoom == world.base_zoom {
            // stale base tiles are the horizon — drop without thinking
            true
        } else if !vis.get(entity).map(|v| v.get()).unwrap_or(true) {
            // not on screen last frame
            true
        } else if lod::out_of_horizon(abs, zoom_size(&world, key.zoom), key) {
            true
        } else if !coverage_dirty.0 {
            false
        } else {
            // visible and in range: evict only if parent/children cover the
            // area, otherwise keep it to avoid holes in the ground
            covered(&index.resident, &world, key)
        };
        if remove {
            evict.push((key, entity));
        }
    }
    for (key, entity) in evict {
        commands.entity(entity).despawn();
        index.resident.remove(&key);
        grids.0.remove(&key);
    }
    coverage_dirty.0 = false;
}

fn covered(resident: &HashMap<TileKey, Entity>, world: &WorldConfig, key: TileKey) -> bool {
    let has = |zoom: u8, x: i32, z: i32| resident.contains_key(&TileKey { zoom, x, z });
    if key.zoom > world.base_zoom && has(key.zoom - 1, key.x >> 1, key.z >> 1) {
        return true;
    }
    if key.zoom < world.max_zoom {
        let (cx, cz) = (key.x * 2, key.z * 2);
        if has(key.zoom + 1, cx, cz) && has(key.zoom + 1, cx + 1, cz) && has(key.zoom + 1, cx, cz + 1) && has(key.zoom + 1, cx + 1, cz + 1) {
            return true;
        }
    }
    // grandparent / grandchildren: rare, but happens when zoom levels are
    // skipped by distance-based loading during fast movement
    if key.zoom > world.base_zoom + 1 && has(key.zoom - 2, key.x >> 2, key.z >> 2) {
        return true;
    }
    if key.zoom + 1 < world.max_zoom {
        let (cx, cz) = (key.x * 4, key.z * 4);
        return (0..4).all(|ox| (0..4).all(|oz| has(key.zoom + 2, cx + ox, cz + oz)));
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub fn promote(
    mut commands: Commands,
    mut index: ResMut<TileIndex>,
    mut pending: ResMut<PendingPayloads>,
    mut grids: ResMut<HeightGrids>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<TerrainMaterial>>,
    mut coverage_dirty: ResMut<CoverageDirty>,
    zoom_meshes: Res<ZoomMeshes>,
    source: Res<SourceRes>,
    world: Res<WorldConfig>,
    streaming: Res<StreamingConfig>,
    rendering: Res<RenderingConfig>,
    anchor: Res<TerrainAnchor>,
) {
    // drain the source: one non-blocking sweep per frame
    let mut ready = Vec::new();
    let mut dropped = Vec::new();
    source.0.drain(&mut ready, &mut dropped);
    for d in dropped {
        match d {
            TileDrop::Failed(key, reason) => {
                index.loading.remove(&key);
                warn!("tile {key:?} failed: {reason} - dropping until the next desired rebuild");
            }
            TileDrop::Cancelled(key) => {
                index.loading.remove(&key);
                // wanted again by the time the drop arrived — re-request now
                if index.desired.contains(&key) && !index.resident.contains_key(&key) {
                    request_tile(&source.0, &mut index, &world, key);
                }
            }
        }
    }
    pending.0.extend(ready);

    // budgeted promotion: entity + assets creation staggered per frame
    for _ in 0..streaming.max_promotions_per_frame {
        let Some(payload) = pending.0.pop_front() else { break };
        let key = payload.key;
        index.loading.remove(&key);
        if !index.desired.contains(&key) || index.resident.contains_key(&key) {
            continue; // no longer wanted (payload data just drops)
        }

        let albedo = images.add(make_image(payload.albedo.as_raw(), payload.albedo.width(), payload.albedo.height(), true));
        let height_rgba = rgb_to_rgba(&payload.height);
        let heightmap = images.add(make_image(&height_rgba, payload.height.width(), payload.height.height(), false));
        let normals_rgba = rgb_to_rgba(&payload.normals);
        let normals = images.add(make_image(&normals_rgba, payload.normals.width(), payload.normals.height(), false));
        let material = materials.add(TerrainMaterial {
            albedo,
            heightmap,
            normals,
            params: TerrainParams::from_config(&rendering),
        });

        let idx = (key.zoom - world.base_zoom) as usize;
        let size = zoom_size(&world, key.zoom);
        let abs_x = (f64::from(key.x) + 0.5) * size;
        let abs_z = (f64::from(key.z) + 0.5) * size;
        // user = absolute + offset, added in f64 before the single f32 cast
        let ux = (abs_x + f64::from(anchor.world_offset.x)) as f32;
        let uz = (abs_z + f64::from(anchor.world_offset.z)) as f32;
        let half = size as f32 * 0.5 * world.skirt_overlap[idx];

        let entity = commands
            .spawn((
                Mesh3d(zoom_meshes.0[idx].clone()),
                MeshMaterial3d(material),
                Transform::from_xyz(ux, 0.0, uz),
                // displacement happens on the GPU: the auto AABB (flat plane)
                // would cull visible mountains. Cover the full height column
                // (Dead Sea −430 m up to Everest, with margin).
                Aabb {
                    center: Vec3A::new(0.0, MAX_WORLD_HEIGHT * 0.5 - 250.0, 0.0),
                    half_extents: Vec3A::new(half, MAX_WORLD_HEIGHT * 0.5 + 300.0, half),
                },
                Tile { key, size: size as f32, abs_x, abs_z },
            ))
            .id();
        index.resident.insert(key, entity);
        grids.0.insert(key, payload.grid);
        coverage_dirty.0 = true; // a new resident can cover parent/children
    }
}

pub fn update_desired(
    mut index: ResMut<TileIndex>,
    mut last: ResMut<LastDesiredPos>,
    mut coverage_dirty: ResMut<CoverageDirty>,
    source: Res<SourceRes>,
    world: Res<WorldConfig>,
    streaming: Res<StreamingConfig>,
    anchor: Res<TerrainAnchor>,
    cams: Query<&Transform, With<TerrainCamera>>,
    mut scratch: Local<Vec<TileKey>>,
) {
    let Ok(cam) = cams.single() else { return };
    let abs = cam.translation - anchor.world_offset;
    if abs.distance_squared(last.0) <= streaming.update_distance * streaming.update_distance {
        return;
    }
    last.0 = abs;

    scratch.clear();
    let opts = LodOptions {
        base_zoom: world.base_zoom,
        max_zoom: world.max_zoom,
        base_tile_size: world.tile_size,
        radius: streaming.radius,
        thresholds: streaming.thresholds,
    };
    lod::desired_tiles(&opts, abs, &mut scratch);
    index.desired = scratch.iter().copied().collect();
    coverage_dirty.0 = true;

    // cancel once, here — the desired set only changes in this system
    let stale: Vec<TileKey> = index.loading.iter().filter(|k| !index.desired.contains(k)).copied().collect();
    for key in stale {
        source.0.cancel(key);
    }

    let missing: Vec<TileKey> = index
        .desired
        .iter()
        .filter(|k| !index.resident.contains_key(k) && !index.loading.contains(k))
        .copied()
        .collect();
    for key in missing {
        request_tile(&source.0, &mut index, &world, key);
    }
}

fn request_tile(source: &TileSource, index: &mut TileIndex, world: &WorldConfig, key: TileKey) {
    let scale = 1i32 << (key.zoom - world.base_zoom);
    source.request(TileRequest {
        key,
        x: key.x + world.anchor_x * scale,
        z: key.z + world.anchor_z * scale,
    });
    index.loading.insert(key);
}

/// Initial-loading contract: `loading` flips only once a desired set EXISTS
/// and is fully serviced. This system must run after `update_desired` — the
/// C++ engine shipped a bug from evaluating this before the first rebuild.
pub fn status(mut st: ResMut<TerrainStatus>, index: Res<TileIndex>, pending: Res<PendingPayloads>) {
    st.resident = index.resident.len();
    if index.desired.is_empty() {
        st.progress = 0.0;
        return;
    }
    let have = index.desired.iter().filter(|k| index.resident.contains_key(k)).count();
    st.progress = have as f32 / index.desired.len() as f32;
    if st.loading && index.loading.is_empty() && pending.0.is_empty() {
        st.loading = false;
        info!("initial load complete: {} tiles resident", index.resident.len());
    }
}

/// Rebake every tile transform after a large-world rebase.
pub fn rebase(anchor: Res<TerrainAnchor>, mut tiles: Query<(&Tile, &mut Transform)>) {
    if !anchor.is_changed() {
        return;
    }
    for (tile, mut tf) in &mut tiles {
        tf.translation.x = (tile.abs_x + f64::from(anchor.world_offset.x)) as f32;
        tf.translation.z = (tile.abs_z + f64::from(anchor.world_offset.z)) as f32;
    }
}

/// Push RenderingConfig changes to every live material.
pub fn sync_rendering(rendering: Res<RenderingConfig>, mut materials: ResMut<Assets<TerrainMaterial>>) {
    if !rendering.is_changed() {
        return;
    }
    let params = TerrainParams::from_config(&rendering);
    let handles: Vec<_> = materials.iter().map(|(id, _)| id).collect();
    for id in handles {
        if let Some(mat) = materials.get_mut(id) {
            mat.params = params;
        }
    }
}

// ---------------------------------------------------------------------------
// image helpers

fn rgb_to_rgba(img: &image::RgbImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.as_raw().len() / 3 * 4);
    for p in img.pixels() {
        out.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    out
}

/// srgb=true for the albedo (color data); FALSE for heightmap/normals — an
/// sRGB view would gamma-warp the Terrarium decode into garbage.
fn make_image(data: &[u8], w: u32, h: u32, srgb: bool) -> Image {
    let format = if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm };
    let mut img = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data.to_vec(),
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..Default::default()
    });
    img
}
