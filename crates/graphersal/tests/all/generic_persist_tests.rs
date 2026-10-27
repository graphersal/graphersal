//! The snapshot writer, the snapshot reader and recovery over a storage other than
//! `TraversalGraph` (`book/src/for_developers/custom_storage.md`): the `Forwarding`
//! proof storage (its own handle types, `PersistentStorage` forwarded) is written, loaded and
//! recovered, and must give the bytes and the state `TraversalGraph` gives.
#![cfg(feature = "persist")]

use std::sync::Arc;

use crate::forwarding::Forwarding;
use graphersal::catalog::{Definition, QueryDefinition};
use graphersal::error::GraphError;
use graphersal::persist::{
    self, Durability, Journal, PersistError, ReadOptions, Recovery, RecoveryTarget, SharedBuffer,
    SnapshotInfo, WriteOptions,
};
use graphersal::prelude::*;
use graphersal::schema::{GraphSchema, PropertySchema, SchemaMode};
use graphersal::storage::{AnyGraph, Graph, GraphStorage, PersistentStorage, Transactional};

/// A graph with a schema, a catalog entry, multi-label and unlabeled vertices, an unlabeled
/// edge, nested values and a dropped element (a gap in the auto ids).
fn rich_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .transaction(|g| {
            g.set_schema(
                GraphSchema::new()
                    .with_mode(SchemaMode::Open)
                    .with_vertex("person", PropertySchema::object()),
            )?;
            g.set_definition(Definition::query(
                "people",
                QueryDefinition::new("fn people() { g.v().has_label(\"person\") }"),
            ))?;
            let mut nested = PropertyMap::new();
            nested.insert(
                "tags",
                ElementProperty::Array(vec!["a".into(), 2i64.into()]),
            );
            let robot = g.add_vertex(None, vec!["robot".into(), "toy".into()], nested)?;
            let nobody = g.add_vertex(None, vec![], PropertyMap::new())?;
            g.add_edge(None, None, robot, nobody, PropertyMap::new())?;
            let gone = g.add_vertex(None, vec![], PropertyMap::new())?;
            g.drop_vertex(gone)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    graph
}

/// The modern graph with a fixed lineage id (every segment header carries it), so two
/// instances write the same bytes.
fn modern_with_graph_id() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.set_graph_id(7).unwrap();
    graph
}

fn write<S: PersistentStorage>(graph: &S) -> Vec<u8> {
    let mut bytes = Vec::new();
    persist::write_snapshot(graph, &mut bytes, &WriteOptions::new()).unwrap();
    bytes
}

/// A packed snapshot without its head and manifest section (which records the write time and
/// a memory estimate): the schema and segment sections, every byte of the data.
fn sections_after_manifest(packed: &[u8]) -> &[u8] {
    let len = u64::from_le_bytes(packed[17..25].try_into().unwrap()) as usize;
    &packed[25 + len..packed.len() - 4]
}

/// What the manifest says, without the write time.
fn info(packed: &[u8]) -> SnapshotInfo {
    let mut info = persist::read_snapshot_info(packed).unwrap();
    info.created_at = 0;
    info
}

#[test]
fn a_snapshot_loads_into_another_storage_type_and_is_rewritten_byte_for_byte() {
    for graph in [
        TraversalGraph::new(),
        TraversalGraph::tinkerpop_modern(),
        rich_graph(),
    ] {
        let original = write(&graph);
        let forwarded = persist::read_snapshot_into(
            original.as_slice(),
            &ReadOptions::new(),
            Forwarding::default(),
        )
        .unwrap();
        let loaded = persist::read_snapshot(original.as_slice(), &ReadOptions::new()).unwrap();
        assert_eq!(forwarded.vertex_count(), graph.vertex_count());
        assert_eq!(forwarded.edge_count(), graph.edge_count());
        assert_eq!(forwarded.graph_id(), graph.graph_id());
        assert_eq!(
            forwarded.last_commit_seq().unwrap(),
            graph.last_commit_seq()
        );
        assert_eq!(forwarded.last_committed_at(), graph.last_committed_at());
        assert_eq!(forwarded.id_sequences(), graph.id_sequences());
        assert_eq!(forwarded.get_schema().unwrap(), graph.get_schema().unwrap());
        assert_eq!(
            forwarded.definitions().unwrap().count(),
            graph.definitions().unwrap().count()
        );

        let from_forwarded = write(&forwarded);
        let from_loaded = write(&loaded);
        // The data sections are the original's, from either storage.
        assert!(sections_after_manifest(&from_forwarded) == sections_after_manifest(&original));
        assert!(sections_after_manifest(&from_loaded) == sections_after_manifest(&original));
        // Both loaded storages hold the same graph, so the manifests agree too (the memory
        // estimate of a loaded graph may differ from the graph it was written from).
        assert_eq!(info(&from_forwarded), info(&from_loaded));
        let (mut a, mut b) = (info(&from_forwarded), info(&original));
        (a.memory_bytes, b.memory_bytes) = (0, 0);
        assert_eq!(a, b);
    }
}

#[test]
fn a_snapshot_is_not_loaded_into_a_storage_that_holds_elements() {
    let original = write(&TraversalGraph::tinkerpop_modern());
    let full = Forwarding::new(TraversalGraph::tinkerpop_modern());
    let err = persist::read_snapshot_into(original.as_slice(), &ReadOptions::new(), full)
        .err()
        .unwrap();
    assert!(
        matches!(err, PersistError::Graph(GraphError::Unsupported { .. })),
        "{err}"
    );
    // A cleared storage is empty again.
    let mut cleared = Forwarding::new(TraversalGraph::tinkerpop_modern());
    cleared.clear().unwrap();
    let loaded =
        persist::read_snapshot_into(original.as_slice(), &ReadOptions::new(), cleared).unwrap();
    assert_eq!(loaded.vertex_count(), 6);
}

