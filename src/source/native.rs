//! Native backend: worker threads, blocking HTTP, on-disk cache with atomic
//! write-through. See the module docs in [`super`].

use super::{
    decode_png, expand_url, synthesize_heightmap, Kind, TileDrop, TilePayload, TileRequest,
    CANCELLED,
};
use crate::config::NetworkConfig;
use crate::height::HeightGrid;
use crate::lod::TileKey;
use crate::synth;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use image::RgbImage;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

struct Job {
    req: TileRequest,
    cancelled: Arc<AtomicBool>,
}

/// Background synthesis: write the missing lineage-sibling heightmaps along
/// one derived tile's ancestry (4 children per level — never the full
/// subtree, which would be ~21k files per parent at z22). Best-effort.
#[derive(Clone, Copy)]
struct DeriveTask {
    zoom: u8,
    x: i32,
    z: i32,
}

#[derive(Clone)]
struct Shared {
    in_flight: Arc<Mutex<HashMap<TileKey, Arc<AtomicBool>>>>,
    derive_done: Arc<Mutex<HashSet<TileKey>>>,
}

/// The background tile fetcher. Owns the worker threads; hand out requests
/// and drain results once per frame. Dropping it disconnects the queues and
/// joins the workers (an in-flight HTTP read may delay that by its timeout).
pub struct TileSource {
    cfg: Arc<NetworkConfig>,
    shared: Shared,
    jobs_tx: Sender<Job>,
    derive_tx: Sender<DeriveTask>,
    payload_rx: Receiver<TilePayload>,
    drop_rx: Receiver<TileDrop>,
    workers: Vec<JoinHandle<()>>,
}

impl TileSource {
    /// Spawn the worker pool.
    ///
    /// # Panics
    /// If `cfg.native_terrain_zoom` lies outside `[MIN_ZOOM, MAX_ZOOM]`.
    pub fn new(cfg: &NetworkConfig) -> Self {
        assert!(
            (crate::config::MIN_ZOOM..=crate::config::MAX_ZOOM).contains(&cfg.native_terrain_zoom),
            "native_terrain_zoom {} outside supported range",
            cfg.native_terrain_zoom
        );
        let cfg = Arc::new(cfg.clone());
        let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<Job>();
        let (derive_tx, derive_rx) = crossbeam_channel::unbounded::<DeriveTask>();
        let (payload_tx, payload_rx) = crossbeam_channel::unbounded::<TilePayload>();
        let (drop_tx, drop_rx) = crossbeam_channel::unbounded::<TileDrop>();
        let shared = Shared {
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            derive_done: Arc::new(Mutex::new(HashSet::new())),
        };
        let workers = (0..cfg.threads.max(1))
            .map(|_| {
                let w = Worker {
                    cfg: cfg.clone(),
                    shared: shared.clone(),
                    jobs: jobs_rx.clone(),
                    derives: derive_rx.clone(),
                    derive_tx: derive_tx.clone(),
                    payloads: payload_tx.clone(),
                    drops: drop_tx.clone(),
                };
                std::thread::spawn(move || w.run())
            })
            .collect();
        Self {
            cfg,
            shared,
            jobs_tx,
            derive_tx,
            payload_rx,
            drop_rx,
            workers,
        }
    }

    /// Queue one tile fetch. Dedup by key: a no-op while the key is in flight
    /// (a cancelled-but-uncollected job counts). Every request is answered
    /// exactly once — payload or drop.
    pub fn request(&self, req: TileRequest) {
        let mut in_flight = self.shared.in_flight.lock().unwrap();
        if in_flight.contains_key(&req.key) {
            return;
        }
        let flag = Arc::new(AtomicBool::new(false));
        in_flight.insert(req.key, flag.clone());
        let _ = self.jobs_tx.send(Job {
            req,
            cancelled: flag,
        });
    }

