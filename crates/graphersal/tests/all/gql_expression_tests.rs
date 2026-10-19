//! GQL stage 2c map projections, list comprehensions and pattern comprehensions (feature `gql`,
//! design item 7, M2, M4, E12, E13, E14): Cypher map projections `v {.name, .*, x, k: expr}`
//! and list comprehensions `[x IN list WHERE cond | expr]` run end to end through the ordinary
//! evaluator steps (no new step); a pattern comprehension `[(a)-->(b) WHERE cond | expr]` is a
//! child of the `gql:*` step evaluating it, ending in `gql:value`. Differential twins against
//! Gremlin (Rhai): map projections vs `project()` / `value_map()`, comprehensions vs
//! `unfold().is(..).fold()`, pattern comprehensions vs `map(__.out(..)..fold())` /
//! `project().by(__.out(..)..fold())`; rows compared as multisets (design D18). The `NULL`
//! cases are hand-computed rows.
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::prelude::*;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn rows(graph: &Arc<Graph>, query: &str) -> Vec<ElementProperty> {
    graph
        .read()
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// The failure of `query`: the message of its GQL root cause and its GQLSTATUS.
fn failure(graph: &Arc<Graph>, query: &str) -> String {
    match graph.read().traversal().gql(query).to_list() {
        Ok(rows) => panic!("{query} gave {rows:?}"),
        Err(error) => match error.root_cause() {
            TraverserError::Gql {
                message, gqlstatus, ..
            } => format!("{message} [{gqlstatus}]"),
            other => panic!("{query}: not a GQL error: {other}"),
        },
    }
}

fn profile(graph: &Arc<Graph>, query: &str) -> String {
    graph
        .read()
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .to_string()
}

/// The steps of a profile, without the metrics columns.
fn steps(profile: &str) -> Vec<String> {
    profile
        .lines()
        .skip_while(|line| !line.starts_with("====="))
        .skip(1)
        .take_while(|line| !line.starts_with("====="))
        .filter(|line| !line.trim_start().starts_with('[') && !line.contains("TOTAL:"))
        .map(|line| {
            let step = line.trim_start();
            step[..step.find("  ").unwrap_or(step.len())].to_string()
        })
        .collect()
}

fn column<'r>(row: &'r ElementProperty, key: &str) -> &'r ElementProperty {
    match row {
        ElementProperty::Object(map) => map
            .get(key)
            .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
        other => panic!("not a row map: {other:?}"),
    }
}

/// A value as comparable text keeping its type: `null`, `'text'`, `i:1`, `f:1.0`, `true`,
/// `[..]`, `{k: v, ..}` (keys sorted), `v<id>`, `e<id>`.
fn norm(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => format!("'{s}'"),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) if n.is_i64() => format!("i:{i}"),
            (_, Some(f)) => format!("f:{f:?}"),
            _ => format!("{n:?}"),
        },
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(norm).collect();
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Object(map) => {
            let mut items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", norm(v)))
                .collect();
            items.sort();
            format!("{{{}}}", items.join(", "))
        }
        ElementProperty::Vertex(v) => format!("v{}", v.id()),
        ElementProperty::Edge(e) => format!("e{}", e.id()),
        other => format!("{other:?}"),
    }
}

