//! Change capture: the undo log of [`TraversalGraph`] read forward as a list of
//! [`Mutation`]s with before AND after values (`book/src/users_guide/transactions.md`).
//!
//! The log holds the before-values only (what rollback needs). The after-values are derived at
//! commit by walking the log backwards over a *shadow* of the touched elements: the shadow
//! starts as the graph's current state (the end of the unit) and each entry's inverse is
//! applied to the shadow, never to the graph. The after-value of an entry is the shadow's value
//! right before its inverse is applied. Nothing is recorded during the mutations themselves, so
//! a graph without capture pays nothing; the walk runs once per committed unit while capture is
//! on and clones the touched elements.

use std::collections::HashMap;

use compact_str::CompactString;

use crate::graph::TraversalGraph;
use crate::graph::change::{EdgeState, ElementRef, Mutation, VertexState};
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::schema::GraphSchema;
use crate::graph::undo::{EdgeImage, UndoEntry, restore_property};
use crate::value::{ElementProperty, PropertyMap};

/// A vertex as the walk sees it at one position of the log.
struct ShadowVertex {
    labels: Vec<CompactString>,
    properties: PropertyMap,
}

/// An edge as the walk sees it at one position of the log.
struct ShadowEdge {
    label: Option<CompactString>,
    properties: Option<PropertyMap>,
    source: CompactString,
    target: CompactString,
}

impl ShadowEdge {
    fn of(edge: &EdgeImage) -> Self {
        Self {
            label: edge.label.clone(),
            properties: edge.properties.clone(),
            source: edge.source.clone(),
            target: edge.target.clone(),
        }
    }
}

/// The touched elements and the schema at the current position of the backwards walk; `None`
/// means "absent there". An element is loaded from the graph on first touch.
struct Shadow<'g> {
    graph: &'g TraversalGraph,
    vertices: HashMap<CompactString, Option<ShadowVertex>>,
    edges: HashMap<CompactString, Option<ShadowEdge>>,
    schema: Option<GraphSchema>,
    definitions: HashMap<(DefinitionKind, CompactString), Option<Box<Definition>>>,
}

impl<'g> Shadow<'g> {
    fn new(graph: &'g TraversalGraph) -> Self {
        Self {
            graph,
            vertices: HashMap::new(),
            edges: HashMap::new(),
            schema: None,
            definitions: HashMap::new(),
        }
    }

    fn vertex(&mut self, id: &CompactString) -> Option<&mut ShadowVertex> {
        let graph = self.graph;
        self.vertices
            .entry(id.clone())
            .or_insert_with(|| {
                let vertex = graph.vertex_weight(graph.index.get_node(&graph.graph, id)?)?;
                Some(ShadowVertex {
                    labels: vertex.1.clone(),
                    properties: vertex.2.clone(),
                })
            })
            .as_mut()
    }

    fn edge(&mut self, id: &CompactString) -> Option<&mut ShadowEdge> {
        let graph = self.graph;
        self.edges
            .entry(id.clone())
            .or_insert_with(|| {
                let index = graph.resolve_edge(graph.index.get_edge(&graph.graph, id)?)?;
                let (from, to) = graph.graph.edge_endpoints(index)?;
                let edge = graph.graph.edge_weight(index)?;
                Some(ShadowEdge {
                    label: edge.1.clone(),
                    properties: edge.2.clone(),
                    source: graph.graph.node_weight(from)?.0.clone(),
                    target: graph.graph.node_weight(to)?.0.clone(),
                })
            })
            .as_mut()
    }

    /// Marks a vertex absent and returns its state (the state right after it was added).
    fn take_vertex(&mut self, id: &CompactString) -> Option<ShadowVertex> {
        self.vertex(id)?;
        self.vertices.insert(id.clone(), None).flatten()
    }

    /// Edge counterpart of [`Self::take_vertex`].
    fn take_edge(&mut self, id: &CompactString) -> Option<ShadowEdge> {
        self.edge(id)?;
        self.edges.insert(id.clone(), None).flatten()
    }

