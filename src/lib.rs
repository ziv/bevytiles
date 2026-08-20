//! bevytiles — geo-spatial terrain streaming for Bevy (a port of raytiles).
//!
//! Quick start:
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

pub mod config;
pub mod height;
pub mod lod;
pub mod material;
pub mod source;
pub mod store;
pub mod synth;

pub mod prelude {
    pub use crate::config::{
        NetworkConfig, RenderingConfig, StreamingConfig, TerrainAnchor, TerrainCamera,
        TerrainStatus, WorldConfig, MAX_ZOOM, MIN_ZOOM, ZOOM_LEVELS,
    };
    pub use crate::height::{ground_height, HeightGrids};
    pub use crate::lod::TileKey;
    pub use crate::TerrainPlugin;
}

use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TerrainSet {
    /// Evict residents that are no longer needed.
    Reconcile,
    /// Drain the source and spawn newly finished tiles (budgeted).
    Promote,
    /// Rebuild the desired set / cancel / request (movement-gated).
    UpdateDesired,
    /// Loading progress bookkeeping (must run after UpdateDesired).
    Status,
}

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
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
                    (store::status, store::rebase, store::sync_rendering).in_set(TerrainSet::Status),
                ),
            );
    }
}
