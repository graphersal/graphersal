//! Snapshot chunks decode the objects nested in property values `Shaped` (stage V3b of the
//! compact-memory track): from the second sighting of a key sequence in a chunk, a nested map
//! is built over a shape of the chunk instead of an `IndexMap`, and the graph swaps in its own
//! shape when it stores the value. The stored form must not depend on it: the reference is the
//! former decode (`TraversalGraph::with_nested_maps_decoded_indexed`: every nested map decoded
//! `Indexed` and converted when stored), and every load path must give the same graph either
//! way (the exact content, the full memory figures, the memory report with its shape table
//! count, the rule statistics, the compression invariant): Store open (snapshot and WAL
//! replay), merged checkpoints of an open and of a closed store, backups, read-only opens at
//! the latest and at an earlier commit, packed bytes and `.gsnap` files; with and without
//! top-level `dedup`, lz4 and nested-path rules; small chunks (sequences that repeat across
//! chunks, parallel decoding) and objects deeper than the shaped levels.
#![cfg(all(feature = "persist", feature = "io"))]

use std::sync::Arc;

use graphersal::catalog::{Codec, CompressionDefinition};
use graphersal::persist::{
    self, MemDir, ReadOptions, RecoveryTarget, Store, StoreOptions, WriteOptions,
};
use graphersal::prelude::*;
use graphersal::storage::{ElementKind, MemoryReportRequest};

/// A small chunk target: the fixture spans many chunks.
const CHUNK: u32 = 4096;

fn object(entries: Vec<(String, ElementProperty)>) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    ElementProperty::Object(Box::new(map))
}

fn kv(key: &str, value: impl Into<ElementProperty>) -> (String, ElementProperty) {
    (key.to_string(), value.into())
}

/// An object `levels` deep: `{"d": {"d": ... {"leaf": i}}}`.
fn deep(levels: usize, i: usize) -> ElementProperty {
    let mut value = object(vec![kv("leaf", i as i64)]);
    for _ in 0..levels {
        value = object(vec![("d".to_string(), value)]);
    }
    value
}

/// The properties of vertex `i`: repeated nested key sequences (`_hashes` with one of 37 key
/// names, objects in an array, a `doc` under a nested-path rule), key sets that never repeat
/// (`by_id`), an empty object, a map holding a map of its own key sequence (`self`: the outer
/// one is the first sighting in the graph although the chunk saw the inner one first), an
/// object with the key sequence of the element maps of `other`, and values deeper than the
/// shaped levels.
fn item(i: usize) -> PropertyMap {
    let mut entries = vec![
        kv("name", format!("m{i}")),
        (
            "_hashes".to_string(),
            object(vec![kv(
                &format!("ACM:{}", i % 37),
                format!("{:016x}", i % 13),
            )]),
        ),
        (
            "attrs".to_string(),
            ElementProperty::Array(vec![
                object(vec![kv("line", i as i64), kv("kind", "call")]),
                object(vec![kv("line", 1i64), kv("kind", "use")]),
                object(vec![kv("kind", "use"), kv("line", 2i64)]),
            ]),
        ),
        (
            "doc".to_string(),
            object(vec![
                kv(
                    "title",
                    format!("a document title long enough to be shared {}", i % 3),
                ),
                kv("page", i as i64),
            ]),
        ),
        (
            "by_id".to_string(),
            object(vec![kv(&format!("id-{i}"), i as i64), kv("common", true)]),
        ),
        ("empty".to_string(), object(Vec::new())),
        (
            "self".to_string(),
            object(vec![(
                format!("s{}", i % 5),
                object(vec![kv(&format!("s{}", i % 5), i as i64)]),
            )]),
        ),
        (
            "like_other".to_string(),
            object(vec![kv("x", i as i64), kv("y", "z")]),
        ),
        ("deep".to_string(), deep(9 + i % 3, i)),
        (
            "note".to_string(),
            ElementProperty::from(format!(
                "{} -- a long note that the lz4 rule compresses {}",
                "lorem ipsum dolor sit amet ".repeat(4),
                i % 7
            )),
        ),
    ];
    if i % 5 == 2 {
        entries.retain(|(key, _)| key != "attrs");
    }
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    map
}

fn other(i: usize) -> PropertyMap {
    let mut map = PropertyMap::default();
    map.insert("x", ElementProperty::from(i as i64));
    map.insert("y", ElementProperty::from("z"));
    map
}

