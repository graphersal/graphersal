//! The `JsonPath` forms of `has(key, value)`, `has(key, P)`, `has(key)` and `hasNot(key)`.
//!
//! The builders in `step_has.rs`, `step_has_p.rs`, `step_has_property.rs` and `step_has_not.rs`
//! accept a [`PropKey`]; a plain name keeps its existing step, a path builds one of the steps
//! here. They keep the step names (`has`, `has_p`, `has_property`, `has_not`), so name-based
//! rules (the where-unnest rewrite, path analysis) treat both forms alike, but they never expose
//! `keys()`, `labels()` or `ids()`: a path key is not an index key and is never folded into a
//! source or barrier (Task 5 of the jpath plan; the optimizer rules match steps by what they
//! expose, see `traversal/optimizer`).
//!
//! A path that does not exist (missing key, out-of-range index, type mismatch on the way) is an
//! absent property: `has` filters out and `hasNot` keeps. Reads descend by reference
//! ([`peek_path`]).

use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::traversal::step::prop_path::peek_path;
use crate::value::JsonPath;
use crate::*;

#[derive(Clone)]
struct StepHasPath {
    path: JsonPath,
    value: ElementProperty,
}

impl fmt::Display for StepHasPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "has({}, {})",
            crate::value::literal::rhai_jpath(&self.path),
            crate::value::literal::rhai_literal(&self.value)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasPath {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        if let Some(crate::value::json_path::JsonPathSegment::Key(key)) =
            self.path.segments().first()
        {
            visit(Some(key), crate::graph::workload::AccessKind::FilterScan);
        }
    }
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        Ok(
            peek_path(context, &source.value, self.path.segments(), |leaf| {
                leaf.eq(&self.value)
            })?
            .unwrap_or(false),
        )
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[derive(Clone)]
struct StepHasPathP<B: Into<PValue> + Debug + Clone + Send + Sync> {
    path: JsonPath,
    predicate: P<B>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepHasPathP<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "has({}, {})",
            crate::value::literal::rhai_jpath(&self.path),
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepHasPathP<B>
{
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        // Only the value the path ends on is cloned, to hand the predicate an owned value.
        match peek_path(context, &source.value, self.path.segments(), |leaf| {
            leaf.to_logical()
        })? {
            Some(leaf) => self
                .predicate
                .test(context, &TraverserValue::Computed(leaf, None)),
            None => Ok(false),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// `has(jpath)` (`present == true`) and `hasNot(jpath)` (`present == false`).
#[derive(Clone)]
struct StepHasPathExists {
    path: JsonPath,
    present: bool,
}

impl fmt::Display for StepHasPathExists {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = if self.present { "has" } else { "has_not" };
        write!(
            f,
            "{name}({})",
            crate::value::literal::rhai_jpath(&self.path)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasPathExists {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        let found = peek_path(context, &source.value, self.path.segments(), |_| ())?.is_some();
        Ok(found == self.present)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// `has(path, value)`.
pub(crate) fn has_path_step<'g, S: GraphStorage + 'g>(
    path: JsonPath,
    value: ElementProperty,
) -> GraphStep<'g, S> {
    GraphStep::Filter("has", Box::new(StepHasPath { path, value }))
}

/// `has(path, P)`.
pub(crate) fn has_path_p_step<'g, S, B>(path: JsonPath, predicate: P<B>) -> GraphStep<'g, S>
where
    S: GraphStorage + 'g,
    B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
{
    GraphStep::Filter("has_p", Box::new(StepHasPathP { path, predicate }))
}

/// `has(path)`.
pub(crate) fn has_path_exists_step<'g, S: GraphStorage + 'g>(path: JsonPath) -> GraphStep<'g, S> {
    GraphStep::Filter(
        "has_property",
        Box::new(StepHasPathExists {
            path,
            present: true,
        }),
    )
}

/// `hasNot(path)`.
pub(crate) fn has_not_path_step<'g, S: GraphStorage + 'g>(path: JsonPath) -> GraphStep<'g, S> {
    GraphStep::Filter(
        "has_not",
        Box::new(StepHasPathExists {
            path,
            present: false,
        }),
    )
}
