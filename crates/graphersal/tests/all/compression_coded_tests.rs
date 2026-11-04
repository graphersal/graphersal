//! The extended `dedup` codec on top-level keys (book page "Compressed Properties", section
//! "Deduplicating repeated values"; stage V1 of the compact-memory track): every value at a
//! rule's top-level key, of ANY type, is stored once in the rule's value dictionary and the
//! element's row holds a 4-byte code. Reads stay plain and exact (types, offsets, key order),
//! query results and files never change. Covers every value type, in-place writes, key-adding
//! and key-removing writes, jpath writes on coded values, undo and rollback order, define /
//! drop / recompress, label changes, the dictionary bound under overwrites, persistence (Store
//! WAL replay, checkpoint, backup), change sets, property indexes, memory accounting, the
//! stored form of `min_bytes` 0, an existing rule's `min_bytes`, the advisor
//! and a differential run of every documented DSL example with dedup rules on every key.
#![cfg(all(feature = "script", feature = "io"))]

use std::sync::{Arc, Mutex};

use graphersal::advisor::{AdviceRequest, RecommendedAction};
use graphersal::catalog::{Codec, CompressionDefinition, Definition, PropertyIndexDefinition};
use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::prelude::*;
use graphersal::storage::ElementKind;
use graphersal::temporal::{Temporal, TemporalKind};

use crate::script_host::*;

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

/// An object value with entries in this order.
fn object(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    ElementProperty::Object(Box::new(props(entries)))
}

/// The keys the rules code, one per value type.
const CODED_KEYS: [&str; 11] = [
    "kind", "n", "f", "flag", "day", "at", "tags", "meta", "uid", "note", "nothing",
];

/// The properties of item `i`: every value type at a coded key (few distinct values each),
/// plus `name` (unique, no rule).
fn item(i: usize) -> PropertyMap {
    let mut entries = vec![
        ("name", ElementProperty::from(format!("item-{i}"))),
        ("kind", ElementProperty::from(["a", "bb", "ccc", ""][i % 4])),
        ("n", ElementProperty::from((i % 5) as i64)),
        // 1.0 is not the int 1: identity keeps the type.
        ("f", ElementProperty::from([1.0f64, 2.5, 1.0][i % 3])),
        ("flag", ElementProperty::Boolean(i % 2 == 0)),
        (
            "day",
            temporal(TemporalKind::Date, ["2024-01-01", "2024-02-29"][i % 2]),
        ),
        // One instant at two offsets: two values (identity keeps the offset).
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
        // The same entries in another order: two values.
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
        (
            "note",
            ElementProperty::from(format!(
                "a note that is long enough to own heap memory, number {}",
                i % 6
            )),
        ),
        ("nothing", ElementProperty::Null),
    ];
    // Some items lack a coded key (other shapes).
    if i % 7 == 3 {
        entries.retain(|(key, _)| *key != "n");
    }
    props(entries)
}

/// `n` items and `n / 2` edges, no rules.
fn plain_graph(n: usize) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for i in 0..n {
        graph
            .add_vertex(Some(format!("i{i}").into()), vec!["item".into()], item(i))
            .unwrap();
    }
    for i in 0..n / 2 {
        graph
            .add_edge_by_ids(
                Some(format!("r{i}").into()),
                Some("rel".into()),
                &format!("i{i}"),
                &format!("i{}", (i * 3 + 1) % n),
                props(vec![
                    ("w", ElementProperty::from([0.5f64, 1.5][i % 2])),
                    ("kind", ElementProperty::from(["near", "far"][i % 2])),
                ]),
            )
            .unwrap();
    }
    graph
}

fn dedup(element: ElementKind, label: &str, key: &str) -> CompressionDefinition {
    CompressionDefinition::new(element, label, key)
        .unwrap()
        .with_codec(Codec::Dedup)
}

/// A dedup rule on every coded key of the items and on both edge keys.
fn define_rules(graph: &mut TraversalGraph) {
    let mut g = graph.traversal_mut();
    for key in CODED_KEYS {
        g.define_compression(key, dedup(ElementKind::Vertex, "item", key))
            .unwrap();
    }
    g.define_compression("rel_w", dedup(ElementKind::Edge, "rel", "w"))
        .unwrap();
    g.define_compression("rel_kind", dedup(ElementKind::Edge, "rel", "kind"))
        .unwrap();
    drop(g);
}

