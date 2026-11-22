use crate::graph::{GraphStorage, LayerOrigin};
use crate::*;

/// The bit of `origin` in a [`StepHasLayer`] mask.
#[inline]
const fn origin_bit(origin: LayerOrigin) -> u8 {
    match origin {
        LayerOrigin::Base => 1,
        LayerOrigin::Added => 2,
        LayerOrigin::Modified => 4,
    }
}

/// Every origin: what `has_layer()` without arguments keeps.
const ALL_ORIGINS: u8 = origin_bit(LayerOrigin::Base)
    | origin_bit(LayerOrigin::Added)
    | origin_bit(LayerOrigin::Modified);

/// `has_layer(origins...)`: keeps the vertices and edges whose layer origin is one of the named
/// ones. The names are parsed once when the step is built into a bit mask; an unknown name is
/// kept for the plan-time validation pass ([`TraverserError::UnknownLayerOrigin`]), so it fails
/// before any traverser runs.
#[derive(Clone)]
struct StepHasLayer {
    /// The names as given, for rendering; `None` for `has_layer()`.
    names: Option<Vec<String>>,
    /// The origins kept, one bit each ([`origin_bit`]).
    mask: u8,
    /// The first name that is not an origin.
    unknown: Option<String>,
}

impl StepHasLayer {
    fn new(origins: ArgStrings) -> Self {
        let names = match origins {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let Some(list) = &names else {
            return Self {
                names,
                mask: ALL_ORIGINS,
                unknown: None,
            };
        };
        let mut mask = 0;
        let mut unknown = None;
        for name in list {
            match LayerOrigin::from_name(name) {
                Some(origin) => mask |= origin_bit(origin),
                None => {
                    if unknown.is_none() {
                        unknown = Some(name.clone());
                    }
                }
            }
        }
        Self {
            names,
            mask,
            unknown,
        }
    }

    #[inline]
    fn keeps(&self, origin: Option<LayerOrigin>) -> bool {
        // `None`: removed while the traversal ran; like `has_label()` a removed element no
        // longer passes.
        origin.is_some_and(|origin| self.mask & origin_bit(origin) != 0)
    }
}

impl fmt::Display for StepHasLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "has_layer({})", display_option_vec_string(&self.names))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps the vertices and edges whose layer origin (see [`Self::layer`]) is one of
    /// `origins`: `"base"`, `"added"` or `"modified"`. On a graph without layers every element
    /// is `"base"`. `has_layer(None)` (the DSL's `has_layer()`) keeps every vertex and edge, an
    /// empty list keeps nothing. An unknown name fails the traversal before it runs
    /// ([`TraverserError::UnknownLayerOrigin`]). A Graphersal extension.
    pub fn has_layer<T: Into<ArgStrings>>(self, origins: T) -> Self {
        self.push_step(GraphStep::Filter(
            "has_layer",
            Box::new(StepHasLayer::new(origins.into())),
        ))
    }
}

impl __ {
    /// Anonymous [`GraphTraversalSource::has_layer`] (`__.has_layer("added")`).
    pub fn has_layer<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        origins: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_layer(origins)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Child-traversal form of [`GraphTraversalSource::has_layer`].
    pub fn has_layer<T: Into<ArgStrings>>(self, origins: T) -> Self {
        self.push_step(GraphStep::Filter(
            "has_layer",
            Box::new(StepHasLayer::new(origins.into())),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasLayer {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Vertex(node_index) => {
                Ok(self.keeps(context.storage.vertex_layer(*node_index)?))
            }
            TraverserValue::Edge(edge_index) => {
                Ok(self.keeps(context.storage.edge_layer(*edge_index)?))
            }
            t => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge"],
                t,
            ))),
        }
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match &self.unknown {
            Some(name) => Err(Box::new(TraverserError::UnknownLayerOrigin {
                name: name.clone(),
            })),
            None => Ok(()),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_into_a_mask() {
        let step = StepHasLayer::new(vec!["added", "modified"].into());
        assert_eq!(step.mask, 6);
        assert!(step.unknown.is_none());
        assert_eq!(StepHasLayer::new(ArgStrings::None).mask, ALL_ORIGINS);
        assert_eq!(StepHasLayer::new(Vec::<String>::new().into()).mask, 0);
        let bad = StepHasLayer::new(vec!["base", "new", "old"].into());
        assert_eq!(bad.mask, 1);
        assert_eq!(bad.unknown.as_deref(), Some("new"));
        assert!(!step.keeps(None));
        assert!(step.keeps(Some(LayerOrigin::Added)));
        assert!(!step.keeps(Some(LayerOrigin::Base)));
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_plain_graph_is_all_base() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_layer("base").count().to_list(),
            rhai: [r#"g.v().has_layer("base").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_layer(vec!["added", "modified"]).count().to_list(),
            rhai: [r#"g.e().has_layer("added", "modified").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .has_label("person")
            .has_layer("base")
            .count()
            .profile()
            .unwrap();
        assert!(
            result.to_string().contains("has_layer(\"base\")"),
            "{result}"
        );
    }
}
