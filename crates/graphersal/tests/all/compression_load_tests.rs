//! Loads code the rows of top-level `dedup` rules as they enter the graph (stage V2 of the
//! compact-memory track): a packed snapshot restores its catalog before its elements, so every
//! row is coded straight into the rules' value dictionaries while the chunks are read, and the
//! end of the load only normalizes the labels of the lz4 and nested-path rules. The reference is
//! the former way, the whole-graph pass at the end of the load
//! (`TraversalGraph::with_load_post_pass`): every load path must give the same graph either way
//! (the exact content, the coded state of every value, the memory figures, the rule statistics,
//! the compression invariant): Store open (snapshot and WAL replay), merged checkpoints of an
//! open and of a closed store, backups, read-only opens at the latest and at an earlier commit,
//! and `.gsnap` files; every value type; `min_bytes` 0 and 25; lz4 and nested-path rules beside
//! the coded ones; a vertex whose labels name rules of two codecs on one key.
#![cfg(all(feature = "persist", feature = "io"))]

use std::sync::Arc;

use graphersal::catalog::{Codec, CompressionDefinition, Definition, DefinitionKind};
use graphersal::persist::{MemDir, ReadOptions, RecoveryTarget, Store, StoreOptions};
use graphersal::prelude::*;
use graphersal::storage::ElementKind;
use graphersal::temporal::{Temporal, TemporalKind};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    map
}

fn temporal(kind: TemporalKind, text: &str) -> ElementProperty {
    ElementProperty::Temporal(Temporal::parse_as(kind, text).unwrap())
}

fn object(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    ElementProperty::Object(Box::new(props(entries)))
}

/// The top-level keys of the items: one per value type, `note` under a rule with `min_bytes`
/// 25 (its short values stay in the row), `text` under an lz4 rule, `doc` with a nested-path
/// dedup rule on `doc.title`, `name` without a rule.
const ITEM_KEYS: [&str; 15] = [
    "name", "kind", "n", "f", "flag", "day", "at", "tags", "meta", "uid", "nothing", "note",
    "text", "doc", "big",
];

/// The properties of item `i`.
fn item(i: usize) -> PropertyMap {
    let mut entries = vec![
        ("name", ElementProperty::from(format!("item-{i}"))),
        ("kind", ElementProperty::from(["a", "bb", "ccc", ""][i % 4])),
        ("n", ElementProperty::from((i % 5) as i64)),
        ("f", ElementProperty::from([1.0f64, 2.5, 1.0][i % 3])),
        ("flag", ElementProperty::Boolean(i % 2 == 0)),
        (
            "day",
            temporal(TemporalKind::Date, ["2024-01-01", "2024-02-29"][i % 2]),
        ),
        (
            "at",
            temporal(
                TemporalKind::DateTime,
                ["2024-01-01T10:00:00+02:00", "2024-01-01T08:00:00Z"][i % 2],
            ),
        ),
        (
            "tags",
            ElementProperty::Array(vec![
                ElementProperty::from("x"),
                ElementProperty::from((i % 3) as i64),
            ]),
        ),
        (
            "meta",
            if i % 2 == 0 {
                object(vec![("a", 1i64.into()), ("b", "two".into())])
            } else {
                object(vec![("b", "two".into()), ("a", 1i64.into())])
            },
        ),
        (
            "uid",
            ElementProperty::Uuid(0x1234_5678_u128 + (i % 3) as u128),
        ),
        ("nothing", ElementProperty::Null),
        (
            "note",
            if i % 3 == 0 {
                ElementProperty::from("short")
            } else {
                ElementProperty::from(format!(
                    "a note that is long enough to be coded, number {}",
                    i % 4
                ))
            },
        ),
        (
            "text",
            ElementProperty::from(format!(
                "{} -- a long text that the lz4 rule compresses, repeated {}",
                "lorem ipsum dolor sit amet ".repeat(8),
                i % 9
            )),
        ),
        (
            "doc",
            object(vec![
                (
                    "title",
                    ElementProperty::from(format!(
                        "a document title long enough to be shared {}",
                        i % 3
                    )),
                ),
                ("page", ElementProperty::from(i as i64)),
            ]),
        ),
        ("big", ElementProperty::from(i as i64 * 1_000_003)),
    ];
    // Some items lack a coded key (other shapes).
    if i % 7 == 3 {
        entries.retain(|(key, _)| *key != "n");
    }
    if i % 11 == 5 {
        entries.retain(|(key, _)| *key != "kind" && *key != "text");
    }
    props(entries)
}

