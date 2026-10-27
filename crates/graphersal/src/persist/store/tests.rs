//! Crate tests of the Store that need injected failures (a failing `fsync`, a failing cut)
//! or interrupted multi-file operations (crash simulations of rollback and restore). They run
//! on a [`MemDir`](crate::persist::MemDir): a crash image is a `deep_copy` of it.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::wal_files::Faults;
use super::*;
use crate::graph::{GraphStorage, Transactional};
use crate::persist::MemDir;

fn root(dir: &MemDir) -> DirPath {
    DirPath::root(dir.clone().into_store_dir())
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

pub(super) fn names_of(graph: &TraversalGraph) -> Vec<String> {
    let mut names: Vec<String> = graph
        .vertices()
        .filter_map(|v| match graph.vertex_properties(v)?.get("name")? {
            crate::value::ElementProperty::String(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    names.sort();
    names
}

fn last_wal(dir: &MemDir) -> DirPath {
    list_wal(&root(dir)).unwrap().pop().unwrap().1
}

#[test]
fn a_commit_whose_fsync_fails_is_cut_off_and_never_replayed() {
    let dir = MemDir::new();
    let faults = Arc::new(Faults::default());
    let mut options = StoreOptions::new();
    options.faults = Some(faults.clone());
    let store = Store::create(&dir, options).unwrap();
    add_person(&store, "ann").unwrap();
    let wal = last_wal(&dir);
    let durable = wal.len().unwrap();

    faults.fail_sync.store(true, Ordering::Release);
    let err = add_person(&store, "bob").unwrap_err();
    assert!(err.contains("syncing the journal failed"), "{err}");
    assert!(err.contains("cut off again"), "{err}");
    // The refused record is gone from the file, and the durable end did not move.
    assert_eq!(wal.len().unwrap(), durable);
    assert_eq!(store.durable_end().offset, durable);
    assert_eq!(names_of(&store.graph().read()), ["ann"]);
    // Poisoned until reopened, even when the disk recovers.
    faults.fail_sync.store(false, Ordering::Release);
    assert!(add_person(&store, "cy").is_err());
    drop(store);

    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(names_of(&store.graph().read()), ["ann"]);
    add_person(&store, "dan").unwrap();
    drop(store);
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(names_of(&store.graph().read()), ["ann", "dan"]);
}

#[test]
fn a_refused_record_that_cannot_be_cut_is_recorded_and_cut_at_the_next_open() {
    let dir = MemDir::new();
    let faults = Arc::new(Faults::default());
    let mut options = StoreOptions::new();
    options.faults = Some(faults.clone());
    let store = Store::create(&dir, options).unwrap();
    add_person(&store, "ann").unwrap();
    let wal = last_wal(&dir);
    let durable = wal.len().unwrap();

    faults.fail_sync.store(true, Ordering::Release);
    faults.fail_truncate.store(true, Ordering::Release);
    let err = add_person(&store, "bob").unwrap_err();
    assert!(err.contains("recorded"), "{err}");
    // The refused record is still in the file, its position is in GRAPH.
    assert!(wal.len().unwrap() > durable);
    let file = read_graph_file(&root(&dir)).unwrap().file;
    assert_eq!(file.wal_cut.map(|(_, offset)| offset), Some(durable));
    // A crash now: nothing reads the refused record.
    let image = dir.deep_copy();
    drop(store);

    let past = Store::open_read_only(&image, RecoveryTarget::Latest).unwrap();
    assert_eq!(names_of(&past.graph), ["ann"]);
    let verify = Store::verify_dir(&image).unwrap();
    assert!(verify.is_ok(), "{verify:?}");
    assert!(
        verify.notes.iter().any(|n| n.contains("refused")),
        "{verify:?}"
    );

    let store = Store::open(&image, StoreOptions::new()).unwrap();
    let cut = store.open_report().refused_cut.expect("cut at open");
    assert_eq!(cut.offset, durable);
    assert_eq!(names_of(&store.graph().read()), ["ann"]);
    assert_eq!(last_wal(&image).len().unwrap(), durable);
    assert!(
        read_graph_file(&root(&image))
            .unwrap()
            .file
            .wal_cut
            .is_none()
    );
    add_person(&store, "cy").unwrap();
    drop(store);
    let store = Store::open(&image, StoreOptions::new()).unwrap();
    assert_eq!(names_of(&store.graph().read()), ["ann", "cy"]);
}

/// ann (1), mark "m1", bob (2), checkpoint, cy (3), dan (4); closed.
fn history_image(segment_bytes: u64) -> MemDir {
    let dir = MemDir::new();
    let store = Store::create(
        &dir,
        StoreOptions::new().with_wal_segment_bytes(segment_bytes),
    )
    .unwrap();
    add_person(&store, "ann").unwrap();
    store.mark("m1").unwrap();
    add_person(&store, "bob").unwrap();
    store.checkpoint(None).unwrap();
    add_person(&store, "cy").unwrap();
    add_person(&store, "dan").unwrap();
    store.close().unwrap();
    dir
}

#[test]
fn a_rollback_interrupted_after_any_step_is_completed_at_the_next_open() {
    for segment_bytes in [1u64, 1 << 20] {
        let image = history_image(segment_bytes);
        for stop in 1..=16 {
            let dir = image.deep_copy();
            let path = root(&dir);
            let intent = rollback::plan_rollback(&path, 1, 4, RollbackMode::Move, "test").unwrap();
            intent.write(&path).unwrap();
            rollback::apply_rollback(&path, &intent, Some(stop)).unwrap();
            let store = Store::open(&dir, StoreOptions::new())
                .unwrap_or_else(|err| panic!("stop {stop}: {err}"));
            let context = format!("segments {segment_bytes}, stop {stop}");
            assert_eq!(names_of(&store.graph().read()), ["ann"], "{context}");
            assert!(!path.join(files::INTENT_FILE).exists(), "{context}");
            let info = store.info().unwrap();
            assert_eq!(info.lineage.len(), 2, "{context}");
            assert_eq!(info.attic.len(), 1, "{context}");
            let id = info.attic[0].id.clone();
            let moved: Vec<u64> = store
                .attic_changes(&id)
                .unwrap()
                .map(|c| c.unwrap().commit_seq())
                .collect();
            assert_eq!(moved, [2, 3, 4], "{context}");
            assert!(
                store.verify().unwrap().is_ok(),
                "{context}: {:?}",
                store.verify()
            );
            // And the entry still restores.
            assert_eq!(store.attic_restore(&id).unwrap(), 4, "{context}");
            assert_eq!(
                names_of(&store.graph().read()),
                ["ann", "bob", "cy", "dan"],
                "{context}"
            );
        }
    }
}

#[test]
fn a_restore_interrupted_after_any_step_is_completed_at_the_next_open() {
    let image = history_image(1 << 20);
    let store = Store::open(&image, StoreOptions::new()).unwrap();
    let id = store
        .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move)
        .unwrap()
        .attic
        .unwrap();
    store.close().unwrap();
    for stop in 1..=10 {
        let dir = image.deep_copy();
        let path = root(&dir);
        let intent = rollback::plan_restore(&path, &id, 1).unwrap();
        intent.write(&path).unwrap();
        rollback::apply_restore(&path, &intent, Some(stop)).unwrap();
        let store = Store::open(&dir, StoreOptions::new())
            .unwrap_or_else(|err| panic!("stop {stop}: {err}"));
        assert_eq!(
            names_of(&store.graph().read()),
            ["ann", "bob", "cy", "dan"],
            "stop {stop}"
        );
        let info = store.info().unwrap();
        assert_eq!(info.lineage.len(), 1, "stop {stop}");
        assert!(info.attic.is_empty(), "stop {stop}");
        assert_eq!(info.marks.len(), 1, "stop {stop}");
        assert!(
            store.verify().unwrap().is_ok(),
            "stop {stop}: {:?}",
            store.verify()
        );
    }
}

/// The automatic checkpoint without a background thread (the browser): due once the WAL
/// since the last snapshot reaches the threshold, run by `run_due_checkpoint`.
#[test]
fn the_due_checkpoint_runs_synchronously_without_a_thread() {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new().with_checkpoint_after_bytes(0)).unwrap();
    add_person(&store, "ann").unwrap();
    // Off (threshold 0): never due.
    assert!(store.run_due_checkpoint().unwrap().is_none());
    drop(store);
    // Natively a thread would run it at once; open without one (threshold 0 starts none, as on
    // wasm32) and lower the threshold afterwards, so no thread can take the checkpoint first.
    let store = Store::open(&dir, StoreOptions::new().with_checkpoint_after_bytes(0)).unwrap();
    store
        .core
        .shared
        .wal
        .checkpoint_after
        .store(1, Ordering::Release);
    assert!(store.wal_bytes_since_checkpoint() > 0);
    let snapshot = store.run_due_checkpoint().unwrap().expect("due");
    assert_eq!(snapshot.commit_seq, 1);
    // Only the Checkpoint record is left since the snapshot: nothing is due.
    assert!(store.wal_bytes_since_checkpoint() < 100);
    assert!(store.run_due_checkpoint().unwrap().is_none());
    assert_eq!(store.snapshots().unwrap().len(), 2);
}

/// A packed snapshot of `graph` whose segment table gains a file of segment kind `kind`
/// (format spec 7.5) carried as a named extra file (section kind 5, format spec 8).
fn packed_with_extra_file(graph: &TraversalGraph, kind: u8, file: &[u8]) -> Vec<u8> {
    use super::super::codec::crc as crc32;
    use super::super::manifest::SegmentEntry;
    use super::super::snapshot::{
        PACKED_MAGIC, PackedReader, SECTION_MANIFEST, SECTION_SCHEMA, extra_file_prefix,
        section_kind,
    };
    let mut plain = Vec::new();
    graph.write_snapshot(&mut plain).unwrap();
    let mut reader = PackedReader::open(plain.as_slice()).unwrap();
    let mut segments = Vec::new();
    for _ in 0..reader.manifest.segments.len() {
        let mut bytes = Vec::new();
        reader.copy_segment(&mut bytes).unwrap();
        segments.push(bytes);
    }
    let mut manifest = reader.manifest.clone();
    let schema = reader.schema.clone();
    reader.finish().unwrap();
    let kinds: Vec<u8> = manifest.segments.iter().map(|s| s.kind).collect();
    manifest.segments.push(SegmentEntry {
        kind,
        path: "extra.bin".into(),
        first_id: "".into(),
        last_id: "".into(),
        count: 0,
        byte_len: file.len() as u64,
        crc: crc32(file),
    });
    let manifest_bytes = manifest.encode();
    let mut out = PACKED_MAGIC.to_vec();
    out.extend_from_slice(&super::super::codec::FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(2 + manifest.segments.len() as u32).to_le_bytes());
    let mut section = |kind: u8, bytes: &[u8]| {
        out.push(kind);
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(bytes);
    };
    section(SECTION_MANIFEST, &manifest_bytes);
    section(SECTION_SCHEMA, &schema);
    for (segment, segment_kind) in segments.iter().zip(kinds) {
        section(section_kind(segment_kind), segment);
    }
    let mut named = extra_file_prefix("extra.bin");
    named.extend_from_slice(file);
    section(section_kind(kind), &named);
    let trailer = crc32(&out);
    out.extend_from_slice(&trailer.to_le_bytes());
    out
}

#[test]
fn files_of_other_segment_kinds_are_checked_skipped_and_a_critical_one_is_read_only() {
    let graph = TraversalGraph::tinkerpop_modern();
    let names = names_of(&graph);
    // An ignorable kind (3..=127): loads, opens writable, a checkpoint does not carry it.
    let packed = packed_with_extra_file(&graph, 3, b"derived data");
    let loaded = crate::persist::read_snapshot(packed.as_slice(), &ReadOptions::new()).unwrap();
    assert_eq!(names_of(&loaded), names);
    let dir = MemDir::new();
    let store =
        Store::create_from_packed(dir.clone(), packed.as_slice(), StoreOptions::new()).unwrap();
    assert!(store.read_only_reason().unwrap().is_none());
    assert!(store.verify().unwrap().is_ok());
    let first = store.snapshots().unwrap()[0].commit_seq;
    let first_dir = root(&dir)
        .join(SNAPSHOTS_DIR)
        .join(files::snapshot_name(first));
    assert!(first_dir.join("extra.bin").exists());
    // Exported again as packed: the directory streams it as the same section.
    let mut exported = Vec::new();
    store.export_packed(Some(first), &mut exported).unwrap();
    assert_eq!(exported, packed);
    add_person(&store, "ann").unwrap();
    let info = store.checkpoint(None).unwrap();
    let (_, manifest) = snapshot_dir::read_manifest(
        &root(&dir)
            .join(SNAPSHOTS_DIR)
            .join(files::snapshot_name(info.commit_seq)),
    )
    .unwrap();
    assert!(manifest.segments.iter().all(|s| s.kind == 1 || s.kind == 2));
    store.close().unwrap();

    // A damaged extra file is damage of the snapshot.
    let mut damaged = packed.clone();
    let at = damaged.len() - 6;
    damaged[at] ^= 1;
    assert!(crate::persist::read_snapshot(damaged.as_slice(), &ReadOptions::new()).is_err());

    // A critical kind (128..=255): the store opens read-only.
    let packed = packed_with_extra_file(&graph, 200, b"index");
    let store =
        Store::create_from_packed(MemDir::new(), packed.as_slice(), StoreOptions::new()).unwrap();
    let reason = store.read_only_reason().unwrap().unwrap();
    assert!(reason.contains("critical segment kind 200"), "{reason}");
    assert!(add_person(&store, "bob").unwrap_err().contains("read-only"));
}

// ---------------------------------------------------------------------------------------------
// The streaming snapshot encoder into snapshot directories
// ---------------------------------------------------------------------------------------------

/// Writes `graph` as a snapshot directory of `dir` with the streaming encoder and returns the
/// directory read back as its packed stream (and the encoder's figures); the directory is
/// removed again.
fn dir_snapshot(
    dir: Arc<dyn StoreDir>,
    graph: &TraversalGraph,
    options: &WriteOptions,
) -> (Vec<u8>, crate::persist::encoder::EncodeStats) {
    use std::io::Read as _;
    let root = DirPath::root(dir);
    let snapshot = snapshot_dir::NewSnapshotDir::begin(&root, graph.last_commit_seq()).unwrap();
    let stats = write_graph_snapshot(snapshot.tmp(), graph, options).unwrap();
    let (path, _) = snapshot.finish().unwrap();
    let mut bytes = Vec::new();
    DirSnapshotReader::open(&path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    path.remove().unwrap();
    (bytes, stats)
}

fn identity_options(codec: Codec, chunk: u32, segment: u64) -> WriteOptions {
    WriteOptions::new()
        .with_codec(codec)
        .with_chunk_bytes(chunk)
        .with_segment_bytes(segment)
        .with_name("identity")
        .with_meta("{\"m\":1}")
        .with_created_at(1_700_000_000_000_000)
}

/// A snapshot directory written chunk by chunk holds exactly the sections of the packed
/// stream of the same graph and options (read back as the packed stream it is equivalent to,
/// format spec 8), for every graph, codec, chunk and segment size.
#[test]
fn directory_snapshots_are_the_sections_of_the_packed_stream() {
    use crate::persist::encoder::tests::{SIZES, codecs, graphs};
    use crate::persist::snapshot::write_packed;
    let dir: Arc<dyn StoreDir> = Arc::new(MemDir::new());
    for (name, graph) in graphs() {
        for codec in codecs() {
            for (chunk, segment) in SIZES {
                let options = identity_options(codec, chunk, segment);
                let mut packed = Vec::new();
                write_packed(&graph, &mut packed, &options).unwrap();
                let (streamed, _) = dir_snapshot(dir.clone(), &graph, &options);
                assert!(
                    packed == streamed,
                    "{name} {codec:?} chunk {chunk} segment {segment}"
                );
            }
        }
    }
}

/// The same on the file-system and the single-file backends.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn directory_snapshots_are_the_same_bytes_on_every_backend() {
    use crate::persist::encoder::tests::generated_graph;
    use crate::persist::snapshot::write_packed;
    use crate::persist::{FsDir, SingleFileDir};
    let base = std::env::temp_dir().join(format!(
        "graphersal-streaming-{}-{}",
        std::process::id(),
        crate::value::uuid::new_v7()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let backends: Vec<Arc<dyn StoreDir>> = vec![
        Arc::new(FsDir::new(base.join("dir"))),
        Arc::new(SingleFileDir::new(base.join("store.gstore"))),
    ];
    let graphs = [TraversalGraph::tinkerpop_modern(), generated_graph(120, 3)];
    for dir in backends {
        for graph in &graphs {
            for (chunk, segment) in [(700, 5000), (1 << 20, 256 << 20)] {
                let options = identity_options(Codec::Lz4, chunk, segment);
                let mut packed = Vec::new();
                write_packed(graph, &mut packed, &options).unwrap();
                let (streamed, _) = dir_snapshot(dir.clone(), graph, &options);
                assert!(packed == streamed, "{} chunk {chunk}", dir.location());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// The encoder holds one chunk at a time: its peak does not grow with the graph, stays near
/// the chunk target and far below the snapshot; the index is 24 bytes per element; every chunk
/// is encoded twice (plan and emit).
#[test]
fn the_streaming_encoder_holds_one_chunk_whatever_the_graph_size() {
    use crate::persist::encoder::tests::generated_graph;
    const CHUNK: u32 = 2048;
    let options = WriteOptions::new().with_chunk_bytes(CHUNK);
    let mut peaks = Vec::new();
    for vertices in [300usize, 3000] {
        let graph = generated_graph(vertices, 11);
        let elements = graph.vertex_count() + graph.edge_count();
        let mut packed = Vec::new();
        let (info, stats) =
            crate::persist::snapshot::write_packed(&graph, &mut packed, &options).unwrap();
        let (_, dir_stats) = dir_snapshot(Arc::new(MemDir::new()), &graph, &options);
        let manifest = crate::persist::snapshot::PackedReader::open(packed.as_slice())
            .unwrap()
            .manifest;
        for stats in [stats, dir_stats] {
            assert_eq!(stats.index_bytes, 24 * elements, "{vertices}");
            assert_eq!(
                stats.chunks_encoded,
                2 * manifest.chunks.len(),
                "{vertices}"
            );
            assert!(
                stats.peak_chunk_bytes < 8 * CHUNK as usize + (16 << 10),
                "{vertices}: peak {} bytes",
                stats.peak_chunk_bytes
            );
            assert!(
                (stats.peak_chunk_bytes as u64) * 10 < packed.len() as u64,
                "{vertices}: peak {} bytes for a snapshot of {} bytes",
                stats.peak_chunk_bytes,
                packed.len()
            );
            peaks.push(stats.peak_chunk_bytes);
        }
        assert_eq!(info.vertex_count as usize, graph.vertex_count());
    }
    let (small, big) = (peaks[0].max(peaks[1]), peaks[2].max(peaks[3]));
    assert!(big < 2 * small + (16 << 10), "peak {small} -> {big} bytes");
}

/// Adds the person `id` (also its name) with an explicit id: ids grow, so a new one lands in
/// the last segment and the others stay unchanged (references between snapshots).
fn add_id(store: &Store, id: &str) {
    store
        .graph()
        .write()
        .transaction(|g| {
            let mut properties = crate::value::PropertyMap::default();
            properties.insert("name", crate::value::ElementProperty::from(id));
            g.insert_vertex(Some(id.into()), vec!["person".into()], properties)
                .map(|_| ())
        })
        .unwrap();
}

/// p000..p009 (commits 1..10), snapshot 10; p010, p011, snapshot 12 (it copies the unchanged
/// segments); p012, snapshot 13 (it references the twins in snapshot 10); p013, p014 (commit
/// 15); a rollback to `first` (12: attic entry E1 holds snapshot 13, which references snapshot
/// 10's files, and the WAL 13..15, base 12; 13: E1 holds the WAL 14..15, base 13). Small chunks
/// and segments, WAL segments of `wal_bytes`. Returns the closed store and E1's id.
fn two_rollbacks_image(wal_bytes: u64, first: u64) -> (MemDir, String, StoreOptions) {
    let options = StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_chunk_bytes(60)
        .with_snapshot_segment_bytes(200)
        .with_wal_segment_bytes(wal_bytes);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    for i in 0..15 {
        add_id(&store, &format!("p{i:03}"));
        if [10, 12, 13].contains(&(i + 1)) {
            store.checkpoint(None).unwrap();
        }
    }
    let snapshot_13 = list_snapshots(&root(&dir)).unwrap().pop().unwrap().1;
    let (_, manifest) = super::snapshot_dir::read_manifest(&snapshot_13).unwrap();
    assert!(
        manifest
            .segments
            .iter()
            .any(|s| s.path.starts_with(&files::snapshot_name(10))),
        "snapshot 13 references snapshot 10"
    );
    let id = store
        .rollback_to(RecoveryTarget::CommitSeq(first), RollbackMode::Move)
        .unwrap()
        .attic
        .unwrap();
    store.close().unwrap();
    (dir, id, options)
}

/// The persons p000..p<n-1>.
fn persons(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("p{i:03}")).collect()
}

/// The attic base gap (format spec 11.1 `rebase`, 11.2): a second rollback to a commit BEFORE
/// the first rollback's base — the first entry's base snapshot moves away (12 or 13), its
/// snapshot's referenced files move away too (9) — interrupted after every step, in move and
/// delete mode: at the next open both entries rebuild their histories, the store verifies, the
/// newer entry restores and then the older one, back to the original history.
#[test]
fn a_rollback_before_an_older_entrys_base_keeps_that_entry_whole_after_any_step() {
    for (wal_bytes, first_target) in [(1u64, 12u64), (1 << 20, 12), (1, 13), (1 << 20, 13)] {
        let (image, first, options) = two_rollbacks_image(wal_bytes, first_target);
        let mut targets = vec![first_target - 1, 11, 9];
        targets.dedup();
        for target in targets {
            for mode in [RollbackMode::Move, RollbackMode::Delete] {
                for stop in 1..=40 {
                    let context = format!(
                        "wal {wal_bytes}, first {first_target}, target {target}, {mode:?}, \
                         stop {stop}"
                    );
                    let dir = image.deep_copy();
                    let path = root(&dir);
                    let intent =
                        rollback::plan_rollback(&path, target, first_target, mode, "test").unwrap();
                    assert!(intent.param("rebase").is_some(), "{context}");
                    assert_eq!(
                        intent.param("drop").is_some(),
                        first_target == 12 && target < 10,
                        "{context}: the older entry's snapshot references snapshot 10"
                    );
                    intent.write(&path).unwrap();
                    rollback::apply_rollback(&path, &intent, Some(stop)).unwrap();
                    let store = Store::open(&dir, options.clone())
                        .unwrap_or_else(|err| panic!("{context}: {err}"));
                    assert_eq!(
                        names_of(&store.graph().read()),
                        persons(target as usize),
                        "{context}"
                    );
                    let report = store.verify().unwrap();
                    assert!(report.is_ok(), "{context}: {:?}", report.problems);
                    let read = ReadOptions::new();
                    let (entry, _, state) = rollback::attic_state(&path, &first, &read).unwrap();
                    assert_eq!(names_of(&state), persons(15), "{context}");
                    assert_eq!(state.last_commit_seq(), 15, "{context}");
                    assert!(entry.base_snapshot <= target, "{context}: {entry:?}");
                    let entries = store.attic_list().unwrap();
                    if mode == RollbackMode::Delete {
                        assert_eq!(entries.len(), 1, "{context}");
                        // The older entry cannot be restored (the store's lineage differs),
                        // but it forks.
                        assert!(store.attic_can_restore(&first).is_err(), "{context}");
                        let fork = MemDir::new();
                        store.attic_fork(&first, &fork).unwrap();
                        let forked = Store::open(&fork, StoreOptions::new()).unwrap();
                        assert_eq!(names_of(&forked.graph().read()), persons(15), "{context}");
                        continue;
                    }
                    assert_eq!(entries.len(), 2, "{context}");
                    let second = entries.iter().find(|e| e.id != first).unwrap().id.clone();
                    let (_, _, state) = rollback::attic_state(&path, &second, &read).unwrap();
                    let at = first_target as usize;
                    assert_eq!(names_of(&state), persons(at), "{context}");
                    // The older entry first: refused (the lineage changed), nothing touched.
                    assert!(store.attic_restore(&first).is_err(), "{context}");
                    // The newer one, then the older one.
                    assert_eq!(
                        store.attic_restore(&second).unwrap(),
                        first_target,
                        "{context}"
                    );
                    assert_eq!(names_of(&store.graph().read()), persons(at), "{context}");
                    assert!(store.verify().unwrap().is_ok(), "{context}");
                    assert_eq!(store.attic_restore(&first).unwrap(), 15, "{context}");
                    assert_eq!(names_of(&store.graph().read()), persons(15), "{context}");
                    let report = store.verify().unwrap();
                    assert!(report.is_ok(), "{context}: {:?}", report.problems);
                    assert!(store.attic_list().unwrap().is_empty(), "{context}");
                    drop(store);
                    let reopened = Store::open(&dir, options.clone()).unwrap();
                    assert_eq!(names_of(&reopened.graph().read()), persons(15), "{context}");
                }
            }
        }
    }
}

/// Rainy day of the attic planning (format spec 11.1): a hostile `INTENT` (an unsafe or
/// malformed `copies` line, a copy or a `drop` outside the attic, `kept` that is no hex) fails the
/// rollback with nothing outside the attic touched; a `rebase` of an entry removed meanwhile is
/// fine; an entry whose base snapshot is gone, or whose snapshot is damaged, is a verify
/// problem; a restore whose destination the store already holds is refused; the names-only
/// prune estimate follows a rebased entry's base.
#[test]
fn hostile_intents_and_broken_entries_are_refused_or_reported() {
    let (image, first, options) = two_rollbacks_image(1 << 20, 13);
    let path_of = |dir: &MemDir| root(dir);
    let intent =
        rollback::plan_rollback(&path_of(&image), 11, 13, RollbackMode::Move, "t").unwrap();
    let copies = intent.param("copies").unwrap().to_string();
    let with = |key: &str, value: &str| {
        let mut bad = intent.clone();
        bad.params.retain(|(k, _)| k != key);
        bad.params.push((key.to_string(), value.to_string()));
        bad
    };
    let first_copy = copies.lines().next().unwrap().to_string();
    let (source, _) = first_copy.split_once(' ').unwrap();
    for bad in [
        with("copies", "no-space-here"),
        with("copies", &format!("{source} ../outside.wal")),
        with("copies", &format!("{source} wal/00000000000000000099.wal")),
        with("copies", &format!("../x attic/{first}/wal/x.wal")),
        with("drop", "snapshots/00000000000000000010"),
        with("drop", "attic/../GRAPH"),
        with("kept", "zz"),
    ] {
        let dir = image.deep_copy();
        let path = path_of(&dir);
        let before: Vec<String> = dir
            .file_names()
            .into_iter()
            .filter(|n| !n.starts_with("attic/"))
            .collect();
        bad.write(&path).unwrap();
        let result = rollback::apply_rollback(&path, &bad, None);
        assert!(result.is_err(), "{:?}", bad.params);
        let after: Vec<String> = dir
            .file_names()
            .into_iter()
            .filter(|n| !n.starts_with("attic/") && n != files::INTENT_FILE)
            .collect();
        if bad.param("kept").is_none() {
            assert_eq!(after, before, "{:?}", bad.params);
        }
    }
    // A rebase of an entry that is gone: nothing to do.
    rebase_check(&image, &options);

    // The entry's base snapshot gone: attic_state and verify say so.
    let dir = image.deep_copy();
    let path = path_of(&dir);
    path.join(files::SNAPSHOTS_DIR)
        .join(files::snapshot_name(13))
        .remove()
        .unwrap();
    let err = rollback::attic_state(&path, &first, &ReadOptions::new())
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("base snapshot at commit 13"), "{err}");
    let report = Store::verify_dir(&dir).unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.path == format!("attic/{first}")),
        "{:?}",
        report.problems
    );

    // A damaged snapshot of an entry: a verify problem of that snapshot.
    let (image12, first12, _) = two_rollbacks_image(1 << 20, 12);
    let dir = image12.deep_copy();
    let manifest = format!(
        "attic/{first12}/snapshots/{}/manifest",
        files::snapshot_name(13)
    );
    let mut bytes = dir.file(&manifest).unwrap();
    bytes[4100] ^= 1; // the variable part (covered by its CRC)
    dir.put_file(&manifest, bytes);
    let report = Store::verify_dir(&dir).unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.path == format!("attic/{first12}/snapshots/{}", files::snapshot_name(13))),
        "{:?}",
        report.problems
    );

    // A restore whose destination exists: refused, nothing changed.
    let dir = image12.deep_copy();
    let taken = format!("snapshots/{}/stray", files::snapshot_name(13));
    dir.put_file(&taken, vec![1]);
    let store = Store::open(&dir, options.clone()).unwrap();
    let err = store.attic_restore(&first12).unwrap_err().to_string();
    assert!(err.contains("already holds"), "{err}");
    drop(store);

    // The names-only estimate of an entry's base follows its rebase.
    let dir = image.deep_copy();
    let store = Store::open(&dir, options).unwrap();
    store
        .rollback_to(RecoveryTarget::CommitSeq(11), RollbackMode::Move)
        .unwrap();
    let by_name = rollback::attic_bases_by_name(&path_of(&dir)).unwrap();
    let actual = rollback::attic_bases(&path_of(&dir));
    let mut by_name: Vec<u64> = by_name.into_iter().map(|(_, b)| b).collect();
    let mut actual: Vec<u64> = actual.into_iter().map(|(_, b)| b).collect();
    by_name.sort();
    actual.sort();
    assert_eq!(by_name, actual);
}

/// A rollback planned with an older entry to rebase, that entry removed before the rollback
/// runs: the rollback completes.
fn rebase_check(image: &MemDir, options: &StoreOptions) {
    let dir = image.deep_copy();
    let path = root(&dir);
    let intent = rollback::plan_rollback(&path, 11, 13, RollbackMode::Move, "t").unwrap();
    let id = intent.param("rebase").unwrap().to_string();
    rollback::attic_remove(&path, &id).unwrap();
    // (Its copies would recreate the entry's wal/ directory: drop them too, as a removal
    // before the plan would have.)
    let mut intent = intent;
    intent.params.retain(|(k, _)| k != "copies");
    intent.write(&path).unwrap();
    rollback::apply_rollback(&path, &intent, None).unwrap();
    let store = Store::open(&dir, options.clone()).unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), 11);
    assert_eq!(store.attic_list().unwrap().len(), 1);
    assert!(store.verify().unwrap().is_ok());
}

