//! The shared core of TinkerPop's list functions `combine()`, `difference()`, `disjunct()`,
//! `intersect()`, `merge()` and `product()` (TinkerPop 3.7+, `ListFunction`): one `Map` step that
//! combines the list a traverser holds with a second list, given as a constant or as the first
//! result of a child traversal. The builders and tests of each step live in its own
//! `step_<name>.rs`; `conjoin()` (a list to a string) is a separate step.
//!
//! The semantics follow the vendored 3.8.2 feature files (`map/Combine.feature`, ...):
//!
//! - the incoming value must be a list (a `fold()`, a `path()`, a list-valued property, a list
//!   constant); `merge()` also takes a map, and then needs a map argument. A `null` or any other
//!   value is an error ([`ValueError::ListFunctionInput`](crate::error::ValueError));
//! - the argument is a list constant or a child traversal run on the incoming traverser (path
//!   included) whose **first** result must be a list (a map for `merge()` of a map); `null`, a
//!   scalar or a traversal without a result is an error;
//! - `combine()` concatenates (duplicates kept), `product()` yields every `[a, b]` pair (in the
//!   order of the incoming list, then the argument), the other four have set semantics: each
//!   element at most once, first occurrence first (`difference` = A \ B, `intersect` = A ∩ B,
//!   `merge` = A ∪ B, `disjunct` = (A \ B) ∪ (B \ A));
//! - elements compare like `dedup()` compares traversers: vertices and edges by identity,
//!   everything else by its materialized value; the kept elements stay what they were (a folded
//!   vertex list keeps its vertex handles);
//! - `merge()` of two maps keeps the entries of the incoming map and adds or overrides them with
//!   the argument's entries.

use std::collections::HashSet;

use fnv::FnvBuildHasher;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::flatbarrier_steps::step_dedup::DedupKey;
use crate::traversal::step::local_scope::{LocalShape, classify, is_null, stored_value};
use crate::*;

/// The set of element keys of one list.
type KeySet<S> = HashSet<DedupKey<S>, FnvBuildHasher>;

/// Which list function a [`StepListFunction`] computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListFunction {
    /// `combine()`: A followed by B, duplicates kept.
    Combine,
    /// `difference()`: the distinct elements of A that are not in B.
    Difference,
    /// `disjunct()`: the distinct elements in exactly one of A and B.
    Disjunct,
    /// `intersect()`: the distinct elements of A that are also in B.
    Intersect,
    /// `merge()`: the distinct elements of A and B; for two maps, A's entries overridden by B's.
    Merge,
    /// `product()`: every `[a, b]` pair, `a` from A, `b` from B.
    Product,
}

impl ListFunction {
    /// The step's name, as `.profile()` and the errors spell it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            ListFunction::Combine => "combine",
            ListFunction::Difference => "difference",
            ListFunction::Disjunct => "disjunct",
            ListFunction::Intersect => "intersect",
            ListFunction::Merge => "merge",
            ListFunction::Product => "product",
        }
    }

    /// Whether the function compares elements (set semantics).
    fn uses_keys(self) -> bool {
        match self {
            ListFunction::Combine | ListFunction::Product => false,
            ListFunction::Difference
            | ListFunction::Disjunct
            | ListFunction::Intersect
            | ListFunction::Merge => true,
        }
    }
}

/// A constant list argument with the keys of its elements, computed once when the step is built.
struct ConstList<S: GraphStorage> {
    items: Vec<ElementProperty>,
    keys: Vec<DedupKey<S>>,
    set: KeySet<S>,
}

impl<S: GraphStorage> Clone for ConstList<S> {
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            keys: self.keys.clone(),
            set: self.set.clone(),
        }
    }
}

/// The second operand of a list function.
enum Operand<'g, S: GraphStorage + 'g> {
    /// A list constant.
    List(ConstList<S>),
    /// A map constant (only `merge()` of a map takes one).
    Map(ElementProperty),
    /// A constant that is neither a list nor a map (`null`, a number, ...): rejected before
    /// execution by [`GraphStepMap::plan_check`].
    Invalid(ElementProperty),
    /// A child traversal; its first result is the operand.
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for Operand<'g, S> {
    fn clone(&self) -> Self {
        match self {
            Operand::List(list) => Operand::List(list.clone()),
            Operand::Map(map) => Operand::Map(map.clone()),
            Operand::Invalid(value) => Operand::Invalid(value.clone()),
            Operand::Traversal(traversal) => Operand::Traversal(traversal.clone()),
        }
    }
}

