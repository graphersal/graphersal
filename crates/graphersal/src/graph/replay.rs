//! Applying a [`ChangeSet`] to a storage (`book/src/users_guide/transactions.md`): the shared
//! body of [`GraphStorage::apply_change_set`] (a live, captured unit) and
//! [`TraversalGraph::replay`](crate::graph::TraversalGraph::replay) (an unpublished graph,
//! outside units).
//!
//! Every mutation is applied through the storage's own mutating methods, so the schema is
//! enforced and an explicit numeric id raises the auto-id sequence as for any other write. Ids
//! are preserved. Only the after-values are used: the before-values of a mutation are not
//! compared with the target (a journal is replayed onto the state it was recorded against).

use compact_str::CompactString;

use crate::error::{DefinitionError, GraphError};
use crate::graph::change::{ChangeSet, ElementRef, Mutation};
use crate::graph::schema::ElementKind;
use crate::graph::storage::GraphStorage;
use crate::traversal::memory_budget::UnitBudget;
use crate::value::{ElementProperty, PropertyMap};

/// Applies every mutation of `changes` in order; stops at the first failure (the caller rolls
/// back or drops the graph). With a memory `budget` (a live apply under a configured
/// `memory.limit`), every [`UNIT_CHECK_INTERVAL`](crate::traversal::memory_budget::UNIT_CHECK_INTERVAL)-th
/// mutation checks it.
///
/// `copy` makes the map an added element is inserted with from the mutation's map:
/// [`clone_map`] for any storage, [`TraversalGraph::stored_copy`](crate::graph::TraversalGraph)
/// for a `TraversalGraph` (a `Shaped` map of its own table, no `IndexMap` round trip).
pub(crate) fn apply_mutations<S: GraphStorage + ?Sized>(
    storage: &mut S,
    changes: &ChangeSet,
    budget: &mut Option<UnitBudget>,
    copy: &mut impl FnMut(&mut S, &PropertyMap) -> PropertyMap,
) -> Result<(), GraphError> {
    for mutation in changes.mutations() {
        apply_mutation_with(storage, mutation, copy)?;
        crate::traversal::memory_budget::tick(budget, &*storage)?;
    }
    Ok(())
}

/// The map an added element is inserted with on any storage: a clone of the mutation's.
#[inline]
pub(crate) fn clone_map<S: ?Sized>(_: &mut S, map: &PropertyMap) -> PropertyMap {
    map.clone()
}

fn vertex_of<S: GraphStorage + ?Sized>(storage: &S, id: &str) -> Result<S::VertexId, GraphError> {
    storage
        .get_vertex_by_id(id)
        .ok_or_else(|| GraphError::VertexNotFound { id: id.to_string() })
}

fn edge_of<S: GraphStorage + ?Sized>(storage: &S, id: &str) -> Result<S::EdgeId, GraphError> {
    storage
        .get_edge_by_id(id)
        .ok_or_else(|| GraphError::EdgeNotFound { id: id.to_string() })
}

pub(crate) fn apply_mutation<S: GraphStorage + ?Sized>(
    storage: &mut S,
    mutation: &Mutation,
) -> Result<(), GraphError> {
    apply_mutation_with(storage, mutation, &mut clone_map)
}

