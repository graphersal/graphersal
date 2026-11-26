//! Change capture of a [`LayeredGraph`] (S1.9): the layer's undo log (`units`) read forward as
//! a list of [`Mutation`]s with before AND after values, the same list in the same order as
//! `TraversalGraph`'s capture (`graph/capture.rs`) derives for the same writes on the same data.
//!
//! The method is the same: the log holds before-values only; the after-values come from a
//! backwards walk over a *shadow* of the touched elements, keyed by id (an id dropped and
//! reused in one unit is two elements one after the other, as there). The shadow starts as the
//! layer's current state, read through the layer's own reads; each entry's inverse is applied
//! to the shadow, never to the layer. What the layer's log adds:
//!
//! - **Base elements** are named by their base handle; their id, endpoints and, for an entry
//!   written without an image before (`had_image == false`), the whole state before come from
//!   the frozen base.
//! - **A dropped base element** reports the image it had (its labels and properties in the
//!   layer), else the base's state.
//! - **A write of an identical value** to a base element stored nothing; it is reported as a
//!   `SetProperty` with equal before and after values, as `TraversalGraph` reports it.
//!
//! The walk runs once per committed unit while a hook is registered (and for a dry run); it
//! clones the touched elements. Values are made logical at the end, as there.

use std::collections::HashMap;

use compact_str::CompactString;

use super::LayeredGraph;
use super::delta::End;
use super::units::LayerUndo;
use crate::graph::change::{EdgeState, ElementRef, Mutation, VertexState};
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::schema::GraphSchema;
use crate::graph::undo::restore_property;
use crate::graph::{EdgeHandle, GraphStorage, VertexHandle};
use crate::value::{ElementProperty, PropertyMap, logical_element};

/// A vertex as the walk sees it at one position of the log.
#[derive(Clone)]
struct ShadowVertex {
    labels: Vec<CompactString>,
    properties: PropertyMap,
}

/// An edge as the walk sees it at one position of the log.
#[derive(Clone)]
struct ShadowEdge {
    label: Option<CompactString>,
    properties: Option<PropertyMap>,
    source: CompactString,
    target: CompactString,
}

impl ShadowEdge {
    fn state(&self) -> EdgeState {
        EdgeState {
            label: self.label.as_ref().map(|l| l.to_string()),
            out_id: self.source.to_string(),
            in_id: self.target.to_string(),
            properties: self.properties.clone().unwrap_or_default(),
        }
    }
}

/// The touched elements, the schema and the definitions at the current position of the
/// backwards walk; `None` means "absent there". An element is loaded from the layer on first
/// touch.
struct Shadow<'g> {
    layer: &'g LayeredGraph,
    vertices: HashMap<CompactString, Option<ShadowVertex>>,
    edges: HashMap<CompactString, Option<ShadowEdge>>,
    schema: Option<GraphSchema>,
    definitions: HashMap<(DefinitionKind, CompactString), Option<Box<Definition>>>,
}

fn strings(labels: &[CompactString]) -> Vec<String> {
    labels.iter().map(|l| l.to_string()).collect()
}

/// The mutation of one property entry: nothing when the property was absent before and after
/// (as `graph/capture.rs`).
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

impl<'g> Shadow<'g> {
    fn new(layer: &'g LayeredGraph) -> Self {
        Self {
            layer,
            vertices: HashMap::new(),
            edges: HashMap::new(),
            schema: None,
            definitions: HashMap::new(),
        }
    }

    /// The layer's current vertex `id`, read through its own reads (`None`: absent).
    fn load_vertex(layer: &LayeredGraph, id: &str) -> Option<ShadowVertex> {
        let node = GraphStorage::get_vertex_by_id(layer, id).ok()??;
        Some(ShadowVertex {
            labels: GraphStorage::vertex_labels(layer, node).ok()?.to_vec(),
            properties: GraphStorage::vertex_properties(layer, node)
                .ok()?
                .map(logical_element)
                .unwrap_or_default(),
        })
    }

