//! Shared tile edges. Each tile's shader displaces its surface from its own
//! heightmap, so neighbouring tiles disagree along their common edge: two
//! tiles of one zoom sample their own last texel row, about a texel apart,
//! and a finer tile's extra edge vertices miss the coarser tile's straight
//! edge between its vertices. Both open cracks.
//!
//! Every tile instead takes its edge vertices' heights from a small `R32Float`
//! texture (`res + 1` texels a side, a row per side) that this module keeps:
//! the mean of the two tiles' own borders against a resident tile of its zoom,
//! the coarser tile's edge where it borders one, and its own border otherwise.
//! A tile's edges are recomputed when a tile touching it is promoted or evicted.

use crate::config::WorldConfig;
use crate::lod::TileKey;
use crate::mesh::mesh_res;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use image::RgbImage;
use std::collections::HashMap;

/// The sides of a tile, in its edge texture's row order: the edge at `v = 0`
/// (towards `z - 1`), at `v = 1` (`z + 1`), at `u = 0` (`x - 1`) and at
/// `u = 1` (`x + 1`). Along the first two the vertices run with `u`, along the
/// last two with `v`.
const SIDES: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];

fn opposite(side: usize) -> usize {
    side ^ 1
}

/// A resident tile's border as its own heightmap gives it, and the edge
/// texture its shader reads.
pub struct Border {
    /// Quads a side of the tile's mesh.
    pub res: u32,
    /// Heights (meters) at the edge vertices, `res + 1` per side, by side.
    own: [Vec<f32>; 4],
    /// The edge heights last written to `image`.
    shown: Vec<f32>,
    /// The tile's edge texture.
    pub image: Handle<Image>,
}

/// Every resident tile's [`Border`], kept in step with residency by the store,
/// and the tiles promoted or evicted since the edges were last stitched.
#[derive(Resource, Default)]
pub struct TileBorders {
    tiles: HashMap<TileKey, Border>,
    changed: Vec<TileKey>,
}

impl TileBorders {
    /// A promoted tile's border from its Terrarium heightmap, and its edge
    /// texture, filled with that border until the next stitch.
    pub fn insert(
        &mut self,
        images: &mut Assets<Image>,
        world: &WorldConfig,
        key: TileKey,
        height: &RgbImage,
    ) -> Handle<Image> {
        let res = mesh_res(world, key.zoom);
        let own = std::array::from_fn(|side| {
            (0..=res)
                .map(|i| {
                    let (u, v) = side_uv(side, i as f32 / res as f32);
                    terrarium_sample(height, u, v)
                })
                .collect()
        });
        let border = Border {
            res,
            own,
            shown: Vec::new(),
            image: Handle::default(),
        };
        let shown: Vec<f32> = border.own.concat();
        let image = images.add(edge_image(res, &shown));
        self.tiles.insert(
            key,
            Border {
                shown,
                image: image.clone(),
                ..border
            },
        );
        self.changed.push(key);
        image
    }

    /// An evicted tile's border.
    pub fn remove(&mut self, key: TileKey) {
        if self.tiles.remove(&key).is_some() {
            self.changed.push(key);
        }
    }

