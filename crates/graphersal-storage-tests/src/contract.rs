//! The contract items, one public function each.
//!
//! Every function takes a fresh, empty storage (`&mut S`) and panics with a descriptive message
//! when the storage violates the contract. [`conformance!`](crate::conformance) generates one
//! `#[test]` per function. Where the trait documentation is silent the reference implementation
//! (`TraversalGraph`) decides; such points say so in their doc comment.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use graphersal::changes::{ChangeSet, CommitHook, HookError, RollbackReason};

use graphersal::catalog::DefinitionFlags;
use graphersal::error::PathWriteFault;
use graphersal::schema::{GraphSchema, PropertySchema, SchemaMode};
use graphersal::storage::CompactString;
use graphersal::{
    ElementProperty,
    error::GraphError,
    prelude::{JsonPath, Number, PropertyMap, VertexValue},
};
use graphersal::{
    catalog::{Definition, DefinitionKind, QueryDefinition, QueryParam},
    storage::{GraphNodeType, GraphStatistics, GraphStorage},
};

// ---------------------------------------------------------------------------------------------
// Helpers (public so a backend's own tests can reuse them)
// ---------------------------------------------------------------------------------------------

/// Builds an element property map from `(key, value)` pairs.
pub fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut element = PropertyMap::default();
    for (key, value) in pairs {
        element.insert(CompactString::new(key), value.clone());
    }
    element
}

/// The statistics `storage` reports, or a full recomputation when it keeps none.
pub fn statistics_of<S: GraphStorage>(storage: &S) -> GraphStatistics {
    storage
        .statistics()
        .unwrap()
        .cloned()
        .unwrap_or_else(|| GraphStatistics::from_storage(storage))
}

fn label_vec(labels: &[&str]) -> Vec<CompactString> {
    labels.iter().map(|l| CompactString::new(l)).collect()
}

/// Adds a vertex, panicking on failure.
pub fn vertex<S: GraphStorage>(
    storage: &mut S,
    id: Option<&str>,
    labels: &[&str],
    properties: PropertyMap,
) -> S::VertexId {
    storage
        .insert_vertex(id.map(CompactString::new), label_vec(labels), properties)
        .unwrap_or_else(|err| panic!("add_vertex({id:?}, {labels:?}) failed: {err}"))
}

/// Adds an edge, panicking on failure.
pub fn edge<S: GraphStorage>(
    storage: &mut S,
    id: Option<&str>,
    label: Option<&str>,
    from: S::VertexId,
    to: S::VertexId,
    properties: PropertyMap,
) -> S::EdgeId {
    storage
        .insert_edge(
            id.map(CompactString::new),
            label.map(CompactString::new),
            from,
            to,
            properties,
        )
        .unwrap_or_else(|err| panic!("add_edge({id:?}, {label:?}) failed: {err}"))
}

fn vertex_id_of<S: GraphStorage>(storage: &S, v: S::VertexId) -> String {
    storage
        .vertex_id(v)
        .unwrap_or_else(|| panic!("vertex {v:?} has no id"))
        .to_string()
}

fn name_set<S: GraphStorage>(storage: &S, label: &str) -> HashSet<S::VertexId> {
    storage.vertices_by_label(label).collect()
}

fn ints(values: &[i32]) -> ElementProperty {
    ElementProperty::Array(values.iter().map(|v| ElementProperty::from(*v)).collect())
}

fn path(text: &str) -> JsonPath {
    text.parse()
        .unwrap_or_else(|err| panic!("test path {text:?} must parse: {err:?}"))
}

/// Checks every cross-method invariant of a storage: counts equal full scans, ids are unique and
/// resolve back to their handle, label sets are deduplicated with the primary label first, the
/// `*_by_label` answers and the statistics equal scans (and reported statistics equal a
/// recomputation), every edge's endpoints exist and appear
/// in the adjacency of both ends. Called after every mutation by the contract items.
pub fn assert_consistent<S: GraphStorage>(storage: &S) {
    let vertices: Vec<S::VertexId> = storage.vertices().collect();
    let edges: Vec<S::EdgeId> = storage.edges().collect();
    assert_eq!(
        storage.vertex_count(),
        vertices.len(),
        "vertex_count() != vertices().count()"
    );
    assert_eq!(
        storage.edge_count(),
        edges.len(),
        "edge_count() != edges().count()"
    );

    let mut seen_ids = HashSet::new();
    let mut label_scan: HashMap<String, HashSet<S::VertexId>> = HashMap::new();
    for &v in &vertices {
        let id = vertex_id_of(storage, v);
        assert!(
            seen_ids.insert(id.clone()),
            "vertex id {id:?} is not unique"
        );
        assert_eq!(
            storage.get_vertex_by_id(&id),
            Some(v),
            "get_vertex_by_id({id:?}) != the handle"
        );
        let labels = storage.vertex_labels(v);
        let distinct: HashSet<&CompactString> = labels.iter().collect();
        assert_eq!(
            distinct.len(),
            labels.len(),
            "labels of {id:?} are not deduplicated: {labels:?}"
        );
        assert_eq!(
            storage.vertex_label(v),
            labels.first().map(|l| l.as_str()),
            "primary label of {id:?}"
        );
        assert!(
            storage.vertex_properties(v).is_some(),
            "vertex_properties({id:?}) is None"
        );
        for label in labels {
            assert!(
                storage.vertex_has_label(v, label),
                "vertex_has_label({id:?}, {label})"
            );
            label_scan.entry(label.to_string()).or_default().insert(v);
        }
    }
    // Primary-label counts: every labeled vertex exactly once, under its first label.
    let mut primary_scan: HashMap<String, usize> = HashMap::new();
    for &v in &vertices {
        if let Some(label) = storage.vertex_label(v) {
            *primary_scan.entry(label.to_string()).or_default() += 1;
        }
    }
    // A storage that reports statistics must report exactly the recomputed ones.
    let stats = GraphStatistics::from_storage(storage);
    if let Some(reported) = storage.statistics().unwrap() {
        assert_eq!(
            reported, &stats,
            "statistics() != GraphStatistics::from_storage"
        );
    }
    assert_eq!(
        stats.vertex_count(),
        vertices.len(),
        "statistics vertex_count"
    );
    assert_eq!(stats.edge_count(), edges.len(), "statistics edge_count");
    let primary: HashMap<String, usize> = stats
        .primary_vertex_label_counts()
        .into_iter()
        .map(|(l, c)| (l.to_string(), c))
        .collect();
    assert_eq!(
        primary, primary_scan,
        "primary_vertex_label_counts() != scan of vertex_label()"
    );
    let counts = stats.vertex_label_counts();
    assert_eq!(
        counts.len(),
        label_scan.len(),
        "vertex_label_counts keys {counts:?}"
    );
    for (label, members) in &label_scan {
        assert_eq!(
            stats.vertex_label_count(label),
            members.len(),
            "vertex_label_count({label})"
        );
        let by_label: HashSet<S::VertexId> = storage.vertices_by_label(label).collect();
        assert_eq!(&by_label, members, "vertices_by_label({label}) != scan");
    }

    let mut edge_ids = HashSet::new();
    let mut edge_label_scan: HashMap<String, HashSet<S::EdgeId>> = HashMap::new();
    let mut out_total = 0;
    for &e in &edges {
        let id = storage
            .edge_id(e)
            .unwrap_or_else(|| panic!("edge {e:?} has no id"))
            .to_string();
        assert!(edge_ids.insert(id.clone()), "edge id {id:?} is not unique");
        assert_eq!(
            storage.get_edge_by_id(&id),
            Some(e),
            "get_edge_by_id({id:?}) != the handle"
        );
        let (from, to) = storage
            .edge_endpoints(e)
            .unwrap_or_else(|| panic!("edge {id:?} has no endpoints"));
        assert!(
            storage.vertex_id(from).is_some() && storage.vertex_id(to).is_some(),
            "edge {id:?} endpoint missing"
        );
        assert!(
            storage.out_edges(from).any(|x| x == e),
            "edge {id:?} missing from out_edges of its source"
        );
        assert!(
            storage.in_edges(to).any(|x| x == e),
            "edge {id:?} missing from in_edges of its target"
        );
        assert!(
            storage.edge_properties(e).is_some(),
            "edge_properties({id:?}) is None"
        );
        if let Some(label) = storage.edge_label(e) {
            edge_label_scan
                .entry(label.to_string())
                .or_default()
                .insert(e);
        }
    }
    for &v in &vertices {
        for e in storage.out_edges(v) {
            out_total += 1;
            assert_eq!(
                storage.edge_endpoints(e).map(|(from, _)| from),
                Some(v),
                "out_edges contains a foreign edge"
            );
        }
        for e in storage.in_edges(v) {
            assert_eq!(
                storage.edge_endpoints(e).map(|(_, to)| to),
                Some(v),
                "in_edges contains a foreign edge"
            );
        }
    }
    assert_eq!(
        out_total,
        edges.len(),
        "the out_edges of all vertices do not add up to edge_count()"
    );
    let counts = stats.edge_label_counts();
    assert_eq!(
        counts.len(),
        edge_label_scan.len(),
        "edge_label_counts keys {counts:?}"
    );
    for (label, members) in &edge_label_scan {
        assert_eq!(
            stats.edge_label_count(label),
            members.len(),
            "edge_label_count({label})"
        );
        let by_label: HashSet<S::EdgeId> = storage.edges_by_label(label).collect();
        assert_eq!(&by_label, members, "edges_by_label({label}) != scan");
    }
}

