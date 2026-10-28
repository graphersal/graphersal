use crate::graph::GraphStorage;
use crate::*;
use fnv::FnvBuildHasher;
use indexmap::IndexMap;
use indexmap::map::Entry;

/// `InvalidModulator` reasons specific to `group()`/`group_count()`; `help()` keys on them.
pub(crate) mod reason {
    /// A sort direction, not a projection: `group()`'s `by()` picks a key or a value, it does not
    /// order one.
    pub(crate) const ORDER: &str = "an order is not a projection this step can use";
    /// A second `by()` on `group_count()`: it counts per key, so it has no value projection.
    pub(crate) const TOO_MANY: &str = "group_count() takes a single by()";
}

/// Resolves the grouping key of one traverser.
///
/// A lazy property handle (`VertexProperty`/`EdgeProperty`, e.g. after `values("lang")`) is keyed
/// by the value it points at, not by the handle: the handle carries the element it was read from,
/// so two elements holding the same value would otherwise form two groups that then collide when
/// the result is rendered as a map (`java: 1` instead of `java: 2`). Group is a materializing
/// barrier, and a property value is cloned once per traverser only here.
pub(crate) fn group_key<'g, S: GraphStorage + 'g>(
    context: &mut TraverserContext<'_, S>,
    traverser: &Traverser<S>,
    traverser_value: &TraverserValue<S>,
    by: &By<'g, S>,
) -> TraverserResult<Option<TraverserValue<S>>> {
    let key = TraverserValue::fetch_traverser_value_by(context, traverser, traverser_value, by)?;
    Ok(match key {
        Some(
            handle @ (TraverserValue::VertexProperty(_, _, _)
            | TraverserValue::EdgeProperty(_, _, _)),
        ) => Some(handle.into_node_property(context.storage)?.into()),
        other => other,
    })
}

pub(crate) fn bulk_overflow(step: &str) -> Box<TraverserError> {
    Box::new(TraverserError::BulkOverflow {
        step: step.to_string(),
    })
}

/// Appends `value` to `bucket` `n` times (one per unit of the traverser's bulk). The caller has
/// already checked the step's total against `bulk.max_expansion`.
pub(crate) fn push_n<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    bucket: &mut Vec<TraverserValue<S>>,
    value: TraverserValue<S>,
    n: u64,
    step: &str,
) -> TraverserResult<()> {
    if n > 1 {
        bulk::ExpansionLimits::of(&context.options).check_copies(&value, n, step)?;
    }
    bulk::push_copies(bucket, value, n, &mut context.deadline)
}

