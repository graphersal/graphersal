//! The undo log of [`TraversalGraph`]: what makes a unit of work (one traversal, later an explicit
//! transaction) atomic (`book/src/users_guide/transactions.md`).
//!
//! While a unit is open ([`GraphStorage::begin_unit`](crate::graph::GraphStorage::begin_unit)),
//! every mutating `GraphStorage` method of `TraversalGraph` pushes the inverse of what it did:
//!
//! | Mutation | Undo entry |
//! |---|---|
//! | `insert_vertex` | [`UndoEntry::AddedVertex`]: remove it again (the auto-id sequence stays advanced) |
//! | `insert_edge` | [`UndoEntry::AddedEdge`]: remove it again (the auto-id sequence stays advanced) |
//! | `drop_vertex` | [`UndoEntry::DroppedVertex`]: image of the vertex and its cascaded edges |
//! | `drop_edge` | [`UndoEntry::DroppedEdge`]: image of the edge |
//! | `set_*_property`, `drop_*_property`, jpath writes and removals | [`UndoEntry::VertexProperty`] / [`UndoEntry::EdgeProperty`]: old value and position of the top-level key, or "absent" |
//! | `add_vertex_label`, `remove_vertex_label` | [`UndoEntry::VertexLabels`]: the old label set |
//! | `set_edge_label` | [`UndoEntry::EdgeLabel`]: the old label, or "unlabeled" |
//! | `set_schema` | [`UndoEntry::Schema`]: the old stored schema |
//! | `set_definition`, `remove_definition` | [`UndoEntry::Definition`]: the old definition of that kind and name, or "absent" |
//!
//! Entries hold string ids and element images, never handles: an undone drop puts the element
//! back into a (possibly different) slot with a new generation, so every handle taken before the
//! drop stays stale (section 1a). Rolling back replays the entries in reverse order down to the
//! unit's mark; each step restores the petgraph structure AND the id index, the label index (and
//! with them the label counts). The auto-id sequences are NOT restored: an id once handed out is
//! never reused, as with database sequences (gaps are fine). Because the replay is strictly
//! reverse, the graph is in exactly the state right after the mutation being undone, so every id
//! an entry names resolves.
//!
//! Not restored: iteration order. A restored element may come back in another slot (`v()` /
//! `e()` order), at the head of a neighbour's adjacency list (`out()` / `in()` order) and at
//! the end of a label bucket. Graph iteration order is unspecified, as in TinkerGraph.
//!
//! Cost: nothing outside a unit (one branch per mutation); inside a unit one `Vec` push per
//! mutation, plus the old value (property writes, label changes, schema) or the removed image
//! (drops), which the mutation already owns. Reads never touch the log.

use compact_str::CompactString;
use petgraph::Direction;
use petgraph::visit::EdgeRef;

use crate::error::GraphError;
use crate::graph::change::{ChangeSet, CommitMetadata, CommitStamp};
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::handle::EdgeGenerations;
use crate::graph::hooks::{CommitHooks, RollbackReason};
use crate::graph::schema::GraphSchema;
use crate::graph::storage::UnitMark;
use crate::graph::{EdgeHandle, EdgeIndex, TraversalGraph, VertexHandle};
use crate::traversal::memory_budget::MemoryBudget;
use crate::value::{ElementProperty, PropertyMap};

/// The operation [`GraphError::ResourceLimitExceeded`] names when an outermost unit fails its
/// memory budget at its commit.
pub(crate) const COMMIT_OPERATION: &str = "commit";

/// The [`RollbackReason`] message of a unit whose commit a panic unwound through (a commit hook's
/// `before_commit`, or an engine bug while the change set was derived).
pub(crate) const HOOK_PANICKED: &str = "a panic unwound through the commit (in a commit hook's before_commit); the unit was rolled back";

/// A log grown beyond this many entries is freed, not kept for reuse, when its outermost unit
/// ends, so one huge unit does not pin its memory for the life of the graph.
const KEEP_CAPACITY: usize = 1024;