// ---------------------------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------------------------

/// An explicit vertex id is stored verbatim, found by `get_vertex_by_id`, and non-numeric ids work.
pub fn explicit_vertex_ids_are_preserved<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, Some("alpha"), &["n"], PropertyMap::default());
    let b = vertex(storage, Some("42"), &["n"], PropertyMap::default());
    assert_eq!(storage.vertex_id(a), Some("alpha"));
    assert_eq!(storage.vertex_id(b), Some("42"));
    assert_eq!(storage.get_vertex_by_id("alpha"), Some(a));
    assert_eq!(storage.get_vertex_by_id("42"), Some(b));
    assert_eq!(storage.get_vertex_by_id("missing"), None);
    assert_consistent(storage);
}

/// Vertices added without an id get distinct, non-empty ids.
pub fn auto_vertex_ids_are_unique<S: GraphStorage>(storage: &mut S) {
    let handles: Vec<S::VertexId> = (0..20)
        .map(|_| vertex(storage, None, &[], PropertyMap::default()))
        .collect();
    let ids: HashSet<String> = handles.iter().map(|&v| vertex_id_of(storage, v)).collect();
    assert_eq!(ids.len(), 20, "auto ids collide");
    assert!(ids.iter().all(|id| !id.is_empty()));
    assert_consistent(storage);
}

/// An explicit numeric id advances the auto-id sequence, so a later auto id neither collides with
/// it nor falls below it.
///
/// The trait is silent on the sequence; `TraversalGraph` keeps one counter per element kind and an
/// explicit numeric id raises it to at least that value.
pub fn explicit_numeric_vertex_id_bumps_the_sequence<S: GraphStorage>(storage: &mut S) {
    vertex(storage, Some("10"), &[], PropertyMap::default());
    let auto = vertex(storage, None, &[], PropertyMap::default());
    let id = vertex_id_of(storage, auto);
    assert_ne!(id, "10", "auto id collided with the explicit id");
    let numeric: u64 = id
        .parse()
        .unwrap_or_else(|_| panic!("auto id {id:?} is not numeric after a numeric explicit id"));
    assert!(
        numeric > 10,
        "auto id {id} does not follow the explicit numeric id 10"
    );
    assert_consistent(storage);
}

/// Adding a vertex whose id exists fails with `VertexIdUniqueConstraint` and changes nothing.
pub fn duplicate_vertex_id_is_rejected<S: GraphStorage>(storage: &mut S) {
    vertex(storage, Some("x"), &["a"], PropertyMap::default());
    let err = storage
        .insert_vertex(Some("x".into()), label_vec(&["b"]), PropertyMap::default())
        .expect_err("a duplicate vertex id must be rejected");
    assert!(
        matches!(err, GraphError::VertexIdUniqueConstraint { .. }),
        "got {err:?}"
    );
    assert_eq!(storage.vertex_count(), 1);
    assert_eq!(
        name_set(storage, "b").len(),
        0,
        "the rejected vertex left a label index entry"
    );
    assert_consistent(storage);
}

/// Edge ids are preserved and unique, a duplicate fails with `EdgeIdUniqueConstraint`, and an
/// explicit numeric id advances the auto sequence like it does for vertices.
pub fn edge_ids_are_preserved_unique_and_bump_the_sequence<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &[], PropertyMap::default());
    let b = vertex(storage, None, &[], PropertyMap::default());
    let e = edge(storage, Some("e1"), Some("l"), a, b, PropertyMap::default());
    assert_eq!(storage.edge_id(e), Some("e1"));
    assert_eq!(storage.get_edge_by_id("e1"), Some(e));
    let err = storage
        .insert_edge(
            Some("e1".into()),
            Some("l".into()),
            a,
            b,
            PropertyMap::default(),
        )
        .expect_err("a duplicate edge id must be rejected");
    assert!(
        matches!(err, GraphError::EdgeIdUniqueConstraint { .. }),
        "got {err:?}"
    );
    assert_eq!(storage.edge_count(), 1);
    edge(storage, Some("20"), Some("l"), a, b, PropertyMap::default());
    let auto = edge(storage, None, Some("l"), a, b, PropertyMap::default());
    let id = storage.edge_id(auto).expect("auto edge id").to_string();
    let numeric: u64 = id
        .parse()
        .unwrap_or_else(|_| panic!("auto edge id {id:?} is not numeric"));
    assert!(
        numeric > 20,
        "auto edge id {id} does not follow the explicit numeric id 20"
    );
    assert_consistent(storage);
}

/// After a drop, the id lookups and the accessors of the stale handle find nothing.
pub fn lookups_after_drop_find_nothing<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, Some("a"), &["p"], props(&[("k", 1.into())]));
    let b = vertex(storage, Some("b"), &["p"], PropertyMap::default());
    let e = edge(storage, Some("e"), Some("l"), a, b, PropertyMap::default());
    storage.drop_edge(e).expect("drop_edge");
    assert_eq!(storage.get_edge_by_id("e"), None);
    assert_eq!(storage.edge_id(e), None);
    assert!(storage.edge_endpoints(e).is_none());
    assert!(storage.edge_properties(e).is_none());
    storage.drop_vertex(a).expect("drop_vertex");
    assert_eq!(storage.get_vertex_by_id("a"), None);
    assert_eq!(storage.vertex_id(a), None);
    assert!(storage.vertex_properties(a).is_none());
    assert!(storage.vertex_labels(a).is_empty());
    assert_eq!(storage.vertex_label(a), None);
    assert_eq!(storage.vertex_count(), 1);
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------------------------

/// A vertex's labels are an ordered set: insertion order is kept, duplicates collapse, the first
/// label is the primary one, `vertex_has_label` and `vertices_by_label` use set membership.
pub fn vertex_labels_are_an_ordered_set<S: GraphStorage>(storage: &mut S) {
    let v = vertex(
        storage,
        Some("v"),
        &["b", "a", "b", "c", "a"],
        PropertyMap::default(),
    );
    let labels: Vec<&str> = storage
        .vertex_labels(v)
        .iter()
        .map(|l| l.as_str())
        .collect();
    assert_eq!(labels, ["b", "a", "c"]);
    assert_eq!(storage.vertex_label(v), Some("b"));
    assert!(storage.vertex_has_label(v, "c") && !storage.vertex_has_label(v, "z"));
    for label in ["a", "b", "c"] {
        assert!(
            name_set(storage, label).contains(&v),
            "vertices_by_label({label}) misses a member"
        );
    }
    let stats = statistics_of(storage);
    assert_eq!(
        (
            stats.vertex_label_count("a"),
            stats.vertex_label_count("b"),
            stats.vertex_label_count("c")
        ),
        (1, 1, 1)
    );
    assert_eq!(stats.primary_vertex_label_counts(), vec![("b", 1)]);
    assert_consistent(storage);
}