/// Every row of `query` as `a|b|..` over `keys`, sorted (a multiset).
fn table(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = rows(graph, query)
        .iter()
        .map(|row| {
            keys.iter()
                .map(|k| norm(column(row, k)))
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();
    out.sort();
    out
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

/// The keys of the map in column `key` of every row, in the map's own order.
fn key_orders(graph: &Arc<Graph>, query: &str, key: &str) -> Vec<Vec<String>> {
    rows(graph, query)
        .iter()
        .map(|row| match column(row, key) {
            ElementProperty::Object(map) => map.iter().map(|(k, _)| k.to_string()).collect(),
            other => panic!("{query}: not a map: {other:?}"),
        })
        .collect()
}

// ----- differential twins against Gremlin (Rhai) -----

#[cfg(feature = "script")]
mod twins {
    use std::sync::Arc;

    use graphersal::prelude::*;

    use super::{modern, norm};

    /// The values of the Rhai script `script` (a traversal), as sorted `norm` texts.
    fn values(graph: &Arc<Graph>, script: &str) -> Vec<String> {
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
        let lock = graph.read();
        let mut out: Vec<String> = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .iter()
            .map(norm)
            .collect();
        out.sort();
        out
    }

    /// The `x` column of the GQL script `gql` (`g.gql(..)`, selected) against the values of the
    /// Gremlin twin, as multisets.
    fn twin(graph: &Arc<Graph>, gql: &str, gremlin: &str) {
        let left = values(graph, &format!("{gql}.select(\"x\")"));
        let right = values(graph, gremlin);
        assert!(!left.is_empty(), "{gql}: no rows to compare");
        assert_eq!(left, right, "\nGQL:  {gql}\ntwin: {gremlin}");
    }

    #[test]
    fn map_projections_match_project_and_value_map() {
        let graph = modern();
        for (gql, gremlin) in [
            (
                r#"g.gql("MATCH (p:person) RETURN p {.name, .age} AS x")"#,
                r#"g.V().hasLabel("person").project("name", "age").by("name").by("age")"#,
            ),
            (
                r#"g.gql("MATCH (n) RETURN n {.*} AS x")"#,
                "g.V().value_map().by(__.unfold())",
            ),
            (
                r#"g.gql("MATCH ()-[e]->() RETURN e {.*} AS x")"#,
                "g.E().value_map().by(__.unfold())",
            ),
            (
                r#"g.gql("MATCH (s:software) RETURN s {.name, .lang} AS x")"#,
                r#"g.V().hasLabel("software").value_map("name", "lang").by(__.unfold())"#,
            ),
            (
                r#"g.gql("MATCH (p:person)-[e:knows]->(f) RETURN p {.name, friend: f.name, w: e.weight} AS x")"#,
                r#"g.V().hasLabel("person").as("p").out_e("knows").as("e").in_v().project("name", "friend", "w").by(__.select("p").values("name")).by("name").by(__.select("e").values("weight"))"#,
            ),
            // An element inside stays an element.
            (
                r#"g.gql("MATCH (p:person)-[:created]->(s) RETURN p {.name, s} AS x")"#,
                r#"g.V().hasLabel("person").as("p").out("created").project("name", "s").by(__.select("p").values("name")).by()"#,
            ),
            // A projection per row of a substituted name.
            (
                r#"g.gql("MATCH (p:person) LET a = p.age + 1 RETURN p {.name, a} AS x")"#,
                r#"g.V().hasLabel("person").project("name", "a").by("name").by(__.values("age").math("_ + 1").as_number(GType::LONG))"#,
            ),
        ] {
            twin(&graph, gql, gremlin);
        }
    }

    #[test]
    fn comprehensions_match_unfold_is_fold() {
        let graph = modern();
        for (gql, gremlin) in [
            (
                r#"g.gql("RETURN [x IN [1, 2, 3, 4] WHERE x > 2] AS x")"#,
                "g.inject([1, 2, 3, 4]).local(__.unfold().is(P.gt(2)).fold())",
            ),
            (
                r#"g.gql("RETURN [y IN ['a', 'b', 'c'] WHERE y <> 'b' | upper(y)] AS x")"#,
                r#"g.inject(["a", "b", "c"]).local(__.unfold().is(P.neq("b")).to_upper().fold())"#,
            ),
            // Per row: a collected list filtered and mapped.
            (
                r#"g.gql("MATCH (p:person)-[:created]->(s) WITH p, collect_list(s.name) AS ns RETURN [y IN ns WHERE y <> 'lop' | upper(y)] AS x")"#,
                r#"g.V().hasLabel("person").filter(__.out("created")).map(__.out("created").values("name").is(P.neq("lop")).to_upper().fold())"#,
            ),
            // The list part reads the row, the WHERE part the row and the item.
            (
                r#"g.gql("MATCH (p:person)-[:knows]->(f) RETURN [y IN [27, 29, 32, 35] WHERE y > f.age] AS x")"#,
                r#"g.V().hasLabel("person").out("knows").as("f").map(__.constant([27, 29, 32, 35]).unfold().where(P.gt("f")).by().by("age").fold())"#,
            ),
            // In WHERE and in FOR.
            (
                r#"g.gql("MATCH (p:person) WHERE size([y IN [p.age] WHERE y > 30]) > 0 RETURN p.name AS x")"#,
                r#"g.V().hasLabel("person").has("age", P.gt(30)).values("name")"#,
            ),
            (
                r#"g.gql("FOR x IN [y IN [1, 2, 3] WHERE y > 1 | y * 2] RETURN x")"#,
                "g.inject(0).flat_map(__.constant([4, 6]).unfold())",
            ),
            // A projection per item.
            (
                r#"g.gql("MATCH (p:person {name: 'marko'})-[:knows]->(f) WITH collect_list(f) AS fs FOR x IN [y IN fs | y {.name}] RETURN x")"#,
                r#"g.V().has("person", "name", "marko").out("knows").project("name").by("name")"#,
            ),
        ] {
            twin(&graph, gql, gremlin);
        }
    }

    #[test]
    fn pattern_comprehensions_match_map_fold() {
        let graph = modern();
        for (gql, gremlin) in [
            (
                r#"g.gql("MATCH (p:person) RETURN [(p)-[:knows]->(f) | f.name] AS x")"#,
                r#"g.V().hasLabel("person").map(__.out("knows").values("name").fold())"#,
            ),
            // The body's WHERE, an incoming edge variable.
            (
                r#"g.gql("MATCH (p:person) RETURN [(p)-[:created]->(s) WHERE s.lang = 'java' AND p.age > 30 | s.name] AS x")"#,
                r#"g.V().hasLabel("person").map(__.choose(__.has("age", P.gt(30)), __.out("created").has("lang", "java").values("name").fold(), __.constant([])))"#,
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN [(p)<-[e:knows]-(a) | e.weight] AS x")"#,
                r#"g.V().hasLabel("person").map(__.in_e("knows").values("weight").fold())"#,
            ),
            // A path variable inside.
            (
                r#"g.gql("MATCH (p:person) RETURN [q = (p)-[:knows]->(f) | [n IN nodes(q) | n.name]] AS x")"#,
                r#"g.V().hasLabel("person").map(__.out("knows").path().by("name").map(__.unfold().fold()).fold())"#,
            ),
            // Inside a map projection (an LLM's JSON answer) and nested.
            (
                r#"g.gql("MATCH (p:person) RETURN p {.name, friends: [(p)-[:knows]->(f) | f.name]} AS x")"#,
                r#"g.V().hasLabel("person").project("name", "friends").by("name").by(__.out("knows").values("name").fold())"#,
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN [(p)-[:knows]->(f) | [(f)-[:created]->(s) | s.name]] AS x")"#,
                r#"g.V().hasLabel("person").map(__.out("knows").map(__.out("created").values("name").fold()).fold())"#,
            ),
            // In WHERE, WITH and an aggregate.
            (
                r#"g.gql("MATCH (p:person) WHERE size([(p)-->(b) | b]) > 1 RETURN p.name AS x")"#,
                r#"g.V().hasLabel("person").where(__.out().count().is(P.gt(1))).values("name")"#,
            ),
            (
                r#"g.gql("MATCH (p:person) WITH p, [(p)-[:created]->(s) | s.name] AS l WHERE size(l) > 0 RETURN l AS x")"#,
                r#"g.V().hasLabel("person").filter(__.out("created")).map(__.out("created").values("name").fold())"#,
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN collect_list(size([(p)-->(b) | b])) AS x")"#,
                r#"g.V().hasLabel("person").map(__.out().count()).fold()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN sum(size([(p)<--(b) | b])) AS x")"#,
                r#"g.V().hasLabel("person").in_().count()"#,
            ),
            // From a unit row: no import, a child-leading scan.
            (
                r#"g.gql("RETURN size([(a:person)-[:created]->(s:software) WHERE s.name = 'lop' | a.name]) AS x")"#,
                r#"g.inject(0).map(__.V().hasLabel("person").where(__.out("created").has("name", "lop")).count())"#,
            ),
        ] {
            twin(&graph, gql, gremlin);
        }
    }

    /// The JSON-shaped answer an LLM asks for, `p {.name, friends: [(p)-->(f) | f {.name}]}`,
    /// renders as nested JSON objects and arrays exactly like its Gremlin twin
    /// `project().by(__.out().project().fold())`; an element inside renders as the element.
    #[test]
    fn a_nested_answer_renders_as_json_like_its_twin() {
        let render = |script: &str| {
            let value = crate::script_host::eval_value(modern(), script)
                .unwrap_or_else(|e| panic!("{script}: {e}"));
            crate::script_host::try_render_value(
                value,
                graphersal::schema::VisualizeFormat::Auto,
                &graphersal::exec::RenderOptions::unlimited(),
            )
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .text
        };
        let gql = render(
            r#"g.gql("MATCH (p:person {name: 'marko'}) RETURN p {.name, friends: [(p)-->(f) | f {.name}]} AS answer").to_json()"#,
        );
        let twin = render(
            r#"g.V().has("person", "name", "marko").project("answer").by(__.project("name", "friends").by("name").by(__.out().project("name").by("name").fold())).to_json()"#,
        );
        assert_eq!(gql, twin);
        let expected = "[\n  {\n    \"answer\": {\n      \"name\": \"marko\",\n      \
                        \"friends\": [\n        {\n          \"name\": \"josh\"\n        },\n        \
                        {\n          \"name\": \"lop\"\n        },\n        {\n          \"name\": \
                        \"vadas\"\n        }\n      ]\n    }\n  }\n]";
        assert_eq!(gql, expected);
        // An element kept in the nested answer renders as the element (id, label, properties),
        // like the twin's vertex; a person without friends has an empty array.
        let gql = render(
            r#"g.gql("MATCH (p:person) WHERE p.name IN ['marko', 'peter'] RETURN p {.name, friends: [(p)-[:knows]->(f) | f]} AS answer ORDER BY p.name").to_json()"#,
        );
        let twin = render(
            r#"g.V().has("person", "name", P.within(["marko", "peter"])).order().by("name").project("answer").by(__.project("name", "friends").by("name").by(__.out("knows").fold())).to_json()"#,
        );
        assert_eq!(gql, twin);
        for part in [
            "\"label\": \"person\"",
            "\"name\": \"josh\"",
            "\"age\": 27",
            "\"friends\": []",
        ] {
            assert!(gql.contains(part), "{gql}");
        }
    }
}

// ----- map projections -----

#[test]
fn a_projection_keeps_written_order_with_star_in_place() {
    let graph = modern();
    assert_eq!(
        key_orders(
            &graph,
            "MATCH (p:person {name: 'marko'}) RETURN p {.age, .name} AS m",
            "m"
        ),
        [["age", "name"]]
    );
    assert_eq!(
        key_orders(
            &graph,
            "MATCH (p:person {name: 'marko'}) RETURN p {k: 1, .*, z: 2} AS m",
            "m"
        ),
        [["k", "name", "age", "z"]]
    );
    // E12: a key written twice keeps its first place and takes the later value.
    let query = "MATCH (p:person {name: 'marko'}) RETURN p {.name, .age, name: 'x', age: p.age + 1} \
                 AS m";
    assert_eq!(key_orders(&graph, query, "m"), [["name", "age"]]);
    assert_eq!(table(&graph, query, &["m"]), ["{age: i:30, name: 'x'}"]);
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person {name: 'marko'}) RETURN p {age: 0, .*} AS m",
            &["m"]
        ),
        ["{age: i:29, name: 'marko'}"]
    );
}

