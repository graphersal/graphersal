use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::traversal::step::prop_path::PathKeys;
use crate::*;

#[derive(Default, Clone)]
struct StepElementMap {
    args: Option<Vec<String>>,
    precomputed_keys: Option<Arc<Vec<RecordKey>>>,
    /// The `jpath(..)` key arguments, if any; `None` for a plain-name call.
    path_keys: Option<PathKeys>,
}

impl StepElementMap {
    fn new(keys: PropKeys) -> Self {
        let (args, path_keys) = PathKeys::split(keys);
        let precomputed_keys = args.as_ref().filter(|_| path_keys.is_none()).map(|a| {
            let mut k = Vec::with_capacity(a.len() + 2);
            k.push(RecordKey::Id);
            k.push(RecordKey::Label);
            k.extend(a.iter().map(|s| RecordKey::from(s.as_str())));
            Arc::new(k)
        });
        Self {
            args,
            precomputed_keys,
            path_keys,
        }
    }
}

impl fmt::Display for StepElementMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path_keys {
            Some(path_keys) => write!(f, "element_map({})", path_keys.render()),
            None => write!(f, "element_map({})", display_option_vec_string(&self.args)),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// The element as a map: its id and label under the token keys [`RecordKey::Id`] and
    /// [`RecordKey::Label`] (an edge also its endpoints under [`RecordKey::Out`] and
    /// [`RecordKey::In`], each a map of the vertex's id and label), then all (or the named)
    /// properties under [`RecordKey::Name`]. A property named `id` or `label` is its own entry:
    /// `select("label")` reads it, `select(RecordKey::Label)` the label. Materialized (a
    /// terminal), the tokens are written `id`, `label`, `OUT`, `IN` and a property of the same
    /// name wins. A key may be a [`JsonPath`]: the entry's key is the canonical path text
    /// (`$.meta.k`), a missing path adds no entry.
    pub fn element_map<T: Into<PropKeys>>(self, args: T) -> Self {
        let args = args.into();
        self.push_step(GraphStep::Map(
            "element_map",
            Box::new(StepElementMap::new(args)),
        ))
    }
}

impl __ {
    /// Turns each vertex or edge into a map of its id and label (token keys), the given (or all)
    /// properties and, for an edge, its endpoints; see [`GraphTraversalSource::element_map`].
    /// Starts an anonymous traversal (`__.element_map()`).
    pub fn element_map<'g, S: GraphStorage + 'g, T: Into<PropKeys>>(
        args: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().element_map(args)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Turns each vertex or edge into a map of its id and label (token keys), the given (or all)
    /// properties and, for an edge, its endpoints; see [`GraphTraversalSource::element_map`].
    pub fn element_map<T: Into<PropKeys>>(self, args: T) -> Self {
        let args = args.into();
        let step = GraphStep::Map("element_map", Box::new(StepElementMap::new(args)));
        self.push_step(step)
    }
}

impl StepElementMap {
    /// Appends all (or the named) properties and then the `jpath(..)` keys.
    fn push_properties<S: GraphStorage>(
        &self,
        props: Option<&PropertyMap>,
        keys: &mut Vec<RecordKey>,
        values: &mut Vec<TraverserValue<S>>,
    ) {
        if let Some(props) = props {
            for (k, v) in props {
                if self
                    .args
                    .as_ref()
                    .is_none_or(|args| args.iter().any(|arg| arg == k.as_str()))
                {
                    keys.push(RecordKey::Name(k.clone()));
                    values.push(v.to_logical().into());
                }
            }
        }
        if let Some(path_keys) = &self.path_keys {
            path_keys.append(props, keys, values, TraverserValue::from);
        }
    }