    /// The definition of `kind` named `name` at the current position (`None`: absent there).
    fn definition(
        &mut self,
        kind: DefinitionKind,
        name: &CompactString,
    ) -> &mut Option<Box<Definition>> {
        let graph = self.graph;
        self.definitions
            .entry((kind, name.clone()))
            .or_insert_with(|| graph.definitions.get(kind, name).cloned().map(Box::new))
    }

    fn schema(&mut self) -> &mut GraphSchema {
        let graph = self.graph;
        self.schema
            .get_or_insert_with(|| graph.schema.read().clone())
    }
}

fn strings(labels: &[CompactString]) -> Vec<String> {
    labels.iter().map(|l| l.to_string()).collect()
}

fn edge_state(edge: &EdgeImage) -> EdgeState {
    EdgeState {
        label: edge.label.as_ref().map(|l| l.to_string()),
        out_id: edge.source.to_string(),
        in_id: edge.target.to_string(),
        properties: edge.properties.clone().unwrap_or_default(),
    }
}

/// The mutation of one property entry: nothing when the property was absent before and after.
fn property_mutation(
    element: ElementRef,
    key: &str,
    before: Option<ElementProperty>,
    after: Option<ElementProperty>,
) -> Option<Mutation> {
    match (before, after) {
        (before, Some(after)) => Some(Mutation::SetProperty {
            element,
            key: key.to_string(),
            before,
            after,
        }),
        (Some(before), None) => Some(Mutation::RemoveProperty {
            element,
            key: key.to_string(),
            before,
        }),
        (None, None) => None,
    }
}