/// `store_id` (format spec 5.1 offset 100, 5.5) round-trips; a garbled one is caught by the
/// fixed part's CRC, and a zero one with a valid CRC is damage too (never a store without an
/// identity). Both GRAPH copies garbled: the open finds no valid GRAPH (maintenance mode).
#[test]
fn a_garbled_store_id_in_graph_is_damage() {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    add_person(&store, "ann").unwrap();
    let id = store.store_id().unwrap();
    assert_ne!(id, 0);
    store.close().unwrap();
    let root = root(&dir);
    let read = files::read_graph_file(&root).unwrap();
    assert!(read.repaired.is_none());
    assert_eq!(read.file.store_id, id);
    let bytes = read.file.encode();
    assert_eq!(GraphFile::decode(&bytes).unwrap(), read.file);
    assert_eq!(&bytes[100..116], &id.to_be_bytes());

    // A flipped bit of the id: the CRC of the fixed part.
    let mut flipped = bytes.clone();
    flipped[107] ^= 0x10;
    let err = GraphFile::decode(&flipped).unwrap_err();
    assert!(err.contains("checksum"), "{err}");
    // Zero with a recomputed CRC: still refused.
    let mut zero = bytes.clone();
    zero[100..116].fill(0);
    let crc = crate::persist::codec::crc(&zero[..252]);
    zero[252..256].copy_from_slice(&crc.to_le_bytes());
    let err = GraphFile::decode(&zero).unwrap_err();
    assert!(err.contains("store id"), "{err}");

    // One copy garbled: the other is used and the id kept; both: damage.
    dir.write_atomic(files::GRAPH_FILE, &zero).unwrap();
    let read = files::read_graph_file(&root).unwrap();
    assert!(read.repaired.is_some());
    assert_eq!(read.file.store_id, id);
    dir.write_atomic(files::GRAPH_COPY, &flipped).unwrap();
    assert!(files::read_graph_file(&root).is_err());
    let opened = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(opened.maintenance().is_some());
    // The scrub refuses it as no store (no valid GRAPH) or reports it.
    assert!(!matches!(Store::verify_dir(&dir), Ok(report) if report.is_ok()));
}

