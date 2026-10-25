use crate::graph::GraphStorage;
use crate::*;

#[derive(Clone)]
struct StepHas {
    key: String,
    value: ElementProperty,
}

impl fmt::Display for StepHas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "has({}, {})",
            crate::value::literal::rhai_string(&self.key),
            crate::value::literal::rhai_literal(&self.value)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `has(key, value)`. `key` is a property name (a `&str`/`String` is always a literal name)
    /// or a [`JsonPath`] into a nested property value; a path that does
    /// not exist filters the element out, it is not an error. A path key is evaluated by the
    /// path step in `step_has_path.rs` and is never pushed into an index or a source step.
    pub fn has<K: Into<PropKey>, T: Into<ElementProperty>>(self, field: K, value: T) -> Self {
        self.push_step(has_step(field.into(), value.into()))
    }
}

impl __ {
    /// Keeps elements that have the property (and, with a value or predicate, whose value matches);
    /// the 3-argument form also tests the label. Starts an anonymous traversal (`__.has()`).
    pub fn has<'g, S: GraphStorage + 'g, K: Into<PropKey>, T: Into<ElementProperty>>(
        field: K,
        value: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has(field, value)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `has(key, value)`; see [`GraphTraversalSource::has`].
    pub fn has<K: Into<PropKey>, T: Into<ElementProperty>>(self, field: K, value: T) -> Self {
        self.push_step(has_step(field.into(), value.into()))
    }
}

/// The step for `has(key, value)`: the literal-name step for a name, the path step for a path.
fn has_step<'g, S: GraphStorage + 'g>(key: PropKey, value: ElementProperty) -> GraphStep<'g, S> {
    match key {
        PropKey::Name(name) => GraphStep::Filter(
            "has",
            Box::new(StepHas {
                key: name.to_string(),
                value,
            }),
        ),
        PropKey::Path(path) => super::step_has_path::has_path_step(path, value),
    }
}

/// Whether vertex `node` has the property `key` equal to `value` — the equality of
/// `has(key, value)`, shared with the lookup of `mergeV()`.
pub(crate) fn vertex_has<S: GraphStorage>(
    storage: &S,
    node: S::VertexId,
    key: &str,
    value: &ElementProperty,
) -> bool {
    storage
        .vertex_properties(node)
        .and_then(|properties| properties.get(key))
        .is_some_and(|stored| stored.eq(value))
}

