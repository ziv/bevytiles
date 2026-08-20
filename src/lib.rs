//! # bevytiles
//!
//! Geo-spatial terrain streaming for Bevy — a Rust port of the
//! [raytiles](https://github.com/ziv/raytiles) engine. Streams satellite
//! imagery, [Terrarium](https://registry.opendata.aws/terrain-tiles/)
//! heightmaps, and normal maps around a moving camera and renders them as
//! GPU-displaced terrain.
//!
//! ## Quick start
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevytiles::prelude::*;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins)
//!     .insert_resource(WorldConfig::from_lat_lon(46.206889, 9.497194))
//!     .add_plugins(TerrainPlugin)
//!     .add_systems(Startup, |mut c: Commands, w: Res<WorldConfig>| {
//!         c.spawn((Camera3d::default(), TerrainCamera,
//!                  Transform::from_translation(w.initial_position(5000.0))));
//!     })
//!     .run();
//! ```
//!
//! Configuration is four resources ([`WorldConfig`](config::WorldConfig),
//! [`StreamingConfig`](config::StreamingConfig),
//! [`RenderingConfig`](config::RenderingConfig),
//! [`NetworkConfig`](config::NetworkConfig)), all defaulted — insert any of
//! them before `run()` to override. Mark the streaming camera with
//! [`TerrainCamera`](config::TerrainCamera).
//!
//! ## Architecture
//!
//! One [`Entity`] per resident tile; Bevy's visibility system culls them via
//! per-tile AABBs; the [`material::TerrainMaterial`] displaces flat shared
//! grid meshes in the vertex shader. Around that, four components mirror the
//! raytiles design:
//!
//! | module | role |
//! |---|---|
//! | [`lod`] | pure desired-set policy (which tiles *should* exist for a camera position) |
//! | [`source`] | worker threads: HTTP + disk cache + PNG/JPEG decode + terrain synthesis, delivering whole-tile payloads through channels |
//! | [`store`] | ECS systems tying it together: eviction, budgeted promotion, desired-set upkeep, status |
//! | [`height`] | CPU-side height grids answering [`ground_height`](height::ground_height) queries |
//!
//! ## Coordinate spaces
//!
//! Two frames, one convention: **`absolute = user − world_offset`**.
//! Tile math lives in absolute space (fixed origin at the anchor tile);
//! entity transforms live in user space (small floats near the camera).
//! For worlds larger than a few kilometers the app must *rebase*: shift the
//! camera, every user-space entity, and
//! [`TerrainAnchor::world_offset`](config::TerrainAnchor) by the same amount
//! — the plugin rebakes tile transforms whenever the offset changes. See
//! `examples/demo.rs` for the pattern.
//!
//! ## Frame loop
//!
//! Systems run in [`Update`], ordered by [`TerrainSet`]:
//! reconcile → promote → update-desired → status. The order is load-bearing;
//! see each variant's documentation.

#![warn(missing_docs)]

pub mod config;
pub mod height;
pub mod lod;
pub mod material;
pub mod source;
pub mod store;
pub mod synth;

/// The commonly needed surface: plugin, configs, markers, and queries.
pub mod prelude {
    pub use crate::config::{
        NetworkConfig, RenderingConfig, StreamingConfig, TerrainAnchor, TerrainCamera,
        TerrainStatus, WorldConfig, MAX_ZOOM, MIN_ZOOM, ZOOM_LEVELS,
    };
    pub use crate::height::{ground_height, HeightGrids};
    pub use crate::lod::TileKey;
    pub use crate::TerrainPlugin;
}

use bevy::asset::load_internal_asset;
use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;
use bevy::shader::Shader;

/// Ordering of the terrain systems within [`Update`]. Chained in declaration
/// order; the sequence mirrors raytiles' frame loop and is load-bearing —
/// in particular, [`Status`](TerrainSet::Status) must observe the desired set
/// *after* [`UpdateDesired`](TerrainSet::UpdateDesired) rebuilt it, or the
/// initial-loading flag flips before anything was ever requested.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TerrainSet {
    /// Evict resident tiles that are no longer needed (not desired and
    /// off-screen / beyond the horizon / covered by other zoom levels).
    Reconcile,
    /// Drain the [`source`](crate::source::TileSource) and spawn newly
    /// finished tiles, capped per frame
    /// ([`StreamingConfig::max_promotions_per_frame`](config::StreamingConfig)).
    Promote,
    /// Rebuild the desired tile set when the camera moved far enough; cancel
    /// stale downloads and request missing tiles.
    UpdateDesired,
    /// Update [`TerrainStatus`](config::TerrainStatus), rebake transforms
    /// after a rebase, and push [`RenderingConfig`](config::RenderingConfig)
    /// changes to live materials.
    Status,
}

/// The terrain engine. Add after `DefaultPlugins`; insert any of the config
/// resources first to override the defaults (they are also insertable
/// afterwards — everything is read at `Startup` or later).
///
/// Requires exactly one camera marked with
/// [`TerrainCamera`](config::TerrainCamera) to drive streaming; with none
/// present the terrain systems idle.
pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        // embed the terrain shader into the binary so the plugin is
        // self-contained — consumers need no asset-folder setup
        load_internal_asset!(
            app,
            material::TERRAIN_SHADER_HANDLE,
            "../assets/shaders/terrain.wgsl",
            Shader::from_wgsl
        );

        app.add_plugins(MaterialPlugin::<material::TerrainMaterial>::default())
            .init_resource::<config::WorldConfig>()
            .init_resource::<config::StreamingConfig>()
            .init_resource::<config::RenderingConfig>()
            .init_resource::<config::NetworkConfig>()
            .init_resource::<config::TerrainAnchor>()
            .init_resource::<config::TerrainStatus>()
            .init_resource::<height::HeightGrids>()
            .init_resource::<store::TileIndex>()
            .init_resource::<store::PendingPayloads>()
            .init_resource::<store::LastDesiredPos>()
            .init_resource::<store::CoverageDirty>()
            .configure_sets(
                Update,
                (
                    TerrainSet::Reconcile,
                    TerrainSet::Promote,
                    TerrainSet::UpdateDesired,
                    TerrainSet::Status,
                )
                    .chain(),
            )
            .add_systems(Startup, store::setup)
            .add_systems(
                Update,
                (
                    store::reconcile.in_set(TerrainSet::Reconcile),
                    store::promote.in_set(TerrainSet::Promote),
                    store::update_desired.in_set(TerrainSet::UpdateDesired),
                    (store::status, store::rebase, store::sync_rendering)
                        .in_set(TerrainSet::Status),
                ),
            );
    }
}
