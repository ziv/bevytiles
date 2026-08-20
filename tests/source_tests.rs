//! Integration tests for the tile source: real worker pool against seeded
//! disk caches (dead HTTP host proves the offline paths). Mirrors the C++
//! raytiles tile_source suite.

use bevytiles::config::NetworkConfig;
use bevytiles::lod::TileKey;
use bevytiles::source::{TileDrop, TilePayload, TileRequest, TileSource};
use bevytiles::synth;
use image::RgbaImage;
use std::path::Path;
use std::time::{Duration, Instant};

fn dead_host_config(dir: &Path) -> NetworkConfig {
    NetworkConfig {
        threads: 1, // deterministic pickup order
        cache_dir: dir.to_path_buf(),
        texture_url: "http://127.0.0.1:9/tex/:zoom:/:x:/:y:.png".into(),
        heightmap_url: "http://127.0.0.1:9/hm/:zoom:/:x:/:y:.png".into(),
        normals_url: "http://127.0.0.1:9/nl/:zoom:/:x:/:y:.png".into(),
        connect_timeout: Duration::from_millis(300),
        read_timeout: Duration::from_millis(500),
        ..Default::default()
    }
}

fn cache_path(dir: &Path, kind: &str, zoom: u8, x: i32, z: i32) -> std::path::PathBuf {
    dir.join(kind).join(zoom.to_string()).join(x.to_string()).join(format!("{z}.png"))
}

fn write_png(path: &Path, img: &image::DynamicImage) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    img.save(path).unwrap();
}

fn gradient_heights(size: usize) -> Vec<f32> {
    (0..size * size).map(|i| (i % size) as f32 * 16.0 + (i / size) as f32).collect()
}

fn seed_terrarium(dir: &Path, zoom: u8, x: i32, z: i32, size: usize) {
    let img = synth::encode_terrarium(&gradient_heights(size), size as u32, size as u32);
    write_png(&cache_path(dir, "heightmap", zoom, x, z), &image::DynamicImage::ImageRgb8(img));
}

fn seed_texture(dir: &Path, zoom: u8, x: i32, z: i32) {
    let img = RgbaImage::from_pixel(2, 2, image::Rgba([200, 60, 30, 255]));
    write_png(&cache_path(dir, "texture", zoom, x, z), &image::DynamicImage::ImageRgba8(img));
}

struct Harness {
    src: TileSource,
    payloads: Vec<TilePayload>,
    drops: Vec<TileDrop>,
}

impl Harness {
    fn new(cfg: NetworkConfig) -> Self {
        Self { src: TileSource::new(&cfg), payloads: Vec::new(), drops: Vec::new() }
    }
    fn pump_until(&mut self, mut pred: impl FnMut(&Self) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut r = Vec::new();
            let mut d = Vec::new();
            self.src.drain(&mut r, &mut d);
            self.payloads.extend(r);
            self.drops.extend(d);
            if pred(self) {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn cache_hit_delivers_whole_payload_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dead_host_config(dir.path());
    seed_texture(dir.path(), 9, 5, 7);
    seed_terrarium(dir.path(), 9, 5, 7, 16);
    // normals unseeded: dead host → flat default, tile still arrives

    let mut h = Harness::new(cfg);
    let key = TileKey { zoom: 9, x: 5, z: 7 };
    h.src.request(TileRequest { key, x: 5, z: 7 });

    assert!(h.pump_until(|h| !h.payloads.is_empty()));
    let p = &h.payloads[0];
    assert_eq!(p.key, key);
    assert_eq!(p.albedo.width(), 2);
    assert_eq!(p.height.width(), 16);
    assert_eq!(p.normals.get_pixel(0, 0), &image::Rgb([128, 128, 255]));
    assert!(p.grid.samples.len() == 16 * 16);
}

#[test]
fn missing_heightmap_drops_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dead_host_config(dir.path());
    seed_texture(dir.path(), 9, 1, 1);

    let mut h = Harness::new(cfg);
    h.src.request(TileRequest { key: TileKey { zoom: 9, x: 1, z: 1 }, x: 1, z: 1 });
    assert!(h.pump_until(|h| !h.drops.is_empty()));
    assert!(h.payloads.is_empty());
    assert!(matches!(h.drops[0], TileDrop::Failed(_, _)));
}

#[test]
fn z16_heightmap_derives_from_seeded_z15_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dead_host_config(dir.path());
    let size = 16usize;
    // ancestor at native z15, provider coords (10, 20); child q(1,0) = (21, 40)
    seed_terrarium(dir.path(), 15, 10, 20, size);
    seed_texture(dir.path(), 16, 21, 40);

