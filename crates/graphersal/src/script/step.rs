//! The DSL step AST: `Step`, one variant per Rhai-built step, recorded by the builder methods of
//! `ScriptGraphTraversalSource` and lowered to engine steps by `convert.rs`.

use super::*;

/// The argument of a DSL `mergeV(..)` / `option(Merge.x, ..)`: a map's entries (with the
/// per-entry `Cardinality.x(v)` cardinality; `()` is no entries), a traversal, or any other value,
/// which the step rejects before execution.
#[derive(Debug, Clone)]
pub(super) enum ScriptMergeArg {
    Entries(Vec<PropertyEntry>),
    Traversal(ScriptGraphTraversalSource),
    Value(ElementProperty),
}

/// The argument of a DSL list function (`combine`, `difference`, `disjunct`, `intersect`,
/// `merge`, `product`): a constant (a list; a map for `merge`; anything else is rejected before
/// execution) or a child traversal whose first result is the operand.
#[derive(Debug, Clone)]
pub(super) enum ScriptListArg {
    Value(ElementProperty),
    Traversal(ScriptGraphTraversalSource),
}

/// The query vector of a DSL `similarity()` / `nearest()`: a constant vector (a list of numbers
/// is made one when the step is built), a child traversal whose first result is the query, or a
/// text embedded by the model the attached schema declares for the property.
#[derive(Debug, Clone)]
pub(super) enum ScriptVectorQuery {
    Value(crate::value::Vector),
    Traversal(ScriptGraphTraversalSource),
    /// A text, embedded with the embedder of the model the schema declares for the property.
    Text(String),
}

/// Which token `property(T.x, v)` names in the DSL.
#[derive(Debug, Clone, Copy)]
pub(super) enum PropertyTokenKey {
    Id,
    Label,
}

/// A step that configures the traversal source `g` instead of adding a step to the pipeline.
/// Gremlin has these only on `g`; the DSL records them on `__` too, so the lowering applies them
/// to the root source and turns one inside a child traversal into
/// [`crate::error::TraverserError::SourceStepInChild`] (see `convert.rs`).
#[derive(Debug, Clone)]
pub(super) enum SourceStep {
    /// `withSack(initial)`.
    Sack(ElementProperty),
    /// `withSack(initial, Operator)`: the second argument is the merge operator.
    SackMerge(ElementProperty, Operator),
    /// `withSideEffect(name, initial)`.
    SideEffect(CompactString, ElementProperty),
    /// `withSideEffect(name, initial, Operator)`: a side effect that `aggregate`/`store` reduce.
    SideEffectReduced(CompactString, ElementProperty, Operator),
    /// `withSideEffect(name, Set.of(..))`: a side effect declared as a set.
    SideEffectSet(CompactString, Vec<ElementProperty>),
    /// `withSideEffect(name, graph)`: the explicit target graph of `subgraph(name)`.
    SideEffectGraph(CompactString, ScriptGraphTarget),
    /// `withPath()`: changes nothing on `g` (the path analysis records what the query reads).
    Path,
}

