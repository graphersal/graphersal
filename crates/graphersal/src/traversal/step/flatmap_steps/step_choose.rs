//! `choose(...)` and its chained modulator `.option(...)` (TinkerPop `ChooseStep`).
//!
//! The rules below follow the vendored TinkerPop 3.8.2 feature files (`Choose.feature`,
//! `Branch.feature`), which win over any prose reference:
//!
//! - `choose()` and `branch()` (`step_branch.rs`) share one selection mechanism (`StepBranch`
//!   below): a selector, evaluated per traverser, whose first result is tested against a list of
//!   `(option_key, branch_traversal)` pairs (`.option(key, traversal)`), kept in declaration order.
//! - **Option keys.** A literal (`option("person", ..)`, `option(2, ..)`), a predicate
//!   (`option(P.between(26, 30), ..)`), `Pick.none`, `Pick.unproductive`, `Pick.any` (`branch()`
//!   only), or, for `branch()` only, a traversal key (`option(__.hasLabel("software"), ..)`).
//!   A literal is compared with the same comparison core as `P.eq` (`value/predicate.rs`), so
//!   numbers match by value (`option(2L, ..)` matches the int `2` and the float `2.0`). A literal
//!   or predicate operand is always data: a bare string never names a step label or a vertex id.
//!   A traversal key matches when it produces a result with the selector's value as its start.
//! - **`choose()` picks at most one branch**: the first matching option in declaration order.
//!   It rejects `Pick.any` and a traversal key with a [`TraverserError::InvalidOption`] when the
//!   traversal executes ("at most one" and "always additionally" contradict each other; use a
//!   predicate, a literal or `branch()`).
//! - **`branch()` runs every matching option** (a traverser can flow into several branches),
//!   plus every `Pick.any` option, which is additive and runs whatever the selector produced.
//! - **Nothing matches** (the selector produced a value, no option key accepts it): the
//!   `Pick.none` option runs (`choose()`: the first one, `branch()`: all of them). Without one,
//!   `choose()` passes the traverser through *unchanged* and `branch()` drops it.
//! - **The selector produces nothing**: the `Pick.unproductive` option runs (same multiplicity
//!   rule). Without one, `choose()` passes the traverser through unchanged (even when a
//!   `Pick.none` option exists, which is only for a produced value that matched nothing) and
//!   `branch()` drops it.
//! - The 3-argument `choose(predicate, onTrue, onFalse)` if/then/else form is sugar over the same
//!   mechanism: the selector is evaluated via `exists()` and the two options are the literals
//!   `true` and `false`. Both are always registered, so this form never drops a traverser, and
//!   it rejects further `.option()` calls (`locked: true`).
//! - **Batch rule.** The selector runs per traverser, but the traversers routed to an option are
//!   fed to that option's child as one stream when the child holds global state (`limit`,
//!   `count`, `order`, `dedup`, `fold`, ...), exactly as `BranchStep.addStart` feeds every
//!   traverser into the same child instance. The options then run in declaration order
//!   (option-major output); traversers that `choose()` passes through unchanged follow the
//!   buckets. A child without global state runs per traverser, with traverser-major output.
//!   `local(...)` is the per-traverser form of a stateful child. See
//!   `book/src/users_guide/branch_batches.md`.
//! - `choose()`/`branch()` accept a nested [`AnonymousTraversal`] selector only (no raw
//!   `Function`).
//!
//! # Deviations from TinkerPop
//!
//! The consolidated list is in `book/src/users_guide/option_semantics.md`.
//!
//! - `Pick.any` runs for a traverser whose selector produced nothing, too (its documented
//!   meaning is "always"); the feature files do not cover this case.
//! - The profile attributes the steps of a traversal key to the nested steps of the option
//!   (the key's steps are numbered before its branch's steps).
//! - A traversal key is evaluated with the selector's value as start but the traverser's own
//!   path, so a key that reads `path()` sees the path of the choosing traverser.

use crate::graph::GraphStorage;
use crate::*;

