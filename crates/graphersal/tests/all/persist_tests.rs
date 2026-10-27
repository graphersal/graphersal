//! Persistence over streams (feature `persist`, `book/src/persistence/format.md`): packed
//! snapshot round trips, the journal (write-ahead, veto, poisoning, always-last slot, durable
//! end, marks) and recovery to every target kind, torn tails and damage.
#![cfg(feature = "persist")]

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
use graphersal::persist::{
    self, Codec, Durability, Journal, JournalRecord, PersistError, ReadOptions, Recovery,
    RecoveryTarget, SharedBuffer, WriteOptions,
};
use graphersal::prelude::*;
use graphersal::prelude::{Number, PropertyMap};
use graphersal::storage::PersistentStorage;
use graphersal::{
    changes::{ChangeSet, CommitHook, HookError, Mutation},
    schema::GraphSchema,
    storage::{GraphStatistics, GraphStorage},
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A canonical text of a value: exact float bits, property order kept.
fn value_text(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".into(),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => format!("i{i}"),
            (_, Some(f)) => format!("f{:016x}", f.to_bits()),
            _ => "n?".into(),
        },
        ElementProperty::String(s) => format!("{s:?}"),
        ElementProperty::Uuid(u) => format!("u{u:032x}"),
        ElementProperty::Array(items) => format!(
            "[{}]",
            items.iter().map(value_text).collect::<Vec<_>>().join(",")
        ),
        ElementProperty::Object(object) => format!("{{{}}}", element_text(object)),
        other => format!("?{other:?}"),
    }
}

