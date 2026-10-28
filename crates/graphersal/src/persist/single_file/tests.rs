//! Unit tests of the single-file container: names and bytes survive a reopen, the table and
//! the header slots, torn tails at every offset of the last record, bit flips in the header
//! slots, the table and the records, compaction, and the OS lock between two containers.

use super::*;

/// A fresh path under the temp directory (removed on drop).
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "graphersal-container-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir.join("store.gstore"))
    }

    fn other(&self, name: &str) -> PathBuf {
        self.0.with_file_name(name)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

/// A container of `path` that does not share the process registry (another process's view).
fn unshared(path: &Path) -> SingleFileDir {
    SingleFileDir {
        inner: Arc::new(Container {
            path: path.to_path_buf(),
            state: Mutex::new(State::default()),
            released: Condvar::new(),
        }),
    }
}

/// Every file with its bytes, and every directory.
fn image(dir: &dyn StoreDir) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
    fn walk(
        dir: &dyn StoreDir,
        at: &str,
        files: &mut Vec<(String, Vec<u8>)>,
        dirs: &mut Vec<String>,
    ) {
        let mut entries = dir.list(at).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            let name = if at.is_empty() {
                entry.name.clone()
            } else {
                format!("{at}/{}", entry.name)
            };
            match entry.kind {
                EntryKind::File => {
                    let bytes = dir.read(&name, u64::MAX).unwrap();
                    assert_eq!(bytes.len() as u64, dir.file_len(&name).unwrap());
                    files.push((name, bytes));
                }
                _ => {
                    dirs.push(name.clone());
                    walk(dir, &name, files, dirs);
                }
            }
        }
    }
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    walk(dir, "", &mut files, &mut dirs);
    (files, dirs)
}

/// A container with every kind of record.
fn populate(dir: &SingleFileDir) {
    dir.create_dir_all("snapshots/tmp-1").unwrap();
    dir.create_dir_all("wal").unwrap();
    let mut file = dir.create("snapshots/tmp-1/manifest", true).unwrap();
    file.write_all(&[7u8; 5000]).unwrap();
    file.sync().unwrap();
    drop(file);
    dir.rename("snapshots/tmp-1", "snapshots/00000000000000000001")
        .unwrap();
    dir.write_atomic("GRAPH", b"graph-v1").unwrap();
    dir.write_atomic("GRAPH", b"graph-v2").unwrap();
    let mut wal = dir.create("wal/1.wal", true).unwrap();
    for i in 0..20u8 {
        wal.write_all(&[i; 100]).unwrap();
        wal.flush().unwrap();
    }
    wal.set_len(1500).unwrap();
    wal.write_all(b"tail").unwrap();
    wal.sync().unwrap();
    drop(wal);
    dir.append("marks", b"m1").unwrap();
    dir.append("marks", b"m2").unwrap();
    dir.write_atomic("INTENT", b"x").unwrap();
    dir.remove_file("INTENT").unwrap();
    dir.create_dir_all("attic/a/wal").unwrap();
    dir.append("attic/a/wal/x", b"gone").unwrap();
    dir.remove_tree("attic/a").unwrap();
    dir.sync_dir("").unwrap();
}

#[test]
fn names_and_bytes_survive_a_reopen() {
    let tmp = TempFile::new("reopen");
    let dir = unshared(&tmp.0);
    assert_eq!(dir.entry_kind("").unwrap(), None);
    populate(&dir);
    let expected = image(&dir);
    assert_eq!(dir.read("GRAPH", 100).unwrap(), b"graph-v2");
    assert_eq!(dir.read("marks", 100).unwrap(), b"m1m2");
    let wal = dir.read("wal/1.wal", 10_000).unwrap();
    assert_eq!(wal.len(), 1504);
    assert_eq!(&wal[1500..], b"tail");
    assert_eq!(
        dir.read_range("wal/1.wal", 150, 100).unwrap()[..50],
        [1u8; 50]
    );
    assert!(dir.read_range("wal/1.wal", 1500, 5).is_err());
    assert!(dir.read("GRAPH", 3).is_err());
    assert_eq!(dir.entry_kind("attic").unwrap(), Some(EntryKind::Dir));
    assert_eq!(dir.entry_kind("attic/a").unwrap(), None);
    drop(dir);
    let reopened = unshared(&tmp.0);
    assert_eq!(image(&reopened), expected);
    assert!(reopened.check().unwrap().is_ok(), "{:?}", reopened.check());
    let mut read = String::new();
    reopened
        .open_read("GRAPH")
        .unwrap()
        .read_to_string(&mut read)
        .unwrap();
    assert_eq!(read, "graph-v2");
}