/// The undo log of one [`TraversalGraph`]: the inverse operations of the open units, oldest
/// first, and how many units are open (nested units are savepoints: positions in the log).
#[derive(Debug, Default)]
pub(crate) struct UndoLog {
    entries: Vec<UndoEntry>,
    depth: u32,
    /// The commit hooks, in registration order. Change capture (`book/src/users_guide/transactions.md`)
    /// is on while at least one is registered: then the outermost commit of a unit that
    /// changed something derives a [`ChangeSet`] from the log and hands it to them.
    pub(crate) hooks: CommitHooks,
    /// The commit sequence number of the last unit that changed something (0: none yet).
    pub(crate) last_commit_seq: u64,
    /// The commit time of the last unit that changed something, in microseconds since the Unix
    /// epoch (0: none yet). The next commit's time is never earlier (monotonic per graph); a
    /// rollback does not touch it.
    pub(crate) last_committed_at: i64,
    /// `true` while a graph that is not published yet is being loaded
    /// ([`TraversalGraph::load_outside_units`]): a schema change is then not a unit of its own.
    pub(crate) loading: bool,
    /// Whether the outermost open unit recorded anything, also when a savepoint rolled it back
    /// since: its rollback is then reported to the hooks (an attempted change).
    changed: bool,
    /// Whether [`GraphStorage::raise_id_sequences`](crate::graph::GraphStorage::raise_id_sequences)
    /// really raised an auto-id sequence while the outermost unit was open: its commit is then a
    /// commit even without entries (a sequence-only commit, so the raise reaches the hooks and
    /// the journal). Reset when the outermost unit closes; a rollback keeps the raise in memory.
    sequences_raised: bool,
    /// The [`CommitMetadata`] the outermost open unit commits with (set by
    /// [`Transactional::transaction_with`](crate::graph::Transactional::transaction_with)); reset when the outermost unit closes.
    pub(crate) metadata: CommitMetadata,
    /// The memory budget the outermost open unit is checked against at its commit, started
    /// when it opened; `None` when the graph's execution policy sets no `memory.limit` (then
    /// nothing is paid). Reset when the outermost unit closes.
    budget: Option<MemoryBudget>,
    /// The start of the outermost open unit, in microseconds since the Unix epoch (`None`
    /// outside a unit): `GraphStorage::unit_started_at`, the time of `defaultFunction`
    /// `now`/`today`. Taken when the outermost unit opens; reset when it closes.
    started_at: Option<i64>,
    /// The start of the last outermost unit: the next one never starts earlier.
    last_started_at: i64,
}

impl UndoLog {
    /// `true` while a unit is open: only then do mutations record their inverse.
    #[inline]
    pub(crate) fn is_recording(&self) -> bool {
        self.depth > 0
    }

    /// Records `entry` if a unit is open.
    #[inline]
    pub(crate) fn record(&mut self, entry: UndoEntry) {
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

    /// The recorded entries, oldest first.
    pub(crate) fn entries(&self) -> &[UndoEntry] {
        &self.entries
    }

    /// The number of recorded entries (for tests and diagnostics).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Closes one unit; the outermost one clears the log.
    fn close(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.changed = false;
            self.sequences_raised = false;
            self.metadata = CommitMetadata::default();
            self.budget = None;
            self.started_at = None;
            if self.entries.capacity() > KEEP_CAPACITY {
                self.entries = Vec::new();
            } else {
                self.entries.clear();
            }
        }
    }
}

/// Image of a removed vertex: everything needed to put it back.
#[derive(Debug)]
pub(crate) struct VertexImage {
    pub(crate) id: CompactString,
    pub(crate) labels: Vec<CompactString>,
    pub(crate) properties: PropertyMap,
}

/// Image of a removed edge; the endpoints are named by their string ids.
#[derive(Debug)]
pub(crate) struct EdgeImage {
    pub(crate) id: CompactString,
    pub(crate) label: Option<CompactString>,
    pub(crate) properties: Option<PropertyMap>,
    pub(crate) source: CompactString,
    pub(crate) target: CompactString,
}

/// The old state of one top-level property: `Some((position, value))` when it existed,
/// `None` when it was absent.
pub(crate) type OldProperty = Option<(usize, ElementProperty)>;

/// The inverse of one mutation; see the module docs for the mapping.
#[derive(Debug)]
pub(crate) enum UndoEntry {
    /// A vertex was added. The auto-id sequence is NOT restored on rollback: an id once handed
    /// out is never handed out again (database sequences behave the same way; gaps are fine).
    AddedVertex { id: CompactString },
    /// An edge was added. The auto-id sequence is not restored, see [`UndoEntry::AddedVertex`].
    AddedEdge { id: CompactString },
    /// A vertex was dropped together with `edges`, in removal order.
    DroppedVertex {
        vertex: Box<VertexImage>,
        edges: Vec<EdgeImage>,
    },
    /// An edge was dropped.
    DroppedEdge(Box<EdgeImage>),
    /// A top-level vertex property was written or removed.
    VertexProperty {
        id: CompactString,
        key: CompactString,
        old: OldProperty,
    },
    /// A top-level edge property was written or removed; `map_was_none` restores an edge that had
    /// no property map at all.
    EdgeProperty {
        id: CompactString,
        key: CompactString,
        old: OldProperty,
        map_was_none: bool,
    },
    /// The label set of a vertex changed.
    VertexLabels {
        id: CompactString,
        old: Vec<CompactString>,
    },
    /// The label of an edge changed; `old` is the label before (`None`: unlabeled).
    EdgeLabel {
        id: CompactString,
        old: Option<CompactString>,
    },
    /// The stored schema was replaced.
    Schema(Box<GraphSchema>),
    /// A catalog definition was stored, replaced or removed; `old` is what was there before
    /// (`None`: nothing).
    Definition {
        kind: DefinitionKind,
        name: CompactString,
        old: Option<Box<Definition>>,
    },
}

