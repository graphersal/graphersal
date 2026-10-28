use crate::graph::GraphStorage;
use crate::*;

/// `key()`: the property key name of a `VertexProperty`/`EdgeProperty` traverser, as a plain
/// string — the read half of the pair with [`value()`](GraphTraversalSource::value). The key is
/// already carried in the reference's own first field (`properties()`/`values(key)` construct it
/// as `VertexProperty(key, ...)`/`EdgeProperty(key, ...)`, `flatmap_steps/step_values.rs`), so
/// this never touches storage, unlike `value()`.
#[derive(Default, Clone)]
struct StepKey {}

impl fmt::Display for StepKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "key()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// The key name of a property-object traverser (produced by
    /// [`properties()`](Self::properties) or a raw [`values()`](Self::values) reference), as a
    /// string. Errors (not panics, rule 1) when the traverser isn't a property object at all —
    /// e.g. called directly on a bare vertex without going through `properties()`/`values()`
    /// first, matching vanilla Gremlin's own behaviour.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let keys = lock.traversal().v("1").properties(None).key().to_list().unwrap();
    /// assert_eq!(keys.len(), 2);
    /// ```
    pub fn key(self) -> Self {
        self.push_step(GraphStep::Map("key", Box::new(StepKey::default())))
    }
}

impl __ {
    /// Anonymous `key()`; see [`GraphTraversalSource::key`].
    pub fn key<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().key()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `key()`; see [`GraphTraversalSource::key`].
    pub fn key(self) -> Self {
        let step = GraphStep::Map("key", Box::new(StepKey::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepKey {
    #[inline]
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        match &source.value {
            TraverserValue::PathProperty(prop) => Ok(Traverser::with_parts(
                TraverserValue::Computed(ElementProperty::String(prop.0.clone()), None),
                source.path,
                source.bulk,
                source.sack,
            )),
            TraverserValue::VertexProperty(key, _, _) | TraverserValue::EdgeProperty(key, _, _) => {
                Ok(Traverser::with_parts(
                    TraverserValue::Computed(ElementProperty::String(key.clone()), None),
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

    /// `g.V().properties().key()`: the correct multiset of vertex property
    /// key names across all six `tinkerpop_modern` vertices — four `person`s (`name`, `age`
    /// each) and two `software`s (`name`, `lang` each): six `name`s, four `age`s, two `lang`s.
    #[cfg(feature = "script")]
    #[test]
    fn v_all_keys() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).properties(None).key().to_list(),
            rhai: [
                r#"g.V().properties().key()"#,
                r#"g.v().properties().key()"#,
            ],
            assert: |res| {
                let mut keys: Vec<&str> = res.iter().map(|v| v.as_string().unwrap()).collect();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    vec!["age", "age", "age", "age", "lang", "lang", "name", "name", "name", "name", "name", "name"]
                );
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
            .key()
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
        let result = g.v("1").properties("name").key().profile().unwrap();
        assert!(result.to_string().contains("key("));
    }
}
