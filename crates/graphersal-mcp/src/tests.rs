//! The unit tests moved from the dev server's `src/server/mcp.rs` (unchanged in substance),
//! and the tests of the transport entry point ([`crate::Mcp::handle`]) without HTTP.

use serde_json::{Value, json};

use crate::answer::*;
use crate::catalog::*;
use crate::protocol::*;
use crate::resources::*;
use crate::tools::*;

/// A vector fill above the threshold is refused without `confirm_lock` (the session's
/// warning text plus the instruction), applied with it; a small fill is applied with the
/// warning as a note; a schema without a fill passes silently.
#[test]
fn a_large_fill_needs_confirm_lock() {
    let large = json!({
        "fill": [{"needs_confirmation": true}],
        "fill_warning": "Applying this schema fills vectors: 20,000 elements."
    });
    match fill_gate(&large, false) {
        FillGate::Refused(text) => {
            assert!(
                text.starts_with("Nothing was applied.\nApplying this schema"),
                "{text}"
            );
            assert!(text.ends_with("Repeat with confirm_lock: true."), "{text}");
        }
        FillGate::Note(note) => panic!("applied: {note}"),
    }
    assert!(matches!(fill_gate(&large, true), FillGate::Note(n) if n.contains("20,000")));
    let small = json!({
        "fill": [{"needs_confirmation": false}],
        "fill_warning": "Applying this schema fills vectors: 3 elements."
    });
    assert!(matches!(fill_gate(&small, false), FillGate::Note(n) if n.contains("3 elements")));
    assert!(matches!(fill_gate(&json!({"fill": []}), false), FillGate::Note(n) if n.is_empty()));
}

/// A GQL example of the playground (`lang: "gql"`, plain GQL with its `params`) is listed as
/// the `g.gql(..)` call the `query` tool runs; a Rhai example as it is.
#[test]
fn examples_list_a_gql_example_as_its_call() {
    let examples = serde_json::json!([
        {"group": "GQL", "graph": "modern", "title": "Older", "lang": "gql",
         "script": "MATCH (p:person) WHERE p.age > $min\nRETURN p.name AS name", "params": {"min": 30}},
        {"group": "GQL", "graph": "modern", "title": "Count", "script": "g.v().count()"},
    ]);
    let text = examples_text(&examples);
    assert!(
        text.contains(
            "```\ng.gql(\"MATCH (p:person) WHERE p.age > $min\\nRETURN p.name AS name\", #{min: 30})\n```"
        ),
        "{text}"
    );
    assert!(text.contains("```\ng.v().count()\n```"), "{text}");
}

/// The gql tool writes like `query` (W41): offered with `query`'s annotations, and as a
/// reading tool under `--mcp-read-only`, where the read-only authorizer refuses writes.
#[test]
fn gql_writes_unless_read_only() {
    let query = all_tools().into_iter().find(|t| t.name == "query").unwrap();
    for read_only in [false, true] {
        let tool = tools(read_only)
            .into_iter()
            .find(|t| t.name == "gql")
            .expect("gql listed");
        let listing = tool.listing(read_only);
        for hint in ["readOnlyHint", "destructiveHint", "idempotentHint"] {
            assert_eq!(
                listing["annotations"][hint],
                query.listing(read_only)["annotations"][hint],
                "{hint}"
            );
        }
        assert_eq!(
            listing["annotations"]["readOnlyHint"], read_only,
            "{listing}"
        );
        assert_eq!(
            listing["annotations"]["destructiveHint"], !read_only,
            "{listing}"
        );
        assert_eq!(listing["inputSchema"]["required"], json!(["query"]));
        for key in ["query", "params", "profile", "max_rows"] {
            assert!(
                listing["inputSchema"]["properties"][key].is_object(),
                "{key}"
            );
        }
    }
    for named in [
        "OPTIONAL MATCH",
        "RETURN *",
        "graphersal://examples",
        "\"GQL\"",
    ] {
        assert!(GQL_DESCRIPTION.contains(named), "{named}");
    }
    assert!(INSTRUCTIONS.contains("`gql`"));
    assert!(INSTRUCTIONS.contains("CALL folder.name(args) YIELD col"));
}

/// Stage 3h: run_saved_query names the GQL call of a saved query.
#[test]
fn run_saved_query_names_the_gql_call() {
    let tool = all_tools()
        .into_iter()
        .find(|t| t.name == "run_saved_query")
        .expect("run_saved_query listed");
    assert!(
        tool.description
            .contains("the gql tool calls it as CALL folder.name(args) YIELD col"),
        "{}",
        tool.description
    );
}

