//! Per-execution storage for traverser paths.
//!
//! A traverser does not own its path. It holds a [`PathRef`] into the [`PathArena`] of the
//! execution it belongs to, and every path is a chain of [`PathNode`]s linked to their parent.
//! Siblings produced by one fan-out share the prefix they came from, so cloning a traverser copies
//! four bytes and extending a path is a single `Vec::push`.
//!
//! Which positions are recorded at all is decided at compile time by the path requirement
//! analysis (`traversal/optimizer/path_analysis.rs`): a query with no path consumer never pushes a
//! node. Nothing that escapes an execution holds a [`PathRef`]; a path that leaves the engine is
//! materialized into an owned [`TraverserPath`].

use std::sync::Arc;

use compact_str::CompactString;

use crate::error::{TraverserError, TraverserResult};
use crate::graph::GraphStorage;
use crate::value::path::{TraverserPath, TraverserPathStep};
use crate::value::{ElementProperty, Pop, PropertyHandleKind, TraverserValue};

/// A handle to the last node of a traverser path inside a [`PathArena`].
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct PathRef(u32);

impl PathRef {
    /// The empty path: the traverser records nothing.
    pub(crate) const NONE: PathRef = PathRef(u32::MAX);

    /// Returns `true` when this handle points at no node.
    #[inline]
    pub(crate) fn is_none(self) -> bool {
        self == Self::NONE
    }
}

impl Default for PathRef {
    fn default() -> Self {
        Self::NONE
    }
}

/// One recorded position of a path.
pub(crate) struct PathNode<S: GraphStorage> {
    /// The previous position, or [`PathRef::NONE`] for the first one.
    pub(crate) parent: PathRef,
    /// The object at this position, kept as a lazy handle and never as materialized properties.
    pub(crate) value: TraverserValue<S>,
    /// Step labels attached to this position by `as()`, shared with the step (refcount only).
    pub(crate) labels: Option<Arc<[CompactString]>>,
}

/// Arena of path nodes owned by one traversal execution.
///
/// Nested traversals run on the same context and therefore share the arena. The arena is dropped
/// together with the context, so a [`PathRef`] never outlives the execution it was created in.
pub(crate) struct PathArena<S: GraphStorage> {
    nodes: Vec<PathNode<S>>,
    /// Maximum number of nodes; `u32::MAX - 1` in production, smaller in tests.
    capacity: u32,
    /// Estimated heap bytes of the values the nodes own (`0` for element handles, so a path of
    /// vertices and edges adds nothing): part of what `repeat()` checks against
    /// `traversal.max_materialized_bytes`, because a loop that records paths of growing values
    /// (`repeat(__.path())`) keeps every earlier value here as well.
    value_bytes: u64,
}