fn edge_props(i: usize) -> PropertyMap {
    let mut map = PropertyMap::default();
    map.insert("w", ElementProperty::from((i % 4) as i64));
    map.insert(
        "meta",
        object(vec![kv("since", (i % 9) as i64), kv("via", "x")]),
    );
    map
}

/// Vertices `from..to` (`Method`, every tenth an `other`) and edges `edges_from..edges_to`
/// in one unit.
fn add_data(graph: &mut TraversalGraph, from: usize, to: usize, edges: std::ops::Range<usize>) {
    graph
        .transaction(|unit| -> Result<(), GraphError> {
            for i in from..to {
                if i % 10 == 9 {
                    unit.add_vertex(Some(format!("o{i}").into()), vec!["other".into()], other(i))?;
                }
                unit.add_vertex(Some(format!("m{i}").into()), vec!["Method".into()], item(i))?;
            }
            for i in edges {
                unit.add_edge_by_ids(
                    Some(format!("r{i}").into()),
                    Some("calls".into()),
                    &format!("m{}", i % to),
                    &format!("m{}", (i * 7 + 1) % to),
                    edge_props(i),
                )?;
            }
            Ok(())
        })
        .unwrap();
}

/// The rule names of [`define_rules`].
const RULES: [&str; 5] = ["hashes", "attrs", "doc_title", "note", "calls_meta"];

/// Top-level `dedup` on two object-valued keys and on an edge's object, a nested-path `dedup`
/// rule on `doc.title`, lz4 on `note`.
fn define_rules(graph: &mut TraversalGraph) {
    let dedup = |element, label: &str, key: &str| {
        CompressionDefinition::new(element, label, key)
            .unwrap()
            .with_codec(Codec::Dedup)
    };
    let mut g = graph.traversal_mut();
    g.define_compression("hashes", dedup(ElementKind::Vertex, "Method", "_hashes"))
        .unwrap();
    g.define_compression("attrs", dedup(ElementKind::Vertex, "Method", "attrs"))
        .unwrap();
    g.define_compression(
        "doc_title",
        CompressionDefinition::with_path(
            ElementKind::Vertex,
            "Method",
            "doc.title".parse::<JsonPath>().unwrap(),
        )
        .with_codec(Codec::Dedup),
    )
    .unwrap();
    g.define_compression(
        "note",
        CompressionDefinition::new(ElementKind::Vertex, "Method", "note")
            .unwrap()
            .with_min_bytes(16),
    )
    .unwrap();
    g.define_compression("calls_meta", dedup(ElementKind::Edge, "calls", "meta"))
        .unwrap();
}

const ITEMS: usize = 300;
const EDGES: usize = 200;

fn rules_for(ruled: bool) -> &'static [&'static str] {
    if ruled { &RULES } else { &[] }
}

/// The later changes (they reach the WAL): new elements, nested objects overwritten.
fn later_changes(graph: &mut TraversalGraph) {
    add_data(graph, ITEMS, ITEMS + 40, EDGES..EDGES + 20);
    graph
        .traversal_mut()
        .v(vec!["m1", "m2", "m3"])
        .property(
            "_hashes",
            object(vec![kv("ACM:new", "v"), kv("ACM:other", "w")]),
        )
        .to_list()
        .unwrap();
}

fn graph(ruled: bool) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    add_data(&mut graph, 0, ITEMS, 0..EDGES);
    if ruled {
        define_rules(&mut graph);
    }
    graph
}

// ---------------------------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------------------------

