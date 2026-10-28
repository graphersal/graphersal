//! `sample(n)`: a random sample of `n` traversers of the stream, optionally weighted by a
//! `by()` modulator.

use rand::Rng;

use crate::graph::GraphStorage;
use crate::traversal::ByModulatorCheck;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::value::Scope;
use crate::*;

/// `InvalidModulator` reasons of `sample()`.
mod reason {
    /// A second `by()`.
    pub(super) const TOO_MANY: &str = "sample() takes at most one by() modulator, the weight";
    /// A `by()` kind that projects no weight.
    pub(super) const KIND: &str = "an order or Count is not a weight; sample() weighs each \
                                   traverser by a property, T.id/T.label or a traversal";
}

/// `sample(n)` (TinkerPop `SampleGlobalStep`): keeps a random sample of `n` traversers of the
/// whole stream, drawn without replacement. With `by(weight)` a traverser is drawn with a
/// probability proportional to its weight; without it every traverser is equally likely.
struct StepSample<'g, S: GraphStorage + 'g> {
    amount: usize,
    by: Option<By<'g, S>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> StepSample<'g, S> {
    fn step(amount: usize) -> GraphStep<'g, S> {
        GraphStep::FlatBarrier(
            "sample",
            Box::new(StepSample {
                amount,
                by: None,
                by_check: ByModulatorCheck::default(),
            }),
        )
    }
}

