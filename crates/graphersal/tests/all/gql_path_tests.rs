//! GQL stage 1b path variables (feature `gql`): `p = (a)-[e]->(b)`, `RETURN p`,
//! `path_length(p)` / Cypher `length(p)`. The path is assembled from the path labels of the
//! pattern's elements (plan mechanism 6, decision C11), never from `path()`: differential tests
//! pair queries on the modern graph with a hand-written `path()` twin (the planner may start the
//! walk anywhere in the pattern, the value is always in text order), and the plan records labels
//! only, never a full path.
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

fn profile(graph: &Arc<Graph>, query: &str) -> String {
    graph
        .read()
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .to_string()
}

fn column<'r>(row: &'r ElementProperty, key: &str) -> &'r ElementProperty {
    match row {
        ElementProperty::Object(map) => map
            .get(key)
            .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
        other => panic!("not a row map: {other:?}"),
    }
}

/// A value as text: a vertex by its name, an edge as `-label->`, a path or list as its items
/// joined by spaces, `null`, a string, an integer, a boolean.
fn text(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => s.to_string(),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => format!("{}", n.as_i64().unwrap_or_default()),
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(text).collect();
            format!("[{}]", items.join(" "))
        }
        ElementProperty::Vertex(vertex) => match vertex.properties().get("name") {
            Some(ElementProperty::String(name)) => name.to_string(),
            _ => format!("v{}", vertex.id()),
        },
        ElementProperty::Edge(edge) => format!("-{}->", edge.label().unwrap_or_default()),
        other => format!("{other:?}"),
    }
}

/// The rows as `a|b|..` texts of `keys`, sorted (a multiset, design D18).
fn table(rows: &[ElementProperty], keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|row| {
            keys.iter()
                .map(|k| text(column(row, k)))
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

#[test]
fn return_p_is_the_path_in_text_order() {
    let graph = modern();
    assert_eq!(
        table(
            &rows(&graph, "MATCH p = (a:person)-[:knows]->(b) RETURN p"),
            &["p"]
        ),
        sorted(&["[marko -knows-> vadas]", "[marko -knows-> josh]"])
    );
    // Written right to left: the path follows the text, not the edge direction.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (s {name: 'ripple'})<-[:created]-(x) RETURN p"
            ),
            &["p"]
        ),
        sorted(&["[ripple -created-> josh]"])
    );
    // The planner starts at the selective node at the end; the path still starts at `a`.
    let query = "MATCH p = (a)-[:knows]->(b)-[:created]->(c:software {name: 'ripple'}) RETURN p";
    let plan = profile(&graph, query);
    assert!(
        plan.lines()
            .find(|l| l.starts_with("v("))
            .is_some_and(|l| l.contains("software")),
        "{plan}"
    );
    assert_eq!(
        table(&rows(&graph, query), &["p"]),
        sorted(&["[marko -knows-> josh -created-> ripple]"])
    );
    // Two paths of one clause, and a single-node path.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a {name: 'josh'})-[:created]->(s), q = (a) RETURN p, q"
            ),
            &["p", "q"]
        ),
        sorted(&[
            "[josh -created-> ripple]|[josh]",
            "[josh -created-> lop]|[josh]"
        ])
    );
}

