//! `glob_path(pattern)`: a Gremlin extension that walks the tree-shaped part of a graph below each
//! incoming vertex along a filesystem-style glob pattern and emits every vertex the pattern matches.
//!
//! The pattern is compiled once, when the step is built, into the automaton of
//! [`crate::value::glob_path::GlobPattern`]; the walk is an iterative depth-first search that
//! carries the automaton's live states as a `u64` mask per stacked vertex and prunes a branch as
//! soon as its mask is empty.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;

use fnv::FnvHasher;

use crate::graph::GraphStorage;
use crate::traversal::context::tick_deadline;
use crate::value::glob_path::GlobPattern;
use crate::value::{PathRef, SackRef};
use crate::*;

/// The `reason` texts of `TraverserError::InvalidModulator` for `glob_path()`; its `help()` keys
/// its advice on them.
pub(crate) mod reason {
    /// No `by()` at all: the match key is required.
    pub(crate) const MISSING_KEY: &str = "glob_path() needs a first by() naming the match key";
    /// The first `by()` is a vertex-label list.
    pub(crate) const LABELS_AS_KEY: &str =
        "the first by() is the match key; the vertex-label list goes into the second by()";
    /// The first `by()` is neither a property key nor `T.id`/`T.label`.
    pub(crate) const KEY_KIND: &str = "the match key must be a property key, T.id or T.label";
    /// The first `by()` is a `jpath(..)` key: the match key is a stored property name, so a path
    /// (which reaches into a nested value) is not accepted; a plain string names the property.
    pub(crate) const JPATH_KEY: &str = "the match key must be a plain property name, not a jpath(..) path; \
         glob_path() matches the stored value of one property, use by(\"name\") instead";
    /// The second `by()` is not a vertex-label list.
    pub(crate) const FILTER_KIND: &str = "the second by() must be a list of vertex labels";
    /// The second `by()` is an empty vertex-label list.
    pub(crate) const EMPTY_LABELS: &str = "the vertex-label list is empty";
    /// A third `by()`.
    pub(crate) const TOO_MANY: &str = "glob_path() takes at most two by() modulators";
}

/// The `InvalidOption` key of an out-of-range `max_depth`.
pub(crate) const MAX_DEPTH_OPTION: &str = "glob_path.max_depth";

/// What a named pattern segment is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MatchKey {
    /// A property value; only an `ElementProperty::String` can match.
    Property(CompactString),
    /// The vertex id.
    Id,
    /// Any of the vertex labels.
    Label,
}

/// One `by()` modulator as `glob_path()` received it, reduced to what the step needs: a
/// `By::Traversal`'s child traversal is never run, so only its kind is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobBy {
    /// `by("key")`.
    Property(CompactString),
    /// `by(T.id)`.
    Id,
    /// `by(T.label)`.
    Label,
    /// `by(["a", "b"])`.
    Labels(Vec<CompactString>),
    /// Any other `by()` argument, by its rendered name; always rejected.
    Other(&'static str),
}

/// Renders `s` as a double-quoted string literal, escaping `"` and `\`.
fn write_quoted(f: &mut std::fmt::Formatter<'_>, s: &str) -> std::fmt::Result {
    f.write_str(&crate::value::literal::rhai_string(s))
}

impl fmt::Display for GlobBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobBy::Property(key) => write_quoted(f, key),
            GlobBy::Id => write!(f, "T.id"),
            GlobBy::Label => write!(f, "T.label"),
            GlobBy::Labels(labels) => {
                write!(f, "[")?;
                for (i, label) in labels.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write_quoted(f, label)?;
                }
                write!(f, "]")
            }
            GlobBy::Other(kind) => write!(f, "{kind}"),
        }
    }
}

/// Options of [`GraphTraversalSource::glob_path_with`]: a depth bound and a prune condition for
/// the walk. **A Gremlin extension**, like the step itself.
///
/// `GlobPathOptions::default()` sets nothing, so `glob_path_with(p, GlobPathOptions::default())`
/// is `glob_path(p)`. Each builder method sets one option:
///
/// ```rust
/// use graphersal::prelude::*;
///
/// let graph = TraversalGraph::file_tree();
/// let options = GlobPathOptions::default()
///     .max_depth(3)
///     .prune(__::has("name", "author"));
/// let files = graph.traversal().v("root").glob_path_with("**/*.rs", options).by("name").count().next()?;
/// assert_eq!(files.and_then(|count| count.as_i64()), Some(9));
/// # Ok::<(), Box<TraverserError>>(())
/// ```
#[non_exhaustive]
pub struct GlobPathOptions<'g, S: GraphStorage + 'g> {
    /// The deepest level below the start vertex the walk visits; kept as given (Rhai passes an
    /// `i64`) and validated when the step executes.
    max_depth: Option<i64>,
    /// A visited vertex for which this child traversal yields a result is not descended into.
    prune: Option<AnonymousTraversal<'g, S>>,
}