    /// The layer's current edge `id` (`None`: absent).
    fn load_edge(layer: &LayeredGraph, id: &str) -> Option<ShadowEdge> {
        let edge = GraphStorage::get_edge_by_id(layer, id).ok()??;
        let (from, to) = GraphStorage::edge_endpoints(layer, edge).ok()??;
        Some(ShadowEdge {
            label: GraphStorage::edge_label(layer, edge).ok()?.map(Into::into),
            properties: GraphStorage::edge_properties(layer, edge)
                .ok()?
                .map(logical_element),
            source: GraphStorage::vertex_id(layer, from).ok()??.into(),
            target: GraphStorage::vertex_id(layer, to).ok()??.into(),
        })
    }

    fn vertex(&mut self, id: &CompactString) -> Option<&mut ShadowVertex> {
        let layer = self.layer;
        self.vertices
            .entry(id.clone())
            .or_insert_with(|| Self::load_vertex(layer, id))
            .as_mut()
    }

    fn edge(&mut self, id: &CompactString) -> Option<&mut ShadowEdge> {
        let layer = self.layer;
        self.edges
            .entry(id.clone())
            .or_insert_with(|| Self::load_edge(layer, id))
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

    fn definition(
        &mut self,
        kind: DefinitionKind,
        name: &CompactString,
    ) -> &mut Option<Box<Definition>> {
        let layer = self.layer;
        self.definitions
            .entry((kind, name.clone()))
            .or_insert_with(|| layer.catalog.get(kind, name).cloned().map(Box::new))
    }

    fn schema(&mut self) -> &mut GraphSchema {
        let layer = self.layer;
        self.schema.get_or_insert_with(|| layer.schema.clone())
    }

    /// The id of the base vertex `node` (the base is frozen: also for a dropped one).
    fn base_vertex_id(&self, node: VertexHandle) -> Option<CompactString> {
        GraphStorage::vertex_id(&*self.layer.base, node)
            .ok()?
            .map(Into::into)
    }

    /// The id of the base edge `edge`.
    fn base_edge_id(&self, edge: EdgeHandle) -> Option<CompactString> {
        GraphStorage::edge_id(&*self.layer.base, edge)
            .ok()?
            .map(Into::into)
    }

    /// The base's own state of the vertex `node` (what a base vertex without an image reads).
    fn base_vertex(&self, node: VertexHandle) -> Option<ShadowVertex> {
        let base = &*self.layer.base;
        Some(ShadowVertex {
            labels: GraphStorage::vertex_labels(base, node).ok()?.to_vec(),
            properties: GraphStorage::vertex_properties(base, node)
                .ok()?
                .map(logical_element)
                .unwrap_or_default(),
        })
    }

    /// The base's own state of the edge `edge`.
    fn base_edge(&self, edge: EdgeHandle) -> Option<ShadowEdge> {
        let base = &*self.layer.base;
        let (from, to) = GraphStorage::edge_endpoints(base, edge).ok()??;
        Some(ShadowEdge {
            label: GraphStorage::edge_label(base, edge).ok()?.map(Into::into),
            properties: GraphStorage::edge_properties(base, edge)
                .ok()?
                .map(logical_element),
            source: GraphStorage::vertex_id(base, from).ok()??.into(),
            target: GraphStorage::vertex_id(base, to).ok()??.into(),
        })
    }

    /// The id an endpoint of a removed added edge names.
    fn end_id(&self, end: &End) -> Option<CompactString> {
        match end {
            End::Base(node) => self.base_vertex_id(*node),
            End::Delta(id) => Some(id.clone()),
        }
    }

    /// A vertex property entry (`old` is the key's state before the write) of the vertex `id`;
    /// `base`: the vertex had no image before the write, so its whole state before was the
    /// base's.
    fn vertex_property(
        &mut self,
        id: &CompactString,
        key: &CompactString,
        old: &Option<(usize, ElementProperty)>,
        base: Option<ShadowVertex>,
    ) -> Option<Mutation> {
        let Some(vertex) = self.vertex(id) else {
            tracing::error!(%id, "layer capture: vertex of a property write not found");
            return None;
        };
        let after = vertex.properties.get(key.as_str()).cloned();
        let before = match base {
            Some(base) => {
                let before = base.properties.get(key.as_str()).cloned();
                *vertex = base;
                before
            }
            None => {
                restore_property(&mut vertex.properties, key, old.clone());
                old.as_ref().map(|(_, value)| value.clone())
            }
        };
        property_mutation(ElementRef::vertex(id.as_str()), key, before, after)
    }

    /// Edge counterpart of [`Self::vertex_property`].
    fn edge_property(
        &mut self,
        id: &CompactString,
        key: &CompactString,
        old: &Option<(usize, ElementProperty)>,
        map_was_none: bool,
        base: Option<ShadowEdge>,
    ) -> Option<Mutation> {
        let Some(edge) = self.edge(id) else {
            tracing::error!(%id, "layer capture: edge of a property write not found");
            return None;
        };
        let after = edge
            .properties
            .as_ref()
            .and_then(|properties| properties.get(key.as_str()))
            .cloned();
        let before = match base {
            Some(base) => {
                let before = base
                    .properties
                    .as_ref()
                    .and_then(|properties| properties.get(key.as_str()))
                    .cloned();
                *edge = base;
                before
            }
            None => {
                if map_was_none {
                    edge.properties = None;
                } else {
                    restore_property(
                        edge.properties.get_or_insert_with(PropertyMap::default),
                        key,
                        old.clone(),
                    );
                }
                old.as_ref().map(|(_, value)| value.clone())
            }
        };
        property_mutation(ElementRef::edge(id.as_str()), key, before, after)
    }

    /// A label set entry of the vertex `id` (newest first into `out`): removals, then
    /// additions in forward order; `base` replaces the shadow (no image before).
    fn vertex_labels(
        &mut self,
        id: &CompactString,
        old: &[CompactString],
        base: Option<ShadowVertex>,
        out: &mut Vec<Mutation>,
    ) {
        let Some(vertex) = self.vertex(id) else {
            tracing::error!(%id, "layer capture: vertex of a label change not found");
            return;
        };
        let after = std::mem::replace(&mut vertex.labels, old.to_vec());
        if let Some(base) = base {
            *vertex = base;
        }
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

    /// A label entry of the edge `id`; `base` replaces the shadow (no image before).
    fn edge_label(
        &mut self,
        id: &CompactString,
        old: &Option<CompactString>,
        base: Option<ShadowEdge>,
    ) -> Option<Mutation> {
        let Some(edge) = self.edge(id) else {
            tracing::error!(%id, "layer capture: edge of a label change not found");
            return None;
        };
        let after = std::mem::replace(&mut edge.label, old.clone());
        if let Some(base) = base {
            *edge = base;
        }
        match after {
            Some(after) => Some(Mutation::SetEdgeLabel {
                id: id.to_string(),
                before: old.as_ref().map(|l| l.to_string()),
                after: after.to_string(),
            }),
            None => {
                tracing::error!(%id, "layer capture: edge label change to no label");
                None
            }
        }
    }
}

impl LayeredGraph {
    /// The mutations recorded in the layer's undo log from position `from` on (`0`: the whole
    /// log, the outermost unit), in execution order (module docs). The walk only goes back to
    /// `from`, so a suffix (a savepoint, a nested dry run) derives exactly its own mutations.
    pub(super) fn derive_layer_mutations(&self, from: usize) -> Vec<Mutation> {
        let entries = self.units.entries();
        let entries = entries.get(from..).unwrap_or_default();
        let mut shadow = Shadow::new(self);
        // Built newest first, reversed at the end: every multi-mutation entry pushes its
        // mutations in reverse forward order.
        let mut out: Vec<Mutation> = Vec::with_capacity(entries.len());
        for entry in entries.iter().rev() {
            match entry {
                LayerUndo::AddedVertex { id } => match shadow.take_vertex(id) {
                    Some(vertex) => out.push(Mutation::AddVertex {
                        id: id.to_string(),
                        labels: strings(&vertex.labels),
                        properties: vertex.properties,
                    }),
                    None => tracing::error!(%id, "layer capture: added vertex not found"),
                },
                LayerUndo::AddedEdge { id } => match shadow.take_edge(id) {
                    Some(edge) => out.push(Mutation::AddEdge {
                        id: id.to_string(),
                        label: edge.label.as_ref().map(|l| l.to_string()),
                        out_id: edge.source.to_string(),
                        in_id: edge.target.to_string(),
                        properties: edge.properties.unwrap_or_default(),
                    }),
                    None => tracing::error!(%id, "layer capture: added edge not found"),
                },
                LayerUndo::RemovedVertex(removed) => {
                    let vertex = ShadowVertex {
                        labels: removed.labels.clone(),
                        properties: removed.properties.clone(),
                    };
                    drop_vertex(&mut shadow, &mut out, removed.id.clone(), vertex);
                }
                LayerUndo::RemovedEdge(removed) => {
                    let (Some(source), Some(target)) =
                        (shadow.end_id(&removed.from), shadow.end_id(&removed.to))
                    else {
                        tracing::error!(id = %removed.id, "layer capture: endpoint not found");
                        continue;
                    };
                    let edge = ShadowEdge {
                        label: removed.label.clone(),
                        properties: removed.properties.clone(),
                        source,
                        target,
                    };
                    drop_edge(&mut shadow, &mut out, removed.id.clone(), edge);
                }
                LayerUndo::RemovedBaseVertex { node, image } => {
                    let Some(id) = shadow.base_vertex_id(*node) else {
                        tracing::error!("layer capture: dropped base vertex not found");
                        continue;
                    };
                    let vertex = match image {
                        Some(image) => ShadowVertex {
                            labels: image.labels.clone(),
                            properties: image.properties.clone(),
                        },
                        None => match shadow.base_vertex(*node) {
                            Some(vertex) => vertex,
                            None => continue,
                        },
                    };
                    drop_vertex(&mut shadow, &mut out, id, vertex);
                }
                LayerUndo::RemovedBaseEdge { edge, image, .. } => {
                    let (Some(id), Some(mut state)) =
                        (shadow.base_edge_id(*edge), shadow.base_edge(*edge))
                    else {
                        tracing::error!("layer capture: dropped base edge not found");
                        continue;
                    };
                    if let Some(image) = image {
                        state.label = image.label.clone();
                        state.properties = image.properties.clone();
                    }
                    drop_edge(&mut shadow, &mut out, id, state);
                }
                // A restored base element is reported as the create it equals (its base state,
                // as `TraversalGraph` reports the create of an element with that state).
                LayerUndo::RestoredBaseVertex { node } => {
                    let Some(id) = shadow.base_vertex_id(*node) else {
                        tracing::error!("layer capture: restored base vertex not found");
                        continue;
                    };
                    match shadow.take_vertex(&id) {
                        Some(vertex) => out.push(Mutation::AddVertex {
                            id: id.to_string(),
                            labels: strings(&vertex.labels),
                            properties: vertex.properties,
                        }),
                        None => tracing::error!(%id, "layer capture: restored vertex not found"),
                    }
                }
                LayerUndo::RestoredBaseEdge { edge } => {
                    let Some(id) = shadow.base_edge_id(*edge) else {
                        tracing::error!("layer capture: restored base edge not found");
                        continue;
                    };
                    match shadow.take_edge(&id) {
                        Some(edge) => out.push(Mutation::AddEdge {
                            id: id.to_string(),
                            label: edge.label.as_ref().map(|l| l.to_string()),
                            out_id: edge.source.to_string(),
                            in_id: edge.target.to_string(),
                            properties: edge.properties.unwrap_or_default(),
                        }),
                        None => tracing::error!(%id, "layer capture: restored edge not found"),
                    }
                }
                LayerUndo::VertexProperty { id, key, old } => {
                    out.extend(shadow.vertex_property(id, key, old, None));
                }
                LayerUndo::EdgeProperty {
                    id,
                    key,
                    old,
                    map_was_none,
                } => out.extend(shadow.edge_property(id, key, old, *map_was_none, None)),
                LayerUndo::VertexLabels { id, old } => {
                    shadow.vertex_labels(id, old, None, &mut out);
                }
                LayerUndo::EdgeLabel { id, old } => out.extend(shadow.edge_label(id, old, None)),
                LayerUndo::BaseVertexProperty {
                    node,
                    key,
                    old,
                    had_image,
                } => {
                    let Some(id) = shadow.base_vertex_id(*node) else {
                        continue;
                    };
                    let base = if *had_image {
                        None
                    } else {
                        shadow.base_vertex(*node)
                    };
                    out.extend(shadow.vertex_property(&id, key, old, base));
                }
                LayerUndo::BaseEdgeProperty {
                    edge,
                    key,
                    old,
                    map_was_none,
                    had_image,
                } => {
                    let Some(id) = shadow.base_edge_id(*edge) else {
                        continue;
                    };
                    let base = if *had_image {
                        None
                    } else {
                        shadow.base_edge(*edge)
                    };
                    out.extend(shadow.edge_property(&id, key, old, *map_was_none, base));
                }
                LayerUndo::BaseVertexLabels {
                    node,
                    old,
                    had_image,
                } => {
                    let Some(id) = shadow.base_vertex_id(*node) else {
                        continue;
                    };
                    let base = if *had_image {
                        None
                    } else {
                        shadow.base_vertex(*node)
                    };
                    shadow.vertex_labels(&id, old, base, &mut out);
                }
                LayerUndo::BaseEdgeLabel {
                    edge,
                    old,
                    had_image,
                    ..
                } => {
                    let Some(id) = shadow.base_edge_id(*edge) else {
                        continue;
                    };
                    let base = if *had_image {
                        None
                    } else {
                        shadow.base_edge(*edge)
                    };
                    out.extend(shadow.edge_label(&id, old, base));
                }
                LayerUndo::UnchangedBaseVertexProperty { node, key } => {
                    let Some(id) = shadow.base_vertex_id(*node) else {
                        continue;
                    };
                    let value = shadow
                        .vertex(&id)
                        .and_then(|vertex| vertex.properties.get(key.as_str()).cloned());
                    match value {
                        Some(value) => out.push(Mutation::SetProperty {
                            element: ElementRef::vertex(id.as_str()),
                            key: key.to_string(),
                            before: Some(value.clone()),
                            after: value,
                        }),
                        None => tracing::error!(%id, "layer capture: unchanged property not found"),
                    }
                }
                LayerUndo::UnchangedBaseEdgeProperty { edge, key } => {
                    let Some(id) = shadow.base_edge_id(*edge) else {
                        continue;
                    };
                    let value = shadow.edge(&id).and_then(|edge| {
                        edge.properties
                            .as_ref()
                            .and_then(|properties| properties.get(key.as_str()))
                            .cloned()
                    });
                    match value {
                        Some(value) => out.push(Mutation::SetProperty {
                            element: ElementRef::edge(id.as_str()),
                            key: key.to_string(),
                            before: Some(value.clone()),
                            after: value,
                        }),
                        None => tracing::error!(%id, "layer capture: unchanged property not found"),
                    }
                }
                LayerUndo::Schema(old) => {
                    let after = std::mem::replace(shadow.schema(), (**old).clone());
                    out.push(Mutation::SetSchema {
                        before: old.clone(),
                        after: Box::new(after),
                    });
                }
                LayerUndo::Definition { kind, name, old } => {
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
        // A change set holds logical values only, as `TraversalGraph`'s.
        out.iter_mut().for_each(Mutation::make_logical);
        out
    }
}

/// A dropped vertex `id` whose state before the drop was `vertex`: its `DropVertex`, and the
/// shadow holds it again (its cascaded edges are entries of their own, older in the log).
fn drop_vertex(
    shadow: &mut Shadow<'_>,
    out: &mut Vec<Mutation>,
    id: CompactString,
    vertex: ShadowVertex,
) {
    out.push(Mutation::DropVertex {
        id: id.to_string(),
        before: VertexState {
            labels: strings(&vertex.labels),
            properties: vertex.properties.clone(),
        },
    });
    shadow.vertices.insert(id, Some(vertex));
}

/// A dropped edge `id` whose state before the drop was `edge`.
fn drop_edge(
    shadow: &mut Shadow<'_>,
    out: &mut Vec<Mutation>,
    id: CompactString,
    edge: ShadowEdge,
) {
    out.push(Mutation::DropEdge {
        id: id.to_string(),
        before: edge.state(),
    });
    shadow.edges.insert(id, Some(edge));
}
