//! Graph state snapshots and the side-effect diff (design T8), the way the TCK defines side
//! effects: a comparison of two states, not counters kept by the engine.
//!
//! - nodes and relationships: the element ids present (`GraphStorage::vertex_id` / `edge_id`),
//!   not the storage handles: a failing query rolls back, and the undo log may re-insert a
//!   removed element under a new handle with its old id and content, which is no side effect;
//!   a deleted and a new element have different ids, so they are told apart;
//! - labels: the distinct NODE label names present in the graph (relationship types are not
//!   labels), so `-labels 1` means a name disappeared from the graph;
//! - properties: (element id, key, value) triples on nodes and relationships; a changed value
//!   counts as one removed and one added triple, and the properties of a deleted element count
//!   as removed.
//!
//! Values compare with `==`, except that two values both unequal to themselves (`NaN`) count as
//! the same value.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use graphersal::storage::GraphStorage;
use graphersal::{CompactString, ElementProperty};

use super::gherkin::SideEffect;

/// One element of a snapshot: its kind and its id (vertex and edge ids are separate
/// namespaces).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Element {
    Node(CompactString),
    Relationship(CompactString),
}

/// A property of an element: (element, key).
type PropertyKey = (Element, CompactString);

/// The state of a graph at one moment.
pub struct Snapshot {
    nodes: HashSet<CompactString>,
    relationships: HashSet<CompactString>,
    labels: BTreeSet<CompactString>,
    properties: HashMap<PropertyKey, ElementProperty>,
}

/// The id of an element, or (for a handle the storage gives no id, which the storage contract
/// rules out) its handle's debug text, so the element still counts.
fn id_or_handle(id: Option<&str>, handle: &dyn std::fmt::Debug) -> CompactString {
    match id {
        Some(id) => CompactString::from(id),
        None => CompactString::from(format!("#handle {handle:?}")),
    }
}

/// Takes a snapshot of `graph`.
pub fn snapshot<S: GraphStorage>(graph: &S) -> Snapshot {
    let mut snap = Snapshot {
        nodes: HashSet::new(),
        relationships: HashSet::new(),
        labels: BTreeSet::new(),
        properties: HashMap::new(),
    };
    for v in graph.vertices() {
        let id = id_or_handle(graph.vertex_id(v), &v);
        snap.labels.extend(graph.vertex_labels(v).iter().cloned());
        if let Some(map) = graph.vertex_properties(v) {
            for (key, value) in map.iter() {
                snap.properties
                    .insert((Element::Node(id.clone()), key.clone()), value.clone());
            }
        }
        snap.nodes.insert(id);
    }
    for e in graph.edges() {
        let id = id_or_handle(graph.edge_id(e), &e);
        if let Some(map) = graph.edge_properties(e) {
            for (key, value) in map.iter() {
                snap.properties.insert(
                    (Element::Relationship(id.clone()), key.clone()),
                    value.clone(),
                );
            }
        }
        snap.relationships.insert(id);
    }
    snap
}

/// The side effects between two states: every key of [`SideEffect::ALL`] with its count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SideEffects(pub BTreeMap<SideEffect, u64>);

impl SideEffects {
    /// The count of `key` (0 when absent).
    pub fn get(&self, key: SideEffect) -> u64 {
        self.0.get(&key).copied().unwrap_or(0)
    }

    /// Whether every count is zero.
    pub fn is_empty(&self) -> bool {
        self.0.values().all(|n| *n == 0)
    }

    /// The expected side effects of a step table (keys not listed are zero).
    pub fn expected(table: &[(SideEffect, u64)]) -> Self {
        let mut map = BTreeMap::new();
        for (key, n) in table {
            *map.entry(*key).or_insert(0) += n;
        }
        Self(map)
    }

    /// The keys whose counts differ from `expected`, as `+nodes: expected 1, got 0` items.
    pub fn differences(&self, expected: &SideEffects) -> Vec<String> {
        SideEffect::ALL
            .iter()
            .filter(|(key, _)| self.get(*key) != expected.get(*key))
            .map(|(key, name)| {
                format!(
                    "{name}: expected {}, got {}",
                    expected.get(*key),
                    self.get(*key)
                )
            })
            .collect()
    }

    /// `+nodes 1, -labels 2` (non-zero counts only), or `none`.
    pub fn render(&self) -> String {
        let items: Vec<String> = SideEffect::ALL
            .iter()
            .filter(|(key, _)| self.get(*key) != 0)
            .map(|(key, name)| format!("{name} {}", self.get(*key)))
            .collect();
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(", ")
        }
    }
}

