use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

/// `where(P)`, optionally followed by `by()` modulators.
///
/// **Resolution order (user's guide: "Predicates: Comparison and Resolution"):** a bare string
/// operand of `eq`/`neq`/`lt`/`gt`/`lte`/`gte`/`within`/`without` is resolved as a path label
/// first, then as a side-effect key (`aggregate`/`store`/`withSideEffect`), and only then taken
/// as a literal. A side effect holding a collection compares as a whole value for `eq`/`neq`
/// and is incomparable (false) for the ordering predicates. (For `lt`/`lte`/`gt`/`gte` the label
/// resolution is new: they used to compare the label text itself.)
///
/// **`by()` ring (TinkerPop's `WherePredicateStep` traversal ring):** the current object is
/// projected through the first `by()`, each label operand through the next ones, in the textual
/// order of the predicate (`and`/`or`/`not` children included, whatever short-circuiting skips),
/// cycling when there are fewer `by()` than operands: `where(P.gt("a")).by("age")` compares the
/// `age` of the current vertex with the `age` of the vertex labelled `a`;
/// `.by("age").by("weight")` reads `age` of the current object and `weight` of the labelled one.
/// A `by()` that yields no value drops the traverser (also under a `not`). A comparison without a
/// label operand (`where(P.gt(30)).by("age")`) projects only the current object.
///
/// **Deviations from TinkerPop:** the ring index of a label operand follows the textual order of
/// the predicate, not the evaluation order of TinkerPop's connective handling (not verified;
/// no vendored scenario covers a composite predicate with `by()`). `where(P).by()` with step
/// labels in `within`/`without` is not implemented. See the "where(P) and by()" section of the
/// `where()` scope page.
///
/// **`where("a", P)`** (`start`): the predicate tests the object labelled `a` instead of the
/// current object, and the `by()` ring starts with it (TinkerPop's `WherePredicateStep` with a
/// start key).
struct StepWhereP<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync> {
    predicate: P<B>,
    /// `where("a", P)`: the label whose object (map key, side effect or path label, as
    /// `select("a")` reads it) is tested instead of the current object.
    start: Option<CompactString>,
    by: Vec<By<'g, S>>,
    by_check: ByModulatorCheck,
    /// Whether a `by(__...)` child needs the projected object recorded as a one-element path
    /// (rule 13, see `step_path.rs`). Computed once at plan time.
    child_needs_path: bool,
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync> Clone
    for StepWhereP<'g, S, B>
{
    fn clone(&self) -> Self {
        Self {
            predicate: self.predicate.clone(),
            start: self.start.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
            child_needs_path: self.child_needs_path,
        }
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync> StepWhereP<'g, S, B> {
    fn new(predicate: P<B>) -> Self {
        Self {
            predicate,
            start: None,
            by: Vec::new(),
            by_check: ByModulatorCheck::default(),
            child_needs_path: false,
        }
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display
    for StepWhereP<'g, S, B>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.start {
            Some(start) => write!(
                f,
                "where({}, {})",
                crate::value::literal::rhai_string(start),
                crate::value::predicate::render_predicate(&self.predicate)
            )?,
            None => write!(
                f,
                "where({})",
                crate::value::predicate::render_predicate(&self.predicate)
            )?,
        }
        for by in &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Graphersal form of where(predicate): compares the current object with a labelled one,
    /// modulated by by().
    pub fn where_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        predicate: T,
    ) -> Self {
        self.push_step(GraphStep::Filter(
            "where_p",
            Box::new(StepWhereP::new(predicate.into())),
        ))
    }

    /// `where("a", P)`: like [`Self::where_p`], but the predicate tests the object labelled
    /// `start` (looked up like `select(start)`: a key of the current map, a side effect, then
    /// the path label) instead of the current object; a traverser without it is filtered out.
    /// The `by()` ring starts with that object.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let co_creators = lock
    ///     .traversal()
    ///     .v("1")
    ///     .as_("a")
    ///     .out("created")
    ///     .in_("created")
    ///     .as_("b")
    ///     .where_label_p("a", P::Neq("b"))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(co_creators.len(), 2);
    /// ```
    pub fn where_label_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        start: &str,
        predicate: T,
    ) -> Self {
        self.push_step(where_label_step(start, predicate.into()))
    }
}

