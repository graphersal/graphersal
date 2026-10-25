//! Builder methods of side-effect, mutation, predicate and child-traversal steps
//! (`withSideEffect`, `addV`, `property`, `mergeV`, `is`, `where`, `and`, `union`, ...).

use crate::script::*;

impl ScriptGraphTraversalSource {
    // ========================================================================

    /// `withSideEffect(name, value)`. A [`ScriptSet`] declares a set bucket; a graph value (the
    /// result of `cap("sg")`/`to_graph()`, or `GraphSource::empty()`) is the explicit target of
    /// `subgraph(name)`; anything else is a seed value.
    ///
    /// The target graph must be a `TraversalGraph` (every graph a script builds is one): a host
    /// graph of another storage fails here with the diagnostic of
    /// [`GraphError::Unsupported`](crate::error::GraphError::Unsupported), naming its storage.
    pub fn with_side_effect(
        ctx: NativeCallContext,
        this: &mut Self,
        prop: String,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        if let Some(set) = arg.read_lock::<ScriptSet>() {
            return Ok(this.add_step(Step::Source(SourceStep::SideEffectSet(
                CompactString::new(prop),
                set.0.clone(),
            ))));
        }
        if let Some(target) = arg.read_lock::<ScriptGraphTraversalSource>() {
            // A bare graph value (no steps yet) is a target. The session's own `g` is one too; the
            // step then fails when it runs, because the traversed graph is locked.
            if target.steps.is_empty()
                && let Some(graph) = &target.g
            {
                return Ok(this.add_step(Step::Source(SourceStep::SideEffectGraph(
                    CompactString::new(prop),
                    ScriptGraphTarget {
                        graph: subgraph_target(graph)?,
                        private: target.private,
                    },
                ))));
            }
        }
        let value = value_arg(&ctx, arg)?;
        Ok(this.add_step(Step::Source(SourceStep::SideEffect(
            CompactString::new(prop),
            value,
        ))))
    }