/// The physical step of `combine()`, `difference()`, `disjunct()`, `intersect()`, `merge()` and
/// `product()`; see the module documentation for the semantics.
pub(super) struct StepListFunction<'g, S: GraphStorage + 'g> {
    kind: ListFunction,
    operand: Operand<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepListFunction<'g, S> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            operand: self.operand.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepListFunction<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.kind.name();
        match &self.operand {
            Operand::List(list) => write!(
                f,
                "{name}({})",
                constant_text(&ElementProperty::Array(list.items.clone()))
            ),
            Operand::Map(value) | Operand::Invalid(value) => {
                write!(f, "{name}({})", constant_text(value))
            }
            Operand::Traversal(traversal) => write!(f, "{name}({traversal})"),
        }
    }
}

/// The text of a constant argument in the plan: its JSON form through the physical codec.
fn constant_text(value: &ElementProperty) -> String {
    crate::value::literal::rhai_literal(value)
}

/// The argument of a list function builder: a constant or a child traversal.
pub(crate) enum ListArg<'g, S: GraphStorage + 'g> {
    /// A constant (a list; a map for `merge()` of a map).
    Value(ElementProperty),
    /// A child traversal whose first result is the operand.
    Traversal(AnonymousTraversal<'g, S>),
}

/// Builds the physical step of a list function.
pub(crate) fn list_function_step<'g, S: GraphStorage + 'g>(
    kind: ListFunction,
    arg: ListArg<'g, S>,
) -> GraphStep<'g, S> {
    let operand = match arg {
        ListArg::Traversal(traversal) => Operand::Traversal(traversal),
        ListArg::Value(ElementProperty::Array(items)) => {
            let keys: Vec<DedupKey<S>> = items.iter().cloned().map(DedupKey::Value).collect();
            let set = keys.iter().cloned().collect();
            Operand::List(ConstList { items, keys, set })
        }
        ListArg::Value(map @ (ElementProperty::Object(_) | ElementProperty::Map(_))) => {
            Operand::Map(map)
        }
        ListArg::Value(other) => Operand::Invalid(other),
    };
    GraphStep::Map(kind.name(), Box::new(StepListFunction { kind, operand }))
}

#[cfg(feature = "script")]
impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// One list function with its argument; the DSL lowering's single entry for all six.
    pub(crate) fn list_function(self, kind: ListFunction, arg: ListArg<'g, S>) -> Self {
        self.push_step(list_function_step(kind, arg))
    }
}

/// Generates the three builders (`GraphTraversalSource`, `__`, `AnonymousTraversal`) of one list
/// function, each in a constant form and a `_traversal` form.
macro_rules! list_function_builders {
    ($kind:expr, $name:ident, $name_t:ident, $doc:literal, $example:literal) => {
        impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
            #[doc = $doc]
            ///
            /// The argument here is a constant (`vec!["a", "b"]`, an `ElementProperty`); see the
            /// `_traversal` form for a child traversal. Elements compare like `dedup()` compares
            /// traversers (vertices and edges by identity). A `null` or non-list incoming value
            /// or argument is a `ValueError::ListFunctionInput`. Details:
            /// `book/src/traversal/steps/list_functions.md`.
            ///
            /// ```rust
            /// use graphersal::prelude::*;
            ///
            /// let graph = GraphSource::tinkerpop_modern();
            /// let lock = graph.read();
            /// let g = lock.traversal();
            #[doc = $example]
            /// ```
            pub fn $name(self, list: impl Into<ElementProperty>) -> Self {
                self.push_step(list_function_step($kind, ListArg::Value(list.into())))
            }

            #[doc = concat!("`", stringify!($name), "(traversal)`: like [`", stringify!($name), "`](Self::", stringify!($name), "), but the second operand is the first result of `traversal`, run on the incoming traverser (path included); it must be a list (end the child with `fold()`). A traversal without a result is an error.")]
            pub fn $name_t(self, traversal: AnonymousTraversal<'g, S>) -> Self {
                self.push_step(list_function_step($kind, ListArg::Traversal(traversal)))
            }
        }

        impl __ {
            #[doc = concat!("Anonymous `", stringify!($name), "()`; see [`GraphTraversalSource::", stringify!($name), "`].")]
            pub fn $name<'g, S: GraphStorage + 'g>(
                list: impl Into<ElementProperty>,
            ) -> AnonymousTraversal<'g, S> {
                AnonymousTraversal::new().$name(list)
            }

            #[doc = concat!("Anonymous `", stringify!($name), "(traversal)`; see [`GraphTraversalSource::", stringify!($name_t), "`].")]
            pub fn $name_t<'g, S: GraphStorage + 'g>(
                traversal: AnonymousTraversal<'g, S>,
            ) -> AnonymousTraversal<'g, S> {
                AnonymousTraversal::new().$name_t(traversal)
            }
        }

        impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
            #[doc = concat!("`", stringify!($name), "()`; see [`GraphTraversalSource::", stringify!($name), "`].")]
            pub fn $name(self, list: impl Into<ElementProperty>) -> Self {
                self.push_step(list_function_step($kind, ListArg::Value(list.into())))
            }

            #[doc = concat!("`", stringify!($name), "(traversal)`; see [`GraphTraversalSource::", stringify!($name_t), "`].")]
            pub fn $name_t(self, traversal: AnonymousTraversal<'g, S>) -> Self {
                self.push_step(list_function_step($kind, ListArg::Traversal(traversal)))
            }
        }
    };
}
pub(super) use list_function_builders;