/// `group()` / `group_count()`. `group_count()` takes a single `by()`; a second one is
/// `InvalidModulator` (user's guide: "The by() Modulator: Single-by Steps"). `group()` itself takes
/// the key `by()` and the value `by()`.
struct StepGroup<'g, S: GraphStorage + 'g> {
    by: Option<Vec<By<'g, S>>>,
    group_count: bool,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Default for StepGroup<'g, S> {
    fn default() -> Self {
        Self {
            by: None,
            group_count: false,
            by_check: ByModulatorCheck::default(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> Clone for StepGroup<'g, S> {
    fn clone(&self) -> Self {
        Self {
            by: self.by.clone(),
            group_count: self.group_count,
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGroup<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by = "".to_string();
        if let Some(by_list) = self.by.as_ref() {
            by_list
                .iter()
                .for_each(|b| by.push_str(&crate::traversal::render_by(b)));
        }
        let name = if self.group_count {
            "group_count"
        } else {
            "group"
        };
        write!(f, "{}(){}", name, by)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Groups the stream into a map: key by the first by(), values by the second by() (default: the
    /// elements).
    pub fn group(self) -> Self {
        self.push_step(GraphStep::Barrier("group", Box::new(StepGroup::default())))
    }

    /// Counts the stream per key (the first by(), default the element) into a map.
    pub fn group_count(self) -> Self {
        self.push_step(GraphStep::Barrier(
            "group_count",
            Box::new(StepGroup {
                group_count: true,
                ..Default::default()
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Groups the stream into a map: key by the first by(), values by the second by() (default: the
    /// elements).
    pub fn group(self) -> Self {
        let step = GraphStep::Barrier("group", Box::new(StepGroup::default()));
        self.push_step(step)
    }

    /// Counts the stream per key (the first by(), default the element) into a map.
    pub fn group_count(self) -> Self {
        let step = GraphStep::Barrier(
            "group_count",
            Box::new(StepGroup {
                group_count: true,
                ..Default::default()
            }),
        );
        self.push_step(step)
    }
}

/// Whether a value traversal holds a barrier at its top level, TinkerPop's
/// `Grouping.determineBarrierStep()`: an aggregating step (`fold()`, `count()`, `sum()`,
/// `group()`, `cap()`, ...) or a collecting/filtering barrier (`order()`, `dedup()`,
/// `limit()`/`range()`/`skip()`/`tail()`). `repeat()` and the list filters `all()`/`any()`/`none()`
/// are `FlatBarrier`s here but not barriers in TinkerPop.
pub(crate) fn has_barrier<'g, S: GraphStorage + 'g>(traversal: &AnonymousTraversal<'g, S>) -> bool {
    traversal.steps.iter().any(|step| match step {
        GraphStep::Barrier(_, _) | GraphStep::Count(_, _) => true,
        GraphStep::FlatBarrier(name, _) => {
            matches!(
                *name,
                "dedup" | "order" | "range" | "limit" | "skip" | "tail"
            )
        }
        GraphStep::Init(_, _)
        | GraphStep::Append(_, _)
        | GraphStep::Map(_, _)
        | GraphStep::FlatMap(_, _)
        | GraphStep::Filter(_, _)
        | GraphStep::SideEffect(_, _)
        | GraphStep::SideEffectMut(_, _)
        | GraphStep::Modulator(_, _)
        | GraphStep::As(_, _) => false,
    })
}

fn is_count_traversal<'g, S: GraphStorage + 'g>(traversal: &AnonymousTraversal<'g, S>) -> bool {
    traversal.steps.len() == 1 && matches!(traversal.steps.first(), Some(GraphStep::Count(_, _)))
}

/// Whether a value traversal is the bare identity, the lowering of a bare `by()`: TinkerPop's
/// `GroupStep.convertValueTraversal` turns it into `map(identity).fold()`, so the key collects
/// its members as a list instead of keeping the last one.
fn is_identity_traversal<'g, S: GraphStorage + 'g>(traversal: &AnonymousTraversal<'g, S>) -> bool {
    traversal.steps.len() == 1
        && matches!(traversal.steps.first(), Some(GraphStep::Map("identity", _)))
}

/// How a group step reduces the members of one key, decided by its value `by()`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GroupValueKind {
    /// `group_count()`, `by(Count)`, `by(__.count())`: the bulk-weighted member count.
    Count,
    /// A projection (no `by()`, a property key, `T.id`, `T.label`, a bare `by()`, ...): a list of
    /// one projected value per unit of bulk.
    List,
    /// A value traversal without a barrier: the first result of the last productive member.
    Assign,
    /// A value traversal with a barrier: run once over all members of a key.
    Barrier,
}

/// The [`GroupValueKind`] of a value `by()`.
pub(crate) fn group_value_kind<'g, S: GraphStorage + 'g>(
    group_count: bool,
    value_by: &By<'g, S>,
) -> GroupValueKind {
    if group_count {
        return GroupValueKind::Count;
    }
    match value_by {
        By::Count => GroupValueKind::Count,
        By::Traversal(t) if is_count_traversal(t) => GroupValueKind::Count,
        By::Traversal(t) if is_identity_traversal(t) => GroupValueKind::List,
        By::Traversal(t) if !has_barrier(t) => GroupValueKind::Assign,
        By::Traversal(_) => GroupValueKind::Barrier,
        By::Property(_)
        | By::Order(_)
        | By::Label
        | By::Id
        | By::Value
        | By::Identity
        | By::Key
        | By::JsonPath(_)
        | By::Labels(_)
        | By::Column(_) => GroupValueKind::List,
    }
}

/// The key `by()` and the value `by()` of a group step: the first and second `by()`, defaulting
/// to the incoming value and to a list of members (a count for `group_count()`).
pub(crate) fn group_bys<'a, 'g, S: GraphStorage + 'g>(
    by: Option<&'a [By<'g, S>]>,
    group_count: bool,
) -> (&'a By<'g, S>, &'a By<'g, S>) {
    let default_value: &'a By<'g, S> = if group_count {
        &By::Count
    } else {
        &By::Identity
    };
    match by {
        Some(list) => (
            list.first().unwrap_or(&By::Identity),
            list.get(1).unwrap_or(default_value),
        ),
        None => (&By::Identity, default_value),
    }
}

/// Rejects a value `by()` that is not a projection a [`GroupValueKind::List`] key can collect
/// (an order, a vertex-label list, a column). `by_check` already fails the step for an order and
/// a column; this keeps the value path total, and fails even when no traverser arrives.
pub(crate) fn check_value_by<'g, S: GraphStorage + 'g>(
    value_by: &By<'g, S>,
    step: &str,
) -> TraverserResult<()> {
    let reason = match value_by {
        By::Order(_) => reason::ORDER,
        By::Labels(_) => "a vertex-label list is not a projection this step can use",
        By::Column(_) => crate::traversal::by_reason::COLUMN,
        By::Property(_)
        | By::Traversal(_)
        | By::Label
        | By::Id
        | By::Value
        | By::Identity
        | By::Key
        | By::Count
        | By::JsonPath(_) => return Ok(()),
    };
    Err(Box::new(TraverserError::InvalidModulator {
        step: step.to_string(),
        position: 2,
        modulator: crate::traversal::by_reason::MODULATOR,
        reason: reason.to_string(),
    }))
}

/// The value one member adds to a [`GroupValueKind::List`] key, or `None` when the projection
/// is unproductive for it (the key still exists then: TinkerPop runs the projection as
/// `map(projection).fold()`). The incoming value itself (no `by()`, `By::Identity`, a bare `by()`)
/// is the caller's business, so it can move the value instead of cloning it.
pub(crate) fn group_list_value<'g, S: GraphStorage + 'g>(
    context: &mut TraverserContext<'_, S>,
    traverser: &Traverser<S>,
    value_by: &By<'g, S>,
) -> TraverserResult<Option<TraverserValue<S>>> {
    Ok(match value_by {
        By::Label => TraverserValue::fetch_label(context, &traverser.value)?.map(|label| {
            TraverserValue::Computed(ElementProperty::String(label.as_ref().into()), None)
        }),
        By::Id => TraverserValue::fetch_id(context, &traverser.value)?
            .map(|id| TraverserValue::Computed(ElementProperty::String(id.as_ref().into()), None)),
        By::Property(property) => {
            TraverserValue::fetch_property(context, &traverser.value, property)?
                .map(|prop| TraverserValue::Computed(prop, None))
        }
        By::Identity | By::Traversal(_) => Some(traverser.value.clone()),
        By::JsonPath(_)
        | By::Value
        | By::Key
        | By::Count
        | By::Order(_)
        | By::Labels(_)
        | By::Column(_) => TraverserValue::fetch_traverser_value_by(
            context,
            traverser,
            &traverser.value,
            value_by,
        )?,
    })
}

/// Whether a [`GroupValueKind::List`] value `by()` keeps the incoming value as it is.
pub(crate) fn takes_incoming_value<'g, S: GraphStorage + 'g>(value_by: &By<'g, S>) -> bool {
    match value_by {
        By::Identity | By::Traversal(_) => true,
        By::Property(_)
        | By::Order(_)
        | By::Label
        | By::Id
        | By::Value
        | By::Key
        | By::Count
        | By::JsonPath(_)
        | By::Labels(_)
        | By::Column(_) => false,
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepGroup<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.by.iter().flatten().filter_map(By::property_key) {
            visit(Some(key), crate::graph::workload::AccessKind::OrderGroup);
        }
    }
    /// Builds an aggregation/materialization buffer (D3 of the memory-inspection spec) — reported
    /// to `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    /// TinkerPop's `group()`/`groupCount()` are seeded reducing barriers: an empty stream reduces
    /// to `{}`, not nothing (confirmed against TinkerPop 3.7's `GroupStep`/`GroupCountStep`, whose
    /// seed is an empty map).
    fn always_emits(&self) -> bool {
        true
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        let group_name = if self.group_count {
            "group_count"
        } else {
            "group"
        };
        self.by_check.check(group_name)?;
        let (by, second_by) = group_bys(self.by.as_deref(), self.group_count);
        check_value_by(second_by, group_name)?;
        let kind = group_value_kind(self.group_count, second_by);

        let mut result: StreamMap<S> = StreamMap::default();

        match kind {
            GroupValueKind::Count => {
                // A merged traverser counts `bulk` times (checked, never wraps).
                let mut tmp: IndexMap<TraverserValue<S>, u64, FnvBuildHasher> = IndexMap::default();
                for source_item in source {
                    context.tick()?;
                    if let Some(key) = group_key(context, &source_item, &source_item.value, by)? {
                        let slot = tmp.entry(key).or_insert(0);
                        *slot = slot
                            .checked_add(source_item.bulk)
                            .ok_or_else(|| bulk_overflow(group_name))?;
                    }
                }
                for (k, count) in tmp {
                    result.insert(
                        k,
                        TraverserValue::Computed(bulk::count_property(count, group_name)?, None),
                    );
                }
            }
            GroupValueKind::List => {
                // One value per unit of bulk; the expansion of the whole step is bounded by
                // `bulk.max_expansion`.
                if context.bulk_active {
                    let total = bulk::total_bulk(context, &source, group_name)?;
                    let limit = context.options.bulk_max_expansion;
                    if total > limit {
                        return Err(Box::new(TraverserError::BulkExpansionLimit {
                            step: "group".to_string(),
                            requested: total,
                            limit,
                        }));
                    }
                }
                let incoming = takes_incoming_value(second_by);
                let mut tmp: IndexMap<TraverserValue<S>, Vec<TraverserValue<S>>, FnvBuildHasher> =
                    IndexMap::default();
                for source_item in source {
                    context.tick()?;
                    if let Some(key) = group_key(context, &source_item, &source_item.value, by)? {
                        let n = source_item.bulk;
                        let value = if incoming {
                            Some(source_item.value)
                        } else {
                            group_list_value(context, &source_item, second_by)?
                        };
                        // The key exists once its `by()` is productive, whatever the value
                        // `by()` yields: a key none of whose members has the property maps to
                        // `[]`.
                        let bucket = tmp.entry(key).or_default();
                        if let Some(value) = value {
                            push_n(context, bucket, value, n, group_name)?;
                        }
                    }
                }
                for (k, list) in tmp {
                    result.insert(k, TraverserValue::Array(list));
                }
            }
            // TinkerPop's `GroupStep`: a value traversal without a barrier runs per member and
            // keeps one value per key, the first result of the last member that produced one
            // (`Operator.assign`); a key no member produced a value for is left out.
            GroupValueKind::Assign => {
                let By::Traversal(child) = second_by else {
                    return Err(Box::new(TraverserError::wrong_query(
                        "group(): an assigning value by() must be a traversal",
                    )));
                };
                for source_item in source {
                    context.tick()?;
                    if let Some(key) = group_key(context, &source_item, &source_item.value, by)?
                        && let Some(first) = child.first(context, &source_item)?
                    {
                        result.insert(key, first.value);
                    }
                }
            }
            // With a barrier (`fold()`, `count()`, `sum()`, `order()`, ...) the value traversal
            // runs once over all members of a key, and the key maps to its first result.
            GroupValueKind::Barrier => {
                let By::Traversal(child) = second_by else {
                    return Err(Box::new(TraverserError::wrong_query(
                        "group(): a reducing value by() must be a traversal",
                    )));
                };
                let mut tmp: IndexMap<TraverserValue<S>, Vec<Traverser<S>>, FnvBuildHasher> =
                    IndexMap::default();
                for source_item in source {
                    if let Some(key) = group_key(context, &source_item, &source_item.value, by)? {
                        match tmp.entry(key) {
                            Entry::Occupied(mut e) => e.get_mut().push(source_item),
                            Entry::Vacant(e) => {
                                e.insert(vec![source_item]);
                            }
                        }
                    }
                }
                for (k, v) in tmp {
                    // batch: bulk preserved (the members' bulks reach the value traversal's
                    // `count()`/`sum()`/...).
                    let x = child.to_list(&mut *context, v)?;
                    if let Some(first) = x.into_iter().next() {
                        result.insert(k, first.value);
                    }
                }
            }
        }

        Ok(Some(Traverser::start(
            context,
            TraverserValue::Object(result),
        )))
    }
    fn is_group_by_label_count(&self) -> bool {
        if self.by_check.has_rejected() {
            return false;
        }
        let binding = &self.by;
        let first_by = match binding.as_ref().and_then(|b| b.first()) {
            Some(b) => b,
            None => return false,
        };
        if !matches!(first_by, By::Label) {
            return false;
        }

        if self.group_count {
            return true;
        }

        match binding.as_ref().and_then(|b| b.get(1)) {
            Some(By::Count) => true,
            Some(By::Traversal(t)) => is_count_traversal(t),
            _ => false,
        }
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe(&args);
        if matches!(args, By::Order(_)) {
            self.by_check.reject_last(reason::ORDER);
        }
        if self.group_count && self.by.is_some() {
            self.by_check.reject_last(reason::TOO_MANY);
        }
        if let Some(x) = self.by.as_mut() {
            x.push(args);
        } else {
            self.by = Some(vec![args]);
        }
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by
            .iter_mut()
            .flatten()
            .filter_map(By::traversal_mut)
            .collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // `by(__...)` children run on the incoming traverser, path included.
        match &mut self.by {
            Some(by_list) => By::analyze_all_paths(by_list, analysis),
            None => crate::traversal::optimizer::path_analysis::PathRequirement::None,
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::By::*;

    /// TinkerPop 3.7's `group()`/`groupCount()` are seeded reducing barriers (`GroupStep`/
    /// `GroupCountStep`, seed: an empty map): an empty input stream still reduces to `{}`, not
    /// nothing (confirmed via `graphersal`: `g.V().hasLabel("nope").group()` → `#{}`). A fused
    /// `count()` must then read `1`.
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_group_reduces_to_an_empty_map() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").group().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").group()"#,
                r#"g.v().has_label("nope").group()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Object(map) => assert!(map.is_empty()),
                    other => panic!("expected an empty map, got {other:?}"),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn empty_input_group_count_is_one() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").group().count().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").group().count()"#,
                r#"g.v().has_label("nope").group().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn empty_input_group_count_step_reduces_to_an_empty_map() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").group_count().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").groupCount()"#,
                r#"g.v().has_label("nope").group_count()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Object(map) => assert!(map.is_empty()),
                    other => panic!("expected an empty map, got {other:?}"),
                }
            },
        );
    }

    /// The known gap this task fixes: a value traversal with a barrier (`by(__.values("age")
    /// .fold())`) must keep a key whose members are all unproductive for that traversal, holding
    /// the barrier's own empty seed (`[]`) — TinkerPop keeps the key; dropping it was the bug.
    /// `software` vertices have no `age`, so `values("age")` produces nothing for them and
    /// `fold()` (fixed by this task) seeds `[]` for that key instead of the key vanishing.
    #[cfg(feature = "script")]
    #[test]
    fn v_by_label_by_values_fold_keeps_the_unproductive_key_as_an_empty_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).by(__::values("age").fold()).to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(__.values("age").fold())"#,
                r#"g.v().group().by(By::Label).by(__.values("age").fold())"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        let persons = result.get("person").unwrap().as_array().unwrap();
                        assert_eq!(persons.len(), 4);
                        let software = result.get("software").unwrap().as_array().unwrap();
                        assert!(software.is_empty(), "expected software: [], got {software:?}");
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_label_id() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).by(By::Id).to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(By::Id)"#,
                r#"g.v().group().by(By::Label).by(By::Id)"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        let value = &result["person"];
                        match value {
                            ElementProperty::Array(value) => {
                                assert_eq!(value[0].as_string(), Some("1"));
                                assert_eq!(value[1], ElementProperty::String("2".into()));
                                assert_eq!(value[2], ElementProperty::String("4".into()));
                                assert_eq!(value[3], ElementProperty::String("6".into()));
                            },
                            _ => panic!(),
                        }
                        let value = &result["software"];
                        match value {
                            ElementProperty::Array(value) => {
                                assert_eq!(value[0], ElementProperty::String("3".into()));
                                assert_eq!(value[1], ElementProperty::String("5".into()));
                            },
                            _ => panic!(),
                        }
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_label_where_id_fold() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).by(__::id().fold()).to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(__.id().fold())"#,
                r#"g.v().group().by(By::Label).by(__.id().fold())"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        let value = &result["person"];
                        match value {
                            ElementProperty::Array(value) => {
                                assert_eq!(value[0].as_string(), Some("1"));
                                assert_eq!(value[1], ElementProperty::String("2".into()));
                                assert_eq!(value[2], ElementProperty::String("4".into()));
                                assert_eq!(value[3], ElementProperty::String("6".into()));
                            },
                            _ => panic!(),
                        }
                        let value = &result["software"];
                        match value {
                            ElementProperty::Array(value) => {
                                assert_eq!(value[0], ElementProperty::String("3".into()));
                                assert_eq!(value[1], ElementProperty::String("5".into()));
                            },
                            _ => panic!(),
                        }
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_by_label_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(Label).by(Count).to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(By::Count)"#,
                r#"g.v().group().by(By::Label).by(By::Count)"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        assert_eq!(result.get("person").unwrap().as_i64(), Some(4));
                        assert_eq!(result.get("software").unwrap().as_i64(), Some(2));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_by_age_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by("age").by(Count).to_list(),
            rhai: [
                r#"g.V().group().by("age").by(By::Count)"#,
                r#"g.v().group().by("age").by(By::Count)"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Map(index_map) => {
                        assert_eq!(index_map.len(), 4);
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_by_label_last_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(Label).by(Count).count().to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(By::Count).count()"#,
                r#"g.v().group().by(By::Label).by(By::Count).count()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_i64(), 1.into());
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_by_label_where_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(Label).by(__::count()).to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(__.count())"#,
                r#"g.v().group().by(By::Label).by(__.count())"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        assert_eq!(result.get("person").unwrap().as_i64(), Some(4));
                        assert_eq!(result.get("software").unwrap().as_i64(), Some(2));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_group_count_by_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group_count().by(Label).to_list(),
            rhai: [
                r#"g.V().groupCount().by(By::Label)"#,
                r#"g.v().group_count().by(By::Label)"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        assert_eq!(result.get("person").unwrap().as_i64(), Some(4));
                        assert_eq!(result.get("software").unwrap().as_i64(), Some(2));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_by_label_default_value() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).to_list(),
            rhai: [
                r#"g.V().group().by(By::Label)"#,
                r#"g.v().group().by(By::Label)"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        let persons = result.get("person").unwrap().as_array().unwrap();
                        assert_eq!(persons.len(), 4);
                        let software = result.get("software").unwrap().as_array().unwrap();
                        assert_eq!(software.len(), 2);
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_by_label_by_property() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).by("name").to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by("name")"#,
                r#"g.v().group().by(By::Label).by("name")"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        let persons = result.get("person").unwrap().as_array().unwrap();
                        assert_eq!(persons.len(), 4);
                        let names: Vec<_> = persons.iter().map(|p| p.as_string().unwrap()).collect();
                        assert!(names.contains(&"marko"));
                        assert!(names.contains(&"vadas"));
                        assert!(names.contains(&"josh"));
                        assert!(names.contains(&"peter"));

                        let software = result.get("software").unwrap().as_array().unwrap();
                        assert_eq!(software.len(), 2);
                        let sw_names: Vec<_> = software.iter().map(|p| p.as_string().unwrap()).collect();
                        assert!(sw_names.contains(&"lop"));
                        assert!(sw_names.contains(&"ripple"));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_by_name_by_id() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by("name").by(By::Id).to_list(),
            rhai: [
                r#"g.V().group().by("name").by(By::Id)"#,
                r#"g.v().group().by("name").by(By::Id)"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        assert_eq!(result.len(), 6);
                        let marko_ids = result.get("marko").unwrap().as_array().unwrap();
                        assert_eq!(marko_ids.len(), 1);
                        assert_eq!(marko_ids[0].as_string(), Some("1"));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_group_count_default() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").group_count().to_list(),
            rhai: [
                r#"g.V().values("age").groupCount()"#,
                r#"g.v().values("age").group_count()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Map(result) => {
                        assert_eq!(result.len(), 4);
                        for age in [29, 27, 32, 35] {
                            let key = ElementProperty::Number(crate::value::Number::from_i64(age));
                            assert_eq!(result.get(&key).unwrap().as_i64(), Some(1));
                        }
                    },
                    _ => panic!(),
                }
            },
        );
    }