/// A `NULL` target gives `NULL`, where the record constructor gives a map of `NULL`s; a missing
/// property keeps its key.
#[test]
fn a_null_target_gives_null() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WHERE s.lang = 'java' \
             RETURN p.name AS n, s {.name} AS m, {name: s.name} AS r",
            &["n", "m", "r"]
        ),
        sorted(&[
            "'josh'|{name: 'lop'}|{name: 'lop'}",
            "'josh'|{name: 'ripple'}|{name: 'ripple'}",
            "'marko'|{name: 'lop'}|{name: 'lop'}",
            "'peter'|{name: 'lop'}|{name: 'lop'}",
            "'vadas'|null|{name: null}",
        ])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (s:software) RETURN s {.name, .age} AS m",
            &["m"]
        ),
        sorted(&["{age: null, name: 'lop'}", "{age: null, name: 'ripple'}"])
    );
    // A map target, and a map built by a projection read again.
    assert_eq!(
        table(
            &graph,
            "LET r = {a: 1, b: 'x'} RETURN r {.b, .c} AS m, r {.*}.a AS a",
            &["m", "a"]
        ),
        ["{b: 'x', c: null}|i:1"]
    );
}

#[test]
fn a_projection_of_anything_but_an_element_or_a_map_is_a_data_error() {
    let graph = modern();
    for query in [
        "MATCH (p:person) LET a = p.age RETURN a {.x} AS m",
        "MATCH q = (p:person)-->() RETURN q {.x} AS m",
        "MATCH (p:person)-[e:knows]->{1,2}(f) RETURN e {.weight} AS m",
    ] {
        let text = failure(&graph, query);
        assert!(
            text.contains("a map projection reads a node, an edge or a map"),
            "{query}: {text}"
        );
        assert!(text.contains("22G03"), "{query}: {text}");
    }
}