/// The gql tool's description lists the stage 1b, 1c, 2a, 2b, 2c, 2d and 2e subset as supported,
/// and the 3a, 3c, 3d and 3e writes (INSERT, CREATE, SET, REMOVE, DELETE, MERGE) as supported, calls GQL experimental
/// and still names what is refused.
#[test]
fn gql_description_lists_stage_1b_and_what_is_still_refused() {
    let (supported, refused) = GQL_DESCRIPTION
        .split_once("Not yet supported")
        .expect("a not-yet list");
    for named in [
        "OPTIONAL MATCH",
        "WITH",
        "LET",
        "FILTER",
        "collect_list",
        "count(*)",
        "DISTINCT",
        "->{1,3}",
        "-[e*1..3]->",
        "p = (a)-->(b)",
        "path_length(p)",
        "ORDER BY",
        "GROUP BY",
        "nodes(p)",
        "relationships(p)",
        "CAST(",
        "toInteger",
        "OrNull",
        "substring",
        "round(",
        "keys",
        "properties",
        "startNode(e)",
        "FOR x IN list",
        "UNWIND",
        "WITH ORDINALITY",
        "$ids",
        "may start with",
        "EXISTS { MATCH",
        "EXISTS { <pattern> }",
        "pattern predicates",
        "never NULL",
        "NOT EXISTS { (p)-[:created]->() }",
        "map projections p {.name",
        "list comprehensions [x IN list WHERE",
        "pattern comprehensions [(p)-[:knows]->(f)",
        "JSON-shaped answers",
        "RETURN p {.name, friends: [(p)-[:knows]->(f) | f.name]}",
        "all(x IN list WHERE cond)",
        "reduce(acc = 0, x IN list | acc + x)",
        "COUNT { <pattern> }",
        "COLLECT { MATCH ... RETURN x }",
        "q1 UNION q2",
        "UNION ALL",
        "q1 NEXT q2",
        "CALL (p) { MATCH ... RETURN ... }",
        "OPTIONAL CALL",
        "top k per group",
        "CALL (p) { MATCH (p)-[e:created]->(s) RETURN s.name AS top ORDER BY e.weight DESC LIMIT 1 }",
        "quantified path patterns",
        "((x)-[:knows]->(y) WHERE x.age < y.age){1,3}",
        "one per iteration",
        "NULL when ? matched nothing",
        "MATCH (a:person) ((x)-[:created]->(s)<-[:created]-(y)){1,2} (b)",
        "INSERT",
        "CREATE",
        "FINISH",
        "INSERT (p)-[:likes]->(f:fruit {name: 'apple'})",
        "SET",
        "REMOVE",
        "SET n += {k: expr}",
        "SET p.age = p.age + 1, p:admin",
        "SET n.k = NULL removes",
        "DELETE",
        "DETACH DELETE",
        "NODETACH",
        "G1001",
        "MATCH (p:person {name: 'marko'}) DETACH DELETE p",
        "DELETE r, n",
        "properties(p) AS props",
        "MERGE",
        "ON CREATE SET",
        "ON MATCH SET",
        "MERGE (c:city {name: 'Berlin'})",
        "MERGE (a)-[e:knows]->(b)",
        "coalesce(p.visits, 0) + 1",
        "created from left to right",
        "22004",
        "CALL folder.name(args) YIELD col",
        "CALL people.adults(30) YIELD name RETURN name ORDER BY name",
        "YIELD * the same",
    ] {
        assert!(supported.contains(named), "{named}");
    }
    for named in [
        "nested inside a quantified path pattern",
        "EXCEPT and INTERSECT",
        "IN TRANSACTIONS",
        "randomUUID",
        "FOREACH",
        "use the query tool",
        "CALL db.labels()",
        "saved query written in Rhai",
    ] {
        assert!(refused.contains(named), "{named}");
    }
    for gone in [
        "without MATCH",
        "UNWIND",
        "EXISTS",
        "comprehension",
        "projection",
        "COUNT { }",
        "COLLECT",
        "UNION",
        "NEXT",
        "reduce()",
        "((a)-->(b)){1,3}",
        "DELETE",
        "MERGE",
        "merge_v",
    ] {
        assert!(!refused.contains(gone), "{gone}");
    }
    assert!(GQL_DESCRIPTION.contains("a failing query changes nothing"));
    assert!(GQL_DESCRIPTION.contains("run writes only when the user asked"));
    assert!(GQL_DESCRIPTION.contains("experimental"));
}

#[test]
fn secrets_targets_and_sentences() {
    assert!(same_secret("abc", "abc"));
    assert!(!same_secret("abd", "abc"));
    assert!(!same_secret("ab", "abc"));
    assert_eq!(view_note(&Value::Null), None);
    let note = view_note(&json!({"commitSeq": 3, "target": "commit 3"})).unwrap();
    assert!(note.starts_with("the served state is a read-only view at commit 3, opened"));
    let note = view_note(&json!({"commitSeq": 3, "target": "mark \"m1\""})).unwrap();
    assert!(note.starts_with("the served state is a read-only view at commit 3 (mark \"m1\")"));
    assert_eq!(
        doc_line("Filters by label.\n\nExample: `g.v().has_label(\"x\")`  returns `[]`"),
        "Filters by label. Example: `g.v().has_label(\"x\")` returns `[]`"
    );
    assert_eq!(doc_line("Only a sentence."), "Only a sentence.");
    assert_eq!(
        json_arg(
            &json!({"schema": "{\"vertices\": {}}"}),
            "schema",
            Some("open")
        )
        .unwrap(),
        r#"{"mode":"open","vertices":{}}"#
    );
    assert!(json_arg(&json!({}), "schema", None).is_err());
    assert!(cut("x".repeat(MAX_TEXT_CHARS + 5)).contains("cut at"));
}

