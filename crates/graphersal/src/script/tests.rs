use super::eval::RESERVED_PARAM_NAMES;
use super::*;

/// Bare constants `graph_scope` pushes that a parameter may deliberately shadow.
const SHADOWABLE_BARE_CONSTANTS: [&str; 8] = [
    "first", "last", "all", "mixed", "second", "minute", "hour", "day",
];

#[test]
fn reserved_param_names_match_graph_scope() {
    let scope = graph_scope(Arc::new(<Graph>::default()));
    let pushed: Vec<&str> = scope.iter().map(|(name, _, _)| name).collect();
    for name in &pushed {
        assert!(
            RESERVED_PARAM_NAMES.contains(name) || SHADOWABLE_BARE_CONSTANTS.contains(name),
            "graph_scope pushes '{name}' but RESERVED_PARAM_NAMES does not list it"
        );
    }
    for name in RESERVED_PARAM_NAMES {
        // `params` is bound per call by `eval_with_params`, not by `graph_scope`.
        assert!(
            name == "params" || pushed.contains(&name),
            "RESERVED_PARAM_NAMES lists '{name}' but graph_scope does not push it"
        );
    }
    for name in SHADOWABLE_BARE_CONSTANTS {
        assert!(pushed.contains(&name), "stale shadowable constant '{name}'");
    }
}

#[test]
fn test_registered_step_names() {
    let names = registered_step_names();
    assert!(names.contains(&"hasLabel".to_string()));
    assert!(names.contains(&"has_label".to_string()));
    assert!(names.contains(&"identity".to_string()));
    assert!(names.contains(&"V".to_string()));
    assert!(names.contains(&"v".to_string()));
    assert!(names.contains(&"in".to_string()));
    assert!(names.contains(&"where".to_string()));
    assert!(names.contains(&"union".to_string()));
    assert!(names.contains(&"infer_schema".to_string()));
    assert!(names.contains(&"visualize".to_string()));
}

#[test]
fn test_anonymous_traversal_scope_binding() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let engine = engine_with_limits(
        &ScriptLimits::default(),
        std::sync::Arc::new(crate::graph::AllowAll),
    );
    let mut scope =
        graph_scope_with_options(graph, &[], std::sync::Arc::new(crate::graph::AllowAll));
    let result = engine.eval_with_scope::<rhai::Array>(&mut scope, "__.v().count().to_list()");
    assert!(
        result.is_ok(),
        "Failed to evaluate anonymous traversal __ scope binding: {:?}",
        result.err()
    );
}

#[test]
fn test_capitalized_sources_and_clean_keywords() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let engine = engine_with_limits(
        &ScriptLimits::default(),
        std::sync::Arc::new(crate::graph::AllowAll),
    );
    let mut scope =
        graph_scope_with_options(graph, &[], std::sync::Arc::new(crate::graph::AllowAll));
    let script =
        r#"g.V().has_label("person").in("knows").as("a").values("age").is(P::Eq(29)).to_list()"#;
    let result = engine.eval_with_scope::<rhai::Array>(&mut scope, script);
    assert!(
        result.is_ok(),
        "Failed clean keywords evaluation: {:?}",
        result.err()
    );

    let script_e = r#"g.E().count().to_list()"#;
    let result_e = engine.eval_with_scope::<rhai::Array>(&mut scope, script_e);
    assert!(
        result_e.is_ok(),
        "Failed V/E capital sources evaluation: {:?}",
        result_e.err()
    );
}

#[test]
fn test_gremlin_camel_case_and_unified_where() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let engine = engine_with_limits(
        &ScriptLimits::default(),
        std::sync::Arc::new(crate::graph::AllowAll),
    );
    let mut scope =
        graph_scope_with_options(graph, &[], std::sync::Arc::new(crate::graph::AllowAll));

    let script1 = r#"g.V().hasLabel("person").outE("knows").elementMap().toList()"#;
    let res1 = engine.eval_with_scope::<rhai::Array>(&mut scope, script1);
    assert!(res1.is_ok(), "Failed query 1: {:?}", res1.err());

    let script2 = r#"g.V().where(__.hasLabel("person")).values("name").toList()"#;
    let res2 = engine.eval_with_scope::<rhai::Array>(&mut scope, script2);
    assert!(res2.is_ok(), "Failed query 2: {:?}", res2.err());

    let script3 = r#"g.V().hasId("1").in("knows").as("a").toList()"#;
    let res3 = engine.eval_with_scope::<rhai::Array>(&mut scope, script3);
    assert!(res3.is_ok(), "Failed query 3: {:?}", res3.err());
}