#[test]
fn file_system_refusals_are_kept() {
    let tmp = TempFile::new("refusals");
    let dir = unshared(&tmp.0);
    assert!(dir.create("a/b", false).is_err(), "no parent directory");
    dir.create_dir_all("a").unwrap();
    dir.create("a/b", true).unwrap();
    assert_eq!(
        dir.create("a/b", true).err().map(|e| e.kind()),
        Some(io::ErrorKind::AlreadyExists)
    );
    assert!(dir.create_dir_all("a/b/c").is_err(), "a file is in the way");
    assert!(dir.remove_file("a/zz").is_err());
    dir.remove_tree("a/zz").unwrap();
    assert!(dir.rename("a", "a/inner").is_err());
    assert!(dir.entry_kind("../x").is_err());
    assert!(dir.list("missing").is_err());
}

#[test]
fn a_table_is_written_when_the_tail_grows_and_at_the_writers_close() {
    let tmp = TempFile::new("table");
    let dir = unshared(&tmp.0);
    let guard = dir.lock(LockKind::Writer).unwrap().unwrap();
    dir.create_dir_all("wal").unwrap();
    let mut wal = dir.create("wal/1.wal", true).unwrap();
    let block = vec![3u8; 1 << 20];
    for _ in 0..17 {
        wal.write_all(&block).unwrap();
        wal.flush().unwrap();
    }
    wal.sync().unwrap();
    drop(wal);
    let generation = |dir: &SingleFileDir| dir.inner.state().open.as_ref().unwrap().generation;
    assert!(generation(&dir) >= 2, "a table after 16 MiB of records");
    dir.write_atomic("GRAPH", b"g").unwrap();
    let before = generation(&dir);
    drop(guard);
    assert_eq!(generation(&dir), before + 1, "a table at close");
    let reopened = unshared(&tmp.0);
    assert!(reopened.inner.state().open.is_none());
    assert_eq!(reopened.file_len("wal/1.wal").unwrap(), 17 << 20);
    let st = reopened.inner.state();
    let open = st.open.as_ref().unwrap();
    assert_eq!(open.tail_bytes, 0, "nothing to replay after a close");
}

