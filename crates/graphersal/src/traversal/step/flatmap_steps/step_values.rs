use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
pub(super) struct StepValues {
    pub(super) args: Option<Vec<String>>,
}

impl fmt::Display for StepValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "values({})", display_option_vec_string(&self.args))
    }
}

/// The step for `values(keys)`: the literal-name step when every key is a name, the path-aware
/// step as soon as one key is a path.
fn values_step<'g, S: GraphStorage + 'g>(keys: PropKeys) -> GraphStep<'g, S> {
    match keys.into_names() {
        Ok(args) => GraphStep::FlatMap("values", Box::new(StepValues { args })),
        Err(keys) => super::step_values_path::values_keys_step(keys),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `values(keys...)`. A key is a property name (a string is always a literal name) or a
    /// [`JsonPath`] into a nested property value; a path that does not
    /// exist yields nothing, it is not an error. See `step_values_path.rs`.
    pub fn values<T: Into<PropKeys>>(self, args: T) -> Self {
        self.push_step(values_step(args.into()))
    }
}

impl __ {
    /// Yields the values of the given (or all) properties; a jpath() key reads a nested value.
    /// Starts an anonymous traversal (`__.values()`).
    pub fn values<'g, S: GraphStorage + 'g, T: Into<PropKeys>>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().values(labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the values of the given (or all) properties; a jpath() key reads a nested value.
    pub fn values<T: Into<PropKeys>>(self, args: T) -> Self {
        self.push_step(values_step(args.into()))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepValues {
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
                                PropertyHandleKind::Value,
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
                                    PropertyHandleKind::Value,
                                )
                            },
                        );
                    }
                    // A removed edge has no properties (stale handle): nothing to emit.
                    None if !context.storage.contains_edge(edge_index)? => {}
                    None => {
                        return Err(Box::new(TraverserError::Value(
                            ValueError::PropertyNotFound {
                                key: "properties".to_string(),
                                element_type: "edge",
                            },
                        )));
                    }
                }
                Ok(())
            }

            TraverserValue::Object(obj) => {
                drain_object_values(
                    obj,
                    &self.args,
                    source.path,
                    source.bulk,
                    source.sack,
                    output,
                );
                Ok(())
            }

            // A property whose stored value is itself an object/array (e.g. written via
            // `property_json`) is resolved here rather than at the first `values(key)`: that
            // step deliberately stays lazy (a `VertexProperty` reference, AGENTS.md rule 9) so a
            // downstream filter can drop the traverser before the property is ever fetched.
            // Chaining a second `values(...)` is what forces the fetch, mirroring how vanilla
            // Gremlin lets `values()` dig into a nested `Map`-valued property.
            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let prop = context
                    .storage
                    .vertex_properties(node_index)?
                    .and_then(|p| p.get(prop_name.as_str()).map(ElementProperty::to_logical));
                resolve_nested_property_values(
                    prop,
                    &self.args,
                    source.path,
                    source.bulk,
                    source.sack,
                    output,
                )
            }

            TraverserValue::EdgeProperty(prop_name, edge_index, _) => {
                let prop = context
                    .storage
                    .edge_properties(edge_index)?
                    .and_then(|p| p.get(prop_name.as_str()).map(ElementProperty::to_logical));
                resolve_nested_property_values(
                    prop,
                    &self.args,
                    source.path,
                    source.bulk,
                    source.sack,
                    output,
                )
            }

            TraverserValue::Record { keys, values } => {
                match &self.args {
                    Some(args) => {
                        for arg in args {
                            if let Some(idx) = keys.iter().position(|k| k == arg) {
                                output.push(Traverser::with_parts(
                                    values[idx].clone(),
                                    source.path,
                                    source.bulk,
                                    source.sack,
                                ));
                            }
                        }
                    }
                    None => {
                        for v in values {
                            output.push(Traverser::with_parts(
                                v,
                                source.path,
                                source.bulk,
                                source.sack,
                            ));
                        }
                    }
                }
                Ok(())
            }

            v => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge", "object"],
                &v,
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// Emits one `TraverserValue::VertexProperty`/`EdgeProperty` reference per property key that
/// matches `args` (every key, in insertion order, when `args` is `None`) from an already-fetched
/// property map. `make_ref` builds the concrete `VertexProperty`/`EdgeProperty` variant, since the
/// two only differ in which element id they carry.
///
/// Shared by `values()`'s own `Vertex`/`Edge` handling — which stays lazy here too, per rule 4;
/// the reference is resolved to a scalar later, either by a terminal step's `into_node_property`
/// or an explicit second `values()`/`value()` call — and by `properties()`
/// (`flatmap_steps/step_properties.rs`), which stops at the exact same reference and lets
/// `has_key()`/`has_value()`/`value()`/`key()` act on it as the property object TinkerPop's
/// `properties()` returns.
pub(crate) fn emit_matching_property_refs<S: GraphStorage>(
    props: &PropertyMap,
    args: &Option<Vec<String>>,
    path: PathRef,
    bulk: u64,
    sack: SackRef,
    output: &mut Vec<Traverser<S>>,
    make_ref: impl Fn(CompactString) -> TraverserValue<S>,
) {
    match args {
        Some(args) => {
            for key in args {
                if props.contains_key(key.as_str()) {
                    output.push(Traverser::with_parts(
                        make_ref(CompactString::new(key.as_str())),
                        path,
                        bulk,
                        sack,
                    ));
                }
            }
        }
        None => {
            for key in props.keys() {
                output.push(Traverser::with_parts(
                    make_ref(key.clone()),
                    path,
                    bulk,
                    sack,
                ));
            }
        }
    }
}

