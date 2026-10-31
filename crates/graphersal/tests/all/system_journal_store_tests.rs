//! The system journal of a Store (book page "System Journal", format spec 4.7): records kept
//! across close and open, the query filters and the cursor, torn tails and damage of the
//! segments, rotation, instances, read-only opens, a failing journal that never fails a commit,
//! sinks and subscriptions, and every backend.
#![cfg(feature = "persist")]

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use graphersal::persist::{
    DirEntry, DirLockGuard, EntryKind, LockKind, MemDir, SingleFileDir, Store, StoreDir, StoreFile,
    StoreOptions,
};
use graphersal::prelude::*;
use graphersal::system_journal::{JournalQuery, JournalRecord, JournalSink, Level, SystemJournal};

pub(crate) fn options() -> StoreOptions {
    StoreOptions::new().with_checkpoint_after_bytes(0)
}

pub(crate) fn add_person(store: &Store, name: &str) {
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", name)
        .to_list()
        .unwrap();
}

pub(crate) fn kinds(records: &[JournalRecord]) -> Vec<String> {
    records.iter().map(|record| record.kind.clone()).collect()
}

pub(crate) fn all(dir: impl graphersal::persist::IntoStoreDir) -> Vec<JournalRecord> {
    Store::journal_dir(dir, &JournalQuery::new().limit(10_000))
        .unwrap()
        .records
}

fn segments(dir: &MemDir) -> Vec<String> {
    dir.file_names()
        .into_iter()
        .filter(|name| name.starts_with("journal/"))
        .collect()
}

#[test]
fn records_survive_close_and_open_and_the_seq_continues() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_person(&store, "a");
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let first = all(&dir);
    assert_eq!(
        kinds(&first),
        [
            "store.created",
            "store.opened",
            "store.checkpoint_done",
            "store.closed"
        ]
    );
    // Strictly increasing seq and non-decreasing time, one origin.
    assert!(
        first
            .windows(2)
            .all(|w| w[0].seq < w[1].seq && w[0].time <= w[1].time)
    );
    assert!(
        first
            .iter()
            .all(|r| r.origin == first[0].origin && r.origin_seq == r.seq)
    );
    assert_eq!(first[2].field("merged"), Some(&ElementProperty::from(true)));
    assert_eq!(first[3].field("clean"), Some(&ElementProperty::from(true)));

    let store = Store::open(&dir, options()).unwrap();
    let journal = store.journal().unwrap();
    // The graph's journal is the store's.
    assert!(Arc::ptr_eq(
        &journal,
        &store.graph().system_journal().unwrap()
    ));
    let reopened = journal.query(&JournalQuery::new()).unwrap().records;
    assert_eq!(&reopened[..4], &first[..]);
    assert_eq!(reopened[4].kind, "store.opened");
    assert_eq!(reopened[4].seq, first[3].seq + 1);
    // The instance is the same machine's: the same origin.
    assert_eq!(reopened[4].origin, first[0].origin);
    assert!(reopened[4].time >= first[3].time);
    store.close().unwrap();
}

#[test]
fn queries_filter_by_level_source_kind_time_origin_and_page_by_cursor() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    store.checkpoint(None).unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let every = all(&dir);
    let query = |q: JournalQuery| kinds(&Store::journal_dir(&dir, &q).unwrap().records);

    // Level: notice and more severe.
    assert_eq!(
        query(JournalQuery::new().min_level(Level::Notice)),
        ["store.created", "store.backup_done"]
    );
    assert!(query(JournalQuery::new().min_level(Level::Error)).is_empty());
    // Source: a prefix of the hierarchy, never a partial name.
    assert_eq!(
        query(JournalQuery::new().source_prefix("store.backup")),
        ["store.backup_done"]
    );
    assert_eq!(
        query(JournalQuery::new().source_prefix("store")).len(),
        every.len()
    );
    assert!(query(JournalQuery::new().source_prefix("stor")).is_empty());
    assert_eq!(
        query(JournalQuery::new().kind("store.checkpoint_done")),
        ["store.checkpoint_done"]
    );
    // Time: since is inclusive, until exclusive.
    let checkpoint = every
        .iter()
        .find(|r| r.kind == "store.checkpoint_done")
        .unwrap();
    assert!(
        query(JournalQuery::new().since(checkpoint.time))
            .contains(&"store.checkpoint_done".to_string())
    );
    assert!(
        !query(JournalQuery::new().until(checkpoint.time))
            .contains(&"store.checkpoint_done".to_string())
    );
    assert_eq!(
        query(JournalQuery::new().origin(every[0].origin.id)).len(),
        every.len()
    );
    assert!(query(JournalQuery::new().origin(7)).is_empty());

    // The cursor: one record per page, every record once, in order.
    let mut after = 0;
    let mut paged = Vec::new();
    loop {
        let page = Store::journal_dir(&dir, &JournalQuery::new().after(after).limit(1)).unwrap();
        paged.extend(page.records.iter().map(|r| r.seq));
        after = page.next_after;
        if !page.more {
            break;
        }
    }
    assert_eq!(paged, every.iter().map(|r| r.seq).collect::<Vec<_>>());
    // A cursor at the end: nothing, and the cursor stays.
    let end = Store::journal_dir(&dir, &JournalQuery::new().after(after)).unwrap();
    assert!(end.records.is_empty() && !end.more);
    assert_eq!(end.next_after, after);
}

