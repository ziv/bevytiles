//! Browser backend: `fetch` futures on Bevy's [`IoTaskPool`] (single-threaded
//! on wasm — everything below runs on the main thread between frames). See
//! the module docs in [`super`].
//!
//! Semantics preserved from the native pool: dedup by key, every request
//! answered exactly once, cancel flags checked between assets, and at most
//! [`NetworkConfig::threads`] tiles in flight at once (browsers cap
//! connections per host, and an unbounded fan-out would starve the ones the
//! camera actually needs). No disk: fetched bytes are not persisted, but the
//! decoded native-zoom heightmaps that feed synthesis are kept in a small
//! memory cache so the 4–16 descendants of one parent cost one download.
//!
//! [`IoTaskPool`]: bevy::tasks::IoTaskPool

use super::{
    decode_png, expand_url, synthesize_heightmap, Kind, TileDrop, TilePayload, TileRequest,
    CANCELLED,
};
use crate::config::NetworkConfig;
use crate::height::HeightGrid;
use crate::lod::TileKey;
use crate::synth;
use bevy::tasks::IoTaskPool;
use crossbeam_channel::{Receiver, Sender};
use image::RgbImage;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Decoded float heights of one native-zoom heightmap, shared between the
/// descendants synthesized from it.
struct ParentHeights {
    floats: Vec<f32>,
    w: usize,
    h: usize,
}

/// Parent cache cap: when exceeded the cache is flushed wholesale (a moving
/// camera outruns any smarter policy's gain; 64 × 256 KB ≈ 16 MB at most).
const PARENT_CACHE_CAP: usize = 64;

struct Job {
    req: TileRequest,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    in_flight: HashMap<TileKey, Arc<AtomicBool>>,
    queue: VecDeque<Job>,
    active: usize,
    parents: HashMap<(i32, i32), Arc<ParentHeights>>,
}

struct Inner {
    cfg: NetworkConfig,
    max_active: usize,
    state: Mutex<State>,
    payloads: Sender<TilePayload>,
    drops: Sender<TileDrop>,
}

/// The background tile fetcher. Hand out requests and drain results once per
/// frame. Dropping it lets in-flight fetches finish and discard their
/// results (there are no threads to join).
pub struct TileSource {
    inner: Arc<Inner>,
    payload_rx: Receiver<TilePayload>,
    drop_rx: Receiver<TileDrop>,
}

impl TileSource {
    /// Create the fetcher. Must be called after Bevy's task pools exist (i.e.
    /// from a system or after `App` construction), which [`crate::store::setup`]
    /// guarantees.
    ///
    /// # Panics
    /// If `cfg.native_terrain_zoom` lies outside `[MIN_ZOOM, MAX_ZOOM]`.
    pub fn new(cfg: &NetworkConfig) -> Self {
        assert!(
            (crate::config::MIN_ZOOM..=crate::config::MAX_ZOOM).contains(&cfg.native_terrain_zoom),
            "native_terrain_zoom {} outside supported range",
            cfg.native_terrain_zoom
        );
        let (payloads, payload_rx) = crossbeam_channel::unbounded();
        let (drops, drop_rx) = crossbeam_channel::unbounded();
        Self {
            inner: Arc::new(Inner {
                cfg: cfg.clone(),
                max_active: cfg.threads.max(1),
                state: Mutex::new(State::default()),
                payloads,
                drops,
            }),
            payload_rx,
            drop_rx,
        }
    }

    /// Queue one tile fetch. Dedup by key: a no-op while the key is in flight
    /// (a cancelled-but-uncollected job counts). Every request is answered
    /// exactly once — payload or drop.
    pub fn request(&self, req: TileRequest) {
        {
            let mut st = self.inner.state.lock().unwrap();
            if st.in_flight.contains_key(&req.key) {
                return;
            }
            let flag = Arc::new(AtomicBool::new(false));
            st.in_flight.insert(req.key, flag.clone());
            st.queue.push_back(Job {
                req,
                cancelled: flag,
            });
        }
        Inner::pump(&self.inner);
    }

