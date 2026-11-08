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
/// Indexes") instead of its label buckets or of every element. It runs after
/// `source_filter_pushdown`, on a source with no ids, and looks at two kinds of conditions on a
/// literal property name:
///
/// - the `has(key, value)` conditions folded into the source (scalar values only), and
/// - the `has(key, P)` steps in the run of pure filters right after it (`eq`, `within`, the
///   ranges; see `step_has_p::index_ranges`), at the root and in a child level alike (a
///   string operand is a value, never a path label).
/// - the `has(key, __.to_lower().is(P))` steps in that run (TinkerPop's `has(key, traversal)`
///   with exactly that child, `P` as above), served by an index with the transform `to_lower`
///   on the key, read with `P`'s constants as given (the child compares the lowered value with
///   them). Only while that index skipped no value (`PropertyIndexStats::skipped` 0): the
///   `to_lower()` of the child fails on a non-string value, and a scan must report it.
/// - the `has(key, __.unfold().is(P))` steps in that run ("arrays containing", design section
///   4), served by an index with the transform `each` (whatever it skipped: a nested array, an
///   object or a vector that `unfold()` emits never equals nor ranges with a scalar), and
///   `has(key, __.unfold().to_lower().is(P))` by `["each", "to_lower"]` while it skipped
///   nothing. Any other `has(key, traversal)` ends the run of pure filters.
/// - a condition on the whole value (a folded `has(key, value)`, `has(key, P)`) with no index
///   of no transform on the key: an `each` index serves it as a SUPERSET (an array holding the
///   value is a candidate; the source's re-check of the whole value drops it), never `exact`.
///
/// Which indexes can serve a condition on `key` (design D1/D2):
///
/// - a source WITHOUT labels: only the label-less index on `key` (every element of the kind);
/// - a source with folded labels: the per-label indexes on `key` (every label must have one;
///   their estimates are summed), or the label-less index on `key`, whose candidates the source
///   then checks for its labels. The lower estimate wins (a tie: the per-label indexes, which
///   need no label check).
///
/// Among the conditions an index covers it picks the most selective by the indexes' stats
/// (equality: entries / distinct values per index and value; a range: half the entries) and
/// records it on the source ([`IndexAccess`]). Nothing else changes: the source still checks its
/// folded conditions on each candidate and the `has(key, P)` step stays in the plan, so the
/// result equals the scan's (only the order of the elements may differ). When no re-check can
/// drop a candidate the access is marked `exact`, so `count_pushdown` can fuse a following
/// `count()` answered from the index (design section 8). Without indexes the rule does nothing.
/// `g.with("optimizer.disabled", ["property_index"])` turns it off.
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

/// Whether an index slice of this equality holds exactly the elements `has(key, value)` finds
/// (a number is ranged by its `float64` value, a superset; `null` is left to the scan).
fn exact_value(value: &crate::ElementProperty) -> bool {
    use crate::ElementProperty as P;
    match value {
        P::String(_) | P::Compressed(_) | P::Deduped(_) | P::Boolean(_) | P::Uuid(_) => true,
        P::Temporal(_) => true,
        P::Null
        | P::Number(_)
        | P::Vector(_)
        | P::Blob(_)
        | P::Object(_)
        | P::Array(_)
        | P::Vertex(_)
        | P::Edge(_)
        | P::Map(_) => false,
    }
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
        // `labels`: `None` = a source without labels (the label-less indexes only).
        let (element, labels, has_access, servable) = match &plan[0] {
            GraphStep::Init(name, init) if *name == "v" || *name == "e" => {
                let element = if *name == "v" {
                    ElementKind::Vertex
                } else {
                    ElementKind::Edge
                };
                let servable = init.ids().is_none()
                    && !init.is_count_only()
                    && !init.is_group_count_by_label()
                    // An empty label list matches nothing: no index read.
                    && init.labels().is_none_or(|labels| !labels.is_empty());
                (
                    element,
                    init.labels().map(<[String]>::to_vec),
                    init.index_access().is_some(),
                    servable,
                )
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
        if !servable || context.indexes.is_empty() {
            return clear(plan);
        }
        // A transformed index serves only while it skipped no value (see the rule's doc).
        let usable = |info: &PropertyIndexInfo,
                      label: Option<&str>,
                      key: &str,
                      transform: Option<&PropertyIndexTransform>| {
            info.element == element
                && info.label.as_deref() == label
                && info.key == key
                && info.transform.as_ref() == transform
                && (transform.is_none_or(|transform| !transform.can_fail())
                    || info.stats.skipped == 0)
        };
        // The cheapest way to serve `key` with `ranges`: `(cost, label_less)`, `None` when no
        // index serves it.
        let cost = |key: &str,
                    transform: Option<&PropertyIndexTransform>,
                    ranges: &[PropertyIndexRange]|
         -> Option<(u64, bool)> {
            let label_less = context
                .indexes
                .iter()
                .find(|info| usable(info, None, key, transform))
                .map(|info| estimate(info, ranges));
            let per_label = labels.as_ref().and_then(|labels| {
                labels.iter().try_fold(0u64, |total, label| {
                    let info = context
                        .indexes
                        .iter()
                        .find(|info| usable(info, Some(label), key, transform))?;
                    Some(total.saturating_add(estimate(info, ranges)))
                })
            });
            match (per_label, label_less) {
                (Some(per_label), Some(any)) if any < per_label => Some((any, true)),
                (Some(per_label), _) => Some((per_label, false)),
                (None, Some(any)) => Some((any, true)),
                (None, None) => None,
            }
        };
        let filters = match &plan[0] {
            GraphStep::Init(_, init) => init.property_filters().len(),
            _ => 0,
        };
        let single_label = labels.as_ref().is_some_and(|labels| labels.len() == 1);
        let mut best: Option<(u64, IndexAccess)> = None;
        let mut consider =
            |key: &str,
             transform: Option<PropertyIndexTransform>,
             ranges: Vec<PropertyIndexRange>,
             folded_value: Option<&crate::ElementProperty>| {
                // A condition on the whole value is served exactly by an index without a
                // transform, else by an `each` index as a superset (its candidates' arrays fail
                // the re-check of the whole value).
                let (transform, found) = match cost(key, transform.as_ref(), &ranges) {
                    None if transform.is_none() => {
                        let each = Some(PropertyIndexTransform::Each);
                        let found = cost(key, each.as_ref(), &ranges);
                        (each, found)
                    }
                    found => (transform, found),
                };
                if let Some((cost, label_less)) = found
                    && best.as_ref().is_none_or(|(best, _)| cost < *best)
                {
                    // Count from the postings: one folded equality, nothing else to re-check.
                    let exact = folded_value.is_some_and(exact_value)
                        && filters == 1
                        && transform.is_none()
                        && if label_less {
                            labels.is_none()
                        } else {
                            single_label
                        };
                    best = Some((
                        cost,
                        IndexAccess {
                            key: key.to_string(),
                            transform,
                            ranges,
                            label_less,
                            exact,
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
                        Some(&filter.value),
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
                        consider(condition.key, condition.transform, condition.ranges, None);
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