/// A vertex without labels has an empty label set and no primary label, and is in no label count.
pub fn unlabeled_vertex_has_no_labels<S: GraphStorage>(storage: &mut S) {
    let v = vertex(storage, None, &[], PropertyMap::default());
    assert!(storage.vertex_labels(v).is_empty());
    assert_eq!(storage.vertex_label(v), None);
    assert!(statistics_of(storage).vertex_label_counts().is_empty());
    assert_consistent(storage);
}

/// `add_vertex_label` and `remove_vertex_label` report whether the set changed, keep the order,
/// make the next label primary when the primary one goes, and keep the label index in step.
pub fn add_and_remove_vertex_label<S: GraphStorage>(storage: &mut S) {
    let v = vertex(storage, Some("v"), &["a"], PropertyMap::default());
    assert!(storage.add_vertex_label(v, "b".into()).expect("add"));
    assert!(
        !storage.add_vertex_label(v, "b".into()).expect("add again"),
        "re-adding is a no-op"
    );
    let labels: Vec<&str> = storage
        .vertex_labels(v)
        .iter()
        .map(|l| l.as_str())
        .collect();
    assert_eq!(labels, ["a", "b"]);
    assert_consistent(storage);
    assert!(storage.remove_vertex_label(v, "a").expect("remove"));
    assert!(!storage.remove_vertex_label(v, "a").expect("remove again"));
    assert_eq!(
        storage.vertex_label(v),
        Some("b"),
        "the next label becomes primary"
    );
    assert!(name_set(storage, "a").is_empty());
    assert_consistent(storage);
    assert!(storage.remove_vertex_label(v, "b").expect("remove last"));
    assert!(storage.vertex_labels(v).is_empty());
    assert_consistent(storage);
}

/// A label containing `::` is rejected with `InvalidLabel` (GraphML reserves it as a delimiter),
/// by `insert_vertex` and by `add_vertex_label`, and nothing is stored.
pub fn label_with_the_reserved_delimiter_is_rejected<S: GraphStorage>(storage: &mut S) {
    let err = storage
        .insert_vertex(None, label_vec(&["a::b"]), PropertyMap::default())
        .expect_err("reserved delimiter");
    assert!(
        matches!(err, GraphError::InvalidLabel { .. }),
        "got {err:?}"
    );
    assert_eq!(storage.vertex_count(), 0);
    let v = vertex(storage, None, &["ok"], PropertyMap::default());
    let err = storage
        .add_vertex_label(v, "x::y".into())
        .expect_err("reserved delimiter");
    assert!(
        matches!(err, GraphError::InvalidLabel { .. }),
        "got {err:?}"
    );
    assert_eq!(storage.vertex_labels(v).len(), 1);
    assert_consistent(storage);
}

/// Counts, `*_by_label` and label counts equal full scans after every mutation of a mixed
/// sequence (add, relabel, drop of vertices and edges).
pub fn labels_index_matches_scans_after_every_mutation<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, Some("a"), &["x", "y"], PropertyMap::default());
    assert_consistent(storage);
    let b = vertex(storage, Some("b"), &["y"], PropertyMap::default());
    assert_consistent(storage);
    let c = vertex(storage, Some("c"), &[], PropertyMap::default());
    assert_consistent(storage);
    let e1 = edge(storage, None, Some("l"), a, b, PropertyMap::default());
    assert_consistent(storage);
    let e2 = edge(storage, None, None, b, c, PropertyMap::default());
    assert_consistent(storage);
    storage.add_vertex_label(c, "x".into()).expect("add label");
    assert_consistent(storage);
    storage.remove_vertex_label(a, "x").expect("remove label");
    assert_consistent(storage);
    storage.drop_edge(e1).expect("drop edge");
    assert_consistent(storage);
    storage.drop_vertex(b).expect("drop vertex");
    assert_consistent(storage);
    assert_eq!(storage.edge_count(), 0, "dropping b must take e2 with it");
    let _ = e2;
    storage.drop_vertex(a).expect("drop vertex");
    storage.drop_vertex(c).expect("drop vertex");
    assert_consistent(storage);
    assert_eq!(storage.vertex_count(), 0);
    let stats = statistics_of(storage);
    assert!(stats.vertex_label_counts().is_empty() && stats.edge_label_counts().is_empty());
}

// ---------------------------------------------------------------------------------------------
// Edges
// ---------------------------------------------------------------------------------------------

/// An edge has the endpoints it was added with (direction matters), one optional label, and its
/// properties.
pub fn edge_endpoints_and_single_label<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &[], PropertyMap::default());
    let b = vertex(storage, None, &[], PropertyMap::default());
    let labeled = edge(
        storage,
        None,
        Some("knows"),
        a,
        b,
        props(&[("w", 1.into())]),
    );
    let plain = edge(storage, None, None, b, a, PropertyMap::default());
    assert_eq!(storage.edge_endpoints(labeled), Some((a, b)));
    assert_eq!(storage.edge_endpoints(plain), Some((b, a)));
    assert_eq!(storage.edge_label(labeled), Some("knows"));
    assert_eq!(storage.edge_label(plain), None);
    assert_eq!(
        storage.edge_properties(labeled).and_then(|p| p.get("w")),
        Some(&ElementProperty::from(1))
    );
    assert_consistent(storage);
}

/// `set_edge_label` reports whether the label changed, labels an unlabeled edge, and keeps the
/// edge label index and the edge label counts in step. A storage that keeps the default
/// (`GraphError::Unsupported`) passes.
pub fn set_edge_label_changes_label_index_and_counts<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &[], PropertyMap::default());
    let b = vertex(storage, None, &[], PropertyMap::default());
    let labeled = edge(
        storage,
        Some("ab"),
        Some("knows"),
        a,
        b,
        props(&[("w", 1.into())]),
    );
    let plain = edge(storage, Some("ba"), None, b, a, PropertyMap::default());
    match storage.set_edge_label(labeled, "likes".into()) {
        Err(GraphError::Unsupported { .. }) => return,
        result => assert!(result.expect("relabel"), "a new label is a change"),
    }
    assert!(
        !storage
            .set_edge_label(labeled, "likes".into())
            .expect("same label"),
        "the same label again is a no-op"
    );
    assert_eq!(storage.edge_label(labeled), Some("likes"));
    assert_eq!(
        storage.edge_properties(labeled).and_then(|p| p.get("w")),
        Some(&ElementProperty::from(1)),
        "a label change keeps the properties"
    );
    assert_eq!(storage.edge_endpoints(labeled), Some((a, b)));
    assert!(
        storage
            .set_edge_label(plain, "likes".into())
            .expect("label")
    );
    assert_eq!(storage.edge_label(plain), Some("likes"));
    let mut by_label: Vec<S::EdgeId> = storage.edges_by_label("likes").collect();
    by_label.sort_by_key(|e| storage.edge_id(*e).map(str::to_string));
    assert_eq!(by_label, vec![labeled, plain]);
    assert_eq!(storage.edges_by_label("knows").count(), 0);
    let stats = statistics_of(storage);
    assert_eq!(stats.edge_label_count("likes"), 2);
    assert_eq!(stats.edge_label_count("knows"), 0);
    assert_consistent(storage);
    storage.drop_edge(plain).expect("drop");
    assert!(
        storage.set_edge_label(plain, "x".into()).is_err(),
        "a dropped edge is an error"
    );
    assert_consistent(storage);
}

/// `out_edges` and `in_edges` list exactly the edges leaving and entering a vertex, self-loops and
/// parallel edges included.
pub fn adjacency_matches_endpoints<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &[], PropertyMap::default());
    let b = vertex(storage, None, &[], PropertyMap::default());
    let e1 = edge(storage, None, Some("l"), a, b, PropertyMap::default());
    let e2 = edge(storage, None, Some("l"), a, b, PropertyMap::default());
    let loop_edge = edge(storage, None, Some("self"), a, a, PropertyMap::default());
    let out_a: HashSet<S::EdgeId> = storage.out_edges(a).collect();
    let in_a: HashSet<S::EdgeId> = storage.in_edges(a).collect();
    let in_b: HashSet<S::EdgeId> = storage.in_edges(b).collect();
    assert_eq!(out_a, HashSet::from([e1, e2, loop_edge]));
    assert_eq!(in_a, HashSet::from([loop_edge]));
    assert_eq!(in_b, HashSet::from([e1, e2]));
    assert_eq!(storage.out_edges(b).count(), 0);
    assert_consistent(storage);
}

