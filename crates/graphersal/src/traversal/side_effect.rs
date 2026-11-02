//! The value of one named side effect (`aggregate`/`store`/`tree`/`subgraph`/`withSideEffect`).
//!
//! A side effect holds **stream values**, not snapshots: `aggregate("a")` over vertices stores
//! the vertex handles themselves, so `cap("a").unfold()` yields vertices that `dedup()`,
//! `where()` and `out()` treat by identity, as TinkerPop does. Materialization into
//! `ElementProperty` happens only at a terminal step.

use crate::graph::Graph;
use crate::graph::GraphStorage;
use crate::value::membership::{MemberIndex, MemberIndexCell};
use crate::*;
use fnv::FnvBuildHasher;
use indexmap::{IndexMap, IndexSet};
use std::sync::Arc;

/// One side-effect bucket.
///
/// Kept exhaustive on purpose (CLAUDE.md rule 15): a new bucket kind must update every reader.
pub(crate) enum SideEffectValue<S: GraphStorage> {
    /// A single value: a `withSideEffect` seed, a `tree()` result, a `subgraph()` schema.
    Value(TraverserValue<S>),
    /// An `aggregate()`/`store()` bucket: every collected value, duplicates kept.
    ///
    /// The list only ever grows at its end (`push_side_effect_n`); the [`MemberIndexCell`] is the
    /// `within`/`without` index over it, extended lazily by the next test. Code that changed the
    /// list in any other way would have to reset the cell.
    List(Vec<TraverserValue<S>>, MemberIndexCell),
    /// A bucket declared as a set (`withSideEffect("a", Set.of())`): every value is kept once, in
    /// first-seen order. Bulk is invisible: a traverser of bulk `n` inserts its value once.
    /// Append-only like [`Self::List`], with the same index.
    Set(IndexSet<TraverserValue<S>, FnvBuildHasher>, MemberIndexCell),
    /// A bucket declared with an [`Operator`] (`withSideEffect("a", 0, Operator.sum)`): `aggregate()`
    /// and `store()` combine into `value` with `op` instead of collecting a list.
    ///
    /// `pending` holds the values one `aggregate()` step execution has collected so far (bulk
    /// expanded); the step folds them into `value` once per batch (see
    /// `step_aggregate::flush_reduced`), because `assign` and `addAll` need the whole batch as
    /// one collection. It is empty whenever a reader can see the bucket.
    Reduced {
        /// The reducer, `Operator.sum` in `withSideEffect("a", 0, Operator.sum)`.
        op: Operator,
        /// The accumulated value, starting at the seed.
        value: ElementProperty,
        /// Values collected by the running step execution, not yet folded into `value`.
        pending: Vec<ElementProperty>,
    },
    /// A `subgraph("a")` bucket: the edges selected so far, as lazy handles (deduplicated, so bulk
    /// and repeated visits are invisible). Nothing is copied while the stream runs; `cap("a")`
    /// builds the snapshot graph from the handles ([`Self::read`]).
    ///
    /// `target` is the graph of `withSideEffect("a", graph)`: with one, the step flushes the
    /// handles into it at the end of every step execution (and `edges` is then drained), without
    /// one `cap` builds a new [`TraversalGraph`](crate::TraversalGraph).
    Subgraph {
        /// The selected edges, in first-seen order.
        edges: IndexSet<S::EdgeId, FnvBuildHasher>,
        /// The explicit target of `withSideEffect(name, graph)`, if any.
        target: Option<Arc<Graph>>,
    },
    /// A `group("a")`/`groupCount("a")` bucket: the map under construction, one slot per key.
    /// Readers see it as the finished map ([`GroupBucket::to_map`]).
    Group(GroupBucket<S>),
}

/// The map a `group("a")`/`groupCount("a")` side effect builds, in first-seen key order.
///
/// Each key holds a [`GroupSlot`] whose kind follows the value `by()` of the step that created
/// it. The slots are updated per traverser, except [`GroupSlot::Members`], whose value traversal
/// holds a barrier and so is re-run over all members of a key at the end of each step execution
/// that added one (`finish_batch`).
pub(crate) struct GroupBucket<S: GraphStorage> {
    pub(crate) slots: IndexMap<TraverserValue<S>, GroupSlot<S>, FnvBuildHasher>,
}

