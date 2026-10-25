//! The property filters (`has(k)`, `hasNot(k)`, `has(k, v)`, `has(k, P)`, `hasValue(..)` in all
//! three forms) over every stream shape, in both DSL spellings: each shape is either filtered or
//! fails with a typed cast error naming what it got. Never a panic, never a silent drop of a
//! shape the filter cannot read.

use std::sync::Arc;

use graphersal::prelude::*;

thread_local! {
    static ENGINE: rhai::Engine = crate::script_host::engine();
    static MODERN: Arc<Graph> = Arc::new(GraphSource::tinkerpop_modern());
}

fn run(script: &str) -> Result<rhai::Dynamic, String> {
    ENGINE.with(|engine| {
        let mut scope = MODERN.with(|g| crate::script_host::graph_scope(g.clone()));
        engine
            .eval_with_scope::<rhai::Dynamic>(&mut scope, script)
            .map_err(|err| err.to_string())
    })
}

/// Shape producers (one traverser each, except where noted).
const VERTEX: &str = r#"g.V("1")"#;
const EDGE: &str = r#"g.E("0")"#;
const VERTEX_PROPERTY: &str = r#"g.V("1").properties("name")"#;
const EDGE_PROPERTY: &str = r#"g.E("0").properties("weight")"#;
const PATH: &str = r#"g.V("1").path()"#;
const PATH_PROPERTY: &str = r#"g.V("1").properties(jpath("name"))"#;
const ENTRY: &str = r#"g.V("1").valueMap().unfold()"#;
const RECORD: &str = r#"g.V("1").valueMap()"#;
const OBJECT: &str = r#"g.V("1").project("name").by("name")"#;
const ARRAY: &str = r#"g.V("1").fold()"#;
const SCALAR: &str = r#"g.inject(1)"#;
const SCHEMA: &str = r#"g.V().infer_schema()"#;
const GRAPH: &str = r#"g.E().subgraph("sg").cap("sg")"#;

/// `Ok(n)`: the filter keeps `n` of the producer's traversers; `Err(t)`: a cast error naming `t`.
type Expect = Result<i64, &'static str>;

fn check(producer: &str, filters: &[&str], expected: Expect) {
    for filter in filters {
        let script = format!("{producer}.{filter}.count().next()");
        match (run(&script), expected) {
            (Ok(value), Ok(n)) => assert_eq!(value.as_int().ok(), Some(n), "{script}"),
            (Err(report), Err(got)) => assert!(
                report.contains(&format!("got type '{got}'")),
                "{script}: expected a cast error naming '{got}', got:\n{report}"
            ),
            (other, _) => panic!("{script}: expected {expected:?}, got {other:?}"),
        }
    }
}

#[test]
fn key_filters_over_every_shape() {
    // has(k) / hasNot(k) / has(k, v) / has(k, P), each in both spellings, keyed on "name"
    let keep = [
        r#"has("name")"#,
        r#"has("name", "marko")"#,
        r#"has("name", P.eq("marko"))"#,
        r#"has("name", P::eq("marko"))"#,
    ];
    let drop = [r#"hasNot("name")"#, r#"has_not("name")"#];
    for (producer, expected) in [
        (VERTEX, Ok(1)),
        (RECORD, Ok(1)),
        (OBJECT, Ok(1)),
        (VERTEX_PROPERTY, Err("string")),
        (EDGE_PROPERTY, Err("float64")),
        (SCALAR, Err("int64")),
        (PATH, Err("path")),
        (ENTRY, Err("entry")),
        (ARRAY, Err("array")),
        (SCHEMA, Err("schema")),
        (GRAPH, Err("graph")),
    ] {
        // valueMap() holds lists: has("name", "marko") compares ["marko"], not "marko"
        if producer == RECORD {
            check(producer, &keep[..1], expected);
        } else {
            check(producer, &keep, expected);
        }
        check(producer, &drop, expected.map(|n| 1 - n));
    }
    // an edge has no "name": kept by hasNot, dropped by the others
    check(EDGE, &keep, Ok(0));
    check(EDGE, &drop, Ok(1));
}

#[test]
fn value_filters_over_every_shape() {
    let filters = [
        r#"hasValue("marko")"#,
        r#"has_value("marko")"#,
        r#"hasValue(P.eq("marko"))"#,
        r#"hasValue("nope", "marko")"#,
    ];
    for (producer, expected) in [
        (VERTEX, Ok(1)),
        (VERTEX_PROPERTY, Ok(1)),
        (PATH_PROPERTY, Ok(1)),
        (OBJECT, Ok(1)),
        (EDGE, Ok(0)),
        (EDGE_PROPERTY, Ok(0)),
        (SCALAR, Ok(0)),
        // a valueMap() record holds lists, which the value filters do not search
        (RECORD, Err("array")),
        (PATH, Err("path")),
        (ENTRY, Err("entry")),
        (ARRAY, Err("array")),
        (SCHEMA, Err("schema")),
        (GRAPH, Err("graph")),
    ] {
        check(producer, &filters, expected);
    }
}

#[test]
fn has_value_with_several_values_matches_any() {
    check(VERTEX_PROPERTY, &[r#"hasValue("x", "marko", 3)"#], Ok(1));
    check(
        r#"g.V("1").properties()"#,
        &[r#"hasValue("marko", 29)"#],
        Ok(2),
    );
    check(EDGE_PROPERTY, &[r#"hasValue(0.5, 2)"#], Ok(1));
    check(EDGE, &[r#"has_value(0.5, 0.4)"#], Ok(1));
    check(SCALAR, &[r#"hasValue(2, 1)"#], Ok(1));
    check(
        r#"g.V()"#,
        &[r#"hasValue("lop", "ripple", "nobody")"#],
        Ok(2),
    );
    check(
        r#"g.V().where(__.hasValue("vadas", "josh"))"#,
        &[r#"has("age")"#],
        Ok(2),
    );
}

/// A `properties(jpath(..))` value is a `pathProperty` for the key filters too, as everywhere
/// else (adjacency steps, `drop`); an unfolded map entry stays an `entry`.
#[test]
fn key_filters_name_a_path_property_by_its_kind() {
    check(
        PATH_PROPERTY,
        &[
            r#"has("a")"#,
            r#"hasNot("a")"#,
            r#"has_not("a")"#,
            r#"has("a", 1)"#,
            r#"has("a", P.gt(0))"#,
            r#"has("a", P::gt(0))"#,
        ],
        Err("pathProperty"),
    );
    check(
        r#"g.V("1").valueMap().unfold()"#,
        &[
            r#"has("a")"#,
            r#"hasNot("a")"#,
            r#"has("a", 1)"#,
            r#"has("a", P.gt(0))"#,
        ],
        Err("entry"),
    );
}
