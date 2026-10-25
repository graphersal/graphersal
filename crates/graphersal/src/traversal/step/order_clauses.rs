//! The sort-key clauses of `order()`, shared by the stream-wide step (`order()`, a
//! `FlatBarrier`) and the per-traverser one (`order(Scope.local)`, a `Map`), so both build, resolve
//! and compare their keys with one implementation.

use crate::error::TraverserResult;
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::{
    AnonymousTraversal, By, ByModulation, ByModulatorCheck, Order, Traverser, TraverserContext,
};
use crate::value::{ElementProperty, TraverserValue, compare_element_property};

/// One sort-key clause: an optional key selector (`None` selects the value itself) and the
/// direction to sort it by.
type OrderClause<'g, S> = (Option<By<'g, S>>, Order);

/// `order()`'s ordered list of sort-key clauses: `clauses[0]` is the primary key, each subsequent
/// entry a tiebreaker applied only when every earlier clause compares equal. An empty list means
/// natural order (the value itself, ascending).
///
/// Multiple `.by()` calls accumulate into this list rather than overwriting each other.
/// [`push_by`](Self::push_by) distinguishes two cases for each incoming `.by()` call:
/// - A bare direction (`By::Order`) updates the *direction of the current (most recent) clause*
///   in place. This is what lets `.by("age", Order.desc)` — expanded into
///   `.by("age").by(Order.desc)` at the Rhai/Rust builder layer — compose into a single clause
///   with key `"age"` and direction `desc`, rather than becoming two separate clauses.
/// - Any other `By` (a property key, `By::Label`, `By::Id`, a `Column`, or a nested traversal)
///   always starts a new clause, appended with an implicit ascending direction (subject to being
///   overridden by a following bare-direction call, per the rule above).
pub(crate) struct OrderClauses<'g, S: GraphStorage + 'g> {
    clauses: Vec<OrderClause<'g, S>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Default for OrderClauses<'g, S> {
    fn default() -> Self {
        Self {
            clauses: Vec::new(),
            by_check: ByModulatorCheck::default(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> Clone for OrderClauses<'g, S> {
    fn clone(&self) -> Self {
        Self {
            clauses: self.clauses.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> OrderClauses<'g, S> {
    /// The properties the clauses sort by (`by(key)`).
    pub(crate) fn property_keys(&self) -> impl Iterator<Item = &str> {
        self.clauses
            .iter()
            .filter_map(|(by, _)| by.as_ref().and_then(By::property_key))
    }

    /// Writes the clauses as `.by(key, direction)` suffixes, for the step's profile name.
    pub(crate) fn fmt_clauses(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (by, order) in &self.clauses {
            match by {
                Some(by) if *order == Order::Asc => write!(f, ".by({by})")?,
                Some(by) => write!(f, ".by({by}, {order})")?,
                None => write!(f, ".by({order})")?,
            }
        }
        Ok(())
    }

    /// Merges a `.by()` modulator into the clause list (see [`OrderClauses`] for the merge
    /// rules). `by(Column::Keys | Column::Values)` is accepted as a sort key.
    pub(crate) fn push_by(&mut self, by: By<'g, S>) -> ByModulation {
        self.by_check.observe_sort_key(&by);
        match by {
            By::Order(order) => match self.clauses.last_mut() {
                Some((_, existing_order)) => *existing_order = order,
                None => self.clauses.push((None, order)),
            },
            other => self.clauses.push((Some(other), Order::Asc)),
        }
        ByModulation::Accepted
    }

    /// Fails with `InvalidModulator` for `step` when a `by()` it received was rejected.
    #[inline]
    pub(crate) fn check(&self, step: &str) -> TraverserResult<()> {
        self.by_check.check(step)
    }

    /// The sort keys of `traverser`'s value, one per clause (one natural key when there is no
    /// clause), or `None` when a clause is unproductive (a missing property, a child traversal
    /// yielding nothing, a `Column` on a non-map): TinkerPop 3.6+ then drops the value. A
    /// `by(__...)` child sorts by its first result and runs on `traverser`, path included.
    pub(crate) fn keys(
        &self,
        context: &mut TraverserContext<'_, S>,
        traverser: &Traverser<S>,
    ) -> TraverserResult<Option<Vec<ElementProperty>>> {
        let mut keys = Vec::with_capacity(self.clauses.len().max(1));
        if self.clauses.is_empty() {
            keys.push(traverser.value.into_node_property(context.storage)?);
            return Ok(Some(keys));
        }
        for (by, _) in &self.clauses {
            let key = match by {
                Some(by) => {
                    match TraverserValue::fetch_traverser_value_by(
                        context,
                        traverser,
                        &traverser.value,
                        by,
                    )? {
                        Some(value) => value.into_node_property(context.storage)?,
                        None => return Ok(None),
                    }
                }
                None => traverser.value.into_node_property(context.storage)?,
            };
            keys.push(key);
        }
        Ok(Some(keys))
    }

    /// Sorts `keyed` (keys from [`keys`](Self::keys)) by the clauses. Never `sort_unstable_by`:
    /// TinkerPop's `order()` is a stable sort, so items whose keys compare equal on every clause
    /// keep their relative input order.
    ///
    /// `Order.shuffle` follows TinkerPop's `MultiComparator`: when a clause shuffles, the items
    /// are first shuffled with the execution's random number generator (`random.seed` makes it
    /// reproducible), then stably sorted by the clauses *after* the last shuffling one; every
    /// clause before it, and its own key, only decide (through [`keys`](Self::keys)) which items
    /// are dropped as unproductive.
    pub(crate) fn sort<T>(
        &self,
        keyed: &mut [(Vec<ElementProperty>, T)],
        context: &mut TraverserContext<'_, S>,
    ) {
        let natural = [Order::Asc];
        let mut orders: Vec<&Order> = if self.clauses.is_empty() {
            natural.iter().collect()
        } else {
            self.clauses.iter().map(|(_, order)| order).collect()
        };
        // Index of the first clause that still sorts, after the last shuffling one.
        let start = match orders.iter().rposition(|order| **order == Order::Shuffle) {
            Some(last_shuffle) => {
                use rand::seq::SliceRandom;
                keyed.shuffle(context.rng());
                last_shuffle + 1
            }
            None => 0,
        };
        if start >= orders.len() {
            return;
        }
        orders.drain(..start);
        keyed.sort_by(|(a_keys, _), (b_keys, _)| {
            for ((a, b), order) in a_keys[start..]
                .iter()
                .zip(b_keys[start..].iter())
                .zip(orders.iter())
            {
                let cmp = compare_element_property(a, b);
                let cmp = match order {
                    Order::Asc => cmp,
                    Order::Desc => cmp.reverse(),
                    // Only clauses after the last shuffle reach the comparator.
                    Order::Shuffle => std::cmp::Ordering::Equal,
                };
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            std::cmp::Ordering::Equal
        });
    }

    /// Every `by(__...)` child traversal, in clause order.
    pub(crate) fn traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.clauses
            .iter_mut()
            .filter_map(|(by, _)| by.as_mut().and_then(By::traversal_mut))
            .collect()
    }

    /// What the `by(__...)` children read from the path of the traverser they run on.
    pub(crate) fn analyze_paths(&mut self, analysis: &PathAnalysis) -> PathRequirement {
        let mut requirement = PathRequirement::None;
        for (by, _) in &mut self.clauses {
            if let Some(by) = by {
                requirement = requirement.union(by.analyze_paths(analysis));
            }
        }
        requirement
    }
}