fn same_value(a: &ElementProperty, b: &ElementProperty) -> bool {
    #[allow(clippy::eq_op)]
    let self_unequal = |v: &ElementProperty| v != v;
    a == b || (self_unequal(a) && self_unequal(b))
}

/// The side effects that turned `before` into `after`.
pub fn diff(before: &Snapshot, after: &Snapshot) -> SideEffects {
    let count = |n: usize| n as u64;
    let mut map = BTreeMap::new();
    map.insert(
        SideEffect::AddedNodes,
        count(after.nodes.difference(&before.nodes).count()),
    );
    map.insert(
        SideEffect::RemovedNodes,
        count(before.nodes.difference(&after.nodes).count()),
    );
    map.insert(
        SideEffect::AddedRelationships,
        count(
            after
                .relationships
                .difference(&before.relationships)
                .count(),
        ),
    );
    map.insert(
        SideEffect::RemovedRelationships,
        count(
            before
                .relationships
                .difference(&after.relationships)
                .count(),
        ),
    );
    map.insert(
        SideEffect::AddedLabels,
        count(after.labels.difference(&before.labels).count()),
    );
    map.insert(
        SideEffect::RemovedLabels,
        count(before.labels.difference(&after.labels).count()),
    );
    let added = after
        .properties
        .iter()
        .filter(|(k, v)| before.properties.get(*k).is_none_or(|b| !same_value(b, v)))
        .count();
    let removed = before
        .properties
        .iter()
        .filter(|(k, v)| after.properties.get(*k).is_none_or(|a| !same_value(a, v)))
        .count();
    map.insert(SideEffect::AddedProperties, count(added));
    map.insert(SideEffect::RemovedProperties, count(removed));
    SideEffects(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal::GraphTraversalSource;
    use graphersal::TraversalGraph;

    fn run(graph: &mut TraversalGraph, query: &str) {
        GraphTraversalSource::new(&mut *graph)
            .gql(query)
            .iterate()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
    }

    #[test]
    fn a_create_adds_nodes_relationships_labels_and_properties() {
        let mut graph = TraversalGraph::default();
        run(&mut graph, "CREATE (:A {x: 1})");
        let before = snapshot(&graph);
        run(&mut graph, "CREATE (:A {x: 1})-[:T {w: 2}]->(:B:C)");
        let after = snapshot(&graph);
        let effects = diff(&before, &after);
        assert_eq!(
            effects.render(),
            "+nodes 2, +relationships 1, +labels 2, +properties 2"
        );
        assert!(diff(&after, &after).is_empty());
        let reverse = diff(&after, &before);
        assert_eq!(
            reverse.render(),
            "-nodes 2, -relationships 1, -labels 2, -properties 2"
        );
    }

    #[test]
    fn a_rolled_back_delete_has_no_side_effects() {
        let mut graph = TraversalGraph::default();
        run(
            &mut graph,
            "CREATE (:A {num: 0})-[:T {w: 1}]->(:B {num: 1})",
        );
        let handles_before: Vec<_> = graph.vertices().collect();
        let before = snapshot(&graph);
        // Fails after the delete (W28: a returned property of a deleted node), so the unit
        // rolls back and the undo log re-inserts the elements, possibly under new handles.
        let failed = GraphTraversalSource::new(&mut graph)
            .gql("MATCH (n:A) DETACH DELETE n RETURN n.num")
            .iterate();
        assert!(failed.is_err(), "the query must fail and roll back");
        let handles_after: Vec<_> = graph.vertices().collect();
        let after = snapshot(&graph);
        assert!(
            diff(&before, &after).is_empty(),
            "{} (handles {handles_before:?} -> {handles_after:?})",
            diff(&before, &after).render()
        );
    }

    #[test]
    fn a_delete_and_a_new_element_with_the_same_content_are_told_apart() {
        let mut graph = TraversalGraph::default();
        run(&mut graph, "CREATE (:A {num: 0})-[:T]->(:B)");
        let before = snapshot(&graph);
        run(
            &mut graph,
            "MATCH (n:A) DETACH DELETE n CREATE (:A {num: 0})-[:T]->(:B)",
        );
        let after = snapshot(&graph);
        assert_eq!(
            diff(&before, &after).render(),
            "+nodes 2, -nodes 1, +relationships 1, -relationships 1, +properties 1, -properties 1"
        );
    }

    #[test]
    fn expected_side_effects_default_to_zero() {
        let expected = SideEffects::expected(&[(SideEffect::AddedNodes, 1)]);
        let got =
            SideEffects::expected(&[(SideEffect::AddedNodes, 1), (SideEffect::AddedLabels, 1)]);
        assert_eq!(
            got.differences(&expected),
            vec!["+labels: expected 0, got 1"]
        );
        assert!(expected.differences(&expected).is_empty());
    }
}
