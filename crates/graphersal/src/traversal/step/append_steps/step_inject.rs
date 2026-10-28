use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepInject {
    values: Vec<ElementProperty>,
}

impl fmt::Display for StepInject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "inject({})",
            crate::value::literal::rhai_literals(&self.values)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `inject(values)`: emits one traverser per element of `values`, in order; mid-traversal
    /// the injected values join the incoming stream. Every element is exactly one value:
    /// [`ElementProperty::Null`] injects one `null` and an [`ElementProperty::Array`] one list
    /// (TinkerPop's `inject(null)` / `inject([1, 2])`). An empty `vec![]` injects nothing.
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let locked = graph.read();
    /// // One `null` and one list: two traversers.
    /// let list = ElementProperty::Array(vec![1_i64.into(), 2_i64.into()]);
    /// let values = locked
    ///     .traversal()
    ///     .inject(vec![ElementProperty::Null, list.clone()])
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(values, vec![ElementProperty::Null, list]);
    /// ```
    pub fn inject<T: Into<ElementProperty>>(self, values: Vec<T>) -> Self {
        let values = values.into_iter().map(|r| r.into()).collect();
        self.push_step(GraphStep::Append("inject", Box::new(StepInject { values })))
    }
}

impl __ {
    /// Anonymous `inject(values)`; see [`GraphTraversalSource::inject`].
    pub fn inject<'g, S: GraphStorage + 'g, T: Into<ElementProperty>>(
        values: Vec<T>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().inject(values)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `inject(values)` inside a child traversal; see [`GraphTraversalSource::inject`].
    pub fn inject<T: Into<ElementProperty>>(self, values: Vec<T>) -> Self {
        let values = values.into_iter().map(|r| r.into()).collect();
        let step = GraphStep::Append("inject", Box::new(StepInject { values }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepAppend<'g, S> for StepInject {
    #[inline]
    fn reads_elements(&self) -> (bool, bool) {
        (
            self.values
                .iter()
                .any(|value| matches!(value, ElementProperty::Vertex(_))),
            self.values
                .iter()
                .any(|value| matches!(value, ElementProperty::Edge(_))),
        )
    }

    fn process(&self, context: &mut TraverserContext<'_, S>) -> TraverserResult<Vec<Traverser<S>>> {
        let mut res: Vec<Traverser<S>> = vec![];
        for r in self.values.iter() {
            match r {
                // A vertex or an edge value is injected as the stored element of its id.
                ElementProperty::Vertex(vertex) => {
                    if let Some(vertex_idx) = context.storage.get_vertex_by_id(vertex.id())? {
                        res.push(Traverser::start(
                            context,
                            TraverserValue::Vertex(vertex_idx),
                        ));
                    }
                }

                ElementProperty::Edge(edge) => {
                    if let Some(edge_idx) = context.storage.get_edge_by_id(edge.id())? {
                        res.push(Traverser::start(context, TraverserValue::Edge(edge_idx)));
                    }
                }

                // `null`, scalars, a map and a list are each injected as exactly one value
                // (TinkerPop: `inject(null)` emits one null, `inject([1, 2])` one list).
                ElementProperty::Null
                | ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_)
                | ElementProperty::Number(_)
                | ElementProperty::Boolean(_)
                | ElementProperty::Uuid(_)
                | ElementProperty::Temporal(_)
                | ElementProperty::Vector(_)
                | ElementProperty::Object(_)
                | ElementProperty::Map(_)
                | ElementProperty::Array(_) => {
                    // A map or list enters the stream as the same navigable value kind a
                    // projected one has (`TraverserValue::Object`/`Array`, nested values
                    // converted too), so `values(k)`, `select(k)` and `has(k, ..)` read its keys.
                    res.push(Traverser::start(context, TraverserValue::from(r)));
                }
            }
        }
        Ok(res)
    }

    fn clone_box(&self) -> Box<dyn GraphStepAppend<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).to_list(),
            rhai: [
                r#"g.inject("hello")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("hello"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::inject(vec!["1"])).id().to_list(),
            rhai: [
                r#"g.V("1").where(__.inject("1")).id()"#,
                r#"g.v("1").where(__.inject("1")).id()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("1"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::out(None).inject(vec!["1"])).id().to_list(),
            rhai: [
                r#"g.V("1").where(__.out().inject("1")).id()"#,
                r#"g.v("1").where(__.out().inject("1")).id()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("1"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn i64() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![42_i64]).to_list(),
            rhai: [
                r#"g.inject(42)"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_i64(), Some(42));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn f64() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![42.42_f64]).to_list(),
            rhai: [
                r#"g.inject(42.42)"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_f64(), Some(42.42_f64));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn two_string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello", "world"]).to_list(),
            rhai: [
                r#"g.inject("hello").inject("world")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_string(), Some("hello"));
                assert_eq!(res[1].as_string(), Some("world"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").inject(vec!["hello"]).to_list(),
            rhai: [
                r#"g.V("1").inject("hello")"#,
                r#"g.v("1").inject("hello")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[1].as_string(), Some("hello"));
            },
        );
    }

    #[test]
    fn v_vertex() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let first = g_lock.traversal().v("1").next().unwrap().unwrap();
        let result = g_lock
            .traversal()
            .v("2")
            .inject(vec![first])
            .to_list()
            .unwrap();
        assert_eq!(result.len(), 2);
        match &result[0] {
            ElementProperty::Vertex(vertex) => {
                assert_eq!(Some(vertex.id()), Some("2"));
            }
            _ => panic!(),
        }
        match &result[1] {
            ElementProperty::Vertex(vertex) => {
                assert_eq!(Some(vertex.id()), Some("1"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn v_vertex_out() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let first = g_lock.traversal().v("1").next().unwrap().unwrap();
        let result = g_lock
            .traversal()
            .inject::<ElementProperty>(vec![])
            .inject(vec![first])
            .out(None)
            .to_list()
            .unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn v_edge() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let first = g_lock
            .traversal()
            .v("1")
            .out_e(None)
            .next()
            .unwrap()
            .unwrap();
        let result = g_lock
            .traversal()
            .inject::<ElementProperty>(vec![])
            .inject(vec![first])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    #[test]
    fn v_edge_in() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let first = g_lock
            .traversal()
            .v("1")
            .out_e(None)
            .next()
            .unwrap()
            .unwrap();
        let result = g_lock
            .traversal()
            .inject::<ElementProperty>(vec![])
            .inject(vec![first])
            .in_v()
            .id()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_string(), Some("4"));
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).inject(vec!["hello"]).profile().unwrap();
        assert!(result.to_string().contains("inject("));
    }

    #[test]
    fn profile_1() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec!["hello"]).profile().unwrap();
        assert!(result.to_string().contains("inject("));
    }
}
