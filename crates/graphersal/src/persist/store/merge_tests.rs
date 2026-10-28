//! Crate tests of the merge checkpoint (`store::merge`, format spec 12.3): over random histories
//! the merged snapshot holds byte for byte the elements the streaming encoder writes for the
//! live graph (the graph a load of the base plus a replay of the WAL builds), with the same
//! identity, position, sequences, counts, schema and catalog; untouched segments are reused by
//! reference; a lineage change rewrites everything; the bound falls back to the encoder; the
//! delta is all the merge holds.

use compact_str::CompactString;

use super::files::snapshot_name;
use super::snapshot_dir::{MANIFEST_FILE, SCHEMA_FILE};
use super::*;
use crate::graph::definition::{Definition, DefinitionFlags, DefinitionKind, QueryDefinition};
use crate::graph::schema::GraphSchema;
use crate::graph::{GraphStorage, Transactional, TraversalGraph};
use crate::persist::MemDir;
use crate::persist::codec::Codec;
use crate::persist::encoder::tests::{fingerprint, generated_graph};
use crate::persist::manifest::SegmentEntry;
use crate::persist::snapshot::{ReadOptions, read_snapshot, write_packed};
use crate::value::number::Number;
use crate::value::{ElementProperty, PropertyMap};

/// SplitMix64.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn value(rng: &mut Rng) -> ElementProperty {
    match rng.below(6) {
        0 => ElementProperty::Null,
        1 => ElementProperty::Boolean(rng.below(2) == 0),
        2 => ElementProperty::Number(Number::from_i64(rng.next() as i64 % 1000)),
        3 => ElementProperty::String(format!("text {}", rng.next() % 997).into()),
        4 => ElementProperty::Uuid(u128::from(rng.next())),
        _ => ElementProperty::Array(vec![
            ElementProperty::Number(Number::from_i64(1));
            rng.below(3)
        ]),
    }
}

const LABELS: [&str; 4] = ["person", "software", "city", "ç-label"];

fn some_vertex(graph: &TraversalGraph, rng: &mut Rng) -> Option<crate::graph::VertexHandle> {
    let count = graph.vertex_count();
    if count == 0 {
        return None;
    }
    graph.vertices().unwrap().nth(rng.below(count))
}

fn some_edge(graph: &TraversalGraph, rng: &mut Rng) -> Option<crate::graph::EdgeHandle> {
    let count = graph.edge_count();
    if count == 0 {
        return None;
    }
    graph.edges().unwrap().nth(rng.below(count))
}

/// One random change in a unit of its own (a change that fails, a schema violation for
/// example, leaves nothing and writes no WAL record).
pub(super) fn random_change(graph: &mut TraversalGraph, rng: &mut Rng, round: usize) {
    let op = rng.below(14);
    let _ = graph.transaction(|g| -> Result<(), crate::error::GraphError> {
        match op {
            0 | 1 => {
                // A new id, or (sometimes) one that was dropped before.
                let id = if rng.below(4) == 0 {
                    format!("n{}", rng.below(round + 1))
                } else {
                    format!("n{round}-{}", rng.next() % 1000)
                };
                let labels: Vec<CompactString> = (0..rng.below(3))
                    .map(|i| CompactString::from(LABELS[(i + rng.below(4)) % 4]))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let mut properties = PropertyMap::default();
                for _ in 0..rng.below(4) {
                    properties.insert(format!("p{}", rng.below(8)), value(rng));
                }
                g.insert_vertex(Some(id.into()), labels, properties)?;
            }
            2 => {
                if let (Some(from), Some(to)) = (some_vertex(g, rng), some_vertex(g, rng)) {
                    let label = (rng.below(3) > 0).then(|| CompactString::from("knows"));
                    let mut properties = PropertyMap::default();
                    if rng.below(2) == 0 {
                        properties.insert("w", value(rng));
                    }
                    let id = format!("e{round}-{}", rng.next() % 1000);
                    g.insert_edge(Some(id.into()), label, from, to, properties)?;
                }
            }
            3 => {
                if let Some(v) = some_vertex(g, rng) {
                    g.drop_vertex(v)?;
                }
            }
            4 => {
                if let Some(e) = some_edge(g, rng) {
                    g.drop_edge(e)?;
                }
            }
            5 | 6 => {
                if let Some(v) = some_vertex(g, rng) {
                    let key = format!("p{}", rng.below(8));
                    g.set_vertex_property(v, key.into(), value(rng))?;
                }
            }
            7 => {
                if let Some(v) = some_vertex(g, rng) {
                    let key = format!("p{}", rng.below(8));
                    g.drop_vertex_property(&key, v)?;
                }
            }
            8 => {
                if let Some(e) = some_edge(g, rng) {
                    g.set_edge_property(e, "w".into(), value(rng))?;
                }
            }
            9 => {
                if let Some(v) = some_vertex(g, rng) {
                    g.add_vertex_label(v, LABELS[rng.below(4)].into())?;
                }
            }
            10 => {
                if let Some(v) = some_vertex(g, rng) {
                    g.remove_vertex_label(v, LABELS[rng.below(4)])?;
                }
            }
            11 => {
                let schema = if rng.below(2) == 0 {
                    GraphSchema::default()
                } else {
                    GraphSchema::from_json(&format!(
                        r#"{{"mode": "none", "meta": {{"round": {round}}}, "vertices": {{"person": {{"schema": {{"type": "object"}}}}}}}}"#
                    ))
                    .expect("schema")
                };
                g.set_schema(schema)?;
            }
            12 => {
                let name = format!("q{}", rng.below(3));
                if rng.below(3) == 0 {
                    g.remove_definition(DefinitionKind::QUERY, &name)?;
                } else {
                    let source = format!("fn {name}() {{ g.v().count() + {round} }}");
                    g.set_definition(Definition::query(name, QueryDefinition::new(source)))?;
                }
            }
            _ => {
                // A definition of a kind this version does not know: kept byte for byte.
                let name = format!("u{}", rng.below(2));
                let payload = vec![1, 1, b'k', 3, (round % 60) as u8];
                g.set_definition(Definition::opaque(
                    DefinitionKind::from_code(200),
                    DefinitionFlags::NONE,
                    name,
                    payload,
                ))?;
            }
        }
        Ok(())
    });
}

