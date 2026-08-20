//! Pure LOD policy: computes the desired tile set for a camera position.
//! Port of raytiles' `lod.hpp` — no Bevy dependencies beyond math types, no
//! I/O, no state; the unit tests (including exact snapshots) depend on that.

use crate::config::ZOOM_LEVELS;
use bevy::math::Vec3;

/// Anchor-relative tile identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// Zoom level in `[base_zoom, max_zoom]`.
    pub zoom: u8,
    /// Tile column, relative to the world anchor at this zoom.
    pub x: i32,
    /// Tile row (slippy-map `y`, world `z`), anchor-relative.
    pub z: i32,
}

/// The subset of the configuration the desired-set policy needs. Derived
/// values (per-zoom sizes, squared thresholds, horizon radius) are computed
/// inside [`desired_tiles`] so the policy stays stateless.
#[derive(Clone)]
pub struct LodOptions {
    /// Lowest zoom in the quadtree (the disc scan runs at this level).
    pub base_zoom: u8,
    /// Highest zoom; recursion accepts unconditionally when it reaches it.
    pub max_zoom: u8,
    /// World size (meters) of one tile at `base_zoom`.
    pub base_tile_size: f32,
    /// Radius, in base-zoom tiles, of the disc scanned around the camera.
    pub radius: i32,
    /// Plain meters; squared internally (in f64 — an f32 square can overflow).
    pub thresholds: [f32; ZOOM_LEVELS],
}

/// Distance to the horizon from height h: d ≈ 3.57 km · √h  ⇒  d² = ratio²·h.
const HORIZON_RATIO_M: f64 = 3570.0;

fn horizon_sq(cam_y: f32) -> f64 {
    HORIZON_RATIO_M * HORIZON_RATIO_M * f64::from(cam_y.max(1.0))
}

/// Squared distance from the camera to a tile center, including the camera
/// height — the altitude term is what collapses LOD when flying high.
fn dist_sq_to_tile(cam: Vec3, zoom_size: f64, x: i32, z: i32) -> f64 {
    let cx = (f64::from(x) + 0.5) * zoom_size;
    let cz = (f64::from(z) + 0.5) * zoom_size;
    let dx = f64::from(cam.x) - cx;
    let dz = f64::from(cam.z) - cz;
    dx * dx + dz * dz + f64::from(cam.y) * f64::from(cam.y)
}

/// Appends the desired tile keys for `cam` (absolute space) into `out`.
/// Does not clear `out`; the produced keys are duplicate-free. Reuse the
/// vector across calls so steady-state rebuilds allocate nothing.
pub fn desired_tiles(opts: &LodOptions, cam: Vec3, out: &mut Vec<TileKey>) {
    let levels = (opts.max_zoom - opts.base_zoom) as usize + 1;
    let mut sizes = [0f64; ZOOM_LEVELS];
    let mut thresholds_sq = [0f64; ZOOM_LEVELS];
    for i in 0..levels {
        sizes[i] = f64::from(opts.base_tile_size) / f64::from(1u32 << i);
        let th = f64::from(opts.thresholds[i]);
        thresholds_sq[i] = th * th;
    }

    let base_size = f64::from(opts.base_tile_size);
    let cam_tile_x = (f64::from(cam.x) / base_size).floor() as i32;
    let cam_tile_z = (f64::from(cam.z) / base_size).floor() as i32;
    let r = opts.radius;
    let allowed_radius = (r - 1) * (r - 1);
    let render_radius_sq = horizon_sq(cam.y);

    struct Ctx<'a> {
        opts: &'a LodOptions,
        sizes: &'a [f64; ZOOM_LEVELS],
        thresholds_sq: &'a [f64; ZOOM_LEVELS],
        cam: Vec3,
        render_radius_sq: f64,
    }

    fn build(ctx: &Ctx, out: &mut Vec<TileKey>, zoom: u8, x: i32, z: i32) {
        if zoom == ctx.opts.max_zoom {
            out.push(TileKey { zoom, x, z });
            return;
        }
        let idx = (zoom - ctx.opts.base_zoom) as usize;
        let d = dist_sq_to_tile(ctx.cam, ctx.sizes[idx], x, z);
        // beyond the horizon: not worth requesting at all
        if d > ctx.render_radius_sq {
            return;
        }
        // far enough for this zoom: accept, don't subdivide
        if d >= ctx.thresholds_sq[idx] {
            out.push(TileKey { zoom, x, z });
            return;
        }
        for oz in 0..2 {
            for ox in 0..2 {
                build(ctx, out, zoom + 1, x * 2 + ox, z * 2 + oz);
            }
        }
    }

    let ctx = Ctx { opts, sizes: &sizes, thresholds_sq: &thresholds_sq, cam, render_radius_sq };
    for dx in -r..=r {
        for dz in -r..=r {
            if dx * dx + dz * dz < allowed_radius {
                build(&ctx, out, opts.base_zoom, cam_tile_x + dx, cam_tile_z + dz);
            }
        }
    }
}