impl<S: GraphStorage> Default for GroupBucket<S> {
    fn default() -> Self {
        Self {
            slots: IndexMap::default(),
        }
    }
}

impl<S: GraphStorage> Clone for GroupBucket<S> {
    fn clone(&self) -> Self {
        Self {
            slots: self.slots.clone(),
        }
    }
}

/// The value of one key of a [`GroupBucket`].
///
/// Kept exhaustive on purpose (CLAUDE.md rule 15).
pub(crate) enum GroupSlot<S: GraphStorage> {
    /// `groupCount("a")`, or a value `by()` that counts: the bulk-weighted member count. Never
    /// above `i64::MAX` (the step fails with `BulkOverflow` first).
    Count(u64),
    /// A projecting value `by()` (none, a property key, `T.id`, `T.label`, ...): one value per
    /// unit of bulk, TinkerPop's `map(projection).fold()`.
    List(Vec<TraverserValue<S>>),
    /// A value traversal without a barrier: the first result of the last member that produced
    /// one (TinkerPop's `Operator.assign`).
    Assigned(TraverserValue<S>),
    /// A value traversal with a barrier: the members of the key, and the traversal's first
    /// result over all of them (`None` when it produced nothing: the key is then left out).
    Members {
        /// Every member traverser seen so far, path and sack included.
        members: Vec<Traverser<S>>,
        /// The value traversal's result over `members`, as of the last step execution.
        reduced: Option<TraverserValue<S>>,
        /// Whether `members` grew since `reduced` was computed.
        stale: bool,
    },
}

impl<S: GraphStorage> Clone for GroupSlot<S> {
    fn clone(&self) -> Self {
        match self {
            GroupSlot::Count(n) => GroupSlot::Count(*n),
            GroupSlot::List(items) => GroupSlot::List(items.clone()),
            GroupSlot::Assigned(value) => GroupSlot::Assigned(value.clone()),
            GroupSlot::Members {
                members,
                reduced,
                stale,
            } => GroupSlot::Members {
                members: members.clone(),
                reduced: reduced.clone(),
                stale: *stale,
            },
        }
    }
}

impl<S: GraphStorage> GroupBucket<S> {
    /// The finished map as readers (`cap`, `select`, `within`) see it.
    pub(crate) fn to_map(&self) -> TraverserValue<S> {
        let mut map: StreamMap<S> = StreamMap::with_capacity(self.slots.len());
        for (key, slot) in &self.slots {
            let value = match slot {
                GroupSlot::Count(n) => TraverserValue::Computed(
                    ElementProperty::Number(i64::try_from(*n).unwrap_or(i64::MAX).into()),
                    None,
                ),
                GroupSlot::List(items) => TraverserValue::Array(items.clone()),
                GroupSlot::Assigned(value) => value.clone(),
                GroupSlot::Members { reduced, .. } => match reduced {
                    Some(value) => value.clone(),
                    None => continue,
                },
            };
            map.insert(key.clone(), value);
        }
        TraverserValue::Object(map)
    }
}

impl<S: GraphStorage> Clone for SideEffectValue<S> {
    fn clone(&self) -> Self {
        match self {
            SideEffectValue::Value(value) => SideEffectValue::Value(value.clone()),
            SideEffectValue::List(items, index) => {
                SideEffectValue::List(items.clone(), index.clone())
            }
            SideEffectValue::Set(items, index) => {
                SideEffectValue::Set(items.clone(), index.clone())
            }
            SideEffectValue::Reduced { op, value, pending } => SideEffectValue::Reduced {
                op: *op,
                value: value.clone(),
                pending: pending.clone(),
            },
            SideEffectValue::Subgraph { edges, target } => SideEffectValue::Subgraph {
                edges: edges.clone(),
                target: target.clone(),
            },
            SideEffectValue::Group(bucket) => SideEffectValue::Group(bucket.clone()),
        }
    }
}

