//! The write side of a [`LayeredGraph`](super::LayeredGraph): the layer as the read view of the
//! write rules (`graph/write_rules.rs`) and its auto-id sequences.
//!
//! Every write of the layer calls its write-rule function first (nothing is mutated before the
//! rule passed; the rule includes the unique property indexes, checked over the index
//! overlays), then, for a create, `check_*_id_free`, then stores what the rule returned in
//! the delta. The view answers with the layer's CURRENT state: base elements as the base holds
//! them minus tombstones, images in place of the base state of modified ones, delta elements
//! from the delta, ids
//! across both.

use std::ops::Deref;

use compact_str::CompactString;

use super::LayeredGraph;
use super::handle::{LayeredEdge, LayeredVertex};
use crate::error::{EDGE_ID_SEQUENCE, GraphError, VERTEX_ID_SEQUENCE};
use crate::graph::GraphStorage;
use crate::graph::schema::{ElementKind, GraphSchema};
use crate::graph::vector_rules::VectorRules;
use crate::graph::write_rules::{EdgeView, WriteView};
use crate::value::{ElementProperty, PropertyMap};

impl WriteView for LayeredGraph {
    type Vertex = LayeredVertex;
    type Edge = LayeredEdge;

    /// The layer's own schema.
    fn schema(&self) -> impl Deref<Target = GraphSchema> + '_ {
        &self.schema
    }

    fn vertex(&self, vertex: LayeredVertex) -> Option<(&[CompactString], &PropertyMap)> {
        self.vertex_read(
            vertex,
            |base, node| WriteView::vertex(base, node),
            |view| view.map(|v| (v.labels, v.properties)),
        )
    }

    fn edge(&self, edge: LayeredEdge) -> Option<EdgeView<'_, LayeredVertex>> {
        self.edge_read(
            edge,
            |base, edge| {
                WriteView::edge(base, edge).map(|e| EdgeView {
                    label: e.label,
                    properties: e.properties,
                    from: LayeredVertex::from_base(e.from),
                    to: LayeredVertex::from_base(e.to),
                })
            },
            |view| {
                view.map(|e| EdgeView {
                    label: e.label,
                    properties: e.properties,
                    from: e.endpoints.0,
                    to: e.endpoints.1,
                })
            },
        )
    }

    fn out_edges(&self, vertex: LayeredVertex) -> impl Iterator<Item = LayeredEdge> + '_ {
        GraphStorage::out_edges(self, vertex).into_iter().flatten()
    }

    fn in_edges(&self, vertex: LayeredVertex) -> impl Iterator<Item = LayeredEdge> + '_ {
        GraphStorage::in_edges(self, vertex).into_iter().flatten()
    }

    fn contains_vertex_id(&self, id: &str) -> bool {
        self.get_vertex_by_id(id).is_ok_and(|found| found.is_some())
    }

    fn contains_edge_id(&self, id: &str) -> bool {
        self.get_edge_by_id(id).is_ok_and(|found| found.is_some())
    }

    /// Over the index overlays (`index.rs`, [`LayeredGraph::layer_unique_check`]).
    fn check_unique_vertex(
        &self,
        me: Option<LayeredVertex>,
        labels: &[CompactString],
        properties: Option<&PropertyMap>,
        write: Option<(&str, &ElementProperty)>,
    ) -> Result<(), GraphError> {
        self.layer_unique_check(
            ElementKind::Vertex,
            me.map(LayeredVertex::slot),
            labels.iter().map(CompactString::as_str),
            properties,
            write,
        )
    }

    fn check_unique_edge(
        &self,
        me: Option<LayeredEdge>,
        label: Option<&str>,
        properties: Option<&PropertyMap>,
        write: Option<(&str, &ElementProperty)>,
    ) -> Result<(), GraphError> {
        self.layer_unique_check(
            ElementKind::Edge,
            me.map(LayeredEdge::slot),
            label,
            properties,
            write,
        )
    }

    /// The layer's compiled vector rules inside a live unit with the rules on (module `rules`).
    #[inline]
    fn derived_rules(&self) -> Option<&VectorRules> {
        self.live_vector_rules()
    }
}

impl LayeredGraph {
    /// The id of a vertex to create: `id` as given (a numeric one raises the sequence, as
    /// `TraversalGraph::insert_vertex` does), or the next free auto id. Returns the id and the
    /// sequence before, which the caller restores when the id turns out to be taken.
    pub(super) fn vertex_id_for(
        &mut self,
        id: Option<CompactString>,
    ) -> Result<(CompactString, u64), GraphError> {
        let sequence = self.node_sequence;
        let id = match id {
            Some(id) => {
                if let Ok(parsed) = id.parse::<u64>() {
                    self.node_sequence = self.node_sequence.max(parsed);
                }
                id
            }
            None => self.next_vertex_id()?,
        };
        Ok((id, sequence))
    }

    /// The next free vertex auto id: the sequence advanced past every id taken in the base or
    /// the delta. Never wraps ([`GraphError::SequenceOverflow`]).
    fn next_vertex_id(&mut self) -> Result<CompactString, GraphError> {
        loop {
            self.node_sequence =
                self.node_sequence
                    .checked_add(1)
                    .ok_or(GraphError::SequenceOverflow {
                        sequence: VERTEX_ID_SEQUENCE,
                    })?;
            let candidate = CompactString::from(self.node_sequence.to_string());
            if !WriteView::contains_vertex_id(self, &candidate) {
                return Ok(candidate);
            }
        }
    }

    /// The id of an edge to create, its uniqueness checked first (as `TraversalGraph` checks an
    /// explicit edge id before it raises the sequence), or the next free auto id.
    pub(super) fn edge_id_for(
        &mut self,
        id: Option<CompactString>,
    ) -> Result<CompactString, GraphError> {
        match id {
            Some(id) => {
                crate::graph::write_rules::check_edge_id_free(self, &id)?;
                if let Ok(parsed) = id.parse::<u64>() {
                    self.edge_sequence = self.edge_sequence.max(parsed);
                }
                Ok(id)
            }
            None => loop {
                self.edge_sequence =
                    self.edge_sequence
                        .checked_add(1)
                        .ok_or(GraphError::SequenceOverflow {
                            sequence: EDGE_ID_SEQUENCE,
                        })?;
                let candidate = CompactString::from(self.edge_sequence.to_string());
                if !WriteView::contains_edge_id(self, &candidate) {
                    return Ok(candidate);
                }
            },
        }
    }
}