fn element_text(element: &PropertyMap) -> String {
    element
        .iter()
        .map(|(k, v)| format!("{k:?}:{}", value_text(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Every vertex and edge of `graph` as one line each, sorted.
pub(crate) fn dump(graph: &TraversalGraph) -> Vec<String> {
    let mut lines = Vec::new();
    for v in graph.vertices() {
        let labels: Vec<&str> = graph.vertex_labels(v).iter().map(|l| l.as_str()).collect();
        lines.push(format!(
            "v {} {:?} {}",
            graph.vertex_id(v).unwrap_or("?"),
            labels,
            graph
                .vertex_properties(v)
                .map(element_text)
                .unwrap_or_default()
        ));
    }
    for e in graph.edges() {
        let (from, to) = graph.edge_endpoints(e).expect("endpoints");
        lines.push(format!(
            "e {} {:?} {}->{} {}",
            graph.edge_id(e).unwrap_or("?"),
            graph.edge_label(e),
            graph.vertex_id(from).unwrap_or("?"),
            graph.vertex_id(to).unwrap_or("?"),
            graph
                .edge_properties(e)
                .map(element_text)
                .unwrap_or_default()
        ));
    }
    lines.sort();
    lines
}

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    let mut element = PropertyMap::default();
    for (key, value) in entries {
        element.insert(key, value);
    }
    element
}

fn object(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    ElementProperty::Object(Box::new(props(entries)))
}

/// A graph with every value type and label/edge shape.
pub(crate) fn varied_graph() -> TraversalGraph {
    let mut g = TraversalGraph::new();
    let nan_with_payload = f64::from_bits(0x7FF8_0000_0000_1234);
    let a = g
        .add_vertex(
            Some("a".into()),
            vec!["person".into(), "admin".into(), "x".into()],
            props(vec![
                // Insertion order differs from sorted order on purpose.
                ("z", ElementProperty::String("last key first".into())),
                ("name", ElementProperty::String("ann ünïcødé 🚀".into())),
                ("empty", ElementProperty::String("".into())),
                ("null", ElementProperty::Null),
                ("t", ElementProperty::Boolean(true)),
                ("f", ElementProperty::Boolean(false)),
                ("min", ElementProperty::Number(Number::from_i64(i64::MIN))),
                ("max", ElementProperty::Number(Number::from_i64(i64::MAX))),
                ("float", ElementProperty::Number(Number::from_f64(-1.5e300))),
                (
                    "nan",
                    ElementProperty::Number(Number::from_f64(nan_with_payload)),
                ),
                (
                    "inf",
                    ElementProperty::Number(Number::from_f64(f64::NEG_INFINITY)),
                ),
                (
                    "uuid",
                    ElementProperty::Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
                ),
                ("list", ElementProperty::Array(vec![])),
                ("map", object(vec![])),
                (
                    "nested",
                    ElementProperty::Array(vec![
                        object(vec![
                            ("k", ElementProperty::Array(vec![ElementProperty::Null])),
                            ("b", object(vec![("deep", ElementProperty::Uuid(7))])),
                        ]),
                        ElementProperty::Number(Number::from_i64(-3)),
                    ]),
                ),
            ]),
        )
        .unwrap();
    let b = g
        .add_vertex(Some("b".into()), vec![], PropertyMap::default())
        .unwrap();
    let c = g
        .add_vertex(
            Some("ç-unicode-id".into()),
            vec!["person".into()],
            props(vec![("name", ElementProperty::String("c".into()))]),
        )
        .unwrap();
    g.add_edge(
        Some("e1".into()),
        Some("knows".into()),
        a,
        b,
        PropertyMap::default(),
    )
    .unwrap();
    g.add_edge(
        Some("e2".into()),
        None,
        b,
        c,
        props(vec![("w", ElementProperty::Number(Number::from_f64(0.25)))]),
    )
    .unwrap();
    g.add_edge(
        Some("loop".into()),
        Some("self".into()),
        a,
        a,
        PropertyMap::default(),
    )
    .unwrap();
    // Auto ids too.
    let d = g
        .add_vertex(None, vec!["auto".into()], PropertyMap::default())
        .unwrap();
    g.add_edge(None, Some("auto".into()), d, c, PropertyMap::default())
        .unwrap();
    g
}

fn snapshot_bytes(graph: &TraversalGraph, options: &WriteOptions) -> Vec<u8> {
    let mut out = Vec::new();
    persist::write_snapshot(graph, &mut out, options).expect("write");
    out
}

fn load(bytes: &[u8]) -> TraversalGraph {
    TraversalGraph::read_snapshot(bytes).expect("read")
}

fn add_person(graph: &mut TraversalGraph, name: &str) {
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", name)
        .to_list()
        .unwrap();
}

pub(crate) fn names(graph: &TraversalGraph) -> Vec<String> {
    let mut names: Vec<String> = graph
        .vertices()
        .filter_map(|v| match graph.vertex_properties(v)?.get("name")? {
            ElementProperty::String(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    names.sort();
    names
}

/// A graph with a base snapshot and an attached journal.
fn journaled(graph: TraversalGraph) -> (TraversalGraph, Vec<u8>, SharedBuffer) {
    let mut graph = graph;
    let snapshot = snapshot_bytes(&graph, &WriteOptions::new());
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::EveryCommit)
        .attach(&mut graph)
        .unwrap();
    (graph, snapshot, wal)
}

// ---------------------------------------------------------------------------------------------
// Packed snapshot
// ---------------------------------------------------------------------------------------------

#[test]
fn snapshot_round_trip_keeps_every_value_type_and_shape() {
    let graph = varied_graph();
    let loaded = load(&snapshot_bytes(&graph, &WriteOptions::new()));
    assert_eq!(dump(&loaded), dump(&graph));
    // Statistics and indexes are rebuilt.
    assert_eq!(
        loaded.statistics().unwrap().cloned(),
        Some(GraphStatistics::from_storage(&loaded))
    );
    assert_eq!(loaded.statistics().unwrap(), graph.statistics().unwrap());
    let count = |g: GraphTraversalSource<'_, &TraversalGraph>| g.count().next().unwrap();
    assert_eq!(
        count(loaded.traversal().v(None::<()>).has_label("person")),
        Some(2.into())
    );
    assert_eq!(count(loaded.traversal().e("e1")), Some(1.into()));
}

#[test]
fn snapshot_round_trip_with_tiny_chunks_segments_and_every_codec() {
    let mut graph = varied_graph();
    for i in 0..300 {
        add_person(&mut graph, &format!("p{i:03}"));
    }
    #[allow(unused_mut)]
    let mut codecs = vec![Codec::None, Codec::Lz4];
    #[cfg(feature = "persist-zstd")]
    codecs.push(Codec::Zstd);
    for codec in codecs {
        let options = WriteOptions::new()
            .with_codec(codec)
            .with_chunk_bytes(256)
            .with_segment_bytes(2048);
        let mut bytes = Vec::new();
        let info = persist::write_snapshot(&graph, &mut bytes, &options).unwrap();
        assert!(info.segment_count > 2, "{codec:?}: {}", info.segment_count);
        assert_eq!(dump(&load(&bytes)), dump(&graph), "{codec:?}");
    }
}

#[test]
fn empty_graph_round_trips() {
    let graph = TraversalGraph::new();
    let loaded = load(&snapshot_bytes(&graph, &WriteOptions::new()));
    assert_eq!(loaded.vertex_count(), 0);
    assert_eq!(loaded.graph_id(), graph.graph_id());
}

#[test]
fn snapshot_restores_schema_position_sequences_and_lineage() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let schema = GraphSchema::from_json(
        r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
            "properties": {"name": {"type": "string"}}, "required": ["name"]}}}}"#,
    )
    .unwrap();
    graph.set_schema(schema.clone()).unwrap();
    add_person(&mut graph, "ann");
    // A dropped auto id must never come back.
    graph
        .traversal_mut()
        .v(None::<()>)
        .has("name", "ann")
        .drop()
        .to_list()
        .unwrap();
    let (seq, time) = (graph.last_commit_seq(), graph.last_committed_at());
    let (nodes, edges) = (graph.node_sequence(), graph.edge_sequence());
    assert!(seq >= 3 && time > 0);

    let bytes = snapshot_bytes(&graph, &WriteOptions::new());
    let info = persist::read_snapshot_info(bytes.as_slice()).unwrap();
    assert_eq!(info.commit_seq, seq);
    assert_eq!(info.vertex_count, 6);

    let mut loaded = load(&bytes);
    assert_eq!(loaded.last_commit_seq(), seq);
    assert_eq!(loaded.last_committed_at(), time);
    assert_eq!(
        (loaded.node_sequence(), loaded.edge_sequence()),
        (nodes, edges)
    );
    assert_eq!(loaded.graph_id(), graph.graph_id());
    assert_eq!(loaded.get_schema().unwrap(), Some(schema));
    // The schema is enforced again after the load.
    assert!(loaded.traversal_mut().add_v("person").to_list().is_err());
    // Numbering continues.
    add_person(&mut loaded, "bob");
    assert_eq!(loaded.last_commit_seq(), seq + 1);
    assert!(loaded.last_committed_at() >= time);
    let ids: Vec<String> = loaded
        .vertices()
        .filter_map(|v| loaded.vertex_id(v).map(str::to_string))
        .collect();
    assert!(ids.contains(&(nodes + 1).to_string()), "{ids:?}");
}

