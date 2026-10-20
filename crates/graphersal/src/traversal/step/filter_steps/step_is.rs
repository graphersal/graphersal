use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::*;

#[derive(Clone)]
struct StepIs<B: Into<PValue> + Debug + Clone + Send + Sync> {
    predicate: P<B>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepIs<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "is({})",
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

/// Returns the optimizer tag for an `is()` step.
///
/// `typeOf()` reads a type where every other predicate reads a value, which makes it as cheap
/// and as selective as a label check. The tag is what carries that to the optimizer; the name
/// shown in `.profile()` still comes from the step's canonical rendering, so the plan reads
/// `is(P.typeOf(GType.STRING))` exactly as the query was written.
fn is_step_tag<B: Into<PValue> + Debug + Clone + Send + Sync>(predicate: &P<B>) -> &'static str {
    match predicate {
        P::TypeOf(_) => "is_type_of",
        _ => "is",
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps values equal to the given value or matching the predicate.
    pub fn is<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        let predicate = predicate.into();
        self.push_step(GraphStep::Filter(
            is_step_tag(&predicate),
            Box::new(StepIs { predicate }),
        ))
    }
}

impl __ {
    /// Keeps values equal to the given value or matching the predicate. Starts an anonymous
    /// traversal (`__.is()`).
    pub fn is<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + std::fmt::Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().is(predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Keeps values equal to the given value or matching the predicate.
    pub fn is<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        let predicate = predicate.into();
        let step = GraphStep::Filter(is_step_tag(&predicate), Box::new(StepIs { predicate }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepIs<B>
{
    fn value_index_ranges(&self) -> Option<Vec<crate::graph::PropertyIndexRange>> {
        super::step_has_p::index_ranges(&self.predicate)
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.predicate.test(context, &source.value)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v_name() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .values("name")
            .is("marko")
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    #[test]
    fn v_count_str() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().is(6).to_list(),
            rhai: [
                r#"g.V().count().is(6)"#,
                r#"g.v().count().is(6)"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn v_count_str_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().is(1).to_list(),
            rhai: [
                r#"g.V().count().is(1)"#,
                r#"g.v().count().is(1)"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[test]
    fn v_count_eq() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().is(P::Eq(6)).to_list(),
            rhai: [
                r#"g.V().count().is(P::Eq(6))"#,
                r#"g.v().count().is(P::Eq(6))"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn v_count_neq() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().is(P::Neq(1)).to_list(),
            rhai: [
                r#"g.V().count().is(P::Neq(1))"#,
                r#"g.v().count().is(P::Neq(1))"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn v_count_neq_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().is(P::Neq(6)).to_list(),
            rhai: [
                r#"g.V().count().is(P::Neq(6))"#,
                r#"g.v().count().is(P::Neq(6))"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[test]
    fn v_by_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out("created").in_("created").is(P::Neq("A")).id().to_list(),
            rhai: [
                r#"g.V("1").as("A").out("created").in("created").is(P::Neq("A")).id()"#,
                r#"g.v("1").as("A").out("created").in("created").is(P::Neq("A")).id()"#,
            ],
            // `is()` compares with the literal "A" (TinkerPop), never the label `A`: no vertex
            // equals it, so all three pass.
            assert: |res| {
                let mut ids: Vec<_> = res.iter().filter_map(|id| id.as_string()).collect();
                ids.sort();
                assert_eq!(ids, ["1", "4", "6"]);
            },
        );
    }

    #[test]
    fn v_string_within() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().is(P::WithIn(vec!["1", "2"])).to_list(),
            rhai: [
                r#"g.V().id().is(P::WithIn(["1", "2"]))"#,
                r#"g.v().id().is(P::WithIn(["1", "2"]))"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_string(), Some("1"));
                assert_eq!(res[1].as_string(), Some("2"));
            },
        );
    }

    #[test]
    fn v_number_within() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").is(P::WithIn(vec![29, 32])).to_list(),
            rhai: [
                r#"g.V().values("age").is(P::WithIn([29, 32]))"#,
                r#"g.v().values("age").is(P::WithIn([29, 32]))"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_i64(), Some(29));
                assert_eq!(res[1].as_i64(), Some(32));
            },
        );
    }

    #[test]
    fn v_string_without() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).label().is(P::WithOut(vec!["person"])).to_list(),
            rhai: [
                r#"g.V().label().is(P::WithOut(["person"]))"#,
                r#"g.v().label().is(P::WithOut(["person"]))"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_string(), Some("software"));
                assert_eq!(res[1].as_string(), Some("software"));
            },
        );
    }

    #[test]
    fn e_number_within() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").is(P::WithIn(vec![0.5, 0.2])).to_list(),
            rhai: [
                r#"g.E().values("weight").is(P::WithIn([0.5, 0.2]))"#,
                r#"g.e().values("weight").is(P::WithIn([0.5, 0.2]))"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_f64(), Some(0.5));
                assert_eq!(res[1].as_f64(), Some(0.2));
            },
        );
    }

    #[test]
    fn v_age_inside() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").is(P::Inside(1, 100)).to_list(),
            rhai: [
                r#"g.V("1").values("age").is(P::Inside(1, 100))"#,
                r#"g.v("1").values("age").is(P::Inside(1, 100))"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(29)),
        );
    }

    #[test]
    fn v_age_inside_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").is(P::Inside(29, 100)).to_list(),
            rhai: [
                r#"g.V("1").values("age").is(P::Inside(29, 100))"#,
                r#"g.v("1").values("age").is(P::Inside(29, 100))"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[test]
    fn v_age_outside() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").is(P::Outside(50, 100)).to_list(),
            rhai: [
                r#"g.V("1").values("age").is(P::Outside(50, 100))"#,
                r#"g.v("1").values("age").is(P::Outside(50, 100))"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(29)),
        );
    }

    #[test]
    fn v_age_outside_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").is(P::Outside(1, 100)).to_list(),
            rhai: [
                r#"g.V("1").values("age").is(P::Outside(1, 100))"#,
                r#"g.v("1").values("age").is(P::Outside(1, 100))"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[test]
    fn v_age_between() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").is(P::Between(29, 100)).to_list(),
            rhai: [
                r#"g.V("1").values("age").is(P::Between(29, 100))"#,
                r#"g.v("1").values("age").is(P::Between(29, 100))"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(29)),
        );
    }

    #[test]
    fn v_starting_with() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").is(P::StartingWith("mar")).to_list(),
            rhai: [
                r#"g.V("1").values("name").is(P::StartingWith("mar"))"#,
                r#"g.v("1").values("name").is(P::StartingWith("mar"))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn v_not_starting_with() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").is(P::NotStartingWith("xxx")).to_list(),
            rhai: [
                r#"g.V("1").values("name").is(P::NotStartingWith("xxx"))"#,
                r#"g.v("1").values("name").is(P::NotStartingWith("xxx"))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn v_ending_with() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").is(P::EndingWith("rko")).to_list(),
            rhai: [
                r#"g.V("1").values("name").is(P::EndingWith("rko"))"#,
                r#"g.v("1").values("name").is(P::EndingWith("rko"))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn v_not_ending_with() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").is(P::NotEndingWith("xxx")).to_list(),
            rhai: [
                r#"g.V("1").values("name").is(P::NotEndingWith("xxx"))"#,
                r#"g.v("1").values("name").is(P::NotEndingWith("xxx"))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn v_containing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").is(P::Containing("rko")).to_list(),
            rhai: [
                r#"g.V("1").values("name").is(P::Containing("rko"))"#,
                r#"g.v("1").values("name").is(P::Containing("rko"))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn v_not_containing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").is(P::NotContaining("xxx")).to_list(),
            rhai: [
                r#"g.V("1").values("name").is(P::NotContaining("xxx"))"#,
                r#"g.v("1").values("name").is(P::NotContaining("xxx"))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn is_bool_false() {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property("deleted", false)
            .next()
            .unwrap();

        test_dual!(
            graph: graph,
            rust: |g| g.v("1").values("deleted").is(false).to_list(),
            rhai: [
                r#"g.V("1").values("deleted").is(false)"#,
                r#"g.v("1").values("deleted").is(false)"#,
            ],
            assert: |res| assert_eq!(res[0].as_boolean(), Some(false)),
        );
    }

    #[test]
    fn is_bool_true() {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property("deleted", true)
            .next()
            .unwrap();

        test_dual!(
            graph: graph,
            rust: |g| g.v("1").values("deleted").is(true).to_list(),
            rhai: [
                r#"g.V("1").values("deleted").is(true)"#,
                r#"g.v("1").values("deleted").is(true)"#,
            ],
            assert: |res| assert_eq!(res[0].as_boolean(), Some(true)),
        );
    }

    /// Text predicates read the lazy `id()`/`label()` handles of edges and vertices as strings.
    #[test]
    fn text_predicates_over_edge_and_vertex_ids_and_labels() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).label().is(P::StartingWith("kn")).count().to_list(),
            rhai: [
                r#"g.E().label().is(P.startingWith("kn")).count()"#,
                r#"g.e().label().is(P::starting_with("kn")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).id().is(P::StartingWith("1")).to_list(),
            rhai: [r#"g.E().id().is(P.startingWith("1"))"#],
            assert: |res| assert_eq!(res, &["1".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).id().is(P::Eq("1")).to_list(),
            rhai: [r#"g.E().id().is(P.eq("1"))"#],
            assert: |res| assert_eq!(res, &["1".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).label().is(P::Eq("created")).count().to_list(),
            rhai: [r#"g.E().label().is(P.eq("created")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().is(P::EndingWith("1")).to_list(),
            rhai: [r#"g.V().id().is(P.endingWith("1"))"#],
            assert: |res| assert_eq!(res, &["1".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).label().is(P::Containing("oft")).count().to_list(),
            rhai: [r#"g.V().label().is(P.containing("oft")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        // a number never starts with a text: filtered, not an error
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").is(P::StartingWith("0")).count().to_list(),
            rhai: [r#"g.E().values("weight").is(P.startingWith("0")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    /// TinkerPop's `TextP` only matches a `String`: a list, a map, a path, a vertex or an edge
    /// is not one, so the predicate is `false` (its negation `true`), exactly like a number,
    /// never an error.
    #[test]
    fn text_predicates_on_non_strings_filter_instead_of_failing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().is(P::StartingWith("a")).count().to_list(),
            rhai: [
                r#"g.V().fold().is(TextP.startingWith("a")).count()"#,
                r#"g.v().fold().is(TextP::starting_with("a")).count()"#,
                r#"g.V().fold().is(P.startingWith("a")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().is(P::NotStartingWith("a")).count().to_list(),
            rhai: [r#"g.V().fold().is(TextP.notStartingWith("a")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).is(P::Containing("1")).count().to_list(),
            rhai: [
                r#"g.V().is(TextP.containing("1")).count()"#,
                r#"g.v().is(TextP::containing("1")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).is(P::NotEndingWith("x")).count().to_list(),
            rhai: [r#"g.E().is(TextP.notEndingWith("x")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(None).is(P::EndingWith("o")).count().to_list(),
            rhai: [r#"g.V("1").valueMap().is(TextP.endingWith("o")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).path().is(P::Containing("a")).count().to_list(),
            rhai: [r#"g.V("1").out().path().is(TextP.containing("a")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }
}
