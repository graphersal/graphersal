use crate::graph::subgraph::copy_edges_into;
use crate::graph::{Graph, GraphStorage};
use crate::traversal::side_effect::SideEffectValue;
use crate::*;
use indexmap::IndexSet;
use std::sync::Arc;

/// `subgraph(label)`: collects the selected edges into a side-effect graph, and
/// `cap(label)` returns that graph as a value ([`TraverserValue::Graph`]).
///
/// What the graph holds: the selected edges and both endpoint vertices of each, with ids, vertex
/// label sets and all properties at full depth (nested objects and arrays included). It is a
/// snapshot (a copy, never a view) built when `cap` runs, and the schema the source stores is
/// copied with it. Edges are collected as lazy handles, so the step costs one insert per edge;
/// bulk is invisible (the same edge once or `bulk` times gives the same subgraph).
///
/// By default `cap` builds a new [`TraversalGraph`]. `withSideEffect("sg", graph)`
/// ([`GraphTraversalSource::with_side_effect_graph`]) names an explicit target instead, written
/// through the [`GraphStorage`] mutation API at the end of the step.
///
/// Deviation from TinkerPop: none intended for the vanilla form; `to_graph()` without a label is
/// a Graphersal extension, and the Rust API reads the result with [`GraphTraversalSource::to_graph`]
/// (`next()`/`to_list()` show a graph as `{"vertices": [..], "edges": [..]}`). See the "Subgraph"
/// section of `book/src/users_guide/set_side_effects.md` and `tinkerpop_deviations.md`.
#[derive(Clone)]
struct StepSubgraph {
    label: String,
}

impl fmt::Display for StepSubgraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "subgraph({})",
            crate::value::literal::rhai_string(&self.label)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Collects the incoming edges with both endpoints into a graph side effect; cap() it or use
    /// to_graph() (see the book page Set Side Effects).
    pub fn subgraph(self, label: &str) -> Self {
        self.push_step(GraphStep::SideEffect(
            "subgraph",
            Box::new(StepSubgraph {
                label: label.to_string(),
            }),
        ))
    }
}