    /// Flag a job for cancellation (checked at pickup and between assets).
    pub fn cancel(&self, key: TileKey) {
        if let Some(flag) = self.shared.in_flight.lock().unwrap().get(&key) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Collect everything finished since the last drain (non-blocking).
    pub fn drain(&self, ready: &mut Vec<TilePayload>, dropped: &mut Vec<TileDrop>) {
        ready.extend(self.payload_rx.try_iter());
        dropped.extend(self.drop_rx.try_iter());
    }
}

impl Drop for TileSource {
    fn drop(&mut self) {
        // disconnect the queues; workers exit when both are empty+disconnected.
        // an in-flight HTTP read may delay this by its timeout.
        drop(std::mem::replace(
            &mut self.jobs_tx,
            crossbeam_channel::bounded(0).0,
        ));
        drop(std::mem::replace(
            &mut self.derive_tx,
            crossbeam_channel::bounded(0).0,
        ));
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
        let _ = self.cfg; // silence unused in some cfgs
    }
}

// ---------------------------------------------------------------------------

struct Worker {
    cfg: Arc<NetworkConfig>,
    shared: Shared,
    jobs: Receiver<Job>,
    derives: Receiver<DeriveTask>,
    derive_tx: Sender<DeriveTask>,
    payloads: Sender<TilePayload>,
    drops: Sender<TileDrop>,
}

impl Worker {
    fn run(self) {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.cfg.connect_timeout)
            .timeout_read(self.cfg.read_timeout)
            .build();
        loop {
            // real jobs always win over background synthesis
            match self.jobs.try_recv() {
                Ok(job) => {
                    self.handle(&agent, job);
                    continue;
                }
                Err(TryRecvError::Disconnected) if self.jobs.is_empty() => return,
                Err(_) => {}
            }
            if let Ok(task) = self.derives.try_recv() {
                self.run_derive(task);
                continue;
            }
            // nothing ready: block on either queue
            crossbeam_channel::select! {
                recv(self.jobs) -> msg => match msg {
                    Ok(job) => self.handle(&agent, job),
                    Err(_) => return,
                },
                recv(self.derives) -> msg => if let Ok(task) = msg { self.run_derive(task) },
            }
        }
    }

    fn handle(&self, agent: &ureq::Agent, job: Job) {
        let key = job.req.key;
        let cancelled = || job.cancelled.load(Ordering::Relaxed);
        let finish_in_flight = || {
            self.shared.in_flight.lock().unwrap().remove(&key);
        };
        if cancelled() {
            finish_in_flight();
            let _ = self.drops.send(TileDrop::Cancelled(key));
            return;
        }

        let result: Result<TilePayload, String> = (|| {
            let albedo_bytes =
                self.fetch_bytes(agent, Kind::Texture, key.zoom, job.req.x, job.req.z)?;
            let albedo = decode_png(&albedo_bytes)?.into_rgba8();
            if cancelled() {
                return Err(CANCELLED.into());
            }
            let height = self.fetch_heightmap(agent, &job.req)?;
            if cancelled() {
                return Err(CANCELLED.into());
            }
            let normals = self.fetch_normals(agent, &job.req);
            let grid = HeightGrid::from_terrarium(&height);
            Ok(TilePayload {
                key,
                albedo,
                height,
                normals,
                grid,
            })
        })();

        finish_in_flight();
        match result {
            _ if cancelled() => {
                let _ = self.drops.send(TileDrop::Cancelled(key));
            }
            Ok(payload) => {
                let _ = self.payloads.send(payload);
            }
            Err(e) if e == CANCELLED => {
                let _ = self.drops.send(TileDrop::Cancelled(key));
            }
            Err(e) => {
                let _ = self.drops.send(TileDrop::Failed(key, e));
            }
        }
    }

    // -- assets -------------------------------------------------------------