/// Whether edge `edge` has the property `key` equal to `value` — the equality of
/// `has(key, value)`, shared with the lookup of `mergeE()`.
pub(crate) fn edge_has<S: GraphStorage>(
    storage: &S,
    edge: S::EdgeId,
    key: &str,
    value: &ElementProperty,
) -> bool {
    storage
        .edge_properties(edge)
        .and_then(|properties| properties.get(key))
        .is_some_and(|stored| stored.eq(value))
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHas {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        visit(
            Some(&self.key),
            crate::graph::workload::AccessKind::FilterScan,
        );
    }
    fn property_equality(&self) -> Option<crate::traversal::step::PropertyFilter> {
        Some(crate::traversal::step::PropertyFilter {
            key: self.key.clone(),
            value: self.value.clone(),
        })
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Vertex(v) => {
                Ok(vertex_has(context.storage, *v, &self.key, &self.value))
            }

            TraverserValue::Computed(vertex, _) => match vertex {
                ElementProperty::Object(vertex) => {
                    let value = vertex.get(self.key.as_str());
                    if let Some(value) = value {
                        Ok(value.eq(&self.value))
                    } else {
                        Ok(false)
                    }
                }
                x => Err(Box::new(TraverserError::cast_exception_str(
                    &["object"],
                    crate::graph::schema::value_type_name(x),
                ))),
            },

            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let prop_name = prop_name.clone();
                let vertex = context.storage.vertex_properties(*node_index);
                if let Some(vertex) = vertex {
                    let value = vertex.get(&prop_name);
                    match value {
                        Some(value) => match value {
                            ElementProperty::Object(vertex) => {
                                let value = vertex.get(self.key.as_str());
                                if let Some(value) = value {
                                    Ok(value.eq(&self.value))
                                } else {
                                    Ok(false)
                                }
                            }
                            x => Err(Box::new(TraverserError::cast_exception_str(
                                &["object"],
                                crate::graph::schema::value_type_name(x),
                            ))),
                        },
                        None => Err(Box::new(TraverserError::property_not_found_on_element(
                            prop_name, "vertex",
                        ))),
                    }
                } else {
                    Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name, "vertex",
                    )))
                }
            }

            TraverserValue::EdgeProperty(prop_name, edge_index, _) => {
                let prop_name = prop_name.clone();
                let edge = context.storage.edge_properties(*edge_index);
                match edge {
                    Some(edge) => {
                        let value = edge.get(&prop_name);
                        match value {
                            Some(value) => match value {
                                ElementProperty::Object(edge) => {
                                    let value = edge.get(self.key.as_str());
                                    if let Some(value) = value {
                                        Ok(value.eq(&self.value))
                                    } else {
                                        Ok(false)
                                    }
                                }
                                x => Err(Box::new(TraverserError::cast_exception_str(
                                    &["object"],
                                    crate::graph::schema::value_type_name(x),
                                ))),
                            },
                            None => Err(Box::new(TraverserError::property_not_found_on_element(
                                prop_name, "edge",
                            ))),
                        }
                    }
                    None => Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name, "edge",
                    ))),
                }
            }

            TraverserValue::Edge(edge_index) => {
                let edge_map = context.storage.edge_properties(*edge_index);
                match edge_map {
                    Some(edge_map) => {
                        let value: Option<_> = edge_map.get(self.key.as_str());
                        if let Some(value) = value {
                            Ok(value.eq(&self.value))
                        } else {
                            Ok(false)
                        }
                    }
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
                if let Some(val) = obj.get(&key_tv) {
                    let ep = val.into_node_property(context.storage)?;
                    Ok(ep.eq(&self.value))
                } else {
                    Ok(false)
                }
            }

            TraverserValue::Record { keys, values } => {
                if let Some(idx) = keys.iter().position(|k| k.as_str() == self.key.as_str()) {
                    let val = values[idx].into_node_property(context.storage)?;
                    Ok(val.eq(&self.value))
                } else {
                    Ok(false)
                }
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

    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has("lang", "java").count().to_list(),
            rhai: [
                r#"g.V().has("lang", "java").count()"#,
                r#"g.v().has("lang", "java").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn v_bool() {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property("deleted", true)
            .next()
            .unwrap();

        test_dual!(
            graph: graph,
            rust: |g| g.v(None).has("deleted", true).count().to_list(),
            rhai: [
                r#"g.V().has("deleted", true).count()"#,
                r#"g.v().has("deleted", true).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_where() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::has("lang", "java")).count().to_list(),
            rhai: [
                r#"g.V().where(__.has("lang", "java")).count()"#,
                r#"g.v().where(__.has("lang", "java")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn v_where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().has("lang", "java")).count().to_list(),
            rhai: [
                r#"g.V().where(__.element().has("lang", "java")).count()"#,
                r#"g.v().where(__.element().has("lang", "java")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn v_number() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has("age", 29).id().to_list(),
            rhai: [
                r#"g.V().has("age", 29).id()"#,
                r#"g.v().has("age", 29).id()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("1"));
            },
        );
    }

    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has("weight", 0.2).label().to_list(),
            rhai: [
                r#"g.E().has("weight", 0.2).label()"#,
                r#"g.e().has("weight", 0.2).label()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("created"));
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).has("age", 29).id().profile().unwrap();
        assert!(result.to_string().contains("has("));
    }

    use crate::traversal::test_utils::nested_doc_graph;

    #[test]
    fn nested_key_of_an_object_valued_property_is_compared() {
        // The nested `a` (1) is compared, not d1's top-level `a` (100).
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("doc").properties("meta").has("a", 1).count().to_list(),
            rhai: [
                r#"g.V().hasLabel("doc").properties("meta").has("a", 1).count()"#,
                r#"g.v().has_label("doc").properties("meta").has("a", 1).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("doc").properties("meta").has("a", 100).count().to_list(),
            rhai: [
                r#"g.V().hasLabel("doc").properties("meta").has("a", 100).count()"#,
                r#"g.v().has_label("doc").properties("meta").has("a", 100).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has("w", 2).count().to_list(),
            rhai: [
                r#"g.E().properties("info").has("w", 2).count()"#,
                r#"g.e().properties("info").has("w", 2).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has("missing", 2).count().to_list(),
            rhai: [
                r#"g.E().properties("info").has("missing", 2).count()"#,
                r#"g.e().properties("info").has("missing", 2).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn scalar_property_values_are_cast_errors() {
        test_dual_err!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("plain").properties("meta").has("a", 1).to_list(),
            rhai: [
                r#"g.V().hasLabel("plain").properties("meta").has("a", 1)"#,
                r#"g.v().has_label("plain").properties("meta").has("a", 1)"#,
            ],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
        test_dual_err!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("note").has("a", 1).to_list(),
            rhai: [
                r#"g.E().properties("note").has("a", 1)"#,
                r#"g.e().properties("note").has("a", 1)"#,
            ],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::empty(),
            rust: |g| g.inject(vec![5]).has("a", 1).to_list(),
            rhai: [r#"g.inject(5).has("a", 1)"#],
            assert: |err| assert!(err.contains("got type 'int64'"), "{err}"),
        );
    }

    #[test]
    fn maps_compare_the_value_under_the_key() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).project(vec!["n", "a"]).by("name").by("age").has("n", "josh").to_list(),
            rhai: [
                r#"g.V().project("n", "a").by("name").by("age").has("n", "josh")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_object().and_then(|o| o.get("a")), Some(&32.into()));
            },
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).element_map(None).has("name", "lop").count().to_list(),
            rhai: [
                r#"g.V().elementMap().has("name", "lop").count()"#,
                r#"g.v().element_map().has("name", "lop").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).element_map(None).has("nope", "lop").count().to_list(),
            rhai: [r#"g.V().elementMap().has("nope", "lop").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: GraphSource::empty(),
            rust: |g| g
                .inject(vec![ElementProperty::from_entries(vec![("a".into(), 1.into())])])
                .has("a", 1)
                .count()
                .to_list(),
            rhai: [r#"g.inject(#{a: 1}).has("a", 1).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn non_element_inputs_are_cast_errors() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").path().has("a", 1).to_list(),
            rhai: [r#"g.V("1").path().has("a", 1)"#],
            assert: |err| assert!(err.contains("got type 'path'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").fold().has("a", 1).to_list(),
            rhai: [r#"g.V("1").fold().has("a", 1)"#],
            assert: |err| assert!(err.contains("got type 'array'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(None).unfold().has("a", 1).to_list(),
            rhai: [r#"g.V("1").valueMap().unfold().has("a", 1)"#],
            assert: |err| assert!(err.contains("got type 'entry'"), "{err}"),
        );
    }
}
