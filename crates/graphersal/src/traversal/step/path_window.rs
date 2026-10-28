//! The `from()`/`to()`/`by()` machinery shared by `path()`, `simple_path()` and `cyclic_path()`
//! (TinkerPop's `PathProcessor` steps).
//!
//! A [`PathWindow`] selects the part of a traverser's path between two labelled positions (TinkerPop
//! `Path.subPath(fromLabel, toLabel)`); [`apply_ring`] projects the objects of that window through
//! the step's `by()` ring, which restarts at the first object of the window.
//! [`SimplicityFilter`] is the shared body of `simple_path()` and `cyclic_path()`.

use compact_str::CompactString;

use crate::error::{TraverserError, TraverserResult, ValueError};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::{
    AnonymousTraversal, ArgsModulation, By, ByModulation, ByModulatorCheck, Traverser,
    TraverserContext, args_reason,
};
use crate::value::path::TraverserPathStep;
use crate::value::{PathRef, TraverserValue};

/// The `from()`/`to()` labels of a path step.
///
/// Bounds follow TinkerPop's `Path.subPath(from, to)`: the window starts at the LAST position
/// carrying the `from` label (position 0 without `from()`) and ends at the LAST position carrying
/// the `to` label (the final position without `to()`), both inclusive and found independently of
/// each other; a window that ends before it starts is an error. A second `from()` (or `to()`)
/// replaces the first, as TinkerPop's `addFrom`/`addTo` setters do.
#[derive(Debug, Clone, Default)]
pub(crate) struct PathWindow {
    from: Option<CompactString>,
    to: Option<CompactString>,
    /// Number of `from()`/`to()` calls received, for the position of a rejection.
    calls: usize,
    /// Position and reason of the first rejected `from()`/`to()` call.
    rejected: Option<(usize, &'static str)>,
}

impl PathWindow {
    /// Whether `from()` or `to()` was given.
    pub(crate) fn is_set(&self) -> bool {
        self.from.is_some() || self.to.is_some()
    }

    /// Takes one `from()`/`to()` call. Exactly one label is expected; anything else is recorded
    /// and raised by [`check`](Self::check).
    pub(crate) fn push(&mut self, args: Vec<String>, modulator: &str) -> ArgsModulation {
        self.calls += 1;
        let mut args = args.into_iter();
        match (args.next(), args.next()) {
            (Some(label), None) => {
                let label = CompactString::from(label);
                if modulator == "to" {
                    self.to = Some(label);
                } else {
                    self.from = Some(label);
                }
            }
            _ => {
                if self.rejected.is_none() {
                    self.rejected = Some((self.calls, args_reason::ONE_LABEL));
                }
            }
        }
        ArgsModulation::Accepted
    }

    /// Fails with `InvalidModulator` for `step` when a `from()`/`to()` call was rejected.
    pub(crate) fn check(&self, step: &str) -> TraverserResult<()> {
        match self.rejected {
            Some((position, reason)) => Err(Box::new(TraverserError::InvalidModulator {
                step: step.to_string(),
                position,
                modulator: args_reason::MODULATOR,
                reason: reason.to_string(),
            })),
            None => Ok(()),
        }
    }

    /// The window as a half-open index range into `steps`.
    pub(crate) fn bounds<S: GraphStorage>(
        &self,
        steps: &[TraverserPathStep<S>],
    ) -> TraverserResult<std::ops::Range<usize>> {
        let last_with = |label: &str| {
            steps.iter().rposition(|step| {
                step.label
                    .as_ref()
                    .is_some_and(|labels| labels.iter().any(|candidate| candidate == label))
            })
        };
        let missing = |label: &CompactString| {
            Box::new(TraverserError::Value(ValueError::PropertyNotFound {
                key: label.to_string(),
                element_type: "path",
            }))
        };
        let start = match &self.from {
            Some(label) => last_with(label).ok_or_else(|| missing(label))?,
            None => 0,
        };
        let end = match &self.to {
            Some(label) => last_with(label).ok_or_else(|| missing(label))?,
            None => steps.len().saturating_sub(1),
        };
        if start > end {
            return Err(Box::new(TraverserError::QueryExecution {
                query: "path window".to_string(),
                reason: format!(
                    "the path position labelled '{}' comes after the one labelled '{}', so from() \
                     and to() select no sub-path; swap them or label the positions in path order \
                     with as()",
                    self.from.as_deref().unwrap_or(""),
                    self.to.as_deref().unwrap_or(""),
                ),
            }));
        }
        if steps.is_empty() {
            return Ok(0..0);
        }
        Ok(start..end + 1)
    }

    /// Cuts `steps` down to the window.
    pub(crate) fn slice<S: GraphStorage>(
        &self,
        steps: &mut Vec<TraverserPathStep<S>>,
    ) -> TraverserResult<()> {
        if !self.is_set() {
            return Ok(());
        }
        let range = self.bounds(steps)?;
        steps.truncate(range.end);
        steps.drain(..range.start);
        Ok(())
    }