    /// The record of `keys`/`values`, sharing the precomputed key list when it matches.
    fn record<S: GraphStorage>(
        &self,
        keys: Vec<RecordKey>,
        values: Vec<TraverserValue<S>>,
        source: &Traverser<S>,
    ) -> Traverser<S> {
        let keys = match &self.precomputed_keys {
            Some(precomputed) if keys == **precomputed => Arc::clone(precomputed),
            _ => Arc::new(keys),
        };
        Traverser::with_parts(
            TraverserValue::Record { keys, values },
            source.path,
            source.bulk,
            source.sack,
        )
    }
}

/// Appends a vertex's `T.id` and (when it has one) `T.label` entries.
fn push_vertex_tokens<S: GraphStorage>(
    storage: &S,
    vertex: S::VertexId,
    keys: &mut Vec<RecordKey>,
    values: &mut Vec<TraverserValue<S>>,
) -> TraverserResult<()> {
    let id = crate::graph::storage::stored_vertex_id(storage, vertex)?;
    keys.push(RecordKey::Id);
    values.push(TraverserValue::Computed(
        ElementProperty::String(CompactString::new(id)),
        None,
    ));
    if let Some(label) = storage.vertex_label(vertex)? {
        keys.push(RecordKey::Label);
        values.push(TraverserValue::Computed(
            ElementProperty::String(CompactString::new(label)),
            None,
        ));
    }
    Ok(())
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepElementMap {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        match (&self.args, &self.path_keys) {
            (None, None) => visit(None, crate::graph::workload::AccessKind::Read),
            (args, paths) => {
                args.iter()
                    .flatten()
                    .for_each(|key| visit(Some(key), crate::graph::workload::AccessKind::Read));
                for (path, _) in paths.iter().flat_map(|p| p.paths.iter()) {
                    if let Some(crate::value::json_path::JsonPathSegment::Key(key)) =
                        path.segments().first()
                    {
                        visit(Some(key), crate::graph::workload::AccessKind::Read);
                    }
                }
            }
        }
    }
    /// Builds an aggregation/materialization buffer (D3 of the memory-inspection spec) — reported
    /// to `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        match &source.value {
            TraverserValue::Vertex(node_index) => {
                let props = context.storage.vertex_properties(*node_index)?;
                let mut keys: Vec<RecordKey> =
                    Vec::with_capacity(props.map(|p| p.len()).unwrap_or(0) + 2);
                let mut values: Vec<TraverserValue<S>> = Vec::with_capacity(keys.capacity());
                push_vertex_tokens(context.storage, *node_index, &mut keys, &mut values)?;
                self.push_properties(props, &mut keys, &mut values);
                Ok(self.record(keys, values, &source))
            }

            TraverserValue::Edge(edge_index) => {
                let id = crate::graph::storage::stored_edge_id(&*context.storage, *edge_index)?;
                let label = context.storage.edge_label(*edge_index)?;
                let props = context.storage.edge_properties(*edge_index)?;

                let mut keys: Vec<RecordKey> =
                    Vec::with_capacity(props.map(|p| p.len()).unwrap_or(0) + 4);
                let mut values: Vec<TraverserValue<S>> = Vec::with_capacity(keys.capacity());

                keys.push(RecordKey::Id);
                values.push(TraverserValue::Computed(
                    ElementProperty::String(CompactString::new(id)),
                    None,
                ));
                if let Some(label) = label {
                    keys.push(RecordKey::Label);
                    values.push(TraverserValue::Computed(
                        ElementProperty::String(CompactString::new(label)),
                        None,
                    ));
                }
                // The endpoints, as TinkerPop's `Direction.OUT`/`Direction.IN` entries: a map of
                // the vertex's `T.id` and `T.label`.
                if let Some((out_vertex, in_vertex)) =
                    context.storage.edge_endpoints(*edge_index)?
                {
                    for (direction, vertex) in
                        [(RecordKey::Out, out_vertex), (RecordKey::In, in_vertex)]
                    {
                        let mut end_keys = Vec::with_capacity(2);
                        let mut end_values = Vec::with_capacity(2);
                        push_vertex_tokens(
                            context.storage,
                            vertex,
                            &mut end_keys,
                            &mut end_values,
                        )?;
                        keys.push(direction);
                        values.push(TraverserValue::Record {
                            keys: Arc::new(end_keys),
                            values: end_values,
                        });
                    }
                }
                self.push_properties(props, &mut keys, &mut values);
                Ok(self.record(keys, values, &source))
            }

            TraverserValue::Record {
                keys: rec_keys,
                values: rec_vals,
            } => {
                if let Some(args) = &self.args {
                    let mut keys = Vec::new();
                    let mut values = Vec::new();
                    for (k, v) in rec_keys.iter().zip(rec_vals.iter()) {
                        if k.is_token()
                            || k == "id"
                            || k == "label"
                            || k.name()
                                .is_some_and(|name| args.iter().any(|arg| arg == name))
                        {
                            keys.push(k.clone());
                            values.push(v.clone());
                        }
                    }
                    if let Some(path_keys) = &self.path_keys {
                        path_keys.append_from_value(
                            context,
                            &source.value,
                            &mut keys,
                            &mut values,
                        )?;
                    }
                    Ok(Traverser::with_parts(
                        TraverserValue::Record {
                            keys: Arc::new(keys),
                            values,
                        },
                        source.path,
                        source.bulk,
                        source.sack,
                    ))
                } else {
                    Ok(Traverser::with_parts(
                        TraverserValue::Record {
                            keys: Arc::clone(rec_keys),
                            values: rec_vals.clone(),
                        },
                        source.path,
                        source.bulk,
                        source.sack,
                    ))
                }
            }

            TraverserValue::Object(value) => {
                let mut keys = Vec::new();
                let mut values = Vec::new();
                for (k, v) in value {
                    if let TraverserValue::Computed(ElementProperty::String(k_str), _) = k {
                        if let Some(args) = &self.args {
                            if args.iter().any(|arg| arg == k_str.as_str())
                                || k_str == "id"
                                || k_str == "label"
                            {
                                keys.push(RecordKey::Name(k_str.clone()));
                                values.push(v.clone());
                            }
                        } else {
                            keys.push(RecordKey::Name(k_str.clone()));
                            values.push(v.clone());
                        }
                    }
                }
                if let Some(path_keys) = &self.path_keys {
                    path_keys.append_from_value(context, &source.value, &mut keys, &mut values)?;
                }
                Ok(Traverser::with_parts(
                    TraverserValue::Record {
                        keys: Arc::new(keys),
                        values,
                    },
                    source.path,
                    source.bulk,
                    source.sack,
                ))
            }

            v => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge", "object"],
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
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_element_map_vertex() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").element_map(vec!["name", "age"]).to_list(),
            rhai: [
                r#"g.V("1").elementMap(["name", "age"])"#,
                r#"g.v("1").element_map(["name", "age"])"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.get("id").unwrap().as_string(), Some("1"));
                        assert_eq!(obj.get("label").unwrap().as_string(), Some("person"));
                        assert_eq!(obj.get("name").unwrap().as_string(), Some("marko"));
                        assert_eq!(obj.get("age").unwrap().as_i64(), Some(29));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    /// `element_map()` called with no arguments must render as empty parens in `.profile()`
    /// output, not `element_map(None)` — `display_option_vec_string`'s `None` arm must produce
    /// an empty string, not the literal text "None".
    #[test]
    fn profile_no_args_renders_empty_parens() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").element_map(None).profile().unwrap();
        assert!(result.to_string().contains("element_map()"));
        assert!(!result.to_string().contains("(None)"));
    }

    /// The `Some` case must keep rendering its args normally — only the `None` arm changed.
    #[test]
    fn profile_with_args_renders_args() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").element_map(vec!["name"]).profile().unwrap();
        assert!(result.to_string().contains("element_map(\"name\")"));
    }