impl<S: GraphStorage> std::fmt::Debug for SideEffectValue<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SideEffectValue::Value(value) => f.debug_tuple("Value").field(value).finish(),
            SideEffectValue::List(items, _) => f.debug_tuple("List").field(items).finish(),
            SideEffectValue::Set(items, _) => f.debug_tuple("Set").field(items).finish(),
            SideEffectValue::Reduced { op, value, .. } => f
                .debug_struct("Reduced")
                .field("op", op)
                .field("value", value)
                .finish(),
            SideEffectValue::Subgraph { edges, target } => f
                .debug_struct("Subgraph")
                .field("edges", &edges.len())
                .field("explicit_target", &target.is_some())
                .finish(),
            SideEffectValue::Group(bucket) => f
                .debug_struct("Group")
                .field("keys", &bucket.slots.len())
                .finish(),
        }
    }
}

impl<S: GraphStorage> SideEffectValue<S> {
    /// The seed of `withSideEffect(name, value)`: an array seed is a list that `aggregate`
    /// keeps appending to, any other value is a single value.
    pub(crate) fn seed(value: ElementProperty) -> Self {
        match TraverserValue::from(value) {
            TraverserValue::Array(items) => SideEffectValue::list(items),
            other => SideEffectValue::Value(other),
        }
    }

    /// What `cap`/`select`/`math` read: the value itself, or the bucket as a list.
    pub(crate) fn to_traverser_value(&self) -> TraverserValue<S> {
        match self {
            SideEffectValue::Value(value) => value.clone(),
            SideEffectValue::List(items, _) => TraverserValue::Array(items.clone()),
            SideEffectValue::Set(items, _) => {
                TraverserValue::Array(items.iter().cloned().collect())
            }
            SideEffectValue::Reduced { value, .. } => TraverserValue::from(value.clone()),
            // Readers without storage access (`within(..)`, `math`) see the selected edges; the
            // graph itself comes from `read`.
            SideEffectValue::Subgraph { edges, .. } => TraverserValue::Array(
                edges
                    .iter()
                    .map(|edge| TraverserValue::Edge(*edge))
                    .collect(),
            ),
            SideEffectValue::Group(bucket) => bucket.to_map(),
        }
    }

    /// What `cap`/`select` return: [`Self::to_traverser_value`], except that a `subgraph()`
    /// bucket becomes the snapshot graph ([`TraverserValue::Graph`]).
    ///
    /// Without an explicit target a new [`TraversalGraph`](crate::TraversalGraph) is built from the
    /// collected edges and the schema stored by `storage` (ids, labels and properties preserved);
    /// with one the graph already holds the edges (flushed by the step) and is returned as is.
    pub(crate) fn read(&self, storage: &S) -> TraverserResult<TraverserValue<S>> {
        match self {
            SideEffectValue::Subgraph {
                edges,
                target: Some(target),
            } => {
                let _ = edges;
                Ok(TraverserValue::Graph(target.clone()))
            }
            SideEffectValue::Subgraph {
                edges,
                target: None,
            } => {
                let mut graph = TraversalGraph::new();
                crate::graph::subgraph::copy_edges_into(
                    storage,
                    edges.iter().copied(),
                    &mut graph,
                    true,
                )?;
                Ok(TraverserValue::Graph(Arc::new(Graph::new(graph))))
            }
            other => Ok(other.to_traverser_value()),
        }
    }

    /// The seed of `withSideEffect(name, value, Operator)`: a reduced bucket, even for an array seed.
    pub(crate) fn seed_reduced(value: ElementProperty, op: Operator) -> Self {
        SideEffectValue::Reduced {
            op,
            value,
            pending: Vec::new(),
        }
    }

    /// The seed of `withSideEffect(name, Set.of(..))`: duplicates collapse, first-seen order kept.
    pub(crate) fn seed_set(items: impl IntoIterator<Item = ElementProperty>) -> Self {
        SideEffectValue::Set(
            items.into_iter().map(TraverserValue::from).collect(),
            MemberIndexCell::default(),
        )
    }