impl<'g, S: GraphStorage + 'g> Default for GlobPathOptions<'g, S> {
    /// No option set: an unbounded walk that prunes only where the pattern or the label filter
    /// cannot match.
    fn default() -> Self {
        Self {
            max_depth: None,
            prune: None,
        }
    }
}

impl<'g, S: GraphStorage + 'g> Clone for GlobPathOptions<'g, S> {
    fn clone(&self) -> Self {
        Self {
            max_depth: self.max_depth,
            prune: self.prune.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Debug for GlobPathOptions<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobPathOptions")
            .field("max_depth", &self.max_depth)
            .field("prune", &self.prune)
            .finish()
    }
}

impl<'g, S: GraphStorage + 'g> GlobPathOptions<'g, S> {
    /// Never visits a vertex more than `depth` levels below the start vertex: the start vertex
    /// is level 0 and its children are level 1, so `max_depth(1)` visits the children only. The
    /// bound applies to the whole walk, not only to `**`. `0` fails with
    /// [`TraverserError::InvalidOption`] (key `glob_path.max_depth`) when the step executes.
    pub fn max_depth(mut self, depth: u32) -> Self {
        self.max_depth = Some(i64::from(depth));
        self
    }

    /// `max_depth` as the Rhai DSL receives it: any integer, validated when the step executes.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn max_depth_raw(mut self, depth: i64) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Does not descend into a visited vertex for which `traversal`, started from that vertex,
    /// yields at least one result (like `find -prune`). The vertex itself is still emitted when
    /// it matches the pattern. The child starts from a vertex traverser that carries the path of
    /// the incoming traverser, so it can read step labels set before `glob_path`; errors it
    /// raises propagate.
    pub fn prune(mut self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.prune = Some(traversal);
        self
    }
}

/// Reusable buffers of one walk, owned by the `TraverserContext` between walks so the step
/// allocates nothing per visited vertex, and nothing per incoming traverser once they have grown.
struct WalkBuffers<V> {
    /// The DFS stack: a vertex, the automaton states live at it and its depth below the start.
    stack: WalkStack<V>,
    /// The seen-state mask of the start vertex and of every visited vertex with in-degree >= 2
    /// (walks without `max_depth`).
    visited: WalkVisited<V>,
    /// With `max_depth`: the offset in `depths` of each tracked vertex's per-state slots.
    depth_slots: HashMap<V, usize, BuildHasherDefault<FnvHasher>>,
    /// With `max_depth`: per tracked vertex, `n + 1` slots (one per automaton state, the accept
    /// state last) holding the smallest depth at which the vertex was reached with that state,
    /// `u32::MAX` for never.
    depths: Vec<u32>,
}

impl<V> Default for WalkBuffers<V> {
    fn default() -> Self {
        Self {
            stack: Vec::new(),
            visited: HashMap::default(),
            depth_slots: HashMap::default(),
            depths: Vec::new(),
        }
    }
}

/// Reusable scratch buffers of `glob_path()`, owned by the `TraverserContext`.
///
/// A walk moves them out of the context for its duration and puts them back afterwards, so a
/// `glob_path()` inside a `prune` child traversal (which runs on the same context during the
/// walk) gets its own buffers instead of aliasing the outer walk's.
pub(crate) struct GlobPathScratch<V> {
    /// The walk's stack and seen-state bookkeeping.
    walk: WalkBuffers<V>,
    /// Emission dedup (and the match set of `count()`), used only after a tracked vertex was
    /// expanded a second time, or by `count()` with `prune`.
    emitted: HashSet<V, BuildHasherDefault<FnvHasher>>,
}

impl<V> Default for GlobPathScratch<V> {
    fn default() -> Self {
        Self {
            walk: WalkBuffers::default(),
            emitted: HashSet::default(),
        }
    }
}