/// A non-traversal `.option(key, traversal)` key (TinkerPop `Pick`, a raw object or a predicate).
///
/// Literals and predicates are tested against the selector's first result with the comparison
/// core of [`P`] (numbers by value, no step-label or vertex-id resolution of a bare string).
#[derive(Clone, Debug)]
pub(crate) enum OptionToken {
    /// A literal, matched like `P.eq(literal)` against the selector's result.
    Value(ElementProperty),
    /// A predicate (`option(P.between(26, 30), ..)`), tested against the selector's result.
    Predicate(P<PValue>),
    // Only the Rhai DSL (`Pick.any`/`Pick.none`/`Pick.unproductive`) constructs the `Pick`
    // sentinels; the Rust API has no builder for them yet, so they are unconstructed without `script`.
    /// `Pick.any`: `branch()` only. Additive: this option's branch runs for every traverser, on
    /// top of whichever option(s) also matched.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    Any,
    /// `Pick.none`: runs when the selector produced a value that no option key accepted.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    None,
    /// `Pick.unproductive`: runs when the selector produced nothing.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    Unproductive,
}

/// The key as the DSL writes it: a literal, `P.gt(1)`, `Pick.any`.
impl fmt::Display for OptionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptionToken::Value(value) => f.write_str(&crate::value::literal::rhai_literal(value)),
            OptionToken::Predicate(predicate) => {
                f.write_str(&crate::value::predicate::render_predicate(predicate))
            }
            OptionToken::Any => f.write_str("Pick.any"),
            OptionToken::None => f.write_str("Pick.none"),
            OptionToken::Unproductive => f.write_str("Pick.unproductive"),
        }
    }
}

/// The key of one option: a [`OptionToken`], or a traversal (`branch()` only; `choose()` rejects
/// it when the traversal executes).
pub(crate) enum OptionKey<'g, S: GraphStorage + 'g> {
    /// A literal, a predicate or a `Pick` sentinel.
    Token(OptionToken),
    /// Matches when the traversal produces a result with the selector's value as its start.
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for OptionKey<'g, S> {
    fn clone(&self) -> Self {
        match self {
            OptionKey::Token(token) => OptionKey::Token(token.clone()),
            OptionKey::Traversal(traversal) => OptionKey::Traversal(traversal.clone()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for OptionKey<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptionKey::Token(token) => write!(f, "{token}"),
            OptionKey::Traversal(traversal) => write!(f, "{traversal}"),
        }
    }
}

/// What the selector produced for one traverser.
enum Selection<S: GraphStorage> {
    /// The selector produced nothing.
    Unproductive,
    /// The selector's first result.
    Value(Traverser<S>),
}

/// The options that fire for one traverser.
struct Picked {
    /// Option indices in firing order.
    indices: Vec<usize>,
    /// `choose()` found neither a matching option nor a fallback: the traverser continues
    /// unchanged.
    pass_through: bool,
}

/// Which of `choose()`/`branch()` a [`StepBranch`] is, controlling multiplicity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BranchKind {
    /// At most one option fires per traverser.
    Choose,
    /// Every matching option fires per traverser (fan-out).
    Branch,
}