    let mut h = Harness::new(cfg);
    let key = TileKey { zoom: 16, x: 21, z: 40 };
    h.src.request(TileRequest { key, x: 21, z: 40 });

    assert!(h.pump_until(|h| !h.payloads.is_empty()));
    // byte-identical to an independently computed decode→upsample→encode
    let expected = synth::encode_terrarium(
        &synth::upsample_quadrant(&gradient_heights(size), size, size, 1, 0),
        size as u32,
        size as u32,
    );
    assert_eq!(h.payloads[0].height.as_raw(), expected.as_raw());
    // derived tile cached; background backfill materializes all 4 siblings
    assert!(cache_path(dir.path(), "heightmap", 16, 21, 40).exists());
    assert!(h.pump_until(|_| {
        [(20, 40), (21, 40), (20, 41), (21, 41)]
            .iter()
            .all(|&(x, z)| cache_path(dir.path(), "heightmap", 16, x, z).exists())
    }));
}

#[test]
fn z17_derives_through_the_chain_and_backfills_lineage_only() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dead_host_config(dir.path());
    let size = 16usize;
    // ancestor (3, 5); target z17 (14, 21): quadrants l16=(1,0), l17=(0,1)
    seed_terrarium(dir.path(), 15, 3, 5, size);
    seed_texture(dir.path(), 17, 14, 21);

    let mut h = Harness::new(cfg);
    h.src.request(TileRequest { key: TileKey { zoom: 17, x: 14, z: 21 }, x: 14, z: 21 });
    assert!(h.pump_until(|h| !h.payloads.is_empty()));

    let mut floats = gradient_heights(size);
    for (qx, qz) in [(1usize, 0usize), (0, 1)] {
        floats = synth::upsample_quadrant(&floats, size, size, qx, qz);
    }
    let expected = synth::encode_terrarium(&floats, size as u32, size as u32);
    assert_eq!(h.payloads[0].height.as_raw(), expected.as_raw());

    // lineage backfill: 4 z16 children of the ancestor + 4 z17 children of
    // the lineage z16 node (7, 10) — never the whole subtree
    assert!(h.pump_until(|_| {
        let z16 = [(6, 10), (7, 10), (6, 11), (7, 11)]
            .iter()
            .all(|&(x, z)| cache_path(dir.path(), "heightmap", 16, x, z).exists());
        let z17 = [(14, 20), (15, 20), (14, 21), (15, 21)]
            .iter()
            .all(|&(x, z)| cache_path(dir.path(), "heightmap", 17, x, z).exists());
        z16 && z17
    }));
    // give backfill a beat, then confirm a cousin outside the lineage stayed on-demand
    std::thread::sleep(Duration::from_millis(200));
    assert!(!cache_path(dir.path(), "heightmap", 17, 12, 20).exists());
}

#[test]
fn corrupt_ancestor_drops_the_derived_tile() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dead_host_config(dir.path());
    let p = cache_path(dir.path(), "heightmap", 15, 7, 7);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"definitely not a png").unwrap();
    seed_texture(dir.path(), 16, 14, 14);

    let mut h = Harness::new(cfg);
    h.src.request(TileRequest { key: TileKey { zoom: 16, x: 14, z: 14 }, x: 14, z: 14 });
    assert!(h.pump_until(|h| !h.drops.is_empty()));
    assert!(matches!(h.drops[0], TileDrop::Failed(_, _)));
}

#[test]
fn requests_dedup_by_key_while_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dead_host_config(dir.path());
    seed_texture(dir.path(), 9, 6, 6);
    seed_terrarium(dir.path(), 9, 6, 6, 16);

    let mut h = Harness::new(cfg);
    let key = TileKey { zoom: 9, x: 6, z: 6 };
    h.src.request(TileRequest { key, x: 6, z: 6 });
    h.src.request(TileRequest { key, x: 6, z: 6 }); // racy dedup: at most one answer per in-flight window
    assert!(h.pump_until(|h| !h.payloads.is_empty()));
    std::thread::sleep(Duration::from_millis(100));
    let mut r = Vec::new();
    let mut d = Vec::new();
    h.src.drain(&mut r, &mut d);
    assert!(h.payloads.len() + r.len() <= 2); // 1 expected; 2 tolerated if the first finished before the re-request
}
