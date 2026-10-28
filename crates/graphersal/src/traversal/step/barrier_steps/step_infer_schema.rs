use std::fmt;

use crate::error::TraverserResult;
use crate::graph::GraphStorage;
use crate::graph::schema::SchemaInferer;
use crate::traversal::context::TraverserContext;
use crate::traversal::step::{GraphStep, GraphStepBarrier};
use crate::value::{PropertyMap, TraverserValue};
use crate::{__, AnonymousTraversal, GraphTraversalSource, Traverser};

/// `infer_schema()` over a stream: the schema the vertices and edges of the stream satisfy (the
/// same inference as the source method `g.infer_schema()`, see [`SchemaInferer`]). A vertex
/// counts for every one of its labels; property values of other traversers are ignored.
#[derive(Clone, Default)]
pub(crate) struct StepInferSchema;

impl fmt::Display for StepInferSchema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "infer_schema()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepInferSchema {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        // Not a TinkerPop step: nothing on an empty stream.
        if source.is_empty() {
            return Ok(None);
        }
        let mut inferer = SchemaInferer::new();
        let empty = PropertyMap::default();
        for traverser in source {
            match &traverser.value {
                TraverserValue::Vertex(node)
                | TraverserValue::VertexProperty(_, node, _)
                | TraverserValue::Computed(_, Some(node)) => {
                    inferer.observe_vertex(
                        context.storage.vertex_labels(*node)?,
                        context.storage.vertex_properties(*node)?.unwrap_or(&empty),
                    );
                }
                TraverserValue::Edge(edge) | TraverserValue::EdgeProperty(_, edge, _) => {
                    let Some((from, to)) = context.storage.edge_endpoints(*edge)? else {
                        continue;
                    };
                    inferer.observe_edge(
                        context.storage.edge_label(*edge)?,
                        context.storage.vertex_labels(from)?,
                        context.storage.vertex_labels(to)?,
                        context.storage.edge_properties(*edge)?.unwrap_or(&empty),
                    );
                }
                _ => {}
            }
        }
        Ok(Some(Traverser::start(
            context,
            TraverserValue::Schema(Box::new(inferer.finish())),
        )))
    }

    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// The `infer_schema()` step: the schema the stream's vertices and edges satisfy, as one
    /// value. (The source method [`Self::infer_schema`] infers the whole graph's.)
    pub fn infer_schema_step(self) -> Self {
        self.push_step(GraphStep::Barrier(
            "infer_schema",
            Box::new(StepInferSchema),
        ))
    }
}

impl __ {
    /// `__.infer_schema()`.
    pub fn infer_schema<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().infer_schema()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// The `infer_schema()` step (see [`GraphTraversalSource::infer_schema_step`]).
    pub fn infer_schema(self) -> Self {
        self.push_step(GraphStep::Barrier(
            "infer_schema",
            Box::new(StepInferSchema),
        ))
    }
}

/// The raw stream value of the step (a `TraverserValue::Schema`), before any materialization.
#[cfg(test)]
mod tests {
    use crate::graph::schema::EdgeConnection;
    use crate::traversal::{__, GraphSource};
    use crate::value::TraverserValue;

    #[test]
    fn test_v_has_label_infer_schema_step() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let traversers = g_lock
            .traversal()
            .v(None)
            .has_label("person")
            .infer_schema_step()
            .internal_to_list()
            .unwrap();
        assert_eq!(traversers.len(), 1);
        let schema = traversers[0].as_schema().expect("a schema value");
        assert_eq!(schema.vertices.len(), 1);
        assert_eq!(schema.edges.len(), 0);
        let person = &schema.vertices["person"].schema;
        assert_eq!(person.properties["name"].type_name(), "string");
        assert_eq!(person.properties["age"].type_name(), "int64");
        assert_eq!(person.required, vec!["name", "age"]);
    }

    /// An empty stream yields no traverser (no schema of nothing).
    #[test]
    fn test_infer_schema_step_on_an_empty_stream() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let traversers = g_lock
            .traversal()
            .v(None)
            .has_label("non_existent")
            .infer_schema_step()
            .internal_to_list()
            .unwrap();
        assert!(traversers.is_empty());
    }

    #[test]
    fn test_out_e_infer_schema_step() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let traversers = g_lock
            .traversal()
            .v(None)
            .out_e("created")
            .infer_schema_step()
            .internal_to_list()
            .unwrap();
        let schema = traversers[0].as_schema().expect("a schema value");
        let created = &schema.edges["created"];
        assert!(created.schema.properties.contains_key("weight"));
        assert_eq!(
            created.connections,
            vec![EdgeConnection::new("person", "software")]
        );
    }

    #[test]
    fn test_union_with_infer_schema_branch() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let traversers = g_lock
            .traversal()
            .v(None)
            .has_label("person")
            .union(vec![__::identity(), __::infer_schema()])
            .internal_to_list()
            .unwrap();
        // `infer_schema()` is a barrier: a union branch holding it is fed the whole stream.
        assert_eq!(traversers.len(), 5);
        let schemas = traversers
            .iter()
            .filter(|t| matches!(t.value, TraverserValue::Schema(_)))
            .count();
        assert_eq!(schemas, 1);
    }
}