/// Shared representation of `choose(...)` and `branch(traversal)`: a selector plus a list of
/// `(option_key, branch_traversal)` options, differing only in [`BranchKind`]'s "first match" vs.
/// "every match" multiplicity and in the unmatched rule at execution time. See the module doc
/// comment for the semantics this implements.
pub(crate) struct StepBranch<'g, S: GraphStorage + 'g> {
    kind: BranchKind,
    /// For the 3-argument `choose(predicate, onTrue, onFalse)` sugar form: evaluated via
    /// `exists()` (a boolean), not by materializing a value. `false` for the
    /// `choose(traversal).option(...)`/`branch(traversal).option(...)` chained forms.
    predicate_mode: bool,
    selector: AnonymousTraversal<'g, S>,
    options: Vec<(OptionKey<'g, S>, AnonymousTraversal<'g, S>)>,
    /// `true` for the 3-argument sugar form, which is already fully configured and rejects
    /// further `.option()` calls (`ChooseStep`'s IF_THEN-mode `addChildOption` override).
    locked: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepBranch<'g, S> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            predicate_mode: self.predicate_mode,
            selector: self.selector.clone(),
            options: self.options.clone(),
            locked: self.locked,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepBranch<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.kind {
            BranchKind::Choose => "choose",
            BranchKind::Branch => "branch",
        };
        if self.predicate_mode {
            let on_true = self.options.first().map(|(_, t)| t);
            let on_false = self.options.get(1).map(|(_, t)| t);
            write!(f, "{name}({}", self.selector)?;
            for branch in [on_true, on_false].into_iter().flatten() {
                write!(f, ", {branch}")?;
            }
            return f.write_str(")");
        }
        write!(f, "{name}({})", self.selector)?;
        for (key, branch) in &self.options {
            write!(f, ".option({key}, {branch})")?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> StepBranch<'g, S> {
    pub(crate) fn new_switch(kind: BranchKind, selector: AnonymousTraversal<'g, S>) -> Self {
        Self {
            kind,
            predicate_mode: false,
            selector,
            options: Vec::new(),
            locked: false,
        }
    }

    pub(crate) fn new_if_then_else(
        predicate: AnonymousTraversal<'g, S>,
        on_true: AnonymousTraversal<'g, S>,
        on_false: AnonymousTraversal<'g, S>,
    ) -> Self {
        Self {
            kind: BranchKind::Choose,
            predicate_mode: true,
            selector: predicate,
            options: vec![
                (
                    OptionKey::Token(OptionToken::Value(ElementProperty::Boolean(true))),
                    on_true,
                ),
                (
                    OptionKey::Token(OptionToken::Value(ElementProperty::Boolean(false))),
                    on_false,
                ),
            ],
            locked: true,
        }
    }

    /// Appends a chained `.option(key, traversal)`. Returns `false` (does nothing) when this
    /// step is `locked` (the 3-argument sugar form) — the caller pushes a dangling-option marker
    /// instead, matching `RepeatWithoutBody`'s "no host step" precedent.
    pub(crate) fn push_option(
        &mut self,
        key: OptionKey<'g, S>,
        branch: AnonymousTraversal<'g, S>,
    ) -> bool {
        if self.locked {
            return false;
        }
        self.options.push((key, branch));
        true
    }

    fn select(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<Selection<S>> {
        if self.predicate_mode {
            let matched = self.selector.exists(context, source)?;
            return Ok(Selection::Value(Traverser::with_parts(
                TraverserValue::Computed(ElementProperty::Boolean(matched), None),
                source.path,
                1,
                source.sack,
            )));
        }
        let mut results = self.selector.apply_to(context, source)?;
        if results.is_empty() {
            return Ok(Selection::Unproductive);
        }
        Ok(Selection::Value(results.swap_remove(0)))
    }

    fn run_branch(
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
        branch: &AnonymousTraversal<'g, S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let results = branch.apply_to(context, source)?;
        output.extend(results);
        Ok(())
    }
}

/// `true` when `value` equals `literal` the way `P.eq(literal)` compares (numbers by value).
fn literal_matches<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    value: &TraverserValue<S>,
    literal: &ElementProperty,
) -> TraverserResult<bool> {
    let operand = match literal {
        ElementProperty::Null => PValue::Null,
        ElementProperty::String(text) => PValue::String(text.clone()),
        ElementProperty::Compressed(text) => PValue::String(text.text()),
        ElementProperty::Deduped(text) => PValue::String(text.as_str().into()),
        ElementProperty::Number(number) => PValue::Number(number.clone()),
        ElementProperty::Boolean(flag) => PValue::Boolean(*flag),
        ElementProperty::Uuid(bits) => PValue::Uuid(*bits),
        ElementProperty::Temporal(value) => PValue::Temporal(*value),
        ElementProperty::Vector(value) => PValue::Vector(value.clone()),
        // A container literal has no predicate operand: structural equality.
        ElementProperty::Object(_)
        | ElementProperty::Map(_)
        | ElementProperty::Array(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_) => {
            return Ok(&value.into_node_property(context.storage)? == literal);
        }
    };
    P::Eq(operand).test_literal(context, value)
}

impl<'g, S: GraphStorage + 'g> StepBranch<'g, S> {
    /// Fails for the invalid configurations of `choose()`: a `Pick.any` option or a traversal key.
    fn check_options(&self) -> TraverserResult<()> {
        if !matches!(self.kind, BranchKind::Choose) {
            return Ok(());
        }
        for (key, _) in &self.options {
            match key {
                OptionKey::Token(OptionToken::Any) => {
                    return Err(Box::new(TraverserError::InvalidOption {
                        key: "choose().option".to_string(),
                        reason: "choose() picks at most one branch and does not accept Pick.any, \
                            branch()'s always-additional catch-all; use branch() instead, or Pick.none \
                            for a default branch."
                            .to_string(),
                    }));
                }
                OptionKey::Traversal(_) => {
                    return Err(Box::new(TraverserError::InvalidOption {
                        key: "choose().option".to_string(),
                        reason: "a traversal is not allowed as a choose() option key; use a \
                            predicate (P.eq(..), P.between(..)), a literal, or branch(), which \
                            accepts a traversal key."
                            .to_string(),
                    }));
                }
                OptionKey::Token(_) => {}
            }
        }
        Ok(())
    }

