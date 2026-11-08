use crate::graph::schema::ElementKind;
use crate::graph::{GraphStorage, PropertyIndexInfo, PropertyIndexRange, PropertyIndexTransform};
use crate::traversal::optimizer::OptimizerContext;
use crate::traversal::optimizer::plan::Plan;
use crate::traversal::optimizer::rule::OptimizationRule;
use crate::traversal::step::{Answered, GraphStep, IndexAccess};

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
/// records it on the source ([`IndexAccess`]). The source still checks its folded conditions on
/// each candidate and the `has(key, P)` steps stay in the plan, so the result equals the scan's
/// (only the order of the elements may differ), with ONE exception (user decision 2026-11-08,
/// book page "Property Indexes", section "Kept exact"): when the picked condition is answered
/// exactly by the index ([`exact_shape`]) in a plan that does not write, on a storage claiming
/// `StorageCapabilities::exact_property_index_lookups` (`OptimizerContext::exact_lookups`), its
/// re-check is dropped: a folded `has(key, value)` is skipped on the candidates
/// ([`Answered::Folded`]), a filter step is removed from the plan ([`Answered::Step`]); every
/// other condition, the source's label check of a label-less index's candidates included, is
/// still checked. The index is then trusted: a wrong entry shows in the result instead of
/// being hidden by a re-check. When nothing but that condition is left and the slice needs no
/// label check, the access is `countable`, so `count_pushdown` can fuse a following `count()`
/// answered from the postings (design section 8). Without indexes the rule does nothing.
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

/// THE definition of a proven-exact index access (book page "Property Indexes", section "Kept
/// exact"): whether the elements of the slices `ranges` of an index of `transform` are exactly
/// the elements the condition served by it accepts, each once, so no re-check can drop one.
///
/// - every slice is a point (`[v, v]`: an equality, or one operand of `P.within`) of a string,
///   boolean, uuid or temporal value ([`exact_value`]): a number slice spans `int64` and
///   `float64` (a superset), `null` and ranges are left to the re-check;
/// - the index has no transform (the condition is on the whole value: a folded `has(key,
///   value)`, `has(key, P.eq/within)`), or exactly `each` serving `has(key, __.unfold().is(P))`
///   (an element is in the postings of each of its values once, D15; the source deduplicates
///   several slices; an object, nested array or vector `unfold()` emits never equals a scalar,
///   and the index skips exactly those);
/// - or `to_lower` serving `has(key, __.to_lower().is(P))`, or `["each", "to_lower"]` serving
///   `has(key, __.unfold().to_lower().is(P))` (user decision 2026-11-08): the rule reads such
///   an index only while it skipped no value (`usable`), and nothing writes during a reading
///   run, so every value at the key is a string (or a list of strings), `to_lower()` cannot
///   fail, and the lowered keys answer the condition exactly (a `null` constant is never
///   served: `index_condition`);
/// - NOT `each` serving a whole-value condition (`superset`: arrays holding the value are
///   candidates).
///
/// The label side (which index, a label check of a label-less index's candidates) is not part
/// of it: the source checks labels either way.
fn exact_shape(
    transform: Option<&PropertyIndexTransform>,
    superset: bool,
    ranges: &[PropertyIndexRange],
) -> bool {
    use std::ops::Bound::Included;
    let transform_ok = match transform {
        None => true,
        Some(transform) => !superset && transform.is_known(),
    };
    transform_ok
        && !ranges.is_empty()
        && ranges
            .iter()
            .all(|range| match (&range.lower, &range.upper) {
                (Included(a), Included(b)) => a == b && exact_value(a),
                _ => false,
            })
}