    /// An `aggregate()`/`store()` list bucket holding `items`.
    pub(crate) fn list(items: Vec<TraverserValue<S>>) -> Self {
        SideEffectValue::List(items, MemberIndexCell::default())
    }

    /// The members of an append-only collection bucket (a list or a set) long enough for a
    /// `within` index, with the bucket's index cell; `None` for every other bucket (scanned).
    pub(crate) fn indexable_members(
        &mut self,
    ) -> Option<(IndexableMembers<'_, S>, &mut MemberIndexCell)> {
        match self {
            SideEffectValue::List(items, index) if MemberIndex::covers(items.len()) => {
                Some((IndexableMembers::List(items), index))
            }
            SideEffectValue::Set(items, index) if MemberIndex::covers(items.len()) => {
                Some((IndexableMembers::Set(items), index))
            }
            SideEffectValue::Value(_)
            | SideEffectValue::List(..)
            | SideEffectValue::Set(..)
            | SideEffectValue::Reduced { .. }
            | SideEffectValue::Subgraph { .. }
            | SideEffectValue::Group(_) => None,
        }
    }

    /// The members a `within`/`without` test ranges over: the list or set items, or the one value.
    pub(crate) fn members(&self) -> Members<'_, S> {
        match self {
            SideEffectValue::Value(value) => Members::One(std::iter::once(value)),
            SideEffectValue::List(items, _) => Members::Slice(items.iter()),
            SideEffectValue::Set(items, _) => Members::Set(items.iter()),
            SideEffectValue::Subgraph { edges, .. } => Members::Owned(
                edges
                    .iter()
                    .map(|edge| TraverserValue::Edge(*edge))
                    .collect::<Vec<_>>()
                    .into_iter(),
            ),
            SideEffectValue::Reduced { value, .. } => match value {
                ElementProperty::Array(items) => Members::Owned(
                    items
                        .iter()
                        .cloned()
                        .map(TraverserValue::from)
                        .collect::<Vec<_>>()
                        .into_iter(),
                ),
                other => Members::Owned(vec![TraverserValue::from(other.clone())].into_iter()),
            },
            SideEffectValue::Group(bucket) => Members::Owned(vec![bucket.to_map()].into_iter()),
        }
    }
}

/// The members of a list or set bucket by position ([`SideEffectValue::indexable_members`]).
pub(crate) enum IndexableMembers<'a, S: GraphStorage> {
    List(&'a [TraverserValue<S>]),
    Set(&'a IndexSet<TraverserValue<S>, FnvBuildHasher>),
}

impl<'a, S: GraphStorage> IndexableMembers<'a, S> {
    /// The number of members.
    pub(crate) fn len(&self) -> usize {
        match self {
            IndexableMembers::List(items) => items.len(),
            IndexableMembers::Set(items) => items.len(),
        }
    }

    /// The member at `position`.
    pub(crate) fn get(&self, position: usize) -> Option<&'a TraverserValue<S>> {
        match self {
            IndexableMembers::List(items) => items.get(position),
            IndexableMembers::Set(items) => items.get_index(position),
        }
    }
}

/// Allocation-free iterator over [`SideEffectValue::members`].
pub(crate) enum Members<'a, S: GraphStorage> {
    One(std::iter::Once<&'a TraverserValue<S>>),
    Slice(std::slice::Iter<'a, TraverserValue<S>>),
    Set(indexmap::set::Iter<'a, TraverserValue<S>>),
    /// A reduced bucket builds its members on demand (it stores an `ElementProperty`).
    Owned(std::vec::IntoIter<TraverserValue<S>>),
}

impl<'a, S: GraphStorage> Iterator for Members<'a, S> {
    type Item = std::borrow::Cow<'a, TraverserValue<S>>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Members::One(it) => it.next().map(std::borrow::Cow::Borrowed),
            Members::Slice(it) => it.next().map(std::borrow::Cow::Borrowed),
            Members::Set(it) => it.next().map(std::borrow::Cow::Borrowed),
            Members::Owned(it) => it.next().map(std::borrow::Cow::Owned),
        }
    }
}