/// The exact content: the GraphSON export (typed values, key order, element order).
fn exact(graph: &TraversalGraph) -> String {
    let mut out = Vec::new();
    graph.traversal().export_graphson_writer(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// The memory report's figures that depend on the stored form (no timings, no process heap).
fn report(graph: &TraversalGraph) -> String {
    let report = graph
        .traversal()
        .memory_report(&MemoryReportRequest::new().with_limit(usize::MAX))
        .unwrap();
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        report.properties, report.labels, report.shapes, report.compression_rules, report.structure
    )
}

/// Asserts that `direct` (nested maps decoded `Shaped`) and `reference` (decoded `Indexed`,
/// converted when stored) hold the same graph in the same stored form.
#[track_caller]
fn assert_same(direct: &TraversalGraph, reference: &TraversalGraph, rules: &[&str], what: &str) {
    for graph in [direct, reference] {
        if let Err(reason) = graph.check_compression_invariant() {
            panic!("{what}: invariant broken: {reason}");
        }
    }
    let (a, b) = (exact(direct), exact(reference));
    for (x, y) in a.lines().zip(b.lines()) {
        assert_eq!(x, y, "{what}: export");
    }
    assert_eq!(a.lines().count(), b.lines().count(), "{what}: export lines");
    let stats = direct.memory_usage().unwrap().unwrap();
    assert_eq!(
        stats,
        reference.memory_usage().unwrap().unwrap(),
        "{what}: memory figures"
    );
    assert!(stats.shape_bytes > 0, "{what}: shapes in use");
    assert_eq!(report(direct), report(reference), "{what}: memory report");
    for rule in rules {
        assert_eq!(
            direct.compression_stats(rule),
            reference.compression_stats(rule),
            "{what}: statistics of rule {rule}"
        );
        assert!(
            direct.compression_stats(rule).is_some(),
            "{what}: rule {rule} served"
        );
    }
    assert!(direct.vertex_count() >= ITEMS, "{what}: loaded");
}

/// A store with the fixture in its snapshot and the later changes in its WAL.
fn store(ruled: bool) -> MemDir {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new().with_chunk_bytes(CHUNK)).unwrap();
    {
        let graph = store.graph();
        let mut graph = graph.write();
        add_data(&mut graph, 0, ITEMS, 0..EDGES);
        if ruled {
            define_rules(&mut graph);
        }
    }
    store.checkpoint(None).unwrap();
    {
        let graph = store.graph();
        let mut graph = graph.write();
        later_changes(&mut graph);
    }
    store.close().unwrap();
    dir
}