fn options(codec: Codec, chunk: u32, segment: u64) -> StoreOptions {
    StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_codec(codec)
        .with_chunk_bytes(chunk)
        .with_snapshot_segment_bytes(segment)
}

/// The fingerprint of a graph's elements, independent of how a snapshot of it is segmented:
/// the graph written with one fixed set of options.
fn elements(graph: &TraversalGraph) -> u32 {
    let mut packed = Vec::new();
    write_packed(
        graph,
        &mut packed,
        &WriteOptions::new()
            .with_codec(Codec::None)
            .with_chunk_bytes(512)
            .with_segment_bytes(1 << 30),
    )
    .unwrap();
    fingerprint(&packed)
}

/// The newest snapshot of `store`, loaded alone.
fn newest_snapshot(store: &Store) -> (SnapshotInfo, TraversalGraph, Vec<u8>) {
    let mut packed = Vec::new();
    let info = store.export_packed(None, &mut packed).unwrap();
    let graph = read_snapshot(packed.as_slice(), &ReadOptions::new()).unwrap();
    (info, graph, packed)
}

/// Everything a snapshot of `graph` records except its creation time, memory estimate and
/// segmentation.
fn identity(graph: &TraversalGraph) -> String {
    let definitions = crate::persist::definitions::records_of(graph.definitions.iter()).unwrap();
    format!(
        "{:x} {} {} {} {} {} {} {} {:?}",
        graph.graph_id(),
        graph.last_commit_seq(),
        graph.last_committed_at(),
        graph.node_sequence,
        graph.edge_sequence,
        graph.vertex_count(),
        graph.edge_count(),
        graph.schema.read().to_json(),
        definitions
    )
}

/// Merges, then checks the newest snapshot against the live graph.
fn check_merge(store: &Store, label: &str) -> CheckpointReport {
    let report = store.core.shared.checkpoint(None, true).unwrap();
    assert!(report.written, "{label}: nothing written");
    let live = store.graph().read();
    let (info, loaded, _) = newest_snapshot(store);
    assert_eq!(info.commit_seq, live.last_commit_seq(), "{label}");
    assert_eq!(identity(&loaded), identity(&live), "{label}");
    assert_eq!(elements(&loaded), elements(&live), "{label}");
    report
}

/// Random histories on generated graphs: every merge equals the live graph exactly, also a
/// merge onto a merged (incremental) snapshot, and the store verifies and reopens to it.
#[test]
fn merged_snapshots_equal_the_live_graph_over_random_histories() {
    let mut merged = 0;
    for seed in 0..6u64 {
        for (codec, chunk, segment) in [
            (Codec::None, 512, 1 << 30),
            (Codec::Lz4, 700, 4000),
            (Codec::None, 64, 1),
        ] {
            let dir = MemDir::new();
            let store = Store::create_with(
                &dir,
                generated_graph(120, seed),
                options(codec, chunk, segment),
            )
            .unwrap();
            let mut rng = Rng(seed * 31 + u64::from(chunk));
            for checkpoint in 0..4 {
                for round in 0..(5 + 20 * checkpoint) {
                    random_change(
                        &mut store.graph().write(),
                        &mut rng,
                        round + 100 * checkpoint,
                    );
                }
                let label =
                    format!("seed {seed} {codec:?} {chunk}/{segment} checkpoint {checkpoint}");
                let report = check_merge(&store, &label);
                merged += usize::from(report.merged);
                assert!(store.verify().unwrap().is_ok(), "{label}");
            }
            let live = elements(&store.graph().read());
            drop(store);
            let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
            assert_eq!(elements(&reopened.graph().read()), live);
        }
    }
    assert!(merged > 60, "only {merged} merges");
}

/// One segment of each kind and every one touched: the merged snapshot is byte for byte what
/// the streaming encoder writes for the live graph.
#[test]
fn a_merge_that_rewrites_every_segment_writes_the_encoders_bytes() {
    for codec in [Codec::None, Codec::Lz4] {
        for seed in 0..4u64 {
            let store = Store::create_with(
                MemDir::new(),
                generated_graph(200, seed),
                options(codec, 700, 1 << 30),
            )
            .unwrap();
            let mut rng = Rng(seed);
            for round in 0..40 {
                random_change(&mut store.graph().write(), &mut rng, round);
            }
            check_merge(&store, "rewrite");
            let (_, _, packed) = newest_snapshot(&store);
            let mut expected = Vec::new();
            write_packed(
                &*store.graph().read(),
                &mut expected,
                &WriteOptions::new()
                    .with_codec(codec)
                    .with_chunk_bytes(700)
                    .with_segment_bytes(1 << 30),
            )
            .unwrap();
            assert_eq!(
                fingerprint(&packed),
                fingerprint(&expected),
                "{codec:?} {seed}"
            );
        }
    }
}

/// The files of every element segment of snapshot `seq` (relative to the store).
fn segment_files(dir: &MemDir, seq: u64) -> Vec<String> {
    let root = DirPath::root(dir.clone().into_store_dir());
    let path = root
        .join(super::files::SNAPSHOTS_DIR)
        .join(snapshot_name(seq));
    let (_, manifest) = read_manifest(&path).unwrap();
    manifest
        .segments
        .iter()
        .map(|s| {
            super::snapshot_dir::segment_path(&path, &s.path)
                .unwrap()
                .relative_to(&root)
        })
        .collect()
}

/// The donor rule (format spec 12.3): the newest snapshot shares no file with the previous one.
fn assert_donors(dir: &MemDir, label: &str) {
    let root = DirPath::root(dir.clone().into_store_dir());
    let snapshots = list_snapshots(&root).unwrap();
    assert!(
        maintenance::files_without_donor(&root, &snapshots)
            .unwrap()
            .is_empty(),
        "{label}"
    );
    if let [.., (previous, _), (newest, _)] = snapshots.as_slice() {
        let older = segment_files(dir, *previous);
        assert!(
            segment_files(dir, *newest)
                .iter()
                .all(|file| !older.contains(file)),
            "{label}"
        );
    }
}

