use crate::graph::GraphStorage;
use crate::*;

/// `has(key)`: TinkerPop's element-level existence filter — keeps an element (or bare
/// property/computed value) that carries the given property key, regardless of its value.
///
/// This is deliberately **not** the same thing as [`super::step_has_key::StepHasKey`]
/// (`hasKey(key)`), even though both end up checking "does this key exist" on this engine's own
/// `Vertex`/`Edge` traversers. In vanilla TinkerPop, `hasKey(String...)` is a **property**-level
/// step: it is implemented as `has(T.key, ...)`, and `T.key` only resolves on a `Property`/
/// `VertexProperty` object — calling it directly on a `Vertex`/`Edge` traverser throws a
/// `ClassCastException` there. `has(String key)`, by contrast, is the element-level existence
/// check and is the vanilla counterpart of what `g.V().has("age")` means: "keep vertices that
/// have an `age` property". This engine's `has_key()` already accepts `Vertex`/`Edge` traversers
/// too (kept as-is; it has its own tests and callers), so the two now overlap in what they accept,
/// but they stay distinct steps/methods so a future stricter `hasKey()` (property-object-only)
/// doesn't have to drag this one along, and so `.profile()`/error text can keep telling them apart.
#[derive(Clone)]
struct StepHasProperty {
    key: String,
}

impl fmt::Display for StepHasProperty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "has({})", crate::value::literal::rhai_string(&self.key))
    }
}

/// The step for `has(key)`: the literal-name step for a name, the path step for a path.
fn has_property_step<'g, S: GraphStorage + 'g>(key: PropKey) -> GraphStep<'g, S> {
    match key {
        PropKey::Name(name) => GraphStep::Filter(
            "has_property",
            Box::new(StepHasProperty {
                key: name.to_string(),
            }),
        ),
        PropKey::Path(path) => super::step_has_path::has_path_exists_step(path),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `has(key)`: keeps elements (or bare property/computed values) that carry `key`, regardless
    /// of value. See this module's doc comment (on the private `StepHasProperty` step type) for
    /// why this is a distinct step from `has_key()`.
    ///
    /// `key` may be a [`JsonPath`]: the element is kept when the path
    /// exists (even when it ends on `null`).
    pub fn has_property(self, key: impl Into<PropKey>) -> Self {
        self.push_step(has_property_step(key.into()))
    }
}