    /// Flag a job for cancellation (checked at pickup and between assets).
    pub fn cancel(&self, key: TileKey) {
        if let Some(flag) = self.inner.state.lock().unwrap().in_flight.get(&key) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Collect everything finished since the last drain (non-blocking).
    pub fn drain(&self, ready: &mut Vec<TilePayload>, dropped: &mut Vec<TileDrop>) {
        ready.extend(self.payload_rx.try_iter());
        dropped.extend(self.drop_rx.try_iter());
    }
}

impl Inner {
    /// Start queued jobs while below the concurrency cap. Cancelled jobs are
    /// answered immediately without occupying a slot.
    fn pump(this: &Arc<Inner>) {
        loop {
            let job = {
                let mut st = this.state.lock().unwrap();
                if st.active >= this.max_active {
                    return;
                }
                let Some(job) = st.queue.pop_front() else {
                    return;
                };
                if job.cancelled.load(Ordering::Relaxed) {
                    st.in_flight.remove(&job.req.key);
                    let _ = this.drops.send(TileDrop::Cancelled(job.req.key));
                    continue;
                }
                st.active += 1;
                job
            };
            let inner = this.clone();
            IoTaskPool::get()
                .spawn(async move {
                    inner.handle(job).await;
                    inner.state.lock().unwrap().active -= 1;
                    Inner::pump(&inner);
                })
                .detach();
        }
    }

    async fn handle(&self, job: Job) {
        let key = job.req.key;
        let cancelled = || job.cancelled.load(Ordering::Relaxed);

        let result: Result<TilePayload, String> = async {
            let albedo_bytes = self
                .fetch_bytes(Kind::Texture, key.zoom, job.req.x, job.req.z)
                .await?;
            let albedo = decode_png(&albedo_bytes)?.into_rgba8();
            if cancelled() {
                return Err(CANCELLED.into());
            }
            let height = self.fetch_heightmap(&job.req).await?;
            if cancelled() {
                return Err(CANCELLED.into());
            }
            let normals = self.fetch_normals(&job.req).await;
            let grid = HeightGrid::from_terrarium(&height);
            Ok(TilePayload {
                key,
                albedo,
                height,
                normals,
                grid,
            })
        }
        .await;

        self.state.lock().unwrap().in_flight.remove(&key);
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

    async fn fetch_bytes(&self, kind: Kind, zoom: u8, x: i32, z: i32) -> Result<Vec<u8>, String> {
        let url = expand_url(kind.url_template(&self.cfg), zoom, x, z);
        let resp = ehttp::fetch_async(ehttp::Request::get(&url))
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        if !resp.ok {
            return Err(format!(
                "GET {url}: HTTP {} {}",
                resp.status, resp.status_text
            ));
        }
        Ok(resp.bytes)
    }

    /// Heightmaps above the native zoom are synthesized from the native
    /// ancestor (memory-cached); no HTTP above the native zoom.
    async fn fetch_heightmap(&self, req: &TileRequest) -> Result<RgbImage, String> {
        let native = self.cfg.native_terrain_zoom;
        if req.key.zoom <= native {
            let bytes = self
                .fetch_bytes(Kind::Heightmap, req.key.zoom, req.x, req.z)
                .await?;
            return Ok(decode_png(&bytes)?.into_rgb8());
        }
        let dz = req.key.zoom - native;
        let parent = self.parent_heights(req.x >> dz, req.z >> dz).await?;
        Ok(synthesize_heightmap(
            &parent.floats,
            parent.w,
            parent.h,
            native,
            req.key.zoom,
            req.x,
            req.z,
        ))
    }

    async fn parent_heights(&self, ax: i32, az: i32) -> Result<Arc<ParentHeights>, String> {
        if let Some(p) = self.state.lock().unwrap().parents.get(&(ax, az)) {
            return Ok(p.clone());
        }
        let native = self.cfg.native_terrain_zoom;
        let bytes = self.fetch_bytes(Kind::Heightmap, native, ax, az).await?;
        let img = decode_png(&bytes)?.into_rgb8();
        let parent = Arc::new(ParentHeights {
            floats: synth::decode_terrarium_floats(&img),
            w: img.width() as usize,
            h: img.height() as usize,
        });
        let mut st = self.state.lock().unwrap();
        if st.parents.len() >= PARENT_CACHE_CAP {
            st.parents.clear();
        }
        st.parents.insert((ax, az), parent.clone());
        Ok(parent)
    }

    /// Normals never fail a tile: above the native zoom no HTTP is attempted;
    /// below it, any failure falls back to the flat default.
    async fn fetch_normals(&self, req: &TileRequest) -> RgbImage {
        if req.key.zoom > self.cfg.native_terrain_zoom {
            return synth::default_normals(256);
        }
        let attempt = async {
            let bytes = self
                .fetch_bytes(Kind::Normals, req.key.zoom, req.x, req.z)
                .await?;
            decode_png(&bytes).map(|i| i.into_rgb8())
        };
        attempt
            .await
            .unwrap_or_else(|_: String| synth::default_normals(256))
    }
}