#[track_caller]
fn assert_open_same(dir: &MemDir, ruled: bool, what: &str) {
    let direct = Store::open(dir, StoreOptions::new()).unwrap();
    let direct_graph = Arc::clone(direct.graph());
    direct.close().unwrap();
    let reference = TraversalGraph::with_nested_maps_decoded_indexed(|| {
        Store::open(dir, StoreOptions::new()).unwrap()
    });
    assert_same(
        &direct_graph.read(),
        &reference.graph().read(),
        rules_for(ruled),
        what,
    );
    reference.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn packed_snapshots_and_gsnap_files_load_as_the_indexed_decode_does() {
    for ruled in [false, true] {
        let mut graph = graph(ruled);
        later_changes(&mut graph);
        let mut bytes = Vec::new();
        persist::write_snapshot(
            &graph,
            &mut bytes,
            &WriteOptions::new().with_chunk_bytes(CHUNK),
        )
        .unwrap();
        let direct = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
        let reference = TraversalGraph::with_nested_maps_decoded_indexed(|| {
            TraversalGraph::read_snapshot(bytes.as_slice())
        })
        .unwrap();
        assert_same(&direct, &reference, rules_for(ruled), "packed bytes");
        // The loaded graph holds what the live graph holds.
        let live = exact(&graph);
        assert_eq!(exact(&direct).lines().count(), live.lines().count());
        // One large chunk as well (every sighting in one chunk).
        let mut one = Vec::new();
        persist::write_snapshot(&graph, &mut one, &WriteOptions::new()).unwrap();
        let direct = TraversalGraph::read_snapshot(one.as_slice()).unwrap();
        let reference = TraversalGraph::with_nested_maps_decoded_indexed(|| {
            TraversalGraph::read_snapshot(one.as_slice())
        })
        .unwrap();
        assert_same(&direct, &reference, rules_for(ruled), "one chunk");
        // A `.gsnap` file.
        let path = std::env::temp_dir().join(format!(
            "graphersal-nested-decode-{}-{:?}-{ruled}.gsnap",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, &bytes).unwrap();
        let options = ReadOptions::new();
        let (direct, _) = persist::read_snapshot_file(&path, &options).unwrap();
        let (reference, _) = TraversalGraph::with_nested_maps_decoded_indexed(|| {
            persist::read_snapshot_file(&path, &options)
        })
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_same(
            &direct.read(),
            &reference.read(),
            rules_for(ruled),
            ".gsnap file",
        );
    }
}

#[test]
fn a_store_open_loads_as_the_indexed_decode_does() {
    for ruled in [false, true] {
        assert_open_same(&store(ruled), ruled, "snapshot + WAL");
    }
}

#[test]
fn merged_checkpoints_load_as_the_indexed_decode_does() {
    for ruled in [false, true] {
        let dir = store(ruled);
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        assert_open_same(&dir, ruled, "merged checkpoint");
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        store
            .graph()
            .write()
            .traversal_mut()
            .v(vec!["m7", "m8"])
            .property(
                "doc",
                object(vec![kv("title", "changed"), kv("page", 0i64)]),
            )
            .to_list()
            .unwrap();
        store.close().unwrap();
        Store::checkpoint_dir(&dir, StoreOptions::new(), None).unwrap();
        assert_open_same(&dir, ruled, "closed-store checkpoint");
    }
}

#[test]
fn a_backup_loads_as_the_indexed_decode_does() {
    for ruled in [false, true] {
        let dir = store(ruled);
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        let backup = MemDir::new();
        store.backup(&backup).unwrap();
        store.close().unwrap();
        let direct = Store::open_backup(&backup, StoreOptions::new()).unwrap();
        let reference = TraversalGraph::with_nested_maps_decoded_indexed(|| {
            Store::open_backup(&backup, StoreOptions::new()).unwrap()
        });
        assert_same(
            &direct.graph().read(),
            &reference.graph().read(),
            rules_for(ruled),
            "backup",
        );
        direct.close().unwrap();
        reference.close().unwrap();
    }
}

#[test]
fn read_only_opens_and_history_views_load_as_the_indexed_decode_does() {
    for ruled in [false, true] {
        let dir = store(ruled);
        let direct = Store::open_read_only(&dir, RecoveryTarget::Latest).unwrap();
        let reference = TraversalGraph::with_nested_maps_decoded_indexed(|| {
            Store::open_read_only(&dir, RecoveryTarget::Latest).unwrap()
        });
        assert_same(
            &direct.graph,
            &reference.graph,
            rules_for(ruled),
            "read-only",
        );
        let at = {
            let store = Store::open(&dir, StoreOptions::new()).unwrap();
            let seq = store
                .snapshots()
                .unwrap()
                .iter()
                .map(|snapshot| snapshot.commit_seq)
                .max()
                .unwrap();
            store.close().unwrap();
            seq
        };
        let direct = Store::open_read_only(&dir, RecoveryTarget::CommitSeq(at)).unwrap();
        let reference = TraversalGraph::with_nested_maps_decoded_indexed(|| {
            Store::open_read_only(&dir, RecoveryTarget::CommitSeq(at)).unwrap()
        });
        assert_same(
            &direct.graph,
            &reference.graph,
            rules_for(ruled),
            "history view",
        );
    }
}

#[test]
fn a_reloaded_graph_keeps_its_memory_figures_and_values() {
    // Load, write, load again: the figures stay (the decode reaches the stored form of the
    // first load), and the values read back as the live graph holds them.
    let graph = graph(false);
    let mut bytes = Vec::new();
    persist::write_snapshot(
        &graph,
        &mut bytes,
        &WriteOptions::new().with_chunk_bytes(CHUNK),
    )
    .unwrap();
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    let mut again = Vec::new();
    persist::write_snapshot(
        &loaded,
        &mut again,
        &WriteOptions::new().with_chunk_bytes(CHUNK),
    )
    .unwrap();
    let reloaded = TraversalGraph::read_snapshot(again.as_slice()).unwrap();
    assert_eq!(
        loaded.memory_usage().unwrap(),
        reloaded.memory_usage().unwrap()
    );
    for id in ["m0", "m1", "m42", "m299"] {
        for key in ["_hashes", "attrs", "by_id", "empty", "self", "deep"] {
            let read = |g: &TraversalGraph| g.traversal().v(id).values(key).to_list().unwrap();
            assert_eq!(read(&graph), read(&reloaded), "{id} {key}");
        }
    }
    // A stored nested map compares equal to a free-standing one.
    let wanted = object(vec![kv("ACM:5", format!("{:016x}", 42 % 13))]);
    let found = reloaded
        .traversal()
        .v(None)
        .has("_hashes", wanted)
        .id()
        .to_list()
        .unwrap();
    assert_eq!(found, vec![ElementProperty::from("m42")]);
}