    fn key_matches(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
        selected: &Traverser<S>,
        key: &OptionKey<'g, S>,
    ) -> TraverserResult<bool> {
        match key {
            OptionKey::Token(OptionToken::Value(literal)) => {
                literal_matches(context, &selected.value, literal)
            }
            OptionKey::Token(OptionToken::Predicate(predicate)) => {
                predicate.test_literal(context, &selected.value)
            }
            OptionKey::Token(OptionToken::Any | OptionToken::None | OptionToken::Unproductive) => {
                Ok(false)
            }
            OptionKey::Traversal(traversal) => {
                let start =
                    Traverser::with_parts(selected.value.clone(), source.path, 1, source.sack);
                traversal.exists(context, &start)
            }
        }
    }

    /// The options that fire for `source`, in firing order. See the module doc comment.
    fn pick(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<Picked> {
        let is_choose = matches!(self.kind, BranchKind::Choose);
        let mut indices = Vec::new();
        let selection = self.select(context, source)?;
        match &selection {
            Selection::Value(selected) => {
                for (idx, (key, _)) in self.options.iter().enumerate() {
                    if self.key_matches(context, source, selected, key)? {
                        indices.push(idx);
                        if is_choose {
                            break;
                        }
                    }
                }
                if indices.is_empty() {
                    self.push_fallback(&mut indices, |t| matches!(t, OptionToken::None), is_choose);
                }
            }
            Selection::Unproductive => {
                self.push_fallback(
                    &mut indices,
                    |t| matches!(t, OptionToken::Unproductive),
                    is_choose,
                );
            }
        }
        let pass_through = is_choose && indices.is_empty();
        if !is_choose {
            for (idx, (key, _)) in self.options.iter().enumerate() {
                if matches!(key, OptionKey::Token(OptionToken::Any)) {
                    indices.push(idx);
                }
            }
        }
        Ok(Picked {
            indices,
            pass_through,
        })
    }

    /// Adds the options whose token satisfies `is_fallback`: the first for `choose()`, all for
    /// `branch()`.
    fn push_fallback(
        &self,
        indices: &mut Vec<usize>,
        is_fallback: impl Fn(&OptionToken) -> bool,
        is_choose: bool,
    ) {
        for (idx, (key, _)) in self.options.iter().enumerate() {
            if matches!(key, OptionKey::Token(token) if is_fallback(token)) {
                indices.push(idx);
                if is_choose {
                    return;
                }
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepBranch<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        self.check_options()?;
        let picked = self.pick(context, &source)?;
        if picked.pass_through {
            output.push(source);
            return Ok(());
        }
        for idx in picked.indices {
            Self::run_branch(context, &source, &self.options[idx].1, output)?;
        }
        Ok(())
    }

    fn is_batch(&self) -> bool {
        self.options.iter().any(|(_, branch)| branch.has_barrier())
    }

    fn process_batch(
        &self,
        context: &mut TraverserContext<'_, S>,
        input: Vec<Traverser<S>>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        self.check_options()?;
        if input.is_empty() {
            return Ok(());
        }
        // The selector still runs per traverser; only the chosen option runs over a whole bucket.
        let mut buckets: Vec<Vec<Traverser<S>>> = vec![Vec::new(); self.options.len()];
        let mut passed = Vec::new();
        for source in input {
            let picked = self.pick(context, &source)?;
            if picked.pass_through {
                passed.push(source);
                continue;
            }
            for idx in picked.indices {
                buckets[idx].push(source.clone());
            }
        }
        for (idx, bucket) in buckets.into_iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            output.extend(self.options[idx].1.to_list(context, bucket)?);
        }
        output.extend(passed);
        Ok(())
    }

    /// The selector (the argument of `choose()`/`branch()`) first, then every option in
    /// attachment order: its key traversal (when the key is one), then its branch.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        let mut children = vec![&mut self.selector];
        for (key, branch) in &mut self.options {
            if let OptionKey::Traversal(key) = key {
                children.push(key);
            }
            children.push(branch);
        }
        children
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The selector is evaluated for its value only; its own output does not continue the
        // path (same convention as `where_t()`). Every option branch's output does continue the
        // enclosing path, like `union()`/`coalesce()`/`local()`.
        let mut need = self.selector.analyze_paths(
            analysis,
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
        );
        for (key, branch) in &mut self.options {
            // A traversal key is evaluated like the selector: for its result only.
            if let OptionKey::Traversal(key) = key {
                need = need.union(key.analyze_paths(
                    analysis,
                    crate::traversal::optimizer::path_analysis::PathRequirement::None,
                ));
            }
            need = need.union(branch.analyze_paths(analysis, downstream.clone()));
        }
        need
    }

    fn as_branch_mut(&mut self) -> Option<&mut StepBranch<'g, S>> {
        Some(self)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// A dangling `.option(...)` with no preceding `choose()`/`branch()` to attach to (or one that
/// tried to attach to the locked 3-argument `choose(predicate, onTrue, onFalse)` form). Mirrors
/// `StepRepeat`'s `RepeatWithoutBody` precedent: the malformed step is still pushed, and fails
/// clearly with a [`TraverserError`] only if the traversal is actually executed.
struct StepOptionWithoutBranch {
    rendered: String,
}

impl fmt::Display for StepOptionWithoutBranch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepOptionWithoutBranch {
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        _source: Traverser<S>,
        _output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        Err(Box::new(TraverserError::OptionWithoutBranch {
            modulator: self.rendered.clone(),
        }))
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(Self {
            rendered: self.rendered.clone(),
        })
    }
}

/// The placeholder for a dangling `.option(...)` (rendered as written), which fails with
/// [`TraverserError::OptionWithoutBranch`] when executed. Also used by `option(Merge.x, ..)`
/// without a preceding `mergeV()`/`mergeE()`.
pub(crate) fn option_without_branch<'g, S: GraphStorage + 'g>(
    rendered: String,
) -> GraphStep<'g, S> {
    GraphStep::FlatMap("option", Box::new(StepOptionWithoutBranch { rendered }))
}