/// Adding an edge to a vertex that was dropped is an error, never a panic, and stores nothing.
///
/// The trait documents no error for this; returning `Err` is the only behaviour consistent with
/// "no panics" and with `add_vertex_label`/`set_vertex_property`, which fail on an invalid handle.
pub fn edge_to_a_dropped_vertex_is_an_error_not_a_panic<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &[], PropertyMap::default());
    let b = vertex(storage, None, &[], PropertyMap::default());
    storage.drop_vertex(b).expect("drop_vertex");
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        storage
            .insert_edge(None, Some("l".into()), a, b, PropertyMap::default())
            .map(|_| ())
    }));
    match outcome {
        Err(_) => panic!("add_edge to a dropped vertex panicked instead of returning an error"),
        Ok(Ok(())) => panic!("add_edge to a dropped vertex succeeded"),
        Ok(Err(_)) => {}
    }
    assert_eq!(storage.edge_count(), 0);
    assert_consistent(storage);
}

/// The storage contract on `GraphStorage::VertexId`/`EdgeId`: a handle to a removed element never
/// resolves to another element, also not to one stored in the same place later. Slot reuse is
/// forced by dropping and adding (a backend that never reuses passes trivially). Every read through
/// the stale handle behaves as "removed", and writes through it fail without touching the new
/// element.
pub fn stale_handle_never_aliases<S: GraphStorage>(storage: &mut S) {
    let a = vertex(
        storage,
        Some("a"),
        &["old"],
        props(&[("name", "old".into())]),
    );
    let b = vertex(storage, Some("b"), &["n"], PropertyMap::default());
    let stale_edge = edge(
        storage,
        Some("e"),
        Some("old"),
        a,
        b,
        props(&[("w", 1.into())]),
    );
    storage.drop_edge(stale_edge).expect("drop_edge");
    let fresh_edge = edge(
        storage,
        Some("e2"),
        Some("new"),
        b,
        b,
        props(&[("w", 2.into())]),
    );
    let stale_vertex = a;
    storage.drop_vertex(stale_vertex).expect("drop_vertex");
    let fresh_vertex = vertex(
        storage,
        Some("c"),
        &["new"],
        props(&[("name", "new".into())]),
    );

    assert_ne!(
        stale_vertex, fresh_vertex,
        "a new vertex got the stale handle"
    );
    assert!(
        !storage.contains_vertex(stale_vertex),
        "the stale vertex handle resolves"
    );
    assert!(storage.contains_vertex(fresh_vertex));
    assert_eq!(
        storage.vertex_id(stale_vertex),
        None,
        "stale handle reads another id"
    );
    assert_eq!(storage.vertex_label(stale_vertex), None);
    assert!(storage.vertex_labels(stale_vertex).is_empty());
    assert!(storage.vertex_properties(stale_vertex).is_none());
    assert_eq!(storage.out_edges(stale_vertex).count(), 0);
    assert_eq!(storage.in_edges(stale_vertex).count(), 0);

    assert_ne!(stale_edge, fresh_edge, "a new edge got the stale handle");
    assert!(
        !storage.contains_edge(stale_edge),
        "the stale edge handle resolves"
    );
    assert!(storage.contains_edge(fresh_edge));
    assert_eq!(
        storage.edge_id(stale_edge),
        None,
        "stale handle reads another id"
    );
    assert_eq!(storage.edge_label(stale_edge), None);
    assert!(storage.edge_properties(stale_edge).is_none());
    assert_eq!(storage.edge_endpoints(stale_edge), None);

    // Writes through the stale handles fail and leave the new elements alone.
    assert!(matches!(
        storage.set_vertex_property(stale_vertex, "name".into(), "x".into()),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Vertex,
            ..
        })
    ));
    assert!(matches!(
        storage.add_vertex_label(stale_vertex, "x".into()),
        Err(GraphError::ElementRemoved { .. })
    ));
    assert!(matches!(
        storage.set_edge_property(stale_edge, "w".into(), 9.into()),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Edge,
            ..
        })
    ));
    assert!(matches!(
        storage.insert_edge(None, None, stale_vertex, b, PropertyMap::default()),
        Err(GraphError::ElementRemoved { .. })
    ));
    storage.drop_vertex(stale_vertex).expect("idempotent");
    storage.drop_edge(stale_edge).expect("idempotent");
    assert_eq!(
        storage
            .vertex_properties(fresh_vertex)
            .and_then(|p| p.get("name")),
        Some(&ElementProperty::from("new"))
    );
    assert_eq!(
        storage.vertex_labels(fresh_vertex),
        &[CompactString::from("new")]
    );
    assert_eq!(
        storage.edge_properties(fresh_edge).and_then(|p| p.get("w")),
        Some(&ElementProperty::from(2))
    );
    assert_eq!(storage.edge_label(fresh_edge), Some("new"));
    assert_eq!(storage.vertex_count(), 2);
    assert_eq!(storage.edge_count(), 1);
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------------------------

fn sample_values() -> Vec<(&'static str, ElementProperty)> {
    let nested = props(&[
        ("inner", ints(&[1, 2])),
        ("flag", ElementProperty::Boolean(true)),
    ]);
    vec![
        ("null", ElementProperty::Null),
        ("string", "text".into()),
        ("int", ElementProperty::from(7)),
        ("float", ElementProperty::Number(Number::from_f64(2.5))),
        ("bool", ElementProperty::Boolean(false)),
        (
            "uuid",
            ElementProperty::Uuid(0x550e8400_e29b_41d4_a716_446655440000),
        ),
        (
            "array",
            ElementProperty::Array(vec![
                1.into(),
                "two".into(),
                ElementProperty::Null,
                ints(&[3]),
            ]),
        ),
        ("object", ElementProperty::Object(Box::new(nested))),
    ]
}

/// Every [`ElementProperty`] kind (null, string, int, float, bool, uuid, nested array, nested
/// object) round-trips on vertices and edges, through `add_*` and through `set_*_property`.
pub fn every_property_kind_round_trips<S: GraphStorage>(storage: &mut S) {
    let initial: PropertyMap = sample_values()
        .into_iter()
        .map(|(k, v)| (CompactString::new(k), v))
        .collect();
    let v = vertex(storage, None, &["p"], initial.clone());
    let w = vertex(storage, None, &["p"], PropertyMap::default());
    let e = edge(storage, None, Some("l"), v, w, initial.clone());
    for (key, value) in sample_values() {
        assert_eq!(
            storage.vertex_properties(v).and_then(|p| p.get(key)),
            Some(&value),
            "vertex {key} via add_vertex"
        );
        assert_eq!(
            storage.edge_properties(e).and_then(|p| p.get(key)),
            Some(&value),
            "edge {key} via add_edge"
        );
        storage
            .set_vertex_property(w, key.into(), value.clone())
            .expect("set_vertex_property");
        assert_eq!(
            storage.vertex_properties(w).and_then(|p| p.get(key)),
            Some(&value),
            "vertex {key} via set"
        );
    }
    let f = edge(storage, None, Some("l"), v, w, PropertyMap::default());
    for (key, value) in sample_values() {
        storage
            .set_edge_property(f, key.into(), value.clone())
            .expect("set_edge_property");
        assert_eq!(
            storage.edge_properties(f).and_then(|p| p.get(key)),
            Some(&value),
            "edge {key} via set"
        );
    }
    assert_consistent(storage);
}

/// A property set to null is present with the value null; a property never set is absent.
pub fn null_is_stored_and_absent_is_not<S: GraphStorage>(storage: &mut S) {
    let v = vertex(storage, None, &[], props(&[("n", ElementProperty::Null)]));
    let props_of_v = storage.vertex_properties(v).expect("properties");
    assert_eq!(props_of_v.get("n"), Some(&ElementProperty::Null));
    assert!(props_of_v.get("absent").is_none());
    assert!(!props_of_v.contains_key("absent"));
    assert_consistent(storage);
}