    /// The four sides' edge heights of `key`, `res + 1` per side.
    fn stitched(&self, world: &WorldConfig, key: TileKey) -> [Vec<f32>; 4] {
        let mut sides: [Vec<f32>; 4] = std::array::from_fn(|side| self.side(world, key, side));
        for (cu, cv) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let along_v = cv as usize;
            let along_u = 2 + cu as usize;
            let res = self.tiles[&key].res as usize;
            let corner = if self.coarser(world, key, along_v).is_some() {
                sides[along_v][cu as usize * res]
            } else if self.coarser(world, key, along_u).is_some() {
                sides[along_u][cv as usize * res]
            } else {
                self.corner_mean(key, cu, cv)
            };
            sides[along_v][cu as usize * res] = corner;
            sides[along_u][cv as usize * res] = corner;
        }
        sides
    }

    /// One side's edge heights: shared with a resident tile of the same zoom,
    /// taken from a coarser resident neighbour's edge, or the tile's own.
    fn side(&self, world: &WorldConfig, key: TileKey, side: usize) -> Vec<f32> {
        let border = &self.tiles[&key];
        let (dx, dz) = SIDES[side];
        let same = TileKey {
            zoom: key.zoom,
            x: key.x + dx,
            z: key.z + dz,
        };
        if let Some(other) = self.tiles.get(&same) {
            return border.own[side]
                .iter()
                .zip(&other.own[opposite(side)])
                .map(|(a, b)| (a + b) * 0.5)
                .collect();
        }
        let Some(coarse) = self.coarser(world, key, side) else {
            return border.own[side].clone();
        };
        let edge = &self.stitched(world, coarse)[opposite(side)];
        let res_c = self.tiles[&coarse].res as f32;
        let dzoom = key.zoom - coarse.zoom;
        let start = if side < 2 {
            key.x - (coarse.x << dzoom)
        } else {
            key.z - (coarse.z << dzoom)
        };
        let span = (1u32 << dzoom) as f32;
        (0..=border.res)
            .map(|i| {
                let t = (start as f32 + i as f32 / border.res as f32) / span;
                let at = (t * res_c).clamp(0.0, res_c);
                let j = (at.floor() as usize).min(edge.len() - 2);
                let f = at - j as f32;
                edge[j] * (1.0 - f) + edge[j + 1] * f
            })
            .collect()
    }

    /// The resident tile of a coarser zoom across `side`, if one borders this
    /// tile there; a coarser tile covering this one isn't a neighbour.
    fn coarser(&self, world: &WorldConfig, key: TileKey, side: usize) -> Option<TileKey> {
        let (dx, dz) = SIDES[side];
        for dzoom in 1..=(key.zoom - world.base_zoom) {
            let across = TileKey {
                zoom: key.zoom - dzoom,
                x: (key.x + dx) >> dzoom,
                z: (key.z + dz) >> dzoom,
            };
            if across.x == key.x >> dzoom && across.z == key.z >> dzoom {
                return None;
            }
            if self.tiles.contains_key(&across) {
                return Some(across);
            }
        }
        None
    }

    /// The mean of the own corner heights of the resident tiles of this zoom
    /// meeting at corner `(cu, cv)` of `key`.
    fn corner_mean(&self, key: TileKey, cu: i32, cv: i32) -> f32 {
        let mut sum = 0.0;
        let mut count = 0.0;
        for ox in [0, 2 * cu - 1] {
            for oz in [0, 2 * cv - 1] {
                let other = TileKey {
                    zoom: key.zoom,
                    x: key.x + ox,
                    z: key.z + oz,
                };
                if let Some(border) = self.tiles.get(&other) {
                    let (lu, lv) = ((cu - ox) as usize, (cv - oz) as usize);
                    sum += border.own[lv][lu * border.res as usize];
                    count += 1.0;
                }
            }
        }
        sum / count
    }
}

/// After promotion and eviction: the edges of every resident tile touching a
/// tile promoted or evicted since the last stitch, written to the edge
/// textures that changed.
pub fn stitch(
    mut borders: ResMut<TileBorders>,
    mut images: ResMut<Assets<Image>>,
    world: Res<WorldConfig>,
) {
    if borders.changed.is_empty() {
        return;
    }
    let changed = std::mem::take(&mut borders.changed);
    let max_zoom = world.max_zoom;
    let touched: Vec<TileKey> = borders
        .tiles
        .keys()
        .copied()
        .filter(|&key| changed.iter().any(|&c| touching(key, c, max_zoom)))
        .collect();
    for key in touched {
        let shown = borders.stitched(&world, key).concat();
        let border = borders.tiles.get_mut(&key).expect("a resident tile");
        if shown == border.shown {
            continue;
        }
        if let Some(mut image) = images.get_mut(&border.image) {
            image.data = Some(f32_bytes(&shown));
        }
        border.shown = shown;
    }
}

/// Whether two tiles' squares meet or overlap, edges and corners included.
fn touching(a: TileKey, b: TileKey, max_zoom: u8) -> bool {
    let span = |key: TileKey| {
        let shift = max_zoom.saturating_sub(key.zoom);
        let (x, z) = (i64::from(key.x) << shift, i64::from(key.z) << shift);
        (x, z, x + (1i64 << shift), z + (1i64 << shift))
    };
    let (ax0, az0, ax1, az1) = span(a);
    let (bx0, bz0, bx1, bz1) = span(b);
    ax0 <= bx1 && bx0 <= ax1 && az0 <= bz1 && bz0 <= az1
}

/// The `(u, v)` of the point `t` along `side`.
fn side_uv(side: usize, t: f32) -> (f32, f32) {
    match side {
        0 => (t, 0.0),
        1 => (t, 1.0),
        2 => (0.0, t),
        _ => (1.0, t),
    }
}