/// A [`MemDir`] whose snapshot directories (`snapshots/tmp-*`, where every checkpoint starts)
/// can be held (the checkpoint blocks until released) or refused (the checkpoint fails), and
/// that counts the checkpoints that got there.
#[derive(Debug)]
struct Gated {
    inner: Arc<dyn crate::persist::StoreDir>,
    /// `(held, failures left)`: `held` blocks, `failures left` (`usize::MAX`: always) refuses.
    state: std::sync::Mutex<(bool, usize)>,
    released: std::sync::Condvar,
    attempts: std::sync::atomic::AtomicUsize,
}

impl Gated {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemDir::new().into_store_dir(),
            state: std::sync::Mutex::new((false, 0)),
            released: std::sync::Condvar::new(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn set(&self, held: bool, failures: usize) {
        *self.state.lock().unwrap() = (held, failures);
        self.released.notify_all();
    }

    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::Acquire)
    }

    fn gate(&self, name: &str) -> std::io::Result<()> {
        if !name.contains("snapshots/tmp-") {
            return Ok(());
        }
        self.attempts.fetch_add(1, Ordering::AcqRel);
        let mut state = self.state.lock().unwrap();
        while state.0 {
            state = self.released.wait(state).unwrap();
        }
        if state.1 > 0 {
            if state.1 != usize::MAX {
                state.1 -= 1;
            }
            return Err(std::io::Error::other("injected snapshot failure"));
        }
        Ok(())
    }
}