    /// `withSideEffect(name, initial, Operator)`: `aggregate(name)`/`store(name)` combine into the
    /// value with the `Operator`. Only an `Operator` is accepted (no `BinaryOperator` lambdas).
    pub fn with_side_effect_reduced(
        ctx: NativeCallContext,
        this: &mut Self,
        prop: String,
        initial: Dynamic,
        operator: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        match operator.read_lock::<Operator>() {
            Some(operator) => Ok(this.add_step(Step::Source(SourceStep::SideEffectReduced(
                CompactString::new(prop),
                value_arg(&ctx, initial)?,
                *operator,
            )))),
            None => Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                "an Operator (with_side_effect(name, initial, BinaryOperator) lambdas are not \
                 supported: pass an Operator, for example with_side_effect(\"a\", 0, Operator.sum))"
                    .to_string(),
                type_name_of(&operator),
                rhai::Position::NONE,
            ))),
        }
    }

    /// `subgraph()` without a label: always an error naming the working forms.
    pub fn subgraph_no_label(&mut self) -> Result<Self, Box<rhai::EvalAltResult>> {
        let err = crate::TraverserError::QueryExecution {
            query: "subgraph()".to_string(),
            reason: "subgraph() needs a label".to_string(),
        };
        Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
            Dynamic::from(crate::error::format_diagnostic(&err)),
            rhai::Position::NONE,
        )))
    }

    /// Collects the incoming edges with both endpoints into a graph side effect; cap() it or use
    /// to_graph() (see the book page Set Side Effects). Rhai binding of `subgraph()`.
    pub fn subgraph(&mut self, arg: String) -> Self {
        self.add_step(Step::Subgraph(CompactString::new(arg)))
    }

    /// `aggregate(sideEffectKey)` — see `Step::Aggregate`'s doc comment.
    pub fn aggregate(&mut self, key: String) -> Self {
        self.add_step(Step::Aggregate(CompactString::new(key)))
    }

    /// `store(sideEffectKey)` — see `Step::Store`'s doc comment.
    pub fn store(&mut self, key: String) -> Self {
        self.add_step(Step::Store(CompactString::new(key)))
    }

    /// `tree()` — see `Step::Tree`'s doc comment.
    pub fn tree_empty(&mut self) -> Self {
        self.add_step(Step::Tree(None))
    }

    /// `tree(sideEffectKey)` — see `Step::Tree`'s doc comment.
    pub fn tree_single(&mut self, key: String) -> Self {
        self.add_step(Step::Tree(Some(CompactString::new(key))))
    }

    /// Removes the incoming vertices, edges or properties from the graph (a vertex takes its edges
    /// with it) and yields nothing (see the book page Dropping Elements). Rhai binding of `drop()`.
    pub fn drop(&mut self) -> Self {
        self.add_step(Step::Drop)
    }

    /// `remove_property()`: no argument removes every property.
    pub fn remove_property_empty(&mut self) -> Self {
        self.add_step(Step::RemoveProperty(vec![]))
    }

    /// `remove_property(key)` / `remove_property([keys])`: a string, a `jpath(..)` or an array of
    /// them (a path removes the value below the named property).
    pub fn remove_property(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let items = if arg.is_array() {
            arg.into_array().unwrap_or_default()
        } else {
            vec![arg]
        };
        let mut keys = Vec::with_capacity(items.len());
        for item in items {
            if let Some(path) = item.read_lock::<crate::value::JsonPath>() {
                keys.push(PropKey::Path(path.clone()));
                continue;
            }
            match item.read_lock::<rhai::ImmutableString>() {
                Some(key) => keys.push(PropKey::Name(CompactString::new(key.as_str()))),
                None => {
                    return Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                        Dynamic::from(crate::error::format_diagnostic(
                            &crate::error::ScriptError::ArgumentMismatch {
                                step_name: "remove_property".to_string(),
                                expected: "property keys as strings or jpath(..) values"
                                    .to_string(),
                                actual: format!(
                                    "a value of type {} (`{}`)",
                                    type_name_of(&item),
                                    dynamic_text(&item)
                                ),
                            },
                        )),
                        rhai::Position::NONE,
                    )));
                }
            }
        }
        // An empty array would silently mean "everything": refuse it.
        if keys.is_empty() {
            return Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                Dynamic::from(crate::error::format_diagnostic(
                    &crate::error::ScriptError::ArgumentMismatch {
                        step_name: "remove_property".to_string(),
                        expected: "property keys as strings".to_string(),
                        actual: "an empty array".to_string(),
                    },
                )),
                rhai::Position::NONE,
            )));
        }
        Ok(self.add_step(Step::RemoveProperty(keys)))
    }

    /// `add_label(label)`: adds a label to each incoming vertex.
    pub fn add_label(&mut self, label: String) -> Self {
        self.add_step(Step::AddLabel(CompactString::new(label)))
    }

    /// `drop_label(label)`: removes a label from each incoming vertex.
    pub fn drop_label(&mut self, label: String) -> Self {
        self.add_step(Step::DropLabel(CompactString::new(label)))
    }

    /// `set_label(label)`: changes the label of each incoming edge.
    pub fn set_label(&mut self, label: String) -> Self {
        self.add_step(Step::SetLabel(CompactString::new(label)))
    }

    /// Adds a vertex with the given label (or the label a child traversal yields); chain property()
    /// to set its properties. Rhai binding of `add_v()`, the overload without arguments.
    pub fn add_v_empty(&mut self) -> Self {
        self.add_step(Step::AddV(None))
    }

    /// Adds a vertex with the given label (or the label a child traversal yields); chain property()
    /// to set its properties. Rhai binding of `add_v()`.
    pub fn add_v(&mut self, arg: String) -> Self {
        self.add_step(Step::AddV(Some(vec![CompactString::new(arg)])))
    }

    /// `addV(traversal)`: the label is the first result of the traversal; see
    /// [`crate::GraphTraversalSource::add_v_by`].
    pub fn add_v_traversal(&mut self, label: ScriptGraphTraversalSource) -> Self {
        self.add_step(Step::AddVTraversal(label))
    }

    /// `add_v(["Person", "Admin"])` — a list argument creates a multi-label vertex (D8), in the
    /// order given. Mirrors `has_label`'s single-arg/`Vec<Dynamic>` overload split.
    pub fn add_v_labels(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::AddV(Some(vec_dynamic_to_compact_string(
            "add_v", arg,
        )?))))
    }

    // pub fn add_v_(&mut self, arg: String) -> Self {
    //     self.add_step(Step::AddV_(Some(arg)))
    // }

    /// Adds an edge with the given label; its endpoints come from from() and to() (see the book
    /// page addE() Endpoints). Rhai binding of `add_e()`, the overload without arguments.
    pub fn add_e_empty(&mut self) -> Self {
        self.add_step(Step::AddE(None))
    }

    /// `add_e(label)` or `add_e(traversal)`.
    pub fn add_e(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        if arg.is::<ScriptGraphTraversalSource>() {
            return Ok(self.add_step(Step::AddETraversal(try_dynamic_to_gts(arg)?)));
        }
        match arg.into_string() {
            Ok(label) => Ok(self.add_step(Step::AddE(Some(CompactString::new(label))))),
            Err(type_name) => Err(script_error_from_script(
                crate::error::ScriptError::ArgumentMismatch {
                    step_name: "add_e".to_string(),
                    expected: "a label string or a traversal".to_string(),
                    actual: super::super::register::display_type_name(type_name),
                },
            )),
        }
    }

    /// Graphersal form of has(key, predicate): keeps elements whose property matches the predicate
    /// (a bare value means P.eq). Rhai binding of `has_p()`.
    pub fn has_p(&mut self, prop: Dynamic, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let prop = dynamic_to_prop_key("has", prop)?;
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::HasP(prop, val)))
    }

    /// `property(key, value|traversal)`: `key` is a plain name or a `jpath(..)` (written below the
    /// stored property, creating missing objects; see `JsonPath`).
    pub fn property(
        ctx: NativeCallContext,
        this: &mut Self,
        prop: Dynamic,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        // `property(traversal, value)`: the key is the traversal's first result.
        if prop.is::<ScriptGraphTraversalSource>() {
            let key = try_dynamic_to_gts(prop)?;
            if arg.is::<ScriptGraphTraversalSource>() {
                let value = try_dynamic_to_gts(arg)?;
                return Ok(this.add_step(Step::PropertyKeyTraversalBy(key, value)));
            }
            let value = value_arg(&ctx, arg)?;
            return Ok(this.add_step(Step::PropertyKeyTraversal(key, value)));
        }
        let prop = property_write_key("property", prop)?;
        if arg.is::<ScriptGraphTraversalSource>()
            || arg.type_name() == std::any::type_name::<ScriptGraphTraversalSource>()
        {
            let gts = try_dynamic_to_gts(arg)?;
            Ok(this.add_step(Step::PropertyTraversal(prop, gts)))
        } else {
            let val = value_arg(&ctx, arg)?;
            Ok(this.add_step(Step::Property(prop, val)))
        }
    }

    /// `property(Cardinality, key, value|traversal)`; a `jpath(..)` key fails when executed.
    pub fn property_cardinality(
        ctx: NativeCallContext,
        this: &mut Self,
        cardinality: Cardinality,
        prop: Dynamic,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let prop = property_write_key("property", prop)?;
        if arg.is::<ScriptGraphTraversalSource>() {
            let gts = try_dynamic_to_gts(arg)?;
            Ok(this.add_step(Step::PropertyCardinalityTraversal(cardinality, prop, gts)))
        } else {
            let val = value_arg(&ctx, arg)?;
            Ok(this.add_step(Step::PropertyCardinality(cardinality, prop, val)))
        }
    }

    /// `property(map)`: one write per entry; a `Cardinality.x(v)` value carries its own
    /// cardinality.
    pub fn property_map(
        ctx: NativeCallContext,
        this: &mut Self,
        map: rhai::Map,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let entries = property_entries_of(map, access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::PropertyMap(None, entries)))
    }

    /// `property(null)`: writes nothing.
    pub fn property_unit(&mut self, _: ()) -> Self {
        self.add_step(Step::PropertyMap(None, Vec::new()))
    }

    /// `property(Cardinality, map)`.
    pub fn property_cardinality_map(
        ctx: NativeCallContext,
        this: &mut Self,
        cardinality: Cardinality,
        map: rhai::Map,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let entries = property_entries_of(map, access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::PropertyMap(Some(cardinality), entries)))
    }

    /// `property(Cardinality, null)`: writes nothing.
    pub fn property_cardinality_unit(&mut self, cardinality: Cardinality, _: ()) -> Self {
        self.add_step(Step::PropertyMap(Some(cardinality), Vec::new()))
    }

    /// `mergeV()` / `merge_v()`: the incoming traverser is the merge map
    /// (`mergeV(__.identity())`).
    pub fn merge_v_empty(&mut self) -> Self {
        let identity = ScriptGraphTraversalSource {
            g: None,
            steps: vec![Step::Identity],
            options: Default::default(),
            access: Default::default(),
            private: true,
            anonymous: true,
            now: None,
        };
        self.add_step(Step::MergeV(ScriptMergeArg::Traversal(identity)))
    }

    /// `mergeV(map|()|traversal)`: a map's `Cardinality.x(v)` values carry their cardinality;
    /// `Merge.outV`/`Merge.inV` values become the reserved `"Merge.outV"`/`"Merge.inV"`.
    pub fn merge_v(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let arg = script_merge_arg(arg, access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::MergeV(arg)).stamped(&ctx))
    }

    /// `mergeE()` / `merge_e()`: the incoming traverser is the merge map
    /// (`mergeE(__.identity())`).
    pub fn merge_e_empty(&mut self) -> Self {
        let identity = ScriptGraphTraversalSource {
            g: None,
            steps: vec![Step::Identity],
            options: Default::default(),
            access: Default::default(),
            private: true,
            anonymous: true,
            now: None,
        };
        self.add_step(Step::MergeE(ScriptMergeArg::Traversal(identity)))
    }

    /// `mergeE(map|()|traversal)`: `"Direction.OUT"`/`"Direction.IN"` take a vertex id or
    /// `Merge.outV`/`Merge.inV` (resolved by `option(Merge.outV|inV, ..)`).
    pub fn merge_e(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let arg = script_merge_arg(arg, access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::MergeE(arg)).stamped(&ctx))
    }

    /// `option(Merge.x, map|()|traversal, Cardinality)`: the map's default cardinality.
    pub fn option_merge_cardinality(
        ctx: NativeCallContext,
        this: &mut Self,
        token: Merge,
        arg: Dynamic,
        cardinality: Cardinality,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let arg = script_merge_arg(arg, access::value_depth_of(&ctx))?;
        Ok(this.add_step(Step::MergeOption(token, arg, Some(cardinality))))
    }

    /// `property(T.id, v)` / `property(T.label, v)` on the element `addV()`/`addE()` creates.
    pub fn property_token(
        ctx: NativeCallContext,
        this: &mut Self,
        token: ArgBy,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let key =
            match token {
                ArgBy::Id => PropertyTokenKey::Id,
                ArgBy::Label => PropertyTokenKey::Label,
                _ => return Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                    Dynamic::from(
                        "property() takes only T.id or T.label as a token key; write a property \
                         with a string key instead, for example property(\"name\", \"marko\")"
                            .to_string(),
                    ),
                    rhai::Position::NONE,
                ))),
            };
        let value = value_arg(&ctx, arg)?;
        Ok(this.add_step(Step::PropertyToken(key, value)))
    }

    #[cfg(feature = "serde")]
    /// Sets every key of the given map (or JSON object) as a property of the incoming vertex
    /// (Graphersal extension). Rhai binding of `property_json()`.
    pub fn property_json(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let max = access::value_depth_of(&ctx);
        let json = dynamic_to_json_value(arg, max, max)?;
        Ok(this.add_step(Step::PropertyJson(json)))
    }

    /// Direct-access JSONPath navigation (`.json_path(path)`). Parses `path`
    /// eagerly, mirroring `ArgByModule::JsonPath`, so a syntactically invalid path (recursive
    /// descent, wildcards, filter expressions - unsupported by this engine's JSONPath subset)
    /// surfaces as a script error at the call site instead of silently propagating to a later
    /// step.
    pub fn json_path(&mut self, path: String) -> Result<Self, Box<rhai::EvalAltResult>> {
        path.parse::<crate::value::JsonPath>().map_err(|err| {
            Box::new(rhai::EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                rhai::Position::NONE,
            ))
        })?;
        Ok(self.add_step(Step::JsonPath(CompactString::new(path))))
    }

    /// `.glob_path(pattern)`: a Gremlin extension that walks the tree below
    /// each incoming vertex along a glob pattern (see `GraphTraversalSource::glob_path`). Like
    /// every by()-modulated step, an invalid pattern or `by()` surfaces when the traversal
    /// executes, as `InvalidGlobPattern` / `InvalidModulator`.
    pub fn glob_path(&mut self, pattern: String) -> Self {
        self.add_step(Step::GlobPath(
            CompactString::new(pattern),
            ScriptGlobPathOptions::default(),
        ))
    }

    /// `.glob_path(pattern, #{max_depth: n, prune: __...})` `glob_path` with
    /// options (see `GraphTraversalSource::glob_path_with`). An unknown or reserved key, or a
    /// value of the wrong type, fails here with `ScriptError::ArgumentMismatch`; an out-of-range
    /// `max_depth` fails when the traversal executes, as `InvalidOption`.
    pub fn glob_path_with(
        &mut self,
        pattern: String,
        options: rhai::Map,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let options = glob_path_options(options)?;
        Ok(self.add_step(Step::GlobPath(CompactString::new(pattern), options)))
    }

    /// Keeps values equal to the given value or matching the predicate. Rhai binding of `is()`.
    pub fn is(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::Is(val)))
    }

    /// Graphersal form of where(predicate): compares the current object with a labelled one,
    /// modulated by by(). Rhai binding of `where_p()`.
    pub fn where_p(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::WhereP(val)))
    }

    /// Keeps a list whose every element matches the predicate (an empty list matches). Rhai binding
    /// of `all()`.
    pub fn all(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::All(val)))
    }

    /// Keeps a list where at least one element matches the predicate. Rhai binding of `any()`.
    pub fn any(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::Any(val)))
    }

    /// `none(P)`: keeps a list none of whose elements matches; see
    /// [`crate::GraphTraversalSource::none`].
    pub fn none(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::NoneP(val)))
    }

    /// `by()` with no argument: TinkerPop's identity modulator, the same as
    /// `by(__.identity())` (the traverser's own value; `group()` collects the members of a key
    /// as a list, as TinkerPop does for an identity value traversal).
    pub fn by_empty(&mut self) -> Self {
        let mut identity = self.clone();
        identity.steps = vec![Step::Identity];
        self.add_step(Step::By(ArgBy::Traversal(identity)))
    }

    /// Modulates the step before it: the key, token or child traversal a value is read with, and
    /// for order() the direction. Rhai binding of `by()`.
    pub fn by(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_by(arg)?;
        Ok(self.add_step(Step::By(val)))
    }

    /// `where("a", P)`: the predicate tests the object labelled `a`; see
    /// [`crate::GraphTraversalSource::where_label_p`].
    pub fn where_label_p(
        &mut self,
        start: &str,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let val = try_dynamic_to_argp(&arg)?;
        Ok(self.add_step(Step::WhereLabelP(start.into(), val)))
    }

    /// Graphersal form of where(traversal): keeps the traverser when the child traversal yields a
    /// result. Rhai binding of `where_t()`.
    pub fn where_t(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::WhereT(gts)))
    }

    /// Keeps the traverser when the child traversal yields a result, or when a labelled object
    /// matches a predicate on another label (see the book page where() Start and End Labels). Rhai
    /// binding of `where()`.
    pub fn where_step(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        if arg.is::<ScriptGraphTraversalSource>()
            || arg.type_name() == std::any::type_name::<ScriptGraphTraversalSource>()
        {
            self.where_t(arg)
        } else {
            self.where_p(arg)
        }
    }

    /// Emits the results of the child traversal, or the traverser itself when it yields nothing.
    /// Rhai binding of `optional()`.
    pub fn optional(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::Optional(gts)))
    }

    /// `local(traversal)`: runs `traversal` once per incoming traverser, in isolation from every
    /// other traverser in the batch. Vanilla TinkerPop has no camelCase variant for `local` (it's
    /// already a single word), so `local` is the whole dual-spelling story here.
    pub fn local(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::Local(gts)))
    }

    /// `map(traversal)`: the first result of the child; see [`crate::GraphTraversalSource::map`].
    pub fn map(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::MapT(gts)))
    }

    /// `flatMap(traversal)` / `flat_map(traversal)`: every result of the child; see
    /// [`crate::GraphTraversalSource::flat_map`].
    pub fn flat_map(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::FlatMapT(gts)))
    }

    /// `sideEffect(traversal)` / `side_effect(traversal)`: runs the child traversal for every
    /// incoming traverser and discards its output; the traverser continues unchanged.
    pub fn side_effect(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::SideEffect(gts)))
    }

    /// One-argument form of `and`/`or`/`coalesce`/`union`: a single traversal or an array of them.
    fn one_or_many(arg: Dynamic) -> Vec<Dynamic> {
        if arg.is_array() {
            arg.into_array().unwrap_or_default()
        } else {
            vec![arg]
        }
    }

    /// `and()` with no argument, the infix form `a.and().b`: the steps before it (back to the
    /// start of the traversal) and the steps after it become the operands of one `and()`
    /// (TinkerPop's `ConnectiveStrategy`; `and()` binds tighter than `or()`). The Rust API
    /// writes the prefix form, `and(vec![a, b])`.
    pub fn and_infix(&mut self) -> Self {
        self.add_step(Step::AndInfix)
    }

    /// `or()` with no argument, the infix form `a.or().b`; see [`Self::and_infix`].
    pub fn or_infix(&mut self) -> Self {
        self.add_step(Step::OrInfix)
    }

    /// `and(traversal)` / `and([traversals])`.
    pub fn and_one(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.and(Self::one_or_many(arg))
    }

    /// `or(traversal)` / `or([traversals])`.
    pub fn or_one(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.or(Self::one_or_many(arg))
    }

    /// `coalesce(traversal)` / `coalesce([traversals])`.
    pub fn coalesce_one(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.coalesce(Self::one_or_many(arg))
    }

    /// `union()` with no branch: produces nothing.
    pub fn union_empty(&mut self) -> Self {
        self.add_step(Step::Union(Vec::new()))
    }

    /// `union(traversal)` / `union([traversals])`.
    pub fn union_one(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.union(Self::one_or_many(arg))
    }

    /// Keeps the traverser when every child traversal yields a result. Rhai binding of `and()`.
    pub fn and(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_vec_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::And(gts)))
    }

    /// Runs the child traversals in order and emits the results of the first one that yields
    /// anything. Rhai binding of `coalesce()`.
    pub fn coalesce(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_vec_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::Coalesce(gts)))
    }

    /// Runs every child traversal on the incoming traversers and merges their results (see the book
    /// page Branch Children and Global State). Rhai binding of `union()`.
    pub fn union(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_vec_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::Union(gts)))
    }

    /// `filter(traversal)`: keeps a traverser when the child traversal yields a result.
    pub fn filter_t(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::FilterT(gts)))
    }

    /// Keeps the traverser when the child traversal yields nothing. Rhai binding of `not()`.
    pub fn not(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::Not(gts)))
    }

    /// Keeps the traverser when at least one child traversal yields a result. Rhai binding of
    /// `or()`.
    pub fn or(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_vec_dynamic_to_gts(arg)?;
        Ok(self.add_step(Step::Or(gts)))
    }
}

/// The `withSideEffect(name, graph)` target `graph` as the engine's `subgraph()` target: a
/// [`Graph<TraversalGraph>`](Graph) (`book/src/for_developers/custom_storage.md`). A graph of
/// another storage fails with the diagnostic of
/// [`GraphError::Unsupported`](crate::error::GraphError::Unsupported), naming its storage type.
fn subgraph_target(graph: &Arc<dyn AnyGraph>) -> Result<Arc<Graph>, Box<rhai::EvalAltResult>> {
    let storage = graph.storage_type_name();
    graph
        .clone()
        .into_any_arc()
        .downcast::<Graph>()
        .map_err(|_| {
            let diagnostic = crate::error::format_diagnostic(&crate::TraverserError::from(
                crate::error::GraphError::unsupported(crate::error::SUBGRAPH_TARGET),
            ));
            let text = match diagnostic.split_once("\nHelp: ") {
                Some((body, help)) => {
                    format!("{body} (the target graph's storage is {storage})\nHelp: {help}")
                }
                None => format!("{diagnostic} (the target graph's storage is {storage})"),
            };
            Box::new(rhai::EvalAltResult::ErrorRuntime(
                Dynamic::from(text),
                rhai::Position::NONE,
            ))
        })
}
