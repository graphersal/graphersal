use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::*;

/// `loops()` / `loops(name)`: the iteration count of the enclosing `repeat()`, read from the
/// loop frames of the execution.
#[derive(Default, Clone)]
struct StepLoops {
    name: Option<Arc<str>>,
}

impl fmt::Display for StepLoops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(name) => write!(f, "loops({})", crate::value::literal::rhai_string(name)),
            None => write!(f, "loops()"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// The number of iterations the innermost enclosing `repeat()` has completed for the
    /// traverser: `0` before the first iteration and inside it, `k + 1` after iteration `k`.
    /// Outside a `repeat()` it is `0`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// // until(loops() is 2) is times(2).
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .repeat(__::out(None))
    ///     .until(__::loops().is(2))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// ```
    pub fn loops(self) -> Self {
        self.push_step(GraphStep::Map("loops", Box::new(StepLoops::default())))
    }

    /// The iteration count of the nearest enclosing loop started with
    /// [`repeat_named(name, ..)`](Self::repeat_named). Fails with
    /// [`TraverserError::UnknownLoopName`] when no enclosing loop has that name.
    pub fn loops_named(self, name: &str) -> Self {
        self.push_step(GraphStep::Map(
            "loops",
            Box::new(StepLoops {
                name: Some(Arc::from(name)),
            }),
        ))
    }
}

impl __ {
    /// Anonymous `loops()`; see [`GraphTraversalSource::loops`].
    pub fn loops<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().loops()
    }

    /// Anonymous `loops(name)`; see [`GraphTraversalSource::loops_named`].
    pub fn loops_named<'g, S: GraphStorage + 'g>(name: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().loops_named(name)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `loops()`; see [`GraphTraversalSource::loops`].
    pub fn loops(self) -> Self {
        self.push_step(GraphStep::Map("loops", Box::new(StepLoops::default())))
    }

    /// Anonymous `loops(name)`; see [`GraphTraversalSource::loops_named`].
    pub fn loops_named(self, name: &str) -> Self {
        self.push_step(GraphStep::Map(
            "loops",
            Box::new(StepLoops {
                name: Some(Arc::from(name)),
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepLoops {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let loops = match &self.name {
            None => context.loop_stack.last().map_or(0, |frame| frame.loops),
            Some(name) => match context
                .loop_stack
                .iter()
                .rev()
                .find(|frame| frame.name.as_deref() == Some(&**name))
            {
                Some(frame) => frame.loops,
                None => {
                    return Err(Box::new(TraverserError::UnknownLoopName {
                        name: name.to_string(),
                        active: context
                            .loop_stack
                            .iter()
                            .rev()
                            .filter_map(|frame| frame.name.as_deref().map(str::to_string))
                            .collect(),
                    }));
                }
            },
        };
        Ok(Traverser::with_parts(
            TraverserValue::Computed(ElementProperty::Number(i64::from(loops).into()), None),
            source.path,
            source.bulk,
            source.sack,
        ))
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn loops_outside_a_repeat_is_zero() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").loops().to_list(),
            rhai: [r#"g.V("1").loops()"#, r#"g.v("1").loops()"#],
            assert: |res| assert_eq!(res, &[ElementProperty::Number(0.into())]),
        );
    }

    #[test]
    fn loops_inside_until() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").repeat(__::out(None)).until(__::loops().is(1)).count().to_list(),
            rhai: [r#"g.V("1").repeat(__.out()).until(__.loops().is(1)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn named_loops_inside_until() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .repeat_named("a", __::out(None))
                .until(__::loops_named("a").is(2))
                .count()
                .to_list(),
            rhai: [r#"g.V("1").repeat("a", __.out()).until(__.loops("a").is(2)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }
}