impl<'g, S: GraphStorage + 'g> Clone for StepSample<'g, S> {
    fn clone(&self) -> Self {
        Self {
            amount: self.amount,
            by: self.by.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepSample<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sample({})", self.amount)?;
        if let Some(by) = &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `sample(n)` (TinkerPop `SampleGlobalStep`): a random sample of `n` traversers of the whole
    /// stream, drawn without replacement (a stream of at most `n` traversers passes unchanged).
    ///
    /// A following `by()` weighs each traverser: `g.E().sample(2).by("weight")` draws edges with
    /// a probability proportional to their `weight`. The weight must be a number of at least 0
    /// (a traverser with weight 0 is never drawn, so fewer than `n` traversers can come back);
    /// a traverser whose `by()` is unproductive (a missing property) is dropped, as in
    /// `order()`. Only one `by()` is allowed.
    ///
    /// Deviation from TinkerPop: a weight that is not a number of at least 0 is a
    /// `ValueError::SampleWeight`, and zero-weight traversers are never drawn (TinkerPop's
    /// sampling loop does not end when only zero weights are left); see
    /// `book/src/for_developers/tinkerpop_deviations.md`.
    ///
    /// The draws come from the execution's random number generator: set
    /// `g.with("random.seed", n)` for a reproducible sample. See
    /// [`sample_scoped`](Self::sample_scoped) for `Scope::Local`.
    /// Details: `book/src/traversal/steps/sample.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let picked = lock.traversal().with("random.seed", 7).v(None).sample(2).to_list().unwrap();
    /// assert_eq!(picked.len(), 2);
    /// ```
    pub fn sample(self, n: usize) -> Self {
        self.push_step(StepSample::step(n))
    }

    /// `sample(Scope, n)`: `Scope::Global` is [`sample(n)`](Self::sample); `Scope::Local`
    /// samples `n` elements of the list (or entries of the map) each traverser holds, in the
    /// order they are drawn; any other value passes through unchanged
    /// (`g.V("1").values("age").sample(Scope::Local, 5)` → `29`).
    pub fn sample_scoped(self, scope: Scope, n: usize) -> Self {
        match scope {
            Scope::Global => self.sample(n),
            Scope::Local => self.push_step(super::super::map_steps::sample_local_step(n)),
        }
    }
}

impl __ {
    /// Anonymous `sample(n)`; see [`GraphTraversalSource::sample`].
    pub fn sample<'g, S: GraphStorage + 'g>(n: usize) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().sample(n)
    }

    /// Anonymous `sample(Scope, n)`; see [`GraphTraversalSource::sample_scoped`].
    pub fn sample_scoped<'g, S: GraphStorage + 'g>(
        scope: Scope,
        n: usize,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().sample_scoped(scope, n)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `sample(n)`; see [`GraphTraversalSource::sample`].
    pub fn sample(self, n: usize) -> Self {
        self.push_step(StepSample::step(n))
    }

    /// `sample(Scope, n)`; see [`GraphTraversalSource::sample_scoped`].
    pub fn sample_scoped(self, scope: Scope, n: usize) -> Self {
        match scope {
            Scope::Global => self.sample(n),
            Scope::Local => self.push_step(super::super::map_steps::sample_local_step(n)),
        }
    }
}

/// The weight of one traverser: the `by()` value as a number of at least 0.
fn weight_of<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> TraverserResult<f64> {
    let property = value.into_node_property(context.storage)?;
    let weight = match &property {
        ElementProperty::Number(number) => number
            .as_f64()
            .or_else(|| number.as_i64().map(|n| n as f64)),
        _ => None,
    };
    match weight {
        Some(weight) if weight.is_finite() && weight >= 0.0 => Ok(weight),
        _ => {
            let actual = match &property {
                ElementProperty::Number(number) => format!("{number}"),
                other => crate::graph::schema::value_type_name(other),
            };
            Err(Box::new(TraverserError::Value(ValueError::SampleWeight {
                actual,
            })))
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepSample<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        self.by_check.check("sample")?;
        // The candidates and their weights (per unit of bulk); an unproductive `by()` drops
        // the traverser.
        let mut candidates: Vec<Traverser<S>> = Vec::with_capacity(source.len());
        let mut weights: Vec<f64> = Vec::new();
        match &self.by {
            None => candidates = source,
            Some(by) => {
                weights.reserve(source.len());
                for traverser in source {
                    let Some(value) = TraverserValue::fetch_traverser_value_by(
                        context,
                        &traverser,
                        &traverser.value,
                        by,
                    )?
                    else {
                        continue;
                    };
                    weights.push(weight_of(context, value)?);
                    candidates.push(traverser);
                }
            }
        }
        // Cumulative bulk: candidate `i` stands for the units `ends[i - 1]..ends[i]`.
        let mut ends = Vec::with_capacity(candidates.len());
        let mut total = 0u64;
        for candidate in &candidates {
            total = total.checked_add(candidate.bulk).ok_or_else(|| {
                Box::new(TraverserError::BulkOverflow {
                    step: "sample".to_string(),
                })
            })?;
            ends.push(total);
        }
        if total <= self.amount as u64 {
            return Ok(Some(candidates));
        }
        // How many units of each candidate's bulk were drawn.
        let mut taken = vec![0u64; candidates.len()];
        if self.by.is_none() {
            // Uniform: draw `amount` distinct units of the whole (bulk-expanded) stream.
            let units = usize::try_from(total).map_err(|_| {
                Box::new(TraverserError::BulkOverflow {
                    step: "sample".to_string(),
                })
            })?;
            for unit in rand::seq::index::sample(context.rng(), units, self.amount) {
                let unit = unit as u64;
                let index = ends.partition_point(|&end| end <= unit);
                if let Some(slot) = taken.get_mut(index) {
                    *slot += 1;
                }
            }
        } else {
            // Weighted, without replacement: each draw picks one remaining unit with a
            // probability proportional to its weight.
            for _ in 0..self.amount {
                let remaining: f64 = candidates
                    .iter()
                    .zip(&weights)
                    .zip(&taken)
                    .map(|((candidate, weight), taken)| weight * (candidate.bulk - taken) as f64)
                    .sum();
                if remaining <= 0.0 {
                    // Only zero-weight units are left.
                    break;
                }
                let mut target = context.rng().r#gen::<f64>() * remaining;
                let mut chosen = None;
                for (index, (candidate, weight)) in candidates.iter().zip(&weights).enumerate() {
                    let left = candidate.bulk - taken[index];
                    if left == 0 || *weight <= 0.0 {
                        continue;
                    }
                    chosen = Some(index);
                    let mass = weight * left as f64;
                    if target < mass {
                        break;
                    }
                    target -= mass;
                }
                // `chosen` is the last eligible candidate when rounding ran past the end.
                if let Some(index) = chosen {
                    taken[index] += 1;
                }
            }
        }
        Ok(Some(
            candidates
                .into_iter()
                .zip(taken)
                .filter(|(_, taken)| *taken > 0)
                .map(|(candidate, taken)| {
                    Traverser::with_parts(candidate.value, candidate.path, taken, candidate.sack)
                })
                .collect(),
        ))
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe(&args);
        match &args {
            By::Order(_) | By::Count => self.by_check.reject_last(reason::KIND),
            By::Property(_)
            | By::Traversal(_)
            | By::Label
            | By::Id
            | By::Value
            | By::Identity
            | By::Key
            | By::JsonPath(_)
            | By::Labels(_)
            | By::Column(_) => {}
        }
        if self.by.is_some() {
            self.by_check.reject_last(reason::TOO_MANY);
        } else {
            self.by = Some(args);
        }
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.by.iter_mut().filter_map(By::traversal_mut).collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // A `by(__...)` weight runs on the incoming traverser, path included.
        match &mut self.by {
            Some(by) => by.analyze_paths(analysis),
            None => PathRequirement::None,
        }
    }

    fn get_name(&self) -> String {
        self.to_string()
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn keeps_n_members_of_the_stream() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with("random.seed", 3).e(None).sample(2).id().to_list(),
            rhai: [
                r#"g.with("random.seed", 3).E().sample(2).id()"#,
                r#"g.with("random.seed", 3).e().sample(2).id()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_ne!(res[0], res[1]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_short_stream_passes_unchanged() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("software").sample(5).values("name").to_list(),
            rhai: [r#"g.V().hasLabel("software").sample(5).values("name")"#],
            assert: |res| assert_eq!(res, &["lop".into(), "ripple".into()]),
        );
    }

    /// With `random.seed` the sample is the same on every run.
    #[test]
    fn seeded_sample_is_reproducible() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let run = |seed: i64| {
            lock.traversal()
                .with("random.seed", seed)
                .v(None)
                .sample(3)
                .id()
                .to_list()
                .unwrap()
        };
        let first = run(11);
        assert_eq!(first.len(), 3);
        for _ in 0..5 {
            assert_eq!(run(11), first);
        }
        assert!((0..20).any(|seed| run(seed) != first));
    }

    /// A zero weight is never drawn; a missing weight drops the traverser.
    #[cfg(feature = "script")]
    #[test]
    fn weights_decide() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .sample(1)
                .by(__::choose(
                    __::has("name", "josh"),
                    __::constant(1),
                    __::constant(0),
                ))
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V().sample(1).by(__.choose(__.has("name", "josh"), __.constant(1), __.constant(0))).values("name")"#,
            ],
            assert: |res| assert_eq!(res, &["josh".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).sample(10).by("age").count().to_list(),
            rhai: [r#"g.V().sample(10).by("age").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn rejects_a_second_by_and_a_bad_weight() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).sample(1).by("age").by(By::Id).to_list(),
            rhai: [r#"g.V().sample(1).by("age").by(T.id)"#],
            assert: |err| assert!(err.to_string().contains("by()"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).sample(1).by("name").to_list(),
            rhai: [r#"g.V().sample(1).by("name")"#],
            assert: |err| assert!(err.to_string().contains("at least 0"), "{err}"),
        );
    }

    /// Merged traversers are sampled unit by unit: the sample never exceeds `n` in bulk.
    #[cfg(feature = "script")]
    #[test]
    fn respects_bulk() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).both(None).barrier().sample(3).count().to_list(),
            rhai: [r#"g.V().both().barrier().sample(3).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn inside_repeat_and_local() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with("random.seed", 1).v(None).repeat(__::sample(2)).times(2).count().to_list(),
            rhai: [r#"g.with("random.seed", 1).V().repeat(__.sample(2)).times(2).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).local(__::out_e(None).sample(1).by("weight")).count().to_list(),
            rhai: [r#"g.V().local(__.outE().sample(1).by("weight")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let result = lock
            .traversal()
            .e(None)
            .sample(1)
            .by("weight")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("sample(1)"), "{result}");
    }
}
