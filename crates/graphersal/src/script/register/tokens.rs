//! Registration of the token classes in all their spellings (`X.y` on the scope markers,
//! `X::y` static modules, bare globals) and of the token value types (`ArgP`, `ArgBy`,
//! `ScriptUuid`, `ScriptSet`, `ScriptMap`, ...).

use super::Registry;
use crate::script::*;

/// Registers every token class.
pub(super) fn register(registry: &mut Registry) {
    registry
        .register_type_with_name::<ArgBy>("By")
        .register_static_module("By", exported_module!(ArgByModule).into());

    // The predicate value (`P.eq(1)`, `gt(2)`, `p.and(q)`). Not `P`: that is the name of the
    // `P` scope marker, and a predicate is a value, not the token class.
    registry.register_type_with_name::<ArgP>("Predicate");

    registry
        .register_type_with_name::<ScriptP>("P")
        .register_fn("eq", |_: ScriptP, val: Dynamic| ArgP::Eq(val))
        .register_fn("neq", |_: ScriptP, val: Dynamic| ArgP::Neq(val))
        .register_fn("lt", |_: ScriptP, val: Dynamic| ArgP::Lt(val))
        .register_fn("lte", |_: ScriptP, val: Dynamic| ArgP::Lte(val))
        .register_fn("gt", |_: ScriptP, val: Dynamic| ArgP::Gt(val))
        .register_fn("gte", |_: ScriptP, val: Dynamic| ArgP::Gte(val))
        .register_fn("inside", |_: ScriptP, v1: Dynamic, v2: Dynamic| {
            ArgP::Inside(v1, v2)
        })
        .register_fn("outside", |_: ScriptP, v1: Dynamic, v2: Dynamic| {
            ArgP::Outside(v1, v2)
        })
        .register_fn("between", |_: ScriptP, v1: Dynamic, v2: Dynamic| {
            ArgP::Between(v1, v2)
        })
        .register_fn("within", |_: ScriptP, val: Dynamic| script_p_within(val))
        .register_fn("without", |_: ScriptP, val: Dynamic| script_p_without(val))
        // `P.WithIn`/`P.WithOut` (dot, PascalCase): the same casing gap `Order.Desc` had before
        // its fix (`24f82a1`) — `P::WithIn`/`P::WithOut` (colon, via `ArgPModule` below) already
        // worked, but dot access only ever registered the lowercase Java-Gremlin spelling. Purely
        // additive: `P.within`, `P.WithIn`, `P::within`, `P::WithIn` all now produce identical
        // results (and likewise for `without`/`WithOut`).
        .register_fn("WithIn", |_: ScriptP, val: Dynamic| script_p_within(val))
        .register_fn("WithOut", |_: ScriptP, val: Dynamic| script_p_without(val))
        .register_fn_as(
            &["startingWith", "starting_with"],
            |_: ScriptP, val: Dynamic| ArgP::StartingWith(val),
        )
        .register_fn_as(
            &["notStartingWith", "not_starting_with"],
            |_: ScriptP, val: Dynamic| ArgP::NotStartingWith(val),
        )
        .register_fn_as(
            &["endingWith", "ending_with"],
            |_: ScriptP, val: Dynamic| ArgP::EndingWith(val),
        )
        .register_fn_as(
            &["notEndingWith", "not_ending_with"],
            |_: ScriptP, val: Dynamic| ArgP::NotEndingWith(val),
        )
        .register_fn("containing", |_: ScriptP, val: Dynamic| {
            ArgP::Containing(val)
        })
        .register_fn_as(
            &["notContaining", "not_containing"],
            |_: ScriptP, val: Dynamic| ArgP::NotContaining(val),
        )
        // Only the vanilla camelCase spelling exists here: `type_of` is a Rhai keyword and a
        // language built-in that reports a value's script type name, so it cannot be a step
        // name. The Rust API spells the same predicate `P::type_of`.
        .register_fn("typeOf", |_: ScriptP, target: GType| ArgP::TypeOf(target))
        .register_fn("typeOf", |_: ScriptP, name: &str| type_of_name(name))
        .register_fn("not", |_: ScriptP, inner: Dynamic| {
            predicate_operand("not", inner).map(|p| ArgP::Not(Box::new(p)))
        });

    // `P.within(1, 2, 3)`/`P.WithIn(...)`/`P.without(...)`/`P.WithOut(...)`: true positional
    // multi-arg predicate args (D2 of the true-variadic-overloads spec), matching the existing
    // single-value and array-literal (`P.within([1, 2, 3])`) forms rather than replacing them.
    register_variadic!(registry, ["within", "WithIn"], |_: ScriptP, args| {
        script_p_within(Dynamic::from_array(args))
    });
    register_variadic!(registry, ["without", "WithOut"], |_: ScriptP, args| {
        script_p_without(Dynamic::from_array(args))
    });

    // Predicate composition: `p.and(q)` / `p.or(q)` (one spelling in Gremlin). The operand is
    // checked here so a non-predicate fails with a `help()` rather than "function not found".
    registry
        .register_fn("and", |p: &mut ArgP, other: Dynamic| {
            predicate_operand("and", other).map(|q| ArgP::And(Box::new(p.clone()), Box::new(q)))
        })
        .register_fn("or", |p: &mut ArgP, other: Dynamic| {
            predicate_operand("or", other).map(|q| ArgP::Or(Box::new(p.clone()), Box::new(q)))
        });

    // The bare globals (`within(1, 2)`, `gt(30)`) are the lowercase `ArgPModule`; the static
    // module `P::` adds the PascalCase twins (`P::WithIn`, `P::Eq`), which are never bare
    // globals. The positional-arity overloads are added to both.
    let mut p_module = exported_module!(ArgPModule);
    register_variadic_module!(&mut p_module, ["within"], |args| script_p_within(
        Dynamic::from_array(args)
    ));
    register_variadic_module!(&mut p_module, ["without"], |args| {
        script_p_without(Dynamic::from_array(args))
    });
    // `P::not(p)` (colon form of the static `P.not(p)`), as the module-level twin of the
    // `ScriptP` registration above.
    p_module.set_native_fn("not", |inner: Dynamic| {
        predicate_operand("not", inner).map(|p| ArgP::Not(Box::new(p)))
    });
    let mut p_static = p_module.clone();
    p_static.combine(exported_module!(ArgPPascalModule));
    register_variadic_module!(&mut p_static, ["WithIn"], |args| script_p_within(
        Dynamic::from_array(args)
    ));
    register_variadic_module!(&mut p_static, ["WithOut"], |args| {
        script_p_without(Dynamic::from_array(args))
    });
    registry
        .register_static_module("P", p_static.into())
        .register_global_module(p_module.into());

    registry
        .register_type_with_name::<ScriptTextP>("TextP")
        .register_fn_as(
            &["startingWith", "starting_with"],
            |_: ScriptTextP, val: Dynamic| ArgP::StartingWith(val),
        )
        .register_fn_as(
            &["notStartingWith", "not_starting_with"],
            |_: ScriptTextP, val: Dynamic| ArgP::NotStartingWith(val),
        )
        .register_fn_as(
            &["endingWith", "ending_with"],
            |_: ScriptTextP, val: Dynamic| ArgP::EndingWith(val),
        )
        .register_fn_as(
            &["notEndingWith", "not_ending_with"],
            |_: ScriptTextP, val: Dynamic| ArgP::NotEndingWith(val),
        )
        .register_fn("containing", |_: ScriptTextP, val: Dynamic| {
            ArgP::Containing(val)
        })
        .register_fn("regex", |_: ScriptTextP, pattern: &str| {
            script_text_regex(pattern, false)
        })
        .register_fn_as(
            &["notRegex", "not_regex"],
            |_: ScriptTextP, pattern: &str| script_text_regex(pattern, true),
        )
        .register_fn_as(
            &["notContaining", "not_containing"],
            |_: ScriptTextP, val: Dynamic| ArgP::NotContaining(val),
        )
        .register_static_module("TextP", {
            let mut text_p = exported_module!(TextPModule);
            text_p.combine(exported_module!(TextPPascalModule));
            text_p.into()
        })
        .register_global_module(exported_module!(TextPModule).into());

    // `Column` tokens in all three notations: `Column.keys` (dot, on the scope marker),
    // `Column::keys` (static module) and the bare scope constants `keys` / `values`.
    registry
        .register_type_with_name::<Column>("Column")
        .register_type_with_name::<ScriptColumn>("ColumnTokens")
        .register_get("keys", |_: &mut ScriptColumn| Column::Keys)
        .register_get("values", |_: &mut ScriptColumn| Column::Values)
        .register_static_module("Column", exported_module!(ColumnModule).into());

    // `Scope` tokens: `Scope.local` / `Scope.global` (dot, on the scope marker, plus the
    // PascalCase `Scope.Local` / `Scope.Global`) and `Scope::local` / `Scope::global` (static
    // module). No bare constants (see `ScriptScope`).
    registry
        .register_type_with_name::<TraversalScope>("Scope")
        .register_type_with_name::<ScriptScope>("ScopeTokens")
        .register_get("local", |_: &mut ScriptScope| TraversalScope::Local)
        .register_get("global", |_: &mut ScriptScope| TraversalScope::Global)
        .register_get("Local", |_: &mut ScriptScope| TraversalScope::Local)
        .register_get("Global", |_: &mut ScriptScope| TraversalScope::Global)
        .register_static_module("Scope", exported_module!(ScopeModule).into());

    // `SchemaMode` tokens of `set_schema(schema, mode)`: `SchemaMode.none` / `.open` / `.closed`
    // (dot, on the scope marker) and `SchemaMode::none` ... (static module). No bare constants
    // (see `ScriptSchemaMode`); the schema methods also accept the strings.
    registry
        .register_type_with_name::<SchemaMode>("SchemaMode")
        .register_type_with_name::<ScriptSchemaMode>("SchemaModeTokens")
        .register_get("none", |_: &mut ScriptSchemaMode| SchemaMode::None)
        .register_get("open", |_: &mut ScriptSchemaMode| SchemaMode::Open)
        .register_get("closed", |_: &mut ScriptSchemaMode| SchemaMode::Closed)
        .register_static_module("SchemaMode", exported_module!(SchemaModeModule).into());

    // `Pop` tokens: `Pop.first` ... (dot, on the scope marker, plus the PascalCase `Pop.First` ...)
    // and `Pop::first` ... (static module); the bare `first` / `last` / `all` / `mixed` are scope
    // constants (see `graph_scope_with_options` and `ScriptPop`).
    registry
        .register_type_with_name::<Pop>("Pop")
        .register_type_with_name::<ScriptPop>("PopTokens")
        .register_get("first", |_: &mut ScriptPop| Pop::First)
        .register_get("last", |_: &mut ScriptPop| Pop::Last)
        .register_get("all", |_: &mut ScriptPop| Pop::All)
        .register_get("mixed", |_: &mut ScriptPop| Pop::Mixed)
        .register_get("First", |_: &mut ScriptPop| Pop::First)
        .register_get("Last", |_: &mut ScriptPop| Pop::Last)
        .register_get("All", |_: &mut ScriptPop| Pop::All)
        .register_get("Mixed", |_: &mut ScriptPop| Pop::Mixed)
        .register_static_module("Pop", exported_module!(PopModule).into());

    // `Cardinality` / `Direction` / `Merge` tokens: dot form on the scope markers, `::` form via
    // the static modules. No bare constants (explicit class prefixes only).
    // `Cardinality.single(v)` (dot call) wraps a value, like `Cardinality::single(v)`.
    registry
        .register_type_with_name::<Cardinality>("Cardinality")
        .register_type_with_name::<CardinalityValue>("CardinalityValue")
        .register_type_with_name::<ScriptCardinality>("CardinalityTokens")
        .register_get("single", |_: &mut ScriptCardinality| Cardinality::Single)
        .register_get("list", |_: &mut ScriptCardinality| Cardinality::List)
        .register_get("set", |_: &mut ScriptCardinality| Cardinality::Set)
        .register_fn("single", |_: &mut ScriptCardinality, value: Dynamic| {
            CardinalityValue::new(Cardinality::Single, value)
        })
        .register_fn("list", |_: &mut ScriptCardinality, value: Dynamic| {
            CardinalityValue::new(Cardinality::List, value)
        })
        .register_fn("set", |_: &mut ScriptCardinality, value: Dynamic| {
            CardinalityValue::new(Cardinality::Set, value)
        })
        .register_static_module("Cardinality", exported_module!(CardinalityModule).into());
    registry
        .register_type_with_name::<Direction>("Direction")
        .register_type_with_name::<ScriptDirection>("DirectionTokens")
        .register_get("OUT", |_: &mut ScriptDirection| Direction::Out)
        .register_get("IN", |_: &mut ScriptDirection| Direction::In)
        .register_get("BOTH", |_: &mut ScriptDirection| Direction::Both)
        .register_get("from", |_: &mut ScriptDirection| Direction::Out)
        .register_get("to", |_: &mut ScriptDirection| Direction::In)
        .register_static_module("Direction", exported_module!(DirectionModule).into());
    registry
        .register_type_with_name::<Merge>("Merge")
        .register_type_with_name::<ScriptMerge>("MergeTokens")
        .register_get("onCreate", |_: &mut ScriptMerge| Merge::OnCreate)
        .register_get("onMatch", |_: &mut ScriptMerge| Merge::OnMatch)
        .register_get("outV", |_: &mut ScriptMerge| Merge::OutV)
        .register_get("inV", |_: &mut ScriptMerge| Merge::InV)
        .register_static_module("Merge", exported_module!(MergeModule).into());
    // `Operator` tokens (`Operator.sum` dot form on the marker, `Operator::sum` static module).
    registry
        .register_type_with_name::<Operator>("Operator")
        .register_type_with_name::<ScriptOperator>("OperatorTokens")
        .register_get("sum", |_: &mut ScriptOperator| Operator::Sum)
        .register_get("minus", |_: &mut ScriptOperator| Operator::Minus)
        .register_get("mult", |_: &mut ScriptOperator| Operator::Mult)
        .register_get("div", |_: &mut ScriptOperator| Operator::Div)
        .register_get("max", |_: &mut ScriptOperator| Operator::Max)
        .register_get("min", |_: &mut ScriptOperator| Operator::Min)
        .register_get("assign", |_: &mut ScriptOperator| Operator::Assign)
        .register_get("and", |_: &mut ScriptOperator| Operator::And)
        .register_get("or", |_: &mut ScriptOperator| Operator::Or)
        .register_get("addAll", |_: &mut ScriptOperator| Operator::AddAll)
        .register_get("sumLong", |_: &mut ScriptOperator| Operator::SumLong)
        .register_static_module("Operator", exported_module!(OperatorModule).into());
    // `Barrier` token (`Barrier.normSack` dot form on the marker, `Barrier::normSack` module).
    registry
        .register_type_with_name::<Barrier>("Barrier")
        .register_type_with_name::<ScriptBarrier>("BarrierTokens")
        .register_get("normSack", |_: &mut ScriptBarrier| Barrier::NormSack)
        .register_static_module("Barrier", exported_module!(BarrierModule).into());
    // `WithOptions` token (`WithOptions.tokens` dot form on the marker, `WithOptions::tokens`
    // module) of the step-level `with()`.
    registry
        .register_type_with_name::<crate::value::WithOptions>("WithOptions")
        .register_type_with_name::<ScriptWithOptions>("WithOptionsTokens");
    for option in crate::value::WithOptions::ALL {
        registry.register_get(option.name(), move |_: &mut ScriptWithOptions| option);
    }
    registry.register_static_module("WithOptions", exported_module!(WithOptionsModule).into());

    register_set(registry);
    register_script_map(registry);
    registry
        .register_type_with_name::<GType>("GType")
        .register_type_with_name::<ScriptGType>("GTypeTokens")
        .register_static_module("GType", exported_module!(GTypeModule).into())
        .register_type_with_name::<CastPolicy>("CastPolicy")
        .register_static_module("CastPolicy", exported_module!(CastPolicyModule).into())
        .register_type_with_name::<Order>("Order")
        .register_static_module("Order", exported_module!(OrderModule).into())
        .register_type_with_name::<Pick>("Pick")
        .register_static_module("Pick", exported_module!(PickModule).into());

    // `UUID` literals in all three notations: `UUID.fromString(..)` / `UUID.from_string(..)`
    // (dot methods on the `UUID` scope marker) and `UUID::fromString(..)` / `UUID::from_string(..)`
    // (static module). The value type `Uuid` carries the bits natively, so a value read by a
    // terminal and fed back into a filter still matches the stored `Uuid`.
    registry
        .register_type_with_name::<ScriptUuidTokens>("UUIDTokens")
        .register_fn_as(
            &["fromString", "from_string"],
            |_: ScriptUuidTokens, value: &str| script_uuid_from_string(value),
        )
        .register_static_module("UUID", exported_module!(UuidModule).into())
        // `UUID()`: a random UUID (the Gremlin grammar's `UUID()`, Java `UUID.randomUUID()`).
        .register_fn("UUID", script_uuid_random)
        .register_type_with_name::<ScriptUuid>("Uuid")
        .register_fn_as(
            &["to_string", "to_debug", "as_string", "asString"],
            |uuid: &mut ScriptUuid| uuid.canonical(),
        )
        .register_fn("==", |a: ScriptUuid, b: ScriptUuid| a == b)
        .register_fn("!=", |a: ScriptUuid, b: ScriptUuid| a != b)
        // A UUID never equals a string (TinkerGraph: `UUID.equals(String)` is false). Registered
        // explicitly so the comparison answers instead of failing with "function not found".
        .register_fn("==", |_: ScriptUuid, _: rhai::ImmutableString| false)
        .register_fn("==", |_: rhai::ImmutableString, _: ScriptUuid| false)
        .register_fn("!=", |_: ScriptUuid, _: rhai::ImmutableString| true)
        .register_fn("!=", |_: rhai::ImmutableString, _: ScriptUuid| true);

    // Dot access for `By`/`CastPolicy` (`By.Label`, `CastPolicy.Skip`): a pre-existing DSL-parity
    // gap found while scoping the `ProfileType` bitmask spec — `P`/`GType`/`TextP`/`V` already had
    // this dual registration (properties/methods on a scope marker, alongside the `::` static
    // module above), but `By`/`CastPolicy` only ever got the `::` half. Fixed here, same session.
    registry
        .register_type_with_name::<ScriptBy>("By")
        .register_get("Label", |_: &mut ScriptBy| ArgBy::Label)
        .register_get("Id", |_: &mut ScriptBy| ArgBy::Id)
        .register_get("Value", |_: &mut ScriptBy| ArgBy::Value)
        .register_get("Count", |_: &mut ScriptBy| ArgBy::Count)
        .register_fn("Property", |_: ScriptBy, prop: String| {
            ArgBy::Property(CompactString::new(prop))
        })
        .register_fn(
            "Traversal",
            |_: ScriptBy, prop: ScriptGraphTraversalSource| ArgBy::Traversal(prop),
        )
        .register_fn(
            "JsonPath",
            |_: ScriptBy, path: String| -> Result<ArgBy, Box<rhai::EvalAltResult>> {
                path.parse::<crate::value::JsonPath>()
                    .map(ArgBy::JsonPath)
                    .map_err(|err| {
                        Box::new(rhai::EvalAltResult::ErrorRuntime(
                            rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                            rhai::Position::NONE,
                        ))
                    })
            },
        );

    registry
        .register_type_with_name::<ScriptCastPolicy>("CastPolicy")
        .register_get("Error", |_: &mut ScriptCastPolicy| CastPolicy::Error)
        .register_get("Skip", |_: &mut ScriptCastPolicy| CastPolicy::Skip)
        .register_fn(
            "Default",
            |ctx: NativeCallContext, _: ScriptCastPolicy, value: Dynamic| {
                Ok::<_, Box<rhai::EvalAltResult>>(CastPolicy::Default(value_arg(&ctx, value)?))
            },
        );

    // `Order` never had either half of the dual registration before the `order()` comparison-
    // infra spec (part 1 of the `order()` series): `Order::Asc`/`Order::Desc` (`::`, via
    // `OrderModule` above) and `Order.asc`/`Order.desc` (dot, via `ScriptOrder` here) are added
    // together, following the exact `By`/`CastPolicy` precedent rather than inventing a new
    // mechanism. Lowercase `asc`/`desc` match the Gremlin-Groovy convention for `Order`'s own
    // dot-accessible tokens (`Order.asc`/`Order.desc`), same as the crate's step names elsewhere
    // being snake_case aliases of their PascalCase Rust counterparts.
    //
    // `Order` is the one dual-registered token in this crate where dot- and `::`-access
    // deliberately used different casing (lowercase for real Gremlin-Groovy compatibility,
    // PascalCase to match the Rust `Order::Asc`/`Order::Desc` variant names) — every sibling
    // token (`By`, `CastPolicy`, `GType`, `ProfileType`) already uses the *same* casing for both
    // access forms, since none of them has a real Gremlin-Groovy spelling pulling it toward
    // lowercase the way `Order` does. That mismatch meant a mechanical `::` -> `.` substitution
    // on copied Rust-style code (`Order::Desc` -> `Order.Desc`) silently failed. Also register
    // the PascalCase spellings for dot access so both substitution directions work.
    registry
        .register_type_with_name::<ScriptOrder>("Order")
        .register_get("asc", |_: &mut ScriptOrder| Order::Asc)
        .register_get("desc", |_: &mut ScriptOrder| Order::Desc)
        .register_get("Asc", |_: &mut ScriptOrder| Order::Asc)
        .register_get("Desc", |_: &mut ScriptOrder| Order::Desc)
        .register_get("shuffle", |_: &mut ScriptOrder| Order::Shuffle)
        .register_get("Shuffle", |_: &mut ScriptOrder| Order::Shuffle);

    // `Pick.any`/`Pick.none`: same dual-registration treatment as `Order` above, lowercase to
    // match real Gremlin-Groovy's own `Pick.any`/`Pick.none` spelling (`Pick::Any`/`Pick::None`,
    // PascalCase, is the `::`-access half, via `PickModule` above).
    registry
        .register_type_with_name::<ScriptPick>("Pick")
        .register_get("any", |_: &mut ScriptPick| Pick::Any)
        .register_get("none", |_: &mut ScriptPick| Pick::None)
        .register_get("unproductive", |_: &mut ScriptPick| Pick::Unproductive)
        .register_get("Unproductive", |_: &mut ScriptPick| Pick::Unproductive)
        .register_get("Any", |_: &mut ScriptPick| Pick::Any)
        .register_get("None", |_: &mut ScriptPick| Pick::None);

    // TinkerPop's `T` token: `T.id`/`T.label`/`T.key`/`T.value` (dot, via the `ScriptT` scope
    // marker) and `T::id`/... (`::`, via the static module). Lowercase only, matching
    // Gremlin-Groovy's own spelling; the values are the same `ArgBy::Id`/`ArgBy::Label`/
    // `ArgBy::Value` that `By.Id`/`By.Label`/`By.Value` produce, so every `by()` consumer accepts
    // them unchanged. `T.key` reads a property's key (`ArgBy::Key`).
    let mut t_module = Module::new();
    t_module.set_var("id", ArgBy::Id);
    t_module.set_var("label", ArgBy::Label);
    t_module.set_var("key", ArgBy::Key);
    t_module.set_var("value", ArgBy::Value);
    registry
        .register_type_with_name::<ScriptT>("TTokens")
        .register_get("id", |_: &mut ScriptT| ArgBy::Id)
        .register_get("label", |_: &mut ScriptT| ArgBy::Label)
        .register_get("key", |_: &mut ScriptT| ArgBy::Key)
        .register_get("value", |_: &mut ScriptT| ArgBy::Value)
        .register_static_module("T", t_module.into());

    // `ProfileType` gets the same dual registration from the start: `ProfileType.Memory` (dot,
    // via `ScriptProfileType`) and `ProfileType::Memory` (`::`, via the static module below) both
    // work, matching `V`'s own full treatment rather than repeating the `By`/`CastPolicy` gap.
    let mut profile_type_module = Module::new();
    profile_type_module.set_var("Memory", ProfileType::Memory);
    registry
        .register_type_with_name::<ProfileType>("ProfileType")
        .register_fn_as(&["to_string", "to_debug"], |p: &mut ProfileType| {
            crate::value::literal::TokenLiteral::token_literal(&*p)
        })
        // `bitmask_enum`'s `BitOr` impl is a plain Rust trait, which Rhai does not pick up
        // automatically for a registered custom type — the operator needs its own registration,
        // proven out now (with the crate's one flag) so it already works once a second flag
        // (`Network`/`Disk`) exists and `ProfileType::Memory | ProfileType::Network` becomes a
        // real, meaningful combination.
        .register_fn("|", |a: ProfileType, b: ProfileType| a | b)
        .register_type_with_name::<ScriptProfileType>("ProfileTypeTokens")
        .register_get("Memory", |_: &mut ScriptProfileType| ProfileType::Memory)
        .register_static_module("ProfileType", profile_type_module.into());

    register_gtype_tokens!(
        registry;
        NULL, BOOLEAN, INT, LONG, FLOAT, DOUBLE, NUMBER, STRING, UUID, DATETIME, DURATION, DATE,
        LOCALDATETIME, TIME, OFFSETTIME, VECTOR, LIST, MAP, VERTEX, EDGE, PATH, GRAPH,
    );

    // Dates and times: the value types, constructors, `now()`/`today()`, and the `DT` token.
    crate::script::tokens::register_temporal(registry);

    // Vectors: the value type, `vector([..])` / `vector([..], "i8")` and its methods.
    crate::script::tokens::register_vector(registry);

    // Global Predicate Functions
    registry
        .register_fn("eq", |val: Dynamic| ArgP::Eq(val))
        .register_fn("neq", |val: Dynamic| ArgP::Neq(val))
        .register_fn("lt", |val: Dynamic| ArgP::Lt(val))
        .register_fn("lte", |val: Dynamic| ArgP::Lte(val))
        .register_fn("gt", |val: Dynamic| ArgP::Gt(val))
        .register_fn("gte", |val: Dynamic| ArgP::Gte(val))
        .register_fn("inside", |v1: Dynamic, v2: Dynamic| ArgP::Inside(v1, v2))
        .register_fn("outside", |v1: Dynamic, v2: Dynamic| ArgP::Outside(v1, v2))
        .register_fn("between", |v1: Dynamic, v2: Dynamic| ArgP::Between(v1, v2))
        .register_fn("within", script_p_within)
        .register_fn("without", script_p_without)
        .register_fn_as(&["startingWith", "starting_with"], |val: Dynamic| {
            ArgP::StartingWith(val)
        })
        .register_fn("notStartingWith", |val: Dynamic| ArgP::NotStartingWith(val))
        .register_fn("not_starting_with", |val: Dynamic| {
            ArgP::NotStartingWith(val)
        })
        .register_fn_as(&["endingWith", "ending_with"], |val: Dynamic| {
            ArgP::EndingWith(val)
        })
        .register_fn_as(&["notEndingWith", "not_ending_with"], |val: Dynamic| {
            ArgP::NotEndingWith(val)
        })
        .register_fn("containing", |val: Dynamic| ArgP::Containing(val))
        .register_fn("regex", |pattern: &str| script_text_regex(pattern, false))
        .register_fn_as(&["notRegex", "not_regex"], |pattern: &str| {
            script_text_regex(pattern, true)
        })
        .register_fn_as(&["notContaining", "not_containing"], |val: Dynamic| {
            ArgP::NotContaining(val)
        })
        .register_fn("typeOf", |target: GType| ArgP::TypeOf(target))
        .register_fn("typeOf", type_of_name)
        .register_fn("not", |inner: Dynamic| {
            predicate_operand("not", inner).map(|p| ArgP::Not(Box::new(p)))
        });

    registry
        .register_type_with_name::<ScriptV>("V")
        .register_get("Auto", |_: &mut ScriptV| VisualizeFormat::Auto)
        .register_get("Table", |_: &mut ScriptV| VisualizeFormat::Table)
        .register_get("Markdown", |_: &mut ScriptV| VisualizeFormat::Markdown)
        .register_get("Json", |_: &mut ScriptV| VisualizeFormat::Json)
        .register_get("JsonSchema", |_: &mut ScriptV| VisualizeFormat::JsonSchema)
        .register_get("Mermaid", |_: &mut ScriptV| VisualizeFormat::Mermaid)
        .register_get("PlantUML", |_: &mut ScriptV| VisualizeFormat::PlantUML)
        .register_get("Tree", |_: &mut ScriptV| VisualizeFormat::Tree)
        .register_fn("Auto", |_: ScriptV| VisualizeFormat::Auto)
        .register_fn("Table", |_: ScriptV| VisualizeFormat::Table)
        .register_fn("Markdown", |_: ScriptV| VisualizeFormat::Markdown)
        .register_fn("Json", |_: ScriptV| VisualizeFormat::Json)
        .register_fn("JsonSchema", |_: ScriptV| VisualizeFormat::JsonSchema)
        .register_fn("Mermaid", |_: ScriptV| VisualizeFormat::Mermaid)
        .register_fn("PlantUML", |_: ScriptV| VisualizeFormat::PlantUML)
        .register_fn("Tree", |_: ScriptV| VisualizeFormat::Tree)
        .register_static_module("V", exported_module!(script_v_module).into());

    registry
        .register_type_with_name::<ScriptV>("V")
        .register_global_module(exported_module!(script_v_module).into());

    // The scope markers (`Scope`, `P`, ...) and a `Set.of(..)` seed print as their DSL text;
    // registered last, after every type name, so the metadata names their types.
    macro_rules! register_literals {
        ($($ty:ty),* $(,)?) => {
            $(
                registry.register_fn_as(&["to_string", "to_debug"], |value: &mut $ty| {
                    crate::value::literal::TokenLiteral::token_literal(&*value)
                });
            )*
        };
    }
    crate::script::tokens::for_each_marker!(register_literals);
    register_literals!(ScriptSet);
}

