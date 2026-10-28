use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepHasLabel {
    labels: Option<Vec<String>>,
}

impl fmt::Display for StepHasLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "has_label({})", display_option_vec_string(&self.labels))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps a vertex/edge whose label set intersects `labels` (an OR over label-set membership,
    /// not just single-label equality): `true` iff any candidate label is one of the element's
    /// labels. For a single-label vertex this is exactly the pre-multi-label exact-match OR, so
    /// single-label queries are unaffected. See [`Self::labels`] for reading a vertex's full label
    /// set instead of testing membership in it.
    ///
    /// `has_label(None)` (no argument, the DSL's `has_label()`) keeps the elements that have a
    /// label at all: a vertex with a non-empty label set, an edge with a label, every vertex
    /// property (labeled with its key). Its complement `not(__.has_label())` selects the
    /// unlabeled elements. TinkerPop has no zero-argument `hasLabel()`: this form is a Graphersal
    /// extension. An empty list (`has_label(Vec::<String>::new())`) keeps nothing, and so does
    /// `has_label(ElementProperty::Null)` (a value that names nothing converts to an empty list,
    /// never to "no argument"; the DSL's `has_label(null)` and TinkerPop's `hasLabel(null)` agree).
    pub fn has_label<T: Into<ArgStrings>>(self, labels: T) -> Self {
        let labels = match labels.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        self.push_step(GraphStep::Filter(
            "has_label",
            Box::new(StepHasLabel { labels }),
        ))
    }
}

impl __ {
    /// Anonymous [`GraphTraversalSource::has_label`]; `__::has_label(None)` keeps the labeled
    /// elements.
    pub fn has_label<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_label(labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Child-traversal form of [`GraphTraversalSource::has_label`]; `has_label(None)` keeps the
    /// labeled elements.
    pub fn has_label<T: Into<ArgStrings>>(self, labels: T) -> Self {
        let labels = match labels.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::Filter("has_label", Box::new(StepHasLabel { labels }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasLabel {
    fn labels(&self) -> Option<&[String]> {
        self.labels.as_deref()
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Vertex(node_index) => {
                // Label-set intersection (D4): `true` iff any candidate label is one of the
                // vertex's labels. For a single-label vertex this is exactly the old exact-match
                // OR, so single-label behavior is unchanged; under multi-label it correctly
                // matches a vertex on any one of the labels it carries. Without candidates
                // (`has_label()`) a vertex passes iff its label set is not empty.
                let vertex_labels = context.storage.vertex_labels(*node_index)?;
                match &self.labels {
                    Some(labels) => Ok(labels
                        .iter()
                        .any(|candidate| vertex_labels.iter().any(|l| l.as_str() == candidate))),
                    None => Ok(!vertex_labels.is_empty()),
                }
            }

            TraverserValue::Edge(edge_index) => {
                let edge_label = context.storage.edge_label(*edge_index)?;
                match edge_label {
                    Some(edge_label) => match &self.labels {
                        Some(labels) => Ok(labels.iter().any(|s| s == edge_label)),
                        None => Ok(true),
                    },
                    None => Ok(false),
                }
            }

            // A vertex property element (`properties()`) is labeled with its key, so it always
            // has a label. A value handle and an edge property are not elements and stay a cast
            // error.
            TraverserValue::VertexProperty(key, _, PropertyHandleKind::Element) => {
                match &self.labels {
                    Some(labels) => Ok(labels.iter().any(|candidate| candidate == key.as_str())),
                    None => Ok(true),
                }
            }

            // A path property element (`properties(jpath(..))`) is labeled with its path text.
            TraverserValue::PathProperty(prop) => match &self.labels {
                Some(labels) => Ok(labels.iter().any(|candidate| candidate == prop.0.as_str())),
                None => Ok(true),
            },

            t => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge", "vertex property"],
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
    fn v_args_single() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("software").count().to_list(),
            rhai: [
                r#"g.V().hasLabel("software").count()"#,
                r#"g.v().has_label("software").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::has_label("software")).count().to_list(),
            rhai: [
                r#"g.V().where(__.hasLabel("software")).count()"#,
                r#"g.v().where(__.has_label("software")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().has_label("software")).count().to_list(),
            rhai: [
                r#"g.V().where(__.element().hasLabel("software")).count()"#,
                r#"g.v().where(__.element().has_label("software")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_args_array_1() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label(vec!["person"]).count().to_list(),
            rhai: [
                r#"g.V().hasLabel(["person"]).count()"#,
                r#"g.v().has_label(["person"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_args_array_2() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label(vec!["software", "person"]).count().to_list(),
            rhai: [
                r#"g.V().hasLabel(["software", "person"]).count()"#,
                r#"g.v().has_label(["software", "person"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_args_single() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_label("knows").count().to_list(),
            rhai: [
                r#"g.E().hasLabel("knows").count()"#,
                r#"g.e().has_label("knows").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_args_array() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_label(vec!["knows", "created"]).count().to_list(),
            rhai: [
                r#"g.E().hasLabel(["knows", "created"]).count()"#,
                r#"g.e().has_label(["knows", "created"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .as_("x")
            .has_label("software")
            .count()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("has_label("));
    }
}
