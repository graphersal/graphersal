//! `repeat()` and its modulators `times()`, `until()` and `emit()` (TinkerPop
//! `RepeatStep`/`RepeatEndStep`).
//!
//! The whole frontier of a loop is processed together, which gives breadth-first order on the
//! eager engine. Depth-first order (`g.with("repeat.order", "dfs")`) walks an explicit stack, never
//! Rust recursion. Loop counters live in `TraverserContext::loop_stack`, one frame per active loop,
//! so traversers carry no loop state.

use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::traversal::bulk;
use crate::traversal::context::{LoopFrame, RepeatOrder};
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

/// A stop (`until`/`times`) or emit condition of a `repeat()`.
pub(crate) enum LoopCondition<'g, S: GraphStorage + 'g> {
    /// `emit()` without an argument: always true.
    Always,
    /// `times(n)`, which is `until(loops() >= n)`.
    Times(u32),
    /// True when the traversal yields at least one result for the traverser.
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for LoopCondition<'g, S> {
    fn clone(&self) -> Self {
        match self {
            LoopCondition::Always => LoopCondition::Always,
            LoopCondition::Times(n) => LoopCondition::Times(*n),
            LoopCondition::Traversal(t) => LoopCondition::Traversal(t.clone()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> LoopCondition<'g, S> {
    /// The child traversal of the condition, if it has one.
    fn traversal_mut(&mut self) -> Option<&mut AnonymousTraversal<'g, S>> {
        match self {
            LoopCondition::Traversal(t) => Some(t),
            _ => None,
        }
    }

    /// Evaluates the condition for `traverser`, whose loop counter is `loops`.
    fn holds(
        &self,
        context: &mut TraverserContext<'_, S>,
        traverser: &Traverser<S>,
        loops: u32,
    ) -> TraverserResult<bool> {
        match self {
            LoopCondition::Always => Ok(true),
            LoopCondition::Times(n) => Ok(loops >= *n),
            LoopCondition::Traversal(t) => t.exists(context, traverser),
        }
    }
}

/// Which modulator of a `repeat()` is being attached.
pub(crate) enum LoopModulator<'g, S: GraphStorage + 'g> {
    /// `until(traversal)` or `times(n)`.
    Until(LoopCondition<'g, S>),
    /// `emit()` or `emit(traversal)`.
    Emit(LoopCondition<'g, S>),
}

/// `repeat(body)` with its modulators.
pub(crate) struct StepRepeat<'g, S: GraphStorage + 'g> {
    name: Option<Arc<str>>,
    /// `None` for a placeholder created by a modulator written before `repeat()`.
    body: Option<AnonymousTraversal<'g, S>>,
    until: Option<LoopCondition<'g, S>>,
    emit: Option<LoopCondition<'g, S>>,
    /// `until`/`times` was written before `repeat()` (while-do).
    until_first: bool,
    /// `emit` was written before `repeat()`.
    emit_first: bool,
    /// `emit` was attached before `until`/`times` (only meaningful when both are set): the
    /// attachment order that numbers their child traversals (`GraphStep::child_traversals_mut`).
    emit_attached_first: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepRepeat<'g, S> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            body: self.body.clone(),
            until: self.until.clone(),
            emit: self.emit.clone(),
            until_first: self.until_first,
            emit_first: self.emit_first,
            emit_attached_first: self.emit_attached_first,
        }
    }
}

/// Renders a child traversal the way it is written: `__.out().has(...)`.
fn render_traversal<'g, S: GraphStorage + 'g>(traversal: &AnonymousTraversal<'g, S>) -> String {
    traversal.to_string()
}

fn render_until<'g, S: GraphStorage + 'g>(condition: &LoopCondition<'g, S>) -> String {
    match condition {
        LoopCondition::Times(n) => format!("times({n})"),
        LoopCondition::Always => "until()".to_string(),
        LoopCondition::Traversal(t) => format!("until({})", render_traversal(t)),
    }
}

fn render_emit<'g, S: GraphStorage + 'g>(condition: &LoopCondition<'g, S>) -> String {
    match condition {
        LoopCondition::Traversal(t) => format!("emit({})", render_traversal(t)),
        _ => "emit()".to_string(),
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepRepeat<'g, S> {
    /// The step as written: prefix modulators, `repeat(...)`, postfix modulators.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if self.emit_first
            && let Some(emit) = &self.emit
        {
            parts.push(render_emit(emit));
        }
        if self.until_first
            && let Some(until) = &self.until
        {
            parts.push(render_until(until));
        }
        if let Some(body) = &self.body {
            let body = render_traversal(body);
            parts.push(match &self.name {
                Some(name) => format!(
                    "repeat({}, {body})",
                    crate::value::literal::rhai_string(name)
                ),
                None => format!("repeat({body})"),
            });
        }
        if !self.until_first
            && let Some(until) = &self.until
        {
            parts.push(render_until(until));
        }
        if !self.emit_first
            && let Some(emit) = &self.emit
        {
            parts.push(render_emit(emit));
        }
        write!(f, "{}", parts.join("."))
    }
}

/// Loop statistics of one `repeat()` execution, for `.profile()`.
#[derive(Default)]
struct LoopStats {
    /// Body executions.
    total_loops: usize,
    /// Highest iteration count reached.
    max_depth: usize,
    /// Sum of the loop counters of the output traversers, weighted by their bulk (so the average
    /// over the total bulk equals the average of an unmerged run).
    counter_sum: u128,
}

impl<'g, S: GraphStorage + 'g> StepRepeat<'g, S> {
    fn new(name: Option<Arc<str>>, body: Option<AnonymousTraversal<'g, S>>) -> Self {
        Self {
            name,
            body,
            until: None,
            emit: None,
            until_first: false,
            emit_first: false,
            emit_attached_first: false,
        }
    }

    /// Sets a modulator. On the placeholder (no body yet) it is a prefix condition, on a
    /// `repeat()` with a body a postfix one. Setting a condition again replaces it.
    fn modulate(&mut self, modulator: LoopModulator<'g, S>) {
        let prefix = self.body.is_none();
        match modulator {
            LoopModulator::Until(condition) => {
                self.until = Some(condition);
                self.until_first = prefix;
                // The latest attachment of each modulator counts.
                self.emit_attached_first = self.emit.is_some();
            }
            LoopModulator::Emit(condition) => {
                self.emit = Some(condition);
                self.emit_first = prefix;
                self.emit_attached_first = self.until.is_none();
            }
        }
    }

    fn limit_exceeded(
        &self,
        context: &TraverserContext<'_, S>,
        body: &AnonymousTraversal<'g, S>,
    ) -> BoxedTraverserError {
        Box::new(TraverserError::RepeatLimitExceeded {
            max_loops: context.options.repeat_max_loops,
            loop_name: self.name.as_deref().map(str::to_string),
            body: render_traversal(body),
        })
    }

    /// Applies the prefix checks to `traverser` at loop counter `k`. Returns the traverser when
    /// it continues into the body.
    fn prefix(
        &self,
        context: &mut TraverserContext<'_, S>,
        traverser: Traverser<S>,
        k: u32,
        out: &mut Vec<Traverser<S>>,
        stats: &mut LoopStats,
    ) -> TraverserResult<Option<Traverser<S>>> {
        if self.until_first
            && let Some(until) = &self.until
            && until.holds(context, &traverser, k)?
        {
            stats.counter_sum += u128::from(k) * u128::from(traverser.bulk);
            out.push(traverser);
            return Ok(None);
        }
        if self.emit_first
            && let Some(emit) = &self.emit
            && emit.holds(context, &traverser, k)?
        {
            stats.counter_sum += u128::from(k) * u128::from(traverser.bulk);
            out.push(traverser.clone());
        }
        Ok(Some(traverser))
    }

    /// Applies the postfix checks to a body output at loop counter `k` (one more than the
    /// iteration that produced it). Returns the traverser when it goes round again.
    fn postfix(
        &self,
        context: &mut TraverserContext<'_, S>,
        traverser: Traverser<S>,
        k: u32,
        out: &mut Vec<Traverser<S>>,
        stats: &mut LoopStats,
    ) -> TraverserResult<Option<Traverser<S>>> {
        if !self.until_first
            && let Some(until) = &self.until
            && until.holds(context, &traverser, k)?
        {
            stats.counter_sum += u128::from(k) * u128::from(traverser.bulk);
            out.push(traverser);
            return Ok(None);
        }
        if !self.emit_first
            && let Some(emit) = &self.emit
            && emit.holds(context, &traverser, k)?
        {
            stats.counter_sum += u128::from(k) * u128::from(traverser.bulk);
            out.push(traverser.clone());
        }
        Ok(Some(traverser))
    }

    /// The iteration limit, or `None` when `times(n)` already bounds the loop.
    fn max_loops(&self, context: &TraverserContext<'_, S>) -> Option<u32> {
        match self.until {
            Some(LoopCondition::Times(_)) => None,
            _ => Some(context.options.repeat_max_loops),
        }
    }

    /// Breadth first: every traverser of iteration `k` before any of iteration `k + 1`.
    fn run_bfs(
        &self,
        context: &mut TraverserContext<'_, S>,
        body: &AnonymousTraversal<'g, S>,
        source: Vec<Traverser<S>>,
        frame: usize,
        stats: &mut LoopStats,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let max_loops = self.max_loops(context);
        let mut out = Vec::new();
        // Merge points of the BFS frontier: the input and every iteration's body output. All
        // traversers of one frontier share the loop counters (`loop_stack`), so merging equal
        // traversers within one frontier is exact. The merge runs with the path mode that the
        // previous step recorded (`context.path_mode`) and only joins identical `PathRef`s.
        // The input frontier (often every vertex, all distinct) probes too, but its verdict does
        // not count: only a body output tells whether the loop's expansions converge.
        // A sack merge operator or `bulk.one` switches the automatic merge points off.
        let mut merging = context.auto_merge;
        let mut frontier = if merging {
            bulk::merge_traversers_adaptive(context, source, "repeat")?.0
        } else {
            source
        };
        // Adaptive skip: a body output big enough to probe, in which almost nothing merged,
        // switches merging off for the remaining iterations of this loop.
        let mut k: u32 = 0;
        while !frontier.is_empty() {
            context.check_deadline()?;
            if max_loops.is_some_and(|max| k >= max) {
                return Err(self.limit_exceeded(context, body));
            }
            set_loops(context, frame, k);
            let next = if self.until_first || self.emit_first {
                let mut next = Vec::with_capacity(frontier.len());
                for traverser in frontier {
                    context.tick()?;
                    if let Some(t) = self.prefix(context, traverser, k, &mut out, stats)? {
                        next.push(t);
                    }
                }
                next
            } else {
                frontier
            };
            if next.is_empty() {
                break;
            }
            // batch: bulk preserved
            let produced = body.to_list(context, next)?;
            // Before the postfix checks: fewer condition evaluations, and the explosion of
            // equal traversers never builds up.
            let produced = if merging {
                let (produced, worthwhile) =
                    bulk::merge_traversers_adaptive(context, produced, "repeat")?;
                merging = worthwhile;
                produced
            } else {
                produced
            };
            let k_next = k.saturating_add(1);
            stats.total_loops += 1;
            stats.max_depth = stats.max_depth.max(k_next as usize);
            set_loops(context, frame, k_next);
            frontier = Vec::with_capacity(produced.len());
            for traverser in produced {
                context.tick()?;
                // The body may wrap the value again on every iteration (`repeat(__.fold())`).
                context.check_loop_traverser(&traverser)?;
                if let Some(t) = self.postfix(context, traverser, k_next, &mut out, stats)? {
                    frontier.push(t);
                }
            }
            k = k_next;
        }
        Ok(out)
    }

    /// Depth first, pre-order, with an explicit stack: a traverser's children are visited, in
    /// body order, before its siblings.
    fn run_dfs(
        &self,
        context: &mut TraverserContext<'_, S>,
        body: &AnonymousTraversal<'g, S>,
        source: Vec<Traverser<S>>,
        frame: usize,
        stats: &mut LoopStats,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let max_loops = self.max_loops(context);
        let mut out = Vec::new();
        let mut stack: Vec<(Traverser<S>, u32)> =
            source.into_iter().rev().map(|t| (t, 0)).collect();
        while let Some((traverser, k)) = stack.pop() {
            context.check_deadline()?;
            if max_loops.is_some_and(|max| k >= max) {
                return Err(self.limit_exceeded(context, body));
            }
            set_loops(context, frame, k);
            let Some(traverser) = self.prefix(context, traverser, k, &mut out, stats)? else {
                continue;
            };
            // batch: bulk preserved. No merging in DFS: it visits one traverser at a time, so
            // there is no frontier of equal traversers to join.
            let produced = body.to_list(context, vec![traverser])?;
            let k_next = k.saturating_add(1);
            stats.total_loops += 1;
            stats.max_depth = stats.max_depth.max(k_next as usize);
            set_loops(context, frame, k_next);
            let mark = stack.len();
            for child in produced {
                context.tick()?;
                context.check_loop_traverser(&child)?;
                if let Some(c) = self.postfix(context, child, k_next, &mut out, stats)? {
                    stack.push((c, k_next));
                }
            }
            // Pushed in body order; reversed so the first child is visited first.
            if let Some(children) = stack.get_mut(mark..) {
                children.reverse();
            }
        }
        Ok(out)
    }
}

/// Sets the counter of the loop frame at `frame`.
#[inline]
fn set_loops<S: GraphStorage>(context: &mut TraverserContext<'_, S>, frame: usize, loops: u32) {
    if let Some(frame) = context.loop_stack.get_mut(frame) {
        frame.loops = loops;
    }
}

/// Attaches `times()`/`until()`/`emit()` to the `repeat()` that is the last step of `steps`, or
/// starts a placeholder `repeat()` that a following `repeat(body)` fills in (TinkerPop
/// `GraphTraversal.emit/until/times`).
pub(crate) fn attach_loop_modulator<'g, S: GraphStorage + 'g>(
    steps: &mut Vec<GraphStep<'g, S>>,
    modulator: LoopModulator<'g, S>,
) {
    if let Some(repeat) = last_repeat(steps) {
        repeat.modulate(modulator);
        return;
    }
    let mut placeholder = StepRepeat::new(None, None);
    placeholder.modulate(modulator);
    steps.push(GraphStep::FlatBarrier("repeat", Box::new(placeholder)));
}

/// Appends `repeat(body)`, filling in the placeholder a prefix modulator left when there is one.
pub(crate) fn attach_repeat<'g, S: GraphStorage + 'g>(
    steps: &mut Vec<GraphStep<'g, S>>,
    name: Option<Arc<str>>,
    body: AnonymousTraversal<'g, S>,
) {
    if let Some(repeat) = last_repeat(steps)
        && repeat.body.is_none()
    {
        repeat.body = Some(body);
        repeat.name = name;
        return;
    }
    steps.push(GraphStep::FlatBarrier(
        "repeat",
        Box::new(StepRepeat::new(name, Some(body))),
    ));
}

/// The last non-modulator step of `steps`, when it is a `repeat()`.
fn last_repeat<'a, 'g, S: GraphStorage + 'g>(
    steps: &'a mut [GraphStep<'g, S>],
) -> Option<&'a mut StepRepeat<'g, S>> {
    match steps
        .iter_mut()
        .rev()
        .find(|s| !matches!(s, GraphStep::Modulator(_, _)))
    {
        Some(GraphStep::FlatBarrier(_, step)) => step.as_repeat_mut(),
        _ => None,
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Loops `body` over the traversers: each iteration feeds the output of the body back into
    /// it, until a `times()`/`until()` condition stops a traverser or the body yields nothing.
    ///
    /// - `times(n)`/`until(t)` written **after** `repeat()` is checked after each iteration
    ///   (do-while); written **before** it, before each iteration (while-do).
    /// - `emit()`/`emit(t)` also outputs traversers that go round again: after each iteration
    ///   when written after `repeat()`, before each iteration when written before it.
    /// - `loops()` reads the iteration count of the enclosing loop, `loops_named(name)` that of
    ///   the loop started with [`repeat_named`](Self::repeat_named).
    ///
    /// The loop runs breadth first: every traverser of iteration `k` comes out before any of
    /// iteration `k + 1`. `g.with("repeat.order", "dfs")` walks it depth first (pre-order)
    /// instead; both produce the same results for a body without barrier steps, in a different
    /// order. A loop without `times()` fails with
    /// [`TraverserError::RepeatLimitExceeded`] after `repeat.max_loops` iterations (10 000 by
    /// default, `g.with("repeat.max_loops", n)` to change it) instead of running forever on a
    /// cycle; `simple_path()` in the body stops cycles.
    ///
    /// A `dedup()` in the body keeps one seen-set for the whole loop, like TinkerPop: an element
    /// met again in a later iteration (or by another traverser) is dropped, in both orders. A
    /// `dedup()` in a per-traverser child of the body (`where`, `local`, `by(traversal)`, loop
    /// conditions) starts fresh for every traverser. Other barriers (`limit()`, `range()`,
    /// `tail()`, `count()`) run once per iteration (breadth first), which is what TinkerPop 3.8.2
    /// does too (its feature files expect `repeat(both().limit(1)).times(2)` to count 1). In
    /// depth-first order, which TinkerPop lacks, they see one traverser at a time: a deliberate
    /// deviation, documented under "Recursive Traversals" in the user's guide.
    /// `simple_path()` still stops cycles.
    ///
    /// In breadth-first order the frontier is merged after every iteration: equal traversers (the
    /// same vertex reached by different routes) become one traverser with a bulk, so a dense
    /// graph does not explode the frontier. This is invisible in the results (same multiset;
    /// only the order of duplicates can change) and is not a visited set: every route still
    /// counts. It is skipped for depth-first order, full paths and mutating plans, and can be
    /// turned off with `g.with("bulk.merge", false)`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .repeat(__::out(None))
    ///     .times(2)
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2); // ripple and lop, two hops away from marko
    /// ```
    pub fn repeat(mut self, body: AnonymousTraversal<'g, S>) -> Self {
        attach_repeat(&mut self.steps, None, body);
        self
    }

    /// [`repeat`](Self::repeat) with a loop name that `loops_named(name)` reads, also from a
    /// nested loop.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .repeat_named("a", __::out(None))
    ///     .until(__::loops_named("a").is(2))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// ```
    pub fn repeat_named(mut self, name: &str, body: AnonymousTraversal<'g, S>) -> Self {
        attach_repeat(&mut self.steps, Some(Arc::from(name)), body);
        self
    }

    /// Stops a [`repeat`](Self::repeat) loop after `n` iterations; `until(loops() >= n)`.
    ///
    /// After `repeat()` the body runs at least once, so `repeat(..).times(0)` runs it once, as
    /// in TinkerPop; before it, `times(0).repeat(..)` passes its input through unchanged.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let count = lock.traversal().v("1").repeat(__::out(None)).times(3).count().next().unwrap();
    /// assert_eq!(count.and_then(|c| c.as_i64()), Some(0)); // nothing is three hops away
    /// ```
    pub fn times(mut self, n: u32) -> Self {
        attach_loop_modulator(
            &mut self.steps,
            LoopModulator::Until(LoopCondition::Times(n)),
        );
        self
    }

    /// Stops a [`repeat`](Self::repeat) loop for a traverser once `condition` yields a result
    /// for it, and outputs that traverser.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let path = lock
    ///     .traversal()
    ///     .v("1")
    ///     .repeat(__::out(None))
    ///     .until(__::has("name", "ripple"))
    ///     .path()
    ///     .by("name")
    ///     .next()
    ///     .unwrap();
    /// assert!(path.is_some()); // [marko, josh, ripple]
    /// ```
    pub fn until(mut self, condition: AnonymousTraversal<'g, S>) -> Self {
        attach_loop_modulator(
            &mut self.steps,
            LoopModulator::Until(LoopCondition::Traversal(condition)),
        );
        self
    }

    /// Also outputs every traverser a [`repeat`](Self::repeat) loop visits, not only those it
    /// ends with: after each iteration when written after `repeat()`, before each iteration
    /// (the input included) when written before it.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .emit()
    ///     .repeat(__::out("knows"))
    ///     .times(2)
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 3); // marko, josh, vadas
    /// ```
    pub fn emit(mut self) -> Self {
        attach_loop_modulator(&mut self.steps, LoopModulator::Emit(LoopCondition::Always));
        self
    }

    /// Like [`emit`](Self::emit), for the traversers for which `condition` yields a result.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .repeat(__::out(None))
    ///     .times(2)
    ///     .emit_with(__::has_label("person"))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 4); // vadas and josh emitted, then ripple and lop at the end
    /// ```
    pub fn emit_with(mut self, condition: AnonymousTraversal<'g, S>) -> Self {
        attach_loop_modulator(
            &mut self.steps,
            LoopModulator::Emit(LoopCondition::Traversal(condition)),
        );
        self
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `repeat()`; see [`GraphTraversalSource::repeat`].
    pub fn repeat(mut self, body: AnonymousTraversal<'g, S>) -> Self {
        attach_repeat(&mut self.steps, None, body);
        self
    }

    /// Anonymous named `repeat()`; see [`GraphTraversalSource::repeat_named`].
    pub fn repeat_named(mut self, name: &str, body: AnonymousTraversal<'g, S>) -> Self {
        attach_repeat(&mut self.steps, Some(Arc::from(name)), body);
        self
    }

    /// Anonymous `times()`; see [`GraphTraversalSource::times`].
    pub fn times(mut self, n: u32) -> Self {
        attach_loop_modulator(
            &mut self.steps,
            LoopModulator::Until(LoopCondition::Times(n)),
        );
        self
    }

    /// Anonymous `until()`; see [`GraphTraversalSource::until`].
    pub fn until(mut self, condition: AnonymousTraversal<'g, S>) -> Self {
        attach_loop_modulator(
            &mut self.steps,
            LoopModulator::Until(LoopCondition::Traversal(condition)),
        );
        self
    }

    /// Anonymous `emit()`; see [`GraphTraversalSource::emit`].
    pub fn emit(mut self) -> Self {
        attach_loop_modulator(&mut self.steps, LoopModulator::Emit(LoopCondition::Always));
        self
    }

    /// Anonymous `emit(traversal)`; see [`GraphTraversalSource::emit_with`].
    pub fn emit_with(mut self, condition: AnonymousTraversal<'g, S>) -> Self {
        attach_loop_modulator(
            &mut self.steps,
            LoopModulator::Emit(LoopCondition::Traversal(condition)),
        );
        self
    }
}