/// Extracts values by key (or every value, with no keys) from an already map-shaped stream
/// value. Shared between a `TraverserValue::Object` already in the stream and one resolved from
/// storage by [`resolve_nested_property_values`], so both paths dig into nested objects the same
/// way.
fn drain_object_values<S: GraphStorage>(
    mut obj: StreamMap<S>,
    args: &Option<Vec<String>>,
    path: PathRef,
    bulk: u64,
    sack: SackRef,
    output: &mut Vec<Traverser<S>>,
) {
    match args {
        Some(args) => {
            args.iter().for_each(|key| {
                let key =
                    TraverserValue::Computed(ElementProperty::String(key.clone().into()), None);
                if let Some(value) = obj.shift_remove(&key) {
                    output.push(Traverser::with_parts(value, path, bulk, sack));
                }
            });
        }
        None => {
            for value in obj.0.into_values() {
                output.push(Traverser::with_parts(value, path, bulk, sack));
            }
        }
    }
}

/// Resolves a property fetched fresh from storage into what `values(key)` would emit for it:
/// descends into it when it is an object (mirroring vanilla Gremlin's `values()` reading into a
/// `Map`-valued property), passes a scalar through unchanged for a bare `values()`, and rejects a
/// scalar for `values(key)` since there is no `key` to find in it.
fn resolve_nested_property_values<S: GraphStorage>(
    prop: Option<ElementProperty>,
    args: &Option<Vec<String>>,
    path: PathRef,
    bulk: u64,
    sack: SackRef,
    output: &mut Vec<Traverser<S>>,
) -> TraverserResult<()> {
    let Some(prop) = prop else {
        return Ok(());
    };

    let actual_type = crate::graph::schema::value_type_name(&prop);
    match prop.into() {
        TraverserValue::Object(obj) => {
            drain_object_values(obj, args, path, bulk, sack, output);
            Ok(())
        }
        other => match args {
            None => {
                output.push(Traverser::with_parts(other, path, bulk, sack));
                Ok(())
            }
            Some(_) => Err(Box::new(TraverserError::cast_exception_str(
                &["object"],
                actual_type,
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("2").values("age").to_list(),
            rhai: [
                r#"g.V("2").values("age")"#,
                r#"g.v("2").values("age")"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(27)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn obj_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().as_("A").out(None).out(None).id().as_("B").select(vec!["A", "B"]).values("B").to_list(),
            rhai: [
                r#"g.V().id().as("A").out().out().id().as("B").select(["A", "B"]).values("B")"#,
                r#"g.v().id().as_("A").out().out().id().as_("B").select(["A", "B"]).values("B")"#,
            ],
            assert: |res| {
                assert_eq!(res[0], ElementProperty::String("5".into()));
                assert_eq!(res[1], ElementProperty::String("3".into()));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_double() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("2").values(vec!["age", "name"]).to_list(),
            rhai: [
                r#"g.V("2").values(["age", "name"])"#,
                r#"g.v("2").values(["age", "name"])"#,
            ],
            assert: |res| {
                assert_eq!(res[0].as_i64(), Some(27));
                assert_eq!(res[1], ElementProperty::String("vadas".into()));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn obj_double() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().as_("A").out(None).out(None).id().as_("B").select(vec!["A", "B"]).values(vec!["A", "B"]).to_list(),
            rhai: [
                r#"g.V().id().as("A").out().out().id().as("B").select(["A", "B"]).values(["A", "B"])"#,
                r#"g.v().id().as_("A").out().out().id().as_("B").select(["A", "B"]).values(["A", "B"])"#,
            ],
            assert: |res| {
                assert_eq!(res[0], ElementProperty::String("1".into()));
                assert_eq!(res[1], ElementProperty::String("5".into()));
                assert_eq!(res[2], ElementProperty::String("1".into()));
                assert_eq!(res[3], ElementProperty::String("3".into()));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn obj_none() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().as_("A").out(None).out(None).id().as_("B").select(vec!["A", "B"]).values(None).to_list(),
            rhai: [
                r#"g.V().id().as("A").out().out().id().as("B").select(["A", "B"]).values()"#,
                r#"g.v().id().as_("A").out().out().id().as_("B").select(["A", "B"]).values()"#,
            ],
            assert: |res| {
                assert_eq!(res[0], ElementProperty::String("1".into()));
                assert_eq!(res[1], ElementProperty::String("5".into()));
                assert_eq!(res[2], ElementProperty::String("1".into()));
                assert_eq!(res[3], ElementProperty::String("3".into()));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").values("weight").to_list(),
            rhai: [
                r#"g.E("1").values("weight")"#,
                r#"g.e("1").values("weight")"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(0.4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_all() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").values(None).fold().count_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.E("1").values().fold().count(Scope.local)"#,
                r#"g.e("1").values().fold().count(Scope.local)"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_empty() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").values("name").to_list(),
            rhai: [
                r#"g.E("1").values("name")"#,
                r#"g.e("1").values("name")"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::values(None).fold().count_scoped(Scope::Local).is(2)).id().to_list(),
            rhai: [
                r#"g.V("1").where(__.values().fold().count(Scope.local).is(2)).id()"#,
                r#"g.v("1").where(__.values().fold().count(Scope.local).is(2)).id()"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("1")),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::element().values(vec!["age", "name"]).fold().count_scoped(Scope::Local).is(2)).id().to_list(),
            rhai: [
                r#"g.V("1").where(__.element().values(["age", "name"]).fold().count(Scope.local).is(2)).id()"#,
                r#"g.v("1").where(__.element().values(["age", "name"]).fold().count(Scope.local).is(2)).id()"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("1")),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e("1").values("weight").profile().unwrap();
        assert!(result.to_string().contains("values("));
    }

    /// `values()` with no arguments must render as empty parens, not `values(None)`.
    #[test]
    fn profile_no_args_renders_empty_parens() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").values(None).profile().unwrap();
        assert!(result.to_string().contains("values()"));
        assert!(!result.to_string().contains("(None)"));
    }

    /// `values(key)` on a vertex stays a lazy `VertexProperty` reference (AGENTS.md rule 9); a
    /// second `values(key)` must resolve it from storage and descend into a nested object, the
    /// idiom behind digging into a `Map`-valued property written by `property_json`.
    #[test]
    #[cfg(feature = "serde")]
    fn nested_object_property_digs_via_second_values() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1, "nested": {"deep": "hi"}}}))
            .values("meta")
            .values("k")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));

        let deep = graph
            .traversal_mut()
            .v("1")
            .values("meta")
            .values("nested")
            .values("deep")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(deep, ElementProperty::String("hi".into()));
    }

    /// The same nested-object descent must work once the value has already been materialized by
    /// `element_map()` (`TraverserValue::Object`, not a lazy `VertexProperty` reference).
    #[test]
    #[cfg(feature = "serde")]
    fn nested_object_property_digs_after_element_map() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .element_map(None)
            .values("meta")
            .values("k")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    /// `values(key)` on a scalar property (no nested object to descend into) must be a normal
    /// `CastException` diagnostic, not a panic.
    #[test]
    #[cfg(feature = "serde")]
    fn values_on_scalar_property_is_cast_exception_not_panic() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let err = g
            .v("1")
            .values("name")
            .values("anything")
            .next()
            .expect_err("a scalar property has no sub-key to find");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::CastException { .. })
        ));
    }
}