/// The traversal `script` builds (without running it): the step builders that convert script
/// values take the engine's call context, so they are called through the engine.
fn built(script: &str) -> ScriptGraphTraversalSource {
    let mut scope = graph_scope(Arc::new(<Graph>::default()));
    engine()
        .eval_with_scope::<Dynamic>(&mut scope, script)
        .expect("the script builds a traversal")
        .cast::<ScriptGraphTraversalSource>()
}

#[test]
fn test_is_mutating_coverage() {
    let mut source = ScriptGraphSource::empty();
    assert!(!source.is_mutating());

    // Read-only traversal
    let read_source = source
        .v_empty()
        .has_label(vec![Dynamic::from("person")])
        .expect("has_label")
        .values_single(Dynamic::from("name"))
        .expect("values");
    assert!(!read_source.is_mutating());

    // Mutating: add_v
    let add_v_source = source.add_v("person".to_string());
    assert!(add_v_source.is_mutating());

    let add_v_empty_source = source.add_v_empty();
    assert!(add_v_empty_source.is_mutating());

    // Mutating: add_e
    let add_e_source = source.add_e(Dynamic::from("knows".to_string())).unwrap();
    assert!(add_e_source.is_mutating());

    let add_e_empty_source = source.add_e_empty();
    assert!(add_e_empty_source.is_mutating());

    // Mutating: property (constant value)
    let prop_source = built(r#"__.property("name", "Alice")"#);
    assert!(prop_source.is_mutating());

    // Mutating: property (sub-traversal value)
    let prop_by_source = built(r#"__.property("name", __.V().values("name"))"#);
    assert!(prop_by_source.is_mutating());

    // Mutating: drop
    let drop_source = source.drop();
    assert!(drop_source.is_mutating());

    // repeat()/until()/emit() are mutating when their child traversal is.
    let mut mutating_child = built(r#"__.out().property("seen", true)"#);
    let read_child = ScriptGraphSource::empty().out_empty();
    let repeat_read = source
        .repeat(Dynamic::from(read_child.clone()))
        .expect("repeat() takes a traversal")
        .times(2)
        .expect("times(2) is valid");
    assert!(!repeat_read.is_mutating());
    let repeat_mut = source
        .repeat_named("a".to_string(), Dynamic::from(mutating_child.clone()))
        .expect("repeat() takes a traversal");
    assert!(repeat_mut.is_mutating());
    let until_mut = source
        .until(Dynamic::from(mutating_child.clone()))
        .expect("until() takes a traversal");
    assert!(until_mut.is_mutating());
    let emit_mut = source
        .emit(Dynamic::from(mutating_child.drop()))
        .expect("emit() takes a traversal");
    assert!(emit_mut.is_mutating());
    assert!(!source.emit_empty().is_mutating());
    // A sack is not graph state: withSack()/sack() never classify a plan as mutating.
    assert!(!built("__.withSack(1).sack()").is_mutating());

    // by() child traversals: mutating when the child is (directly or nested), read-only
    // otherwise; non-traversal by() arguments stay read-only.
    let by_read = source
        .v_empty()
        .project_single(Dynamic::from("a"))
        .expect("project")
        .by(Dynamic::from(ScriptGraphSource::empty().out_empty()))
        .expect("by(traversal)");
    assert!(!by_read.is_mutating());
    let by_prop = source
        .v_empty()
        .order()
        .by(Dynamic::from("age"))
        .expect("by(name)");
    assert!(!by_prop.is_mutating());
    let by_mut = source
        .v_empty()
        .project_single(Dynamic::from("a"))
        .expect("project")
        .by(Dynamic::from(
            ScriptGraphSource::empty().add_v("x".to_string()),
        ))
        .expect("by(traversal)");
    assert!(by_mut.is_mutating());
    let nested = ScriptGraphSource::empty()
        .local(Dynamic::from(
            ScriptGraphSource::empty().add_v("x".to_string()),
        ))
        .expect("local(traversal)");
    let by_nested = source
        .v_empty()
        .project_single(Dynamic::from("a"))
        .expect("project")
        .by(Dynamic::from(nested))
        .expect("by(traversal)");
    assert!(by_nested.is_mutating());

    // Mutating: property_json
    #[cfg(feature = "serde")]
    {
        let prop_json_source = built("__.property_json(#{})");
        assert!(prop_json_source.is_mutating());
    }
}

/// Upper bound for building [`ENGINES`] engines after the first one of the thread.
const ENGINES_BOUND: std::time::Duration = std::time::Duration::from_millis(50);
/// Number of engines [`building_an_engine_is_cheap`] builds.
const ENGINES: u32 = 50;

/// A timing guard, not a benchmark: the DSL registrations and Rhai's standard library are built
/// once per thread and attached to each new engine by pointer. Measured in the test profile
/// (Apple Silicon, 2026-10): the 50 engines took ~630 ms when every engine registered
/// everything itself and ~0.6 ms now, so the bound fails the old code by 12x and leaves the new
/// one an 80x margin. Not on wasm32 (no reliable clock under the test runner).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn building_an_engine_is_cheap() {
    if skip_on_ci("timing") {
        return;
    }
    let _ = engine();
    let start = web_time::Instant::now();
    for _ in 0..ENGINES {
        std::hint::black_box(engine());
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < ENGINES_BOUND,
        "{ENGINES} engines took {elapsed:?} (bound {ENGINES_BOUND:?})"
    );
}

/// The DSL text of every token a class of the graph scope offers, for
/// [`every_graph_scope_token_renders_as_its_dsl_literal`]: its getters (`Scope.local`) and its
/// methods called with a sample argument per parameter type (`P.gt(1)`, `Cardinality.single(1)`),
/// read from the engine's function signatures, so a new token or constructor is covered without
/// touching the test.
fn token_expressions_of(class: &str, marker_type: &str, signatures: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for signature in signatures {
        let Some((name, rest)) = signature.split_once('(') else {
            continue;
        };
        let Some((params, _)) = rest.split_once(") ->") else {
            continue;
        };
        let mut types = params
            .split(", ")
            .map(|param| param.trim_start_matches("_: ").trim_start_matches("&mut "));
        if types.next() != Some(marker_type) {
            continue;
        }
        if let Some(getter) = name.strip_prefix("get$") {
            // `By` names both the marker and the `by()` argument value, whose accessors these are.
            if class == "By" && matches!(getter, "enum_type" | "value") {
                continue;
            }
            out.push(format!("{class}.{getter}"));
            continue;
        }
        if matches!(name, "to_string" | "to_debug")
            || !name.starts_with(|c: char| c.is_ascii_alphabetic())
        {
            continue;
        }
        let args: Vec<&str> = types
            .map(|ty| match (ty, name) {
                ("string", "fromString" | "from_string") => {
                    "\"00000000-0000-0000-0000-00000000002a\""
                }
                ("string", "typeOf") => "\"Long\"",
                ("string", "JsonPath") => "\"a.b[1]\"",
                ("string", _) => "\"a\\\"b\"",
                ("GType", _) => "GType.LONG",
                ("GraphTraversalSource", _) => "__.out_e(\"knows\").has(\"age\", P.gt(30))",
                ("array", _) => "[1, \"a\"]",
                (_, "not") => "P.within(1, 2)",
                _ => "2.5",
            })
            .collect();
        out.push(format!("{class}.{name}({})", args.join(", ")));
    }
    out
}

/// A07: every token class pushed by `graph_scope()` renders as its DSL literal (rule 14): the
/// session visualizer, `to_string()` and `${..}` all print the same text, which never holds a
/// Rust path (`::`), and evaluating that text gives a value with the same text again.
#[test]
fn every_graph_scope_token_renders_as_its_dsl_literal() {
    let graph = Arc::new(<Graph>::default());
    let engine = engine();
    let signatures = engine.gen_fn_signatures(false);
    let scope = graph_scope(graph.clone());
    let eval = |script: &str| {
        eval_value(graph.clone(), script).unwrap_or_else(|err| panic!("{script}: {err}"))
    };

    let mut expressions: Vec<String> = Vec::new();
    for (name, _, value) in scope.iter() {
        if matches!(name, "g" | "_g") {
            continue; // graph sources: shown by running them
        }
        expressions.push(name.to_string());
        if name == "__" {
            expressions.push("__.out_e(\"knows\").has(\"age\", P.gt(30)).values(\"name\")".into());
            expressions.push("__.order().by(\"age\", Order.desc).limit(2)".into());
            continue;
        }
        let marker_type = engine.map_type_name(value.type_name()).to_string();
        let tokens = token_expressions_of(name, &marker_type, &signatures);
        let is_bare_constant = token_literal(&value).is_some_and(|text| text.contains('.'));
        assert!(
            is_bare_constant || !tokens.is_empty(),
            "the class {name} offers no token"
        );
        expressions.extend(tokens);
    }
    // Composites and the `::` spelling.
    expressions.extend(
        [
            "P.gt(1).and(P.lt(3)).or(P.eq(\"x\"))",
            "P.gt(1).negate()",
            "Scope::local",
            "Order::Desc",
            "T::id",
            "GType::STRING",
            "Pick::Unproductive",
            "Cardinality::list([1, 2])",
            "By.Traversal(__.out())",
            "ProfileType.Memory | ProfileType.Memory",
        ]
        .map(String::from),
    );

    let options = RenderOptions::default();
    for expression in &expressions {
        let value = eval(expression);
        if let Some(uuid) = value.read_lock::<ScriptUuid>() {
            // `UUID.fromString(..)` makes a value, not a token: it shows its canonical text.
            assert_eq!(debug_value(&value), uuid.canonical(), "{expression}");
            continue;
        }
        let Some(literal) = token_literal(&value) else {
            panic!("{expression} is not a token value: {}", value.type_name());
        };
        assert!(
            !literal.contains("::") && !literal.contains("graphersal"),
            "{expression} renders as {literal}"
        );
        let shown = render_value(value, VisualizeFormat::Auto, &options).text;
        assert_eq!(shown, literal, "session rendering of {expression}");
        let printed = eval(&format!("({expression}).to_string()")).to_string();
        assert_eq!(printed, literal, "to_string() of {expression}");
        let interpolated = eval(&format!("`${{{expression}}}`")).to_string();
        assert_eq!(interpolated, literal, "interpolation of {expression}");
        let again = eval(&literal);
        assert_eq!(
            token_literal(&again).as_deref(),
            Some(literal.as_str()),
            "{expression} renders as {literal}, which does not read back"
        );
    }
}

/// A07: nested tokens render as literals too, and the session's `render.spelling` respells the
/// step and predicate names in them like the steps of `.profile()`.
#[test]
fn nested_tokens_render_as_literals_in_the_session_spelling() {
    let graph = Arc::new(<Graph>::default());
    let value = eval_value(
        graph,
        "[#{p: TextP.starting_with(\"a\"), t: __.out_e()}, Scope.local, keys]",
    )
    .unwrap_or_else(|err| panic!("{err}"));
    let snake = render_value(
        value.clone(),
        VisualizeFormat::Auto,
        &RenderOptions::default(),
    );
    assert_eq!(
        snake.text,
        "#{\"p\": TextP.starting_with(\"a\"), \"t\": __.out_e()}\nScope.local\nColumn.keys"
    );
    let camel = render_value(
        value,
        VisualizeFormat::Auto,
        &RenderOptions::default().with_spelling(crate::traversal::Spelling::Camel),
    );
    assert_eq!(
        camel.text,
        "#{\"p\": TextP.startingWith(\"a\"), \"t\": __.outE()}\nScope.local\nColumn.keys"
    );
}

/// Whether this run is on a CI runner (`CI` set and not empty, "false" or "0", as GitHub Actions
/// sets it). A test whose result depends on how much CPU time or memory the machine gives it
/// (an upper bound on wall-clock time, a speed ratio, measured process memory) returns early
/// there, because a shared runner guarantees neither; such tests run locally, before every
/// release in particular. Prints why it skipped.
fn skip_on_ci(what: &str) -> bool {
    let on_ci = std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "false" && v != "0");
    if on_ci {
        eprintln!("skipped on CI ({what}): depends on the runner's CPU or memory");
    }
    on_ci
}