#[test]
fn quantified_groups_contribute_their_edges_and_inner_vertices() {
    let graph = modern();
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a {name: 'marko'})-[e]->{0,2}(b) RETURN p, path_length(p) AS n, \
                 length(p) AS l"
            ),
            &["p", "n", "l"]
        ),
        sorted(&[
            "[marko]|0|0",
            "[marko -knows-> vadas]|1|1",
            "[marko -knows-> josh]|1|1",
            "[marko -created-> lop]|1|1",
            "[marko -knows-> josh -created-> ripple]|2|2",
            "[marko -knows-> josh -created-> lop]|2|2",
        ])
    );
    // Walked from the far end (the `software` label is the better start): text order kept.
    let query = "MATCH p = (a)-[e]->{2}(b:software) RETURN p, e";
    let plan = profile(&graph, query);
    assert!(plan.contains("in_e()") && plan.contains("#gv"), "{plan}");
    assert_eq!(
        table(&rows(&graph, query), &["p", "e"]),
        sorted(&[
            "[marko -knows-> josh -created-> ripple]|[-knows-> -created->]",
            "[marko -knows-> josh -created-> lop]|[-knows-> -created->]",
        ])
    );
    // A group between plain edges, either direction on the knows tree.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (s {name: 'lop'})<-[:created]-(x)-[:knows]-{1,2}(y) RETURN p"
            ),
            &["p"]
        ),
        sorted(&[
            "[lop -created-> marko -knows-> vadas]",
            "[lop -created-> marko -knows-> josh]",
            "[lop -created-> josh -knows-> marko]",
            "[lop -created-> josh -knows-> marko -knows-> vadas]",
        ])
    );
    // `{0}`: the zero-length path, the node once.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a {name: 'vadas'})-[e]->{0}(b) RETURN p, e"
            ),
            &["p", "e"]
        ),
        sorted(&["[vadas]|[]"])
    );
}

#[test]
fn path_values_in_expressions_and_result_operations() {
    let graph = modern();
    // In the WHERE of its own MATCH, placed once every element is bound.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a)-[*1..2]->(b) WHERE path_length(p) = 2 RETURN p"
            ),
            &["p"]
        ),
        sorted(&[
            "[marko -knows-> josh -created-> ripple]",
            "[marko -knows-> josh -created-> lop]",
        ])
    );
    // Equality is element-wise.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a {name: 'josh'})-[:created]->(b), q = (a)-[:created]->(c) \
                 RETURN b.name AS b, c.name AS c, p = q AS same"
            ),
            &["b", "c", "same"]
        ),
        sorted(&["lop|ripple|false", "ripple|lop|false"])
    );
    // DISTINCT, ORDER BY, aggregation and grouping over paths.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a {name: 'marko'})-->(b)-->(c) RETURN DISTINCT p ORDER BY p"
            ),
            &["p"]
        ),
        sorted(&[
            "[marko -knows-> josh -created-> ripple]",
            "[marko -knows-> josh -created-> lop]",
        ])
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a {name: 'marko'})-[:knows]->(b) RETURN count(p) AS n, \
                 collect_list(path_length(p)) AS ls"
            ),
            &["n", "ls"]
        ),
        sorted(&["2|[1 1]"])
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a:person)-[:created]->(s) RETURN p, count(*) AS n"
            ),
            &["p", "n"]
        ),
        sorted(&[
            "[marko -created-> lop]|1",
            "[josh -created-> ripple]|1",
            "[josh -created-> lop]|1",
            "[peter -created-> lop]|1",
        ])
    );
    // Through a materializing WITH (a value column), and a pattern continuing from a WITH
    // column inside a path.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH p = (a:person)-[:knows]->(b) WITH p ORDER BY path_length(p) LIMIT 5 \
                 RETURN p"
            ),
            &["p"]
        ),
        sorted(&["[marko -knows-> vadas]", "[marko -knows-> josh]"])
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a:person) WITH a ORDER BY a.name LIMIT 1 MATCH p = (a)-->(b) RETURN p"
            ),
            &["p"]
        ),
        sorted(&["[josh -created-> ripple]", "[josh -created-> lop]"])
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (x)-[e:created]->(s {name: 'ripple'}) WITH e LIMIT 1 \
                 MATCH p = (a)-[e]->(b) RETURN p"
            ),
            &["p"]
        ),
        sorted(&["[josh -created-> ripple]"])
    );
}

