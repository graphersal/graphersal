use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// `otherV()`: from an edge, the vertex on the side opposite the one the traversal visited right
/// before reaching this edge.
///
/// TinkerPop defines "the other side" relative to where the traversal came from, not as a fixed
/// endpoint — `g.V(1).bothE("knows").otherV()` returns vertex `1`'s neighbors, never vertex `1`
/// itself, regardless of whether the edge happened to be stored with `1` as its tail or its head.
///
/// # Path lookback (D1)
///
/// A bare `TraverserValue::Edge(id)` carries no memory of which vertex produced it, so `otherV()`
/// has to read the traverser's recorded path. This codebase's path infrastructure
/// (`traversal/optimizer/path_analysis.rs`) only offers two things a step can ask for:
/// [`PathRequirement::Labels`], which records exactly the positions an explicit `as()` labels and
/// nothing else, and [`PathRequirement::Full`], which records every position. There is no
/// existing "one position back, unlabeled" mode, and `otherV()` cannot use `Labels` because the
/// vertex it needs was very likely never given an `as()` label — `g.V(1).bothE("knows").otherV()`
/// has no `as()` in it at all.
///
/// `otherV()` therefore declares [`PathRequirement::Full`] and, at execution time, walks
/// `source.path` backward (`TraverserContext::paths`, the path arena) for the nearest position
/// whose value is a [`TraverserValue::Vertex`] — mirroring TinkerPop's own `EdgeOtherVertexStep`,
/// which likewise scans the traverser's path backward for the nearest vertex rather than reading
/// a fixed slot. Adding a narrower `PathRequirement` variant just for "the previous position"
/// was considered and rejected: it would need its own bookkeeping in the analysis pass and the
/// executor (`traversal/source.rs`'s `touch_paths`/`extends_path` machinery) for a saving that
/// only matters on long chains that also use `otherV()`, which is not
/// a known hot path — reusing `Full` is the smallest correct change, per this spec's
/// non-goals. The cost is paid only by queries that actually call `otherV()`: the path
/// requirement analysis (`PathAnalysis::analyze_level`) propagates `Full` upstream from this step
/// alone, so a query without `otherV()` is unaffected.
///
/// # No preceding vertex
///
/// `g.E().otherV()` starts directly from edges: there is no vertex earlier in the path to compare
/// against. TinkerPop's own behaviour here is not something a bare edge traverser can recover
/// (there is no "from" to speak of), so this step fails with
/// [`TraverserError::OtherVWithoutOrigin`] rather than guessing an endpoint — per rule 1, an
/// explicit, helpful error instead of silently picking a side.
#[derive(Default, Clone)]
struct StepOtherV {}

impl fmt::Display for StepOtherV {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "other_v()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// From an edge, the vertex opposite the one the traversal came from. "Came from" is the
    /// nearest vertex found by walking the traverser's recorded path backward from this edge;
    /// starting directly from edges, with no vertex earlier in the path (e.g. `g.E().other_v()`),
    /// fails with [`TraverserError::OtherVWithoutOrigin`].
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .both_e("knows")
    ///     .other_v()
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// // Marko's "knows" neighbors, never marko himself.
    /// assert!(!names.iter().any(|n| n.as_string() == Some("marko")));
    /// ```
    pub fn other_v(self) -> Self {
        self.push_step(GraphStep::FlatMap("other_v", Box::new(StepOtherV {})))
    }
}

impl __ {
    /// Anonymous `other_v()`; see [`GraphTraversalSource::other_v`].
    pub fn other_v<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().other_v()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `other_v()`; see [`GraphTraversalSource::other_v`].
    pub fn other_v(self) -> Self {
        let step = GraphStep::FlatMap("other_v", Box::new(StepOtherV {}));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepOtherV {
    fn is_adjacency(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let edge_index: Option<S::EdgeId> = match source.value {
            TraverserValue::Edge(edge_index) | TraverserValue::EdgeProperty(_, edge_index, _) => {
                Some(edge_index)
            }
            TraverserValue::Vertex(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "vertex".to_string(),
                )));
            }
            TraverserValue::VertexProperty(_, _, _) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "vertexProperty".to_string(),
                )));
            }
            TraverserValue::Computed(_, _) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "computed".to_string(),
                )));
            }
            TraverserValue::Path(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "path".to_string(),
                )));
            }
            TraverserValue::PathProperty(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "pathProperty".to_string(),
                )));
            }
            TraverserValue::Entry(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "entry".to_string(),
                )));
            }
            TraverserValue::Object(_) | TraverserValue::Record { .. } => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "object".to_string(),
                )));
            }
            TraverserValue::Array(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "array".to_string(),
                )));
            }
            TraverserValue::Schema(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "schema".to_string(),
                )));
            }
            TraverserValue::Graph(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "graph".to_string(),
                )));
            }
        };

        let edge_index = match edge_index {
            Some(edge_index) => edge_index,
            None => {
                return Err(Box::new(TraverserError::cast_exception(
                    &["edge", "edgeProperty"],
                    &source.value,
                )));
            }
        };

        // D1: walk the recorded path backward for the nearest vertex position — the vertex the
        // traversal visited right before reaching this edge.
        let origin = context
            .paths
            .iter_rev(source.path)
            .find_map(|node| match &node.value {
                TraverserValue::Vertex(vertex) => Some(*vertex),
                _ => None,
            });

        let origin = match origin {
            Some(origin) => origin,
            None => {
                return Err(Box::new(TraverserError::OtherVWithoutOrigin {
                    edge: format!("{:?}", edge_index),
                }));
            }
        };

        match context.storage.edge_endpoints(edge_index)? {
            Some((from, to)) => {
                let other = if origin == from { to } else { from };
                output.push(Traverser::with_parts(
                    TraverserValue::Vertex(other),
                    source.path,
                    source.bulk,
                    source.sack,
                ));
                Ok(())
            }
            // A removed edge (stale handle) has no endpoints: nothing to emit.
            None if !context.storage.contains_edge(edge_index)? => Ok(()),
            None => Err(Box::new(TraverserError::Graph(GraphError::EdgeNotFound {
                id: format!("{:?}", edge_index),
            }))),
        }
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::Full
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_neighbor_not_the_origin() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").both_e("knows").other_v().values("name").to_list(),
            rhai: [
                r#"g.V("1").bothE("knows").otherV().values("name")"#,
                r#"g.v("1").both_e("knows").other_v().values("name")"#,
            ],
            assert: |res| {
                let names: Vec<_> = res.iter().filter_map(|v| v.as_string()).collect();
                assert!(!names.contains(&"marko"), "{names:?}");
                assert_eq!(names.len(), 2);
            },
        );
    }

    #[test]
    fn survives_a_chain_of_hops() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).out_e(None).other_v().id().to_list(),
            rhai: [
                r#"g.V("1").out().outE().otherV().id()"#,
                r#"g.v("1").out().out_e().other_v().id()"#,
            ],
            assert: |res| assert!(!res.is_empty()),
        );
    }

    #[test]
    fn no_preceding_vertex_is_an_error() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let err = lock.traversal().e(None).other_v().to_list().unwrap_err();
        let report = crate::error::format_diagnostic(&*err);
        assert!(report.contains("no vertex earlier"), "{report}");
        assert!(report.contains("both_v()"), "{report}");
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").both_e("knows").other_v().id().profile().unwrap();
        assert!(result.to_string().contains("other_v("));
    }
}