/// `glob_path(pattern[, options]).by(key).by([labels])`.
struct StepGlobPath<'g, S: GraphStorage + 'g> {
    /// The pattern as written, for rendering and errors.
    raw: CompactString,
    /// Compiled eagerly; a failure keeps its position and reason (`TraverserError` is not
    /// `Clone`) and surfaces as `ValueError::InvalidGlobPattern` on the first `process()`.
    pattern: Result<GlobPattern, (usize, String)>,
    /// The `by()` modulators in the order received; validated on execution.
    by: Vec<GlobBy>,
    /// `max_depth` and `prune`; `max_depth` is validated on execution.
    options: GlobPathOptions<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGlobPath<'g, S> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            pattern: self.pattern.clone(),
            by: self.by.clone(),
            options: self.options.clone(),
        }
    }
}

/// Renders a child traversal as it is written in the Rhai DSL: `__.step().step()`, without the
/// `by()` modulators that the steps already render themselves.
fn write_child<'g, S: GraphStorage + 'g>(
    f: &mut std::fmt::Formatter<'_>,
    traversal: &AnonymousTraversal<'g, S>,
) -> std::fmt::Result {
    write!(f, "{traversal}")
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGlobPath<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "glob_path(")?;
        write_quoted(f, &self.raw)?;
        let GlobPathOptions { max_depth, prune } = &self.options;
        if max_depth.is_some() || prune.is_some() {
            write!(f, ", {{")?;
            if let Some(depth) = max_depth {
                write!(f, "max_depth: {depth}")?;
            }
            if let Some(prune) = prune {
                if max_depth.is_some() {
                    write!(f, ", ")?;
                }
                write!(f, "prune: ")?;
                write_child(f, prune)?;
            }
            write!(f, "}}")?;
        }
        write!(f, ")")?;
        for by in &self.by {
            write!(f, ".by({by})")?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> StepGlobPath<'g, S> {
    fn new(pattern: &str, options: GlobPathOptions<'g, S>) -> Self {
        let compiled = GlobPattern::compile(pattern).map_err(|err| match err {
            ValueError::InvalidGlobPattern {
                position, reason, ..
            } => (position, reason),
            other => (0, other.to_string()),
        });
        Self {
            raw: CompactString::new(pattern),
            pattern: compiled,
            by: Vec::new(),
            options,
        }
    }

    /// The compiled pattern, or the stored compile failure.
    fn pattern(&self) -> TraverserResult<&GlobPattern> {
        match &self.pattern {
            Ok(pattern) => Ok(pattern),
            Err((position, reason)) => Err(Box::new(TraverserError::Value(
                ValueError::InvalidGlobPattern {
                    pattern: self.raw.to_string(),
                    position: *position,
                    reason: reason.clone(),
                },
            ))),
        }
    }

    /// Validates the positional `by()` modulators: the match key and the optional label filter.
    fn config(&self) -> TraverserResult<(MatchKey, Option<&[CompactString]>)> {
        let invalid = |position: usize,
                       reason: &str|
         -> TraverserResult<(MatchKey, Option<&[CompactString]>)> {
            Err(Box::new(TraverserError::InvalidModulator {
                step: "glob_path".to_string(),
                position,
                modulator: crate::traversal::by_reason::MODULATOR,
                reason: reason.to_string(),
            }))
        };
        let key = match self.by.first() {
            None => return invalid(1, reason::MISSING_KEY),
            Some(GlobBy::Property(key)) => MatchKey::Property(key.clone()),
            Some(GlobBy::Id) => MatchKey::Id,
            Some(GlobBy::Label) => MatchKey::Label,
            Some(GlobBy::Labels(_)) => return invalid(1, reason::LABELS_AS_KEY),
            Some(GlobBy::Other("<json_path>")) => return invalid(1, reason::JPATH_KEY),
            Some(GlobBy::Other(_)) => return invalid(1, reason::KEY_KIND),
        };
        let filter = match self.by.get(1) {
            None => None,
            Some(GlobBy::Labels(labels)) if labels.is_empty() => {
                return invalid(2, reason::EMPTY_LABELS);
            }
            Some(GlobBy::Labels(labels)) => Some(labels.as_slice()),
            Some(GlobBy::Property(_) | GlobBy::Id | GlobBy::Label | GlobBy::Other(_)) => {
                return invalid(2, reason::FILTER_KIND);
            }
        };
        if self.by.len() > 2 {
            return invalid(3, reason::TOO_MANY);
        }
        Ok((key, filter))
    }

    /// Validates `max_depth`: 1 to `u32::MAX` levels.
    fn max_depth(&self) -> TraverserResult<Option<u32>> {
        let Some(depth) = self.options.max_depth else {
            return Ok(None);
        };
        let reason = if depth < 1 {
            format!("must be at least 1, got {depth}")
        } else {
            match u32::try_from(depth) {
                Ok(depth) => return Ok(Some(depth)),
                Err(_) => format!("must be at most {}, got {depth}", u32::MAX),
            }
        };
        Err(Box::new(TraverserError::InvalidOption {
            key: MAX_DEPTH_OPTION.to_string(),
            reason,
        }))
    }
}