#[test]
fn a_torn_tail_is_cut_at_every_offset_of_the_last_record() {
    let dir = MemDir::new();
    Store::create(&dir, options()).unwrap().close().unwrap();
    let names = segments(&dir);
    assert_eq!(names.len(), 1, "{names:?}");
    let bytes = dir.file(&names[0]).unwrap();
    let records = all(&dir);
    // The last frame starts at the last sync marker (format spec 4.7).
    let last_start = bytes
        .windows(4)
        .rposition(|w| w == [0x47, 0x53, 0x52, 0x4A])
        .unwrap();
    let last_len = bytes.len() - last_start;
    assert!(last_len > 26, "{last_len}");
    for cut in bytes.len() - last_len..bytes.len() {
        let image = dir.deep_copy();
        image.put_file(&names[0], bytes[..cut].to_vec());
        let read = Store::journal_dir(&image, &JournalQuery::new()).unwrap();
        assert_eq!(read.records.len() + 1, records.len(), "cut {cut}");
        assert!(read.notes.is_empty(), "cut {cut}: {:?}", read.notes);
        // The next writer cuts the torn tail and continues after the last valid record.
        let store = Store::open(&image, options()).unwrap();
        store.close().unwrap();
        let after = all(&image);
        assert_eq!(after.len(), records.len() + 1, "cut {cut}");
        assert_eq!(after[records.len() - 1].kind, "store.opened", "cut {cut}");
        assert!(
            Store::verify_dir(&image).unwrap().journal_notes.is_empty(),
            "cut {cut}"
        );
    }
}

#[test]
fn a_damaged_sealed_segment_is_skipped_by_resync_noted_and_never_damage() {
    let dir = MemDir::new();
    let options = options().with_journal_segment_bytes(150);
    let store = Store::create(&dir, options.clone()).unwrap();
    for _ in 0..6 {
        store.checkpoint(None).unwrap();
    }
    store.close().unwrap();
    let names = segments(&dir);
    assert!(names.len() >= 3, "rotation at the size: {names:?}");
    let before = all(&dir);
    // A byte of the first record of the first (sealed) segment.
    let mut bytes = dir.file(&names[0]).unwrap();
    bytes[64 + 30] ^= 0x5a;
    dir.put_file(&names[0], bytes);
    let page = Store::journal_dir(&dir, &JournalQuery::new()).unwrap();
    assert!(!page.notes.is_empty());
    assert_eq!(page.records.len() + 1, before.len());
    let report = Store::verify_dir(&dir).unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert!(!report.journal_notes.is_empty());
    assert!(report.notes.iter().any(|n| n.contains("system journal")));
    // The store opens writable and keeps committing.
    let store = Store::open(&dir, options).unwrap();
    assert!(!store.open_report().maintenance);
    add_person(&store, "b");
    store.close().unwrap();
}