impl crate::persist::StoreDir for Gated {
    fn location(&self) -> String {
        self.inner.location()
    }
    fn entry_kind(&self, name: &str) -> std::io::Result<Option<crate::persist::EntryKind>> {
        self.inner.entry_kind(name)
    }
    fn list(&self, dir: &str) -> std::io::Result<Vec<crate::persist::DirEntry>> {
        self.inner.list(dir)
    }
    fn file_len(&self, name: &str) -> std::io::Result<u64> {
        self.inner.file_len(name)
    }
    fn read(&self, name: &str, max: u64) -> std::io::Result<Vec<u8>> {
        self.inner.read(name, max)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> std::io::Result<()> {
        self.gate(name)?;
        self.inner.create_dir_all(name)
    }
    fn create(
        &self,
        name: &str,
        exclusive: bool,
    ) -> std::io::Result<Box<dyn crate::persist::StoreFile>> {
        self.inner.create(name, exclusive)
    }
    fn open_write(
        &self,
        name: &str,
        offset: u64,
    ) -> std::io::Result<Box<dyn crate::persist::StoreFile>> {
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
    fn lock(
        &self,
        kind: crate::persist::LockKind,
    ) -> std::io::Result<Option<crate::persist::DirLockGuard>> {
        self.inner.lock(kind)
    }
}

/// Text that LZ4 cannot shrink much: a commit of it is about `len` WAL bytes.
fn incompressible(seed: u64, len: usize) -> String {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (b'a' + ((x >> 33) % 26) as u8) as char
        })
        .collect()
}

