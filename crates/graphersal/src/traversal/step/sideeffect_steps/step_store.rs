use crate::graph::GraphStorage;
use crate::*;

use super::step_aggregate::push_side_effect_n;

/// `store("key")`: collects every traverser passing through it into `context.side_effect[key]`,
/// for later use by `cap(key)` or a `where(within(key))`/`where(without(key))`-style filter,
/// exactly like `aggregate()`. An optional `.by(...)` modulator reshapes what gets stored, same
/// as `group()`/`fold()`'s `.by()`.
///
/// **This is `aggregate()`'s sibling — see `step_aggregate.rs`'s doc comment for the confirmed
/// `aggregate()`/`store()` semantic comparison.** In short: TinkerPop documents `store()` as the
/// *lazy* counterpart to `aggregate()`'s *eager* barrier (a downstream `limit()` can in principle
/// leave `store()` having seen only part of the stream, while `aggregate()` never lets that
/// happen), but this engine's executor already fully materializes the incoming
/// `Vec<Traverser<S>>` batch before any `SideEffect` step runs — there is no partial stream for
/// either step to observe differently here, so both are implemented identically, down to
/// preserving duplicates in the stored list.
///
/// A single `by()` only; a second is `InvalidModulator` (see `step_aggregate.rs`, and the user's
/// guide page "The by() Modulator: Single-by Steps").
struct StepStore<'g, S: GraphStorage + 'g> {
    key: CompactString,
    by: Option<By<'g, S>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Clone for StepStore<'g, S> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepStore<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by = String::new();
        if let Some(b) = self.by.as_ref() {
            by.push_str(&crate::traversal::render_by(b));
        }
        write!(
            f,
            "store({}){}",
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
    /// TinkerPop documents `store()` as the lazy counterpart of `aggregate()`. This engine
    /// materializes each batch before a side-effect step runs, so the difference is not
    /// observable here: both behave identically and keep duplicates.
    pub fn store(self, key: &str) -> Self {
        self.push_step(GraphStep::SideEffect(
            "store",
            Box::new(StepStore {
                key: CompactString::new(key),
                by: None,
                by_check: ByModulatorCheck::default(),
            }),
        ))
    }
}

impl __ {
    /// Like aggregate(), but adds each value lazily, without a barrier. Starts an anonymous
    /// traversal (`__.store()`).
    pub fn store<'g, S: GraphStorage + 'g>(key: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().store(key)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Like aggregate(), but adds each value lazily, without a barrier.
    pub fn store(self, key: &str) -> Self {
        let step = GraphStep::SideEffect(
            "store",
            Box::new(StepStore {
                key: CompactString::new(key),
                by: None,
                by_check: ByModulatorCheck::default(),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffect<'g, S> for StepStore<'g, S> {
    /// Builds a growing aggregation bucket (D3 of the memory-inspection spec) — reported to
    /// `.profile()` as a non-zero `bytes_materialized`, matching `aggregate()`.
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
        self.by_check.check("store")?;
        // Without by() the value itself is collected (a property element stays a handle).
        let resolved = match &self.by {
            Some(by) => {
                TraverserValue::fetch_traverser_value_by(context, source, &source.value, by)?
            }
            None => Some(source.value.clone()),
        };
        if let Some(resolved) = resolved {
            push_side_effect_n(context, &self.key, resolved, source.bulk, "store")?;
            // `store()` is lazy: a reduced bucket folds after every element.
            super::step_aggregate::flush_reduced(&mut context.side_effect, self.key.as_str())?;
        }
        Ok(())
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe(&args);
        if self.by.is_some() {
            self.by_check
                .reject_last(super::step_aggregate::reason::TOO_MANY);
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
            rust: |g| g.v("1").out("knows").store("friends").cap("friends").to_list(),
            rhai: [
                r#"g.V("1").out("knows").store("friends").cap("friends")"#,
                r#"g.v("1").out("knows").store("friends").cap("friends")"#,
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
            rust: |g| g.v("1").out("knows").store("names").by("name").cap("names").to_list(),
            rhai: [
                r#"g.V("1").out("knows").store("names").by("name").cap("names")"#,
                r#"g.v("1").out("knows").store("names").by("name").cap("names")"#,
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
            rust: |g| g.v(None).store("all").count().to_list(),
            rhai: [
                r#"g.V().store("all").count()"#,
                r#"g.v().store("all").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    /// `store()`'s defining, TinkerPop-documented trait relative to a hypothetical deduplicating
    /// bucket: every duplicate stays in the list, none collapsed. `aggregate()`'s own
    /// `duplicates_preserved` test (`step_aggregate.rs`) asserts the identical outcome, which is
    /// the point — see both steps' doc comments for why they share this behavior here.
    #[test]
    fn duplicates_preserved() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .has_label("person")
            .store("labels")
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
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .out("knows")
                .store("friends")
                .where_t(__::where_p(vec!["friends"]))
                .count()
                .to_list(),
            rhai: [
                r#"g.V("1").out("knows").store("friends").where(P.within("friends")).count()"#,
                r#"g.v("1").out("knows").store("friends").where(P.within("friends")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).store("x").cap("x").profile().unwrap();
        assert!(result.to_string().contains("store("));
    }
}