#[test]
fn a_new_instance_writes_into_a_new_segment() {
    let dir = MemDir::new();
    Store::create(&dir, options().with_instance_name("alpha"))
        .unwrap()
        .close()
        .unwrap();
    let first = all(&dir);
    assert_eq!(first[0].origin.name, "alpha");
    let before = segments(&dir);
    // Another machine: the instance file does not travel.
    dir.remove_file("INSTANCE").unwrap();
    Store::open(&dir, options().with_instance_name("beta"))
        .unwrap()
        .close()
        .unwrap();
    let after = segments(&dir);
    assert_eq!(after.len(), before.len() + 1, "{after:?}");
    let records = all(&dir);
    let new = &records[first.len()..];
    assert!(new.iter().all(|r| r.origin.name == "beta"));
    assert_ne!(new[0].origin.id, first[0].origin.id);
    // Same instance, renamed: same id, same segment.
    Store::open(&dir, options().with_instance_name("gamma"))
        .unwrap()
        .close()
        .unwrap();
    assert_eq!(segments(&dir).len(), after.len());
    let last = all(&dir).pop().unwrap();
    assert_eq!(
        (last.origin.id, last.origin.name.as_str()),
        (new[0].origin.id, "gamma")
    );
}

#[test]
fn a_read_only_open_reads_the_journal_and_writes_nothing() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let files_before: Vec<(String, Vec<u8>)> = backup
        .file_names()
        .into_iter()
        .map(|name| (name.clone(), backup.file(&name).unwrap()))
        .collect();
    let opened = Store::open_backup(&backup, options()).unwrap();
    let journal = opened.journal().unwrap();
    let records = journal.query(&JournalQuery::new()).unwrap().records;
    // The source's records (copied by the backup), then this open's, in memory.
    assert_eq!(records[0].kind, "store.created");
    let last = records.last().unwrap();
    assert_eq!(last.kind, "store.opened");
    assert_eq!(last.field("read_only"), Some(&ElementProperty::from(true)));
    assert!(records.windows(2).all(|w| w[0].seq < w[1].seq));
    opened.close().unwrap();
    let files_after: Vec<(String, Vec<u8>)> = backup
        .file_names()
        .into_iter()
        .map(|name| (name.clone(), backup.file(&name).unwrap()))
        .collect();
    assert_eq!(files_after, files_before);
}

/// A [`MemDir`] whose `journal/` writes fail while `fail` is set.
#[derive(Debug, Clone)]
struct FailingJournal {
    inner: MemDir,
    fail: Arc<AtomicBool>,
}

impl FailingJournal {
    fn check(&self, name: &str) -> io::Result<()> {
        if self.fail.load(Ordering::SeqCst) && name.starts_with("journal") {
            return Err(io::Error::other("the journal's disk is full"));
        }
        Ok(())
    }
}

impl StoreDir for FailingJournal {
    fn location(&self) -> String {
        "memory:failing-journal".into()
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
        self.check(name)?;
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        self.check(name)?;
        self.inner.create(name, exclusive)
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        self.check(name)?;
        self.inner.open_write(name, offset)
    }
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.check(name)?;
        self.inner.append(name, bytes)
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_atomic(name, bytes)
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        self.check(name)?;
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
}

#[test]
fn a_failing_journal_never_fails_a_commit_and_the_loss_is_reported() {
    let inner = MemDir::new();
    let fail = Arc::new(AtomicBool::new(false));
    let dir: Arc<dyn StoreDir> = Arc::new(FailingJournal {
        inner: inner.clone(),
        fail: fail.clone(),
    });
    let store = Store::create(&dir, options()).unwrap();
    let journal = store.journal().unwrap();
    fail.store(true, Ordering::SeqCst);
    add_person(&store, "a");
    store.checkpoint(None).unwrap();
    add_person(&store, "b");
    // The batch cannot be written: the flush says so, the store does not care.
    assert!(journal.flush().is_err());
    let lost = journal.lost_records();
    assert!(lost >= 2, "{lost}");
    add_person(&store, "c");
    fail.store(false, Ordering::SeqCst);
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    assert_eq!(store_people(&inner), 3);
    let records = all(&inner);
    let lost_record = records
        .iter()
        .find(|r| r.kind == "journal.records_lost")
        .expect("the loss is reported");
    assert_eq!(lost_record.level, Level::Warn);
    assert_eq!(
        lost_record.field("count"),
        Some(&ElementProperty::from(lost as i64))
    );
    // The records written before the failure and after it are there.
    assert_eq!(records[0].kind, "store.created");
    assert_eq!(records.last().unwrap().kind, "store.closed");
}

fn store_people(dir: &MemDir) -> usize {
    let store = Store::open(dir, options()).unwrap();
    let count = store.graph().read().vertex_count();
    store.close().unwrap();
    count
}

