use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepHasKey {
    key_list: Option<Vec<String>>,
}

impl fmt::Display for StepHasKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "has_key({})", display_option_vec_string(&self.key_list))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps properties whose key is one of the given keys.
    pub fn has_key<T: Into<ArgStrings>>(self, key_list: T) -> Self {
        let key_list = match key_list.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        self.push_step(GraphStep::Filter(
            "has_key",
            Box::new(StepHasKey { key_list }),
        ))
    }
}

impl __ {
    /// Keeps properties whose key is one of the given keys. Starts an anonymous traversal
    /// (`__.has_key()`).
    pub fn has_key<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        key_list: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_key(key_list)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Keeps properties whose key is one of the given keys.
    pub fn has_key<T: Into<ArgStrings>>(self, key_list: T) -> Self {
        let key_list = match key_list.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::Filter("has_key", Box::new(StepHasKey { key_list }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasKey {
    fn keys(&self) -> Option<&[String]> {
        self.key_list.as_deref()
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Vertex(node_index) => {
                let vertex = context.storage.vertex_properties(*node_index)?;
                match vertex {
                    Some(vertex) => match &self.key_list {
                        Some(key_list) => {
                            for key in key_list {
                                if vertex.contains_key(key.as_str()) {
                                    return Ok(true);
                                }
                            }
                            Ok(false)
                        }
                        None => Ok(false),
                    },
                    None => Ok(false),
                }
            }

            TraverserValue::Edge(edge_index) => match &self.key_list {
                Some(key_list) => {
                    let edge = context.storage.edge_properties(*edge_index)?;
                    match edge {
                        Some(edge) => {
                            for key in key_list {
                                if edge.contains_key(key.as_str()) {
                                    return Ok(true);
                                }
                            }
                            Ok(false)
                        }
                        None => Ok(false),
                    }
                }
                None => Ok(false),
            },

            // A property object's own key, from `properties()`/`values(key)`'s lazy reference —
            // distinct from the `Object` arm below, which looks for this key among an
            // already-materialized map's keys. This arm was previously missing even though the
            // catch-all's diagnostic already advertised `vertexProperty`/`edgeProperty` as
            // accepted; nothing produced a bare property-object traverser to exercise it before
            // `properties()` (the property/value/key/valueMap spec) existed.
            TraverserValue::VertexProperty(prop, _, _)
            | TraverserValue::EdgeProperty(prop, _, _) => match &self.key_list {
                Some(key_list) => Ok(key_list.iter().any(|k| k == prop.as_str())),
                None => Ok(false),
            },

            // A path property element (`properties(jpath(..))`): its key is the canonical path text.
            TraverserValue::PathProperty(prop) => match &self.key_list {
                Some(key_list) => Ok(key_list.iter().any(|k| k == prop.0.as_str())),
                None => Ok(false),
            },

            TraverserValue::Object(obj) => {
                match &self.key_list {
                    Some(key_list) => {
                        let keys: Vec<&TraverserValue<S>> = obj.keys().collect();
                        for key in keys {
                            match key {
                                TraverserValue::VertexProperty(prop, _, _)
                                | TraverserValue::EdgeProperty(prop, _, _) => {
                                    return Ok(key_list.iter().any(|k| k == prop.as_str()));
                                }
                                // A non-string key value is not a valid Gremlin property key —
                                // it simply doesn't match, so move on to the next key (D2).
                                TraverserValue::Computed(ElementProperty::String(value), _) => {
                                    return Ok(key_list.iter().any(|k| k == value.as_str()));
                                }
                                // An unexpected key kind (not a property key or a string) is
                                // out-of-spec input, not an engine bug — skip it (D2).
                                _ => {}
                            }
                        }
                        Ok(false)
                    }
                    None => Ok(false),
                }
            }

            TraverserValue::Record { keys, .. } => match &self.key_list {
                Some(key_list) => {
                    for k in keys.iter() {
                        if key_list.iter().any(|item| item == k.as_str()) {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
                None => Ok(false),
            },

            t => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "vertexProperty", "edge", "edgeProperty", "object"],
                t,
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
    fn v_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").has_key("lang").count().to_list(),
            rhai: [
                r#"g.V("1").hasKey("lang").count()"#,
                r#"g.v("1").has_key("lang").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn v_wrong_multiple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").has_key(vec!["lang", "wrong"]).count().to_list(),
            rhai: [
                r#"g.V("1").hasKey(["lang", "wrong"]).count()"#,
                r#"g.v("1").has_key(["lang", "wrong"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").has_key("name").count().to_list(),
            rhai: [
                r#"g.V("1").hasKey("name").count()"#,
                r#"g.v("1").has_key("name").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_where() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::has_key("name")).count().to_list(),
            rhai: [
                r#"g.V("1").where(__.hasKey("name")).count()"#,
                r#"g.v("1").where(__.has_key("name")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::element().has_key("name")).count().to_list(),
            rhai: [
                r#"g.V("1").where(__.element().hasKey("name")).count()"#,
                r#"g.v("1").where(__.element().has_key("name")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_multiple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").has_key(vec!["name", "wrong"]).count().to_list(),
            rhai: [
                r#"g.V("1").hasKey(["name", "wrong"]).count()"#,
                r#"g.v("1").has_key(["name", "wrong"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn e_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").has_key("lang").count().to_list(),
            rhai: [
                r#"g.E("1").hasKey("lang").count()"#,
                r#"g.e("1").has_key("lang").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn e_wrong_multiple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").has_key(vec!["lang", "wrong"]).count().to_list(),
            rhai: [
                r#"g.E("1").hasKey(["lang", "wrong"]).count()"#,
                r#"g.e("1").has_key(["lang", "wrong"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").has_key("weight").count().to_list(),
            rhai: [
                r#"g.E("1").hasKey("weight").count()"#,
                r#"g.e("1").has_key("weight").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn e_multiple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").has_key(vec!["weight", "wrong"]).count().to_list(),
            rhai: [
                r#"g.E("1").hasKey(["weight", "wrong"]).count()"#,
                r#"g.e("1").has_key(["weight", "wrong"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn obj_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out(None).as_("B").select(vec!["A", "B"]).has_key("C").count().to_list(),
            rhai: [
                r#"g.V("1").as("A").out().as("B").select(["A", "B"]).hasKey("C").count()"#,
                r#"g.v("1").as("A").out().as("B").select(["A", "B"]).has_key("C").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn obj_wrong_multiple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out(None).as_("B").select(vec!["A", "B"]).has_key(vec!["C", "D"]).count().to_list(),
            rhai: [
                r#"g.V("1").as("A").out().as("B").select(["A", "B"]).hasKey(["C", "D"]).count()"#,
                r#"g.v("1").as("A").out().as("B").select(["A", "B"]).has_key(["C", "D"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn obj() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out(None).as_("B").select(vec!["A", "B"]).has_key("A").count().to_list(),
            rhai: [
                r#"g.V("1").as("A").out().as("B").select(["A", "B"]).hasKey("A").count()"#,
                r#"g.v("1").as("A").out().as("B").select(["A", "B"]).has_key("A").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn obj_multiple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out(None).as_("B").select(vec!["A", "B"]).has_key(vec!["A", "B"]).count().to_list(),
            rhai: [
                r#"g.V("1").as("A").out().as("B").select(["A", "B"]).hasKey(["A", "B"]).count()"#,
                r#"g.v("1").as("A").out().as("B").select(["A", "B"]).has_key(["A", "B"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn obj_non_string_key_skipped_not_panic() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by("age").has_key("wrong").count().to_list(),
            rhai: [
                r#"g.V().group().by("age").hasKey("wrong").count()"#,
                r#"g.v().group().by("age").has_key("wrong").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn obj_unexpected_key_kind_skipped_not_panic() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(__::identity()).has_key("wrong").count().to_list(),
            rhai: [
                r#"g.V().group().by(__.identity()).hasKey("wrong").count()"#,
                r#"g.v().group().by(__.identity()).has_key("wrong").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    /// `has_key()` applied directly to a bare property-object traverser (from `properties()`,
    /// the property/value/key/valueMap spec), not wrapped in an `Object`/`Record` — filters by
    /// the property's own key. Regression for the previously-missing `VertexProperty`/
    /// `EdgeProperty` match arm.
    #[test]
    fn vertex_property_own_key() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").properties(None).has_key("age").key().to_list(),
            rhai: [
                r#"g.V("1").properties().hasKey("age").key()"#,
                r#"g.v("1").properties().has_key("age").key()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0], ElementProperty::String("age".into()));
            },
        );
    }

    #[test]
    fn edge_property_own_key_no_match() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").properties(None).has_key("lang").count().to_list(),
            rhai: [
                r#"g.E("1").properties().hasKey("lang").count()"#,
                r#"g.e("1").properties().has_key("lang").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").has_key("name").count().profile().unwrap();
        assert!(result.to_string().contains("has_key("));
    }
}