/// [`apply_mutation`] whose added elements are inserted with `copy`'s map (see
/// [`apply_mutations`]).
pub(crate) fn apply_mutation_with<S: GraphStorage + ?Sized>(
    storage: &mut S,
    mutation: &Mutation,
    copy: &mut impl FnMut(&mut S, &PropertyMap) -> PropertyMap,
) -> Result<(), GraphError> {
    match mutation {
        Mutation::AddVertex {
            id,
            labels,
            properties,
        } => {
            let labels = labels.iter().map(CompactString::from).collect();
            let properties = copy(storage, properties);
            storage.insert_vertex(Some(id.into()), labels, properties)?;
        }
        Mutation::DropVertex { id, .. } => {
            let vertex = vertex_of(storage, id)?;
            storage.drop_vertex(vertex)?;
        }
        Mutation::AddEdge {
            id,
            label,
            out_id,
            in_id,
            properties,
        } => {
            let from = vertex_of(storage, out_id)?;
            let to = vertex_of(storage, in_id)?;
            let properties = copy(storage, properties);
            storage.insert_edge(
                Some(id.into()),
                label.as_deref().map(CompactString::from),
                from,
                to,
                properties,
            )?;
        }
        Mutation::DropEdge { id, .. } => {
            let edge = edge_of(storage, id)?;
            storage.drop_edge(edge)?;
        }
        Mutation::SetProperty {
            element,
            key,
            after,
            ..
        } => {
            let ElementRef { kind, id } = element;
            match kind {
                ElementKind::Vertex => {
                    let vertex = vertex_of(storage, id)?;
                    storage.set_vertex_property(vertex, key.into(), after.clone())?;
                }
                ElementKind::Edge => {
                    let edge = edge_of(storage, id)?;
                    storage.set_edge_property(edge, key.into(), after.clone())?;
                }
            }
        }
        Mutation::RemoveProperty { element, key, .. } => {
            let ElementRef { kind, id } = element;
            match kind {
                ElementKind::Vertex => {
                    let vertex = vertex_of(storage, id)?;
                    storage.drop_vertex_property(key, vertex)?;
                }
                ElementKind::Edge => {
                    let edge = edge_of(storage, id)?;
                    storage.drop_edge_property(key, edge)?;
                }
            }
        }
        Mutation::AddLabel { id, label } => {
            let vertex = vertex_of(storage, id)?;
            storage.add_vertex_label(vertex, label.into())?;
        }
        Mutation::RemoveLabel { id, label } => {
            let vertex = vertex_of(storage, id)?;
            storage.remove_vertex_label(vertex, label)?;
        }
        Mutation::SetEdgeLabel { id, after, .. } => {
            let edge = edge_of(storage, id)?;
            storage.set_edge_label(edge, after.into())?;
        }
        Mutation::SetSchema { after, .. } => {
            storage.set_schema((**after).clone())?;
        }
        Mutation::SetDefinition {
            kind, name, after, ..
        } => match after {
            Some(definition) => storage.set_definition((**definition).clone())?,
            None => {
                if !storage.remove_definition(*kind, name)? {
                    return Err(DefinitionError::NotFound {
                        kind: kind.to_string(),
                        name: name.clone(),
                    }
                    .into());
                }
            }
        },
    }
    Ok(())
}

impl crate::graph::TraversalGraph {
    /// The map a change set's added element is inserted with: a `Shaped` copy in this graph's
    /// own shape table ([`ShapeTable::shaped_copy`](crate::graph::shape::ShapeTable::shaped_copy)),
    /// so the insert adopts it with one comparison.
    pub(crate) fn stored_copy(&mut self, map: &PropertyMap) -> PropertyMap {
        self.shapes.shaped_copy(map)
    }

    /// The body of [`Self::replay`] on a graph in place: every mutation outside units, then the
    /// position and the id sequences raised to the change set's.
    pub(crate) fn replay_in_place(&mut self, changes: &ChangeSet) -> Result<(), GraphError> {
        self.load_outside_units(|graph| {
            apply_mutations(graph, changes, &mut None, &mut Self::stored_copy)
        })?;
        self.raise_replayed_position(changes);
        Ok(())
    }

    /// Raises the commit sequence number, the commit time and both auto-id sequences to the
    /// change set's (never lowers them).
    fn raise_replayed_position(&mut self, changes: &ChangeSet) {
        self.undo.last_commit_seq = self.undo.last_commit_seq.max(changes.commit_seq());
        self.undo.last_committed_at = self.undo.last_committed_at.max(changes.committed_at());
        self.node_sequence = self.node_sequence.max(changes.node_sequence());
        self.edge_sequence = self.edge_sequence.max(changes.edge_sequence());
    }
}

/// One mutation a lenient replay could not apply as recorded (a store repair, format spec 13).
#[cfg_attr(
    any(not(feature = "persist"), target_arch = "wasm32"),
    allow(dead_code)
)]
#[derive(Debug, Clone)]
pub(crate) struct ReplayIssue {
    /// The element the mutation names (`None`: a schema or catalog change).
    pub(crate) element: Option<ElementRef>,
    /// What did not match or failed.
    pub(crate) reason: String,
}