    fn entry<'a>(map: &'a ElementProperty, key: &str) -> Option<&'a ElementProperty> {
        map.as_object().and_then(|m| m.get(key))
    }

    #[test]
    fn an_edge_map_keeps_id_label_and_the_selected_keys() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("0").element_map(vec!["weight"]).to_list(),
            rhai: [
                r#"g.E("0").elementMap("weight")"#,
                r#"g.e("0").element_map("weight")"#,
            ],
            assert: |res| {
                assert_eq!(entry(&res[0], "id"), Some(&"0".into()));
                assert_eq!(entry(&res[0], "label"), Some(&"knows".into()));
                assert_eq!(entry(&res[0], "weight"), Some(&0.5.into()));
            },
        );
        // a jpath key is listed under its canonical path text
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .e("0")
                .element_map("weight".parse::<JsonPath>().unwrap())
                .to_list(),
            rhai: [r#"g.E("0").elementMap(jpath("weight"))"#],
            assert: |res| {
                assert_eq!(entry(&res[0], "$.weight"), Some(&0.5.into()));
                assert!(entry(&res[0], "weight").is_none());
            },
        );
    }

    #[test]
    fn a_map_is_reselected_keeping_id_and_label() {
        // an element map narrowed again keeps id and label and the selected keys
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").element_map(None).element_map(vec!["age"]).to_list(),
            rhai: [r#"g.V("1").elementMap().elementMap("age")"#],
            assert: |res| {
                let m = res[0].as_object().unwrap();
                assert_eq!(m.len(), 3);
                assert_eq!(m.get("age"), Some(&29.into()));
                assert_eq!(m.get("label"), Some(&"person".into()));
            },
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").element_map(None).element_map(None).to_list(),
            rhai: [r#"g.V("1").elementMap().elementMap()"#],
            assert: |res| assert_eq!(res[0].as_object().unwrap().len(), 4),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .element_map(None)
                .element_map("name".parse::<JsonPath>().unwrap())
                .to_list(),
            rhai: [r#"g.V("1").elementMap().elementMap(jpath("name"))"#],
            assert: |res| assert_eq!(entry(&res[0], "$.name"), Some(&"marko".into())),
        );
        // an object (project) is turned into a map of its string keys
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .project(vec!["id", "age", "name"])
                .by(By::Id)
                .by("age")
                .by("name")
                .element_map(vec!["age"])
                .to_list(),
            rhai: [r#"g.V("1").project("id", "age", "name").by(T.id).by("age").by("name").elementMap("age")"#],
            assert: |res| {
                let m = res[0].as_object().unwrap();
                assert_eq!(m.len(), 2);
                assert_eq!(m.get("id"), Some(&"1".into()));
                assert_eq!(m.get("age"), Some(&29.into()));
            },
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .project(vec!["age", "name"])
                .by("age")
                .by("name")
                .element_map(None)
                .to_list(),
            rhai: [r#"g.V("1").project("age", "name").by("age").by("name").elementMap()"#],
            assert: |res| assert_eq!(res[0].as_object().unwrap().len(), 2),
        );
    }

    #[test]
    fn a_property_value_is_a_cast_error() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").element_map(None).to_list(),
            rhai: [r#"g.V("1").values("name").elementMap()"#],
            assert: |err| assert!(
                err.contains("expected type [vertex or edge or object], got type 'vertexProperty'"),
                "{err}"
            ),
        );
    }
}