#[test]
fn name_and_meta_are_listed_and_the_memory_limit_refuses_a_load() {
    let graph = TraversalGraph::tinkerpop_modern();
    let options = WriteOptions::new()
        .with_name("nightly")
        .with_meta(r#"{"owner": "ops"}"#);
    let bytes = snapshot_bytes(&graph, &options);
    let info = persist::read_snapshot_info(bytes.as_slice()).unwrap();
    assert_eq!(info.name, "nightly");
    assert_eq!(info.meta, r#"{"owner": "ops"}"#);
    assert!(info.memory_bytes > 0);
    let refused =
        persist::read_snapshot(bytes.as_slice(), &ReadOptions::new().with_memory_limit(10));
    assert!(matches!(refused, Err(PersistError::MemoryBudget { .. })));
    let fits = persist::read_snapshot(
        bytes.as_slice(),
        &ReadOptions::new().with_memory_limit(info.memory_bytes),
    );
    assert!(fits.is_ok());
}

#[test]
fn values_deeper_than_the_read_limit_are_refused() {
    let mut value = ElementProperty::Null;
    for _ in 0..10 {
        value = ElementProperty::Array(vec![value]);
    }
    let mut graph = TraversalGraph::new();
    graph
        .add_vertex(Some("v".into()), vec![], props(vec![("deep", value)]))
        .unwrap();
    let bytes = snapshot_bytes(&graph, &WriteOptions::new());
    assert!(
        persist::read_snapshot(
            bytes.as_slice(),
            &ReadOptions::new().with_max_value_depth(5)
        )
        .is_err()
    );
    assert!(
        persist::read_snapshot(
            bytes.as_slice(),
            &ReadOptions::new().with_max_value_depth(10)
        )
        .is_ok()
    );
}

#[test]
fn every_flipped_bit_of_a_packed_snapshot_is_detected() {
    let mut graph = TraversalGraph::new();
    let a = graph
        .add_vertex(
            Some("a".into()),
            vec!["l".into()],
            props(vec![("k", "v".into())]),
        )
        .unwrap();
    graph
        .add_edge(
            Some("e".into()),
            Some("r".into()),
            a,
            a,
            PropertyMap::default(),
        )
        .unwrap();
    let bytes = snapshot_bytes(&graph, &WriteOptions::new());
    for at in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[at] ^= 1 << (at % 8);
        assert!(
            TraversalGraph::read_snapshot(damaged.as_slice()).is_err(),
            "a flipped bit at byte {at} of {} went unnoticed",
            bytes.len()
        );
    }
    // Every truncation too.
    for len in 0..bytes.len() {
        assert!(
            TraversalGraph::read_snapshot(&bytes[..len]).is_err(),
            "cut at {len}"
        );
    }
}

#[test]
fn a_snapshot_of_another_shape_is_not_a_snapshot() {
    for bytes in [&b""[..], b"GRSLPACK", b"<graphml/>", &[0u8; 64][..]] {
        assert!(TraversalGraph::read_snapshot(bytes).is_err());
    }
}

// ---------------------------------------------------------------------------------------------
// Journal and recovery
// ---------------------------------------------------------------------------------------------

/// Collects every change set it sees committed.
struct Collect(Arc<parking_lot::Mutex<Vec<ChangeSet>>>);

impl CommitHook for Collect {
    fn after_commit(&mut self, changes: &ChangeSet) {
        self.0.lock().push(changes.clone());
    }
}

#[test]
fn journal_records_are_the_lossless_change_sets() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .register_commit_hook(Box::new(Collect(seen.clone())))
        .unwrap();
    let (mut graph, _, wal) = journaled(graph);
    add_person(&mut graph, "ann");
    graph
        .traversal_mut()
        .v("1")
        .property(
            "nested",
            ElementProperty::Array(vec![object(vec![("u", ElementProperty::Uuid(9))])]),
        )
        .to_list()
        .unwrap();
    graph.traversal_mut().v("2").drop().to_list().unwrap();
    graph
        .set_schema(GraphSchema::from_json(r#"{"mode": "open"}"#).unwrap())
        .unwrap();
    graph
        .transaction_with(
            graphersal::changes::CommitMetadata::new()
                .with_principal("alice")
                .with_attribute("ticket", 42i64),
            |g| -> Result<(), GraphError> {
                let v = g.get_vertex_by_id("1").unwrap();
                g.add_vertex_label(v, "admin".into())?;
                g.drop_vertex_property("nested", v)?;
                Ok(())
            },
        )
        .unwrap();
    // A big commit: compressed record.
    graph
        .transaction(|g| -> Result<(), GraphError> {
            for i in 0..200 {
                g.add_vertex(
                    None,
                    vec!["bulk".into()],
                    props(vec![("i", (i as i64).into())]),
                )?;
            }
            Ok(())
        })
        .unwrap();

    let records: Vec<ChangeSet> = persist::read_journal(wal.contents().as_slice())
        .unwrap()
        .map(|record| match record.unwrap() {
            JournalRecord::Commit(changes) => changes,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(records, *seen.lock());
    assert_eq!(records.len(), 6);
}

#[test]
fn recovery_replays_to_latest_commit_time_and_mark() {
    let mut modern = TraversalGraph::tinkerpop_modern();
    add_person(&mut modern, "zed");
    let (mut graph, snapshot, wal) = journaled(modern);
    let base = graph.last_commit_seq();
    assert_eq!(base, 1);
    add_person(&mut graph, "ann");
    std::thread::sleep(std::time::Duration::from_millis(3));
    add_person(&mut graph, "bob");
    let after_bob = graph.mark("after-bob").unwrap();
    assert_eq!(after_bob.commit_seq(), base + 2);
    std::thread::sleep(std::time::Duration::from_millis(3));
    add_person(&mut graph, "cid");
    let journal = wal.contents();

    let run = |target| persist::recover(snapshot.as_slice(), [journal.as_slice()], target);

    let latest = run(RecoveryTarget::Latest).unwrap();
    assert_eq!(dump(&latest.graph), dump(&graph));
    assert_eq!(latest.report.commit_seq, base + 3);
    assert_eq!(latest.report.commits_replayed, 3);
    assert!(latest.report.reached_end);
    assert_eq!(latest.report.marks.len(), 1);
    assert_eq!(latest.graph.last_commit_seq(), graph.last_commit_seq());
    assert_eq!(latest.graph.last_committed_at(), graph.last_committed_at());
    assert_eq!(latest.graph.node_sequence(), graph.node_sequence());

    let one = run(RecoveryTarget::CommitSeq(base + 1)).unwrap();
    assert!(names(&one.graph).contains(&"ann".to_string()));
    assert!(!names(&one.graph).contains(&"bob".to_string()));
    assert!(!one.report.reached_end);

    let mark = run(RecoveryTarget::Mark("after-bob".into())).unwrap();
    assert_eq!(mark.report.commit_seq, base + 2);
    assert!(names(&mark.graph).contains(&"bob".to_string()));
    assert!(!names(&mark.graph).contains(&"cid".to_string()));

    // Time: the commit time of bob's commit.
    let times: Vec<i64> = persist::read_journal(journal.as_slice())
        .unwrap()
        .filter_map(|r| match r.unwrap() {
            JournalRecord::Commit(c) => Some(c.committed_at()),
            _ => None,
        })
        .collect();
    let at_bob = run(RecoveryTarget::Time(times[1])).unwrap();
    assert_eq!(at_bob.report.commit_seq, base + 2);
    let future = run(RecoveryTarget::Time(i64::MAX)).unwrap();
    assert_eq!(future.report.commit_seq, base + 3);

    // Targets that are not there.
    assert!(matches!(
        run(RecoveryTarget::CommitSeq(base + 9)),
        Err(PersistError::TargetNotReached { .. })
    ));
    assert!(matches!(
        run(RecoveryTarget::Mark("nope".into())),
        Err(PersistError::TargetNotReached { .. })
    ));
    assert!(matches!(
        run(RecoveryTarget::CommitSeq(base - 1)),
        Err(PersistError::TargetNotReached { .. })
    ));
    assert!(matches!(
        run(RecoveryTarget::Time(1)),
        Err(PersistError::TargetNotReached { .. })
    ));
}

#[test]
fn recovery_continues_numbering_and_a_second_journal_chains() {
    let (mut graph, snapshot, wal) = journaled(TraversalGraph::new());
    add_person(&mut graph, "ann");
    graph.mark("m1").unwrap();
    let first = wal.contents();

    let recovered = persist::recover(
        snapshot.as_slice(),
        [first.as_slice()],
        RecoveryTarget::Latest,
    )
    .unwrap();
    let mut graph = recovered.graph;
    let wal2 = SharedBuffer::new();
    let marks: Vec<String> = recovered
        .report
        .marks
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    Journal::new(wal2.clone(), Durability::EveryCommit)
        .with_known_marks(marks)
        .attach(&mut graph)
        .unwrap();
    assert!(matches!(
        graph.mark("m1"),
        Err(GraphError::MarkRejected { .. })
    ));
    add_person(&mut graph, "bob");
    assert_eq!(graph.last_commit_seq(), 2);

    let both = Recovery::new()
        .snapshot(snapshot.as_slice())
        .journal(first.as_slice())
        .journal(wal2.contents().as_slice())
        .run()
        .unwrap();
    assert_eq!(names(&both.graph), ["ann", "bob"]);
    // Without the snapshot (the journal starts at commit 1).
    let no_snapshot = Recovery::new()
        .journal(first.as_slice())
        .journal(wal2.contents().as_slice())
        .run()
        .unwrap();
    assert_eq!(dump(&no_snapshot.graph), dump(&both.graph));
    // Only the second journal: commit 1 is missing.
    let gap = Recovery::new()
        .snapshot(snapshot.as_slice())
        .journal(wal2.contents().as_slice())
        .run();
    assert!(
        matches!(
            gap,
            Err(PersistError::Gap {
                expected: 1,
                found: 2,
                ..
            })
        ),
        "{gap:?}"
    );
}

#[test]
fn a_journal_of_another_lineage_is_refused() {
    let (mut graph, _, wal) = journaled(TraversalGraph::new());
    add_person(&mut graph, "ann");
    let other = snapshot_bytes(&TraversalGraph::new(), &WriteOptions::new());
    let result = persist::recover(
        other.as_slice(),
        [wal.contents().as_slice()],
        RecoveryTarget::Latest,
    );
    assert!(
        matches!(result, Err(PersistError::LineageMismatch { .. })),
        "{result:?}"
    );
}

#[test]
fn marks_need_a_journal_a_closed_unit_and_a_unique_name() {
    let mut plain = TraversalGraph::new();
    assert!(matches!(
        plain.mark("x"),
        Err(GraphError::MarkUnavailable { .. })
    ));
    let (mut graph, _, _) = journaled(TraversalGraph::new());
    assert!(matches!(
        graph.mark(""),
        Err(GraphError::MarkUnavailable { .. })
    ));
    assert!(matches!(
        graph.mark(&"x".repeat(256)),
        Err(GraphError::MarkUnavailable { .. })
    ));
    let inside = graph.transaction(|g| -> Result<(), GraphError> {
        assert!(matches!(
            g.mark("in"),
            Err(GraphError::MarkUnavailable { .. })
        ));
        Ok(())
    });
    assert!(inside.is_ok());
    graph.mark("x").unwrap();
    assert!(matches!(
        graph.mark("x"),
        Err(GraphError::MarkRejected { .. })
    ));
    // A second journal is refused.
    let second = Journal::new(SharedBuffer::new(), Durability::Os).attach(&mut graph);
    assert!(second.is_err());
}

/// A writer that fails once `budget` bytes were written, or whose sync fails on demand.
#[derive(Clone)]
struct Flaky {
    out: SharedBuffer,
    budget: Arc<AtomicUsize>,
    fail_sync: Arc<AtomicBool>,
}

impl Write for Flaky {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let left = self.budget.load(Ordering::SeqCst);
        if left == 0 {
            return Err(std::io::Error::other("disk full"));
        }
        let n = buf.len().min(left);
        self.budget.fetch_sub(n, Ordering::SeqCst);
        self.out.write(&buf[..n])
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn flaky_journal(budget: usize) -> (TraversalGraph, Flaky) {
    let flaky = Flaky {
        out: SharedBuffer::new(),
        budget: Arc::new(AtomicUsize::new(budget)),
        fail_sync: Arc::new(AtomicBool::new(false)),
    };
    let mut graph = TraversalGraph::new();
    let fail_sync = flaky.fail_sync.clone();
    Journal::new(flaky.clone(), Durability::EveryCommit)
        .with_sync(move |_| {
            if fail_sync.load(Ordering::SeqCst) {
                Err(std::io::Error::other("fsync failed"))
            } else {
                Ok(())
            }
        })
        .attach(&mut graph)
        .unwrap();
    (graph, flaky)
}

#[test]
fn a_failed_write_vetoes_the_commit_and_poisons_the_journal() {
    let (mut graph, flaky) = flaky_journal(64 + 30);
    let err = graph
        .traversal_mut()
        .add_v("person")
        .property("name", "a long enough name to overflow the budget")
        .to_list()
        .unwrap_err();
    assert!(err.to_string().contains("rejected"), "{err}");
    assert_eq!(graph.vertex_count(), 0);
    assert_eq!(graph.last_commit_seq(), 0);
    // Poisoned: even with room again, nothing more is accepted.
    flaky.budget.store(1 << 20, Ordering::SeqCst);
    assert!(graph.traversal_mut().add_v("x").to_list().is_err());
    assert!(graph.mark("m").is_err());
    assert_eq!(graph.vertex_count(), 0);
    // The partial record is a torn tail: recovery yields the empty graph.
    let recovered = Recovery::new()
        .journal(flaky.out.contents().as_slice())
        .run()
        .unwrap();
    assert_eq!(recovered.graph.vertex_count(), 0);
    assert!(recovered.report.torn_tail.is_some());
}

#[test]
fn a_failed_sync_vetoes_the_commit_and_poisons_the_journal() {
    let (mut graph, flaky) = flaky_journal(1 << 20);
    add_person(&mut graph, "ann");
    flaky.fail_sync.store(true, Ordering::SeqCst);
    assert!(graph.traversal_mut().add_v("x").to_list().is_err());
    assert_eq!(graph.vertex_count(), 1);
    flaky.fail_sync.store(false, Ordering::SeqCst);
    assert!(graph.traversal_mut().add_v("y").to_list().is_err());
    assert_eq!(graph.vertex_count(), 1);
}

#[test]
fn a_file_journal_cuts_a_record_whose_sync_failed_so_recovery_never_replays_it() {
    let path = std::env::temp_dir().join(format!(
        "graphersal-refused-{}-{}.wal",
        std::process::id(),
        line!()
    ));
    let mut graph = TraversalGraph::new();
    let base = snapshot_bytes(&graph, &WriteOptions::new());
    let fail = Arc::new(AtomicBool::new(false));
    let switch = fail.clone();
    // A journal appended to a file that already holds bytes: only its own bytes are cut.
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"prefix").unwrap();
    Journal::for_file(file, Durability::EveryCommit)
        .with_sync(move |file: &mut std::fs::File| {
            if switch.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("injected fsync failure"));
            }
            file.sync_data()
        })
        .attach(&mut graph)
        .unwrap();
    add_person(&mut graph, "ann");
    let durable = std::fs::metadata(&path).unwrap().len();
    fail.store(true, Ordering::SeqCst);
    let err = graph
        .traversal_mut()
        .add_v("person")
        .property("name", "bob")
        .to_list()
        .unwrap_err();
    assert!(err.to_string().contains("cut off again"), "{err}");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), durable);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let recovered =
        persist::recover(base.as_slice(), [&bytes[6..]], RecoveryTarget::Latest).unwrap();
    assert_eq!(names(&recovered.graph), ["ann"]);
    assert!(recovered.report.torn_tail.is_none());
}