/// Polls `done` for up to 20 s.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !done() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The automatic checkpoint's options: due after 1000 WAL bytes (a commit of 4000
/// [`incompressible`] bytes exceeds it on its own; a Checkpoint record does not).
fn auto_options() -> StoreOptions {
    StoreOptions::new().with_checkpoint_after_bytes(1000)
}

fn newest_snapshot(store: &Store) -> u64 {
    store
        .snapshots()
        .unwrap()
        .last()
        .map_or(0, |s| s.commit_seq)
}

fn gated_store(dir: &Arc<Gated>) -> Store {
    Store::create(
        dir.clone() as Arc<dyn crate::persist::StoreDir>,
        auto_options(),
    )
    .unwrap()
}

/// A commit at or above the threshold that lands while the automatic checkpoint runs is
/// covered by a further checkpoint right after it, without any further commit (before the
/// fix only the commit crossing the threshold woke the thread, and none crossed it again).
#[test]
fn a_large_commit_during_an_automatic_checkpoint_gets_its_own_checkpoint() {
    let dir = Gated::new();
    let store = gated_store(&dir);
    let before = dir.attempts();
    dir.set(true, 0);
    add_person(&store, &incompressible(1, 4000)).unwrap();
    eventually("the checkpoint to start", || dir.attempts() > before);
    add_person(&store, &incompressible(2, 4000)).unwrap();
    assert!(store.wal_bytes_since_checkpoint() >= 2000);
    dir.set(false, 0);
    // The snapshot appears before its checkpoint lowers the WAL counter: wait for both.
    eventually("a snapshot at commit 2", || {
        newest_snapshot(&store) == 2 && store.wal_bytes_since_checkpoint() < 1000
    });
    // The Checkpoint records (at commits 1 and 2) never moved the durable end back.
    assert_eq!(store.durable_end().commit_seq, 2);
    assert!(store.last_auto_checkpoint_error().unwrap().is_none());
}

