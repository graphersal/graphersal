use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepElement {}

impl fmt::Display for StepElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "element()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Moves from a property to the vertex or edge that owns it.
    pub fn element(self) -> Self {
        self.push_step(GraphStep::FlatMap(
            "element",
            Box::new(StepElement::default()),
        ))
    }
}

impl __ {
    /// Moves from a property to the vertex or edge that owns it. Starts an anonymous traversal
    /// (`__.element()`).
    pub fn element<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().element()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Moves from a property to the vertex or edge that owns it.
    pub fn element(self) -> Self {
        let step = GraphStep::FlatMap("element", Box::new(StepElement::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepElement {
    #[inline]
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        match source.value {
            TraverserValue::Computed(_, node_index) => {
                if let Some(node_index) = node_index {
                    output.push(Traverser::with_parts(
                        TraverserValue::Vertex(node_index),
                        source.path,
                        source.bulk,
                        source.sack,
                    ));
                }
                Ok(())
            }
            TraverserValue::VertexProperty(_, node_index, _) => {
                output.push(Traverser::with_parts(
                    TraverserValue::Vertex(node_index),
                    source.path,
                    source.bulk,
                    source.sack,
                ));
                Ok(())
            }
            TraverserValue::Vertex(_) => {
                output.push(source);
                Ok(())
            }
            TraverserValue::Edge(edge_index) => {
                output.push(Traverser::with_parts(
                    TraverserValue::Edge(edge_index),
                    source.path,
                    source.bulk,
                    source.sack,
                ));
                Ok(())
            }
            TraverserValue::EdgeProperty(_, edge_index, _) => {
                output.push(Traverser::with_parts(
                    TraverserValue::Edge(edge_index),
                    source.path,
                    source.bulk,
                    source.sack,
                ));
                Ok(())
            }
            TraverserValue::Path(_) => Ok(()),
            TraverserValue::Object(_) | TraverserValue::Record { .. } => Ok(()),
            TraverserValue::PathProperty(_) | TraverserValue::Entry(_) => Ok(()),
            TraverserValue::Array(_) => Ok(()),
            TraverserValue::Schema(_) | TraverserValue::Graph(_) => Ok(()),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
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
            rust: |g| g.v("1").element().to_list(),
            rhai: [
                r#"g.V("1").element()"#,
                r#"g.v("1").element()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Vertex(vertex) => {
                        assert_eq!(vertex.id(), "1");
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").label().element().to_list(),
            rhai: [
                r#"g.V("1").label().element()"#,
                r#"g.v("1").label().element()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Vertex(vertex) => {
                        assert_eq!(vertex.id(), "1");
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_where() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::element().has_id("1")).to_list(),
            rhai: [
                r#"g.V("1").where(__.element().hasId("1"))"#,
                r#"g.v("1").where(__.element().has_id("1"))"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Vertex(vertex) => {
                        assert_eq!(vertex.id(), "1");
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::label().element()).to_list(),
            rhai: [
                r#"g.V("1").where(__.label().element())"#,
                r#"g.v("1").where(__.label().element())"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Vertex(vertex) => {
                        assert_eq!(vertex.id(), "1");
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").element().to_list(),
            rhai: [
                r#"g.E("1").element()"#,
                r#"g.e("1").element()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Edge(vertex) => {
                        assert_eq!(vertex.id(), "1");
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").label().element().to_list(),
            rhai: [
                r#"g.E("1").label().element()"#,
                r#"g.e("1").label().element()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Edge(vertex) => {
                        assert_eq!(vertex.id(), "1");
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").element().profile().unwrap();
        assert!(result.to_string().contains("element("));
    }
}
