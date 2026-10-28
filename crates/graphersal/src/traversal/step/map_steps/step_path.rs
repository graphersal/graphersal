use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::path_window::{PathWindow, apply_ring};
use crate::*;

/// `path()`: maps each traverser to the objects it went through, the current one included.
///
/// `by()` modulators are applied round-robin to the path objects; a traverser whose object does
/// not yield a value for its modulator is dropped. Modulators run at `path()` time only: the path
/// arena never stores properties.
///
/// `from(label)` / `to(label)` cut the path down to the window between two labelled positions
/// before the `by()` ring runs (the ring restarts at the first object of the window); the
/// traverser's own path is untouched, so a later `path()` is the whole path again. Following
/// TinkerPop's `Path.subPath`, each bound is the LAST position carrying its label, found
/// independently of the other; a label no position carries, or a `to()` position before the
/// `from()` position, is an error; a repeated `from()`/`to()` replaces the earlier one. See the
/// "Path windows" page of the user's guide.
struct StepPath<'g, S: GraphStorage + 'g> {
    by: Vec<By<'g, S>>,
    window: PathWindow,
    by_check: ByModulatorCheck,
    /// Whether a `by(__...)` child needs the path object it runs on recorded as a one-element
    /// path (`[object]`), so `by(__.path())` sees that single position instead of an empty path
    /// — see `step_select.rs`'s `child_needs_path` for the full rationale (rule 13). Computed
    /// once at plan time, never per traverser.
    child_needs_path: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepPath<'g, S> {
    fn clone(&self) -> Self {
        Self {
            by: self.by.clone(),
            window: self.window.clone(),
            by_check: self.by_check.clone(),
            child_needs_path: self.child_needs_path,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepPath<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "path(){}", self.window.render())?;
        for by in &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> StepPath<'g, S> {
    fn new() -> Self {
        Self {
            by: Vec::new(),
            window: PathWindow::default(),
            by_check: ByModulatorCheck::default(),
            child_needs_path: false,
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Maps each traverser to its path: every object it went through, the current one included.
    ///
    /// Follow it with `by()` to transform the path objects round-robin.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .out(None)
    ///     .out(None)
    ///     .path()
    ///     .by("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// assert_eq!(
    ///     names[0],
    ///     ElementProperty::Array(vec!["marko".into(), "josh".into(), "ripple".into()])
    /// );
    /// ```
    pub fn path(self) -> Self {
        self.push_step(GraphStep::Map("path", Box::new(StepPath::new())))
    }
}

impl __ {
    /// Anonymous `path()`; see [`GraphTraversalSource::path`].
    pub fn path<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().path()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Maps each traverser to its path; see [`GraphTraversalSource::path`].
    pub fn path(self) -> Self {
        let step = GraphStep::Map("path", Box::new(StepPath::new()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepPath<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.by.iter().filter_map(By::property_key) {
            visit(Some(key), crate::graph::workload::AccessKind::Read);
        }
    }
    /// Builds an aggregation/materialization buffer (D3 of the memory-inspection spec) — reported
    /// to `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.plan_check()?;
        let mut path = context.paths.materialize(source.path);
        self.window.slice(&mut path.steps)?;
        Ok(Traverser::with_parts(
            TraverserValue::Path(path),
            source.path,
            source.bulk,
            source.sack,
        ))
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.plan_check()?;
        let mut path = context.paths.materialize(source.path);
        self.window.slice(&mut path.steps)?;
        if !apply_ring(&self.by, self.child_needs_path, context, &mut path.steps)? {
            return Ok(None);
        }
        Ok(Some(Traverser::with_parts(
            TraverserValue::Path(path),
            source.path,
            source.bulk,
            source.sack,
        )))
    }

    fn may_filter(&self) -> bool {
        !self.by.is_empty()
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::Full
    }

    fn plan_check(&self) -> TraverserResult<()> {
        self.by_check.check("path")?;
        self.window.check("path")
    }

    fn push_modulator_args(&mut self, args: Vec<String>, modulator_name: &str) -> ArgsModulation {
        self.window.push(args, modulator_name)
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // A `by(__...)` runs on a path object, a fresh traverser: it reads nothing from the
        // incoming path. What it records is whether the child itself needs a path at all.
        let entry = By::analyze_all_paths(&mut self.by, analysis);
        self.child_needs_path = !entry.is_none();
        PathRequirement::None
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe_ring(&args);
        self.by.push(args);
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by.iter_mut().filter_map(By::traversal_mut).collect()
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    fn names(value: &ElementProperty) -> Vec<String> {
        match value {
            ElementProperty::Array(items) => items
                .iter()
                .map(|item| item.as_string().unwrap_or_default().to_string())
                .collect(),
            other => panic!("expected a path array, got {other:?}"),
        }
    }

    #[cfg(feature = "script")]
    #[test]
    fn out_out_by_name() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).out(None).path().by("name").to_list(),
            rhai: [
                r#"g.V("1").out().out().path().by("name")"#,
                r#"g.v("1").out().out().path().by("name")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(names(&res[0]), vec!["marko", "josh", "ripple"]);
                assert_eq!(names(&res[1]), vec!["marko", "josh", "lop"]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn by_missing_key_drops_the_traverser() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).path().by("age").to_list(),
            rhai: [
                r#"g.V("1").out().path().by("age")"#,
                r#"g.v("1").out().path().by("age")"#,
            ],
            // lop is software and has no age.
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn profile_shows_path() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let exec = g
            .v("1")
            .out(None)
            .path()
            .execute_with(crate::ExecOptions::profile(crate::ProfileType::none()));
        assert_eq!(exec.results().len(), 3);
        let rendered = exec.profile().expect("profile").to_string();
        assert!(rendered.contains("path()"), "{rendered}");
    }
}