/// Item labels: most are `item`; some also `tagged` (after or before `item`: the first label
/// wins a key both name, so `kind` is coded on one and lz4-compressed on the other).
fn labels(i: usize) -> Vec<CompactString> {
    match i % 10 {
        4 => vec!["item".into(), "tagged".into()],
        7 => vec!["tagged".into(), "item".into()],
        _ => vec!["item".into()],
    }
}

fn add_items(graph: &mut TraversalGraph, from: usize, to: usize) {
    for i in from..to {
        graph
            .add_vertex(Some(format!("i{i}").into()), labels(i), item(i))
            .unwrap();
    }
}

fn edge_props(i: usize) -> PropertyMap {
    props(vec![
        ("w", ElementProperty::from([0.5f64, 1.5][i % 2])),
        ("kind", ElementProperty::from(["near", "far"][i % 2])),
        (
            "comment",
            ElementProperty::from(format!(
                "{} edge comment {}",
                "a comment that the edge lz4 rule compresses ".repeat(4),
                i % 5
            )),
        ),
    ])
}

/// Items `from..to` and edges `edges_from..edges_to` in one unit (so a Store journals them).
fn add_data(
    graph: &mut TraversalGraph,
    from: usize,
    to: usize,
    edges_from: usize,
    edges_to: usize,
) {
    graph
        .transaction(|unit| -> Result<(), GraphError> {
            add_items(unit, from, to);
            add_edges(unit, to, edges_from, edges_to);
            Ok(())
        })
        .unwrap();
}

fn add_edges(graph: &mut TraversalGraph, n: usize, from: usize, to: usize) {
    for i in from..to {
        graph
            .add_edge_by_ids(
                Some(format!("r{i}").into()),
                Some("rel".into()),
                &format!("i{}", i % n),
                &format!("i{}", (i * 3 + 1) % n),
                edge_props(i),
            )
            .unwrap();
    }
}

fn dedup(element: ElementKind, label: &str, key: &str) -> CompressionDefinition {
    CompressionDefinition::new(element, label, key)
        .unwrap()
        .with_codec(Codec::Dedup)
}

fn lz4(element: ElementKind, label: &str, key: &str) -> CompressionDefinition {
    CompressionDefinition::new(element, label, key)
        .unwrap()
        .with_min_bytes(16)
}

/// The rule names (every rule the fixture defines).
const RULES: [&str; 17] = [
    "kind",
    "n",
    "f",
    "flag",
    "day",
    "at",
    "tags",
    "meta",
    "uid",
    "nothing",
    "note",
    "big",
    "text",
    "doc_title",
    "tagged_kind",
    "rel_w",
    "rel_kind",
];

/// Every rule: dedup with `min_bytes` 0 on every value type, dedup with `min_bytes` 25 on
/// `note`, lz4 on `text`, a nested-path dedup rule on `doc.title`, lz4 on `kind` of `tagged`,
/// dedup on both edge keys and lz4 (with a dictionary) on the edge comments.
fn define_rules(graph: &mut TraversalGraph) {
    let mut definitions = Vec::new();
    for key in [
        "kind", "n", "f", "flag", "day", "at", "tags", "meta", "uid", "nothing", "big",
    ] {
        definitions.push((key.to_string(), dedup(ElementKind::Vertex, "item", key)));
    }
    definitions.push((
        "note".to_string(),
        dedup(ElementKind::Vertex, "item", "note").with_min_bytes(25),
    ));
    definitions.push(("text".to_string(), lz4(ElementKind::Vertex, "item", "text")));
    definitions.push((
        "doc_title".to_string(),
        CompressionDefinition::with_path(
            ElementKind::Vertex,
            "item",
            "doc.title".parse::<JsonPath>().unwrap(),
        )
        .with_codec(Codec::Dedup),
    ));
    definitions.push((
        "tagged_kind".to_string(),
        lz4(ElementKind::Vertex, "tagged", "kind").with_min_bytes(1),
    ));
    definitions.push(("rel_w".to_string(), dedup(ElementKind::Edge, "rel", "w")));
    definitions.push((
        "rel_kind".to_string(),
        dedup(ElementKind::Edge, "rel", "kind"),
    ));
    definitions.push((
        "rel_comment".to_string(),
        lz4(ElementKind::Edge, "rel", "comment").with_dictionary(true),
    ));
    let mut g = graph.traversal_mut();
    for (name, rule) in definitions {
        g.define_compression(&name, rule).unwrap();
    }
}

