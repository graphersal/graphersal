//! The Store menu's calls read no history: `Session::store_summary` (the menu's first call),
//! `Session::store_details` (its second) and `Session::store_info` (both in one) read only names,
//! lengths and the small metadata files of a store, never its WAL, its attic's WAL or a model
//! blob, so they cost the same on a store with a long history as on a short one. Measured
//! through a backend that counts every byte read.

use std::io::{Cursor, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use graphersal::persist::{
    DirEntry, DirLockGuard, EntryKind, ImportModelOptions, IntoStoreDir, LockKind, MemDir,
    ModelRole, ModelSource, NewModel, RecoveryTarget, RollbackMode, SpaceUsage, Store, StoreDir,
    StoreFile, StoreOptions,
};
use graphersal_session::Session;
use graphersal_session::store::STORE_DETAIL_KEYS;
use serde_json::Value;

/// The tokenizer of the fake model: a word-level vocabulary of two tokens, so the model loads
/// (with `graphersal/model2vec` on, which Cargo's feature unification does when this crate is
/// tested in one command with `graphersal-cli`, the import checks that it does).
const TOKENIZER: &str = r#"{"version": "1.0", "truncation": null, "padding": null,
    "added_tokens": [], "normalizer": null, "pre_tokenizer": {"type": "Whitespace"},
    "post_processor": null, "decoder": null,
    "model": {"type": "WordLevel", "vocab": {"[UNK]": 0, "graph": 1}, "unk_token": "[UNK]"}}"#;

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

/// Text that compresses badly (the WAL keeps about its size).
fn text(seed: u64, len: usize) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (b'a' + (x % 26) as u8) as char
        })
        .collect()
}

/// A session on a store in memory with a model blob (256 KiB), three checkpoints, two marks,
/// `commits` commits of 4 KiB each before and after two rollbacks of `commits` commits each (two
/// attic entries), and with `orphan` a blob no commit references (removed by the next prune);
/// returns the session and the counter of bytes read.
fn session_with_history(commits: u64, orphan: bool) -> (Session, Arc<AtomicU64>) {
    let bytes = Arc::new(AtomicU64::new(0));
    let dir = Arc::new(Counting {
        inner: MemDir::new().into_store_dir(),
        bytes: bytes.clone(),
    });
    let store = Store::create(dir as Arc<dyn StoreDir>, StoreOptions::new()).unwrap();
    let header =
        r#"{"embeddings": {"dtype": "F32", "shape": [16384, 4], "data_offsets": [0, 262144]}}"#;
    let mut weights = (header.len() as u64).to_le_bytes().to_vec();
    weights.extend_from_slice(header.as_bytes());
    weights.resize(weights.len() + 262_144, 7);
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
                        Cursor::new(TOKENIZER.as_bytes().to_vec()),
                    )
                    .with_file(
                        "model.safetensors",
                        ModelRole::Weights,
                        Cursor::new(weights),
                    )
                    .with_repo("acme/tiny")
                    .with_license("mit"),
            ),
            ImportModelOptions::new(),
        )
        .unwrap();
    let mut seed = 0;
    let mut commit = |store: &Store| {
        seed += 1;
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("doc")
            .property("text", text(seed, 4096))
            .iterate()
            .unwrap();
    };
    store.checkpoint(Some("start")).unwrap();
    for round in 0..2 {
        let mark = format!("before-{round}");
        store.mark(&mark).unwrap();
        for _ in 0..commits {
            commit(&store);
        }
        store
            .rollback_to(RecoveryTarget::Mark(mark), RollbackMode::Move)
            .unwrap();
        for _ in 0..commits {
            commit(&store);
        }
        store.checkpoint(None).unwrap();
    }
    if orphan {
        drop(
            store
                .write_blob(&mut &b"no commit references this"[..])
                .unwrap(),
        );
    }
    let mut session = Session::new();
    session.attach_store(Arc::new(store), "main").unwrap();
    (session, bytes)
}

