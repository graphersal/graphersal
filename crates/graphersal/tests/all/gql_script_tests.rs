//! The Rhai binding `g.gql(query[, #{params}])` (features `script` + `gql`): rows, parameters,
//! multi-line backtick queries, Gremlin steps after it, the start-only rule, parameter errors and
//! the column order of the JSON rendering.
#![cfg(all(feature = "script", feature = "gql"))]

use std::sync::Arc;

use crate::script_host::{ScriptGraphTraversalSource, eval, eval_value, try_render_value};
use graphersal::exec::RenderOptions;
use graphersal::prelude::*;
use graphersal::schema::VisualizeFormat;
use graphersal::script::rhai::{Array, Dynamic};

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn strings(value: Dynamic) -> Vec<String> {
    value
        .cast::<Array>()
        .into_iter()
        .map(|item| item.to_string())
        .collect()
}

fn error_text(script: &str) -> String {
    match eval_value(modern(), script) {
        Ok(value) => panic!("`{script}` should fail, gave {value}"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn gql_without_parameters_returns_rows() {
    let names = eval_value(
        modern(),
        r#"g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name ORDER BY name")
            .select("name").to_list()"#,
    )
    .unwrap();
    assert_eq!(strings(names), ["josh", "peter"]);
    let rows = eval_value(
        modern(),
        r#"g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age ORDER BY age").to_list()"#,
    )
    .unwrap()
    .cast::<Array>();
    assert_eq!(rows.len(), 4);
    let first = rows[0].clone().cast::<graphersal::script::rhai::Map>();
    assert_eq!(first["name"].to_string(), "vadas");
    assert_eq!(first["age"].as_int().unwrap(), 27);
}

#[test]
fn gql_with_parameters_reads_their_values() {
    let ages = eval_value(
        modern(),
        r#"g.gql("MATCH (p:person {name: $name}) RETURN p.age AS age", #{name: "marko"})
            .select("age").to_list()"#,
    )
    .unwrap();
    assert_eq!(strings(ages), ["29"]);
    let names = eval_value(
        modern(),
        r#"let min = 30;
           g.gql("MATCH (p:person) WHERE p.age > $min RETURN p.name AS n ORDER BY n", #{min: min})
            .select("n").to_list()"#,
    )
    .unwrap();
    assert_eq!(strings(names), ["josh", "peter"]);
    // `()` is no parameters.
    let count = eval_value(
        modern(),
        r#"g.gql("MATCH (p:person) RETURN p", ()).count().next()"#,
    )
    .unwrap();
    assert_eq!(count.as_int().unwrap(), 4);
}

#[test]
fn gql_in_a_multi_line_backtick_string() {
    // Rhai backtick strings span lines; `${..}` in them is Rhai interpolation (the value is
    // spliced into the query text), so a GQL value is better passed as a `$param`.
    let names = eval_value(
        modern(),
        "let min = 30;
         g.gql(`MATCH (p:person)
                WHERE p.age > ${min}   // interpolated by Rhai
                  AND p.name <> $skip  -- a GQL parameter
                RETURN p.name AS name
                ORDER BY name`, #{skip: \"peter\"}).select(\"name\").to_list()",
    )
    .unwrap();
    assert_eq!(strings(names), ["josh"]);
}

#[test]
fn gremlin_steps_follow_gql() {
    let created = eval_value(
        modern(),
        r#"g.gql("MATCH (p:person {name: 'marko'}) RETURN p").select("p").out("created")
            .values("name").to_list()"#,
    )
    .unwrap();
    assert_eq!(strings(created), ["lop"]);
    // Source steps may come before it.
    let count = eval_value(
        modern(),
        r#"g.with_sack(1).gql("MATCH (p:person) RETURN p").count().next()"#,
    )
    .unwrap();
    assert_eq!(count.as_int().unwrap(), 4);
}

#[test]
fn gql_is_refused_inside_an_anonymous_traversal() {
    let text = error_text(r#"g.v().where(__.gql("MATCH (p) RETURN p")).count().next()"#);
    assert!(text.contains("inside __"), "{text}");
    assert!(text.contains("g.gql("), "{text}");
}

#[test]
fn gql_is_refused_after_other_steps() {
    let text = error_text(r#"g.v().gql("MATCH (p) RETURN p").count().next()"#);
    assert!(text.contains("after other steps"), "{text}");
}

#[test]
fn gql_parameters_must_be_a_map_of_values() {
    let text = error_text(r#"g.gql("MATCH (p) RETURN p", [1, 2]).to_list()"#);
    assert!(text.contains("the query parameters as a map"), "{text}");
    let text =
        error_text(r#"g.gql("MATCH (p) WHERE p.x = $t RETURN p", #{t: __.out()}).to_list()"#);
    assert!(text.contains("parameter `t`"), "{text}");
}

#[test]
fn gql_errors_fail_the_traversal_with_the_gql_report() {
    let text = error_text(r#"g.gql("MATCH (p) RETRUN p").to_list()"#);
    assert!(text.contains("GQLSTATUS"), "{text}");
}

#[test]
fn gql_is_not_mutating() {
    let traversal = eval_value(modern(), r#"g.gql("MATCH (p:person) RETURN p")"#)
        .unwrap()
        .cast::<ScriptGraphTraversalSource>();
    assert!(!traversal.is_mutating());
}

#[test]
fn json_rendering_keeps_the_written_column_order() {
    let json = eval_value(
        modern(),
        r#"g.gql("MATCH (p:person {name: 'marko'}) RETURN p.name AS name, p.age AS age, p.name AS a")
            .to_json()"#,
    )
    .unwrap();
    let json = try_render_value(json, VisualizeFormat::Auto, &RenderOptions::unlimited())
        .unwrap()
        .text;
    let name = json.find("\"name\"").unwrap();
    let age = json.find("\"age\"").unwrap();
    let a = json.find("\"a\"").unwrap();
    assert!(name < age && age < a, "{json}");
}

#[test]
fn rendered_rows_show_every_column() {
    let text = eval(
        modern(),
        r#"g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age ORDER BY age")"#,
    )
    .unwrap();
    assert!(text.contains("name") && text.contains("age"), "{text}");
    assert!(
        text.find("vadas").unwrap() < text.find("peter").unwrap(),
        "{text}"
    );
}