/// A failed automatic checkpoint is retried at the next commit (which succeeds), and its
/// error is cleared.
#[test]
fn a_failed_automatic_checkpoint_is_retried_at_the_next_commit() {
    let dir = Gated::new();
    let store = gated_store(&dir);
    dir.set(false, 1);
    add_person(&store, &incompressible(1, 4000)).unwrap();
    eventually("the checkpoint to fail", || {
        store.last_auto_checkpoint_error().unwrap().is_some()
    });
    assert_eq!(newest_snapshot(&store), 0);
    // A small commit is enough: the level is still above the threshold.
    add_person(&store, "ann").unwrap();
    eventually("a snapshot at commit 2", || newest_snapshot(&store) == 2);
    eventually("the error to clear", || {
        store.last_auto_checkpoint_error().unwrap().is_none()
    });
}

/// A checkpoint that keeps failing is attempted at most once per commit: it never spins on
/// its own while nothing is committed.
#[test]
fn a_persistently_failing_automatic_checkpoint_does_not_spin() {
    let dir = Gated::new();
    let store = gated_store(&dir);
    let before = dir.attempts();
    dir.set(false, usize::MAX);
    add_person(&store, &incompressible(1, 4000)).unwrap();
    eventually("the checkpoint to fail", || {
        store.last_auto_checkpoint_error().unwrap().is_some()
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    let after_first = dir.attempts();
    assert_eq!(after_first - before, 1, "one commit, one attempt");
    for name in ["ann", "bob", "cy"] {
        add_person(&store, name).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let after_three = dir.attempts();
    assert!(
        (after_first + 1..=after_first + 3).contains(&after_three),
        "three commits retry 1 to 3 times, got {}",
        after_three - after_first
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(dir.attempts(), after_three, "no attempt without a commit");
    assert_eq!(newest_snapshot(&store), 0);
    // Once the cause is gone, the next commit checkpoints.
    dir.set(false, 0);
    add_person(&store, "dee").unwrap();
    eventually("a snapshot at commit 5", || newest_snapshot(&store) == 5);
}

/// A [`MemDir`] whose reads fail from the `n`-th one on after it is armed (see [`Self::only`]): a read error at any
/// point of an operation (the planning, the file changes, the reload of the graph).
#[derive(Debug)]
struct FailingReads {
    inner: Arc<dyn crate::persist::StoreDir>,
    /// Reads left before they fail; `usize::MAX`: disarmed.
    left: std::sync::atomic::AtomicUsize,
    /// Only reads of files with this last name component count and fail (`None`: every read).
    only: Option<&'static str>,
    /// The reads that counted so far.
    seen: std::sync::atomic::AtomicUsize,
}

impl FailingReads {
    fn new(inner: &MemDir) -> Arc<Self> {
        Arc::new(Self {
            inner: inner.clone().into_store_dir(),
            left: std::sync::atomic::AtomicUsize::new(usize::MAX),
            only: None,
            seen: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Counts only the reads of files named `name` (the last name component); once armed, only
    /// the `n`-th of them fails (the later ones succeed again).
    fn only(inner: &MemDir, name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            inner: inner.clone().into_store_dir(),
            left: std::sync::atomic::AtomicUsize::new(usize::MAX),
            only: Some(name),
            seen: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn arm(&self, reads: usize) {
        self.left.store(reads, Ordering::Release);
    }

    fn gate(&self, name: &str) -> std::io::Result<()> {
        if self
            .only
            .is_some_and(|only| name.rsplit('/').next() != Some(only))
        {
            return Ok(());
        }
        self.seen.fetch_add(1, Ordering::AcqRel);
        let left = self.left.load(Ordering::Acquire);
        if left == usize::MAX {
            return Ok(());
        }
        if left == 0 {
            if self.only.is_some() {
                self.left.store(usize::MAX, Ordering::Release);
            }
            return Err(std::io::Error::other("injected read failure"));
        }
        self.left.store(left - 1, Ordering::Release);
        Ok(())
    }
}

impl crate::persist::StoreDir for FailingReads {
    fn location(&self) -> String {
        self.inner.location()
    }
    fn entry_kind(&self, name: &str) -> std::io::Result<Option<crate::persist::EntryKind>> {
        self.inner.entry_kind(name)
    }
    fn list(&self, dir: &str) -> std::io::Result<Vec<crate::persist::DirEntry>> {
        self.inner.list(dir)
    }
    fn file_len(&self, name: &str) -> std::io::Result<u64> {
        self.inner.file_len(name)
    }
    fn read(&self, name: &str, max: u64) -> std::io::Result<Vec<u8>> {
        self.gate(name)?;
        self.inner.read(name, max)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        self.gate(name)?;
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        self.gate(name)?;
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> std::io::Result<()> {
        self.inner.create_dir_all(name)
    }
    fn create(
        &self,
        name: &str,
        exclusive: bool,
    ) -> std::io::Result<Box<dyn crate::persist::StoreFile>> {
        self.inner.create(name, exclusive)
    }
    fn open_write(
        &self,
        name: &str,
        offset: u64,
    ) -> std::io::Result<Box<dyn crate::persist::StoreFile>> {
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
    fn lock(
        &self,
        kind: crate::persist::LockKind,
    ) -> std::io::Result<Option<crate::persist::DirLockGuard>> {
        self.inner.lock(kind)
    }
}

/// Schema + saved query (1), ann (2), bob (3), checkpoint, cy (4), dan (5); closed.
fn schema_history_image() -> MemDir {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    store
        .graph()
        .write()
        .transaction(|g| {
            g.set_schema(
                crate::graph::schema::GraphSchema::from_json(
                    r#"{"vertices": {"person": {"schema": {"type": "object"}}}}"#,
                )
                .unwrap(),
            )?;
            g.set_definition(crate::graph::definition::Definition::query(
                "people",
                crate::graph::definition::QueryDefinition::new("fn people() { g.v().count() }"),
            ))
        })
        .unwrap();
    add_person(&store, "ann").unwrap();
    add_person(&store, "bob").unwrap();
    store.checkpoint(None).unwrap();
    add_person(&store, "cy").unwrap();
    add_person(&store, "dan").unwrap();
    store.close().unwrap();
    dir
}

fn is_empty(graph: &TraversalGraph) -> bool {
    graph.vertex_count() == 0
        && graph.edge_count() == 0
        && graph.get_schema().unwrap().is_none()
        && graph.definitions().unwrap().next().is_none()
}

/// A read failing at every point of an in-place rollback and of an attic restore: when the
/// reload of the graph fails, the graph in memory is EMPTY (never a partial state), the error
/// says so (`ReloadFailed`, with its help), the store is stopped (writes refused, named in
/// `info`), and a reopen completes the change from the files. A failure before the reload
/// leaves the graph as it was.
#[test]
fn a_failed_reload_leaves_an_empty_graph_a_stopped_store_and_a_clear_error() {
    use crate::error::DiagnosticError;
    let all = ["ann", "bob", "cy", "dan"];
    let image = schema_history_image();
    // The attic entry the restore case starts from.
    let rolled_image = image.deep_copy();
    let attic_id = {
        let store = Store::open(&rolled_image, StoreOptions::new()).unwrap();
        let id = store
            .rollback_to(RecoveryTarget::CommitSeq(2), RollbackMode::Move)
            .unwrap()
            .attic
            .unwrap();
        store.close().unwrap();
        id
    };
    for restore in [false, true] {
        let (start, done, other): (&MemDir, &[&str], &[&str]) = if restore {
            (&rolled_image, &all, &["ann"])
        } else {
            (&image, &["ann"], &all)
        };
        let mut reload_failures = 0;
        for n in 0.. {
            let context = format!("restore {restore}, read {n}");
            let dir = start.deep_copy();
            let failing = FailingReads::new(&dir);
            let store = Store::open(
                failing.clone() as Arc<dyn crate::persist::StoreDir>,
                StoreOptions::new(),
            )
            .unwrap();
            failing.arm(n);
            let result = if restore {
                store.attic_restore(&attic_id).map(|_| ())
            } else {
                store
                    .rollback_to(RecoveryTarget::CommitSeq(2), RollbackMode::Move)
                    .map(|_| ())
            };
            failing.arm(usize::MAX);
            let err = match result {
                Ok(()) => {
                    assert_eq!(names_of(&store.graph().read()), done, "{context}");
                    break;
                }
                Err(err) => err,
            };
            let reload_failed = matches!(
                &err,
                PersistError::Store {
                    cause: StoreFailure::ReloadFailed,
                    ..
                }
            );
            if reload_failed {
                reload_failures += 1;
                assert!(is_empty(&store.graph().read()), "{context}");
                let text = err.to_string();
                let change = if restore { "restore" } else { "rollback" };
                assert!(
                    text.contains(&format!(
                        "the reload of the graph after the {change} failed"
                    )),
                    "{context}: {text}"
                );
                // The real cause is in the text, never a follow-on failure of it (such as
                // starting a WAL segment over one whose header could not be read).
                assert!(text.contains("injected read failure"), "{context}: {text}");
                assert!(
                    text.contains("the graph in memory was emptied, the store is stopped"),
                    "{context}: {text}"
                );
                let help = err.help().unwrap_or_default();
                assert!(
                    help.contains("Close the store and open it again (restart the server"),
                    "{context}: {help}"
                );
                let stopped = store.stopped_reason().unwrap().expect("stopped");
                assert!(stopped.contains("was emptied"), "{context}: {stopped}");
                assert_eq!(store.info().unwrap().stopped, Some(stopped), "{context}");
                // Writes are refused: commits (the journal is detached), marks, checkpoints.
                assert!(add_person(&store, "eve").is_err(), "{context}");
                assert!(is_empty(&store.graph().read()), "{context}");
                assert!(
                    matches!(
                        store.mark("m"),
                        Err(PersistError::Store {
                            cause: StoreFailure::Stopped,
                            ..
                        })
                    ),
                    "{context}"
                );
                assert!(store.checkpoint(None).is_err(), "{context}");
            } else if store.stopped_reason().unwrap().is_some() {
                // The files changed part-way: the graph keeps the state before the change.
                assert_eq!(names_of(&store.graph().read()), other, "{context}: {err}");
                assert!(add_person(&store, "eve").is_err(), "{context}");
            } else {
                // Refused before anything changed.
                assert_eq!(names_of(&store.graph().read()), other, "{context}: {err}");
            }
            drop(store);
            // The reopen completes the change (the INTENT) or finds it undone.
            let store = Store::open(&dir, StoreOptions::new())
                .unwrap_or_else(|e| panic!("{context}: reopen: {e}"));
            {
                let graph = store.graph().read();
                let names = names_of(&graph);
                if reload_failed {
                    // The files were changed completely before the reload.
                    assert_eq!(names, done, "{context}");
                } else {
                    assert!(names == done || names == other, "{context}: {names:?}");
                }
                assert!(graph.get_schema().unwrap().is_some(), "{context}");
                assert_eq!(graph.definitions().unwrap().count(), 1, "{context}");
            }
            assert!(store.verify().unwrap().is_ok(), "{context}");
        }
        assert!(
            reload_failures > 0,
            "restore {restore}: no read of the reload failed"
        );
    }
}

/// The open's check for critical unknown segment kinds (format spec 16) reads the newest
/// snapshot's manifest again after the load: a read failing there fails the open with that
/// read error, it is never taken as "nothing critical". Every other failing manifest read
/// fails the open too. The forward-version guard's header reads (format spec 15) come first and
/// leave an unreadable header to the operation's own checks, so they are not armed here.
#[test]
fn a_failed_manifest_read_after_the_load_fails_the_open() {
    let image = schema_history_image();
    // How many manifest reads the forward-version guard makes before the open proper.
    let guard = FailingReads::only(&image, snapshot_dir::MANIFEST_FILE);
    files::refuse_other_version(&files::DirPath::root(
        guard.clone() as Arc<dyn crate::persist::StoreDir>
    ))
    .unwrap();
    let guard_reads = guard.seen.load(Ordering::Acquire);
    // How many manifest reads an open makes.
    let counting = FailingReads::only(&image, snapshot_dir::MANIFEST_FILE);
    let store = Store::open(
        counting.clone() as Arc<dyn crate::persist::StoreDir>,
        StoreOptions::new(),
    )
    .unwrap();
    let reads = counting.seen.load(Ordering::Acquire);
    store.close().unwrap();
    assert!(
        reads >= 2,
        "the load and the critical check read it: {reads}"
    );
    assert!(guard_reads < reads, "{guard_reads} of {reads}");
    for n in guard_reads..reads {
        let dir = image.deep_copy();
        let failing = FailingReads::only(&dir, snapshot_dir::MANIFEST_FILE);
        failing.arm(n);
        let result = Store::open(
            failing.clone() as Arc<dyn crate::persist::StoreDir>,
            StoreOptions::new(),
        );
        match result {
            // Only the best-effort check of the manifest's two fixed copies may let the open
            // go on, and then it says so.
            Ok(store) => {
                let warnings = &store.open_report().warnings;
                assert!(
                    warnings
                        .iter()
                        .any(|w| w.contains("could not be read again")
                            && w.contains("injected read failure")),
                    "manifest read {n} of {reads} failed silently: {warnings:?}"
                );
            }
            Err(err) => {
                let err = err.to_string();
                assert!(err.contains("injected read failure"), "read {n}: {err}");
            }
        }
        // Nothing was lost: the next open works.
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(names_of(&store.graph().read()), ["ann", "bob", "cy", "dan"]);
    }
}

/// Poisons `lock`: a thread panics while it holds it (the panic is caught by the join).
fn poison<T: Send>(lock: &std::sync::Mutex<T>) {
    std::thread::scope(|scope| {
        let joined = scope
            .spawn(|| {
                let _guard = lock.lock();
                panic!("poisoning the lock on purpose");
            })
            .join();
        assert!(joined.is_err());
    });
    assert!(lock.is_poisoned());
}

/// A store lock poisoned by a panicking thread is `StoreFailure::LockPoisoned` from every
/// accessor that reads through it: formerly "writable" (`None`), an empty damage list, the
/// default damage policy or the store id 0.
#[test]
fn a_poisoned_store_lock_is_an_error_not_a_default() {
    let store = Store::create(MemDir::new(), StoreOptions::new()).unwrap();
    let poisoned = |err: PersistError| {
        assert!(
            matches!(
                err,
                PersistError::Store {
                    cause: StoreFailure::LockPoisoned,
                    ..
                }
            ),
            "{err:?}"
        );
    };
    assert_eq!(store.read_only_reason().unwrap(), None);
    poison(&store.core.shared.read_only);
    poisoned(store.read_only_reason().unwrap_err());
    // A store that cannot say whether it is read-only writes nothing.
    assert!(store.core.writes_nothing());

    assert!(store.damage_found().unwrap().is_empty());
    poison(&store.core.shared.found);
    poisoned(store.damage_found().unwrap_err());

    assert_eq!(store.stopped_reason().unwrap(), None);
    poison(&store.core.shared.stopped);
    poisoned(store.stopped_reason().unwrap_err());

    assert!(store.store_id().unwrap() != 0);
    poison(&store.core.shared.graph_file);
    poisoned(store.store_id().unwrap_err());
    poisoned(store.damage_policy().unwrap_err());

    assert!(store.last_auto_checkpoint_error().unwrap().is_none());
    poison(&store.core.shared.last_auto_error);
    poisoned(store.last_auto_checkpoint_error().unwrap_err());
}

/// The copy `last_auto_checkpoint_error` keeps of an error that `run_due_checkpoint` also
/// returns: the same variant and text, a graph error as `AutoCheckpointFailed` with its text.
#[test]
fn a_duplicated_persist_error_keeps_its_variant_and_text() {
    let io = PersistError::io(
        "writing a snapshot",
        std::io::Error::new(std::io::ErrorKind::StorageFull, "disk full"),
    );
    let copy = io.duplicate();
    assert!(
        matches!(&copy, PersistError::Io { source, .. } if source.kind() == std::io::ErrorKind::StorageFull),
        "{copy:?}"
    );
    assert_eq!(copy.to_string(), io.to_string());
    let refused = PersistError::store("write a checkpoint", StoreFailure::NameTaken, "taken");
    assert_eq!(refused.duplicate().to_string(), refused.to_string());
    let graph = PersistError::Graph(crate::error::GraphError::unsupported("snapshots"));
    let copy = graph.duplicate();
    assert!(
        matches!(
            &copy,
            PersistError::Store {
                cause: StoreFailure::AutoCheckpointFailed,
                ..
            }
        ),
        "{copy:?}"
    );
    assert!(
        copy.to_string().contains("does not support snapshots"),
        "{copy}"
    );
}
