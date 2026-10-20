use crate::graph::schema::ElementKind;
use crate::graph::{GraphStorage, PropertyIndexInfo, PropertyIndexRange, PropertyIndexTransform};
use crate::traversal::optimizer::OptimizerContext;
use crate::traversal::optimizer::plan::Plan;
use crate::traversal::optimizer::rule::OptimizationRule;
use crate::traversal::step::{GraphStep, IndexAccess};

/// The filter steps a `has(key, P)` served by an index may follow directly after the source:
/// pure filters on the element itself (no child traversal, no side effect), so reading fewer
/// candidates cannot change what any of them observes.
const PURE_FILTERS: [&str; 7] = [
    "has",
    "has_p",
    "has_label",
    "has_id",
    "has_not",
    "has_key",
    "has_property",
];

/// Optimizer rule that lets a `v()`/`e()` source read a property index (book page "Property
/// Indexes") instead of its label buckets. It runs after `source_filter_pushdown`, on a source
/// with folded labels (every one of them must have an index on the key) and no ids, and looks
/// at two kinds of conditions on a literal property name:
///
/// - the `has(key, value)` conditions folded into the source (scalar values only), and
/// - the `has(key, P)` steps in the run of pure filters right after it (`eq`, `within`, the
///   ranges; see `step_has_p::index_ranges`), at the root and in a child level alike (a
///   string operand is a value, never a path label).
/// - the `has(key, __.to_lower().is(P))` steps in that run (TinkerPop's `has(key, traversal)`
///   with exactly that child, `P` as above), served by an index with the transform `to_lower`
///   on the key, read with `P`'s constants as given (the child compares the lowered value with
///   them). Only while that index skipped no value (`PropertyIndexStats::skipped` 0): the
///   `to_lower()` of the child fails on a non-string value, and a scan must report it. Any
///   other `has(key, traversal)` ends the run of pure filters.
///
/// Among the conditions an index covers it picks the most selective by the indexes' stats
/// (equality: entries / distinct values per label and value; a range: half the entries) and
/// records it on the source ([`IndexAccess`]). Nothing else changes: the source still checks its
/// folded conditions on each candidate and the `has(key, P)` step stays in the plan, so the
/// result equals the scan's (only the order of the elements may differ). Without indexes the
/// rule does nothing. `g.with("optimizer.disabled", ["property_index"])` turns it off.
#[derive(Default)]
pub(crate) struct PropertyIndexRule;

impl PropertyIndexRule {
    pub fn new() -> Self {
        Self
    }
}

/// The estimated number of candidates `ranges` read from `info`.
fn estimate(info: &PropertyIndexInfo, ranges: &[PropertyIndexRange]) -> u64 {
    let entries = info.stats.entries;
    let per_value = entries / info.stats.distinct_values.max(1);
    ranges
        .iter()
        .map(|range| match (&range.lower, &range.upper) {
            (std::ops::Bound::Included(a), std::ops::Bound::Included(b)) if a == b => {
                per_value.max(1)
            }
            _ => (entries / 2).max(1),
        })
        .fold(0u64, u64::saturating_add)
}

impl<'g, S: GraphStorage + 'g> OptimizationRule<'g, S> for PropertyIndexRule {
    fn name(&self) -> &'static str {
        "property_index"
    }

    fn apply(&self, _plan: &mut Plan<'g, S>) -> bool {
        // Without the indexes of the storage there is nothing to pick from.
        false
    }

    fn apply_in(&self, plan: &mut Plan<'g, S>, context: &OptimizerContext<'_>) -> bool {
        if plan.is_empty() {
            return false;
        }
        let (element, labels, has_access) = match &plan[0] {
            GraphStep::Init(name, init) if *name == "v" || *name == "e" => {
                let element = if *name == "v" {
                    ElementKind::Vertex
                } else {
                    ElementKind::Edge
                };
                let labels = match init.labels() {
                    Some(labels)
                        if !labels.is_empty()
                            && init.ids().is_none()
                            && !init.is_count_only()
                            && !init.is_group_count_by_label() =>
                    {
                        Some(labels.to_vec())
                    }
                    _ => None,
                };
                (element, labels, init.index_access().is_some())
            }
            _ => return false,
        };
        let clear = |plan: &mut Plan<'g, S>| {
            if has_access && let GraphStep::Init(_, init) = plan.step_mut(0) {
                init.set_index_access(None);
                return true;
            }
            false
        };
        let Some(labels) = labels else {
            return clear(plan);
        };
        if context.indexes.is_empty() {
            return clear(plan);
        }
        // The cost of serving `key` with `ranges`: `None` unless every label has an index on it.
        // A transformed index serves only while it skipped no value (see the rule's doc).
        let cost = |key: &str,
                    transform: Option<&PropertyIndexTransform>,
                    ranges: &[PropertyIndexRange]|
         -> Option<u64> {
            labels.iter().try_fold(0u64, |total, label| {
                let info = context.indexes.iter().find(|info| {
                    info.element == element
                        && info.label == *label
                        && info.key == key
                        && info.transform.as_ref() == transform
                        && (transform.is_none() || info.stats.skipped == 0)
                })?;
                Some(total.saturating_add(estimate(info, ranges)))
            })
        };
        let mut best: Option<(u64, IndexAccess)> = None;
        let mut consider = |key: &str,
                            transform: Option<PropertyIndexTransform>,
                            ranges: Vec<PropertyIndexRange>| {
            if let Some(cost) = cost(key, transform.as_ref(), &ranges)
                && best.as_ref().is_none_or(|(best, _)| cost < *best)
            {
                best = Some((
                    cost,
                    IndexAccess {
                        key: key.to_string(),
                        transform,
                        ranges,
                    },
                ));
            }
        };
        if let GraphStep::Init(_, init) = &plan[0] {
            for filter in init.property_filters() {
                if crate::graph::property_index::is_indexable(&filter.value) {
                    consider(
                        &filter.key,
                        None,
                        vec![PropertyIndexRange::equal(filter.value.clone())],
                    );
                }
            }
        }
        for index in 1..plan.len() {
            match &plan[index] {
                // `has(key, traversal)` is pure only in the served shape (`index_condition`).
                GraphStep::Filter(name, filter)
                    if PURE_FILTERS.contains(name)
                        || (*name == "has_traversal" && filter.index_condition().is_some()) =>
                {
                    if let Some(condition) = filter.index_condition() {
                        consider(condition.key, condition.transform, condition.ranges);
                    }
                }
                _ => break,
            }
        }
        match best {
            Some((_, access)) => {
                if let GraphStep::Init(_, init) = plan.step_mut(0) {
                    init.set_index_access(Some(access))
                } else {
                    false
                }
            }
            None => clear(plan),
        }
    }
}