/// Where the condition an access serves comes from.
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    /// The folded `has(key, value)` of the source.
    Folded,
    /// The filter step at this plan position.
    Step(usize),
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
        // A source whose answered condition left the plan keeps its access: the condition is
        // only on the access now (a plan is optimized once; this keeps a second pass sound).
        if let GraphStep::Init(_, init) = &plan[0]
            && init
                .index_access()
                .is_some_and(|access| matches!(access.answered, Some(Answered::Step(_))))
        {
            return false;
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
        let mut best: Option<(u64, IndexAccess, Option<Origin>)> = None;
        let mut consider = |key: &str,
                            transform: Option<PropertyIndexTransform>,
                            ranges: Vec<PropertyIndexRange>,
                            origin: Origin| {
            // A condition on the whole value is served exactly by an index without a
            // transform, else by an `each` index as a superset (its candidates' arrays fail
            // the re-check of the whole value).
            let (transform, found, superset) = match cost(key, transform.as_ref(), &ranges) {
                None if transform.is_none() => {
                    let each = Some(PropertyIndexTransform::Each);
                    let found = cost(key, each.as_ref(), &ranges);
                    (each, found, true)
                }
                found => (transform, found, false),
            };
            if let Some((cost, label_less)) = found
                && best.as_ref().is_none_or(|(best, _, _)| cost < *best)
            {
                let answered = (context.exact_lookups
                    && exact_shape(transform.as_ref(), superset, &ranges))
                .then_some(origin);
                // Count from the postings: one slice, nothing else to check (no other
                // folded condition, no label check).
                let countable = answered.is_some()
                    && ranges.len() == 1
                    && filters == usize::from(origin == Origin::Folded)
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
                        answered: None,
                        countable,
                    },
                    answered,
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
                        Origin::Folded,
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
                        consider(
                            condition.key,
                            condition.transform,
                            condition.ranges,
                            Origin::Step(index),
                        );
                    }
                }
                _ => break,
            }
        }
        match best {
            Some((_, mut access, answered)) => {
                access.answered = match answered {
                    None => None,
                    Some(Origin::Folded) => Some(Answered::Folded),
                    Some(Origin::Step(index)) => {
                        // The step's work moves into the source: it answers it exactly.
                        let rendered = plan[index].get_name();
                        plan.absorb(index, 0);
                        Some(Answered::Step(rendered))
                    }
                };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{PropertyIndexDefinition, TraversalGraph};
    use crate::traversal::optimizer::rules::filter_pushdown::SourceFilterPushdownRule;

    /// The plan of `steps` after `source_filter_pushdown` and this rule, with or without
    /// `exact_lookups`: the rendered step names.
    fn planned(
        graph: &TraversalGraph,
        steps: Vec<GraphStep<'_, &TraversalGraph>>,
        exact_lookups: bool,
    ) -> Vec<String> {
        let indexes = graph.property_index_infos();
        let mut plan = Plan::from_steps(steps, true);
        SourceFilterPushdownRule::new().apply(&mut plan);
        let context = OptimizerContext {
            indexes: &indexes,
            exact_lookups,
        };
        assert!(PropertyIndexRule::new().apply_in(&mut plan, &context));
        plan.into_steps().iter().map(GraphStep::get_name).collect()
    }

    #[test]
    fn the_recheck_is_dropped_only_with_exact_lookups() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        graph
            .traversal_mut()
            .create_index(
                "name_any",
                PropertyIndexDefinition::label_less(ElementKind::Vertex, "name"),
            )
            .unwrap();
        graph
            .traversal_mut()
            .create_index(
                "tags_each",
                PropertyIndexDefinition::label_less(ElementKind::Vertex, "tags")
                    .with_transform(Some(PropertyIndexTransform::Each)),
            )
            .unwrap();
        let folded = |graph: &TraversalGraph, exact| {
            planned(
                graph,
                graph.traversal().v(None).has("name", "josh").take_steps(),
                exact,
            )
        };
        // A storage without the capability (or a plan that writes): every re-check stays.
        assert_eq!(
            folded(&graph, false),
            [r#"v().has("name", "josh") [index: vertex name, any label]"#]
        );
        assert_eq!(
            folded(&graph, true),
            [r#"v().has("name", "josh") [index: vertex name, any label, exact]"#]
        );
        let each = |graph: &TraversalGraph, exact| {
            planned(
                graph,
                graph
                    .traversal()
                    .v(None)
                    .has_traversal("tags", crate::__::unfold().is("x"))
                    .take_steps(),
                exact,
            )
        };
        assert_eq!(each(&graph, false).len(), 2, "the has() step stays");
        assert_eq!(
            each(&graph, true),
            [
                r#"v().has("tags", __.unfold().is(P.eq("x"))) [index: vertex tags each, any label, exact]"#
            ]
        );
    }

    #[test]
    fn exact_shapes_are_points_of_exact_values() {
        use crate::ElementProperty as V;
        use std::ops::Bound::{Excluded, Included, Unbounded};
        let point = |value: V| PropertyIndexRange::equal(value);
        let each = PropertyIndexTransform::Each;
        assert!(exact_shape(None, false, &[point("x".into())]));
        assert!(exact_shape(
            None,
            false,
            &[point("x".into()), point(true.into()), point(V::Uuid(7))]
        ));
        assert!(exact_shape(Some(&each), false, &[point("x".into())]));
        // A superset, a transform that can fail, numbers, null, ranges, nothing.
        assert!(exact_shape(
            Some(&PropertyIndexTransform::ToLower),
            false,
            &[point("x".into())]
        ));
        let chain = PropertyIndexTransform::from_names(["each", "to_lower"]).unwrap();
        assert!(exact_shape(Some(&chain), false, &[point("x".into())]));
        assert!(!exact_shape(
            Some(&PropertyIndexTransform::Unknown("upper".into())),
            false,
            &[point("x".into())]
        ));
        assert!(!exact_shape(Some(&each), true, &[point("x".into())]));
        assert!(!exact_shape(None, false, &[point(1.into())]));
        assert!(!exact_shape(
            None,
            false,
            &[point("x".into()), point(1.into())]
        ));
        assert!(!exact_shape(None, false, &[point(V::Null)]));
        assert!(!exact_shape(
            None,
            false,
            &[PropertyIndexRange::new(Included("a".into()), Unbounded)]
        ));
        assert!(!exact_shape(
            None,
            false,
            &[PropertyIndexRange::new(
                Included("a".into()),
                Excluded("a".into())
            )]
        ));
        assert!(!exact_shape(None, false, &[]));
    }
}
