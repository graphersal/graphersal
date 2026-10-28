use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::*;

pub(super) struct StepSelect<'g, S: GraphStorage + 'g> {
    args: Option<Vec<String>>,
    /// Which occurrence of a repeated path label is read (`Pop::Last` for plain `select("a")`).
    /// Applies only to a label read from the path, after the map-key and side-effect lookups.
    pop: Pop,
    /// Deduplicated keys (first-occurrence order): `select("a","a")` produces one `"a"` entry,
    /// matching TinkerPop's `SelectStep`, which builds its result in a `LinkedHashMap` keyed by
    /// `selectKeys` — a repeated key collapses to one map entry, keeping its first position but
    /// the last `by()`-ring value written to it.
    precomputed_keys: Option<Arc<Vec<RecordKey>>>,
    /// For each position in `args` (same length, same order), the index into
    /// `precomputed_keys`/the runtime `values` vector that position writes to. Several positions
    /// share a slot exactly when `args` repeats a key.
    position_to_slot: Option<Vec<usize>>,
    by: Option<Vec<By<'g, S>>>,
    by_check: ByModulatorCheck,
    /// Whether a `by(__...)` child needs the projected object recorded as a one-element path
    /// (`[object]`, TinkerPop's `TraversalUtil.apply(object, traversal)`), so `by(__.path())`
    /// sees that single position instead of an empty path. Computed once, at plan time, by
    /// `analyze_nested_paths` — never per traverser (rule 4).
    child_needs_path: bool,
}

/// The object `key` names for `source`, in TinkerPop's `Scoping` order (a key of the traverser's
/// own map value, a side-effect key, a path label); `None` when it is found nowhere. The same
/// lookup `select()` uses, shared with the start/end labels of `where(traversal)`.
pub(crate) fn scope_value<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    source: &Traverser<S>,
    key: &str,
) -> TraverserResult<Option<TraverserValue<S>>> {
    StepSelect::<'_, S>::scope_value(context, source, &source.value, key, Pop::Last)
}

impl<'g, S: GraphStorage + 'g> Clone for StepSelect<'g, S> {
    fn clone(&self) -> Self {
        Self {
            args: self.args.clone(),
            pop: self.pop,
            precomputed_keys: self.precomputed_keys.clone(),
            position_to_slot: self.position_to_slot.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
            child_needs_path: self.child_needs_path,
        }
    }
}

