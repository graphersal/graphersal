//! Timing harness for the Store menu's info call (`Session::store_info`) on a large store with a
//! long history: ignored, run by hand (debug or `--release`):
//!
//! ```text
//! STORE_INFO_DIR=/path/new-dir cargo test -p graphersal-session --test store_info_timing \
//!   -- --ignored --nocapture build
//! STORE_INFO_DIR=/path/new-dir cargo test -p graphersal-session --test store_info_timing \
//!   -- --ignored --nocapture measure
//! ```
//!
//! `build` writes a store (scale by `STORE_INFO_COMMITS`, `STORE_INFO_VERTICES`,
//! `STORE_INFO_TEXT`, `STORE_INFO_MODEL_MB`, `STORE_INFO_ROLLBACKS`, `STORE_INFO_ROLLBACK_COMMITS`;
//! a path ending in `.gstore` is a single-file store); `measure` opens it through a counting
//! backend and times each part of the info call, with the bytes each part reads.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{Cursor, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use graphersal::persist::{
    DirEntry, DirLockGuard, Durability, EntryKind, ImportModelOptions, IntoStoreDir, LockKind,
    MemDir, ModelRole, ModelSource, NewModel, RecoveryTarget, RollbackMode, SingleFileDir,
    SpaceUsage, Store, StoreDir, StoreFile, StoreOptions,
};
use graphersal::prelude::*;
use graphersal_session::Session;

/// A backend that counts the bytes read through it.
#[derive(Debug)]
struct Counting {
    inner: Arc<dyn StoreDir>,
    bytes: Arc<AtomicU64>,
}

struct CountingReader {
    inner: Box<dyn Read + Send>,
    bytes: Arc<AtomicU64>,
}

impl Read for CountingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl StoreDir for Counting {
    fn location(&self) -> String {
        self.inner.location()
    }
    fn is_volatile(&self) -> bool {
        self.inner.is_volatile()
    }
    fn local_path(&self) -> Option<&std::path::Path> {
        self.inner.local_path()
    }
    fn entry_kind(&self, name: &str) -> std::io::Result<Option<EntryKind>> {
        self.inner.entry_kind(name)
    }
    fn list(&self, dir: &str) -> std::io::Result<Vec<DirEntry>> {
        self.inner.list(dir)
    }
    fn file_len(&self, name: &str) -> std::io::Result<u64> {
        self.inner.file_len(name)
    }
    fn read(&self, name: &str, max: u64) -> std::io::Result<Vec<u8>> {
        let out = self.inner.read(name, max)?;
        self.bytes.fetch_add(out.len() as u64, Ordering::Relaxed);
        Ok(out)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        let out = self.inner.read_range(name, offset, len)?;
        self.bytes.fetch_add(out.len() as u64, Ordering::Relaxed);
        Ok(out)
    }
    fn open_read(&self, name: &str) -> std::io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(CountingReader {
            inner: self.inner.open_read(name)?,
            bytes: self.bytes.clone(),
        }))
    }
    fn create_dir_all(&self, name: &str) -> std::io::Result<()> {
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> std::io::Result<Box<dyn StoreFile>> {
        self.inner.create(name, exclusive)
    }
    fn open_write(&self, name: &str, offset: u64) -> std::io::Result<Box<dyn StoreFile>> {
        self.inner.open_write(name, offset)
    }
    fn append(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.inner.append(name, bytes)
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.inner.write_atomic(name, bytes)
    }
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        self.inner.rename(from, to)
    }
    fn truncate(&self, name: &str, len: u64) -> std::io::Result<()> {
        self.inner.truncate(name, len)
    }
    fn remove_file(&self, name: &str) -> std::io::Result<()> {
        self.inner.remove_file(name)
    }
    fn remove_tree(&self, name: &str) -> std::io::Result<()> {
        self.inner.remove_tree(name)
    }
    fn sync_dir(&self, name: &str) -> std::io::Result<()> {
        self.inner.sync_dir(name)
    }
    fn lock(&self, kind: LockKind) -> std::io::Result<Option<DirLockGuard>> {
        self.inner.lock(kind)
    }
    fn space_usage(&self) -> std::io::Result<Option<SpaceUsage>> {
        self.inner.space_usage()
    }
    fn free_space(&self) -> std::io::Result<Option<u64>> {
        self.inner.free_space()
    }
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn backend(path: &str) -> Arc<dyn StoreDir> {
    if path.ends_with(".gstore") {
        SingleFileDir::new(path).into_store_dir()
    } else {
        std::path::Path::new(path).into_store_dir()
    }
}