/// The error of an input a list function cannot take.
pub(crate) fn input_error(
    step: &'static str,
    operand: &'static str,
    expected: &'static str,
    actual: String,
) -> Box<TraverserError> {
    Box::new(TraverserError::Value(ValueError::ListFunctionInput {
        step,
        operand,
        expected,
        actual,
    }))
}

/// The kind of a value, for a diagnostic: `null`, the logical type of a computed or stored value,
/// otherwise the stream kind (`vertex`, `path`, ...). A value the storage cannot read is
/// [`UNREADABLE_DETAIL`](crate::error::UNREADABLE_DETAIL): the step's own error is the one to
/// report, only without the kind.
pub(crate) fn kind_name<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> String {
    read_kind_name(context, value).unwrap_or_else(|error| {
        crate::error::unreadable_detail("the kind of a value", &error);
        crate::error::UNREADABLE_DETAIL.to_string()
    })
}

/// [`kind_name`], `Err` when the storage cannot read the value.
fn read_kind_name<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> Result<String, crate::error::GraphError> {
    if is_null(context, value)? {
        return Ok("null".to_string());
    }
    Ok(match value {
        TraverserValue::Computed(property, _) => crate::graph::schema::value_type_name(property),
        TraverserValue::VertexProperty(..) | TraverserValue::EdgeProperty(..) => {
            match stored_value(context, value)? {
                Some(stored) => crate::graph::schema::value_type_name(stored),
                None => value.type_name().to_string(),
            }
        }
        TraverserValue::Array(_) => "array".to_string(),
        TraverserValue::Object(_) | TraverserValue::Record { .. } => "map".to_string(),
        _ => value.type_name().to_string(),
    })
}

/// The collection kind of a value, read without taking it apart.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    List,
    Map,
    Other,
}

