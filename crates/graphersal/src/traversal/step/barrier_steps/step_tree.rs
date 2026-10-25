use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

/// Applies `by` round-robin to every object of `values` (root first) in place, as TinkerPop's
/// traversal ring does, like `path()`'s own `.by()` handling (`map_steps/step_path.rs`). An object
/// whose `by()` is unproductive (e.g. a missing property) is removed, so the rest of the path
/// moves up one level: TinkerPop 3.6+'s "productive `by()`" rule for `tree()` skips that level
/// rather than dropping the whole path (unlike `path()`, which filters the traverser).
///
/// Takes `values` as a caller-owned, caller-reused buffer (root first) rather than returning a
/// freshly allocated `Vec`, so a `process()` loop calling this once per traverser allocates
/// nothing here (Rule 4: no per-item allocation) — see [`PathArena::collect_values_into`].
fn resolve_tree_path<'g, S: GraphStorage + 'g>(
    context: &mut TraverserContext<'_, S>,
    values: &mut Vec<TraverserValue<S>>,
    by: &[By<'g, S>],
    child_needs_path: bool,
) -> TraverserResult<()> {
    if by.is_empty() {
        return Ok(());
    }
    let mut kept = 0;
    for position in 0..values.len() {
        let modulator = &by[position % by.len()];
        let placeholder = TraverserValue::Computed(ElementProperty::Null, None);
        let mut object = Traverser::start(
            context,
            std::mem::replace(&mut values[position], placeholder),
        );
        // See `step_select.rs`'s `child_needs_path` doc comment: a `by(__.path())` child must
        // see the projected object as a one-element path, not an empty one (verified against
        // real TinkerPop 3.7/TinkerGraph: `tree().by(__.path())` wraps each level in
        // `path[<object>]`).
        if child_needs_path && matches!(modulator, By::Traversal(_)) {
            object.path =
                context
                    .paths
                    .push(crate::value::PathRef::NONE, object.value.clone(), None)?;
        }
        if let Some(resolved) =
            TraverserValue::fetch_traverser_value_by(context, &object, &object.value, modulator)?
        {
            values[kept] = resolved;
            kept += 1;
        }
    }
    values.truncate(kept);
    Ok(())
}

/// Merges one traverser's resolved path (root first) into the accumulating tree: each object
/// becomes a nested key, in visitation order, and a path sharing a prefix with an earlier one
/// descends into the same branch instead of creating a sibling duplicate. A leaf (an object
/// nothing was reached from) is an empty `TraverserValue::Object`, matching TinkerPop's own
/// `Tree` representation (every node, leaf included, is itself a map). Recursion depth is one
/// path's length, called once per traverser — not a per-item hot loop. Takes `values` as a slice,
/// not an owned `Vec`, so the caller's per-traverser buffer is never consumed: the only clones are
/// the ones `IndexMap::entry` requires to look a key up, whether or not the branch already exists.
fn merge_tree_path<S: GraphStorage>(tree: &mut StreamMap<S>, values: &[TraverserValue<S>]) {
    if let Some((value, rest)) = values.split_first() {
        let entry = tree
            .0
            .entry(value.clone())
            .or_insert_with(|| TraverserValue::Object(StreamMap::default()));
        if let TraverserValue::Object(inner) = entry {
            merge_tree_path(inner, rest);
        }
    }
}

/// `tree()`: aggregates every traverser's full path into one hierarchical tree, where each
/// path's objects become nested keys in visitation order and traversers sharing a path prefix
/// merge into the same branch. Represented as `TraverserValue::Object(StreamMap<S>)` — the same
/// lazy, nested key→value shape `group()` already uses for "a map keyed by graph elements, valued
/// by more maps" — rather than inventing a third representation; it materializes into nested
/// `ElementProperty::Object` maps at the terminal, exactly like `group()`'s result does. A leaf is
/// an empty `Object`, matching TinkerPop's own `Tree`.
///
/// `tree()` ignores traverser bulk, as TinkerPop's does: a path shared by merged traversers is
/// one branch either way, so the result is the same with and without merging.
///
/// This is the no-key form only. `tree("key")` is a different step, [`StepTreeSideEffect`]: with a
/// key the traversers pass through unchanged and the tree is only a side effect, as in TinkerPop's
/// `TreeSideEffectStep`.
struct StepTree<'g, S: GraphStorage + 'g> {
    /// Set when the caller passed more than one key (`tree()` only ever takes zero or one);
    /// surfaced as a query-execution error at `process()` time rather than panicking at
    /// construction, since `tree()` returns `Self` (not `Result`) to stay fluent.
    invalid_key: bool,
    by: Vec<By<'g, S>>,
    by_check: ByModulatorCheck,
    /// Whether a `by(__...)` child needs the path object it runs on recorded as a one-element
    /// path — see `step_select.rs`'s `child_needs_path` doc comment. Computed once at plan time.
    child_needs_path: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepTree<'g, S> {
    fn clone(&self) -> Self {
        Self {
            invalid_key: self.invalid_key,
            by: self.by.clone(),
            by_check: self.by_check.clone(),
            child_needs_path: self.child_needs_path,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepTree<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tree()")?;
        for by in &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

/// The side-effect form `tree("key")` (TinkerPop `TreeSideEffectStep`): every traverser passes
/// through unchanged (bulk untouched) while its path is folded into the tree stored under `key`
/// (`SideEffectValue::Value(Object)`). A later `tree("key")` or a seeded tree accumulates into the
/// same tree; `cap("key")` / `select("key")` read it. The engine evaluates whole batches, so the
/// tree is complete before the next step runs, which is what TinkerPop's `LazyBarrierStrategy`
/// produces for `out().tree("a").select("a")`. A step that reads the tree *while* traversers still
/// flow (`order().local(tree("a")).select("a")`) needs a streaming executor and differs; see
/// `book/src/users_guide/tree_side_effect.md`.
///
/// Like `tree()`, it ignores traverser bulk. An existing side effect under `key` that is not a
/// tree fails with `TraverserError::QueryExecution`.
struct StepTreeSideEffect<'g, S: GraphStorage + 'g> {
    key: CompactString,
    by: Vec<By<'g, S>>,
    by_check: ByModulatorCheck,
    child_needs_path: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepTreeSideEffect<'g, S> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
            child_needs_path: self.child_needs_path,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepTreeSideEffect<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tree({})",
            crate::value::literal::rhai_string(self.key.as_str())
        )?;
        for by in &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

/// Folds the paths of `source` into `tree`, reusing one buffer for every traverser.
fn fold_paths<'g, S: GraphStorage + 'g>(
    context: &mut TraverserContext<'_, S>,
    tree: &mut StreamMap<S>,
    source: &[Traverser<S>],
    by: &[By<'g, S>],
    child_needs_path: bool,
) -> TraverserResult<()> {
    // Reused across every traverser (Rule 4: no per-item allocation) — capacity grows to the
    // longest path seen and then stays put, the same pattern `PathArena::is_simple` uses.
    let mut values: Vec<TraverserValue<S>> = Vec::new();
    for item in source {
        context.tick()?;
        context.paths.collect_values_into(item.path, &mut values);
        values.reverse(); // arena order is leaf first; tree merging needs root first
        resolve_tree_path(context, &mut values, by, child_needs_path)?;
        merge_tree_path(tree, &values);
    }
    Ok(())
}

pub(crate) fn side_effect_kind<S: GraphStorage>(
    value: &crate::traversal::side_effect::SideEffectValue<S>,
) -> &'static str {
    use crate::traversal::side_effect::SideEffectValue;
    match value {
        SideEffectValue::Value(_) => "single value",
        SideEffectValue::List(_) => "list",
        SideEffectValue::Set(_) => "set",
        SideEffectValue::Reduced { .. } => "reduced value",
        SideEffectValue::Subgraph { .. } => "subgraph",
        SideEffectValue::Group(_) => "group map",
    }
}

fn args_to_key(args: ArgStrings) -> (Option<CompactString>, bool) {
    match args {
        ArgStrings::None => (None, false),
        ArgStrings::String(v) => (Some(CompactString::new(v)), false),
        ArgStrings::List(_) => (None, true),
    }
}

/// `tree()` (the reducing barrier) without a key, `tree("key")` (the pass-through side effect) with
/// one.
fn tree_step<'g, S: GraphStorage + 'g>(args: ArgStrings) -> GraphStep<'g, S> {
    match args_to_key(args) {
        (Some(key), _) => GraphStep::FlatBarrier(
            "tree",
            Box::new(StepTreeSideEffect {
                key,
                by: Vec::new(),
                by_check: ByModulatorCheck::default(),
                child_needs_path: false,
            }),
        ),
        (None, invalid_key) => GraphStep::Barrier(
            "tree",
            Box::new(StepTree {
                invalid_key,
                by: Vec::new(),
                by_check: ByModulatorCheck::default(),
                child_needs_path: false,
            }),
        ),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Aggregates every traverser's full path into a hierarchical tree: each path's objects become
    /// nested keys in visitation order, traversers sharing a path prefix merge into the same
    /// branch, and a leaf is an empty map, matching TinkerPop's `Tree`.
    ///
    /// `tree()` without a key is a barrier that replaces the traversal's output with the tree.
    /// `tree("key")` is a side effect instead (TinkerPop's `TreeSideEffectStep`): every traverser
    /// passes through unchanged and the paths accumulate into the tree `key`, read with
    /// `cap("key")` or `select("key")`; a second `tree("key")` adds to the same tree. Using `key`
    /// for a side effect that is not a tree is an error.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let result = lock
    ///     .traversal()
    ///     .v("1")
    ///     .repeat(graphersal::prelude::__::out(None))
    ///     .times(2)
    ///     .tree(None)
    ///     .next()
    ///     .unwrap()
    ///     .unwrap();
    /// match result {
    ///     // The keys are vertices, so the tree is a map whose keys keep their type.
    ///     ElementProperty::Map(root) => assert_eq!(root.len(), 1),
    ///     _ => panic!("expected a tree"),
    /// }
    /// ```
    pub fn tree<T: Into<ArgStrings>>(self, args: T) -> Self {
        self.push_step(tree_step(args.into()))
    }
}

impl __ {
    /// Anonymous `tree()`; see [`GraphTraversalSource::tree`].
    pub fn tree<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        args: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().tree(args)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Aggregates every traverser's full path into a hierarchical tree; see
    /// [`GraphTraversalSource::tree`].
    pub fn tree<T: Into<ArgStrings>>(self, args: T) -> Self {
        self.push_step(tree_step(args.into()))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepTree<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.by.iter().filter_map(By::property_key) {
            visit(Some(key), crate::graph::workload::AccessKind::Read);
        }
    }
    /// Builds a genuinely new nested aggregation structure, matching `group()`/`fold()`'s
    /// precedent — reported to `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    /// TinkerPop's `tree()` is a seeded reducing barrier: an empty stream reduces to `{}`, not
    /// nothing (confirmed against TinkerPop 3.7's `TreeStep`, whose seed is an empty `Tree`).
    fn always_emits(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.by_check.check("tree")?;
        if self.invalid_key {
            return Err(Box::new(TraverserError::QueryExecution {
                query: "tree".to_string(),
                reason: "tree() takes at most one side-effect key".to_string(),
            }));
        }

        let mut tree: StreamMap<S> = StreamMap::default();
        fold_paths(context, &mut tree, &source, &self.by, self.child_needs_path)?;
        Ok(Some(Traverser::start(
            context,
            TraverserValue::Object(tree),
        )))
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::Full
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // A `by(__...)` runs on a path object, a fresh traverser: it reads nothing from the
        // incoming path, exactly like `path()`'s own `.by()` (see `map_steps/step_path.rs`).
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

    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepTreeSideEffect<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        use crate::traversal::side_effect::SideEffectValue;
        self.by_check.check("tree")?;
        // Taken out of the map so the fold can borrow `context` mutably; put back below.
        let mut tree: StreamMap<S> = match context.side_effect.remove(&self.key) {
            None => StreamMap::default(),
            Some(SideEffectValue::Value(TraverserValue::Object(tree))) => tree,
            Some(other) => {
                let kind = side_effect_kind(&other);
                context.side_effect.insert(self.key.clone(), other);
                return Err(Box::new(TraverserError::QueryExecution {
                    query: "tree".to_string(),
                    reason: format!(
                        "side effect '{key}' already holds a {kind}, tree(\"{key}\") needs an unused key",
                        key = self.key
                    ),
                }));
            }
        };
        let folded = fold_paths(context, &mut tree, &source, &self.by, self.child_needs_path);
        context.side_effect.insert(
            self.key.clone(),
            SideEffectValue::Value(TraverserValue::Object(tree)),
        );
        folded?;
        Ok(Some(source))
    }

    fn declares_side_effect_key(&self) -> Option<&str> {
        Some(&self.key)
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::Full
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
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

    fn materializes(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walks into the single child of a one-key tree node, without hardcoding the (Debug-format,
    /// vertex-keyed) key text — keeps the assertion robust to how a default (no `.by()`) key
    /// renders.
    fn only_child(value: &ElementProperty) -> &ElementProperty {
        match value {
            ElementProperty::Object(map) => {
                assert_eq!(map.len(), 1, "expected exactly one key, got {map:?}");
                map.values().next().unwrap()
            }
            // Vertex keys keep their type, so a default tree is a `Map`.
            ElementProperty::Map(map) => {
                assert_eq!(map.len(), 1, "expected exactly one key, got {map:?}");
                map.values().next().unwrap()
            }
            other => panic!("expected a tree object, got {other:?}"),
        }
    }

    fn object_len(value: &ElementProperty) -> usize {
        match value {
            ElementProperty::Object(map) => map.len(),
            ElementProperty::Map(map) => map.len(),
            other => panic!("expected a tree object, got {other:?}"),
        }
    }

    /// TinkerPop 3.7's `tree()` is a seeded reducing barrier (`TreeStep`, seed: an empty `Tree`):
    /// an empty input stream still reduces to `{}`, not nothing (confirmed via `graphersal`:
    /// `g.V().hasLabel("nope").tree()` → `#{}`). A fused `count()` must then read `1`.
    #[test]
    fn empty_input_reduces_to_an_empty_tree() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").tree(ArgStrings::None).to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").tree()"#,
                r#"g.v().has_label("nope").tree()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(object_len(&res[0]), 0);
            },
        );
    }

    #[test]
    fn empty_input_tree_count_is_one() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").tree(ArgStrings::None).count().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").tree().count()"#,
                r#"g.v().has_label("nope").tree().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    /// The motivating example: `g.V(1).repeat(__.out()).times(2).tree()`. On
    /// `tinkerpop_modern`, marko(1)'s two-hop `out()` walk only survives via josh(4) (vadas(2) and
    /// lop(3) both dead-end after one hop), landing on ripple(5) and lop(3) — so the tree is
    /// marko -> josh -> {lop, ripple}.
    #[test]
    fn roadmap_repeat_times_two() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").repeat(__::out(None)).times(2).tree(ArgStrings::None).to_list(),
            rhai: [
                r#"g.V("1").repeat(__.out()).times(2).tree()"#,
                r#"g.v("1").repeat(__.out()).times(2).tree()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(object_len(&res[0]), 1, "one root: marko");
                let marko_children = only_child(&res[0]);
                assert_eq!(object_len(marko_children), 1, "marko has one child: josh");
                let josh_children = only_child(marko_children);
                assert_eq!(object_len(josh_children), 2, "josh has two children: lop, ripple");
            },
        );
    }

    /// Proves the merge logic combines a shared path prefix rather than duplicating it: josh(4)
    /// is reached from two different `out("created")` traversers (josh->lop and josh->ripple).
    /// A broken merge (each traverser writing a fresh top-level "josh" entry instead of sharing
    /// one) would leave `josh`'s subtree with only one child (whichever write ran last), not two.
    #[test]
    fn branching_paths_merge_not_duplicate() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").out("created").tree(ArgStrings::None).to_list(),
            rhai: [
                r#"g.V().hasLabel("person").out("created").tree()"#,
                r#"g.v().has_label("person").out("created").tree()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Map(root) => {
                        // marko, josh, peter: three distinct root keys, none merged into another.
                        assert_eq!(root.len(), 3);
                        let sizes: Vec<usize> = root.values().map(object_len).collect();
                        // Exactly one root (josh) reaches two children (lop, ripple); the other
                        // two (marko, peter) reach only lop.
                        assert_eq!(sizes.iter().filter(|&&n| n == 2).count(), 1);
                        assert_eq!(sizes.iter().filter(|&&n| n == 1).count(), 2);
                    }
                    other => panic!("expected a tree object, got {other:?}"),
                }
            },
        );
    }

    /// `tree().by(...)` reshapes what gets stored at each level — here, vertex names instead of
    /// whole vertices, making the keys directly assertable.
    #[test]
    fn tree_by_name() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").repeat(__::out(None)).times(2).tree(ArgStrings::None).by("name").to_list(),
            rhai: [
                r#"g.V("1").repeat(__.out()).times(2).tree().by("name")"#,
                r#"g.v("1").repeat(__.out()).times(2).tree().by("name")"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(root) => {
                        assert_eq!(root.len(), 1);
                        let marko_children = root.get("marko").expect("root's key is marko");
                        let josh = match marko_children {
                            ElementProperty::Object(children) => {
                                children.get("josh").expect("marko's child is josh")
                            }
                            other => panic!("expected a tree object, got {other:?}"),
                        };
                        match josh {
                            ElementProperty::Object(children) => {
                                assert_eq!(children.len(), 2);
                                assert!(children.contains_key("lop"));
                                assert!(children.contains_key("ripple"));
                            }
                            other => panic!("expected a tree object, got {other:?}"),
                        }
                    }
                    other => panic!("expected a tree object, got {other:?}"),
                }
            },
        );
    }

    /// `.to_tree()` renders the same hierarchy the structural tests above check by hand, as
    /// Unix-`tree`-style box drawing (`traversal::tree_render`). Asserts on substrings rather
    /// than the whole string, since `lop`/`ripple`'s sibling order isn't this test's concern —
    /// `tree_render`'s own unit tests already lock down the exact connector/indentation format
    /// against controlled fixtures.
    #[test]
    fn to_tree_renders_hierarchy() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let rendered = g
            .v("1")
            .repeat(__::out(None))
            .times(2)
            .tree(ArgStrings::None)
            .by("name")
            .to_tree()
            .unwrap();
        assert_eq!(
            rendered.lines().next(),
            Some("marko"),
            "single root, printed bare, in: {rendered}"
        );
        assert!(
            rendered.contains("└─ josh"),
            "marko's only child, in: {rendered}"
        );
        assert!(
            rendered.contains("lop") && rendered.contains("ripple"),
            "josh's two children, in: {rendered}"
        );
    }

    /// `tree("key")` is a side effect (see [`StepTreeSideEffect`]): `cap("key")` reads the tree,
    /// and the stream before `cap` is the unchanged input traversers.
    #[test]
    fn tree_key_reaches_cap() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").repeat(__::out(None)).times(2).tree("t").by("name").cap("t").to_list(),
            rhai: [
                r#"g.V("1").repeat(__.out()).times(2).tree("t").by("name").cap("t")"#,
                r#"g.v("1").repeat(__.out()).times(2).tree("t").by("name").cap("t")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Object(root) => {
                        assert_eq!(root.len(), 1);
                        assert!(root.contains_key("marko"));
                    }
                    other => panic!("expected a tree object, got {other:?}"),
                }
            },
        );
    }

    /// The keyed form passes its input through, and a keyed tree and the reducing barrier agree.
    #[test]
    fn keyed_tree_passes_traversers_through() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).tree("a").values("name").to_list(),
            rhai: [
                r#"g.V("1").out().tree("a").values("name")"#,
                r#"g.v("1").out().tree("a").values("name")"#,
            ],
            assert: |res| assert_eq!(res.len(), 3),
        );
    }

    #[test]
    fn profile_shows_path_and_materializes() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v("1")
            .out(None)
            .out(None)
            .tree(ArgStrings::None)
            .profile()
            .unwrap();
        let rendered = result.to_string();
        assert!(rendered.contains("tree("), "{rendered}");
        assert!(rendered.contains("[path:"), "{rendered}");
    }

    /// `tree()` materializes (`materializes() == true`), like `group()`/`fold()`: it must report a
    /// non-zero `bytes_materialized` under `.profile_with(ProfileType::Memory)`.
    #[test]
    fn profile_memory_reports_materialization() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let stats = g
            .v("1")
            .out(None)
            .out(None)
            .tree(ArgStrings::None)
            .profile_with(ProfileType::Memory)
            .unwrap();
        #[allow(unused_variables)]
        let node = stats
            .metrics()
            .iter()
            .find(|n| n.name().contains("tree("))
            .expect("a tree() node is present");
        #[cfg(not(feature = "alloc-tracking"))]
        {
            assert!(node.memory().unwrap().bytes_materialized() > 0);
        }
    }
}
