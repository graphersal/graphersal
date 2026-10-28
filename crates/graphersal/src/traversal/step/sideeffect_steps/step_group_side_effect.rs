use crate::graph::GraphStorage;
use crate::traversal::side_effect::{GroupBucket, GroupSlot, SideEffectValue};
use crate::traversal::step::barrier_steps::step_group::{
    GroupValueKind, bulk_overflow, check_value_by, group_bys, group_key, group_list_value,
    group_value_kind, reason, takes_incoming_value,
};
use crate::*;

/// `group("a")` / `groupCount("a")`: TinkerPop's `GroupSideEffectStep` /
/// `GroupCountSideEffectStep`. Every traverser passes through unchanged; on the way it is added
/// to the map held by the side effect `a`, exactly as `group()`/`group_count()` would group it
/// (same `by()` rules: the first `by()` is the key, the second the value; `group_count("a")`
/// takes a single `by()`). `cap("a")`, `select("a")` and `where(P.within("a"))` read the map.
///
/// The map is built incrementally, so a reader later in the same traversal sees what the step
/// has collected so far (all of the current batch: `g.V().groupCount("a").select("a")` sees the
/// whole count, `local(groupCount("a").select("a"))` a growing one, as in TinkerPop). A value
/// traversal with a barrier (`by(__.out().fold())`) is re-run over all members of each key that
/// grew, at the end of every execution of the step. The side effect exists, as `{}`, as soon as
/// the step has run, even when no traverser reached it.
///
/// **Deviation from TinkerPop:** a key that already holds another kind of side effect (a
/// `withSideEffect` seed, an `aggregate()` list, a tree), or that another group step fills with a
/// different value reduction, is an error; TinkerPop merges into whatever the key holds. See
/// `book/src/traversal/steps/group.md` and `tinkerpop_deviations.md`.
struct StepGroupSideEffect<'g, S: GraphStorage + 'g> {
    key: CompactString,
    by: Option<Vec<By<'g, S>>>,
    group_count: bool,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGroupSideEffect<'g, S> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            by: self.by.clone(),
            group_count: self.group_count,
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGroupSideEffect<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = if self.group_count {
            "group_count"
        } else {
            "group"
        };
        write!(
            f,
            "{name}({})",
            crate::value::literal::rhai_string(&self.key)
        )?;
        for by in self.by.iter().flatten() {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> StepGroupSideEffect<'g, S> {
    fn new(key: &str, group_count: bool) -> Self {
        Self {
            key: CompactString::new(key),
            by: None,
            group_count,
            by_check: ByModulatorCheck::default(),
        }
    }

    fn step_name(&self) -> &'static str {
        if self.group_count {
            "group_count"
        } else {
            "group"
        }
    }