/// The system clock in microseconds since the Unix epoch (UTC), through `web_time` (never
/// `std::time`, which panics on wasm32). A clock before the epoch reads as 0 and one beyond
/// `i64::MAX` microseconds saturates; the caller makes the result monotonic.
pub(crate) fn now_micros() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        })
}

/// The log position a mark stands for.
pub(crate) fn mark_position(mark: UnitMark) -> usize {
    usize::try_from(mark.raw()).unwrap_or(usize::MAX)
}

/// Puts a property map back to `old` for `key`: removes what the mutation left and re-inserts
/// the old value at its old position.
pub(crate) fn restore_property(properties: &mut PropertyMap, key: &str, old: OldProperty) {
    properties.shift_remove(key);
    if let Some((position, value)) = old {
        let position = position.min(properties.len());
        properties
            .0
            .shift_insert(position, CompactString::from(key), value);
    }
}

impl TraversalGraph {
    /// Opens a unit; the mark is the log position to roll back to.
    pub(crate) fn begin_undo_unit(&mut self) -> UnitMark {
        if self.undo.depth == 0 {
            // The outermost unit takes the starting point of the graph's memory budget, if its
            // policy sets one: the footprint is cached by the commit position, so this is no
            // walk on a graph that has not changed since the last one.
            self.undo.budget =
                crate::traversal::memory_budget::unit_limit(&self.execution_policy, None)
                    .map(|limit| MemoryBudget::new(limit).start(&*self));
            // The unit's time (`defaultFunction` `now`): never earlier than the previous unit's
            // or the last commit's, so a later unit never stores an earlier `now`.
            let started_at = now_micros()
                .max(self.undo.last_started_at)
                .max(self.undo.last_committed_at);
            self.undo.started_at = Some(started_at);
            self.undo.last_started_at = started_at;
        }
        self.undo.depth = self.undo.depth.saturating_add(1);
        UnitMark::new(self.undo.entries.len() as u64)
    }

