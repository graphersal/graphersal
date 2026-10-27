//! A panic inside the Store's journal (feature `persist`, book
//! `persistence/journal_and_recovery.md` "Failures while committing"): a WAL write or sync that
//! panics, on every backend. The record is cut off again (or, when the cut fails too, its
//! position recorded in `GRAPH`), the journal is poisoned, the panic continues, the graph rolls
//! the unit back; a reopen gives the state before the unit.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use graphersal::persist::{
    DirCheck, DirCompaction, DirEntry, DirLockGuard, EntryKind, FsDir, LockKind, MemDir,
    PersistError, SingleFileDir, SpaceUsage, Store, StoreDir, StoreFile, StoreOptions,
};

use crate::persist_tests::names;

/// What the wrapped WAL files do wrong.
#[derive(Debug, Default)]
struct Faults {
    /// Bytes a WAL file may still take before a write panics (`usize::MAX`: no limit).
    budget: AtomicUsize,
    /// A WAL file's sync panics.
    panic_sync: AtomicBool,
    /// A WAL file's `set_len` (the cut of a refused record) panics.
    panic_cut: AtomicBool,
}

/// A backend wrapper whose WAL files panic on demand.
#[derive(Debug)]
struct Panicking {
    inner: Arc<dyn StoreDir>,
    faults: Arc<Faults>,
}

struct PanickingFile {
    inner: Box<dyn StoreFile>,
    faults: Arc<Faults>,
}

impl io::Write for PanickingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.faults.budget.load(Ordering::SeqCst);
        if left == usize::MAX {
            return self.inner.write(buf);
        }
        let n = buf.len().min(left);
        self.inner.write_all(&buf[..n])?;
        self.faults.budget.fetch_sub(n, Ordering::SeqCst);
        if n < buf.len() {
            panic!("injected panic while writing a WAL file");
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl StoreFile for PanickingFile {
    fn sync(&mut self) -> io::Result<()> {
        if self.faults.panic_sync.load(Ordering::SeqCst) {
            panic!("injected panic while syncing a WAL file");
        }
        self.inner.sync()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        if self.faults.panic_cut.load(Ordering::SeqCst) {
            panic!("injected panic while cutting a WAL file");
        }
        self.inner.set_len(len)
    }
}

impl Panicking {
    fn wrap(&self, name: &str, file: Box<dyn StoreFile>) -> Box<dyn StoreFile> {
        if name.starts_with("wal/") {
            Box::new(PanickingFile {
                inner: file,
                faults: self.faults.clone(),
            })
        } else {
            file
        }
    }
}

impl StoreDir for Panicking {
    fn location(&self) -> String {
        self.inner.location()
    }
    fn local_path(&self) -> Option<&std::path::Path> {
        self.inner.local_path()
    }
    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
        self.inner.entry_kind(name)
    }
    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
        self.inner.list(dir)
    }
    fn file_len(&self, name: &str) -> io::Result<u64> {
        self.inner.file_len(name)
    }
    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        self.inner.read(name, max)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> io::Result<Box<dyn io::Read + Send>> {
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        Ok(self.wrap(name, self.inner.create(name, exclusive)?))
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        Ok(self.wrap(name, self.inner.open_write(name, offset)?))
    }
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.inner.append(name, bytes)
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_atomic(name, bytes)
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        self.inner.truncate(name, len)
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.inner.remove_file(name)
    }
    fn remove_tree(&self, name: &str) -> io::Result<()> {
        self.inner.remove_tree(name)
    }
    fn sync_dir(&self, name: &str) -> io::Result<()> {
        self.inner.sync_dir(name)
    }
    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        self.inner.lock(kind)
    }
    fn space_usage(&self) -> io::Result<Option<SpaceUsage>> {
        self.inner.space_usage()
    }
    fn compact(&self) -> io::Result<Option<DirCompaction>> {
        self.inner.compact()
    }
    fn free_space(&self) -> io::Result<Option<u64>> {
        self.inner.free_space()
    }
    fn check(&self) -> io::Result<DirCheck> {
        self.inner.check()
    }
}