#[test]
fn projections_follow_order_by_and_aggregation() {
    let graph = modern();
    // An ORDER BY key reading a projection of a column alias and of a variable alias.
    let names: Vec<String> = rows(
        &graph,
        "MATCH (p:person) RETURN p AS q, p.age AS a ORDER BY q {a}.a DESC",
    )
    .iter()
    .map(|row| norm(column(row, "a")))
    .collect();
    assert_eq!(names, ["i:35", "i:32", "i:29", "i:27"]);
    // A projection beside an aggregate reads grouping keys.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person)-[:created]->(s) RETURN p, p {.name, made: count(s)} AS m",
            &["m"]
        ),
        sorted(&[
            "{made: i:1, name: 'marko'}",
            "{made: i:1, name: 'peter'}",
            "{made: i:2, name: 'josh'}",
        ])
    );
}

#[test]
fn a_projection_lowers_into_the_project_step_alone() {
    let graph = modern();
    assert_eq!(
        steps(&profile(
            &graph,
            "MATCH (p:person) RETURN p {.name, .age} AS m"
        )),
        ["v(labels: [\"person\"])", "gql:project(\"m\")"]
    );
    assert_eq!(
        steps(&profile(
            &graph,
            "MATCH (p:person) RETURN [x IN [1, 2] WHERE x > 1 | x + p.age] AS l"
        )),
        ["v(labels: [\"person\"])", "gql:project(\"l\")"]
    );
}