/// A journal target that panics once its byte budget is used up (`usize::MAX`: never) or in
/// its sync while `panic_sync` is set, and can be cut back.
#[derive(Clone, Default)]
struct Panicky {
    out: Arc<parking_lot::Mutex<Vec<u8>>>,
    budget: Arc<AtomicUsize>,
    panic_sync: Arc<AtomicBool>,
}

impl Write for Panicky {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let left = self.budget.load(Ordering::SeqCst);
        let n = buf.len().min(left);
        self.out.lock().extend_from_slice(&buf[..n]);
        if n < buf.len() {
            panic!("injected panic in the journal's writer");
        }
        if left != usize::MAX {
            self.budget.fetch_sub(n, Ordering::SeqCst);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A graph with a journal on a [`Panicky`] target that holds "ann".
fn panicky_journal() -> (TraversalGraph, Panicky) {
    let target = Panicky {
        budget: Arc::new(AtomicUsize::new(usize::MAX)),
        ..Panicky::default()
    };
    let mut graph = TraversalGraph::new();
    let panic_sync = target.panic_sync.clone();
    Journal::new(target.clone(), Durability::EveryCommit)
        .with_sync(move |_| {
            if panic_sync.load(Ordering::SeqCst) {
                panic!("injected panic in the journal's sync");
            }
            Ok(())
        })
        .with_truncate(|target: &mut Panicky, len| {
            target.out.lock().truncate(len as usize);
            Ok(())
        })
        .attach(&mut graph)
        .unwrap();
    add_person(&mut graph, "ann");
    (graph, target)
}

/// A panic inside the journal's write path (after part of the record, or after all of it,
/// in the sync) is handled like a failed write: the record is cut off again, the journal is
/// poisoned, the panic continues, and the graph rolls the unit back on the way out. Neither
/// memory nor the journal keeps the unit; later commits and marks get the poisoned error.
#[test]
fn a_panic_in_the_journal_cuts_the_record_rolls_back_and_poisons() {
    for in_sync in [false, true] {
        let (mut graph, target) = panicky_journal();
        let before = target.out.lock().len();
        if in_sync {
            target.panic_sync.store(true, Ordering::SeqCst);
        } else {
            target.budget.store(10, Ordering::SeqCst);
        }
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            graph
                .traversal_mut()
                .add_v("person")
                .property("name", "bob")
                .to_list()
        }));
        assert!(panicked.is_err(), "in_sync {in_sync}: the panic continues");
        // Memory: rolled back. The journal: cut back to before the unit.
        assert_eq!(names(&graph), ["ann"], "in_sync {in_sync}");
        assert_eq!(graph.last_commit_seq(), 1);
        assert_eq!(target.out.lock().len(), before, "in_sync {in_sync}");
        // Poisoned, even once the target works again.
        target.budget.store(usize::MAX, Ordering::SeqCst);
        target.panic_sync.store(false, Ordering::SeqCst);
        let err = graph.traversal_mut().add_v("x").to_list().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("poisoned"), "{text}");
        assert!(
            text.contains("a panic while writing a commit record"),
            "{text}"
        );
        assert!(text.contains("cut off again"), "{text}");
        assert!(graph.mark("m").is_err());
        assert_eq!(names(&graph), ["ann"]);
        // Recovery replays exactly the commit that happened.
        let bytes = target.out.lock().clone();
        let recovered = Recovery::new().journal(bytes.as_slice()).run().unwrap();
        assert_eq!(names(&recovered.graph), ["ann"], "in_sync {in_sync}");
        assert!(recovered.report.torn_tail.is_none());
    }
}

