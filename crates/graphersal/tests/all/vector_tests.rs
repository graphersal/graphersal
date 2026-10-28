//! Vector values (book page "Vectors"): the value type through the Rust API (construction checks,
//! quantization, equality, order, sizes), the DSL (`vector([..])`, `as_vector()`, `quantize()`,
//! `GType.VECTOR`, filters and ordering), the schema (the vector declaration, coercion on write,
//! validation, inference, diff, the JSON Schema export), every format (packed snapshot, Store
//! WAL replay and checkpoint, damaged journal payloads, the serialized `ChangeSet`, GraphSON,
//! GraphML), the property index (vectors are no keys) and `memory_usage()`.

#![cfg_attr(
    not(all(
        feature = "persist",
        feature = "io",
        feature = "serde",
        feature = "script"
    )),
    allow(dead_code, unused_imports)
)]

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::vector::{MAX_VECTOR_DIMENSION, VectorElement, VectorError};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn f32v(values: &[f32]) -> Vector {
    Vector::from_f32(values.to_vec()).unwrap()
}

/// One vector of every element kind, from the same f32 values.
fn every_kind() -> Vec<(&'static str, Vector)> {
    let base = f32v(&[0.5, -1.0, 0.25, 0.0, 2.0, -0.0, 1e-3, 3.5, -7.25]);
    vec![
        ("f32", base.clone()),
        ("f16", base.quantize(VectorElement::F16).unwrap()),
        ("i8", base.quantize(VectorElement::I8).unwrap()),
        ("bit", base.quantize(VectorElement::Bit).unwrap()),
        ("one", f32v(&[42.0])),
    ]
}

/// A graph with one vertex `v` per vector of [`every_kind`] (property `emb`), one edge with a
/// vector property and a nested vector inside an object.
fn vector_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for (name, vector) in every_kind() {
        let mut props = PropertyMap::default();
        props.insert("emb", ElementProperty::Vector(vector.clone()));
        let mut nested = PropertyMap::default();
        nested.insert("inner", ElementProperty::Vector(vector));
        props.insert("meta", ElementProperty::Object(Box::new(nested)));
        graph
            .add_vertex(Some(name.into()), vec!["doc".into()], props)
            .unwrap();
    }
    let a = graph.get_vertex_by_id("f32").unwrap().unwrap();
    let b = graph.get_vertex_by_id("i8").unwrap().unwrap();
    let mut props = PropertyMap::default();
    props.insert("w", ElementProperty::Vector(f32v(&[1.0, 2.0])));
    graph
        .add_edge(Some("e".into()), Some("near".into()), a, b, props)
        .unwrap();
    graph
}

/// Every vector of `graph` with its place, exactly (element kind, dimension, components).
fn vectors_of(graph: &TraversalGraph) -> Vec<String> {
    let mut out = Vec::new();
    for (name, _) in every_kind() {
        let v = graph.get_vertex_by_id(name).unwrap().expect("vertex");
        let props = graph.vertex_properties(v).unwrap().unwrap();
        out.push(format!("{name}.emb = {}", exact(props.get("emb").unwrap())));
        out.push(format!(
            "{name}.meta = {}",
            exact(props.get("meta").unwrap())
        ));
    }
    let e = graph.get_edge_by_id("e").unwrap().unwrap();
    out.push(format!(
        "e.w = {}",
        exact(graph.edge_properties(e).unwrap().unwrap().get("w").unwrap())
    ));
    out
}