    /// Closes a unit and keeps its changes (the outermost unit clears the log).
    ///
    /// The outermost commit of a unit that changed something, or that only raised an auto-id
    /// sequence (a sequence-only commit: no mutations, the raised sequences in the
    /// [`ChangeSet`]'s stamp), advances the commit sequence
    /// number (checked: at `u64::MAX` the unit is rolled back and the commit fails with
    /// [`GraphError::SequenceOverflow`]) and takes the commit time, never earlier than the
    /// previous commit's. While hooks are registered it first derives the unit's [`ChangeSet`] and runs
    /// every `before_commit`: the first veto rolls the whole unit back, sends `after_rollback`
    /// to every hook and comes back as [`GraphError::CommitRejected`] (the sequence number stays
    /// where it was); otherwise the unit becomes final and every hook gets `after_commit`.
    /// A panic in a `before_commit` (or while the change set is derived) rolls the unit back,
    /// sends `after_rollback` to every hook (a second panic there is dropped) and then
    /// continues; the sequence number stays where it was. A panic in an `after_commit` comes
    /// after the unit became final: it stays committed and the panic continues.
    ///
    /// Before all that, when the graph's execution policy sets a memory budget (`memory.limit`),
    /// the outermost unit is checked against it (`MemoryBudget::unit_check`): a unit that grew
    /// the memory in use over the budget is rolled back (the hooks get `after_rollback`) and
    /// fails with [`GraphError::ResourceLimitExceeded`] (operation `"commit"`).
    pub(crate) fn commit_undo_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        if self.undo.depth != 1 || (self.undo.entries.is_empty() && !self.undo.sequences_raised) {
            self.undo.close();
            return Ok(());
        }
        if let Some(budget) = self.undo.budget
            && let Err(error) = budget.unit_check(&*self, COMMIT_OPERATION)
        {
            self.rollback_undo_unit(mark, &RollbackReason::failed(&error));
            return Err(error);
        }
        let Some(commit_seq) = self.undo.last_commit_seq.checked_add(1) else {
            // The commit sequence never wraps: the unit cannot get a number, so it is rolled
            // back like a failed one.
            let error = GraphError::SequenceOverflow {
                sequence: crate::error::COMMIT_SEQUENCE,
            };
            self.rollback_undo_unit(mark, &RollbackReason::failed(&error));
            return Err(error);
        };
        let committed_at = now_micros().max(self.undo.last_committed_at);
        if !self.undo.hooks.is_active() {
            self.finish_commit(commit_seq, committed_at);
            return Ok(());
        }
        // Deriving the change set and asking the hooks may panic (a user hook's `before_commit`,
        // or an engine bug): the unit is then rolled back first, so the panic never leaves it
        // open with its changes in effect (the opener's unwind guard only covers its body). A
        // user hook runs before the journal, so a panicking one leaves nothing in the journal; a
        // panicking journal cuts its record off again and poisons itself before the panic gets here.
        let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mutations = self.derive_mutations_since(0);
            if mutations.is_empty() && !self.undo.sequences_raised {
                // Unreachable in practice (every log entry is a change); nothing to report.
                return None;
            }
            let metadata = std::mem::take(&mut self.undo.metadata);
            let stamp = CommitStamp {
                commit_seq,
                committed_at,
                node_sequence: self.node_sequence,
                edge_sequence: self.edge_sequence,
            };
            let change_set = ChangeSet::new(stamp, metadata, mutations);
            let verdict = self.undo.hooks.before_commit(&change_set);
            Some((change_set, verdict))
        }));
        let (change_set, verdict) = match asked {
            Ok(Some(asked)) => asked,
            Ok(None) => {
                self.finish_commit(commit_seq, committed_at);
                return Ok(());
            }
            Err(payload) => {
                self.undo_to(mark);
                self.undo.close();
                let reason = RollbackReason::failed(&HOOK_PANICKED);
                // A second panic (from `after_rollback`) is dropped: the first one continues.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.undo.hooks.after_rollback(&reason);
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
                self.finish_commit(commit_seq, committed_at);
                self.undo.hooks.after_commit(&change_set);
                Ok(())
            }
            Err(error) => {
                self.undo_to(mark);
                self.undo.close();
                self.undo
                    .hooks
                    .after_rollback(&RollbackReason::from_error(&error));
                Err(error)
            }
        }
    }

    /// Makes the outermost unit final under `commit_seq` and `committed_at`.
    fn finish_commit(&mut self, commit_seq: u64, committed_at: i64) {
        self.undo.last_commit_seq = commit_seq;
        self.undo.last_committed_at = committed_at;
        self.undo.close();
    }

    /// Runs `load` as the loading of a graph nobody sees yet (`book/src/users_guide/transactions.md`):
    /// outside units, so nothing is recorded, captured or reported to hooks, also not a
    /// schema change (which otherwise opens a unit of its own).
    pub(crate) fn load_outside_units<R>(&mut self, load: impl FnOnce(&mut Self) -> R) -> R {
        let previous = std::mem::replace(&mut self.undo.loading, true);
        let result = load(self);
        self.undo.loading = previous;
        result
    }

    /// Stores `schema` and records the old one when a unit is open.
    pub(crate) fn set_schema_recorded(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        let old = self
            .undo
            .is_recording()
            .then(|| Box::new(self.schema.read().clone()));
        self.store_schema(schema)?;
        if let Some(old) = old {
            self.undo.record(UndoEntry::Schema(old));
        }
        Ok(())
    }

    /// The commit sequence number of the last unit that changed something.
    pub(crate) fn commit_seq(&self) -> u64 {
        self.undo.last_commit_seq
    }

    /// Undoes every change since `mark`, newest first, and closes the unit. The rollback of an
    /// outermost unit that changed something (also when a savepoint already undid it) sends
    /// `after_rollback(reason)` to every hook.
    pub(crate) fn rollback_undo_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        let notify = self.undo.depth == 1 && self.undo.hooks.is_active() && self.undo.changed;
        let sweep = self.undo.depth == 1 && self.undo.changed;
        self.undo_to(mark);
        self.undo.close();
        // The values the unit wrote are gone: a dedup table drops the entries only it holds.
        if sweep {
            self.compression.sweep_dedup();
        }
        if notify {
            self.undo.hooks.after_rollback(reason);
        }
    }

    /// What the open unit `mark` holds that is still in effect: data and/or schema changes.
    pub(crate) fn undo_unit_changes(&self, mark: UnitMark) -> crate::graph::UnitChanges {
        let mut changes = crate::graph::UnitChanges::NONE;
        if self.undo.depth == 0 {
            return changes;
        }
        for entry in self.undo.entries.iter().skip(mark_position(mark)) {
            match entry {
                UndoEntry::Schema(_) => changes.schema = true,
                UndoEntry::Definition { .. } => changes.definitions = true,
                UndoEntry::AddedVertex { .. }
                | UndoEntry::AddedEdge { .. }
                | UndoEntry::DroppedVertex { .. }
                | UndoEntry::DroppedEdge(_)
                | UndoEntry::VertexProperty { .. }
                | UndoEntry::EdgeProperty { .. }
                | UndoEntry::VertexLabels { .. }
                | UndoEntry::EdgeLabel { .. } => changes.data = true,
            }
            if changes.data && changes.schema && changes.definitions {
                break;
            }
        }
        changes
    }

    /// The start of the outermost open unit (`GraphStorage::unit_started_at`); `None` outside a
    /// unit.
    pub(crate) fn undo_unit_started_at(&self) -> Option<i64> {
        self.undo.started_at
    }

    /// The depth of open units (`0`: none).
    pub(crate) fn undo_depth(&self) -> u32 {
        self.undo.depth
    }

    /// Undoes every change since `mark` and closes the unit WITHOUT telling the hooks: the end
    /// of a dry run, which never was a commit attempt.
    pub(crate) fn discard_undo_unit(&mut self, mark: UnitMark) {
        self.undo_to(mark);
        self.undo.close();
    }

    /// [`Self::discard_undo_unit`] that first derives what the unit changed, as the hooks would
    /// have received it ([`GraphStorage::discard_unit`](crate::graph::GraphStorage::discard_unit)):
    /// never committed (`commit_seq` and commit time `0`, no metadata), the auto-id sequences
    /// after the unit.
    pub(crate) fn discard_undo_unit_with_changes(&mut self, mark: UnitMark) -> ChangeSet {
        let mutations = self.derive_mutations_since(mark_position(mark));
        let stamp = CommitStamp {
            commit_seq: 0,
            committed_at: 0,
            node_sequence: self.node_sequence,
            edge_sequence: self.edge_sequence,
        };
        self.discard_undo_unit(mark);
        ChangeSet::new(stamp, CommitMetadata::default(), mutations)
    }

    /// The metadata of the open unit's commit
    /// ([`GraphStorage::set_unit_metadata`](crate::graph::GraphStorage::set_unit_metadata)):
    /// with only the outermost unit open, `metadata` takes precedence over what nested units
    /// left; inside a nested unit it adds only what is missing (the enclosing unit wins).
    /// Ignored without an open unit.
    pub(crate) fn set_undo_metadata(&mut self, metadata: CommitMetadata) {
        match self.undo.depth {
            0 => {}
            1 => {
                let nested = std::mem::replace(&mut self.undo.metadata, metadata);
                self.undo.metadata.merge_missing(nested);
            }
            _ => self.undo.metadata.merge_missing(metadata),
        }
    }

    /// Undoes every change since `mark`, newest first; the unit stays open.
    fn undo_to(&mut self, mark: UnitMark) {
        let position = mark_position(mark);
        while self.undo.entries.len() > position {
            let Some(entry) = self.undo.entries.pop() else {
                break;
            };
            self.undo_entry(entry);
        }
    }

    /// Applies the inverse of one mutation, without recording anything and without schema
    /// checks (it restores a state the schema already accepted).
    fn undo_entry(&mut self, entry: UndoEntry) {
        match entry {
            UndoEntry::AddedVertex { id } => match self.index.get_node(&self.graph, &id) {
                Some(handle) => {
                    self.remove_vertex_raw(handle, false);
                }
                None => tracing::error!(%id, "undo: added vertex not found"),
            },
            UndoEntry::AddedEdge { id } => match self.index.get_edge(&self.graph, &id) {
                Some(handle) => {
                    self.remove_edge_raw(handle, false);
                }
                None => tracing::error!(%id, "undo: added edge not found"),
            },
            UndoEntry::DroppedVertex { vertex, edges } => {
                let VertexImage {
                    id,
                    labels,
                    properties,
                } = *vertex;
                let handle = self.insert_vertex_raw(id, labels, properties);
                // A restored value is normalized under the rules current now (D10).
                self.compression_normalize_vertex(handle);
                // Re-added in reverse removal order, so the vertex's own adjacency lists come
                // back in their old order.
                for edge in edges.into_iter().rev() {
                    self.restore_edge(edge);
                }
            }
            UndoEntry::DroppedEdge(edge) => self.restore_edge(*edge),
            UndoEntry::VertexProperty { id, key, old } => {
                if let Some(handle) = self.index.get_node(&self.graph, &id) {
                    self.pindex_vertex(handle, false, Some(&key));
                }
                let vertex = self
                    .index
                    .get_node(&self.graph, &id)
                    .and_then(|handle| self.vertex_weight_mut(handle));
                match vertex {
                    Some(vertex) => restore_property(&mut vertex.2, &key, old),
                    None => tracing::error!(%id, "undo: vertex of a property write not found"),
                }
                if let Some(handle) = self.index.get_node(&self.graph, &id) {
                    self.compression_normalize_vertex_key(handle, &key);
                    self.pindex_vertex(handle, true, Some(&key));
                }
            }
            UndoEntry::EdgeProperty {
                id,
                key,
                old,
                map_was_none,
            } => {
                if let Some(handle) = self.index.get_edge(&self.graph, &id) {
                    self.pindex_edge(handle, false, Some(&key));
                }
                let edge = self
                    .index
                    .get_edge(&self.graph, &id)
                    .and_then(|handle| self.edge_weight_mut(handle));
                match edge {
                    Some(edge) => {
                        if map_was_none {
                            edge.2 = None;
                        } else {
                            restore_property(
                                edge.2.get_or_insert_with(PropertyMap::default),
                                &key,
                                old,
                            );
                        }
                    }
                    None => tracing::error!(%id, "undo: edge of a property write not found"),
                }
                if let Some(handle) = self.index.get_edge(&self.graph, &id) {
                    self.compression_normalize_edge_key(handle, &key);
                    self.pindex_edge(handle, true, Some(&key));
                }
            }
            UndoEntry::VertexLabels { id, old } => {
                let Some(handle) = self.index.get_node(&self.graph, &id) else {
                    tracing::error!(%id, "undo: vertex of a label change not found");
                    return;
                };
                self.pindex_vertex(handle, false, None);
                let Some(vertex) = self.vertex_weight_mut(handle) else {
                    tracing::error!(%id, "undo: vertex of a label change not found");
                    return;
                };
                let current = std::mem::replace(&mut vertex.1, old.clone());
                self.statistics.vertex_relabeled(&current, &old);
                for label in current.iter().filter(|l| !old.contains(l)) {
                    self.index.remove_node_label(label, handle);
                }
                for label in old.into_iter().filter(|l| !current.contains(l)) {
                    self.index.insert_node_label(label, handle);
                }
                self.compression_normalize_vertex(handle);
                self.pindex_vertex(handle, true, None);
            }
            UndoEntry::EdgeLabel { id, old } => {
                let Some(handle) = self.index.get_edge(&self.graph, &id) else {
                    tracing::error!(%id, "undo: edge of a label change not found");
                    return;
                };
                self.pindex_edge(handle, false, None);
                let Some(edge) = self.edge_weight_mut(handle) else {
                    tracing::error!(%id, "undo: edge of a label change not found");
                    return;
                };
                let current = std::mem::replace(&mut edge.1, old.clone());
                self.statistics
                    .edge_relabeled(current.as_deref(), old.as_deref());
                if let Some(current) = current {
                    self.index.remove_edge_label(&current, handle);
                }
                if let Some(old) = old {
                    self.index.insert_edge_label(old, handle);
                }
                self.compression_normalize_edge(handle);
                self.pindex_edge(handle, true, None);
            }
            UndoEntry::Schema(old) => *self.schema.write() = *old,
            UndoEntry::Definition { kind, name, old } => {
                let replaced = match old {
                    Some(old) => self.definitions.insert(*old),
                    None => self.definitions.remove(kind, &name),
                };
                // Undoing a rule change re-runs its pass (D10); an index is rebuilt.
                self.catalog_after_change(kind, replaced.as_ref(), &name);
            }
        }
    }

    /// Stores `definition` (validated first; a new one only while the catalog has room) and
    /// records what was there before when a unit is open. Storing an equal definition again
    /// changes nothing and records nothing.
    pub(crate) fn set_definition_recorded(
        &mut self,
        definition: Definition,
    ) -> Result<(), GraphError> {
        definition.validate()?;
        crate::graph::compression::check_conflict(self, &definition)?;
        crate::graph::property_index::check_conflict(self, &definition)?;
        let current = self.definitions.get(definition.kind(), definition.name());
        if current == Some(&definition) {
            return Ok(());
        }
        if current.is_none() && self.definitions.len() >= crate::graph::definition::MAX_DEFINITIONS
        {
            return Err(crate::error::DefinitionError::TooManyDefinitions {
                limit: crate::graph::definition::MAX_DEFINITIONS,
            }
            .into());
        }
        let kind = definition.kind();
        let name = CompactString::from(definition.name());
        let old = self.definitions.insert(definition);
        // The explicit pass of a rule change (D8) or the build of an index: in the same unit,
        // recorded only as the definition change (neither changes logical data).
        self.catalog_after_change(kind, old.as_ref(), &name);
        self.undo.record(UndoEntry::Definition {
            kind,
            name,
            old: old.map(Box::new),
        });
        Ok(())
    }

    /// Removes the definition of `kind` named `name` (`false`: there was none) and records it
    /// when a unit is open.
    pub(crate) fn remove_definition_recorded(&mut self, kind: DefinitionKind, name: &str) -> bool {
        match self.definitions.remove(kind, name) {
            Some(old) => {
                self.catalog_after_change(kind, Some(&old), name);
                self.undo.record(UndoEntry::Definition {
                    kind,
                    name: CompactString::from(name),
                    old: Some(Box::new(old)),
                });
                true
            }
            None => false,
        }
    }

    /// The derived state of a catalog change of `kind` (`old`: what was stored before, the
    /// catalog already holds the new state): a compression rule runs its pass, a property index
    /// is rebuilt from the data.
    pub(crate) fn catalog_after_change(
        &mut self,
        kind: DefinitionKind,
        old: Option<&Definition>,
        name: &str,
    ) {
        if kind == DefinitionKind::COMPRESSION {
            self.compression_after_change(old, name);
        } else if kind == DefinitionKind::PROPERTY_INDEX {
            self.property_index_after_change(name);
        }
    }

    /// Records the current state of the top-level vertex property `key` before a write that
    /// changes it in place (jpath writes and removals). Clones the old value; nothing happens
    /// outside a unit or for a removed vertex.
    pub(crate) fn record_vertex_property(&mut self, node: VertexHandle, key: &str) {
        if !self.undo.is_recording() {
            return;
        }
        let Some(vertex) = self.vertex_weight(node) else {
            return;
        };
        let entry = UndoEntry::VertexProperty {
            id: vertex.0.clone(),
            key: CompactString::from(key),
            old: vertex
                .2
                .0
                .get_full(key)
                .map(|(position, _, value)| (position, value.clone())),
        };
        self.undo.record(entry);
    }

    /// Edge counterpart of [`Self::record_vertex_property`].
    pub(crate) fn record_edge_property(&mut self, edge: EdgeHandle, key: &str) {
        if !self.undo.is_recording() {
            return;
        }
        let Some(edge) = self.edge_weight(edge) else {
            return;
        };
        let entry = UndoEntry::EdgeProperty {
            id: edge.0.clone(),
            key: CompactString::from(key),
            old: edge
                .2
                .as_ref()
                .and_then(|properties| properties.0.get_full(key))
                .map(|(position, _, value)| (position, value.clone())),
            map_was_none: edge.2.is_none(),
        };
        self.undo.record(entry);
    }

    /// Re-inserts a dropped edge between the vertices its image names.
    fn restore_edge(&mut self, edge: EdgeImage) {
        let EdgeImage {
            id,
            label,
            properties,
            source,
            target,
        } = edge;
        match (
            self.index.get_node(&self.graph, &source),
            self.index.get_node(&self.graph, &target),
        ) {
            (Some(from), Some(to)) => {
                let handle = self.insert_edge_raw(id, label, properties, from, to);
                self.compression_normalize_edge(handle);
            }
            _ => tracing::error!(%id, "undo: endpoint of a dropped edge not found"),
        }
    }

    /// Stores a vertex and indexes it: no validation, no sequence change, nothing recorded.
    /// The id must be free.
    pub(crate) fn insert_vertex_raw(
        &mut self,
        id: CompactString,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> VertexHandle {
        // The slot petgraph fills is known only after `add_node`: insert with a placeholder
        // generation and patch it right after (nothing reads in between).
        let index = self.graph.add_node((
            id,
            labels,
            properties,
            crate::graph::handle::FIRST_GENERATION,
        ));
        let generation = self.node_generations.current(index.index());
        let handle = VertexHandle::new(index, generation);
        if let Some(vertex) = self.graph.node_weight_mut(index) {
            vertex.3 = generation;
            // Indexed after the generation patch, so the entry's handle matches its slot (the
            // id index keeps no copy of the id, see `GraphIndex`).
            self.index.insert_node(&vertex.0, handle);
            self.statistics.vertex_added(&vertex.1);
            for label in &vertex.1 {
                self.index.insert_node_label(label.clone(), handle);
            }
        }
        self.pindex_vertex(handle, true, None);
        handle
    }

    /// Stores an edge between two live vertices and indexes it: no validation, no sequence
    /// change, nothing recorded. The id must be free.
    pub(crate) fn insert_edge_raw(
        &mut self,
        id: CompactString,
        label: Option<CompactString>,
        properties: Option<PropertyMap>,
        from: VertexHandle,
        to: VertexHandle,
    ) -> EdgeHandle {
        let mut generations = EdgeGenerations {
            own: crate::graph::handle::FIRST_GENERATION,
            source: from.generation(),
            target: to.generation(),
        };
        let edge_index = self.graph.add_edge(
            from.index(),
            to.index(),
            (id, label.clone(), properties, generations),
        );
        // See `insert_vertex_raw`: the slot is known only now.
        generations.own = self.edge_generations.current(edge_index.index());
        let handle = EdgeHandle::new(edge_index, generations.own);
        if let Some(edge) = self.graph.edge_weight_mut(edge_index) {
            edge.3 = generations;
            self.index.insert_edge(&edge.0, handle);
        }
        self.statistics.edge_added(label.as_deref());
        if let Some(label) = label {
            self.index.insert_edge_label(label, handle);
        }
        self.pindex_edge(handle, true, None);
        handle
    }

    /// Removes a live edge and its index entries and retires its slot generation. With
    /// `capture`, returns its image (endpoints by string id).
    pub(crate) fn remove_edge_raw(&mut self, edge: EdgeHandle, capture: bool) -> Option<EdgeImage> {
        let index = self.resolve_edge(edge)?;
        self.remove_edge_at(index, edge, capture)
    }

    fn remove_edge_at(
        &mut self,
        index: EdgeIndex,
        edge: EdgeHandle,
        capture: bool,
    ) -> Option<EdgeImage> {
        let endpoints = if capture {
            self.graph.edge_endpoints(index).and_then(|(from, to)| {
                let source = self.graph.node_weight(from)?.0.clone();
                let target = self.graph.node_weight(to)?.0.clone();
                Some((source, target))
            })
        } else {
            None
        };
        self.pindex_edge(edge, false, None);
        let (id, label, properties, _) = self.graph.remove_edge(index)?;
        self.statistics.edge_removed(label.as_deref());
        if let Some(label) = &label {
            self.index.remove_edge_label(label, edge);
        }
        self.index.remove_edge(&id, edge);
        self.edge_generations.bump(index.index());
        let (source, target) = endpoints?;
        Some(EdgeImage {
            id,
            label,
            properties,
            source,
            target,
        })
    }

    /// Removes a live vertex, its incident edges and all their index entries, and retires the
    /// slot generations. With `capture`, returns the [`UndoEntry::DroppedVertex`] image.
    pub(crate) fn remove_vertex_raw(
        &mut self,
        node: VertexHandle,
        capture: bool,
    ) -> Option<UndoEntry> {
        let index = self.resolve_vertex(node)?;
        // The incident edges, each once: a self-loop shows up in both directions and is taken
        // from the outgoing side only. Removed one by one (not by `StableGraph::remove_node`), so
        // their index entries go too and their images can be captured. Left behind, freed edge
        // slots would be reused by later edges and stale ids would resolve to a different edge.
        let mut incident: Vec<EdgeHandle> = Vec::new();
        for direction in [Direction::Outgoing, Direction::Incoming] {
            for edge in self.graph.edges_directed(index, direction) {
                if direction == Direction::Incoming && edge.source() == index {
                    continue;
                }
                incident.push(EdgeHandle::new(edge.id(), edge.weight().3.own));
            }
        }
        let mut edges = Vec::new();
        if capture {
            edges.reserve_exact(incident.len());
        }
        for edge in incident {
            if let Some(image) = self.remove_edge_at(edge.index(), edge, capture) {
                edges.push(image);
            }
        }
        self.pindex_vertex(node, false, None);
        let (id, labels, properties, _) = self.graph.remove_node(index)?;
        self.statistics.vertex_removed(&labels);
        self.index.remove_node(&id, node);
        for label in &labels {
            self.index.remove_node_label(label, node);
        }
        self.node_generations.bump(index.index());
        capture.then(|| UndoEntry::DroppedVertex {
            vertex: Box::new(VertexImage {
                id,
                labels,
                properties,
            }),
            edges,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphStorage;

    #[test]
    fn nothing_is_recorded_outside_a_unit() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let v =
            GraphStorage::insert_vertex(&mut graph, None, vec!["x".into()], PropertyMap::default())
                .expect("add");
        GraphStorage::set_vertex_property(&mut graph, v, "k".into(), 1.into()).expect("set");
        assert_eq!(graph.undo.len(), 0);
    }

    #[test]
    fn outermost_commit_clears_and_nested_commit_keeps() {
        let mut graph = TraversalGraph::new();
        let outer = graph.begin_unit();
        GraphStorage::insert_vertex(&mut graph, None, vec![], PropertyMap::default()).expect("add");
        let inner = graph.begin_unit();
        GraphStorage::insert_vertex(&mut graph, None, vec![], PropertyMap::default()).expect("add");
        graph.commit_unit(inner).expect("commit");
        assert_eq!(graph.undo.len(), 2);
        graph.commit_unit(outer).expect("commit");
        assert_eq!(graph.undo.len(), 0);
        assert!(!graph.undo.is_recording());
        assert_eq!(graph.vertex_count(), 2);
    }

    #[test]
    fn restore_property_puts_the_key_back_at_its_position() {
        let mut props = PropertyMap::default();
        props.insert("a", 1.into());
        props.insert("b", 2.into());
        props.insert("c", 3.into());
        let (position, _, value) = props.0.shift_remove_full("b").expect("b");
        restore_property(&mut props, "b", Some((position, value)));
        let keys: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["a", "b", "c"]);
        props.insert("d", 4.into());
        restore_property(&mut props, "d", None);
        assert_eq!(props.len(), 3);
    }
}