/// The last non-modulator step of `steps`, when it is a `choose()`/`branch()`.
fn last_branch<'a, 'g, S: GraphStorage + 'g>(
    steps: &'a mut [GraphStep<'g, S>],
) -> Option<&'a mut StepBranch<'g, S>> {
    match steps
        .iter_mut()
        .rev()
        .find(|s| !matches!(s, GraphStep::Modulator(_, _)))
    {
        Some(GraphStep::FlatMap(_, step)) => step.as_branch_mut(),
        _ => None,
    }
}

/// Attaches a chained `.option(pick, traversal)` to the `choose()`/`branch()` that is the last
/// step of `steps`; pushes a [`StepOptionWithoutBranch`] otherwise (no preceding step, or the
/// preceding step is the locked 3-argument `choose()` form).
pub(crate) fn attach_option<'g, S: GraphStorage + 'g>(
    steps: &mut Vec<GraphStep<'g, S>>,
    key: OptionKey<'g, S>,
    branch: AnonymousTraversal<'g, S>,
) {
    let rendered = format!("option({key}, {branch})");
    if let Some(step) = last_branch(steps)
        && step.push_option(key, branch)
    {
        return;
    }
    steps.push(GraphStep::FlatMap(
        "option",
        Box::new(StepOptionWithoutBranch { rendered }),
    ));
}

impl __ {
    /// `__.choose(predicate, onTrue, onFalse)`: the anonymous-traversal if/then/else form; see
    /// [`GraphTraversalSource::choose`].
    pub fn choose<'g, S: GraphStorage + 'g>(
        predicate: AnonymousTraversal<'g, S>,
        on_true: AnonymousTraversal<'g, S>,
        on_false: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().choose(predicate, on_true, on_false)
    }