/// A value with every vector's raw data spelled out (equality alone would hide `-0.0`).
fn exact(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Vector(v) => {
            let raw = if let Some(values) = v.as_f32() {
                format!(
                    "{:?}",
                    values.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                )
            } else if let Some(bits) = v.as_f16_bits() {
                format!("{bits:?}")
            } else if let Some((codes, scale)) = v.as_i8() {
                format!("{codes:?} * {}", scale.to_bits())
            } else {
                format!("{:?}", v.as_bits().unwrap())
            };
            format!("{}[{}] {raw}", v.element(), v.dimension())
        }
        ElementProperty::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{key}: {}", exact(item)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        other => format!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// The value type
// ---------------------------------------------------------------------------------------------

#[test]
fn sizes_are_unchanged() {
    assert_eq!(std::mem::size_of::<ElementProperty>(), 32);
    assert_eq!(std::mem::size_of::<TraverserValue<TraversalGraph>>(), 64);
}

#[test]
fn construction_refuses_bad_vectors() {
    assert!(matches!(
        Vector::from_f32(vec![]),
        Err(VectorError::Dimension { dimension: 0, .. })
    ));
    assert!(Vector::from_f32(vec![1.0; MAX_VECTOR_DIMENSION]).is_ok());
    assert!(Vector::from_f32(vec![1.0; MAX_VECTOR_DIMENSION + 1]).is_err());
    assert!(Vector::from_f32(vec![0.0, f32::NAN]).is_err());
    assert!(Vector::from_f32(vec![f32::INFINITY]).is_err());
    assert!(Vector::from_f64(&[f64::MAX]).is_err());
    assert!(Vector::from_i8(vec![1], -0.5).is_err());
    assert!(Vector::from_bits(vec![0b0000_0001], 7).is_err());
    let error = Vector::from_f32(vec![f32::NAN]).unwrap_err();
    assert!(error.to_string().contains("NaN"), "{error}");
    assert!(error.help().contains("finite"), "{}", error.help());
}

#[test]
fn quantization_and_dequantization() {
    let v = f32v(&[0.5, -1.0, 0.25]);
    let i8 = v.quantize(VectorElement::I8).unwrap();
    assert_eq!(i8.as_i8().unwrap().0, &[64, -127, 32]);
    assert_eq!(i8.data_bytes(), 3);
    let back = i8.quantize(VectorElement::F32).unwrap();
    assert_eq!(back.element(), VectorElement::F32);
    for (a, b) in back.iter_f32().zip(v.iter_f32()) {
        assert!((a - b).abs() <= 1.0 / 127.0, "{a} vs {b}");
    }
    let f16 = v.quantize(VectorElement::F16).unwrap();
    assert_eq!(f16.to_f32_vec(), vec![0.5, -1.0, 0.25]);
    assert_eq!(f16.data_bytes(), 6);
    let bit = v.quantize(VectorElement::Bit).unwrap();
    assert_eq!(bit.to_f32_vec(), vec![1.0, -1.0, 1.0]);
    assert_eq!(bit.data_bytes(), 1);
    assert!(f32v(&[1e6]).quantize(VectorElement::F16).is_err());
}

#[test]
fn equality_hash_and_order() {
    use std::collections::HashSet;
    let kinds = every_kind();
    let set: HashSet<ElementProperty> = kinds
        .iter()
        .map(|(_, v)| ElementProperty::Vector(v.clone()))
        .collect();
    assert_eq!(set.len(), kinds.len());
    assert_eq!(f32v(&[0.0, 1.0]), f32v(&[-0.0, 1.0]));
    assert_ne!(
        ElementProperty::Vector(f32v(&[1.0, 2.0])),
        ElementProperty::Array(vec![1.0.into(), 2.0.into()])
    );
    // Kind, then dimension, then the components.
    let mut sorted = [
        f32v(&[1.0]).quantize(VectorElement::I8).unwrap(),
        f32v(&[2.0, 0.0]),
        f32v(&[1.0, 5.0]),
        f32v(&[9.0]),
    ];
    sorted.sort();
    assert_eq!(
        sorted.iter().map(Vector::to_string).collect::<Vec<_>>(),
        [
            "vector([9.0])",
            "vector([1.0, 5.0])",
            "vector([2.0, 0.0])",
            "vector([1.0], \"i8\")"
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// The DSL
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
mod dsl {
    use super::*;
    use crate::script_host::{debug_value, eval_value};

    fn run_on(graph: Arc<Graph>, script: &str) -> String {
        match eval_value(graph, script) {
            Ok(value) => debug_value(&value),
            Err(err) => panic!("{script}: {err}"),
        }
    }

    fn run(script: &str) -> String {
        run_on(Arc::new(GraphSource::empty()), script)
    }

    fn fails(script: &str) -> String {
        match eval_value(Arc::new(GraphSource::empty()), script) {
            Ok(value) => panic!("{script} must fail, got {}", debug_value(&value)),
            Err(err) => err.to_string(),
        }
    }

    fn docs() -> Arc<Graph> {
        let graph = Arc::new(GraphSource::empty());
        run_on(
            graph.clone(),
            r#"g.add_v("doc").property("name", "a").property("emb", vector([0.5, 0.5])).iterate();
               g.add_v("doc").property("name", "b").property("emb", vector([0.1, 0.9])).iterate();
               g.add_v("doc").property("name", "c").property("emb", vector([0.9, 0.1], "i8")).iterate();
               g.add_v("doc").property("name", "d").iterate();"#,
        );
        graph
    }

    #[test]
    fn the_constructor_prints_its_literal() {
        assert_eq!(run("vector([0.5, 1])"), "vector([0.5, 1.0])");
        assert_eq!(
            run("vector([0.5, -1.0], \"bit\")"),
            "vector([1.0, -1.0], \"bit\")"
        );
        assert_eq!(run("vector([0.5, -1.0], \"f16\").element"), "\"f16\"");
        assert_eq!(run("vector([1, 2, 3]).dimension"), "3");
        assert_eq!(run("type_of(vector([1]))"), "\"Vector\"");
        assert_eq!(run("vector([0.5, 2]).to_list()"), "[0.5, 2.0]");
        assert_eq!(run("vector([0.5, 2]).toList()"), "[0.5, 2.0]");
        assert_eq!(run("vector([1.0]) == vector([1.0])"), "true");
        assert_eq!(run("vector([1.0]) == [1.0]"), "false");
        assert_eq!(run("vector([1.0, 2.0]).quantize(\"i8\").element"), "\"i8\"");
        // A long vector is cut like a list.
        // Unlimited rendering shows every component; to_string() is cut after 16.
        let long = run("let a = []; for x in 0..400 { a.push(x * 1.0) } vector(a)");
        assert!(long.ends_with("398.0, 399.0])"), "{long}");
        let text = run("let a = []; for x in 0..400 { a.push(x * 1.0) } vector(a).to_string()");
        assert!(text.contains(", …]) (400 items)"), "{text}");
        // Interpolation and to_string use the literal too.
        assert_eq!(run("`${vector([1.5])}`"), "\"vector([1.5])\"");
    }

    #[test]
    fn construction_errors_explain_themselves() {
        assert!(fails("vector([])").contains("1 to 65536 components"));
        assert!(fails("vector([1, \"x\"])").contains("component 1"));
        assert!(fails("vector([1.0], \"f64\")").contains("unknown vector element"));
        assert!(fails("vector([1e300])").contains("finite"));
        assert!(fails("vector([1e10], \"f16\")").contains("beyond the range"));
    }

    #[test]
    fn as_vector_and_quantize_in_both_spellings() {
        assert_eq!(
            run("g.inject([0.5, 1]).as_vector().next()"),
            "vector([0.5, 1.0])"
        );
        assert_eq!(
            run("g.inject([0.5, 1]).as_vector().next()"),
            "vector([0.5, 1.0])"
        );
        assert_eq!(
            run("g.inject(1, 2).fold().as_vector().next()"),
            "vector([1.0, 2.0])"
        );
        assert_eq!(
            run("g.inject(vector([0.5, -1.0])).quantize(\"bit\").next()"),
            "vector([1.0, -1.0], \"bit\")"
        );
        assert_eq!(run("g.inject(()).as_vector().next()"), "()");
        assert_eq!(
            run("g.inject(vector([1.0, 2.0])).quantize(\"f16\").quantize(\"f32\").next()"),
            "vector([1.0, 2.0])"
        );
        let err = fails("g.inject(\"x\").quantize(\"i8\").next()");
        assert!(err.contains("expected a vector"), "{err}");
        assert!(err.contains("as_vector()"), "{err}");
        assert!(fails("g.inject([]).as_vector().next()").contains("components"));
        assert!(fails("g.inject(1).quantize(\"x\")").contains("unknown vector element"));
        // `as_string()` gives the full literal.
        assert_eq!(
            run("g.inject(vector([0.25])).as_string().next()"),
            "\"vector([0.25])\""
        );
    }

    #[test]
    fn the_profile_shows_the_steps() {
        let profile = run("g.inject([1.0]).as_vector().quantize(\"f16\").profile().to_string()");
        assert!(profile.contains("as_vector()"), "{profile}");
        assert!(profile.contains(r#"quantize(\"f16\")"#), "{profile}");
    }

    #[test]
    fn gtype_vector_matches_every_element_kind() {
        let graph = docs();
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().values(\"emb\").is(typeOf(GType.VECTOR)).count().next()"
            ),
            "3"
        );
        assert_eq!(
            run_on(graph.clone(), "GType.VECTOR.to_string()"),
            "\"GType.VECTOR\""
        );
        assert_eq!(
            run_on(
                graph,
                "g.v().values(\"emb\").is(typeOf(\"Vector\")).count().next()"
            ),
            "3"
        );
    }

    #[test]
    fn filters_compare_vectors_by_value() {
        let graph = docs();
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has(\"emb\", vector([0.1, 0.9])).values(\"name\").next()"
            ),
            "\"b\""
        );
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has(\"emb\", P.eq(vector([0.5, 0.5]))).values(\"name\").next()"
            ),
            "\"a\""
        );
        // A list is not the vector; another kind is not the vector either.
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has(\"emb\", [0.1, 0.9]).count().next()"
            ),
            "0"
        );
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has(\"emb\", vector([0.1, 0.9], \"f16\")).count().next()"
            ),
            "0"
        );
        // A vector read by a terminal and fed back matches again.
        assert_eq!(
            run_on(
                graph.clone(),
                "let e = g.v().has(\"name\", \"c\").values(\"emb\").next(); \
                 g.v().has(\"emb\", e).values(\"name\").next()"
            ),
            "\"c\""
        );
        // Ordered by kind, dimension, components; dedup by value.
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has(\"emb\").order().by(\"emb\").values(\"name\").to_list()"
            ),
            "[\"b\", \"a\", \"c\"]"
        );
        assert_eq!(
            run_on(graph, "g.v().values(\"emb\").dedup().count().next()"),
            "3"
        );
    }

    #[test]
    fn a_vector_is_one_value_in_local_steps_and_has_no_min() {
        let graph = docs();
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has(\"name\", \"a\").values(\"emb\").count(Scope.local).next()"
            ),
            "1"
        );
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has(\"name\", \"a\").values(\"emb\").unfold().count().next()"
            ),
            "1"
        );
        let err = match eval_value(graph, "g.v().values(\"emb\").min().next()") {
            Ok(value) => panic!("min of vectors: {}", debug_value(&value)),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("vector"), "{err}");
    }

    #[test]
    fn a_property_index_skips_vectors_and_has_still_finds_them() {
        let graph = docs();
        run_on(
            graph.clone(),
            r#"g.create_index(#{label: "doc", key: "emb"})"#,
        );
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().has_label(\"doc\").has(\"emb\", vector([0.1, 0.9])).values(\"name\").next()"
            ),
            "\"b\""
        );
        assert_eq!(
            run_on(
                graph,
                "g.v().has_label(\"doc\").has(\"emb\", P.within([vector([0.5, 0.5]), vector([0.1, 0.9])])).count().next()"
            ),
            "2"
        );
    }

    #[test]
    fn quantize_an_existing_property_recipe() {
        let graph = docs();
        run_on(
            graph.clone(),
            r#"g.v().has_label("doc").has("emb").property("emb", __.values("emb").quantize("i8")).iterate()"#,
        );
        assert_eq!(
            run_on(graph, "g.v().values(\"emb\").to_list().map(|v| v.element)"),
            "[\"i8\", \"i8\", \"i8\"]"
        );
    }

    #[test]
    fn element_maps_and_json_show_the_values() {
        let graph = docs();
        let json = run_on(
            graph.clone(),
            "g.v().has(\"name\", \"a\").values(\"emb\").to_json()",
        );
        assert!(json.contains("0.5"), "{json}");
        let table = run_on(graph, "g.v().has(\"name\", \"a\").element_map().to_table()");
        assert!(table.contains("vector[2 f32] [0.5, 0.5]"), "{table}");
    }

    #[test]
    fn a_schema_declared_vector_is_coerced_and_validated() {
        let graph = Arc::new(GraphSource::empty());
        run_on(
            graph.clone(),
            r#"g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
                emb: #{type: "array", format: "vector", element: "i8", dimension: 3, metric: "cosine"}}}}}})"#,
        );
        // A list becomes an i8 vector, an f32 vector is quantized.
        run_on(
            graph.clone(),
            r#"g.add_v("doc").property("name", "a").property("emb", [0.5, -1.0, 0.25]).iterate();
               g.add_v("doc").property("name", "b").property("emb", vector([1.0, 2.0, 3.0])).iterate();"#,
        );
        assert_eq!(
            run_on(
                graph.clone(),
                "g.v().values(\"emb\").to_list().map(|v| v.element)"
            ),
            "[\"i8\", \"i8\"]"
        );
        // A dimension mismatch is refused, with the conversion in the help.
        let err = match eval_value(
            graph.clone(),
            r#"g.add_v("doc").property("emb", [1.0, 2.0]).iterate()"#,
        ) {
            Ok(_) => panic!("a 2-dimensional vector was accepted"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("vector<i8, 3>"), "{err}");
        assert!(err.contains("as_vector().quantize(\"i8\")"), "{err}");
        assert!(match eval_value(
            graph.clone(),
            r#"g.add_v("doc").property("emb", ["x", 1, 2]).iterate()"#
        ) {
            Ok(_) => false,
            Err(err) => err.to_string().contains("not a list of finite numbers"),
        });
        // The schema prints its declaration back (a Rhai map sorts its keys).
        let schema = run_on(graph.clone(), "g.get_schema().to_json()");
        assert!(
            schema.contains(
                r#"\"emb\":{\"dimension\":3,\"element\":\"i8\",\"format\":\"vector\",\"metric\":\"cosine\",\"type\":\"array\"}"#
            ),
            "{schema}"
        );
        // Freeze what is there: inference reports the vector.
        let inferred = run_on(graph, "g.infer_schema().to_json()");
        assert!(
            inferred.contains(
                r#"\"emb\":{\"dimension\":3,\"element\":\"i8\",\"format\":\"vector\",\"type\":\"array\"}"#
            ),
            "{inferred}"
        );
    }

    #[test]
    fn a_saved_query_takes_a_vector_argument() {
        let graph = docs();
        run_on(
            graph.clone(),
            r#"g.define_query(#{name: "same", body: `fn same(q) { g.v().has("emb", q).values("name") }`,
                params: #{q: #{schema: #{type: "array", format: "vector", element: "f32", dimension: 2}}}})"#,
        );
        assert_eq!(
            run_on(
                graph.clone(),
                "g.query(\"same\", #{q: vector([0.1, 0.9])}).to_list()"
            ),
            "[\"b\"]"
        );
        // A list is coerced to the declared vector.
        assert_eq!(
            run_on(graph, "g.query(\"same\", #{q: [0.5, 0.5]}).to_list()"),
            "[\"a\"]"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------------

mod schema {
    use super::*;
    use graphersal::schema::{PropertySchema, SchemaChangeKind, ValueType, VectorSchema};

    fn with_property(mode: &str, property: &str) -> Result<GraphSchema, SchemaError> {
        GraphSchema::from_json(&format!(
            r#"{{"mode": "{mode}", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{"emb": {property}}}}}}}}}}}"#
        ))
    }

    #[test]
    fn the_declaration_loads_and_prints_canonically() {
        let schema = with_property(
            "open",
            r#"{"format": "vector", "dimension": 4, "type": "array"}"#,
        )
        .unwrap();
        let node = &schema.vertices["doc"].schema.properties["emb"];
        assert_eq!(
            node.vector,
            Some(VectorSchema::new(VectorElement::F32).with_dimension(4))
        );
        assert_eq!(node.type_name(), "vector<f32, 4>");
        let text = schema.to_json();
        assert!(
            text.contains(
                r#""emb":{"type":"array","format":"vector","element":"f32","dimension":4}"#
            ),
            "{text}"
        );
        assert_eq!(GraphSchema::from_json(&text).unwrap(), schema);
        assert_eq!(
            PropertySchema::vector(VectorElement::Bit, None).type_name(),
            "vector<bit>"
        );
        assert_eq!(
            ValueType::Vector(VectorElement::I8).to_string(),
            "vector<i8>"
        );
    }

    #[test]
    fn misplaced_vector_keywords_are_errors() {
        for (property, needle) in [
            (
                r#"{"type": "array", "dimension": 3}"#,
                "needs \"format\": \"vector\"",
            ),
            (
                r#"{"type": "string", "format": "vector"}"#,
                "format applies to array",
            ),
            (
                r#"{"format": "vector"}"#,
                "format vector needs \"type\": \"array\"",
            ),
            (
                r#"{"type": "array", "format": "vector", "items": {"type": "number"}}"#,
                "items is not allowed next to format vector",
            ),
            (
                r#"{"type": "array", "format": "vector", "element": "f64"}"#,
                "unknown vector element",
            ),
            (
                r#"{"type": "array", "format": "vector", "dimension": 0}"#,
                "dimension must be between 1 and 65536",
            ),
            (
                r#"{"type": "array", "format": "vector", "metric": "manhattan"}"#,
                "unknown vector metric",
            ),
            (r#"{"type": "vector"}"#, "unknown type 'vector'"),
        ] {
            let err = with_property("open", property).unwrap_err();
            let text = format!("{err} {}", err.help().unwrap_or_default());
            assert!(text.contains(needle), "{property}: {text}");
        }
    }

    #[test]
    fn element_and_dimension_changes_are_incompatible() {
        let base = with_property(
            "open",
            r#"{"type": "array", "format": "vector", "element": "f32", "dimension": 3}"#,
        )
        .unwrap();
        let cases = [
            (
                r#"{"type": "array", "format": "vector", "element": "i8", "dimension": 3}"#,
                false,
            ),
            (
                r#"{"type": "array", "format": "vector", "element": "f32", "dimension": 4}"#,
                false,
            ),
            (
                r#"{"type": "array", "format": "vector", "element": "f32"}"#,
                true,
            ),
            (r#"{"type": "array"}"#, false),
            (
                r#"{"type": "array", "format": "vector", "element": "f32", "dimension": 3, "metric": "dot"}"#,
                true,
            ),
        ];
        for (property, compatible) in cases {
            let new = with_property("open", property).unwrap();
            let diff = graphersal::schema::SchemaDiff::between(Some(&base), &new);
            assert_eq!(diff.compatible(), compatible, "{property}: {diff:?}");
            if !compatible {
                assert!(
                    diff.changes
                        .iter()
                        .any(|change| change.kind == SchemaChangeKind::TypeChanged),
                    "{property}: {diff:?}"
                );
            }
        }
    }

    #[test]
    fn existing_lists_violate_a_new_vector_declaration_and_vectors_freeze() {
        let mut graph = TraversalGraph::new();
        let mut props = PropertyMap::default();
        props.insert("emb", ElementProperty::Array(vec![1.0.into(), 2.0.into()]));
        graph
            .add_vertex(Some("l".into()), vec!["doc".into()], props)
            .unwrap();
        let declared = with_property(
            "open",
            r#"{"type": "array", "format": "vector", "element": "f32", "dimension": 2}"#,
        )
        .unwrap();
        let err = graph.set_schema(declared).unwrap_err();
        assert!(err.to_string().contains("vector<f32, 2>"), "{err}");

        let mut graph = vector_graph();
        let inferred = graphersal::schema::SchemaInferer::infer_from_storage(&graph).unwrap();
        let emb = &inferred.vertices["doc"].schema.properties["emb"];
        // Several element kinds at one position: one anyOf member per kind.
        let members = emb.any_of.as_ref().expect("an anyOf");
        assert_eq!(members.len(), 4, "{emb:?}");
        graph
            .set_schema(inferred.with_mode(SchemaMode::Closed))
            .unwrap();
    }

    #[test]
    fn the_json_schema_export_spells_out_the_array() {
        let schema = with_property(
            "open",
            r#"{"type": "array", "format": "vector", "element": "i8", "dimension": 3}"#,
        )
        .unwrap();
        let text = schema.visualize(VisualizeFormat::JsonSchema);
        assert!(text.contains("\"minItems\": 3"), "{text}");
        assert!(text.contains("\"maxItems\": 3"), "{text}");
        assert!(text.contains("\"items\""), "{text}");
        assert!(text.contains("\"format\": \"vector\""), "{text}");
    }
}

// ---------------------------------------------------------------------------------------------
// Binary format
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
mod binary {
    use super::*;
    use graphersal::persist::{
        self, Durability, Journal, JournalRecord, MemDir, PersistError, ReadOptions, SharedBuffer,
        Store, StoreOptions, WriteOptions,
    };

    #[test]
    fn a_packed_snapshot_keeps_every_vector_exactly() {
        let graph = vector_graph();
        let mut bytes = Vec::new();
        persist::write_snapshot(&graph, &mut bytes, &WriteOptions::new()).unwrap();
        let loaded = persist::read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
        assert_eq!(vectors_of(&loaded), vectors_of(&graph));
    }

    #[test]
    fn a_store_keeps_vectors_through_wal_replay_checkpoint_and_backup() {
        let dir = MemDir::new();
        let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
        let source = vector_graph();
        store
            .graph()
            .write()
            .apply_change_set(&{
                let mut scratch = TraversalGraph::new();
                let ((), changes) = scratch
                    .dry_run(|g| {
                        for (name, vector) in every_kind() {
                            let v = source.get_vertex_by_id(name).unwrap().unwrap();
                            g.add_vertex(
                                Some(name.into()),
                                vec!["doc".into()],
                                source.vertex_properties(v).unwrap().unwrap().clone(),
                            )?;
                            let _ = vector;
                        }
                        let a = g.get_vertex_by_id("f32").unwrap().unwrap();
                        let b = g.get_vertex_by_id("i8").unwrap().unwrap();
                        let e = source.get_edge_by_id("e").unwrap().unwrap();
                        g.add_edge(
                            Some("e".into()),
                            Some("near".into()),
                            a,
                            b,
                            source.edge_properties(e).unwrap().unwrap().clone(),
                        )?;
                        Ok::<_, GraphError>(())
                    })
                    .unwrap();
                changes
            })
            .unwrap();
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(vectors_of(&store.graph().read()), vectors_of(&source));
        store.checkpoint(None).unwrap();
        assert!(store.verify().unwrap().problems.is_empty());
        let backup = MemDir::new();
        store.backup(&backup).unwrap();
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(vectors_of(&store.graph().read()), vectors_of(&source));
        let opened = Store::open_backup(&backup, StoreOptions::new()).unwrap();
        assert_eq!(vectors_of(&opened.graph().read()), vectors_of(&source));
    }

    fn journal_with(value: ElementProperty) -> Vec<u8> {
        let mut graph = TraversalGraph::new();
        let wal = SharedBuffer::new();
        Journal::new(wal.clone(), Durability::EveryCommit)
            .attach(&mut graph)
            .unwrap();
        graph
            .transaction(|g| {
                let mut props = PropertyMap::default();
                props.insert("v", value);
                g.add_vertex(Some("x".into()), vec!["x".into()], props)?;
                Ok::<_, GraphError>(())
            })
            .unwrap();
        wal.contents()
    }

    fn read_all(bytes: &[u8]) -> Result<Vec<JournalRecord>, PersistError> {
        persist::read_journal(bytes)?.collect()
    }

    const WAL_HEADER: usize = 64;
    const FRAME: usize = 25;

    /// CRC-32C (Castagnoli), bitwise.
    fn crc32c(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0x82F6_3B78
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn with_payload(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = bytes[..WAL_HEADER + FRAME].to_vec();
        out[WAL_HEADER + 4..WAL_HEADER + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let header_crc = crc32c(&out[WAL_HEADER..WAL_HEADER + 17]);
        out[WAL_HEADER + 17..WAL_HEADER + 21].copy_from_slice(&header_crc.to_le_bytes());
        out[WAL_HEADER + 21..WAL_HEADER + 25].copy_from_slice(&crc32c(payload).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn with_header_version(mut bytes: Vec<u8>, version: u32) -> Vec<u8> {
        bytes[8..12].copy_from_slice(&version.to_le_bytes());
        let crc = crc32c(&bytes[..60]);
        bytes[60..64].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    #[test]
    fn damaged_vector_payloads_are_errors_never_panics() {
        for (name, vector) in every_kind() {
            let bytes = journal_with(ElementProperty::Vector(vector));
            let payload = bytes[WAL_HEADER + FRAME..].to_vec();
            assert_eq!(read_all(&with_payload(&bytes, &payload)).unwrap().len(), 1);
            for cut in 0..payload.len() {
                let _ = read_all(&with_payload(&bytes, &payload[..cut]));
            }
            for index in 0..payload.len() {
                for bit in 0..8 {
                    let mut flipped = payload.clone();
                    flipped[index] ^= 1 << bit;
                    let _ = read_all(&with_payload(&bytes, &flipped));
                }
            }
            let _ = name;
        }
    }

    #[test]
    fn invalid_vector_payloads_are_damage() {
        let bytes = journal_with(ElementProperty::Vector(f32v(&[1.0, 2.0])));
        let payload = bytes[WAL_HEADER + FRAME..].to_vec();
        // Tag 14, element 0 (f32), dimension 2, then 8 bytes.
        let mut good = vec![14u8, 0, 2];
        good.extend_from_slice(&1.0f32.to_le_bytes());
        good.extend_from_slice(&2.0f32.to_le_bytes());
        let at = payload
            .windows(good.len())
            .position(|w| w == good.as_slice())
            .expect("the encoded vector");
        let mut nan = vec![14u8, 0, 2];
        nan.extend_from_slice(&f32::NAN.to_le_bytes());
        nan.extend_from_slice(&2.0f32.to_le_bytes());
        let mut negative_scale = vec![14u8, 2, 2];
        negative_scale.extend_from_slice(&(-1.0f32).to_le_bytes());
        negative_scale.extend_from_slice(&[1, 2, 0, 0, 0, 0]);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("unknown element", vec![14, 9, 2, 0, 0, 0, 0, 0, 0, 0, 0]),
            ("dimension 0", vec![14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            ("NaN", nan),
            ("negative scale", negative_scale),
            ("padding bits", vec![14, 3, 2, 0xff, 0, 0, 0, 0, 0, 0, 0]),
            (
                "infinite half",
                vec![14, 1, 2, 0x00, 0x7c, 0, 0, 0, 0, 0, 0],
            ),
        ];
        for (what, case) in cases {
            let mut damaged = payload[..at].to_vec();
            damaged.extend_from_slice(&case);
            damaged.extend_from_slice(&payload[at + good.len()..]);
            match read_all(&with_payload(&bytes, &damaged)) {
                Err(PersistError::Corrupt { .. }) => {}
                other => panic!("{what}: expected damage, got {other:?}"),
            }
        }
        // A dimension beyond the remaining bytes never allocates it.
        let mut huge = payload[..at].to_vec();
        huge.extend_from_slice(&[14, 0, 0x80, 0x80, 0x04]);
        assert!(read_all(&with_payload(&bytes, &huge)).is_err());
        // A vector in a file of format version 1 is damage.
        match read_all(&with_header_version(bytes, 1)) {
            Err(PersistError::Corrupt { reason, .. }) => {
                assert!(reason.contains("format version 1"), "{reason}")
            }
            other => panic!("expected damage, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// ChangeSet serde
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "serde")]
mod change_set_serde {
    use super::*;
    use graphersal::changes::{ChangeSet, Mutation};

    fn values_of(set: &ChangeSet) -> Vec<String> {
        set.mutations()
            .iter()
            .flat_map(|m| match m {
                Mutation::AddVertex { properties, .. } => properties
                    .iter()
                    .map(|(k, v)| format!("{k} = {}", exact(v)))
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn every_element_kind_round_trips_exactly_with_its_tag() {
        let mut graph = TraversalGraph::new();
        let ((), set) = graph
            .dry_run(|g| {
                for (name, vector) in every_kind() {
                    let mut props = PropertyMap::default();
                    props.insert("emb", ElementProperty::Vector(vector));
                    g.add_vertex(Some(name.into()), vec![], props)?;
                }
                Ok::<_, GraphError>(())
            })
            .unwrap();
        let text = serde_json::to_string(&set).unwrap();
        for part in [
            r#"{"@type":"graphersal:Vector","@value":{"element":"f32","data":[0.5,-1.0,0.25,0.0,2.0,-0.0,0.001,3.5,-7.25]}}"#,
            r#""element":"i8","scale":"#,
            r#""element":"bit","dimension":9,"bits":"#,
            r#""element":"f16","data":["#,
        ] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        let back: ChangeSet = serde_json::from_str(&text).unwrap();
        assert_eq!(values_of(&back), values_of(&set));
    }

    #[test]
    fn the_documented_payloads_read_back() {
        let mut graph = TraversalGraph::new();
        let ((), set) = graph
            .dry_run(|g| {
                let mut props = PropertyMap::default();
                props.insert(
                    "i",
                    ElementProperty::Vector(
                        f32v(&[0.5, -1.0]).quantize(VectorElement::I8).unwrap(),
                    ),
                );
                props.insert(
                    "b",
                    ElementProperty::Vector(
                        f32v(&[0.5, -1.0, 0.25])
                            .quantize(VectorElement::Bit)
                            .unwrap(),
                    ),
                );
                g.add_vertex(None, vec![], props)?;
                Ok::<_, GraphError>(())
            })
            .unwrap();
        let text = serde_json::to_string(&set).unwrap();
        // The examples of the book page "Transactions and Change Capture".
        assert!(
            text.contains(r#"{"element":"i8","scale":0.007874016,"codes":[64,-127]}"#),
            "{text}"
        );
        assert!(
            text.contains(r#"{"element":"bit","dimension":3,"bits":"a0"}"#),
            "{text}"
        );
    }

    #[test]
    fn malformed_vector_tags_are_refused() {
        let set = {
            let mut graph = TraversalGraph::new();
            let ((), set) = graph
                .dry_run(|g| {
                    let mut props = PropertyMap::default();
                    props.insert("emb", ElementProperty::Vector(f32v(&[1.0])));
                    g.add_vertex(None, vec![], props)?;
                    Ok::<_, GraphError>(())
                })
                .unwrap();
            set
        };
        let text = serde_json::to_string(&set).unwrap();
        let good = r#"{"element":"f32","data":[1.0]}"#;
        assert!(text.contains(good), "{text}");
        for bad in [
            r#"{"element":"f32","data":[]}"#,
            r#"{"element":"f32","data":["x"]}"#,
            r#"{"element":"f64","data":[1.0]}"#,
            r#"{"element":"f32","data":[1.0],"extra":1}"#,
            r#"{"element":"i8","scale":-1.0,"codes":[1]}"#,
            r#"{"element":"i8","scale":1.0,"codes":[300]}"#,
            r#"{"element":"bit","dimension":3,"bits":"ff"}"#,
            r#"{"element":"bit","dimension":3,"bits":"A0"}"#,
            r#"{"element":"f16","data":[0.1]}"#,
            r#"[1.0]"#,
        ] {
            let damaged = text.replace(good, bad);
            assert!(
                serde_json::from_str::<ChangeSet>(&damaged).is_err(),
                "{bad} was accepted"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// GraphSON and GraphML
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "io")]
mod io {
    use super::*;
    use graphersal::io::GraphSONImport;

    fn declared(mode: &str) -> GraphSchema {
        GraphSchema::from_json(&format!(
            r#"{{"mode": "{mode}", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{
                "emb": {{"type": "array", "format": "vector", "element": "i8", "dimension": 9}}}}}}}}}}}}"#
        ))
        .unwrap()
    }

    fn i8_graph() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        let vector = every_kind()[2].1.clone();
        let mut props = PropertyMap::default();
        props.insert("emb", ElementProperty::Vector(vector));
        graph
            .add_vertex(Some("v".into()), vec!["doc".into()], props)
            .unwrap();
        graph
    }

    fn emb(graph: &TraversalGraph) -> ElementProperty {
        let v = graph.get_vertex_by_id("v").unwrap().unwrap();
        graph
            .vertex_properties(v)
            .unwrap()
            .unwrap()
            .get("emb")
            .unwrap()
            .clone()
    }

    #[test]
    fn graphson_exports_a_list_of_floats_and_a_declared_target_restores_the_vector() {
        let graph = Graph::new(i8_graph());
        let mut out = Vec::new();
        graph.export_graphson_writer(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(r#"{"@type":"g:List","@value":[{"@type":"g:Float","@value":"#),
            "{text}"
        );
        // Without a schema: a list (rule 7).
        let mut plain = TraversalGraph::new();
        GraphSONImport::new()
            .import_reader(&mut plain, text.as_bytes())
            .unwrap();
        assert!(matches!(emb(&plain), ElementProperty::Array(_)));
        // With a declared vector (mode open): the same i8 vector again.
        let mut target = TraversalGraph::new();
        target.set_schema(declared("open")).unwrap();
        GraphSONImport::new()
            .import_reader(&mut target, text.as_bytes())
            .unwrap();
        assert_eq!(exact(&emb(&target)), exact(&emb(&i8_graph())));
    }

    #[test]
    fn graphson_reads_the_exact_vector_tag() {
        let line = r#"{"id":"v","label":"doc","properties":{"emb":[{"value":{"@type":"graphersal:Vector","@value":{"element":"bit","dimension":3,"bits":"a0"}}}]}}"#;
        let mut graph = TraversalGraph::new();
        let report = GraphSONImport::new()
            .import_reader(&mut graph, line.as_bytes())
            .unwrap();
        assert!(!report.has_skips(), "{report}");
        assert_eq!(exact(&emb(&graph)), "bit[3] [160]");
        let bad = line.replace("a0", "zz");
        let mut graph = TraversalGraph::new();
        let report = GraphSONImport::new()
            .import_reader(&mut graph, bad.as_bytes())
            .unwrap();
        assert!(report.has_skips(), "{report}");
    }

    #[test]
    fn graphml_restores_a_vector_from_the_target_schema() {
        let graph = Graph::new(i8_graph());
        let mut out = Vec::new();
        graph.export_graphml_writer(&mut out).unwrap();
        let xml = String::from_utf8(out).unwrap();
        // The JSON text of the values.
        assert!(xml.contains(">[0.5137795,-1.027559,"), "{xml}");
        // Mode none: GraphML still restores the declared type.
        let mut target = Graph::new(TraversalGraph::new());
        target.write().set_schema(declared("none")).unwrap();
        target
            .import_graphml_reader(std::io::Cursor::new(xml.into_bytes()))
            .unwrap();
        assert_eq!(exact(&emb(&target.read())), exact(&emb(&i8_graph())));
    }
}

// ---------------------------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------------------------

#[test]
fn memory_usage_counts_the_components() {
    let mut small = TraversalGraph::new();
    let mut big = TraversalGraph::new();
    for (graph, dimension) in [(&mut small, 4usize), (&mut big, 4096)] {
        let mut props = PropertyMap::default();
        props.insert(
            "emb",
            ElementProperty::Vector(Vector::from_f32(vec![0.5; dimension]).unwrap()),
        );
        graph
            .add_vertex(Some("v".into()), vec!["doc".into()], props)
            .unwrap();
    }
    let small_bytes = small.memory_usage().unwrap().unwrap().property_bytes;
    let big_bytes = big.memory_usage().unwrap().unwrap().property_bytes;
    assert!(
        big_bytes >= small_bytes + (4096 - 4) * 4,
        "{small_bytes} vs {big_bytes}"
    );
    let i8 = ElementProperty::Vector(
        Vector::from_f32(vec![0.5; 4096])
            .unwrap()
            .quantize(VectorElement::I8)
            .unwrap(),
    );
    assert!(i8.estimated_heap_bytes() >= 4096);
    assert!(i8.estimated_heap_bytes() < 4096 + 128);
}
