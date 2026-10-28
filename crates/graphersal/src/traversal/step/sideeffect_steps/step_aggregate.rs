use crate::graph::GraphStorage;
use crate::traversal::side_effect::SideEffectValue;
use crate::*;

/// The `reason` texts of `TraverserError::InvalidModulator` for `aggregate()` and `store()`; their
/// `help()` keys on them.
pub(crate) mod reason {
    /// A second `by()`: both steps collect one projection per traverser.
    pub(crate) const TOO_MANY: &str = "aggregate() and store() take a single by()";
}

/// `aggregate("key")` / `store("key")` share this: append `value` (already resolved by a
/// `.by(...)` modulator, or the traverser's own value with none) into `side_effect[key]`,
/// growing a [`SideEffectValue::List`] across every traverser that passes through rather than
/// overwriting it. The value is stored as it is (a vertex stays a vertex handle, never a
/// snapshot). If `key` was already populated by something else (`with_side_effect()`, most
/// plausibly) with a single value, that value becomes the first element instead of being
/// silently discarded.
///
/// `n` copies (the traverser's bulk) are pushed, bounded by `bulk.max_expansion` and
/// `traversal.max_materialized_bytes` and counted against the `evaluationTimeout` of `context`.
pub(crate) fn push_side_effect_n<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    key: &CompactString,
    value: TraverserValue<S>,
    n: u64,
    step: &str,
) -> TraverserResult<()> {
    let limits = crate::traversal::bulk::ExpansionLimits::of(&context.options);
    let storage: &S = context.storage;
    let side_effect = &mut context.side_effect;
    let deadline = &mut context.deadline;
    // Bulk > 1 only exists after a merge, so plain queries never reach the expansion check.
    let too_many = |requested: u64| {
        Box::new(TraverserError::BulkExpansionLimit {
            step: step.to_string(),
            requested,
            limit: limits.max_values,
        })
    };
    let copies = if n > 1 {
        limits.check_copies(&value, n, step)?;
        usize::try_from(n).map_err(|_| too_many(n))?
    } else {
        1
    };
    if let Some(SideEffectValue::Set(items)) = side_effect.get_mut(key) {
        // A set bucket holds each value once; bulk is invisible. A lazily read property value
        // (`values()`) is compared by its value, not by which element it came from, so it is
        // materialized; elements, property elements (`properties()`, compared by their element
        // and key) and containers keep their identity. (TinkerPop compares an edge property by
        // key and value; here it stays by identity, see book "Property Elements".)
        let value = match value {
            TraverserValue::VertexProperty(_, _, kind)
            | TraverserValue::EdgeProperty(_, _, kind)
                if kind.is_value() =>
            {
                TraverserValue::Computed(value.into_node_property(storage)?, None)
            }
            other => other,
        };
        items.insert(value);
        return Ok(());
    }
    if let Some(SideEffectValue::Reduced { pending, .. }) = side_effect.get_mut(key) {
        // A reduced bucket (`withSideEffect(key, init, Operator)`) only queues the value here;
        // `flush_reduced` folds the queue into the accumulator once per batch.
        let value = value.into_node_property(storage)?;
        pending.try_reserve(copies).map_err(|_| too_many(n))?;
        return crate::traversal::bulk::push_copies(pending, value, n, deadline);
    }
    let entry = side_effect
        .entry(key.clone())
        .or_insert_with(|| SideEffectValue::List(Vec::new()));
    if let SideEffectValue::Value(_) = entry {
        let SideEffectValue::Value(existing) =
            std::mem::replace(entry, SideEffectValue::List(Vec::new()))
        else {
            return Ok(());
        };
        if let SideEffectValue::List(items) = entry {
            items.push(existing);
        }
    }
    if let SideEffectValue::List(items) = entry {
        items.try_reserve(copies).map_err(|_| too_many(n))?;
        crate::traversal::bulk::push_copies(items, value, n, deadline)?;
    }
    Ok(())
}

/// Folds the values a reduced bucket (`withSideEffect(key, init, Operator)`) has queued into its
/// accumulator, once per step execution (`GraphStepSideEffect::finish_batch` of `aggregate()`,
/// directly after each element for `store()`). A key that holds no reduced bucket, or an empty
/// queue, is left alone.
///
/// The rule, inferred from `sideEffect/Aggregate.feature` (see the book page "Sack and Operators"):
/// the queued values `v1..vk` of one execution (bulk expanded, `by()` applied, unproductive ones
/// skipped) reach the operator as follows.
/// - `assign` and `addAll` receive them as **one list** (`op(value, [v1..vk])`): TinkerPop hands
///   the reducer one collection per step execution, and these two operators take a collection
///   whole (`assign` keeps the list, `addAll` concatenates it);
/// - every other operator folds them one by one in stream order (`op(op(value, v1), v2)`), which
///   is what the expectations for `sum`, `minus`, `mult`, `div`, `min`, `max`, `and`, `or` need.
///
/// Deviation: TinkerPop 3.7.2 passes the collection (a `BulkSet`) to every operator and so fails
/// for `sum` (`ClassCastException`); the expectations (`sum` = 124) show 3.8.2 differs.
pub(crate) fn flush_reduced<S: GraphStorage>(
    side_effect: &mut std::collections::HashMap<CompactString, SideEffectValue<S>>,
    key: &str,
) -> TraverserResult<()> {
    let Some(SideEffectValue::Reduced { op, value, pending }) = side_effect.get_mut(key) else {
        return Ok(());
    };
    if pending.is_empty() {
        return Ok(());
    }
    let items = std::mem::take(pending);
    let mut acc = std::mem::replace(value, ElementProperty::Null);
    match op {
        Operator::Assign | Operator::AddAll => {
            acc = op.apply_owned(acc, ElementProperty::Array(items))?;
        }
        _ => {
            for item in items {
                acc = op.apply_owned(acc, item)?;
            }
        }
    }
    *value = acc;
    Ok(())
}