    fn graph_step(self) -> GraphStep<'g, S> {
        let name = self.step_name();
        GraphStep::SideEffect(name, Box::new(self))
    }

    /// The group bucket of this step's key; a key that holds another kind of side effect (a
    /// `withSideEffect` seed, an `aggregate()` list) is an error, the two cannot be merged.
    fn bucket<'c>(
        &self,
        side_effect: &'c mut std::collections::HashMap<CompactString, SideEffectValue<S>>,
    ) -> TraverserResult<&'c mut GroupBucket<S>> {
        match side_effect
            .entry(self.key.clone())
            .or_insert_with(|| SideEffectValue::Group(GroupBucket::default()))
        {
            SideEffectValue::Group(bucket) => Ok(bucket),
            other => Err(Box::new(TraverserError::QueryExecution {
                query: self.step_name().to_string(),
                reason: format!(
                    "side effect '{key}' already holds a {kind}, {step}(\"{key}\") needs an unused key or one another group step fills",
                    key = self.key,
                    kind =
                        crate::traversal::step::barrier_steps::step_tree::side_effect_kind(other),
                    step = self.step_name(),
                ),
            })),
        }
    }

    /// The error for two group steps that fill one key with different value reductions
    /// (`groupCount("a")` and `group("a").by(..).by(__.fold())`).
    fn mixed_slots(&self) -> Box<TraverserError> {
        Box::new(TraverserError::QueryExecution {
            query: self.step_name().to_string(),
            reason: format!(
                "side effect '{}' is filled by group steps whose value by() reduce differently",
                self.key
            ),
        })
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `group("key")`: the side-effect form of [`Self::group`]. Every traverser passes through
    /// unchanged and is grouped into the map held by the side effect `key` (first `by()` the
    /// key, second `by()` the value, the same rules as `group()`); read it with `cap("key")` or
    /// `select("key")`.
    pub fn group_side_effect(self, key: &str) -> Self {
        self.push_step(StepGroupSideEffect::new(key, false).graph_step())
    }

    /// `groupCount("key")`: the side-effect form of [`Self::group_count`]. Every traverser
    /// passes through unchanged and is counted into the map held by the side effect `key` (an
    /// optional single `by()` gives the key); read it with `cap("key")` or `select("key")`.
    pub fn group_count_side_effect(self, key: &str) -> Self {
        self.push_step(StepGroupSideEffect::new(key, true).graph_step())
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `group("key")` in a child traversal; see [`GraphTraversalSource::group_side_effect`].
    pub fn group_side_effect(self, key: &str) -> Self {
        self.push_step(StepGroupSideEffect::new(key, false).graph_step())
    }

    /// `groupCount("key")` in a child traversal; see
    /// [`GraphTraversalSource::group_count_side_effect`].
    pub fn group_count_side_effect(self, key: &str) -> Self {
        self.push_step(StepGroupSideEffect::new(key, true).graph_step())
    }
}

impl __ {
    /// `__.group("key")`; see [`GraphTraversalSource::group_side_effect`].
    pub fn group_side_effect<'g, S: GraphStorage + 'g>(key: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().group_side_effect(key)
    }

    /// `__.groupCount("key")`; see [`GraphTraversalSource::group_count_side_effect`].
    pub fn group_count_side_effect<'g, S: GraphStorage + 'g>(
        key: &str,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().group_count_side_effect(key)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffect<'g, S> for StepGroupSideEffect<'g, S> {
    fn materializes(&self) -> bool {
        true
    }

    fn declares_side_effect_key(&self) -> Option<&str> {
        Some(self.key.as_str())
    }

    /// An unused key starts as an empty group map, so `cap(key)` of a step no traverser reached
    /// reads `{}`.
    fn declare_side_effect(
        &self,
        side_effect: &mut std::collections::HashMap<CompactString, SideEffectValue<S>>,
    ) {
        if !side_effect.contains_key(self.key.as_str()) {
            side_effect.insert(
                self.key.clone(),
                SideEffectValue::Group(GroupBucket::default()),
            );
        }
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<()> {
        let step = self.step_name();
        self.by_check.check(step)?;
        let (key_by, value_by) = group_bys(self.by.as_deref(), self.group_count);
        check_value_by(value_by, step)?;
        let Some(key) = group_key(context, source, &source.value, key_by)? else {
            return Ok(());
        };
        match group_value_kind(self.group_count, value_by) {
            GroupValueKind::Count => {
                let bucket = self.bucket(&mut context.side_effect)?;
                let slot = bucket.slots.entry(key).or_insert(GroupSlot::Count(0));
                let GroupSlot::Count(count) = slot else {
                    return Err(self.mixed_slots());
                };
                let next = count
                    .checked_add(source.bulk)
                    .filter(|n| i64::try_from(*n).is_ok())
                    .ok_or_else(|| bulk_overflow(step))?;
                *count = next;
            }
            GroupValueKind::List => {
                if source.bulk > 1 {
                    let limit = context.options.bulk_max_expansion;
                    if source.bulk > limit {
                        return Err(Box::new(TraverserError::BulkExpansionLimit {
                            step: step.to_string(),
                            requested: source.bulk,
                            limit,
                        }));
                    }
                }
                let value = if takes_incoming_value(value_by) {
                    Some(source.value.clone())
                } else {
                    group_list_value(context, source, value_by)?
                };
                if let Some(value) = &value
                    && source.bulk > 1
                {
                    crate::traversal::bulk::ExpansionLimits::of(&context.options).check_copies(
                        value,
                        source.bulk,
                        step,
                    )?;
                }
                let deadline = &mut context.deadline;
                let bucket = self.bucket(&mut context.side_effect)?;
                let slot = bucket
                    .slots
                    .entry(key)
                    .or_insert_with(|| GroupSlot::List(Vec::new()));
                let GroupSlot::List(items) = slot else {
                    return Err(self.mixed_slots());
                };
                if let Some(value) = value {
                    crate::traversal::bulk::push_copies(items, value, source.bulk, deadline)?;
                }
            }
            GroupValueKind::Assign => {
                let By::Traversal(child) = value_by else {
                    return Err(self.mixed_slots());
                };
                let Some(first) = child.first(context, source)? else {
                    return Ok(());
                };
                let bucket = self.bucket(&mut context.side_effect)?;
                match bucket.slots.get_mut(&key) {
                    Some(GroupSlot::Assigned(value)) => *value = first.value,
                    Some(_) => return Err(self.mixed_slots()),
                    None => {
                        bucket.slots.insert(key, GroupSlot::Assigned(first.value));
                    }
                }
            }
            GroupValueKind::Barrier => {
                let bucket = self.bucket(&mut context.side_effect)?;
                let slot = bucket
                    .slots
                    .entry(key)
                    .or_insert_with(|| GroupSlot::Members {
                        members: Vec::new(),
                        reduced: None,
                        stale: false,
                    });
                let GroupSlot::Members { members, stale, .. } = slot else {
                    return Err(self.mixed_slots());
                };
                // Derived: the member keeps its path, sack and bulk for the value traversal.
                members.push(Traverser::with_parts(
                    source.value.clone(),
                    source.path,
                    source.bulk,
                    source.sack,
                ));
                *stale = true;
            }
        }
        Ok(())
    }

    /// Re-runs a reducing value traversal over the members of every key that grew in this
    /// execution of the step, so readers after the step see the reduced values.
    fn finish_batch(&self, context: &mut TraverserContext<'_, S>) -> TraverserResult<()> {
        let (_, value_by) = group_bys(self.by.as_deref(), self.group_count);
        if group_value_kind(self.group_count, value_by) != GroupValueKind::Barrier {
            return Ok(());
        }
        let By::Traversal(child) = value_by else {
            return Ok(());
        };
        let stale: Vec<(usize, Vec<Traverser<S>>)> = match context.side_effect.get(&self.key) {
            Some(SideEffectValue::Group(bucket)) => bucket
                .slots
                .values()
                .enumerate()
                .filter_map(|(index, slot)| match slot {
                    GroupSlot::Members {
                        members,
                        stale: true,
                        ..
                    } => Some((index, members.clone())),
                    GroupSlot::Members { .. }
                    | GroupSlot::Count(_)
                    | GroupSlot::List(_)
                    | GroupSlot::Assigned(_) => None,
                })
                .collect(),
            _ => return Ok(()),
        };
        for (index, members) in stale {
            // batch: bulk preserved (the members' bulks reach the value traversal's
            // `count()`/`sum()`/...).
            let result = child.to_list(&mut *context, members)?.into_iter().next();
            if let Some(SideEffectValue::Group(bucket)) = context.side_effect.get_mut(&self.key)
                && let Some((_, GroupSlot::Members { reduced, stale, .. })) =
                    bucket.slots.get_index_mut(index)
            {
                *reduced = result.map(|t| t.value);
                *stale = false;
            }
        }
        Ok(())
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe(&args);
        if matches!(args, By::Order(_)) {
            self.by_check.reject_last(reason::ORDER);
        }
        if self.group_count && self.by.is_some() {
            self.by_check.reject_last(reason::TOO_MANY);
        }
        self.by.get_or_insert_with(Vec::new).push(args);
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
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

    fn clone_box(&self) -> Box<dyn GraphStepSideEffect<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn group_count_side_effect_by_name() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).out("created").group_count_side_effect("a").by("name").cap("a").to_list(),
            rhai: [
                r#"g.V().out("created").groupCount("a").by("name").cap("a")"#,
                r#"g.v().out("created").group_count("a").by("name").cap("a")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Object(map) => {
                        assert_eq!(map.get("lop").and_then(|v| v.as_i64()), Some(3));
                        assert_eq!(map.get("ripple").and_then(|v| v.as_i64()), Some(1));
                    }
                    other => panic!("expected a map, got {other:?}"),
                }
            },
        );
    }

    #[test]
    fn traversers_pass_through_unchanged() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group_side_effect("a").by(By::Label).values("name").count().to_list(),
            rhai: [
                r#"g.V().group("a").by(T.label).values("name").count()"#,
                r#"g.v().group("a").by(T.label).values("name").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn no_traverser_reads_an_empty_map() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").group_count_side_effect("a").cap("a").to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").groupCount("a").cap("a")"#,
                r#"g.v().has_label("nope").group_count("a").cap("a")"#,
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

    #[test]
    fn reducing_value_traversal_runs_over_all_members() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .group_side_effect("a")
                .by(By::Label)
                .by(__::values("age").sum())
                .cap("a")
                .to_list(),
            rhai: [
                r#"g.V().group("a").by(T.label).by(__.values("age").sum()).cap("a")"#,
                r#"g.v().group("a").by(T.label).by(__.values("age").sum()).cap("a")"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(map) => {
                        assert_eq!(map.get("person").and_then(|v| v.as_i64()), Some(123));
                    }
                    other => panic!("expected a map, got {other:?}"),
                }
            },
        );
    }

    #[test]
    fn second_by_on_group_count_is_rejected() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group_count_side_effect("a").by("name").by("age").to_list(),
            rhai: [
                r#"g.V().groupCount("a").by("name").by("age")"#,
                r#"g.v().group_count("a").by("name").by("age")"#,
            ],
            assert: |err| assert!(err.contains("by()"), "{err}"),
        );
    }

    #[test]
    fn key_holding_another_side_effect_is_an_error() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).aggregate("a").group_count_side_effect("a").to_list(),
            rhai: [
                r#"g.V().aggregate("a").groupCount("a")"#,
                r#"g.v().aggregate("a").group_count("a")"#,
            ],
            assert: |err| assert!(err.contains("already holds"), "{err}"),
        );
    }
}