#[test]
fn a_path_of_an_optional_match_not_matched_is_null() {
    let graph = modern();
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a:person) OPTIONAL MATCH p = (a)-[:created]->(s) RETURN a.name AS a, p"
            ),
            &["a", "p"]
        ),
        sorted(&[
            "marko|[marko -created-> lop]",
            "josh|[josh -created-> ripple]",
            "josh|[josh -created-> lop]",
            "peter|[peter -created-> lop]",
            "vadas|null",
        ])
    );
    // Every element bound before the part: its WHERE decides, the anchor tells.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a {name: 'marko'})-[e:knows]->(b) OPTIONAL MATCH p = (a)-[e]->(b) \
                 WHERE b.age > 30 RETURN b.name AS b, p"
            ),
            &["b", "p"]
        ),
        sorted(&["josh|[marko -knows-> josh]", "vadas|null"])
    );
    // A part without a new node: a zero-length match is a path, a part not matched NULL.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a:person) OPTIONAL MATCH p = (a)-[e:knows]->{0,1}(a) WHERE a.age > 30 \
                 RETURN a.name AS a, p"
            ),
            &["a", "p"]
        ),
        sorted(&["josh|[josh]", "marko|null", "peter|[peter]", "vadas|null"])
    );
    // A leading OPTIONAL MATCH that finds nothing: one row, the path NULL.
    assert_eq!(
        table(
            &rows(
                &graph,
                "OPTIONAL MATCH p = (a {name: 'nobody'})-->(b) MATCH (c {name: 'lop'}) \
                 RETURN c.name AS c, p"
            ),
            &["c", "p"]
        ),
        sorted(&["lop|null"])
    );
}

#[test]
fn the_plan_records_labels_only_never_a_full_path() {
    let graph = modern();
    for query in [
        "MATCH p = (a:person)-[:knows]->(b) RETURN p",
        "MATCH p = (a)-[:knows]->(b)-[:created]->(c {name: 'ripple'}) RETURN p",
        "MATCH p = (a)-[e]->{1,3}(b:software) RETURN p",
        "MATCH p = (a)-[:knows]-{1,2}(b) RETURN path_length(p) AS n",
        "MATCH (a:person) OPTIONAL MATCH p = (a)-[:created]->{1,2}(s) RETURN p",
        "MATCH (a:person) WITH a ORDER BY a.name LIMIT 1 MATCH p = (a)-->(b) RETURN p",
    ] {
        let plan = profile(&graph, query);
        assert!(plan.contains("[path: labels("), "{query}: {plan}");
        assert!(!plan.contains("full"), "{query}: {plan}");
        assert!(!plan.contains("path()"), "{query}: {plan}");
    }
    // An unread path variable labels nothing.
    let plan = profile(&graph, "MATCH p = (a:person)-[:knows]->(b) RETURN b.name");
    assert!(!plan.contains("[path:"), "{plan}");
}

/// D21 reverses C12: `nodes(p)` / `relationships(p)` are no longer refused before the run
/// (stage 1c T1; T5 implements them and T6 asserts their rows).
#[test]
fn nodes_and_relationships_are_no_longer_refused() {
    let graph = modern();
    for function in ["nodes", "relationships"] {
        let query = format!("MATCH p = (a)-->(b) RETURN {function}(p)");
        if let Err(error) = graph.read().traversal().gql(&query).to_list() {
            let text = error.to_string();
            assert!(!text.contains("gql:error"), "{query}: {text}");
            assert!(!text.contains("over a path"), "{query}: {text}");
        }
    }
}

#[cfg(feature = "script")]
mod differential {
    use std::sync::Arc;

    use crate::script_host::{ScriptGraphTraversalSource, engine, eval_value, graph_scope};
    use graphersal::exec::RenderOptions;
    use graphersal::prelude::*;
    use graphersal::schema::VisualizeFormat;

