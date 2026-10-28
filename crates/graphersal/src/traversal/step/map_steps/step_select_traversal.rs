use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

use super::step_select::StepSelect;

/// `select(Pop, traversal)` / `select(traversal)`: TinkerPop's `TraversalSelectStep`. The key is
/// not a literal but the **first result** of a child traversal run on the incoming traverser
/// (path included); that key is then looked up exactly like `select("key")` (a key of the
/// traverser's own map, a side effect, a path label read with the `Pop`).
///
/// A key that is not a string (a vertex, a number, ...) is only looked up as a key of the
/// incoming map (`group().by()` keys are elements). A key traversal without a result, or a key
/// that is not found, filters the traverser (TinkerPop raises `KeyNotFoundException`, which
/// `select` turns into an empty traverser). Only one `by()` applies, the last one.
pub(super) struct StepSelectTraversal<'g, S: GraphStorage + 'g> {
    pop: Pop,
    key: AnonymousTraversal<'g, S>,
    by: Option<Vec<By<'g, S>>>,
    by_check: ByModulatorCheck,
    /// See `StepSelect::child_needs_path`; computed once at plan time.
    child_needs_path: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepSelectTraversal<'g, S> {
    fn clone(&self) -> Self {
        Self {
            pop: self.pop,
            key: self.key.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
            child_needs_path: self.child_needs_path,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepSelectTraversal<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by = String::new();
        if let Some(by_list) = self.by.as_ref() {
            by_list
                .iter()
                .for_each(|b| by.push_str(&crate::traversal::render_by(b)));
        }
        if self.pop == Pop::Last {
            write!(f, "select({}){}", self.key, by)
        } else {
            write!(f, "select(Pop.{}, {}){}", self.pop, self.key, by)
        }
    }
}

impl<'g, S: GraphStorage + 'g> StepSelectTraversal<'g, S> {
    fn new(pop: Pop, key: AnonymousTraversal<'g, S>) -> Self {
        Self {
            pop,
            key,
            by: None,
            by_check: ByModulatorCheck::default(),
            child_needs_path: false,
        }
    }
}

fn select_traversal_step<'g, S: GraphStorage + 'g>(
    pop: Pop,
    key: AnonymousTraversal<'g, S>,
) -> GraphStep<'g, S> {
    GraphStep::Map("select", Box::new(StepSelectTraversal::new(pop, key)))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `select(Pop, traversal)` (and plain `select(traversal)` with [`Pop::Last`]): the key is the
    /// first result of `key`, a child traversal run on the incoming traverser with its path. A
    /// string key is read like [`select_pop`](Self::select_pop) reads a label; any other key
    /// only as a key of the incoming map. No result, or a key that is not found, filters the
    /// traverser.
    ///
    /// Because the key is unknown at plan time, the full path is recorded (no bulk merging
    /// upstream), see `book/src/users_guide/select_pop.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let g = lock.traversal();
    /// let names = g
    ///     .v("1")
    ///     .as_("a")
    ///     .out("knows")
    ///     .as_("a")
    ///     .select_traversal(Pop::All, __::constant("a"))
    ///     .by(__::unfold().values("name").fold())
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// ```
    pub fn select_traversal(self, pop: Pop, key: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(select_traversal_step(pop, key))
    }
}

impl __ {
    /// Anonymous `select(Pop, traversal)`; see [`GraphTraversalSource::select_traversal`].
    pub fn select_traversal<'g, S: GraphStorage + 'g>(
        pop: Pop,
        key: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().select_traversal(pop, key)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `select(Pop, traversal)`; see [`GraphTraversalSource::select_traversal`].
    pub fn select_traversal(self, pop: Pop, key: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(select_traversal_step(pop, key))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepSelectTraversal<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        // `may_filter()` is always true, so the executor calls `process_filter`.
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "select() dropped a traverser in a context that cannot filter".to_string(),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.by_check.check("select")?;
        // TinkerPop `TraversalSelectStep`: the first result of the key traversal, run on the
        // incoming traverser (path included), is the key.
        let Some(first) = self.key.first(context, &source)? else {
            return Ok(None);
        };
        let key_value =
            StepSelect::<'_, S>::resolve_lazy(context, &first.value)?.unwrap_or(first.value);
        let resolved = StepSelect::<'_, S>::resolve_lazy(context, &source.value)?;
        let map = resolved.as_ref().unwrap_or(&source.value);
        let value = match &key_value {
            TraverserValue::Computed(ElementProperty::String(key), _) => {
                StepSelect::<'_, S>::scope_value(context, &source, map, key.as_str(), self.pop)?
            }
            // Any other key (a vertex, a number, ...) can only be a key of the incoming map, as
            // in TinkerPop's `TraversalSelectStep` (side effects and path labels are named by
            // strings): `group().by()` keys are elements, `groupCount()` keys numbers.
            other => match map {
                TraverserValue::Object(entries) => entries.get(other).cloned(),
                _ => None,
            },
        };
        let Some(value) = value else {
            return Ok(None);
        };
        let by = self.by.as_deref().and_then(<[By<'g, S>]>::last);
        Ok(
            StepSelect::<'g, S>::project(context, value, by, self.child_needs_path)?
                .map(|value| Traverser::with_parts(value, source.path, source.bulk, source.sack)),
        )
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe_ring(&args);
        if let Some(x) = self.by.as_mut() {
            x.push(args);
        } else {
            self.by = Some(vec![args]);
        }
        ByModulation::Accepted
    }

    fn path_requirement(&self) -> PathRequirement {
        // The key is computed at run time, so no label can be named at plan time, and the
        // non-`last` pops need every occurrence anyway: record the full path.
        PathRequirement::Full
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // The key traversal runs on the incoming traverser, path included, and its output does
        // not continue the path. A `by(__...)` runs on the selected object, a fresh traverser;
        // whether it needs a path of its own decides `child_needs_path`.
        let key_need = self.key.analyze_paths(analysis, PathRequirement::None);
        if let Some(by_list) = &mut self.by {
            let entry = By::analyze_all_paths(by_list, analysis);
            self.child_needs_path = !entry.is_none();
        }
        key_need
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        let mut children = vec![&mut self.key];
        children.extend(self.by.iter_mut().flatten().filter_map(By::traversal_mut));
        children
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