// ----- list comprehensions -----

#[test]
fn a_comprehension_keeps_items_whose_condition_is_true() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "RETURN [x IN [1, null, 3] WHERE x > 1] AS a, [x IN [true, false, null] WHERE x] AS b, \
             [x IN [1, null] | x IS NULL] AS c, [x IN [3, 1, 2]] AS d",
            &["a", "b", "c", "d"]
        ),
        ["[i:3]|[true]|[false, true]|[i:3, i:1, i:2]"]
    );
    // A NULL list gives NULL; an empty list an empty list.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person {name: 'marko'}) RETURN [x IN p.missing | x] AS a, [x IN [] | x] AS b",
            &["a", "b"]
        ),
        ["null|[]"]
    );
}

#[test]
fn a_comprehension_over_anything_but_a_list_is_a_data_error() {
    let graph = modern();
    for query in [
        "RETURN [x IN 5 | x] AS l",
        "MATCH (p:person) RETURN [x IN p | x] AS l",
        "MATCH (p:person) RETURN [x IN p.name | x] AS l",
    ] {
        let text = failure(&graph, query);
        assert!(
            text.contains("a list comprehension iterates over a list"),
            "{query}: {text}"
        );
        assert!(text.contains("22G03"), "{query}: {text}");
    }
}

#[test]
fn comprehensions_nest_and_read_the_row() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WHERE p.age < 30 RETURN p.name AS n, \
             [x IN [1, 2] | [y IN [10] | x + y + p.age]] AS l",
            &["n", "l"]
        ),
        sorted(&["'marko'|[[i:40], [i:41]]", "'vadas'|[[i:38], [i:39]]"])
    );
    // An aggregate in the list part.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN size([x IN collect_list(p.age) WHERE x > 30]) AS n",
            &["n"]
        ),
        ["i:2"]
    );
    // A subquery in the WHERE part, read per row (the variable is not read inside it).
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, \
             [x IN [1, 2] WHERE EXISTS { MATCH (p)-[:created]->() } | x] AS l",
            &["n", "l"]
        ),
        sorted(&[
            "'josh'|[i:1, i:2]",
            "'marko'|[i:1, i:2]",
            "'peter'|[i:1, i:2]",
            "'vadas'|[]",
        ])
    );
    // The same name in two comprehensions side by side.
    assert_eq!(
        table(
            &graph,
            "RETURN [x IN [1] | x] AS a, [x IN [2] | x * 2] AS b",
            &["a", "b"]
        ),
        ["[i:1]|[i:4]"]
    );
}