    /// `__.choose_switch(traversal)`: the anonymous-traversal chained form, for `.option(...)` to
    /// attach to; see [`GraphTraversalSource::choose_switch`].
    pub fn choose_switch<'g, S: GraphStorage + 'g>(
        selector: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().choose_switch(selector)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `choose(predicate, onTrue, onFalse)`: for each incoming traverser, runs `predicate`
    /// against it and continues with `onTrue` when `predicate` produces at least one result for
    /// it, `onFalse` otherwise — always exactly one of the two (TinkerPop's if/then/else sugar
    /// form; see this module's doc comment for the confirmed semantics).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v(None)
    ///     .choose(__::has_label("person"), __::out("knows"), __::out("created"))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert!(!names.is_empty());
    /// ```
    pub fn choose(
        self,
        predicate: AnonymousTraversal<'g, S>,
        on_true: AnonymousTraversal<'g, S>,
        on_false: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "choose",
            Box::new(StepBranch::new_if_then_else(predicate, on_true, on_false)),
        ))
    }

    /// `choose(traversal).option(pick, traversal)...`: the switch/chained form. `selector` runs
    /// once per incoming traverser; its first produced value is tested, with the comparison core of `P.eq` (numbers by value), against
    /// every chained `.option(pick, traversal)`'s `pick` key. At most one matching option's
    /// traversal runs — the first one, in the order `.option()` was written — or `Pick.none`'s
    /// when nothing else matched. With no match and no `Pick.none` (or when `selector` produces
    /// nothing and there is no `Pick.unproductive`), the traverser passes through unchanged.
    /// See this module's doc comment and `book/src/users_guide/option_semantics.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let labels = lock
    ///     .traversal()
    ///     .v(None)
    ///     .choose_switch(__::label())
    ///     .option("person".into(), __::constant("human"))
    ///     .option("software".into(), __::constant("code"))
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(labels.len(), 6);
    /// ```
    pub fn choose_switch(self, selector: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "choose",
            Box::new(StepBranch::new_switch(BranchKind::Choose, selector)),
        ))
    }

    /// Chains a `.option(pick, traversal)` onto the preceding `choose(traversal)`/
    /// `branch(traversal)`; see [`choose_switch`](Self::choose_switch)/[`branch`](Self::branch).
    /// `pick` is either an [`ElementProperty`] (equality-matched against the selector's produced
    /// value) or one of the `Pick` sentinels — pass [`ElementProperty::Boolean`]/etc. directly
    /// from Rust, or use `OptionToken` via the Rhai bindings' `Pick.any`/`Pick.none`.
    pub fn option(self, pick: ElementProperty, branch: AnonymousTraversal<'g, S>) -> Self {
        self.option_token(OptionToken::Value(pick), branch)
    }

    pub(crate) fn option_token(
        mut self,
        token: OptionToken,
        branch: AnonymousTraversal<'g, S>,
    ) -> Self {
        attach_option(&mut self.steps, OptionKey::Token(token), branch);
        self
    }

    /// `.option(predicate, traversal)`: the option fires when `predicate` holds for the
    /// selector's result (`option(P.between(26, 30), ..)`). Chains onto the preceding
    /// `choose(traversal)`/`branch(traversal)` like [`option`](Self::option).
    pub fn option_p(self, predicate: P<PValue>, branch: AnonymousTraversal<'g, S>) -> Self {
        self.option_token(OptionToken::Predicate(predicate), branch)
    }

    /// `.option(traversal, traversal)`: the option fires when `key` produces a result with the
    /// selector's value as its start. `branch()` only: `choose()` raises
    /// [`TraverserError::InvalidOption`] when the traversal executes.
    pub fn option_traversal(
        mut self,
        key: AnonymousTraversal<'g, S>,
        branch: AnonymousTraversal<'g, S>,
    ) -> Self {
        attach_option(&mut self.steps, OptionKey::Traversal(key), branch);
        self
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `choose(predicate, onTrue, onFalse)`; see [`GraphTraversalSource::choose`].
    pub fn choose(
        self,
        predicate: AnonymousTraversal<'g, S>,
        on_true: AnonymousTraversal<'g, S>,
        on_false: AnonymousTraversal<'g, S>,
    ) -> Self {
        let step = GraphStep::FlatMap(
            "choose",
            Box::new(StepBranch::new_if_then_else(predicate, on_true, on_false)),
        );
        self.push_step(step)
    }

    /// Anonymous `choose(traversal)`; see [`GraphTraversalSource::choose_switch`].
    pub fn choose_switch(self, selector: AnonymousTraversal<'g, S>) -> Self {
        let step = GraphStep::FlatMap(
            "choose",
            Box::new(StepBranch::new_switch(BranchKind::Choose, selector)),
        );
        self.push_step(step)
    }

    /// Anonymous `.option(pick, traversal)`; see [`GraphTraversalSource::option`].
    pub fn option(self, pick: ElementProperty, branch: AnonymousTraversal<'g, S>) -> Self {
        self.option_token(OptionToken::Value(pick), branch)
    }

    pub(crate) fn option_token(
        mut self,
        token: OptionToken,
        branch: AnonymousTraversal<'g, S>,
    ) -> Self {
        attach_option(&mut self.steps, OptionKey::Token(token), branch);
        self
    }

    /// Anonymous `.option(predicate, traversal)`; see [`GraphTraversalSource::option_p`].
    pub fn option_p(self, predicate: P<PValue>, branch: AnonymousTraversal<'g, S>) -> Self {
        self.option_token(OptionToken::Predicate(predicate), branch)
    }

    /// Anonymous `.option(traversal, traversal)`; see [`GraphTraversalSource::option_traversal`].
    pub fn option_traversal(
        mut self,
        key: AnonymousTraversal<'g, S>,
        branch: AnonymousTraversal<'g, S>,
    ) -> Self {
        attach_option(&mut self.steps, OptionKey::Traversal(key), branch);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn choose_if_then_else() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .choose(__::has_label("person"), __::out("knows"), __::out("created"))
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V().choose(__.hasLabel("person"), __.out("knows"), __.out("created")).values("name")"#,
                r#"g.v().choose(__.has_label("person"), __.out("knows"), __.out("created")).values("name")"#,
            ],
            assert: |res| {
                let mut names: Vec<String> = res.iter().filter_map(|v| v.as_string().map(String::from)).collect();
                names.sort();
                // person vertices (marko/vadas/josh/peter) take out("knows"), software vertices
                // (lop/ripple) take out("created") (nothing, they have no out-edges).
                assert_eq!(names, vec!["josh", "vadas"]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn choose_switch_with_default() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .choose_switch(__::label())
                .option(ElementProperty::from("person"), __::constant(ElementProperty::from("human")))
                .option(ElementProperty::from("software"), __::constant(ElementProperty::from("code")))
                .to_list(),
            rhai: [
                r#"g.V().choose(__.label()).option("person", __.constant("human")).option("software", __.constant("code"))"#,
                r#"g.v().choose(__.label()).option("person", __.constant("human")).option("software", __.constant("code"))"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 6);
                let humans = res.iter().filter(|v| v.as_string() == Some("human")).count();
                let code = res.iter().filter(|v| v.as_string() == Some("code")).count();
                assert_eq!(humans, 4);
                assert_eq!(code, 2);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn choose_switch_no_match_passes_traverser_through() {
        // Step 3c changed the meaning of this test (it used to assert a drop): with no matching
        // option and no Pick.none, choose() passes the traverser through unchanged
        // (`Choose.feature::g_V_chooseXout_countX_optionX2L_nameX_optionX3L_ageX`).
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .choose_switch(__::label())
                .option(ElementProperty::from("person"), __::constant(ElementProperty::from("human")))
                .to_list(),
            rhai: [
                r#"g.V().choose(__.label()).option("person", __.constant("human"))"#,
                r#"g.v().choose(__.label()).option("person", __.constant("human"))"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 6); // the 2 software vertices pass through unchanged
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn choose_first_match_wins() {
        // Two options with the same token: choose() takes only the first (at-most-one, unlike
        // branch()'s fan-out).
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .choose_switch(__::label())
                .option(ElementProperty::from("person"), __::out("knows"))
                .option(ElementProperty::from("person"), __::out("created"))
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V("1").choose(__.label()).option("person", __.out("knows")).option("person", __.out("created")).values("name")"#,
                r#"g.v("1").choose(__.label()).option("person", __.out("knows")).option("person", __.out("created")).values("name")"#,
            ],
            assert: |res| {
                // Only the first "person" option (out("knows")) fires.
                assert_eq!(res.len(), 2);
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .choose(
                __::has_label("person"),
                __::out("knows"),
                __::out("created"),
            )
            .profile()
            .unwrap();
        assert!(result.to_string().contains("choose("));
    }

    #[test]
    fn option_without_branch_fails_at_execution() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let err = g
            .v(None)
            .option(ElementProperty::from("person"), __::identity())
            .to_list()
            .unwrap_err();
        assert!(err.to_string().contains("option"));
    }
}