impl<'g, S: GraphStorage + 'g> StepSelect<'g, S> {
    fn new(args: Option<Vec<String>>, pop: Pop) -> Self {
        let (precomputed_keys, position_to_slot) = match &args {
            Some(a) if a.len() > 1 => {
                let mut keys: Vec<RecordKey> = Vec::with_capacity(a.len());
                let mut slots: Vec<usize> = Vec::with_capacity(a.len());
                for raw in a {
                    let key = RecordKey::from(raw.as_str());
                    let slot = match keys.iter().position(|k| *k == key) {
                        Some(existing) => existing,
                        None => {
                            keys.push(key);
                            keys.len() - 1
                        }
                    };
                    slots.push(slot);
                }
                (Some(Arc::new(keys)), Some(slots))
            }
            _ => (None, None),
        };
        Self {
            args,
            pop,
            precomputed_keys,
            position_to_slot,
            by: None,
            by_check: ByModulatorCheck::default(),
            child_needs_path: false,
        }
    }

    /// Resolves a lazy `values(key)` reference (`VertexProperty`/`EdgeProperty`) from storage, so
    /// a map-valued property can be read by key; `None` for every other value, which is used as is.
    ///
    /// A `values(key)` result stays an unresolved reference until something forces the fetch
    /// (rule 4's lazy-materialization guidance), so this resolves it once per traverser, not once
    /// per selected key.
    pub(super) fn resolve_lazy(
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
    ) -> Result<Option<TraverserValue<S>>, GraphError> {
        Ok(match value {
            TraverserValue::VertexProperty(prop_name, node_index, _) => context
                .storage
                .vertex_properties(*node_index)?
                .and_then(|p| p.get(prop_name.as_str()).map(ElementProperty::to_logical))
                .map(TraverserValue::from),
            TraverserValue::EdgeProperty(prop_name, edge_index, _) => context
                .storage
                .edge_properties(*edge_index)?
                .and_then(|p| p.get(prop_name.as_str()).map(ElementProperty::to_logical))
                .map(TraverserValue::from),
            _ => None,
        })
    }

    /// Reads `key` from the incoming traverser's value when it is map-shaped - the vanilla Gremlin
    /// idiom behind `elementMap().select("name")` or `valueMap().select("name")`, which needs no
    /// `as()` path label at all. `None` when the value isn't map-shaped or lacks the key, so the
    /// caller falls back to side-effect keys and path labels, as TinkerPop's `Scoping` does.
    fn select_from_map(key: &str, value: &TraverserValue<S>) -> Option<TraverserValue<S>> {
        match value {
            TraverserValue::Object(obj) => {
                let lookup = TraverserValue::Computed(ElementProperty::String(key.into()), None);
                obj.get(&lookup).cloned()
            }
            TraverserValue::Record { keys, values } => {
                let idx = keys.iter().position(|k| k == key)?;
                values.get(idx).cloned()
            }
            _ => None,
        }
    }

    /// The value `select()` reads for `key`, in TinkerPop's `Scoping` order: a key of the
    /// incoming map, then a side-effect key, then a path label. `None` when found nowhere: the
    /// caller filters the traverser, exactly like TinkerPop's `KeyNotFoundException` ->
    /// `EmptyTraverser` (verified against real TinkerPop 3.7/TinkerGraph — see the module-level
    /// doc comment). A label with no declaration anywhere in the query is rejected earlier, at
    /// plan time (`required_labels`/the path analysis's undeclared-label check), so by the time
    /// this runs a miss here only ever means "declared, but not on this traverser".
    pub(super) fn scope_value(
        context: &TraverserContext<'_, S>,
        source: &Traverser<S>,
        map: &TraverserValue<S>,
        key: &str,
        pop: Pop,
    ) -> TraverserResult<Option<TraverserValue<S>>> {
        if let Some(value) = Self::select_from_map(key, map) {
            return Ok(Some(value));
        }
        if let Some(value) = context.side_effect.get(key) {
            // A `subgraph()` bucket reads as its snapshot graph.
            return value.read(&*context.storage).map(Some);
        }
        Ok(match pop {
            Pop::Last => context.paths.by_label(source.path, key),
            Pop::First | Pop::All | Pop::Mixed => {
                context
                    .paths
                    .labelled(context.storage, source.path, key, pop)?
            }
        })
    }

    /// Applies one `by()` of the ring to a selected value. The modulator runs on the selected
    /// object as a fresh traverser, as TinkerPop's `TraversalUtil.produce(start, traversal)`
    /// does; `None` means the `by()` was unproductive.
    ///
    /// A property key can only be read from an element or a map: on any other value (a list,
    /// a path, a scalar, ...) `by("key")` is a `PropertyNotFound` error, as TinkerPop's
    /// `ValueTraversal` fails there, not an unproductive `by()` that would silently filter.
    ///
    /// `child_needs_path` seeds the fresh traverser's path with the projected object itself
    /// (`[object]`) before a `By::Traversal` child runs, exactly as TinkerPop's
    /// `TraversalUtil.apply(object, traversal)` generates it — verified against real TinkerPop
    /// 3.7/TinkerGraph: `select("a").by(__.path())` returns `path[<a's value>]`, a one-element
    /// path, never an empty one. Gated on `child_needs_path` (computed once at plan time by
    /// `analyze_nested_paths`, not per traverser) so a `by()` that never reads a path costs
    /// nothing extra (rule 4).
    pub(super) fn project(
        context: &mut TraverserContext<'_, S>,
        value: TraverserValue<S>,
        by: Option<&By<'g, S>>,
        child_needs_path: bool,
    ) -> TraverserResult<Option<TraverserValue<S>>> {
        match by {
            None => Ok(Some(value)),
            Some(by) => {
                let mut start = Traverser::start(context, value);
                if child_needs_path && matches!(by, By::Traversal(_)) {
                    start.path = context.paths.push(
                        crate::value::PathRef::NONE,
                        start.value.clone(),
                        None,
                    )?;
                }
                let projected =
                    TraverserValue::fetch_traverser_value_by(context, &start, &start.value, by)?;
                if let (None, By::Property(key)) = (&projected, by)
                    && let Some(element_type) = Self::propertyless_kind(&start.value)
                {
                    return Err(Box::new(TraverserError::Value(
                        ValueError::PropertyNotFound {
                            key: key.to_string(),
                            element_type,
                        },
                    )));
                }
                Ok(projected)
            }
        }
    }

    /// The kind of a value that has no properties at all (neither an element nor a map), for the
    /// `by("key")` diagnostic; `None` for an element, a property reference or a map.
    fn propertyless_kind(value: &TraverserValue<S>) -> Option<&'static str> {
        match value {
            TraverserValue::Computed(property, _) => match property {
                ElementProperty::Object(_)
                | ElementProperty::Map(_)
                | ElementProperty::Vertex(_)
                | ElementProperty::Edge(_) => None,
                ElementProperty::Null
                | ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_)
                | ElementProperty::Number(_)
                | ElementProperty::Boolean(_)
                | ElementProperty::Uuid(_)
                | ElementProperty::Temporal(_)
                | ElementProperty::Vector(_) => Some("scalar value"),
                ElementProperty::Array(_) => Some("list"),
            },
            TraverserValue::Array(_) => Some("list"),
            TraverserValue::Path(_) => Some("path object"),
            TraverserValue::PathProperty(_) => Some("path property"),
            TraverserValue::Entry(_) => Some("map entry"),
            TraverserValue::Schema(_) => Some("schema"),
            TraverserValue::Graph(_) => Some("graph"),
            TraverserValue::VertexProperty(..)
            | TraverserValue::EdgeProperty(..)
            | TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::Record { .. }
            | TraverserValue::Object(_) => None,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepSelect<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by = "".to_string();
        if let Some(by_list) = self.by.as_ref() {
            by_list
                .iter()
                .for_each(|b| by.push_str(&crate::traversal::render_by(b)));
        }
        if self.pop == Pop::Last {
            write!(f, "select({}){}", display_option_vec_string(&self.args), by)
        } else {
            let sep = if self.args.as_ref().is_some_and(|a| !a.is_empty()) {
                ", "
            } else {
                ""
            };
            write!(
                f,
                "select(Pop.{}{}{}){}",
                self.pop,
                sep,
                display_option_vec_string(&self.args),
                by
            )
        }
    }
}