/// The step of `where(start, P)`.
fn where_label_step<'g, S: GraphStorage + 'g, B>(start: &str, predicate: P<B>) -> GraphStep<'g, S>
where
    B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
{
    let mut step = StepWhereP::new(predicate);
    step.start = Some(CompactString::new(start));
    GraphStep::Filter("where_p", Box::new(step))
}

impl __ {
    /// Graphersal form of where(predicate): compares the current object with a labelled one,
    /// modulated by by(). Starts an anonymous traversal (`__.where_p()`).
    pub fn where_p<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + std::fmt::Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().where_p(predicate)
    }

    /// Anonymous `where("a", P)`; see [`GraphTraversalSource::where_label_p`].
    pub fn where_label_p<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + std::fmt::Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        start: &str,
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().where_label_p(start, predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Graphersal form of where(predicate): compares the current object with a labelled one,
    /// modulated by by().
    pub fn where_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        predicate: T,
    ) -> Self {
        let step = GraphStep::Filter("where_p", Box::new(StepWhereP::new(predicate.into())));
        self.push_step(step)
    }

    /// `where("a", P)` in a child traversal; see [`GraphTraversalSource::where_label_p`].
    pub fn where_label_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        start: &str,
        predicate: T,
    ) -> Self {
        self.push_step(where_label_step(start, predicate.into()))
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepWhereP<'g, S, B>
{
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.by_check.check("where")?;
        let started;
        let current = match &self.start {
            None => &source.value,
            Some(start) => {
                let Some(value) = crate::traversal::step::map_steps::step_select::scope_value(
                    context, source, start,
                )?
                else {
                    return Ok(false);
                };
                started = value;
                &started
            }
        };
        if self.by.is_empty() {
            // Resolves a bare string against a step label, then a side-effect bucket, then a
            // literal; a composite predicate resolves each child on its own (`P::test_where`).
            return self.predicate.test_where(context, current, source.path);
        }
        Ok(self
            .predicate
            .test_where_by(
                context,
                current,
                source.path,
                &self.by,
                self.child_needs_path,
                0,
            )?
            .unwrap_or(false))
    }

    fn path_requirement(&self) -> PathRequirement {
        let operands = self.predicate.path_requirement();
        match &self.start {
            Some(start) => operands.union(PathRequirement::labels([start.clone()])),
            None => operands,
        }
    }

    fn plan_check(&self) -> TraverserResult<()> {
        self.by_check.check("where")
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe_ring(&args);
        self.by.push(args);
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by.iter_mut().filter_map(By::traversal_mut).collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // A `by(__...)` runs on a projected object, a fresh traverser: it reads nothing from the
        // incoming path; what it records is whether the child needs a path at all.
        let entry = By::analyze_all_paths(&mut self.by, analysis);
        self.child_needs_path = !entry.is_none();
        PathRequirement::None
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
    fn v_name() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").where_p("marko").count().to_list(),
            rhai: [
                r#"g.V().values("name").where("marko").count()"#,
                r#"g.v().values("name").where("marko").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").where_t(__::where_p("marko")).count().to_list(),
            rhai: [
                r#"g.V().values("name").where(__.where("marko")).count()"#,
                r#"g.v().values("name").where(__.where("marko")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::values("name").where_p("marko")).count().to_list(),
            rhai: [
                r#"g.V().where(__.values("name").where("marko")).count()"#,
                r#"g.v().where(__.values("name").where("marko")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_count_str() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().where_p(6).to_list(),
            rhai: [
                r#"g.V().count().where(6)"#,
                r#"g.v().count().where(6)"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_count_str_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().where_p(1).to_list(),
            rhai: [
                r#"g.V().count().where(1)"#,
                r#"g.v().count().where(1)"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_count_eq() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().where_p(P::Eq(6)).to_list(),
            rhai: [
                r#"g.V().count().where(P::Eq(6))"#,
                r#"g.v().count().where(P::Eq(6))"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_count_neq() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().where_p(P::Neq(1)).to_list(),
            rhai: [
                r#"g.V().count().where(P::Neq(1))"#,
                r#"g.v().count().where(P::Neq(1))"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_count_neq_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().where_p(P::Neq(6)).to_list(),
            rhai: [
                r#"g.V().count().where(P::Neq(6))"#,
                r#"g.v().count().where(P::Neq(6))"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).count().where_p(P::Neq(6)).profile().unwrap();
        assert!(result.to_string().contains("where(P.neq(6))"), "{result}");
    }
}