/// `aggregate("key")`: collects every traverser passing through it into
/// `context.side_effect[key]`, for later use by `cap(key)` or a `where(within(key))`/
/// `where(without(key))`-style filter (see `value/predicate.rs`'s `P::WithIn` side-effect
/// resolution). Traversers continue downstream unchanged — this is a pure accumulation, never a
/// filter. An optional `.by(...)` modulator reshapes what gets stored, exactly like
/// `group()`/`fold()`'s `.by()` (e.g. `aggregate("names").by("name")` stores names, not whole
/// vertices).
///
/// **`aggregate()` vs. `store()` (confirmed, not assumed):** TinkerPop's reference historically
/// describes `aggregate()` (`Scope.global`, the only scope this crate implements — see the
/// `Scope.local` gap noted in this feature's implementation report) as *eager* — a barrier that
/// forces complete evaluation of everything upstream before any traverser continues — while
/// `store()` is documented as *lazy*, so a downstream short-circuit (e.g. `limit()`) can leave
/// `store()` having seen only part of the stream. `aggregate()`'s bucket is further documented as
/// a `BulkSet`, a weighted multi-set that compresses repeated identical values into a
/// `(value, count)` pair internally. That compression is a memory optimization only: a `BulkSet`
/// is still a *multi*-set, so listing/iterating it yields every original occurrence, duplicates
/// included — the same content a plain list produces. Neither difference is observable in this
/// crate: this engine's executor already fully materializes the incoming `Vec<Traverser<S>>` batch
/// before any `SideEffect` step runs (`traversal/source.rs`'s `GraphStep::SideEffect` arm iterates
/// the whole batch with no way for a later step to have already short-circuited it), so there is
/// no partial stream for `aggregate()`'s eagerness to force early and none for `store()`'s
/// laziness to observe differently; and duplicate values are kept either way, matching what
/// `BulkSet` itself would have produced on read. `aggregate()` and `store()` are therefore
/// implemented identically here, down to the output shape, and differ only in name/debug
/// rendering — see `step_store.rs` for the sibling step and its own doc comment.
///
/// **Deviation from TinkerPop 3.8.2:** `aggregate()` and `store()` accept a single `by()`; a
/// second one is `InvalidModulator`. (User's guide: "The by() Modulator: Single-by Steps".)
struct StepAggregate<'g, S: GraphStorage + 'g> {
    key: CompactString,
    by: Option<By<'g, S>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Clone for StepAggregate<'g, S> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepAggregate<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by = String::new();
        if let Some(b) = self.by.as_ref() {
            by.push_str(&crate::traversal::render_by(b));
        }
        write!(
            f,
            "aggregate({}){}",
            crate::value::literal::rhai_string(&self.key),
            by
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Collects every traverser passing through this point into `context.side_effect[key]`, for
    /// later use by `cap(key)` or a `where(within(key))`-style filter; traversers continue
    /// downstream unchanged, and an optional `.by(...)` reshapes what gets stored.
    ///
    /// TinkerPop documents `aggregate()` as eager and `store()` as lazy. This engine materializes
    /// each batch before a side-effect step runs, so the difference is not observable here: both
    /// behave identically and keep duplicates.
    pub fn aggregate(self, key: &str) -> Self {
        self.push_step(GraphStep::SideEffect(
            "aggregate",
            Box::new(StepAggregate {
                key: CompactString::new(key),
                by: None,
                by_check: ByModulatorCheck::default(),
            }),
        ))
    }
}

impl __ {
    /// Collects every incoming value into the named list side effect before passing them on (a
    /// barrier); read it with cap() or select(). Starts an anonymous traversal (`__.aggregate()`).
    pub fn aggregate<'g, S: GraphStorage + 'g>(key: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().aggregate(key)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Collects every incoming value into the named list side effect before passing them on (a
    /// barrier); read it with cap() or select().
    pub fn aggregate(self, key: &str) -> Self {
        let step = GraphStep::SideEffect(
            "aggregate",
            Box::new(StepAggregate {
                key: CompactString::new(key),
                by: None,
                by_check: ByModulatorCheck::default(),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffect<'g, S> for StepAggregate<'g, S> {
    /// Builds a growing aggregation bucket (D3 of the memory-inspection spec) — reported to
    /// `.profile()` as a non-zero `bytes_materialized`, matching `group()`/`fold()`.
    fn materializes(&self) -> bool {
        true
    }

    fn declares_side_effect_key(&self) -> Option<&str> {
        Some(self.key.as_str())
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<()> {
        self.by_check.check("aggregate")?;
        // Without by() the value itself is collected (a property element stays a handle).
        let resolved = match &self.by {
            Some(by) => {
                TraverserValue::fetch_traverser_value_by(context, source, &source.value, by)?
            }
            None => Some(source.value.clone()),
        };
        if let Some(resolved) = resolved {
            push_side_effect_n(context, &self.key, resolved, source.bulk, "aggregate")?;
        }
        Ok(())
    }

    fn finish_batch(&self, context: &mut TraverserContext<'_, S>) -> TraverserResult<()> {
        flush_reduced(&mut context.side_effect, self.key.as_str())
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe(&args);
        if self.by.is_some() {
            self.by_check.reject_last(reason::TOO_MANY);
        }
        self.by = Some(args);
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by
            .as_mut()
            .and_then(By::traversal_mut)
            .into_iter()
            .collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        match &mut self.by {
            Some(by) => by.analyze_paths(analysis),
            None => crate::traversal::optimizer::path_analysis::PathRequirement::None,
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffect<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out("knows").aggregate("friends").cap("friends").to_list(),
            rhai: [
                r#"g.V("1").out("knows").aggregate("friends").cap("friends")"#,
                r#"g.v("1").out("knows").aggregate("friends").cap("friends")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Array(items) => assert_eq!(items.len(), 2),
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn by_name() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out("knows").aggregate("names").by("name").cap("names").to_list(),
            rhai: [
                r#"g.V("1").out("knows").aggregate("names").by("name").cap("names")"#,
                r#"g.v("1").out("knows").aggregate("names").by("name").cap("names")"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Array(items) => {
                        let names: Vec<_> = items.iter().map(|v| v.as_string().unwrap()).collect();
                        assert!(names.contains(&"vadas"));
                        assert!(names.contains(&"josh"));
                    }
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn passthrough_does_not_filter() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).aggregate("all").count().to_list(),
            rhai: [
                r#"g.V().aggregate("all").count()"#,
                r#"g.v().aggregate("all").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn duplicates_preserved() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        // Every "person"-labeled traverser reached twice: once by out("created")'s creators and
        // once directly via v(None), so aggregate() must keep both occurrences of any repeat.
        let result = g
            .v(None)
            .has_label("person")
            .aggregate("labels")
            .by(By::Label)
            .cap("labels")
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Array(items) => {
                assert_eq!(items.len(), 4);
                assert!(items.iter().all(|v| v.as_string() == Some("person")));
            }
            _ => panic!(),
        }
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_within_side_effect_matches() {
        // marko's own aggregated "friends" (vadas, josh) must each test positive for
        // `within("friends")` — the same vertices the bucket was built from.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .out("knows")
                .aggregate("friends")
                .where_t(__::where_p(vec!["friends"]))
                .count()
                .to_list(),
            rhai: [
                r#"g.V("1").out("knows").aggregate("friends").where(P.within("friends")).count()"#,
                r#"g.v("1").out("knows").aggregate("friends").where(P.within("friends")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_without_side_effect_excludes() {
        // The motivating combinator, `not(within(key))`, exercised on a case with a real
        // match: marko is who knows vadas/josh, but marko himself was never aggregated into
        // "friends", so `in("knows")` from {vadas, josh} back to marko must pass the filter both
        // times (once per incoming edge).
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .out("knows")
                .aggregate("friends")
                .in_("knows")
                .where_t(__::not(__::where_p(vec!["friends"])))
                .count()
                .to_list(),
            rhai: [
                r#"g.V("1").out("knows").aggregate("friends").in("knows").where(__.not(__.where(P.within("friends")))).count()"#,
                r#"g.v("1").out("knows").aggregate("friends").in("knows").where(__.not(__.where(P.within("friends")))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn roadmap_acceptance_query_literal() {
        // The motivating example, verbatim: g.V(1).out("knows").aggregate("friends")
        // .out("knows").where(not(within("friends"))). On tinkerpop_modern, vadas/josh (marko's
        // "friends") have no outgoing "knows" edges of their own, so nothing reaches `where()` —
        // the count is 0. Real, non-trivial exercises of the same `within(key)`/`without(key)`
        // resolution live in the two tests above; this one is kept as a literal reproduction of
        // the query this feature's acceptance section named.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .out("knows")
                .aggregate("friends")
                .out("knows")
                .where_t(__::not(__::where_p(vec!["friends"])))
                .count()
                .to_list(),
            rhai: [
                r#"g.V("1").out("knows").aggregate("friends").out("knows").where(__.not(__.where(P.within("friends")))).count()"#,
                r#"g.v("1").out("knows").aggregate("friends").out("knows").where(__.not(__.where(P.within("friends")))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).aggregate("x").cap("x").profile().unwrap();
        assert!(result.to_string().contains("aggregate("));
    }
}