/// A Terrarium heightmap's height (meters) at `(u, v)`, bilinear between
/// texel centres and clamped at the edges, as the shader's sampler reads it.
fn terrarium_sample(img: &RgbImage, u: f32, v: f32) -> f32 {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let texel = |x: i32, y: i32| {
        let p = img.get_pixel(x.clamp(0, w - 1) as u32, y.clamp(0, h - 1) as u32);
        f32::from(p[0]) * 256.0 + f32::from(p[1]) + f32::from(p[2]) / 256.0 - 32768.0
    };
    let fx = u * w as f32 - 0.5;
    let fy = v * h as f32 - 0.5;
    let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let top = texel(x0, y0) * (1.0 - tx) + texel(x0 + 1, y0) * tx;
    let bottom = texel(x0, y0 + 1) * (1.0 - tx) + texel(x0 + 1, y0 + 1) * tx;
    top * (1.0 - ty) + bottom * ty
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn edge_image(res: u32, heights: &[f32]) -> Image {
    Image::new(
        Extent3d {
            width: res + 1,
            height: 4,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        f32_bytes(heights),
        TextureFormat::R32Float,
        RenderAssetUsages::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> WorldConfig {
        WorldConfig {
            base_zoom: 10,
            max_zoom: 12,
            ..Default::default()
        }
    }

    /// A heightmap whose height rises with x, by `slope` meters a texel, plus
    /// `offset`
    fn ramp(slope: f32, offset: f32) -> RgbImage {
        RgbImage::from_fn(256, 256, |x, _| {
            let h = offset + slope * x as f32 + 32768.0;
            let r = (h / 256.0).floor();
            let g = (h - r * 256.0).floor();
            let b = ((h - r * 256.0 - g) * 256.0).round();
            image::Rgb([r as u8, g as u8, b as u8])
        })
    }

    fn key(zoom: u8, x: i32, z: i32) -> TileKey {
        TileKey { zoom, x, z }
    }

    #[test]
    fn tiles_of_one_zoom_share_their_common_edge() {
        let world = world();
        let mut images = Assets::<Image>::default();
        let mut borders = TileBorders::default();
        let (a, b) = (key(11, 0, 0), key(11, 1, 0));
        borders.insert(&mut images, &world, a, &ramp(1.0, 0.0));
        borders.insert(&mut images, &world, b, &ramp(1.0, 300.0));
        let east = &borders.stitched(&world, a)[3];
        let west = &borders.stitched(&world, b)[2];
        assert_eq!(east, west);
        assert!((east[3] - (255.0 + 300.0) / 2.0).abs() < 0.01);
    }

    #[test]
    fn a_finer_tile_follows_its_coarser_neighbours_edge() {
        let world = world();
        let mut images = Assets::<Image>::default();
        let mut borders = TileBorders::default();
        let coarse = key(10, 1, 0);
        let fine = key(11, 1, 1);
        borders.insert(&mut images, &world, coarse, &ramp(0.0, 1000.0));
        borders.insert(&mut images, &world, fine, &ramp(2.0, 0.0));
        let coarse_west = &borders.stitched(&world, coarse)[2];
        let fine_east = &borders.stitched(&world, fine)[3];
        assert!(coarse_west.iter().all(|&h| (h - 1000.0).abs() < 0.01));
        assert!(fine_east.iter().all(|&h| (h - 1000.0).abs() < 0.01));
    }

    #[test]
    fn a_covering_parent_is_no_neighbour() {
        let world = world();
        let mut images = Assets::<Image>::default();
        let mut borders = TileBorders::default();
        let parent = key(10, 0, 0);
        let child = key(11, 0, 0);
        borders.insert(&mut images, &world, parent, &ramp(0.0, 1000.0));
        borders.insert(&mut images, &world, child, &ramp(0.0, 5.0));
        assert_eq!(borders.coarser(&world, child, 3), None);
        assert!(borders.stitched(&world, child)[3]
            .iter()
            .all(|&h| (h - 5.0).abs() < 0.01));
    }

    #[test]
    fn touching_includes_corners_and_excludes_gaps() {
        assert!(touching(key(11, 0, 0), key(11, 1, 1), 12));
        assert!(touching(key(10, 0, 0), key(11, 2, 1), 12));
        assert!(!touching(key(11, 0, 0), key(11, 2, 0), 12));
    }
}