/// What a `select(...)` call reads: path labels / map keys, or one [`Column`] of a map.
///
/// Built through [`IntoSelectArgs`]; not constructed directly.
#[non_exhaustive]
pub enum SelectArgs {
    /// `select("a")`, `select(["a", "b"])`: path labels, side-effect keys or map keys.
    Labels(ArgStrings),
    /// `select(Column::Keys)` / `select(Column::Values)`: the keys or values of a map or entry.
    Column(Column),
    /// `select(RecordKey::Label)` (DSL `select(T.label)`), `RecordKey::Id`, `Out`, `In`: the
    /// token entry of a map built by `element_map()` / `value_map(true)`. A Graphersal
    /// extension; a [`RecordKey::Name`] is the same as `select(name)`.
    Token(RecordKey),
}

/// Argument accepted by `select()`: anything convertible into label strings, or a [`Column`].
pub trait IntoSelectArgs {
    /// Converts the argument into the [`SelectArgs`] form `select()` dispatches on.
    fn into_select_args(self) -> SelectArgs;
}

impl<T: Into<ArgStrings>> IntoSelectArgs for T {
    fn into_select_args(self) -> SelectArgs {
        SelectArgs::Labels(self.into())
    }
}

impl IntoSelectArgs for Column {
    fn into_select_args(self) -> SelectArgs {
        SelectArgs::Column(self)
    }
}

impl IntoSelectArgs for RecordKey {
    fn into_select_args(self) -> SelectArgs {
        SelectArgs::Token(self)
    }
}

/// Builds the physical step for `select(args)`: the label/key `StepSelect`, or the
/// `select(Column)` step, which reads nothing from the path.
fn select_step<'g, S: GraphStorage + 'g>(args: SelectArgs, pop: Pop) -> GraphStep<'g, S> {
    match args {
        SelectArgs::Labels(labels) => {
            let labels = match labels {
                ArgStrings::None => None,
                ArgStrings::String(v) => Some(vec![v]),
                ArgStrings::List(v) => Some(v),
            };
            GraphStep::Map("select", Box::new(StepSelect::new(labels, pop)))
        }
        SelectArgs::Column(column) => super::step_select_column::select_column_step(column),
        SelectArgs::Token(RecordKey::Name(name)) => GraphStep::Map(
            "select",
            Box::new(StepSelect::new(Some(vec![name.to_string()]), pop)),
        ),
        SelectArgs::Token(key) => super::step_select_token::select_token_step(key),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `select(labels)` reads path labels (`as()`), side-effect keys or map keys;
    /// `select(Column::Keys)` / `select(Column::Values)` reads the keys/values of a map, or the
    /// key/value of one map entry produced by [`unfold()`](Self::unfold).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let g = lock.traversal();
    /// let ids = g.v("1").as_("a").out(None).select("a").id().to_list().unwrap();
    /// assert_eq!(ids.len(), 3);
    /// let names = lock.traversal().v(None::<()>).group_count().by("name").select(Column::Keys).unfold().to_list().unwrap();
    /// assert_eq!(names.len(), 6);
    /// ```
    pub fn select<T: IntoSelectArgs>(self, args: T) -> Self {
        self.push_step(select_step(args.into_select_args(), Pop::Last))
    }

    /// `select(Pop, labels)`: reads path labels like [`select`](Self::select), choosing which
    /// occurrence of a label that was set several times (for example `as_("a")` inside `repeat()`)
    /// is read: [`Pop::First`], [`Pop::Last`] (what `select` does), [`Pop::All`] (a list of every
    /// occurrence) or [`Pop::Mixed`] (the value for one occurrence, a list for several).
    ///
    /// Only a label read from the path is affected: a key of the traverser's own map and a
    /// side-effect key come first and ignore the pop. Every pop except `Last` records the full
    /// path (no bulk merging upstream), see `book/src/users_guide/select_pop.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let g = lock.traversal();
    /// let oldest = g
    ///     .v("1")
    ///     .as_("a")
    ///     .repeat(__::out(None).as_("a"))
    ///     .times(2)
    ///     .select_pop(Pop::First, "a")
    ///     .id()
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(oldest, vec![ElementProperty::String("1".into()); 2]);
    /// ```
    pub fn select_pop<T: Into<ArgStrings>>(self, pop: Pop, labels: T) -> Self {
        self.push_step(select_step(SelectArgs::Labels(labels.into()), pop))
    }
}