/// A user hook that vetoes while `veto` is set.
struct Veto(Arc<AtomicBool>);

impl CommitHook for Veto {
    fn name(&self) -> &str {
        "veto"
    }
    fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
        if self.0.load(Ordering::SeqCst) {
            Err("vetoed by the application".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn a_hook_registered_after_the_journal_still_runs_before_it() {
    let (mut graph, snapshot, wal) = journaled(TraversalGraph::new());
    let veto = Arc::new(AtomicBool::new(false));
    graph
        .register_commit_hook(Box::new(Veto(veto.clone())))
        .unwrap();
    add_person(&mut graph, "ann");
    let before = wal.len();
    veto.store(true, Ordering::SeqCst);
    let err = graph
        .traversal_mut()
        .add_v("person")
        .property("name", "vetoed")
        .to_list()
        .unwrap_err();
    assert!(err.to_string().contains("veto"), "{err}");
    // The journal never saw the vetoed unit.
    assert_eq!(wal.len(), before);
    veto.store(false, Ordering::SeqCst);
    add_person(&mut graph, "bob");
    let recovered = persist::recover(
        snapshot.as_slice(),
        [wal.contents().as_slice()],
        RecoveryTarget::Latest,
    )
    .unwrap();
    assert_eq!(names(&recovered.graph), ["ann", "bob"]);
    assert_eq!(recovered.graph.last_commit_seq(), 2);
}

#[test]
fn the_durable_end_follows_the_synced_records() {
    let mut graph = TraversalGraph::new();
    let wal = SharedBuffer::new();
    let end = Journal::new(wal.clone(), Durability::EveryCommit)
        .attach(&mut graph)
        .unwrap();
    assert_eq!(end.offset(), 64);
    add_person(&mut graph, "ann");
    assert_eq!(end.offset(), wal.len() as u64);
    assert_eq!(end.commit_seq(), 1);

    // A long interval: records are written but not synced, the durable end stays.
    let mut lazy = TraversalGraph::new();
    let wal = SharedBuffer::new();
    let end = Journal::new(
        wal.clone(),
        Durability::Interval(std::time::Duration::from_secs(3600)),
    )
    .attach(&mut lazy)
    .unwrap();
    add_person(&mut lazy, "ann");
    assert_eq!(end.offset(), 64);
    assert!(wal.len() > 64);
}

#[test]
fn a_torn_tail_is_cut_at_every_byte_of_the_last_record() {
    let (mut graph, snapshot, wal) = journaled(TraversalGraph::new());
    add_person(&mut graph, "ann");
    let complete = wal.len();
    add_person(&mut graph, "bob");
    let journal = wal.contents();
    for cut in complete..journal.len() {
        let recovered = persist::recover(
            snapshot.as_slice(),
            [&journal[..cut]],
            RecoveryTarget::Latest,
        )
        .unwrap_or_else(|err| panic!("cut at {cut}: {err}"));
        assert_eq!(names(&recovered.graph), ["ann"], "cut at {cut}");
        assert_eq!(
            recovered.report.torn_tail.is_some(),
            cut > complete,
            "cut at {cut}"
        );
    }
    // A torn record in a journal that is not the last one is damage.
    let result = Recovery::new()
        .snapshot(snapshot.as_slice())
        .journal(&journal[..journal.len() - 1])
        .journal(&journal[..64])
        .run();
    assert!(
        matches!(result, Err(PersistError::Corrupt { .. })),
        "{result:?}"
    );
}

#[test]
fn flipped_bits_in_complete_records_are_damage_never_silently_skipped() {
    let (mut graph, snapshot, wal) = journaled(TraversalGraph::new());
    add_person(&mut graph, "ann");
    add_person(&mut graph, "bob");
    let journal = wal.contents();
    for at in 0..journal.len() {
        let mut damaged = journal.clone();
        damaged[at] ^= 1 << (at % 8);
        // Every flip is detected: the header CRC covers marker, length, commit_seq and type
        // (so a damaged length of the LAST record is damage too, never a torn tail), the body
        // CRC the payload (format spec 9.6).
        let result = persist::recover(
            snapshot.as_slice(),
            [damaged.as_slice()],
            RecoveryTarget::Latest,
        );
        assert!(
            matches!(result, Err(PersistError::Corrupt { .. })),
            "flip at {at}: {:?}",
            result.map(|r| r.report)
        );
    }
}

#[test]
fn a_damaged_length_of_the_last_record_is_damage_a_cut_last_record_is_a_torn_tail() {
    let (mut graph, snapshot, wal) = journaled(TraversalGraph::new());
    add_person(&mut graph, "ann");
    let last = wal.len();
    add_person(&mut graph, "bob");
    let journal = wal.contents();
    // Every bit of the last record's length field (frame bytes 4..8).
    for bit in 0..32 {
        let mut damaged = journal.clone();
        damaged[last + 4 + bit / 8] ^= 1 << (bit % 8);
        let result = persist::recover(
            snapshot.as_slice(),
            [damaged.as_slice()],
            RecoveryTarget::Latest,
        );
        match result {
            Err(PersistError::Corrupt { offset, reason, .. }) => {
                assert_eq!(offset, last as u64, "bit {bit}");
                assert!(reason.contains("header checksum"), "bit {bit}: {reason}");
            }
            other => panic!("bit {bit}: {:?}", other.map(|r| r.report)),
        }
    }
    // The same record genuinely cut: a torn tail, the commit before it is kept.
    let recovered = persist::recover(
        snapshot.as_slice(),
        [&journal[..journal.len() - 3]],
        RecoveryTarget::Latest,
    )
    .unwrap();
    assert_eq!(names(&recovered.graph), ["ann"]);
    assert_eq!(recovered.report.torn_tail.unwrap().offset, last as u64);
    // Zeros after the last complete record (space allocated, never written) are a torn tail.
    let mut zeroed = journal[..last].to_vec();
    zeroed.extend(std::iter::repeat_n(0u8, 4096));
    let recovered = persist::recover(
        snapshot.as_slice(),
        [zeroed.as_slice()],
        RecoveryTarget::Latest,
    )
    .unwrap();
    assert_eq!(names(&recovered.graph), ["ann"]);
    assert_eq!(recovered.report.torn_tail.unwrap().bytes, 4096);
}

// ---------------------------------------------------------------------------------------------
// The catalog of definitions (format spec 6.3, 9.3, 16)
// ---------------------------------------------------------------------------------------------

fn saved_query(name: &str) -> Definition {
    Definition::query(
        name,
        QueryDefinition::new(format!("fn {name}() {{ g.v() }}")).with_folder("a/b"),
    )
}

fn unknown_definition(name: &str, critical: bool) -> Definition {
    Definition::opaque(
        DefinitionKind::from_code(99),
        graphersal::catalog::DefinitionFlags::NONE.with_critical(critical),
        name,
        vec![0xAB, 0, 0xFF, 7],
    )
}

/// The encoded form of a small opaque definition (format spec 6.3).
fn opaque_record(definition: &Definition) -> Vec<u8> {
    let payload = definition.opaque_payload().unwrap();
    let mut out = vec![
        definition.kind().code(),
        definition.flags().bits(),
        definition.name().len() as u8,
    ];
    out.extend_from_slice(definition.name().as_bytes());
    out.push(payload.len() as u8);
    out.extend_from_slice(payload);
    out
}

fn catalog_of(graph: &TraversalGraph) -> Vec<Definition> {
    graph.definitions().unwrap().cloned().collect()
}

#[test]
fn a_packed_snapshot_keeps_the_catalog_and_writes_unknown_definitions_back_unchanged() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.set_definition(saved_query("people")).unwrap();
    graph
        .set_definition(unknown_definition("later", false))
        .unwrap();
    // An in-memory graph may hold a critical one (only a store goes read-only).
    graph
        .set_definition(unknown_definition("index", true))
        .unwrap();
    let bytes = snapshot_bytes(&graph, &WriteOptions::new());
    let loaded = load(&bytes);
    assert_eq!(catalog_of(&loaded), catalog_of(&graph));
    let again = snapshot_bytes(&loaded, &WriteOptions::new());
    for name in ["later", "index"] {
        let record = opaque_record(
            loaded
                .definition(DefinitionKind::from_code(99), name)
                .unwrap()
                .unwrap(),
        );
        assert!(
            again.windows(record.len()).any(|w| w == record.as_slice()),
            "{name}"
        );
    }
    assert_eq!(catalog_of(&load(&again)), catalog_of(&graph));
}

#[test]
fn journal_records_carry_catalog_changes_and_every_damage_of_them_is_detected() {
    let (mut graph, snapshot, wal) = journaled(TraversalGraph::new());
    graph.set_definition(saved_query("first")).unwrap();
    graph
        .set_definition(unknown_definition("later", false))
        .unwrap();
    let complete = wal.len();
    // The last record changes and drops in one unit (before and after images).
    graph
        .transaction(|g| -> Result<(), GraphError> {
            let mut moved = saved_query("first");
            moved
                .as_query_mut()
                .unwrap()
                .set_description(Some("moved".into()));
            g.set_definition(moved)?;
            g.remove_definition(DefinitionKind::from_code(99), "later")?;
            Ok(())
        })
        .unwrap();
    let journal = wal.contents();
    let recovered = persist::recover(
        snapshot.as_slice(),
        [journal.as_slice()],
        RecoveryTarget::Latest,
    )
    .unwrap();
    assert_eq!(catalog_of(&recovered.graph), catalog_of(&graph));
    // The records are the change sets: before and after images, unknown kinds opaque.
    let records: Vec<JournalRecord> = persist::read_journal(journal.as_slice())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let JournalRecord::Commit(last) = records.last().unwrap() else {
        panic!("a commit");
    };
    assert_eq!(last.mutations().len(), 2);
    assert!(matches!(
        &last.mutations()[1],
        Mutation::SetDefinition { before: Some(before), after: None, .. } if before.is_opaque()
    ));

    // Every flipped bit of the op 10 records is damage, never skipped.
    for at in 64..journal.len() {
        let mut damaged = journal.clone();
        damaged[at] ^= 1 << (at % 8);
        let result = persist::recover(
            snapshot.as_slice(),
            [damaged.as_slice()],
            RecoveryTarget::Latest,
        );
        assert!(
            matches!(result, Err(PersistError::Corrupt { .. })),
            "flip at {at}: {:?}",
            result.map(|r| r.report)
        );
    }
    // Every cut of the last record is a torn tail: the catalog before it is recovered.
    let before = {
        let r = persist::recover(
            snapshot.as_slice(),
            [&journal[..complete]],
            RecoveryTarget::Latest,
        )
        .unwrap();
        catalog_of(&r.graph)
    };
    assert_eq!(before.len(), 2);
    for cut in complete + 1..journal.len() {
        let recovered = persist::recover(
            snapshot.as_slice(),
            [&journal[..cut]],
            RecoveryTarget::Latest,
        )
        .unwrap_or_else(|err| panic!("cut at {cut}: {err}"));
        assert_eq!(catalog_of(&recovered.graph), before, "cut at {cut}");
        assert!(recovered.report.torn_tail.is_some(), "cut at {cut}");
    }
}

// ---------------------------------------------------------------------------------------------
// Rhai
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
#[test]
fn rhai_marks_go_to_the_graphs_journal() {
    use crate::script_host::{ScriptLimits, eval_value_with_limits};

    let run = |graph: &Arc<Graph>, script: &str| {
        eval_value_with_limits(
            graph.clone(),
            script,
            Default::default(),
            &ScriptLimits::default(),
        )
    };
    let plain = Arc::new(GraphSource::empty());
    let err = run(&plain, r#"g.mark("m")"#).unwrap_err().to_string();
    assert!(err.contains("no journal is registered"), "{err}");
    assert!(err.contains("register one first"), "{err}");

    let mut graph = TraversalGraph::new();
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::Os)
        .attach(&mut graph)
        .unwrap();
    let shared: Arc<Graph> = Arc::new(Graph::new(graph));
    let mark = run(&shared, r#"g.addV("x").next(); g.mark("m")"#).unwrap();
    let mark = mark.cast::<graphersal::script::rhai::Map>();
    assert_eq!(mark["name"].clone().into_string().unwrap(), "m");
    assert_eq!(mark["commit_seq"].as_int().unwrap(), 1);
    let err = run(&shared, r#"g.mark("m")"#).unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    assert!(err.contains("unique within a journal"), "{err}");
    let marks: Vec<String> = persist::read_journal(wal.contents().as_slice())
        .unwrap()
        .filter_map(|r| match r.unwrap() {
            JournalRecord::Mark(m) => Some(m.name().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(marks, ["m"]);
}

/// A user hook that panics in `before_commit` while `panic` is set.
struct PanicBeforeCommit(Arc<AtomicBool>);

impl CommitHook for PanicBeforeCommit {
    fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
        if self.0.load(Ordering::SeqCst) {
            panic!("a hook panics");
        }
        Ok(())
    }
}

#[test]
fn a_hook_panicking_before_commit_leaves_nothing_in_the_journal() {
    let (mut graph, snapshot, wal) = journaled(TraversalGraph::new());
    let panic = Arc::new(AtomicBool::new(false));
    graph
        .register_commit_hook(Box::new(PanicBeforeCommit(panic.clone())))
        .unwrap();
    add_person(&mut graph, "ann");
    let before = wal.len();
    panic.store(true, Ordering::SeqCst);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        add_person(&mut graph, "lost");
    }));
    assert!(outcome.is_err());
    // The journal (always last) was never asked: nothing written, the unit rolled back.
    assert_eq!(wal.len(), before);
    assert_eq!(names(&graph), ["ann"]);
    assert_eq!(graph.last_commit_seq(), 1);
    panic.store(false, Ordering::SeqCst);
    add_person(&mut graph, "bob");
    let recovered = persist::recover(
        snapshot.as_slice(),
        [wal.contents().as_slice()],
        RecoveryTarget::Latest,
    )
    .unwrap();
    assert_eq!(names(&recovered.graph), ["ann", "bob"]);
    assert_eq!(recovered.graph.last_commit_seq(), 2);
}