#[test]
fn an_error_record_is_written_at_once() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    // A backup into a directory that holds something else fails: an error record.
    let target = MemDir::new();
    target.put_file("unrelated", b"x".to_vec());
    assert!(store.backup(&target).is_err());
    // Read from the files while the store is open: the error record is there at once (with
    // the batched records before it).
    let on_disk = kinds(&all(&dir));
    assert!(
        on_disk.contains(&"store.backup_failed".to_string()),
        "{on_disk:?}"
    );
    let failed = all(&dir)
        .into_iter()
        .find(|r| r.kind == "store.backup_failed")
        .unwrap();
    assert_eq!(failed.level, Level::Error);
    assert_eq!(failed.field("side"), Some(&ElementProperty::from("none")));
    store.close().unwrap();
}

/// A sink recording the kinds it receives (and panicking for `panic_on`).
struct Recorder {
    seen: Arc<Mutex<Vec<String>>>,
    panic_on: Option<&'static str>,
}

impl JournalSink for Recorder {
    fn record(&self, record: &JournalRecord) {
        if self.panic_on == Some(record.kind.as_str()) {
            panic!("a broken sink");
        }
        self.seen.lock().unwrap().push(record.kind.clone());
    }

    fn name(&self) -> &str {
        "recorder"
    }
}

#[test]
fn sinks_get_the_written_records_and_a_panicking_one_is_isolated() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    let journal: Arc<SystemJournal> = store.journal().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let id = journal
        .add_sink(Box::new(Recorder {
            seen: seen.clone(),
            panic_on: None,
        }))
        .unwrap();
    let broken_seen = Arc::new(Mutex::new(Vec::new()));
    journal
        .add_sink(Box::new(Recorder {
            seen: broken_seen.clone(),
            panic_on: Some("store.checkpoint_done"),
        }))
        .unwrap();
    let mut subscription = journal
        .subscribe(journal.query(&JournalQuery::new()).unwrap().next_after)
        .unwrap();
    store.checkpoint(None).unwrap();
    store.verify().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        ["store.checkpoint_done", "store.verified"]
    );
    // The broken sink lost its record, the others and the operation did not.
    assert_eq!(*broken_seen.lock().unwrap(), ["store.verified"]);
    assert_eq!(journal.sink_failures(), 1);
    assert_eq!(
        kinds(
            &subscription
                .wait(std::time::Duration::from_millis(10))
                .unwrap()
        ),
        ["store.checkpoint_done", "store.verified"]
    );
    assert!(subscription.poll().unwrap().is_empty());
    journal.remove_sink(id).unwrap();
    store.checkpoint(None).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
    // A waiting subscriber is woken by a new record.
    let waiter = {
        let journal = Arc::clone(&journal);
        let after = subscription.after();
        std::thread::spawn(move || {
            let mut subscription = journal.subscribe(after).unwrap();
            let mut got = Vec::new();
            for _ in 0..50 {
                got = subscription
                    .wait(std::time::Duration::from_millis(200))
                    .unwrap();
                if !got.is_empty() {
                    break;
                }
            }
            kinds(&got)
        })
    };
    store.verify().unwrap();
    let got = waiter.join().unwrap();
    assert!(
        got == ["store.checkpoint_done", "store.verified"] || got == ["store.checkpoint_done"],
        "{got:?}"
    );
    store.close().unwrap();
}

#[test]
fn every_backend_keeps_the_journal() {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "graphersal-journal-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let backends: Vec<Arc<dyn StoreDir>> = vec![
        Arc::new(MemDir::new()),
        Arc::new(graphersal::persist::FsDir::new(root.join("dir"))),
        Arc::new(SingleFileDir::new(root.join("one.gstore"))),
    ];
    for dir in backends {
        let store = Store::create(&dir, options()).unwrap();
        add_person(&store, "a");
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&dir, options()).unwrap();
        store.close().unwrap();
        assert_eq!(
            kinds(&all(&dir)),
            [
                "store.created",
                "store.opened",
                "store.checkpoint_done",
                "store.closed",
                "store.opened",
                "store.closed"
            ],
            "{}",
            dir.location()
        );
        assert!(Store::verify_dir(&dir).unwrap().journal_notes.is_empty());
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A [`MemDir`] that counts the reads of journal segments (newest-first reading is bounded).
#[derive(Debug, Clone)]
struct CountingReads {
    inner: MemDir,
    reads: Arc<Mutex<Vec<String>>>,
}