/// Setting a key again overwrites it (one entry per key), and `drop_*_property` removes it; removing
/// an absent property is a no-op, not an error.
pub fn property_overwrite_and_removal<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &[], props(&[("k", 1.into())]));
    let b = vertex(storage, None, &[], PropertyMap::default());
    let e = edge(storage, None, Some("l"), a, b, props(&[("k", 1.into())]));
    storage
        .set_vertex_property(a, "k".into(), "two".into())
        .expect("overwrite");
    storage
        .set_edge_property(e, "k".into(), "two".into())
        .expect("overwrite");
    assert_eq!(storage.vertex_properties(a).map(|p| p.len()), Some(1));
    assert_eq!(
        storage.vertex_properties(a).and_then(|p| p.get("k")),
        Some(&"two".into())
    );
    assert_eq!(
        storage.edge_properties(e).and_then(|p| p.get("k")),
        Some(&"two".into())
    );
    storage.drop_vertex_property("k", a).expect("drop");
    storage.drop_edge_property("k", e).expect("drop");
    assert!(storage.vertex_properties(a).is_some_and(|p| p.is_empty()));
    assert!(storage.edge_properties(e).is_some_and(|p| p.is_empty()));
    storage
        .drop_vertex_property("k", a)
        .expect("dropping an absent property is a no-op");
    storage
        .drop_edge_property("k", e)
        .expect("dropping an absent property is a no-op");
    assert_consistent(storage);
}

/// Behaviour on a stale handle (documented on `GraphStorage::VertexId`): `drop_vertex` and
/// `drop_edge` are idempotent (`Ok`), every other write fails with `ElementRemoved` of the right
/// kind.
pub fn drop_on_an_invalid_handle<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &["l"], PropertyMap::default());
    let b = vertex(storage, None, &[], PropertyMap::default());
    let e = edge(storage, None, Some("l"), a, b, PropertyMap::default());
    storage.drop_edge(e).expect("drop_edge");
    storage.drop_vertex(a).expect("drop_vertex");
    storage
        .drop_edge(e)
        .expect("dropping a dropped edge is idempotent");
    storage
        .drop_vertex(a)
        .expect("dropping a dropped vertex is idempotent");
    assert!(matches!(
        storage.set_vertex_property(a, "k".into(), 1.into()),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Vertex,
            ..
        })
    ));
    assert!(matches!(
        storage.drop_vertex_property("k", a),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Vertex,
            ..
        })
    ));
    assert!(matches!(
        storage.add_vertex_label(a, "x".into()),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Vertex,
            ..
        })
    ));
    assert!(matches!(
        storage.remove_vertex_label(a, "l"),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Vertex,
            ..
        })
    ));
    assert!(matches!(
        storage.set_edge_property(e, "k".into(), 1.into()),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Edge,
            ..
        })
    ));
    assert!(matches!(
        storage.drop_edge_property("k", e),
        Err(GraphError::ElementRemoved {
            kind: GraphNodeType::Edge,
            ..
        })
    ));
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// JSONPath writes and removals
// ---------------------------------------------------------------------------------------------

/// `set_*_property_path`: missing object keys are created, `[len]` appends, an overwrite replaces
/// the leaf only, and a type mismatch fails with `InvalidPropertyPath` leaving the value untouched.
pub fn jpath_writes_create_append_and_fault<S: GraphStorage>(storage: &mut S) {
    let a = vertex(
        storage,
        None,
        &[],
        props(&[("name", "x".into()), ("tags", ints(&[1]))]),
    );
    let b = vertex(storage, None, &[], PropertyMap::default());
    let e = edge(storage, None, Some("l"), a, b, PropertyMap::default());

    storage
        .set_vertex_property_path(a, &path("meta.k"), 5.into())
        .expect("creates meta.k");
    let meta = props(&[("k", 5.into())]);
    assert_eq!(
        storage.vertex_properties(a).and_then(|p| p.get("meta")),
        Some(&ElementProperty::Object(Box::new(meta)))
    );
    storage
        .set_vertex_property_path(a, &path("meta.k"), 6.into())
        .expect("overwrites the leaf");
    storage
        .set_vertex_property_path(a, &path("meta.deep.x"), true.into())
        .expect("creates nested keys");
    storage
        .set_vertex_property_path(a, &path("tags[1]"), 2.into())
        .expect("[len] appends");
    assert_eq!(
        storage.vertex_properties(a).and_then(|p| p.get("tags")),
        Some(&ints(&[1, 2]))
    );
    storage
        .set_vertex_property_path(a, &path("tags[0]"), 9.into())
        .expect("overwrites an element");
    assert_eq!(
        storage.vertex_properties(a).and_then(|p| p.get("tags")),
        Some(&ints(&[9, 2]))
    );

    let before = storage.vertex_properties(a).cloned();
    let err = storage
        .set_vertex_property_path(a, &path("name.x"), 1.into())
        .expect_err("key on a string");
    assert!(
        matches!(
            err,
            GraphError::InvalidPropertyPath {
                fault: PathWriteFault::KeyOnNonObject { .. },
                ..
            }
        ),
        "got {err:?}"
    );
    let err = storage
        .set_vertex_property_path(a, &path("tags[7]"), 1.into())
        .expect_err("index out of range");
    assert!(
        matches!(
            err,
            GraphError::InvalidPropertyPath {
                fault: PathWriteFault::IndexOutOfRange { .. },
                ..
            }
        ),
        "got {err:?}"
    );
    let err = storage
        .set_vertex_property_path(a, &path("name[0]"), 1.into())
        .expect_err("index on a string");
    assert!(
        matches!(
            err,
            GraphError::InvalidPropertyPath {
                fault: PathWriteFault::IndexOnNonArray { .. },
                ..
            }
        ),
        "got {err:?}"
    );
    assert_eq!(
        storage.vertex_properties(a).cloned(),
        before,
        "a failed path write must not change the value"
    );

    storage
        .set_edge_property_path(e, &path("meta.k"), 1.into())
        .expect("edge path write");
    assert!(
        storage
            .edge_properties(e)
            .is_some_and(|p| p.contains_key("meta"))
    );
    let err = storage
        .set_edge_property_path(e, &path("meta.k.z"), 1.into())
        .expect_err("key on a number");
    assert!(
        matches!(err, GraphError::InvalidPropertyPath { .. }),
        "got {err:?}"
    );
    assert_consistent(storage);
}

/// `remove_*_property_path`: an object key is removed (the emptied parent stays), an array element
/// is removed with shift, a one-segment path is a plain property removal, and a missing path or a
/// type mismatch on the way is not an error and changes nothing.
pub fn jpath_removals_shift_and_ignore_missing<S: GraphStorage>(storage: &mut S) {
    let meta = props(&[("k", 1.into())]);
    let a = vertex(
        storage,
        None,
        &[],
        props(&[
            ("tags", ints(&[1, 2, 3])),
            ("meta", ElementProperty::Object(Box::new(meta))),
            ("plain", 1.into()),
        ]),
    );
    let b = vertex(storage, None, &[], PropertyMap::default());
    let e = edge(
        storage,
        None,
        Some("l"),
        a,
        b,
        props(&[("tags", ints(&[4, 5]))]),
    );

    storage
        .remove_vertex_property_path(a, &path("tags[0]"))
        .expect("removes with shift");
    assert_eq!(
        storage.vertex_properties(a).and_then(|p| p.get("tags")),
        Some(&ints(&[2, 3]))
    );
    storage
        .remove_vertex_property_path(a, &path("tags[-1]"))
        .expect("negative index counts from the end");
    assert_eq!(
        storage.vertex_properties(a).and_then(|p| p.get("tags")),
        Some(&ints(&[2]))
    );
    storage
        .remove_vertex_property_path(a, &path("meta.k"))
        .expect("removes the key");
    assert_eq!(
        storage.vertex_properties(a).and_then(|p| p.get("meta")),
        Some(&ElementProperty::Object(Box::default())),
        "the emptied parent stays"
    );
    storage
        .remove_vertex_property_path(a, &path("plain"))
        .expect("one segment = drop_vertex_property");
    assert!(
        storage
            .vertex_properties(a)
            .is_some_and(|p| !p.contains_key("plain"))
    );

    let before = storage.vertex_properties(a).cloned();
    for missing in ["nothing", "nothing.x", "tags[9]", "meta.absent", "tags.k"] {
        storage
            .remove_vertex_property_path(a, &path(missing))
            .unwrap_or_else(|err| {
                panic!("removing the missing path {missing} must not fail: {err}")
            });
    }
    assert_eq!(
        storage.vertex_properties(a).cloned(),
        before,
        "removals of missing paths change nothing"
    );

    storage
        .remove_edge_property_path(e, &path("tags[0]"))
        .expect("edge removal");
    assert_eq!(
        storage.edge_properties(e).and_then(|p| p.get("tags")),
        Some(&ints(&[5]))
    );
    storage
        .remove_edge_property_path(e, &path("tags"))
        .expect("edge one segment");
    assert!(storage.edge_properties(e).is_some_and(|p| p.is_empty()));
    assert_consistent(storage);
}