#[cfg_attr(
    any(not(feature = "persist"), target_arch = "wasm32"),
    allow(dead_code)
)]
impl crate::graph::TraversalGraph {
    /// Applies the mutations of `changes` that `keep` selects, one by one and past failures
    /// (outside units, no hooks): a store repair replays the WAL onto a state that may lack
    /// elements. With `check`, every mutation's before image is compared with the current
    /// state first; a mismatch or a failure is returned as an issue (the element DIVERGED). A
    /// schema change that fails validation is stored anyway (it was valid when committed).
    /// Position, time and sequences advance as in [`Self::replay`].
    pub(crate) fn replay_lenient(
        &mut self,
        changes: &ChangeSet,
        keep: impl Fn(&Mutation) -> bool,
        check: bool,
    ) -> Vec<ReplayIssue> {
        let mut issues = Vec::new();
        self.load_outside_units(|graph| {
            for mutation in changes.mutations().iter().filter(|m| keep(m)) {
                let element = mutation_element(mutation);
                if check && let Some(reason) = before_mismatch(&*graph, mutation, false) {
                    issues.push(ReplayIssue {
                        element: element.clone(),
                        reason,
                    });
                }
                if let Err(err) = apply_mutation_with(graph, mutation, &mut Self::stored_copy) {
                    match mutation {
                        Mutation::SetSchema { after, .. } => {
                            *graph.schema.write() = (**after).clone();
                        }
                        // A definition that was valid when committed is stored anyway.
                        Mutation::SetDefinition {
                            after: Some(after), ..
                        } => {
                            let old = graph.definitions.insert((**after).clone());
                            graph.catalog_after_change(after.kind(), old.as_ref(), after.name());
                        }
                        _ => {}
                    }
                    issues.push(ReplayIssue {
                        element,
                        reason: err.to_string(),
                    });
                }
            }
        });
        self.raise_replayed_position(changes);
        issues
    }

    /// [`Self::replay`] that checks every mutation's before image against the current state
    /// before applying it (a merge checkpoint, format spec 12.3: the WAL must continue exactly
    /// the snapshot it is merged onto). Fails with the element and the reason at the first
    /// mismatch or failure; the graph is then to be dropped.
    pub(crate) fn replay_verified(&mut self, changes: &ChangeSet) -> Result<(), String> {
        let result = self.load_outside_units(|graph| {
            for mutation in changes.mutations() {
                let subject = || mutation_subject(mutation);
                if let Some(reason) = before_mismatch(&*graph, mutation, true) {
                    return Err(format!("{}: {reason}", subject()));
                }
                apply_mutation_with(graph, mutation, &mut Self::stored_copy)
                    .map_err(|err| format!("{}: {err}", subject()))?;
            }
            Ok(())
        });
        result?;
        self.raise_replayed_position(changes);
        Ok(())
    }
}

/// The element a mutation names (`None`: a schema or catalog change).
#[cfg_attr(
    any(not(feature = "persist"), target_arch = "wasm32"),
    allow(dead_code)
)]
pub(crate) fn mutation_element(mutation: &Mutation) -> Option<ElementRef> {
    let element = |kind: ElementKind, id: &String| {
        Some(ElementRef {
            kind,
            id: id.clone(),
        })
    };
    match mutation {
        Mutation::AddVertex { id, .. }
        | Mutation::DropVertex { id, .. }
        | Mutation::AddLabel { id, .. }
        | Mutation::RemoveLabel { id, .. } => element(ElementKind::Vertex, id),
        Mutation::AddEdge { id, .. }
        | Mutation::DropEdge { id, .. }
        | Mutation::SetEdgeLabel { id, .. } => element(ElementKind::Edge, id),
        Mutation::SetProperty { element, .. } | Mutation::RemoveProperty { element, .. } => {
            Some(element.clone())
        }
        Mutation::SetSchema { .. } | Mutation::SetDefinition { .. } => None,
    }
}

/// What a mutation names, for a replay error: `vertex 'x'`, `edge 'x'`, or the catalog/schema.
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) fn mutation_subject(mutation: &Mutation) -> String {
    mutation_element(mutation)
        .map(|e| format!("{} '{}'", e.kind, e.id))
        .unwrap_or_else(|| "the catalog or schema".to_string())
}