    /// Runs the GQL query and its Gremlin twin on `graph`; compares the rows as multisets.
    fn same(graph: &Arc<Graph>, query: &str, twin: &str) {
        let lock = graph.read();
        let left = lock
            .traversal()
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        drop(lock);
        let engine = engine();
        let mut scope = graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<ScriptGraphTraversalSource>(&mut scope, twin)
            .unwrap_or_else(|e| panic!("`{twin}` failed to evaluate: {e}"));
        let lock = graph.read();
        let right = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{twin}: {e}"));
        let text = |rows: &[ElementProperty]| -> Vec<String> {
            let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
            out.sort();
            out
        };
        assert_eq!(text(&left), text(&right), "\nGQL:  {query}\ntwin: {twin}");
        assert!(!left.is_empty(), "{query}: no rows to compare");
    }

    /// On the modern graph every directed walk is a trail and the knows edges form a tree, so
    /// `path()` over the same walk is the twin of a path variable.
    #[test]
    fn path_variables_match_their_path_twins() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let cases: &[(&str, &str)] = &[
            (
                "MATCH p = (a:person)-[:knows]->(b) RETURN p",
                r#"g.V().hasLabel("person").outE("knows").inV().project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (a)-[:knows]->(b)-[:created]->(c) RETURN p",
                r#"g.V().outE("knows").inV().outE("created").inV().project("p").by(__.path())"#,
            ),
            // The planner starts at `c`; the twin walks the text order.
            (
                "MATCH p = (a)-[:knows]->(b)-[:created]->(c:software {name: 'ripple'}) RETURN p",
                r#"g.V().outE("knows").inV().outE("created").inV().has("name", "ripple").project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (s:software)<-[:created]-(x) RETURN p",
                r#"g.V().hasLabel("software").inE("created").outV().project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (a)-[e:knows]-(b) RETURN p",
                r#"g.V().bothE("knows").otherV().project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (a {name: 'marko'})-[*0..2]->(b) RETURN p",
                r#"g.V().has("name", "marko").emit().repeat(__.outE().inV()).times(2).project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (a)-[:knows|created]->{2}(b:software) RETURN p",
                r#"g.V().repeat(__.outE("knows", "created").inV()).times(2).hasLabel("software").project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (a {name: 'vadas'})-[:knows]-{1,2}(b) RETURN p",
                r#"g.V().has("name", "vadas").repeat(__.bothE("knows").otherV().simplePath()).emit().times(2).project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (a {name: 'josh'})<-[:knows]-(m)-[:created]->{1,2}(s) RETURN p",
                r#"g.V().has("name", "josh").inE("knows").outV().repeat(__.outE("created").inV()).emit().times(2).project("p").by(__.path())"#,
            ),
            (
                "MATCH p = (a:person)-[:created]->(s) RETURN path_length(p) AS n, length(p) AS l",
                r#"g.V().hasLabel("person").out("created").project("n", "l").by(__.constant(1)).by(__.constant(1))"#,
            ),
        ];
        for (query, twin) in cases {
            same(&graph, query, twin);
        }
    }

    /// A row holding a path renders as JSON like the Gremlin path value: the elements in order.
    #[test]
    fn json_rendering_of_a_row_with_a_path() {
        let render = |script: &str| {
            let value = eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|e| panic!("{script}: {e}"));
            crate::script_host::try_render_value(
                value,
                VisualizeFormat::Auto,
                &RenderOptions::unlimited(),
            )
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .text
        };
        let gql = render(
            r#"g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b {name: 'vadas'}) RETURN p, a.name AS n").to_json()"#,
        );
        let twin = render(
            r#"g.V().has("name", "marko").as("a").outE("knows").inV().has("name", "vadas").project("p", "n").by(__.path()).by(__.select("a").values("name")).to_json()"#,
        );
        assert_eq!(gql, twin);
        assert!(gql.contains("\"p\": ["), "{gql}");
        let marko = gql.find("\"marko\"").unwrap_or(usize::MAX);
        let knows = gql.find("\"knows\"").unwrap_or(usize::MAX);
        let vadas = gql.find("\"vadas\"").unwrap_or(usize::MAX);
        assert!(
            marko < knows && knows < vadas && vadas < usize::MAX,
            "{gql}"
        );
    }
}