impl<S: GraphStorage> Default for PathArena<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: GraphStorage> PathArena<S> {
    /// Creates an empty arena. It allocates nothing until the first node is pushed.
    pub(crate) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            capacity: u32::MAX - 1,
            value_bytes: 0,
        }
    }

    /// Creates an arena that holds at most `capacity` nodes, so exhaustion can be tested.
    #[cfg(test)]
    pub(crate) fn with_capacity_limit(capacity: u32) -> Self {
        Self {
            nodes: Vec::new(),
            capacity: capacity.min(u32::MAX - 1),
            value_bytes: 0,
        }
    }

    /// Estimated heap bytes of the values recorded so far (see the field).
    #[inline]
    pub(crate) fn value_bytes(&self) -> u64 {
        self.value_bytes
    }

    /// Number of nodes pushed so far.
    #[allow(dead_code)]
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    #[inline]
    fn node(&self, path: PathRef) -> Option<&PathNode<S>> {
        if path.is_none() {
            None
        } else {
            self.nodes.get(path.0 as usize)
        }
    }

    /// Appends a position after `parent` and returns the handle to it.
    ///
    /// Fails with [`TraverserError::PathArenaExhausted`] instead of wrapping around when the
    /// arena is full.
    #[inline]
    pub(crate) fn push(
        &mut self,
        parent: PathRef,
        value: TraverserValue<S>,
        labels: Option<Arc<[CompactString]>>,
    ) -> TraverserResult<PathRef> {
        let index = self.nodes.len();
        if index >= self.capacity as usize {
            return Err(Box::new(TraverserError::PathArenaExhausted {
                capacity: u64::from(self.capacity),
            }));
        }
        // One `match` for an element handle; a container is walked once.
        self.value_bytes = self.value_bytes.saturating_add(
            crate::traversal::step::profile::estimate_traverser_value_bytes(&value),
        );
        self.nodes.push(PathNode {
            parent,
            value,
            labels,
        });
        Ok(PathRef(index as u32))
    }

    /// Attaches `labels` to the last position of `path` and returns the handle to use instead.
    ///
    /// Nodes are shared between siblings, so the last node is never mutated: a copy carrying the
    /// merged labels replaces it. When `path` is empty, a new first position holding `value` is
    /// created.
    pub(crate) fn relabel_last(
        &mut self,
        path: PathRef,
        value: &TraverserValue<S>,
        labels: &Arc<[CompactString]>,
    ) -> TraverserResult<PathRef> {
        let (parent, node_value, merged) = match self.node(path) {
            None => return self.push(PathRef::NONE, value.clone(), Some(labels.clone())),
            Some(node) => {
                let merged = match &node.labels {
                    None => labels.clone(),
                    Some(existing) => {
                        let mut all: Vec<CompactString> = existing.iter().cloned().collect();
                        for label in labels.iter() {
                            if !all.contains(label) {
                                all.push(label.clone());
                            }
                        }
                        Arc::from(all)
                    }
                };
                (node.parent, node.value.clone(), merged)
            }
        };
        self.push(parent, node_value, Some(merged))
    }

    /// Iterates the positions of `path` from the last one back to the first.
    pub(crate) fn iter_rev(&self, path: PathRef) -> PathIter<'_, S> {
        PathIter {
            arena: self,
            current: path,
        }
    }

    /// Number of positions recorded in `path`.
    #[allow(dead_code)]
    pub(crate) fn len(&self, path: PathRef) -> usize {
        self.iter_rev(path).count()
    }

    /// Returns the object most recently labelled `label` (last match wins), as `select()` reads it.
    pub(crate) fn by_label(&self, path: PathRef, label: &str) -> Option<TraverserValue<S>> {
        self.iter_rev(path)
            .find(|node| {
                node.labels
                    .as_ref()
                    .is_some_and(|labels| labels.iter().any(|l| l == label))
            })
            .map(|node| node.value.clone())
    }

    /// Reads the objects labelled `label` along `path` as `select(Pop, "label")` does.
    ///
    /// `None` when no position carries the label (the traverser is filtered, as TinkerPop's
    /// `Scoping` only reads the path for a label it has). Several labels on one position are one
    /// node, hence one occurrence. The rules follow TinkerPop's `ImmutablePath`, the path every
    /// live traversal uses (`javap -c` of gremlin-core 3.7.2; see `book/src/users_guide/select_pop.md`):
    ///
    /// - `First` / `Last`: the oldest / newest occurrence, returned as it is (a list-valued
    ///   occurrence stays whole).
    /// - `All`: always a list of the occurrences in path order (`[[1, 2]]` for one list-valued
    ///   occurrence).
    /// - `Mixed`: TinkerPop's `Path.get(label)`: the single occurrence itself; for several
    ///   occurrences a list in path order, except that when the *oldest* occurrence is itself a
    ///   list the later occurrences are appended to a copy of it (TinkerPop appends to the stored
    ///   list in place, which this does not).
    ///
    /// `Last` allocates nothing; `First` clones one value; `All`/`Mixed` build one result `Vec`
    /// (rule 4: nothing per traverser besides the result).
    pub(crate) fn labelled(
        &self,
        storage: &S,
        path: PathRef,
        label: &str,
        pop: Pop,
    ) -> Result<Option<TraverserValue<S>>, crate::error::GraphError> {
        let has = |node: &&PathNode<S>| {
            node.labels
                .as_ref()
                .is_some_and(|labels| labels.iter().any(|l| l == label))
        };
        Ok(match pop {
            Pop::Last => self.iter_rev(path).find(has).map(|node| node.value.clone()),
            Pop::First => self
                .iter_rev(path)
                .filter(has)
                .last()
                .map(|node| node.value.clone()),
            Pop::All => {
                let mut found: Vec<TraverserValue<S>> = self
                    .iter_rev(path)
                    .filter(has)
                    .map(|node| node.value.clone())
                    .collect();
                if found.is_empty() {
                    return Ok(None);
                }
                found.reverse();
                Some(TraverserValue::Array(found))
            }
            Pop::Mixed => {
                let mut found: Vec<TraverserValue<S>> = self
                    .iter_rev(path)
                    .filter(has)
                    .map(|node| node.value.clone())
                    .collect();
                found.reverse();
                match found.len() {
                    0 => None,
                    1 => found.pop(),
                    _ => {
                        if let Some(mut items) = list_items(storage, &found[0])? {
                            items.extend(found.drain(1..));
                            Some(TraverserValue::Array(items))
                        } else {
                            Some(TraverserValue::Array(found))
                        }
                    }
                }
            }
        })
    }

    /// Appends `path`'s objects into `buf`, **last position first** (matching [`Self::iter_rev`]),
    /// clearing `buf` first. Unlike [`Self::materialize`], this skips the `TraverserPathStep`
    /// wrapper (no per-position label clone) and, called with a buffer a caller reuses across
    /// many paths in a loop, allocates nothing once the buffer's capacity has grown to the
    /// longest path seen — the same reused-scratch-buffer pattern as [`Self::is_simple`].
    /// Callers that need root-first order reverse `buf` themselves after this returns.
    pub(crate) fn collect_values_into(&self, path: PathRef, buf: &mut Vec<TraverserValue<S>>) {
        buf.clear();
        let mut current = path;
        while let Some(node) = self.node(current) {
            buf.push(node.value.clone());
            current = node.parent;
        }
    }

    /// Collects `path` into an owned path, first position first.
    pub(crate) fn materialize(&self, path: PathRef) -> TraverserPath<S> {
        let mut steps: Vec<TraverserPathStep<S>> = self
            .iter_rev(path)
            .map(|node| TraverserPathStep {
                value: node.value.clone(),
                label: node.labels.as_ref().map(|labels| labels.to_vec()),
            })
            .collect();
        steps.reverse();
        TraverserPath { steps }
    }

    /// Tests whether no object repeats along `path` (TinkerPop `Path.isSimple()`).
    ///
    /// Vertices and edges compare by identity, every other object by value, and a lazy property
    /// handle is compared by the value it points at. `scratch` is a buffer reused between calls,
    /// so after warm-up the test does not allocate.
    pub(crate) fn is_simple(
        &self,
        storage: &S,
        path: PathRef,
        scratch: &mut Vec<(u64, PathRef)>,
    ) -> Result<bool, crate::error::GraphError> {
        use std::hash::{Hash, Hasher};

        scratch.clear();
        let mut current = path;
        while let Some(node) = self.node(current) {
            let mut hasher = fnv::FnvHasher::default();
            PathObject::resolve(storage, &node.value)?.hash(&mut hasher);
            scratch.push((hasher.finish(), current));
            current = node.parent;
        }
        if scratch.len() < 2 {
            return Ok(true);
        }
        scratch.sort_unstable_by_key(|(hash, _)| *hash);

        let mut start = 0;
        while start < scratch.len() {
            let mut end = start + 1;
            while end < scratch.len() && scratch[end].0 == scratch[start].0 {
                end += 1;
            }
            for i in start..end {
                for j in (i + 1)..end {
                    let (Some(a), Some(b)) = (self.node(scratch[i].1), self.node(scratch[j].1))
                    else {
                        continue;
                    };
                    if PathObject::resolve(storage, &a.value)?
                        == PathObject::resolve(storage, &b.value)?
                    {
                        return Ok(false);
                    }
                }
            }
            start = end;
        }
        Ok(true)
    }
}

