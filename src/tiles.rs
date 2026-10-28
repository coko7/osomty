//! Background vector tile fetching (OpenFreeMap, OpenMapTiles schema) with memory + disk cache.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::mvt::Tile;

pub const MAX_ZOOM: u8 = 14;
const TILEJSON_URL: &str = "https://tiles.openfreemap.org/planet";
const MEMORY_TILES: usize = 160;
const RETRY_AFTER: Duration = Duration::from_secs(15);
pub const USER_AGENT: &str = concat!("osomty/", env!("CARGO_PKG_VERSION"), " (terminal map viewer)");

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

enum Slot {
    Ready(Arc<Tile>, u64),
    Failed(Instant),
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<TileId>,
    in_flight: HashSet<TileId>,
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

pub struct TileManager {
    shared: Shared,
    results: Receiver<(TileId, Result<Tile, String>)>,
    slots: HashMap<TileId, Slot>,
    tick: u64,
    pub last_error: Option<String>,
}

impl TileManager {
    pub fn new(workers: usize) -> Self {
        let shared: Shared = Arc::default();
        let (tx, rx) = channel();
        let fetcher = Arc::new(Fetcher::new());
        for _ in 0..workers {
            let shared = shared.clone();
            let tx = tx.clone();
            let fetcher = fetcher.clone();
            std::thread::spawn(move || worker(shared, tx, fetcher));
        }
        TileManager {
            shared,
            results: rx,
            slots: HashMap::new(),
            tick: 0,
            last_error: None,
        }
    }

    pub fn get(&mut self, id: TileId) -> Option<Arc<Tile>> {
        self.tick += 1;
        match self.slots.get_mut(&id) {
            Some(Slot::Ready(tile, used)) => {
                *used = self.tick;
                Some(tile.clone())
            }
            _ => None,
        }
    }

    /// Replace the download queue with `wanted` (most important first).
    pub fn request(&mut self, wanted: &[TileId]) {
        let (lock, cvar) = &*self.shared;
        let mut q = lock.lock().unwrap();
        q.pending.clear();
        for id in wanted {
            let skip = match self.slots.get(id) {
                Some(Slot::Ready(..)) => true,
                Some(Slot::Failed(at)) => at.elapsed() < RETRY_AFTER,
                None => false,
            };
            if !skip && !q.in_flight.contains(id) {
                q.pending.push_back(*id);
            }
        }
        cvar.notify_all();
    }

    /// Collect finished downloads. Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((id, res)) = self.results.try_recv() {
            self.shared.0.lock().unwrap().in_flight.remove(&id);
            self.tick += 1;
            match res {
                Ok(tile) => {
                    self.last_error = None;
                    self.slots.insert(id, Slot::Ready(Arc::new(tile), self.tick));
                }
                Err(e) => {
                    self.last_error = Some(e);
                    self.slots.insert(id, Slot::Failed(Instant::now()));
                }
            }
            changed = true;
        }
        if changed {
            self.evict();
        }
        changed
    }

    pub fn loading(&self) -> usize {
        let q = self.shared.0.lock().unwrap();
        q.pending.len() + q.in_flight.len()
    }

    fn evict(&mut self) {
        if self.slots.len() <= MEMORY_TILES {
            return;
        }
        let mut by_age: Vec<(u64, TileId)> = self
            .slots
            .iter()
            .map(|(id, s)| match s {
                Slot::Ready(_, used) => (*used, *id),
                Slot::Failed(_) => (0, *id),
            })
            .collect();
        by_age.sort_unstable_by_key(|(used, _)| *used);
        let excess = self.slots.len() - MEMORY_TILES * 3 / 4;
        for (_, id) in by_age.into_iter().take(excess) {
            self.slots.remove(&id);
        }
    }
}