/// Why the current state of `storage` does not match `mutation`'s before image (`None`: it
/// matches), read through the [`GraphStorage`] read API. A schema change is compared only with
/// `check_schema`.
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) fn before_mismatch<S: GraphStorage + ?Sized>(
    storage: &S,
    mutation: &Mutation,
    check_schema: bool,
) -> Option<String> {
    let vertex = |id: &str| storage.get_vertex_by_id(id);
    let edge = |id: &str| storage.get_edge_by_id(id);
    let property = |element: &ElementRef, key: &str| -> Result<Option<ElementProperty>, String> {
        match element.kind {
            ElementKind::Vertex => vertex(&element.id)
                .map(|v| {
                    storage
                        .vertex_properties(v)
                        .and_then(|p| p.get(key).cloned())
                })
                .ok_or_else(|| "the vertex is missing".to_string()),
            ElementKind::Edge => edge(&element.id)
                .map(|e| storage.edge_properties(e).and_then(|p| p.get(key).cloned()))
                .ok_or_else(|| "the edge is missing".to_string()),
        }
    };
    match mutation {
        Mutation::AddVertex { id, .. } => vertex(id).map(|_| "the vertex exists already".into()),
        Mutation::AddEdge { id, .. } => edge(id).map(|_| "the edge exists already".into()),
        Mutation::DropVertex { id, before } => match vertex(id) {
            None => Some("the vertex is missing".into()),
            Some(v) => {
                let labels: Vec<&str> = storage
                    .vertex_labels(v)
                    .iter()
                    .map(CompactString::as_str)
                    .collect();
                let expected: Vec<&str> = before.labels.iter().map(String::as_str).collect();
                let properties = storage.vertex_properties(v).cloned().unwrap_or_default();
                (labels != expected || properties != before.properties)
                    .then(|| "its labels or properties differ from the dropped ones".into())
            }
        },
        Mutation::DropEdge { id, before } => match edge(id) {
            None => Some("the edge is missing".into()),
            Some(e) => {
                let properties = storage.edge_properties(e).cloned().unwrap_or_default();
                let same_ends = storage.edge_endpoints(e).is_some_and(|(out_v, in_v)| {
                    storage.vertex_id(out_v) == Some(before.out_id.as_str())
                        && storage.vertex_id(in_v) == Some(before.in_id.as_str())
                });
                (storage.edge_label(e) != before.label.as_deref()
                    || !same_ends
                    || properties != before.properties)
                    .then(|| {
                        "its label, endpoints or properties differ from the dropped ones".into()
                    })
            }
        },
        Mutation::SetProperty {
            element,
            key,
            before,
            ..
        } => match property(element, key) {
            Err(reason) => Some(reason),
            Ok(current) => (current != *before)
                .then(|| format!("property '{key}' differs from the recorded before value")),
        },
        Mutation::RemoveProperty {
            element,
            key,
            before,
        } => match property(element, key) {
            Err(reason) => Some(reason),
            Ok(current) => (current.as_ref() != Some(before))
                .then(|| format!("property '{key}' differs from the recorded before value")),
        },
        Mutation::AddLabel { id, label } => match vertex(id) {
            None => Some("the vertex is missing".into()),
            Some(v) => storage
                .vertex_has_label(v, label)
                .then(|| format!("the vertex has the label '{label}' already")),
        },
        Mutation::RemoveLabel { id, label } => match vertex(id) {
            None => Some("the vertex is missing".into()),
            Some(v) => (!storage.vertex_has_label(v, label))
                .then(|| format!("the vertex lacks the label '{label}'")),
        },
        Mutation::SetEdgeLabel { id, before, .. } => match edge(id) {
            None => Some("the edge is missing".into()),
            Some(e) => (storage.edge_label(e) != before.as_deref())
                .then(|| "its label differs from the recorded before label".into()),
        },
        // A storage that cannot read its schema or catalog fails the check with its error: the
        // before image cannot be confirmed.
        Mutation::SetSchema { before, .. } if check_schema => match storage.get_schema() {
            Ok(schema) => (schema.unwrap_or_default().to_json() != before.to_json())
                .then(|| "the schema differs from the recorded before image".to_string()),
            Err(err) => Some(format!("the schema cannot be read: {err}")),
        },
        Mutation::SetSchema { .. } => None,
        Mutation::SetDefinition {
            kind, name, before, ..
        } => match storage.definition(*kind, name) {
            Ok(found) => (found != before.as_deref())
                .then(|| format!("the {kind} '{name}' differs from the recorded before image")),
            Err(err) => Some(format!("the {kind} '{name}' cannot be read: {err}")),
        },
    }
}