/// Registers `Set.of(a, b, ...)` / `Set::of(..)` for 0..=10 arguments and `Set.ofList([..])` /
/// `Set::of_list([..])` (native functions have a fixed arity, so each count is its own overload).
fn register_set(registry: &mut Registry) {
    let mut module = rhai::Module::new();
    macro_rules! set_of {
        ($($arg:ident),*) => {{
            registry.register_fn("of", |_: ScriptSetTokens, $($arg: Dynamic),*| script_set_from(vec![$($arg),*]));
            module.set_native_fn("of", |$($arg: Dynamic),*| script_set_from(vec![$($arg),*]));
        }};
    }
    set_of!();
    set_of!(a);
    set_of!(a, b);
    set_of!(a, b, c);
    set_of!(a, b, c, d);
    set_of!(a, b, c, d, e);
    set_of!(a, b, c, d, e, f);
    set_of!(a, b, c, d, e, f, g);
    set_of!(a, b, c, d, e, f, g, h);
    set_of!(a, b, c, d, e, f, g, h, i);
    set_of!(a, b, c, d, e, f, g, h, i, j);
    for name in ["ofList", "of_list"] {
        registry.register_fn(name, |_: ScriptSetTokens, items: rhai::Array| {
            script_set_from(items)
        });
        module.set_native_fn(name, |items: rhai::Array| script_set_from(items));
    }
    registry
        .register_type_with_name::<ScriptSetTokens>("SetTokens")
        .register_type_with_name::<ScriptSet>("Set")
        .register_static_module("Set", module.into());
}