impl __ {
    /// Starts an anonymous traversal with `repeat()`; see [`GraphTraversalSource::repeat`].
    pub fn repeat<'g, S: GraphStorage + 'g>(
        body: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().repeat(body)
    }

    /// Starts an anonymous traversal with a named `repeat()`; see
    /// [`GraphTraversalSource::repeat_named`].
    pub fn repeat_named<'g, S: GraphStorage + 'g>(
        name: &str,
        body: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().repeat_named(name, body)
    }

    /// Starts an anonymous traversal with `times()`, a prefix loop bound for a following
    /// `repeat()`; see [`GraphTraversalSource::times`].
    pub fn times<'g, S: GraphStorage + 'g>(n: u32) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().times(n)
    }

    /// Starts an anonymous traversal with `until()`, a prefix stop condition for a following
    /// `repeat()`; see [`GraphTraversalSource::until`].
    pub fn until<'g, S: GraphStorage + 'g>(
        condition: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().until(condition)
    }

    /// Starts an anonymous traversal with `emit()`; see [`GraphTraversalSource::emit`].
    pub fn emit<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().emit()
    }

    /// Starts an anonymous traversal with `emit(traversal)`; see
    /// [`GraphTraversalSource::emit_with`].
    pub fn emit_with<'g, S: GraphStorage + 'g>(
        condition: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().emit_with(condition)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepRepeat<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        let Some(body) = &self.body else {
            return Err(Box::new(TraverserError::RepeatWithoutBody {
                modulator: self.to_string(),
            }));
        };
        // TinkerPop: `inject()` cannot be a direct step of a repeat body (a nested one, such as in
        // `union(__.inject(..))`, is fine). Graphersal raises `TraverserError::RepeatInject` when the
        // traversal executes (documented in the user's guide page "Recursive Traversals").
        if body
            .steps
            .iter()
            .any(|step| matches!(step, GraphStep::Append("inject", _)))
        {
            return Err(Box::new(TraverserError::RepeatInject {
                loop_name: self.name.as_ref().map(|name| name.to_string()),
            }));
        }
        let frame = context.loop_stack.len();
        context.loop_stack.push(LoopFrame::new(self.name.clone()));
        // The body runs in the loop's persistent scope: its `dedup()` steps keep one seen-set
        // for the whole loop (TinkerPop). Restored on every exit path.
        let outer_persist = std::mem::replace(&mut context.loop_persist, true);
        let mut stats = LoopStats::default();
        let result = match context.options.repeat_order {
            RepeatOrder::Bfs => self.run_bfs(context, body, source, frame, &mut stats),
            RepeatOrder::Dfs => self.run_dfs(context, body, source, frame, &mut stats),
        };
        // Popped on every exit path, errors included.
        context.loop_stack.truncate(frame);
        context.loop_persist = outer_persist;
        let out = result?;
        if context.strategy.contains(TraversalStrategy::ProfileEnabled) {
            let total = bulk::total_bulk(context, &out, "repeat")?;
            let avg = if total == 0 {
                0.0
            } else {
                stats.counter_sum as f64 / total as f64
            };
            context.profile.merge_loop_summary(
                LoopMetrics {
                    total_loops: stats.total_loops as u64,
                    max_depth: stats.max_depth as u64,
                    avg_loops_per_traverser: avg,
                },
                out.len(),
            );
        }
        Ok(Some(out))
    }

    /// The body first (the argument of `repeat()`), then the `until()` and `emit()` conditions
    /// in the order they were attached; `times(n)` and a bare `emit()` hold no traversal.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        let mut children: Vec<&mut AnonymousTraversal<'g, S>> = Vec::new();
        if let Some(body) = &mut self.body {
            children.push(body);
        }
        let until = self.until.as_mut().and_then(LoopCondition::traversal_mut);
        let emit = self.emit.as_mut().and_then(LoopCondition::traversal_mut);
        if self.emit_attached_first {
            children.extend(emit);
            children.extend(until);
        } else {
            children.extend(until);
            children.extend(emit);
        }
        children
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        downstream: &PathRequirement,
    ) -> PathRequirement {
        // The conditions test a traverser without continuing its path.
        let mut conditions = PathRequirement::None;
        if let Some(t) = self.until.as_mut().and_then(LoopCondition::traversal_mut) {
            conditions = conditions.union(t.analyze_paths(analysis, PathRequirement::None));
        }
        if let Some(t) = self.emit.as_mut().and_then(LoopCondition::traversal_mut) {
            conditions = conditions.union(t.analyze_paths(analysis, PathRequirement::None));
        }
        let Some(body) = &mut self.body else {
            return conditions;
        };
        // The body is loop-carried: its output feeds the steps after the loop, the conditions
        // and its own input. Iterate until the need stops growing; requirements only grow over
        // a finite label set and `Full` absorbs, so this settles within a few passes.
        let base = downstream.clone().union(conditions.clone());
        let mut need = base.clone();
        let mut entry = body.analyze_paths(analysis, need.clone());
        for _ in 0..8 {
            let next = base.clone().union(entry.clone());
            if next == need {
                break;
            }
            need = next;
            entry = body.analyze_paths(analysis, need.clone());
        }
        entry.union(conditions)
    }

    fn holds_loop_state(&self) -> bool {
        self.body.as_ref().is_some_and(|body| {
            body.steps.iter().any(|step| match step {
                GraphStep::FlatBarrier(name, inner) => *name == "dedup" || inner.holds_loop_state(),
                _ => false,
            })
        })
    }

    fn as_repeat_mut(&mut self) -> Option<&mut StepRepeat<'g, S>> {
        Some(self)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the steps of a traversal as their names, for the attachment shapes of D2.
    fn shape(traversal: AnonymousTraversal<'static, crate::graph::TraversalGraph>) -> Vec<String> {
        traversal.steps.iter().map(|s| s.get_name()).collect()
    }

    #[test]
    fn modulators_attach_as_in_tinkerpop() {
        assert_eq!(
            shape(__::out(None).repeat(__::out(None)).times(2)),
            ["out()", "repeat(__.out()).times(2)"]
        );
        assert_eq!(
            shape(__::times(2).repeat(__::out(None))),
            ["times(2).repeat(__.out())"]
        );
        assert_eq!(
            shape(__::emit().repeat(__::out(None)).times(2)),
            ["emit().repeat(__.out()).times(2)"]
        );
        assert_eq!(
            shape(__::repeat(__::out(None)).emit().until(__::has_label("x"))),
            [r#"repeat(__.out()).until(__.has_label("x")).emit()"#]
        );
        assert_eq!(
            shape(
                __::repeat(__::out(None))
                    .times(2)
                    .emit()
                    .repeat(__::in_(None))
            ),
            ["repeat(__.out()).times(2).emit()", "repeat(__.in())"]
        );
        assert_eq!(shape(__::out(None).times(2)), ["out()", "times(2)"]);
        assert_eq!(
            shape(__::repeat_named("a", __::out(None)).times(1)),
            [r#"repeat("a", __.out()).times(1)"#]
        );
    }

    #[test]
    fn placement_flags() {
        let mut steps = __::emit::<crate::graph::TraversalGraph>()
            .repeat(__::out(None))
            .times(2)
            .steps;
        let GraphStep::FlatBarrier(_, step) = &mut steps[0] else {
            panic!("repeat() is a flat barrier");
        };
        let repeat = step.as_repeat_mut().expect("a repeat step");
        assert!(repeat.emit_first);
        assert!(!repeat.until_first);
        assert!(matches!(repeat.until, Some(LoopCondition::Times(2))));
    }

    #[test]
    fn profile_shows_the_loop() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let result = g_lock
            .traversal()
            .v("1")
            .repeat(__::out(None))
            .times(2)
            .profile()
            .unwrap();
        let rendered = result.to_string();
        assert!(rendered.contains("repeat(__.out()).times(2)"), "{rendered}");
        assert!(
            rendered.contains("[loops: 2, max_depth: 2, avg_loops: 2.0]"),
            "{rendered}"
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn repeat_times_dual() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").repeat(__::out(None)).times(2).count().to_list(),
            rhai: [
                r#"g.V("1").repeat(__.out()).times(2).count()"#,
                r#"g.v("1").times(2).repeat(__.out()).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }
}
