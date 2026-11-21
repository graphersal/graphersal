//! Units of atomicity of a [`LayeredGraph`](super::LayeredGraph): the layer's own undo log
//! (S1.8), the counterpart of `TraversalGraph`'s `graph/undo.rs`, with the same semantics.
//!
//! While a unit is open every write of the layer pushes the inverse of what it did to the
//! DELTA (the base is never written, so there is nothing to undo there):
//!
//! | Write | Undo entry |
//! |---|---|
//! | create (`insert_*`, `add_*`) | [`LayerUndo::AddedVertex`] / [`LayerUndo::AddedEdge`]: remove it again |
//! | drop of an added element | [`LayerUndo::RemovedVertex`] / [`LayerUndo::RemovedEdge`]: store it again (its cascaded edges are entries of their own, logged first) |
//! | drop of a base element | [`LayerUndo::RemovedBaseVertex`] / [`LayerUndo::RemovedBaseEdge`]: clear the tombstone, put the image back |
//! | property, path and label writes of an added element | the old value of the key (position included), the old label set or label |
//! | property, path and label writes of a base element | the same, plus whether the element had an image before: an undo of the write that copied it up drops the image again; a return to clean in between is undone by copying it up again |
//! | a property write of a base element that changes nothing (an identical value) | [`LayerUndo::UnchangedBaseVertexProperty`] / [`LayerUndo::UnchangedBaseEdgeProperty`]: nothing to undo; recorded only so the change set and the commit count match `TraversalGraph`'s, which records such a write |
//! | `set_schema` | [`LayerUndo::Schema`]: the layer's old schema |
//! | catalog writes | [`LayerUndo::Definition`]: the old definition of that kind and name, or "absent", one per definition |
//!
//! **Handles.** Entries name added elements by id and base elements by their base handle. A
//! base handle never changes (the base is frozen), so undoing the drop of a base element makes
//! every handle of it valid again. An added element whose drop is undone is stored again in a
//! new slot generation, so handles taken before its drop stay stale, exactly as in a
//! `TraversalGraph` (whose undo puts a dropped element back with a new generation too): the
//! delta's slot store bumps a generation on every removal, and reusing an old generation would
//! let a handle of an element removed earlier alias the restored one.
//!
//! **Not restored**, as in a `TraversalGraph`: the auto-id sequences (an id once handed out is
//! never handed out again), the dirty bits (a set bit means only "consult the delta"), stubs a
//! unit created (empty and invisible), and iteration order of restored added elements (a new
//! slot, the front of a neighbour's adjacency list, the end of a label bucket). Base elements
//! come back in the base's order: their slots never move.
//!
//! **Cost:** nothing outside a unit (one branch per write); inside a unit one `Vec` push per
//! write plus the old value or the removed image, which the write already owns.
//!
//! **Change capture and commit hooks (S1.9)**, with `TraversalGraph`'s semantics
//! (`graph/undo.rs`, `graph/capture.rs`): the log carries the layer's [`CommitHooks`] (with the
//! instance [`CommitClock`](crate::graph::CommitClock)); every outermost commit that changed
//! something takes a commit time from it (never earlier than the previous commit); while a hook
//! is registered the commit derives the unit's [`ChangeSet`] from the log (`capture`) and asks
//! every `before_commit`: a veto (or a panic there) rolls the unit back and sends
//! `after_rollback`. A rollback of an outermost unit that recorded anything (also when a
//! savepoint undid it since) sends `after_rollback(reason)`; a discarded unit (a dry run) sends
//! nothing and returns its change set.

use compact_str::CompactString;

use super::LayeredGraph;
use super::delta::{EdgeImage, OldProperty, RemovedEdge, RemovedVertex, VertexCopy};
use crate::error::GraphError;
use crate::graph::change::{ChangeSet, CommitMetadata, CommitStamp};
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::hooks::{CommitHooks, RollbackReason};
use crate::graph::schema::GraphSchema;
use crate::graph::storage::{UNWOUND, UnitMark, close_on_unwind};
use crate::graph::undo::{HOOK_PANICKED, mark_position, now_nanos};
use crate::graph::{EdgeHandle, UnitChanges, VertexHandle};