/// The traverser kinds `out()` accepts: a vertex, a vertex property with its vertex, a computed
/// value linked to a vertex.
const VERTEX_INPUT: &[&str] = &["vertex", "vertexProperty", "computed (linked to vertex)"];

/// The start vertex of an incoming traverser, or the cast exception `out()` raises.
fn start_vertex<S: GraphStorage>(value: &TraverserValue<S>) -> TraverserResult<S::VertexId> {
    let vertex = match value {
        TraverserValue::Vertex(vertex) | TraverserValue::VertexProperty(_, vertex, _) => {
            Some(*vertex)
        }
        TraverserValue::Computed(_, vertex) => *vertex,
        TraverserValue::Path(_)
        | TraverserValue::Edge(_)
        | TraverserValue::EdgeProperty(_, _, _)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Object(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Array(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => None,
    };
    vertex.ok_or_else(|| Box::new(TraverserError::cast_exception(VERTEX_INPUT, value)))
}

/// The automaton transition for `child`, reading its match key only when a live state needs it.
fn transition<S: GraphStorage>(
    storage: &S,
    pattern: &GlobPattern,
    key: &MatchKey,
    mask: u64,
    child: S::VertexId,
) -> Result<u64, GraphError> {
    if mask & pattern.needs_key() == 0 {
        return Ok(pattern.step(mask, None));
    }
    Ok(match key {
        MatchKey::Property(key) => {
            let value = storage
                .vertex_properties(child)?
                .and_then(|properties| properties.get(key.as_str()));
            // A compressed string is matched on its text.
            if let Some(ElementProperty::Compressed(text)) = value {
                return Ok(text.with_text(|text| pattern.step(mask, Some(text))));
            }
            let value = value.and_then(|value| match value {
                ElementProperty::String(value) => Some(value.as_str()),
                ElementProperty::Deduped(value) => Some(value.as_str()),
                ElementProperty::Compressed(_)
                | ElementProperty::Null
                | ElementProperty::Number(_)
                | ElementProperty::Boolean(_)
                | ElementProperty::Uuid(_)
                | ElementProperty::Temporal(_)
                | ElementProperty::Vector(_)
                | ElementProperty::Object(_)
                | ElementProperty::Map(_)
                | ElementProperty::Array(_)
                | ElementProperty::Vertex(_)
                | ElementProperty::Edge(_) => None,
            });
            pattern.step(mask, value)
        }
        MatchKey::Id => pattern.step(mask, storage.vertex_id(child)?),
        MatchKey::Label => {
            let labels = storage.vertex_labels(child)?;
            if labels.is_empty() {
                return Ok(pattern.step(mask, None));
            }
            labels.iter().fold(0u64, |next, label| {
                next | pattern.step(mask, Some(label.as_str()))
            })
        }
    })
}

/// The DFS stack of the walk: a vertex, the automaton states live at it and its depth.
type WalkStack<V> = Vec<(V, u64, u32)>;
/// The seen-state masks of the walk, keyed by vertex.
type WalkVisited<V> = HashMap<V, u64, BuildHasherDefault<FnvHasher>>;

/// What a walk needs besides the context, its buffers and its match callback.
struct Walk<'a, 'g, S: GraphStorage + 'g> {
    pattern: &'a GlobPattern,
    key: MatchKey,
    filter: Option<&'a [CompactString]>,
    start: S::VertexId,
    /// The deepest level below the start the walk visits.
    max_depth: Option<u32>,
    /// Evaluated on each vertex before it is expanded; a result stops the descent.
    prune: Option<&'a AnonymousTraversal<'g, S>>,
    /// The path of the incoming traverser, carried by the `prune` probe.
    path: PathRef,
    /// The sack of the incoming traverser, carried by the `prune` probe.
    sack: SackRef,
}

impl<'g, S: GraphStorage + 'g> Walk<'_, 'g, S> {
    /// Walks below `start` and calls `on_match` for every accepted vertex, the single walk shared
    /// by `process()` (which emits traversers) and `count()` (which only counts), so the two
    /// cannot diverge.
    ///
    /// On a tree every vertex is reached at most once, so `on_match` sees each matched vertex
    /// exactly once. After a cycle or a diamond it may see a vertex again; the returned flag
    /// ("a tracked vertex was re-expanded") says so, and the caller then deduplicates.
    ///
    /// `prune` runs on the context, so the caller must have moved its buffers out of it first.
    /// It is evaluated when a vertex is popped for expansion, which is the same set of vertices
    /// as "would be pushed": the start vertex (depth 0) is never evaluated, and a vertex at
    /// `max_depth` is never pushed.
    fn run(
        &self,
        context: &mut TraverserContext<'_, S>,
        buffers: &mut WalkBuffers<S::VertexId>,
        mut on_match: impl FnMut(S::VertexId),
    ) -> TraverserResult<bool> {
        let Self {
            pattern,
            ref key,
            filter,
            start,
            max_depth,
            prune,
            path,
            sack,
        } = *self;
        let WalkBuffers {
            stack,
            visited,
            depth_slots,
            depths,
        } = buffers;
        let live_states = pattern.accept_bit() - 1;
        // States `0..n` and the accept state `n`.
        let slots = pattern.n() + 1;
        let mut reexpanded = false;

        stack.clear();
        visited.clear();
        depth_slots.clear();
        depths.clear();
        if max_depth.is_some() {
            depth_slots.insert(start, 0);
            depths.resize(slots, u32::MAX);
            for_each_state(pattern.initial(), |state| {
                if let Some(depth) = depths.get_mut(state) {
                    *depth = 0;
                }
            });
        } else {
            visited.insert(start, pattern.initial());
        }
        stack.push((start, pattern.initial(), 0));

        while let Some((vertex, mask, depth)) = stack.pop() {
            if let (Some(prune), true) = (prune, depth > 0) {
                let probe = Traverser::with_parts(TraverserValue::Vertex(vertex), path, 1, sack);
                if prune.exists(context, &probe)? {
                    continue;
                }
            }
            let child_depth = depth.saturating_add(1);
            if max_depth.is_some_and(|max| child_depth > max) {
                continue;
            }
            // Expand children only while the vertex is at a depth whose children may be pushed.
            let expandable = max_depth.is_none_or(|max| child_depth < max);
            let storage: &S = &*context.storage;
            for edge in storage.out_edges(vertex)? {
                let Some((_, child)) = storage.edge_endpoints(edge)? else {
                    continue;
                };
                tick_deadline(&mut context.deadline)?;
                if let Some(labels) = filter
                    && !has_any_label(storage, child, labels)?
                {
                    continue;
                }
                let mut next = transition(storage, pattern, key, mask, child)?;
                if next == 0 {
                    continue;
                }
                // Cycles and diamonds can only close at the start or at a vertex with
                // in-degree >= 2, so only those carry seen-state bookkeeping; a tree tracks
                // nothing.
                if child == start || storage.in_edges(child)?.nth(1).is_some() {
                    if max_depth.is_some() {
                        // A state seen only deeper must be expanded again from here: the
                        // remaining depth budget is larger, so more of the subtree is in reach.
                        let (slot, revisit) = match depth_slots.get(&child) {
                            Some(&slot) => (slot, true),
                            None => {
                                let slot = depths.len();
                                depths.resize(slot + slots, u32::MAX);
                                depth_slots.insert(child, slot);
                                (slot, false)
                            }
                        };
                        let mut new = 0u64;
                        for_each_state(next, |state| {
                            if let Some(seen) = depths.get_mut(slot + state)
                                && *seen > child_depth
                            {
                                *seen = child_depth;
                                new |= 1u64 << state;
                            }
                        });
                        if new == 0 {
                            continue;
                        }
                        reexpanded |= revisit;
                        next = new;
                    } else {
                        match visited.get_mut(&child) {
                            Some(seen) => {
                                let new = next & !*seen;
                                if new == 0 {
                                    continue;
                                }
                                *seen |= next;
                                next = new;
                                reexpanded = true;
                            }
                            None => {
                                visited.insert(child, next);
                            }
                        }
                    }
                }
                if pattern.accepts(next) && child != start {
                    on_match(child);
                }
                if expandable && next & live_states != 0 {
                    stack.push((child, next, child_depth));
                }
            }
        }
        Ok(reexpanded)
    }
}

/// Whether `vertex` carries one of `labels` (stops at the first).
fn has_any_label<S: GraphStorage>(
    storage: &S,
    vertex: S::VertexId,
    labels: &[CompactString],
) -> Result<bool, GraphError> {
    for label in labels {
        if storage.vertex_has_label(vertex, label)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Calls `f` with the index of every set bit of `mask`.
fn for_each_state(mut mask: u64, mut f: impl FnMut(usize)) {
    while mask != 0 {
        f(mask.trailing_zeros() as usize);
        mask &= mask - 1;
    }
}

impl<'g, S: GraphStorage + 'g> StepGlobPath<'g, S> {
    /// The walk for one incoming traverser: validates the pattern, then the `by()` modulators,
    /// then the options, then the incoming value.
    fn prepare<'a>(&'a self, source: &Traverser<S>) -> TraverserResult<Walk<'a, 'g, S>> {
        let pattern = self.pattern()?;
        let (key, filter) = self.config()?;
        let max_depth = self.max_depth()?;
        Ok(Walk {
            pattern,
            key,
            filter,
            start: start_vertex(&source.value)?,
            max_depth,
            prune: self.options.prune.as_ref(),
            path: source.path,
            sack: source.sack,
        })
    }
}

/// Keeps only the first of the vertex traversers in `output[before..]` that share a vertex.
fn dedup_emitted<S: GraphStorage>(
    emitted: &mut HashSet<S::VertexId, BuildHasherDefault<FnvHasher>>,
    output: &mut Vec<Traverser<S>>,
    before: usize,
) {
    emitted.clear();
    let mut write = before;
    for read in before..output.len() {
        // The walk emits vertex traversers only.
        let keep = match output.get(read).map(|traverser| &traverser.value) {
            Some(TraverserValue::Vertex(vertex)) => emitted.insert(*vertex),
            Some(
                TraverserValue::Computed(..)
                | TraverserValue::VertexProperty(..)
                | TraverserValue::Edge(_)
                | TraverserValue::EdgeProperty(..)
                | TraverserValue::Path(_)
                | TraverserValue::Record { .. }
                | TraverserValue::Object(_)
                | TraverserValue::PathProperty(_)
                | TraverserValue::Entry(_)
                | TraverserValue::Array(_)
                | TraverserValue::Schema(_)
                | TraverserValue::Graph(_),
            )
            | None => true,
        };
        if keep {
            output.swap(write, read);
            write += 1;
        }
    }
    output.truncate(write);
}

/// Runs `f` with the `glob_path()` scratch buffers moved out of the context, and puts them back
/// afterwards, also on error. A nested `glob_path()` (in a `prune` child) that runs on the
/// context meanwhile finds empty buffers of its own.
fn with_scratch<S: GraphStorage, T>(
    context: &mut TraverserContext<'_, S>,
    f: impl FnOnce(
        &mut TraverserContext<'_, S>,
        &mut GlobPathScratch<S::VertexId>,
    ) -> TraverserResult<T>,
) -> TraverserResult<T> {
    let mut scratch = std::mem::take(&mut context.glob_path);
    let result = f(context, &mut scratch);
    context.glob_path = scratch;
    result
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Walks the tree below each incoming vertex along a filesystem-style glob `pattern` and
    /// emits every vertex it matches. **A Gremlin extension**, not part of TinkerPop.
    ///
    /// ```text
    /// g.v("root").glob_path("**/author/*/some/*.rs").by("name").by(["directory", "file"])
    /// ```
    ///
    /// - The incoming vertex is the root: the first segment matches its children; the start
    ///   vertex itself is never checked and never emitted.
    /// - Every outgoing edge is followed, whatever its label.
    /// - The `by()` modulators are positional. The first (required) is the match key a named
    ///   segment is compared with: a property key (`by("name")`), `By::Id` or `By::Label` (any
    ///   of the vertex labels). The second (optional) is a non-empty vertex-label list
    ///   (`by(["directory", "file"])`): every visited vertex below the start must carry one of
    ///   them. Anything else fails with `TraverserError::InvalidModulator` on execution.
    /// - Syntax: `*`, `?`, `**` (a whole segment: any number of levels), `[abc]`, `[a-z]`,
    ///   `[!abc]`, `{a,b}` within one segment, and `\` escaping. An invalid pattern fails with
    ///   `ValueError::InvalidGlobPattern` on execution.
    /// - A missing key or a non-string value never matches a named segment (no error, no
    ///   stringification); `*` and `**` never read the key.
    /// - The path grows by exactly one element, the matched vertex.
    /// - Each matched vertex is emitted at most once per incoming traverser; cycles terminate.
    ///
    /// Same as [`glob_path_with`](Self::glob_path_with) with `GlobPathOptions::default()`.
    pub fn glob_path(self, pattern: &str) -> Self {
        self.glob_path_with(pattern, GlobPathOptions::default())
    }

    /// [`glob_path`](Self::glob_path) with [`GlobPathOptions`]: `max_depth` bounds the walk to
    /// the levels `1..=max_depth` below the start vertex, and `prune` stops the descent below
    /// every visited vertex for which its child traversal yields a result (the vertex itself is
    /// still emitted when it matches). An out-of-range `max_depth` fails with
    /// `TraverserError::InvalidOption` on execution, after the pattern and `by()` checks.
    pub fn glob_path_with(self, pattern: &str, options: GlobPathOptions<'g, S>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "glob_path",
            Box::new(StepGlobPath::new(pattern, options)),
        ))
    }
}