/// The elements of `value` when it is a list (an array value, or a lazily read array property),
/// as TinkerPop's `instanceof List` test sees it; `None` for every other value. Only `Pop.mixed`
/// over several occurrences asks (see [`PathArena::labelled`]).
fn list_items<S: GraphStorage>(
    storage: &S,
    value: &TraverserValue<S>,
) -> Result<Option<Vec<TraverserValue<S>>>, crate::error::GraphError> {
    let from_property = |property: Option<&ElementProperty>| match property {
        Some(ElementProperty::Array(items)) => {
            Some(items.iter().map(TraverserValue::from).collect())
        }
        _ => None,
    };
    Ok(match value {
        TraverserValue::Array(items) => Some(items.clone()),
        TraverserValue::Computed(property, _) => from_property(Some(property)),
        TraverserValue::VertexProperty(key, vertex, PropertyHandleKind::Value) => from_property(
            storage
                .vertex_properties(*vertex)?
                .and_then(|p| p.get(key.as_str())),
        ),
        TraverserValue::EdgeProperty(key, edge, PropertyHandleKind::Value) => from_property(
            storage
                .edge_properties(*edge)?
                .and_then(|p| p.get(key.as_str())),
        ),
        _ => None,
    })
}

/// Tests whether no object repeats in `values` (the objects of a windowed, projected path), with
/// the same equality as [`PathArena::is_simple`]. Allocates a scratch buffer, so only the
/// modulated forms of `simple_path()`/`cyclic_path()` use it.
pub(crate) fn is_simple_values<S: GraphStorage>(
    storage: &S,
    values: &[&TraverserValue<S>],
) -> Result<bool, crate::error::GraphError> {
    use std::hash::{Hash, Hasher};

    if values.len() < 2 {
        return Ok(true);
    }
    let mut hashed: Vec<(u64, usize)> = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let mut hasher = fnv::FnvHasher::default();
        PathObject::resolve(storage, value)?.hash(&mut hasher);
        hashed.push((hasher.finish(), index));
    }
    hashed.sort_unstable_by_key(|(hash, _)| *hash);
    let mut start = 0;
    while start < hashed.len() {
        let mut end = start + 1;
        while end < hashed.len() && hashed[end].0 == hashed[start].0 {
            end += 1;
        }
        for i in start..end {
            for j in (i + 1)..end {
                if PathObject::resolve(storage, values[hashed[i].1])?
                    == PathObject::resolve(storage, values[hashed[j].1])?
                {
                    return Ok(false);
                }
            }
        }
        start = end;
    }
    Ok(true)
}

