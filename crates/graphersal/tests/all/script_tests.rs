//! Integration tests for script evaluation (Rhai Gremlin DSL) in `graphersal`.

#[cfg(feature = "script")]
mod tests {
    use crate::script_host::{
        ScriptGraphSource, eval, eval_value, eval_value_with_params, eval_with_params,
        eval_with_positional_params,
    };
    use graphersal::exec::RenderOptions;
    use graphersal::prelude::Number;
    use graphersal::prelude::*;
    use graphersal::schema::{PropertySchema, ValueType};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Tests evaluation of vertex creation and count traversal via Rhai script DSL.
    #[test]
    fn test_script_eval_vertex_creation() {
        let graph = Arc::new(GraphSource::empty());

        let script = r#"
            g.addV("person").property("name", "Charlie").toList();
            g.V().count().toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("1") || output.contains("[1]"),
            "Unexpected output: {}",
            output
        );
    }

    /// Tests evaluation of vertex filtering and property extraction via Rhai script DSL.
    #[test]
    fn test_script_eval_filtering() {
        let graph = Arc::new(GraphSource::empty());

        let script = r#"
            g.addV("person").property("name", "Alice").toList();
            g.addV("software").property("name", "Graphersal").toList();
            g.V().hasLabel("software").values("name").toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("Graphersal"),
            "Output should contain Graphersal: {}",
            output
        );
    }

    /// Tests evaluation using predefined tinkerpop modern graph dataset.
    #[test]
    fn test_script_eval_tinkerpop_modern() {
        let _g_src = ScriptGraphSource::tinkerpop_modern();
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"g.V().hasLabel("person").out("knows").values("name").toList()"#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("vadas") || output.contains("josh"),
            "Output should contain vadas or josh: {}",
            output
        );
    }

    /// Tests edge creation between vertices using label aliasing (`as`, `from`, `to`).
    #[test]
    fn test_script_eval_edge_creation() {
        let graph = Arc::new(GraphSource::empty());

        let script = r#"
            g.addV("person").as("a").addV("person").as("b").addE("knows").from("a").to("b").toList();
            g.E().count().toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("1") || output.contains("[1]"),
            "Unexpected edge count output: {}",
            output
        );
    }

    /// Tests mutating an existing vertex property via script DSL.
    #[test]
    fn test_script_eval_property_mutation() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"
            g.V("1").property("age", 35).toList();
            g.V("1").values("age").toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("35"),
            "Output should contain updated age 35: {}",
            output
        );
    }

    /// Rule 3: every new mutating `property()` form writes through the DSL, with a terminal and
    /// as a bare (rendered) traversal.
    #[test]
    fn test_script_eval_property_cardinality_map_and_token_mutations() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            g.addV("person").property(T.id, "p1").property(T.label, "human").toList();
            g.V("p1").property(Cardinality.single, "name", "a").toList();
            g.V("p1").property(Cardinality::single, "age", __.constant(5)).toList();
            g.V("p1").property(#{"city": "x", "zip": Cardinality.single(1)}).toList();
            g.V("p1").property(Cardinality.single, #{"code": 7}).toList();
            g.V("p1").addE("self").toList();
            g.V("p1").hasLabel("human").has("name", "a").has("age", 5).has("city", "x")
                .has("zip", 1).has("code", 7).out("self").count().next()
        "#;
        let result = eval_value(graph.clone(), script).expect("mutations must run");
        assert_eq!(result.as_int().ok(), Some(1));

        // A bare mutating traversal as the last statement is executed by the visualizer.
        for bare in [
            r#"g.V("p1").property(Cardinality.single, "bare1", 1)"#,
            r#"g.V("p1").property(#{"bare2": 2})"#,
            r#"g.addV().property(T.id, "p2")"#,
        ] {
            eval(graph.clone(), bare).expect("bare mutation must run");
        }
        let check = r#"g.V("p1").has("bare1", 1).has("bare2", 2).count().next() + g.V("p2").count().next()"#;
        assert_eq!(eval_value(graph, check).unwrap().as_int().ok(), Some(2));
    }

    /// Rule 3: `property(jpath(..), v)` (and its traversal-value form) mutates through the DSL,
    /// with a terminal and as a bare (rendered) traversal, and persists across evaluations.
    #[test]
    fn test_script_eval_property_jpath_mutations() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            g.addV("person").property(T.id, "p1").property(jpath("meta.tags[0]"), "x").toList()
        "#;
        // `tags` does not exist yet: an index never creates an array. The failing traversal is
        // rolled back, so the vertex it added is gone again.
        assert!(eval_value(graph.clone(), script).is_err());
        let gone = eval_value(graph.clone(), r#"g.V("p1").count().next()"#).unwrap();
        assert_eq!(gone.as_int().ok(), Some(0));
        let script = r#"
            g.addV("person").property(T.id, "p1").toList();
            g.V("p1").property(jpath("meta.k"), 1).toList();
            g.V("p1").property(jpath("meta.tags"), ["a"]).toList();
            g.V("p1").property(jpath("meta.tags[1]"), "b").toList();
            g.V("p1").property(jpath("meta.copy"), __.values(jpath("meta.k"))).toList();
            g.V("p1").has(jpath("meta.tags[1]"), "b").has(jpath("meta.copy"), 1).count().next()
        "#;
        let result = eval_value(graph.clone(), script).expect("mutations must run");
        assert_eq!(result.as_int().ok(), Some(1));

        // A bare mutating traversal as the last statement is executed by the visualizer.
        eval(
            graph.clone(),
            r#"g.V("p1").property(jpath("meta.bare"), 2)"#,
        )
        .expect("bare mutation must run");
        let check = r#"g.V("p1").has(jpath("meta.bare"), 2).count().next()"#;
        assert_eq!(eval_value(graph, check).unwrap().as_int().ok(), Some(1));
    }

    /// Rule 3: `remove_property(jpath(..))` / `removeProperty(..)` mutate through the DSL, with a
    /// terminal and as a bare (rendered) traversal; a missing path is a no-op, not an error.
    #[test]
    fn test_script_eval_remove_property_jpath_mutations() {
        let graph = Arc::new(GraphSource::empty());
        let setup = r#"
            g.addV("person").property(T.id, "p1").property_json(#{meta: #{k: 1, tags: ["a", "b", "c"]}}).toList();
            g.V("p1").remove_property(jpath("meta.k")).toList();
            g.V("p1").removeProperty(jpath("meta.tags[0]")).toList();
            g.V("p1").remove_property(jpath("meta.absent[3]")).toList();
            g.V("p1").has(jpath("meta.tags[0]"), "b").hasNot(jpath("meta.k")).count().next()
        "#;
        let result = eval_value(graph.clone(), setup).expect("removals must run");
        assert_eq!(result.as_int().ok(), Some(1));

        // A bare mutating traversal as the last statement is executed by the visualizer.
        eval(
            graph.clone(),
            r#"g.V("p1").remove_property(jpath("meta.tags[-1]"))"#,
        )
        .expect("bare removal must run");
        let check = r#"g.V("p1").values(jpath("meta.tags[0]")).toList().len() + g.V("p1").has(jpath("meta.tags[1]")).count().next()"#;
        assert_eq!(eval_value(graph, check).unwrap().as_int().ok(), Some(1));
    }

    /// Rule 3: the label steps `add_label`/`addLabel`, `drop_label`/`dropLabel` (vertices) and
    /// `set_label`/`setLabel` (edges) mutate through the DSL in both spellings, with a terminal
    /// and as a bare (rendered) traversal, and inside `__`.
    #[test]
    fn test_script_eval_label_steps_in_both_spellings() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script = r#"
            g.V("1").add_label("employee").toList();
            g.V("2").addLabel("employee").addLabel("employee").toList();
            g.V("4").add_label("a").add_label("b").drop_label("person").toList();
            g.V("6").dropLabel("person").toList();
            g.E("0").set_label("met").toList();
            g.E("1").setLabel("met").setLabel("met").toList();
            [
                g.V().has_label("employee").count().next(),
                g.V("1").labels().next(),
                g.V("4").label().next(),
                g.V("4").labels().next(),
                g.V("6").labels().next(),
                g.V().has_label("person").count().next(),
                g.E().has_label("met").count().next(),
                g.E().has_label("knows").count().next(),
            ]
        "#;
        let out = eval_value(graph.clone(), script).unwrap().to_string();
        assert_eq!(
            out,
            r#"[2, ["person", "employee"], "a", ["a", "b"], [], 2, 2, 1]"#
        );

        // A bare mutating traversal as the last statement is executed by the visualizer; a
        // child traversal relabels too.
        eval(graph.clone(), r#"g.V("3").add_label("tool")"#).expect("bare add_label");
        eval(graph.clone(), r#"g.V("5").local(__.addLabel("tool"))"#).expect("anonymous");
        eval(graph.clone(), r#"g.V("5").out_e().set_label("x")"#).expect("bare set_label");
        let check = r#"g.V().has_label("tool").count().next()"#;
        assert_eq!(
            eval_value(graph.clone(), check).unwrap().as_int().ok(),
            Some(2)
        );

        // A vertex's bulk-n traverser changes it once; the step passes its element on.
        let bulk = r#"g.V().out("created").add_label("made").dedup().count().next()"#;
        assert_eq!(
            eval_value(graph.clone(), bulk).unwrap().as_int().ok(),
            Some(2)
        );
        let labels = r#"g.V("3").labels().next()"#;
        assert_eq!(
            eval_value(graph, labels).unwrap().to_string(),
            r#"["software", "tool", "made"]"#
        );
    }

    /// A failing traversal rolls its unit back: the label changes made before the failure are
    /// undone, for vertices and edges.
    #[test]
    fn test_script_eval_label_steps_roll_back_with_a_failing_traversal() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let err = eval_value(
            graph.clone(),
            r#"g.V("1").add_label("x").drop_label("person").out_e("knows").set_label("met").in_v().set_label("boom").toList()"#,
        )
        .expect_err("set_label on a vertex must fail");
        assert!(err.to_string().contains("set_label"), "{err}");
        let check = r#"[g.V("1").labels().next(), g.E().has_label("met").count().next()]"#;
        assert_eq!(
            eval_value(graph, check).unwrap().to_string(),
            r#"[["person"], 0]"#
        );
    }

    /// The freeze recipe of `book/src/users_guide/schema_enforcement.md`: unlabeled vertices
    /// and edges block a `closed` freeze; labelled with `add_label`/`set_label`, the freeze of
    /// the inferred schema succeeds.
    #[test]
    fn test_script_eval_label_steps_make_a_closed_freeze_succeed() {
        let mut inner = TraversalGraph::new();
        let a = inner
            .add_vertex(Some("a".into()), vec!["a".into()], Default::default())
            .unwrap();
        let bare = inner
            .add_vertex(Some("bare".into()), vec![], Default::default())
            .unwrap();
        inner
            .add_edge(Some("nolabel".into()), None, a, bare, Default::default())
            .unwrap();
        let graph = Arc::new(Graph::new(inner));
        let freeze = r#"g.set_schema(g.infer_schema(), SchemaMode.closed)"#;
        let err = eval_value(graph.clone(), freeze).expect_err("unlabeled elements");
        assert!(err.to_string().contains("add_label"), "{err}");

        let fix = r#"
            g.v().not(__.has_label()).add_label("thing").toList();
            g.e().not(__.has_label()).set_label("link").toList();
            g.set_schema(g.infer_schema(), SchemaMode.closed);
            [g.get_schema().mode, g.v("bare").label().next(), g.e("nolabel").label().next()]
        "#;
        assert_eq!(
            eval_value(graph.clone(), fix).unwrap().to_string(),
            r#"["closed", "thing", "link"]"#
        );
        // The closed schema now guards the labels: an undeclared one is rejected, and dropping
        // the last label of a vertex too.
        assert!(eval_value(graph.clone(), r#"g.v("a").add_label("other").toList()"#).is_err());
        assert!(eval_value(graph.clone(), r#"g.v("a").drop_label("a").toList()"#).is_err());
        assert!(eval_value(graph, r#"g.e("nolabel").set_label("other").toList()"#).is_err());
    }

    /// Rule 3: `mergeV()`/`merge_v()` mutate through the DSL — with a terminal, as a bare
    /// (rendered) traversal, with a mutating onMatch traversal, and persisting across separate
    /// evaluations on the same graph (the `-e` statements of `graphersal`).
    #[test]
    fn test_script_eval_merge_v_mutations() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            g.mergeV(#{"T.label": "person", "name": "marko"}).toList();
            g.merge_v(#{"T.label": "person", "name": "marko"}).option(Merge.onMatch, #{"age": 29}).toList();
            g.merge_v(#{"T.id": "s"}).option(Merge::onCreate, #{"T.label": "person", "name": "stephen"}).toList();
            g.V().has("person", "name", "marko").has("age", 29).count().next() + g.V("s").count().next()
        "#;
        assert_eq!(
            eval_value(graph.clone(), script).unwrap().as_int().ok(),
            Some(2)
        );

        // A bare mutating traversal as the last statement is executed by the visualizer.
        for bare in [
            r#"g.mergeV(#{"name": "bare1"})"#,
            r#"g.merge_v(#{"name": "bare2"})"#,
            r#"g.mergeV(#{"name": "marko"}).option(Merge.onMatch, #{"seen": true})"#,
        ] {
            eval(graph.clone(), bare).expect("bare mutation must run");
        }
        // The onMatch traversal itself mutates.
        eval(
            graph.clone(),
            r#"g.mergeV(#{"name": "bare1"}).option(Merge.onMatch, __.property("touched", 1).constant(#{})).toList()"#,
        )
        .expect("mutating onMatch traversal must run");
        let check = r#"g.V().has("name", "bare1").has("touched", 1).count().next()
            + g.V().has("name", "bare2").count().next()
            + g.V().has("name", "marko").has("seen", true).count().next()
            + g.V().count().next()"#;
        assert_eq!(eval_value(graph, check).unwrap().as_int().ok(), Some(7));
    }

    /// Rule 3: `mergeE()`/`merge_e()` mutate through the DSL — with a terminal, as a bare
    /// (rendered) traversal, with a mutating onMatch traversal, and persisting across separate
    /// evaluations on the same graph.
    #[test]
    fn test_script_eval_merge_e_mutations() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            g.addV("person").property(T.id, "a").addV("person").property(T.id, "b").toList();
            g.mergeE(#{"T.label": "knows", "Direction.OUT": "a", "Direction.IN": "b"}).toList();
            g.merge_e(#{"T.label": "knows", "Direction.OUT": "a", "Direction.IN": "b"}).option(Merge.onMatch, #{"w": 1}).toList();
            g.merge_e(#{"T.id": "e1"}).option(Merge::onCreate, #{"T.label": "likes", "Direction.OUT": Merge.outV, "Direction.IN": "a"}).option(Merge::outV, #{"T.id": "b"}).toList();
            g.E().hasLabel("knows").has("w", 1).count().next() + g.E("e1").count().next()
        "#;
        assert_eq!(
            eval_value(graph.clone(), script).unwrap().as_int().ok(),
            Some(2)
        );

        // A bare mutating traversal as the last statement is executed by the visualizer.
        for bare in [
            r#"g.mergeE(#{"T.label": "bare1", "Direction.OUT": "a", "Direction.IN": "a"})"#,
            r#"g.merge_e(#{"T.label": "bare2", "Direction.OUT": "b", "Direction.IN": "b"})"#,
            r#"g.mergeE(#{"T.label": "knows"}).option(Merge.onMatch, #{"seen": true})"#,
        ] {
            eval(graph.clone(), bare).expect("bare mutation must run");
        }
        // The onMatch traversal itself mutates.
        eval(
            graph.clone(),
            r#"g.mergeE(#{"T.label": "bare1"}).option(Merge.onMatch, __.property("touched", 1).constant(#{})).toList()"#,
        )
        .expect("mutating onMatch traversal must run");
        let check = r#"g.E().hasLabel("bare1").has("touched", 1).count().next()
            + g.E().hasLabel("bare2").count().next()
            + g.E().hasLabel("knows").has("seen", true).count().next()
            + g.E().count().next()"#;
        assert_eq!(eval_value(graph, check).unwrap().as_int().ok(), Some(7));
    }

    /// Tests bulk-assigning properties from a JSON object via `property_json()`, including its
    /// `propertyJson()` camelCase alias, via script DSL.
    #[test]
    #[cfg(feature = "serde")]
    fn test_script_eval_property_json_mutation() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"
            g.V("1").property_json(#{tags: ["x", "y"], meta: #{k: 1}}).toList();
            g.V("1").elementMap().toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("tags"),
            "Output should contain the tags property: {}",
            output
        );
        assert!(
            output.contains("meta"),
            "Output should contain the meta property: {}",
            output
        );

        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let camel_script = r#"
            g.V("2").propertyJson(#{score: 42}).toList();
            g.V("2").values("score").toList()
        "#;

        let result = eval(graph, camel_script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("42"),
            "Output should contain the score 42: {}",
            output
        );
    }

    /// DSL-parity half of `select_by_json_path_off_vertex`
    /// (`crates/graphersal/src/traversal/step/map_steps/step_select.rs`): a `By::JsonPath(...)`
    /// modulator built from Rhai must resolve a nested `property_json()` value through
    /// `select().by(...)` exactly like the Rust API does, in both spellings of `By`/`by`/`as`
    /// (`By::JsonPath`/`propertyJson`/`as`/`select` and `By::JsonPath`/`property_json`/`as_`).
    #[test]
    #[cfg(feature = "serde")]
    fn test_script_eval_select_by_json_path() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script = r#"
            g.V("1").propertyJson(#{meta: #{k: 1}}).as("A").select("A").by(By::JsonPath("meta.k")).toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains('1'),
            "Output should contain the resolved meta.k value 1: {}",
            output
        );

        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let snake_script = r#"
            g.v("1").property_json(#{meta: #{k: 1}}).as_("A").select("A").by(By::JsonPath("meta.k")).toList()
        "#;

        let result = eval(graph, snake_script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains('1'),
            "Output should contain the resolved meta.k value 1: {}",
            output
        );
    }

    /// A malformed JSONPath string (per step 3's rejected syntax) must surface as a normal script
    /// evaluation error tracing back to `ValueError::InvalidJsonPath`, not panic or produce a
    /// confusing unrelated error. This is also the first fallible `ArgBy`/`By` constructor in the
    /// DSL, so it establishes that fallible-constructor errors surface at all.
    #[test]
    #[cfg(feature = "serde")]
    fn test_script_eval_select_by_json_path_invalid_syntax() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script = r#"
            g.V("1").propertyJson(#{meta: #{k: 1}}).as("A").select("A").by(By::JsonPath("$..bad")).toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_err(),
            "Expected script evaluation to fail on an invalid JSONPath"
        );
        let err = format!("{:?}", result.err().unwrap());
        assert!(
            err.contains("JSONPath") || err.contains("recursive descent"),
            "Error should trace back to ValueError::InvalidJsonPath's diagnostic: {}",
            err
        );
    }

    /// DSL parity for the direct-access `.json_path()`/`.jsonPath()` step (step 6's Rust-API-only
    /// `StepJsonPath`, added in `crates/graphersal/src/traversal/step/flatmap_steps/step_json_path.rs`).
    /// Mirrors `test_script_eval_property_json_mutation`'s shape: seed a nested value via
    /// `property_json()`, then resolve it with `json_path()`/`jsonPath()`, checking both
    /// spellings. Not `#[cfg(feature = "serde")]` on the `json_path()` calls themselves - only
    /// the `property_json()` setup needs that feature; `json_path()` operates on
    /// `ElementProperty::Object`/`Array`, which exist unconditionally.
    #[test]
    #[cfg(feature = "serde")]
    fn test_script_eval_json_path() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let snake_script = r#"
            g.V("1").property_json(#{meta: #{k: 1}}).toList();
            g.V("1").json_path("meta.k").toList()
        "#;

        let result = eval(graph, snake_script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains('1'),
            "Output should contain the resolved meta.k value 1: {}",
            output
        );

        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let camel_script = r#"
            g.V("2").propertyJson(#{meta: #{k: 42}}).toList();
            g.V("2").jsonPath("meta.k").toList()
        "#;

        let result = eval(graph, camel_script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("42"),
            "Output should contain the resolved meta.k value 42: {}",
            output
        );
    }

    /// A malformed JSONPath string passed directly to `.jsonPath()` (not via `By::JsonPath`) must
    /// surface as a normal script evaluation error tracing back to `ValueError::InvalidJsonPath`,
    /// matching the Rust-API-side `rejected_syntax_is_invalid_json_path_error_not_panic` test in
    /// `step_json_path.rs`.
    #[test]
    fn test_script_eval_json_path_invalid_syntax() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script = r#"g.V("1").jsonPath("$..bad").toList()"#;

        let result = eval(graph, script);
        assert!(
            result.is_err(),
            "Expected script evaluation to fail on an invalid JSONPath"
        );
        let err = format!("{:?}", result.err().unwrap());
        assert!(
            err.contains("JSONPath") || err.contains("recursive descent"),
            "Error should trace back to ValueError::InvalidJsonPath's diagnostic: {}",
            err
        );
    }

    /// Tests dropping vertices matching a filter via script DSL.
    #[test]
    fn test_script_eval_drop() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"
            g.V().hasLabel("software").drop().toList();
            g.V().count().toList()
        "#;

        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "Script evaluation failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("4"),
            "Output should reflect 4 remaining vertices: {}",
            output
        );
    }

    /// Tests that mutations persist across multiple sequential eval calls on the same graph instance.
    #[test]
    fn test_script_eval_mutation_persistence() {
        let graph = Arc::new(GraphSource::empty());

        let script1 = r#"g.addV("person").property("name", "Alice").toList()"#;
        let res1 = eval(graph.clone(), script1);
        assert!(res1.is_ok(), "Script 1 failed: {:?}", res1.err());

        let script2 = r#"g.addV("person").property("name", "Bob").toList()"#;
        let res2 = eval(graph.clone(), script2);
        assert!(res2.is_ok(), "Script 2 failed: {:?}", res2.err());

        let script3 = r#"g.V().count().toList()"#;
        let res3 = eval(graph.clone(), script3);
        assert!(res3.is_ok(), "Script 3 failed: {:?}", res3.err());
        let output = res3.unwrap();
        assert!(
            output.contains("2") || output.contains("[2]"),
            "Expected count 2 across sessions: {}",
            output
        );
    }

    /// Rule 3: a GQL `INSERT` / `CREATE` mutates through the DSL: with a terminal, as a bare
    /// (rendered) traversal, with parameters, returning rows, in camelCase scripts, and
    /// persisting across separate evaluations on the same graph.
    #[cfg(feature = "gql")]
    #[test]
    fn test_script_eval_gql_insert_mutations() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            g.gql("INSERT (:person {name: 'ann'})").iterate();
            g.gql("CREATE (:person {name: $n})", #{n: "bob"}).toList();
            let rows = g.gql("MATCH (a:person {name: 'ann'}), (b:person {name: 'bob'}) INSERT (a)-[k:knows {w: 1}]->(b) RETURN k.w AS w").toList();
            g.V().count().next() * 100 + g.E().count().next() * 10 + rows.len()
        "#;
        assert_eq!(
            eval_value(graph.clone(), script).unwrap().as_int().ok(),
            Some(211)
        );
        // A bare writing traversal as the last statement is executed by the visualizer.
        for bare in [
            r#"g.gql("INSERT (:bare {n: 1})")"#,
            r#"g.gql("MATCH (p:person) INSERT (p)-[:OWNS]->(:pet)")"#,
        ] {
            eval(graph.clone(), bare).expect("bare GQL write must run");
        }
        // camelCase steps after a GQL write.
        let out = eval_value(
            graph.clone(),
            r#"g.gql("INSERT (x:tagged) RETURN x").select("x").property("k", 2).iterate();
               g.V().hasLabel("tagged").has("k", 2).count().next()"#,
        )
        .unwrap();
        assert_eq!(out.as_int().ok(), Some(1));
        // The writes persist across evaluations.
        let check =
            r#"g.V().hasLabel("pet").count().next() * 10 + g.V().hasLabel("bare").count().next()"#;
        assert_eq!(
            eval_value(graph.clone(), check).unwrap().as_int().ok(),
            Some(21)
        );
        assert_eq!(graph.read().vertex_count(), 6);
        assert_eq!(graph.read().edge_count(), 3);
    }

    /// A mutating `by()` child traversal makes the plan mutating (behaviour change: it used to
    /// fail with "Cannot mutate immutable graph reference"): the child's writes reach the graph,
    /// once per evaluated traverser, also when nested deeper than one level.
    #[test]
    fn test_script_eval_mutating_by_child() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let out = eval(
            graph.clone(),
            r#"
            g.V().hasLabel("person").project("a").by(__.addV("x").property("n", 1).id()).toList();
            g.V().hasLabel("x").count().toList()
        "#,
        )
        .expect("project().by(__.addV()) mutates");
        assert!(
            out.contains('4'),
            "four person traversers add four vertices: {out}"
        );

        let out = eval(
            graph.clone(),
            r#"
            g.V().hasLabel("software").group().by(__.local(__.addV("y")).label()).toList();
            g.V().hasLabel("y").count().toList()
        "#,
        )
        .expect("nested mutating by child");
        assert!(
            out.contains('2'),
            "two software traversers add two vertices: {out}"
        );

        // Read-only by() children stay read-only and leave the graph unchanged.
        let out = eval(
            graph,
            r#"
            g.V().hasLabel("person").order().by(__.out().count()).by("name").toList();
            g.V().count().toList()
        "#,
        )
        .expect("read-only by child");
        assert!(
            out.contains("12"),
            "6 modern + 4 + 2 added above, nothing from the read-only query: {out}"
        );
    }

    #[test]
    fn test_script_eval_graph_source_module() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            let g2 = GraphSource::tinkerpop_modern();
            g2.V().count().toList()
        "#;
        let result = eval(graph, script);
        assert!(
            result.is_ok(),
            "GraphSource::tinkerpop_modern failed: {:?}",
            result.err()
        );
    }

    #[cfg(feature = "io")]
    #[test]
    fn test_script_export_and_import_graphml() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let xml_path = std::env::temp_dir()
            .join("script_export_test.xml")
            .to_str()
            .unwrap()
            .to_string();
        // The path as a Rhai string literal: a Windows path's backslashes are escapes there.
        let xml_literal = xml_path.replace('\\', "\\\\");

        let script_export = format!(
            r#"
            g.export_graphml("{}");
        "#,
            xml_literal
        );

        let result = eval(graph, &script_export);
        assert!(result.is_ok(), "Script export failed: {:?}", result.err());

        assert!(std::path::Path::new(&xml_path).exists());

        let empty_graph = Arc::new(GraphSource::empty());
        let script_import = format!(
            r#"
            let g_xml = GraphSource::file("{}");
            g_xml.V().count().toList()
        "#,
            xml_literal
        );

        let result_import = eval(empty_graph, &script_import);
        assert!(
            result_import.is_ok(),
            "Script GraphSource::file failed: {:?}",
            result_import.err()
        );
        assert!(result_import.unwrap().contains("6"));

        let _ = std::fs::remove_file(xml_path);
    }

    /// `GraphSource::file` reads GraphSON and GraphML only; any other extension is refused by name.
    #[cfg(feature = "io")]
    #[test]
    fn test_script_graph_source_file_rejects_unsupported_extension() {
        let graph = Arc::new(GraphSource::empty());

        let result = eval(
            graph,
            r#"GraphSource::file("graph.csv").V().count().toList()"#,
        );

        let err = result.expect_err("an unsupported extension must not load");
        let err = format!("{err:?}");
        assert!(
            err.contains("Unsupported graph file format"),
            "diagnostic: {err}"
        );
        assert!(err.contains("GraphML"), "diagnostic: {err}");
        assert!(err.contains("GraphSON"), "diagnostic: {err}");
    }

    /// `export_graphson`/`exportGraphson` and `import_graphson`/`importGraphson`, and
    /// `GraphSource::file` on a `.json` file.
    #[cfg(feature = "io")]
    #[test]
    fn test_script_export_and_import_graphson_in_both_spellings() {
        let dir = std::env::temp_dir().join(format!("script-graphson-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (export, import) in [
            ("export_graphson", "import_graphson"),
            ("exportGraphson", "importGraphson"),
        ] {
            let path = dir.join(format!("{export}.json"));
            let path = path.to_str().unwrap().replace('\\', "/");
            let graph = Arc::new(GraphSource::tinkerpop_modern());
            let msg = eval_value(graph, &format!(r#"g.{export}("{path}")"#)).unwrap();
            assert_eq!(
                msg.into_string().unwrap(),
                format!("Exported GraphSON to {path}")
            );

            let target = Arc::new(GraphSource::empty());
            let msg = eval_value(target.clone(), &format!(r#"g.{import}("{path}")"#))
                .unwrap()
                .into_string()
                .unwrap();
            assert!(
                msg.starts_with(&format!(
                    "Imported GraphSON from {path}: imported 6 vertices and 6 edges"
                )),
                "{msg}"
            );
            assert_eq!(target.read().vertex_count(), 6);

            let n = eval_value(
                Arc::new(GraphSource::empty()),
                &format!(r#"GraphSource::file("{path}").V().out("knows").count().next()"#),
            )
            .unwrap();
            assert_eq!(n.as_int().unwrap(), 2);
        }
        // The report text names what was skipped (a year, which Graphersal cannot hold); a
        // date it holds (`seen`) is imported and not reported.
        let skipped = dir.join("dates.json");
        std::fs::write(
            &skipped,
            r#"{"id":"v","properties":{"born":[{"value":{"@type":"gx:Year","@value":"2016"}}],"seen":[{"value":{"@type":"g:Date","@value":1}}]}}"#,
        )
        .unwrap();
        let path = skipped.to_str().unwrap().replace('\\', "/");
        let target = Arc::new(GraphSource::empty());
        let msg = eval_value(target.clone(), &format!(r#"g.importGraphson("{path}")"#))
            .unwrap()
            .into_string()
            .unwrap();
        assert!(
            msg.contains("skipped date/time values (gx:Year, a type Graphersal cannot hold): 1"),
            "{msg}"
        );
        assert!(!msg.contains("g:Date"), "{msg}");
        let seen = eval_value(
            target,
            r#"g.V("v").values("seen").next() == datetime("1970-01-01T00:00:00.001Z")"#,
        )
        .unwrap();
        assert!(seen.as_bool().unwrap());
        assert!(
            msg.contains(r#"line 1: vertex "v", property "born""#),
            "{msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Tests adding vertex without explicit ID on prepopulated tinkerpop_modern graph.
    #[test]
    fn test_script_eval_add_v_on_tinkerpop_modern() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"
            g.addV("person").property("name", "Alice").toList();
            g.V().count().toList()
        "#;

        let result = eval(graph.clone(), script);
        assert!(
            result.is_ok(),
            "addV on tinkerpop_modern failed: {:?}",
            result.err()
        );
        let count_output = result.unwrap();
        assert!(
            count_output.contains("7") || count_output.contains("[7]"),
            "Expected count 7 after adding vertex: {}",
            count_output
        );

        let script_id = r#"g.V("7").values("name").toList()"#;
        let id_result = eval(graph, script_id);
        assert!(
            id_result.is_ok(),
            "query for new vertex ID 7 failed: {:?}",
            id_result.err()
        );
        let id_output = id_result.unwrap();
        assert!(
            id_output.contains("Alice"),
            "Expected Alice for vertex ID 7: {}",
            id_output
        );
    }

    /// Tests adding edge without explicit ID on prepopulated tinkerpop_modern graph.
    ///
    /// Both endpoint labels have to be reachable from a single traverser path, hence the
    /// navigation through `out("created")` rather than a second `V(...)` source step.
    #[test]
    fn test_script_eval_add_e_on_tinkerpop_modern() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"
            g.V("1").as("a").out("created").as("b").addE("likes").from("a").to("b").toList();
            g.E().hasLabel("likes").count().toList()
        "#;

        let result = eval(graph.clone(), script);
        assert!(
            result.is_ok(),
            "addE on tinkerpop_modern failed: {:?}",
            result.err()
        );
        let count_output = result.unwrap();
        assert!(
            count_output.contains("1") || count_output.contains("[1]"),
            "Expected 1 'likes' edge: {}",
            count_output
        );

        let script_total = r#"g.E().count().toList()"#;
        let total_result = eval(graph, script_total);
        assert!(
            total_result.is_ok(),
            "query for total edges failed: {:?}",
            total_result.err()
        );
        let total_output = total_result.unwrap();
        assert!(
            total_output.contains("7") || total_output.contains("[7]"),
            "Expected 7 total edges (6 existing + 1 new): {}",
            total_output
        );
    }

    /// Tests evaluation of optimized source filter and count pushdowns via Rhai DSL.
    #[test]
    fn test_script_eval_optimized_v_has_label_count_and_list() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // Count pushdown with label filter: g.V().hasLabel("person").count().toList()
        let count_script = r#"g.V().hasLabel("person").count().toList()"#;
        let count_res = eval(graph.clone(), count_script);
        assert!(
            count_res.is_ok(),
            "count query failed: {:?}",
            count_res.err()
        );
        let count_out = count_res.unwrap();
        assert!(
            count_out.contains("4") || count_out.contains("[4]"),
            "Expected 4 person vertices, got: {}",
            count_out
        );

        // Source filter pushdown: g.V().hasLabel("person").values("name").toList()
        let list_script = r#"g.V().hasLabel("person").values("name").toList()"#;
        let list_res = eval(graph.clone(), list_script);
        assert!(list_res.is_ok(), "list query failed: {:?}", list_res.err());
        let list_out = list_res.unwrap();
        assert!(
            list_out.contains("marko")
                && list_out.contains("vadas")
                && list_out.contains("josh")
                && list_out.contains("peter"),
            "Expected all 4 person names, got: {}",
            list_out
        );

        // Edge count with label pushdown: g.E().hasLabel("knows").count().toList()
        let edge_count_script = r#"g.E().hasLabel("knows").count().toList()"#;
        let edge_count_res = eval(graph, edge_count_script);
        assert!(
            edge_count_res.is_ok(),
            "edge count query failed: {:?}",
            edge_count_res.err()
        );
        let edge_count_out = edge_count_res.unwrap();
        assert!(
            edge_count_out.contains("2") || edge_count_out.contains("[2]"),
            "Expected 2 knows edges, got: {}",
            edge_count_out
        );
    }

    /// Tests that script profile() reports the optimized physical execution plan accurately.
    #[test]
    fn test_script_eval_optimized_profile_physical_plan() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // g.V().hasLabel("person").count().profile()
        let profile_script = r#"g.V().hasLabel("person").count().profile().to_string()"#;
        let res = eval(graph.clone(), profile_script);
        assert!(res.is_ok(), "profile query failed: {:?}", res.err());
        let profile_out = res.unwrap();

        // Physical plan should show fused v step with .count()
        assert!(
            profile_out.contains(".count()"),
            "Physical plan should show folded .count() step: {}",
            profile_out
        );
        // Logical hasLabel step should be fused into the source init step
        assert!(
            !profile_out.contains("has_label(") && !profile_out.contains("has_label("),
            "Physical plan should not display separate hasLabel step: {}",
            profile_out
        );

        // Edge flatmap folding profile: g.V("1").outE("knows").profile()
        let edge_profile_script = r#"g.V("1").outE("knows").profile().to_string()"#;
        let edge_res = eval(graph, edge_profile_script);
        assert!(
            edge_res.is_ok(),
            "edge profile query failed: {:?}",
            edge_res.err()
        );
        let edge_profile_out = edge_res.unwrap();
        assert!(
            edge_profile_out.contains("out_e"),
            "Physical plan should contain out_e step: {}",
            edge_profile_out
        );
    }

    #[test]
    fn test_script_eval_group_count() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // g.V().groupCount().by(By::Label).toList()
        let script1 = r#"g.V().groupCount().by(By::Label).toList()"#;
        let res1 = eval(graph.clone(), script1);
        assert!(res1.is_ok(), "groupCount failed: {:?}", res1.err());
        let out1 = res1.unwrap();
        assert!(
            out1.contains("person") && out1.contains("software"),
            "Expected person and software in groupCount: {}",
            out1
        );

        // g.V().group().by(By::Label).by(By::Count).toList()
        let script2 = r#"g.V().group().by(By::Label).by(By::Count).toList()"#;
        let res2 = eval(graph, script2);
        assert!(res2.is_ok(), "group by count failed: {:?}", res2.err());
        let out2 = res2.unwrap();
        assert!(
            out2.contains("person") && out2.contains("software"),
            "Expected person and software in group by count: {}",
            out2
        );
    }

    #[test]
    fn test_script_eval_p_dot_notation_comparisons() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // P.gt(30) -> Josh (32), Peter (35)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", P.gt(30)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.gt failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("josh") && out.contains("peter") && !out.contains("marko"),
            "P.gt output: {}",
            out
        );

        // P.lt(30) -> Marko (29), Vadas (27)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", P.lt(30)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.lt failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("marko") && out.contains("vadas") && !out.contains("josh"),
            "P.lt output: {}",
            out
        );

        // P.gte(32) -> Josh (32), Peter (35)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", P.gte(32)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.gte failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("josh") && out.contains("peter") && !out.contains("marko"),
            "P.gte output: {}",
            out
        );

        // P.lte(29) -> Marko (29), Vadas (27)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", P.lte(29)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.lte failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("marko") && out.contains("vadas") && !out.contains("josh"),
            "P.lte output: {}",
            out
        );

        // P.eq(29) -> Marko (29)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", P.eq(29)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.eq failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("marko") && !out.contains("vadas"),
            "P.eq output: {}",
            out
        );

        // P.neq("marko")
        let res = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", P.neq("marko")).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.neq failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            !out.contains("marko") && out.contains("vadas") && out.contains("josh"),
            "P.neq output: {}",
            out
        );

        // P.between(27, 32) -> Vadas (27), Marko (29) (inclusive lower, exclusive upper)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", P.between(27, 32)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.between failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("vadas") && out.contains("marko") && !out.contains("josh"),
            "P.between output: {}",
            out
        );

        // P.inside(27, 32) -> Marko (29) (exclusive both)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", P.inside(27, 32)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.inside failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("marko") && !out.contains("vadas") && !out.contains("josh"),
            "P.inside output: {}",
            out
        );

        // P.outside(28, 33) -> Vadas (27), Peter (35)
        let res = eval(
            graph,
            r#"g.V().has("age", P.outside(28, 33)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.outside failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("vadas") && out.contains("peter") && !out.contains("marko"),
            "P.outside output: {}",
            out
        );
    }

    #[test]
    fn test_script_eval_p_dot_notation_collections() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // P.within array: ["marko", "vadas"]
        let res = eval(
            graph.clone(),
            r#"g.V().has("name", P.within(["marko", "vadas"])).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.within array failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("marko") && out.contains("vadas") && !out.contains("josh"),
            "P.within array: {}",
            out
        );

        // P.within scalar: "marko"
        let res = eval(
            graph.clone(),
            r#"g.V().has("name", P.within("marko")).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.within scalar failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("marko") && !out.contains("vadas"),
            "P.within scalar: {}",
            out
        );

        // P.without array: ["marko", "vadas", "peter"]
        let res = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", P.without(["marko", "vadas", "peter"])).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.without array failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            out.contains("josh") && !out.contains("marko") && !out.contains("vadas"),
            "P.without array: {}",
            out
        );

        // P.without scalar: "marko"
        let res = eval(
            graph,
            r#"g.V().hasLabel("person").has("name", P.without("marko")).values("name").toList()"#,
        );
        assert!(res.is_ok(), "P.without scalar failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(
            !out.contains("marko") && out.contains("vadas") && out.contains("josh"),
            "P.without scalar: {}",
            out
        );
    }

    #[test]
    fn test_script_eval_textp_and_p_dot_notation() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // TextP.startingWith / starting_with
        let res1 = eval(
            graph.clone(),
            r#"g.V().has("name", TextP.startingWith("ma")).values("name").toList()"#,
        );
        assert!(res1.is_ok(), "TextP.startingWith failed: {:?}", res1.err());
        assert!(res1.unwrap().contains("marko"));

        let res2 = eval(
            graph.clone(),
            r#"g.V().has("name", TextP.starting_with("va")).values("name").toList()"#,
        );
        assert!(res2.is_ok(), "TextP.starting_with failed: {:?}", res2.err());
        assert!(res2.unwrap().contains("vadas"));

        // TextP.containing
        let res3 = eval(
            graph.clone(),
            r#"g.V().has("name", TextP.containing("ar")).values("name").toList()"#,
        );
        assert!(res3.is_ok(), "TextP.containing failed: {:?}", res3.err());
        assert!(res3.unwrap().contains("marko"));

        // TextP.endingWith / ending_with
        let res4 = eval(
            graph.clone(),
            r#"g.V().has("name", TextP.endingWith("sh")).values("name").toList()"#,
        );
        assert!(res4.is_ok(), "TextP.endingWith failed: {:?}", res4.err());
        assert!(res4.unwrap().contains("josh"));

        let res5 = eval(
            graph.clone(),
            r#"g.V().has("name", TextP.ending_with("er")).values("name").toList()"#,
        );
        assert!(res5.is_ok(), "TextP.ending_with failed: {:?}", res5.err());
        assert!(res5.unwrap().contains("peter"));

        // TextP.notContaining / not_containing
        let res6 = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", TextP.notContaining("a")).values("name").toList()"#,
        );
        assert!(res6.is_ok(), "TextP.notContaining failed: {:?}", res6.err());
        let out6 = res6.unwrap();
        assert!(out6.contains("josh") && out6.contains("peter") && !out6.contains("marko"));

        let res7 = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", TextP.not_containing("a")).values("name").toList()"#,
        );
        assert!(
            res7.is_ok(),
            "TextP.not_containing failed: {:?}",
            res7.err()
        );
        let out7 = res7.unwrap();
        assert!(out7.contains("josh") && out7.contains("peter") && !out7.contains("marko"));

        // TextP.notStartingWith / not_starting_with
        let res8 = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", TextP.notStartingWith("m")).values("name").toList()"#,
        );
        assert!(
            res8.is_ok(),
            "TextP.notStartingWith failed: {:?}",
            res8.err()
        );
        assert!(!res8.unwrap().contains("marko"));

        let res9 = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", TextP.not_starting_with("m")).values("name").toList()"#,
        );
        assert!(
            res9.is_ok(),
            "TextP.not_starting_with failed: {:?}",
            res9.err()
        );
        assert!(!res9.unwrap().contains("marko"));

        // TextP.notEndingWith / not_ending_with
        let res10 = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", TextP.notEndingWith("o")).values("name").toList()"#,
        );
        assert!(
            res10.is_ok(),
            "TextP.notEndingWith failed: {:?}",
            res10.err()
        );
        assert!(!res10.unwrap().contains("marko"));

        let res11 = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").has("name", TextP.not_ending_with("o")).values("name").toList()"#,
        );
        assert!(
            res11.is_ok(),
            "TextP.not_ending_with failed: {:?}",
            res11.err()
        );
        assert!(!res11.unwrap().contains("marko"));

        // P.<text_fn> delegation
        let res12 = eval(
            graph.clone(),
            r#"g.V().has("name", P.containing("ar")).values("name").toList()"#,
        );
        assert!(res12.is_ok(), "P.containing failed: {:?}", res12.err());
        assert!(res12.unwrap().contains("marko"));

        let res13 = eval(
            graph,
            r#"g.V().has("name", P.startingWith("ma")).values("name").toList()"#,
        );
        assert!(res13.is_ok(), "P.startingWith failed: {:?}", res13.err());
        assert!(res13.unwrap().contains("marko"));
    }

    #[test]
    fn test_script_eval_global_predicate_functions() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // gt(30)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", gt(30)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "gt failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(out.contains("josh") && out.contains("peter") && !out.contains("marko"));

        // lt(30)
        let res = eval(
            graph.clone(),
            r#"g.V().has("age", lt(30)).values("name").toList()"#,
        );
        assert!(res.is_ok(), "lt failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(out.contains("marko") && out.contains("vadas") && !out.contains("josh"));

        // within(["marko", "josh"])
        let res = eval(
            graph.clone(),
            r#"g.V().has("name", within(["marko", "josh"])).values("name").toList()"#,
        );
        assert!(res.is_ok(), "within failed: {:?}", res.err());
        let out = res.unwrap();
        assert!(out.contains("marko") && out.contains("josh") && !out.contains("vadas"));

        // containing("ar")
        let res = eval(
            graph.clone(),
            r#"g.V().has("name", containing("ar")).values("name").toList()"#,
        );
        assert!(res.is_ok(), "containing failed: {:?}", res.err());
        assert!(res.unwrap().contains("marko"));

        // startingWith("va")
        let res = eval(
            graph.clone(),
            r#"g.V().has("name", startingWith("va")).values("name").toList()"#,
        );
        assert!(res.is_ok(), "startingWith failed: {:?}", res.err());
        assert!(res.unwrap().contains("vadas"));

        // endingWith("sh")
        let res = eval(
            graph,
            r#"g.V().has("name", endingWith("sh")).values("name").toList()"#,
        );
        assert!(res.is_ok(), "endingWith failed: {:?}", res.err());
        assert!(res.unwrap().contains("josh"));
    }

    #[test]
    fn test_script_eval_polymorphic_has_and_is_gremlin_compatibility() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // 1. Scalar literal equality in .has()
        let res1 = eval(
            graph.clone(),
            r#"g.V().has("name", "marko").values("name").toList()"#,
        );
        assert!(res1.is_ok(), "has scalar string failed: {:?}", res1.err());
        let out1 = res1.unwrap();
        assert!(
            out1.contains("marko") && !out1.contains("vadas"),
            "has scalar string: {}",
            out1
        );

        let res2 = eval(
            graph.clone(),
            r#"g.V().has("age", 29).values("name").toList()"#,
        );
        assert!(res2.is_ok(), "has scalar int failed: {:?}", res2.err());
        let out2 = res2.unwrap();
        assert!(
            out2.contains("marko") && !out2.contains("vadas"),
            "has scalar int: {}",
            out2
        );

        // 2. Predicate object in .has()
        let res3 = eval(
            graph.clone(),
            r#"g.V().has("age", P.gt(30)).values("name").toList()"#,
        );
        assert!(res3.is_ok(), "has(P.gt) failed: {:?}", res3.err());
        let out3 = res3.unwrap();
        assert!(
            out3.contains("josh") && out3.contains("peter") && !out3.contains("marko"),
            "has(P.gt): {}",
            out3
        );

        let res4 = eval(
            graph.clone(),
            r#"g.V().has("age", P.within([29, 32])).values("name").toList()"#,
        );
        assert!(res4.is_ok(), "has(P.within) failed: {:?}", res4.err());
        let out4 = res4.unwrap();
        assert!(
            out4.contains("marko") && out4.contains("josh") && !out4.contains("vadas"),
            "has(P.within): {}",
            out4
        );

        let res5 = eval(
            graph.clone(),
            r#"g.V().has("name", TextP.startingWith("m")).values("name").toList()"#,
        );
        assert!(
            res5.is_ok(),
            "has(TextP.startingWith) failed: {:?}",
            res5.err()
        );
        let out5 = res5.unwrap();
        assert!(
            out5.contains("marko") && !out5.contains("vadas"),
            "has(TextP.startingWith): {}",
            out5
        );

        let res6 = eval(
            graph.clone(),
            r#"g.V().has("age", gt(30)).values("name").toList()"#,
        );
        assert!(res6.is_ok(), "has(gt) failed: {:?}", res6.err());
        let out6 = res6.unwrap();
        assert!(
            out6.contains("josh") && out6.contains("peter") && !out6.contains("marko"),
            "has(gt): {}",
            out6
        );

        // 3. .is() with predicate object and scalar literal
        let res7 = eval(
            graph.clone(),
            r#"g.V().values("age").is(P.lt(30)).toList()"#,
        );
        assert!(res7.is_ok(), "is(P.lt(30)) failed: {:?}", res7.err());
        let out7 = res7.unwrap();
        assert!(
            out7.contains("29") && out7.contains("27") && !out7.contains("32"),
            "is(P.lt): {}",
            out7
        );

        let res8 = eval(graph, r#"g.V().values("age").is(32).toList()"#);
        assert!(res8.is_ok(), "is(32) scalar failed: {:?}", res8.err());
        let out8 = res8.unwrap();
        assert!(
            out8.contains("32") && !out8.contains("29") && !out8.contains("27"),
            "is(32): {}",
            out8
        );
    }

    #[test]
    fn test_script_eval_predicate_backward_compatibility() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // P::Gt(30)
        let res1 = eval(
            graph.clone(),
            r#"g.V().has_p("age", P::Gt(30)).values("name").toList()"#,
        );
        assert!(res1.is_ok(), "P::Gt failed: {:?}", res1.err());
        assert!(res1.unwrap().contains("josh"));

        // P::gt(30)
        let res2 = eval(
            graph.clone(),
            r#"g.V().has_p("age", P::gt(30)).values("name").toList()"#,
        );
        assert!(res2.is_ok(), "P::gt failed: {:?}", res2.err());
        assert!(res2.unwrap().contains("josh"));

        // P::WithIn(["marko", "vadas"])
        let res3 = eval(
            graph.clone(),
            r#"g.V().has_p("name", P::WithIn(["marko", "vadas"])).values("name").toList()"#,
        );
        assert!(res3.is_ok(), "P::WithIn failed: {:?}", res3.err());
        assert!(res3.unwrap().contains("marko"));

        // P::within(["marko", "vadas"])
        let res4 = eval(
            graph.clone(),
            r#"g.V().has_p("name", P::within(["marko", "vadas"])).values("name").toList()"#,
        );
        assert!(res4.is_ok(), "P::within failed: {:?}", res4.err());
        assert!(res4.unwrap().contains("marko"));

        // P::containing("ar")
        let res5 = eval(
            graph.clone(),
            r#"g.V().has_p("name", P::containing("ar")).values("name").toList()"#,
        );
        assert!(res5.is_ok(), "P::containing failed: {:?}", res5.err());
        assert!(res5.unwrap().contains("marko"));

        // TextP::containing("ar")
        let res6 = eval(
            graph,
            r#"g.V().has_p("name", TextP::containing("ar")).values("name").toList()"#,
        );
        assert!(res6.is_ok(), "TextP::containing failed: {:?}", res6.err());
        assert!(res6.unwrap().contains("marko"));
    }

    #[test]
    fn test_script_eval_predicate_in_is_and_where() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // .values("age").is(P.gt(30))
        let res1 = eval(
            graph.clone(),
            r#"g.V().values("age").is(P.gt(30)).toList()"#,
        );
        assert!(res1.is_ok(), "is(P.gt(30)) failed: {:?}", res1.err());
        let out1 = res1.unwrap();
        assert!(
            out1.contains("32") && out1.contains("35") && !out1.contains("29"),
            "is(P.gt(30)) out: {}",
            out1
        );

        // .values("age").is(gt(30))
        let res2 = eval(graph.clone(), r#"g.V().values("age").is(gt(30)).toList()"#);
        assert!(res2.is_ok(), "is(gt(30)) failed: {:?}", res2.err());
        let out2 = res2.unwrap();
        assert!(
            out2.contains("32") && out2.contains("35") && !out2.contains("29"),
            "is(gt(30)) out: {}",
            out2
        );

        // .values("name").is(TextP.containing("ar"))
        let res3 = eval(
            graph.clone(),
            r#"g.V().values("name").is(TextP.containing("ar")).toList()"#,
        );
        assert!(
            res3.is_ok(),
            "is(TextP.containing) failed: {:?}",
            res3.err()
        );
        assert!(res3.unwrap().contains("marko"));

        // .where_p(P.gt(30))
        let res4 = eval(
            graph,
            r#"g.V().values("age").where_p(P.gt(30)).count().toList()"#,
        );
        assert!(res4.is_ok(), "where_p failed: {:?}", res4.err());
        let out4 = res4.unwrap();
        assert!(
            out4.contains("2") || out4.contains("[2]"),
            "Expected count 2: {}",
            out4
        );
    }

    /// Tests evaluation of `identity()` step in Rhai scripting DSL.
    #[test]
    fn test_script_eval_identity() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let res1 = eval(graph.clone(), r#"g.V().identity().count().toList()"#);
        assert!(
            res1.is_ok(),
            "g.V().identity().count() failed: {:?}",
            res1.err()
        );
        let out1 = res1.unwrap();
        assert!(
            out1.contains("6") || out1.contains("[6]"),
            "Expected count 6: {}",
            out1
        );

        let res2 = eval(graph.clone(), r#"g.v().identity().to_list()"#);
        assert!(
            res2.is_ok(),
            "g.v().identity().to_list() failed: {:?}",
            res2.err()
        );

        let res3 = eval(
            graph.clone(),
            r#"g.V("1").identity().values("name").toList()"#,
        );
        assert!(
            res3.is_ok(),
            "g.V(1).identity().values(name) failed: {:?}",
            res3.err()
        );
        let out3 = res3.unwrap();
        assert!(out3.contains("marko"), "Expected name marko: {}", out3);

        let res4 = eval(graph, r#"g.V().identity().profile().to_string()"#);
        assert!(
            res4.is_ok(),
            "g.V().identity().profile() failed: {:?}",
            res4.err()
        );
        let out4 = res4.unwrap();
        assert!(
            out4.contains("identity("),
            "Expected identity in profile: {}",
            out4
        );
    }

    /// Tests evaluation of `union()` branching step in Rhai scripting DSL.
    #[test]
    fn test_script_eval_union() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let res1 = eval(
            graph.clone(),
            r#"g.V("1").union([__.out("knows"), __.out("created")]).values("name").toList()"#,
        );
        assert!(
            res1.is_ok(),
            "union knows and created failed: {:?}",
            res1.err()
        );
        let out1 = res1.unwrap();
        assert!(
            out1.contains("josh") && out1.contains("vadas") && out1.contains("lop"),
            "Expected josh, vadas, lop: {}",
            out1
        );

        let res2 = eval(
            graph.clone(),
            r#"g.v("1").union([__.identity(), __.out("knows")]).to_list()"#,
        );
        assert!(res2.is_ok(), "identity union failed: {:?}", res2.err());

        let res3 = eval(graph.clone(), r#"g.v("1").union([]).to_list()"#);
        assert!(res3.is_ok(), "empty union failed: {:?}", res3.err());

        let res4 = eval(
            graph,
            r#"g.V("1").union([__.out("knows"), __.out("created")]).profile().to_string()"#,
        );
        assert!(res4.is_ok(), "union profile failed: {:?}", res4.err());
        let out4 = res4.unwrap();
        assert!(
            out4.contains("union("),
            "Expected union in profile: {}",
            out4
        );
    }

    /// A schema is a map in the script (its canonical format); the session visualizer renders it
    /// in the requested format, a script value prints as the schema table by default.
    #[test]
    fn test_script_eval_schema_visualize() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let render = |script: &str, format: VisualizeFormat| {
            let value =
                eval_value(graph.clone(), script).unwrap_or_else(|e| panic!("{script}: {e}"));
            crate::script_host::render_value(value, format, &RenderOptions::unlimited()).text
        };
        for script in [
            "g.infer_schema()",
            "g.inferSchema()",
            "let s = g.infer_schema(); s",
        ] {
            let mermaid = render(script, VisualizeFormat::Mermaid);
            assert!(
                mermaid.contains("erDiagram") && mermaid.contains("person"),
                "{mermaid}"
            );
        }
        assert!(render("g.infer_schema()", VisualizeFormat::PlantUML).contains("@startuml"));
        assert!(
            render("g.infer_schema()", VisualizeFormat::JsonSchema).contains("\"vertex:person\"")
        );
        let table = eval(graph.clone(), "g.infer_schema()").unwrap();
        assert!(table.contains("Schema (mode: none)"), "{table}");
        // The map is data: its parts are readable.
        let mode = eval(graph.clone(), r#"g.infer_schema().mode"#).unwrap();
        assert_eq!(mode.trim(), "none");
        let required = eval(graph, r#"g.infer_schema().vertices.person.schema.required"#).unwrap();
        assert!(required.contains("name"), "{required}");
    }

    #[test]
    fn test_script_eval_schema_meta_graph() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"let s = g.infer_schema(); let g_meta = s.to_graph(); g_meta.v("person").values("label_name").to_list()"#;
        let out = eval(graph.clone(), script).expect("meta graph querying failed");
        assert!(out.contains("person"), "Expected person label_name: {out}");
        let out = eval(
            graph.clone(),
            r#"g.infer_schema().toGraph().v().count().next()"#,
        )
        .unwrap();
        assert_eq!(out.trim(), "2");

        let script_subgraph = r#"let s = g.infer_schema(); let g_sub = s.subgraph("person"); g_sub.v().values("label_name").to_list()"#;
        let out_sub = eval(graph.clone(), script_subgraph).expect("subgraph querying failed");
        assert!(out_sub.contains("person"), "{out_sub}");
        assert!(!out_sub.contains("software"), "{out_sub}");
        let both = eval(
            graph.clone(),
            r#"g.infer_schema().subgraph_labels(["person", "software"]).e().count().next()"#,
        )
        .unwrap();
        assert_eq!(both.trim(), "2");
        // A map that is not a schema is rejected with the format error.
        let err = eval(graph, r#"#{mdoe: "closed"}.to_graph()"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown key 'mdoe'"), "{err}");
    }

    #[test]
    fn test_script_eval_schema_in_union() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script = r#"g.v().union([__.identity(), __.infer_schema()]).to_list()"#;
        let res = eval(graph, script);
        assert!(
            res.is_ok(),
            "union with __.infer_schema() failed: {:?}",
            res.err()
        );
    }

    /// Regression: `g.with(key, value)` must not leak into later statements sharing the same
    /// `Scope` — exactly how `graphersal`'s REPL/multi-`-e` mode evaluates one script per line against
    /// one long-lived `scope` (`main.rs`'s `engine.eval_with_scope(&mut scope, line)` inside its
    /// loop), unlike `eval()`/`eval_value()` in this file, which build a fresh scope per call and
    /// so would never have caught this. A prior version of `ScriptGraphTraversalSource::with`
    /// mutated `self.options` before cloning, instead of cloning first — since Rhai calls a
    /// `&mut self` method on the actual scope variable, that mutated `g` itself permanently, so
    /// `optimizer.disabled` set by one line silently applied to every later line in the session,
    /// even ones that never called `.with()`.
    #[test]
    fn with_on_g_does_not_leak_into_a_later_statement_sharing_the_scope() {
        use crate::script_host::{engine, graph_scope};

        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let engine = engine();
        let mut scope = graph_scope(graph);

        let disabled: rhai::Dynamic = engine
            .eval_with_scope(
                &mut scope,
                r#"g.with("optimizer.disabled", ["all"]).V().hasLabel("person").out("knows").profile().to_string()"#,
            )
            .expect("first statement");
        assert!(
            !disabled.to_string().contains("Optimizer rules applied:"),
            "expected no rules to fire with optimizer.disabled: [\"all\"], got: {disabled}"
        );

        // Same scope, same `g`, a plain query with no .with() at all: must optimize normally,
        // not silently inherit the previous statement's optimizer.disabled.
        let default: rhai::Dynamic = engine
            .eval_with_scope(
                &mut scope,
                r#"g.V().hasLabel("person").out("knows").profile().to_string()"#,
            )
            .expect("second statement");
        let default_text = default.to_string();
        assert!(
            default_text.contains("Optimizer rules applied:"),
            "optimizer.disabled leaked from the prior statement into this one: {default_text}"
        );
        assert!(
            default_text.contains("source_filter_pushdown"),
            "{default_text}"
        );

        // optimizer.enabled: ["all"] alone (no optimizer.disabled on this statement) must not
        // fail as a contradiction — it only would if the prior disabled: ["all"] had leaked too.
        let enabled: rhai::Dynamic = engine
            .eval_with_scope(
                &mut scope,
                r#"g.with("optimizer.enabled", ["all"]).V().count().next()"#,
            )
            .expect("third statement must not fail as a disabled/enabled contradiction");
        assert_eq!(enabled.as_int().unwrap(), 6);
    }

    #[test]
    fn test_script_eval_schema_visualizer_option() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"g.set_visualizer(V.Mermaid); g.v().count().to_list()"#;
        let res = eval(graph, script);
        assert!(res.is_ok(), "set_visualizer failed: {:?}", res.err());
    }

    #[test]
    fn test_script_eval_table_visualization() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // Default to_table
        let res_table =
            eval(graph.clone(), r#"g.V().hasLabel("person").to_table()"#).expect("to_table failed");
        assert!(res_table.contains("marko"));
        assert!(res_table.contains("name"));

        // to_table with column inclusion array
        let res_inc = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").to_table(["name", "age"])"#,
        )
        .expect("to_table array failed");
        assert!(res_inc.contains("name"));
        assert!(res_inc.contains("age"));
        assert!(!res_inc.contains("label"));

        // to_markdown with column exclusion array
        let res_exc = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").to_markdown(["!id", "!label"])"#,
        )
        .expect("to_markdown exclusion failed");
        assert!(res_exc.contains("age"));
        assert!(res_exc.contains("name"));
        assert!(!res_exc.contains("label"));
        assert!(!res_exc.contains("id"));

        // to_markdown with map options
        let res_map = eval(graph.clone(), r#"g.V().hasLabel("person").to_markdown(#{ row_numbers: true, columns: ["!id", "!label"] })"#).expect("to_markdown map failed");
        assert!(res_map.contains("#"));
        assert!(res_map.contains("age"));
        assert!(res_map.contains("name"));
        assert!(res_map.contains("marko"));

        // camelCase alias toTable / toMarkdown
        let res_alias = eval(
            graph.clone(),
            r#"g.V().hasLabel("person").toTable(["name"])"#,
        )
        .expect("toTable alias failed");
        assert!(res_alias.contains("name"));
    }

    #[test]
    fn test_script_eval_tree_visualization() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // to_tree() on tree()'s own nested-Object output: box-drawing hierarchy, root unwrapped.
        let res_tree = eval(
            graph.clone(),
            r#"g.V("1").repeat(__.out()).times(2).tree().by("name").to_tree()"#,
        )
        .expect("to_tree failed");
        assert_eq!(res_tree.lines().next(), Some("marko"));
        assert!(res_tree.contains("└─ josh"));

        // Generic, non-tree()-shaped input degrades to a flat list of root lines rather than
        // erroring — the same VisualizeFormat::Tree path handles both.
        let res_flat = eval(graph.clone(), r#"g.V().hasLabel("person").toTree()"#)
            .expect("toTree alias failed");
        assert_eq!(res_flat.lines().count(), 4);
    }

    /// `toList()` hands script code real data, not rendered text: results bind to a variable,
    /// keep their native script types, and stay addressable by property key.
    #[test]
    fn test_script_to_list_returns_native_values() {
        use crate::script_host::{eval_value, rhai};

        let graph = Arc::new(GraphSource::tinkerpop_modern());

        // Aggregations arrive as numbers, not as text.
        let count = eval_value(
            graph.clone(),
            r#"g.V().hasLabel("person").count().toList()[0]"#,
        )
        .expect("count evaluation failed");
        assert_eq!(count.as_int().expect("count must be an integer"), 4);

        // Vertices arrive as Vertex values whose properties are addressable by key.
        let name = eval_value(graph.clone(), r#"g.V("1").toList()[0].name"#)
            .expect("vertex property access failed");
        assert_eq!(name.into_string().expect("name must be a string"), "marko");

        // Edges keep their endpoints and label.
        let edge = eval_value(graph.clone(), r#"g.E("0").toList()[0]"#)
            .expect("edge evaluation failed")
            .cast::<graphersal::script::ScriptEdge>();
        let edge = edge.as_edge_value();
        assert_eq!(edge.label(), Some("knows"));
        assert_eq!(edge.out_v().id(), "1");
        assert_eq!(edge.in_v().id(), "2");
        assert_eq!(
            edge.properties()["weight"]
                .as_f64()
                .expect("weight must be a float"),
            0.5
        );

        // The whole result set binds to a variable and is processed with plain script code.
        let labelled = eval_value(
            graph.clone(),
            r#"
                let people = g.V().hasLabel("person").toList();
                let labelled = [];
                for person in people {
                    labelled.push(person.label + ":" + person.name);
                }
                labelled
            "#,
        )
        .expect("iteration over results failed")
        .cast::<rhai::Array>();
        assert_eq!(labelled.len(), 4);
        assert!(
            labelled
                .iter()
                .any(|item| item.clone().into_string().unwrap() == "person:marko")
        );

        // `values()` yields plain strings that feed straight back into the DSL.
        let round_trip = eval_value(
            graph,
            r#"
                let names = g.V().hasLabel("person").values("name").toList();
                g.V().has("name", within(names)).count().toList()[0]
            "#,
        )
        .expect("round trip through within() failed");
        assert_eq!(round_trip.as_int().unwrap(), 4);
    }

    /// A non-string group key keeps its type (a vertex stays a vertex snapshot, a number a number),
    /// and the key prints through the display text, never the `Debug` rendering such as `v["1"]`.
    #[test]
    fn test_script_group_keys_keep_their_type() {
        use crate::script_host::eval_value;

        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let by_vertex = eval_value(
            graph.clone(),
            r#"g.V("1").groupCount().by(__.identity()).next().keys()[0].id"#,
        )
        .expect("groupCount() by vertex failed");
        assert_eq!(
            by_vertex.into_string().unwrap(),
            "1",
            "the key is the vertex itself, not its text"
        );

        let by_age = eval_value(graph, r#"g.V("1").groupCount().by("age").next()[29]"#)
            .expect("groupCount() by age failed");
        assert_eq!(by_age.as_int().unwrap(), 1);
    }

    /// `next()` materializes a single result, and yields `()` for an empty traversal.
    #[test]
    fn test_script_next_returns_single_value() {
        use crate::script_host::eval_value;

        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let name =
            eval_value(graph.clone(), r#"g.V("1").values("name").next()"#).expect("next() failed");
        assert_eq!(name.into_string().unwrap(), "marko");

        let empty = eval_value(graph, r#"g.V().hasLabel("missing").next()"#)
            .expect("next() on an empty traversal failed");
        assert!(
            empty.is_unit(),
            "an empty traversal must yield (), got: {empty:?}"
        );
    }

    /// A traversal that was never terminated is rendered with the default visualizer, and
    /// `set_visualizer(...)` overrides that strategy for every pipeline derived from `g`.
    #[test]
    fn test_script_default_visualization_strategy() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let default_render = eval(graph.clone(), r#"g.V().hasLabel("person")"#)
            .expect("implicit visualization failed");
        assert!(
            default_render.contains("marko"),
            "the default visualization must render the result: {default_render}"
        );

        let overridden = eval(
            graph.clone(),
            r#"g.set_visualizer(V.Mermaid); g.V().hasLabel("person")"#,
        )
        .expect("overridden visualization failed");
        // Mermaid has no rendering for traversal results: they fall back to one compact JSON
        // value per line, never their `Debug` text.
        assert!(
            overridden.contains(r#""id":"1""#) && !overridden.contains(r#"v["1"]"#),
            "set_visualizer must switch the implicit rendering: {overridden}"
        );

        // The explicit no-argument terminal step honours the same setting.
        let explicit = eval(
            graph.clone(),
            r#"g.set_visualizer(V.Mermaid); g.V().hasLabel("person").visualize()"#,
        )
        .expect("visualize() failed");
        assert_eq!(explicit, overridden);

        // An explicit format still wins over the configured default.
        let forced = eval(
            graph,
            r#"g.set_visualizer(V.Mermaid); g.V().hasLabel("person").visualize(V.Table)"#,
        )
        .expect("visualize(V.Table) failed");
        assert!(
            !forced.contains(r#"v["1"]"#),
            "an explicit format must win over the configured default: {forced}"
        );
    }

    /// A failing traversal fails the script instead of returning its diagnostic as data.
    #[test]
    fn test_script_to_list_reports_failures_as_errors() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let err = eval(graph, r#"g.V().where_t(123).toList()"#)
            .expect_err("an invalid traversal must fail the script");
        assert!(
            !err.to_string().is_empty(),
            "the script error must carry a message"
        );
    }

    /// `iterate()` runs a mutating traversal for its side effects and returns unit, in both
    /// spellings of the surrounding steps; a failing `iterate()` rolls its traversal back.
    #[test]
    fn test_script_iterate_runs_for_side_effects() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let unit = eval_value(graph.clone(), r#"g.addV("x").property("a", 1).iterate()"#)
            .expect("iterate() must run");
        assert!(unit.is_unit(), "iterate() returns unit, got {unit:?}");
        let count = eval_value(
            graph.clone(),
            r#"g.add_v("x").iterate(); g.V().hasLabel("x").count().next()"#,
        )
        .expect("count");
        assert_eq!(count.as_int().ok(), Some(2));
        let err = eval(
            graph.clone(),
            r#"g.V("1").property("age", 99).fail("stop").iterate()"#,
        )
        .expect_err("fail() must fail the script");
        assert!(err.to_string().contains("stop"), "{err}");
        let age = eval_value(graph, r#"g.V("1").values("age").next()"#).expect("age");
        assert_eq!(
            age.as_int().ok(),
            Some(29),
            "the failed iterate() is rolled back"
        );
    }

    /// Evaluates a script against the modern graph and returns the raw value of its last
    /// statement.
    fn eval_on_modern(script: &str) -> rhai::Dynamic {
        use crate::script_host::eval_value;

        eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
            .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
    }

    /// Every registered spelling of the coercion steps produces the same pipeline, so a Gremlin
    /// query transfers unchanged whichever casing it uses (`AGENTS.md` rule 2).
    #[test]
    fn test_script_coercion_step_spellings() {
        let cases = [
            (
                r#"g.V().hasLabel("person").values("age").as_string().count().next()"#,
                4,
            ),
            (
                r#"g.V().hasLabel("person").values("age").asString().count().next()"#,
                4,
            ),
            (r#"g.inject("true").as_bool().count().next()"#, 1),
            (r#"g.inject("true").asBool().count().next()"#, 1),
            (
                r#"g.V().hasLabel("person").values("age").as_number().count().next()"#,
                4,
            ),
            (
                r#"g.V().hasLabel("person").values("age").asNumber().count().next()"#,
                4,
            ),
            (
                r#"g.V().hasLabel("person").values("age").as_number(GType::LONG).count().next()"#,
                4,
            ),
            (
                r#"g.V().hasLabel("person").values("age").asNumber(GType.LONG).count().next()"#,
                4,
            ),
            (
                r#"g.V().hasLabel("person").values("age").cast(GType::DOUBLE).count().next()"#,
                4,
            ),
            (
                r#"g.V().hasLabel("person").values("age").cast(GType.DOUBLE).count().next()"#,
                4,
            ),
        ];

        for (script, expected) in cases {
            let value = eval_on_modern(script);
            assert_eq!(
                value
                    .as_int()
                    .unwrap_or_else(|err| panic!("script `{script}` did not yield a count: {err}")),
                expected,
                "unexpected result for `{script}`"
            );
        }
    }

    /// `as_number()` keeps the natural numeric type, `as_number(GType)` pins it, and the result
    /// reaches the script as a native value.
    #[test]
    fn test_script_as_number_targets() {
        let natural = eval_on_modern(r#"g.inject("12").as_number().next()"#);
        assert_eq!(natural.as_int().unwrap(), 12);

        let pinned = eval_on_modern(r#"g.inject("12").as_number(GType::DOUBLE).next()"#);
        assert_eq!(pinned.as_float().unwrap(), 12.0);

        let rendered = eval_on_modern(r#"g.inject(29).as_string().next()"#);
        assert_eq!(rendered.into_string().unwrap(), "29");
    }

    /// Every spelling of the `typeOf()` predicate resolves, in both the dotted vanilla form and
    /// the Rhai module path form.
    #[test]
    fn test_script_type_of_spellings() {
        let cases = [
            r#"g.V().values("age").is(typeOf(GType::LONG)).count().next()"#,
            r#"g.V().values("age").is(P.typeOf(GType.LONG)).count().next()"#,
            r#"g.V().values("age").is(P::typeOf(GType::LONG)).count().next()"#,
            r#"g.V().values("age").is(P::TypeOf(GType::LONG)).count().next()"#,
        ];

        // `type_of` is a Rhai keyword and language built-in reporting a value's script type
        // name, so the predicate carries only its vanilla camelCase spelling here. The Rust API
        // spells it `P::type_of`.
        let builtin = eval_on_modern("type_of(42)");
        assert_eq!(builtin.into_string().unwrap(), "i64");

        for script in cases {
            let value = eval_on_modern(script);
            assert_eq!(
                value
                    .as_int()
                    .unwrap_or_else(|err| panic!("script `{script}` did not yield a count: {err}")),
                4,
                "unexpected result for `{script}`"
            );
        }
    }

    /// `typeOf()` names graph elements, which is what separates a mixed stream.
    #[test]
    fn test_script_type_of_element_tokens() {
        let vertices = eval_on_modern(r#"g.V().is(typeOf(GType::VERTEX)).count().next()"#);
        assert_eq!(vertices.as_int().unwrap(), 6);

        let edges = eval_on_modern(r#"g.E().is(typeOf(GType::EDGE)).count().next()"#);
        assert_eq!(edges.as_int().unwrap(), 6);

        let not_a_map = eval_on_modern(r#"g.V().is(typeOf(GType::MAP)).count().next()"#);
        assert_eq!(not_a_map.as_int().unwrap(), 0);
    }

    /// The anonymous form is registered too, which is what a sub-traversal argument needs.
    #[test]
    fn test_script_coercion_in_an_anonymous_traversal() {
        let value = eval_on_modern(
            r#"g.V().hasLabel("person").where(__.values("age").as_number(GType::LONG)).count().next()"#,
        );
        assert_eq!(value.as_int().unwrap(), 4);
    }

    /// Each `CastPolicy` variant is reachable from a script.
    #[test]
    fn test_script_cast_policies() {
        let skipped = eval_on_modern(
            r#"g.V().hasLabel("person").values("name").as_number(GType::LONG, CastPolicy::Skip).count().next()"#,
        );
        assert_eq!(skipped.as_int().unwrap(), 0);

        let defaulted = eval_on_modern(
            r#"g.V().hasLabel("person").values("name").as_number(GType::LONG, CastPolicy::Default(0)).count().next()"#,
        );
        assert_eq!(defaulted.as_int().unwrap(), 4);

        let strict = eval(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().hasLabel("person").values("name").as_number(GType::LONG, CastPolicy::Error).toList()"#,
        );
        assert!(
            strict.is_err(),
            "the explicit Error policy must fail the script"
        );
    }

    /// `ProfileType` bitmask spec (D5): the pre-existing `By`/`CastPolicy` DSL-parity gap — only
    /// `::` (static module) access worked, `.` (dot) access failed with "Variable not found" —
    /// is fixed. Both spellings now reach every `By`/`CastPolicy` variant, mirroring the dual
    /// registration `P`/`GType`/`TextP`/`V` already had.
    #[test]
    fn test_script_by_and_cast_policy_dot_and_colon_access() {
        use std::sync::Arc as StdArc;
        let eval_str = |script: &str| {
            crate::script_host::eval(StdArc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
        };

        // `By`'s no-argument variants, dot and colon.
        let dot = eval_str(r#"g.V().groupCount().by(By.Label).toList()"#);
        assert!(
            dot.contains("person") && dot.contains("software"),
            "Got: {dot}"
        );
        let colon = eval_str(r#"g.V().groupCount().by(By::Label).toList()"#);
        assert!(
            colon.contains("person") && colon.contains("software"),
            "Got: {colon}"
        );

        let dot_count = eval_str(r#"g.V().group().by(By.Label).by(By.Count).toList()"#);
        assert!(
            dot_count.contains("person") && dot_count.contains("software"),
            "Got: {dot_count}"
        );

        // `By`'s argument-taking constructor, dot and colon.
        let dot_prop =
            eval_str(r#"g.V().hasLabel("person").group().by(By.Property("name")).toList()"#);
        assert!(dot_prop.contains("marko"), "Got: {dot_prop}");
        let colon_prop =
            eval_str(r#"g.V().hasLabel("person").group().by(By::Property("name")).toList()"#);
        assert!(colon_prop.contains("marko"), "Got: {colon_prop}");

        // `CastPolicy`'s no-argument variants, dot and colon.
        let dot_skip = eval_on_modern(
            r#"g.V().hasLabel("person").values("name").as_number(GType::LONG, CastPolicy.Skip).count().next()"#,
        );
        assert_eq!(dot_skip.as_int().unwrap(), 0);
        let colon_skip = eval_on_modern(
            r#"g.V().hasLabel("person").values("name").as_number(GType::LONG, CastPolicy::Skip).count().next()"#,
        );
        assert_eq!(colon_skip.as_int().unwrap(), 0);

        // `CastPolicy::Default(value)`, dot and colon.
        let dot_default = eval_on_modern(
            r#"g.V().hasLabel("person").values("name").as_number(GType::LONG, CastPolicy.Default(0)).count().next()"#,
        );
        assert_eq!(dot_default.as_int().unwrap(), 4);
    }

    /// order() comparison-infra spec (part 1 of the `order()` series), D2: `Order` never had
    /// either half of dot/colon dual registration before this spec. Confirms both
    /// `Order.asc`/`Order.desc` (dot, Java Gremlin style) and `Order::Asc`/`Order::Desc` (colon,
    /// this crate's Rust-style static module) parse and evaluate to the same underlying `Order`
    /// value — following the exact `By`/`CastPolicy` precedent (`test_script_by_and_cast_policy_
    /// dot_and_colon_access` above), not a new mechanism.
    #[test]
    fn test_script_order_dot_and_colon_access() {
        use std::sync::Arc as StdArc;
        let eval_string = |script: &str| -> String {
            let graph = StdArc::new(GraphSource::tinkerpop_modern());
            eval_value(graph, script)
                .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
                .to_string()
        };

        let dot_asc = eval_string(r#"Order.asc.to_string()"#);
        let colon_asc = eval_string(r#"Order::Asc.to_string()"#);
        assert_eq!(
            dot_asc, colon_asc,
            "Order.asc and Order::Asc must be the same value"
        );
        assert_eq!(dot_asc, "Order.asc");

        let dot_desc = eval_string(r#"Order.desc.to_string()"#);
        let colon_desc = eval_string(r#"Order::Desc.to_string()"#);
        assert_eq!(
            dot_desc, colon_desc,
            "Order.desc and Order::Desc must be the same value"
        );
        assert_eq!(dot_desc, "Order.desc");

        // Asc and Desc are themselves distinct, both ways.
        assert_ne!(dot_asc, dot_desc);
        assert_ne!(colon_asc, colon_desc);

        // `Order` is unique among this crate's dual-registered tokens: its dot form used
        // lowercase (`Order.asc`/`Order.desc`, matching real Gremlin-Groovy) while `::` used
        // PascalCase (`Order::Asc`/`Order::Desc`, matching the Rust enum variant names) - so a
        // mechanical `::` -> `.` substitution on copied Rust-style code silently failed. The
        // PascalCase spelling must also work via dot access, so both substitution directions
        // hold, exactly as they already do for every other dual-registered token in this crate.
        let dot_asc_pascal = eval_string(r#"Order.Asc.to_string()"#);
        let dot_desc_pascal = eval_string(r#"Order.Desc.to_string()"#);
        assert_eq!(
            dot_asc_pascal, dot_asc,
            "Order.Asc and Order.asc must be the same value"
        );
        assert_eq!(
            dot_desc_pascal, dot_desc,
            "Order.Desc and Order.desc must be the same value"
        );
    }

    /// `ProfileType` bitmask spec (D5): `ProfileType.Memory` (dot) and `ProfileType::Memory`
    /// (colon) both reach `g.profile(flags)`, and both produce the memory-columned table.
    #[test]
    fn test_script_profile_type_dot_and_colon_access() {
        let eval_str = |script: &str| {
            crate::script_host::eval(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
        };

        let dot = eval_str(r#"g.V().hasLabel("person").count().profile(ProfileType.Memory)"#);
        assert!(dot.contains("Mem"), "Got: {dot}");
        assert!(dot.contains("TOTAL: memory materialized:"), "Got: {dot}");

        let colon = eval_str(r#"g.V().hasLabel("person").count().profile(ProfileType::Memory)"#);
        assert!(colon.contains("Mem"), "Got: {colon}");
        assert!(
            colon.contains("TOTAL: memory materialized:"),
            "Got: {colon}"
        );

        // Bare `.profile()` (no argument) still shows no memory data.
        let bare = eval_str(r#"g.V().hasLabel("person").count().profile().to_string()"#);
        assert!(!bare.contains("Mem"), "Got: {bare}");
    }

    /// An unsupported conversion target fails loudly instead of passing the value through.
    #[test]
    fn test_script_unsupported_cast_target_is_diagnosed() {
        let err = eval(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().values("age").cast(GType::LIST).toList()"#,
        )
        .expect_err("a family target must not evaluate")
        .to_string();
        assert!(
            err.contains("Unsupported cast target 'list'"),
            "the target must be named, got: {err}"
        );
    }

    /// UUID is a working cast target (design decision D7): the Rhai binding is exact parity
    /// with the Rust `.cast(&ValueType::Uuid)` API (`AGENTS.md` rule 2). The result reaches the
    /// script as a native `Uuid` (type system step 4), not as a string.
    #[test]
    fn test_script_uuid_cast_target_converts_a_canonical_string() {
        let value = eval_on_modern(
            r#"let x = g.inject("550e8400-e29b-41d4-a716-446655440000").cast(GType::UUID).next();
               [type_of(x), x.to_string()]"#,
        );
        let value = value.into_typed_array::<String>().unwrap();
        assert_eq!(value, vec!["Uuid", "550e8400-e29b-41d4-a716-446655440000"]);
    }

    /// A `GType` token prints as its DSL literal (A07); `INT` and `FLOAT` are the same values as
    /// `LONG` and `DOUBLE`.
    #[test]
    fn test_script_gtype_names_are_pinned() {
        let tokens = [
            ("NULL", "GType.NULL"),
            ("BOOLEAN", "GType.BOOLEAN"),
            ("INT", "GType.LONG"),
            ("LONG", "GType.LONG"),
            ("FLOAT", "GType.DOUBLE"),
            ("DOUBLE", "GType.DOUBLE"),
            ("NUMBER", "GType.NUMBER"),
            ("STRING", "GType.STRING"),
            ("UUID", "GType.UUID"),
            ("LIST", "GType.LIST"),
            ("MAP", "GType.MAP"),
            ("VERTEX", "GType.VERTEX"),
            ("EDGE", "GType.EDGE"),
            ("PATH", "GType.PATH"),
        ];

        for (token, rendered) in tokens {
            for script in [
                format!("GType::{token}.to_string()"),
                format!("GType.{token}.to_string()"),
            ] {
                let value = eval_on_modern(&script);
                assert_eq!(
                    value.into_string().unwrap(),
                    rendered,
                    "unexpected rendering for `{script}`"
                );
            }
        }
    }

    /// `.profile()` names a coercion step the way the query wrote it (`AGENTS.md` rule 11).
    #[test]
    fn test_script_profile_names_the_coercion_step() {
        let plan = eval(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().hasLabel("person").values("age").as_number(GType::LONG).max().profile().to_string()"#,
        )
        .expect("profile failed");
        assert!(
            plan.contains("as_number(GType.LONG)"),
            "the plan must name the step as written: {plan}"
        );
        assert!(
            plan.contains(r#"v(labels: ["person"])"#),
            "the plan must stay the optimized physical one: {plan}"
        );
    }

    /// The originating fix, end to end: a `person.age` field holding four integers and one
    /// string is normalized to a single `Int64` by one query, verified by re-inferring the
    /// schema afterwards.
    #[test]
    fn test_script_property_traversal_normalizes_mixed_types() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let before = eval(
            graph.clone(),
            r#"g.V("4").property("age", "999").toList();
               g.V().hasLabel("person").inferSchema().toMarkdown()"#,
        )
        .expect("setup script failed");
        assert!(
            before.contains("union"),
            "the fixture must start with a mixed-type age field: {before}"
        );

        let after = eval(
            graph,
            r#"g.V().hasLabel("person").property("age", __.values("age").as_number(GType::LONG)).toList();
               g.V().hasLabel("person").inferSchema().toMarkdown()"#,
        )
        .expect("normalization script failed");
        assert!(
            !after.contains("union"),
            "the age field must no longer be a union: {after}"
        );
        assert!(
            after.contains("| age | int64 |"),
            "the age field must be a single int64: {after}"
        );
    }

    /// A nested traversal used as `property()`'s value yields only its first result; producing a
    /// list requires an explicit `.fold()`.
    #[test]
    fn test_script_property_traversal_takes_only_first_result() {
        use crate::script_host::{eval_value, rhai};

        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let expected_ages: Vec<i64> = eval_value(
            graph.clone(),
            r#"g.V("1").out("knows").values("age").toList()"#,
        )
        .expect("reference script failed")
        .cast::<rhai::Array>()
        .into_iter()
        .map(|v| v.as_int().expect("age is numeric"))
        .collect();
        assert!(
            expected_ages.len() > 1,
            "the fixture must have more than one known vertex: {expected_ages:?}"
        );

        let first_value = eval_value(
            graph.clone(),
            r#"g.V("1").property("first_known_age", __.out("knows").values("age")).toList();
               g.V("1").values("first_known_age").toList()"#,
        )
        .expect("first-result script failed")
        .cast::<rhai::Array>();
        assert_eq!(first_value.len(), 1);
        assert_eq!(first_value[0].as_int().unwrap(), expected_ages[0]);

        let folded_value: Vec<i64> = eval_value(
            graph,
            r#"g.V("1").property("all_known_ages", __.out("knows").values("age").fold()).toList();
               g.V("1").values("all_known_ages").toList()"#,
        )
        .expect(".fold() script failed")
        .cast::<rhai::Array>()[0]
            .clone()
            .cast::<rhai::Array>()
            .into_iter()
            .map(|v| v.as_int().expect("age is numeric"))
            .collect();
        assert_eq!(folded_value, expected_ages);
    }

    /// A nested traversal that yields nothing leaves the property untouched rather than writing
    /// `Null` (a failed conversion must not turn into data loss).
    #[test]
    fn test_script_property_traversal_empty_result_leaves_property_unchanged() {
        use crate::script_host::{eval_value, rhai};

        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let before = eval_value(graph.clone(), r#"g.V("1").values("nickname").toList()"#)
            .expect("setup script failed")
            .cast::<rhai::Array>();
        assert!(before.is_empty(), "unexpected baseline: {before:?}");

        let after = eval_value(
            graph,
            r#"g.V("1").property("nickname", __.values("nickname")).toList();
               g.V("1").values("nickname").toList()"#,
        )
        .expect("no-op script failed")
        .cast::<rhai::Array>();
        assert!(after.is_empty(), "the property must stay unset: {after:?}");
    }

    /// Builds a named parameter map from `(name, value)` pairs.
    fn named(params: Vec<(&str, ElementProperty)>) -> HashMap<String, ElementProperty> {
        params
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }

    /// A string parameter is bound as a variable and drives a `has_label` filter.
    #[test]
    fn test_eval_with_params_binds_string() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let rendered = eval_with_params(
            graph,
            r#"g.v().has_label(target_label).values("name").to_list()"#,
            named(vec![(
                "target_label",
                ElementProperty::String("software".into()),
            )]),
        )
        .expect("parameterized evaluation failed");

        assert!(rendered.contains("lop"), "unexpected output: {rendered}");
        assert!(rendered.contains("ripple"), "unexpected output: {rendered}");
        assert!(
            !rendered.contains("marko"),
            "the label filter must exclude person vertices: {rendered}"
        );
    }

    /// A numeric parameter reaches a predicate argument as a number, not as text.
    #[test]
    fn test_eval_with_params_binds_number() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let rendered = eval_with_params(
            graph,
            r#"g.v().has_label("person").has("age", P.gt(min_age)).values("name").to_list()"#,
            named(vec![(
                "min_age",
                ElementProperty::Number(Number::from_i64(30)),
            )]),
        )
        .expect("parameterized evaluation failed");

        assert!(rendered.contains("josh"), "unexpected output: {rendered}");
        assert!(rendered.contains("peter"), "unexpected output: {rendered}");
        assert!(
            !rendered.contains("vadas"),
            "vadas is 27 and must not pass P.gt(30): {rendered}"
        );
    }

    /// A boolean parameter is bound as a boolean and can steer script control flow.
    #[test]
    fn test_eval_with_params_binds_boolean() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let script = r#"
            if only_people {
                g.v().has_label("person").count().next()
            } else {
                g.v().count().next()
            }
        "#;

        let people = eval_with_params(
            graph.clone(),
            script,
            named(vec![("only_people", ElementProperty::Boolean(true))]),
        )
        .expect("parameterized evaluation failed");
        assert!(people.contains('4'), "unexpected output: {people}");

        let everything = eval_with_params(
            graph,
            script,
            named(vec![("only_people", ElementProperty::Boolean(false))]),
        )
        .expect("parameterized evaluation failed");
        assert!(everything.contains('6'), "unexpected output: {everything}");
    }

    /// Named parameters are also reachable as entries of the `params` object map.
    #[test]
    fn test_eval_with_params_exposes_params_map() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let rendered = eval_with_params(
            graph,
            r#"g.v().has_label(params.target_label).count().next()"#,
            named(vec![(
                "target_label",
                ElementProperty::String("person".into()),
            )]),
        )
        .expect("parameterized evaluation failed");

        assert!(rendered.contains('4'), "unexpected output: {rendered}");
    }

    /// An array parameter arrives as a script array and can be spread over `within`.
    #[test]
    fn test_eval_with_params_binds_array() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let rendered = eval_with_params(
            graph,
            r#"g.v().has("name", P.within(names)).values("name").to_list()"#,
            named(vec![(
                "names",
                ElementProperty::Array(vec![
                    ElementProperty::String("marko".into()),
                    ElementProperty::String("ripple".into()),
                ]),
            )]),
        )
        .expect("parameterized evaluation failed");

        assert!(rendered.contains("marko"), "unexpected output: {rendered}");
        assert!(rendered.contains("ripple"), "unexpected output: {rendered}");
        assert!(
            !rendered.contains("vadas"),
            "only the listed names must pass: {rendered}"
        );
    }

    /// `eval_value_with_params` returns the raw script value instead of a rendering.
    #[test]
    fn test_eval_value_with_params_returns_raw_value() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let value = eval_value_with_params(
            graph,
            r#"g.v().has_label(target_label).values("name").to_list()"#,
            named(vec![(
                "target_label",
                ElementProperty::String("software".into()),
            )]),
        )
        .expect("parameterized evaluation failed");

        let names = value.cast::<crate::script_host::rhai::Array>();
        assert_eq!(names.len(), 2, "expected both software vertices");
    }

    /// A parameter name shadowing a DSL global is rejected before the script runs.
    #[test]
    fn test_eval_with_params_rejects_reserved_name() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let err = eval_with_params(
            graph,
            r#"g.v().count().next()"#,
            named(vec![("g", ElementProperty::Boolean(true))]),
        )
        .expect_err("a parameter shadowing `g` must be rejected");

        let message = err.to_string();
        assert!(
            message.contains("Invalid script parameter 'g'"),
            "unexpected diagnostic: {message}"
        );
        assert!(
            message.contains("reserved by the script scope"),
            "unexpected diagnostic: {message}"
        );
        assert!(
            message.contains("Help:"),
            "the diagnostic must carry actionable help: {message}"
        );
    }

    /// A parameter name that is not an identifier is rejected before the script runs.
    #[test]
    fn test_eval_with_params_rejects_non_identifier_name() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let err = eval_with_params(
            graph,
            r#"g.v().count().next()"#,
            named(vec![("2nd label", ElementProperty::Boolean(true))]),
        )
        .expect_err("a non-identifier parameter name must be rejected");

        let message = err.to_string();
        assert!(
            message.contains("Invalid script parameter '2nd label'"),
            "unexpected diagnostic: {message}"
        );
        assert!(
            message.contains("must be an identifier"),
            "unexpected diagnostic: {message}"
        );
    }

    /// Positional placeholders `$1` and `$2` bind in order and keep their value types.
    #[test]
    fn test_eval_with_positional_params_binds_in_order() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let rendered = eval_with_positional_params(
            graph,
            r#"g.v().has_label($1).has("age", P.gt($2)).values("name").to_list()"#,
            vec![
                ElementProperty::String("person".into()),
                ElementProperty::Number(Number::from_i64(30)),
            ],
        )
        .expect("positional evaluation failed");

        assert!(rendered.contains("josh"), "unexpected output: {rendered}");
        assert!(rendered.contains("peter"), "unexpected output: {rendered}");
        assert!(
            !rendered.contains("marko"),
            "marko is 29 and must not pass P.gt(30): {rendered}"
        );
    }

    /// Positional parameters are also reachable as `params[0]`, `params[1]`.
    #[test]
    fn test_eval_with_positional_params_exposes_params_array() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let rendered = eval_with_positional_params(
            graph,
            r#"g.v().has_label(params[0]).count().next()"#,
            vec![ElementProperty::String("software".into())],
        )
        .expect("positional evaluation failed");

        assert!(rendered.contains('2'), "unexpected output: {rendered}");
    }

    /// The same placeholder may be referenced more than once.
    #[test]
    fn test_eval_with_positional_params_allows_repeated_placeholder() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let rendered = eval_with_positional_params(
            graph,
            r#"g.v().has_label($1).count().next() + g.v().has_label($1).count().next()"#,
            vec![ElementProperty::String("person".into())],
        )
        .expect("positional evaluation failed");

        assert!(rendered.contains('8'), "unexpected output: {rendered}");
    }

    /// A `$` sequence inside a string literal stays literal text.
    #[test]
    fn test_eval_with_positional_params_ignores_placeholders_in_literals() {
        let graph = Arc::new(GraphSource::empty());

        let rendered = eval_with_positional_params(
            graph,
            r#"
                // $9 in a comment is not a placeholder either
                g.add_v($1).property("note", "cost is $2 per unit").to_list();
                g.v().values("note").to_list()
            "#,
            vec![ElementProperty::String("item".into())],
        )
        .expect("positional evaluation failed");

        assert!(
            rendered.contains("cost is $2 per unit"),
            "a placeholder inside a string literal must stay literal: {rendered}"
        );
    }

    /// A placeholder with no matching parameter fails before the script is parsed.
    #[test]
    fn test_eval_with_positional_params_rejects_out_of_range() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let err = eval_with_positional_params(
            graph,
            r#"g.v().has_label($1).has("age", P.gt($2)).to_list()"#,
            vec![ElementProperty::String("person".into())],
        )
        .expect_err("an unbound placeholder must be rejected");

        let message = err.to_string();
        assert!(
            message.contains("Invalid script parameter '$2'"),
            "unexpected diagnostic: {message}"
        );
        assert!(
            message.contains("only 1 parameter(s) were supplied"),
            "unexpected diagnostic: {message}"
        );
    }

    /// Positional placeholders are 1-based, so `$0` is reported as a mistake.
    #[test]
    fn test_eval_with_positional_params_rejects_zero_index() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());

        let err = eval_with_positional_params(
            graph,
            r#"g.v().has_label($0).to_list()"#,
            vec![ElementProperty::String("person".into())],
        )
        .expect_err("`$0` must be rejected");

        let message = err.to_string();
        assert!(
            message.contains("1-based"),
            "unexpected diagnostic: {message}"
        );
    }

    /// `P.WithIn`/`P.WithOut` (dot, PascalCase) is the same casing gap `Order.Desc` had before
    /// its fix (`24f82a1`): only the lowercase Java-Gremlin spelling was reachable via dot access,
    /// even though `P::WithIn`/`P::WithOut` (colon) already worked. All four spellings —
    /// `P.within`, `P.WithIn`, `P::within`, `P::WithIn` (and their `without`/`WithOut`
    /// counterparts) — must now produce identical results.
    #[test]
    fn test_script_p_within_without_dot_pascal_case() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let names = |script: &str| -> String {
            eval(graph.clone(), script)
                .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
        };

        let dot_lower = names(
            r#"g.v().has_label("person").has("age", P.within([29, 27])).values("name").to_list()"#,
        );
        let dot_pascal = names(
            r#"g.v().has_label("person").has("age", P.WithIn([29, 27])).values("name").to_list()"#,
        );
        let colon_lower = names(
            r#"g.v().has_label("person").has("age", P::within([29, 27])).values("name").to_list()"#,
        );
        let colon_pascal = names(
            r#"g.v().has_label("person").has("age", P::WithIn([29, 27])).values("name").to_list()"#,
        );
        assert_eq!(dot_pascal, dot_lower, "P.WithIn must match P.within");
        assert_eq!(dot_pascal, colon_lower, "P.WithIn must match P::within");
        assert_eq!(dot_pascal, colon_pascal, "P.WithIn must match P::WithIn");
        assert!(dot_pascal.contains("marko") && dot_pascal.contains("vadas"));

        let dot_lower = names(
            r#"g.v().has_label("person").has("age", P.without([29, 27])).values("name").to_list()"#,
        );
        let dot_pascal = names(
            r#"g.v().has_label("person").has("age", P.WithOut([29, 27])).values("name").to_list()"#,
        );
        assert_eq!(dot_pascal, dot_lower, "P.WithOut must match P.without");
        assert!(dot_pascal.contains("josh") && dot_pascal.contains("peter"));
    }

    /// True positional multi-arg predicate args: `P.within(29, 27)` (no array literal) must match
    /// the pre-existing array form `P.within([29, 27])` exactly — same for `P.without`.
    #[test]
    fn test_script_p_within_without_true_positional() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let names = |script: &str| -> String {
            eval(graph.clone(), script)
                .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
        };

        let positional = names(
            r#"g.v().has_label("person").has("age", P.within(29, 27)).values("name").to_list()"#,
        );
        let array = names(
            r#"g.v().has_label("person").has("age", P.within([29, 27])).values("name").to_list()"#,
        );
        assert_eq!(
            positional, array,
            "P.within(29, 27) must match P.within([29, 27])"
        );

        let positional = names(
            r#"g.v().has_label("person").has("age", P.without(29, 27)).values("name").to_list()"#,
        );
        let array = names(
            r#"g.v().has_label("person").has("age", P.without([29, 27])).values("name").to_list()"#,
        );
        assert_eq!(
            positional, array,
            "P.without(29, 27) must match P.without([29, 27])"
        );
    }

    /// `hasLabel("person", "software")` (true positional, 2 args) must match the pre-existing
    /// array-literal form `hasLabel(["person", "software"])` exactly — the systemic defect (D2 of
    /// the true-variadic-overloads spec) confirmed live via `graphersal`.
    #[test]
    fn test_script_has_label_true_positional_matches_array() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let positional = eval_value(
            graph.clone(),
            r#"g.v().hasLabel("person", "software").count().next()"#,
        )
        .expect("positional hasLabel failed");
        let array = eval_value(
            graph,
            r#"g.v().hasLabel(["person", "software"]).count().next()"#,
        )
        .expect("array hasLabel failed");
        assert_eq!(positional.as_int(), array.as_int());
        assert_eq!(positional.as_int().unwrap(), 6);
    }

    /// The fixed-arity cap is real: a step must work at the maximum implemented arity (7), not
    /// just at arity 2.
    #[test]
    fn test_script_has_label_true_positional_max_arity() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script =
            r#"g.v().hasLabel("person", "software", "x", "y", "z", "w", "q").count().next()"#;
        let result = eval_value(graph, script).expect("7-arg positional hasLabel failed");
        assert_eq!(
            result.as_int().unwrap(),
            6,
            "only person/software exist among the 7 labels"
        );
    }

    /// `g.V(1, 2, 3)`/`g.E(...)` positional multi-arg (real TinkerPop `V(Object...)`/
    /// `E(Object...)`), matching the array form.
    #[test]
    fn test_script_v_e_true_positional() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let positional = eval_value(graph.clone(), r#"g.V(1, 2, 3).count().next()"#)
            .expect("V positional failed");
        let array = eval_value(graph, r#"g.V([1, 2, 3]).count().next()"#).expect("V array failed");
        assert_eq!(positional.as_int(), array.as_int());
        assert_eq!(positional.as_int().unwrap(), 3);
    }

    /// `select("a", "b")` positional — the single most common real-world variadic `select()`
    /// shape.
    #[test]
    fn test_script_select_true_positional() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script = r#"
            g.v().has_label("person").as("a").out("knows").as("b").select("a", "b").count().next()
        "#;
        let result = eval_value(graph, script).expect("select positional failed");
        assert_eq!(result.as_int().unwrap(), 2);
    }

    /// `and(__.has(...), __.has(...))` positional (2 nested traversals, no array wrapper).
    /// `and`/`or`/`coalesce`/`union` already forwarded a `Vec<Dynamic>` under one array-only
    /// registration; this confirms true positional calls (no array literal) now also resolve.
    #[test]
    fn test_script_and_or_true_positional() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let and_script = r#"
            g.v().and(__.hasLabel("person"), __.has("age", P.gt(30))).values("name").to_list()
        "#;
        let and_result = eval(graph.clone(), and_script).expect("and positional failed");
        assert!(and_result.contains("josh") && and_result.contains("peter"));

        let or_script = r#"
            g.v().or(__.hasLabel("software"), __.has("age", P.lt(28))).count().next()
        "#;
        let or_result = eval_value(graph, or_script).expect("or positional failed");
        assert_eq!(or_result.as_int().unwrap(), 3);
    }

    /// `has_value_multi`/`hasValue`/`has_value_any` true positional (D2, extends the array-only
    /// form `has_value_multi` got in `8ed46a9`): `hasValue(v1, v2)` must match `hasValue([v1, v2])`.
    #[test]
    fn test_script_has_value_true_positional() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let positional = eval(
            graph.clone(),
            r#"g.v().has_label("person").values("age").hasValue(29, 27).to_list()"#,
        )
        .expect("hasValue positional failed");
        let array = eval(
            graph,
            r#"g.v().has_label("person").values("age").hasValue([29, 27]).to_list()"#,
        )
        .expect("hasValue array failed");
        assert_eq!(positional, array);
    }

    // ---------------------------------------------------------------------------------------
    // Coercion on write and strict logical validation (type system step 3): Rhai parity with
    // `tests/all/type_coercion_tests.rs`.
    // ---------------------------------------------------------------------------------------

    const CANONICAL_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    /// A schema whose vertex labels declare the given nullable `(key, type)` fields.
    fn coercion_schema(mode: SchemaMode, labels: &[(&str, &[(&str, ValueType)])]) -> GraphSchema {
        let node = |ty: &ValueType| match ty {
            ValueType::Uuid => PropertySchema::uuid(),
            ValueType::String => PropertySchema::string(),
            ValueType::Float64 => PropertySchema::number(),
            ValueType::Int64 => PropertySchema::integer(),
            other => panic!("no node for {other}"),
        };
        let mut schema = GraphSchema::new().with_mode(mode);
        for (label, fields) in labels {
            let mut root = PropertySchema::object();
            for (key, ty) in fields.iter() {
                root = root.with_property(*key, node(ty).nullable());
            }
            schema = schema.with_vertex(*label, root);
        }
        schema
    }

    /// The modern graph with `person` declaring `uid: uuid`, `name: string`, `w: float64`.
    fn modern_with_coercion_schema(mode: SchemaMode) -> Arc<Graph> {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .set_schema(coercion_schema(
                mode,
                &[(
                    "person",
                    &[
                        ("uid", ValueType::Uuid),
                        ("name", ValueType::String),
                        ("w", ValueType::Float64),
                    ],
                )],
            ))
            .unwrap();
        Arc::new(graph)
    }

    fn script_count(graph: &Arc<Graph>, script: &str) -> i64 {
        eval_value(graph.clone(), script)
            .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
            .as_int()
            .unwrap_or_else(|err| panic!("script `{script}` did not return a count: {err}"))
    }

    fn script_error(graph: &Arc<Graph>, script: &str) -> String {
        eval_value(graph.clone(), script)
            .map(|value| panic!("script `{script}` must fail, returned {value:?}"))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn test_script_open_schema_coerces_a_canonical_string_to_uuid() {
        let graph = modern_with_coercion_schema(SchemaMode::Open);
        let script = format!(
            r#"g.addV("person").property("uid", "{CANONICAL_UUID}").next();
               g.V().values("uid").is(P.typeOf(GType.UUID)).count().next()"#
        );
        assert_eq!(script_count(&graph, &script), 1);
    }

    #[test]
    fn test_script_open_schema_rejects_a_non_canonical_uuid_string() {
        let graph = modern_with_coercion_schema(SchemaMode::Open);
        let err = script_error(
            &graph,
            r#"g.add_v("person").property("uid", "not-a-uuid").next()"#,
        );
        assert!(
            err.contains(
                "Cannot store \"not-a-uuid\" in vertex 'person.uid' declared as uuid: not a \
                 canonical UUID (8-4-4-4-12 lowercase hex)"
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_script_open_schema_rejects_an_integer_for_uuid() {
        let graph = modern_with_coercion_schema(SchemaMode::Open);
        let err = script_error(&graph, r#"g.addV("person").property("uid", 42).next()"#);
        assert!(
            err.contains("Type mismatch on vertex 'person.uid': expected uuid, got int64"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_script_string_field_rejects_a_uuid() {
        let graph = modern_with_coercion_schema(SchemaMode::Open);
        let script = format!(
            r#"g.addV("person").property("name", __.constant("{CANONICAL_UUID}").cast(GType.UUID)).next()"#
        );
        let err = script_error(&graph, &script);
        assert!(
            err.contains("Type mismatch on vertex 'person.name': expected string, got uuid"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_script_float64_field_coerces_exact_integers_only() {
        let graph = modern_with_coercion_schema(SchemaMode::Open);
        assert_eq!(
            script_count(
                &graph,
                r#"g.add_v("person").property("w", 3).next();
                   g.v().values("w").is(P.typeOf(GType.DOUBLE)).count().next()"#
            ),
            1
        );
        let err = script_error(
            &graph,
            r#"g.addV("person").property("w", 9007199254740993).next()"#,
        );
        assert!(
            err.contains("integer cannot be represented exactly as float64"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_script_closed_mode_checks_every_declaring_label() {
        let graph = GraphSource::empty();
        graph
            .write()
            .set_schema(coercion_schema(
                SchemaMode::Closed,
                &[
                    ("A", &[("k", ValueType::Uuid)]),
                    ("B", &[("k", ValueType::String)]),
                ],
            ))
            .unwrap();
        let graph = Arc::new(graph);
        let script = format!(r#"g.addV(["A", "B"]).property("k", "{CANONICAL_UUID}").next()"#);
        let err = script_error(&graph, &script);
        assert!(
            err.contains("Type mismatch on vertex 'B.k': expected string, got uuid"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_script_set_property_coerces_by_the_first_declaring_label() {
        let graph = GraphSource::empty();
        graph
            .write()
            .set_schema(coercion_schema(
                SchemaMode::Open,
                &[
                    ("X", &[("other", ValueType::String)]),
                    ("A", &[("k", ValueType::Uuid)]),
                ],
            ))
            .unwrap();
        let graph = Arc::new(graph);
        let script = format!(
            r#"g.addV(["X", "A"]).next();
               g.V().property("k", "{CANONICAL_UUID}").next();
               g.V().values("k").is(P.typeOf(GType.UUID)).count().next()"#
        );
        assert_eq!(script_count(&graph, &script), 1);
    }

    #[test]
    fn test_script_schema_mode_none_never_coerces() {
        let graph = modern_with_coercion_schema(SchemaMode::None);
        let script = format!(
            r#"g.addV("person").property("uid", "{CANONICAL_UUID}").next();
               g.V().values("uid").is(P.typeOf(GType.STRING)).count().next()"#
        );
        assert_eq!(script_count(&graph, &script), 1);
    }

    #[test]
    fn test_script_reads_never_coerce() {
        let graph = modern_with_coercion_schema(SchemaMode::Open);
        let script = format!(
            r#"g.addV("person").property("uid", "{CANONICAL_UUID}").next();
               g.V().has("uid", "{CANONICAL_UUID}").count().next()"#
        );
        assert_eq!(script_count(&graph, &script), 0);
    }

    #[test]
    fn test_script_vertex_and_edge_values_are_rejected_in_every_mode() {
        for graph in [
            Arc::new(GraphSource::tinkerpop_modern()),
            modern_with_coercion_schema(SchemaMode::Open),
        ] {
            let err = script_error(
                &graph,
                r#"g.V("1").property("friend", __.out("knows")).next()"#,
            );
            assert!(
                err.contains(
                    "Property 'friend' cannot hold a vertex: vertices and edges are not property \
                     values"
                ),
                "unexpected error: {err}"
            );
            let err = script_error(
                &graph,
                r#"g.V("1").property("e", __.outE("knows").fold()).next()"#,
            );
            assert!(
                err.contains("Property 'e' cannot hold an edge"),
                "unexpected error: {err}"
            );
            assert_eq!(
                script_count(
                    &graph,
                    r#"g.v("1").properties("friend", "e").count().next()"#
                ),
                0
            );
        }
    }

    #[test]
    fn test_script_schema_reports_uuid_for_uuid_values() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let script = format!(
            r#"g.V("1").property("uid", __.constant("{CANONICAL_UUID}").cast(GType.UUID)).next();
               g.V("2").property("uid_text", "{CANONICAL_UUID}").next();
               g.infer_schema().to_json()"#
        );
        let json = eval_value(graph, &script)
            .expect("schema script failed")
            .into_string()
            .expect("JSON text");
        let schema = GraphSchema::from_json(&json).expect("the format reads back");
        let person = &schema.vertices["person"].schema.properties;
        assert_eq!(person["uid"].type_name(), "uuid");
        assert_eq!(person["uid_text"].type_name(), "string");
    }
}

/// Rhai twins of `uuid_tests.rs` (type system step 4): `Uuid` is a native script value, written
/// with `UUID.fromString(..)` / `UUID.from_string(..)` / `UUID::fromString(..)` /
/// `UUID::from_string(..)`.
#[cfg(feature = "script")]
mod uuid_values {
    use crate::script_host::{eval_value, eval_with_params, rhai::Dynamic};
    use graphersal::prelude::*;
    use graphersal::schema::PropertySchema;
    use std::collections::HashMap;
    use std::sync::Arc;

    const UID_1: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UID_2: &str = "00000000-0000-0000-0000-00000000000a";
    const UID_4: &str = "ffffffff-0000-0000-0000-000000000000";

    /// The same fixture as `uuid_tests::modern_with_uids`: `uid` on vertices 1, 2, 4 and 6, with
    /// 2 and 6 sharing one UUID.
    fn modern_with_uids() -> Arc<Graph> {
        let graph = GraphSource::tinkerpop_modern();
        for (id, uid) in [("1", UID_1), ("2", UID_2), ("4", UID_4), ("6", UID_2)] {
            let uid = ElementProperty::uuid_from_string(uid).expect("canonical UUID literal");
            graph
                .write()
                .traversal_mut()
                .v(id)
                .property("uid", uid)
                .next()
                .expect("write uid");
        }
        Arc::new(graph)
    }

    fn eval(graph: &Arc<Graph>, script: &str) -> Dynamic {
        eval_value(graph.clone(), script)
            .unwrap_or_else(|err| panic!("script failed: {err}\n{script}"))
    }

    fn eval_int(graph: &Arc<Graph>, script: &str) -> i64 {
        eval(graph, script).as_int().expect("an integer result")
    }

    fn eval_strings(graph: &Arc<Graph>, script: &str) -> Vec<String> {
        eval(graph, script)
            .into_typed_array::<String>()
            .expect("an array of strings")
    }

    fn eval_error(graph: &Arc<Graph>, script: &str) -> String {
        eval_value(graph.clone(), script)
            .expect_err("the script must fail")
            .to_string()
    }

    #[test]
    fn a_uuid_literal_finds_the_vertex_in_all_four_notations() {
        let graph = modern_with_uids();
        for literal in [
            "UUID.fromString",
            "UUID.from_string",
            "UUID::fromString",
            "UUID::from_string",
        ] {
            let names = eval_strings(
                &graph,
                &format!(r#"g.v().has("uid", {literal}("{UID_1}")).values("name").to_list()"#),
            );
            assert_eq!(names, vec!["marko".to_string()], "notation {literal}");
        }
        assert_eq!(
            eval_int(
                &graph,
                &format!(r#"g.v().has("uid", P.eq(UUID.fromString("{UID_1}"))).count().next()"#)
            ),
            1
        );
        assert_eq!(
            eval_int(
                &graph,
                &format!(r#"g.v().values("uid").is(UUID.fromString("{UID_2}")).count().next()"#)
            ),
            2
        );
        assert_eq!(
            eval_int(
                &graph,
                &format!(
                    r#"g.v().has("uid", P.within([UUID.fromString("{UID_1}"), UUID.fromString("{UID_4}")])).count().next()"#
                )
            ),
            2
        );
    }

    #[test]
    fn a_plain_string_never_matches_a_stored_uuid() {
        let graph = modern_with_uids();
        assert_eq!(
            eval_int(
                &graph,
                &format!(r#"g.v().has("uid", "{UID_1}").count().next()"#)
            ),
            0
        );
        assert_eq!(
            eval_int(
                &graph,
                &format!(r#"g.v().has("uid", P.eq("{UID_1}")).count().next()"#)
            ),
            0
        );
    }

    #[test]
    fn a_value_read_back_matches_again_and_is_a_uuid() {
        let graph = modern_with_uids();
        let script = r#"
            let x = g.v("1").values("uid").next();
            [type_of(x), g.v().has("uid", x).count().next()]
        "#;
        let result = eval(&graph, script).into_array().expect("an array");
        assert_eq!(result[0].clone().into_string().unwrap(), "Uuid");
        assert_eq!(result[1].as_int().unwrap(), 1);
    }

    #[test]
    fn uuid_values_compare_print_and_convert_like_values() {
        let graph = modern_with_uids();
        let script = format!(
            r#"
            let x = g.v("1").values("uid").next();
            [x.to_string(), `${{x}}`, x.as_string(),
             x == UUID.fromString("{UID_1}"), x != UUID.fromString("{UID_2}"),
             x == "{UID_1}", "{UID_1}" == x, x != "{UID_1}"]
        "#
        );
        let result = eval(&graph, &script).into_array().expect("an array");
        for rendered in &result[0..3] {
            assert_eq!(rendered.clone().into_string().unwrap(), UID_1);
        }
        let flags: Vec<bool> = result[3..].iter().map(|v| v.as_bool().unwrap()).collect();
        assert_eq!(flags, vec![true, true, false, false, true]);
    }

    #[test]
    fn a_non_canonical_literal_is_a_script_error_with_the_canonical_hint() {
        let graph = modern_with_uids();
        for bad in ["550E8400-E29B-41D4-A716-446655440000", "abc"] {
            for literal in ["UUID.fromString", "UUID::from_string"] {
                let message = eval_error(&graph, &format!(r#"{literal}("{bad}")"#));
                assert!(message.contains("Cast exception"), "{message}");
                assert!(
                    message.contains("8-4-4-4-12 lowercase hex")
                        && message.contains(
                            r#"UUID.from_string("550e8400-e29b-41d4-a716-446655440000")"#
                        ),
                    "missing canonical-format hint in: {message}"
                );
            }
        }
    }

    #[test]
    fn to_list_and_nested_maps_carry_uuid_values() {
        let graph = modern_with_uids();
        let listed = eval(&graph, r#"g.v("1").values("uid").to_list()"#)
            .into_array()
            .expect("an array");
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].type_name(),
            std::any::type_name::<crate::script_host::ScriptUuid>()
        );

        let script = r#"
            let xs = g.v("1").values("uid").to_list();
            let vm = g.v("1").value_map("uid").next();
            let em = g.v("1").element_map("uid").next();
            [type_of(xs[0]), xs[0].to_string(), type_of(vm.uid), vm.uid.len().to_string(),
             type_of(vm.uid[0]), type_of(em.uid), em.uid.to_string()]
        "#;
        let result = eval_strings(&graph, script);
        // valueMap() wraps each value in a one-element list (TinkerPop), elementMap() does not.
        assert_eq!(
            result,
            vec!["Uuid", UID_1, "array", "1", "Uuid", "Uuid", UID_1]
        );
    }

    #[test]
    fn order_sorts_uuids_like_their_canonical_strings() {
        let graph = modern_with_uids();
        let ordered = eval_strings(
            &graph,
            r#"g.v().values("uid").order().to_list().map(|x| x.to_string())"#,
        );
        let mut expected = vec![UID_1, UID_2, UID_4, UID_2];
        expected.sort_unstable();
        assert_eq!(ordered, expected);
    }

    #[test]
    fn dedup_and_group_count_key_by_uuid() {
        let graph = modern_with_uids();
        assert_eq!(
            eval_int(&graph, r#"g.v().values("uid").dedup().count().next()"#),
            3
        );
        let groups = eval(&graph, r#"g.v().values("uid").group_count().next()"#)
            .try_cast::<crate::script_host::ScriptMap>()
            .expect("a map with typed keys");
        assert_eq!(groups.as_value_map().len(), 3);
        assert_eq!(
            eval_int(
                &graph,
                &format!(r#"g.v().values("uid").group_count().next()[UUID.fromString("{UID_2}")]"#)
            ),
            2
        );
        assert_eq!(
            eval_int(
                &graph,
                &format!(r#"g.v().values("uid").group_count().next()[UUID.fromString("{UID_1}")]"#)
            ),
            1
        );
    }

    #[test]
    fn string_steps_on_a_uuid_raise_a_cast_exception() {
        let graph = modern_with_uids();
        for step in [
            "to_lower()",
            "toUpper()",
            "substring(0, 3)",
            "length()",
            "trim()",
            "l_trim()",
            "rTrim()",
            r#"replace("a", "b")"#,
        ] {
            let message = eval_error(
                &graph,
                &format!(r#"g.v("1").values("uid").{step}.to_list()"#),
            );
            assert!(
                message.contains("Cast exception") && message.contains("got type 'uuid'"),
                "{step}: {message}"
            );
        }
    }

    #[test]
    fn text_predicates_on_a_uuid_match_nothing_without_error() {
        let graph = modern_with_uids();
        assert_eq!(
            eval_int(
                &graph,
                r#"g.v().has("uid", TextP.containing("550e")).count().next()"#
            ),
            0
        );
    }

    #[test]
    fn as_string_yields_canonical_strings() {
        let graph = modern_with_uids();
        assert_eq!(
            eval_strings(&graph, r#"g.v("1").values("uid").as_string().to_list()"#),
            vec![UID_1.to_string()]
        );
    }

    #[test]
    fn uuid_is_a_reserved_parameter_name() {
        let graph = modern_with_uids();
        let err = eval_with_params(
            graph,
            r#"g.v().count().next()"#,
            HashMap::from([("UUID".to_string(), ElementProperty::Boolean(true))]),
        )
        .expect_err("a parameter shadowing `UUID` must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("Invalid script parameter 'UUID'")
                && message.contains("reserved by the script scope"),
            "unexpected diagnostic: {message}"
        );
    }

    #[test]
    fn a_uuid_parameter_arrives_as_a_uuid() {
        let graph = modern_with_uids();
        let uid = ElementProperty::uuid_from_string(UID_1).unwrap();
        let rendered = eval_with_params(
            graph,
            r#"[type_of(uid), g.v().has("uid", uid).count().next()]"#,
            HashMap::from([("uid".to_string(), uid)]),
        )
        .expect("script with a uuid parameter");
        assert!(rendered.contains("\"Uuid\""), "{rendered}");
        assert!(rendered.contains('1'), "{rendered}");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn property_json_stores_a_string_without_a_schema_and_a_uuid_when_declared() {
        let script = format!(
            r#"g.v("1").property_json(#{{uid: UUID.fromString("{UID_1}")}}).to_list();
               type_of(g.v("1").values("uid").next())"#
        );

        let graph = Arc::new(GraphSource::tinkerpop_modern());
        assert_eq!(eval(&graph, &script).into_string().unwrap(), "string");

        let schema = GraphSchema::new().with_mode(SchemaMode::Open).with_vertex(
            "person",
            PropertySchema::object().with_property("uid", PropertySchema::uuid().nullable()),
        );
        let graph = GraphSource::tinkerpop_modern();
        graph.write().set_schema(schema).unwrap();
        let graph = Arc::new(graph);
        assert_eq!(eval(&graph, &script).into_string().unwrap(), "Uuid");
    }
}

/// Rhai twins of `tests/all/column_select_tests.rs`: `unfold()` of a map and `select(Column)` in
/// every DSL spelling (`Column.keys`, `Column::keys`, the bare `keys` / `values` tokens).
#[cfg(feature = "script")]
mod column_tokens {
    use crate::script_host::{eval, eval_value, eval_with_params, rhai};
    use graphersal::prelude::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// The modern graph's vertex names in `group()` (insertion) order.
    const NAMES: [&str; 6] = ["marko", "vadas", "lop", "josh", "ripple", "peter"];

    fn eval_list(script: &str) -> rhai::Array {
        eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
            .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"))
            .into_array()
            .unwrap_or_else(|t| panic!("script `{script}` did not yield a list but {t}"))
    }

    fn eval_strings(script: &str) -> Vec<String> {
        eval_list(script)
            .into_iter()
            .map(|v| {
                v.into_string()
                    .unwrap_or_else(|t| panic!("`{script}`: not a string but {t}"))
            })
            .collect()
    }

    /// The two queries from the Gremlin console idiom, verbatim and without a terminal: both
    /// render every name, one per traverser.
    #[test]
    fn group_keys_queries_render_every_name() {
        for script in [
            r#"g.V().group().by("name").by(__.out().count()).unfold().select(Column.keys)"#,
            r#"g.V().group().by("name").by(__.out().count()).select(Column.keys).unfold()"#,
        ] {
            let rendered = eval(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|err| panic!("script `{script}` failed: {err}"));
            for name in NAMES {
                assert!(
                    rendered.contains(name),
                    "`{script}` misses {name}: {rendered}"
                );
            }
            assert!(!rendered.contains("Error"), "`{script}` failed: {rendered}");
        }
    }

    #[test]
    fn keys_in_every_spelling() {
        for script in [
            r#"g.V().group().by("name").by(__.out().count()).unfold().select(Column.keys).toList()"#,
            r#"g.V().group().by("name").by(__.out().count()).select(Column.keys).unfold().toList()"#,
            r#"g.V().group().by("name").by(__.out().count()).unfold().select(Column::keys).to_list()"#,
            r#"g.V().group().by("name").by(__.out().count()).unfold().select(keys).toList()"#,
            r#"g.V().group().by("name").by(__.out().count()).select(keys).unfold().toList()"#,
            r#"g.v().group_count().by("name").unfold().select(keys).to_list()"#,
        ] {
            assert_eq!(
                eval_strings(script),
                NAMES,
                "unexpected result for `{script}`"
            );
        }
    }

    #[test]
    fn values_in_every_spelling() {
        for script in [
            r#"g.V().group().by("name").by(__.out().count()).unfold().select(values).toList()"#,
            r#"g.V().group().by("name").by(__.out().count()).unfold().select(Column.values).toList()"#,
            r#"g.V().group().by("name").by(__.out().count()).select(Column::values).unfold().toList()"#,
        ] {
            let counts: Vec<i64> = eval_list(script)
                .into_iter()
                .map(|v| {
                    v.as_int()
                        .unwrap_or_else(|t| panic!("`{script}`: not an int but {t}"))
                })
                .collect();
            assert_eq!(
                counts,
                vec![3, 0, 0, 2, 0, 1],
                "unexpected result for `{script}`"
            );
        }
    }

    /// `select(Column)` on a whole map yields ONE list; on an entry, the scalar.
    #[test]
    fn map_yields_a_list_entry_yields_a_scalar() {
        let whole = eval_list(r#"g.V("1").groupCount().by("name").select(keys).toList()"#);
        assert_eq!(whole.len(), 1);
        let inner = whole[0].clone().into_array().expect("a list of keys");
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].clone().into_string().unwrap(), "marko");

        let entry = eval_list(r#"g.V("1").groupCount().by("name").unfold().select(keys).toList()"#);
        assert_eq!(entry.len(), 1);
        assert_eq!(entry[0].clone().into_string().unwrap(), "marko");
    }

    /// An entry left in the stream reaches the script as a single-key map.
    #[test]
    fn unfolded_entries_are_single_key_maps() {
        let entries = eval_list(r#"g.V().groupCount().by("name").unfold().toList()"#);
        assert_eq!(entries.len(), 6);
        let first = entries[0]
            .clone()
            .try_cast::<rhai::Map>()
            .expect("an entry is a map");
        assert_eq!(first.len(), 1);
        assert_eq!(first.get("marko").and_then(|v| v.as_int().ok()), Some(1));
    }

    /// The bare `values` token does not shadow the `values(key)` step: Rhai keeps variables and
    /// methods in separate namespaces.
    #[test]
    fn values_step_still_works_next_to_the_values_token() {
        let ages: Vec<i64> = eval_list(r#"g.V().hasLabel("person").values("age").toList()"#)
            .into_iter()
            .map(|v| v.as_int().unwrap())
            .collect();
        assert_eq!(ages, vec![29, 27, 32, 35]);

        let printed = eval_value(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"[Column.keys.to_string(), values.to_string(), type_of(keys)]"#,
        )
        .unwrap()
        .into_array()
        .unwrap();
        let printed: Vec<String> = printed
            .into_iter()
            .map(|v| v.into_string().unwrap())
            .collect();
        assert_eq!(printed, vec!["Column.keys", "Column.values", "Column"]);
    }

    #[test]
    fn select_column_on_a_vertex_fails_with_guidance() {
        let rendered = eval(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().select(Column.keys)"#,
        )
        .unwrap();
        assert!(
            rendered.contains("expected type [map or entry], got type 'vertex'"),
            "{rendered}"
        );
        assert!(rendered.contains("unfold()"), "{rendered}");

        let err = eval_value(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().values("name").select(keys).toList()"#,
        )
        .expect_err("a string is not a map");
        assert!(err.to_string().contains("got type 'string'"), "{err}");
    }

    /// Every `Scope` token spelling evaluates to the `Scope` type and prints as `local`/`global`;
    /// there are no bare `local`/`global` constants.
    #[test]
    fn scope_tokens_in_every_notation() {
        let printed = eval_value(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"[Scope.local.to_string(), Scope.Local.to_string(), Scope::local.to_string(),
                Scope::Global.to_string(), Scope.global.to_string(), Scope::global.to_string(),
                type_of(Scope.local), type_of(Scope::Global)]"#,
        )
        .unwrap()
        .into_array()
        .unwrap();
        let printed: Vec<String> = printed
            .into_iter()
            .map(|v| v.into_string().unwrap())
            .collect();
        assert_eq!(
            printed,
            vec![
                "Scope.local",
                "Scope.local",
                "Scope.local",
                "Scope.global",
                "Scope.global",
                "Scope.global",
                "Scope",
                "Scope"
            ]
        );
        // No bare `local` token: `local` is the `local(traversal)` step.
        assert!(
            eval_value(
                Arc::new(GraphSource::tinkerpop_modern()),
                "g.V().fold().count(local)"
            )
            .is_err()
        );
    }

    /// `count/sum/min/max/mean(Scope)` in both token notations, on the traversal source and on
    /// an anonymous traversal.
    #[test]
    fn scoped_reducers_in_the_dsl() {
        for (script, expected) in [
            (r#"g.V().fold().count(Scope.local).next()"#, 6_i64),
            (r#"g.V().fold().count(Scope::local).next()"#, 6),
            (r#"g.V().values("age").fold().sum(Scope.local).next()"#, 123),
            (r#"g.V().values("age").fold().min(Scope::Local).next()"#, 27),
            (r#"g.V().values("age").fold().max(Scope.Local).next()"#, 35),
            (r#"g.V().values("age").count(Scope.global).next()"#, 4),
            (
                r#"g.V("1").local(__.out().fold().count(Scope.local)).next()"#,
                3,
            ),
            (
                r#"g.V().where(__.out().fold().count(Scope::local).is(P.gt(2))).count().next()"#,
                1,
            ),
        ] {
            let value = eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|err| panic!("{script}: {err}"));
            assert_eq!(value.as_int().ok(), Some(expected), "{script}");
        }
        let mean = eval_value(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().values("age").fold().mean(Scope.local).next()"#,
        )
        .unwrap();
        assert_eq!(mean.as_float().ok(), Some(30.75));
    }

    /// `limit/skip/tail/range(Scope, …)`: both `tail(Scope)` arities (`tail(Scope.local)` is
    /// `tail(Scope.local, 1)`), both token notations, on the source and in an anonymous traversal.
    #[test]
    fn scoped_range_family_in_the_dsl() {
        let render = |script: &str| {
            let value = eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|err| panic!("{script}: {err}"));
            format!("{value:?}")
        };
        for (script, expected) in [
            (
                r#"g.V().values("name").fold().tail(Scope.local).next()"#,
                r#"["peter"]"#,
            ),
            (
                r#"g.V().values("name").fold().tail(Scope::local, 1).next()"#,
                r#"["peter"]"#,
            ),
            (
                r#"g.V().values("name").fold().tail(Scope.local, 2).next()"#,
                r#"["ripple", "peter"]"#,
            ),
            (
                r#"g.V().values("name").fold().limit(Scope::Local, 2).next()"#,
                r#"["marko", "vadas"]"#,
            ),
            (
                r#"g.V().values("name").fold().skip(Scope.Local, 5).next()"#,
                r#"["peter"]"#,
            ),
            (
                r#"g.V().values("name").fold().range(Scope.local, 1, 2).next()"#,
                r#"["vadas"]"#,
            ),
            (
                r#"g.V().values("name").order().tail(Scope.global).next()"#,
                r#""vadas""#,
            ),
            (
                r#"g.V("1").local(__.out().values("name").order().fold().limit(Scope.local, 1)).next()"#,
                r#"["josh"]"#,
            ),
        ] {
            assert_eq!(render(script), expected, "{script}");
        }
    }

    /// `order(Scope)`, `dedup(Scope)`, `dedup(Scope, label, ...)` and `by(Column)` (dot, static
    /// module and bare-constant notations), on the source and in an anonymous traversal.
    #[test]
    fn scoped_order_dedup_and_by_column_in_the_dsl() {
        let render = |script: &str| {
            let value = eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|err| panic!("{script}: {err}"));
            format!("{value:?}")
        };
        for (script, expected) in [
            (
                r#"g.V().values("age").fold().order(Scope.local).next()"#,
                "[27, 29, 32, 35]",
            ),
            (
                r#"g.V().values("age").fold().order(Scope::Local).by(Order.desc).next()"#,
                "[35, 32, 29, 27]",
            ),
            (r#"g.V().values("age").order(Scope.global).next()"#, "27"),
            (
                r#"g.V().fold().order(Scope.local).by("age", Order.desc).unfold().values("name").fold().next()"#,
                r#"["peter", "josh", "marko", "vadas"]"#,
            ),
            (
                r#"g.V().out().in().values("name").fold().dedup(Scope::local).next()"#,
                r#"["marko", "peter", "josh"]"#,
            ),
            (
                r#"g.V().out().in().values("name").fold().dedup(Scope.local, "a", "b").count(Scope.local).next()"#,
                "3",
            ),
            (r#"g.V().out().dedup(Scope.global).count().next()"#, "4"),
            (
                r#"g.V("1").local(__.out().values("name").fold().order(Scope.local).by(Order.desc)).next()"#,
                r#"["vadas", "lop", "josh"]"#,
            ),
            (
                r#"g.V().hasLabel("person").group().by("name").by("age").unfold().order().by(Column.values, Order.desc).limit(1).select(Column.keys).next()"#,
                r#""peter""#,
            ),
            (
                r#"g.V().hasLabel("person").group().by("name").by("age").unfold().order().by(Column::keys).limit(1).select(keys).next()"#,
                r#""josh""#,
            ),
            (
                r#"g.V().hasLabel("person").group().by("name").by("age").order(Scope.local).by(values, Order.desc).select(Column.keys).next()"#,
                r#"["peter", "josh", "marko", "vadas"]"#,
            ),
        ] {
            assert_eq!(render(script), expected, "{script}");
        }
        // The label form `dedup("a", "b")` keys by the labelled path objects; a first argument
        // that is neither a `Scope` nor a label is still a missing overload.
        assert_eq!(
            render(r#"g.v().as("a").both().as("b").dedup("a", "b").by(T.label).count().next()"#),
            "3"
        );
        let err = eval_value(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().dedup(1, "b").toList()"#,
        )
        .expect_err("dedup(1, label) does not exist");
        assert!(err.to_string().contains("dedup"), "{err}");
    }

    /// The string steps with a `Scope`: camelCase and snake_case spellings, both token
    /// notations, the single-argument `substring(start)`, on the source and in an anonymous
    /// traversal.
    #[test]
    fn scoped_string_steps_in_the_dsl() {
        let render = |script: &str| {
            let value = eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|err| panic!("{script}: {err}"));
            format!("{value:?}")
        };
        let names = r#"g.V().hasLabel("software").values("name").order().fold()"#;
        for (camel, snake, expected) in [
            (
                "toUpper(Scope.local)",
                "to_upper(Scope::local)",
                r#"["LOP", "RIPPLE"]"#,
            ),
            (
                "toLower(Scope.Local)",
                "to_lower(Scope::Local)",
                r#"["lop", "ripple"]"#,
            ),
            (
                "lTrim(Scope.local)",
                "l_trim(Scope::local)",
                r#"["lop", "ripple"]"#,
            ),
            (
                "rTrim(Scope.local)",
                "r_trim(Scope::local)",
                r#"["lop", "ripple"]"#,
            ),
            (
                "trim(Scope.local)",
                "trim(Scope::local)",
                r#"["lop", "ripple"]"#,
            ),
            ("length(Scope.local)", "length(Scope::local)", "[3, 6]"),
            (
                "asString(Scope.local)",
                "as_string(Scope::local)",
                r#"["lop", "ripple"]"#,
            ),
            (
                r#"replace(Scope.local, "p", "g")"#,
                r#"replace(Scope::local, "p", "g")"#,
                r#"["log", "riggle"]"#,
            ),
            (
                "substring(Scope.local, 2)",
                "substring(Scope::local, 2)",
                r#"["p", "pple"]"#,
            ),
            (
                "substring(Scope.local, 1, 4)",
                "substring(Scope::local, 1, 4)",
                r#"["op", "ipp"]"#,
            ),
        ] {
            for step in [camel, snake] {
                let script = format!("{names}.{step}.next()");
                assert_eq!(render(&script), expected, "{script}");
            }
        }
        for (script, expected) in [
            (
                r#"g.V().hasLabel("software").values("name").substring(2).toList()"#,
                r#"["p", "pple"]"#,
            ),
            (
                r#"g.V().hasLabel("software").values("name").substring(-3).toList()"#,
                r#"["lop", "ple"]"#,
            ),
            (
                r#"g.V().hasLabel("software").values("name").toUpper(Scope.global).toList()"#,
                r#"["LOP", "RIPPLE"]"#,
            ),
            (
                r#"g.V("1").local(__.out().values("name").order().fold().toUpper(Scope.local)).next()"#,
                r#"["JOSH", "LOP", "VADAS"]"#,
            ),
            (
                r#"g.V("1").local(__.out().values("name").order().fold().substring(Scope::local, 0, 1)).next()"#,
                r#"["j", "l", "v"]"#,
            ),
            (r#"g.inject("").substring(-1, 2).next()"#, r#""""#),
        ] {
            assert_eq!(render(script), expected, "{script}");
        }
    }

    #[test]
    fn scope_is_a_reserved_parameter_name() {
        let err = eval_with_params(
            Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.v().count().next()"#,
            HashMap::from([("Scope".to_string(), ElementProperty::Boolean(true))]),
        )
        .expect_err("a parameter shadowing the Scope token must be rejected");
        assert!(
            err.to_string().contains("reserved by the script scope"),
            "{err}"
        );
    }

    #[test]
    fn column_tokens_are_reserved_parameter_names() {
        for name in ["Column", "keys", "values"] {
            let err = eval_with_params(
                Arc::new(GraphSource::tinkerpop_modern()),
                r#"g.v().count().next()"#,
                HashMap::from([(name.to_string(), ElementProperty::Boolean(true))]),
            )
            .expect_err("a parameter shadowing a Column token must be rejected");
            assert!(
                err.to_string().contains("reserved by the script scope"),
                "unexpected diagnostic for {name}: {err}"
            );
        }
    }
}

/// Display limits in the DSL: the session default comes from the scope options, a per-call
/// options map overrides it, and data terminals are never bounded.
#[cfg(all(feature = "script", feature = "display", feature = "serde"))]
mod bounded_rendering {
    use crate::script_host::rhai::Dynamic;
    use crate::script_host::{
        engine, graph_scope, graph_scope_with_options, render_scope_options, try_render_value,
    };
    use graphersal::exec::RenderOptions;
    use graphersal::prelude::*;
    use std::sync::Arc;

    fn table_rows(table: &str) -> usize {
        table
            .lines()
            .filter(|line| line.starts_with('│'))
            .count()
            .saturating_sub(1)
    }

    /// Evaluates `script` in a scope whose session limit is `max_rows` rows (none: unbounded).
    fn eval_in_session(max_rows: Option<usize>, script: &str) -> Result<Dynamic, String> {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let mut scope = match max_rows {
            Some(n) => graph_scope_with_options(
                graph,
                &render_scope_options(&RenderOptions::new().with_max_rows(n)),
            ),
            None => graph_scope(graph),
        };
        engine()
            .eval_with_scope::<Dynamic>(&mut scope, script)
            .map_err(|err| err.to_string())
    }

    fn text(value: Dynamic) -> String {
        try_render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited())
            .expect("a renderable value")
            .text
    }

    #[test]
    fn a_session_default_bounds_explicit_visualizers() {
        let table = text(eval_in_session(Some(2), "g.v().to_table()").unwrap());
        assert_eq!(table_rows(&table), 2, "{table}");
        let json = text(eval_in_session(Some(2), "g.v().to_json()").unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        let md = text(eval_in_session(Some(2), "g.v().visualize(V.Markdown)").unwrap());
        assert_eq!(md.lines().count(), 4, "{md}");
    }

    #[test]
    fn without_session_options_explicit_visualizers_are_unbounded() {
        let table = text(eval_in_session(None, "g.v().to_table()").unwrap());
        assert_eq!(table_rows(&table), 6, "{table}");
    }

    #[test]
    fn a_per_call_map_overrides_the_session_default() {
        let table = text(eval_in_session(Some(2), "g.v().to_table(#{max_rows: 3})").unwrap());
        assert_eq!(table_rows(&table), 3, "{table}");
        let table = text(eval_in_session(Some(2), "g.v().toTable(#{no_limit: true})").unwrap());
        assert_eq!(table_rows(&table), 6, "{table}");
        let table = text(eval_in_session(Some(2), "g.v().to_table(#{no_limit: false})").unwrap());
        assert_eq!(table_rows(&table), 2, "{table}");
        let json = text(eval_in_session(Some(2), "g.v().toJson(#{noLimit: true})").unwrap());
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 6);
        let tree =
            text(eval_in_session(None, r#"g.v().values("name").to_tree(#{max_rows: 1})"#).unwrap());
        assert_eq!(tree, "marko");
        // `g.with("render.max_rows", n)` sets the default for one query.
        let table =
            text(eval_in_session(None, r#"g.with("render.max_rows", 1).v().to_table()"#).unwrap());
        assert_eq!(table_rows(&table), 1, "{table}");
    }

    #[test]
    fn zero_limits_and_contradictory_options_fail_with_help() {
        let err = eval_in_session(Some(2), "g.v().to_table(#{max_rows: 0})").unwrap_err();
        assert!(err.contains("Invalid value for option 'max_rows'"), "{err}");
        assert!(err.contains("g.v().to_table(#{no_limit: true})"), "{err}");
        let err = eval_in_session(Some(2), "g.v().to_json(#{max_items: 0})").unwrap_err();
        assert!(
            err.contains("Invalid value for option 'max_items'"),
            "{err}"
        );
        let err =
            eval_in_session(Some(2), "g.v().to_table(#{no_limit: true, max_rows: 5})").unwrap_err();
        assert!(err.contains("Invalid value for option 'no_limit'"), "{err}");
        assert!(
            err.contains("cannot be combined with max_rows or max_items"),
            "{err}"
        );
    }

    #[test]
    fn a_cut_explicit_visualizer_string_carries_its_notice() {
        let value = eval_in_session(Some(2), "g.v().to_table()").unwrap();
        let rendered =
            try_render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited()).unwrap();
        assert_eq!(table_rows(&rendered.text), 2);
        assert!(rendered.truncation.expect("cut").more_rows);
        // A string that no cut visualizer returned carries no report.
        let value = eval_in_session(Some(2), r#""plain text""#).unwrap();
        let rendered =
            try_render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited()).unwrap();
        assert_eq!(rendered.truncation, None);
    }

    #[test]
    fn a_final_list_is_cut_but_data_terminals_are_not() {
        let limits = RenderOptions::new().with_max_rows(2).with_max_items(1);
        let value = eval_in_session(None, "g.v().element_map().to_list()").unwrap();
        let rendered = try_render_value(value, VisualizeFormat::Auto, &limits).unwrap();
        assert_eq!(rendered.text.lines().count(), 2, "{}", rendered.text);
        assert!(
            rendered.text.lines().all(|line| line.ends_with(", …}")),
            "{}",
            rendered.text
        );
        let truncation = rendered.truncation.expect("cut");
        assert!(truncation.more_rows && truncation.items_cut);

        let value = eval_in_session(Some(2), "let xs = g.v().to_list(); xs.len()").unwrap();
        assert_eq!(value.as_int().unwrap(), 6);
    }
}

/// `value_map()`'s arguments are property keys (strings), optionally preceded by the boolean token
/// flag; a null key is dropped, and any other argument fails with an argument mismatch instead of
/// being stringified into a key that never matches.
#[cfg(feature = "script")]
mod value_map_arguments {
    use std::sync::Arc;

    use crate::script_host::rhai::EvalAltResult;
    use graphersal::prelude::*;

    /// The runtime error message of a failing `script` on the modern graph.
    fn modern_err(script: &str) -> String {
        match crate::script_host::eval_value(Arc::new(GraphSource::tinkerpop_modern()), script) {
            Ok(value) => panic!("`{script}` unexpectedly succeeded with {value:?}"),
            Err(err) => match *err {
                EvalAltResult::ErrorRuntime(message, ..) => message.to_string(),
                other => other.to_string(),
            },
        }
    }

    fn modern(script: &str) -> String {
        let value =
            crate::script_host::eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
        format!("{value:?}")
    }

    #[test]
    fn a_boolean_is_accepted_only_as_the_first_argument() {
        for (script, step) in [
            (r#"g.v().value_map("name", true).to_list()"#, "value_map"),
            (r#"g.v().valueMap("name", true).to_list()"#, "valueMap"),
            ("g.v().valueMap([\"a\", false]).to_list()", "valueMap"),
            (r#"g.v().valueMap(true, true).to_list()"#, "valueMap"),
        ] {
            let err = modern_err(script);
            assert!(
                err.contains(&format!(
                    "Argument mismatch for step '{step}': expected property keys as strings, \
                     optionally preceded by a boolean, got a boolean"
                )),
                "`{script}`: {err}"
            );
            assert!(
                err.contains("a leading `true` adds id and label"),
                "`{script}`: {err}"
            );
        }
    }

    #[test]
    fn a_leading_boolean_is_the_token_flag() {
        for (script, expected) in [
            (
                r#"g.v("1").value_map(true, "name").to_list()"#,
                r#"[#{"id": "1", "label": "person", "name": ["marko"]}]"#,
            ),
            (
                r#"g.v("1").valueMap(true, "name").to_list()"#,
                r#"[#{"id": "1", "label": "person", "name": ["marko"]}]"#,
            ),
            (
                r#"g.v("1").valueMap(false, "name").to_list()"#,
                r#"[#{"name": ["marko"]}]"#,
            ),
            (
                r#"g.v("1").valueMap(false).to_list()"#,
                r#"[#{"age": [29], "name": ["marko"]}]"#,
            ),
            (
                r#"g.v("1").valueMap(true).to_list()"#,
                r#"[#{"age": [29], "id": "1", "label": "person", "name": ["marko"]}]"#,
            ),
        ] {
            assert_eq!(modern(script), expected, "`{script}`");
        }
    }

    #[test]
    fn a_boolean_is_accepted_in_an_anonymous_traversal() {
        assert_eq!(
            modern(r#"g.v("1").local(__.value_map(true, "name")).to_list()"#),
            r#"[#{"id": "1", "label": "person", "name": ["marko"]}]"#
        );
    }

    #[test]
    fn a_null_key_is_dropped_wherever_it_stands() {
        for script in [
            r#"g.v("1").valueMap("name", "age", ()).to_list()"#,
            r#"g.v("1").valueMap((), "name", "age").to_list()"#,
            r#"g.v("1").value_map(["name", (), "age"]).to_list()"#,
        ] {
            assert_eq!(
                modern(script),
                r#"[#{"age": [29], "name": ["marko"]}]"#,
                "`{script}`"
            );
        }
        // A lone null equals valueMap() (all keys): documented deviation.
        assert_eq!(
            modern(r#"g.v("1").value_map(()).to_list()"#),
            modern(r#"g.v("1").value_map().to_list()"#)
        );
    }

    #[test]
    fn other_non_string_keys_are_rejected() {
        for (script, step, actual) in [
            (
                "g.v().value_map(1).to_list()",
                "value_map",
                "a value of type int64 (`1`)",
            ),
            (
                "g.v().valueMap(1.5).to_list()",
                "valueMap",
                "a value of type float64 (`1.5`)",
            ),
            (
                r#"g.v().value_map(["name", 2]).to_list()"#,
                "value_map",
                "a value of type int64 (`2`)",
            ),
            (
                r#"g.v().value_map(UUID.fromString("123e4567-e89b-12d3-a456-426614174000")).to_list()"#,
                "value_map",
                "a UUID (`123e4567-e89b-12d3-a456-426614174000`)",
            ),
        ] {
            let err = modern_err(script);
            assert!(
                err.contains(&format!(
                    "Argument mismatch for step '{step}': expected property keys as strings, \
                     optionally preceded by a boolean, got {actual}"
                )),
                "`{script}`: {err}"
            );
            assert!(
                err.contains("a leading `true` adds id and label"),
                "`{script}`: {err}"
            );
        }
    }

    #[test]
    fn string_keys_still_work() {
        assert_eq!(
            modern(r#"g.v("1").value_map("name").to_list()"#),
            r#"[#{"name": ["marko"]}]"#
        );
        assert_eq!(
            modern(r#"g.v("1").value_map("name", "age").to_list()"#),
            modern(r#"g.v("1").value_map(["name", "age"]).to_list()"#)
        );
        assert_eq!(
            modern(r#"g.v("1").value_map().to_list()"#),
            r#"[#{"age": [29], "name": ["marko"]}]"#
        );
    }
}

/// `ScriptGraphTraversalSource::to_typed_list()`: the Rust-only typed counterpart of `toList()`.
#[cfg(feature = "script")]
mod typed_list_tests {
    use crate::script_host::{ScriptGraphTraversalSource, engine, graph_scope, rhai};
    use graphersal::prelude::*;
    use std::sync::Arc;

    /// Evaluates `script` (which must not end in a terminal step) on the modern graph and
    /// collects its typed values.
    fn typed(script: &str) -> Vec<ElementProperty> {
        let mut scope = graph_scope(Arc::new(GraphSource::tinkerpop_modern()));
        let value: rhai::Dynamic = engine()
            .eval_with_scope(&mut scope, script)
            .expect("script evaluates");
        assert!(
            value.is::<ScriptGraphTraversalSource>(),
            "not a traversal: {value}"
        );
        value
            .cast::<ScriptGraphTraversalSource>()
            .to_typed_list()
            .expect("traversal executes")
    }

    /// A vertex keeps its element identity instead of becoming an object map.
    #[test]
    fn vertex_comes_back_as_element_property_vertex() {
        let values = typed(r#"g.V("1")"#);
        assert_eq!(values.len(), 1);
        assert!(
            matches!(&values[0], ElementProperty::Vertex(_)),
            "expected a vertex, got {:?}",
            values[0]
        );
    }

    /// An edge keeps its kind as well, and scalars stay scalars.
    #[test]
    fn edge_and_scalar_values_keep_their_kind() {
        let edges = typed(r#"g.V("1").outE("knows")"#);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| matches!(e, ElementProperty::Edge(_))));

        let names = typed(r#"g.V("1").values("name")"#);
        assert_eq!(names, vec![ElementProperty::String("marko".into())]);
    }
}

mod reserved_param_names {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::script_host::eval_with_params;
    use graphersal::prelude::*;

    const TOKEN_GLOBALS: [&str; 26] = [
        "g",
        "__",
        "_g",
        "P",
        "TextP",
        "V",
        "UUID",
        "Set",
        "Column",
        "keys",
        "values",
        "params",
        "Scope",
        "Cardinality",
        "Direction",
        "Merge",
        "Operator",
        "Barrier",
        "Pop",
        "T",
        "By",
        "Order",
        "Pick",
        "GType",
        "CastPolicy",
        "ProfileType",
    ];

    fn run(name: &str) -> Result<String, String> {
        eval_with_params(
            Arc::new(GraphSource::tinkerpop_modern()),
            "g.v().count().next()",
            HashMap::from([(name.to_string(), ElementProperty::Boolean(true))]),
        )
        .map_err(|e| e.to_string())
    }

    #[test]
    fn every_reserved_name_is_rejected() {
        for name in TOKEN_GLOBALS {
            let err = run(name).expect_err(name);
            assert!(
                err.contains("reserved by the script scope"),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn non_reserved_names_still_work() {
        for name in ["label", "first", "all", "Tx", "order"] {
            assert!(run(name).is_ok(), "{name} must stay usable as a parameter");
        }
    }
}