#[test]
fn comprehension_variables_shadow_nothing_and_stay_inside() {
    let graph = modern();
    for (query, message) in [
        (
            "MATCH (p:person) RETURN [p IN [1] | p] AS l",
            "the comprehension variable `p` shadows the variable `p` in scope",
        ),
        (
            "RETURN [x IN [1] | x] AS l ORDER BY x",
            "the variable `x` is not defined",
        ),
        (
            "MATCH (p:person) RETURN [x IN [1] WHERE count(*) > 0] AS l",
            "cannot be used inside the WHERE or | part of a list comprehension",
        ),
    ] {
        let text = failure(&graph, query);
        assert!(text.contains(message), "{query}: {text}");
        assert!(text.contains("42002"), "{query}: {text}");
    }
    let text = failure(
        &graph,
        "MATCH (p:person) RETURN [x IN [1] WHERE EXISTS { MATCH (p)-->(q) WHERE q.age = x }] AS l",
    );
    assert!(
        text.contains("reading the comprehension variable `x` inside a subquery is not yet"),
        "{text}"
    );
}

/// The deepest nesting the parser accepts runs on a test thread's stack (debug builds
/// included): comprehensions and projections nest one evaluator level each.
#[test]
fn the_deepest_accepted_nesting_runs() {
    let graph = modern();
    let mut ran = 0;
    for depth in (1..=80).rev() {
        let mut comprehension = "1".to_string();
        let mut projection = "p.age".to_string();
        for i in 0..depth {
            comprehension = format!("[x{i} IN [1] WHERE x{i} > 0 | {comprehension}]");
            projection = format!("p {{.name, k: {projection}}}");
        }
        let query = format!(
            "MATCH (p:person {{name: 'marko'}}) RETURN size({comprehension}) AS c, \
             {projection} AS m"
        );
        match graph.read().traversal().gql(&query).to_list() {
            Ok(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(norm(column(&rows[0], "c")), "i:1");
                ran = depth;
                break;
            }
            Err(error) => match error.root_cause() {
                TraverserError::Gql { gqlstatus, .. } if gqlstatus == "54001" => {}
                other => panic!("depth {depth}: {other}"),
            },
        }
    }
    assert!(ran > 30, "nothing ran below the nesting bound");
}

// ----- pattern comprehensions (2c.T3) -----

/// A `NULL` import (an `OPTIONAL MATCH` that did not match) matches nothing: the list is empty,
/// never `NULL`.
#[test]
fn a_null_import_gives_an_empty_list() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) OPTIONAL MATCH (p)-[:knows]->(f) \
             RETURN p.name AS n, f.name AS f, [(f)-[:created]->(s) | s.name] AS l",
            &["n", "f", "l"],
        ),
        sorted(&[
            "'josh'|null|[]",
            "'marko'|'josh'|['ripple', 'lop']",
            "'marko'|'vadas'|[]",
            "'peter'|null|[]",
            "'vadas'|null|[]",
        ])
    );
}

