//! The tile source: background fetching of texture + heightmap + normals per
//! tile, PNG decoding, heightmap synthesis above the native zoom, and
//! delivery of whole-tile payloads through channels drained once per frame.
//!
//! Two backends share this module's types and helpers:
//!
//! * [`native`] (everything but wasm): plain `std::thread` workers + blocking
//!   HTTP (`ureq`) + an on-disk cache with atomic write-through. Deliberately
//!   NOT bevy_tasks: the raytiles pool semantics — dedup by key, cancel flags
//!   checked between assets, real jobs prioritized over background synthesis
//!   — are ported as-is.
//! * [`web`] (`wasm32`): browser `fetch` futures on Bevy's [`IoTaskPool`]
//!   (which runs on the main thread in the browser), with
//!   [`NetworkConfig::threads`] reinterpreted as the max number of concurrent
//!   tile fetches and a small in-memory cache of native-zoom heightmaps in
//!   place of the disk cache (`cache_dir` is ignored).
//!
//! [`IoTaskPool`]: bevy::tasks::IoTaskPool
//! [`NetworkConfig::threads`]: crate::config::NetworkConfig::threads

use crate::height::HeightGrid;
use crate::lod::TileKey;
use image::{ImageReader, RgbImage, RgbaImage};
use std::io::Cursor;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::TileSource;

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::TileSource;

/// One tile fetch: anchor-relative identity + absolute provider coordinates
/// at `key.zoom` (the caller resolves the anchor; the source knows nothing
/// about world anchoring).
#[derive(Clone, Copy, Debug)]
pub struct TileRequest {
    /// Anchor-relative identity, used for dedup/cancel and echoed in the
    /// payload/drop.
    pub key: TileKey,
    /// Absolute provider tile column at `key.zoom`.
    pub x: i32,
    /// Absolute provider tile row at `key.zoom`.
    pub z: i32,
}

/// A completed tile: all three assets decoded, plus the CPU height grid
/// (built here so the main thread never pays for it).
pub struct TilePayload {
    /// The request's anchor-relative key.
    pub key: TileKey,
    /// Decoded satellite imagery.
    pub albedo: RgbaImage,
    /// Decoded (or synthesized) Terrarium heightmap.
    pub height: RgbImage,
    /// Decoded normal map, or the flat default.
    pub normals: RgbImage,
    /// CPU height grid derived from `height` on the worker.
    pub grid: HeightGrid,
}

/// A tile that will not arrive. Every [`TileSource::request`] is answered by
/// exactly one [`TilePayload`] or one of these.
pub enum TileDrop {
    /// The job was cancelled before completing. If the tile is wanted again,
    /// re-request immediately (the camera came back).
    Cancelled(TileKey),
    /// Fetch or decode failed; the string is the reason. Callers should wait
    /// for the next desired-set rebuild rather than hot-retrying a failing
    /// provider.
    Failed(TileKey, String),
}

/// Which of the three per-tile assets.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Texture,
    Heightmap,
    Normals,
}

impl Kind {
    pub(crate) fn url_template(self, cfg: &crate::config::NetworkConfig) -> &str {
        match self {
            Kind::Texture => &cfg.texture_url,
            Kind::Heightmap => &cfg.heightmap_url,
            Kind::Normals => &cfg.normals_url,
        }
    }
}

pub(crate) const CANCELLED: &str = "\u{0}cancelled";

pub(crate) fn expand_url(template: &str, zoom: u8, x: i32, z: i32) -> String {
    template
        .replacen(":zoom:", &zoom.to_string(), 1)
        .replacen(":x:", &x.to_string(), 1)
        .replacen(":y:", &z.to_string(), 1)
}

pub(crate) fn decode_png(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("png sniff: {e}"))?
        .decode()
        .map_err(|e| format!("png decode: {e}"))
}

/// Synthesize the heightmap for (`zoom`, `x`, `z`) from its decoded native
/// ancestor's float heights (`w`×`h`): quadrant-chain upsample from
/// `native + 1` up to `zoom`.
pub(crate) fn synthesize_heightmap(
    parent_floats: &[f32],
    w: usize,
    h: usize,
    native: u8,
    zoom: u8,
    x: i32,
    z: i32,
) -> RgbImage {
    let mut floats = parent_floats.to_vec();
    for level in (native + 1)..=zoom {
        let shift = zoom - level;
        let qx = ((x >> shift) & 1) as usize;
        let qz = ((z >> shift) & 1) as usize;
        floats = crate::synth::upsample_quadrant(&floats, w, h, qx, qz);
    }
    crate::synth::encode_terrarium(&floats, w as u32, h as u32)
}