impl SourceStep {
    /// The step's name as errors spell steps (snake_case).
    pub(super) fn name(&self) -> &'static str {
        match self {
            SourceStep::Sack(_) | SourceStep::SackMerge(..) => "with_sack",
            SourceStep::SideEffect(..)
            | SourceStep::SideEffectReduced(..)
            | SourceStep::SideEffectSet(..)
            | SourceStep::SideEffectGraph(..) => "with_side_effect",
            SourceStep::Path => "with_path",
        }
    }

    /// Applies the step to the root traversal source.
    pub(super) fn apply<'g, S: GraphStorage + 'g>(
        &self,
        graph: GraphTraversalSource<'g, S>,
    ) -> GraphTraversalSource<'g, S> {
        match self {
            SourceStep::Sack(initial) => graph.with_sack(initial.clone()),
            SourceStep::SackMerge(initial, merge) => graph.with_sack_merge(initial.clone(), *merge),
            SourceStep::SideEffect(name, value) => {
                graph.with_side_effect(name.as_str(), value.clone())
            }
            SourceStep::SideEffectReduced(name, value, operator) => {
                graph.with_side_effect_reduced(name.as_str(), value.clone(), *operator)
            }
            SourceStep::SideEffectSet(name, items) => {
                graph.with_side_effect_set(name.as_str(), items.iter().cloned())
            }
            SourceStep::SideEffectGraph(name, target) => {
                graph.with_side_effect_graph(name.as_str(), target.graph.clone())
            }
            SourceStep::Path => graph,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum Step {
    V(Vec<CompactString>),
    E(Vec<CompactString>),
    Count,
    Label,
    /// `labels()` (D4): the plural counterpart to `Label`, exposing a vertex's full label set.
    Labels,
    Id,
    Identity,
    /// `sack()`: reads the traverser's sack (`null` without `withSack`).
    Sack,
    InferSchema,
    Inject(Vec<ElementProperty>),
    Cap(Vec<CompactString>),
    Fold,
    Unfold,
    Group,
    GroupCount,
    /// `group("key")`: the side-effect form of `group()`.
    GroupSideEffect(CompactString),
    /// `groupCount("key")`: the side-effect form of `groupCount()`.
    GroupCountSideEffect(CompactString),
    /// `order()`, part 2 of the `order()` series: the step itself with no `.by()` or a single
    /// `.by()` clause, attached afterwards via `Step::By`.
    Order,
    By(ArgBy),
    Max,
    Mean,
    Median,
    Min,
    Sum,
    /// `count(Scope)`, `sum(Scope)`, `min(Scope)`, `max(Scope)`, `mean(Scope)`.
    CountScoped(TraversalScope),
    SumScoped(TraversalScope),
    MinScoped(TraversalScope),
    MaxScoped(TraversalScope),
    MeanScoped(TraversalScope),
    /// `limit(Scope, n)`, `skip(Scope, n)`, `tail(Scope, n)`, `range(Scope, low, high)`. The
    /// counts stay `i64`, so a negative one reaches the step for a clear `InvalidRange`.
    LimitScoped(TraversalScope, i64),
    SkipScoped(TraversalScope, i64),
    TailScoped(TraversalScope, i64),
    RangeScoped(TraversalScope, i64, i64),
    /// `order(Scope)`.
    OrderScoped(TraversalScope),
    /// `dedup(Scope)` and `dedup(Scope, label, ...)`; the labels are ignored in local scope.
    DedupScoped(TraversalScope, Vec<String>),
    /// The string steps with a `Scope`: `toUpper(Scope)`, `toLower(Scope)`, `trim(Scope)`,
    /// `lTrim(Scope)`, `rTrim(Scope)`, `length(Scope)`, `asString(Scope)`.
    ToUpperScoped(TraversalScope),
    ToLowerScoped(TraversalScope),
    TrimScoped(TraversalScope),
    LTrimScoped(TraversalScope),
    RTrimScoped(TraversalScope),
    LengthScoped(TraversalScope),
    AsStringScoped(TraversalScope),
    /// `replace(from, to)` and `replace(Scope, from, to)`; `None` is a `null` argument.
    ReplaceScoped(TraversalScope, Option<CompactString>, Option<CompactString>),
    /// `concat(string...)`: the constant arguments (strings or `null`; anything else is rejected
    /// before execution).
    ConcatStrings(Vec<ElementProperty>),
    /// `concat(traversal...)`.
    ConcatTraversals(Vec<ScriptGraphTraversalSource>),
    /// `format(pattern)`.
    Format(String),
    /// `split(separator)` and `split(Scope, separator)`; `None` is a `null` separator.
    Split(TraversalScope, Option<CompactString>),
    /// `reverse()`.
    Reverse,
    /// `index()`: `[element, position]` pairs.
    Index,
    /// The list functions `combine`, `difference`, `disjunct`, `intersect`, `merge`, `product`
    /// (one variant: they share the argument forms and the engine step).
    ListFunction(ListFunction, ScriptListArg),
    /// `conjoin(delimiter)`.
    Conjoin(CompactString),
    /// `substring(start)`, `substring(Scope, start)` and `substring(Scope, start, end)`.
    SubstringScoped(TraversalScope, i32, Option<i32>),
    And(Vec<ScriptGraphTraversalSource>),
    /// `and()` with no argument: the infix marker of `a.and().b`, folded into `And` before
    /// lowering (`connective.rs`).
    AndInfix,
    /// `or()` with no argument: the infix marker of `a.or().b`, folded into `Or` before
    /// lowering (`connective.rs`).
    OrInfix,
    Coin(f32),
    /// `fail()` / `fail(message)`.
    Fail(Option<CompactString>),
    /// `sample(n)` / `sample(Scope, n)`; a negative `n` samples nothing.
    Sample(TraversalScope, usize),
    HasId(Vec<CompactString>),
    HasKey(Vec<CompactString>),
    HasLabel(Vec<CompactString>),
    /// `hasLabel()` without arguments: keeps the elements that have a label at all.
    HasAnyLabel,
    /// `hasId(P)`: a predicate other than `eq`/`within` over the element id.
    HasIdP(ArgP),
    /// `hasKey(P)`: a predicate over a property key.
    HasKeyP(ArgP),
    /// `hasLabel(P)`: a predicate over the element label.
    HasLabelP(ArgP),
    HasP(PropKey, ArgP),
    HasValue(ElementProperty),
    /// `hasValue(v1, v2, ...)` (D1 of the `has_value` TinkerPop-compliance fix): the vanilla
    /// `hasValue(Object, Object...)` multi-value OR overload.
    HasValueAny(Vec<ElementProperty>),
    /// `hasValue(P<V>)` (D1): the vanilla `hasValue(P<Object>)` predicate overload.
    HasValueP(ArgP),
    Has(PropKey, ElementProperty),
    /// `has(key, traversal)`: TinkerPop's `has(String, Traversal)`, i.e.
    /// `filter(values(key).traversal)`.
    HasTraversal(crate::value::HasKey, ScriptGraphTraversalSource),
    /// `has(key)` (element-level existence: keeps the element/property carrying `key`,
    /// regardless of value) — distinct from `HasKey`/`hasKey()`, which in vanilla TinkerPop is a
    /// property-level filter used after `properties()`. See `step_has_property.rs`'s doc comment.
    HasProperty(PropKey),
    /// `hasNot(key)`: the negation of `HasProperty` — keeps the element/property that does not
    /// carry `key`.
    HasNot(PropKey),
    Is(ArgP),
    /// `discard()`: filters every traverser (TinkerPop 3.8's rename of the old `none()`).
    Discard,
    Not(ScriptGraphTraversalSource),
    /// `filter(traversal)`: keeps a traverser when the child yields a result; `as()` in the child is a plain label.
    FilterT(ScriptGraphTraversalSource),
    Or(Vec<ScriptGraphTraversalSource>),
    All(ArgP),
    Any(ArgP),
    /// `none(P)`: keeps a list none of whose elements matches.
    NoneP(ArgP),
    Dedup,
    /// `barrier()` / `barrier(n)`: merge equal traversers; `n` is validated (1 to `u32::MAX`).
    Barrier(Option<u32>),
    Limit(usize),
    Skip(usize),
    Tail(usize),
    /// `range(low, high)`: `[low, high)` stream slicing. `high == -1` means "no upper bound",
    /// per TinkerPop's own sentinel. Kept as `i64` (not `usize`) so a negative `low` or a
    /// `high` below `-1` reaches `StepRange::process` for a clear `TraverserError::InvalidRange`
    /// instead of silently wrapping via `as usize`.
    Range(i64, i64),
    Both(Vec<CompactString>),
    BothE(Vec<CompactString>),
    Coalesce(Vec<ScriptGraphTraversalSource>),
    Union(Vec<ScriptGraphTraversalSource>),
    Element,
    InE(Vec<CompactString>),
    InV,
    In(Vec<CompactString>),
    Optional(ScriptGraphTraversalSource),
    /// `local(traversal)`: runs `traversal` once per incoming traverser, in isolation.
    Local(ScriptGraphTraversalSource),
    /// `map(traversal)`: the child's first result.
    MapT(ScriptGraphTraversalSource),
    /// `flatMap(traversal)`: every result of the child.
    FlatMapT(ScriptGraphTraversalSource),
    SideEffect(ScriptGraphTraversalSource),
    OutE(Vec<CompactString>),
    OutV,
    Out(Vec<CompactString>),
    /// `bothV()`: both incident vertices of an edge, unconditionally.
    BothV,
    /// `otherV()`: the vertex opposite the one the traversal came from.
    OtherV,
    Constant(ElementProperty),
    Length,
    LTrim,
    Math(CompactString),
    Path,
    SimplePath,
    CyclicPath,
    RTrim,
    Select(Vec<CompactString>),
    SelectColumn(Column),
    /// `select(T.id)` / `select(T.label)` / `select(Direction.OUT)` / `select(Direction.IN)`.
    SelectToken(crate::value::RecordKey),
    /// `select(Pop.x, "a", ...)`: path labels read with a [`Pop`].
    SelectPop(Pop, Vec<CompactString>),
    /// `select(Pop.x, <traversal>)` / `select(<traversal>)` (`Pop::Last`): the key is the first
    /// result of the traversal.
    SelectTraversal(Pop, ScriptGraphTraversalSource),
    Substring(i32, i32),
    ToLower,
    ToUpper,
    Trim,
    AsString(CastPolicy),
    AsBool(CastPolicy),
    AsNumber(Option<GType>, CastPolicy),
    Cast(GType, CastPolicy),
    /// `as_date()` / `as_date(GType.X)`.
    AsDate(Option<GType>),
    /// `as_duration()`.
    AsDuration,
    /// `as_vector()`.
    AsVector,
    /// `quantize(kind)`.
    Quantize(crate::value::VectorElement),
    /// `similarity([key,] q[, metric])`: no key compares the incoming vector value.
    Similarity(
        Option<PropKey>,
        ScriptVectorQuery,
        Option<crate::value::VectorMetric>,
    ),
    /// `nearest(key, q, k[, #{metric}])`.
    Nearest(
        PropKey,
        ScriptVectorQuery,
        usize,
        Option<crate::value::VectorMetric>,
    ),
    /// `vector_mean()`.
    VectorMean,
    /// `embed([key][, #{embedder}])`: no key embeds the incoming string.
    Embed(Option<PropKey>, Option<CompactString>),
    /// `date_add(DT, n[, zone])`.
    DateAdd(crate::value::dt::DT, i64, Option<CompactString>),
    /// `date_add(duration[, zone])`.
    DateAddDuration(crate::value::temporal::Duration, Option<CompactString>),
    /// `date_diff(value | traversal[, DT[, zone]])`.
    DateDiff(
        ScriptListArg,
        Option<crate::value::dt::DT>,
        Option<CompactString>,
    ),
    /// `age(value | traversal[, zone])`.
    Age(ScriptListArg, Option<CompactString>),
    /// `date_trunc(DT[, zone])`.
    DateTrunc(crate::value::dt::DT, Option<CompactString>),
    /// `date_part(DT[, zone])`.
    DatePart(crate::value::dt::DT, Option<CompactString>),
    /// `at_zone(zone)`.
    AtZone(CompactString),
    /// `as_local([zone])`.
    AsLocal(Option<CompactString>),
    As(Vec<CompactString>),
    From(Vec<CompactString>),
    To(Vec<CompactString>),
    AddE(Option<CompactString>),
    /// `add_e(traversal)`: the label is the traversal's first result.
    AddETraversal(ScriptGraphTraversalSource),
    /// `from(traversal)` / `to(traversal)`: the endpoint of the `add_e()` before it.
    FromTraversal(ScriptGraphTraversalSource),
    ToTraversal(ScriptGraphTraversalSource),
    /// `add_v()`'s label list (D8): `None` is no labels, `Some(vec![...])` is one or more,
    /// created in the given order. A bare `add_v("x")` becomes `Some(vec!["x".into()])`.
    AddV(Option<Vec<CompactString>>),
    // AddV_(Option<CompactString>),
    Drop,
    /// `remove_property(keys...)`; empty removes every property.
    RemoveProperty(Vec<PropKey>),
    /// `add_label(label)`: adds a label to each incoming vertex.
    AddLabel(CompactString),
    /// `drop_label(label)`: removes a label from each incoming vertex.
    DropLabel(CompactString),
    /// `set_label(label)`: changes the label of each incoming edge.
    SetLabel(CompactString),
    /// `property(key, value)`; the key is a literal name or a `jpath(..)`.
    Property(PropKey, ElementProperty),
    PropertyTraversal(PropKey, ScriptGraphTraversalSource),
    /// `property(traversal, value)`: the key is the first result of the traversal.
    PropertyKeyTraversal(ScriptGraphTraversalSource, ElementProperty),
    /// `property(traversal, traversal)`: key and value from traversals.
    PropertyKeyTraversalBy(ScriptGraphTraversalSource, ScriptGraphTraversalSource),
    /// `addV(traversal)`: the label is the first result of the traversal.
    AddVTraversal(ScriptGraphTraversalSource),
    /// `property(Cardinality, key, value)`.
    PropertyCardinality(Cardinality, PropKey, ElementProperty),
    /// `property(Cardinality, key, traversal)`.
    PropertyCardinalityTraversal(Cardinality, PropKey, ScriptGraphTraversalSource),
    /// `property(map)` / `property(Cardinality, map)` (`None`/`#{}` → no entries); each entry
    /// carries its own `Cardinality.x(v)` cardinality, if any.
    PropertyMap(Option<Cardinality>, Vec<PropertyEntry>),
    /// `property(T.id, v)` / `property(T.label, v)`, folded into the preceding `addV()`/`addE()`.
    PropertyToken(PropertyTokenKey, ElementProperty),
    /// `mergeV(..)`: `()`, a map, or a traversal (`mergeV()` is `mergeV(__.identity())`).
    MergeV(ScriptMergeArg),
    /// `mergeE(..)`: `()`, a map, or a traversal (`mergeE()` is `mergeE(__.identity())`).
    MergeE(ScriptMergeArg),
    /// `option(Merge.x, map|()|traversal[, Cardinality])`, attached to the preceding merge step.
    MergeOption(Merge, ScriptMergeArg, Option<Cardinality>),
    Subgraph(CompactString),
    /// `with(WithOptions.key[, WithOptions.value])`: configures the step before it.
    WithOption(crate::value::WithOptions, Option<crate::value::WithOptions>),
    #[cfg(feature = "serde")]
    PropertyJson(serde_json::Value),
    /// `gql(query[, #{params}])`: a GQL read query with its parameter values; it starts the
    /// root traversal (the builder refuses it elsewhere), see `convert.rs`.
    #[cfg(feature = "gql")]
    Gql(CompactString, Vec<(CompactString, ElementProperty)>),
    JsonPath(CompactString),
    /// `glob_path(pattern[, #{...}])`; the pattern is compiled
    /// and `max_depth` validated by the Rust step.
    GlobPath(CompactString, ScriptGlobPathOptions),
    /// A step of the traversal source `g` (`withSack`, `withSideEffect`); see [`SourceStep`].
    Source(SourceStep),
    /// `sack(Operator)`; a following `by(..)` is attached to it like to any modulated step.
    SackWith(Operator),
    /// `barrier(Barrier.normSack)`.
    BarrierWith(Barrier),
    /// `fold(seed, Operator)`.
    FoldWith(ElementProperty, Operator),
    Values(Vec<PropKey>),
    Project(Vec<CompactString>),
    ElementMap(Vec<PropKey>),
    /// `properties(...)`: the pure-property-object counterpart to `Values` — see
    /// `flatmap_steps/step_properties.rs`.
    Properties(Vec<PropKey>),
    /// `value()`: resolves a property-object traverser to its stored scalar.
    Value,
    /// `key()`: the property key name of a property-object traverser.
    Key,
    /// `valueMap(...)`: like `ElementMap`, minus the `id`/`label` prepending unless the flag
    /// (`valueMap(true, ..)`) asks for them.
    ValueMap(Vec<PropKey>, bool),
    WhereP(ArgP),
    /// `where("a", P)`: the predicate tests the object labelled `a`.
    WhereLabelP(CompactString, ArgP),
    WhereT(ScriptGraphTraversalSource),
    Repeat(Option<CompactString>, ScriptGraphTraversalSource),
    Times(u32),
    Until(ScriptGraphTraversalSource),
    Emit(Option<ScriptGraphTraversalSource>),
    Loops(Option<CompactString>),
    /// `choose(predicate, onTrue, onFalse)`: the 3-argument if/then/else sugar form.
    Choose3(
        ScriptGraphTraversalSource,
        ScriptGraphTraversalSource,
        ScriptGraphTraversalSource,
    ),
    /// `choose(traversal)`: the chained switch form, extended by `Step::Option`.
    ChooseSwitch(ScriptGraphTraversalSource),
    /// `branch(traversal)`: the fan-out counterpart of `Step::ChooseSwitch`.
    Branch(ScriptGraphTraversalSource),
    /// `.option(pick, traversal)`, chained onto the preceding `Step::ChooseSwitch`/`Step::Branch`.
    Option(OptionToken, ScriptGraphTraversalSource),
    /// `.option(traversal, traversal)`: a traversal key (`branch()` only; `choose()` rejects it
    /// when the traversal executes).
    OptionTraversal(ScriptGraphTraversalSource, ScriptGraphTraversalSource),
    /// `aggregate(sideEffectKey)`: collects every traverser into a growing side-effect bucket,
    /// eager/barrier semantics per TinkerPop's documentation (moot in this executor — see
    /// `step_aggregate.rs`'s doc comment). `.by(...)` support arrives the same way `group()`'s
    /// does, via `Step::By` chained afterwards.
    Aggregate(CompactString),
    /// `store(sideEffectKey)`: `Aggregate`'s lazy-documented sibling, implemented identically
    /// here — see `step_store.rs`'s doc comment.
    Store(CompactString),
    /// `tree()` / `tree(sideEffectKey)`: aggregates every traverser's full path into a
    /// hierarchical tree. `None` is the bare, direct-output form; `Some(key)` also reaches
    /// `cap(key)` — see `step_tree.rs`'s doc comment for why this is unlike `Aggregate`/`Store`
    /// (both forms replace the traversal's output either way). `.by(...)` support arrives the
    /// same way `group()`'s does, via `Step::By` chained afterwards.
    Tree(Option<CompactString>),
}
