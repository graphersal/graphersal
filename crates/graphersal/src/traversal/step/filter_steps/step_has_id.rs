use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepHasId {
    id_list: Option<Vec<String>>,
}

impl fmt::Display for StepHasId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "has_id({})", display_option_vec_string(&self.id_list))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps elements whose id is one of the given ids (ids are strings, see the book page Id
    /// Arguments).
    pub fn has_id<T: Into<ArgStrings>>(self, id_list: T) -> Self {
        let id_list = match id_list.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        self.push_step(GraphStep::Filter("has_id", Box::new(StepHasId { id_list })))
    }
}

impl __ {
    /// Keeps elements whose id is one of the given ids (ids are strings, see the book page Id
    /// Arguments). Starts an anonymous traversal (`__.has_id()`).
    pub fn has_id<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        id_list: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_id(id_list)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Keeps elements whose id is one of the given ids (ids are strings, see the book page Id
    /// Arguments).
    pub fn has_id<T: Into<ArgStrings>>(self, id_list: T) -> Self {
        let id_list = match id_list.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::Filter("has_id", Box::new(StepHasId { id_list }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasId {
    fn ids(&self) -> Option<&[String]> {
        self.id_list.as_deref()
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Vertex(node_index) => {
                let Some(vertex_id) =
                    crate::graph::storage::filter_vertex_id(&*context.storage, *node_index)?
                else {
                    return Ok(false);
                };
                match &self.id_list {
                    Some(id_list) => Ok(id_list.iter().any(|id| id == vertex_id)),
                    None => Ok(false),
                }
            }

            TraverserValue::Edge(edge_index) => {
                let Some(edge_id) =
                    crate::graph::storage::filter_edge_id(&*context.storage, *edge_index)?
                else {
                    return Ok(false);
                };
                match &self.id_list {
                    Some(id_list) => Ok(id_list.iter().any(|id| id == edge_id)),
                    None => Ok(false),
                }
            }

            t => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge"],
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

    #[cfg(feature = "script")]
    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").has_id("1").count().to_list(),
            rhai: [
                r#"g.V("1").hasId("1").count()"#,
                r#"g.v("1").has_id("1").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_where() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::has_id("1")).count().to_list(),
            rhai: [
                r#"g.V("1").where(__.hasId("1")).count()"#,
                r#"g.v("1").where(__.has_id("1")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::element().has_id("1")).count().to_list(),
            rhai: [
                r#"g.V("1").where(__.element().hasId("1")).count()"#,
                r#"g.v("1").where(__.element().has_id("1")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("2").has_id("1").count().to_list(),
            rhai: [
                r#"g.V("2").hasId("1").count()"#,
                r#"g.v("2").has_id("1").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_multiply() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(vec!["1", "2"]).has_id(vec!["1", "2"]).count().to_list(),
            rhai: [
                r#"g.V(["1", "2"]).hasId(["1", "2"]).count()"#,
                r#"g.v(["1", "2"]).has_id(["1", "2"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_multiply_2() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(vec!["1", "2", "3"]).has_id(vec!["1", "2"]).count().to_list(),
            rhai: [
                r#"g.V(["1", "2", "3"]).hasId(["1", "2"]).count()"#,
                r#"g.v(["1", "2", "3"]).has_id(["1", "2"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").has_id("1").count().to_list(),
            rhai: [
                r#"g.E("1").hasId("1").count()"#,
                r#"g.e("1").has_id("1").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_where() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").where_t(__::has_id("1")).count().to_list(),
            rhai: [
                r#"g.E("1").where(__.hasId("1")).count()"#,
                r#"g.e("1").where(__.has_id("1")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").where_t(__::element().has_id("1")).count().to_list(),
            rhai: [
                r#"g.E("1").where(__.element().hasId("1")).count()"#,
                r#"g.e("1").where(__.element().has_id("1")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("2").has_id("1").count().to_list(),
            rhai: [
                r#"g.E("2").hasId("1").count()"#,
                r#"g.e("2").has_id("1").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_multiply() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(vec!["1", "2"]).has_id(vec!["1", "2"]).count().to_list(),
            rhai: [
                r#"g.E(["1", "2"]).hasId(["1", "2"]).count()"#,
                r#"g.e(["1", "2"]).has_id(["1", "2"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_multiply_2() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(vec!["1", "2", "3"]).has_id(vec!["1", "2"]).count().to_list(),
            rhai: [
                r#"g.E(["1", "2", "3"]).hasId(["1", "2"]).count()"#,
                r#"g.e(["1", "2", "3"]).has_id(["1", "2"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .e(vec!["1", "2", "3"])
            .as_("x")
            .has_id(vec!["1", "2"])
            .count()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("has_id("));
    }
}
