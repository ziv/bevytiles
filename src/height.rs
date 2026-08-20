//! CPU-side height grids for `ground_height` queries. Built on the worker
//! threads (main-thread time is the scarce resource); 128 KB per 256² tile
//! vs 256 KB for a retained RGBA image.

use crate::config::{TerrainAnchor, WorldConfig};
use crate::lod::TileKey;
use bevy::prelude::*;
use image::RgbImage;
use std::collections::HashMap;

/// `round(height_m) + 32768` per texel — integer-meter resolution (the
/// Terrarium source is only meter-accurate; bilinear sampling smooths it).
#[derive(Clone)]
pub struct HeightGrid {
    pub w: u32,
    pub h: u32,
    pub samples: Vec<u16>,
}

impl HeightGrid {
    pub fn from_terrarium(img: &RgbImage) -> Self {
        let samples = img
            .pixels()
            .map(|p| {
                let h = f64::from(p[0]) * 256.0 + f64::from(p[1]) + f64::from(p[2]) / 256.0 - 32768.0;
                (h.round() + 32768.0).clamp(0.0, 65535.0) as u16
            })
            .collect();
        Self { w: img.width(), h: img.height(), samples }
    }

    /// Bilinear sample at normalized (u, v) ∈ [0, 1]; texel centers at
    /// (i + 0.5)/n, edges clamped. Returns meters.
    pub fn sample(&self, u: f32, v: f32) -> f32 {
        let s = |x: i32, y: i32| -> f32 {
            let x = x.clamp(0, self.w as i32 - 1) as u32;
            let y = y.clamp(0, self.h as i32 - 1) as u32;
            f32::from(self.samples[(y * self.w + x) as usize])
        };
        let fx = u * self.w as f32 - 0.5;
        let fy = v * self.h as f32 - 0.5;
        let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let top = s(x0, y0) * (1.0 - tx) + s(x0 + 1, y0) * tx;
        let bot = s(x0, y0 + 1) * (1.0 - tx) + s(x0 + 1, y0 + 1) * tx;
        (top * (1.0 - ty) + bot * ty) - 32768.0
    }
}

#[derive(Resource, Default)]
pub struct HeightGrids(pub HashMap<TileKey, HeightGrid>);

/// Terrain altitude under `pos` (user space): walks zooms finest→coarsest and
/// samples the first resident grid containing the XZ point. O(zoom levels).
pub fn ground_height(
    grids: &HeightGrids,
    world: &WorldConfig,
    anchor: &TerrainAnchor,
    pos: Vec3,
) -> Option<f32> {
    let abs = pos - anchor.world_offset;
    for zoom in (world.base_zoom..=world.max_zoom).rev() {
        let size = f64::from(world.tile_size) / f64::from(1u32 << (zoom - world.base_zoom));
        let tx = (f64::from(abs.x) / size).floor() as i32;
        let tz = (f64::from(abs.z) / size).floor() as i32;
        if let Some(grid) = grids.0.get(&TileKey { zoom, x: tx, z: tz }) {
            let u = ((f64::from(abs.x) - tx as f64 * size) / size) as f32;
            let v = ((f64::from(abs.z) - tz as f64 * size) / size) as f32;
            return Some(grid.sample(u, v));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_centers_midpoints_and_edges() {
        // 2x1 grid: 100 m and 300 m
        let grid = HeightGrid { w: 2, h: 1, samples: vec![32768 + 100, 32768 + 300] };
        assert!((grid.sample(0.25, 0.5) - 100.0).abs() < 1e-3);
        assert!((grid.sample(0.75, 0.5) - 300.0).abs() < 1e-3);
        assert!((grid.sample(0.5, 0.5) - 200.0).abs() < 1e-3);
        assert!((grid.sample(0.0, 0.0) - 100.0).abs() < 1e-3);
        assert!((grid.sample(1.0, 1.0) - 300.0).abs() < 1e-3);
    }

    #[test]
    fn from_terrarium_rounds_meters() {
        let img = crate::synth::encode_terrarium(&[0.0, 8848.0, -415.0, 100.5], 4, 1);
        let grid = HeightGrid::from_terrarium(&img);
        assert_eq!(grid.samples[0], 32768);
        assert_eq!(grid.samples[1], 32768 + 8848);
        assert_eq!(grid.samples[2], 32768 - 415);
        assert!(grid.samples[3] == 32768 + 100 || grid.samples[3] == 32768 + 101);
    }
}