/// The items and edges the stores start with.
const ITEMS: usize = 120;
const EDGES: usize = 90;

/// A graph with the fixture's data and rules (rules defined after the data).
fn ruled_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    add_data(&mut graph, 0, ITEMS, 0, EDGES);
    define_rules(&mut graph);
    graph
}

/// The later changes of the stores (they reach the WAL): new elements (coded as they are
/// written), overwrites of coded values, a key removed, a label added, a new rule and a dropped
/// one.
fn later_changes(graph: &mut TraversalGraph) {
    add_data(graph, ITEMS, ITEMS + 20, EDGES, EDGES + 10);
    graph
        .traversal_mut()
        .v(vec!["i1", "i2", "i3"])
        .property("kind", "zz")
        .property("n", 42i64)
        .to_list()
        .unwrap();
    graph
        .traversal_mut()
        .v("i5")
        .properties("flag")
        .drop()
        .iterate()
        .unwrap();
    graph
        .traversal_mut()
        .v("i6")
        .add_label("tagged")
        .iterate()
        .unwrap();
    let mut g = graph.traversal_mut();
    g.define_compression("name", dedup(ElementKind::Vertex, "item", "name"))
        .unwrap();
    g.remove_definition(DefinitionKind::COMPRESSION, "uid")
        .unwrap();
}

/// The rule names after [`later_changes`].
fn later_rules() -> Vec<&'static str> {
    let mut rules: Vec<&str> = RULES.iter().copied().filter(|r| *r != "uid").collect();
    rules.push("name");
    rules
}

// ---------------------------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------------------------

