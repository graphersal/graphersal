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

// Stage 1b end to end through `g.gql`: OPTIONAL MATCH, WITH / LET / FILTER, aggregates,
// quantifiers, path variables and parameters, and how such rows look as Rhai values.

fn rows_text(script: &str) -> Vec<String> {
    strings(eval_value(modern(), script).unwrap())
}

/// The REPL rendering of a script's result, one line per row.
fn rendered_lines(script: &str) -> Vec<String> {
    eval(modern(), script)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn gql_optional_match_reads_null_as_unit() {
    let rows = rows_text(
        r#"g.gql(`MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(s)
                  RETURN a.name AS a, s.name AS s ORDER BY a, s`).to_list()"#,
    );
    assert_eq!(
        rows,
        [
            r#"#{"a": "josh", "s": "lop"}"#,
            r#"#{"a": "josh", "s": "ripple"}"#,
            r#"#{"a": "marko", "s": "lop"}"#,
            r#"#{"a": "peter", "s": "lop"}"#,
            r#"#{"a": "vadas", "s": ()}"#,
        ]
    );
    // A GQL NULL is Rhai's `()`.
    let unit = eval_value(
        modern(),
        r#"let r = g.gql(`MATCH (a:person {name: 'vadas'}) OPTIONAL MATCH (a)-[:created]->(s)
                          RETURN s`).next();
           r.s == ()"#,
    )
    .unwrap();
    assert!(unit.as_bool().unwrap());
}

#[test]
fn gql_with_let_and_filter_with_parameters() {
    let rows = rows_text(
        r#"g.gql(`MATCH (a:person) LET x = a.age * $k FILTER x > $min
                  RETURN a.name AS a, x ORDER BY a`, #{k: 2, min: 60}).to_list()"#,
    );
    assert_eq!(
        rows,
        [r#"#{"a": "josh", "x": 64}"#, r#"#{"a": "peter", "x": 70}"#]
    );
    let rows = rows_text(
        r#"g.gql(`MATCH (a:person)-[:created]->(s) WITH a, count(s) AS n WHERE n >= $n
                  RETURN a.name AS a, n`, #{n: 2}).to_list()"#,
    );
    assert_eq!(rows, [r#"#{"a": "josh", "n": 2}"#]);
    // A materializing WITH (ORDER BY + LIMIT) followed by another MATCH.
    let rows = rows_text(
        r#"g.gql(`MATCH (a:person) WITH a ORDER BY a.age DESC LIMIT $top
                  MATCH (a)-[:created]->(s) RETURN a.name AS a, s.name AS s ORDER BY a, s`,
                 #{top: 2}).to_list()"#,
    );
    assert_eq!(
        rows,
        [
            r#"#{"a": "josh", "s": "lop"}"#,
            r#"#{"a": "josh", "s": "ripple"}"#,
            r#"#{"a": "peter", "s": "lop"}"#,
        ]
    );
}

#[test]
fn gql_aggregates_are_rhai_numbers_lists_and_unit() {
    let row = eval_value(
        modern(),
        r#"g.gql(`MATCH (a:person) RETURN count(a) AS c, sum(a.age) AS s, avg(a.age) AS m,
                  min(a.age) AS lo, max(a.age) AS hi, collect_list(a.name) AS names`).next()"#,
    )
    .unwrap()
    .cast::<graphersal::script::rhai::Map>();
    assert_eq!(row["c"].as_int().unwrap(), 4);
    assert_eq!(row["s"].as_int().unwrap(), 123);
    assert_eq!(row["m"].as_float().unwrap(), 30.75);
    assert_eq!(row["lo"].as_int().unwrap(), 27);
    assert_eq!(row["hi"].as_int().unwrap(), 35);
    let mut names = strings(row["names"].clone());
    names.sort();
    assert_eq!(names, ["josh", "marko", "peter", "vadas"]);
    // Empty input: count 0, sum 0, collect_list [], avg NULL (C4).
    let row = eval_value(
        modern(),
        r#"g.gql(`MATCH (a:person) WHERE a.age > $max RETURN count(a) AS c, sum(a.age) AS s,
                  avg(a.age) AS m, collect_list(a.name) AS l`, #{max: 100}).next()"#,
    )
    .unwrap()
    .cast::<graphersal::script::rhai::Map>();
    assert_eq!(row["c"].as_int().unwrap(), 0);
    assert_eq!(row["s"].as_int().unwrap(), 0);
    assert!(row["m"].is_unit());
    assert!(row["l"].clone().cast::<Array>().is_empty());
    // Grouping after an OPTIONAL MATCH: the unmatched person counts 0 and collects [].
    let rows = rows_text(
        r#"g.gql(`MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(s)
                  RETURN a.name AS a, count(s) AS n ORDER BY a`).to_list()"#,
    );
    assert_eq!(
        rows,
        [
            r#"#{"a": "josh", "n": 2}"#,
            r#"#{"a": "marko", "n": 1}"#,
            r#"#{"a": "peter", "n": 1}"#,
            r#"#{"a": "vadas", "n": 0}"#,
        ]
    );
}

#[test]
fn gql_quantified_edges_in_both_spellings() {
    let gql = rows_text(
        r#"g.gql("MATCH (a:person {name: $n})-[:knows]->{1,2}(b) RETURN b.name AS b ORDER BY b",
                 #{n: "marko"}).select("b").to_list()"#,
    );
    let cypher = rows_text(
        r#"g.gql("MATCH (a:person {name: $n})-[:knows*1..2]->(b) RETURN b.name AS b ORDER BY b",
                 #{n: "marko"}).select("b").to_list()"#,
    );
    assert_eq!(gql, ["josh", "vadas"]);
    assert_eq!(gql, cypher);
    // `{0,1}` includes the start node itself.
    let rows = rows_text(
        r#"g.gql(`MATCH (a:person {name: 'marko'})-[:created]->{0,1}(b) RETURN b.name AS b
                  ORDER BY b`).select("b").to_list()"#,
    );
    assert_eq!(rows, ["lop", "marko"]);
}

#[test]
fn gql_path_values_are_rhai_arrays_of_elements() {
    let checks = eval_value(
        modern(),
        r#"let r = g.gql(`MATCH p = (a {name: 'marko'})-[:knows]->(b {name: 'vadas'})
                          RETURN p, a, path_length(p) AS len`).next();
           [type_of(r.p), r.p.len(), type_of(r.p[0]), type_of(r.p[1]), type_of(r.p[2]),
            r.p[0] == r.a, r.len]"#,
    )
    .unwrap();
    assert_eq!(
        strings(checks),
        ["array", "3", "Vertex", "Edge", "Vertex", "true", "1"]
    );
    // Rows with paths, as the REPL shows a `to_list()`.
    let rows = rendered_lines(
        r#"g.gql("MATCH p = (a:person {name: $n})-[:knows]->{1,2}(b) RETURN p ORDER BY b.name",
                 #{n: "marko"}).to_list()"#,
    );
    assert_eq!(
        rows,
        [
            r#"#{"p": [v[1], e[2][1-knows->4], v[4]]}"#,
            r#"#{"p": [v[1], e[0][1-knows->2], v[2]]}"#,
        ]
    );
    // Gremlin steps after it: `select("p").unfold()` walks the elements of each path.
    let names = rows_text(
        r#"g.gql("MATCH p = (a {name: 'marko'})-[:created]->(s) RETURN p").select("p").unfold()
            .has_label("software").values("name").to_list()"#,
    );
    assert_eq!(names, ["lop"]);
    // A path of an unmatched OPTIONAL MATCH is `()`.
    let rows = rendered_lines(
        r#"g.gql(`MATCH (a:person) OPTIONAL MATCH p = (a)-[:created]->(:software {name: 'ripple'})
                  RETURN a.name AS a, p ORDER BY a`).to_list()"#,
    );
    assert_eq!(
        rows,
        [
            r#"#{"a": "josh", "p": [v[4], e[4][4-created->5], v[5]]}"#,
            r#"#{"a": "marko", "p": ()}"#,
            r#"#{"a": "peter", "p": ()}"#,
            r#"#{"a": "vadas", "p": ()}"#,
        ]
    );
}

#[test]
fn rendered_rows_with_paths_and_lists() {
    let text = eval(
        modern(),
        r#"g.gql(`MATCH p = (a:person {name: 'marko'})-[:knows]->(b)
                  RETURN b.name AS b, p ORDER BY b`)"#,
    )
    .unwrap();
    assert!(text.contains("[v[1], e[2][1-knows->4], v[4]]"), "{text}");
    assert!(text.contains("[v[1], e[0][1-knows->2], v[2]]"), "{text}");
    assert!(
        text.find("josh").unwrap() < text.find("vadas").unwrap(),
        "{text}"
    );
    // JSON: a list stays a JSON array, in the written column order.
    let json = eval_value(
        modern(),
        r#"g.gql(`MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(s)
                  RETURN a.name AS a, collect_list(s.name) AS soft, count(s) AS n ORDER BY a`)
            .to_json()"#,
    )
    .unwrap();
    let json = try_render_value(json, VisualizeFormat::Auto, &RenderOptions::unlimited())
        .unwrap()
        .text;
    // vadas created nothing: an empty array and a zero count (pretty-printed JSON).
    let vadas = &json[json.find("\"vadas\"").unwrap()..];
    assert!(
        vadas.contains("\"soft\": []") && vadas.contains("\"n\": 0"),
        "{json}"
    );
    let marko = &json[json.find("\"marko\"").unwrap()..];
    assert!(marko.contains("\"lop\""), "{json}");
    let first = &json[json.find('{').unwrap()..];
    let (a, soft, n) = (
        first.find("\"a\"").unwrap(),
        first.find("\"soft\"").unwrap(),
        first.find("\"n\"").unwrap(),
    );
    assert!(a < soft && soft < n, "{json}");
}