/// The bytes `f` reads through the counting backend.
fn read_by<T>(bytes: &AtomicU64, f: impl FnOnce() -> T) -> (T, u64) {
    let before = bytes.load(Ordering::Relaxed);
    let out = f();
    (out, bytes.load(Ordering::Relaxed) - before)
}

#[test]
fn the_store_menu_reads_the_same_on_a_short_and_a_long_history() {
    menu_reads_on_short_and_long_histories(false);
}

/// A blob no commit references (an orphan the next prune removes) is looked for in the attic
/// entries' WAL by the compaction advice's prune estimate: the summary never reads it, the
/// details read each entry's WAL once per open store (remembered), never on every call.
#[test]
fn the_store_menu_reads_the_attic_wal_at_most_once_for_an_unreferenced_blob() {
    menu_reads_on_short_and_long_histories(true);
}

fn menu_reads_on_short_and_long_histories(orphan: bool) {
    let (short, short_bytes) = session_with_history(10, orphan);
    let (long, long_bytes) = session_with_history(160, orphan);
    let mut reads = Vec::new();
    for (session, bytes) in [(&short, &short_bytes), (&long, &long_bytes)] {
        // In the menu's order, on a store whose info nobody asked for yet.
        let (_, summary) = read_by(bytes, || session.store_summary().unwrap());
        let (_, first_details) = read_by(bytes, || session.store_details().unwrap());
        let (info, whole) = read_by(bytes, || session.store_info().unwrap());
        let (_, details) = read_by(bytes, || session.store_details().unwrap());
        assert_eq!(info["attic"].as_array().unwrap().len(), 2, "{info}");
        assert_eq!(info["models"].as_array().unwrap().len(), 1, "{info}");
        assert_eq!(
            info["storeBytes"], info["compaction"]["totalBytes"],
            "{info}"
        );
        let wal = info["walBytes"].as_u64().unwrap();
        reads.push((summary, details, whole, wal, first_details));
    }
    let (short, long) = (reads[0], reads[1]);
    // The long history holds megabytes of WAL and attic WAL ...
    assert!(long.3 > 8 * short.3, "{reads:?}");
    assert!(long.3 > 1 << 20, "{reads:?}");
    // ... none of which the menu reads: the small metadata files only, as many on both.
    let mut bounded = vec![
        ("summary", short.0, long.0),
        ("details", short.1, long.1),
        ("info", short.2, long.2),
    ];
    if !orphan {
        bounded.push(("first details", short.4, long.4));
    }
    for (what, s, l) in bounded {
        assert!(
            l <= 128 << 10,
            "{what} reads {l} bytes on the long history: {reads:?}"
        );
        assert!(l <= s + 4096, "{what} reads {s} -> {l} bytes: {reads:?}");
    }
}

#[test]
fn the_summary_and_the_details_together_are_the_store_info() {
    let (session, _) = session_with_history(5, false);
    let info = session.store_info().unwrap();
    let summary = session.store_summary().unwrap();
    let details = session.store_details().unwrap();
    assert_eq!(summary["detailsPending"], true);
    assert_eq!(details["commitSeq"], info["commitSeq"]);
    for key in STORE_DETAIL_KEYS {
        assert_eq!(summary[*key], Value::Null, "{key}: {summary}");
        assert_eq!(details[*key], info[*key], "{key}");
    }
    assert!(info["compaction"].is_object(), "{info}");
    assert!(info["graphBytes"].as_u64().unwrap() > 0, "{info}");
    assert_eq!(
        info["storeBytes"], info["compaction"]["totalBytes"],
        "{info}"
    );
    let Value::Object(fields) = &info else {
        panic!("{info}")
    };
    for (key, value) in fields {
        if !STORE_DETAIL_KEYS.contains(&key.as_str()) {
            assert_eq!(&summary[key], value, "{key}");
        }
    }
    // The attic's restorability is in the summary (cheap: the entries' metadata).
    assert!(summary["attic"][0]["restorable"].is_boolean(), "{summary}");
}