/// A log grown beyond this many entries is freed, not kept, when its outermost unit ends (as
/// `TraversalGraph`'s).
const KEEP_CAPACITY: usize = 1024;

/// The inverse of one write of the layer; see the module docs for the mapping.
#[derive(Debug)]
pub(crate) enum LayerUndo {
    /// A vertex was added (the auto-id sequence is not restored).
    AddedVertex { id: CompactString },
    /// An edge was added (the auto-id sequence is not restored; a stub it created stays).
    AddedEdge { id: CompactString },
    /// An added vertex was dropped; its edges were dropped before it, each an entry of its own.
    RemovedVertex(Box<RemovedVertex>),
    /// An added edge was dropped.
    RemovedEdge(Box<RemovedEdge>),
    /// A top-level property of an added vertex was written or removed.
    VertexProperty {
        id: CompactString,
        key: CompactString,
        old: OldProperty,
    },
    /// A top-level property of an added edge was written or removed; `map_was_none` restores
    /// an edge without a map.
    EdgeProperty {
        id: CompactString,
        key: CompactString,
        old: OldProperty,
        map_was_none: bool,
    },
    /// The label set of an added vertex changed.
    VertexLabels {
        id: CompactString,
        old: Vec<CompactString>,
    },
    /// The label of an added edge changed (`None`: it was unlabeled).
    EdgeLabel {
        id: CompactString,
        old: Option<CompactString>,
    },
    /// A base vertex was dropped (its incident edges before it, each an entry of its own);
    /// `image` is the image it had.
    RemovedBaseVertex {
        node: VertexHandle,
        image: Option<Box<VertexCopy>>,
    },
    /// A base edge was dropped; `image` is the image it had, `position` its place among the
    /// base edges relabeled to the image's label.
    RemovedBaseEdge {
        edge: EdgeHandle,
        image: Option<Box<EdgeImage>>,
        position: Option<usize>,
    },
    /// A top-level property of a base vertex was written or removed (in its image);
    /// `had_image`: the image existed before the write (else the write copied it up).
    BaseVertexProperty {
        node: VertexHandle,
        key: CompactString,
        old: OldProperty,
        had_image: bool,
    },
    /// The edge counterpart of [`Self::BaseVertexProperty`].
    BaseEdgeProperty {
        edge: EdgeHandle,
        key: CompactString,
        old: OldProperty,
        map_was_none: bool,
        had_image: bool,
    },
    /// The label set of a base vertex changed (in its image).
    BaseVertexLabels {
        node: VertexHandle,
        old: Vec<CompactString>,
        had_image: bool,
    },
    /// The label of a base edge changed (in its image); `position` is the edge's place among
    /// the base edges relabeled to the old label.
    BaseEdgeLabel {
        edge: EdgeHandle,
        old: Option<CompactString>,
        position: Option<usize>,
        had_image: bool,
    },
    /// A property write of a base vertex stored nothing: the value was identical
    /// (`identity_eq`). Nothing to undo; the capture reports it as a `SetProperty` with equal
    /// before and after values and it makes the commit count, as `TraversalGraph` records such
    /// a write.
    UnchangedBaseVertexProperty {
        node: VertexHandle,
        key: CompactString,
    },
    /// The edge counterpart of [`Self::UnchangedBaseVertexProperty`].
    UnchangedBaseEdgeProperty {
        edge: EdgeHandle,
        key: CompactString,
    },
    /// The layer's schema was replaced.
    Schema(Box<GraphSchema>),
    /// A definition of the layer's catalog was stored, replaced or removed; `old` is what was
    /// there before (`None`: nothing).
    Definition {
        kind: DefinitionKind,
        name: CompactString,
        old: Option<Box<Definition>>,
    },
}