    /// Renders `.from("a").to("b")` for the step's `Debug` text.
    pub(crate) fn render(&self) -> String {
        let mut text = String::new();
        if let Some(from) = &self.from {
            text.push_str(&format!(
                ".from({})",
                crate::value::literal::rhai_string(from)
            ));
        }
        if let Some(to) = &self.to {
            text.push_str(&format!(".to({})", crate::value::literal::rhai_string(to)));
        }
        text
    }
}

/// Projects one object through a `by()`: `None` when the `by()` is unproductive (a missing
/// property, a child yielding nothing). A `by(__...)` child that reads a path sees the object as a
/// one-element path when `child_needs_path` is set (see the path analysis, rule 13).
pub(crate) fn project_object<'g, S: GraphStorage + 'g>(
    by: &By<'g, S>,
    child_needs_path: bool,
    context: &mut TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> TraverserResult<Option<TraverserValue<S>>> {
    let mut object = Traverser::start(context, value.clone());
    if child_needs_path && matches!(by, By::Traversal(_)) {
        object.path = context
            .paths
            .push(PathRef::NONE, object.value.clone(), None)?;
    }
    TraverserValue::fetch_traverser_value_by(context, &object, &object.value, by)
}

/// Projects each object of `steps` through the `by()` ring (restarting at the first object).
/// Returns `false` when a `by()` yields no value, which drops the traverser.
pub(crate) fn apply_ring<'g, S: GraphStorage + 'g>(
    by: &[By<'g, S>],
    child_needs_path: bool,
    context: &mut TraverserContext<'_, S>,
    steps: &mut [TraverserPathStep<S>],
) -> TraverserResult<bool> {
    if by.is_empty() {
        return Ok(true);
    }
    for (position, step) in steps.iter_mut().enumerate() {
        let by = &by[position % by.len()];
        match project_object(by, child_needs_path, context, &step.value)? {
            Some(value) => step.value = value,
            None => return Ok(false),
        }
    }
    Ok(true)
}

/// The shared body of `simple_path()` and `cyclic_path()`: the `from()`/`to()` window, the `by()`
/// ring and the simplicity test.
pub(crate) struct SimplicityFilter<'g, S: GraphStorage + 'g> {
    by: Vec<By<'g, S>>,
    window: PathWindow,
    by_check: ByModulatorCheck,
    child_needs_path: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for SimplicityFilter<'g, S> {
    fn clone(&self) -> Self {
        Self {
            by: self.by.clone(),
            window: self.window.clone(),
            by_check: self.by_check.clone(),
            child_needs_path: self.child_needs_path,
        }
    }
}

impl<'g, S: GraphStorage + 'g> Default for SimplicityFilter<'g, S> {
    fn default() -> Self {
        Self {
            by: Vec::new(),
            window: PathWindow::default(),
            by_check: ByModulatorCheck::default(),
            child_needs_path: false,
        }
    }
}

impl<'g, S: GraphStorage + 'g> SimplicityFilter<'g, S> {
    /// Renders the `.from()`/`.to()`/`.by()` suffix of the step's `Debug` text.
    pub(crate) fn render_modulators(&self) -> String {
        let mut text = self.window.render();
        for by in &self.by {
            text.push_str(&crate::traversal::render_by(by));
        }
        text
    }

    pub(crate) fn push_by(&mut self, by: By<'g, S>) -> ByModulation {
        self.by_check.observe_ring(&by);
        self.by.push(by);
        ByModulation::Accepted
    }

    pub(crate) fn push_args(&mut self, args: Vec<String>, modulator: &str) -> ArgsModulation {
        self.window.push(args, modulator)
    }

    pub(crate) fn plan_check(&self, step: &str) -> TraverserResult<()> {
        self.by_check.check(step)?;
        self.window.check(step)
    }

    /// The `by(__...)` child traversals, in attachment order.
    pub(crate) fn traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.by.iter_mut().filter_map(By::traversal_mut).collect()
    }

    pub(crate) fn analyze(&mut self, analysis: &PathAnalysis) -> PathRequirement {
        let entry = By::analyze_all_paths(&mut self.by, analysis);
        self.child_needs_path = !entry.is_none();
        PathRequirement::None
    }

    /// Tests the simplicity of the (windowed, projected) path: `Some(simple)`, or `None` when a
    /// `by()` yields no value for one of the objects (the traverser is dropped).
    ///
    /// Without a modifier the arena is tested in place, allocation-free after warm-up; with one,
    /// the path is materialized once per traverser.
    pub(crate) fn is_simple(
        &self,
        step: &str,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<Option<bool>> {
        if self.by.is_empty() && !self.window.is_set() {
            return Ok(Some(context.paths.is_simple(
                &*context.storage,
                source.path,
                &mut context.scratch_path,
            )?));
        }
        self.plan_check(step)?;
        let mut path = context.paths.materialize(source.path);
        self.window.slice(&mut path.steps)?;
        if !apply_ring(&self.by, self.child_needs_path, context, &mut path.steps)? {
            return Ok(None);
        }
        let values: Vec<&TraverserValue<S>> = path.steps.iter().map(|step| &step.value).collect();
        Ok(Some(crate::value::path_arena::is_simple_values(
            &*context.storage,
            &values,
        )?))
    }
}