/// Registers the [`ScriptMap`] type: indexer (any key, and a string fallback for `m.name`), `len`,
/// `is_empty`/`isEmpty`, `keys`, `values`, `contains`, `containsKey`/`contains_key`, `to_map`/`toMap`,
/// printing, equality and `for pair in m`.
fn register_script_map(registry: &mut Registry) {
    registry
        .register_type_with_name::<ScriptMap>("Map")
        .register_indexer_get(
            |ctx: NativeCallContext,
             map: &mut ScriptMap,
             key: Dynamic|
             -> Result<Dynamic, Box<rhai::EvalAltResult>> {
                Ok(map
                    .0
                    .get(&value_arg(&ctx, key)?)
                    .map_or(Dynamic::UNIT, |value| {
                        element_property_to_dynamic(value.clone())
                    }))
            },
        )
        .register_indexer_get(
            |map: &mut ScriptMap, key: rhai::ImmutableString| -> Dynamic {
                map.0.get_str(key.as_str()).map_or(Dynamic::UNIT, |value| {
                    element_property_to_dynamic(value.clone())
                })
            },
        )
        .register_fn("len", |map: &mut ScriptMap| map.0.len() as rhai::INT)
        .register_fn_as(&["is_empty", "isEmpty"], |map: &mut ScriptMap| {
            map.0.is_empty()
        })
        .register_fn("keys", |map: &mut ScriptMap| -> rhai::Array {
            map.0
                .keys()
                .cloned()
                .map(element_property_to_dynamic)
                .collect()
        })
        .register_fn("values", |map: &mut ScriptMap| -> rhai::Array {
            map.0
                .values()
                .cloned()
                .map(element_property_to_dynamic)
                .collect()
        })
        .register_fn_as(
            &["contains", "containsKey", "contains_key"],
            |ctx: NativeCallContext,
             map: &mut ScriptMap,
             key: Dynamic|
             -> Result<bool, Box<rhai::EvalAltResult>> {
                Ok(map.0.get(&value_arg(&ctx, key)?).is_some())
            },
        )
        .register_fn_as(&["to_map", "toMap"], |map: &mut ScriptMap| {
            script_map_to_rhai_map(map)
        })
        .register_fn_as(&["to_string", "to_debug"], |map: &mut ScriptMap| {
            script_map_text(map, &crate::traversal::render::ItemLimit::unlimited())
        })
        .register_fn("==", |a: ScriptMap, b: ScriptMap| a == b)
        .register_fn("!=", |a: ScriptMap, b: ScriptMap| a != b)
        .register_iterator::<ScriptMap>();
}

/// `P.typeOf(name)`: the predicate for a TinkerPop type name (`"String"`, `"Long"`, ...; see
/// [`GType::from_type_name`]); an unknown name is a script error.
fn type_of_name(name: &str) -> Result<ArgP, Box<rhai::EvalAltResult>> {
    match GType::from_type_name(name) {
        Some(target) => Ok(ArgP::TypeOf(target)),
        None => Err(script_error_from_script(
            crate::error::ScriptError::ArgumentMismatch {
                step_name: "typeOf".to_string(),
                expected: format!(
                    "a GType token (GType.STRING, ...) or a registered type name ({})",
                    GType::TYPE_NAMES.join(", ")
                ),
                actual: format!("the unknown type name \"{name}\""),
            },
        )),
    }
}