/// The exact content: the GraphSON export (typed values, offsets, key order, element order).
fn exact(graph: &TraversalGraph) -> String {
    let mut out = Vec::new();
    graph.traversal().export_graphson_writer(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// The export's lines sorted, without the vertex property ids (numbered in slot order, which a
/// load may change).
fn sorted(text: String) -> String {
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| {
            let mut out = String::new();
            let mut rest = line;
            while let Some(at) = rest.find(r#""id":{"@type":"g:Int64","@value":"#) {
                out.push_str(&rest[..at]);
                rest = &rest[at..];
                let end = rest.find('}').map_or(rest.len(), |end| end + 1);
                rest = &rest[end..];
            }
            out.push_str(rest);
            out
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

#[track_caller]
fn assert_same_text(a: &str, b: &str, what: &str) {
    if a == b {
        return;
    }
    for (x, y) in a.lines().zip(b.lines()) {
        assert_eq!(x, y, "{what}");
    }
    assert_eq!(a.lines().count(), b.lines().count(), "{what}");
}

/// Every vertex and edge id of `graph`.
fn ids(graph: &TraversalGraph) -> (Vec<String>, Vec<String>) {
    let ids = |values: Vec<ElementProperty>| -> Vec<String> {
        let mut ids: Vec<String> = values
            .iter()
            .map(|value| match value {
                ElementProperty::String(id) => id.to_string(),
                other => panic!("an id that is no string: {other:?}"),
            })
            .collect();
        ids.sort();
        ids
    };
    (
        ids(graph.traversal().v(None).id().to_list().unwrap()),
        ids(graph.traversal().e(None).id().to_list().unwrap()),
    )
}

/// Asserts that `direct` (a load that coded the rows as they entered) and `reference` (the same
/// load through the whole-graph end pass) hold the same graph in the same stored form.
#[track_caller]
fn assert_same_load(
    direct: &TraversalGraph,
    reference: &TraversalGraph,
    rules: &[&str],
    what: &str,
) {
    for graph in [direct, reference] {
        if let Err(reason) = graph.check_compression_invariant() {
            panic!("{what}: invariant broken: {reason}");
        }
    }
    assert_same_text(&exact(direct), &exact(reference), what);
    assert_eq!(
        direct.memory_usage().unwrap().unwrap(),
        reference.memory_usage().unwrap().unwrap(),
        "{what}: memory figures"
    );
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
        assert_eq!(
            direct.dedup_table_len(rule),
            reference.dedup_table_len(rule),
            "{what}: entries of rule {rule}"
        );
    }
    let (vertices, edges) = ids(direct);
    assert_eq!(
        (vertices.clone(), edges.clone()),
        ids(reference),
        "{what}: ids"
    );
    let mut coded = 0usize;
    for id in &vertices {
        for key in ITEM_KEYS {
            let state = direct.is_dedup_coded(ElementKind::Vertex, id, key);
            assert_eq!(
                state,
                reference.is_dedup_coded(ElementKind::Vertex, id, key),
                "{what}: vertex {id} key {key}"
            );
            coded += usize::from(state);
        }
    }
    for id in &edges {
        for key in ["w", "kind", "comment"] {
            let state = direct.is_dedup_coded(ElementKind::Edge, id, key);
            assert_eq!(
                state,
                reference.is_dedup_coded(ElementKind::Edge, id, key),
                "{what}: edge {id} key {key}"
            );
            coded += usize::from(state);
        }
    }
    assert!(
        coded > 1000,
        "{what}: the rows are coded ({coded} coded values)"
    );
}

/// A store holding the fixture: data and rules in its snapshot, the later changes in its WAL.
fn ruled_store() -> MemDir {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    {
        let graph = store.graph();
        let mut graph = graph.write();
        add_data(&mut graph, 0, ITEMS, 0, EDGES);
        define_rules(&mut graph);
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

/// Opens the store in `dir` both ways and compares the graphs.
#[track_caller]
fn assert_open_same(dir: &MemDir, what: &str) {
    let direct = Store::open(dir, StoreOptions::new()).unwrap();
    let direct_graph = Arc::clone(direct.graph());
    direct.close().unwrap();
    let reference =
        TraversalGraph::with_load_post_pass(|| Store::open(dir, StoreOptions::new()).unwrap());
    assert_same_load(
        &direct_graph.read(),
        &reference.graph().read(),
        &later_rules(),
        what,
    );
    reference.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn a_store_open_codes_the_rows_as_the_end_pass_does() {
    let dir = ruled_store();
    // The snapshot (data and rules) plus the WAL (new elements, overwrites, a new rule, a
    // dropped one).
    assert_open_same(&dir, "snapshot + WAL");
}

#[test]
fn a_merged_checkpoint_loads_as_the_end_pass_does() {
    let dir = ruled_store();
    // The checkpoint of an open store merges the WAL into a new snapshot.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    assert_open_same(&dir, "merged checkpoint");
    // More changes, then the checkpoint of the closed store.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .v(vec!["i7", "i8"])
        .property("meta", "now a string")
        .to_list()
        .unwrap();
    store.close().unwrap();
    Store::checkpoint_dir(&dir, StoreOptions::new(), None).unwrap();
    assert_open_same(&dir, "closed-store checkpoint");
}

#[test]
fn a_backup_loads_as_the_end_pass_does() {
    let dir = ruled_store();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let direct = Store::open_backup(&backup, StoreOptions::new()).unwrap();
    let reference = TraversalGraph::with_load_post_pass(|| {
        Store::open_backup(&backup, StoreOptions::new()).unwrap()
    });
    assert_same_load(
        &direct.graph().read(),
        &reference.graph().read(),
        &later_rules(),
        "backup",
    );
    direct.close().unwrap();
    reference.close().unwrap();
}

#[test]
fn read_only_opens_and_history_views_load_as_the_end_pass_does() {
    let dir = ruled_store();
    let direct = Store::open_read_only(&dir, RecoveryTarget::Latest).unwrap();
    let reference = TraversalGraph::with_load_post_pass(|| {
        Store::open_read_only(&dir, RecoveryTarget::Latest).unwrap()
    });
    assert_same_load(&direct.graph, &reference.graph, &later_rules(), "read-only");
    // The state of the snapshot's commit: the rules before the later changes.
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
    let reference = TraversalGraph::with_load_post_pass(|| {
        Store::open_read_only(&dir, RecoveryTarget::CommitSeq(at)).unwrap()
    });
    assert_eq!(direct.graph.vertex_count(), ITEMS);
    assert_same_load(&direct.graph, &reference.graph, &RULES, "history view");
}

#[test]
fn a_packed_snapshot_loads_as_the_end_pass_does() {
    let mut graph = ruled_graph();
    later_changes(&mut graph);
    let mut bytes = Vec::new();
    graph.write_snapshot(&mut bytes).unwrap();
    let direct = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    let reference =
        TraversalGraph::with_load_post_pass(|| TraversalGraph::read_snapshot(bytes.as_slice()))
            .unwrap();
    assert_same_load(&direct, &reference, &later_rules(), "packed bytes");
    // The coded rows hold what the live graph coded when its rules were defined.
    assert_eq!(
        direct.compression_stats("n"),
        graph.compression_stats("n"),
        "live statistics"
    );
    // A `.gsnap` file.
    let path = std::env::temp_dir().join(format!(
        "graphersal-compression-load-{}-{:?}.gsnap",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&path, &bytes).unwrap();
    let options = ReadOptions::new();
    let (direct, _) = graphersal::persist::read_snapshot_file(&path, &options).unwrap();
    let (reference, _) = TraversalGraph::with_load_post_pass(|| {
        graphersal::persist::read_snapshot_file(&path, &options)
    })
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_same_load(
        &direct.read(),
        &reference.read(),
        &later_rules(),
        ".gsnap file",
    );
}

#[test]
fn a_store_whose_rules_are_all_in_the_wal_replays_them() {
    // No rule in the snapshot: the WAL defines them after the data (the live pass codes the
    // rows during the replay).
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    {
        let graph = store.graph();
        let mut graph = graph.write();
        add_data(&mut graph, 0, ITEMS, 0, EDGES);
    }
    store.checkpoint(None).unwrap();
    {
        let graph = store.graph();
        let mut graph = graph.write();
        define_rules(&mut graph);
        later_changes(&mut graph);
    }
    store.close().unwrap();
    assert_open_same(&dir, "rules in the WAL");
}

#[test]
fn rules_without_elements_and_elements_without_rules_load_alike() {
    // Rules whose labels hold nothing.
    let mut empty = TraversalGraph::new();
    define_rules(&mut empty);
    let mut bytes = Vec::new();
    empty.write_snapshot(&mut bytes).unwrap();
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    assert_eq!(
        loaded.compression_stats("kind"),
        empty.compression_stats("kind")
    );
    assert!(loaded.check_compression_invariant().is_ok());
    // Elements whose labels no rule names, beside a rule.
    let mut graph = TraversalGraph::new();
    add_items(&mut graph, 0, 30);
    graph
        .traversal_mut()
        .define_compression("other", dedup(ElementKind::Vertex, "other", "kind"))
        .unwrap();
    let mut bytes = Vec::new();
    graph.write_snapshot(&mut bytes).unwrap();
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    assert!(loaded.check_compression_invariant().is_ok());
    assert!(!loaded.is_dedup_coded(ElementKind::Vertex, "i0", "kind"));
    assert_same_text(
        &sorted(exact(&loaded)),
        &sorted(exact(&graph)),
        "no rule applies",
    );
    assert_eq!(
        loaded.memory_usage().unwrap().unwrap(),
        TraversalGraph::with_load_post_pass(|| TraversalGraph::read_snapshot(bytes.as_slice()))
            .unwrap()
            .memory_usage()
            .unwrap()
            .unwrap()
    );
}

#[test]
fn a_coded_key_whose_rule_turns_lz4_is_compressed_in_the_same_pass() {
    // The end pass and every rule change normalize a coded row by its own values only while
    // its coded values keep their rule: a key whose dedup rule becomes an lz4 rule is decoded
    // and compressed.
    let mut graph = TraversalGraph::new();
    add_data(&mut graph, 0, 40, 0, 0);
    graph
        .traversal_mut()
        .define_compression("note", dedup(ElementKind::Vertex, "item", "note"))
        .unwrap();
    graph
        .traversal_mut()
        .define_compression("text", lz4(ElementKind::Vertex, "item", "text"))
        .unwrap();
    assert!(graph.is_dedup_coded(ElementKind::Vertex, "i1", "note"));
    let plain = sorted(exact(&graph));
    graph
        .traversal_mut()
        .define_compression("note", lz4(ElementKind::Vertex, "item", "note"))
        .unwrap();
    assert!(graph.check_compression_invariant().is_ok());
    assert!(!graph.is_dedup_coded(ElementKind::Vertex, "i1", "note"));
    let stats = graph.compression_stats("note").unwrap();
    assert!(stats.compressed_values > 0, "{stats:?}");
    assert_same_text(&sorted(exact(&graph)), &plain, "content");
    // And back: the lz4 values are coded again.
    graph
        .traversal_mut()
        .define_compression("note", dedup(ElementKind::Vertex, "item", "note"))
        .unwrap();
    assert!(graph.check_compression_invariant().is_ok());
    assert!(graph.is_dedup_coded(ElementKind::Vertex, "i1", "note"));
    assert_same_text(&sorted(exact(&graph)), &plain, "content");
}

/// What the replay must reproduce of `graph`: every vertex with its properties and which of them
/// are stored coded (the order of a vertex's edges depends on how the graph was built, so an
/// export of a live graph and of a loaded one may differ).
fn stored_state(graph: &TraversalGraph) -> Vec<(String, PropertyMap, Vec<bool>)> {
    ids(graph)
        .0
        .into_iter()
        .map(|id| {
            let vertex = GraphStorage::get_vertex_by_id(graph, &id).unwrap().unwrap();
            let properties = graph.vertex_properties(vertex).unwrap().unwrap().clone();
            let coded = ITEM_KEYS
                .iter()
                .map(|key| graph.is_dedup_coded(ElementKind::Vertex, &id, key))
                .collect();
            (id, properties, coded)
        })
        .collect()
}

/// A WAL of rule changes in many commits, mixed with element writes: every open and every
/// history view is the graph the live commits made (the replay defers the rule passes and walks
/// once per run of rule changes, a store open also skips the rebuild of what it just filled).
#[test]
fn rule_changes_of_many_commits_replay_to_the_state_the_live_commits_made() {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let graph = Arc::clone(store.graph());
    add_data(&mut graph.write(), 0, ITEMS, 0, EDGES);
    store.checkpoint(None).unwrap();
    let mut states = Vec::new();
    type Step = Box<dyn Fn(&mut TraversalGraph)>;
    let define = |name: &'static str, rule: CompressionDefinition| -> Step {
        Box::new(move |g: &mut TraversalGraph| {
            g.set_definition(Definition::compression(name, rule.clone()))
                .unwrap();
        })
    };
    let drop_rule = |name: &'static str| -> Step {
        Box::new(move |g: &mut TraversalGraph| {
            g.remove_definition(DefinitionKind::COMPRESSION, name)
                .unwrap();
        })
    };
    let steps: Vec<Step> = vec![
        define("r_kind", dedup(ElementKind::Vertex, "item", "kind")),
        define("r_n", dedup(ElementKind::Vertex, "item", "n")),
        define("r_text", lz4(ElementKind::Vertex, "item", "text")),
        // An element write between rule commits meets the rules before it.
        Box::new(|g: &mut TraversalGraph| {
            g.traversal_mut()
                .v(vec!["i1", "i2", "i3"])
                .property("kind", "written")
                .property("n", 42i64)
                .to_list()
                .unwrap();
        }),
        define("r_flag", dedup(ElementKind::Vertex, "item", "flag")),
        // Stored and removed in a run of rule commits.
        define("r_gone", dedup(ElementKind::Vertex, "item", "day")),
        drop_rule("r_gone"),
        // Removed, then a rule of another codec on the same key.
        drop_rule("r_n"),
        define("r_n2", lz4(ElementKind::Vertex, "item", "n")),
        // Removed and stored again unchanged: the same rule.
        drop_rule("r_kind"),
        define("r_kind", dedup(ElementKind::Vertex, "item", "kind")),
    ];
    for step in &steps {
        let mut g = graph.write();
        step(&mut g);
        let seq = g.last_commit_seq();
        states.push((seq, stored_state(&g)));
    }
    let (_, last) = states.last().unwrap().clone();
    store.close().unwrap();
    let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
    {
        let g = reopened.graph().read();
        g.check_compression_invariant().unwrap();
        assert_eq!(stored_state(&g), last, "the open");
    }
    reopened.close().unwrap();
    for (seq, state) in &states {
        let view = Store::open_read_only(&dir, RecoveryTarget::CommitSeq(*seq)).unwrap();
        view.graph.check_compression_invariant().unwrap();
        assert_eq!(
            stored_state(&view.graph),
            *state,
            "the view at commit {seq}"
        );
    }
}

/// Keeps the change set of every commit.
struct KeepCommits(Arc<std::sync::Mutex<Vec<graphersal::changes::ChangeSet>>>);

impl graphersal::changes::CommitHook for KeepCommits {
    fn before_commit(
        &mut self,
        changes: &graphersal::changes::ChangeSet,
    ) -> Result<(), graphersal::changes::HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

/// Rule changes, batched and one by one, mixed with an element write: a repair, the public
/// replay, a replay of many change sets and a live apply all reach the graph the commits made,
/// each with its own way of walking once per run of rule changes.
#[test]
fn rule_changes_reach_the_same_state_through_every_way_of_applying_them() {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let graph = Arc::clone(store.graph());
    add_data(&mut graph.write(), 0, ITEMS, 0, EDGES);
    store.checkpoint(None).unwrap();
    let base = || {
        let mut graph = TraversalGraph::new();
        add_data(&mut graph, 0, ITEMS, 0, EDGES);
        graph
    };

    let commits = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let mut g = graph.write();
        g.register_commit_hook(Box::new(KeepCommits(Arc::clone(&commits))))
            .unwrap();
        g.set_definitions(vec![
            Definition::compression("r_kind", dedup(ElementKind::Vertex, "item", "kind")),
            Definition::compression("r_n", dedup(ElementKind::Vertex, "item", "n")),
            Definition::compression("r_text", lz4(ElementKind::Vertex, "item", "text")),
        ])
        .unwrap();
        g.set_definition(Definition::compression(
            "r_flag",
            dedup(ElementKind::Vertex, "item", "flag"),
        ))
        .unwrap();
        g.traversal_mut()
            .v(vec!["i1", "i2", "i3"])
            .property("kind", "written")
            .property("n", 42i64)
            .to_list()
            .unwrap();
        g.remove_definitions(&[
            (DefinitionKind::COMPRESSION, "r_n"),
            (DefinitionKind::COMPRESSION, "r_flag"),
        ])
        .unwrap();
        g.set_definition(Definition::compression(
            "r_n2",
            lz4(ElementKind::Vertex, "item", "n"),
        ))
        .unwrap();
    }
    let live = stored_state(&graph.read());
    let commits = commits.lock().unwrap().clone();
    assert_eq!(commits.len(), 5);

    let mut replica = base();
    for changes in &commits {
        replica = replica.replay(changes).unwrap();
    }
    replica.check_compression_invariant().unwrap();
    assert_eq!(stored_state(&replica), live, "the public replay");

    use graphersal::storage::PersistentStorage;
    let mut replica = base();
    replica.begin_replay().unwrap();
    for changes in &commits {
        PersistentStorage::replay(&mut replica, changes, true).unwrap();
    }
    replica.finish_replay().unwrap();
    replica.check_compression_invariant().unwrap();
    assert_eq!(stored_state(&replica), live, "a replay of many change sets");

    let mut replica = base();
    for changes in &commits {
        replica.apply_change_set(changes).unwrap();
    }
    replica.check_compression_invariant().unwrap();
    assert_eq!(stored_state(&replica), live, "a live apply");

    store.close().unwrap();
    let repaired = MemDir::new();
    Store::repair_dir(&dir, &repaired).unwrap();
    let reopened = Store::open(&repaired, StoreOptions::new()).unwrap();
    let g = reopened.graph().read();
    g.check_compression_invariant().unwrap();
    assert_eq!(stored_state(&g), live, "a repair");
}