/// A vertex or an edge can never be stored as a property value, at any depth, by
/// `insert_vertex`, `insert_edge`, `set_*_property` and the path writes (`InvalidPropertyValue`),
/// and nothing is stored.
pub fn graph_elements_are_rejected_as_property_values<S: GraphStorage>(storage: &mut S) {
    let vertex_value = || {
        ElementProperty::Vertex(Box::new(VertexValue::new(
            "x",
            Vec::new(),
            PropertyMap::default(),
        )))
    };
    let edge_value = || ElementProperty::Edge(Box::default());
    let bad_values = [
        ("direct vertex", vertex_value()),
        ("direct edge", edge_value()),
        (
            "vertex in array",
            ElementProperty::Array(vec![1.into(), vertex_value()]),
        ),
        (
            "edge in object",
            ElementProperty::Object(Box::new(props(&[("deep", edge_value())]))),
        ),
    ];
    let v = vertex(storage, None, &[], PropertyMap::default());
    let w = vertex(storage, None, &[], PropertyMap::default());
    let e = edge(storage, None, Some("l"), v, w, PropertyMap::default());
    for (what, bad) in bad_values {
        let err = storage
            .insert_vertex(None, vec![], props(&[("p", bad.clone())]))
            .expect_err(what);
        assert!(
            matches!(err, GraphError::InvalidPropertyValue { .. }),
            "add_vertex {what}: {err:?}"
        );
        let err = storage
            .insert_edge(None, Some("l".into()), v, w, props(&[("p", bad.clone())]))
            .expect_err(what);
        assert!(
            matches!(err, GraphError::InvalidPropertyValue { .. }),
            "add_edge {what}: {err:?}"
        );
        let err = storage
            .set_vertex_property(v, "p".into(), bad.clone())
            .expect_err(what);
        assert!(
            matches!(err, GraphError::InvalidPropertyValue { .. }),
            "set_vertex_property {what}: {err:?}"
        );
        let err = storage
            .set_edge_property(e, "p".into(), bad.clone())
            .expect_err(what);
        assert!(
            matches!(err, GraphError::InvalidPropertyValue { .. }),
            "set_edge_property {what}: {err:?}"
        );
        let err = storage
            .set_vertex_property_path(v, &path("p.q"), bad.clone())
            .expect_err(what);
        assert!(
            matches!(err, GraphError::InvalidPropertyValue { .. }),
            "set_vertex_property_path {what}: {err:?}"
        );
    }
    assert_eq!(
        storage.vertex_count(),
        2,
        "a rejected insert_vertex left a vertex behind"
    );
    assert_eq!(
        storage.edge_count(),
        1,
        "a rejected insert_edge left an edge behind"
    );
    assert!(storage.vertex_properties(v).is_some_and(|p| p.is_empty()));
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// Drops
// ---------------------------------------------------------------------------------------------

/// Dropping a vertex removes every incident edge (incoming, outgoing and self-loops) from the
/// counts, the id lookups, the label index and the adjacency of the other endpoints.
pub fn drop_vertex_cascades_its_edges<S: GraphStorage>(storage: &mut S) {
    let hub = vertex(storage, Some("hub"), &["h"], PropertyMap::default());
    let x = vertex(storage, Some("x"), &["o"], PropertyMap::default());
    let y = vertex(storage, Some("y"), &["o"], PropertyMap::default());
    edge(
        storage,
        Some("out"),
        Some("a"),
        hub,
        x,
        PropertyMap::default(),
    );
    edge(
        storage,
        Some("in"),
        Some("b"),
        y,
        hub,
        PropertyMap::default(),
    );
    edge(
        storage,
        Some("loop"),
        Some("c"),
        hub,
        hub,
        PropertyMap::default(),
    );
    let keep = edge(
        storage,
        Some("keep"),
        Some("d"),
        x,
        y,
        PropertyMap::default(),
    );
    storage.drop_vertex(hub).expect("drop_vertex");
    assert_eq!(storage.vertex_count(), 2);
    assert_eq!(storage.edge_count(), 1);
    for gone in ["out", "in", "loop"] {
        assert_eq!(
            storage.get_edge_by_id(gone),
            None,
            "edge {gone} survived its endpoint"
        );
    }
    assert_eq!(storage.get_edge_by_id("keep"), Some(keep));
    assert_eq!(storage.out_edges(x).count(), 1);
    assert_eq!(storage.in_edges(x).count(), 0);
    assert_eq!(storage.in_edges(y).count(), 1);
    assert_eq!(statistics_of(storage).edge_label_counts().len(), 1);
    assert_eq!(statistics_of(storage).vertex_label_count("h"), 0);
    assert_consistent(storage);
}

/// Dropping an edge updates the adjacency of both ends, the counts and the label index, and leaves
/// parallel edges alone.
pub fn drop_edge_updates_adjacency_and_counts<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, None, &[], PropertyMap::default());
    let b = vertex(storage, None, &[], PropertyMap::default());
    let e1 = edge(storage, Some("e1"), Some("l"), a, b, PropertyMap::default());
    let e2 = edge(storage, Some("e2"), Some("l"), a, b, PropertyMap::default());
    storage.drop_edge(e1).expect("drop_edge");
    assert_eq!(storage.edge_count(), 1);
    assert_eq!(storage.out_edges(a).collect::<Vec<_>>(), vec![e2]);
    assert_eq!(storage.in_edges(b).collect::<Vec<_>>(), vec![e2]);
    assert_eq!(statistics_of(storage).edge_label_count("l"), 1);
    assert_consistent(storage);
    storage.drop_edge(e2).expect("drop_edge");
    assert!(statistics_of(storage).edge_label_counts().is_empty());
    assert_eq!(
        storage.vertex_count(),
        2,
        "dropping edges must keep the vertices"
    );
    assert_consistent(storage);
}