/// Pseudo-random letters (lz4 keeps them near their size).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn text(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| (b'a' + (self.next() % 26) as u8) as char)
            .collect()
    }
}

fn options() -> StoreOptions {
    StoreOptions::new().with_durability(Durability::Os)
}

/// Writes the history into `store`.
fn fill(store: &Store) {
    let vertices = env("STORE_INFO_VERTICES", 20_000);
    let commits = env("STORE_INFO_COMMITS", 20_000);
    let text = env("STORE_INFO_TEXT", 10_000) as usize;
    let per_commit = env("STORE_INFO_PER_COMMIT", 8);
    let model_mb = env("STORE_INFO_MODEL_MB", 64);
    let rollbacks = env("STORE_INFO_ROLLBACKS", 4);
    let rollback_commits = env("STORE_INFO_ROLLBACK_COMMITS", 1_000);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let started = Instant::now();
    for chunk in 0..vertices.div_ceil(1_000) {
        let mut graph = store.graph().write();
        let mut g = graph.traversal_mut();
        for i in 0..1_000.min(vertices - chunk * 1_000) {
            g = g.add_v("doc").property("k", (chunk * 1_000 + i) as i64);
        }
        g.iterate().unwrap();
    }
    let ids: Vec<ElementProperty> = store
        .graph()
        .read()
        .traversal()
        .v(None)
        .id()
        .to_list()
        .unwrap();
    eprintln!("{} vertices in {:?}", ids.len(), started.elapsed());

    // One model with a large weights file.
    let header = format!(
        r#"{{"embeddings": {{"dtype": "F32", "shape": [{}, 4], "data_offsets": [0, {}]}}}}"#,
        model_mb * (1 << 20) / 16,
        model_mb * (1 << 20)
    );
    let mut weights = (header.len() as u64).to_le_bytes().to_vec();
    weights.extend_from_slice(header.as_bytes());
    weights.resize(weights.len() + (model_mb as usize) * (1 << 20), 7);
    store
        .import_model(
            ModelSource::Files(
                NewModel::new()
                    .with_file(
                        "config.json",
                        ModelRole::Config,
                        Cursor::new(b"{}".to_vec()),
                    )
                    .with_file(
                        "tokenizer.json",
                        ModelRole::Tokenizer,
                        Cursor::new(vec![1; 50]),
                    )
                    .with_file(
                        "model.safetensors",
                        ModelRole::Weights,
                        Cursor::new(weights),
                    )
                    .with_repo("acme/big")
                    .with_license("mit"),
            ),
            ImportModelOptions::new(),
        )
        .unwrap();
    eprintln!("model imported at {:?}", started.elapsed());

    let commit = |rng: &mut Rng| {
        let mut chosen = Vec::new();
        for _ in 0..per_commit {
            chosen.push(ids[(rng.next() % ids.len() as u64) as usize].clone());
        }
        let value = rng.text(text);
        let mut graph = store.graph().write();
        let mut g = graph.traversal_mut();
        for id in chosen {
            g = g.v(id).property("text", value.clone());
        }
        g.iterate().unwrap();
    };
    let rollback_every = if rollbacks == 0 {
        u64::MAX
    } else {
        commits / (rollbacks + 1)
    };
    let mut done_rollbacks = 0;
    for c in 0..commits {
        commit(&mut rng);
        if c % 500 == 499 {
            store.mark(&format!("m{c}")).unwrap();
        }
        if c % rollback_every == rollback_every - 1 && done_rollbacks < rollbacks {
            let name = format!("before-rollback-{done_rollbacks}");
            store.mark(&name).unwrap();
            for _ in 0..rollback_commits {
                commit(&mut rng);
            }
            store
                .rollback_to(RecoveryTarget::Mark(name), RollbackMode::Move)
                .unwrap();
            done_rollbacks += 1;
        }
        if c % 2_000 == 1_999 {
            eprintln!("{} commits at {:?}", c + 1, started.elapsed());
        }
    }
    store.checkpoint(Some("final")).unwrap();
    eprintln!("built in {:?}", started.elapsed());
}