/// Classifies a value by reference: a list (`fold()`, `path()`, a list constant or property), a
/// map (`project()`, `elementMap()`, a map constant or property) or anything else.
pub(crate) fn kind_of<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> Result<Kind, crate::error::GraphError> {
    let of_property = |property: &ElementProperty| match property {
        ElementProperty::Array(_) => Kind::List,
        ElementProperty::Object(_) | ElementProperty::Map(_) => Kind::Map,
        ElementProperty::Null
        | ElementProperty::String(_)
        | ElementProperty::Compressed(_) | ElementProperty::Deduped(_)
        | ElementProperty::Number(_)
        | ElementProperty::Boolean(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        // A vector is one value, not a list.
        | ElementProperty::Vector(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_) => Kind::Other,
    };
    Ok(match value {
        TraverserValue::Array(_) | TraverserValue::Path(_) => Kind::List,
        TraverserValue::Object(_) | TraverserValue::Record { .. } => Kind::Map,
        TraverserValue::Computed(property, _) => of_property(property),
        TraverserValue::VertexProperty(..) | TraverserValue::EdgeProperty(..) => {
            stored_value(context, value)?.map_or(Kind::Other, of_property)
        }
        TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => Kind::Other,
    })
}

/// The elements of a value [`kind_of`] reported as a list (a path's objects included).
pub(crate) fn list_items<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> TraverserResult<Vec<TraverserValue<S>>> {
    Ok(match classify(context, value)? {
        LocalShape::List(items) => items,
        LocalShape::Path(path) => path.steps.into_iter().map(|step| step.value).collect(),
        LocalShape::Map(_) | LocalShape::Scalar(_) => Vec::new(),
    })
}

/// The entries of a value [`kind_of`] reported as a map.
fn map_entries<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> TraverserResult<Vec<(TraverserValue<S>, TraverserValue<S>)>> {
    Ok(match classify(context, value)? {
        LocalShape::Map(entries) => entries,
        LocalShape::List(_) | LocalShape::Path(_) | LocalShape::Scalar(_) => Vec::new(),
    })
}

/// The resolved second operand of one traverser.
enum Right<'a, S: GraphStorage> {
    /// The step's constant list.
    Const(&'a ConstList<S>),
    /// A list computed by the child traversal, with the keys of its elements when needed.
    Owned {
        items: Vec<TraverserValue<S>>,
        keys: Vec<DedupKey<S>>,
    },
}

impl<S: GraphStorage> Right<'_, S> {
    fn len(&self) -> usize {
        match self {
            Right::Const(list) => list.items.len(),
            Right::Owned { items, .. } => items.len(),
        }
    }

    /// Element `i`, as a stream value.
    fn item(&self, i: usize) -> TraverserValue<S> {
        match self {
            Right::Const(list) => TraverserValue::from(list.items[i].clone()),
            Right::Owned { items, .. } => items[i].clone(),
        }
    }

    fn key(&self, i: usize) -> &DedupKey<S> {
        match self {
            Right::Const(list) => &list.keys[i],
            Right::Owned { keys, .. } => &keys[i],
        }
    }

    /// Every element, moved out of an owned list.
    fn into_items(self) -> Vec<TraverserValue<S>> {
        match self {
            Right::Const(list) => list
                .items
                .iter()
                .cloned()
                .map(TraverserValue::from)
                .collect(),
            Right::Owned { items, .. } => items,
        }
    }

    /// Whether an element with `key` is in the list.
    fn contains(
        &self,
        key: &DedupKey<S>,
        owned_set: &HashSet<&DedupKey<S>, FnvBuildHasher>,
    ) -> bool {
        match self {
            Right::Const(list) => list.set.contains(key),
            Right::Owned { .. } => owned_set.contains(key),
        }
    }
}

/// The resolved second operand: a list or a map.
enum Resolved<'a, S: GraphStorage> {
    List(Right<'a, S>),
    Map(Vec<(TraverserValue<S>, TraverserValue<S>)>),
}

