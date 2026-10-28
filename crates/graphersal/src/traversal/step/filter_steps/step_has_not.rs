use crate::graph::GraphStorage;
use crate::*;

/// `hasNot(key)`: TinkerPop's element-level negated-existence filter — keeps an element (or bare
/// property/computed value) that does **not** carry the given property key. The exact opposite of
/// [`super::step_has_property::StepHasProperty`] (`has(key)`); see that step's doc comment for why
/// the element-level `has`/`hasNot` pair is kept distinct from the property-level `has_key()`.
///
/// A genuine lookup error (e.g. the traverser's shape can't hold a keyed property at all) still
/// propagates as an error rather than being folded into "not present" — only an actual absence of
/// the key counts as a match.
#[derive(Clone)]
struct StepHasNot {
    key: String,
}

impl fmt::Display for StepHasNot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "has_not({})",
            crate::value::literal::rhai_string(&self.key)
        )
    }
}

/// The step for `hasNot(key)`: the literal-name step for a name, the path step for a path.
fn has_not_step<'g, S: GraphStorage + 'g>(key: PropKey) -> GraphStep<'g, S> {
    match key {
        PropKey::Name(name) => GraphStep::Filter(
            "has_not",
            Box::new(StepHasNot {
                key: name.to_string(),
            }),
        ),
        PropKey::Path(path) => super::step_has_path::has_not_path_step(path),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `hasNot(key)`: keeps elements (or bare property/computed values) that do not carry `key`.
    ///
    /// `key` may be a [`JsonPath`]: the element is kept when the path
    /// does not exist.
    pub fn has_not(self, key: impl Into<PropKey>) -> Self {
        self.push_step(has_not_step(key.into()))
    }
}

impl __ {
    /// `hasNot(key)`, the anonymous-traversal counterpart of [`GraphTraversalSource::has_not`].
    pub fn has_not<'g, S: GraphStorage + 'g>(key: impl Into<PropKey>) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_not(key)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `hasNot(key)`, the anonymous-traversal counterpart of [`GraphTraversalSource::has_not`].
    pub fn has_not(self, key: impl Into<PropKey>) -> Self {
        self.push_step(has_not_step(key.into()))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasNot {
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
                    Some(vertex) => Ok(!vertex.contains_key(self.key.as_str())),
                    None => Ok(true),
                }
            }

            TraverserValue::Computed(vertex, _) => match vertex {
                ElementProperty::Object(vertex) => Ok(!vertex.contains_key(self.key.as_str())),
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
                                Ok(!vertex.contains_key(self.key.as_str()))
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
                                Ok(!edge.contains_key(self.key.as_str()))
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
                    Some(edge_map) => Ok(!edge_map.contains_key(self.key.as_str())),
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
                Ok(obj.get(&key_tv).is_none())
            }