/// The layer's undo log: the inverse writes of the open units, oldest first, and how many
/// units are open (nested units are savepoints: positions in the log).
#[derive(Debug, Default)]
pub(crate) struct UnitLog {
    entries: Vec<LayerUndo>,
    depth: u32,
    /// The commit hooks in registration order, with the instance clock commit times come from.
    /// Change capture is on while at least one hook is registered.
    pub(crate) hooks: CommitHooks,
    /// The commit time of the last outermost unit that changed something, microseconds since
    /// the Unix epoch: the next commit is never earlier. Starts at the base's last commit time.
    last_committed_at: i64,
    /// Whether the outermost open unit recorded anything, also when a savepoint rolled it back
    /// since: its rollback is then reported to the hooks (an attempted change).
    changed: bool,
    /// The [`CommitMetadata`] the outermost open unit commits with; reset when it closes.
    metadata: CommitMetadata,
    /// Whether `raise_id_sequences` really raised a sequence while the outermost unit was
    /// open: its commit then counts as a commit without entries (as in `TraversalGraph`).
    sequences_raised: bool,
    /// The commit sequence number of the last outermost unit that changed something.
    last_commit_seq: u64,
    /// The start of the outermost open unit, nanoseconds since the Unix epoch.
    started_at: Option<i128>,
    /// The start of the last outermost unit: the next one never starts earlier.
    last_started_at: i128,
}

impl UnitLog {
    /// `true` while a unit is open: only then do writes record their inverse.
    #[inline]
    pub(crate) fn is_recording(&self) -> bool {
        self.depth > 0
    }

    /// Records `entry` if a unit is open.
    #[inline]
    pub(crate) fn record(&mut self, entry: LayerUndo) {
        if self.depth > 0 {
            self.entries.push(entry);
            self.changed = true;
        }
    }

    /// Notes that an auto-id sequence was really raised in the open unit (an attempted change:
    /// a rollback of the unit is reported to the hooks).
    #[inline]
    pub(crate) fn mark_sequences_raised(&mut self) {
        if self.depth > 0 {
            self.sequences_raised = true;
            self.changed = true;
        }
    }

    /// A log whose commit times never go below `last_committed_at` (the base's last commit).
    pub(crate) fn after(last_committed_at: i64) -> Self {
        Self {
            last_committed_at,
            ..Self::default()
        }
    }

    /// The recorded entries, oldest first.
    pub(crate) fn entries(&self) -> &[LayerUndo] {
        &self.entries
    }

    /// The commit sequence number of the last outermost unit that changed something.
    pub(crate) fn last_commit_seq(&self) -> u64 {
        self.last_commit_seq
    }

    /// The start of the outermost open unit.
    pub(crate) fn started_at(&self) -> Option<i128> {
        self.started_at
    }

    /// The time the next outermost unit would start with now: the system clock, never earlier
    /// than the previous unit's start, the last commit's time or the instance clock's floor.
    pub(crate) fn next_started_at(&self) -> i128 {
        self.start_from(now_nanos())
    }

    /// `clock` raised to the log's monotonic floor (as `TraversalGraph::unit_start_from`).
    fn start_from(&self, clock: i128) -> i128 {
        clock
            .max(self.last_started_at)
            .max(self.last_committed_at as i128 * 1000)
            .max(self.hooks.clock_ref().floor() as i128 * 1000)
    }

    /// Opens a unit; the outermost one takes `clock` (raised to the log's monotonic floor) as
    /// its time. The mark is the log position.
    fn begin(&mut self, clock: i128) -> UnitMark {
        if self.depth == 0 {
            let started_at = self.start_from(clock);
            self.started_at = Some(started_at);
            self.last_started_at = started_at;
        }
        self.depth = self.depth.saturating_add(1);
        UnitMark::new(self.entries.len() as u64)
    }

    /// Closes one unit; the outermost one clears the log.
    fn close(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.changed = false;
            self.sequences_raised = false;
            self.metadata = CommitMetadata::default();
            self.started_at = None;
            if self.entries.capacity() > KEEP_CAPACITY {
                self.entries = Vec::new();
            } else {
                self.entries.clear();
            }
        }
    }
}

impl LayeredGraph {
    /// Opens a unit (`GraphStorage::begin_unit`); the outermost one starts at `clock`.
    pub(super) fn begin_layer_unit(&mut self, clock: i128) -> UnitMark {
        self.units.begin(clock)
    }

    /// Opens a unit at the system clock.
    pub(super) fn begin_layer_unit_now(&mut self) -> UnitMark {
        self.begin_layer_unit(now_nanos())
    }