impl __ {
    /// Collects the incoming edges with both endpoints into a graph side effect; cap() it or use
    /// to_graph() (see the book page Set Side Effects). Starts an anonymous traversal
    /// (`__.subgraph()`).
    pub fn subgraph<'g, S: GraphStorage + 'g>(label: &'static str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().subgraph(label)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Collects the incoming edges with both endpoints into a graph side effect; cap() it or use
    /// to_graph() (see the book page Set Side Effects).
    pub fn subgraph(self, label: &str) -> Self {
        let step = GraphStep::SideEffect(
            "subgraph",
            Box::new(StepSubgraph {
                label: label.to_string(),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffect<'g, S> for StepSubgraph {
    fn declares_side_effect_key(&self) -> Option<&str> {
        Some(self.label.as_str())
    }

    fn declare_side_effect(
        &self,
        side_effect: &mut std::collections::HashMap<CompactString, SideEffectValue<S>>,
    ) {
        if !side_effect.contains_key(self.label.as_str()) {
            side_effect.insert(
                CompactString::new(&self.label),
                SideEffectValue::Subgraph {
                    edges: IndexSet::default(),
                    target: None,
                },
            );
        }
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<()> {
        match &source.value {
            TraverserValue::Edge(edge) | TraverserValue::EdgeProperty(_, edge, _) => {
                match context.side_effect.get_mut(self.label.as_str()) {
                    Some(SideEffectValue::Subgraph { edges, .. }) => {
                        edges.insert(*edge);
                    }
                    Some(_) => {
                        return Err(Box::new(TraverserError::QueryExecution {
                            query: "subgraph".to_string(),
                            reason: format!(
                                "the side effect '{}' already holds a value that is not a subgraph",
                                self.label
                            ),
                        }));
                    }
                    None => {}
                }
            }
            TraverserValue::Schema(schema) => {
                context.side_effect.insert(
                    CompactString::new(&self.label),
                    SideEffectValue::Value(TraverserValue::Computed(
                        ElementProperty::from(schema.as_ref()),
                        None,
                    )),
                );
            }
            // The kind is checked when the traverser arrives (no allocation, no per-item cost
            // beyond the match): anything but an edge cannot be part of a subgraph. This also
            // holds inside a nested traversal, where the child's stream is checked the same way.
            other => {
                return Err(Box::new(TraverserError::QueryExecution {
                    query: "subgraph".to_string(),
                    reason: format!(
                        "subgraph(\"{}\") collects edges, but the traversal yields a {}",
                        self.label,
                        other.type_name()
                    ),
                }));
            }
        }
        Ok(())
    }

    /// Flushes the collected edges into an explicit `withSideEffect(label, graph)` target. A
    /// default (new) target needs no flush: `cap` builds it from the handles.
    fn finish_batch(&self, context: &mut TraverserContext<'_, S>) -> TraverserResult<()> {
        let Some(SideEffectValue::Subgraph {
            edges,
            target: Some(target),
        }) = context.side_effect.get_mut(self.label.as_str())
        else {
            return Ok(());
        };
        if edges.is_empty() {
            return Ok(());
        }
        let Some(mut guard) = target.try_write() else {
            return Err(Box::new(TraverserError::QueryExecution {
                query: "subgraph".to_string(),
                reason: format!(
                    "the target graph of with_side_effect(\"{}\", graph) is locked: it is the graph being traversed, or in use elsewhere",
                    self.label
                ),
            }));
        };
        let storage: &S = &*context.storage;
        copy_edges_into(storage, edges.drain(..), &mut *guard, true)
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffect<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Terminal: the graph value of this traversal, built from edges. Our extension (TinkerPop has
    /// no such step): the short form of `subgraph("sg")` + `cap("sg")`.
    ///
    /// The traversal must yield edges (every edge becomes part of the graph with both endpoint
    /// vertices, labels, properties and ids, exactly as `subgraph()` copies them), or a single
    /// graph value (`cap("sg")` of a `subgraph("sg")`, returned as is). A stream with no results
    /// gives an empty graph; a stream of anything else, vertices included, is an error.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// let graph = GraphSource::tinkerpop_modern();
    /// let g = graph.read();
    /// let sub = g.traversal().v("1").out_e(None).to_graph().unwrap();
    /// let sub = sub.read();
    /// assert_eq!(sub.edge_count(), 3);
    /// assert_eq!(sub.vertex_count(), 4);
    /// ```
    pub fn to_graph(mut self) -> TraverserResult<Arc<Graph>> {
        // One unit of atomicity, like every terminal: a failure rolls back what the run changed.
        self.atomically(Self::collect_graph)
    }

    /// The body of [`Self::to_graph`].
    fn collect_graph(&mut self) -> TraverserResult<Arc<Graph>> {
        let (traversers, _) = self.internal_to_list_with_context()?;
        let mut edges: IndexSet<S::EdgeId> = IndexSet::default();
        let mut graph_value: Option<Arc<Graph>> = None;
        for traverser in &traversers {
            match &traverser.value {
                TraverserValue::Edge(edge) => {
                    edges.insert(*edge);
                }
                TraverserValue::Graph(graph) if graph_value.is_none() => {
                    graph_value = Some(graph.clone())
                }
                other => {
                    return Err(Box::new(TraverserError::QueryExecution {
                        query: "to_graph".to_string(),
                        reason: format!(
                            "to_graph() needs edges, but the traversal yields a {}",
                            other.type_name()
                        ),
                    }));
                }
            }
        }
        match graph_value {
            Some(graph) if edges.is_empty() && traversers.len() == 1 => Ok(graph),
            Some(_) => Err(Box::new(TraverserError::QueryExecution {
                query: "to_graph".to_string(),
                reason: "to_graph() needs either edges or exactly one graph value, not a mix"
                    .to_string(),
            })),
            None => {
                let mut graph = TraversalGraph::new();
                copy_edges_into(&self.storage, edges.iter().copied(), &mut graph, true)?;
                Ok(Arc::new(Graph::new(graph)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out_e(None).subgraph("S").cap("S").to_list(),
            rhai: [
                r#"g.V("1").outE().subgraph("S").cap("S")"#,
                r#"g.v("1").out_e().subgraph("S").cap("S")"#,
            ],
            assert: |res| {
                // A graph value lists as {"vertices": [..], "edges": [..]}.
                match &res[0] {
                    ElementProperty::Object(map) => {
                        assert!(matches!(map.get("edges"), Some(ElementProperty::Array(e)) if e.len() == 3));
                        assert!(matches!(map.get("vertices"), Some(ElementProperty::Array(v)) if v.len() == 4));
                    },
                    other => panic!("expected the graph listing, got {other:?}"),
                }
            },
        );
    }

    #[test]
    fn vertices_are_an_error() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        // Vertices are not edges: subgraph() fails explicitly instead of collecting nothing.
        assert!(g.v(None).subgraph("S").to_graph().is_err());
        assert!(
            g_lock
                .traversal()
                .v(None)
                .subgraph("S")
                .cap("S")
                .to_graph()
                .is_err()
        );
        assert!(g_lock.traversal().v(None).cap("S").next().is_err());
    }
}
