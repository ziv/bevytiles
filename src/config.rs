//! Public configuration resources. Mirrors raytiles' nested `config` — every
//! field defaulted; insert any of these before `app.run()` to override.

use bevy::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

pub const MIN_ZOOM: u8 = 9;
pub const MAX_ZOOM: u8 = 22;
pub const ZOOM_LEVELS: usize = (MAX_ZOOM - MIN_ZOOM + 1) as usize; // 14

/// Highest terrain elevation the culling AABBs must cover (Everest, meters).
pub const MAX_WORLD_HEIGHT: f32 = 8848.0;

const EQUATOR_CIRCUMFERENCE_M: f64 = 40_075_016.686;

/// World topology. Effectively immutable once the plugin started.
#[derive(Resource, Clone)]
pub struct WorldConfig {
    /// Anchor in tile coordinates at `base_zoom` (world origin sits there).
    pub anchor_x: i32,
    pub anchor_z: i32,
    /// Lowest LOD zoom ever loaded (>= MIN_ZOOM).
    pub base_zoom: u8,
    /// Highest LOD zoom (<= MAX_ZOOM). Defaults to the native terrain ceiling
    /// (15); raising it beyond `NetworkConfig::native_terrain_zoom` opts into
    /// synthesized heightmaps and default normals.
    pub max_zoom: u8,
    /// World size (meters) of one tile at `base_zoom`.
    pub tile_size: f32,
    /// Per-zoom mesh overlap factors, `skirt_overlap[zoom - base_zoom]`.
    pub skirt_overlap: [f32; ZOOM_LEVELS],
    /// World-space offset of the anchor point inside its anchor tile; filled
    /// by `from_lat_lon` so the origin sits exactly on the coordinate.
    pub origin_offset: Vec3,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            anchor_x: 306,
            anchor_z: 207,
            base_zoom: MIN_ZOOM,
            max_zoom: 15,
            tile_size: 66_400.0,
            skirt_overlap: [1.0; ZOOM_LEVELS],
            origin_offset: Vec3::ZERO,
        }
    }
}

impl WorldConfig {
    /// Anchor the world at a geographic coordinate (degrees): derives the
    /// anchor tile, tile size, and origin offset (web-mercator, same math as
    /// raytiles).
    pub fn from_lat_lon(lat: f64, lon: f64) -> Self {
        let n = 2f64.powi(MIN_ZOOM as i32);
        let lat_rad = lat.to_radians();
        let x = (lon + 180.0) / 360.0 * n;
        let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
        let anchor_x = x.floor() as i32;
        let anchor_z = y.floor() as i32;
        let tile_size = EQUATOR_CIRCUMFERENCE_M * lat_rad.cos() / n;
        Self {
            anchor_x,
            anchor_z,
            tile_size: tile_size as f32,
            origin_offset: Vec3::new(
                ((x - anchor_x as f64) * tile_size) as f32,
                0.0,
                ((y - anchor_z as f64) * tile_size) as f32,
            ),
            ..Default::default()
        }
    }

    /// A sensible initial camera position over the anchor, `altitude` m up.
    pub fn initial_position(&self, altitude: f32) -> Vec3 {
        self.origin_offset + Vec3::Y * altitude
    }
}

/// Which tiles are kept resident and how aggressively the set updates.
#[derive(Resource, Clone)]
pub struct StreamingConfig {
    /// Radius, in base-zoom tiles, of the disc loaded around the camera.
    pub radius: i32,
    /// Per-zoom subdivision distance thresholds (meters),
    /// `thresholds[zoom - base_zoom]`.
    pub thresholds: [f32; ZOOM_LEVELS],
    /// Camera travel (meters) that triggers a desired-set rebuild.
    pub update_distance: f32,
    /// Cap on tile promotions (entity spawns + asset creation) per frame.
    pub max_promotions_per_frame: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            radius: 6,
            thresholds: [
                100_000.0, 80_000.0, 40_000.0, 20_000.0, 10_000.0, 5_000.0, 2_500.0, 1_250.0,
                625.0, 312.0, 156.0, 78.0, 39.0, 20.0,
            ],
            update_distance: 500.0,
            max_promotions_per_frame: 8,
        }
    }
}

/// Rendering / shader parameters. All runtime-mutable: mutate the resource
/// and the plugin pushes it to every live material.
#[derive(Resource, Clone)]
pub struct RenderingConfig {
    pub fog_start: f32,
    pub fog_end: f32,
    /// Vertical drop (meters) of skirt geometry below tile edges; 0 disables.
    pub skirt_drop: f32,
    /// Match this to your sky color for a seamless horizon.
    pub fog_color: Color,
    pub ambient: Color,
    /// Normalized internally; magnitude is irrelevant.
    pub sun_direction: Vec3,
    pub sun_scale: f32,
    /// Terrain relief exaggeration (drama factor).
    pub height_scale: f32,
    pub normals_scale: f32,
}

impl Default for RenderingConfig {
    fn default() -> Self {
        Self {
            fog_start: 100_000.0,
            fog_end: 150_000.0,
            skirt_drop: 0.0,
            fog_color: Color::srgb(0.0, 0.0, 1.0),
            ambient: Color::WHITE,
            sun_direction: Vec3::new(0.1, 1.0, 0.1),
            sun_scale: 1.0,
            height_scale: 1.0,
            normals_scale: 1.0,
        }
    }
}

/// Tile download / cache parameters.
#[derive(Resource, Clone)]
pub struct NetworkConfig {
    pub threads: usize,
    /// Root of the on-disk cache: `cache_dir/{texture,heightmap,normals}/z/x/y.png`.
    pub cache_dir: PathBuf,
    /// Provider URL templates with `:zoom:`/`:x:`/`:y:` tokens. The Esri
    /// texture default uses `zoom/y/x` order — that swap is intentional.
    pub texture_url: String,
    pub heightmap_url: String,
    pub normals_url: String,
    /// Highest zoom the terrain providers serve natively (Mapzen: 15). Above
    /// it heightmaps are synthesized from ancestors and normals default; no
    /// HTTP is attempted for either.
    pub native_terrain_zoom: u8,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            threads: 4,
            cache_dir: PathBuf::from(".cache"),
            texture_url:
                "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/:zoom:/:y:/:x:"
                    .into(),
            heightmap_url: "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/:zoom:/:x:/:y:.png".into(),
            normals_url: "https://s3.amazonaws.com/elevation-tiles-prod/normal/:zoom:/:x:/:y:.png".into(),
            native_terrain_zoom: 15,
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(3),
        }
    }
}

/// Large-world shifting input: `absolute = user − world_offset`. The app owns
/// rebasing (shift camera + everything + this offset together); the plugin
/// rebakes tile transforms whenever it changes.
#[derive(Resource, Default, Clone)]
pub struct TerrainAnchor {
    pub world_offset: Vec3,
}

/// Marker for the camera that drives streaming.
#[derive(Component)]
pub struct TerrainCamera;

/// Initial-load status for splash screens. `loading` starts true and flips
/// only once a desired set exists and is fully serviced.
#[derive(Resource)]
pub struct TerrainStatus {
    pub loading: bool,
    /// Fraction of the desired set that is resident, [0, 1].
    pub progress: f32,
    /// Tiles drawn... resident count (debug convenience).
    pub resident: usize,
}

impl Default for TerrainStatus {
    fn default() -> Self {
        Self { loading: true, progress: 0.0, resident: 0 }
    }
}