#[test]
fn the_reference_is_compact() {
    assert_eq!(
        compact_doc(
            "Pushes (see the book page Bulk and Barriers). Converts (zero, see the book \
             page Type Conversion and length()). Ends, see the book page X. Example: `a` \
             returns `b`"
        ),
        "Pushes. Converts (zero). Ends. Example: `a` → `b`"
    );
    assert_eq!(
        compact_doc("X. Example: `g.v().to_list()` returns `[]`"),
        "X. Example: `g.v()` → `[]`"
    );
    assert_eq!(
        compact_doc("Sets (see the book page Prop… Example: `x` returns `y`"),
        "Sets… Example: `x` → `y`"
    );
    assert_eq!(
        compact_doc(&format!("{}. Example: `x` returns `y`", "a".repeat(200))),
        format!("{}… Example: `x` → `y`", "a".repeat(149))
    );
    assert_eq!(camel_case("domain_name"), "domainName");
    assert_eq!(
        compact_signatures(&["(arg1: map)", "(arg1, …, arg3)"]),
        "(map)|(any, any, any)"
    );
    assert_eq!(
        compact_signatures(&[
            "()",
            "(arg1: any)",
            "(val: any)",
            "(arg1, arg2)",
            "(arg1, …, arg10)"
        ]),
        "()|(any…)"
    );
    assert_eq!(
        compact_signatures(&["(pattern: string)"]),
        "(pattern: string)"
    );
}

/// Compact JSON keeps strings and the key order; the item split ignores commas and
/// brackets inside strings; `fit` keeps whole items and valid JSON.
#[test]
fn json_is_compacted_split_and_fitted_at_whole_items() {
    let pretty = "[\n  {\n    \"z\": \"a, b ] \\\" {\",\n    \"a\": [1, 2]\n  },\n  3\n]";
    let compact = compact_json(pretty).unwrap();
    assert_eq!(compact, r#"[{"z":"a, b ] \" {","a":[1,2]},3]"#);
    assert_eq!(compact_json("not json"), None);
    let (open, items) = json_items(&compact).unwrap();
    assert_eq!(open, '[');
    assert_eq!(items, vec![r#"{"z":"a, b ] \" {","a":[1,2]}"#, "3"]);
    assert_eq!(
        json_items(r#"{"a":1,"b":"x,y"}"#).unwrap().1,
        vec![r#""a":1"#, r#""b":"x,y""#]
    );
    assert_eq!(json_items("[]").unwrap().1, Vec::<&str>::new());
    assert_eq!(json_items("12"), None);
    assert_eq!(
        json_prefix('[', &["10", "20", "30"], 7),
        ("[10,20]".to_string(), 2)
    );
    assert_eq!(json_prefix('{', &[r#""a":1"#], 3), ("{}".to_string(), 0));

    // A long JSON line keeps its leading whole items, with a note.
    let list: Vec<String> = (0..2000).map(|i| format!("item number {i}")).collect();
    let text = format!("Header line.\n{}", json!(list));
    let fitted = fit(text, 2000);
    assert!(fitted.chars().count() <= 2000, "{fitted}");
    let lines: Vec<&str> = fitted.lines().collect();
    assert_eq!(lines[0], "Header line.");
    let kept: Vec<String> = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(kept[..], list[..kept.len()]);
    assert!(lines[2].starts_with(&format!("note: shown {} of 2000 items", kept.len())));
    // Plain text keeps whole lines.
    let text = (0..500)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let fitted = fit(text, 1000);
    assert!(fitted.chars().count() <= 1000, "{fitted}");
    assert!(fitted.contains("\nline 9\n") && fitted.ends_with("or page with offset)"));
}

#[test]
fn read_only_lists_only_reading_tools() {
    let names = |read_only: bool| -> Vec<&'static str> {
        all_tools()
            .into_iter()
            .filter(|t| !read_only || t.kind == Kind::Read || t.name == "query")
            .map(|t| t.name)
            .collect()
    };
    assert!(names(false).contains(&"set_schema"));
    assert!(!names(true).contains(&"set_schema"));
    assert!(!names(true).contains(&"patch_schema"));
    assert!(names(true).contains(&"query"));
    assert!(names(true).contains(&"list_compressions"));
    assert!(!names(true).contains(&"define_compression"));
    assert!(!names(true).contains(&"drop_compression"));
    assert!(!names(true).contains(&"recompress"));
    assert!(names(false).contains(&"recompress"));
    let query = all_tools().into_iter().find(|t| t.name == "query").unwrap();
    assert_eq!(query.listing(true)["annotations"]["readOnlyHint"], true);
    assert_eq!(query.listing(false)["annotations"]["readOnlyHint"], false);
}