impl TraversalGraph {
    /// The mutations recorded in the undo log from position `from` on (`0`: the whole log, the
    /// outermost unit), in execution order (see the module docs). The walk starts at the
    /// current state and only goes back to `from`, so a suffix (a savepoint, a nested dry run)
    /// derives exactly its own mutations.
    pub(crate) fn derive_mutations_since(&self, from: usize) -> Vec<Mutation> {
        let entries = self.undo.entries();
        let entries = entries.get(from..).unwrap_or_default();
        let mut shadow = Shadow::new(self);
        // Built newest first, reversed at the end: every multi-mutation entry pushes its
        // mutations in reverse forward order.
        let mut out: Vec<Mutation> = Vec::with_capacity(entries.len());
        for entry in entries.iter().rev() {
            match entry {
                UndoEntry::AddedVertex { id, .. } => match shadow.take_vertex(id) {
                    Some(vertex) => out.push(Mutation::AddVertex {
                        id: id.to_string(),
                        labels: strings(&vertex.labels),
                        properties: vertex.properties,
                    }),
                    None => tracing::error!(%id, "capture: added vertex not found"),
                },
                UndoEntry::AddedEdge { id, .. } => match shadow.take_edge(id) {
                    Some(edge) => out.push(Mutation::AddEdge {
                        id: id.to_string(),
                        label: edge.label.as_ref().map(|l| l.to_string()),
                        out_id: edge.source.to_string(),
                        in_id: edge.target.to_string(),
                        properties: edge.properties.unwrap_or_default(),
                    }),
                    None => tracing::error!(%id, "capture: added edge not found"),
                },
                UndoEntry::DroppedVertex { vertex, edges } => {
                    // Forward order: the cascaded edge drops in removal order, then the vertex.
                    out.push(Mutation::DropVertex {
                        id: vertex.id.to_string(),
                        before: VertexState {
                            labels: strings(&vertex.labels),
                            properties: vertex.properties.clone(),
                        },
                    });
                    shadow.vertices.insert(
                        vertex.id.clone(),
                        Some(ShadowVertex {
                            labels: vertex.labels.clone(),
                            properties: vertex.properties.clone(),
                        }),
                    );
                    for edge in edges.iter().rev() {
                        out.push(Mutation::DropEdge {
                            id: edge.id.to_string(),
                            before: edge_state(edge),
                        });
                        shadow
                            .edges
                            .insert(edge.id.clone(), Some(ShadowEdge::of(edge)));
                    }
                }
                UndoEntry::DroppedEdge(edge) => {
                    out.push(Mutation::DropEdge {
                        id: edge.id.to_string(),
                        before: edge_state(edge),
                    });
                    shadow
                        .edges
                        .insert(edge.id.clone(), Some(ShadowEdge::of(edge)));
                }
                UndoEntry::VertexProperty { id, key, old } => {
                    let Some(vertex) = shadow.vertex(id) else {
                        tracing::error!(%id, "capture: vertex of a property write not found");
                        continue;
                    };
                    let after = vertex.properties.get(key.as_str()).cloned();
                    let before = old.as_ref().map(|(_, value)| value.clone());
                    restore_property(&mut vertex.properties, key, old.clone());
                    out.extend(property_mutation(
                        ElementRef::vertex(id.as_str()),
                        key,
                        before,
                        after,
                    ));
                }
                UndoEntry::EdgeProperty {
                    id,
                    key,
                    old,
                    map_was_none,
                } => {
                    let Some(edge) = shadow.edge(id) else {
                        tracing::error!(%id, "capture: edge of a property write not found");
                        continue;
                    };
                    let after = edge
                        .properties
                        .as_ref()
                        .and_then(|properties| properties.get(key.as_str()))
                        .cloned();
                    let before = old.as_ref().map(|(_, value)| value.clone());
                    if *map_was_none {
                        edge.properties = None;
                    } else {
                        restore_property(
                            edge.properties.get_or_insert_with(PropertyMap::default),
                            key,
                            old.clone(),
                        );
                    }
                    out.extend(property_mutation(
                        ElementRef::edge(id.as_str()),
                        key,
                        before,
                        after,
                    ));
                }
                UndoEntry::VertexLabels { id, old } => {
                    let Some(vertex) = shadow.vertex(id) else {
                        tracing::error!(%id, "capture: vertex of a label change not found");
                        continue;
                    };
                    let after = std::mem::replace(&mut vertex.labels, old.clone());
                    // Forward order: removals, then additions.
                    for label in after.iter().filter(|l| !old.contains(l)).rev() {
                        out.push(Mutation::AddLabel {
                            id: id.to_string(),
                            label: label.to_string(),
                        });
                    }
                    for label in old.iter().filter(|l| !after.contains(l)).rev() {
                        out.push(Mutation::RemoveLabel {
                            id: id.to_string(),
                            label: label.to_string(),
                        });
                    }
                }
                UndoEntry::EdgeLabel { id, old } => {
                    let Some(edge) = shadow.edge(id) else {
                        tracing::error!(%id, "capture: edge of a label change not found");
                        continue;
                    };
                    let after = std::mem::replace(&mut edge.label, old.clone());
                    match after {
                        Some(after) => out.push(Mutation::SetEdgeLabel {
                            id: id.to_string(),
                            before: old.as_ref().map(|l| l.to_string()),
                            after: after.to_string(),
                        }),
                        None => tracing::error!(%id, "capture: edge label change to no label"),
                    }
                }
                UndoEntry::Schema(old) => {
                    let after = std::mem::replace(shadow.schema(), (**old).clone());
                    out.push(Mutation::SetSchema {
                        before: old.clone(),
                        after: Box::new(after),
                    });
                }
                UndoEntry::Definition { kind, name, old } => {
                    let after = std::mem::replace(shadow.definition(*kind, name), old.clone());
                    out.push(Mutation::SetDefinition {
                        kind: *kind,
                        name: name.to_string(),
                        before: old.clone(),
                        after,
                    });
                }
            }
        }
        out.reverse();
        // A change set holds logical values only (D5, D10): compressed strings of the undo log
        // and the shadow are decompressed here.
        out.iter_mut().for_each(Mutation::make_logical);
        out
    }
}