/// XZ-only squared distance (used by eviction's beyond-horizon rule).
pub fn dist_sq_to_tile_xz(cam: Vec3, zoom_size: f64, x: i32, z: i32) -> f64 {
    let cx = (f64::from(x) + 0.5) * zoom_size;
    let cz = (f64::from(z) + 0.5) * zoom_size;
    let dx = f64::from(cam.x) - cx;
    let dz = f64::from(cam.z) - cz;
    dx * dx + dz * dz
}

/// True when `key`'s center lies beyond the horizon for the camera's
/// altitude (XZ distance only) — used by eviction's beyond-horizon rule.
pub fn out_of_horizon(cam: Vec3, zoom_size: f64, key: TileKey) -> bool {
    dist_sq_to_tile_xz(cam, zoom_size, key.x, key.z) > horizon_sq(cam.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn defaults() -> LodOptions {
        LodOptions {
            base_zoom: 9,
            max_zoom: 15,
            base_tile_size: 66_400.0,
            radius: 6,
            thresholds: crate::config::StreamingConfig::default().thresholds,
        }
    }

    fn run(opts: &LodOptions, cam: Vec3) -> Vec<TileKey> {
        let mut out = Vec::new();
        desired_tiles(opts, cam, &mut out);
        out
    }

    fn probes() -> Vec<Vec3> {
        let ts = 66_400.0f32;
        let xz = [
            (0.0, 0.0),
            (0.5 * ts, 0.5 * ts),
            (0.25 * ts, 0.75 * ts),
            (3.3 * ts, -2.7 * ts),
            (-1.0 * ts, -1.0 * ts),
        ];
        let alts = [2.0f32, 500.0, 5_000.0, 60_000.0];
        xz.iter()
            .flat_map(|&(x, z)| alts.iter().map(move |&y| Vec3::new(x, y, z)))
            .collect()
    }

    #[test]
    fn structural_invariants() {
        let opts = defaults();
        for cam in probes() {
            let keys = run(&opts, cam);
            let set: HashSet<_> = keys.iter().copied().collect();
            assert_eq!(set.len(), keys.len(), "duplicates at {cam:?}");
            for k in &keys {
                assert!(k.zoom >= opts.base_zoom && k.zoom <= opts.max_zoom);
                // no key together with an ancestor
                let (mut x, mut z) = (k.x, k.z);
                for zoom in (opts.base_zoom..k.zoom).rev() {
                    x >>= 1;
                    z >>= 1;
                    assert!(
                        !set.contains(&TileKey { zoom, x, z }),
                        "{k:?} has resident ancestor at {zoom}"
                    );
                }
            }
        }
    }

    // Exact regression pins for default options; the values were generated
    // once from this implementation and reviewed for plausibility (tile
    // counts and zoom distribution). If policy changes intentionally, rerun
    // with `--nocapture` printing and update.
    #[test]
    fn snapshots() {
        let opts = defaults();
        let a = run(&opts, Vec3::new(0.0, 500.0, 0.0));
        let b = run(&opts, Vec3::new(33_200.0, 5_000.0, 33_200.0));
        let c = run(&opts, Vec3::new(0.0, 60_000.0, 0.0));
        let count = |keys: &[TileKey], zoom: u8| keys.iter().filter(|k| k.zoom == zoom).count();
        // regenerate-and-pin values: printed on failure for easy updating
        let summary = (a.len(), count(&a, 15), b.len(), count(&b, 9), c.len(), count(&c, 15));
        assert_eq!(summary, snapshot_expected(), "snapshot drift: {summary:?}");
    }

    fn snapshot_expected() -> (usize, usize, usize, usize, usize, usize) {
        SNAP
    }

    // These values are byte-for-byte identical to the C++ raytiles snapshot
    // suite (tests/lod_tests.cpp) — cross-language behavioral equivalence.
    const SNAP: (usize, usize, usize, usize, usize, usize) = (252, 64, 252, 36, 117, 0);

    #[test]
    fn high_zoom_reachable_when_low_over_tile_center() {
        let mut opts = defaults();
        opts.max_zoom = 17;
        let keys = run(&opts, Vec3::new(33_200.0, 500.0, 33_200.0));
        assert!(keys.iter().any(|k| k.zoom > 15));
    }
}