            TraverserValue::Record { keys, .. } => {
                Ok(!keys.iter().any(|k| k.as_str() == self.key.as_str()))
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
    fn v_has_not_age() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_not("age").count().to_list(),
            rhai: [
                r#"g.V().hasNot("age").count()"#,
                r#"g.v().has_not("age").count()"#,
            ],
            // lop, ripple (software vertices, no age)
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_has_not_wrong_key() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_not("wrong").count().to_list(),
            rhai: [
                r#"g.V().hasNot("wrong").count()"#,
                r#"g.v().has_not("wrong").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_has_not_weight() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_not("weight").count().to_list(),
            rhai: [
                r#"g.E().hasNot("weight").count()"#,
                r#"g.e().has_not("weight").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::has_not("age")).count().to_list(),
            rhai: [
                r#"g.V().where(__.hasNot("age")).count()"#,
                r#"g.v().where(__.has_not("age")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).has_not("age").profile().unwrap();
        assert!(result.to_string().contains("has_not(\"age\")"));
    }

    #[cfg(feature = "script")]
    fn obj_a1() -> ElementProperty {
        ElementProperty::from_entries(vec![(ElementProperty::from("a"), ElementProperty::from(1))])
    }

    #[cfg(feature = "script")]
    #[test]
    fn nested_vertex_property_key_is_checked_inside_an_object_value() {
        // d1.meta = {a, b}, d2.meta = {b}: only d2's meta lacks the nested key `a`.
        test_dual!(
            graph: crate::traversal::test_utils::nested_doc_graph(),
            rust: |g| g.v(None).has_label("doc").properties("meta").has_not("a").to_list(),
            rhai: [
                r#"g.V().hasLabel("doc").properties("meta").hasNot("a")"#,
                r#"g.v().has_label("doc").properties("meta").has_not("a")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_object().and_then(|o| o.get("b")), Some(&"y".into()));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn nested_edge_property_key_is_checked_inside_an_object_value() {
        test_dual!(
            graph: crate::traversal::test_utils::nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_not("zz").count().to_list(),
            rhai: [
                r#"g.E().properties("info").hasNot("zz").count()"#,
                r#"g.e().properties("info").has_not("zz").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: crate::traversal::test_utils::nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_not("w").count().to_list(),
            rhai: [
                r#"g.E().properties("info").hasNot("w").count()"#,
                r#"g.e().properties("info").has_not("w").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_scalar_property_value_has_no_nested_keys_and_is_a_cast_error() {
        test_dual_err!(
            graph: crate::traversal::test_utils::nested_doc_graph(),
            rust: |g| g.v(None).has_label("plain").properties("meta").has_not("a").to_list(),
            rhai: [
                r#"g.V().hasLabel("plain").properties("meta").hasNot("a")"#,
                r#"g.v().has_label("plain").properties("meta").has_not("a")"#,
            ],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
        test_dual_err!(
            graph: crate::traversal::test_utils::nested_doc_graph(),
            rust: |g| g.e(None).properties("note").has_not("a").to_list(),
            rhai: [
                r#"g.E().properties("note").hasNot("a")"#,
                r#"g.e().properties("note").has_not("a")"#,
            ],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn maps_keep_or_drop_by_their_keys() {
        // valueMap() (a record) and project() / inject() (objects) are filtered by their keys.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).value_map(None).has_not("age").count().to_list(),
            rhai: [
                r#"g.V().valueMap().hasNot("age").count()"#,
                r#"g.v().value_map().has_not("age").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").project("n").by("name").has_not("n").count().to_list(),
            rhai: [
                r#"g.V("1").project("n").by("name").hasNot("n").count()"#,
                r#"g.v("1").project("n").by("name").has_not("n").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: GraphSource::empty(),
            rust: |g| g.inject(vec![obj_a1()]).has_not("b").to_list(),
            rhai: [
                r#"g.inject(#{a: 1}).hasNot("b")"#,
                r#"g.inject(#{a: 1}).has_not("b")"#,
            ],
            assert: |res| assert_eq!(res, &[obj_a1()]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn non_element_inputs_are_cast_errors() {
        test_dual_err!(
            graph: GraphSource::empty(),
            rust: |g| g.inject(vec![5]).has_not("a").to_list(),
            rhai: [r#"g.inject(5).hasNot("a")"#, r#"g.inject(5).has_not("a")"#],
            assert: |err| assert!(err.contains("got type 'int64'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").path().has_not("a").to_list(),
            rhai: [r#"g.V("1").path().hasNot("a")"#, r#"g.v("1").path().has_not("a")"#],
            assert: |err| assert!(err.contains("got type 'path'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").fold().has_not("a").to_list(),
            rhai: [r#"g.V("1").fold().hasNot("a")"#, r#"g.v("1").fold().has_not("a")"#],
            assert: |err| assert!(err.contains("got type 'array'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(None).unfold().has_not("a").to_list(),
            rhai: [
                r#"g.V("1").valueMap().unfold().hasNot("a")"#,
                r#"g.v("1").value_map().unfold().has_not("a")"#,
            ],
            assert: |err| assert!(err.contains("got type 'entry'"), "{err}"),
        );
    }
}
