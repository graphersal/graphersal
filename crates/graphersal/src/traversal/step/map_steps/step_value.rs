use crate::graph::GraphStorage;
use crate::*;

/// `value()`: resolves a `VertexProperty`/`EdgeProperty` traverser (produced by
/// [`properties()`](GraphTraversalSource::properties) or a raw
/// [`values()`](GraphTraversalSource::values) reference) to its stored scalar value — the read
/// half of the pair with [`key()`](GraphTraversalSource::key).
///
/// Reuses [`TraverserValue::into_node_property`], the same storage lookup a terminal step
/// (`to_list()`/`next()`) already applies to a lazy property reference — `value()` just performs
/// it explicitly, mid-chain, instead of waiting for the traversal's end.
#[derive(Default, Clone)]
struct StepValue {}

impl fmt::Display for StepValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "value()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// The stored value of a property-object traverser. Errors (not panics, rule 1) when the
    /// traverser isn't a property object at all — e.g. called directly on a bare vertex without
    /// going through `properties()`/`values()` first, matching vanilla Gremlin's own behaviour
    /// (`Property`/`VertexProperty` only).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let ages = lock.traversal().v("1").properties("age").value().to_list().unwrap();
    /// assert_eq!(ages[0].as_i64(), Some(29));
    /// ```
    pub fn value(self) -> Self {
        self.push_step(GraphStep::Map("value", Box::new(StepValue::default())))
    }
}

impl __ {
    /// Anonymous `value()`; see [`GraphTraversalSource::value`].
    pub fn value<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().value()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `value()`; see [`GraphTraversalSource::value`].
    pub fn value(self) -> Self {
        let step = GraphStep::Map("value", Box::new(StepValue::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepValue {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        match &source.value {
            TraverserValue::PathProperty(prop) => Ok(Traverser::with_parts(
                TraverserValue::Computed(prop.1.clone(), None),
                source.path,
                source.bulk,
                source.sack,
            )),
            TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
                let resolved = source.value.into_node_property(context.storage)?;
                Ok(Traverser::with_parts(
                    TraverserValue::Computed(resolved, None),
                    source.path,
                    source.bulk,
                    source.sack,
                ))
            }
            v => Err(Box::new(TraverserError::cast_exception(
                &["vertexProperty", "edgeProperty", "pathProperty"],
                v,
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `g.V().hasLabel("person").properties("name", "age").value()`: the
    /// correct multiset of values across all four `person` vertices.
    #[cfg(feature = "script")]
    #[test]
    fn person_values() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").properties(vec!["name", "age"]).value().to_list(),
            rhai: [
                r#"g.V().hasLabel("person").properties(["name", "age"]).value()"#,
                r#"g.v().has_label("person").properties(["name", "age"]).value()"#,
            ],
            assert: |res| {
                let mut names: Vec<&str> = res.iter().filter_map(|v| v.as_string()).collect();
                names.sort_unstable();
                assert_eq!(names, vec!["josh", "marko", "peter", "vadas"]);
                let mut ages: Vec<i64> = res.iter().filter_map(|v| v.as_i64()).collect();
                ages.sort_unstable();
                assert_eq!(ages, vec![27, 29, 32, 35]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn edge_value() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").properties("weight").value().to_list(),
            rhai: [
                r#"g.E("1").properties("weight").value()"#,
                r#"g.e("1").properties("weight").value()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(0.4)),
        );
    }

    /// The full chain: `properties()` -> `dedup()` -> `hasKey()` -> `value()`,
    /// filtered by weight after resolution. `hasValue(P.lt(...))` was a pre-existing gap when
    /// this test was first written — `has_value()` only accepted a plain `ElementProperty`, not a
    /// `P` predicate — so this test used `.value().is(P.lt(0.3))` as a workaround, moving the
    /// predicate check after the resolution `hasValue(P...)` couldn't yet perform. That gap is
    /// now fixed (see `crates/graphersal/src/traversal/step/filter_steps/step_has_value.rs`'s
    /// `StepHasValueP`); this test is kept as-is to keep covering the `.value().is(P...)` path in
    /// its own right, and the no-workaround form now has its own test,
    /// `step_has_value::tests::roadmap_bothe_properties_dedup_haskey_hasvalue_predicate`.
    #[cfg(feature = "script")]
    #[test]
    fn full_chain_bothe_properties_dedup_haskey_value() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .both_e(None)
                .properties(None)
                .dedup()
                .has_key("weight")
                .value()
                .is(P::Lt(0.3))
                .to_list(),
            rhai: [
                r#"g.V().bothE().properties().dedup().hasKey("weight").value().is(P.lt(0.3))"#,
                r#"g.v().both_e().properties().dedup().has_key("weight").value().is(P.lt(0.3))"#,
            ],
            assert: |res| {
                let mut weights: Vec<f64> = res.iter().filter_map(|v| v.as_f64()).collect();
                weights.sort_by(|a, b| a.partial_cmp(b).unwrap());
                assert_eq!(weights, vec![0.2]);
            },
        );
    }

    #[test]
    fn on_bare_vertex_is_cast_exception_not_panic() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let err = g
            .v("1")
            .value()
            .next()
            .expect_err("a bare vertex is not a property object");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::CastException { .. })
        ));
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").properties("name").value().profile().unwrap();
        assert!(result.to_string().contains("value("));
    }
}