    /// Closes a unit and keeps its changes, as `TraversalGraph::commit_undo_unit` does (without
    /// the vector rules, refused on a layer, and without the memory budget check, task S1.11).
    /// The outermost commit of a unit that changed something (or only raised an auto-id
    /// sequence) advances the commit sequence number (checked: at `u64::MAX` the unit is rolled
    /// back and the commit fails with [`GraphError::SequenceOverflow`]) and takes the commit
    /// time from the instance clock. While hooks are registered it first derives the unit's
    /// [`ChangeSet`] and runs every `before_commit`: a veto rolls the unit back, sends
    /// `after_rollback` and comes back as [`GraphError::CommitRejected`]; a panic there rolls
    /// back, sends `after_rollback` and continues. Otherwise every hook gets `after_commit`.
    pub(super) fn commit_layer_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        if self.units.depth != 1 || (self.units.entries.is_empty() && !self.units.sequences_raised)
        {
            self.units.close();
            return Ok(());
        }
        let Some(commit_seq) = self.units.last_commit_seq.checked_add(1) else {
            let error = GraphError::SequenceOverflow {
                sequence: crate::error::COMMIT_SEQUENCE,
            };
            self.rollback_layer_unit(mark, &RollbackReason::failed(&error));
            return Err(error);
        };
        let committed_at = self.units.hooks.commit_time(self.units.last_committed_at);
        if !self.units.hooks.is_active() {
            self.finish_layer_commit(commit_seq, committed_at);
            return Ok(());
        }
        // Deriving the change set and asking the hooks may panic (a user hook's
        // `before_commit`): the unit is rolled back first, as in `TraversalGraph`.
        let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mutations = self.derive_layer_mutations(0);
            if mutations.is_empty() && !self.units.sequences_raised {
                // Unreachable in practice (every log entry is a change); nothing to report.
                return None;
            }
            let metadata = std::mem::take(&mut self.units.metadata);
            let stamp = CommitStamp {
                commit_seq,
                committed_at,
                node_sequence: self.node_sequence,
                edge_sequence: self.edge_sequence,
            };
            let change_set = ChangeSet::new(stamp, metadata, mutations);
            let verdict = self.units.hooks.before_commit(&change_set);
            Some((change_set, verdict))
        }));
        let (change_set, verdict) = match asked {
            Ok(Some(asked)) => asked,
            Ok(None) => {
                self.finish_layer_commit(commit_seq, committed_at);
                return Ok(());
            }
            Err(payload) => {
                self.undo_layer_to(mark);
                self.units.close();
                let reason = RollbackReason::failed(&HOOK_PANICKED);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.units.hooks.after_rollback(&reason);
                }))
                .is_err()
                {
                    tracing::error!("a commit hook panicked in after_rollback after a panic");
                }
                std::panic::resume_unwind(payload)
            }
        };
        match verdict {
            Ok(()) => {
                self.finish_layer_commit(commit_seq, committed_at);
                self.units.hooks.after_commit(&change_set);
                Ok(())
            }
            Err(error) => {
                self.undo_layer_to(mark);
                self.units.close();
                self.units
                    .hooks
                    .after_rollback(&RollbackReason::from_error(&error));
                Err(error)
            }
        }
    }

    /// Makes the outermost unit final under `commit_seq` and `committed_at`.
    fn finish_layer_commit(&mut self, commit_seq: u64, committed_at: i64) {
        self.units.last_commit_seq = commit_seq;
        self.units.last_committed_at = committed_at;
        self.units.close();
    }

    /// Undoes every change since `mark`, newest first, and closes the unit. The rollback of an
    /// outermost unit that recorded anything (also when a savepoint already undid it) sends
    /// `after_rollback(reason)` to every hook.
    pub(super) fn rollback_layer_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        let notify = self.units.depth == 1 && self.units.hooks.is_active() && self.units.changed;
        self.undo_layer_to(mark);
        self.units.close();
        if notify {
            self.units.hooks.after_rollback(reason);
        }
    }

    /// Undoes every change since `mark` and closes the unit WITHOUT telling the hooks, after
    /// deriving what the unit changed (`GraphStorage::discard_unit`, the end of a dry run):
    /// never committed (`commit_seq` and commit time `0`, no metadata), the auto-id sequences
    /// after the unit.
    pub(super) fn discard_layer_unit(&mut self, mark: UnitMark) -> ChangeSet {
        let mutations = self.derive_layer_mutations(mark_position(mark));
        let stamp = CommitStamp {
            commit_seq: 0,
            committed_at: 0,
            node_sequence: self.node_sequence,
            edge_sequence: self.edge_sequence,
        };
        self.undo_layer_to(mark);
        self.units.close();
        ChangeSet::new(stamp, CommitMetadata::default(), mutations)
    }

    /// The metadata of the open unit's commit, as `TraversalGraph::set_undo_metadata`: with
    /// only the outermost unit open it takes precedence over what nested units left; inside a
    /// nested unit it adds only what is missing. Ignored without an open unit.
    pub(super) fn set_layer_unit_metadata(&mut self, metadata: CommitMetadata) {
        match self.units.depth {
            0 => {}
            1 => {
                let nested = std::mem::replace(&mut self.units.metadata, metadata);
                self.units.metadata.merge_missing(nested);
            }
            _ => self.units.metadata.merge_missing(metadata),
        }
    }

    /// Undoes every change since `mark`, newest first; the unit stays open.
    fn undo_layer_to(&mut self, mark: UnitMark) {
        let position = mark_position(mark);
        while self.units.entries.len() > position {
            let Some(entry) = self.units.entries.pop() else {
                break;
            };
            if let Err(error) = self.undo_entry(entry) {
                // Never expected: the replay is strictly reverse, so the layer is in exactly
                // the state right after the write being undone.
                tracing::error!(%error, "layer undo: an entry could not be undone");
            }
        }
    }

    /// What the open unit `mark` holds that is still in effect.
    pub(super) fn layer_unit_changes(&self, mark: UnitMark) -> UnitChanges {
        let mut changes = UnitChanges::NONE;
        if self.units.depth == 0 {
            return changes;
        }
        for entry in self.units.entries.iter().skip(mark_position(mark)) {
            match entry {
                LayerUndo::Schema(_) => changes.schema = true,
                LayerUndo::Definition { .. } => changes.definitions = true,
                LayerUndo::AddedVertex { .. }
                | LayerUndo::AddedEdge { .. }
                | LayerUndo::RemovedVertex(_)
                | LayerUndo::RemovedEdge(_)
                | LayerUndo::VertexProperty { .. }
                | LayerUndo::EdgeProperty { .. }
                | LayerUndo::VertexLabels { .. }
                | LayerUndo::EdgeLabel { .. }
                | LayerUndo::RemovedBaseVertex { .. }
                | LayerUndo::RemovedBaseEdge { .. }
                | LayerUndo::BaseVertexProperty { .. }
                | LayerUndo::BaseEdgeProperty { .. }
                | LayerUndo::BaseVertexLabels { .. }
                | LayerUndo::BaseEdgeLabel { .. }
                | LayerUndo::UnchangedBaseVertexProperty { .. }
                | LayerUndo::UnchangedBaseEdgeProperty { .. } => changes.data = true,
            }
            if changes.data && changes.schema && changes.definitions {
                break;
            }
        }
        changes
    }

    /// Runs `change` (a schema or catalog write) as a unit of its own when no unit is open, as
    /// `TraversalGraph` does: committed when it succeeds (a commit of its own), rolled back
    /// when it fails or panics, so a failing list of definitions leaves nothing.
    pub(super) fn in_own_unit<T>(
        &mut self,
        change: impl FnOnce(&mut Self) -> Result<T, GraphError>,
    ) -> Result<T, GraphError> {
        if self.units.is_recording() {
            return change(self);
        }
        let mark = self.begin_layer_unit_now();
        let changed = close_on_unwind(self, change, |this| {
            this.rollback_layer_unit(mark, &RollbackReason::failed(&UNWOUND))
        });
        match changed {
            Ok(value) => {
                self.commit_layer_unit(mark)?;
                Ok(value)
            }
            Err(err) => {
                self.rollback_layer_unit(mark, &RollbackReason::failed(&err));
                Err(err)
            }
        }
    }

    /// Applies the inverse of one write, without recording anything and without write rules
    /// (it restores a state the rules already accepted).
    fn undo_entry(&mut self, entry: LayerUndo) -> Result<(), GraphError> {
        match entry {
            LayerUndo::AddedVertex { id } => match self.delta.vertex_by_id(&id) {
                Some(node) => {
                    self.delta.remove_vertex(node, &mut self.statistics, None);
                }
                None => tracing::error!(%id, "layer undo: added vertex not found"),
            },
            LayerUndo::AddedEdge { id } => match self.delta.edge_by_id(&id) {
                Some(edge) => {
                    self.delta.remove_edge(edge, &mut self.statistics, false);
                }
                None => tracing::error!(%id, "layer undo: added edge not found"),
            },
            LayerUndo::RemovedVertex(removed) => {
                self.delta.restore_vertex(*removed, &mut self.statistics)?;
            }
            LayerUndo::RemovedEdge(removed) => {
                self.delta.restore_edge(*removed, &mut self.statistics)?;
            }
            LayerUndo::VertexProperty { id, key, old } => {
                self.delta.restore_vertex_property(&id, &key, old);
            }
            LayerUndo::EdgeProperty {
                id,
                key,
                old,
                map_was_none,
            } => self
                .delta
                .restore_edge_property(&id, &key, old, map_was_none),
            LayerUndo::VertexLabels { id, old } => match self.delta.vertex_by_id(&id) {
                Some(node) => {
                    self.delta
                        .set_vertex_labels(node, old, &mut self.statistics);
                }
                None => tracing::error!(%id, "layer undo: vertex of a label change not found"),
            },
            LayerUndo::EdgeLabel { id, old } => match self.delta.edge_by_id(&id) {
                Some(edge) => {
                    self.delta.set_edge_label(edge, old, &mut self.statistics);
                }
                None => tracing::error!(%id, "layer undo: edge of a label change not found"),
            },
            LayerUndo::RemovedBaseVertex { node, image } => {
                self.delta.restore_base_vertex(
                    &self.base,
                    node,
                    image.map(|image| *image),
                    &mut self.statistics,
                )?;
            }
            LayerUndo::RemovedBaseEdge {
                edge,
                image,
                position,
            } => self.delta.restore_base_edge(
                &self.base,
                edge,
                image.map(|image| *image),
                position,
                &mut self.statistics,
            ),
            LayerUndo::BaseVertexProperty {
                node,
                key,
                old,
                had_image,
            } => {
                if had_image {
                    self.delta
                        .restore_base_vertex_property(&self.base, node, &key, old)?;
                } else {
                    // The write copied the vertex up and changed one property only: its labels
                    // are the base's, so dropping the image restores everything.
                    self.delta.clear_vertex_image(node);
                }
            }
            LayerUndo::BaseEdgeProperty {
                edge,
                key,
                old,
                map_was_none,
                had_image,
            } => {
                if had_image {
                    self.delta.restore_base_edge_property(
                        &self.base,
                        edge,
                        &key,
                        old,
                        map_was_none,
                    )?;
                } else {
                    self.delta.clear_edge_image(edge);
                }
            }
            LayerUndo::BaseVertexLabels {
                node,
                old,
                had_image,
            } => {
                // The old labels back first (keeping the label lookups and the statistics in
                // step); without an image before, they are the base's and the image goes.
                self.delta
                    .set_base_vertex_labels(&self.base, node, old, &mut self.statistics)?;
                if !had_image {
                    self.delta.clear_vertex_image(node);
                }
            }
            LayerUndo::BaseEdgeLabel {
                edge,
                old,
                position,
                had_image,
            } => {
                self.delta.set_base_edge_label(
                    &self.base,
                    edge,
                    old,
                    position,
                    &mut self.statistics,
                )?;
                if !had_image {
                    self.delta.clear_edge_image(edge);
                }
            }
            // Nothing was stored: nothing to undo.
            LayerUndo::UnchangedBaseVertexProperty { .. }
            | LayerUndo::UnchangedBaseEdgeProperty { .. } => {}
            LayerUndo::Schema(old) => self.schema = *old,
            LayerUndo::Definition { kind, name, old } => {
                match old {
                    Some(old) => self.catalog.insert(*old),
                    None => self.catalog.remove(kind, &name),
                };
            }
        }
        Ok(())
    }
}