impl __ {
    /// Anonymous [`GraphTraversalSource::glob_path`]: a Gremlin extension that walks the tree
    /// below each incoming vertex along a glob `pattern`; see there for the full semantics.
    pub fn glob_path<'g, S: GraphStorage + 'g>(pattern: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().glob_path(pattern)
    }

    /// Anonymous [`GraphTraversalSource::glob_path_with`]: `glob_path` with a depth bound and a
    /// prune condition; see there for the semantics.
    pub fn glob_path_with<'g, S: GraphStorage + 'g>(
        pattern: &str,
        options: GlobPathOptions<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().glob_path_with(pattern, options)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// [`GraphTraversalSource::glob_path`] in a child traversal: a Gremlin extension that walks
    /// the tree below each incoming vertex along a glob `pattern`; see there for the full
    /// semantics.
    pub fn glob_path(self, pattern: &str) -> Self {
        self.glob_path_with(pattern, GlobPathOptions::default())
    }

    /// [`GraphTraversalSource::glob_path_with`] in a child traversal: `glob_path` with a depth
    /// bound and a prune condition; see there for the semantics.
    pub fn glob_path_with(self, pattern: &str, options: GlobPathOptions<'g, S>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "glob_path",
            Box::new(StepGlobPath::new(pattern, options)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepGlobPath<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let walk = self.prepare(&source)?;
        let before = output.len();
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        with_scratch(context, |context, scratch| {
            let reexpanded = walk.run(context, &mut scratch.walk, |vertex| {
                output.push(Traverser::with_parts(
                    TraverserValue::Vertex(vertex),
                    path,
                    bulk,
                    sack,
                ))
            })?;
            // A re-expanded tracked vertex can reach a vertex below it again with other states;
            // keep only the first emission of each vertex.
            if reexpanded {
                dedup_emitted(&mut scratch.emitted, output, before);
            }
            Ok(())
        })
    }

    /// Counts the matches without creating a traverser: memory stays O(stack + visited),
    /// independent of the number of matches.
    ///
    /// The first pass counts every accepted vertex. It is exact unless a tracked vertex was
    /// re-expanded (never on a tree); then the walk runs a second time and collects the matched
    /// vertices into a set, so the result always equals `process()`'s output length. With
    /// `prune`, the walk collects into the set in its only pass, so the child traversal runs
    /// once per vertex, as in `process()`.
    fn count(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<u64> {
        let walk = self.prepare(&source)?;
        let bulk = source.bulk;
        with_scratch(context, |context, scratch| {
            let GlobPathScratch {
                walk: buffers,
                emitted,
            } = scratch;
            emitted.clear();
            if walk.prune.is_some() {
                walk.run(context, buffers, |vertex| {
                    emitted.insert(vertex);
                })?;
                return crate::traversal::bulk::mul_bulk(emitted.len() as u64, bulk, "glob_path");
            }
            let mut count = 0usize;
            let reexpanded = walk.run(context, buffers, |_| count += 1)?;
            if !reexpanded {
                return crate::traversal::bulk::mul_bulk(count as u64, bulk, "glob_path");
            }
            walk.run(context, buffers, |vertex| {
                emitted.insert(vertex);
            })?;
            crate::traversal::bulk::mul_bulk(emitted.len() as u64, bulk, "glob_path")
        })
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by.push(match args {
            By::Property(key) => GlobBy::Property(key),
            By::Id => GlobBy::Id,
            By::Label => GlobBy::Label,
            By::Labels(labels) => GlobBy::Labels(labels),
            By::Traversal(_) => GlobBy::Other("<traversal>"),
            By::Order(_) => GlobBy::Other("<order>"),
            By::Value => GlobBy::Other("Value"),
            By::Identity => GlobBy::Other("Identity"),
            By::Key => GlobBy::Other("Key"),
            By::Count => GlobBy::Other("Count"),
            By::JsonPath(_) => GlobBy::Other("<json_path>"),
            By::Column(_) => GlobBy::Other("<column>"),
        });
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.options.prune.as_mut().into_iter().collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The prune child is evaluated for its existence only, on a probe that carries the
        // incoming traverser's path; its output does not continue the path.
        match &mut self.options.prune {
            Some(prune) => prune.analyze_paths(
                analysis,
                crate::traversal::optimizer::path_analysis::PathRequirement::None,
            ),
            None => crate::traversal::optimizer::path_analysis::PathRequirement::None,
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Step = StepGlobPath<'static, &'static TraversalGraph>;

    fn by_names(step: &Step) -> String {
        step.to_string()
    }

    #[test]
    fn renders_pattern_and_modulators() {
        let mut step = Step::new("**/\"q\".rs", GlobPathOptions::default());
        step.by.push(GlobBy::Property(CompactString::new("name")));
        step.by.push(GlobBy::Labels(vec![
            CompactString::new("directory"),
            CompactString::new("file"),
        ]));
        assert_eq!(
            by_names(&step),
            r#"glob_path("**/\"q\".rs").by("name").by(["directory","file"])"#
        );
    }

    #[test]
    fn renders_only_the_options_that_are_set() {
        let step = Step::new("**", GlobPathOptions::default().max_depth(3));
        assert_eq!(by_names(&step), r#"glob_path("**", {max_depth: 3})"#);
        let step = Step::new(
            "**",
            GlobPathOptions::default().prune(__::has("name", "author")),
        );
        assert_eq!(
            by_names(&step),
            r#"glob_path("**", {prune: __.has("name", "author")})"#
        );
        let mut step = Step::new(
            "**/*.rs",
            GlobPathOptions::default()
                .max_depth_raw(-1)
                .prune(__::glob_path("*").by("name")),
        );
        step.by.push(GlobBy::Property(CompactString::new("name")));
        assert_eq!(
            by_names(&step),
            r#"glob_path("**/*.rs", {max_depth: -1, prune: __.glob_path("*").by("name")}).by("name")"#
        );
    }

    #[test]
    fn max_depth_is_validated_on_execution() {
        for (depth, expected) in [
            (0, Some("must be at least 1, got 0")),
            (-1, Some("must be at least 1, got -1")),
            (1, None),
            (i64::from(u32::MAX), None),
            (
                i64::from(u32::MAX) + 1,
                Some("must be at most 4294967295, got 4294967296"),
            ),
        ] {
            let step = Step::new("*", GlobPathOptions::default().max_depth_raw(depth));
            match (step.max_depth().map_err(|err| *err), expected) {
                (Ok(Some(actual)), None) => assert_eq!(i64::from(actual), depth),
                (Err(TraverserError::InvalidOption { key, reason }), Some(expected)) => {
                    assert_eq!(key, MAX_DEPTH_OPTION);
                    assert_eq!(reason, expected);
                }
                (other, _) => panic!("max_depth {depth}: unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn config_rejects_by_modulators_by_position() {
        let cases: Vec<(Vec<GlobBy>, usize, &str)> = vec![
            (vec![], 1, reason::MISSING_KEY),
            (
                vec![GlobBy::Labels(vec![CompactString::new("a")])],
                1,
                reason::LABELS_AS_KEY,
            ),
            (vec![GlobBy::Other("<traversal>")], 1, reason::KEY_KIND),
            (vec![GlobBy::Other("<json_path>")], 1, reason::JPATH_KEY),
            (vec![GlobBy::Id, GlobBy::Label], 2, reason::FILTER_KIND),
            (
                vec![GlobBy::Id, GlobBy::Labels(vec![])],
                2,
                reason::EMPTY_LABELS,
            ),
            (
                vec![
                    GlobBy::Id,
                    GlobBy::Labels(vec![CompactString::new("a")]),
                    GlobBy::Id,
                ],
                3,
                reason::TOO_MANY,
            ),
        ];
        for (by, expected_position, expected_reason) in cases {
            let mut step = Step::new("*", GlobPathOptions::default());
            step.by = by;
            match step.config().map(|_| ()).map_err(|err| *err) {
                Err(TraverserError::InvalidModulator {
                    position, reason, ..
                }) => {
                    assert_eq!(position, expected_position);
                    assert_eq!(reason, expected_reason);
                }
                other => panic!("expected InvalidModulator, got {other:?}"),
            }
        }
    }
}