    /// cache-or-HTTP raw bytes (fetched bytes are written through to cache)
    fn fetch_bytes(
        &self,
        agent: &ureq::Agent,
        kind: Kind,
        zoom: u8,
        x: i32,
        z: i32,
    ) -> Result<Vec<u8>, String> {
        let path = self.cache_path(kind, zoom, x, z);
        if path.exists() {
            return std::fs::read(&path).map_err(|e| format!("cache read {path:?}: {e}"));
        }
        let url = expand_url(kind.url_template(&self.cfg), zoom, x, z);
        let resp = agent
            .get(&url)
            .call()
            .map_err(|e| format!("GET {url}: {e}"))?;
        let mut bytes = Vec::new();
        use std::io::Read;
        resp.into_reader()
            .take(32 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read {url}: {e}"))?;
        write_atomic(&path, &bytes)?;
        Ok(bytes)
    }

    /// Heightmaps above the native zoom are synthesized from the native
    /// ancestor: decode → f32 heights → quadrant-chain upsample → carry-safe
    /// re-encode → cached like a fetched tile → lineage backfill queued.
    /// No HTTP above the native zoom.
    fn fetch_heightmap(&self, agent: &ureq::Agent, req: &TileRequest) -> Result<RgbImage, String> {
        let native = self.cfg.native_terrain_zoom;
        if req.key.zoom <= native {
            let bytes = self.fetch_bytes(agent, Kind::Heightmap, req.key.zoom, req.x, req.z)?;
            return Ok(decode_png(&bytes)?.into_rgb8());
        }

        let path = self.cache_path(Kind::Heightmap, req.key.zoom, req.x, req.z);
        if path.exists() {
            let bytes = std::fs::read(&path).map_err(|e| format!("cache read {path:?}: {e}"))?;
            return Ok(decode_png(&bytes)?.into_rgb8());
        }

        let dz = req.key.zoom - native;
        let (ax, az) = (req.x >> dz, req.z >> dz);
        let parent_bytes = self.fetch_bytes(agent, Kind::Heightmap, native, ax, az)?;
        let parent = decode_png(&parent_bytes)?.into_rgb8();
        let (w, h) = (parent.width() as usize, parent.height() as usize);
        let floats = synth::decode_terrarium_floats(&parent);
        let out = synthesize_heightmap(&floats, w, h, native, req.key.zoom, req.x, req.z);
        write_atomic(&path, &encode_png_rgb(&out)?)?;
        self.enqueue_derive(*req);
        Ok(out)
    }

    /// Normals never fail a tile: a pre-baked cache entry is honored at any
    /// zoom; above the native zoom no HTTP is attempted; below it, any
    /// failure to end up with a valid image falls back to the flat default.
    fn fetch_normals(&self, agent: &ureq::Agent, req: &TileRequest) -> RgbImage {
        let attempt = || -> Result<RgbImage, String> {
            let path = self.cache_path(Kind::Normals, req.key.zoom, req.x, req.z);
            if path.exists() {
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                return Ok(decode_png(&bytes)?.into_rgb8());
            }
            if req.key.zoom > self.cfg.native_terrain_zoom {
                return Err("above native".into());
            }
            let bytes = self.fetch_bytes(agent, Kind::Normals, req.key.zoom, req.x, req.z)?;
            Ok(decode_png(&bytes)?.into_rgb8())
        };
        attempt().unwrap_or_else(|_| synth::default_normals(256))
    }

    // -- background lineage backfill ----------------------------------------

    fn enqueue_derive(&self, req: TileRequest) {
        let mut done = self.shared.derive_done.lock().unwrap();
        if !done.insert(req.key) {
            return;
        }
        let _ = self.derive_tx.send(DeriveTask {
            zoom: req.key.zoom,
            x: req.x,
            z: req.z,
        });
    }

    fn run_derive(&self, task: DeriveTask) {
        let native = self.cfg.native_terrain_zoom;
        if task.zoom <= native {
            return;
        }
        let dz = task.zoom - native;
        let parent_path = self.cache_path(Kind::Heightmap, native, task.x >> dz, task.z >> dz);
        let Ok(bytes) = std::fs::read(&parent_path) else {
            return;
        };
        let Ok(parent) = decode_png(&bytes) else {
            return;
        };
        let parent = parent.into_rgb8();
        let (w, h) = (parent.width() as usize, parent.height() as usize);
        let mut floats = synth::decode_terrarium_floats(&parent);

        for level in (native + 1)..=task.zoom {
            let shift = task.zoom - level;
            let (lx, lz) = (task.x >> shift, task.z >> shift);
            let mut lineage_child = None;
            for qz in 0..2usize {
                for qx in 0..2usize {
                    let child = synth::upsample_quadrant(&floats, w, h, qx, qz);
                    let cx = (lx & !1) + qx as i32;
                    let cz = (lz & !1) + qz as i32;
                    let path = self.cache_path(Kind::Heightmap, level, cx, cz);
                    if !path.exists() {
                        let img = synth::encode_terrarium(&child, w as u32, h as u32);
                        if let Ok(png) = encode_png_rgb(&img) {
                            let _ = write_atomic(&path, &png); // best-effort
                        }
                    }
                    if cx == lx && cz == lz {
                        lineage_child = Some(child);
                    }
                }
            }
            match lineage_child {
                Some(c) => floats = c,
                None => return, // unreachable, but never loop on bad math
            }
        }
    }

    // -- paths / urls --------------------------------------------------------

    fn cache_path(&self, kind: Kind, zoom: u8, x: i32, z: i32) -> PathBuf {
        let dir = match kind {
            Kind::Texture => "texture",
            Kind::Heightmap => "heightmap",
            Kind::Normals => "normals",
        };
        self.cfg
            .cache_dir
            .join(dir)
            .join(zoom.to_string())
            .join(x.to_string())
            .join(format!("{z}.png"))
    }
}

fn encode_png_rgb(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| format!("png encode: {e}"))?;
    Ok(out)
}

/// Atomic cache write: unique tmp per writer (background derive tasks can
/// race a direct request on the same path — a shared tmp name would
/// interleave; the rename race itself is benign, identical bytes).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    let tmp = path.with_extension(format!("tmp{}", COUNTER.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {tmp:?}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {path:?}: {e}"))
}