/// A storage may reuse the slot of a dropped element for the next one added. The dropped id must
/// never resolve to the new element (silent wrong results), for vertices, edges and cascaded edges.
pub fn slot_reuse_does_not_resurrect_dropped_ids<S: GraphStorage>(storage: &mut S) {
    let a = vertex(storage, Some("a"), &["n"], PropertyMap::default());
    let b = vertex(storage, Some("b"), &["n"], PropertyMap::default());
    let c = vertex(storage, Some("c"), &["n"], PropertyMap::default());
    edge(storage, Some("e1"), Some("l"), a, b, PropertyMap::default());
    edge(storage, Some("e2"), Some("l"), a, c, PropertyMap::default());
    storage.drop_vertex(b).expect("drop_vertex");
    let d = vertex(storage, Some("d"), &["n"], PropertyMap::default());
    let fresh = edge(
        storage,
        Some("fresh"),
        Some("m"),
        c,
        d,
        PropertyMap::default(),
    );
    assert_eq!(
        storage.get_vertex_by_id("b"),
        None,
        "the dropped vertex id resolves"
    );
    assert_eq!(
        storage.get_edge_by_id("e1"),
        None,
        "the cascaded edge id resolves to another edge"
    );
    assert_eq!(storage.get_edge_by_id("fresh"), Some(fresh));
    assert_eq!(
        storage.edges_by_label("l").count(),
        1,
        "only e2 still has the label l"
    );
    assert_consistent(storage);
    let e2 = storage.get_edge_by_id("e2").expect("e2 survives");
    storage.drop_edge(e2).expect("drop_edge");
    edge(
        storage,
        Some("again"),
        Some("l"),
        c,
        d,
        PropertyMap::default(),
    );
    assert_eq!(
        storage.get_edge_by_id("e2"),
        None,
        "the dropped edge id resolves to the new edge"
    );
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// Traversal smoke layer
// ---------------------------------------------------------------------------------------------

/// The modern graph, loaded through the trait only, answers a handful of traversals exactly like a
/// `TraversalGraph` (see [`crate::smoke`]).
pub fn traversal_smoke_matches_the_reference<S: GraphStorage>(storage: &mut S) {
    crate::smoke::assert_matches_reference(storage);
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// Schema group (conditional on schema support)
// ---------------------------------------------------------------------------------------------

/// Whether a `set_schema` result says the storage has no schema support: the trait default,
/// `GraphError::Unsupported`. The schema items then return early (the item passes without
/// checking anything), so a storage without schemas passes [`conformance!`](crate::conformance).
/// Schema enforcement is not reusable outside `graphersal` yet, so a foreign storage usually
/// keeps that default.
fn schema_unsupported(result: &Result<(), GraphError>) -> bool {
    matches!(result, Err(GraphError::Unsupported { .. }))
}

fn person_schema(mode: SchemaMode) -> GraphSchema {
    GraphSchema::new().with_mode(mode).with_vertex(
        "Person",
        PropertySchema::object().with_required_property("name", PropertySchema::string()),
    )
}

/// `set_schema` in an enforcing mode over existing data that violates it fails with a
/// `GraphError::Schema`, and leaves both the graph and the previously stored schema unchanged.
/// Skipped (passes) when `set_schema` is `GraphError::Unsupported`.
pub fn schema_application_failure_leaves_graph_and_schema_unchanged<S: GraphStorage>(
    storage: &mut S,
) {
    vertex(storage, Some("p"), &["Person"], PropertyMap::default());
    assert!(storage.get_schema().unwrap().is_none());
    let result = storage.set_schema(person_schema(SchemaMode::Closed));
    if schema_unsupported(&result) {
        assert!(
            storage.get_schema().unwrap().is_none(),
            "a storage without schema support stores no schema"
        );
        return;
    }
    let err = result.expect_err("the vertex lacks a required property");
    assert!(matches!(err, GraphError::Schema(_)), "got {err:?}");
    assert!(
        storage.get_schema().unwrap().is_none(),
        "a failed set_schema stored the schema"
    );
    assert_eq!(storage.vertex_count(), 1);
    vertex(storage, Some("q"), &["Person"], PropertyMap::default());
    assert_eq!(
        storage.vertex_count(),
        2,
        "the rejected schema must not be enforced afterwards"
    );
    assert_consistent(storage);
}

/// With a stored `Closed` schema: declared properties are required and typed, undeclared labels
/// are rejected, a required property cannot be removed; mode `None` lifts all of it again.
/// Skipped (passes) when `set_schema` is `GraphError::Unsupported`.
pub fn schema_modes_enforce_declared_properties<S: GraphStorage>(storage: &mut S) {
    let result = storage.set_schema(person_schema(SchemaMode::Closed));
    if schema_unsupported(&result) {
        assert!(
            storage.get_schema().unwrap().is_none(),
            "a storage without schema support stores no schema"
        );
        return;
    }
    result.expect("an empty graph satisfies any schema");
    assert!(storage.get_schema().unwrap().is_some());
    let ok = vertex(storage, None, &["Person"], props(&[("name", "ann".into())]));
    assert!(
        matches!(
            storage.insert_vertex(None, label_vec(&["Person"]), PropertyMap::default()),
            Err(GraphError::Schema(_))
        ),
        "missing required property"
    );
    assert!(
        matches!(
            storage.insert_vertex(None, label_vec(&["Person"]), props(&[("name", 5.into())])),
            Err(GraphError::Schema(_))
        ),
        "wrong property type"
    );
    assert!(
        matches!(
            storage.insert_vertex(None, label_vec(&["Undeclared"]), PropertyMap::default()),
            Err(GraphError::Schema(_))
        ),
        "undeclared label in Closed mode"
    );
    assert!(
        matches!(
            storage.set_vertex_property(ok, "name".into(), 5.into()),
            Err(GraphError::Schema(_))
        ),
        "set with the wrong type"
    );
    assert!(
        matches!(
            storage.drop_vertex_property("name", ok),
            Err(GraphError::Schema(_))
        ),
        "removing a required property"
    );
    assert_eq!(
        storage.vertex_count(),
        1,
        "rejected writes must leave nothing behind"
    );
    assert_eq!(
        storage.vertex_properties(ok).and_then(|p| p.get("name")),
        Some(&"ann".into())
    );
    assert_consistent(storage);

    storage
        .set_schema(GraphSchema::new().with_mode(SchemaMode::None))
        .expect("mode None applies without scanning");
    vertex(storage, None, &["Undeclared"], PropertyMap::default());
    assert_eq!(storage.vertex_count(), 2);
    assert_consistent(storage);
}

/// With a stored `Open` schema declaring literal `default`s: `add_vertex`/`add_edge` (the
/// create path) fill a missing property with its default and keep an explicit value, while the
/// primitive `insert_vertex`/`insert_edge` store exactly what they are given (a required
/// property with a default is still required there). Skipped (passes) when `set_schema` is
/// `GraphError::Unsupported`.
pub fn add_applies_schema_defaults_insert_does_not<S: GraphStorage>(storage: &mut S) {
    let schema = GraphSchema::from_json(
        r#"{"mode": "open",
            "vertices": {"Person": {"schema": {"type": "object",
                "properties": {"level": {"type": "integer", "default": 1}},
                "required": ["level"]}}},
            "edges": {"knows": {"connections": [{"from": "Person", "to": "Person"}],
                "schema": {"type": "object",
                    "properties": {"weight": {"type": "number", "default": 0.5}}}}}}"#,
    )
    .expect("a valid schema");
    let result = storage.set_schema(schema);
    if schema_unsupported(&result) {
        return;
    }
    result.expect("an empty graph satisfies any schema");
    let level = |storage: &S, v: S::VertexId| {
        storage
            .vertex_properties(v)
            .and_then(|p| p.get("level").cloned())
    };

    let added = storage
        .add_vertex(None, label_vec(&["Person"]), PropertyMap::default())
        .expect("add_vertex fills the required property from its default");
    assert_eq!(
        level(storage, added),
        Some(1.into()),
        "the default is stored"
    );
    let explicit = storage
        .add_vertex(None, label_vec(&["Person"]), props(&[("level", 5.into())]))
        .expect("add_vertex with an explicit value");
    assert_eq!(
        level(storage, explicit),
        Some(5.into()),
        "an explicit value wins"
    );

    assert!(
        matches!(
            storage.insert_vertex(None, label_vec(&["Person"]), PropertyMap::default()),
            Err(GraphError::Schema(_))
        ),
        "insert_vertex applies no default, so the required property is missing"
    );
    let inserted = storage
        .insert_vertex(None, label_vec(&["Person"]), props(&[("level", 7.into())]))
        .expect("insert_vertex with the required property");
    assert_eq!(
        storage.vertex_properties(inserted).map(|p| p.len()),
        Some(1),
        "insert_vertex stores exactly the given properties"
    );

    let edge = storage
        .add_edge(
            None,
            Some("knows".into()),
            added,
            explicit,
            PropertyMap::default(),
        )
        .expect("add_edge");
    assert_eq!(
        storage
            .edge_properties(edge)
            .and_then(|p| p.get("weight").cloned()),
        Some(0.5.into()),
        "add_edge stores the edge default"
    );
    let raw_edge = storage
        .insert_edge(
            None,
            Some("knows".into()),
            added,
            inserted,
            PropertyMap::default(),
        )
        .expect("insert_edge");
    assert_eq!(
        storage.edge_properties(raw_edge).map(|p| p.len()),
        Some(0),
        "insert_edge stores exactly the given properties"
    );
    assert_eq!(
        storage.vertex_count(),
        3,
        "the rejected insert left nothing"
    );
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// Read-only storages
// ---------------------------------------------------------------------------------------------

/// A read-only storage (the blanket `&G` view of any storage, e.g. `&TraversalGraph`) answers
/// every read and rejects every write with an error (the trait leaves the error kind open; the
/// `&G` view uses `InvalidElementState`).
/// Unlike the other items it needs a populated storage and a vertex and an edge handle from it.
pub fn read_only_rejects_writes<S: GraphStorage>(
    storage: &mut S,
    vertex: S::VertexId,
    edge: S::EdgeId,
) {
    let (vertices, edges) = (storage.vertex_count(), storage.edge_count());
    assert!(
        storage
            .insert_vertex(None, vec![], PropertyMap::default())
            .is_err()
    );
    assert!(storage.add_vertex_label(vertex, "x".into()).is_err());
    assert!(storage.remove_vertex_label(vertex, "x").is_err());
    assert!(
        storage
            .insert_edge(None, None, vertex, vertex, PropertyMap::default())
            .is_err()
    );
    assert!(
        storage
            .set_vertex_property(vertex, "k".into(), 1.into())
            .is_err()
    );
    assert!(
        storage
            .set_edge_property(edge, "k".into(), 1.into())
            .is_err()
    );
    assert!(storage.drop_vertex(vertex).is_err());
    assert!(storage.drop_edge(edge).is_err());
    assert!(storage.drop_vertex_property("k", vertex).is_err());
    assert!(storage.drop_edge_property("k", edge).is_err());
    let definitions = storage.definitions().unwrap().count();
    assert!(storage.set_definition(sample_query("q")).is_err());
    assert!(
        storage
            .remove_definition(DefinitionKind::QUERY, "q")
            .is_err()
    );
    assert_eq!(
        storage.definitions().unwrap().count(),
        definitions,
        "a rejected catalog write changed the catalog"
    );
    assert_eq!(
        (storage.vertex_count(), storage.edge_count()),
        (vertices, edges),
        "a rejected write changed the storage"
    );
    assert_consistent(storage);
}

// ---------------------------------------------------------------------------------------------
// The catalog of definitions (a backend without one keeps the defaults: nothing stored, writes
// unsupported; the items then check exactly that)
// ---------------------------------------------------------------------------------------------

/// A valid saved query named `name`.
pub fn sample_query(name: &str) -> Definition {
    Definition::query(
        name,
        QueryDefinition::new(format!("fn {name}(limit) {{ g.v().limit(limit) }}"))
            .with_folder("contract")
            .with_param(
                "limit",
                QueryParam::new(PropertySchema::integer()).with_default(10),
            ),
    )
}

/// Definitions are stored (insert or replace), listed by kind and then name, read back equal,
/// validated on write (a refused one changes nothing) and removed; an opaque definition of an
/// unknown kind is kept byte for byte. A storage without a catalog reports
/// `GraphError::Unsupported` for writes and lists nothing.
pub fn definitions_are_stored_validated_and_removed<S: GraphStorage>(storage: &mut S) {
    match storage.set_definition(sample_query("b")) {
        Err(GraphError::Unsupported { .. }) => {
            assert_eq!(storage.definitions().unwrap().count(), 0);
            assert!(
                storage
                    .definition(DefinitionKind::QUERY, "b")
                    .unwrap()
                    .is_none()
            );
            assert!(matches!(
                storage.remove_definition(DefinitionKind::QUERY, "b"),
                Err(GraphError::Unsupported { .. })
            ));
            return;
        }
        other => other.expect("a valid definition is stored"),
    }
    storage
        .set_definition(sample_query("a"))
        .expect("a valid definition is stored");
    let opaque = Definition::opaque(
        DefinitionKind::from_code(200),
        DefinitionFlags::NONE.with_critical(true),
        "future",
        vec![0, 1, 2, 255],
    );
    storage.set_definition(opaque.clone()).expect("opaque");
    let listed: Vec<(u8, String)> = storage
        .definitions()
        .unwrap()
        .map(|d| (d.kind().code(), d.name().to_string()))
        .collect();
    assert_eq!(
        listed,
        [
            (1, "a".to_string()),
            (1, "b".to_string()),
            (200, "future".to_string())
        ],
        "definitions are listed by kind, then by name"
    );
    assert_eq!(
        storage
            .definition(DefinitionKind::from_code(200), "future")
            .unwrap(),
        Some(&opaque)
    );
    // Replace.
    let replaced = Definition::query("a", QueryDefinition::new("fn a() { 1 }"));
    storage.set_definition(replaced.clone()).expect("replace");
    assert_eq!(
        storage.definition(DefinitionKind::QUERY, "a").unwrap(),
        Some(&replaced)
    );
    assert_eq!(storage.definitions().unwrap().count(), 3);
    // Refused: an invalid name, an invalid folder; nothing changes.
    assert!(matches!(
        storage.set_definition(Definition::query(
            "not valid",
            QueryDefinition::new("fn x() {}")
        )),
        Err(GraphError::Definition(_))
    ));
    assert!(matches!(
        storage.set_definition(Definition::query(
            "a",
            QueryDefinition::new("fn a() { 2 }").with_folder("/root")
        )),
        Err(GraphError::Definition(_))
    ));
    assert_eq!(
        storage.definition(DefinitionKind::QUERY, "a").unwrap(),
        Some(&replaced)
    );
    assert_eq!(storage.definitions().unwrap().count(), 3);
    // Remove.
    assert!(
        storage
            .remove_definition(DefinitionKind::QUERY, "a")
            .expect("remove")
    );
    assert!(
        !storage
            .remove_definition(DefinitionKind::QUERY, "a")
            .expect("remove")
    );
    assert!(
        storage
            .definition(DefinitionKind::QUERY, "a")
            .unwrap()
            .is_none()
    );
    assert_eq!(storage.definitions().unwrap().count(), 2);
    // Names are per kind.
    assert!(
        !storage
            .remove_definition(DefinitionKind::from_code(200), "b")
            .expect("remove")
    );
}

/// A commit hook that counts every event it receives (for "no hook is called" checks).
#[derive(Default)]
pub struct EventCounter {
    events: Arc<AtomicUsize>,
}

impl EventCounter {
    /// A counter and a handle to read its count after the hook moved into the storage.
    pub fn new() -> (Self, Arc<AtomicUsize>) {
        let counter = Self::default();
        let events = counter.events.clone();
        (counter, events)
    }

    fn count(&self) {
        self.events.fetch_add(1, Ordering::SeqCst);
    }
}

impl CommitHook for EventCounter {
    fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
        self.count();
        Ok(())
    }

    fn after_commit(&mut self, _: &ChangeSet) {
        self.count();
    }

    fn after_rollback(&mut self, _: &RollbackReason) {
        self.count();
    }
}

/// A mutation outside any unit (a builder, a loader, a generic replay) records nothing: no
/// commit hook is called and [`GraphStorage::last_commit_seq`] does not move. A storage without
/// `change_capture` refuses the hook and is checked for the commit position only.
pub fn mutation_outside_unit_records_nothing<S: GraphStorage>(storage: &mut S) {
    let (hook, events) = EventCounter::new();
    let _ = storage.register_commit_hook(Box::new(hook));
    let seq = storage.last_commit_seq().unwrap();
    let a = vertex(storage, Some("a"), &["person"], props(&[("n", 1.into())]));
    let b = vertex(storage, None, &[], props(&[]));
    let e = edge(storage, Some("ab"), Some("knows"), a, b, props(&[]));
    storage
        .set_vertex_property(a, "n".into(), 2.into())
        .expect("set");
    storage
        .set_edge_property(e, "w".into(), 1.into())
        .expect("set edge prop");
    storage.drop_vertex_property("n", a).expect("drop prop");
    storage.drop_edge(e).expect("drop edge");
    storage.drop_vertex(b).expect("drop vertex");
    assert_eq!(
        events.load(Ordering::SeqCst),
        0,
        "a mutation outside a unit calls no hook"
    );
    assert_eq!(
        storage.last_commit_seq().unwrap(),
        seq,
        "a mutation outside a unit is not a commit"
    );
    assert_eq!(storage.vertex_count(), 1);
    assert_consistent(storage);
}