fn coded_graph(n: usize) -> TraversalGraph {
    let mut graph = plain_graph(n);
    define_rules(&mut graph);
    graph
}

fn invariant(graph: &TraversalGraph) {
    if let Err(reason) = graph.check_compression_invariant() {
        panic!("invariant broken: {reason}");
    }
}

/// The exact content: the GraphSON export (typed values, offsets, key order, element order).
fn exact(graph: &TraversalGraph) -> String {
    let mut out = Vec::new();
    graph.traversal().export_graphson_writer(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// Asserts two exports are equal, naming the first line that differs.
#[track_caller]
fn assert_same(a: &str, b: &str, what: &str) {
    if a == b {
        return;
    }
    for (x, y) in a.lines().zip(b.lines()) {
        assert_eq!(x, y, "{what}");
    }
    assert_eq!(a.lines().count(), b.lines().count(), "{what}");
}

fn shared(graph: TraversalGraph) -> Arc<Graph> {
    Arc::new(Graph::new(graph))
}

fn coded(graph: &TraversalGraph, id: &str, key: &str) -> bool {
    graph.is_dedup_coded(ElementKind::Vertex, id, key)
}

// ---------------------------------------------------------------------------------------------
// 1. Every value type, read back exactly
// ---------------------------------------------------------------------------------------------

#[test]
fn every_value_type_is_coded_and_reads_back_exactly() {
    let graph = coded_graph(60);
    invariant(&graph);
    for key in CODED_KEYS {
        assert!(coded(&graph, "i0", key), "{key}");
    }
    assert!(!coded(&graph, "i0", "name"), "no rule on name");
    assert!(graph.is_dedup_coded(ElementKind::Edge, "r0", "w"));
    assert_same(&exact(&graph), &exact(&plain_graph(60)), "coded vs plain");
    // Identity: 1.0 and 1 are two values, one instant at two offsets two, one object in two key
    // orders two.
    assert_eq!(graph.compression_stats("f").unwrap().distinct_values, 2);
    assert_eq!(graph.compression_stats("at").unwrap().distinct_values, 2);
    assert_eq!(graph.compression_stats("meta").unwrap().distinct_values, 2);
    assert_eq!(graph.compression_stats("kind").unwrap().distinct_values, 4);
    assert_eq!(
        graph.compression_stats("nothing").unwrap().distinct_values,
        1
    );
    let n = graph.compression_stats("n").unwrap();
    assert_eq!(n.distinct_values, 5);
    assert_eq!(
        n.compressed_values,
        60 - 9,
        "items without n are not counted"
    );
    // Queries answer exactly as without rules.
    for script in [
        r#"g.v().has_label("item").values("n").sum().to_list()"#,
        r#"g.v().has_label("item").has("kind", "bb").count().to_list()"#,
        r#"g.v().has_label("item").has("n", P.gt(2)).values("name").to_list()"#,
        r#"g.v().has_label("item").order().by("note").by("name").values("name").limit(5).to_list()"#,
        r#"g.v().has_label("item").values("tags").unfold().dedup().to_list()"#,
        r#"g.v().has_label("item").value_map().limit(3).to_list()"#,
        r#"g.v().has_label("item").element_map().limit(3).to_list()"#,
        r#"g.v().has_label("item").group_count().by("at").to_list()"#,
        r#"g.v().has_label("item").values(jpath("meta.b")).dedup().to_list()"#,
        r#"g.e().has_label("rel").values("w").sum().to_list()"#,
        r#"g.v().has("flag", true).out("rel").values("kind").dedup().to_list()"#,
    ] {
        let with = eval_value(shared(coded_graph(60)), script).unwrap();
        let without = eval_value(shared(plain_graph(60)), script).unwrap();
        assert_eq!(format!("{with:?}"), format!("{without:?}"), "{script}");
    }
}

#[test]
fn memory_drops_and_the_report_reconciles() {
    let plain = plain_graph(400);
    let graph = coded_graph(400);
    let before = plain.memory_usage().unwrap().unwrap();
    let after = graph.memory_usage().unwrap().unwrap();
    assert!(
        after.property_bytes < before.property_bytes,
        "{after:?} vs {before:?}"
    );
    assert!(after.deduped_values > 4000, "{after:?}");
    assert!(after.dedup_table_bytes > 0, "{after:?}");
    for g in [&plain, &graph] {
        let report = g
            .traversal()
            .memory_report(&graphersal::storage::MemoryReportRequest::new().with_limit(usize::MAX))
            .unwrap();
        assert_eq!(
            report.property_bytes(),
            g.memory_usage().unwrap().unwrap().property_bytes,
            "the report attributes the property bytes of memory_usage"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 2. Writes, undo and rollback
// ---------------------------------------------------------------------------------------------

#[test]
fn writes_of_every_kind_keep_the_rows_coded_and_the_content_exact() {
    let shared_coded = shared(coded_graph(40));
    let shared_plain = shared(plain_graph(40));
    for script in [
        // In place: a coded key, a new value and an existing one.
        r#"g.v("i1").property("kind", "brand new").iterate()"#,
        r#"g.v("i2").property("n", 4).iterate()"#,
        // In place: an own key of a coded row.
        r#"g.v("i3").property("name", "renamed").iterate()"#,
        // A value the row codes, of another type at the same key.
        r#"g.v("i4").property("n", "four").iterate()"#,
        // A new key and a removed key (the row changes shape).
        r#"g.v("i5").property("extra", 1).iterate()"#,
        r#"g.v("i6").properties("tags").drop().iterate()"#,
        // jpath writes on a coded object: the shared value is not changed for the others.
        r#"g.v("i8").property(jpath("meta.a"), 99).iterate()"#,
        r#"g.v("i10").property(jpath("tags[0]"), "y").iterate()"#,
        r#"g.v("i12").remove_property(jpath("meta.b")).iterate()"#,
        // An edge.
        r#"g.e("r1").property("w", 9.5).iterate()"#,
        // A new element.
        r#"g.add_v("item").property("kind", "a").property("n", 1).iterate()"#,
        // A label change: the rules of the new label apply.
        r#"g.v("i14").add_label("other").iterate()"#,
        // Drops.
        r#"g.v("i15").drop().iterate()"#,
        r#"g.e("r2").drop().iterate()"#,
    ] {
        eval_value(shared_coded.clone(), script).unwrap();
        eval_value(shared_plain.clone(), script).unwrap();
        invariant(&shared_coded.read());
        assert_same(
            &exact(&shared_coded.read()),
            &exact(&shared_plain.read()),
            script,
        );
    }
    let graph = shared_coded.read();
    assert!(coded(&graph, "i1", "kind"));
    assert!(coded(&graph, "i4", "n"), "a string at n is coded too");
    assert!(coded(&graph, "i5", "kind"));
    assert!(coded(&graph, "i8", "meta"));
    assert!(coded(&graph, "i14", "kind"));
}

#[test]
fn a_rollback_restores_values_and_key_order_exactly() {
    let graph = shared(coded_graph(30));
    let oracle = exact(&graph.read());
    let failing = r#"
        g.v("i1").property("kind", "changed").iterate();
        g.v("i1").property("n", 77).iterate();
        g.v("i2").properties("kind").drop().iterate();
        g.v("i2").property("kind", "back at the end").iterate();
        g.v("i3").property(jpath("meta.c"), 3).iterate();
        g.v("i4").property("fresh", "value").iterate();
        g.v("i5").drop().iterate();
        g.add_v("item").property("kind", "new").iterate();
        g.e("r1").property("w", 2.25).iterate();
        throw "stop";
    "#;
    assert!(
        eval_value_atomic(
            graph.clone(),
            failing,
            std::collections::HashMap::new(),
            &ScriptLimits::default()
        )
        .is_err()
    );
    invariant(&graph.read());
    assert_eq!(exact(&graph.read()), oracle);
    // The dictionaries hold no value the rollback undid.
    let graph = graph.read();
    assert_eq!(
        graph.dedup_table_len("kind").unwrap() as u64,
        graph.compression_stats("kind").unwrap().distinct_values
    );
}

#[test]
fn change_sets_carry_plain_values_before_and_after() {
    struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);
    impl CommitHook for Recorder {
        fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
            self.0.lock().unwrap().push(changes.clone());
            Ok(())
        }
    }
    let mut graph = coded_graph(20);
    let seen = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    for script in [
        r#"g.v("i1").property("kind", "other").iterate()"#,
        r#"g.v("i2").property("meta", "plain now").iterate()"#,
        r#"g.v("i3").properties("tags").drop().iterate()"#,
        r#"g.v("i4").drop().iterate()"#,
    ] {
        let shared = shared(std::mem::take(&mut graph));
        eval_value(shared.clone(), script).unwrap();
        graph = Arc::try_unwrap(shared).ok().unwrap().into_inner();
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    let first = format!("{:?}", seen[0].mutations());
    assert!(first.contains("\"bb\""), "the old value: {first}");
    assert!(first.contains("\"other\""), "{first}");
    for changes in seen.iter() {
        for mutation in changes.mutations() {
            if let Mutation::SetProperty { before, after, .. } = mutation {
                assert!(!after.holds_compressed());
                assert!(before.as_ref().is_none_or(|b| !b.holds_compressed()));
            }
        }
    }
    let removed = format!("{:?}", seen[2].mutations());
    assert!(removed.contains("\"x\""), "the removed array: {removed}");
    // A replica replaying the change sets ends up with the same content.
    let mut replica = coded_graph(20);
    for changes in seen.iter() {
        replica = replica.replay(changes).unwrap();
    }
    invariant(&replica);
    assert_eq!(exact(&replica), exact(&graph));
}

// ---------------------------------------------------------------------------------------------
// 3. Rules: define, drop, recompress, min_bytes, the stored marker
// ---------------------------------------------------------------------------------------------

#[test]
fn define_drop_and_recompress_code_and_uncode_the_rows() {
    let mut graph = plain_graph(30);
    let oracle = exact(&graph);
    define_rules(&mut graph);
    assert!(coded(&graph, "i1", "kind"));
    graph
        .traversal_mut()
        .remove_definition(graphersal::catalog::DefinitionKind::COMPRESSION, "kind")
        .unwrap();
    assert!(!coded(&graph, "i1", "kind"));
    assert!(coded(&graph, "i1", "n"), "other rules stay");
    invariant(&graph);
    assert_same(&exact(&graph), &oracle, "after the change");
    // Overwrites leave unused entries; recompress rebuilds the dictionary from the rows.
    let shared = shared(graph);
    for i in 0..20 {
        eval_value(
            shared.clone(),
            &format!(
                r#"g.v("i1").property("note", "a temporary long note number {i} of i1").iterate()"#
            ),
        )
        .unwrap();
    }
    let mut graph = Arc::try_unwrap(shared).ok().unwrap().into_inner();
    assert!(graph.dedup_table_len("note").unwrap() > 7);
    graph.traversal_mut().recompress("note").unwrap();
    assert_eq!(
        graph.dedup_table_len("note").unwrap() as u64,
        graph.compression_stats("note").unwrap().distinct_values
    );
    invariant(&graph);
}

#[test]
fn a_min_bytes_rule_keeps_short_strings_in_the_row() {
    // An existing rule keeps its stored min_bytes (25 from 0.2.1 on): short strings stay own,
    // every other type is coded.
    let mut graph = plain_graph(20);
    graph
        .traversal_mut()
        .define_compression(
            "note",
            dedup(ElementKind::Vertex, "item", "note").with_min_bytes(25),
        )
        .unwrap();
    graph
        .traversal_mut()
        .define_compression(
            "n",
            dedup(ElementKind::Vertex, "item", "n").with_min_bytes(25),
        )
        .unwrap();
    graph
        .traversal_mut()
        .define_compression(
            "kind",
            dedup(ElementKind::Vertex, "item", "kind").with_min_bytes(25),
        )
        .unwrap();
    assert!(coded(&graph, "i0", "note"));
    assert!(
        coded(&graph, "i0", "n"),
        "a number is coded whatever min_bytes says"
    );
    assert!(
        !coded(&graph, "i0", "kind"),
        "a short string stays in the row"
    );
    invariant(&graph);
    // A long value written to a short-string key is coded, a short one written back is not.
    let shared = shared(graph);
    eval_value(
        shared.clone(),
        r#"g.v("i1").property("kind", "a kind that is long enough to code").iterate()"#,
    )
    .unwrap();
    assert!(coded(&shared.read(), "i1", "kind"));
    eval_value(
        shared.clone(),
        r#"g.v("i1").property("kind", "a").iterate()"#,
    )
    .unwrap();
    assert!(!coded(&shared.read(), "i1", "kind"));
    invariant(&shared.read());
}

#[test]
fn a_rule_with_min_bytes_zero_is_stored_as_it_is() {
    let rule = dedup(ElementKind::Vertex, "item", "kind");
    assert_eq!(rule.min_bytes(), 0);
    assert_eq!(rule.codec().name(), "dedup");
    let payload = rule.to_payload();
    assert_eq!(payload.get("codec"), Some(&ElementProperty::from("dedup")));
    assert_eq!(payload.get("min_bytes"), Some(&ElementProperty::from(0i64)));
    let back = CompressionDefinition::from_payload(&payload).unwrap();
    assert_eq!(back, rule);
    assert!(back.unserved_reason().is_none());
    // A rule an older version wrote (min_bytes 25) reads back with its threshold.
    let mut old = PropertyMap::default();
    old.insert("label", "item".into());
    old.insert("path", "kind".into());
    old.insert("codec", "dedup".into());
    old.insert("min_bytes", ElementProperty::from(25i64));
    old.insert("dictionary", ElementProperty::Boolean(false));
    let old = CompressionDefinition::from_payload(&old).unwrap();
    assert_eq!(old.min_bytes(), 25);
    assert_eq!(old.codec(), Codec::Dedup);
    // The exchange form (catalog file) round trips too.
    let json =
        Definition::compression("kind", dedup(ElementKind::Vertex, "item", "kind")).to_json();
    assert_eq!(
        Definition::from_json(&json).unwrap(),
        Definition::compression("kind", dedup(ElementKind::Vertex, "item", "kind"))
    );
    // lz4 still refuses 0.
    let lz4 = CompressionDefinition::new(ElementKind::Vertex, "item", "note")
        .unwrap()
        .with_min_bytes(0);
    assert!(
        TraversalGraph::new()
            .traversal_mut()
            .define_compression("note", lz4)
            .is_err()
    );
}

#[test]
fn overwrites_in_many_units_keep_the_dictionary_bounded() {
    let graph = shared(coded_graph(10));
    for i in 0..1500 {
        eval_value(
            graph.clone(),
            &format!(r#"g.v("i2").property("n", {})"#, 1000 + i),
        )
        .unwrap();
        let g = graph.read();
        let live = g.compression_stats("n").unwrap().distinct_values as usize;
        let len = g.dedup_table_len("n").unwrap();
        assert!(
            len <= (2 * live).max(256),
            "{i}: {len} entries, {live} live"
        );
    }
    invariant(&graph.read());
}

// ---------------------------------------------------------------------------------------------
// 4. Persistence: the disk holds plain values, a load codes again
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
#[test]
fn a_store_replays_checkpoints_and_backs_up_coded_rules() {
    use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreOptions};
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    {
        let graph = store.graph();
        let mut graph = graph.write();
        for i in 0..40 {
            graph
                .traversal_mut()
                .add_v("item")
                .property("kind", ["a", "bb"][i % 2])
                .property("n", (i % 3) as i64)
                .property(
                    "note",
                    format!("a note long enough to share {}", i % 4).as_str(),
                )
                .to_list()
                .unwrap();
        }
        for key in ["kind", "n", "note"] {
            graph
                .traversal_mut()
                .define_compression(key, dedup(ElementKind::Vertex, "item", key))
                .unwrap();
        }
        graph
            .traversal_mut()
            .add_v("item")
            .property("kind", "ccc")
            .property("n", 9i64)
            .to_list()
            .unwrap();
    }
    // A load may store the elements in another slot order: compare the sorted lines.
    // The export numbers the vertex properties in slot order: their ids are left out.
    let sorted = |text: String| {
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
    };
    let oracle = sorted(exact(&store.graph().read()));
    let stats = store.graph().read().compression_stats("n").unwrap();
    store.close().unwrap();
    let check = |graph: &TraversalGraph| {
        invariant(graph);
        assert_same(&sorted(exact(graph)), &oracle, "after the load");
        assert_eq!(graph.compression_stats("n").unwrap(), stats);
    };
    // WAL replay (the rule in the WAL).
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    check(&store.graph().read());
    // A checkpoint (the rule in the snapshot), then a load.
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    check(&store.graph().read());
    // A backup restores the same.
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let restored = Store::open_backup(&backup, StoreOptions::new()).unwrap();
    check(&restored.graph().read());
    restored.close().unwrap();
    let read_only = Store::open_read_only(&dir, RecoveryTarget::Latest).unwrap();
    check(&read_only.graph);
}

// ---------------------------------------------------------------------------------------------
// 5. Property indexes on coded keys
// ---------------------------------------------------------------------------------------------

#[test]
fn a_property_index_on_a_coded_key_serves_and_follows_writes() {
    let mut graph = coded_graph(50);
    graph
        .traversal_mut()
        .set_definition(Definition::property_index(
            "item_kind",
            PropertyIndexDefinition::new(ElementKind::Vertex, "item", "kind"),
        ))
        .unwrap();
    let mut plain = plain_graph(50);
    plain
        .traversal_mut()
        .set_definition(Definition::property_index(
            "item_kind",
            PropertyIndexDefinition::new(ElementKind::Vertex, "item", "kind"),
        ))
        .unwrap();
    let (graph, plain) = (shared(graph), shared(plain));
    for script in [
        r#"g.v("i1").property("kind", "a").iterate()"#,
        r#"g.v("i2").property("kind", "zzz").iterate()"#,
        r#"g.v("i3").properties("kind").drop().iterate()"#,
    ] {
        eval_value(graph.clone(), script).unwrap();
        eval_value(plain.clone(), script).unwrap();
    }
    for kind in ["a", "bb", "zzz", ""] {
        let query =
            format!(r#"g.v().has_label("item").has("kind", "{kind}").values("name").to_list()"#);
        let with = eval_value(graph.clone(), &query).unwrap();
        let without = eval_value(plain.clone(), &query).unwrap();
        assert_eq!(format!("{with:?}"), format!("{without:?}"), "{kind}");
    }
    let profile = eval_value(
        graph.clone(),
        r#"g.v().has_label("item").has("kind", "a").count().profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(profile.contains("[index: vertex item.kind]"), "{profile}");
    invariant(&graph.read());
}

// ---------------------------------------------------------------------------------------------
// 6. The advisor and the estimate
// ---------------------------------------------------------------------------------------------

/// `n` "row" vertices: `state` with 4 distinct numbers (repeats), `serial` unique.
fn repeated_graph(n: usize) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for i in 0..n {
        graph
            .add_vertex(
                None,
                vec!["row".into()],
                props(vec![
                    ("state", ElementProperty::from((i % 4) as i64)),
                    ("serial", ElementProperty::from(i as i64)),
                ]),
            )
            .unwrap();
    }
    graph
}

#[test]
fn the_advisor_recommends_dedup_for_repeated_values_of_any_type() {
    // 50,000 values, 4 distinct: about 1.3 MiB saved.
    let graph = Graph::new(repeated_graph(50_000));
    let advice = graph.advise(&AdviceRequest::new()).unwrap();
    let ids: Vec<&str> = advice
        .recommendations
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    let rule = advice
        .recommendations
        .iter()
        .find(|r| r.id == "add_compression:vertex:row:state")
        .unwrap_or_else(|| panic!("{ids:?}"));
    let RecommendedAction::AddCompression { candidate, .. } = &rule.action else {
        panic!("{rule:?}");
    };
    assert_eq!(candidate.codec, Codec::Dedup);
    assert_eq!(candidate.min_bytes, 0);
    assert!(rule.memory_bytes >= 1024 * 1024, "{rule:?}");
    assert!(rule.evidence.contains("MiB"), "{}", rule.evidence);
    assert!(rule.evidence.contains("4 distinct"), "{}", rule.evidence);
    assert!(rule.cost.contains("writes"), "{}", rule.cost);
    // Unique values are never advised dedup.
    assert!(!ids.iter().any(|id| id.ends_with(":serial")), "{ids:?}");
    // Applying it saves what it promised (within 2 %).
    let before = graph.read().memory_usage().unwrap().unwrap().property_bytes;
    eval_value(Arc::new(graph), "1").unwrap();
    let mut applied = repeated_graph(50_000);
    applied
        .traversal_mut()
        .define_compression("row_state", dedup(ElementKind::Vertex, "row", "state"))
        .unwrap();
    let after = applied.memory_usage().unwrap().unwrap().property_bytes;
    let saved = (before - after) as i64;
    assert!(
        (saved - rule.memory_bytes).abs() * 50 <= rule.memory_bytes,
        "promised {} saved {saved}",
        rule.memory_bytes
    );
}

#[test]
fn the_advisor_does_not_recommend_dedup_below_a_mebibyte() {
    // 10,000 values save about 270 KiB: below the 1 MiB threshold.
    let graph = Graph::new(repeated_graph(10_000));
    let advice = graph.advise(&AdviceRequest::new()).unwrap();
    assert!(
        !advice
            .recommendations
            .iter()
            .any(|r| r.id.starts_with("add_compression")),
        "{:?}",
        advice.recommendations
    );
}

#[test]
fn the_estimate_counts_every_value_type_at_a_top_level_key() {
    let graph = plain_graph(100);
    let shared = shared(graph);
    let estimate = eval_value(
        shared.clone(),
        r#"let e = g.estimate_compression(#{label: "item", path: "tags", codec: "dedup"});
           let c = e.candidates[0];
           [c.min_bytes, c.compressed_values, c.distinct_values, c.saved_bytes > 0, e.warnings.len()]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(estimate, "[0, 100, 3, true, 0]");
    // The estimate is the rule.
    let promised = eval_value(
        shared.clone(),
        r#"let e = g.estimate_compression(#{label: "item", path: "meta", codec: "dedup", min_bytes: 0});
           let b = e.candidates[0];
           g.define_compression(#{name: "meta", label: "item", path: "meta", codec: "dedup"});
           let r = g.compressions().filter(|r| r.name == "meta")[0];
           [b.stored_bytes == r.stored_bytes, b.plain_bytes == r.plain_bytes, b.distinct_values == r.distinct_values]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(promised, "[true, true, true]");
}

// ---------------------------------------------------------------------------------------------
// 7. Differential: every documented DSL example with dedup rules on every key
// ---------------------------------------------------------------------------------------------

/// The modern graph with a dedup rule on every `(label, key)` it holds.
fn modern_with_rules() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for (element, label, key) in [
        (ElementKind::Vertex, "person", "name"),
        (ElementKind::Vertex, "person", "age"),
        (ElementKind::Vertex, "software", "name"),
        (ElementKind::Vertex, "software", "lang"),
        (ElementKind::Edge, "knows", "weight"),
        (ElementKind::Edge, "created", "weight"),
    ] {
        graph
            .traversal_mut()
            .define_compression(&format!("{label}_{key}"), dedup(element, label, key))
            .unwrap();
    }
    graph
}

/// `(script).to_string()` on `graph`, or the error text.
fn render(graph: TraversalGraph, script: &str) -> Result<String, String> {
    eval_value_with_limits(
        shared(graph),
        &format!("({{ {script} }}).to_string()"),
        std::collections::HashMap::new(),
        &ScriptLimits::default(),
    )
    .map(|value| value.to_string())
    .map_err(|err| err.to_string())
}

#[test]
fn every_documented_example_gives_the_same_result_with_dedup_rules_on_every_key() {
    // Examples whose answer names the catalog or the memory (the rules are there, and they
    // change the figures on purpose).
    let about_rules_or_memory = |name: &str| {
        [
            "compression",
            "compress",
            "memory",
            "definition",
            "catalog",
            "advise",
            "shrink",
        ]
        .iter()
        .any(|word| name.contains(word))
    };
    let mut differences = Vec::new();
    let mut run = 0;
    for (receiver, name, example, result) in graphersal::script::doc_examples() {
        if result.is_none() || about_rules_or_memory(name) {
            continue;
        }
        run += 1;
        let expected = render(TraversalGraph::tinkerpop_modern(), example);
        let actual = render(modern_with_rules(), example);
        if expected != actual {
            differences.push(format!(
                "{receiver}.{name}: `{example}`\n  plain: {expected:?}\n  coded: {actual:?}"
            ));
        }
    }
    assert!(run > 200, "only {run} examples ran");
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