#[test]
fn pattern_comprehensions_order_rows_and_group() {
    let graph = modern();
    // ORDER BY a key holding one.
    let names: Vec<String> = rows(
        &graph,
        "MATCH (p:person) RETURN p.name AS n ORDER BY size([(p)-->(b) | b]) DESC, n",
    )
    .iter()
    .map(|row| norm(column(row, "n")))
    .collect();
    assert_eq!(names, ["'marko'", "'josh'", "'peter'", "'vadas'"]);
    // A list as a grouping key beside an aggregate.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN [(p)-[:created]->(s) | s.lang] AS l, count(*) AS c",
            &["l", "c"],
        ),
        sorted(&["['java', 'java']|i:1", "['java']|i:2", "[]|i:1"])
    );
    // Beside an aggregate it imports only grouping keys.
    let text = failure(
        &graph,
        "MATCH (p:person)-->(b) RETURN p.name AS n, count(*) + size([(b)-->(x) | x]) AS c",
    );
    assert!(text.contains("grouping key"), "{text}");
}

#[test]
fn pattern_comprehension_paths_and_elements_stay_live() {
    let graph = modern();
    let rows = rows(
        &graph,
        "MATCH (p:person {name: 'marko'}) RETURN [q = (p)-->(x) WHERE x.age > 30 | q] AS ps, \
         [(p)-[:created]->(s) | s] AS ss",
    );
    assert_eq!(rows.len(), 1);
    match column(&rows[0], "ps") {
        ElementProperty::Array(paths) => assert_eq!(paths.len(), 1, "{paths:?}"),
        other => panic!("{other:?}"),
    }
    match column(&rows[0], "ss") {
        ElementProperty::Array(items) => {
            assert!(
                matches!(items.as_slice(), [ElementProperty::Vertex(_)]),
                "{items:?}"
            )
        }
        other => panic!("{other:?}"),
    }
}

