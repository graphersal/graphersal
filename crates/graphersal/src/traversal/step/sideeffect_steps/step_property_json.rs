use serde_json::Value;

use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepPropertyJson {
    json: Value,
}

impl fmt::Display for StepPropertyJson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "property_json({})",
            crate::value::literal::rhai_string(&self.json.to_string())
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Sets every key of the given map (or JSON object) as a property of the incoming vertex
    /// (Graphersal extension).
    pub fn property_json(self, json: Value) -> Self {
        self.push_step(GraphStep::SideEffectMut(
            "property_json",
            Box::new(StepPropertyJson { json }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Sets every key of the given map (or JSON object) as a property of the incoming vertex
    /// (Graphersal extension).
    pub fn property_json(self, json: Value) -> Self {
        self.push_step(GraphStep::SideEffectMut(
            "property_json",
            Box::new(StepPropertyJson { json }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepPropertyJson {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        if let Some(object) = self.json.as_object() {
            object
                .keys()
                .for_each(|key| visit(Some(key), crate::graph::workload::AccessKind::Write));
        }
    }
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        match source {
            Some(source) => match &source.value {
                TraverserValue::Vertex(node_index) => {
                    match &self.json {
                        Value::Object(json) => {
                            for (key, prop) in json.iter() {
                                let value = json_to_element_property(prop)?;
                                context.storage.set_vertex_property(
                                    *node_index,
                                    key.clone().into(),
                                    value,
                                )?;
                            }
                        }
                        t => {
                            return Err(Box::new(TraverserError::cast_exception_str(
                                &["object"],
                                json_value_type_name(t).to_string(),
                            )));
                        }
                    }
                    Ok(vec![source])
                }
                t => Err(Box::new(TraverserError::cast_exception(&["vertex"], t))),
            },
            None => Err(Box::new(TraverserError::cast_exception_str(
                &["vertex", "edge"],
                "null".to_string(),
            ))),
        }
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// Converts a raw JSON value into an [`ElementProperty`] through the physical codec: every JSON
/// type becomes the logical type of the same name, and `Array`/`Object` map onto the first-class
/// `ElementProperty::Array`/`Object` variants (design decision D3).
///
/// A JSON integer that is a valid `u64` but exceeds `i64::MAX` is rejected with a
/// `CastException` instead of silently losing precision by downcasting to `f64`.
fn json_to_element_property(value: &Value) -> TraverserResult<ElementProperty> {
    let physical = crate::value::physical::from_serde_json(value).map_err(|err| {
        Box::new(TraverserError::cast_exception_str(
            &["int64", "float64"],
            err.to_string(),
        ))
    })?;
    Ok(crate::value::physical::lift_untyped(physical))
}

/// Names a raw JSON value's kind for a diagnostic.
///
/// JSON has its own vocabulary at this boundary: the value has not been mapped onto an
/// [`ElementProperty`](crate::value::ElementProperty) yet, so [`type_of`](crate::graph::type_of)
/// does not apply to it.
fn json_value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn simple() {
        let graph = TraversalGraph::tinkerpop_modern();
        let result = graph.traversal().v("1").values("color").next().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn simple_string() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"color": "red"}))
            .values("color")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result, ElementProperty::String("red".into()));
    }

    #[test]
    fn simple_float() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"weight": 57.4}))
            .values("weight")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_f64(), Some(57.4));
    }

    #[test]
    fn simple_integer() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"count": 10}))
            .values("count")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(10));
    }

    #[test]
    fn bool_and_null() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"active": true, "nickname": null}))
            .element_map(None)
            .next()
            .unwrap()
            .unwrap();
        let map = result.as_object().unwrap();
        assert_eq!(map.get("active"), Some(&ElementProperty::Boolean(true)));
        assert_eq!(map.get("nickname"), Some(&ElementProperty::Null));
    }

    #[test]
    fn nested_array_and_object() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"tags": ["x", "y"], "meta": {"k": 1}}))
            .element_map(None)
            .next()
            .unwrap()
            .unwrap();
        let map = result.as_object().unwrap();
        assert_eq!(
            map.get("tags"),
            Some(&ElementProperty::Array(vec![
                ElementProperty::String("x".into()),
                ElementProperty::String("y".into()),
            ]))
        );
        let meta = map.get("meta").unwrap().as_object().unwrap();
        assert_eq!(meta.get("k").and_then(|v| v.as_i64()), Some(1));
    }

    #[test]
    fn u64_overflow_is_cast_exception_not_panic() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let err = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"n": 18446744073709551615u64}))
            .next()
            .expect_err("a u64 beyond i64::MAX must not silently downcast to f64");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::CastException { .. })
        ));
    }

    #[test]
    fn profile() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"count": 10}))
            .values("count")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("property_json("));
    }
}