impl<'g, S: GraphStorage + 'g> StepListFunction<'g, S> {
    /// Resolves the second operand for `source`. `incoming` is the kind of the incoming value
    /// (a list or, for `merge()`, a map): it decides which kind the operand must have.
    fn resolve(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
        incoming: Kind,
    ) -> TraverserResult<Resolved<'_, S>> {
        let step = self.kind.name();
        let expected = if incoming == Kind::Map { "map" } else { "list" };
        match &self.operand {
            Operand::List(list) if incoming == Kind::List => Ok(Resolved::List(Right::Const(list))),
            Operand::Map(map) if incoming == Kind::Map => Ok(Resolved::Map(map_entries(
                context,
                TraverserValue::Computed(map.clone(), None),
            )?)),
            Operand::List(_) => Err(input_error(step, "argument", expected, "array".to_string())),
            Operand::Map(_) => Err(input_error(step, "argument", expected, "map".to_string())),
            Operand::Invalid(value) => Err(input_error(
                step,
                "argument",
                expected,
                kind_name(context, &TraverserValue::Computed(value.clone(), None)),
            )),
            Operand::Traversal(traversal) => {
                let Some(first) = traversal.first(context, source)? else {
                    return Err(input_error(
                        step,
                        "traversal argument",
                        expected,
                        "no result (the traversal yielded nothing)".to_string(),
                    ));
                };
                let kind = kind_of(context, &first.value)?;
                if kind != incoming {
                    return Err(input_error(
                        step,
                        "traversal argument",
                        expected,
                        kind_name(context, &first.value),
                    ));
                }
                if incoming == Kind::Map {
                    return Ok(Resolved::Map(map_entries(context, first.value)?));
                }
                let items = list_items(context, first.value)?;
                let keys = if self.kind.uses_keys() {
                    items
                        .iter()
                        .map(|item| DedupKey::of(context, item))
                        .collect::<TraverserResult<Vec<_>>>()?
                } else {
                    Vec::new()
                };
                Ok(Resolved::List(Right::Owned { items, keys }))
            }
        }
    }

    /// Combines the incoming list `left` with `right`.
    fn combine_lists(
        &self,
        context: &TraverserContext<'_, S>,
        left: Vec<TraverserValue<S>>,
        right: Right<'_, S>,
    ) -> TraverserResult<Vec<TraverserValue<S>>> {
        match self.kind {
            ListFunction::Combine => {
                let mut out = left;
                out.extend(right.into_items());
                return Ok(out);
            }
            ListFunction::Product => {
                let mut out =
                    crate::traversal::capacity::vec_capped(left.len().saturating_mul(right.len()));
                for a in &left {
                    for i in 0..right.len() {
                        out.push(TraverserValue::Array(vec![a.clone(), right.item(i)]));
                    }
                }
                return Ok(out);
            }
            ListFunction::Difference
            | ListFunction::Disjunct
            | ListFunction::Intersect
            | ListFunction::Merge => {}
        }
        let owned_set: HashSet<&DedupKey<S>, FnvBuildHasher> = match &right {
            Right::Const(_) => HashSet::default(),
            Right::Owned { keys, .. } => keys.iter().collect(),
        };
        let mut seen: KeySet<S> = HashSet::with_capacity_and_hasher(left.len(), Default::default());
        let mut out = Vec::with_capacity(left.len());
        for item in left {
            let key = DedupKey::of(context, &item)?;
            let in_right = right.contains(&key, &owned_set);
            let keep = match self.kind {
                ListFunction::Difference | ListFunction::Disjunct => !in_right,
                ListFunction::Intersect => in_right,
                ListFunction::Merge => true,
                ListFunction::Combine | ListFunction::Product => true,
            };
            if seen.insert(key) && keep {
                out.push(item);
            }
        }
        if matches!(self.kind, ListFunction::Merge | ListFunction::Disjunct) {
            // `seen` now holds every key of the incoming list.
            for i in 0..right.len() {
                let key = right.key(i);
                if !seen.contains(key) {
                    seen.insert(key.clone());
                    out.push(right.item(i));
                }
            }
        }
        Ok(out)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepListFunction<'g, S> {
    /// Never called by the executor: `may_filter()` is true (so a downstream fused `count()`
    /// still runs the step and raises its errors), and it calls `process_filter`.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: format!("{}() never drops a traverser", self.kind.name()),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        let step = self.kind.name();
        let incoming = kind_of(context, &source.value)?;
        let accepted =
            incoming == Kind::List || (incoming == Kind::Map && self.kind == ListFunction::Merge);
        if !accepted {
            let expected = if self.kind == ListFunction::Merge {
                "list or map"
            } else {
                "list"
            };
            return Err(input_error(
                step,
                "incoming value",
                expected,
                kind_name(context, &source.value),
            ));
        }
        // The child traversal runs on the whole incoming traverser before its value is taken
        // apart.
        let right = self.resolve(context, &source, incoming)?;
        let Traverser {
            value,
            path,
            bulk,
            sack,
        } = source;
        let value = match right {
            Resolved::List(right) => {
                let left = list_items(context, value)?;
                TraverserValue::Array(self.combine_lists(context, left, right)?)
            }
            Resolved::Map(entries) => {
                let left = map_entries(context, value)?;
                let mut merged: StreamMap<S> = StreamMap::with_capacity(left.len() + entries.len());
                merged.extend(left);
                merged.extend(entries);
                TraverserValue::Object(merged)
            }
        };
        Ok(Some(Traverser::with_parts(value, path, bulk, sack)))
    }

    fn may_filter(&self) -> bool {
        true
    }

    /// Builds a new list (or map).
    fn materializes(&self) -> bool {
        true
    }

    fn plan_check(&self) -> TraverserResult<()> {
        if let Operand::Invalid(value) = &self.operand {
            let actual = match value {
                ElementProperty::Null => "null".to_string(),
                other => crate::graph::schema::value_type_name(other),
            };
            let expected = if self.kind == ListFunction::Merge {
                "list or map"
            } else {
                "list"
            };
            return Err(input_error(self.kind.name(), "argument", expected, actual));
        }
        Ok(())
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // The child traversal runs on the incoming traverser, path included; its output does
        // not continue the path.
        match &mut self.operand {
            Operand::Traversal(traversal) => {
                traversal.analyze_paths(analysis, PathRequirement::None)
            }
            Operand::List(_) | Operand::Map(_) | Operand::Invalid(_) => PathRequirement::None,
        }
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        match &mut self.operand {
            Operand::Traversal(traversal) => vec![traversal],
            Operand::List(_) | Operand::Map(_) | Operand::Invalid(_) => Vec::new(),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
