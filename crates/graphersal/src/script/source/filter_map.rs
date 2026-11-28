//! Builder methods of start, filter, reducing and map steps (`V`, `count`, `limit`, `fold`,
//! `group`, `order`, `has*`, `dedup`, `values`, `project`, `elementMap`, ...).

use crate::script::*;

impl ScriptGraphTraversalSource {
    /// Starts at the vertices of the graph (all, or the ones with the given ids); after steps it
    /// starts a new scan per traverser. Rhai binding of `v()`, the overload without arguments.
    pub fn v_empty(&mut self) -> Self {
        self.add_step(Step::V(vec![]))
    }

    /// Starts at the vertices of the graph (all, or the ones with the given ids); after steps it
    /// starts a new scan per traverser. Rhai binding of `v()`, the overload with one argument.
    pub fn v_single(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let ids = ids_from_dynamic("V", vec![arg], access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::V(ids)))
    }

    /// Starts at the vertices of the graph (all, or the ones with the given ids); after steps it
    /// starts a new scan per traverser. Rhai binding of `v()`.
    pub fn v(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let ids = ids_from_dynamic("V", arg, access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::V(ids)))
    }

    /// Starts at the edges of the graph, all or the ones with the given ids. Rhai binding of `e()`,
    /// the overload without arguments.
    pub fn e_empty(&mut self) -> Self {
        self.add_step(Step::E(vec![]))
    }

    /// Starts at the edges of the graph, all or the ones with the given ids. Rhai binding of `e()`,
    /// the overload with one argument.
    pub fn e_single(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let ids = ids_from_dynamic("E", vec![arg], access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::E(ids)))
    }

    /// Starts at the edges of the graph, all or the ones with the given ids. Rhai binding of `e()`.
    pub fn e(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let ids = ids_from_dynamic("E", arg, access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::E(ids)))
    }

    /// Counts the traversers (bulk included); with Scope.local counts the elements of each incoming
    /// collection. Rhai binding of `count()`.
    pub fn count(&mut self) -> Self {
        self.add_step(Step::Count)
    }

    /// Passes on only the first n traversers; with Scope.local the first n elements of each
    /// collection. Rhai binding of `limit()`.
    pub fn limit(&mut self, arg: i64) -> Self {
        self.add_step(Step::Limit(arg as usize))
    }

    /// Skips the first n traversers (Scope.local: elements of each collection). Rhai binding of
    /// `skip()`.
    pub fn skip(&mut self, arg: i64) -> Self {
        self.add_step(Step::Skip(arg as usize))
    }

    /// `tail()`: the last traverser, `tail(1)`.
    pub fn tail_empty(&mut self) -> Self {
        self.add_step(Step::Tail(1))
    }

    /// Passes on only the last n traversers (default 1); with Scope.local the last elements of each
    /// collection. Rhai binding of `tail()`.
    pub fn tail(&mut self, arg: i64) -> Self {
        self.add_step(Step::Tail(arg as usize))
    }

    /// See [`crate::GraphTraversalSource::range`].
    pub fn range(&mut self, low: i64, high: i64) -> Self {
        self.add_step(Step::Range(low, high))
    }

    /// Yields the label of each vertex or edge. Rhai binding of `label()`.
    pub fn label(&mut self) -> Self {
        self.add_step(Step::Label)
    }

    /// See [`crate::GraphTraversalSource::labels`] (D4).
    pub fn labels(&mut self) -> Self {
        self.add_step(Step::Labels)
    }

    /// See [`crate::GraphTraversalSource::layer`]: the layer origin of each vertex or edge.
    pub fn layer(&mut self) -> Self {
        self.add_step(Step::Layer)
    }

    /// `has_layer()` without arguments: keeps every vertex and edge (each has an origin). See
    /// [`crate::GraphTraversalSource::has_layer`].
    pub fn has_any_layer(&mut self) -> Self {
        self.add_step(Step::HasAnyLayer)
    }

    /// Keeps the vertices and edges whose layer origin is the given one. Rhai binding of
    /// `has_layer()`, the overload with one argument.
    pub fn has_layer_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.has_layer(vec![arg])
    }

    /// Keeps the vertices and edges whose layer origin (`"base"`, `"added"`, `"modified"`) is
    /// one of the given ones; an unknown name fails before the traversal runs. Rhai binding of
    /// `has_layer()`. See [`crate::GraphTraversalSource::has_layer`].
    pub fn has_layer(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::HasLayer(vec_dynamic_to_compact_string(
            "has_layer",
            arg,
        )?)))
    }

    /// Yields the id of each vertex or edge (a string). Rhai binding of `id()`.
    pub fn id(&mut self) -> Self {
        self.add_step(Step::Id)
    }

    /// Passes every traverser through unchanged. Rhai binding of `identity()`.
    pub fn identity(&mut self) -> Self {
        self.add_step(Step::Identity)
    }

    /// `sack()`: the traverser's sack as its value (`null` without `withSack`).
    pub fn sack(&mut self) -> Self {
        self.add_step(Step::Sack)
    }

    /// `sack(Operator)`: updates the sack with `operator(sack, operand)`; the operand comes from
    /// a following `by(..)` or is the traverser's own value. Only an `Operator` is accepted
    /// (`sack(BiFunction)` lambdas do not exist).
    pub fn sack_with(&mut self, operator: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        match operator.read_lock::<Operator>() {
            Some(operator) => Ok(self.add_step(Step::SackWith(*operator))),
            None => Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                "an Operator (sack(BiFunction) is not supported: pass an Operator, for example \
                 sack(Operator.sum))"
                    .to_string(),
                type_name_of(&operator),
                rhai::Position::NONE,
            ))),
        }
    }

    /// `withSack(initial)` / `with_sack(initial)`: every generated traverser starts with
    /// `initial` as its sack. A source option, like `withSideEffect`.
    pub fn with_sack(
        ctx: NativeCallContext,
        this: &mut Self,
        initial: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let initial = value_arg(&ctx, "with_sack", initial)?;
        Ok(this.add_step(Step::Source(SourceStep::Sack(initial))))
    }

    /// `withSack(initial, Operator)` / `with_sack(initial, Operator)`: like `withSack(initial)`,
    /// and equal traversers that merge at an explicit `barrier()` combine their sacks with the
    /// merge `Operator`. A second argument that is not an `Operator` (a split operator or a
    /// lambda) is an error.
    pub fn with_sack_merge(
        ctx: NativeCallContext,
        this: &mut Self,
        initial: Dynamic,
        merge: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        match merge.read_lock::<Operator>() {
            Some(merge) => Ok(this.add_step(Step::Source(SourceStep::SackMerge(
                value_arg(&ctx, "with_sack", initial)?,
                *merge,
            )))),
            None => Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                "an Operator: with_sack(initial, UnaryOperator) split operators are not supported; \
                 the second argument is the merge Operator, for example \
                 with_sack(1, Operator.sum)"
                    .to_string(),
                type_name_of(&merge),
                rhai::Position::NONE,
            ))),
        }
    }

    /// `withBulk(false)` / `with_bulk(false)`: sets the option `bulk.one` (a merge keeps bulk 1).
    /// `withBulk(true)` clears it.
    pub fn with_bulk(&mut self, bulk: bool) -> Self {
        self.with_value("bulk.one".to_string(), ElementProperty::Boolean(!bulk))
    }

    /// `withPath()`: accepted on `g` for Gremlin compatibility and changes nothing. Graphersal
    /// records exactly the path positions the query reads (the path analysis), so no step needs
    /// paths to be switched on first. Like every source step it belongs to `g`: inside a child
    /// traversal (`__.withPath()`) it fails with
    /// [`crate::error::TraverserError::SourceStepInChild`] before execution.
    pub fn with_path(&mut self) -> Self {
        self.add_step(Step::Source(SourceStep::Path))
    }

    /// `inject()`: injects nothing.
    pub fn inject_empty(&mut self) -> Self {
        self.add_step(Step::Inject(vec![]))
    }

    /// `inject(x)`: injects `x` as one value. A Rhai array is one list value and `()` is `null`
    /// (TinkerPop's `inject([1, 2])` / `inject(null)`); several values use the variadic form
    /// `inject(a, b, ...)`.
    pub fn inject_single(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let value = value_arg(&ctx, "inject", arg)?;
        Ok(this.add_step(Step::Inject(vec![value])))
    }

    /// `inject(a, b, ...)`: one value per argument, in order (each argument as in
    /// [`Self::inject_single`]).
    pub fn inject(
        ctx: NativeCallContext,
        this: &mut Self,
        args: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let values = value_args(&ctx, "inject", args)?;
        Ok(this.add_step(Step::Inject(values)))
    }

    /// Emits the value of the named side effect(s) (one map for several keys) after the stream is
    /// exhausted. Rhai binding of `cap()`, the overload without arguments.
    pub fn cap_empty(&mut self) -> Self {
        self.add_step(Step::Cap(vec![]))
    }

    /// Emits the value of the named side effect(s) (one map for several keys) after the stream is
    /// exhausted. Rhai binding of `cap()`, the overload with one argument.
    pub fn cap_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Cap(vec![name_arg("cap", arg)?])))
    }

    /// Emits the value of the named side effect(s) (one map for several keys) after the stream is
    /// exhausted. Rhai binding of `cap()`.
    pub fn cap(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Cap(vec_dynamic_to_compact_string("cap", arg)?)))
    }

    /// Collects the whole stream into one list; with a seed and an Operator it reduces the stream
    /// instead. Rhai binding of `fold()`.
    pub fn fold(&mut self) -> Self {
        self.add_step(Step::Fold)
    }

    /// `fold(seed, Operator)`: left fold of `seed` over the stream (see `fold_with`). Only an
    /// `Operator` is accepted (`fold(BiFunction)` lambdas do not exist).
    pub fn fold_with(
        ctx: NativeCallContext,
        this: &mut Self,
        seed: Dynamic,
        operator: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        match operator.read_lock::<Operator>() {
            Some(operator) => Ok(this.add_step(Step::FoldWith(value_arg(&ctx, "fold", seed)?, *operator))),
            None => Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                "an Operator (fold(seed, BiFunction) is not supported: pass an Operator, for example \
                 fold(0, Operator.sum))"
                    .to_string(),
                type_name_of(&operator),
                rhai::Position::NONE,
            ))),
        }
    }

    /// Splits a list (or map, into entries) into its elements, one traverser each. Rhai binding of
    /// `unfold()`.
    pub fn unfold(&mut self) -> Self {
        self.add_step(Step::Unfold)
    }

    /// Groups the stream into a map: key by the first by(), values by the second by() (default: the
    /// elements); with a key it is a side effect. Rhai binding of `group()`.
    pub fn group(&mut self) -> Self {
        self.add_step(Step::Group)
    }

    /// Counts the stream per key (the first by(), default the element) into a map; with a key it is
    /// a side effect. Rhai binding of `group_count()`.
    pub fn group_count(&mut self) -> Self {
        self.add_step(Step::GroupCount)
    }

    /// `group("key")`: groups into the side effect `key` and passes every traverser on; see
    /// [`crate::GraphTraversalSource::group_side_effect`].
    pub fn group_side_effect(&mut self, key: &str) -> Self {
        self.add_step(Step::GroupSideEffect(key.into()))
    }

    /// `groupCount("key")` / `group_count("key")`: counts into the side effect `key` and passes
    /// every traverser on; see [`crate::GraphTraversalSource::group_count_side_effect`].
    pub fn group_count_side_effect(&mut self, key: &str) -> Self {
        self.add_step(Step::GroupCountSideEffect(key.into()))
    }

    /// `order()`: sorts the whole stream. See
    /// [`crate::GraphTraversalSource::order`] for the full contract (part 2 of the `order()`
    /// series — no `.by()` or a single `.by()` clause).
    pub fn order(&mut self) -> Self {
        self.add_step(Step::Order)
    }

    /// `.by("propertyKey", Order.desc)` / `.by(__.outE().count(), Order.desc)` — a single `.by()`
    /// call carrying both a key selector and an explicit direction. Expanded into the same two
    /// underlying steps a chained `.by(key).by(Order.desc)` would produce
    /// (`StepOrder::push_modulator_by`, in `traversal/step/flatbarrier_steps/step_order.rs`,
    /// merges the direction into the key's clause rather than starting a new one).
    pub fn by_with_order(
        &mut self,
        arg: Dynamic,
        order: Order,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_by(arg)?;
        let mut res = self.add_step(Step::By(val));
        Ok(res.add_step(Step::By(ArgBy::Order(order))))
    }

    /// Yields the largest value of the stream (Scope.local: of each collection; see the book page
    /// Local Scope). Rhai binding of `max()`.
    pub fn max(&mut self) -> Self {
        self.add_step(Step::Max)
    }

    /// Yields the arithmetic mean of the stream (Scope.local: of each collection). Rhai binding of
    /// `mean()`.
    pub fn mean(&mut self) -> Self {
        self.add_step(Step::Mean)
    }

    /// Yields the median of the numbers in the stream. Rhai binding of `median()`.
    pub fn median(&mut self) -> Self {
        self.add_step(Step::Median)
    }

    /// Yields the smallest value of the stream (Scope.local: of each collection; see the book page
    /// Local Scope). Rhai binding of `min()`.
    pub fn min(&mut self) -> Self {
        self.add_step(Step::Min)
    }

    /// Yields the sum of the numbers in the stream (Scope.local: of each collection). Rhai binding
    /// of `sum()`.
    pub fn sum(&mut self) -> Self {
        self.add_step(Step::Sum)
    }

    /// `count(Scope.local)` / `count(Scope.global)`; see
    /// [`crate::GraphTraversalSource::count_scoped`].
    pub fn count_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::CountScoped(scope))
    }

    /// `sum(Scope.local)` / `sum(Scope.global)`; see [`crate::GraphTraversalSource::sum_scoped`].
    pub fn sum_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::SumScoped(scope))
    }

    /// `min(Scope.local)` / `min(Scope.global)`; see [`crate::GraphTraversalSource::min_scoped`].
    pub fn min_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::MinScoped(scope))
    }

    /// `max(Scope.local)` / `max(Scope.global)`; see [`crate::GraphTraversalSource::max_scoped`].
    pub fn max_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::MaxScoped(scope))
    }

    /// `mean(Scope.local)` / `mean(Scope.global)`; see
    /// [`crate::GraphTraversalSource::mean_scoped`].
    pub fn mean_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::MeanScoped(scope))
    }

    /// `limit(Scope.local, n)` / `limit(Scope.global, n)`; see
    /// [`crate::GraphTraversalSource::limit_scoped`].
    pub fn limit_scoped(&mut self, scope: TraversalScope, n: i64) -> Self {
        self.add_step(Step::LimitScoped(scope, n))
    }

    /// `skip(Scope.local, n)` / `skip(Scope.global, n)`; see
    /// [`crate::GraphTraversalSource::skip_scoped`].
    pub fn skip_scoped(&mut self, scope: TraversalScope, n: i64) -> Self {
        self.add_step(Step::SkipScoped(scope, n))
    }

    /// `tail(Scope.local)` / `tail(Scope.global)`: TinkerPop's default count of `1`.
    pub fn tail_scoped_default(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::TailScoped(scope, 1))
    }

    /// `tail(Scope.local, n)` / `tail(Scope.global, n)`; see
    /// [`crate::GraphTraversalSource::tail_scoped`].
    pub fn tail_scoped(&mut self, scope: TraversalScope, n: i64) -> Self {
        self.add_step(Step::TailScoped(scope, n))
    }

    /// `range(Scope.local, low, high)` / `range(Scope.global, low, high)`; see
    /// [`crate::GraphTraversalSource::range_scoped`].
    pub fn range_scoped(&mut self, scope: TraversalScope, low: i64, high: i64) -> Self {
        self.add_step(Step::RangeScoped(scope, low, high))
    }

    /// `coin(p)`; see [`crate::GraphTraversalSource::coin`].
    pub fn coin(&mut self, arg: f64) -> Self {
        self.add_step(Step::Coin(arg as f32))
    }

    /// `fail()`: stops the traversal when a traverser reaches it; see
    /// [`crate::GraphTraversalSource::fail`].
    pub fn fail(&mut self) -> Self {
        self.add_step(Step::Fail(None))
    }

    /// `fail(message)`; see [`crate::GraphTraversalSource::fail`].
    pub fn fail_with(&mut self, message: String) -> Self {
        self.add_step(Step::Fail(Some(CompactString::new(message))))
    }

    /// `sample(n)`; see [`crate::GraphTraversalSource::sample`]. A negative `n` samples nothing
    /// (TinkerPop draws until it has `n`, which a negative `n` already is).
    pub fn sample(&mut self, n: i64) -> Self {
        self.sample_scoped(TraversalScope::Global, n)
    }

    /// `sample(Scope, n)`; see [`crate::GraphTraversalSource::sample_scoped`].
    pub fn sample_scoped(&mut self, scope: TraversalScope, n: i64) -> Self {
        self.add_step(Step::Sample(scope, usize::try_from(n).unwrap_or(0)))
    }

    /// `coin(p)` with an integer probability (`coin(1)` keeps every traverser, `coin(0)` none).
    pub fn coin_int(&mut self, arg: i64) -> Self {
        self.add_step(Step::Coin(arg as f32))
    }

    /// Keeps elements whose id is one of the given ids (ids are strings, see the book page Id
    /// Arguments). Rhai binding of `has_id()`, the overload with one argument.
    pub fn has_id_single(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        this.has_id_at(vec![arg], access::value_depth_of(&ctx))
    }

    /// Keeps elements whose id is one of the given ids (ids are strings, see the book page Id
    /// Arguments). Rhai binding of `has_id()`.
    pub fn has_id(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        this.has_id_at(arg, access::value_depth_of(&ctx))
    }

    /// `has_id(..)` with nested id lists bounded by `max_depth` levels.
    fn has_id_at(
        &mut self,
        arg: Vec<Dynamic>,
        max_depth: usize,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(match split_token_args("has_id", arg)? {
            TokenArgs::Literals(values) => {
                self.add_step(Step::HasId(ids_from_dynamic("has_id", values, max_depth)?))
            }
            TokenArgs::Predicate(predicate) => self.add_step(Step::HasIdP(predicate)),
        })
    }

    /// Keeps properties whose key is one of the given keys. Rhai binding of `has_key()`, the
    /// overload with one argument.
    pub fn has_key_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.has_key(vec![arg])
    }

    /// Keeps properties whose key is one of the given keys. Rhai binding of `has_key()`.
    pub fn has_key(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(match split_token_args("has_key", arg)? {
            TokenArgs::Literals(values) => self.add_step(Step::HasKey(
                vec_dynamic_to_compact_string("has_key", values)?,
            )),
            TokenArgs::Predicate(predicate) => self.add_step(Step::HasKeyP(predicate)),
        })
    }

    /// `hasLabel()` without arguments: keeps the elements that have a label at all (a vertex with
    /// a non-empty label set, an edge with a label); `not(__.has_label())` selects the unlabeled
    /// ones. A Graphersal extension: TinkerPop has no zero-argument `hasLabel()`.
    pub fn has_any_label(&mut self) -> Self {
        self.add_step(Step::HasAnyLabel)
    }

    /// Keeps elements whose label is one of the given labels (or matches a predicate); without
    /// arguments it keeps the elements that have a label, so not(__.has_label()) selects the
    /// unlabeled ones (Graphersal extension). Rhai binding of `has_label()`, the overload with one
    /// argument.
    pub fn has_label_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.has_label(vec![arg])
    }

    /// Keeps elements whose label is one of the given labels (or matches a predicate); without
    /// arguments it keeps the elements that have a label, so not(__.has_label()) selects the
    /// unlabeled ones (Graphersal extension). Rhai binding of `has_label()`.
    pub fn has_label(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(match split_token_args("has_label", arg)? {
            TokenArgs::Literals(values) => self.add_step(Step::HasLabel(
                vec_dynamic_to_compact_string("has_label", values)?,
            )),
            TokenArgs::Predicate(predicate) => self.add_step(Step::HasLabelP(predicate)),
        })
    }

    /// `hasValue(value)`/`hasValue(P<V>)`: mirrors `has()`'s exact runtime dispatch (D1 of the
    /// `has_value` TinkerPop-compliance fix) — Rhai is dynamically typed, so the single Rhai
    /// method inspects `arg`'s actual type and routes to the `P`-predicate step
    /// (`Step::HasValueP`) or the plain-literal one (`Step::HasValue`).
    pub fn has_value(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        refuse_traversal("has_value", "a value or a predicate", &arg)?;
        if arg.is::<ArgP>() || arg.type_name() == std::any::type_name::<ArgP>() {
            let val = try_dynamic_to_argp(&arg)?;
            Ok(this.add_step(Step::HasValueP(val)))
        } else {
            let val = value_arg(&ctx, "has_value", arg)?;
            Ok(this.add_step(Step::HasValue(val)))
        }
    }

    /// `hasValue(v1, v2, ...)` (D1): the vanilla `hasValue(Object, Object...)` multi-value OR
    /// overload, called from Rhai as `hasValue([v1, v2, ...])` (this codebase's established
    /// array-argument convention for a variadic Gremlin step, e.g. `hasLabel`/`hasId`).
    pub fn has_value_multi(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        for value in &arg {
            refuse_traversal("has_value", "values", value)?;
        }
        let values = value_args(&ctx, "has_value", arg)?;
        Ok(this.add_step(Step::HasValueAny(values)))
    }

    /// `has_value_p(P<V>)`: the standalone predicate-only counterpart of [`Self::has_value`]'s
    /// dispatch, mirroring [`Self::has_p`]'s own direct-predicate script method. Like `has_p()`,
    /// a bare scalar is also accepted here (`try_dynamic_to_argp` wraps it as `P.eq(value)`).
    pub fn has_value_p(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        refuse_traversal("has_value", "a value or a predicate", &arg)?;
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::HasValueP(val)))
    }

    /// Keeps elements that have the property (and, with a value or predicate, whose value matches;
    /// with a traversal, for whose value the traversal yields a result: `has(key, __.to_lower()
    /// .is("x"))` is `filter(values(key).to_lower().is("x"))`, TinkerPop's `has(String,
    /// Traversal)`); the 3-argument form also tests the label. Rhai binding of `has()`.
    pub fn has(
        ctx: NativeCallContext,
        this: &mut Self,
        prop: Dynamic,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let prop = dynamic_to_prop_key("has", prop)?;
        if arg.is::<ScriptGraphTraversalSource>() {
            let child = try_dynamic_to_gts(arg)?;
            Ok(this.add_step(Step::HasTraversal(prop.into(), child)))
        } else if arg.is::<ArgP>() || arg.type_name() == std::any::type_name::<ArgP>() {
            let val = try_dynamic_to_argp(&arg)?;
            Ok(this.add_step(Step::HasP(prop, val)))
        } else {
            let val = value_arg(&ctx, "has", arg)?;
            Ok(this.add_step(Step::Has(prop, val)))
        }
    }

    /// `has(label, key, value)` / `has(label, key, P)`: TinkerPop's three-argument `has`, i.e.
    /// `hasLabel(label).has(key, value|P)`; see [`crate::GraphTraversalSource::has_labeled`].
    pub fn has_labeled(
        ctx: NativeCallContext,
        this: &mut Self,
        label: String,
        prop: Dynamic,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let mut with_label = this.add_step(Step::HasLabel(vec![CompactString::new(label)]));
        Self::has(ctx, &mut with_label, prop, arg)
    }

    /// `has(key)`: the one-argument, element-level existence overload of `has()` — keeps the
    /// element/property that carries `key`, regardless of value. Distinct from `hasKey()`, which
    /// vanilla TinkerPop reserves for a property-level check after `properties()`; see
    /// `step_has_property.rs`'s doc comment.
    pub fn has_property(&mut self, key: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::HasProperty(dynamic_to_prop_key("has", key)?)))
    }

    /// `hasNot(key)`: keeps the element/property that does *not* carry `key`.
    pub fn has_not(&mut self, key: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::HasNot(dynamic_to_prop_key("has_not", key)?)))
    }

    /// `has(T.id, value)`/`has(T.label, value)`: TinkerPop's token form of `has()`, rewritten to
    /// the equivalent `has_id(..)`/`has_label(..)` step. The value is a literal, `P.eq(v)` or
    /// `P.within(..)`; any other predicate or token is a script error, since `has_id()`/
    /// `has_label()` only match by equality. With a traversal (`has(T.id, __.is("1"))`,
    /// TinkerPop's `has(T, Traversal)`) it is the `has_traversal` step on the id or label.
    pub fn has_t(&mut self, token: ArgBy, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let is_id = match token {
            ArgBy::Id => true,
            ArgBy::Label => false,
            ArgBy::Property(_)
            | ArgBy::Traversal(_)
            | ArgBy::Value
            | ArgBy::Key
            | ArgBy::Count
            | ArgBy::JsonPath(_)
            | ArgBy::Order(_)
            | ArgBy::Labels(_)
            | ArgBy::Column(_) => {
                return Err(has_t_mismatch(
                    "a property key, T.id or T.label as the first argument",
                    by_token_text(&token),
                ));
            }
        };
        if arg.is::<ScriptGraphTraversalSource>() {
            let child = try_dynamic_to_gts(arg)?;
            let token = if is_id {
                crate::RecordKey::Id
            } else {
                crate::RecordKey::Label
            };
            return Ok(self.add_step(Step::HasTraversal(token.into(), child)));
        }
        if arg.is_array() {
            return Err(has_t_mismatch(
                "a value or a predicate with T.id/T.label",
                "an array (use P.within([..]) to match any of several values)".to_string(),
            ));
        }
        if is_id {
            // Not an array (rejected above): no nesting to bound.
            self.has_id_at(vec![arg], crate::value::depth::DEFAULT_MAX_VALUE_DEPTH)
        } else {
            self.has_label_single(arg)
        }
    }

    /// `discard()`: filters every traverser; see [`crate::GraphTraversalSource::discard`].
    pub fn discard(&mut self) -> Self {
        self.add_step(Step::Discard)
    }

    /// Removes duplicates, optionally by a by() key or by the values of path labels; with
    /// Scope.local inside each collection. Rhai binding of `dedup()`.
    pub fn dedup(&mut self) -> Self {
        self.add_step(Step::Dedup)
    }

    /// `barrier()`: merges equal traversers; see [`crate::GraphTraversalSource::barrier`].
    pub fn barrier(&mut self) -> Self {
        self.add_step(Step::Barrier(None))
    }

    /// `barrier(n)`: `n` must be an integer from 1 to 4294967295; `barrier(Barrier.normSack)`
    /// normalises the sacks. Gremlin's `barrier(Consumer)` lambdas are not supported and fail like
    /// any other argument of the wrong type.
    pub fn barrier_sized(&mut self, size: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        if let Some(barrier) = size.read_lock::<Barrier>() {
            return Ok(self.add_step(Step::BarrierWith(*barrier)));
        }
        let Some(n) = size.clone().try_cast::<i64>() else {
            return Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                "integer barrier size (barrier(Consumer) is not supported)".to_string(),
                type_name_of(&size),
                rhai::Position::NONE,
            )));
        };
        match u32::try_from(n) {
            Ok(n) if n >= 1 => Ok(self.add_step(Step::Barrier(Some(n)))),
            _ => Err(format!(
                "barrier({n}): the size must be an integer from 1 to {}",
                u32::MAX
            )
            .into()),
        }
    }

    /// `dedup(Scope.local)` / `dedup(Scope.global)`; see
    /// [`crate::GraphTraversalSource::dedup_scoped`].
    pub fn dedup_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::DedupScoped(scope, Vec::new()))
    }

    /// `dedup(label)`: deduplicates the stream by the object last labelled `label` on the path;
    /// see [`crate::GraphTraversalSource::dedup_scoped_labels`].
    pub fn dedup_label(&mut self, label: &str) -> Self {
        self.add_step(Step::DedupScoped(
            TraversalScope::Global,
            vec![label.to_string()],
        ))
    }

    /// `dedup(Scope, label, ...)` or `dedup(label, label, ...)`: a leading `Scope` is followed
    /// by label strings (ignored in local scope, TinkerPop behaviour); without it every argument
    /// is a label and the scope is global (deduplicating by the labelled path objects). See
    /// [`crate::GraphTraversalSource::dedup_scoped_labels`].
    pub fn dedup_scoped_labels(
        &mut self,
        args: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let signature = format!(
            "dedup ({})",
            args.iter().map(type_name_of).collect::<Vec<_>>().join(", ")
        );
        let mut args = args.into_iter().peekable();
        let scope = match args
            .peek()
            .map(|arg| arg.clone().try_cast::<TraversalScope>())
        {
            Some(Some(scope)) => {
                args.next();
                scope
            }
            Some(None) if args.peek().is_some_and(Dynamic::is_string) => TraversalScope::Global,
            Some(None) => {
                return Err(Box::new(rhai::EvalAltResult::ErrorFunctionNotFound(
                    signature,
                    rhai::Position::NONE,
                )));
            }
            None => return Ok(self.dedup()),
        };
        let mut labels = Vec::new();
        for arg in args {
            match arg.clone().into_immutable_string() {
                Ok(label) => labels.push(label.to_string()),
                Err(actual) => {
                    return Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                        "a step label string".to_string(),
                        super::super::register::display_type_name(actual),
                        rhai::Position::NONE,
                    )));
                }
            }
        }
        Ok(self.add_step(Step::DedupScoped(scope, labels)))
    }
    /// `order(Scope.local)` / `order(Scope.global)`; see
    /// [`crate::GraphTraversalSource::order_scoped`].
    pub fn order_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::OrderScoped(scope))
    }

    /// Yields the values of the given (or all) properties; a jpath() key reads a nested value. Rhai
    /// binding of `values()`, the overload without arguments.
    pub fn values_empty(&mut self) -> Self {
        self.add_step(Step::Values(vec![]))
    }

    /// Yields the values of the given (or all) properties; a jpath() key reads a nested value. Rhai
    /// binding of `values()`, the overload with one argument.
    pub fn values_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Values(vec![dynamic_to_prop_key("values", arg)?])))
    }

    /// Yields the values of the given (or all) properties; a jpath() key reads a nested value. Rhai
    /// binding of `values()`.
    pub fn values(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Values(vec_dynamic_to_prop_key("values", arg)?)))
    }

    /// Turns each traverser into a map with the given keys, one by() per key in order. Rhai binding
    /// of `project()`, the overload with one argument.
    pub fn project_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Project(vec![name_arg("project", arg)?])))
    }

    /// Turns each traverser into a map with the given keys, one by() per key in order. Rhai binding
    /// of `project()`.
    pub fn project(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Project(vec_dynamic_to_compact_string(
            "project", arg,
        )?)))
    }

    /// Turns each vertex or edge into a map of its id, label and the given (or all) properties; an
    /// edge also gets its endpoints. Rhai binding of `element_map()`, the overload without
    /// arguments.
    pub fn element_map_empty(&mut self) -> Self {
        self.add_step(Step::ElementMap(vec![]))
    }

    /// Turns each vertex or edge into a map of its id, label and the given (or all) properties; an
    /// edge also gets its endpoints. Rhai binding of `element_map()`, the overload with one
    /// argument.
    pub fn element_map_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::ElementMap(vec![dynamic_to_prop_key(
            "element_map",
            arg,
        )?])))
    }

    /// Turns each vertex or edge into a map of its id, label and the given (or all) properties; an
    /// edge also gets its endpoints. Rhai binding of `element_map()`.
    pub fn element_map(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::ElementMap(vec_dynamic_to_prop_key(
            "element_map",
            arg,
        )?)))
    }

    /// Yields the property elements of vertices/edges (all, or the given keys). Rhai binding of
    /// `properties()`, the overload without arguments.
    pub fn properties_empty(&mut self) -> Self {
        self.add_step(Step::Properties(vec![]))
    }

    /// Yields the property elements of vertices/edges (all, or the given keys). Rhai binding of
    /// `properties()`, the overload with one argument.
    pub fn properties_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Properties(vec![dynamic_to_prop_key(
            "properties",
            arg,
        )?])))
    }

    /// Yields the property elements of vertices/edges (all, or the given keys). Rhai binding of
    /// `properties()`.
    pub fn properties(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Properties(vec_dynamic_to_prop_key(
            "properties",
            arg,
        )?)))
    }

    /// Yields the value of each property. Rhai binding of `value()`.
    pub fn value(&mut self) -> Self {
        self.add_step(Step::Value)
    }

    /// Yields the key of each property. Rhai binding of `key()`.
    pub fn key(&mut self) -> Self {
        self.add_step(Step::Key)
    }

    /// Turns each element into a map of its properties (each value in a list); value_map(true) adds
    /// id and label. Rhai binding of `value_map()`, the overload without arguments.
    pub fn value_map_empty(&mut self) -> Self {
        self.add_step(Step::ValueMap(vec![], false))
    }

    /// `value_map(key)` / `valueMap(key)`. `step_name` is the spelling the script called, so a
    /// rejected argument is reported under it; see `value_map_keys`.
    pub fn value_map_single(
        &mut self,
        step_name: &str,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let (keys, tokens) = value_map_keys(step_name, vec![arg])?;
        Ok(self.add_step(Step::ValueMap(keys, tokens)))
    }

    /// `value_map(k1, k2, ..)` / `value_map([k1, k2, ..])` and their `valueMap` spelling.
    pub fn value_map(
        &mut self,
        step_name: &str,
        arg: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let (keys, tokens) = value_map_keys(step_name, arg)?;
        Ok(self.add_step(Step::ValueMap(keys, tokens)))
    }

    /// The list functions `combine`, `difference`, `disjunct`, `intersect`, `merge`, `product`:
    /// a traversal argument becomes a child traversal, anything else a constant (a list; a map
    /// for `merge`; another value is rejected before execution).
    /// The constant is converted under the engine's value-depth limit
    /// (`script.max_value_depth`).
    fn list_function(
        ctx: &NativeCallContext,
        this: &mut Self,
        kind: ListFunction,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let arg = if arg.is::<ScriptGraphTraversalSource>() {
            ScriptListArg::Traversal(try_dynamic_to_gts(arg)?)
        } else {
            ScriptListArg::Value(value_arg(ctx, kind.name(), arg)?)
        };
        Ok(this.add_step(Step::ListFunction(kind, arg)))
    }

    /// `combine(list | traversal)`; see [`crate::GraphTraversalSource::combine`].
    pub fn combine(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::list_function(&ctx, this, ListFunction::Combine, arg)
    }

    /// `difference(list | traversal)`; see [`crate::GraphTraversalSource::difference`].
    pub fn difference(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::list_function(&ctx, this, ListFunction::Difference, arg)
    }

    /// `disjunct(list | traversal)`; see [`crate::GraphTraversalSource::disjunct`].
    pub fn disjunct(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::list_function(&ctx, this, ListFunction::Disjunct, arg)
    }

    /// `intersect(list | traversal)`; see [`crate::GraphTraversalSource::intersect`].
    pub fn intersect(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::list_function(&ctx, this, ListFunction::Intersect, arg)
    }

    /// `merge(list | map | traversal)`; see [`crate::GraphTraversalSource::merge`].
    pub fn merge(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::list_function(&ctx, this, ListFunction::Merge, arg)
    }

    /// `product(list | traversal)`; see [`crate::GraphTraversalSource::product`].
    pub fn product(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::list_function(&ctx, this, ListFunction::Product, arg)
    }

    /// `conjoin(delimiter)`; see [`crate::GraphTraversalSource::conjoin`].
    pub fn conjoin(&mut self, delimiter: String) -> Self {
        self.add_step(Step::Conjoin(CompactString::new(delimiter)))
    }

    /// Evaluates an arithmetic expression; _ is the incoming value and other names are path labels
    /// or by() values (see the book page Math Expressions). Rhai binding of `math()`.
    pub fn math(&mut self, arg: String) -> Self {
        self.add_step(Step::Math(CompactString::new(arg)))
    }

    /// `replace(from, to)`; see [`crate::GraphTraversalSource::replace`]. Each argument is a
    /// string or `()` (`null`).
    pub fn replace(
        &mut self,
        str_from: Dynamic,
        str_to: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.replace_scoped(TraversalScope::Global, str_from, str_to)
    }

    /// `concat()` without arguments: the incoming string unchanged.
    pub fn concat_empty(&mut self) -> Self {
        self.add_step(Step::ConcatStrings(Vec::new()))
    }

    /// `concat(x)`; see [`Self::concat`].
    pub fn concat_single(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::concat(ctx, this, vec![arg])
    }

    /// `concat(string...)` / `concat(traversal...)`; see [`crate::GraphTraversalSource::concat`]
    /// and [`crate::GraphTraversalSource::concat_traversal`]. The arguments are all strings (or
    /// `()`) or all traversals.
    pub fn concat(
        ctx: NativeCallContext,
        this: &mut Self,
        args: Vec<Dynamic>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let traversals = args
            .iter()
            .filter(|arg| arg.is::<ScriptGraphTraversalSource>())
            .count();
        if traversals == 0 {
            return Ok(this.add_step(Step::ConcatStrings(value_args(&ctx, "concat", args)?)));
        }
        if traversals != args.len() {
            return Err(script_error_from_script(
                crate::error::ScriptError::ArgumentMismatch {
                    step_name: "concat".to_string(),
                    expected: "only strings or only traversals".to_string(),
                    actual: "strings mixed with traversals".to_string(),
                },
            ));
        }
        let gts = args
            .into_iter()
            .map(try_dynamic_to_gts)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(this.add_step(Step::ConcatTraversals(gts)))
    }

    /// `format(pattern)`; see [`crate::GraphTraversalSource::format`].
    pub fn format(&mut self, pattern: String) -> Self {
        self.add_step(Step::Format(pattern))
    }

    /// `split(separator)`; see [`crate::GraphTraversalSource::split`]. The separator is a
    /// string or `()` (`null`: split on whitespace).
    pub fn split(&mut self, separator: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.split_scoped(TraversalScope::Global, separator)
    }

    /// `split(Scope, separator)`; see [`crate::GraphTraversalSource::split_scoped`].
    pub fn split_scoped(
        &mut self,
        scope: TraversalScope,
        separator: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let separator = nullable_string("split", separator)?;
        Ok(self.add_step(Step::Split(scope, separator)))
    }

    /// `index()`; see [`crate::GraphTraversalSource::index`].
    pub fn index(&mut self) -> Self {
        self.add_step(Step::Index)
    }

    /// `reverse()`; see [`crate::GraphTraversalSource::reverse`].
    pub fn reverse(&mut self) -> Self {
        self.add_step(Step::Reverse)
    }
}

/// A `by()` token as the DSL writes it, for a diagnostic.
fn by_token_text(token: &ArgBy) -> String {
    match token {
        ArgBy::Property(key) => crate::value::literal::rhai_string(key),
        ArgBy::Traversal(_) => "a traversal".to_string(),
        ArgBy::Label => "T.label".to_string(),
        ArgBy::Id => "T.id".to_string(),
        ArgBy::Value => "T.value".to_string(),
        ArgBy::Key => "T.key".to_string(),
        ArgBy::Count => "Count".to_string(),
        ArgBy::JsonPath(path) => crate::value::literal::rhai_jpath(path),
        ArgBy::Column(column) => format!("Column.{column}"),
        ArgBy::Order(order) => order.to_string(),
        ArgBy::Labels(labels) => crate::value::literal::rhai_string_array(labels),
    }
}