/// A base snapshot and the journal of a few commits after it (labels, properties, edges,
/// drops, a schema change and a catalog entry).
fn snapshot_and_journal() -> (Vec<u8>, Vec<u8>, TraversalGraph) {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let snapshot = write(&graph);
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::EveryCommit)
        .attach(&mut graph)
        .unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "ann")
        .to_list()
        .unwrap();
    graph.traversal_mut().v("1").drop().to_list().unwrap();
    graph
        .traversal_mut()
        .v("2")
        .property("age", 30i64)
        .to_list()
        .unwrap();
    graph.mark("middle").unwrap();
    graph
        .transaction(|g| {
            g.set_schema(GraphSchema::new().with_vertex("person", PropertySchema::object()))?;
            g.set_definition(Definition::query(
                "all",
                QueryDefinition::new("fn all() { g.v() }"),
            ))?;
            let a = g.get_vertex_by_id("2").unwrap();
            let b = g.get_vertex_by_id("3").unwrap();
            g.add_edge(None, Some("likes".into()), a, b, PropertyMap::new())?;
            g.add_vertex_label(a, "friend".into())?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    (snapshot, wal.contents(), graph)
}

#[test]
fn recovery_into_another_storage_type_reaches_the_state_of_the_traversal_graph() {
    let (snapshot, wal, live) = snapshot_and_journal();
    for target in [
        RecoveryTarget::Latest,
        RecoveryTarget::CommitSeq(2),
        RecoveryTarget::Mark("middle".into()),
    ] {
        let reference =
            persist::recover(snapshot.as_slice(), [wal.as_slice()], target.clone()).unwrap();
        let forwarded = Recovery::new_in(Forwarding::default())
            .snapshot(snapshot.as_slice())
            .journal(wal.as_slice())
            .target(target.clone())
            .run()
            .unwrap();
        assert_eq!(forwarded.report, reference.report, "{target}");
        let (a, b) = (write(&forwarded.graph), write(&reference.graph));
        assert!(
            sections_after_manifest(&a) == sections_after_manifest(&b),
            "{target}"
        );
        assert_eq!(info(&a), info(&b), "{target}");
    }
    // To the end: the live graph's data.
    let recovered = Recovery::new_in(Forwarding::default())
        .snapshot(snapshot.as_slice())
        .journal(wal.as_slice())
        .run()
        .unwrap();
    assert!(
        sections_after_manifest(&write(&recovered.graph)) == sections_after_manifest(&write(&live))
    );
    assert_eq!(
        recovered.graph.last_commit_seq().unwrap(),
        live.last_commit_seq()
    );
}

#[test]
fn recovery_into_another_storage_type_without_a_snapshot_starts_from_the_journal() {
    let mut graph = TraversalGraph::new();
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::EveryCommit)
        .attach(&mut graph)
        .unwrap();
    graph.traversal_mut().add_v("a").to_list().unwrap();
    graph.traversal_mut().add_v("b").to_list().unwrap();
    let wal = wal.contents();
    let forwarded = Recovery::new_in(Forwarding::default())
        .journal(wal.as_slice())
        .run()
        .unwrap();
    assert_eq!(forwarded.graph.vertex_count(), 2);
    assert_eq!(forwarded.graph.graph_id(), graph.graph_id());
    assert_eq!(forwarded.graph.last_commit_seq().unwrap(), 2);
    // Into a storage that holds elements: refused.
    let err = Recovery::new_in(Forwarding::new(TraversalGraph::tinkerpop_modern()))
        .journal(wal.as_slice())
        .run()
        .err()
        .unwrap();
    assert!(
        matches!(err, PersistError::Graph(GraphError::Unsupported { .. })),
        "{err}"
    );
}

#[test]
fn only_a_persistent_graph_is_written_through_the_dyn_boundary() {
    // `Graph::new_persistent`: written, with the bytes of the same graph as a TraversalGraph.
    let persistent: Arc<dyn AnyGraph> = Arc::new(Graph::new_persistent(Forwarding::new(
        modern_with_graph_id(),
    )));
    let mut bytes = Vec::new();
    let written = persistent.write_snapshot(&mut bytes).unwrap();
    assert_eq!(written.vertex_count, 6);
    let reference = write(&modern_with_graph_id());
    assert!(sections_after_manifest(&bytes) == sections_after_manifest(&reference));
    // `Graph::new` of a storage other than TraversalGraph: unsupported, nothing written.
    let plain: Arc<dyn AnyGraph> = Arc::new(Graph::new(Forwarding::new(
        TraversalGraph::tinkerpop_modern(),
    )));
    let mut bytes = Vec::new();
    let err = plain.write_snapshot(&mut bytes).unwrap_err();
    assert!(
        matches!(err, PersistError::Graph(GraphError::Unsupported { .. })),
        "{err}"
    );
    assert!(bytes.is_empty());
    // A TraversalGraph is written either way.
    for graph in [
        Graph::new(modern_with_graph_id()),
        Graph::new_persistent(modern_with_graph_id()),
    ] {
        let any: Arc<dyn AnyGraph> = Arc::new(graph);
        let mut bytes = Vec::new();
        any.write_snapshot(&mut bytes).unwrap();
        assert!(sections_after_manifest(&bytes) == sections_after_manifest(&reference));
    }
}