    /// Equal property values read from different vertices form one group: the key is the value,
    /// not the per-vertex property handle (`lop` and `ripple` both have `lang: "java"`).
    #[cfg(feature = "script")]
    #[test]
    fn v_group_count_merges_equal_values_from_different_vertices() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("lang").group_count().to_list(),
            rhai: [
                r#"g.V().values("lang").groupCount()"#,
                r#"g.v().values("lang").group_count()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(result) => {
                        assert_eq!(result.len(), 1);
                        assert_eq!(result.get("java").unwrap().as_i64(), Some(2));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[test]
    fn v_group_count_large_110k() {
        let graph = GraphSource::large_110k();
        let g_lock = graph.read();

        // 1. group_count().by("level")
        let g1 = g_lock.traversal();
        let res1 = g1.v(None).group_count().by("level").to_list().unwrap();
        match &res1[0] {
            ElementProperty::Object(result) => {
                assert_eq!(result.len(), 5);
                assert_eq!(result.get("0").unwrap().as_i64(), Some(10));
                assert_eq!(result.get("1").unwrap().as_i64(), Some(100));
                assert_eq!(result.get("2").unwrap().as_i64(), Some(1000));
                assert_eq!(result.get("3").unwrap().as_i64(), Some(10000));
                assert_eq!(result.get("4").unwrap().as_i64(), Some(100000));
            }
            _ => panic!("Expected Object property"),
        }

        // 2. group().by("level").by(Count)
        let g2 = g_lock.traversal();
        let res2 = g2.v(None).group().by("level").by(Count).to_list().unwrap();
        assert_eq!(res1, res2);

        // 3. group().by("level").by(__::count())
        let g3 = g_lock.traversal();
        let res3 = g3
            .v(None)
            .group()
            .by("level")
            .by(__::count())
            .to_list()
            .unwrap();
        assert_eq!(res1, res3);
    }

    /// `By::JsonPath` used as `group()`'s second `.by()` goes through `group()`'s own separate
    /// `match second_by` (not `fetch_traverser_value_by` alone) — this exercises that path
    /// specifically, distinct from the `select()` coverage of the shared lever.
    #[test]
    #[cfg(feature = "serde")]
    fn group_by_json_path_second_by() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .group()
            .by(By::Label)
            .by(By::JsonPath("meta.k".parse().unwrap()))
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Object(result) => {
                let value = &result["person"];
                match value {
                    ElementProperty::Array(value) => {
                        assert_eq!(value[0].as_i64(), Some(1));
                    }
                    _ => panic!("Expected Array property"),
                }
            }
            _ => panic!("Expected Object property"),
        }
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .group()
            .by("name")
            .by(By::Id)
            .count()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("group("));
    }
}