impl CountingReads {
    fn note(&self, name: &str) {
        if name.starts_with("journal/") {
            self.reads.lock().unwrap().push(name.to_string());
        }
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

impl StoreDir for CountingReads {
    fn location(&self) -> String {
        "memory:counting-reads".into()
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
        self.note(name);
        self.inner.read(name, max)
    }
    // Ranges are the version walk's header reads (64 bytes per segment): not counted.
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> io::Result<Box<dyn io::Read + Send>> {
        self.note(name);
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        self.inner.create(name, exclusive)
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        self.inner.open_write(name, offset)
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
}

/// Every record of a page sequence read newest first, following `next_before`.
fn newest_first_pages(
    read: impl Fn(JournalQuery) -> graphersal::system_journal::JournalPage,
    limit: usize,
) -> Vec<JournalRecord> {
    let mut out = Vec::new();
    let mut query = JournalQuery::new().newest_first().limit(limit);
    loop {
        let page = read(query.clone());
        assert!(
            page.records.windows(2).all(|w| w[0].seq > w[1].seq),
            "newest first"
        );
        out.extend(page.records.iter().cloned());
        if !page.more {
            break;
        }
        query = query.before(
            page.next_before
                .expect("a newest-first page has next_before"),
        );
    }
    out
}

#[test]
fn a_newest_first_query_reads_the_segments_from_the_end_and_pages_backwards() {
    let inner = MemDir::new();
    let counting = CountingReads {
        inner: inner.clone(),
        reads: Arc::new(Mutex::new(Vec::new())),
    };
    let dir: Arc<dyn StoreDir> = Arc::new(counting.clone());
    let options = options().with_journal_segment_bytes(150);
    let store = Store::create(&dir, options.clone()).unwrap();
    for i in 0..8 {
        add_person(&store, &format!("p{i}"));
        store.checkpoint(None).unwrap();
    }
    store.close().unwrap();
    let names = segments(&inner);
    assert!(names.len() >= 5, "rotation at the size: {names:?}");
    let every = all(&inner);
    let newest: Vec<u64> = every.iter().rev().map(|r| r.seq).collect();

    // The closed store: the newest two records, from the newest segment(s) only.
    counting.take();
    let page = Store::journal_dir(&dir, &JournalQuery::new().newest_first().limit(2)).unwrap();
    let read = counting.take();
    assert_eq!(
        page.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
        newest[..2]
    );
    assert!(page.more);
    assert_eq!(page.next_after, newest[0], "a reader follows from the end");
    assert_eq!(page.next_before, Some(newest[1]));
    let oldest_name = &names[0];
    assert!(
        !read.contains(oldest_name),
        "the oldest segment is not read for the newest records: {read:?}"
    );
    assert!(read.len() < names.len(), "{read:?} of {names:?}");

    // Paging backwards gives every record once, newest first.
    let backwards = newest_first_pages(|q| Store::journal_dir(&dir, &q).unwrap(), 3);
    assert_eq!(backwards.iter().map(|r| r.seq).collect::<Vec<_>>(), newest);
    // Filters apply: the checkpoints only.
    let checkpoints = Store::journal_dir(
        &dir,
        &JournalQuery::new()
            .newest_first()
            .kind("store.checkpoint_done")
            .limit(100),
    )
    .unwrap();
    assert_eq!(checkpoints.records.len(), 8);
    assert!(!checkpoints.more);

    // An open store: the newest from memory, older ones from the segments, the same order.
    let store = Store::open(&dir, options).unwrap();
    let journal = store.journal().unwrap();
    let live = journal.query(&JournalQuery::new().limit(10_000)).unwrap();
    let live_newest: Vec<u64> = live.records.iter().rev().map(|r| r.seq).collect();
    let open_backwards = newest_first_pages(|q| journal.query(&q).unwrap(), 4);
    assert_eq!(
        open_backwards.iter().map(|r| r.seq).collect::<Vec<_>>(),
        live_newest
    );
    let first = journal
        .query(&JournalQuery::new().newest_first().limit(1))
        .unwrap();
    assert_eq!(first.records[0].kind, "store.opened");
    // Following forwards from the newest page sees exactly what comes next.
    add_person(&store, "later");
    store.checkpoint(None).unwrap();
    let next = journal
        .query(&JournalQuery::new().after(first.next_after))
        .unwrap();
    assert_eq!(kinds(&next.records), ["store.checkpoint_done"]);
    store.close().unwrap();
}