/// A change at the end of the id range touches one segment. The first merge after the store's
/// only snapshot has no twin for the others: it copies them (the base must stay their
/// donor). The next merge references those twins in the snapshot before its base: the
/// manifest is incremental, its packed export self-contained, and the references survive a
/// prune of that snapshot (its files stay in a directory without a manifest). Unchanged ranges
/// alternate between the two copies from then on.
#[test]
fn untouched_segments_are_reused_by_reference() {
    let dir = MemDir::new();
    let store = Store::create_with(
        &dir,
        generated_graph(400, 3),
        options(Codec::Lz4, 1024, 8 << 10),
    )
    .unwrap();
    let base_segments = store.snapshots().unwrap()[0].segment_count;
    assert!(base_segments > 6, "{base_segments}");
    store
        .graph()
        .write()
        .transaction(|g| {
            g.insert_vertex(
                Some("~last".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .map(|_| ())
        })
        .unwrap();
    let report = check_merge(&store, "copy");
    assert!(report.merged);
    assert_eq!(report.segments_written, 1, "{report:?}");
    assert_eq!(report.segments_reused, 0, "{report:?}");
    assert_eq!(report.segments_copied, base_segments as usize - 1);
    assert!(report.bytes_copied > 0);
    assert_donors(&dir, "copy");
    let root = DirPath::root(dir.clone().into_store_dir());
    let (seq, path) = list_snapshots(&root).unwrap().pop().unwrap();
    let (_, manifest) = read_manifest(&path).unwrap();
    assert_eq!(
        manifest.fixed.flags & crate::persist::manifest::FLAG_INCREMENTAL,
        0
    );
    store
        .graph()
        .write()
        .transaction(|g| {
            g.insert_vertex(Some("~more".into()), vec![], PropertyMap::default())
                .map(|_| ())
        })
        .unwrap();
    let report = check_merge(&store, "reuse");
    assert_eq!(report.segments_reused, base_segments as usize - 1);
    assert_eq!(report.segments_copied, 0, "{report:?}");
    assert_donors(&dir, "reuse");
    let (_, path) = list_snapshots(&root).unwrap().pop().unwrap();
    let (_, manifest) = read_manifest(&path).unwrap();
    let first = list_snapshots(&root).unwrap()[0].0;
    assert!(
        manifest
            .segments
            .iter()
            .filter(|s| s.path.contains('/'))
            .all(|s| s.path.starts_with(&snapshot_name(first)))
    );
    assert_eq!(
        manifest.fixed.flags & crate::persist::manifest::FLAG_INCREMENTAL,
        1
    );
    assert_eq!(
        manifest
            .segments
            .iter()
            .filter(|s| s.path.contains('/'))
            .count(),
        report.segments_reused
    );
    // The packed export names only its own files, and a store imports it.
    let (_, _, packed) = newest_snapshot(&store);
    let exported = crate::persist::snapshot::PackedReader::open(packed.as_slice()).unwrap();
    assert!(
        exported
            .manifest
            .segments
            .iter()
            .all(|s| !s.path.contains('/'))
    );
    assert_eq!(exported.manifest.fixed.flags & 1, 0);
    let imported =
        Store::create_from_packed(MemDir::new(), packed.as_slice(), StoreOptions::new()).unwrap();
    assert_eq!(
        elements(&imported.graph().read()),
        elements(&store.graph().read())
    );

    // Another merge onto the incremental snapshot references the copies of the snapshot
    // before it (the files alternate).
    store
        .graph()
        .write()
        .transaction(|g| {
            g.insert_vertex(Some("~most".into()), vec![], PropertyMap::default())
                .map(|_| ())
        })
        .unwrap();
    let again = check_merge(&store, "reuse twice");
    assert_eq!(again.segments_reused, base_segments as usize - 1);
    assert_eq!(again.segments_copied, 0);
    assert_donors(&dir, "reuse twice");
    let (_, path) = list_snapshots(&root).unwrap().pop().unwrap();
    let (_, manifest) = read_manifest(&path).unwrap();
    assert!(
        manifest
            .segments
            .iter()
            .filter(|s| s.path.contains('/'))
            .all(|s| s.path.starts_with(&snapshot_name(seq)))
    );
    // Prune: the first snapshot goes, the files the third one uses stay.
    let pruned = store.prune(seq).unwrap();
    assert_eq!(pruned.snapshots_removed.len(), 1, "{pruned:?}");
    assert_eq!(
        pruned.files_retained.len(),
        base_segments as usize - 1,
        "{pruned:?}"
    );
    assert_eq!(store.snapshots().unwrap().len(), 3);
    assert!(store.verify().unwrap().is_ok());
    let live = elements(&store.graph().read());
    drop(store);
    let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(elements(&reopened.graph().read()), live);
    // Once no snapshot uses them any more, a prune removes them.
    for i in 0..3 {
        reopened
            .graph()
            .write()
            .transaction(|g| {
                for v in g.vertices().unwrap().collect::<Vec<_>>() {
                    g.set_vertex_property(v, "touch".into(), ElementProperty::Number(i.into()))?;
                }
                for e in g.edges().unwrap().collect::<Vec<_>>() {
                    g.set_edge_property(e, "touch".into(), ElementProperty::Number(i.into()))?;
                }
                Ok::<(), crate::error::GraphError>(())
            })
            .unwrap();
        reopened.checkpoint(None).unwrap();
    }
    let last = reopened.snapshots().unwrap().pop().unwrap().commit_seq;
    let pruned = reopened.prune(last).unwrap();
    assert!(pruned.files_retained.is_empty(), "{pruned:?}");
    assert!(super::files::list_holders(&root).unwrap().is_empty());
    assert!(reopened.verify().unwrap().is_ok());
}

/// After an in-place rollback the base is of the old lineage: its segments carry another
/// graph_id, so the merge rewrites them all; the result equals the live graph.
#[test]
fn a_merge_after_a_rollback_rewrites_every_segment() {
    let dir = MemDir::new();
    let store = Store::create_with(
        &dir,
        generated_graph(150, 9),
        options(Codec::Lz4, 600, 3000),
    )
    .unwrap();
    let mut rng = Rng(9);
    for round in 0..20 {
        random_change(&mut store.graph().write(), &mut rng, round);
    }
    check_merge(&store, "before the rollback");
    let target = store.graph().read().last_commit_seq();
    for round in 20..40 {
        random_change(&mut store.graph().write(), &mut rng, round);
    }
    check_merge(&store, "after the rollback target");
    store
        .rollback_to(
            crate::persist::RecoveryTarget::CommitSeq(target),
            RollbackMode::Move,
        )
        .unwrap();
    for round in 40..60 {
        random_change(&mut store.graph().write(), &mut rng, round);
    }
    let report = check_merge(&store, "new lineage");
    assert!(report.merged);
    assert_eq!(report.segments_reused, 0);
    assert!(store.verify().unwrap().is_ok());
}

/// Beyond the bound the loaded graph is encoded instead (also with the bound 0); the result is
/// the same graph.
#[test]
fn a_delta_beyond_the_bound_falls_back_to_the_encoder() {
    for bound in [0, 1] {
        let store = Store::create_with(
            MemDir::new(),
            generated_graph(80, 2),
            options(Codec::Lz4, 600, 3000).with_checkpoint_merge_bytes(bound),
        )
        .unwrap();
        let mut rng = Rng(2);
        for round in 0..15 {
            random_change(&mut store.graph().write(), &mut rng, round);
        }
        let report = check_merge(&store, "bound");
        assert!(!report.merged);
        assert_eq!(report.segments_reused, 0);
    }
}

/// The merge holds the delta, never the graph: a few changes on a large base build a delta
/// graph a small fraction of the graph's size.
#[test]
fn the_merge_holds_the_delta_not_the_graph() {
    let dir = MemDir::new();
    let store = Store::create_with(
        &dir,
        generated_graph(3000, 4),
        options(Codec::Lz4, 4096, 64 << 10),
    )
    .unwrap();
    let mut rng = Rng(4);
    for round in 0..10 {
        random_change(&mut store.graph().write(), &mut rng, round);
    }
    let graph_bytes = crate::graph::memory::compute(&store.graph().read()).total_bytes();
    let graph_file = store.core.shared.graph_file.lock().unwrap().clone();
    let write = checkpoint_write_options(
        &store.core.shared.options,
        store.core.shared.chunk_bytes,
        "",
    );
    let merged = merge::merge_checkpoint(&merge::MergeRequest {
        dir: &store.core.shared.dir,
        graph_file: &graph_file,
        durable: Some(store.durable_end()),
        write: &write,
        read: &ReadOptions::new(),
        max_wal_bytes: u64::MAX,
    })
    .unwrap();
    let merge::Merged::Written { stats, .. } = merged else {
        panic!("no merge");
    };
    assert!(stats.commits > 0);
    assert!(
        stats.delta_bytes * 20 < graph_bytes,
        "delta {} of {graph_bytes}",
        stats.delta_bytes
    );
    // The store's only snapshot has no twins: the unchanged segments are copied.
    assert!(stats.segments_copied > 0, "{stats:?}");
    assert_eq!(stats.segments_reused, 0, "{stats:?}");
}

/// The manifest's estimate of a merged snapshot stays close to what the encoder records for
/// the same graph.
#[test]
fn the_merged_memory_estimate_tracks_the_graph() {
    let store = Store::create_with(
        MemDir::new(),
        generated_graph(500, 6),
        options(Codec::Lz4, 2048, 16 << 10),
    )
    .unwrap();
    let mut rng = Rng(6);
    for round in 0..200 {
        random_change(&mut store.graph().write(), &mut rng, round);
    }
    check_merge(&store, "estimate");
    let (info, loaded, _) = newest_snapshot(&store);
    let fresh = crate::graph::memory::compute(&loaded).total_bytes();
    let ratio = info.memory_bytes as f64 / fresh as f64;
    assert!(
        (0.5..2.0).contains(&ratio),
        "{} vs {fresh}",
        info.memory_bytes
    );
}

/// The vertex segment rows and chunk rows of the newest snapshot: `(first, last)` ids.
fn vertex_ranges(dir: &MemDir) -> (RangeList, RangeList) {
    let root = DirPath::root(dir.clone().into_store_dir());
    let (_, path) = list_snapshots(&root).unwrap().pop().unwrap();
    let (_, manifest) = read_manifest(&path).unwrap();
    let is_vertex = |n: usize| manifest.segments[n].kind == crate::persist::segment::KIND_VERTEX;
    let segments = (0..manifest.segments.len())
        .filter(|&n| is_vertex(n))
        .map(|n| {
            let s = &manifest.segments[n];
            (s.first_id.to_string(), s.last_id.to_string())
        })
        .collect();
    let chunks = manifest
        .chunks
        .iter()
        .filter(|row| is_vertex(row.segment as usize))
        .map(|row| {
            (
                row.chunk.first_id.to_string(),
                row.chunk.last_id.to_string(),
            )
        })
        .collect();
    (segments, chunks)
}

fn touch(graph: &mut TraversalGraph, ids: &[String], value: i64) {
    graph
        .transaction(|g| {
            for id in ids {
                if let Some(v) = g.get_vertex_by_id(id).unwrap() {
                    g.set_vertex_property(
                        v,
                        "touched".into(),
                        ElementProperty::Number(value.into()),
                    )?;
                }
            }
            Ok::<(), crate::error::GraphError>(())
        })
        .unwrap();
}

fn add_ids(graph: &mut TraversalGraph, ids: &[String]) {
    graph
        .transaction(|g| {
            let mut previous = None;
            for id in ids {
                let v = g.insert_vertex(
                    Some(id.as_str().into()),
                    vec!["new".into()],
                    PropertyMap::default(),
                )?;
                if let Some(p) = previous {
                    g.insert_edge(
                        Some(format!("e-{id}").into()),
                        Some("next".into()),
                        p,
                        v,
                        PropertyMap::default(),
                    )?;
                }
                previous = Some(v);
            }
            Ok::<(), crate::error::GraphError>(())
        })
        .unwrap();
}

fn drop_ids(graph: &mut TraversalGraph, ids: &[String]) {
    graph
        .transaction(|g| {
            for id in ids {
                if let Some(v) = g.get_vertex_by_id(id).unwrap() {
                    g.drop_vertex(v)?;
                }
            }
            Ok::<(), crate::error::GraphError>(())
        })
        .unwrap();
}

type Ranges = [(String, String)];
type RangeList = Vec<(String, String)>;
type Case = fn(&mut TraversalGraph, &Ranges, &Ranges);

/// Matrix row 3: the delta at every place of the id space (first, last, middle, every
/// segment, chunk boundaries, before the first and after the last range, between segments), a
/// segment growing past the target (split) or shrinking to nothing, the graph emptied and
/// refilled, a value larger than a chunk, Unicode ids: every merge equals the live graph, on
/// the live store and on the closed one; nothing touched writes nothing.
#[test]
fn the_delta_anywhere_in_the_id_space_merges_exactly() {
    let cases: Vec<(&str, Case)> = vec![
        ("first id", |g, s, _| touch(g, &[s[0].0.clone()], 1)),
        ("last id", |g, s, _| {
            touch(g, &[s[s.len() - 1].1.clone()], 1)
        }),
        ("middle segment", |g, s, _| {
            touch(g, &[s[s.len() / 2].0.clone()], 1)
        }),
        ("every segment", |g, s, _| {
            touch(g, &s.iter().map(|r| r.1.clone()).collect::<Vec<_>>(), 1)
        }),
        ("chunk boundaries", |g, _, c| {
            let ids: Vec<String> = c.iter().flat_map(|r| [r.0.clone(), r.1.clone()]).collect();
            touch(g, &ids, 2)
        }),
        ("before the first range", |g, _, _| {
            add_ids(g, &["!a".into(), "!b".into()])
        }),
        ("after the last range", |g, _, _| {
            add_ids(g, &["~a".into(), "~b".into()])
        }),
        ("between segments", |g, s, _| {
            let ids: Vec<String> = s.iter().map(|r| format!("{}!", r.1)).collect();
            add_ids(g, &ids)
        }),
        ("a segment past its target", |g, s, _| {
            let ids: Vec<String> = (0..300).map(|i| format!("{}!{i:04}", s[0].1)).collect();
            add_ids(g, &ids)
        }),
        ("a segment shrinks to nothing", |g, s, _| {
            let (first, last) = &s[1];
            let ids: Vec<String> = g
                .vertices()
                .unwrap()
                .filter_map(|v| g.vertex_id(v).unwrap().map(str::to_string))
                .filter(|id| id >= first && id <= last)
                .collect();
            drop_ids(g, &ids)
        }),
        ("emptied and refilled", |g, _, _| {
            let ids: Vec<String> = g
                .vertices()
                .unwrap()
                .filter_map(|v| g.vertex_id(v).unwrap().map(str::to_string))
                .collect();
            drop_ids(g, &ids);
            add_ids(g, &["refill-1".into(), "refill-2".into()]);
        }),
        ("a value larger than a chunk", |g, s, _| {
            g.transaction(|g| {
                let v = g.get_vertex_by_id(&s[0].0).unwrap().unwrap();
                g.set_vertex_property(
                    v,
                    "big".into(),
                    ElementProperty::String("x".repeat(20_000).into()),
                )
            })
            .unwrap()
        }),
        ("unicode ids", |g, _, _| {
            add_ids(
                g,
                &[
                    "ü-1".into(),
                    "日本-2".into(),
                    "😀".into(),
                    "a\u{0301}".into(),
                ],
            )
        }),
    ];
    for (name, case) in cases {
        for live in [true, false] {
            let dir = MemDir::new();
            let opts = options(Codec::Lz4, 512, 2048);
            let store = Store::create_with(&dir, generated_graph(250, 11), opts.clone()).unwrap();
            let (segments, chunks) = vertex_ranges(&dir);
            assert!(segments.len() > 4, "{name}");
            case(&mut store.graph().write(), &segments, &chunks);
            let label = format!("{name} (live {live})");
            if live {
                check_merge(&store, &label);
            } else {
                let live_graph = elements(&store.graph().read());
                let id = identity(&store.graph().read());
                store.close().unwrap();
                let report = Store::checkpoint_dir(&dir, opts, None).unwrap();
                assert!(report.written && report.merged, "{label}");
                let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
                let (_, loaded, _) = newest_snapshot(&reopened);
                assert_eq!(elements(&loaded), live_graph, "{label}");
                assert_eq!(identity(&loaded), id, "{label}");
            }
        }
    }
    // Nothing touched: no new snapshot.
    let dir = MemDir::new();
    let store =
        Store::create_with(&dir, generated_graph(50, 1), options(Codec::Lz4, 512, 2048)).unwrap();
    let report = store.core.shared.checkpoint(None, true).unwrap();
    assert!(!report.written);
}

/// A commit record appended to the WAL of a closed store.
fn append_commit(dir: &MemDir, mutations: Vec<crate::graph::change::Mutation>, seq: u64) {
    use crate::graph::change::{ChangeSet, CommitMetadata, CommitStamp};
    let changes = ChangeSet::new(
        CommitStamp {
            commit_seq: seq,
            committed_at: crate::graph::undo::now_micros(),
            node_sequence: 0,
            edge_sequence: 0,
        },
        CommitMetadata::new(),
        mutations,
    );
    let payload = crate::persist::wal::encode_commit(&changes, Codec::None).unwrap();
    let mut record = Vec::new();
    crate::persist::wal::frame_record(
        &mut record,
        crate::persist::wal::RECORD_COMMIT,
        seq,
        &payload,
    )
    .unwrap();
    let root = DirPath::root(dir.clone().into_store_dir());
    let (_, wal) = list_wal(&root).unwrap().pop().unwrap();
    wal.append(&record).unwrap();
}

fn modern_store() -> (MemDir, u64) {
    let dir = MemDir::new();
    let store = Store::create_with(
        &dir,
        TraversalGraph::tinkerpop_modern(),
        options(Codec::Lz4, 512, 2048),
    )
    .unwrap();
    touch(&mut store.graph().write(), &["1".into()], 5);
    let seq = store.graph().read().last_commit_seq();
    store.close().unwrap();
    (dir, seq)
}

/// Every name and file content of a store in memory.
pub(super) fn listing(dir: &MemDir) -> Vec<(String, Vec<u8>)> {
    let root = DirPath::root(dir.clone().into_store_dir());
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(name) = stack.pop() {
        for (entry, kind) in root.join(&name).list().unwrap_or_default() {
            let full = if name.is_empty() {
                entry
            } else {
                format!("{name}/{entry}")
            };
            if kind == super::files::EntryKind::Dir {
                stack.push(full.clone());
                out.push((full, Vec::new()));
            } else {
                out.push((full.clone(), dir.file(&full).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

/// Matrix row 12 (crafted WAL): a commit whose before image does not match the base, one that
/// adds an existing vertex, a gap and an overlap in the commit sequence are refused, with
/// nothing written; the matching before image merges.
#[test]
fn a_wal_that_does_not_continue_the_base_is_refused() {
    use crate::graph::change::{ElementRef, Mutation};
    let set = |before: Option<ElementProperty>| Mutation::SetProperty {
        element: ElementRef::vertex("2"),
        key: "name".into(),
        before,
        after: ElementProperty::String("x".into()),
    };
    let vadas = || Some(ElementProperty::String("vadas".into()));
    let cases: Vec<(&str, Vec<Mutation>, i64)> = vec![
        (
            "diverged before image",
            vec![set(Some(ElementProperty::String("not vadas".into())))],
            1,
        ),
        ("missing before image", vec![set(None)], 1),
        (
            "an existing vertex added",
            vec![Mutation::AddVertex {
                id: "2".into(),
                labels: vec![],
                properties: PropertyMap::default(),
            }],
            1,
        ),
        ("a gap", vec![set(vadas())], 2),
        ("an overlap", vec![set(vadas())], 0),
    ];
    for (name, mutations, offset) in cases {
        let (dir, seq) = modern_store();
        append_commit(&dir, mutations, (seq as i64 + offset) as u64);
        let before = dir.deep_copy();
        let result = Store::checkpoint_dir(&dir, StoreOptions::new(), None);
        assert!(result.is_err(), "{name}: {result:?}");
        assert_eq!(listing(&dir), listing(&before), "{name}");
    }
    let (dir, seq) = modern_store();
    append_commit(&dir, vec![set(vadas())], seq + 1);
    let report = Store::checkpoint_dir(&dir, StoreOptions::new(), None).unwrap();
    assert!(report.merged && report.snapshot.commit_seq == seq + 1);
}

/// Matrix row 15: a WAL of exactly the bound is merged, one byte over falls back to the
/// encoder.
#[test]
fn the_merge_bound_is_exact() {
    let (dir, _) = modern_store();
    let root = DirPath::root(dir.clone().into_store_dir());
    let graph_file = read_graph_file(&root).unwrap().file;
    let write = WriteOptions::new();
    let read = ReadOptions::new();
    let request = merge::MergeRequest {
        dir: &root,
        graph_file: &graph_file,
        durable: None,
        write: &write,
        read: &read,
        max_wal_bytes: 0,
    };
    let base = list_snapshots(&root).unwrap().pop().unwrap().0;
    let wal: u64 = merge::journals(&request, base)
        .unwrap()
        .iter()
        .map(|(_, path, _)| path.len().unwrap())
        .sum();
    assert!(wal > 0);
    for (bound, merged) in [(wal, true), (wal - 1, false)] {
        let image = dir.deep_copy();
        let report = Store::checkpoint_dir(
            &image,
            StoreOptions::new().with_checkpoint_merge_bytes(bound),
            None,
        )
        .unwrap();
        assert!(report.written);
        assert_eq!(report.merged, merged, "bound {bound} of {wal}");
        let opened = Store::open(&image, StoreOptions::new()).unwrap();
        let (_, loaded, _) = newest_snapshot(&opened);
        assert_eq!(elements(&loaded), elements(&opened.graph().read()));
    }
}

/// Matrix row 16: a critical definition of an unknown kind or a file of a critical unknown
/// segment kind in the base refuses the merge (the store is read-only for this version); an
/// ignorable unknown segment kind is not carried into the new snapshot (format spec 7.5).
#[test]
fn unknown_critical_things_refuse_the_merge() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.definitions.insert(Definition::opaque(
        DefinitionKind::from_code(201),
        DefinitionFlags::from_bits(DefinitionFlags::CRITICAL),
        "index",
        vec![0],
    ));
    let dir = MemDir::new();
    Store::create_with(&dir, graph, StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
    let err = Store::checkpoint_dir(&dir, StoreOptions::new(), None).unwrap_err();
    assert!(matches!(err, PersistError::ReadOnly { .. }), "{err}");
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(store.checkpoint(None).is_err());
    drop(store);

    for (kind, refused) in [(200u8, true), (50u8, false)] {
        let (dir, _) = modern_store();
        let root = DirPath::root(dir.clone().into_store_dir());
        let (_, path) = list_snapshots(&root).unwrap().pop().unwrap();
        let (_, mut manifest) = read_manifest(&path).unwrap();
        let file = b"derived data".to_vec();
        manifest.segments.push(SegmentEntry {
            kind,
            path: "extra.bin".into(),
            first_id: "".into(),
            last_id: "".into(),
            count: 0,
            byte_len: file.len() as u64,
            crc: crate::persist::codec::crc(&file),
        });
        let bytes = manifest.encode();
        path.join("extra.bin").write_atomic(&file).unwrap();
        path.join(MANIFEST_FILE).write_atomic(&bytes).unwrap();
        let result = Store::checkpoint_dir(&dir, StoreOptions::new(), None);
        if refused {
            assert!(
                matches!(result, Err(PersistError::ReadOnly { .. })),
                "{result:?}"
            );
        } else {
            let report = result.unwrap();
            assert!(report.merged);
            let (_, newest) = list_snapshots(&root).unwrap().pop().unwrap();
            let (_, merged) = read_manifest(&newest).unwrap();
            assert!(merged.segments.iter().all(|s| s.kind != kind));
            assert!(Store::verify_dir(&dir).unwrap().is_ok());
        }
    }
}

/// A closed store: modern graph as the base snapshot at commit 0, then `commits` commits.
fn modern_with(commits: usize) -> (MemDir, DirPath) {
    let dir = MemDir::new();
    let store = Store::create_with(
        &dir,
        TraversalGraph::tinkerpop_modern(),
        options(Codec::Lz4, 512, 2048),
    )
    .unwrap();
    for i in 0..commits {
        touch(
            &mut store.graph().write(),
            &["1".into(), "2".into()],
            i as i64,
        );
    }
    store.close().unwrap();
    let root = DirPath::root(dir.clone().into_store_dir());
    (dir, root)
}

fn newest(root: &DirPath) -> DirPath {
    list_snapshots(root).unwrap().pop().unwrap().1
}

/// Rewrites the newest snapshot's manifest through `edit` (the CRCs follow: a writer bug, not
/// a flip).
fn edit_manifest(root: &DirPath, edit: impl FnOnce(&mut crate::persist::manifest::Manifest)) {
    let path = newest(root);
    let (_, mut manifest) = read_manifest(&path).unwrap();
    edit(&mut manifest);
    let bytes = manifest.encode();
    path.join(MANIFEST_FILE).write_atomic(&bytes).unwrap();
}

fn merge_closed(
    root: &DirPath,
    durable: Option<crate::persist::DurablePosition>,
) -> Result<merge::Merged, PersistError> {
    let graph_file = read_graph_file(root).unwrap().file;
    let write = WriteOptions::new();
    let read = ReadOptions::new();
    merge::merge_checkpoint(&merge::MergeRequest {
        dir: root,
        graph_file: &graph_file,
        durable,
        write: &write,
        read: &read,
        max_wal_bytes: u64::MAX,
    })
}

/// Every error path of the merge that consistent-looking (checksummed) inputs can reach: a
/// manifest naming another commit than its directory, no snapshot at all, a durable end at the
/// base, a GRAPH whose lineage the WAL does not end in, a critical unknown definition committed
/// in the WAL, a chunk index row past the file, an invalid `schema.json`, a segment count that
/// differs from the segment, a durable position that does not fit the WAL; and the leniency: a
/// torn newer WAL segment is skipped.
#[test]
fn every_reachable_refusal_of_the_merge() {
    use crate::graph::change::Mutation;
    // The manifest names another commit than its directory.
    let (_, root) = modern_with(2);
    newest(&root)
        .rename_to(&root.join("snapshots").join(snapshot_name(9)))
        .unwrap();
    let err = merge_closed(&root, None).err().unwrap();
    assert!(err.to_string().contains("directory's name"), "{err}");

    // No snapshot: no base (and the loaded fallback fails too).
    let (dir, root) = modern_with(1);
    newest(&root).remove().unwrap();
    assert!(matches!(
        merge_closed(&root, None),
        Ok(merge::Merged::NoBase)
    ));
    assert!(Store::checkpoint_dir(&dir, StoreOptions::new(), None).is_err());

    // The durable end at the base: nothing to merge.
    let (_, root) = modern_with(1);
    let at_base = crate::persist::DurablePosition {
        segment: 1,
        offset: 64,
        commit_seq: 0,
    };
    assert!(matches!(
        merge_closed(&root, Some(at_base)),
        Ok(merge::Merged::Current { commit_seq: 0 })
    ));

    // A durable position whose offset lies past its commit (inconsistent): the two reads of the
    // WAL disagree, refused.
    let (_, root) = modern_with(3);
    let (first, wal) = list_wal(&root).unwrap().pop().unwrap();
    let len = wal.len().unwrap();
    let inconsistent = crate::persist::DurablePosition {
        segment: first,
        offset: len,
        commit_seq: 1,
    };
    let err = merge_closed(&root, Some(inconsistent)).err().unwrap();
    assert!(err.to_string().contains("read twice"), "{err}");

    // GRAPH names a lineage the WAL does not end in.
    let (_, root) = modern_with(1);
    let mut file = read_graph_file(&root).unwrap().file;
    let head = file.lineage[0];
    file.graph_id = 0xABCD;
    file.lineage.insert(
        0,
        super::files::LineageEntry {
            graph_id: 0xABCD,
            branched_at: 1,
            time: head.time,
        },
    );
    super::files::write_graph_file(&root, &file).unwrap();
    let err = merge_closed(&root, None).err().unwrap();
    assert!(err.to_string().contains("another lineage"), "{err}");

    // A critical definition of an unknown kind committed in the WAL (by a newer writer: this
    // one refuses to write it, so the record is patched): read-only.
    let (dir, seq) = modern_store();
    let unknown = Definition::opaque(
        DefinitionKind::from_code(202),
        DefinitionFlags::NONE,
        "index",
        vec![0],
    );
    let changes = crate::graph::change::ChangeSet::new(
        crate::graph::change::CommitStamp {
            commit_seq: seq + 1,
            committed_at: crate::graph::undo::now_micros(),
            node_sequence: 0,
            edge_sequence: 0,
        },
        crate::graph::change::CommitMetadata::new(),
        vec![Mutation::SetDefinition {
            kind: unknown.kind(),
            name: "index".into(),
            before: None,
            after: Some(Box::new(unknown)),
        }],
    );
    let mut payload = crate::persist::wal::encode_commit(&changes, Codec::None).unwrap();
    let image = [202u8, 0, 5, b'i', b'n', b'd', b'e', b'x'];
    let at = payload
        .windows(image.len())
        .position(|w| w == image)
        .expect("the definition image");
    payload[at + 1] = DefinitionFlags::CRITICAL;
    let mut record = Vec::new();
    crate::persist::wal::frame_record(
        &mut record,
        crate::persist::wal::RECORD_COMMIT,
        seq + 1,
        &payload,
    )
    .unwrap();
    let root = DirPath::root(dir.clone().into_store_dir());
    list_wal(&root)
        .unwrap()
        .pop()
        .unwrap()
        .1
        .append(&record)
        .unwrap();
    let err = Store::checkpoint_dir(&dir, StoreOptions::new(), None).unwrap_err();
    assert!(matches!(err, PersistError::ReadOnly { .. }), "{err}");

    // A chunk index row past the end of its file.
    let (_, root) = modern_with(1);
    edit_manifest(&root, |m| {
        for row in &mut m.chunks {
            row.chunk.offset = 1 << 40;
        }
    });
    let err = merge_closed(&root, None).err().unwrap();
    assert!(err.to_string().contains("past the file"), "{err}");

    // An invalid schema.json (its length and CRC in the manifest match it).
    let (_, root) = modern_with(1);
    let schema = b"{\"mode\": 7}".to_vec();
    newest(&root)
        .join(SCHEMA_FILE)
        .write_atomic(&schema)
        .unwrap();
    edit_manifest(&root, |m| {
        m.fixed.schema_len = schema.len() as u64;
        m.fixed.schema_crc = crate::persist::codec::crc(&schema);
    });
    let err = merge_closed(&root, None).err().unwrap();
    assert!(err.to_string().contains("invalid schema"), "{err}");

    // A segment row whose element count differs from the segment.
    let (_, root) = modern_with(1);
    edit_manifest(&root, |m| m.segments[0].count += 1);
    let err = merge_closed(&root, None).err().unwrap();
    assert!(err.to_string().contains("count or id range"), "{err}");

    // A newer WAL segment shorter than its header (a crash while it was started): skipped.
    let (dir, root) = modern_with(2);
    dir.put_file("wal/00000000000000000099.wal", vec![0; 10]);
    assert!(matches!(
        merge_closed(&root, None).unwrap(),
        merge::Merged::Written { .. }
    ));
}

/// A pair of snapshots that share segment files (here: the snapshot between them removed by
/// hand, so the newest references the files of the one now before it) is a WARNING of verify,
/// not damage (format spec 12.3, 13.1); the next checkpoint finds no twin for the shared files
/// (the candidate is the same file) and copies them: the rule holds again.
#[test]
fn a_snapshot_without_donors_is_a_warning_and_the_next_checkpoint_heals_it() {
    let dir = MemDir::new();
    let store = Store::create_with(
        &dir,
        generated_graph(400, 5),
        options(Codec::Lz4, 1024, 8 << 10),
    )
    .unwrap();
    let add = |id: &str| {
        store
            .graph()
            .write()
            .transaction(|g| {
                g.insert_vertex(Some(id.into()), vec![], PropertyMap::default())
                    .map(|_| ())
            })
            .unwrap();
    };
    for id in ["~a", "~b"] {
        add(id);
        check_merge(&store, id);
    }
    let root = DirPath::root(dir.clone().into_store_dir());
    let snapshots = list_snapshots(&root).unwrap();
    assert_eq!(snapshots.len(), 3);
    assert!(store.verify().unwrap().without_donor.is_empty());
    // The middle snapshot goes: the newest now shares the files it references with the first.
    snapshots[1].1.remove().unwrap();
    let report = store.verify().unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert!(!report.without_donor.is_empty());
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("no independent donor"))
    );
    add("~c");
    let healed = check_merge(&store, "heal");
    assert_eq!(healed.segments_reused, 0, "{healed:?}");
    assert_eq!(
        healed.segments_copied,
        report.without_donor.len(),
        "{healed:?}"
    );
    assert_donors(&dir, "healed");
    assert!(store.verify().unwrap().without_donor.is_empty());
}

/// Rainy day of the donor rule's copy: an unchanged base segment that is longer than its
/// manifest row, damaged inside, or missing stops the checkpoint (the copy checks every byte
/// against the row's length and CRC) with nothing visible written.
#[test]
fn a_damaged_segment_to_copy_stops_the_checkpoint() {
    type Damage = fn(&mut Vec<u8>) -> bool;
    let cases: [(&str, Damage); 3] = [
        ("longer", |b| {
            b.push(0);
            true
        }),
        ("flipped", |b| {
            let at = b.len() / 2;
            b[at] ^= 0x20;
            true
        }),
        ("missing", |_| false),
    ];
    for (name, damage) in cases {
        let dir = MemDir::new();
        let store = Store::create_with(
            &dir,
            generated_graph(400, 8),
            options(Codec::Lz4, 1024, 8 << 10),
        )
        .unwrap();
        store
            .graph()
            .write()
            .transaction(|g| {
                g.insert_vertex(Some("~last".into()), vec![], PropertyMap::default())
                    .map(|_| ())
            })
            .unwrap();
        let root = DirPath::root(dir.clone().into_store_dir());
        let (seq, _) = list_snapshots(&root).unwrap().pop().unwrap();
        let file = format!("snapshots/{}/v-000000.seg", snapshot_name(seq));
        let mut bytes = dir.file(&file).unwrap();
        if damage(&mut bytes) {
            dir.put_file(&file, bytes);
        } else {
            dir.remove_file(&file).unwrap();
        }
        let before = dir.file_names();
        let result = store.core.shared.checkpoint(None, true);
        assert!(result.is_err(), "{name}: {result:?}");
        assert_eq!(dir.file_names(), before, "{name}");
        assert_eq!(list_snapshots(&root).unwrap().len(), 1, "{name}");
    }
}

/// A graph schema a newer version wrote into the snapshot (a keyword this version does not
/// know; the checksums hold) is never damage (format spec 16): the store opens read-only with
/// the data and without the schema, every operation that would write a state from it is
/// refused as unsupported, verify lists no problem, nothing is changed.
#[test]
fn a_schema_of_a_newer_version_opens_read_only_never_damage() {
    let (dir, root) = modern_with(1);
    let schema =
        br#"{"mode":"open","vertices":{"person":{"schema":{"type":"object","xFuture":true}}}}"#
            .to_vec();
    newest(&root)
        .join(SCHEMA_FILE)
        .write_atomic(&schema)
        .unwrap();
    edit_manifest(&root, |m| {
        m.fixed.schema_len = schema.len() as u64;
        m.fixed.schema_crc = crate::persist::codec::crc(&schema);
    });
    let before = dir.file_names();
    let graph_file = crate::persist::StoreDir::read(&dir, "GRAPH", u64::MAX).unwrap();

    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let reason = store.read_only_reason().unwrap().expect("read-only");
    assert!(reason.contains("newer Graphersal"), "{reason}");
    assert!(reason.contains("xFuture"), "{reason}");
    assert!(store.maintenance().is_none());
    assert_eq!(store.graph().read().vertex_count(), 6);
    let err = store
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .to_list()
        .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
    // A state without its schema is never written anywhere.
    let fork = MemDir::new();
    let err = store.fork(RecoveryTarget::Latest, &fork).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    store.close().unwrap();

    let err = Store::checkpoint_dir(&dir, StoreOptions::new(), None).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    let help = crate::error::DiagnosticError::help(&err).unwrap();
    assert!(help.contains("newer Graphersal"), "{help}");
    let err = Store::repair_dir(&dir, MemDir::new()).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    let report = Store::verify_dir(&dir).unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert!(
        report.notes.iter().any(|n| n.contains("xFuture")),
        "{report:?}"
    );
    assert_eq!(dir.file_names(), before);
    assert_eq!(
        crate::persist::StoreDir::read(&dir, "GRAPH", u64::MAX).unwrap(),
        graph_file
    );
    // A backup copies the bytes as they are (its checks are checksums): the copy keeps the
    // schema for the newer version.
    let copy = MemDir::new();
    Store::backup_dir(&dir, &copy).unwrap();
    let copied = list_snapshots(&DirPath::root(copy.clone().into_store_dir()))
        .unwrap()
        .pop()
        .unwrap()
        .1;
    assert_eq!(
        crate::persist::StoreDir::read(
            &copy,
            &format!(
                "{}/{SCHEMA_FILE}",
                copied.relative_to(&DirPath::root(copy.clone().into_store_dir()))
            ),
            u64::MAX
        )
        .unwrap(),
        schema
    );
}