/// The bytes of the file, and a copy of them at another path.
fn copy_with(tmp: &TempFile, bytes: &[u8], name: &str) -> PathBuf {
    let path = tmp.other(name);
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn every_cut_of_the_last_record_is_a_torn_tail() {
    let tmp = TempFile::new("cuts");
    let dir = unshared(&tmp.0);
    populate(&dir);
    let expected = image(&dir);
    let complete = fs::metadata(&tmp.0).unwrap().len();
    dir.write_atomic("GRAPH", b"graph-v3").unwrap();
    dir.append("marks", b"m3").unwrap();
    let full = fs::read(&tmp.0).unwrap();
    drop(dir);
    let mut checked = 0;
    for cut in complete..full.len() as u64 {
        let path = copy_with(&tmp, &full[..cut as usize], "cut.gstore");
        let cut_dir = unshared(&path);
        let check = cut_dir.check().unwrap();
        assert!(check.damage.is_empty(), "cut at {cut}: {check:?}");
        let (files, dirs) = image(&cut_dir);
        let graph = &files.iter().find(|(n, _)| n == "GRAPH").unwrap().1;
        // The first record (GRAPH v3) is either whole or not there; marks never half.
        assert!(graph == b"graph-v2" || graph == b"graph-v3", "cut at {cut}");
        assert_eq!(dirs, expected.1, "cut at {cut}");
        // The next write cuts the torn tail and goes on.
        cut_dir.append("marks", b"after").unwrap();
        drop(cut_dir);
        let again = unshared(&path);
        assert!(again.check().unwrap().is_ok(), "cut at {cut}");
        let marks = again.read("marks", 100).unwrap();
        assert!(marks.ends_with(b"after"), "cut at {cut}");
        checked += 1;
    }
    assert!(checked > 40);
}

#[test]
fn zero_filled_tails_and_unsynced_reordering_are_torn_tails() {
    let tmp = TempFile::new("zeros");
    let dir = unshared(&tmp.0);
    populate(&dir);
    let expected = image(&dir);
    let synced_len = fs::metadata(&tmp.0).unwrap().len();
    // Two records that are never synced (StoreFile writes without sync).
    dir.create_dir_all("wal").unwrap();
    let mut wal = dir.open_write("wal/1.wal", 1504).unwrap();
    wal.write_all(b"unsynced-1").unwrap();
    wal.flush().unwrap();
    wal.write_all(b"unsynced-2").unwrap();
    wal.flush().unwrap();
    drop(wal);
    let mut full = fs::read(&tmp.0).unwrap();
    drop(dir);
    // The first unsynced record lost (zeros), the second one on disk: a crash's reordering.
    let first_len = 44 + 1 + 9 + 2 + 10;
    for b in &mut full[synced_len as usize..synced_len as usize + first_len] {
        *b = 0;
    }
    let path = copy_with(&tmp, &full, "reordered.gstore");
    let reordered = unshared(&path);
    assert!(reordered.check().unwrap().damage.is_empty());
    assert_eq!(image(&reordered), expected);
    // Only zeros after the synced end.
    let mut zeros = full.clone();
    zeros[synced_len as usize..].fill(0);
    let path = copy_with(&tmp, &zeros, "zeros.gstore");
    assert_eq!(image(&unshared(&path)), expected);
}

#[test]
fn a_damaged_synced_record_followed_by_durable_ones_is_damage() {
    let tmp = TempFile::new("damage");
    let dir = unshared(&tmp.0);
    populate(&dir);
    let at = fs::metadata(&tmp.0).unwrap().len();
    dir.write_atomic("GRAPH", b"graph-v3").unwrap();
    dir.append("marks", b"m3").unwrap();
    let full = fs::read(&tmp.0).unwrap();
    drop(dir);
    // A flipped bit in the header of the (synced) GRAPH record: the later record proves it
    // was durable.
    let mut flipped = full.clone();
    flipped[at as usize + 9] ^= 0x01;
    let path = copy_with(&tmp, &flipped, "flip-head.gstore");
    let damaged = unshared(&path);
    let check = damaged.check().unwrap();
    assert_eq!(check.damage.len(), 2, "{check:?}");
    assert!(check.damage[0].contains("not readable"), "{check:?}");
    assert!(check.damage[1].contains("missing"), "{check:?}");
    assert_eq!(damaged.read("marks", 100).unwrap(), b"m1m2m3");
    assert!(
        damaged.write_atomic("x", b"y").is_err(),
        "a damaged container is read-only"
    );
    // A flipped bit in the payload of a replacement: skipped, the old content stays.
    let mut flipped = full.clone();
    let payload_at = at as usize + 44 + 1 + 5 + 2;
    flipped[payload_at] ^= 0x01;
    let path = copy_with(&tmp, &flipped, "flip-put.gstore");
    let damaged = unshared(&path);
    assert_eq!(damaged.check().unwrap().damage.len(), 1);
    assert_eq!(damaged.read("GRAPH", 100).unwrap(), b"graph-v2");
    // A flipped bit in the payload of the LAST append: handed through (the Store judges).
    let mut flipped = full.clone();
    let last = full.len() - 1;
    flipped[last] ^= 0x01;
    let path = copy_with(&tmp, &flipped, "flip-last.gstore");
    let passed = unshared(&path);
    assert!(passed.check().unwrap().damage.is_empty());
    assert_eq!(passed.read("marks", 100).unwrap(), b"m1m2m2");
}

#[test]
fn header_slots_and_tables_fall_back_to_the_older_generation() {
    let tmp = TempFile::new("slots");
    let dir = unshared(&tmp.0);
    populate(&dir);
    dir.inner.mutate(|open| open.write_table()).unwrap();
    dir.append("marks", b"m3").unwrap();
    dir.inner.mutate(|open| open.write_table()).unwrap();
    dir.append("marks", b"m4").unwrap();
    let expected = image(&dir);
    let (generation, table_at) = {
        let st = dir.inner.state();
        let open = st.open.as_ref().unwrap();
        (open.generation, open.table_at)
    };
    let full = fs::read(&tmp.0).unwrap();
    drop(dir);
    let newest_slot = SLOT_AT[(generation % 2) as usize] as usize;
    for (what, at) in [
        ("newest slot", newest_slot + 20),
        (
            "older slot",
            SLOT_AT[((generation + 1) % 2) as usize] as usize + 20,
        ),
        ("newest table", table_at as usize + 50),
    ] {
        let mut flipped = full.clone();
        flipped[at] ^= 0x04;
        let path = copy_with(&tmp, &flipped, "slot.gstore");
        let opened = unshared(&path);
        let check = opened.check().unwrap();
        assert!(check.damage.is_empty(), "{what}: {check:?}");
        assert!(!check.warnings.is_empty(), "{what}: {check:?}");
        assert_eq!(image(&opened), expected, "{what}");
        // The next write repairs it (a new table into the damaged or older slot).
        opened.append("marks", b"!").unwrap();
        opened.inner.mutate(|open| open.write_table()).unwrap();
        drop(opened);
        let again = unshared(&path);
        assert!(
            again.check().unwrap().is_ok(),
            "{what}: {:?}",
            again.check()
        );
        let _ = fs::remove_file(&path);
    }
    // Both slots damaged: every record from the start.
    let mut flipped = full.clone();
    flipped[SLOT_AT[0] as usize + 20] ^= 0x04;
    flipped[SLOT_AT[1] as usize + 20] ^= 0x04;
    let path = copy_with(&tmp, &flipped, "both.gstore");
    let opened = unshared(&path);
    assert!(opened.check().unwrap().damage.is_empty());
    assert_eq!(image(&opened), expected);
}

#[test]
fn compaction_keeps_every_name_and_byte_and_gives_the_garbage_back() {
    let tmp = TempFile::new("compact");
    let dir = unshared(&tmp.0);
    populate(&dir);
    dir.create_dir_all("snapshots/old").unwrap();
    dir.write_atomic("snapshots/old/seg", &vec![9u8; 2 << 20])
        .unwrap();
    dir.remove_tree("snapshots/old").unwrap();
    let expected = image(&dir);
    let usage = dir.space_usage().unwrap().unwrap();
    assert!(usage.garbage_bytes() > 2 << 20, "{usage:?}");
    let guard = dir.lock(LockKind::Writer).unwrap().unwrap();
    let done = dir.compact().unwrap().unwrap();
    assert!(done.bytes_after < done.bytes_before / 4, "{done:?}");
    assert_eq!(fs::metadata(&tmp.0).unwrap().len(), done.bytes_after);
    let after = dir.space_usage().unwrap().unwrap();
    assert!(after.garbage_bytes() < 4096, "{after:?} ({usage:?} before)");
    assert_eq!(image(&dir), expected);
    // Still the writer of the new file: another container cannot lock it.
    assert!(unshared(&tmp.0).lock(LockKind::Writer).unwrap().is_none());
    dir.append("marks", b"more").unwrap();
    drop(guard);
    let reopened = unshared(&tmp.0);
    assert!(reopened.check().unwrap().is_ok());
    assert_eq!(reopened.read("marks", 100).unwrap(), b"m1m2more");
    assert!(reopened.lock(LockKind::Writer).unwrap().is_some());
    // No temporary file is left next to it.
    let names: Vec<_> = fs::read_dir(tmp.0.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

#[test]
fn the_writer_lock_is_the_os_lock_and_readers_see_the_writers_records() {
    let tmp = TempFile::new("lock");
    let writer = unshared(&tmp.0);
    let guard = writer.lock(LockKind::Writer).unwrap().unwrap();
    assert!(writer.lock(LockKind::Writer).unwrap().is_none());
    // Another process (another container of the same file).
    let other = unshared(&tmp.0);
    assert!(other.lock(LockKind::Writer).unwrap().is_none());
    assert!(
        other.write_atomic("x", b"y").is_err(),
        "writes need the lock"
    );
    writer.write_atomic("GRAPH", b"one").unwrap();
    assert_eq!(other.read("GRAPH", 10).unwrap(), b"one");
    // A pinned view stays as it was.
    let pin = other.lock(LockKind::BackupShared).unwrap().unwrap();
    writer.write_atomic("GRAPH", b"two").unwrap();
    assert_eq!(other.read("GRAPH", 10).unwrap(), b"one");
    drop(pin);
    assert_eq!(other.read("GRAPH", 10).unwrap(), b"two");
    drop(guard);
    other.write_atomic("GRAPH", b"three").unwrap();
    assert_eq!(writer.read("GRAPH", 10).unwrap(), b"three");
    // The registry shares one container per path in a process.
    let a = SingleFileDir::new(&tmp.0);
    let b = SingleFileDir::new(&tmp.0);
    assert!(Arc::ptr_eq(&a.inner, &b.inner));
}

#[test]
fn random_bytes_never_panic() {
    let tmp = TempFile::new("random");
    let dir = unshared(&tmp.0);
    populate(&dir);
    let full = fs::read(&tmp.0).unwrap();
    drop(dir);
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..200 {
        let mut bytes = full.clone();
        for _ in 0..(1 + round % 8) {
            let at = (next() % bytes.len() as u64) as usize;
            bytes[at] = next() as u8;
        }
        if round % 5 == 0 {
            bytes.truncate((next() % bytes.len() as u64) as usize);
        }
        let path = copy_with(&tmp, &bytes, "random.gstore");
        let dir = unshared(&path);
        let _ = dir.check();
        if let Ok(entries) = dir.list("") {
            for entry in entries {
                let _ = dir.read(&entry.name, 1 << 20);
            }
        }
    }
}