/// A fresh directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-journal-panic-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The three backends: a directory, memory, one file.
fn backends(temp: &Temp) -> Vec<(&'static str, Arc<dyn StoreDir>)> {
    vec![
        ("dir", Arc::new(FsDir::new(temp.0.join("dir")))),
        ("memory", Arc::new(MemDir::new())),
        (
            "file",
            Arc::new(SingleFileDir::new(temp.0.join("store.gstore"))),
        ),
    ]
}

fn add_person(store: &Store, name: &str) -> Result<(), String> {
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", name)
        .to_list()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// The length of the newest WAL segment.
fn wal_len(dir: &dyn StoreDir) -> u64 {
    let mut names: Vec<String> = dir
        .list("wal")
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    dir.file_len(&format!("wal/{}", names.last().unwrap()))
        .unwrap()
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    /// A write panics after part of the record.
    PartialWrite,
    /// The sync after the whole record panics.
    Sync,
    /// The sync panics, and so does the cut of the record.
    SyncAndCut,
}

#[test]
fn a_panic_in_the_store_journal_leaves_neither_memory_nor_the_wal_with_the_unit() {
    for (round, fault) in [Fault::PartialWrite, Fault::Sync, Fault::SyncAndCut]
        .into_iter()
        .enumerate()
    {
        let round_temp = Temp::new(&format!("round{round}"));
        for (backend, inner) in backends(&round_temp) {
            let context = format!("{backend}, {fault:?}");
            let faults = Arc::new(Faults {
                budget: AtomicUsize::new(usize::MAX),
                ..Faults::default()
            });
            let dir: Arc<dyn StoreDir> = Arc::new(Panicking {
                inner: inner.clone(),
                faults: faults.clone(),
            });
            let store = Store::create(&dir, StoreOptions::new()).unwrap();
            add_person(&store, "ann").unwrap();
            let durable = wal_len(&*inner);
            match fault {
                Fault::PartialWrite => faults.budget.store(10, Ordering::SeqCst),
                Fault::Sync => faults.panic_sync.store(true, Ordering::SeqCst),
                Fault::SyncAndCut => {
                    faults.panic_sync.store(true, Ordering::SeqCst);
                    faults.panic_cut.store(true, Ordering::SeqCst);
                }
            }
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                add_person(&store, "bob")
            }));
            assert!(panicked.is_err(), "{context}: the panic continues");
            faults.budget.store(usize::MAX, Ordering::SeqCst);
            faults.panic_sync.store(false, Ordering::SeqCst);
            faults.panic_cut.store(false, Ordering::SeqCst);
            // Memory: the unit was rolled back.
            assert_eq!(names(&store.graph().read()), ["ann"], "{context}");
            assert_eq!(store.graph().read().last_commit_seq(), 1, "{context}");
            match fault {
                // The record was cut off again.
                Fault::PartialWrite | Fault::Sync => {
                    assert_eq!(wal_len(&*inner), durable, "{context}")
                }
                // The record stays, its position is recorded.
                Fault::SyncAndCut => assert!(wal_len(&*inner) > durable, "{context}"),
            }
            // Poisoned: every later commit and mark is refused, even with the disk working.
            let err = add_person(&store, "cy").unwrap_err();
            assert!(err.contains("poisoned"), "{context}: {err}");
            assert!(
                err.contains("a panic while writing a commit record"),
                "{context}: {err}"
            );
            assert!(store.mark("m").is_err(), "{context}");
            assert_eq!(names(&store.graph().read()), ["ann"], "{context}");
            // Closing reports the poisoned journal (and does not mark the store clean).
            assert!(
                matches!(store.close(), Err(PersistError::Store { .. })),
                "{context}"
            );

            // A reopen gives the state before the unit, and works.
            let store = Store::open(&inner, StoreOptions::new())
                .unwrap_or_else(|err| panic!("{context}: reopen: {err}"));
            assert_eq!(names(&store.graph().read()), ["ann"], "{context}");
            let report = store.open_report();
            assert_eq!(
                report.refused_cut.is_some(),
                matches!(fault, Fault::SyncAndCut),
                "{context}: {report:?}"
            );
            add_person(&store, "dan").unwrap();
            store.close().unwrap();
            let store = Store::open(&inner, StoreOptions::new()).unwrap();
            assert_eq!(names(&store.graph().read()), ["ann", "dan"], "{context}");
            assert!(store.verify().unwrap().is_ok(), "{context}");
            store.close().unwrap();
        }
    }
}
