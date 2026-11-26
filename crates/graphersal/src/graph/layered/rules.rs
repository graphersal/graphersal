//! Vector rules on a [`LayeredGraph`](super::LayeredGraph) (S4.5b): the layer runs
//! `TraversalGraph`'s rule pass (`graph/vector_rules.rs`, [`run_rule_pass`]) over its own unit
//! log, so the derived vectors of a layer equal those of a `TraversalGraph` holding the same
//! data after the same writes.
//!
//! - **Rules:** compiled from the layer's own schema (`vector_rules`), refreshed by every schema
//!   write of the layer and its undo. A layer over a base with rules compiles the base's rules
//!   when it is created and embeds nothing: the base's vectors are values of its maps, read in
//!   place.
//! - **Refusals:** a direct write or removal of a rule's `to` inside a live unit is refused by
//!   the write rules (`WriteView::derived_rules`, `graph/write_rules.rs`), with the same error
//!   in the same precedence as `TraversalGraph::check_derived_*`.
//! - **The pass:** at the outermost commit and in the discard of a dry run
//!   (`units.rs`), over the log mapped onto [`RuleEntry`] below. Its writes are ordinary writes
//!   of the layer: a base element is copied up only when its vector really changes (a vector
//!   recomputed to an identical value stores nothing, `identity_eq`), so an element whose
//!   sources are untouched is never copied up, except by the full fill of a rule the unit added
//!   or changed (every element of the label; the fill preview counts the base elements it
//!   copies, `PlannedFill::base_copies`).
//! - **`apply_change_set`** runs with the rules off and its log range exempt from the pass (the
//!   change set carries its vectors), as on a `TraversalGraph`.
//!
//! Mapping of the log ([`LayerUndo`] onto [`RuleEntry`]): added elements by their id, base
//! elements by their base id (the base is frozen, so the id borrows from it); a property write
//! (also an identical one, `Unchanged*`, which `TraversalGraph` logs as a property write) is a
//! `Property`, an insert, a restored base element or a label change an `Element`, a schema
//! change a `Schema`; drops and
//! catalog definitions are left out, as in `TraversalGraph`.

use std::sync::Arc;

use super::LayeredGraph;
use super::units::LayerUndo;
use crate::graph::GraphStorage;
use crate::graph::schema::{ElementKind, GraphSchema};
use crate::graph::vector_rules::{RuleEntry, RuleLog, VectorRules};
use crate::traversal::memory_budget::MemoryBudget;

impl LayeredGraph {
    /// Compiles the vector rules of the layer's current schema (after every change of
    /// `self.schema`).
    pub(super) fn refresh_vector_rules(&mut self) {
        self.vector_rules = VectorRules::compile(&self.schema);
    }

    /// Whether the rules act now: inside a live unit, not in `apply_change_set` nor in the rule
    /// pass itself (`TraversalGraph::rules_live`).
    #[inline]
    pub(super) fn layer_rules_live(&self) -> bool {
        self.rules_off == 0 && self.units.is_recording()
    }

    /// The compiled rules while they refuse direct writes (`WriteView::derived_rules`).
    #[inline]
    pub(super) fn live_vector_rules(&self) -> Option<&VectorRules> {
        self.vector_rules
            .as_deref()
            .filter(|_| self.layer_rules_live())
    }
}

impl RuleLog for LayeredGraph {
    #[inline]
    fn rule_set(&self) -> Option<&Arc<VectorRules>> {
        self.vector_rules.as_ref()
    }

    fn for_each_rule_entry<'a>(&'a self, mut visit: impl FnMut(RuleEntry<'a>)) {
        let exempt = self.units.rules_exempt();
        let in_exempt = |position: usize| exempt.iter().any(|(a, b)| (*a..*b).contains(&position));
        let base = &*self.base;
        // A base element's id (the base is frozen: its handles always resolve).
        let base_vertex = |node| GraphStorage::vertex_id(base, node).ok().flatten();
        let base_edge = |edge| GraphStorage::edge_id(base, edge).ok().flatten();
        for (position, entry) in self.units.entries().iter().enumerate() {
            let entry = match entry {
                LayerUndo::Schema(old) => RuleEntry::Schema {
                    before: old,
                    exempt: in_exempt(position),
                },
                _ if in_exempt(position) => continue,
                LayerUndo::VertexProperty { id, key, .. } => RuleEntry::Property {
                    kind: ElementKind::Vertex,
                    id,
                    key,
                },
                LayerUndo::EdgeProperty { id, key, .. } => RuleEntry::Property {
                    kind: ElementKind::Edge,
                    id,
                    key,
                },
                LayerUndo::BaseVertexProperty { node, key, .. }
                | LayerUndo::UnchangedBaseVertexProperty { node, key } => {
                    let Some(id) = base_vertex(*node) else {
                        continue;
                    };
                    RuleEntry::Property {
                        kind: ElementKind::Vertex,
                        id,
                        key,
                    }
                }
                LayerUndo::BaseEdgeProperty { edge, key, .. }
                | LayerUndo::UnchangedBaseEdgeProperty { edge, key } => {
                    let Some(id) = base_edge(*edge) else {
                        continue;
                    };
                    RuleEntry::Property {
                        kind: ElementKind::Edge,
                        id,
                        key,
                    }
                }
                LayerUndo::AddedVertex { id } | LayerUndo::VertexLabels { id, .. } => {
                    RuleEntry::Element {
                        kind: ElementKind::Vertex,
                        id,
                    }
                }
                LayerUndo::AddedEdge { id } | LayerUndo::EdgeLabel { id, .. } => {
                    RuleEntry::Element {
                        kind: ElementKind::Edge,
                        id,
                    }
                }
                LayerUndo::BaseVertexLabels { node, .. }
                | LayerUndo::RestoredBaseVertex { node } => {
                    let Some(id) = base_vertex(*node) else {
                        continue;
                    };
                    RuleEntry::Element {
                        kind: ElementKind::Vertex,
                        id,
                    }
                }
                LayerUndo::BaseEdgeLabel { edge, .. } | LayerUndo::RestoredBaseEdge { edge } => {
                    let Some(id) = base_edge(*edge) else {
                        continue;
                    };
                    RuleEntry::Element {
                        kind: ElementKind::Edge,
                        id,
                    }
                }
                LayerUndo::RemovedVertex(_)
                | LayerUndo::RemovedEdge(_)
                | LayerUndo::RemovedBaseVertex { .. }
                | LayerUndo::RemovedBaseEdge { .. }
                | LayerUndo::Definition { .. } => continue,
            };
            visit(entry);
        }
    }

    fn current_schema(&self) -> GraphSchema {
        self.schema.clone()
    }

    fn unit_principal(&self) -> Option<&str> {
        self.units.metadata().principal()
    }

    fn unit_budget(&self) -> Option<MemoryBudget> {
        self.units.budget()
    }

    #[inline]
    fn rules_off_mut(&mut self) -> &mut u32 {
        &mut self.rules_off
    }
}