/// Bulk: merged rows and merged child results give the same lists as the unmerged run.
#[test]
fn pattern_comprehensions_keep_bulk_semantics() {
    let graph = modern();
    for query in [
        "MATCH ()-->(s:software) RETURN s.name AS n, [(s)<-[:created]-(y) | y.name] AS l",
        "MATCH (s:software) RETURN s.name AS n, [(s)<--()-->(x) | x.name] AS l",
    ] {
        let run = |merge: bool| {
            let mut out: Vec<String> = graph
                .read()
                .traversal()
                .with("bulk.merge", merge)
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"))
                .iter()
                .map(|row| {
                    let mut items: Vec<String> = match column(row, "l") {
                        ElementProperty::Array(items) => items.iter().map(norm).collect(),
                        other => panic!("{other:?}"),
                    };
                    items.sort();
                    format!("{}|{}", norm(column(row, "n")), items.join(","))
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(run(true), run(false), "{query}");
    }
}

#[test]
fn a_pattern_comprehension_is_a_child_ending_in_gql_value() {
    let graph = modern();
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN p.name AS n, [(p)-[:knows]->(f) | f.name] AS l",
    );
    assert_eq!(
        steps(&text),
        [
            r#"v(labels: ["person"])"#,
            r#"gql:project("n", "l")"#,
            r#"\> out("knows")"#,
            r#"\> gql:value("f.name")"#,
        ],
        "{text}"
    );
    // An import read away from the current element is labelled for the child.
    let text = profile(
        &graph,
        "MATCH (p:person)-[:knows]->(f) RETURN [(f)-[:created]->(s) | p.name + s.name] AS l",
    );
    assert!(text.contains(r#"as("p")"#), "{text}");
    assert!(text.contains(r#"gql:value("p.name + s.name")"#), "{text}");
}

#[test]
fn a_pattern_comprehension_obeys_the_limits() {
    let graph = modern();
    let error = graph
        .read()
        .traversal()
        .with("traversal.max_materialized_bytes", 64)
        .gql("MATCH (p:person {name: 'marko'}) RETURN [(p)-->(x) | x.name] AS l")
        .to_list()
        .unwrap_err();
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::ResourceLimitExceeded { limit, .. }
                if limit == "traversal.max_materialized_bytes"
        ),
        "{error}"
    );
    // A comprehension the evaluation never reaches does not run: its unbounded quantifier
    // never meets `repeat.max_loops`; reached, the child obeys it (D14: an error, never a cut
    // list) in every host.
    let lock = graph.read();
    let body = "[(p)-[:knows]->*(q) | q.name]";
    let rows = lock
        .traversal()
        .with("repeat.max_loops", 1)
        .gql(&format!(
            "MATCH (p:person) RETURN CASE WHEN p.age > 100 THEN {body} END AS l"
        ))
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows.len(), 4);
    for query in [
        format!("MATCH (p:person) WHERE size({body}) > 5 RETURN p.name AS n"),
        format!("MATCH (p:person) RETURN {body} AS x"),
        format!("MATCH (p:person) RETURN p.name AS n ORDER BY size({body})"),
        format!("MATCH (p:person) RETURN collect_list({body}) AS c"),
        format!("MATCH (p:person) FOR x IN {body} RETURN x"),
        format!("MATCH (p:person) RETURN [(p)-->(r) | {body}] AS x"),
    ] {
        let error = lock
            .traversal()
            .with("repeat.max_loops", 1)
            .gql(&query)
            .to_list()
            .expect_err(&query);
        assert!(
            matches!(
                error.root_cause(),
                TraverserError::RepeatLimitExceeded { max_loops: 1, .. }
            ),
            "{query}: {error}"
        );
    }
}

#[test]
fn pattern_comprehension_scope_errors() {
    let graph = modern();
    // Its names stay inside.
    let text = failure(&graph, "MATCH (a) RETURN [(a)-->(c) | c] AS l, c");
    assert!(text.contains("`c`"), "{text}");
    // No aggregate in the `|` part.
    let text = failure(&graph, "MATCH (a) RETURN [(a)-->(c) | count(c)] AS l");
    assert!(text.contains("pattern comprehension"), "{text}");
    // A list comprehension variable is not readable inside it yet.
    let text = failure(&graph, "MATCH (a) RETURN [x IN [1] | [(a)-->(c) | x]] AS l");
    assert!(text.contains("not yet"), "{text}");
}

/// The list a pattern comprehension collects is checked against the memory budget before it is
/// built (the engine's estimate in this test binary: the graph's footprint plus what the query
/// holds and is about to build): the host step fails with `memory.limit`, never a cut list; the
/// same query runs without the budget.
#[test]
fn a_pattern_comprehension_list_obeys_the_memory_budget() {
    let graph = modern();
    let footprint = graph.read().memory_usage().unwrap().total_bytes();
    let query = "RETURN size([(a)--(b)--(c)--(d) | [a, b, c, d]]) AS n";
    let lock = graph.read();
    let error = lock
        .traversal()
        .with("memory.limit", (footprint + 2_000) as i64)
        .gql(query)
        .to_list()
        .expect_err("over the budget");
    match error.root_cause() {
        TraverserError::ResourceLimitExceeded { limit, .. } => {
            assert_eq!(limit, "memory.limit", "{error}")
        }
        other => panic!("expected the memory budget, got {other}"),
    }
    // The host step (`gql:project`) refused the list, not a step of the child.
    let mut failed = Vec::new();
    let mut current: &TraverserError = &error;
    while let TraverserError::StepFailed {
        step_name, source, ..
    } = current
    {
        failed.push(step_name.clone());
        current = source;
    }
    assert_eq!(
        failed.last().map(String::as_str),
        Some("gql:project(\"n\")"),
        "{failed:?}"
    );
    let rows = lock
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows.len(), 1);
    // As many items as the pattern has matches (one path pattern: the same trail).
    let count = lock
        .traversal()
        .gql("MATCH (a)--(b)--(c)--(d) RETURN count(*) AS n")
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(column(&rows[0], "n"), column(&count[0], "n"));
    assert_ne!(column(&rows[0], "n"), &ElementProperty::from(0));
}