#[test]
#[ignore = "builds a large store; run by hand"]
fn build() {
    let path = std::env::var("STORE_INFO_DIR").expect("STORE_INFO_DIR");
    let store = Store::create(backend(&path), options()).unwrap();
    fill(&store);
    store.close().unwrap();
}

fn time<T>(what: &str, bytes: &AtomicU64, f: impl FnOnce() -> T) -> T {
    let before = bytes.load(Ordering::Relaxed);
    let started = Instant::now();
    let out = f();
    let elapsed = started.elapsed();
    let read = bytes.load(Ordering::Relaxed) - before;
    println!(
        "| {what:<44} | {:>10.1} ms | {:>14} B |",
        elapsed.as_secs_f64() * 1e3,
        read
    );
    out
}

fn measure_store(store: Store, bytes: Arc<AtomicU64>) {
    let store = Arc::new(store);
    time("Store::quick_info", &bytes, || store.quick_info().unwrap());
    let info = time("Store::info", &bytes, || store.info().unwrap());
    println!(
        "  (snapshots {}, marks {}, wal segments {}, wal bytes {}, attic {}, models {}, blobs {} / {} B)",
        info.snapshots.len(),
        info.marks.len(),
        info.wal_segments,
        info.wal_bytes,
        info.attic.len(),
        info.models.len(),
        info.blobs.count,
        info.blobs.bytes
    );
    time("  Store::snapshots", &bytes, || store.snapshots().unwrap());
    time("  Store::marks", &bytes, || store.marks().unwrap());
    time("  Store::attic_list", &bytes, || {
        store.attic_list().unwrap()
    });
    time("  Store::models", &bytes, || store.models().unwrap());
    time("attic_can_restore (all entries)", &bytes, || {
        for entry in &info.attic {
            let _ = store.attic_can_restore(&entry.id);
        }
    });
    time("compaction_advice", &bytes, || {
        store.compaction_advice().unwrap()
    });
    time("graph memory_usage", &bytes, || {
        store.graph().read().memory_usage().unwrap()
    });
    let mut session = Session::new();
    session.attach_store(store.clone(), "main").unwrap();
    for i in 0..2 {
        time(&format!("Session::store_summary (run {i})"), &bytes, || {
            session.store_summary().unwrap()
        });
        time(&format!("Session::store_details (run {i})"), &bytes, || {
            session.store_details().unwrap()
        });
        time(
            &format!("Session::store_info (whole, run {i})"),
            &bytes,
            || session.store_info().unwrap(),
        );
    }
}

#[test]
#[ignore = "measures a store `build` wrote; run by hand"]
fn measure() {
    let path = std::env::var("STORE_INFO_DIR").expect("STORE_INFO_DIR");
    let bytes = Arc::new(AtomicU64::new(0));
    let dir = Arc::new(Counting {
        inner: backend(&path),
        bytes: bytes.clone(),
    });
    let started = Instant::now();
    let store = Store::open(dir as Arc<dyn StoreDir>, options()).unwrap();
    println!(
        "open: {:?}, {} B read",
        started.elapsed(),
        bytes.load(Ordering::Relaxed)
    );
    measure_store(store, bytes);
}

#[test]
#[ignore = "a store in memory (the wasm path); run by hand"]
fn measure_memory() {
    let bytes = Arc::new(AtomicU64::new(0));
    let dir = Arc::new(Counting {
        inner: MemDir::new().into_store_dir(),
        bytes: bytes.clone(),
    });
    let store = Store::create(dir as Arc<dyn StoreDir>, options()).unwrap();
    fill(&store);
    measure_store(store, bytes);
}