impl __ {
    /// `has(key)`, the anonymous-traversal counterpart of
    /// [`GraphTraversalSource::has_property`].
    pub fn has_property<'g, S: GraphStorage + 'g>(
        key: impl Into<PropKey>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_property(key)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `has(key)`, the anonymous-traversal counterpart of
    /// [`GraphTraversalSource::has_property`].
    pub fn has_property(self, key: impl Into<PropKey>) -> Self {
        self.push_step(has_property_step(key.into()))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasProperty {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        visit(
            Some(&self.key),
            crate::graph::workload::AccessKind::FilterScan,
        );
    }
    fn keys(&self) -> Option<&[String]> {
        Some(std::slice::from_ref(&self.key))
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Vertex(v) => {
                let vertex = context.storage.vertex_properties(*v)?;
                match vertex {
                    Some(vertex) => Ok(vertex.contains_key(self.key.as_str())),
                    None => Ok(false),
                }
            }

            TraverserValue::Computed(vertex, _) => match vertex {
                ElementProperty::Object(vertex) => Ok(vertex.contains_key(self.key.as_str())),
                x => Err(Box::new(TraverserError::cast_exception_str(
                    &["object"],
                    crate::graph::schema::value_type_name(x),
                ))),
            },

            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let prop_name = prop_name.clone();
                let vertex = context.storage.vertex_properties(*node_index)?;
                match vertex {
                    Some(vertex) => match vertex.get(&prop_name) {
                        Some(value) => match value {
                            ElementProperty::Object(vertex) => {
                                Ok(vertex.contains_key(self.key.as_str()))
                            }
                            x => Err(Box::new(TraverserError::cast_exception_str(
                                &["object"],
                                crate::graph::schema::value_type_name(x),
                            ))),
                        },
                        None => Err(Box::new(TraverserError::property_not_found_on_element(
                            prop_name, "vertex",
                        ))),
                    },
                    None => Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name, "vertex",
                    ))),
                }
            }

            TraverserValue::EdgeProperty(prop_name, edge_index, _) => {
                let prop_name = prop_name.clone();
                let edge = context.storage.edge_properties(*edge_index)?;
                match edge {
                    Some(edge) => match edge.get(&prop_name) {
                        Some(value) => match value {
                            ElementProperty::Object(edge) => {
                                Ok(edge.contains_key(self.key.as_str()))
                            }
                            x => Err(Box::new(TraverserError::cast_exception_str(
                                &["object"],
                                crate::graph::schema::value_type_name(x),
                            ))),
                        },
                        None => Err(Box::new(TraverserError::property_not_found_on_element(
                            prop_name, "edge",
                        ))),
                    },
                    None => Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name, "edge",
                    ))),
                }
            }

            TraverserValue::Edge(edge_index) => {
                let edge_map = context.storage.edge_properties(*edge_index)?;
                match edge_map {
                    Some(edge_map) => Ok(edge_map.contains_key(self.key.as_str())),
                    None => Err(Box::new(TraverserError::cast_exception_str(
                        &["edge"],
                        "null".to_string(),
                    ))),
                }
            }

            TraverserValue::Object(obj) => {
                let key_tv = TraverserValue::Computed(
                    ElementProperty::String(CompactString::new(&self.key)),
                    None,
                );
                Ok(obj.get(&key_tv).is_some())
            }

            TraverserValue::Record { keys, .. } => {
                Ok(keys.iter().any(|k| k.as_str() == self.key.as_str()))
            }

            TraverserValue::Path(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "path".to_string(),
            ))),
            TraverserValue::Array(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "array".to_string(),
            ))),
            kind @ (TraverserValue::PathProperty(_) | TraverserValue::Entry(_)) => {
                Err(Box::new(TraverserError::cast_exception_str(
                    &[
                        "computed",
                        "vertexProperty",
                        "edgeProperty",
                        "vertex",
                        "edge",
                    ],
                    kind.type_name().to_string(),
                )))
            }
            TraverserValue::Schema(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "schema".to_string(),
            ))),
            TraverserValue::Graph(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "graph".to_string(),
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn v_has_age() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_property("age").count().to_list(),
            rhai: [
                r#"g.V().has("age").count()"#,
                r#"g.v().has("age").count()"#,
            ],
            // marko, vadas, josh, peter
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_missing_key() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_property("wrong").count().to_list(),
            rhai: [
                r#"g.V().has("wrong").count()"#,
                r#"g.v().has("wrong").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_has_weight() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_property("weight").count().to_list(),
            rhai: [
                r#"g.E().has("weight").count()"#,
                r#"g.e().has("weight").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::has_property("age")).count().to_list(),
            rhai: [
                r#"g.V().where(__.has("age")).count()"#,
                r#"g.v().where(__.has("age")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().has_property("age")).count().to_list(),
            rhai: [
                r#"g.V().where(__.element().has("age")).count()"#,
                r#"g.v().where(__.element().has("age")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).has_property("age").profile().unwrap();
        assert!(result.to_string().contains("has(\"age\")"));
    }

    #[cfg(feature = "script")]
    use crate::traversal::test_utils::nested_doc_graph;

    #[cfg(feature = "script")]
    #[test]
    fn nested_key_existence_inside_object_valued_properties() {
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("doc").properties("meta").has_property("a").count().to_list(),
            rhai: [
                r#"g.V().hasLabel("doc").properties("meta").has("a").count()"#,
                r#"g.v().has_label("doc").properties("meta").has("a").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_property("w").count().to_list(),
            rhai: [r#"g.E().properties("info").has("w").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_property("zz").count().to_list(),
            rhai: [r#"g.E().properties("info").has("zz").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual_err!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("plain").properties("meta").has_property("a").to_list(),
            rhai: [r#"g.V().hasLabel("plain").properties("meta").has("a")"#],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
        test_dual_err!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("note").has_property("a").to_list(),
            rhai: [r#"g.E().properties("note").has("a")"#],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn edges_and_maps_are_kept_by_key_existence() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_property("weight").count().to_list(),
            rhai: [r#"g.E().has("weight").count()"#, r#"g.e().has("weight").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).value_map(None).has_property("lang").count().to_list(),
            rhai: [r#"g.V().valueMap().has("lang").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).project(vec!["a"]).by("age").has_property("a").count().to_list(),
            rhai: [r#"g.V().project("a").by("age").has("a").count()"#],
            // project() leaves out a key whose by() produced nothing (lop, ripple)
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
        test_dual!(
            graph: GraphSource::empty(),
            rust: |g| g
                .inject(vec![ElementProperty::from_entries(vec![("a".into(), 1.into())])])
                .has_property("a")
                .count()
                .to_list(),
            rhai: [r#"g.inject(#{a: 1}).has("a").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn non_element_inputs_are_cast_errors() {
        test_dual_err!(
            graph: GraphSource::empty(),
            rust: |g| g.inject(vec![5]).has_property("a").to_list(),
            rhai: [r#"g.inject(5).has("a")"#],
            assert: |err| assert!(err.contains("got type 'int64'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").path().has_property("a").to_list(),
            rhai: [r#"g.V("1").path().has("a")"#],
            assert: |err| assert!(err.contains("got type 'path'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").fold().has_property("a").to_list(),
            rhai: [r#"g.V("1").fold().has("a")"#],
            assert: |err| assert!(err.contains("got type 'array'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(None).unfold().has_property("a").to_list(),
            rhai: [r#"g.V("1").valueMap().unfold().has("a")"#],
            assert: |err| assert!(err.contains("got type 'entry'"), "{err}"),
        );
    }
}