impl __ {
    /// Anonymous `select()`; see [`GraphTraversalSource::select`].
    pub fn select<'g, S: GraphStorage + 'g, T: IntoSelectArgs>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().select(labels)
    }

    /// Anonymous `select(Pop, labels)`; see [`GraphTraversalSource::select_pop`].
    pub fn select_pop<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        pop: Pop,
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().select_pop(pop, labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `select()`; see [`GraphTraversalSource::select`].
    pub fn select<T: IntoSelectArgs>(self, args: T) -> Self {
        self.push_step(select_step(args.into_select_args(), Pop::Last))
    }

    /// Anonymous `select(Pop, labels)`; see [`GraphTraversalSource::select_pop`].
    pub fn select_pop<T: Into<ArgStrings>>(self, pop: Pop, labels: T) -> Self {
        self.push_step(select_step(SelectArgs::Labels(labels.into()), pop))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepSelect<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.by.iter().flatten().filter_map(By::property_key) {
            visit(Some(key), crate::graph::workload::AccessKind::Read);
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        // Only reached without `by()` (`may_filter()` is false), where nothing is filtered.
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "select() dropped a traverser without a by() modulator".to_string(),
            })
        })
    }

    /// `select()` applies its `by()` modulators as TinkerPop's traversal ring: round-robin over the
    /// selected keys, so one `by()` applies to every key and `select("a", "b", "c").by("name")
    /// .by("age")` reads a: name, b: age, c: name. With a single key, the last `by()` is the one
    /// applied (TinkerPop's `SelectOneStep`). A traverser for which any `by()` is unproductive
    /// (such as `by("age")` on a vertex without `age`) is filtered out, TinkerPop 3.6+'s
    /// "productive `by()`" rule for `select()`.
    ///
    /// A key that resolves nowhere (map, side effect, path label) filters the whole traverser —
    /// verified against real TinkerPop 3.7/TinkerGraph: `union(__.as("a").out("knows"),
    /// __.out("created")).select("a")` keeps only the traversers that went through the labeled
    /// branch, and `select("a","b")` drops the whole traverser the moment *either* key is
    /// missing, never returning a partial map. A key nobody ever declares anywhere in the query
    /// is instead rejected before this runs (see `required_labels`), so a miss here is always the
    /// legitimate "declared but not on this path" case.
    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.by_check.check("select")?;
        let args = match &self.args {
            Some(args) if !args.is_empty() => args,
            _ => {
                return Err(Box::new(TraverserError::Value(
                    ValueError::PropertyNotFound {
                        key: "label".to_string(),
                        element_type: "select",
                    },
                )));
            }
        };
        let resolved = Self::resolve_lazy(context, &source.value)?;
        let map = resolved.as_ref().unwrap_or(&source.value);
        let by_list = self.by.as_deref().unwrap_or_default();

        if let [key] = args.as_slice() {
            let Some(value) = Self::scope_value(context, &source, map, key, self.pop)? else {
                return Ok(None);
            };
            return Ok(
                Self::project(context, value, by_list.last(), self.child_needs_path)?.map(
                    |value| Traverser::with_parts(value, source.path, source.bulk, source.sack),
                ),
            );
        }

        let keys = match &self.precomputed_keys {
            Some(keys) => keys,
            None => {
                return Err(Box::new(TraverserError::Value(
                    ValueError::PropertyNotFound {
                        key: "label".to_string(),
                        element_type: "select",
                    },
                )));
            }
        };
        let slots = self.position_to_slot.as_deref().unwrap_or(&[]);
        // A repeated key (`select("a","a")`) writes every one of its ring positions into the
        // same slot; the last write wins, matching a Java `LinkedHashMap.put` on an existing
        // key (verified against real TinkerPop: `select("a","a").by("name").by(T.label)` ->
        // `{a: <label>}`, the second `by()`'s value). Placeholders are always overwritten before
        // being read: every slot in `0..keys.len()` is hit by construction (`StepSelect::new`).
        let mut values: Vec<TraverserValue<S>> = (0..keys.len())
            .map(|_| TraverserValue::Computed(ElementProperty::Null, None))
            .collect();
        for (i, key) in args.iter().enumerate() {
            let Some(value) = Self::scope_value(context, &source, map, key, self.pop)? else {
                return Ok(None);
            };
            let by = if by_list.is_empty() {
                None
            } else {
                by_list.get(i % by_list.len())
            };
            match Self::project(context, value, by, self.child_needs_path)? {
                Some(value) => {
                    let slot = slots.get(i).copied().unwrap_or(i);
                    if let Some(slot_value) = values.get_mut(slot) {
                        *slot_value = value;
                    }
                }
                None => return Ok(None),
            }
        }
        Ok(Some(Traverser::with_parts(
            TraverserValue::Record {
                keys: Arc::clone(keys),
                values,
            },
            source.path,
            source.bulk,
            source.sack,
        )))
    }

    fn may_filter(&self) -> bool {
        // A key that filters (a declared label absent on this traverser) can happen whether or
        // not a `by()` is attached, so `select()` can always drop a traverser once it has at
        // least one key to look up.
        self.args.as_ref().is_some_and(|args| !args.is_empty())
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

    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // Labels are read from the path; a `select()` on a map value reads nothing from it,
        // but that is only known at run time.
        //
        // Every pop except `Last` needs *every* occurrence of a label, which `Labels` mode cannot
        // promise: the backward walk retracts a label at the last `as()` that sets it, so the
        // earlier `as("a")` of `as("a")...as("a").select(Pop.all, "a")` would be told nobody reads
        // `a` and record nothing; and two labels on one position are one node only in `Full` mode
        // (`relabel_last`), which `all` counts as one occurrence. `Full` is exact by construction
        // (design option A; a history-label set inside `Labels` is a possible later refinement).
        if self.pop != Pop::Last && self.args.as_ref().is_some_and(|args| !args.is_empty()) {
            return crate::traversal::optimizer::path_analysis::PathRequirement::Full;
        }
        match &self.args {
            Some(args) => crate::traversal::optimizer::path_analysis::PathRequirement::labels(
                args.iter().map(String::as_str),
            ),
            None => crate::traversal::optimizer::path_analysis::PathRequirement::None,
        }
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // A `by(__...)` runs on the selected object, a fresh traverser: it reads nothing from the
        // incoming path (the labels themselves are `path_requirement()`). What it records is
        // whether the *child itself* needs a path at all (e.g. a nested `path()`/`as()`), which
        // decides whether `project()` has to seed one (rule 13).
        if let Some(by_list) = &mut self.by {
            let entry = By::analyze_all_paths(by_list, analysis);
            self.child_needs_path = !entry.is_none();
        }
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }

    fn required_labels(&self) -> Option<Vec<compact_str::CompactString>> {
        self.args
            .as_ref()
            .map(|args| args.iter().map(|s| CompactString::new(s)).collect())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by
            .iter_mut()
            .flatten()
            .filter_map(By::traversal_mut)
            .collect()
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn a_first() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out(None).select("A").id().to_list(),
            rhai: [
                r#"g.V("1").as("A").out().select("A").id()"#,
                r#"g.v("1").as_("A").out().select("A").id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    /// `select("a").by(prop)` on a label bound to a `fold()`-produced list has no property to
    /// resolve; this must be a normal `PropertyNotFound` diagnostic, not a panic (D1's `Array`
    /// arm in `fetch_property`).
    #[test]
    fn by_property_on_a_labeled_array_is_an_error_not_a_panic() {
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        let err = locked
            .traversal()
            .v(None)
            .fold()
            .as_("a")
            .select("a")
            .by("weight")
            .next()
            .expect_err("a list has no 'weight' property to select");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::PropertyNotFound { .. })
        ));
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_second() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out(None).as_("B").select("A").id().to_list(),
            rhai: [
                r#"g.V("1").as("A").out().as("B").select("A").id()"#,
                r#"g.v("1").as_("A").out().as_("B").select("A").id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn b_second() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out(None).as_("B").select("B").count().to_list(),
            rhai: [
                r#"g.V("1").as("A").out().as("B").select("B").count()"#,
                r#"g.v("1").as_("A").out().as_("B").select("B").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn b_by_name_by_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("4").as_("A").out(None).as_("B").select(vec!["A", "B"]).by("name").by(By::Label).to_list(),
            rhai: [
                r#"g.V("4").as("A").out().as("B").select(["A", "B"]).by("name").by(By::Label)"#,
                r#"g.v("4").as_("A").out().as_("B").select(["A", "B"]).by("name").by(By::Label)"#,
            ],
            assert: |res| {
                let v1 = &res[0];
                match v1 {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj["A"].as_string().unwrap(), &"josh".to_string());
                        assert_eq!(obj["B"].as_string().unwrap(), &"software".to_string());
                    },
                    _ => panic!(),
                }
                let v2 = &res[1];
                match v2 {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj["A"].as_string().unwrap(), &"josh".to_string());
                        assert_eq!(obj["B"].as_string().unwrap(), &"software".to_string());
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn multiply() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().as_("A").out(None).id().as_("B").out(None).id().as_("C").select(vec!["A", "B", "C"]).to_list(),
            rhai: [
                r#"g.V().id().as("A").out().id().as("B").out().id().as("C").select(["A", "B", "C"])"#,
                r#"g.v().id().as_("A").out().id().as_("B").out().id().as_("C").select(["A", "B", "C"])"#,
            ],
            assert: |res| {
                let x = &res[0];
                match x {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.get("A").unwrap(), &ElementProperty::String("1".into()));
                        assert_eq!(obj.get("B").unwrap(), &ElementProperty::String("4".into()));
                        assert_eq!(obj.get("C").unwrap(), &ElementProperty::String("5".into()));
                    },
                    _ => panic!(),
                }
                let x = &res[1];
                match x {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.get("A").unwrap(), &ElementProperty::String("1".into()));
                        assert_eq!(obj.get("B").unwrap(), &ElementProperty::String("4".into()));
                        assert_eq!(obj.get("C").unwrap(), &ElementProperty::String("3".into()));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v("1")
            .as_("A")
            .out(None)
            .select("A")
            .id()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("select("));
    }

    /// `select()` with no keys must render as empty parens, not `select(None)`. `select()` with
    /// no arguments has no valid key to select and fails at execution, but the step-frame text
    /// attached to that error is exactly the rendered plan this bug affects, so it's asserted on
    /// directly rather than via a successful `.profile()`.
    #[test]
    fn profile_no_args_renders_empty_parens() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let err = g
            .v("1")
            .as_("A")
            .out(None)
            .select(None)
            .id()
            .profile()
            .unwrap_err();
        let rendered = format!("{:?}", err);
        assert!(rendered.contains("select()"));
        assert!(!rendered.contains("select(None)"));
    }

    /// Vanilla Gremlin's `select(key)` reads a `Map`-shaped traverser directly by key (the idiom
    /// behind `elementMap().select("name")`), no `as()` path label required. `TraverserValue::
    /// Object` (e.g. a nested `property_json` value) must work the same way.
    #[test]
    #[cfg(feature = "serde")]
    fn select_key_on_object_needs_no_path_label() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .values("meta")
            .select("k")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    /// The same direct-key read must work once `element_map()` has already turned the vertex
    /// into a `TraverserValue::Record`, and must compose across two levels of nesting.
    #[test]
    #[cfg(feature = "serde")]
    fn select_key_on_element_map_record_and_nested_object() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .element_map(None)
            .select("meta")
            .select("k")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    /// `By::JsonPath` reaches `select()`'s `.by()` modulator through the shared
    /// `fetch_traverser_value_by` lever: a path rooted at a stored vertex property must resolve
    /// end-to-end without any intermediate `values()`/`element_map()` step.
    #[test]
    #[cfg(feature = "serde")]
    fn select_by_json_path_off_vertex() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .as_("A")
            .select("A")
            .by(By::JsonPath("meta.k".parse().unwrap()))
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    // --- Task 1 of the 2026-09-26 Gremlin conformance fixes spec: undeclared vs. absent labels,
    // by(__.path()), duplicate keys. Every reference result below was verified against real
    // Apache TinkerPop 3.7 (gremlin-console/TinkerGraph, `tinkerpop-modern` graph) with a small
    // Java probe (`TinkerFactory.createModern()` + the Java Gremlin API), not just reasoned
    // about; see the task's implementation report for the exact probe and its output. Where our
    // engine's own `out()`/`in()` adjacency order differs from TinkerGraph's (a documented,
    // pre-existing deviation — ROADMAP §5.5), results are asserted as a set, not by position.

    /// `select()` can now drop a traverser whether or not it carries a `by()` (a declared-but-
    /// absent label), so the `CountPushdown`-style "skip a pure map before a fused count()"
    /// optimization (`GraphStep::skipped_before_count`, driven by `may_filter()`) must not skip
    /// it: `select("a").count()` after a `union()` where only some traversers set "a" must count
    /// the *filtered* traversers (2), not the whole incoming stream (6). Confirmed with
    /// `graphersal -e '...select("a").count().profile()'`: `select("a")` reports `In: 6, Out: 2`
    /// and `count()` reports `In: 1, Out: 1` with a final count of `2`, not `6`.
    #[cfg(feature = "script")]
    #[test]
    fn select_before_count_is_not_skipped_by_the_count_fusion_optimization() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .union(vec![__::as_("a").out("knows"), __::out("created")])
                .select("a")
                .count()
                .to_list(),
            rhai: [
                r#"g.V().union(__.as("a").out("knows"), __.out("created")).select("a").count()"#,
                r#"g.v().union([__.as_("a").out("knows"), __.out("created")]).select("a").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    /// `g.V().union(__.as("a").out("knows"), __.out("created")).select("a")`: real TinkerPop
    /// returns `[v[1], v[1]]` — the right branch's four `out("created")` traversers never set
    /// "a" and are filtered, not erroring, even though "a" is declared (by the left branch).
    #[cfg(feature = "script")]
    #[test]
    fn union_declared_label_absent_on_one_branch_filters_not_errors() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .union(vec![__::as_("a").out("knows"), __::out("created")])
                .select("a")
                .id()
                .to_list(),
            rhai: [
                r#"g.V().union(__.as("a").out("knows"), __.out("created")).select("a").id()"#,
                r#"g.v().union([__.as_("a").out("knows"), __.out("created")]).select("a").id()"#,
            ],
            assert: |res| {
                assert_eq!(
                    res,
                    &[ElementProperty::String("1".into()), ElementProperty::String("1".into())]
                );
            },
        );
    }

    /// `g.V().optional(__.out("knows").as("a")).select("a")`: real TinkerPop returns
    /// `[v[2], v[4]]` (vadas, josh) — every vertex without a "knows" edge passes through
    /// `optional()` unchanged, without "a", and is filtered by `select("a")`. Asserted as a set:
    /// this engine's own `out()` adjacency order differs from TinkerGraph's (documented,
    /// pre-existing deviation), so marko's two "knows" edges are visited in the other order.
    #[cfg(feature = "script")]
    #[test]
    fn optional_declared_label_absent_when_child_unproductive_filters() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .optional(__::out("knows").as_("a"))
                .select("a")
                .id()
                .to_list(),
            rhai: [
                r#"g.V().optional(__.out("knows").as("a")).select("a").id()"#,
                r#"g.v().optional(__.out("knows").as_("a")).select("a").id()"#,
            ],
            assert: |res| {
                let mut ids: Vec<_> = res.iter().map(|v| v.as_string().unwrap().to_string()).collect();
                ids.sort();
                assert_eq!(ids, vec!["2".to_string(), "4".to_string()]);
            },
        );
    }

    /// `g.V().choose(__.has("age"), __.as("p"), __.identity()).select("p")`: real TinkerPop
    /// returns `[v[1], v[2], v[4], v[6]]`, the four persons (all of which have "age"); the two
    /// software vertices take the `identity()` branch, never set "p", and are filtered.
    ///
    /// This crate has no element-level `has(key)` presence filter yet (a separate, tracked gap —
    /// design spec task 4, out of scope here); `has_label("person")` partitions the modern graph
    /// identically (exactly the persons have "age") and was verified with `graphersal` to reproduce
    /// the exact same reference result, in the same order.
    #[cfg(feature = "script")]
    #[test]
    fn choose_declared_label_absent_on_the_untaken_branch_filters() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .choose(__::has_label("person"), __::as_("p"), __::identity())
                .select("p")
                .id()
                .to_list(),
            rhai: [
                r#"g.V().choose(__.hasLabel("person"), __.as("p"), __.identity()).select("p").id()"#,
                r#"g.v().choose(__.has_label("person"), __.as_("p"), __.identity()).select("p").id()"#,
            ],
            assert: |res| {
                assert_eq!(
                    res,
                    &[
                        ElementProperty::String("1".into()),
                        ElementProperty::String("2".into()),
                        ElementProperty::String("4".into()),
                        ElementProperty::String("6".into()),
                    ]
                );
            },
        );
    }

    /// A label nobody ever declares anywhere in the query (no `as()`, no `aggregate`/`store`/
    /// `tree` key) is a plan-time error, before any traverser executes — never a silent empty
    /// result. Real TinkerPop, lacking this static check, silently returns `[]` here; the project
    /// owner decided this crate should report the query-authoring mistake instead (design spec
    /// task 1). See `crates/graphersal/tests/all/error_diagnostics_tests.rs`'s
    /// `a_missing_select_label_help_names_as_and_path` for the asserted `help()` text.
    #[test]
    fn select_of_a_label_declared_nowhere_is_a_static_error() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let err = lock
            .traversal()
            .v(None)
            .select("nope")
            .to_list()
            .expect_err("\"nope\" is never declared anywhere in the query");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::UndeclaredLabel { .. })
        ));
    }

    /// A key that legitimately might come from a map (`valueMap()`/`elementMap()`/`project()`/...)
    /// or a stored nested-JSON property must never be reported as statically undeclared, even
    /// though no `as()`/side effect ever declares it — the plan cannot prove it's a mistake.
    #[test]
    #[cfg(feature = "serde")]
    fn a_key_reachable_through_a_map_producing_step_is_never_reported_as_undeclared() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        // No `as("k")`/side effect anywhere in this query — only the `property_json`/`values()`
        // pair (a `MAP_PRODUCING_KINDS` step) makes "k" a legitimate map key, not a mistake.
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .values("meta")
            .select("k")
            .to_list();
        assert!(result.is_ok(), "{result:?}");
    }

    /// `select("a","a")`: real TinkerPop's `SelectStep` builds its result in a `LinkedHashMap`
    /// keyed by `selectKeys`, so a repeated key collapses to one entry — verified against real
    /// TinkerPop: `select("a","a").by("name").by(T.label)` -> `{a: "person"}`, the *second*
    /// `by()`'s value (the traversal ring still runs both positions; the later write wins, like
    /// `LinkedHashMap.put` on an existing key).
    #[cfg(feature = "script")]
    #[test]
    fn select_duplicate_key_collapses_to_one_entry_last_by_wins() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .as_("a")
                .select(vec!["a", "a"])
                .by("name")
                .by(By::Label)
                .to_list(),
            rhai: [
                r#"g.V("1").as("a").select(["a", "a"]).by("name").by(By::Label)"#,
                r#"g.v("1").as_("a").select(["a", "a"]).by("name").by(By::Label)"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.len(), 1, "{obj:?}");
                        assert_eq!(obj["a"].as_string().unwrap(), &"person".to_string());
                    }
                    other => panic!("expected a one-key map, got {other:?}"),
                }
            },
        );
    }

    /// `select("a").by(__.path())`: the `by()` child must see the selected object as a
    /// one-element path (`[object]`, TinkerPop's `TraversalUtil.apply(object, traversal)`), not
    /// an empty one. Verified against real TinkerPop: `select("a").by(__.path())` on marko
    /// returns `path[v[1]]`, a path holding exactly the one selected object.
    #[test]
    fn select_by_path_seeds_the_childs_path_with_the_projected_object() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let results = g
            .v("1")
            .as_("a")
            .out("knows")
            .select("a")
            .by(By::Traversal(__::path()))
            .to_list()
            .unwrap();
        assert_eq!(results.len(), 2);
        for path in &results {
            match path {
                ElementProperty::Array(steps) => assert_eq!(
                    steps.len(),
                    1,
                    "expected a one-element path (the projected object only), got {steps:?}"
                ),
                other => panic!("expected a path array, got {other:?}"),
            }
        }
    }

    /// The same `by(__.path())` fix applies to `tree()`: each level wraps its object in a
    /// one-element path, not an empty one. Verified against real TinkerPop:
    /// `g.V(1).out("knows").tree().by(__.path())` returns
    /// `{path[v[1]]={path[v[2]]={}, path[v[4]]={}}}` — every key, root and children alike, is a
    /// one-element path holding just that key's own object.
    ///
    /// `tree()`'s keys are graph objects (here, one-element paths), not strings; the terminal
    /// materializes them as an `ElementProperty::Map` whose keys keep their type, so each key is
    /// an `Array` with exactly one `Vertex`.
    #[test]
    fn tree_by_path_seeds_every_levels_path_with_its_own_object() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v("1")
            .out("knows")
            .tree(None)
            .by(By::Traversal(__::path()))
            .next()
            .unwrap()
            .unwrap();
        fn one_vertex_path(key: &ElementProperty) {
            match key {
                ElementProperty::Array(items) => {
                    assert_eq!(items.len(), 1, "{key:?}");
                    assert!(matches!(items[0], ElementProperty::Vertex(_)), "{key:?}");
                }
                other => panic!("expected a one-element path key, got {other:?}"),
            }
        }
        match result {
            ElementProperty::Map(root) => {
                assert_eq!(root.len(), 1, "{root:?}");
                let (root_key, children) = root.iter().next().unwrap();
                one_vertex_path(root_key);
                match children {
                    ElementProperty::Map(children) => {
                        assert_eq!(children.len(), 2, "{children:?}");
                        for (child_key, _) in children.iter() {
                            one_vertex_path(child_key);
                        }
                    }
                    other => panic!("expected a nested tree map, got {other:?}"),
                }
            }
            other => panic!("expected a tree map, got {other:?}"),
        }
    }
}
