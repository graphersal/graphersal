use super::step_values::emit_matching_property_refs;
use crate::graph::GraphStorage;
use crate::*;

/// `properties()`: for each vertex/edge traverser, emits one `TraverserValue::VertexProperty`/
/// `EdgeProperty` reference per matching property — every property with no key arguments, or
/// only the named ones when keys are given.
///
/// This is the pure-property-object counterpart to [`values()`](GraphTraversalSource::values):
/// `values(key)` resolves a matched property straight through to its scalar value, while
/// `properties(key)` stops at the reference itself, the same lazy shape `values()` produces
/// internally before it gets resolved (`flatmap_steps/step_values.rs`). That reference is what
/// `has_key()`/`has_value()` already filter on, and what [`value()`](GraphTraversalSource::value)/
/// [`key()`](GraphTraversalSource::key) (this spec's other two new steps) read from.
#[derive(Default, Clone)]
pub(super) struct StepProperties {
    pub(super) args: Option<Vec<String>>,
}

impl fmt::Display for StepProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "properties({})", display_option_vec_string(&self.args))
    }
}

/// The step for `properties(keys)`: the literal-name step when every key is a name, the
/// path-aware step as soon as one key is a path.
fn properties_step<'g, S: GraphStorage + 'g>(keys: PropKeys) -> GraphStep<'g, S> {
    match keys.into_names() {
        Ok(args) => GraphStep::FlatMap("properties", Box::new(StepProperties { args })),
        Err(keys) => super::step_values_path::properties_keys_step(keys),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// For each vertex/edge, the property object for every property (or only the named keys)
    /// rather than its resolved scalar value — see this module's own doc comment for how this
    /// differs from [`values()`](Self::values).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .properties(vec!["name", "age"])
    ///     .key()
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// ```
    ///
    /// The builder accepts a [`JsonPath`] key, but a path has no property
    /// object yet: executing it fails with a query-execution error (use `values(jpath)`).
    pub fn properties<T: Into<PropKeys>>(self, args: T) -> Self {
        self.push_step(properties_step(args.into()))
    }
}

impl __ {
    /// Anonymous `properties()`; see [`GraphTraversalSource::properties`].
    pub fn properties<'g, S: GraphStorage + 'g, T: Into<PropKeys>>(
        args: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().properties(args)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `properties()`; see [`GraphTraversalSource::properties`].
    pub fn properties<T: Into<PropKeys>>(self, args: T) -> Self {
        self.push_step(properties_step(args.into()))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepProperties {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        match &self.args {
            None => visit(None, crate::graph::workload::AccessKind::Read),
            Some(keys) => keys
                .iter()
                .for_each(|key| visit(Some(key), crate::graph::workload::AccessKind::Read)),
        }
    }
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        match source.value {
            TraverserValue::Vertex(node_index) => {
                if let Some(node) = context.storage.vertex_properties(node_index)? {
                    emit_matching_property_refs(
                        node,
                        &self.args,
                        source.path,
                        source.bulk,
                        source.sack,
                        output,
                        |key| {
                            TraverserValue::VertexProperty(
                                key,
                                node_index,
                                PropertyHandleKind::Element,
                            )
                        },
                    );
                }
                Ok(())
            }

            TraverserValue::Edge(edge_index) => {
                match context.storage.edge_properties(edge_index)? {
                    Some(edge) => {
                        emit_matching_property_refs(
                            edge,
                            &self.args,
                            source.path,
                            source.bulk,
                            source.sack,
                            output,
                            |key| {
                                TraverserValue::EdgeProperty(
                                    key,
                                    edge_index,
                                    PropertyHandleKind::Element,
                                )
                            },
                        );
                        Ok(())
                    }
                    // A removed edge has no properties (stale handle): nothing to emit.
                    None if !context.storage.contains_edge(edge_index)? => Ok(()),
                    None => Err(Box::new(TraverserError::Value(
                        ValueError::PropertyNotFound {
                            key: "properties".to_string(),
                            element_type: "edge",
                        },
                    ))),
                }
            }

            v => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge"],
                &v,
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v_all() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").properties(None).key().to_list(),
            rhai: [
                r#"g.V("1").properties().key()"#,
                r#"g.v("1").properties().key()"#,
            ],
            // marko (v1) has two properties: name and age.
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn v_named() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").properties(vec!["name", "age"]).value().to_list(),
            rhai: [
                r#"g.V("1").properties(["name", "age"]).value()"#,
                r#"g.v("1").properties(["name", "age"]).value()"#,
            ],
            assert: |res| {
                assert_eq!(res[0], ElementProperty::String("marko".into()));
                assert_eq!(res[1].as_i64(), Some(29));
            },
        );
    }

    #[test]
    fn e_simple() {
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

    #[test]
    fn vertex_cast_exception_not_panic() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let err = g
            .v("1")
            .values("name")
            .properties(None)
            .next()
            .expect_err("a resolved scalar has no properties of its own");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::CastException { .. })
        ));
    }

    #[test]
    fn profile_no_args_renders_empty_parens() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").properties(None).profile().unwrap();
        assert!(result.to_string().contains("properties()"));
        assert!(!result.to_string().contains("(None)"));
    }

    /// `properties()` is a lazy pass-through, like `values()` (`materializes() == false`): it
    /// must report zero bytes materialized under `.profile_with(ProfileType::Memory)`.
    #[test]
    fn profile_memory_is_zero_allocation() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let g = lock.traversal();
        let stats = g
            .v("1")
            .properties(None)
            .profile_with(ProfileType::Memory)
            .unwrap();
        let node = stats
            .metrics()
            .iter()
            .find(|n| n.name().contains("properties"))
            .expect("a properties() node is present");
        assert_eq!(node.memory().map(|m| m.bytes_materialized()), Some(0));
    }
}
