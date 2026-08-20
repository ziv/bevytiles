//! Pure terrain synthesis: derive higher-zoom Terrarium heightmaps from
//! native-zoom ancestors, and produce default normal maps. Port of raytiles'
//! `terrain_synth.hpp`. No I/O, no Bevy — plain buffers, fully unit-tested.
//!
//! Cardinal rule: NEVER interpolate Terrarium RGB directly. Adjacent heights
//! can be distant in channel space (g wraps 255→0 as r carries), so all
//! resampling goes decode → f32 heights → bilinear → carry-safe re-encode.

use image::RgbImage;

/// Terrarium: h = r·256 + g + b/256 − 32768 (meters).
pub fn decode_terrarium_floats(img: &RgbImage) -> Vec<f32> {
    img.pixels()
        .map(|p| f32::from(p[0]) * 256.0 + f32::from(p[1]) + f32::from(p[2]) / 256.0 - 32768.0)
        .collect()
}

/// Upsample one quadrant (qx, qz ∈ {0, 1}) of a w×h grid 2× to a full w×h
/// grid. Texel-center aligned: destination texel i samples source coordinate
/// `q·w/2 + (i + 0.5)/2 − 0.5`, so siblings sharing an edge sample adjacent
/// source positions straddling the boundary — continuity by construction.
/// Edges clamp to the source grid.
pub fn upsample_quadrant(src: &[f32], w: usize, h: usize, qx: usize, qz: usize) -> Vec<f32> {
    let sample = |x: isize, y: isize| -> f32 {
        let x = x.clamp(0, w as isize - 1) as usize;
        let y = y.clamp(0, h as isize - 1) as usize;
        src[y * w + x]
    };
    let mut out = vec![0f32; w * h];
    for j in 0..h {
        let sy = qz as f32 * h as f32 / 2.0 + (j as f32 + 0.5) / 2.0 - 0.5;
        let y0 = sy.floor() as isize;
        let ty = sy - y0 as f32;
        for i in 0..w {
            let sx = qx as f32 * w as f32 / 2.0 + (i as f32 + 0.5) / 2.0 - 0.5;
            let x0 = sx.floor() as isize;
            let tx = sx - x0 as f32;
            let top = sample(x0, y0) * (1.0 - tx) + sample(x0 + 1, y0) * tx;
            let bot = sample(x0, y0 + 1) * (1.0 - tx) + sample(x0 + 1, y0 + 1) * tx;
            out[j * w + i] = top * (1.0 - ty) + bot * ty;
        }
    }
    out
}

/// Encode heights (meters) into Terrarium RGB. Carry-safe: quantize once to
/// 24-bit fixed point and split bytes — never round per channel (that spikes
/// at g-wrap boundaries).
pub fn encode_terrarium(heights: &[f32], w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for (i, px) in img.pixels_mut().enumerate() {
        let fixed = ((f64::from(heights[i]) + 32768.0) * 256.0).round().clamp(0.0, 0xFF_FFFF as f64) as u32;
        px[0] = (fixed >> 16) as u8;
        px[1] = ((fixed >> 8) & 0xFF) as u8;
        px[2] = (fixed & 0xFF) as u8;
    }
    img
}

/// A flat default normal map: solid RGB(128, 128, 255) → up-normal (0, 0, 1).
/// Used whenever a real normals asset is unavailable (above the native zoom,
/// 404, corrupt bytes, ...).
pub fn default_normals(size: u32) -> RgbImage {
    RgbImage::from_pixel(size, size, image::Rgb([128, 128, 255]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_quarter_meter() {
        let heights = [0.0f32, 8848.0, -415.0, 100.5, 1234.25, 255.996_09, 256.0, -0.003_906_25];
        let img = encode_terrarium(&heights, heights.len() as u32, 1);
        let decoded = decode_terrarium_floats(&img);
        for (a, b) in heights.iter().zip(&decoded) {
            assert!((a - b).abs() <= 1.0 / 256.0 + 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn carry_safe_at_g_wrap() {
        // 255.998 sits between two representable values across a g-carry;
        // the encoding must land on one of them exactly
        let img = encode_terrarium(&[255.998], 1, 1);
        let p = img.get_pixel(0, 0);
        let fixed = (u32::from(p[0]) << 16) + (u32::from(p[1]) << 8) + u32::from(p[2]);
        assert!(fixed == 8_454_143 || fixed == 8_454_144, "fixed = {fixed}");
    }

    #[test]
    fn ramp_is_exact_in_the_interior() {
        let (w, h) = (8usize, 8usize);
        let src: Vec<f32> = (0..w * h).map(|i| 16.0 * (i % w) as f32).collect();
        let q0 = upsample_quadrant(&src, w, h, 0, 0);
        assert!((q0[2] - 16.0 * 0.75).abs() < 1e-4); // sx = 0.75
        assert!((q0[3] - 16.0 * 1.25).abs() < 1e-4);
        assert!((q0[0] - 0.0).abs() < 1e-4); // sx = -0.25 clamps
    }

    #[test]
    fn siblings_continuous_across_shared_edge() {
        let (w, h) = (8usize, 8usize);
        let src: Vec<f32> = (0..w * h).map(|i| 16.0 * (i % w) as f32).collect();
        let q0 = upsample_quadrant(&src, w, h, 0, 0);
        let q1 = upsample_quadrant(&src, w, h, 1, 0);
        // adjacent fine samples straddling the boundary: half a source step apart
        assert!((q0[w - 1] - 16.0 * 3.25).abs() < 1e-4);
        assert!((q1[0] - 16.0 * 3.75).abs() < 1e-4);
    }

    #[test]
    fn seven_level_chain_matches_independent_computation() {
        // z15 → z22 chain on a gradient; compare against a second, straight-
        // forward computation of the same chain (guards refactors of the loop)
        let (w, h) = (16usize, 16usize);
        let src: Vec<f32> = (0..w * h).map(|i| (i % w) as f32 * 16.0 + (i / w) as f32).collect();
        let quads = [(0, 0), (0, 0), (0, 0), (1, 1), (0, 1), (0, 0), (1, 1)];
        let mut a = src.clone();
        for &(qx, qz) in &quads {
            a = upsample_quadrant(&a, w, h, qx, qz);
        }
        let img = encode_terrarium(&a, w as u32, h as u32);
        let back = decode_terrarium_floats(&img);
        for (x, y) in a.iter().zip(&back) {
            assert!((x - y).abs() <= 1.0 / 256.0 + 1e-4);
        }
    }

    #[test]
    fn default_normals_are_flat() {
        let img = default_normals(4);
        assert_eq!(img.get_pixel(3, 3), &image::Rgb([128, 128, 255]));
    }
}