/// Iterator over the nodes of one path, last position first.
pub(crate) struct PathIter<'a, S: GraphStorage> {
    arena: &'a PathArena<S>,
    current: PathRef,
}

impl<'a, S: GraphStorage> Iterator for PathIter<'a, S> {
    type Item = &'a PathNode<S>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.arena.node(self.current)?;
        self.current = node.parent;
        Some(node)
    }
}

/// A path object as `simplePath()` compares it: a borrowed view, so the comparison never clones.
enum PathObject<'a, S: GraphStorage> {
    /// A property value, read from a computed value or through a lazy property handle.
    Value(&'a ElementProperty),
    /// A string read from storage metadata, such as an element label.
    Str(&'a str),
    /// Any other object, compared as the traverser value itself (identity for elements).
    Raw(&'a TraverserValue<S>),
}

impl<'a, S: GraphStorage> PathObject<'a, S> {
    fn resolve(
        storage: &'a S,
        value: &'a TraverserValue<S>,
    ) -> Result<Self, crate::error::GraphError> {
        Ok(match value {
            TraverserValue::Computed(property, _) => PathObject::Value(property),
            TraverserValue::VertexProperty(key, vertex, kind)
                if !matches!(kind, PropertyHandleKind::Id | PropertyHandleKind::Labels) =>
            {
                if *kind == PropertyHandleKind::Label {
                    return Ok(match storage.vertex_label(*vertex)? {
                        Some(label) => PathObject::Str(label),
                        None => PathObject::Raw(value),
                    });
                }
                match storage
                    .vertex_properties(*vertex)?
                    .and_then(|p| p.get(key.as_str()))
                {
                    Some(property) => PathObject::Value(property),
                    None => PathObject::Raw(value),
                }
            }
            TraverserValue::EdgeProperty(key, edge, kind)
                if !matches!(kind, PropertyHandleKind::Id | PropertyHandleKind::Labels) =>
            {
                if *kind == PropertyHandleKind::Label {
                    return Ok(match storage.edge_label(*edge)? {
                        Some(label) => PathObject::Str(label),
                        None => PathObject::Raw(value),
                    });
                }
                match storage
                    .edge_properties(*edge)?
                    .and_then(|p| p.get(key.as_str()))
                {
                    Some(property) => PathObject::Value(property),
                    None => PathObject::Raw(value),
                }
            }
            other => PathObject::Raw(other),
        })
    }
}

impl<S: GraphStorage> std::hash::Hash for PathObject<'_, S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            // A string property and a label string must hash alike to be found equal.
            PathObject::Value(ElementProperty::String(s)) => s.as_str().hash(state),
            PathObject::Str(s) => s.hash(state),
            PathObject::Value(property) => property.hash(state),
            PathObject::Raw(value) => value.hash(state),
        }
    }
}