fn worker(shared: Shared, tx: Sender<(TileId, Result<Tile, String>)>, fetcher: Arc<Fetcher>) {
    loop {
        let id = {
            let (lock, cvar) = &*shared;
            let mut q = lock.lock().unwrap();
            loop {
                if let Some(id) = q.pending.pop_front() {
                    q.in_flight.insert(id);
                    break id;
                }
                q = cvar.wait(q).unwrap();
            }
        };
        let res = fetcher.load(id).map_err(|e| format!("{e:#}"));
        if tx.send((id, res)).is_err() {
            return;
        }
    }
}

struct Fetcher {
    agent: ureq::Agent,
    template: Mutex<Option<String>>,
    cache_dir: Option<PathBuf>,
}

impl Fetcher {
    fn new() -> Self {
        Fetcher {
            agent: http_agent(),
            template: Mutex::new(None),
            cache_dir: dirs::cache_dir().map(|d| d.join("osomty")),
        }
    }

    /// Tile URL template, e.g. `https://tiles.openfreemap.org/planet/<version>/{z}/{x}/{y}.pbf`.
    fn template(&self) -> Result<String> {
        let mut cached = self.template.lock().unwrap();
        if let Some(t) = cached.as_ref() {
            return Ok(t.clone());
        }
        let saved = self.cache_dir.as_ref().map(|d| d.join("template.txt"));
        let fetched = self
            .agent
            .get(TILEJSON_URL)
            .call()
            .map_err(anyhow::Error::from)
            .and_then(|r| Ok(r.into_json::<serde_json::Value>()?))
            .and_then(|json| {
                json["tiles"][0]
                    .as_str()
                    .map(str::to_owned)
                    .context("TileJSON has no tile URL")
            });
        let template = match fetched {
            Ok(t) => {
                if let Some(path) = &saved {
                    let _ = std::fs::create_dir_all(path.parent().unwrap());
                    let _ = std::fs::write(path, &t);
                }
                t
            }
            // Offline: fall back to the last known template so the disk cache still works.
            Err(e) => saved
                .and_then(|p| std::fs::read_to_string(p).ok())
                .ok_or(e)?,
        };
        *cached = Some(template.clone());
        Ok(template)
    }

    fn load(&self, id: TileId) -> Result<Tile> {
        let template = self.template()?;
        let disk = self.cache_dir.as_ref().map(|d| {
            let version = template
                .split("/{z}")
                .next()
                .and_then(|s| s.rsplit('/').next())
                .unwrap_or("default");
            d.join("tiles")
                .join(version)
                .join(format!("{}/{}/{}.pbf", id.z, id.x, id.y))
        });

        // A corrupt cache entry falls through to a fresh download.
        if let Some(bytes) = disk.as_ref().and_then(|p| std::fs::read(p).ok())
            && let Ok(raw) = gunzip(bytes)
            && let Ok(tile) = Tile::decode(&raw)
        {
            return Ok(tile);
        }

        let url = template
            .replace("{z}", &id.z.to_string())
            .replace("{x}", &id.x.to_string())
            .replace("{y}", &id.y.to_string());
        let bytes = match self.agent.get(&url).set("Accept-Encoding", "gzip").call() {
            Ok(resp) if resp.status() == 204 => Vec::new(),
            Ok(resp) => {
                let mut buf = Vec::new();
                resp.into_reader()
                    .take(32 << 20)
                    .read_to_end(&mut buf)
                    .context("reading tile")?;
                buf
            }
            // No data for this tile (e.g. open ocean).
            Err(ureq::Error::Status(404, _)) => Vec::new(),
            Err(e) => bail!("tile {}/{}/{}: {e}", id.z, id.x, id.y),
        };
        if let Some(path) = &disk {
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let _ = std::fs::write(path, &bytes);
        }
        Tile::decode(&gunzip(bytes)?)
    }
}

pub fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .try_proxy_from_env(true)
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
}

fn gunzip(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .context("gunzip tile")?;
        Ok(out)
    } else {
        Ok(bytes)
    }
}