impl<S: GraphStorage> PartialEq for PathObject<'_, S> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PathObject::Value(a), PathObject::Value(b)) => a == b,
            (PathObject::Str(a), PathObject::Str(b)) => a == b,
            (PathObject::Value(ElementProperty::String(a)), PathObject::Str(b))
            | (PathObject::Str(b), PathObject::Value(ElementProperty::String(a))) => {
                a.as_str() == *b
            }
            (PathObject::Raw(a), PathObject::Raw(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DiagnosticError;
    use crate::graph::{TraversalGraph, VertexHandle};

    fn vertex(index: u32) -> TraverserValue<TraversalGraph> {
        TraverserValue::Vertex(VertexHandle::for_test(index))
    }

    #[test]
    fn empty_arena_allocates_nothing() {
        let arena: PathArena<TraversalGraph> = PathArena::new();
        assert_eq!(arena.node_count(), 0);
        assert_eq!(arena.len(PathRef::NONE), 0);
        assert!(arena.materialize(PathRef::NONE).steps.is_empty());
    }

    #[test]
    fn siblings_share_their_prefix() {
        let mut arena: PathArena<TraversalGraph> = PathArena::new();
        let root = arena.push(PathRef::NONE, vertex(0), None).unwrap();
        let a = arena.push(root, vertex(1), None).unwrap();
        let b = arena.push(root, vertex(2), None).unwrap();
        assert_eq!(arena.node_count(), 3);
        assert_eq!(arena.len(a), 2);
        assert_eq!(arena.len(b), 2);
        let path = arena.materialize(b);
        assert_eq!(path.steps[0].value, vertex(0));
        assert_eq!(path.steps[1].value, vertex(2));
    }

    #[test]
    fn by_label_returns_the_last_match() {
        let mut arena: PathArena<TraversalGraph> = PathArena::new();
        let labels: Arc<[CompactString]> = Arc::from(vec![CompactString::new("a")]);
        let first = arena
            .push(PathRef::NONE, vertex(0), Some(labels.clone()))
            .unwrap();
        let second = arena.push(first, vertex(1), Some(labels.clone())).unwrap();
        assert_eq!(arena.by_label(second, "a"), Some(vertex(1)));
        assert_eq!(arena.by_label(first, "a"), Some(vertex(0)));
        assert_eq!(arena.by_label(second, "b"), None);
    }

    #[test]
    fn labelled_reads_first_last_all_and_mixed_in_path_order() {
        let storage = TraversalGraph::new();
        let mut arena: PathArena<TraversalGraph> = PathArena::new();
        let a: Arc<[CompactString]> = Arc::from(vec![CompactString::new("a")]);
        let ab: Arc<[CompactString]> =
            Arc::from(vec![CompactString::new("a"), CompactString::new("b")]);
        let p0 = arena
            .push(PathRef::NONE, vertex(0), Some(a.clone()))
            .unwrap();
        let p1 = arena.push(p0, vertex(1), None).unwrap();
        // one position carrying two labels is one node, hence one occurrence of `a`
        let p2 = arena.push(p1, vertex(2), Some(ab)).unwrap();
        let p3 = arena.push(p2, vertex(3), Some(a)).unwrap();
        let all = |path, label, pop| arena.labelled(&storage, path, label, pop).unwrap();
        assert_eq!(all(p3, "a", Pop::First), Some(vertex(0)));
        assert_eq!(all(p3, "a", Pop::Last), Some(vertex(3)));
        assert_eq!(
            all(p3, "a", Pop::All),
            Some(TraverserValue::Array(vec![vertex(0), vertex(2), vertex(3)]))
        );
        assert_eq!(
            all(p3, "a", Pop::Mixed),
            Some(TraverserValue::Array(vec![vertex(0), vertex(2), vertex(3)]))
        );
        // `b` occurs once: `mixed` is the bare value, `all` wraps it
        assert_eq!(all(p3, "b", Pop::Mixed), Some(vertex(2)));
        assert_eq!(
            all(p3, "b", Pop::All),
            Some(TraverserValue::Array(vec![vertex(2)]))
        );
        assert_eq!(all(p3, "b", Pop::First), Some(vertex(2)));
        // a label on no position filters, for every pop
        for pop in [Pop::First, Pop::Last, Pop::All, Pop::Mixed] {
            assert_eq!(all(p3, "zz", pop), None, "{pop}");
            assert_eq!(all(PathRef::NONE, "a", pop), None, "{pop}");
        }
        // an older prefix sees only what it has
        assert_eq!(
            all(p1, "a", Pop::All),
            Some(TraverserValue::Array(vec![vertex(0)]))
        );
    }

    #[test]
    fn labelled_mixed_appends_to_a_list_valued_oldest_occurrence() {
        let storage = TraversalGraph::new();
        let mut arena: PathArena<TraversalGraph> = PathArena::new();
        let a: Arc<[CompactString]> = Arc::from(vec![CompactString::new("a")]);
        let list = TraverserValue::Array(vec![vertex(0), vertex(1)]);
        let p0 = arena
            .push(PathRef::NONE, list.clone(), Some(a.clone()))
            .unwrap();
        let p1 = arena.push(p0, vertex(2), Some(a)).unwrap();
        // TinkerPop's `Path.get(label)` appends to the list-valued first occurrence
        assert_eq!(
            arena.labelled(&storage, p1, "a", Pop::Mixed).unwrap(),
            Some(TraverserValue::Array(vec![vertex(0), vertex(1), vertex(2)]))
        );
        // `all` keeps the occurrences apart; `first`/`last` do not unwrap
        assert_eq!(
            arena.labelled(&storage, p1, "a", Pop::All).unwrap(),
            Some(TraverserValue::Array(vec![list.clone(), vertex(2)]))
        );
        assert_eq!(
            arena.labelled(&storage, p1, "a", Pop::First).unwrap(),
            Some(list.clone())
        );
        assert_eq!(
            arena.labelled(&storage, p0, "a", Pop::Mixed).unwrap(),
            Some(list)
        );
    }

    #[test]
    fn relabel_last_does_not_mutate_shared_nodes() {
        let mut arena: PathArena<TraversalGraph> = PathArena::new();
        let root = arena.push(PathRef::NONE, vertex(0), None).unwrap();
        let a: Arc<[CompactString]> = Arc::from(vec![CompactString::new("a")]);
        let b: Arc<[CompactString]> = Arc::from(vec![CompactString::new("b")]);
        let with_a = arena.relabel_last(root, &vertex(0), &a).unwrap();
        let with_b = arena.relabel_last(root, &vertex(0), &b).unwrap();
        assert_eq!(arena.by_label(with_a, "a"), Some(vertex(0)));
        assert_eq!(arena.by_label(with_a, "b"), None);
        assert_eq!(arena.by_label(with_b, "b"), Some(vertex(0)));
        assert_eq!(arena.len(with_a), 1);
        let both = arena.relabel_last(with_a, &vertex(0), &b).unwrap();
        assert_eq!(arena.len(both), 1);
        assert_eq!(arena.by_label(both, "a"), Some(vertex(0)));
        assert_eq!(arena.by_label(both, "b"), Some(vertex(0)));
    }

    #[test]
    fn exhausted_arena_returns_an_error() {
        let mut arena: PathArena<TraversalGraph> = PathArena::with_capacity_limit(2);
        let root = arena.push(PathRef::NONE, vertex(0), None).unwrap();
        let next = arena.push(root, vertex(1), None).unwrap();
        let err = arena.push(next, vertex(2), None).unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::PathArenaExhausted { capacity: 2 }
        ));
        assert!(err.help().unwrap_or_default().contains("limit("));
    }

    #[test]
    fn simple_path_detects_repeats() {
        let graph = TraversalGraph::tinkerpop_modern();
        let mut arena: PathArena<TraversalGraph> = PathArena::new();
        let mut scratch = Vec::new();
        let a = arena.push(PathRef::NONE, vertex(0), None).unwrap();
        let b = arena.push(a, vertex(1), None).unwrap();
        assert!(arena.is_simple(&graph, b, &mut scratch).unwrap());
        let c = arena.push(b, vertex(0), None).unwrap();
        assert!(!arena.is_simple(&graph, c, &mut scratch).unwrap());
        let text: TraverserValue<TraversalGraph> =
            TraverserValue::Computed(ElementProperty::String("x".into()), None);
        let d = arena.push(b, text.clone(), None).unwrap();
        let e = arena.push(d, text, None).unwrap();
        assert!(arena.is_simple(&graph, d, &mut scratch).unwrap());
        assert!(!arena.is_simple(&graph, e, &mut scratch).unwrap());
    }

    fn recorded(query: impl FnOnce(&TraversalGraph) -> usize) -> usize {
        let graph = TraversalGraph::tinkerpop_modern();
        query(&graph)
    }

    #[test]
    fn a_query_without_path_consumer_pushes_no_node() {
        let count = recorded(|g| {
            g.traversal()
                .v(None)
                .out(None)
                .out(None)
                .values("name")
                .dedup()
                .internal_path_node_count()
                .unwrap()
        });
        assert_eq!(count, 0);
        let count = recorded(|g| {
            g.traversal()
                .v(None)
                .as_("a")
                .out(None)
                .path()
                .count()
                .internal_path_node_count()
                .unwrap()
        });
        assert_eq!(
            count, 0,
            "neither the unread label nor the skipped path() record"
        );
    }

    #[test]
    fn only_read_labels_push_nodes() {
        // Six `as("a")` positions are read by select("a"); as("b") is never read.
        let count = recorded(|g| {
            g.traversal()
                .v(None)
                .as_("a")
                .out(None)
                .as_("b")
                .select("a")
                .internal_path_node_count()
                .unwrap()
        });
        assert_eq!(count, 6);
    }

    #[test]
    fn a_path_consumer_records_every_position() {
        // v("1"): 1 node, out(): 3 nodes; path() is last and records nothing more.
        let count = recorded(|g| {
            g.traversal()
                .v("1")
                .out(None)
                .path()
                .internal_path_node_count()
                .unwrap()
        });
        assert_eq!(count, 4);
    }
}
