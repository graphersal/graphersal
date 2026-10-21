//! Malformed-input robustness (CLAUDE.md rule 1): GraphML and GraphSON documents, schema JSON, serialized
//! `ChangeSet` journals, `jpath(...)` strings, `UUID.fromString(...)`, script parameters and GQL
//! query strings (feature `gql`, through the Rust entry and through Rhai).
//!
//! Each input family has a handwritten corpus of adversarial cases plus seeded byte/structure
//! mutations of a valid document. Every case runs through [`guarded`] (own thread,
//! `catch_unwind`, watchdog); a panic or hang fails the test unless it is a known, documented
//! finding with an ignored `BUG:` test in `robustness_bug_tests.rs`. After a successful parse the
//! result is exercised further (traversal, export, schema visualization, replay), because a
//! malformed value that is accepted must not crash a later step either.
//!
//! The mutation budget is small in the normal run; `ROBUSTNESS_INPUT_CASES=<n>` raises it:
//!
//! ```text
//! ROBUSTNESS_INPUT_CASES=20000 cargo test --release --package graphersal \
//!   --features script,io,display,serde --test all robustness_input_tests:: -- --nocapture
//! ```
#![cfg(all(feature = "io", feature = "serde", feature = "script"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use graphersal::changes::ChangeSet;
use graphersal::io::{GraphExport, GraphImport};
use graphersal::prelude::*;
use graphersal::schema::SchemaDiff;

use crate::robustness_fuzz_tests::{Outcome, Rng, guarded};
use crate::script_host::{ScriptLimits, eval_value_with_limits};

const WATCHDOG: Duration = Duration::from_secs(30);

fn budget(default: u64) -> u64 {
    std::env::var("ROBUSTNESS_INPUT_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Collects fatal outcomes (panic or hang) with a short description of the input.
#[derive(Default)]
struct Fatal(Vec<String>);

impl Fatal {
    fn check<T>(&mut self, what: &str, result: Result<T, Outcome>) {
        if let Err(outcome) = result {
            let input: String = what.chars().take(300).collect();
            self.0.push(format!("{outcome:?} on {input:?}"));
        }
    }
    fn assert_empty(self, family: &str) {
        assert!(
            self.0.is_empty(),
            "{family}: {} fatal cases:\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

/// Byte-level mutations of a valid document: truncation, a flipped byte, a duplicated slice and
/// the insertion of an adversarial snippet.
fn mutate(rng: &mut Rng, base: &[u8], snippets: &[&[u8]]) -> Vec<u8> {
    let mut out = base.to_vec();
    for _ in 0..1 + rng.below(3) {
        if out.is_empty() {
            break;
        }
        let at = rng.below(out.len());
        match rng.below(5) {
            0 => out.truncate(at),
            1 => out[at] = *rng.pick(b"<>&\"'/=\0\xff\x80{}[]:,-09eE.\\ "),
            2 => {
                let end = (at + rng.below(64)).min(out.len());
                let slice = out[at..end].to_vec();
                out.splice(at..at, slice);
            }
            3 => {
                let end = (at + rng.below(32)).min(out.len());
                out.drain(at..end);
            }
            _ => {
                let snippet = rng.pick(snippets).to_vec();
                out.splice(at..at, snippet);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// GraphML
// ---------------------------------------------------------------------------------------------

fn modern_graphml(with_schema: bool) -> Vec<u8> {
    let graph = GraphSource::tinkerpop_modern();
    if with_schema {
        let schema = GraphSchema::from_json(
            r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
                "properties": {"name": {"type": "string", "pattern": "^[a-z]+$"},
                               "age": {"type": ["integer", "null"], "minimum": 0}},
                "required": ["name"]}}}}"#,
        )
        .expect("valid schema");
        graph.write().set_schema(schema).expect("modern conforms");
    }
    let mut out = Vec::new();
    graph.export_graphml_writer(&mut out).expect("export");
    out
}

const GRAPHML_HEAD: &str =
    r#"<?xml version="1.0"?><graphml xmlns="http://graphml.graphdrawing.org/xmlns">"#;

fn graphml_doc(body: &str) -> Vec<u8> {
    format!(r#"{GRAPHML_HEAD}{body}</graphml>"#).into_bytes()
}

fn graphml_corpus() -> Vec<Vec<u8>> {
    let deep_nesting = format!(
        r#"{GRAPHML_HEAD}<graph edgedefault="directed"><node id="a"><data key="d">{}x{}</data></node></graph></graphml>"#,
        "<x>".repeat(50_000),
        "</x>".repeat(50_000)
    );
    let deep_graphs = format!(
        "{GRAPHML_HEAD}{}{}</graphml>",
        r#"<graph edgedefault="directed"><node id="n">"#.repeat(5_000),
        "</node></graph>".repeat(5_000)
    );
    let many_attrs = format!(
        r#"{GRAPHML_HEAD}<graph><node id="a" {}/></graph></graphml>"#,
        (0..20_000)
            .map(|i| format!("a{i}=\"{i}\""))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let long_id = format!(
        r#"{GRAPHML_HEAD}<graph><node id="{}"/></graph></graphml>"#,
        "i".repeat(1_000_000)
    );
    let mut cases: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"<".to_vec(),
        b"<?xml".to_vec(),
        b"<graphml".to_vec(),
        b"\xff\xfe\x00<\x00g\x00".to_vec(),
        b"<graphml><graph><node id=\"\xc3\x28\"/></graph></graphml>".to_vec(),
        b"<graphml><graph><node id=\"a\"><data key=\"k\">\xf0\x28\x8c\xbc</data></node></graph></graphml>"
            .to_vec(),
        graphml_doc(""),
        graphml_doc("<graph>"),
        graphml_doc("<graph></graph><graph></graph>"),
        graphml_doc(r#"<graph><node/></graph>"#),
        graphml_doc(r#"<graph><node id=""/></graph>"#),
        graphml_doc(r#"<graph><node id="a"/><node id="a"/></graph>"#),
        graphml_doc(r#"<graph><edge source="a" target="b"/></graph>"#),
        graphml_doc(r#"<graph><node id="a"/><edge source="a"/></graph>"#),
        graphml_doc(r#"<graph><node id="a"/><edge target="a"/></graph>"#),
        graphml_doc(r#"<graph><node id="a"/><edge id="e" source="a" target="a"/><edge id="e" source="a" target="a"/></graph>"#),
        graphml_doc(r#"<graph><edge id="e" source="a" target="b"/><node id="a"/><node id="b"/></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="long"/><graph><node id="a"><data key="k">99999999999999999999999999</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="long"/><graph><node id="a"><data key="k">-9223372036854775809</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="int"/><graph><node id="a"><data key="k">2147483648</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="double"/><graph><node id="a"><data key="k">1e999999</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="double"/><graph><node id="a"><data key="k">NaN</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="float"/><graph><node id="a"><data key="k">-inf</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="boolean"/><graph><node id="a"><data key="k">maybe</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="weird"/><graph><node id="a"><data key="k">1</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="nowhere" attr.name="n" attr.type="long"/><graph><node id="a"><data key="k">1</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="long"/><key id="k" for="node" attr.name="m" attr.type="string"/><graph><node id="a"><data key="k">x</data></node></graph>"#),
        graphml_doc(r#"<key id="k" for="node" attr.name="n" attr.type="long"><default>notanumber</default></key><graph><node id="a"/></graph>"#),
        graphml_doc(r#"<graph><node id="a"><data key="undeclared">1</data></node></graph>"#),
        graphml_doc(r#"<graph><node id="a"><data>1</data></node></graph>"#),
        graphml_doc(r#"<key id="labelV" for="node" attr.name="labelV" attr.type="long"/><graph><node id="a"><data key="labelV">5</data></node></graph>"#),
        graphml_doc(r#"<key id="labelV" for="node" attr.name="labelV" attr.type="string"/><graph><node id="a"><data key="labelV"></data></node></graph>"#),
        graphml_doc(r#"<key id="labelV" for="node" attr.name="labelV" attr.type="string"/><graph><node id="a"><data key="labelV">a::b::::</data></node></graph>"#),
        graphml_doc(r#"<key id="labelE" for="edge" attr.name="labelE" attr.type="string"/><graph><node id="a"/><edge source="a" target="a"><data key="labelE"></data></edge></graph>"#),
        graphml_doc(r#"<graph><node id="&lt;&amp;&gt;"/></graph>"#),
        graphml_doc(r#"<graph><node id="&#0;"/></graph>"#),
        graphml_doc(r#"<graph><node id="&#xFFFFFFFF;"/></graph>"#),
        graphml_doc(r#"<graph><node id="&#x110000;"/></graph>"#),
        graphml_doc(r#"<graph><node id="&unknown;"/></graph>"#),
        graphml_doc(r#"<graph><node id="a"><data key="k">&#55296;</data></node></graph>"#),
        format!(r#"<?xml version="1.0"?><!DOCTYPE g [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">]>{}"#,
            r#"<graphml><graph><node id="&c;"/></graph></graphml>"#).into_bytes(),
        br#"<?xml version="1.0"?><!DOCTYPE g [<!ENTITY x SYSTEM "file:///etc/passwd">]><graphml><graph><node id="&x;"/></graph></graphml>"#.to_vec(),
        graphml_doc(r#"<graph><node id="a"><data key="k"><![CDATA[<node id="b"/>]]></data></node></graph>"#),
        graphml_doc(r#"<graph><node id="a"><data key="k"><![CDATA[unterminated"#),
        graphml_doc(r#"<graph><node id="a"><!-- comment --><?pi x?></node></graph>"#),
        // A legacy graph-level schema value (no longer read): ignored like any graph-level data.
        graphml_doc(r#"<key id="s" for="graph" attr.name="graphersal.schema" attr.type="string"/><graph><data key="s">{not json</data></graph>"#),
        deep_nesting.into_bytes(),
        deep_graphs.into_bytes(),
        many_attrs.into_bytes(),
        long_id.into_bytes(),
    ];
    let modern = modern_graphml(true);
    // Truncated at every 97th byte.
    for cut in (0..modern.len()).step_by(97) {
        cases.push(modern[..cut].to_vec());
    }
    cases
}

const GRAPHML_SNIPPETS: &[&[u8]] = &[
    br#"<node id="1"/>"#,
    br#"<node id="zz"/>"#,
    br#"<edge source="zz" target="1"/>"#,
    br#"<edge id="7" source="1" target="1"/>"#,
    br#"<data key="age">99999999999999999999999</data>"#,
    br#"<data key="age">NaN</data>"#,
    br#"<data key="age"></data>"#,
    br#"<data key="labelV"></data>"#,
    br#"<data key="labelV">a::b</data>"#,
    br#"<key id="age" for="node" attr.name="age" attr.type="string"/>"#,
    br#"<key id="x" for="node" attr.name="x" attr.type="weird"/>"#,
    b"&lt;",
    b"&#0;",
    b"&#xD800;",
    b"&nope;",
    b"<![CDATA[x]]>",
    b"\xc3\x28",
    b"\xff",
    br#"<graph edgedefault="undirected">"#,
    b"</graph>",
    b"</graphml>",
    b"<graphml>",
];

/// Imports `bytes` into a fresh graph and, when accepted, also into a live modern graph (one
/// captured unit), then reads and re-exports the result.
fn exercise_graphml(bytes: Vec<u8>) {
    if let Ok(graph) = GraphSource::from_graphml_reader(std::io::Cursor::new(bytes.clone())) {
        let lock = graph.read();
        let _ = lock.traversal().v(None::<()>).element_map(None).to_list();
        let _ = lock.traversal().e(None::<()>).element_map(None).to_list();
        drop(lock);
        let mut out = Vec::new();
        let _ = graph.export_graphml_writer(&mut out);
        // The re-export of an accepted document must import again.
        if !out.is_empty() {
            let _ = GraphSource::from_graphml_reader(std::io::Cursor::new(out));
        }
    }
    let mut live = GraphSource::tinkerpop_modern();
    let _ = live.import_graphml_reader(std::io::Cursor::new(bytes));
    let _ = live.read().traversal().v(None::<()>).count().next();
}

#[test]
fn malformed_graphml_never_panics() {
    let mut fatal = Fatal::default();
    for case in graphml_corpus() {
        let label = String::from_utf8_lossy(&case).into_owned();
        fatal.check(&label, guarded(WATCHDOG, move || exercise_graphml(case)));
    }
    let bases = [modern_graphml(false), modern_graphml(true)];
    let mut rng = Rng::new(0x6EA9_4D1E);
    for _ in 0..budget(300) {
        let base = rng.pick(&bases).clone();
        let case = mutate(&mut rng, &base, GRAPHML_SNIPPETS);
        let label = String::from_utf8_lossy(&case).into_owned();
        fatal.check(&label, guarded(WATCHDOG, move || exercise_graphml(case)));
    }
    fatal.assert_empty("GraphML");
}

// ---------------------------------------------------------------------------------------------
// GraphSON
// ---------------------------------------------------------------------------------------------

fn modern_graphson() -> Vec<u8> {
    let mut out = Vec::new();
    GraphSource::tinkerpop_modern()
        .export_graphson_writer(&mut out)
        .expect("export");
    out
}

fn crew_graphson() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/tinkerpop/data/tinkerpop-crew.json");
    std::fs::read(path).expect("crew")
}

fn graphson_corpus() -> Vec<Vec<u8>> {
    let deep_typed = format!(
        r#"{{"id":"a","properties":{{"p":[{{"value":{}1{}}}]}}}}"#,
        r#"{"@type":"g:List","@value":["#.repeat(5_000),
        "]}".repeat(5_000)
    );
    let deep_plain = format!(
        r#"{{"id":"a","x":{}{}}}"#,
        "[".repeat(50_000),
        "]".repeat(50_000)
    );
    let deep_meta = format!(
        r#"{{"id":"a","properties":{{"p":[{{"value":1,"properties":{{"m":{}1{}}}}}]}}}}"#,
        "{\"k\":".repeat(300),
        "}".repeat(300)
    );
    let many_edges = format!(
        r#"{{"id":"a","outE":{{"l":[{}]}}}}"#,
        (0..2_000)
            .map(|i| format!(r#"{{"id":"e{i}","inV":"a"}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let long_id = format!(r#"{{"id":"{}"}}"#, "x".repeat(100_000));
    let mut cases: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"\n\n\n".to_vec(),
        b"\xef\xbb\xbf".to_vec(),
        b"{".to_vec(),
        b"}".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        b"\"vertex\"".to_vec(),
        b"{}".to_vec(),
        br#"{"vertices":null}"#.to_vec(),
        br#"{"vertices":[1,2,3]}"#.to_vec(),
        br#"{"vertices":[{"id":"a"},{"id":"a"}],"vertices":[]}"#.to_vec(),
        br#"{"id":null,"label":null,"properties":null,"outE":null,"inE":null}"#.to_vec(),
        br#"{"id":[],"label":[],"properties":[],"outE":[],"inE":[]}"#.to_vec(),
        br#"{"id":{},"label":{},"properties":{"k":{}},"outE":{"l":{}},"inE":{"l":[[]]}}"#.to_vec(),
        br#"{"id":"a","label":"::"}"#.to_vec(),
        br#"{"id":"a","label":"a::b::::"}"#.to_vec(),
        br#"{"id":"a","label":""}"#.to_vec(),
        br#"{"id":"","label":"x"}"#.to_vec(),
        br#"{"id":"a","outE":{"":[{"inV":"a"}]}}"#.to_vec(),
        br#"{"id":"a","outE":{"l":[{"id":"e","inV":"a"},{"id":"e","inV":"a"}]},"inE":{"l":[{"id":"e","outV":"b"}]}}"#.to_vec(),
        br#"{"id":"a","outE":{"l":[{"id":{"@type":"g:Int32","@value":1},"inV":{"@type":"g:Int64","@value":1}}]}}"#.to_vec(),
        br#"{"id":{"@type":"g:Int32","@value":1}}
{"id":"1"}
{"id":1}
{"id":1.0}"#
            .to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:Map","@value":["k"]}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:Map","@value":[1,2]}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:Map","@value":["k",1,"k",2]}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:Double","@value":1e999}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:Int64","@value":99999999999999999999}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:Int64","@value":-0}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:Double","@value":-0.0}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:UUID","@value":"00000000-0000-0000-0000-00000000000g"}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":null,"@value":null}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":{"@type":"g:List"}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"_meta":[{"value":1}],"p":[{"value":1,"properties":{"m":2}}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":"\ud800"}]}}"#.to_vec(),
        br#"{"id":"a","properties":{"p":[{"value":"\u0000"}]}}"#.to_vec(),
        b"{\"id\":\"\xc3\x28\"}".to_vec(),
        b"{\"id\":\"a\"}\r{\"id\":\"b\"}".to_vec(),
        br#"{"@class":"java.util.HashMap"}"#.to_vec(),
        br#"{"mode":"NORMAL","vertices":[],"edges":[]}"#.to_vec(),
        deep_typed.into_bytes(),
        deep_plain.into_bytes(),
        deep_meta.into_bytes(),
        many_edges.into_bytes(),
        long_id.into_bytes(),
    ];
    for base in [modern_graphson(), crew_graphson()] {
        for cut in (0..base.len()).step_by(89) {
            cases.push(base[..cut].to_vec());
        }
        // Wrapped and pretty-wrapped forms of the same document.
        let text = String::from_utf8(base).expect("UTF-8");
        let lines: Vec<&str> = text.lines().collect();
        cases.push(format!(r#"{{"vertices":[{}]}}"#, lines.join(",")).into_bytes());
        cases.push(format!("{{\"vertices\":[\n{}\n]}}", lines.join(",\n")).into_bytes());
    }
    cases
}

const GRAPHSON_SNIPPETS: &[&[u8]] = &[
    br#"{"@type":"g:Date","@value":1}"#,
    br#"{"@type":"g:Int32","@value":"x"}"#,
    br#"{"@type":"g:Map","@value":[1]}"#,
    br#"{"@type":"g:List","@value":[{"@type":"g:List","@value":[]}]}"#,
    br#"{"@type":"g:UUID","@value":"41d2e28a-20a4-4ab0-b379-d810dede3786"}"#,
    br#"{"@type":"x:Unknown","@value":{"a":[1]}}"#,
    br#""_meta""#,
    br#""outE""#,
    br#""inE""#,
    br#""inV":"zz""#,
    br#""label":"a::b""#,
    b"99999999999999999999",
    b"1e999",
    b"\\ud800",
    b"\n",
    b"\r\n",
    b"\xc3\x28",
    b"\xef\xbb\xbf",
    b"[[[[[[[[",
    b"{\"vertices\":[",
];

/// Imports `bytes` into a fresh graph and, when accepted, also into a live modern graph (one
/// captured unit), then reads and re-exports the result; an accepted re-export must import
/// again and give the same counts.
fn exercise_graphson(bytes: Vec<u8>) {
    if let Ok((graph, report)) = graphersal::io::GraphSONImport::new()
        .with_max_text_bytes(1 << 20)
        .load(std::io::Cursor::new(bytes.clone()))
    {
        let _ = report.to_string();
        let lock = graph.read();
        let _ = lock.traversal().v(None::<()>).element_map(None).to_list();
        let _ = lock.traversal().e(None::<()>).element_map(None).to_list();
        drop(lock);
        let mut out = Vec::new();
        graph
            .export_graphson_writer(&mut out)
            .expect("an imported graph exports");
        let (again, _) = graphersal::io::GraphSONImport::new()
            .load(std::io::Cursor::new(out))
            .expect("a re-export imports");
        assert_eq!(again.read().vertex_count(), graph.read().vertex_count());
        assert_eq!(again.read().edge_count(), graph.read().edge_count());
    }
    let mut live = GraphSource::tinkerpop_modern();
    let _ = live.import_graphson_reader(std::io::Cursor::new(bytes));
    let _ = live.read().traversal().v(None::<()>).count().next();
}

#[test]
fn malformed_graphson_never_panics() {
    let mut fatal = Fatal::default();
    for case in graphson_corpus() {
        let label = String::from_utf8_lossy(&case).into_owned();
        fatal.check(&label, guarded(WATCHDOG, move || exercise_graphson(case)));
    }
    let bases = [modern_graphson(), crew_graphson()];
    let mut rng = Rng::new(0x6EA9_5011);
    for _ in 0..budget(300) {
        let base = rng.pick(&bases).clone();
        let case = mutate(&mut rng, &base, GRAPHSON_SNIPPETS);
        let label = String::from_utf8_lossy(&case).into_owned();
        fatal.check(&label, guarded(WATCHDOG, move || exercise_graphson(case)));
    }
    fatal.assert_empty("GraphSON");
}

// ---------------------------------------------------------------------------------------------
// Schema JSON
// ---------------------------------------------------------------------------------------------

const SAMPLE_SCHEMA: &str = include_str!("../data/sample_ecommerce_schema.json");

const MODERN_SCHEMA: &str = r#"{"mode": "closed",
  "vertices": {
    "person": {"schema": {"type": "object", "properties": {
        "name": {"type": "string", "minLength": 1, "pattern": "^[a-z]+$"},
        "age": {"type": ["integer", "null"], "minimum": 0, "maximum": 200}},
      "required": ["name"]}},
    "software": {"schema": {"type": "object", "properties": {
        "name": {"type": "string"}, "lang": {"type": "string"}}, "required": ["name", "lang"]}}},
  "edges": {
    "knows": {"connections": [{"from": "person", "to": "person"}],
              "schema": {"type": "object", "properties": {
                "weight": {"type": "number", "minimum": 0.0, "maximum": 1.0}}, "required": ["weight"]}},
    "created": {"connections": [{"from": "person", "to": "software"}],
                "schema": {"type": "object", "properties": {"weight": {"type": "number"}}}}}}"#;

fn schema_corpus() -> Vec<String> {
    let label = |node: &str| {
        format!(
            r#"{{"mode":"open","vertices":{{"v":{{"schema":{{"type":"object","properties":{{"x":{node}}}}}}}}}}}"#
        )
    };
    let deep_object = label(&format!(
        "{}{{\"type\":\"integer\"}}{}",
        r#"{"type":"object","properties":{"y":"#.repeat(2_000),
        "}}".repeat(2_000)
    ));
    let deep_array = label(&format!(
        "{}{{\"type\":\"integer\"}}{}",
        r#"{"type":"array","items":"#.repeat(100),
        "}".repeat(100)
    ));
    let deep_any_of = label(&format!(
        "{}{{\"type\":\"integer\"}}{}",
        r#"{"anyOf":["#.repeat(100),
        "]}".repeat(100)
    ));
    // A chain of definitions that doubles at every level: inlining must stop at the node limit.
    let defs = (0..40)
        .map(|i| {
            format!(
                r##""D{i}":{{"type":"object","properties":{{"a":{{"$ref":"#/$defs/D{n}"}},"b":{{"$ref":"#/$defs/D{n}"}}}}}}"##,
                n = i + 1
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let ref_bomb = format!(
        r##"{{"vertices":{{"v":{{"schema":{{"$defs":{{{defs},"D40":{{"type":"integer"}}}},"type":"object","properties":{{"x":{{"$ref":"#/$defs/D0"}}}}}}}}}}}}"##
    );
    let ref_cycle = r##"{"vertices":{"v":{"schema":{"$defs":{"A":{"$ref":"#/$defs/B"},"B":{"anyOf":[{"$ref":"#/$defs/A"}]}},"type":"object","properties":{"x":{"$ref":"#/$defs/A"}}}}}}"##.to_string();
    let many_labels = format!(
        r#"{{"mode":"open","vertices":{{{}}}}}"#,
        (0..20_000)
            .map(|i| format!(r#""l{i}":{{}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let pattern = |p: &str| {
        format!(
            r#"{{"mode":"open","vertices":{{"person":{{"schema":{{"type":"object","properties":{{"name":{{"type":"string","pattern":{}}}}}}}}}}}}}"#,
            serde_json::to_string(p).unwrap_or_default()
        )
    };
    let field = |f: &str| {
        format!(
            r#"{{"mode":"closed","vertices":{{"person":{{"schema":{{"type":"object","properties":{{"name":{f}}}}}}}}}}}"#
        )
    };
    let mut cases = vec![
        String::new(),
        "null".into(),
        "[]".into(),
        "{}".into(),
        "1".into(),
        r#""schema""#.into(),
        r#"{"$schema":1}"#.into(),
        r#"{"mode":"OPEN"}"#.into(),
        r#"{"mode":null}"#.into(),
        r#"{"meta":[1]}"#.into(),
        r#"{"meta":{"a":{"b":[null,1e300,"\u0000"]}}}"#.into(),
        r#"{"mode":"open","vertices":[]}"#.into(),
        r#"{"mode":"open","vertices":{"":{}}}"#.into(),
        label("{}"),
        label(r#"{"type":"Int64"}"#),
        label(r#"{"type":"array"}"#),
        label(r#"{"anyOf":[]}"#),
        label(r#"{"type":"integer","bogus":1}"#),
        label(r#"{"type":["integer","integer"]}"#),
        label(r##"{"$ref":"#/$defs/missing"}"##),
        label(r##"{"$ref":"#/$defs/"}"##),
        label(r#"{"type":"string","format":"date-time"}"#),
        label(r#"{"type":"object","additionalProperties":{"type":"integer"}}"#),
        label(r#"{"enum":[]}"#),
        label(r#"{"const":{"a":[1,2,{"b":null}]}}"#),
        label(r#"{"type":"object","required":["a","a"]}"#),
        r#"{"mode":"open","vertices":{"v":{"schema":{"type":"object","properties":{"x":{"type":"integer"},"x":{"type":"string"}}}}}}"#.into(),
        r#"{"mode":"open","edges":{"e":{"connections":[{"from":"nope","to":"*"}]}}}"#.into(),
        r#"{"mode":"open","edges":{"e":{"connections":[{}]}}}"#.into(),
        field(r#"{"type":"integer","minimum":9223372036854775807,"maximum":-9223372036854775808}"#),
        field(r#"{"type":"integer","minimum":1e400}"#),
        field(r#"{"type":"number","minimum":"NaN"}"#),
        field(r#"{"type":"string","minLength":-1}"#),
        field(r#"{"type":"string","minLength":9223372036854775807,"maxLength":0}"#),
        field(r#"{"type":"array","items":{"type":"integer"},"minItems":5,"maxItems":1}"#),
        field(r#"{"type":"string","examples":[1,{"b":null}],"default":[[]]}"#),
        field(r#"{"type":"string","nullable":"yes"}"#),
        field(r#"{"type":"string","format":"uuid","minimum":1}"#),
        field(r#"{"type":"null"}"#),
        field(r#"{"type":["string","null"],"exclusiveMinimum":0,"exclusiveMaximum":0}"#),
        pattern("(a+)+$"),
        pattern("(a|aa)*b"),
        pattern("(a{1000}){1000}"),
        pattern("[[[[[[[[[[["),
        pattern(&"(".repeat(10_000)),
        pattern(&format!("{}a{}", "(".repeat(300), ")".repeat(300))),
        pattern(&"a?".repeat(5_000)),
        pattern("\\p{Nonexistent}"),
        pattern("(?<n>a)\\k<n>"),
        pattern("^$"),
        pattern(""),
        deep_object,
        deep_array,
        deep_any_of,
        ref_bomb,
        ref_cycle,
        many_labels,
        "{".repeat(100_000),
        "[".repeat(100_000),
        SAMPLE_SCHEMA.into(),
        MODERN_SCHEMA.into(),
    ];
    for cut in (0..MODERN_SCHEMA.len()).step_by(13) {
        cases.push(MODERN_SCHEMA[..cut].to_string());
    }
    cases
}
const SCHEMA_SNIPPETS: &[&[u8]] = &[
    br#""pattern":"(a+)+$","#,
    br#""minimum":-1e308,"#,
    br#""maxLength":0,"#,
    br#""additionalProperties":false,"#,
    br#""anyOf":[{"type":"integer"},{"type":"string"}],"#,
    br##""$ref":"#/$defs/X","##,
    br#""required":["name","age","x"],"#,
    br#""type":["integer","null"],"#,
    br#""type":"array","items":{"type":"object","properties":{}},"#,
    br#""mode":"closed","#,
    br#""mode":"none","#,
    br#"{"from":"*","to":"*"},"#,
    b"null,",
    b"\"\\ud800\"",
    b"\\u0000",
    b"1e999999",
];

/// Parses `text` as a schema and, when accepted, applies it to a modern graph, validates,
/// visualizes it in every format and round-trips it.
fn exercise_schema(text: String) {
    let Ok(schema) = GraphSchema::from_json(&text) else {
        return;
    };
    let _ = schema.to_json();
    let _ = schema.to_json_schema();
    for format in [
        VisualizeFormat::Table,
        VisualizeFormat::Mermaid,
        VisualizeFormat::PlantUML,
        VisualizeFormat::JsonSchema,
        VisualizeFormat::Json,
        VisualizeFormat::Markdown,
        VisualizeFormat::Tree,
        VisualizeFormat::Auto,
    ] {
        let _ = schema.visualize(format);
    }
    let back = GraphSchema::from_json(&schema.to_json());
    assert_eq!(
        back.as_ref().ok(),
        Some(&schema),
        "the canonical form reads back"
    );
    let _ = schema.to_json_pretty();
    let _ = schema.to_graph();
    let _ = SchemaDiff::between(Some(&schema), &GraphSchema::new());
    let _ = SchemaDiff::between(None, &schema);
    let graph = GraphSource::tinkerpop_modern();
    let applied = graph.write().set_schema(schema.clone()).is_ok();
    if applied {
        // Writes through the enforced schema: a valid-looking and an invalid-looking one.
        let mut lock = graph.write();
        let _ = lock
            .traversal_mut()
            .add_v("person")
            .property("name", "zed")
            .property("age", 3)
            .to_list();
        let _ = lock
            .traversal_mut()
            .v(None::<()>)
            .property("name", ElementProperty::Null)
            .to_list();
        let _ = lock.traversal_mut().v(None::<()>).drop().to_list();
    }
    let empty = GraphSource::empty();
    let _ = empty.write().set_schema(schema);
}

#[test]
fn malformed_schema_json_never_panics() {
    let mut fatal = Fatal::default();
    for case in schema_corpus() {
        let label = case.clone();
        fatal.check(&label, guarded(WATCHDOG, move || exercise_schema(case)));
    }
    let bases = [
        SAMPLE_SCHEMA.as_bytes().to_vec(),
        MODERN_SCHEMA.as_bytes().to_vec(),
    ];
    let mut rng = Rng::new(0x5C4E_3A11);
    for _ in 0..budget(300) {
        let base = rng.pick(&bases).clone();
        let case = String::from_utf8_lossy(&mutate(&mut rng, &base, SCHEMA_SNIPPETS)).into_owned();
        let label = case.clone();
        fatal.check(&label, guarded(WATCHDOG, move || exercise_schema(case)));
    }
    fatal.assert_empty("schema JSON");
}

// ---------------------------------------------------------------------------------------------
// ChangeSet journals
// ---------------------------------------------------------------------------------------------

const VALUES: &[&str] = &[
    "1",
    "-9223372036854775808",
    "9223372036854775807",
    "18446744073709551616",
    "1e308",
    "1.5",
    "null",
    "true",
    r#""x""#,
    r#""""#,
    "[]",
    "[1,[2,[3]]]",
    "{}",
    r#"{"a":{"b":[1,"x",null]}}"#,
    r#"{"@type":"g:UUID","@value":"00000000-0000-0000-0000-000000000001"}"#,
    r#"{"@type":"g:UUID","@value":"zz"}"#,
    r#"{"@type":"graphersal:Object","@value":{"@type":"g:UUID"}}"#,
    r#"{"@type":"graphersal:Object","@value":{"@type":"graphersal:Object","@value":{}}}"#,
    r#"{"@type":"g:Int32","@value":1}"#,
    r#"{"@type":"g:UUID","@value":1}"#,
];

const IDS: &[&str] = &[
    "1",
    "2",
    "3",
    "4",
    "7",
    "9",
    "12",
    "x",
    "",
    "9223372036854775808",
];

fn json_str(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// One random (often inconsistent) mutation as JSON.
fn random_mutation(rng: &mut Rng) -> String {
    let id = json_str(rng.pick(IDS));
    let id2 = json_str(rng.pick(IDS));
    let id3 = json_str(rng.pick(IDS));
    let value = *rng.pick(VALUES);
    let key = json_str(rng.pick(&["name", "age", "weight", "", "k", "T.id"]));
    let label = json_str(rng.pick(&["person", "software", "knows", "", "x"]));
    let kind = rng.pick(&["vertex", "edge", "Vertex", "graph"]);
    let prop_choices = [
        "{}".to_string(),
        format!(r#"{{"name":{value}}}"#),
        format!(r#"{{"age":{value},"name":"x"}}"#),
        "[]".to_string(),
    ];
    let props = rng.pick(&prop_choices).clone();
    match rng.below(12) {
        0 => format!(r#"{{"op":"add_vertex","id":{id},"labels":[{label}],"properties":{props}}}"#),
        1 => format!(
            r#"{{"op":"drop_vertex","id":{id},"before":{{"labels":[{label}],"properties":{props}}}}}"#
        ),
        2 => format!(
            r#"{{"op":"add_edge","id":{id},"label":{label},"out_id":{id2},"in_id":{id3},"properties":{props}}}"#
        ),
        3 => format!(
            r#"{{"op":"drop_edge","id":{id},"before":{{"label":null,"out_id":{id2},"in_id":{id3},"properties":{props}}}}}"#
        ),
        4 => format!(
            r#"{{"op":"set_property","element":{{"kind":"{kind}","id":{id}}},"key":{key},"after":{value}}}"#
        ),
        5 => format!(
            r#"{{"op":"set_property","element":{{"kind":"{kind}","id":{id}}},"key":{key},"before":{value},"after":{value}}}"#
        ),
        6 => format!(
            r#"{{"op":"remove_property","element":{{"kind":"{kind}","id":{id}}},"key":{key},"before":{value}}}"#
        ),
        7 => format!(r#"{{"op":"add_label","id":{id},"label":{label}}}"#),
        8 => format!(r#"{{"op":"remove_label","id":{id},"label":{label}}}"#),
        10 => format!(r#"{{"op":"set_edge_label","id":{id},"before":{label},"after":{label}}}"#),
        9 => format!(
            r#"{{"op":"set_schema","before":{{"version":1,"mode":"none"}},"after":{}}}"#,
            rng.pick(&[
                r#"{"version":1,"mode":"closed"}"#,
                r#"{"version":1,"mode":"open","vertices":{"person":{"properties":{"age":{"type":"string"}}}}}"#,
                r#"{"version":1,"mode":"none"}"#,
                r#"{}"#,
            ])
        ),
        _ => rng.pick(&[
            r#"{"op":"unknown"}"#,
            r#"{"op":"add_vertex"}"#,
            r#"{}"#,
            r#"null"#,
            r#"{"op":"add_vertex","id":1,"labels":"x","properties":{}}"#,
        ])
        .to_string(),
    }
}

fn changeset_json(rng: &mut Rng) -> String {
    let seq = rng.pick(&["1", "0", "18446744073709551615", "-1", "1.5", "\"1\""]);
    let n = rng.below(8);
    let mutations: Vec<String> = (0..n).map(|_| random_mutation(rng)).collect();
    format!(
        r#"{{"commit_seq":{seq},"mutations":[{}]}}"#,
        mutations.join(",")
    )
}

/// Deserializes `text` and replays it into a fresh modern graph (and one with a closed schema),
/// then reads the result.
fn exercise_changeset(text: String) -> u8 {
    let Ok(set) = serde_json::from_str::<ChangeSet>(&text) else {
        return 0;
    };
    let mut status = 1;
    let _ = serde_json::to_string(&set);
    if let Ok(graph) = TraversalGraph::tinkerpop_modern().replay(&set) {
        status = 2;
        let _ = graph.traversal().v(None::<()>).element_map(None).to_list();
        let _ = graph.traversal().e(None::<()>).element_map(None).to_list();
        // Replay twice: the second application meets its own effects.
        let _ = graph.replay(&set);
    }
    let mut closed = TraversalGraph::tinkerpop_modern();
    if let Ok(schema) = GraphSchema::from_json(MODERN_SCHEMA)
        && closed.set_schema(schema).is_ok()
    {
        let _ = closed.replay(&set);
    }
    let _ = TraversalGraph::new().replay(&set);
    status
}

#[test]
fn adversarial_changeset_journals_never_panic() {
    let mut fatal = Fatal::default();
    let handwritten = [
        String::new(),
        "{}".into(),
        r#"{"commit_seq":1}"#.into(),
        r#"{"commit_seq":1,"mutations":{}}"#.into(),
        r#"{"commit_seq":1,"mutations":[{"op":"set_property","element":{"kind":"vertex","id":"1"},"key":"k","after":{"@type":"graphersal:Object","@value":[1]}}]}"#.into(),
        format!(
            r#"{{"commit_seq":1,"mutations":[{{"op":"set_property","element":{{"kind":"vertex","id":"1"}},"key":"k","after":{}1{}}}]}}"#,
            "[".repeat(10_000),
            "]".repeat(10_000)
        ),
        format!(
            r#"{{"commit_seq":1,"mutations":[{{"op":"set_property","element":{{"kind":"vertex","id":"1"}},"key":"k","after":{}1{}}}]}}"#,
            r#"{"@type":"graphersal:Object","@value":{"a":"#.repeat(200),
            "}}".repeat(200)
        ),
        // Drop a vertex that still has edges, without dropping its edges first.
        r#"{"commit_seq":1,"mutations":[{"op":"drop_vertex","id":"1","before":{"labels":["person"],"properties":{}}}]}"#.into(),
        // An edge to a vertex that does not exist.
        r#"{"commit_seq":1,"mutations":[{"op":"add_edge","id":"e","label":"x","out_id":"1","in_id":"nope","properties":{}}]}"#.into(),
        // An id that collides with an existing element.
        r#"{"commit_seq":1,"mutations":[{"op":"add_vertex","id":"1","labels":["person"],"properties":{}}]}"#.into(),
        r#"{"commit_seq":1,"mutations":[{"op":"add_edge","id":"7","label":"x","out_id":"1","in_id":"2","properties":{}}]}"#.into(),
        r#"{"commit_seq":1,"mutations":[{"op":"set_property","element":{"kind":"edge","id":"1"},"key":"k","after":1}]}"#.into(),
        r#"{"commit_seq":1,"mutations":[{"op":"remove_label","id":"1","label":"person"},{"op":"remove_label","id":"1","label":"person"}]}"#.into(),
        r#"{"commit_seq":1,"mutations":[{"op":"add_label","id":"1","label":""}]}"#.into(),
        r#"{"commit_seq":1,"mutations":[{"op":"set_schema","before":{"version":1,"mode":"none"},"after":{"version":1,"mode":"closed"}},{"op":"add_vertex","id":"n","labels":["ghost"],"properties":{}}]}"#.into(),
    ];
    for case in handwritten {
        let label = case.clone();
        fatal.check(&label, guarded(WATCHDOG, move || exercise_changeset(case)));
    }
    let mut rng = Rng::new(0xC4A6_6E5E);
    let mut seen = [0_u64; 3];
    for _ in 0..budget(400) {
        let case = changeset_json(&mut rng);
        let label = case.clone();
        let result = guarded(WATCHDOG, move || exercise_changeset(case));
        if let Ok(status) = &result {
            seen[usize::from(*status)] += 1;
        }
        fatal.check(&label, result);
    }
    fatal.assert_empty("ChangeSet");
    // The generator must reach the replay, not only the deserializer.
    assert!(seen[2] > 20, "parse failures / parsed / replayed: {seen:?}");
}

// ---------------------------------------------------------------------------------------------
// jpath, UUID.fromString and parameters (through the script entry)
// ---------------------------------------------------------------------------------------------

fn run_script(
    script: &'static str,
    params: HashMap<String, ElementProperty>,
) -> Result<(), Outcome> {
    guarded(WATCHDOG, move || {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let _ = eval_value_with_limits(graph, script, params, &ScriptLimits::default());
    })
}

fn random_text(rng: &mut Rng, alphabet: &[char], max: usize) -> String {
    let len = rng.below(max + 1);
    (0..len).map(|_| *rng.pick(alphabet)).collect()
}

const JPATH_ALPHABET: &[char] = &[
    'a', 'b', '.', '.', '[', ']', '[', ']', '-', '0', '1', '9', '\'', '"', '$', '*', '\\', ' ',
    'ü', '\u{0}', '?', '@', '(', ')', ':', ',', 'e',
];

#[test]
fn random_jpath_strings_never_panic() {
    let mut fatal = Fatal::default();
    let mut texts: Vec<String> = [
        "a[-9223372036854775807]",
        "a[9223372036854775807]",
        "a[99999999999999999999999]",
        "a[-]",
        "a[01]",
        "a['\\u0000']",
        "a['\\ud800']",
        "a['\\",
        "$['a'",
        "$[\"a\"]",
        "a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Long paths; a much deeper key path aborts the process today (robustness_bug_tests).
    texts.push(format!("a{}", "[0]".repeat(10_000)));
    texts.push(format!("a{}", ".b".repeat(500)));
    let mut rng = Rng::new(0x1A7B_0001);
    for _ in 0..budget(100) {
        texts.push(random_text(&mut rng, JPATH_ALPHABET, 16));
    }
    for text in texts {
        let label = text.clone();
        let params: HashMap<String, ElementProperty> = [("p".to_string(), text.into())].into();
        fatal.check(
            &label,
            run_script(
                r#"let j = jpath(p);
                   g.v().values(j).toList();
                   g.v().has(j, 1).toList();
                   g.v().properties(j).toList();
                   g.v().property(j, [1, #{a: 2}]).values(j).toList();
                   g.v().remove_property(j).toList();
                   g.inject(#{a: [1, #{b: 2}]}).json_path(p).toList();"#,
                params,
            ),
        );
    }
    fatal.assert_empty("jpath");
}

#[test]
fn random_uuid_strings_never_panic() {
    let mut fatal = Fatal::default();
    let alphabet: Vec<char> = "0123456789abcdefABCDEF-{}xü ".chars().collect();
    let mut texts: Vec<String> = [
        "",
        "00000000-0000-0000-0000-000000000000",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
        "{00000000-0000-0000-0000-000000000000}",
        "00000000000000000000000000000000",
        "00000000-0000-0000-0000-00000000000",
        "00000000-0000-0000-0000-0000000000000",
        "0000000-00000-0000-0000-000000000000",
        "urn:uuid:00000000-0000-0000-0000-000000000000",
        "üüüüüüüü-üüüü-üüüü-üüüü-üüüüüüüüüüüü",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut rng = Rng::new(0x0000_0001);
    for _ in 0..budget(100) {
        let mut text = random_text(&mut rng, &alphabet, 40);
        if rng.chance(50) {
            // Near-canonical shape with random characters.
            let shape: String = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
                .chars()
                .map(|c| if c == 'x' { *rng.pick(&alphabet) } else { c })
                .collect();
            text = shape;
        }
        texts.push(text);
    }
    for text in texts {
        let label = text.clone();
        let params: HashMap<String, ElementProperty> = [("p".to_string(), text.into())].into();
        fatal.check(
            &label,
            run_script(
                r#"let u = UUID.fromString(p);
                   g.addV("x").property("u", u).values("u").toList();
                   g.v().has("u", u).toList();
                   g.inject(u).as_string().as_number().toList();"#,
                params,
            ),
        );
    }
    fatal.assert_empty("UUID.fromString");
}

/// The adversarial parameter values, built inside the case thread (a huge value is neither
/// cloned nor formatted on the test thread). Deep nesting is covered by `robustness_bug_tests`
/// (it aborts the process today).
fn param_value(index: usize) -> ElementProperty {
    match index {
        0 => ElementProperty::Null,
        1 => f64::NAN.into(),
        2 => f64::INFINITY.into(),
        3 => i64::MIN.into(),
        4 => "x".repeat(1_000_000).into(),
        5 => ElementProperty::Array((0..200_000).map(ElementProperty::from).collect()),
        6 => (0..100).fold(ElementProperty::from(1_i64), |inner, _| {
            ElementProperty::Array(vec![inner])
        }),
        _ => ElementProperty::Uuid(u128::MAX),
    }
}

#[test]
fn adversarial_parameters_never_panic() {
    let mut fatal = Fatal::default();
    let names = [
        "p", "g", "__", "_g", "P", "T", "keys", "first", "", " ", "1x", "a-b", "fn", "let", "this",
        "ü",
    ];
    for name in names {
        for index in 0..8 {
            let label = format!("{name} = value #{index}");
            let result = guarded(WATCHDOG, move || {
                let params: HashMap<String, ElementProperty> =
                    [(name.to_string(), param_value(index))].into();
                let graph = Arc::new(GraphSource::tinkerpop_modern());
                let _ = eval_value_with_limits(
                    graph,
                    r#"let x = if is_def_var("p") { p } else { 1 };
                       g.inject(x).toList();
                       g.addV("x").property("k", x).values("k").toList();
                       g.v().has("name", x).toList();
                       g.inject(x).unfold().toList();"#,
                    params,
                    &ScriptLimits::default(),
                );
            });
            fatal.check(&label, result);
        }
    }
    fatal.assert_empty("parameters");
}

// ---------------------------------------------------------------------------------------------
// GQL query strings (feature `gql`), through the Rust entry and through Rhai
// ---------------------------------------------------------------------------------------------

/// Valid GQL queries of every construct of the supported subset, then refused and not yet
/// supported ones: the base documents of the GQL mutations.
#[cfg(feature = "gql")]
const GQL_CORPUS: &[&str] = &[
    "MATCH (p:person) RETURN p",
    "MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name ORDER BY name DESC",
    "MATCH (p:person|software) RETURN p.name, labels(p), element_id(p)",
    "MATCH (p:!person) RETURN p",
    "MATCH (p:%) RETURN p",
    "MATCH (p:(person&!software)|software) RETURN *",
    "MATCH (p {name: 'marko'}) RETURN p.age",
    "MATCH (p:person {name: $name}) RETURN p.age",
    "MATCH (a:person)-[:knows]->(b) RETURN a.name AS a, b.name AS b",
    "MATCH (a)<-[e:created]-(b) RETURN a, e, b, type(e)",
    "MATCH (a)-[e]-(b) RETURN element_id(e), a.name",
    "MATCH (a)-->(b)<--(c) RETURN a, b, c",
    "MATCH (a)--(b) RETURN DISTINCT a",
    "MATCH (a)-[e:knows|created WHERE e.weight > 0.4]->(b) RETURN e.weight",
    "MATCH (a)-[e {weight: 1.0}]->(b) RETURN a, b",
    "MATCH (a)-[:knows]->(b), (b)-[:created]->(c) RETURN a.name, c.name",
    "MATCH (a)-[:created]->(s) MATCH (b)-[:created]->(s) WHERE a <> b RETURN a, b, s",
    "MATCH (a)-[e1]->(b)-[e2]->(c) RETURN a, c",
    "MATCH (a)-[:knows]->(a) RETURN a",
    "MATCH (a), (b) RETURN a, b LIMIT 3",
    "MATCH (p) RETURN p.name ORDER BY p.age, p.name DESC OFFSET 1 LIMIT 2",
    "MATCH (p) RETURN p.name SKIP $o LIMIT $n",
    "MATCH (p) WHERE p.age IS NULL OR p.age IS NOT NULL RETURN p",
    "MATCH (p) WHERE NOT (p.age < 30) XOR p.name STARTS WITH 'm' RETURN p",
    "MATCH (p) WHERE p.name ENDS WITH 'o' AND p.name CONTAINS 'ar' RETURN p",
    "MATCH (p) WHERE p.age IN [27, 29, NULL] RETURN p",
    "MATCH (p) WHERE p IS LABELED person RETURN p",
    "MATCH (p) RETURN CASE WHEN p.age > 30 THEN 'old' ELSE 'young' END AS c",
    "MATCH (p) RETURN CASE p.name WHEN 'marko' THEN 1 END",
    "MATCH (p) RETURN p.age + 1, p.age - 1, p.age * 2, p.age / 2, p.age % 7, -p.age",
    "MATCH (p) RETURN p.name || '!', upper(p.name), lower(p.name), char_length(p.name)",
    "MATCH (p) RETURN coalesce(p.age, 0), size(labels(p)), cardinality([1, 2])",
    "MATCH (p) RETURN [1, 2.5, 'x', TRUE, FALSE, NULL], {a: 1, b: [p.age]}",
    "MATCH (p) RETURN 9223372036854775807 + 1",
    "MATCH (p) RETURN 1 / 0, 1.0 / 0.0",
    "MATCH (p) RETURN p.name + 1",
    "MATCH (`weird name`) RETURN `weird name`",
    "MATCH (p) // a comment\n/* a block */ -- another\nRETURN p",
    "match (p:person) where p.age >= 29 return p.name as n order by n asc limit 1",
    // Refused and not yet supported.
    "CREATE (p:person {name: 'x'})",
    "MATCH (p) SET p.age = 1 RETURN p",
    "MATCH (p) DETACH DELETE p",
    "MERGE (p:person)",
    "MATCH (a)~[e]~(b) RETURN a",
    "MATCH (a)<~(b) RETURN a",
    "MATCH (p)",
    "MATCH (p) RETURN id(p)",
    "MATCH (p) RETURN count(p), sum(p.age), avg(p.age), collect(p.name)",
    "OPTIONAL MATCH (p) RETURN p",
    "MATCH (p) WITH p RETURN p",
    "MATCH p = (a)-->(b) RETURN p",
    "MATCH (a)-[*1..3]->(b) RETURN b",
    "MATCH ((a)-->(b)){1,3} RETURN a",
    "CALL db.labels()",
    "MATCH (p) RETURN p UNION MATCH (q) RETURN q",
    "MATCH (p) WHERE EXISTS { (p)-->() } RETURN p",
    "UNWIND [1, 2] AS x RETURN x",
    "MATCH (p) RETURN p.name ORDER BY p.nope LIMIT -1",
    "MATCH (p) RETURN q",
    "MATCH (p) RETURN p, p",
    "",
    "MATCH",
    "RETURN 1",
];

/// Snippets the GQL mutations insert: keywords, brackets, operators, comments, quotes.
#[cfg(feature = "gql")]
const GQL_SNIPPETS: &[&[u8]] = &[
    b"MATCH ",
    b" RETURN ",
    b" WHERE ",
    b"(",
    b")",
    b"[",
    b"]",
    b"{",
    b"}",
    b"-[e]->",
    b"<-",
    b"~",
    b"|",
    b"&",
    b"!",
    b"%",
    b" AND NOT ",
    b"$n",
    b"$",
    b"'",
    b"\"",
    b"`",
    b"/*",
    b"--",
    b"//",
    b"\n",
    b"\0",
    b"\xff",
    b" ORDER BY ",
    b" LIMIT ",
    b" CASE WHEN ",
    b"element_id(",
    b"9999999999999999999999",
    b"1e309",
    b"\xc3\xbc",
];

/// The tokens the GQL token soup is drawn from.
#[cfg(feature = "gql")]
const GQL_TOKENS: &[&str] = &[
    "MATCH",
    "RETURN",
    "WHERE",
    "ORDER",
    "BY",
    "LIMIT",
    "OFFSET",
    "SKIP",
    "DISTINCT",
    "AS",
    "AND",
    "OR",
    "NOT",
    "XOR",
    "IS",
    "NULL",
    "IN",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ":",
    ",",
    ".",
    "-",
    "->",
    "<-",
    "--",
    "-->",
    "<--",
    "~",
    "*",
    "/",
    "+",
    "=",
    "<>",
    "<",
    ">=",
    "|",
    "&",
    "!",
    "%",
    "p",
    "q",
    "e",
    "person",
    "knows",
    "name",
    "age",
    "$n",
    "$name",
    "'x'",
    "1",
    "2.5",
    "TRUE",
    "element_id",
    "labels",
    "count",
    "`",
    "/*",
    "//",
    "\n",
    "ü",
];

/// Runs `query` through `g.gql()` from Rust (rows, a count, the profile, the diagnostic in both
/// spellings) and from Rhai, on the modern graph plus a self-loop and a `null` age.
#[cfg(feature = "gql")]
fn exercise_gql(query: String) -> Result<(), Outcome> {
    guarded(WATCHDOG, move || {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let _ = graph
            .traversal_mut()
            .add_v("person")
            .property("name", "nullage")
            .property("age", ElementProperty::Null)
            .iterate();
        let _ = graph
            .traversal_mut()
            .v(None)
            .has("name", "marko")
            .as_("m")
            .add_e("knows")
            .from("m")
            .iterate();
        let params = [
            ("n", ElementProperty::from(2)),
            ("o", ElementProperty::from(1)),
            ("name", ElementProperty::from("marko")),
        ];
        let results = [
            graph.traversal().gql_with(&query, params.clone()).to_list(),
            graph
                .traversal()
                .with("render.spelling", "camel")
                .gql_with(&query, params.clone())
                .to_list(),
            graph
                .traversal()
                .gql_with(&query, params.clone())
                .count()
                .to_list(),
        ];
        for result in results {
            if let Err(err) = result {
                let _ = format_diagnostic(&err);
                let _ = err.help();
            }
        }
        match graph.traversal().gql_with(&query, params).profile() {
            Ok(metrics) => {
                let _ = metrics.to_string();
            }
            Err(err) => {
                let _ = format_diagnostic(&err);
            }
        }
        let graph = Arc::new(Graph::new(graph));
        let script_params: HashMap<String, ElementProperty> =
            [("q".to_string(), ElementProperty::from(query.as_str()))].into();
        let _ = eval_value_with_limits(
            graph,
            r#"g.gql(q).toList();
               g.gql(q, #{n: 2, o: 1, name: "marko"}).select("p").out().toList();
               g.with("render.spelling", "camel").gql(q, #{n: -1}).toList();
               g.gql(q, ()).count().next();
               g.v().gql(q).toList();"#,
            script_params,
            &ScriptLimits::default(),
        );
    })
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_strings_never_panic() {
    let mut fatal = Fatal::default();
    let mut texts: Vec<String> = GQL_CORPUS.iter().map(|s| s.to_string()).collect();
    // Deep nesting and long chains: a bounded error, never a stack overflow.
    texts.push(format!(
        "MATCH (p) RETURN {}1{}",
        "(".repeat(50_000),
        ")".repeat(50_000)
    ));
    texts.push(format!(
        "MATCH (p) WHERE {}TRUE RETURN p",
        "NOT ".repeat(50_000)
    ));
    texts.push(format!(
        "MATCH (p) WHERE {}TRUE RETURN p",
        "p.age = 1 AND ".repeat(5_000)
    ));
    texts.push(format!("MATCH (p) RETURN {}1", "-".repeat(50_000)));
    texts.push(format!(
        "MATCH (p:{}person{}) RETURN p",
        "(".repeat(20_000),
        ")".repeat(20_000)
    ));
    texts.push(format!("MATCH (p:{}person) RETURN p", "!".repeat(50_000)));
    texts.push(format!(
        "MATCH (p) RETURN {}1{}",
        "[".repeat(50_000),
        "]".repeat(50_000)
    ));
    texts.push(format!("MATCH (p) RETURN {}", "{a: ".repeat(20_000)));
    // A long path: the trail checks grow with the square of the edge count (every pair of edges
    // whose labels may overlap gets one), so 300 hops stay well inside the watchdog.
    texts.push(format!("MATCH {} RETURN *", "(a)-->".repeat(300) + "(b)"));
    texts.push(format!("MATCH (p) RETURN 1{}", " + 1".repeat(20_000)));
    texts.push(format!("MATCH (p) RETURN p /* {}", "x".repeat(100_000)));
    let mut rng = Rng::new(0x6A1_0001);
    for _ in 0..budget(150) {
        let base = rng.pick(GQL_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(24);
        let soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_TOKENS)).collect();
        let joiner = if rng.chance(50) { " " } else { "" };
        texts.push(soup.join(joiner));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL");
}

// ---------------------------------------------------------------------------------------------
// GQL stage 1b: grammar-generated queries (OPTIONAL MATCH, WITH, LET, FILTER, quantified edges,
// path and group variables, aggregates)
// ---------------------------------------------------------------------------------------------

/// Builds random queries of the stage 1b grammar (`parser.rs` module docs): every one is
/// syntactically valid, and it reads mostly variables in scope (it tracks what each clause
/// declares, what `WITH` keeps and `LET` adds), so most reach the binder, the planner, the
/// lowering and the run; the rest end in a semantic or a data error.
#[cfg(feature = "gql")]
struct Gql1bGenerator<'r> {
    rng: &'r mut Rng,
    /// The variables in scope with their kind.
    scope: Vec<(String, GenKind)>,
    /// The counter for fresh names.
    fresh: usize,
}

/// What a generated variable names.
#[cfg(feature = "gql")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum GenKind {
    Node,
    Edge,
    Group,
    Path,
    Value,
}

#[cfg(feature = "gql")]
impl<'r> Gql1bGenerator<'r> {
    const LABELS: &'static [&'static str] = &["person", "software", "knows", "created", "nope"];
    const KEYS: &'static [&'static str] = &["name", "age", "weight", "lang", "missing"];

    fn new(rng: &'r mut Rng) -> Self {
        Self {
            rng,
            scope: Vec::new(),
            fresh: 0,
        }
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}{}", self.fresh)
    }

    /// A variable in scope of one of `kinds`, if any.
    fn pick_var(&mut self, kinds: &[GenKind]) -> Option<String> {
        let names: Vec<&String> = self
            .scope
            .iter()
            .filter(|(_, kind)| kinds.contains(kind))
            .map(|(name, _)| name)
            .collect();
        (!names.is_empty()).then(|| (*self.rng.pick(&names)).clone())
    }

    fn declare(&mut self, name: &str, kind: GenKind) {
        if !self.scope.iter().any(|(n, _)| n == name) {
            self.scope.push((name.to_string(), kind));
        }
    }

    fn labels(&mut self) -> String {
        let first = *self.rng.pick(Self::LABELS);
        match self.rng.below(5) {
            0 => format!(":{first}|{}", self.rng.pick(Self::LABELS)),
            1 => format!(":!{first}"),
            _ => format!(":{first}"),
        }
    }

    fn literal(&mut self) -> String {
        match self.rng.below(6) {
            0 => "'marko'".to_string(),
            1 => "NULL".to_string(),
            2 => "$n".to_string(),
            3 => "2.5".to_string(),
            _ => self.rng.below(40).to_string(),
        }
    }

    /// A property of an element in scope, or a literal.
    fn property(&mut self) -> String {
        match self.pick_var(&[GenKind::Node, GenKind::Edge]) {
            Some(var) => format!("{var}.{}", self.rng.pick(Self::KEYS)),
            None => self.literal(),
        }
    }

    fn condition(&mut self) -> String {
        let left = self.property();
        match self.rng.below(6) {
            0 => format!("{left} IS NULL"),
            1 => format!("{left} IN [27, 29, NULL]"),
            2 => format!("{left} STARTS WITH 'm'"),
            3 => format!("{left} > {} AND NOT {left} = 1", self.literal()),
            4 => match self.pick_var(&[GenKind::Path]) {
                Some(path) => format!("path_length({path}) > 1"),
                None => format!("{left} <> 0"),
            },
            _ => format!("{left} >= {}", self.literal()),
        }
    }

    fn expression(&mut self) -> String {
        match self.rng.below(8) {
            0 => self
                .pick_var(&[GenKind::Node, GenKind::Edge, GenKind::Value])
                .unwrap_or_else(|| self.literal()),
            1 => format!("coalesce({}, 0)", self.property()),
            2 => match self.pick_var(&[GenKind::Node]) {
                Some(node) => format!("labels({node})"),
                None => self.literal(),
            },
            3 => match self.pick_var(&[GenKind::Path]) {
                Some(path) => format!("path_length({path})"),
                None => self.property(),
            },
            4 => match self.pick_var(&[GenKind::Group]) {
                Some(group) => format!("size({group})"),
                None => self.property(),
            },
            5 => format!("CASE WHEN {} THEN 1 ELSE 0 END", self.condition()),
            _ => self.property(),
        }
    }

    fn aggregate(&mut self) -> String {
        let distinct = if self.rng.chance(20) { "DISTINCT " } else { "" };
        let element = self.pick_var(&[GenKind::Node, GenKind::Edge]);
        match (self.rng.below(7), element) {
            (1, Some(var)) => format!("count({distinct}{var})"),
            (2, _) => format!("sum({distinct}{})", self.property()),
            (3, _) => format!("avg({})", self.property()),
            (4, _) => format!("min({})", self.property()),
            (5, _) => format!("max({})", self.expression()),
            (6, _) => format!("collect_list({distinct}{})", self.expression()),
            _ => "count(*)".to_string(),
        }
    }

    fn node(&mut self) -> String {
        let mut node = String::from("(");
        let joined = if self.rng.chance(35) {
            self.pick_var(&[GenKind::Node])
        } else {
            None
        };
        let var = match joined {
            Some(var) => Some(var),
            None if self.rng.chance(80) => Some(self.fresh("n")),
            None => None,
        };
        if let Some(var) = &var {
            node.push_str(var);
        }
        if self.rng.chance(40) {
            node.push_str(&self.labels());
        }
        match (self.rng.below(6), &var) {
            (0, _) => node.push_str(" {name: 'marko'}"),
            (1, Some(var)) => {
                let key = *self.rng.pick(Self::KEYS);
                node.push_str(&format!(" WHERE {var}.{key} IS NOT NULL"));
            }
            _ => {}
        }
        node.push(')');
        if let Some(var) = var {
            self.declare(&var, GenKind::Node);
        }
        node
    }

    fn quantifier(&mut self) -> String {
        (*self.rng.pick(&[
            "{1,3}", "{2}", "{0,2}", "{1,}", "{,2}", "*", "+", "{0}", "{2,4}",
        ]))
        .to_string()
    }

    fn edge(&mut self) -> String {
        let cypher = self.rng.chance(15);
        let quantified = cypher || self.rng.chance(40);
        let var = self.rng.chance(60).then(|| self.fresh("e"));
        let mut filler = var.clone().unwrap_or_default();
        if self.rng.chance(50) {
            filler.push_str(&self.labels());
        }
        if cypher {
            filler.push_str(self.rng.pick(&["*", "*2", "*1..2", "*..3", "*0.."]));
        } else if let Some(var) = &var
            && self.rng.chance(20)
        {
            filler.push_str(&format!(" WHERE {var}.weight > 0.1"));
        }
        let edge = match self.rng.below(7) {
            0 => format!("<-[{filler}]-"),
            1 => format!("-[{filler}]-"),
            2 if var.is_none() && !cypher => "-->".to_string(),
            3 if var.is_none() && !cypher => "<--".to_string(),
            _ => format!("-[{filler}]->"),
        };
        if let Some(var) = var {
            let kind = if quantified {
                GenKind::Group
            } else {
                GenKind::Edge
            };
            self.declare(&var, kind);
        }
        if quantified && !cypher {
            format!("{edge}{}", self.quantifier())
        } else {
            edge
        }
    }

    fn path(&mut self) -> String {
        let mut path = String::new();
        let variable = self.rng.chance(20).then(|| self.fresh("p"));
        if let Some(var) = &variable {
            path.push_str(&format!("{var} = "));
        }
        path.push_str(&self.node());
        for _ in 0..self.rng.below(3) {
            path.push_str(&self.edge());
            path.push_str(&self.node());
        }
        if let Some(var) = variable {
            self.declare(&var, GenKind::Path);
        }
        path
    }

    fn match_clause(&mut self, first: bool) -> String {
        let optional = if first {
            self.rng.chance(5)
        } else {
            self.rng.chance(40)
        };
        let keyword = if optional { "OPTIONAL MATCH" } else { "MATCH" };
        let mut clause = format!("{keyword} {}", self.path());
        if self.rng.chance(15) {
            let path = self.path();
            clause.push_str(&format!(", {path}"));
        }
        if self.rng.chance(30) {
            let condition = self.condition();
            clause.push_str(&format!(" WHERE {condition}"));
        }
        clause
    }

    /// The items of a `RETURN` / `WITH` and the names of its columns (the sort keys a tail may
    /// use after an aggregate or `DISTINCT`).
    fn items(&mut self, with: bool) -> (String, Vec<(String, GenKind)>) {
        if !with && self.rng.chance(10) {
            return ("*".to_string(), Vec::new());
        }
        let mut items = Vec::new();
        let mut columns = Vec::new();
        for _ in 0..1 + self.rng.below(3) {
            let carried = if self.rng.chance(40) {
                self.pick_var(&[
                    GenKind::Node,
                    GenKind::Edge,
                    GenKind::Group,
                    GenKind::Path,
                    GenKind::Value,
                ])
            } else {
                None
            };
            if let Some(name) = carried
                && !columns.iter().any(|(n, _)| *n == name)
            {
                let kind = self
                    .scope
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map_or(GenKind::Value, |(_, kind)| *kind);
                items.push(name.clone());
                columns.push((name, kind));
                continue;
            }
            let item = if self.rng.chance(35) {
                self.aggregate()
            } else {
                self.expression()
            };
            let alias = self.fresh("v");
            items.push(format!("{item} AS {alias}"));
            columns.push((alias, GenKind::Value));
        }
        (items.join(", "), columns)
    }

    /// `ORDER BY` (a column), `OFFSET` / `SKIP` and `LIMIT` in this order, each maybe.
    fn tail(&mut self, columns: &[(String, GenKind)]) -> String {
        let mut tail = String::new();
        if !columns.is_empty() && self.rng.chance(30) {
            let (key, _) = self.rng.pick(columns).clone();
            let direction = *self.rng.pick(&["", " DESC", " ASC"]);
            tail.push_str(&format!(" ORDER BY {key}{direction}"));
        }
        if self.rng.chance(15) {
            tail.push_str(self.rng.pick(&[" OFFSET 1", " SKIP $o"]));
        }
        if self.rng.chance(20) {
            tail.push_str(self.rng.pick(&[" LIMIT 2", " LIMIT $n"]));
        }
        tail
    }

    fn query(&mut self) -> String {
        let mut clauses = vec![self.match_clause(true)];
        for _ in 0..self.rng.below(4) {
            let clause = match self.rng.below(5) {
                0 | 1 => self.match_clause(false),
                2 => {
                    let distinct = if self.rng.chance(15) { "DISTINCT " } else { "" };
                    let (items, columns) = self.items(true);
                    let mut with = format!("WITH {distinct}{items}");
                    with.push_str(&self.tail(&columns));
                    self.scope = columns;
                    if self.rng.chance(25) {
                        let condition = self.condition();
                        with.push_str(&format!(" WHERE {condition}"));
                    }
                    with
                }
                3 => {
                    let value = self.expression();
                    let name = self.fresh("v");
                    self.declare(&name, GenKind::Value);
                    format!("LET {name} = {value}")
                }
                _ => format!("FILTER {}", self.condition()),
            };
            clauses.push(clause);
        }
        let distinct = if self.rng.chance(15) { "DISTINCT " } else { "" };
        let (items, columns) = self.items(false);
        let tail = self.tail(&columns);
        clauses.push(format!("RETURN {distinct}{items}{tail}"));
        clauses.join(if self.rng.chance(20) { "\n" } else { " " })
    }
}

/// Valid stage 1b queries of every new construct: the base documents of the 1b mutations.
#[cfg(feature = "gql")]
const GQL_1B_CORPUS: &[&str] = &[
    "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name, count(s)",
    "MATCH (p:person) WITH p, count(*) AS c ORDER BY c DESC LIMIT 2 WHERE c > 0 RETURN p, c",
    "MATCH (p:person) LET n = upper(p.name), a = p.age + 1 FILTER a > 28 RETURN n, a",
    "MATCH (a)-[e:knows]->{1,3}(b) RETURN a, e, b",
    "MATCH (a)-[:knows]->*(b) RETURN DISTINCT b",
    "MATCH (a)<-[e*1..2]-(b) RETURN size(e), path_length(p) AS l",
    "MATCH p = (a)-[:knows|created]-+(b) RETURN p, path_length(p)",
    "MATCH (a) RETURN a.lang, collect_list(a.name), collect(DISTINCT a.age), sum(a.age), \
     avg(a.age), min(a.age), max(a.name), count(DISTINCT a)",
    "OPTIONAL MATCH (x:nope) RETURN x",
    "MATCH (a)-[e]-{0,2}(b) WITH DISTINCT b WHERE b.age IS NOT NULL RETURN b.name ORDER BY \
     b.name SKIP $o LIMIT $n",
    "MATCH (a:person) WITH a MATCH (a)-[:created]->(s) OPTIONAL MATCH (s)<-[:created]-(o) \
     RETURN a, s, count(o)",
    "MATCH (a)-[e]->{1,}(b)-[f]->{,2}(c) RETURN count(*)",
    "MATCH p = (a)-[e:knows WHERE e.weight > 0.1]->{1,2}(b) RETURN nodes(p)",
    "MATCH (a) RETURN a.name GROUP BY a.name",
];

/// Tokens of stage 1b for the 1b token soup (a list of its own; `GQL_TOKENS` stays as it is).
#[cfg(feature = "gql")]
const GQL_1B_TOKENS: &[&str] = &[
    "MATCH",
    "OPTIONAL",
    "WITH",
    "LET",
    "FILTER",
    "RETURN",
    "DISTINCT",
    "ORDER BY",
    "LIMIT",
    "WHERE",
    "AS",
    "=",
    "(a)",
    "(b:person)",
    "-[e]->",
    "<-[e:knows]-",
    "-[e*1..2]->",
    "-[*]-",
    "-->",
    "{1,3}",
    "{0}",
    "{2,}",
    "{,2}",
    "{",
    "}",
    ",",
    "*",
    "+",
    "p =",
    "p",
    "e",
    "a",
    "count(*)",
    "count(",
    "sum(a.age)",
    "collect_list(",
    "collect(DISTINCT a)",
    "avg(",
    "path_length(p)",
    "nodes(p)",
    ")",
    "a.age",
    "GROUP BY",
    "1",
    "$n",
];

#[test]
#[cfg(feature = "gql")]
fn random_gql_1b_queries_never_panic() {
    let mut fatal = Fatal::default();
    let mut syntax = Vec::new();
    let mut rng = Rng::new(0x6A1_B001);
    let mut generated = Vec::new();
    for _ in 0..budget(150) {
        generated.push(Gql1bGenerator::new(&mut rng).query());
    }
    // Every generated query parses: an error is a semantic one or a run-time one; and enough
    // of them run to the end that the generator exercises the planner and the lowering.
    let mut ran = 0;
    for query in &generated {
        let text = query.clone();
        let outcome = guarded(WATCHDOG, move || {
            let graph = TraversalGraph::tinkerpop_modern();
            match graph
                .traversal()
                .gql_with(&text, [("n", 2), ("o", 1)])
                .to_list()
            {
                Ok(_) => Ok(()),
                Err(err) => match err.root_cause() {
                    TraverserError::Gql { kind, message, .. } => {
                        Err(Some((*kind, message.clone())))
                    }
                    _ => Err(None),
                },
            }
        });
        match outcome {
            Ok(Ok(())) => ran += 1,
            Ok(Err(Some((graphersal::error::GqlErrorKind::Syntax, message)))) => {
                syntax.push(format!("{query}: {message}"));
            }
            Ok(Err(_)) => {}
            Err(outcome) => fatal.check(query, Err::<(), _>(outcome)),
        }
    }
    assert!(
        ran * 2 >= generated.len(),
        "only {ran} of {} generated 1b queries ran",
        generated.len()
    );
    let mut texts = generated;
    texts.extend(GQL_1B_CORPUS.iter().map(|s| s.to_string()));
    // Long 1b chains: bounded errors or results, never a stack overflow.
    texts.push(format!(
        "MATCH (a) {}RETURN a",
        "OPTIONAL MATCH (a)-->(b) ".repeat(200)
    ));
    texts.push(format!("MATCH (a) {}RETURN a", "WITH a ".repeat(500)));
    texts.push(format!(
        "MATCH (a) LET x0 = 1 {}RETURN x199",
        (1..200)
            .map(|i| format!("LET x{i} = x{} + x{} ", i - 1, i - 1))
            .collect::<String>()
    ));
    texts.push(format!(
        "MATCH (a){}(b) RETURN count(*)",
        "-[]->{1,}()".repeat(30)
    ));
    texts.push(format!(
        "MATCH (a) RETURN {}a.age{}",
        "max(".repeat(50),
        ")".repeat(50)
    ));
    for _ in 0..budget(100) {
        let base = rng.pick(GQL_1B_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(24);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_1B_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (a)");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 1b");
    assert!(
        syntax.is_empty(),
        "generated 1b queries that do not parse:\n{}",
        syntax.join("\n")
    );
}

// ---------------------------------------------------------------------------------------------
// GQL stage 1c: random function calls (every scalar function, its Cypher spellings, the special
// forms CAST / TRIM / IS SOURCE OF, the not-yet names, misspellings) with random arguments
// ---------------------------------------------------------------------------------------------

/// Function names for the 1c call generator and token soup (a list of its own; `GQL_TOKENS`
/// and `GQL_1B_TOKENS` stay as they are): every name of the function table, Cypher spellings,
/// names `gql()` refuses or does not support yet, and near misses.
#[cfg(feature = "gql")]
const GQL_FUNCTION_TOKENS: &[&str] = &[
    "element_id",
    "elementId",
    "labels",
    "type",
    "coalesce",
    "upper",
    "toUpper",
    "lower",
    "toLower",
    "char_length",
    "character_length",
    "size",
    "cardinality",
    "path_length",
    "length",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "collect_list",
    "collect",
    "toInteger",
    "toIntegerOrNull",
    "toFloat",
    "toFloatOrNull",
    "toBoolean",
    "toBooleanOrNull",
    "toString",
    "toStringOrNull",
    "left",
    "right",
    "substring",
    "trim",
    "btrim",
    "ltrim",
    "rtrim",
    "replace",
    "split",
    "reverse",
    "byte_length",
    "octet_length",
    "isEmpty",
    "nullif",
    "abs",
    "floor",
    "ceil",
    "ceiling",
    "round",
    "sign",
    "sqrt",
    "exp",
    "ln",
    "log",
    "log10",
    "power",
    "mod",
    "head",
    "last",
    "tail",
    "range",
    "keys",
    "properties",
    "startNode",
    "endNode",
    "nodes",
    "relationships",
    "TOINTEGER",
    "Substring",
    "rand",
    "randomUUID",
    "sin",
    "atan2",
    "pi",
    "date",
    "duration",
    "toIntegerList",
    "normalize",
    "all",
    "reduce",
    "elements",
    "percentileCont",
    "stDev",
    "exists",
    "id",
    "toInt",
    "substr",
    "lenght",
    "uper",
    "",
];

/// Arguments of the 1c call generator: NULL, numbers at the `i64` and float edges, texts with
/// spaces, UTF-8, number and boolean texts, lists, maps, the pattern's variables and their
/// properties, parameters.
#[cfg(feature = "gql")]
const GQL_FUNCTION_ARGS: &[&str] = &[
    "NULL",
    "0",
    "1",
    "-1",
    "2",
    "2.5",
    "-0.5",
    "1e308",
    "9223372036854775807",
    "-9223372036854775807 - 1",
    "''",
    "' '",
    "'a'",
    "'héllo'",
    "'日本語 🎉'",
    "' 42 '",
    "'0x1F'",
    "'1e3'",
    "'NaN'",
    "'-Infinity'",
    "'true'",
    "'HALF_EVEN'",
    "'half_up'",
    "TRUE",
    "[]",
    "[1, 'a', NULL]",
    "['-', '']",
    "[[1], {a: 1}]",
    "{a: 1, b: NULL}",
    "n",
    "m",
    "e",
    "p",
    "n.name",
    "n.age",
    "m.lang",
    "e.weight",
    "$n",
    "$name",
    "$missing",
];

/// Builds a random expression of nested function calls (depth-bounded).
#[cfg(feature = "gql")]
fn random_gql_call(rng: &mut Rng, depth: usize) -> String {
    if depth == 0 || rng.chance(30) {
        return rng.pick(GQL_FUNCTION_ARGS).to_string();
    }
    let arg = |rng: &mut Rng| random_gql_call(rng, depth - 1);
    match rng.below(12) {
        0 => {
            let target = *rng.pick(&[
                "INTEGER", "INT", "BIGINT", "FLOAT", "DOUBLE", "STRING", "BOOLEAN", "BOOL", "DATE",
                "LIST", "INTEGR",
            ]);
            format!("CAST({} AS {target})", arg(rng))
        }
        1 => {
            let side = *rng.pick(&["", "LEADING ", "TRAILING ", "BOTH "]);
            let character = if rng.chance(50) {
                format!("{} ", arg(rng))
            } else {
                String::new()
            };
            let from = if side.is_empty() && character.is_empty() && rng.chance(50) {
                String::new()
            } else {
                "FROM ".to_string()
            };
            format!("TRIM({side}{character}{from}{})", arg(rng))
        }
        2 => {
            let test = *rng.pick(&[
                "IS SOURCE OF",
                "IS NOT SOURCE OF",
                "IS DESTINATION OF",
                "IS NOT DESTINATION OF",
            ]);
            format!("{} {test} {}", arg(rng), arg(rng))
        }
        3 => format!("SUBSTRING({} FROM 1 FOR 2)", arg(rng)),
        4 => format!("{} || {}", arg(rng), arg(rng)),
        5 => format!("{} + {}", arg(rng), arg(rng)),
        _ => {
            let name = rng.pick(GQL_FUNCTION_TOKENS);
            let count = rng.below(5);
            let args: Vec<String> = (0..count).map(|_| arg(rng)).collect();
            format!("{name}({})", args.join(", "))
        }
    }
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_function_calls_never_panic() {
    let mut fatal = Fatal::default();
    let mut rng = Rng::new(0x6A1_C001);
    let mut texts: Vec<String> = Vec::new();
    // Every function with two random argument counts, on a row with a node, an edge and a
    // path (the unit test `stage_1c_calls_never_panic` covers every count on a test row).
    for name in GQL_FUNCTION_TOKENS {
        for count in [rng.below(5), rng.below(5)] {
            let args: Vec<String> = (0..count)
                .map(|_| rng.pick(GQL_FUNCTION_ARGS).to_string())
                .collect();
            texts.push(format!(
                "MATCH p = (n)-[e]->(m) RETURN {name}({}) AS x",
                args.join(", ")
            ));
        }
    }
    // Blow-ups: refused by the materialization limit before anything is built.
    texts.push("MATCH (n) RETURN range(0, 9223372036854775807) AS x".to_string());
    texts.push(
        "MATCH (n) RETURN range(-9223372036854775807 - 1, 9223372036854775807, 1) AS x".to_string(),
    );
    texts.push(format!(
        "MATCH (n) LET s = '{}' RETURN size(split(replace(s, '', s), '')) AS x",
        "x".repeat(50_000)
    ));
    texts.push(format!(
        "MATCH (n) RETURN toString(split('{}', '')) AS x",
        "é".repeat(50_000)
    ));
    texts.push(format!(
        "MATCH (n) RETURN {}n.name{} AS x",
        "reverse(".repeat(5_000),
        ")".repeat(5_000)
    ));
    // Random nested calls in every clause that evaluates expressions.
    for _ in 0..budget(150) {
        let expr = random_gql_call(&mut rng, 3);
        let query = match rng.below(7) {
            0 => format!("MATCH (n) WHERE {expr} RETURN n.name AS x"),
            1 => format!("MATCH (n) LET x = {expr} RETURN x"),
            2 => format!("MATCH (n)-[e]-(m) WITH n, e, {expr} AS x RETURN x, count(*) AS c"),
            3 => format!("MATCH (n) RETURN n.name AS x ORDER BY {expr}"),
            4 => format!("MATCH (n) OPTIONAL MATCH p = (n)-[e]->(m) RETURN {expr} AS x"),
            5 => format!("MATCH (n) RETURN collect_list({expr}) AS x, max({expr}) AS y"),
            _ => format!("MATCH p = (n)-[e]->{{1,2}}(m) RETURN DISTINCT {expr} AS x"),
        };
        texts.push(query);
    }
    // Mutations of valid 1c queries and a token soup of function names and arguments.
    const BASES: &[&str] = &[
        "MATCH (n) RETURN toInteger(' 42 '), CAST(n.age AS STRING), round(n.age / 7.0, 2, 'UP')",
        "MATCH (n) RETURN TRIM(LEADING 'm' FROM n.name), btrim(n.name, 'mo'), split(n.name, ['a', 'o'])",
        "MATCH p = (n)-[e]->(m) RETURN startNode(e), nodes(p), keys(n), properties(e), n IS SOURCE OF e",
        "MATCH (n) RETURN range(1, 10, 3), head(tail(range(1, 3))), trim([1, 2, 3], 1), log(2, 8)",
    ];
    for _ in 0..budget(100) {
        let base = rng.pick(BASES).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(16);
        let soup: Vec<&str> = (0..len)
            .map(|_| {
                if rng.chance(50) {
                    *rng.pick(GQL_FUNCTION_TOKENS)
                } else {
                    *rng.pick(&[
                        "(", ")", ",", "AS", "FROM", "IS", "SOURCE", "OF", "'x'", "1",
                    ])
                }
            })
            .collect();
        texts.push(format!("MATCH (n) RETURN {}", soup.join(" ")));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 1c functions");
}

// ---------------------------------------------------------------------------------------------
// GQL stage 2a: FOR / UNWIND (WITH ORDINALITY / OFFSET), queries without MATCH and the
// correlated lookup gql:lookup, with and without property indexes
// ---------------------------------------------------------------------------------------------

/// Valid stage 2a queries: the base documents of the 2a mutations.
#[cfg(feature = "gql")]
const GQL_2A_CORPUS: &[&str] = &[
    "FOR x IN [1, 2, 2, null] WITH ORDINALITY i RETURN x, i",
    "UNWIND $ids AS id MATCH (n) WHERE element_id(n) = id RETURN n.name",
    "FOR n IN $names MATCH (p:person {name: n})-[:created]->(s) RETURN n, s.name",
    "MATCH p = (a)-[:knows]->(b) FOR v IN nodes(p) WITH OFFSET o MATCH (v)-->(m) RETURN o, m",
    "RETURN 1 AS one, $n + 1 AS two",
    "LET k = 2 FOR x IN range(1, k) RETURN collect_list(x) AS l",
    "FILTER $n > 0 MATCH (a:person) FOR x IN [a.age, a.name] RETURN a, x",
    "MATCH (p:person) WITH p, collect_list(p.name) AS ns FOR n IN ns OPTIONAL MATCH \
     (q:person {name: n}) RETURN p, q",
    "WITH 3 AS three LIMIT 1 FOR x IN [three] RETURN x",
];

/// Tokens of stage 2a for the 2a token soup (a list of its own; the earlier lists stay as they
/// are).
#[cfg(feature = "gql")]
const GQL_2A_TOKENS: &[&str] = &[
    "FOR",
    "UNWIND",
    "IN",
    "AS",
    "WITH",
    "ORDINALITY",
    "OFFSET",
    "x",
    "i",
    "n",
    "[1, 2]",
    "[]",
    "null",
    "$ids",
    "$names",
    "nodes(p)",
    "MATCH",
    "OPTIONAL",
    "(n)",
    "(n:person {name: x})",
    "WHERE",
    "element_id(n) = x",
    "n.name = x",
    "LET",
    "FILTER",
    "RETURN",
    "*",
    "count(*)",
    "collect_list(",
    ")",
    ",",
    "=",
    "1",
];

/// A random stage 2a query that parses: a start (nothing, a pattern, a path, an `OPTIONAL
/// MATCH`, `LET`, `FILTER`), one to three `FOR` / `UNWIND` clauses over constant, parameter,
/// per-row and element lists, each optionally followed by a `MATCH` that looks a node up by the
/// item (by id, an inline property, a `WHERE` equality, an E3 continuation), then a `RETURN`.
#[cfg(feature = "gql")]
fn random_gql_2a_query(rng: &mut Rng) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut values: Vec<String> = Vec::new();
    let mut nodes: Vec<String> = Vec::new();
    let mut path = false;
    match rng.below(7) {
        0 => {}
        1 => {
            parts.push("MATCH (a:person)".into());
            nodes.push("a".into());
        }
        2 => {
            parts.push("MATCH p = (a)-[e]->(b)".into());
            nodes.extend(["a".to_string(), "b".to_string()]);
            path = true;
        }
        3 => {
            parts.push("OPTIONAL MATCH (a:nope)".into());
            nodes.push("a".into());
        }
        4 => {
            parts.push("LET k = 2".into());
            values.push("k".into());
        }
        5 => parts.push("FILTER $n > 0".into()),
        _ => {
            parts.push("MATCH (a:person) WITH a, count(*) AS c".into());
            nodes.push("a".into());
            values.push("c".into());
        }
    }
    let mut items = Vec::new();
    for f in 0..1 + rng.below(3) {
        let mut lists = vec![
            "[1, 2, 2, null]".to_string(),
            "$ids".into(),
            "$names".into(),
            "null".into(),
            "42".into(),
            "'marko'".into(),
            "range(1, $n)".into(),
            "[]".into(),
            "[[1, 2], [3]]".into(),
            "['1', 1, 1.0, true]".into(),
        ];
        if let Some(v) = values.last() {
            lists.push(format!("[{v}, {v} + 1]"));
        }
        if let Some(a) = nodes.last() {
            lists.push(format!("[{a}]"));
            lists.push(format!("[{a}.name, {a}.age, element_id({a})]"));
        }
        if path {
            lists.push("nodes(p)".into());
            lists.push("relationships(p)".into());
        }
        let list = rng.pick(&lists).clone();
        let x = format!("x{f}");
        let ordinal = format!("i{f}");
        match rng.below(4) {
            0 => parts.push(format!("UNWIND {list} AS {x}")),
            1 => {
                parts.push(format!("FOR {x} IN {list} WITH ORDINALITY {ordinal}"));
                values.push(ordinal);
            }
            2 => {
                parts.push(format!("FOR {x} IN {list} WITH OFFSET {ordinal}"));
                values.push(ordinal);
            }
            _ => parts.push(format!("FOR {x} IN {list}")),
        }
        let n = format!("n{f}");
        let lookup = match rng.below(8) {
            0 => format!("MATCH ({n}) WHERE element_id({n}) = {x}"),
            1 => format!("MATCH ({n}:person {{name: {x}}})"),
            2 => format!("MATCH ({n}:person|software) WHERE {n}.name = {x}"),
            3 => format!("OPTIONAL MATCH ({n}:person {{age: {x}}})"),
            4 => format!("MATCH ({x})-->({n})"),
            5 => format!("MATCH ({n}:person)-[:knows]->(:person {{name: {x}}})"),
            _ => String::new(),
        };
        if lookup.is_empty() {
            values.push(x.clone());
        } else {
            parts.push(lookup);
            nodes.push(n);
        }
        items.push(x);
        if rng.chance(20) {
            parts.push(format!("FILTER {} IS NOT NULL", rng.pick(&items)));
        }
    }
    let x = rng.pick(&items).clone();
    let ret = match rng.below(6) {
        0 => "RETURN count(*) AS c".to_string(),
        1 => format!("RETURN {x} AS v, count(*) AS c"),
        2 => "RETURN *".to_string(),
        3 => format!("RETURN DISTINCT {x} AS v"),
        4 => format!("RETURN collect_list({x}) AS l"),
        _ => match nodes.last() {
            Some(node) => format!("RETURN {node}.name AS name, {x} AS v ORDER BY name LIMIT 5"),
            None => format!("RETURN {x} AS v"),
        },
    };
    parts.push(ret);
    parts.join(" ")
}

/// The modern graph, with property indexes on `person.name`, `person.age` and `software.name`
/// when `indexed` (the lookups of the 2a family then read them).
#[cfg(feature = "gql")]
fn gql_2a_graph(indexed: bool) -> TraversalGraph {
    use graphersal::catalog::PropertyIndexDefinition;
    use graphersal::storage::ElementKind;
    let mut graph = TraversalGraph::tinkerpop_modern();
    if indexed {
        for (label, key) in [("person", "name"), ("person", "age"), ("software", "name")] {
            let _ = graph.traversal_mut().create_index(
                &format!("{label}_{key}"),
                PropertyIndexDefinition::new(ElementKind::Vertex, label, key),
            );
        }
    }
    graph
}

/// The parameters of the 2a family.
#[cfg(feature = "gql")]
fn gql_2a_params() -> Vec<(&'static str, ElementProperty)> {
    vec![
        ("n", ElementProperty::from(2)),
        (
            "ids",
            ElementProperty::Array(vec![
                ElementProperty::from("1"),
                ElementProperty::from("4"),
                ElementProperty::from("99"),
                ElementProperty::from(4),
                ElementProperty::Null,
            ]),
        ),
        (
            "names",
            ElementProperty::Array(vec![
                ElementProperty::from("marko"),
                ElementProperty::from("lop"),
                ElementProperty::from("nobody"),
                ElementProperty::from("marko"),
            ]),
        ),
    ]
}

/// The sorted rows of `query` on the modern graph (indexed or not), or the error kind.
#[cfg(feature = "gql")]
fn gql_2a_rows(query: &str, indexed: bool) -> Result<Vec<String>, String> {
    let graph = gql_2a_graph(indexed);
    match graph.traversal().gql_with(query, gql_2a_params()).to_list() {
        Ok(rows) => {
            let mut rows: Vec<String> = rows.iter().map(|row| format!("{row:?}")).collect();
            rows.sort();
            Ok(rows)
        }
        Err(err) => Err(match err.root_cause() {
            TraverserError::Gql { kind, message, .. } => format!("{kind:?}: {message}"),
            other => other.to_string(),
        }),
    }
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_2a_queries_never_panic_and_indexes_never_change_rows() {
    let mut fatal = Fatal::default();
    let mut syntax = Vec::new();
    let mut differences = Vec::new();
    let mut rng = Rng::new(0x6A2_A001);
    let mut generated = Vec::new();
    for _ in 0..budget(150) {
        generated.push(random_gql_2a_query(&mut rng));
    }
    // Every generated query parses; enough of them run; a property index changes the plan
    // (gql:lookup instead of a scan), never the rows or the error.
    let mut ran = 0;
    let mut checked = generated.clone();
    checked.extend(GQL_2A_CORPUS.iter().map(|s| s.to_string()));
    for query in &checked {
        let text = query.clone();
        let outcome = guarded(WATCHDOG, move || {
            (gql_2a_rows(&text, false), gql_2a_rows(&text, true))
        });
        match outcome {
            Ok((plain, indexed)) => {
                match &plain {
                    Ok(_) => ran += 1,
                    Err(message) if message.starts_with("Syntax") => {
                        syntax.push(format!("{query}: {message}"));
                    }
                    Err(_) => {}
                }
                if plain != indexed {
                    differences.push(format!(
                        "{query}\n  without index: {plain:?}\n  with indexes:  {indexed:?}"
                    ));
                }
            }
            Err(outcome) => fatal.check(query, Err::<(), _>(outcome)),
        }
    }
    assert!(
        ran * 2 >= checked.len(),
        "only {ran} of {} generated 2a queries ran",
        checked.len()
    );
    assert!(
        syntax.is_empty(),
        "generated 2a queries that do not parse:\n{}",
        syntax.join("\n")
    );
    assert!(
        differences.is_empty(),
        "{} queries whose rows depend on an index:\n{}",
        differences.len(),
        differences.join("\n")
    );
    let mut texts = generated;
    texts.extend(GQL_2A_CORPUS.iter().map(|s| s.to_string()));
    // Long FOR chains and big lists: bounded errors or results, never a stack overflow.
    texts.push(format!(
        "{}RETURN count(*) AS c",
        (0..200)
            .map(|i| format!("FOR x{i} IN [1] "))
            .collect::<String>()
    ));
    texts.push(format!(
        "FOR x IN [{}] RETURN count(*) AS c",
        vec!["1"; 5_000].join(", ")
    ));
    texts.push("FOR x IN range(1, 9223372036854775807) RETURN x LIMIT 1".to_string());
    texts.push(format!(
        "FOR x IN {}1{} RETURN x",
        "[".repeat(300),
        "]".repeat(300)
    ));
    texts.push(format!(
        "FOR id IN [{}] MATCH (n) WHERE element_id(n) = id RETURN count(*) AS c",
        (0..2_000)
            .map(|i| format!("'{i}'"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    for _ in 0..budget(100) {
        let base = rng.pick(GQL_2A_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(20);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_2A_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "FOR x IN [1, 2]");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 2a");
}

// ---------------------------------------------------------------------------------------------
// GQL stage 2b: EXISTS subqueries and pattern predicates (fast path and expression hook)
// ---------------------------------------------------------------------------------------------

/// Valid stage 2b queries: the base documents of the 2b mutations.
#[cfg(feature = "gql")]
const GQL_2B_CORPUS: &[&str] = &[
    "MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS n",
    "MATCH (a:person), (b:person) WHERE (a)-[:knows]->(b) RETURN a.name, b.name",
    "MATCH (p) WHERE EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = 'java' } RETURN p",
    "MATCH (p) WHERE EXISTS ((p)<-[:knows]-()) OR p.age > 30 RETURN p.name AS n",
    "MATCH (p:person) RETURN p.name AS n, EXISTS { (p)-->() } AS e ORDER BY EXISTS { (p)-[:knows]->() }, n",
    "MATCH (p:person) RETURN count(CASE WHEN (p)-->() THEN 1 END) AS c, (p)<--() AS i",
    "MATCH (p) OPTIONAL MATCH (p)-[:created]->(s) RETURN p, EXISTS { (s)<-[:created]-(:person {name: 'peter'}) } AS x",
    "MATCH (p) WHERE EXISTS { MATCH (p)-->(q) WHERE EXISTS { MATCH (q)-->(r) WHERE NOT (r)-->(p) } } RETURN p",
    "FOR b IN [EXISTS { MATCH (s:software) }, (a)-->()] RETURN b",
    "MATCH (p:person) LET k = (p)-[:knows]->() FILTER k OR NOT k RETURN p.name AS n, k",
    "MATCH (p:person) WITH p, count(*) AS c ORDER BY EXISTS { (p)-[:created]->() } RETURN p.name AS n",
    "MATCH (a)-[e]->(b) WHERE EXISTS { MATCH (a)-[e]->(b) MATCH (b)-[f]->() WHERE f <> e } RETURN e",
];

/// Tokens of stage 2b for the 2b token soup (a list of its own; the earlier lists stay as they
/// are).
#[cfg(feature = "gql")]
const GQL_2B_TOKENS: &[&str] = &[
    "EXISTS",
    "EXISTS {",
    "EXISTS (",
    "exists(",
    "COUNT {",
    "{",
    "}",
    "(",
    ")",
    "(p)",
    "(q)",
    "()",
    "(p)-->()",
    "(p)-[:knows]->(q)",
    "-->",
    "<--",
    "--",
    "-",
    "-[:created]->",
    "<-[e]-",
    "{1,}",
    "MATCH",
    "OPTIONAL",
    "WHERE",
    "NOT",
    "OR",
    "AND",
    "CASE WHEN",
    "THEN",
    "END",
    "RETURN",
    "ORDER BY",
    "LET k =",
    "FOR x IN [",
    "]",
    "count(",
    "p.age > 30",
    ",",
    "1",
];

/// A random subquery over the node variables `nodes` (the first is the main import): one of
/// the four forms, sometimes negated, sometimes nested, sometimes joined with a plain
/// condition. A pattern predicate in a `value` position is written `EXISTS { .. }` (stage 3f:
/// a pattern alone is a condition only where `WHERE` expects one).
#[cfg(feature = "gql")]
fn random_gql_2b_predicate(
    rng: &mut Rng,
    nodes: &[String],
    edge: Option<&str>,
    depth: usize,
    value: bool,
) -> String {
    let x = nodes.first().cloned().unwrap_or_else(|| "x".into());
    let y = nodes.get(1).cloned();
    // Pattern-predicate bodies: no new named variable.
    let mut plain = vec![
        format!("({x})-->()"),
        format!("({x})<-[:knows]-()"),
        format!("({x})-[:created]->(:software)"),
        format!("({x})-[:knows]->{{1,2}}()"),
        format!("({x})--(:person {{name: 'josh'}})"),
    ];
    if let Some(y) = &y {
        plain.push(format!("({x})--({y})"));
        plain.push(format!("({y})<-[:created]-({x})"));
    }
    if let Some(e) = edge {
        plain.push(format!("({x})-[{e}]-()"));
    }
    let mut bodies = plain.clone();
    bodies.extend([
        format!("({x})-[r]->(o) WHERE r.weight > 0.4"),
        format!("({x})-[:created]->(s) WHERE s.lang = 'java'"),
        "(s:software {name: 'lop'})".to_string(),
        format!("({x})-[:knows*1..2]->(m) WHERE m.age > 30"),
        format!("q = ({x})-->() WHERE length(q) = 1"),
    ]);
    if depth < 2 && rng.chance(25) {
        let inner =
            random_gql_2b_predicate(rng, &["o".to_string(), x.clone()], None, depth + 1, false);
        bodies.push(format!("({x})-->(o) WHERE {inner}"));
    }
    let form = rng.below(4);
    let core = if form == 0 && value {
        format!("EXISTS {{ {} }}", rng.pick(&plain))
    } else if form == 0 {
        rng.pick(&plain).clone()
    } else {
        let body = rng.pick(&bodies).clone();
        match form {
            1 => format!("EXISTS {{ MATCH {body} }}"),
            2 if rng.chance(50) => format!("EXISTS {{ MATCH {body} MATCH ({x})--() }}"),
            2 => format!("EXISTS {{ {body} }}"),
            _ => format!("EXISTS ({body})"),
        }
    };
    let core = if rng.chance(40) {
        format!("NOT {core}")
    } else {
        core
    };
    match rng.below(6) {
        0 => format!("{core} AND {x}.age > 30"),
        1 => format!("({core} OR {x}.name = 'josh')"),
        _ => core,
    }
}

/// A random stage 2b query in four forms with the same rows: the subquery as a `WHERE`
/// condition (the `filter()` / `not()` fast path when it stands alone), the same under `CASE`
/// (the `gql:filter` hook), as a `FILTER` (fast path) and through a `FOR` list (the `gql:for`
/// hook). Returns `[where, where_case, filter, for_filter]`; pairs (0, 1) and (2, 3) must agree.
#[cfg(feature = "gql")]
fn random_gql_2b_queries(rng: &mut Rng) -> [String; 4] {
    let (head, nodes, edge): (&str, &[&str], Option<&str>) = match rng.below(7) {
        0 => ("MATCH (a:person)", &["a"], None),
        1 => ("MATCH (a)-[e]->(b)", &["a", "b"], Some("e")),
        2 => (
            "MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(b)",
            &["b", "a"],
            None,
        ),
        3 => ("MATCH (a:person) WITH a, count(*) AS c", &["a"], None),
        4 => ("FOR n IN $names MATCH (a {name: n})", &["a"], None),
        5 => ("MATCH p = (a)-[:knows]->(b)", &["b", "a"], None),
        _ => ("MATCH (a:software), (b:person)", &["a", "b"], None),
    };
    let nodes: Vec<String> = nodes.iter().map(|n| n.to_string()).collect();
    let pred = random_gql_2b_predicate(rng, &nodes, edge, 0, false);
    let x = &nodes[0];
    let other = random_gql_2b_predicate(rng, &nodes, None, 1, true);
    let ret = match rng.below(6) {
        0 => "RETURN count(*) AS c".to_string(),
        1 => format!("RETURN {x}.name AS n"),
        2 => format!("RETURN DISTINCT {x} AS v"),
        3 => format!("RETURN {x}.name AS n, {other} AS e"),
        4 => format!("RETURN {x}.name AS n ORDER BY {other}, n LIMIT 3"),
        _ => {
            format!("RETURN count(CASE WHEN {other} THEN 1 END) AS c, collect_list({x}.name) AS l")
        }
    };
    [
        format!("{head} WHERE {pred} {ret}"),
        format!("{head} WHERE CASE WHEN {pred} THEN true ELSE false END {ret}"),
        format!("{head} FILTER {pred} {ret}"),
        // The list item is a value: the condition under a searched CASE (stage 3f).
        format!("{head} FOR t IN [CASE WHEN {pred} THEN true ELSE false END] FILTER t {ret}"),
    ]
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_2b_queries_never_panic_and_fast_path_equals_hook() {
    let mut fatal = Fatal::default();
    let mut syntax = Vec::new();
    let mut differences = Vec::new();
    let mut rng = Rng::new(0x6A2_B001);
    let mut generated = Vec::new();
    for _ in 0..budget(120) {
        generated.push(random_gql_2b_queries(&mut rng));
    }
    // Every generated query parses; enough of them run; the fast path and the hook give the
    // same rows (or the same error), with and without property indexes.
    let mut ran = 0;
    for forms in &generated {
        let texts = forms.clone();
        let outcome = guarded(WATCHDOG, move || {
            texts
                .iter()
                .map(|q| (gql_2a_rows(q, false), gql_2a_rows(q, true)))
                .collect::<Vec<_>>()
        });
        match outcome {
            Ok(results) => {
                for (query, (plain, indexed)) in forms.iter().zip(&results) {
                    match plain {
                        Ok(_) => ran += 1,
                        Err(message) if message.starts_with("Syntax") => {
                            syntax.push(format!("{query}: {message}"));
                        }
                        Err(_) => {}
                    }
                    if plain != indexed {
                        differences.push(format!(
                            "{query}\n  without index: {plain:?}\n  with indexes:  {indexed:?}"
                        ));
                    }
                }
                for (a, b) in [(0, 1), (2, 3)] {
                    if results[a].0 != results[b].0 {
                        differences.push(format!(
                            "{}\n  {:?}\n{}\n  {:?}",
                            forms[a], results[a].0, forms[b], results[b].0
                        ));
                    }
                }
            }
            Err(outcome) => fatal.check(&forms[0], Err::<(), _>(outcome)),
        }
    }
    let total = generated.len() * 4;
    assert!(
        ran * 2 >= total,
        "only {ran} of {total} generated 2b queries ran"
    );
    assert!(
        syntax.is_empty(),
        "generated 2b queries that do not parse:\n{}",
        syntax.join("\n")
    );
    assert!(
        differences.is_empty(),
        "{} 2b queries whose rows depend on the form or an index:\n{}",
        differences.len(),
        differences.join("\n")
    );
    let mut texts: Vec<String> = generated.into_iter().flatten().collect();
    texts.extend(GQL_2B_CORPUS.iter().map(|s| s.to_string()));
    // Every host position, nested EXISTS and the bounds: a result or a bounded error.
    let pred = "EXISTS { MATCH (p)-[:knows]->{1,}(q) WHERE (q)-->() }";
    for host in [
        format!("MATCH (p) WHERE {pred} OR p.age > 1 RETURN p"),
        format!("MATCH (p WHERE {pred}) RETURN p"),
        format!("MATCH (p)-[e WHERE {pred}]->() RETURN e"),
        format!("MATCH (p) RETURN {pred} AS x ORDER BY {pred}"),
        format!("MATCH (p) RETURN {pred} AS k, count(CASE WHEN {pred} THEN 1 END) AS c"),
        format!("MATCH (p) FOR x IN [{pred}, NOT {pred}] RETURN x"),
        format!("MATCH (p) LET k = {pred} WITH p, k WHERE k RETURN p"),
        format!("MATCH (p) WITH p, count(*) AS c WHERE {pred} RETURN p"),
        format!("MATCH (p) OPTIONAL MATCH (p)-->(q) WHERE {pred} RETURN p, q"),
        format!("MATCH (p) RETURN CASE WHEN {pred} THEN 1 WHEN NOT {pred} THEN 2 END AS c"),
    ] {
        texts.push(host);
    }
    let mut nested = "(n)-->()".to_string();
    for _ in 0..300 {
        nested = format!("EXISTS {{ MATCH (n) WHERE {nested} }}");
    }
    texts.push(format!("MATCH (n) WHERE {nested} RETURN n"));
    texts.push(format!(
        "MATCH (n) WHERE {} RETURN n",
        vec!["(n)-->()"; 70].join(" OR ")
    ));
    texts.push(format!(
        "MATCH (n) LET a = EXISTS {{ (n)-->() }} LET b = EXISTS {{ MATCH (n)-->(q) WHERE {} }} \
         FILTER {} RETURN n",
        vec!["a"; 50].join(" AND "),
        vec!["b"; 50].join(" AND ")
    ));
    texts.push(format!(
        "MATCH (n) WHERE EXISTS {{ MATCH {} }} RETURN n",
        "(a)-->".repeat(80) + "(b)"
    ));
    texts.push(format!(
        "MATCH (n) WHERE {}(n)-->() RETURN n",
        "NOT ".repeat(5_000)
    ));
    for _ in 0..budget(100) {
        let base = rng.pick(GQL_2B_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(20);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_2B_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (p) WHERE");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 2b");
}

// ---------------------------------------------------------------------------------------------
// GQL stage 2c: map projections, list comprehensions and pattern comprehensions
// ---------------------------------------------------------------------------------------------

/// Valid stage 2c queries: the base documents of the 2c mutations.
#[cfg(feature = "gql")]
const GQL_2C_CORPUS: &[&str] = &[
    "MATCH (p:person) RETURN p {.name, .age} AS m",
    "MATCH (p:person) RETURN p {.*, friends: [(p)-[:knows]->(f) | f {.name}]} AS m",
    "MATCH (n) RETURN n {.name, k: n.age + 1, n} AS m",
    "RETURN [x IN [1, 2, 3, 4] WHERE x > 2 | x * 10] AS l",
    "MATCH (p:person) RETURN [x IN [p.age, 1, NULL] WHERE x > 1] AS l",
    "MATCH (p:person) RETURN [(p)-->(b) WHERE b.age > 30 | b.name] AS l",
    "MATCH (p) RETURN [q = (p)-[:knows]->{1,2}(f) | length(q)] AS l",
    "MATCH (p:person) WHERE size([(p)-[:created]->(s) | s]) > 0 RETURN p.name AS n",
    "MATCH (p:person) RETURN p.name AS n ORDER BY size([(p)<--(x) | x]) DESC, n",
    "MATCH (p) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name, [(s)<-[:created]-(c) | c.name] AS l",
    "MATCH (p:person) FOR f IN [(p)-[:knows]->(f2) | f2 {.name}] RETURN p.name, f",
    "MATCH (p:person) RETURN [(p)-->(b) | [(b)-->(c) | [y IN [c.name] | y]]] AS l",
    "MATCH (p:person) WITH p, [(p)-->(b) | b.name] AS l RETURN l, count(*) AS c",
    "MATCH (p:person) RETURN collect_list(p {.name, n: size([(p)--(o) | o])}) AS l",
];

/// Tokens of stage 2c for the 2c token soup (a list of its own; the earlier lists stay as they
/// are).
#[cfg(feature = "gql")]
const GQL_2C_TOKENS: &[&str] = &[
    "[",
    "]",
    "{",
    "}",
    "(",
    ")",
    "|",
    "x IN",
    "IN",
    "[x IN [1, 2] |",
    "[(p)-->(b) |",
    "p {",
    ".name",
    ".*",
    ":",
    "k:",
    "p",
    "b",
    "x",
    "(p)",
    "(b)",
    "-->",
    "<--",
    "-[:knows]->",
    "q =",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "EXISTS {",
    "MATCH",
    "RETURN",
    "FOR f IN",
    "ORDER BY",
    "size(",
    ",",
    "1",
    "NULL",
];

/// A random pattern comprehension body for the 2c family over the import `x`: the pattern
/// (binding `y`, never a name of the heads), an optional condition and the `|` expression.
#[cfg(feature = "gql")]
fn random_gql_2c_body(rng: &mut Rng, x: &str) -> (String, String, String) {
    let (pattern, own): (String, &[&str]) = match rng.below(8) {
        0 => (format!("({x})-->(y)"), &[]),
        1 => (format!("({x})<-[:knows]-(y)"), &[]),
        2 => (format!("({x})-[:created]->(y:software)"), &[]),
        3 => (format!("({x})-[:knows]->{{1,2}}(y)"), &[]),
        4 => (format!("({x})--(y:person)"), &[]),
        5 => (
            format!("({x})-[r]->(y)"),
            &["r.weight > 0.4", "r.weight < 0.9"],
        ),
        6 => (format!("q = ({x})-->(y)"), &["length(q) = 1"]),
        _ => ("(y:software)<-[:created]-(:person)".to_string(), &[]),
    };
    let mut conditions = vec![
        "y.age > 30".to_string(),
        "y.name <> 'lop'".to_string(),
        "y.name STARTS WITH 'j'".to_string(),
        format!("{x}.age > 30"),
        "EXISTS { (y)-->() }".to_string(),
        "size([(y)-->(z) | z]) > 0".to_string(),
        "size([w IN [1, 2, 3] WHERE w > 1]) = 2".to_string(),
    ];
    conditions.extend(own.iter().map(|c| c.to_string()));
    let condition = match rng.below(3) {
        0 => String::new(),
        1 => format!(" WHERE {}", rng.pick(&conditions)),
        _ => format!(
            " WHERE {} OR {}",
            rng.pick(&conditions),
            rng.pick(&conditions)
        ),
    };
    let map = rng
        .pick(&[
            "y.name",
            "y",
            "y {.name}",
            "y {.*, k: 1}",
            "[(y)-->(z) | z.name]",
            "[w IN [y.age, y.name] WHERE w IS NOT NULL]",
            "size([(y)--(z) | z])",
        ])
        .to_string();
    (pattern, condition, map)
}

/// A random stage 2c query in six forms, three pairs with the same rows:
/// - `size()` of a pattern comprehension vs `OPTIONAL MATCH` + `count()` grouped by the head's
///   variables;
/// - `FOR` over a pattern comprehension vs a later `MATCH` of its pattern;
/// - a map projection vs the record constructor (`NULL` for a `NULL` target).
#[cfg(feature = "gql")]
fn random_gql_2c_queries(rng: &mut Rng) -> [String; 6] {
    let (head, vars, imports): (&str, &[&str], &[&str]) = match rng.below(7) {
        0 => ("MATCH (a:person)", &["a"], &["a"]),
        1 => ("MATCH (a)-[e]->(b)", &["a", "e", "b"], &["a", "b"]),
        2 => (
            "MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(b)",
            &["a", "b"],
            &["b", "a"],
        ),
        3 => (
            "MATCH (a:person) WITH a, count(*) AS c",
            &["a", "c"],
            &["a"],
        ),
        4 => (
            "FOR n IN ['marko', 'lop', 'nobody'] MATCH (a {name: n})",
            &["n", "a"],
            &["a"],
        ),
        5 => ("MATCH p = (a)-[:knows]->(b)", &["a", "b"], &["b", "a"]),
        _ => ("MATCH (a:software), (b:person)", &["a", "b"], &["a", "b"]),
    };
    let x = *rng.pick(imports);
    let keys = vars.join(", ");
    let (pattern, condition, map) = random_gql_2c_body(rng, x);
    let comprehension = format!("[{pattern}{condition} | {map}]");
    [
        format!("{head} RETURN {keys}, size({comprehension}) AS s"),
        format!("{head} OPTIONAL MATCH {pattern}{condition} RETURN {keys}, count(y) AS s"),
        format!("{head} FOR v IN {comprehension} RETURN {keys}, v"),
        format!("{head} MATCH {pattern}{condition} RETURN {keys}, {map} AS v"),
        format!("{head} RETURN {keys}, {x} {{.name, l: {comprehension}}} AS m"),
        format!(
            "{head} RETURN {keys}, CASE WHEN {x} IS NULL THEN NULL ELSE {{name: {x}.name, l: \
             {comprehension}}} END AS m"
        ),
    ]
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_2c_queries_never_panic_and_comprehensions_equal_their_rewrites() {
    let mut fatal = Fatal::default();
    let mut syntax = Vec::new();
    let mut differences = Vec::new();
    let mut rng = Rng::new(0x6A2_C001);
    let mut generated = Vec::new();
    for _ in 0..budget(100) {
        generated.push(random_gql_2c_queries(&mut rng));
    }
    // Every generated query parses; enough of them run; each pair gives the same rows (or the
    // same error), with and without property indexes.
    let mut ran = 0;
    for forms in &generated {
        let texts = forms.clone();
        let outcome = guarded(WATCHDOG, move || {
            texts
                .iter()
                .map(|q| (gql_2a_rows(q, false), gql_2a_rows(q, true)))
                .collect::<Vec<_>>()
        });
        match outcome {
            Ok(results) => {
                for (query, (plain, indexed)) in forms.iter().zip(&results) {
                    match plain {
                        Ok(_) => ran += 1,
                        Err(message) if message.starts_with("Syntax") => {
                            syntax.push(format!("{query}: {message}"));
                        }
                        Err(_) => {}
                    }
                    if plain != indexed {
                        differences.push(format!(
                            "{query}\n  without index: {plain:?}\n  with indexes:  {indexed:?}"
                        ));
                    }
                }
                for (a, b) in [(0, 1), (2, 3), (4, 5)] {
                    if results[a].0 != results[b].0 {
                        differences.push(format!(
                            "{}\n  {:?}\n{}\n  {:?}",
                            forms[a], results[a].0, forms[b], results[b].0
                        ));
                    }
                }
            }
            Err(outcome) => fatal.check(&forms[0], Err::<(), _>(outcome)),
        }
    }
    let total = generated.len() * 6;
    assert!(
        ran * 4 >= total * 3,
        "only {ran} of {total} generated 2c queries ran"
    );
    assert!(
        syntax.is_empty(),
        "generated 2c queries that do not parse:\n{}",
        syntax.join("\n")
    );
    assert!(
        differences.is_empty(),
        "{} 2c queries whose rows depend on the form or an index:\n{}",
        differences.len(),
        differences.join("\n")
    );
    let mut texts: Vec<String> = generated.into_iter().flatten().collect();
    texts.extend(GQL_2C_CORPUS.iter().map(|s| s.to_string()));
    // Every host position, unbounded quantifiers, nesting and the bounds: a result or a
    // bounded error.
    let pc = "[(p)-[:knows]->{1,}(q) WHERE (q)-->() | q {.name, l: [(q)-->(r) | r.name]}]";
    for host in [
        format!("MATCH (p) WHERE size({pc}) > 0 OR p.age > 1 RETURN p"),
        format!("MATCH (p WHERE size({pc}) > 0) RETURN p"),
        format!("MATCH (p)-[e WHERE size({pc}) > 0]->() RETURN e"),
        format!("MATCH (p) RETURN {pc} AS x ORDER BY size({pc})"),
        format!("MATCH (p) RETURN {pc} AS k, collect_list({pc}) AS c"),
        format!("MATCH (p) FOR x IN {pc} RETURN x"),
        format!("MATCH (p) LET k = {pc} WITH p, k WHERE size(k) > 0 RETURN p, k"),
        format!("MATCH (p) WITH p, count(*) AS c WHERE size({pc}) > 0 RETURN p"),
        format!("MATCH (p) OPTIONAL MATCH (p)-->(o) WHERE size({pc}) > 0 RETURN p, o"),
        format!("MATCH (p) RETURN p {{.*, l: {pc}, m: [x IN {pc} WHERE x IS NOT NULL]}} AS m"),
        format!("MATCH (p) RETURN [x IN [1, 2] | {pc}] AS l"),
        format!("MATCH (p) RETURN [(p)-->(o) | [x IN {pc} | x]] AS l"),
        format!("MATCH (p) WHERE EXISTS {{ MATCH (p)-->(o) WHERE size({pc}) > 0 }} RETURN p"),
    ] {
        texts.push(host);
    }
    for (open, close) in [
        ("[(n)-->(m) | ", "]"),
        ("[x IN [1] WHERE ", " | x]"),
        ("n {k: ", "}"),
        ("[(n)-->(m) WHERE size(", ") > 0 | m]"),
    ] {
        texts.push(format!(
            "MATCH (n) RETURN {}1{} AS x",
            open.repeat(300),
            close.repeat(300)
        ));
    }
    // A chain of 40 comprehensions, each from the previous one's element.
    let mut nested = "m40.name".to_string();
    for i in (0..40).rev() {
        nested = format!("[(m{i})-->(m{}) | {nested}]", i + 1);
    }
    texts.push(format!("MATCH (m0) RETURN {nested} AS x"));
    texts.push(format!(
        "MATCH (n) RETURN {} AS x",
        vec!["size([(n)-->(m) | m])"; 70].join(" + ")
    ));
    texts.push(format!(
        "MATCH (n) LET a = size([(n)-->(m) | m]) > 0 LET b = EXISTS {{ MATCH (n)-->(q) WHERE {} \
         }} FILTER {} RETURN n",
        vec!["a"; 50].join(" AND "),
        vec!["b"; 50].join(" AND ")
    ));
    texts.push(format!(
        "MATCH (n) RETURN [{} | b] AS x",
        "(a)-->".repeat(80) + "(b)"
    ));
    texts.push(format!(
        "MATCH (n) RETURN n {{{}}} AS m",
        (0..2_000)
            .map(|i| format!("k{i}: {i}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    texts.push(format!(
        "RETURN [x IN [{}] WHERE x > 1 | [y IN [x, x] | y * x]] AS l",
        vec!["1"; 5_000].join(", ")
    ));
    texts.push("RETURN [x IN range(1, 9223372036854775807) | x] AS l".to_string());
    texts.push("MATCH (a), (b), (c) RETURN [(a)--(d)--(e) | [d, e, b, c]] AS l".to_string());
    for _ in 0..budget(100) {
        let base = rng.pick(GQL_2C_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(20);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_2C_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (p) RETURN");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 2c");
}

// ---------------------------------------------------------------------------------------------
// GQL stage 2d.T5d: the list predicates all / any / none / single and reduce (D23)
// ---------------------------------------------------------------------------------------------

/// Valid queries with the list predicates and `reduce`: the base documents of their mutations.
#[cfg(feature = "gql")]
const GQL_2D_LIST_FUNCTION_CORPUS: &[&str] = &[
    "RETURN all(x IN [1, 2, NULL] WHERE x > 0) AS a, any(x IN [] WHERE x) AS b",
    "MATCH (p:person) WHERE none(x IN [(p)-->(y) | y.age] WHERE x > 30) RETURN p.name AS n",
    "MATCH (p:person) RETURN single(x IN [p.age, 29] WHERE x = 29) AS s",
    "MATCH (p:person) RETURN reduce(s = 0, x IN [(p)-->(y) | y] | s + 1) AS n",
    "MATCH (p:person) RETURN reduce(s = p.age, x IN [1, 2] | s * x) AS n ORDER BY n",
    "MATCH (p:person) RETURN reduce(s = [], x IN collect_list(p.name) | [x] + s) AS l",
    "MATCH (p:person) RETURN [x IN [1, 2] WHERE all(y IN [x, p.age] WHERE y > 0) | x] AS l",
    "MATCH (p:person) FOR v IN reduce(s = [], x IN [1, 2] | s + [x * p.age]) RETURN v",
    "MATCH (p:person) LET k = any(x IN [p.age] WHERE x > 30) FILTER k RETURN p",
];

/// Tokens of the list functions for their token soup (a list of its own).
#[cfg(feature = "gql")]
const GQL_2D_LIST_FUNCTION_TOKENS: &[&str] = &[
    "all(",
    "any(",
    "none(",
    "single(",
    "reduce(",
    "reduce(s = 0,",
    "s =",
    "=",
    "x IN",
    "IN",
    "WHERE",
    "|",
    "(",
    ")",
    "[",
    "]",
    ",",
    "x",
    "s",
    "s + x",
    "p.age",
    "[1, NULL]",
    "[(p)-->(y) | y]",
    "NULL",
    "1",
    "AND",
    "NOT",
    "IS NULL",
    "RETURN",
];

/// A random list function and its rewrite into list comprehensions, `CASE` and `size()`, with
/// the same value on every row (`NULL` lists, `NULL` items and `NULL` conditions included; the
/// conditions never fail, so the early return of a decided predicate is not observable).
#[cfg(feature = "gql")]
fn random_gql_2d_list_function_queries(rng: &mut Rng) -> [String; 2] {
    let (head, keys): (&str, &str) = match rng.below(4) {
        0 => ("MATCH (a:person)", "a"),
        1 => ("MATCH (a)-[e]->(b)", "a, e, b"),
        2 => (
            "MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(b)",
            "a, b",
        ),
        _ => ("MATCH (a) WITH a, a.age AS c", "a, c"),
    };
    let list = *rng.pick(&[
        "[a.age, 30, NULL]",
        "[(a)-->(y) | y.age]",
        "a.missing",
        "[]",
        "[a.age, a.age + 1]",
        "[(a)--(y) | y.name]",
    ]);
    let condition = *rng.pick(&[
        "x > 30",
        "x = 29",
        "x IS NULL",
        "x <> 27",
        "x > a.age",
        "x IN [27, 32, 'lop']",
        "x STARTS WITH 'j'",
    ]);
    let kept = format!("size([x IN {list} WHERE {condition}])");
    let unknown = format!("size([x IN {list} WHERE ({condition}) IS NULL])");
    let any = format!(
        "CASE WHEN {list} IS NULL THEN NULL WHEN {kept} > 0 THEN true WHEN {unknown} > 0 THEN \
         NULL ELSE false END"
    );
    let (form, rewrite) = match rng.below(6) {
        0 => (
            format!("all(x IN {list} WHERE {condition})"),
            format!(
                "CASE WHEN {list} IS NULL THEN NULL WHEN size([x IN {list} WHERE NOT \
                 ({condition})]) > 0 THEN false WHEN {unknown} > 0 THEN NULL ELSE true END"
            ),
        ),
        1 => (format!("any(x IN {list} WHERE {condition})"), any),
        2 => (
            format!("none(x IN {list} WHERE {condition})"),
            format!("NOT ({any})"),
        ),
        3 => (
            format!("single(x IN {list} WHERE {condition})"),
            format!(
                "CASE WHEN {list} IS NULL THEN NULL WHEN {kept} > 1 THEN false WHEN {unknown} > \
                 0 THEN NULL ELSE {kept} = 1 END"
            ),
        ),
        4 => (
            format!("reduce(s = [], x IN {list} | s + [x])"),
            format!("[x IN {list} | x]"),
        ),
        _ => (
            format!("reduce(s = 0, x IN {list} | s + 1)"),
            format!("size({list})"),
        ),
    };
    [
        format!("{head} RETURN {keys}, {form} AS v"),
        format!("{head} RETURN {keys}, {rewrite} AS v"),
    ]
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_2d_list_functions_never_panic_and_equal_their_rewrites() {
    let mut fatal = Fatal::default();
    let mut syntax = Vec::new();
    let mut differences = Vec::new();
    let mut rng = Rng::new(0x6A2_D5D1);
    let mut generated = Vec::new();
    for _ in 0..budget(100) {
        generated.push(random_gql_2d_list_function_queries(&mut rng));
    }
    let mut ran = 0;
    for forms in &generated {
        let texts = forms.clone();
        let outcome = guarded(WATCHDOG, move || {
            texts
                .iter()
                .map(|q| gql_2a_rows(q, false))
                .collect::<Vec<_>>()
        });
        match outcome {
            Ok(results) => {
                for (query, result) in forms.iter().zip(&results) {
                    match result {
                        Ok(_) => ran += 1,
                        Err(message) if message.starts_with("Syntax") => {
                            syntax.push(format!("{query}: {message}"));
                        }
                        Err(_) => {}
                    }
                }
                if results[0] != results[1] {
                    differences.push(format!(
                        "{}\n  {:?}\n{}\n  {:?}",
                        forms[0], results[0], forms[1], results[1]
                    ));
                }
            }
            Err(outcome) => fatal.check(&forms[0], Err::<(), _>(outcome)),
        }
    }
    let total = generated.len() * 2;
    assert!(
        ran * 4 >= total * 3,
        "only {ran} of {total} generated list function queries ran"
    );
    assert!(
        syntax.is_empty(),
        "generated list function queries that do not parse:\n{}",
        syntax.join("\n")
    );
    assert!(
        differences.is_empty(),
        "{} list function queries that differ from their rewrites:\n{}",
        differences.len(),
        differences.join("\n")
    );
    let mut texts: Vec<String> = generated.into_iter().flatten().collect();
    texts.extend(GQL_2D_LIST_FUNCTION_CORPUS.iter().map(|s| s.to_string()));
    // Nesting bounds, long lists and a quadratic fold: a result or a bounded error.
    for (open, close) in [
        ("reduce(s = 0, x IN ", " | s)"),
        ("reduce(s = ", ", x IN [1] | s)"),
        ("all(x IN [1] WHERE ", ")"),
        ("any(x IN ", " WHERE true)"),
    ] {
        texts.push(format!(
            "MATCH (n) RETURN {}1{} AS x",
            open.repeat(300),
            close.repeat(300)
        ));
    }
    texts.push(format!(
        "RETURN reduce(s = [], x IN [{}] | s + [x, x]) AS l",
        vec!["1"; 1_000].join(", ")
    ));
    texts.push("RETURN all(x IN range(1, 9223372036854775807) WHERE x > 0) AS a".to_string());
    texts.push("RETURN reduce(s = 0, x IN range(1, 100000) | s + x) AS a".to_string());
    texts.push("RETURN reduce(s = 9223372036854775807, x IN [1] | s + x) AS a".to_string());
    texts.push(
        "MATCH (a), (b), (c) RETURN reduce(s = [], x IN [(a)--(d) | d] | s + [x, b, c]) AS l"
            .to_string(),
    );
    for _ in 0..budget(100) {
        let base = rng.pick(GQL_2D_LIST_FUNCTION_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(20);
        let mut soup: Vec<&str> = (0..len)
            .map(|_| *rng.pick(GQL_2D_LIST_FUNCTION_TOKENS))
            .collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (p) RETURN");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 2d list functions");
}

// ---------------------------------------------------------------------------------------------
// GQL stage 2d.T6: composite queries (UNION, NEXT, CALL { }, COUNT { }, COLLECT { })
// ---------------------------------------------------------------------------------------------

/// Valid composite queries: the base documents of their mutations.
#[cfg(feature = "gql")]
const GQL_2D_CORPUS: &[&str] = &[
    "MATCH (p:person) RETURN p.name AS n UNION MATCH (s:software) RETURN s.name AS n",
    "RETURN 1 AS x UNION ALL RETURN 1 AS x NEXT RETURN x, count(*) AS c",
    "MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN p.name, s.name",
    "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f ORDER BY f.name LIMIT 1 } RETURN p, f",
    "MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s) RETURN s } RETURN p, s",
    "MATCH (p) CALL { WITH p MATCH (p)-->(f) RETURN count(f) AS c } RETURN p, c",
    "CALL { MATCH (s:software) RETURN s } RETURN s.name AS n",
    "MATCH (p) CALL (*) { CALL (p) { RETURN p.name AS a UNION RETURN 'x' AS a } RETURN a } RETURN a",
    "MATCH (p:person) RETURN COUNT { MATCH (p)-->(f) RETURN f UNION MATCH (p)<--(f) RETURN f } AS c",
    "MATCH (p:person) RETURN COLLECT { MATCH (p)-->(f) RETURN f.name ORDER BY f.name } AS l",
    "MATCH (p) WHERE EXISTS { MATCH (p)-->(f) WITH f WHERE f.age > 30 RETURN f } RETURN p",
    "MATCH (p:person) RETURN p.name AS n ORDER BY n LIMIT 2 UNION RETURN 'z' AS n NEXT RETURN n ORDER BY n DESC",
];

/// Tokens of stage 2d for the composite token soup (a list of its own).
#[cfg(feature = "gql")]
const GQL_2D_TOKENS: &[&str] = &[
    "UNION",
    "UNION ALL",
    "UNION DISTINCT",
    "NEXT",
    "CALL",
    "CALL (p) {",
    "CALL (*) {",
    "CALL {",
    "OPTIONAL CALL",
    "COUNT {",
    "COLLECT {",
    "EXISTS {",
    "IN TRANSACTIONS",
    "{",
    "}",
    "(",
    ")",
    "(p)",
    "(p)-->(f)",
    "MATCH",
    "WITH p",
    "RETURN",
    "RETURN p",
    "RETURN 1 AS x",
    "AS x",
    "ORDER BY",
    "LIMIT 1",
    "count(*)",
    "p",
    "f",
    "x",
    ",",
    "1",
];

/// A random stage 2d query as pairs (or triples) of forms with the same rows; `kind` picks the
/// construct:
/// 0. a `CALL` body (nested 0 to 2 times in further `CALL`s) vs a later `MATCH`;
/// 1. a counting `CALL` body vs `COUNT { }` vs `size()` of a pattern comprehension;
/// 2. `COLLECT { }` vs the pattern comprehension;
/// 3. `UNION` vs `OR` with `DISTINCT`;
/// 4. `UNION ALL` vs a `FOR` over two filters;
/// 5. `NEXT` vs `FILTER`, and an aggregate after `NEXT` over a `UNION` vs `WITH DISTINCT`;
/// 6. a `UNION` body of a `CALL` vs a `DISTINCT` body over either direction;
/// 7. a whole-query `EXISTS` body vs the `MATCH`-only one.
#[cfg(feature = "gql")]
fn random_gql_2d_forms(rng: &mut Rng, kind: usize) -> Vec<Vec<String>> {
    let (head, vars, imports): (&str, &[&str], &[&str]) = match rng.below(7) {
        0 => ("MATCH (a:person)", &["a"], &["a"]),
        1 => ("MATCH (a)-[e]->(b)", &["a", "e", "b"], &["a", "b"]),
        2 => (
            "MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(b)",
            &["a", "b"],
            &["b", "a"],
        ),
        3 => (
            "MATCH (a:person) WITH a, count(*) AS c",
            &["a", "c"],
            &["a"],
        ),
        4 => (
            "FOR n IN ['marko', 'lop', 'nobody'] MATCH (a {name: n})",
            &["n", "a"],
            &["a"],
        ),
        5 => ("MATCH p = (a)-[:knows]->(b)", &["a", "b"], &["b", "a"]),
        _ => ("MATCH (a:software), (b:person)", &["a", "b"], &["a", "b"]),
    };
    let x = *rng.pick(imports);
    let keys = vars.join(", ");
    let (pattern, condition, map) = random_gql_2c_body(rng, x);
    let conditions = [
        format!("{x}.age > 30"),
        format!("{x}.name STARTS WITH 'j'"),
        format!("EXISTS {{ ({x})-[:created]->() }}"),
        format!("{x}.age IS NULL"),
        format!("COUNT {{ ({x})--() }} > 2"),
        format!("{x}.name IN ['lop', 'marko', 'vadas']"),
        format!("size(COLLECT {{ MATCH ({x})-->(z) RETURN z }}) = 1"),
    ];
    let c1 = rng.pick(&conditions).clone();
    let c2 = rng.pick(&conditions).clone();
    match kind {
        0 => {
            let mut body = format!("MATCH {pattern}{condition} RETURN {map} AS v");
            for _ in 0..rng.below(3) {
                body = format!("CALL ({x}) {{ {body} }} RETURN v");
            }
            vec![vec![
                format!("{head} CALL ({x}) {{ {body} }} RETURN {keys}, v"),
                format!("{head} MATCH {pattern}{condition} RETURN {keys}, {map} AS v"),
            ]]
        }
        1 => vec![vec![
            format!(
                "{head} CALL ({x}) {{ MATCH {pattern}{condition} RETURN count(*) AS s }} RETURN \
                 {keys}, s"
            ),
            format!("{head} RETURN {keys}, COUNT {{ MATCH {pattern}{condition} }} AS s"),
            format!("{head} RETURN {keys}, size([{pattern}{condition} | 1]) AS s"),
        ]],
        2 => vec![vec![
            format!(
                "{head} RETURN {keys}, COLLECT {{ MATCH {pattern}{condition} RETURN {map} AS v \
                 }} AS l"
            ),
            format!("{head} RETURN {keys}, [{pattern}{condition} | {map}] AS l"),
        ]],
        3 => vec![vec![
            format!("{head} FILTER {c1} RETURN {keys} UNION {head} FILTER {c2} RETURN {keys}"),
            format!("{head} FILTER {c1} OR {c2} RETURN DISTINCT {keys}"),
        ]],
        4 => vec![vec![
            format!("{head} FILTER {c1} RETURN {keys} UNION ALL {head} FILTER {c2} RETURN {keys}"),
            format!(
                "{head} FOR t IN [1, 2] FILTER CASE WHEN t = 1 THEN {c1} ELSE {c2} END RETURN \
                 {keys}"
            ),
        ]],
        5 => vec![
            vec![
                format!("{head} RETURN {keys} NEXT FILTER {c1} RETURN {keys}"),
                format!("{head} FILTER {c1} RETURN {keys}"),
            ],
            vec![
                format!(
                    "{head} FILTER {c1} RETURN {keys} UNION {head} FILTER {c2} RETURN {keys} \
                     NEXT RETURN count(*) AS k"
                ),
                format!("{head} FILTER {c1} OR {c2} WITH DISTINCT {keys} RETURN count(*) AS k"),
            ],
        ],
        6 => vec![vec![
            format!(
                "{head} CALL ({x}) {{ MATCH ({x})-->(y) RETURN y UNION MATCH ({x})<--(y) RETURN \
                 y }} RETURN {keys}, y"
            ),
            format!("{head} CALL ({x}) {{ MATCH ({x})--(y) RETURN DISTINCT y }} RETURN {keys}, y"),
        ]],
        _ => vec![vec![
            format!(
                "{head} FILTER EXISTS {{ MATCH {pattern}{condition} RETURN {map} AS v }} RETURN \
                 {keys}"
            ),
            format!("{head} FILTER EXISTS {{ MATCH {pattern}{condition} }} RETURN {keys}"),
        ]],
    }
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_2d_composite_queries_never_panic_and_equal_their_rewrites() {
    let mut fatal = Fatal::default();
    let mut syntax = Vec::new();
    let mut differences = Vec::new();
    let mut rng = Rng::new(0x6A2_D7E6);
    let mut generated: Vec<Vec<String>> = Vec::new();
    for _ in 0..budget(100) {
        for _ in 0..3 {
            let kind = rng.below(8);
            generated.extend(random_gql_2d_forms(&mut rng, kind));
        }
    }
    // Every generated query parses; enough of them run; the forms of a group give the same
    // rows (or the same error), with and without property indexes.
    let mut ran = 0;
    let mut total = 0;
    for forms in &generated {
        total += forms.len();
        let texts = forms.clone();
        let outcome = guarded(WATCHDOG, move || {
            texts
                .iter()
                .map(|q| (gql_2a_rows(q, false), gql_2a_rows(q, true)))
                .collect::<Vec<_>>()
        });
        match outcome {
            Ok(results) => {
                for (query, (plain, indexed)) in forms.iter().zip(&results) {
                    match plain {
                        Ok(_) => ran += 1,
                        Err(message) if message.starts_with("Syntax") => {
                            syntax.push(format!("{query}: {message}"));
                        }
                        Err(_) => {}
                    }
                    if plain != indexed {
                        differences.push(format!(
                            "{query}\n  without index: {plain:?}\n  with indexes:  {indexed:?}"
                        ));
                    }
                }
                for (form, result) in forms.iter().zip(&results).skip(1) {
                    if result.0 != results[0].0 {
                        differences.push(format!(
                            "{}\n  {:?}\n{form}\n  {:?}",
                            forms[0], results[0].0, result.0
                        ));
                    }
                }
            }
            Err(outcome) => fatal.check(&forms[0], Err::<(), _>(outcome)),
        }
    }
    assert!(
        ran * 4 >= total * 3,
        "only {ran} of {total} generated 2d queries ran"
    );
    assert!(
        syntax.is_empty(),
        "generated 2d queries that do not parse:\n{}",
        syntax.join("\n")
    );
    assert!(
        differences.is_empty(),
        "{} 2d queries whose rows depend on the form or an index:\n{}",
        differences.len(),
        differences.join("\n")
    );
    let mut texts: Vec<String> = generated.into_iter().flatten().collect();
    texts.extend(GQL_2D_CORPUS.iter().map(|s| s.to_string()));
    // Nesting, long chains and unbounded bodies: a result or a bounded error.
    let mut calls = "RETURN 1 AS x".to_string();
    let mut counts = "MATCH (n) RETURN n".to_string();
    for _ in 0..20 {
        calls = format!("CALL {{ {calls} UNION ALL RETURN 2 AS x }} RETURN x");
        counts = format!("MATCH (n) WHERE COUNT {{ {counts} }} > 0 RETURN n");
    }
    texts.push(calls);
    texts.push(counts);
    texts.push(
        (0..300)
            .map(|i| format!("RETURN {i} AS x"))
            .collect::<Vec<_>>()
            .join(" UNION ALL "),
    );
    texts.push(format!(
        "RETURN 1 AS x {}RETURN x",
        "NEXT RETURN x + 1 AS x ".repeat(300)
    ));
    texts.push(format!(
        "MATCH (p) {} RETURN p",
        (0..60)
            .map(|i| format!("CALL (p) {{ MATCH (p)--(q{i}) RETURN count(q{i}) AS c{i} }}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    texts.push(
        "MATCH (a), (b), (c) CALL (a, b, c) { MATCH (a)--(d)--(e) RETURN [d, e, b, c] AS l } \
         RETURN l"
            .to_string(),
    );
    texts.push(
        "MATCH (p) CALL (p) { MATCH (p)-[]-{1,}(q) RETURN q UNION ALL MATCH (p)-[]-{1,}(q) \
         RETURN q } RETURN count(*) AS c"
            .to_string(),
    );
    texts.push(
        "MATCH (p) RETURN COLLECT { MATCH (p)-[]-{1,}(q) RETURN COLLECT { MATCH (q)-[]-{1,}(r) \
         RETURN r } AS l } AS l"
            .to_string(),
    );
    for _ in 0..budget(100) {
        let base = rng.pick(GQL_2D_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(20);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_2D_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (p) RETURN p");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 2d composite");
}

/// Stage 2e (parenthesized and quantified path patterns) corpus for the mutations.
#[cfg(feature = "gql")]
const GQL_2E_CORPUS: &[&str] = &[
    "MATCH (a) ((x)-[e]->(y)){1,3} (b) RETURN a, b, x, e",
    "MATCH (a {name: 'marko'}) ((x)-[e:knows]->(m)<-[f]-(y) WHERE m.age > 20){1,2} (b) RETURN b, m",
    "MATCH p = (a) ((x)-->(y))? (b) RETURN p, y",
    "MATCH (a:person) WHERE (a) (()-[:created]->()){1} RETURN a",
    "MATCH (a) WHERE ((x)-->(y))+ (a) RETURN a",
    "MATCH (a) RETURN [(a) ((x)-->(y)){1,2} (b) | b.name] AS l",
    "MATCH (a) RETURN [((x)-->(y))? (a) | x] AS l",
    "MATCH (a) WHERE EXISTS { (a) ((x)-->(y))+ (:software) } RETURN a",
    "MATCH (a) CALL (a) { MATCH (a) ((x)--(y)){2} (b) RETURN b } RETURN a, b",
    "MATCH (a) OPTIONAL MATCH (a) ((x)-[e]->(y)){0,2} (b) RETURN a, b, e",
    "MATCH (a) ((x)-[e]-(y)){0} (b) RETURN a, b",
    "MATCH (a) ((x)-->(y)) ((u)<--(v)){1,2} (b) RETURN a, b, v",
    "MATCH (a) ((x)-->(y) WHERE x.age < y.age)* (b) RETURN a, b",
    "MATCH (a) RETURN COUNT { (a) ((x)-[:knows]->(y)){1,} } AS c",
];

/// Tokens of stage 2e for the token soup (a list of its own).
#[cfg(feature = "gql")]
const GQL_2E_TOKENS: &[&str] = &[
    "(",
    ")",
    "((",
    "))",
    "(x)",
    "(y)",
    "(a)",
    "(b)",
    "()",
    "-->",
    "<--",
    "--",
    "-[e]->",
    "-[e:knows]-",
    "<-[f]-",
    "{1,3}",
    "{0}",
    "{2,}",
    "{,2}",
    "{1}",
    "?",
    "*",
    "+",
    "WHERE",
    "x.age > 20",
    "e.weight < 0.5",
    "MATCH",
    "RETURN",
    "RETURN a",
    "a",
    "b",
    "x",
    "e",
    ",",
    "p =",
    "EXISTS {",
    "COUNT {",
    "}",
    "[",
    "|",
    "]",
    "OPTIONAL MATCH",
    "TRAIL",
    "AND",
    "NOT",
    "CALL (a) {",
    "MATCH (a)",
];

/// A random stage 2e query as groups of forms with the same rows; `kind` picks the rewrite:
/// 0. no quantifier vs the plain edge (body variables singletons);
/// 1. a one-edge quantified path pattern vs the quantified edge (`e` a list in both);
/// 2. `?` (conditional singletons) vs `{0,1}` (lists, read with `head`);
/// 3. the body written reversed from the other end (lists read with `reverse`);
/// 4. `{2}` of one edge vs two written edges, and `{1}` of two edges vs the plain chain;
/// 5. nested: `EXISTS { }`, a pattern predicate, `COUNT { }` / a pattern comprehension, `CALL`
///    and `OPTIONAL MATCH` vs the same with the quantified edge.
#[cfg(feature = "gql")]
fn random_gql_2e_forms(rng: &mut Rng, kind: usize) -> Vec<Vec<String>> {
    let a = *rng.pick(&[
        "(a)",
        "(a:person)",
        "(a {name: 'marko'})",
        "(a:software)",
        "(a WHERE a.age > 30)",
    ]);
    let (b, blank_b) = *rng.pick(&[
        ("(b)", "()"),
        ("(b:software)", "(:software)"),
        ("(b {name: 'lop'})", "({name: 'lop'})"),
        ("(b:person)", "(:person)"),
    ]);
    let label = *rng.pick(&["", ":knows", ":created", ":knows|created"]);
    // The edge written forward, the same edge written from its other end.
    let ((l, r), (rl, rr)) = *rng.pick(&[
        (("-[", "]->"), ("<-[", "]-")),
        (("<-[", "]-"), ("-[", "]->")),
        (("-[", "]-"), ("-[", "]-")),
    ]);
    let q = *rng.pick(&[
        "{1,2}", "{0,2}", "{1}", "{2}", "{0}", "{2,3}", "+", "*", "{,2}",
    ]);
    let cond = *rng.pick(&["", " WHERE e.weight > 0.3", " WHERE e.weight <= 0.5"]);
    let edge = format!("{l}e{label}{r}");
    let back = format!("{rl}e{label}{rr}");
    let inline = format!("{l}e{label}{cond}{r}");
    let blank = format!("{l}{label}{r}");
    let outer = cond;
    match kind {
        0 => vec![vec![
            format!("MATCH {a} ((x){edge}(y){cond}) {b} RETURN a, b, e, x, y"),
            format!("MATCH {a}{edge}{b}{outer} RETURN a, b, e, a AS x, b AS y"),
        ]],
        1 => vec![vec![
            format!("MATCH {a} ((x){edge}(y){cond}){q} {b} RETURN a, b, e, size(x) AS n"),
            format!("MATCH {a}{inline}{q}{b} RETURN a, b, e, size(e) AS n"),
        ]],
        2 => vec![vec![
            format!("MATCH {a} ((x){edge}(y){cond})? {b} RETURN a, b, e, y"),
            format!(
                "MATCH {a} ((x){edge}(y){cond}){{0,1}} {b} RETURN a, b, head(e) AS e, head(y) AS y"
            ),
        ]],
        3 => vec![vec![
            format!("MATCH {a} ((x){edge}(y){cond}){q} {b} RETURN a, b, e, x, y"),
            format!(
                "MATCH {b} ((y){back}(x){cond}){q} {a} RETURN a, b, reverse(e) AS e, reverse(x) \
                 AS x, reverse(y) AS y"
            ),
        ]],
        4 => {
            let f = format!("{l}f{label}{r}");
            let second = cond.replace("e.", "f.");
            vec![
                vec![
                    format!("MATCH {a} ((x){edge}(y){cond}){{2}} {b} RETURN a, b, e"),
                    format!(
                        "MATCH {a}{edge}(){f}{b} WHERE true{} RETURN a, b, [e, f] AS e",
                        if cond.is_empty() {
                            String::new()
                        } else {
                            format!(" AND {} AND {}", &cond[7..], &second[7..])
                        }
                    ),
                ],
                vec![
                    format!(
                        "MATCH {a} ((x){edge}(m){f}(y)){{1}} {b} RETURN a, b, head(m) AS m, \
                         head(e) AS e, head(f) AS f"
                    ),
                    format!("MATCH {a}{edge}(m){f}{b} RETURN a, b, m, e, f"),
                ],
            ]
        }
        _ => vec![
            {
                let mut exists = vec![
                    format!(
                        "MATCH {a} WHERE EXISTS {{ (a) ((x){edge}(y){cond}){q} {b} }} RETURN a"
                    ),
                    format!("MATCH {a} WHERE EXISTS {{ (a){inline}{q}{b} }} RETURN a"),
                ];
                // A pattern predicate introduces no variable, so it has no edge condition.
                if cond.is_empty() {
                    exists.push(format!(
                        "MATCH {a} WHERE (a) ((){blank}()){q} {blank_b} RETURN a"
                    ));
                    exists.push(format!("MATCH {a} WHERE (a){blank}{q}{blank_b} RETURN a"));
                }
                exists
            },
            vec![
                format!("MATCH {a} RETURN a, COUNT {{ (a) ((x){edge}(y){cond}){q} {b} }} AS c"),
                format!("MATCH {a} RETURN a, size([(a) ((x){edge}(y){cond}){q} {b} | 1]) AS c"),
                format!("MATCH {a} RETURN a, COUNT {{ (a){inline}{q}{b} }} AS c"),
            ],
            vec![
                format!(
                    "MATCH {a} CALL (a) {{ MATCH (a) ((x){edge}(y){cond}){q} {b} RETURN b, e }} \
                     RETURN a, b, e"
                ),
                format!("MATCH {a} MATCH (a){inline}{q}{b} RETURN a, b, e"),
            ],
            vec![
                format!("MATCH {a} OPTIONAL MATCH (a) ((x){edge}(y){cond}){q} {b} RETURN a, b, e"),
                format!("MATCH {a} OPTIONAL MATCH (a){inline}{q}{b} RETURN a, b, e"),
            ],
        ],
    }
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_2e_queries_never_panic_and_equal_their_rewrites() {
    let mut fatal = Fatal::default();
    let mut syntax = Vec::new();
    let mut differences = Vec::new();
    let mut rng = Rng::new(0x6A2_E5E5);
    let mut generated: Vec<Vec<String>> = Vec::new();
    for _ in 0..budget(100) {
        for _ in 0..2 {
            let kind = rng.below(6);
            generated.extend(random_gql_2e_forms(&mut rng, kind));
        }
    }
    // Every generated query parses; enough of them run; the forms of a group give the same
    // rows (or the same error), with and without property indexes.
    let mut ran = 0;
    let mut total = 0;
    for forms in &generated {
        total += forms.len();
        let texts = forms.clone();
        let outcome = guarded(WATCHDOG, move || {
            texts
                .iter()
                .map(|q| (gql_2a_rows(q, false), gql_2a_rows(q, true)))
                .collect::<Vec<_>>()
        });
        match outcome {
            Ok(results) => {
                for (query, (plain, indexed)) in forms.iter().zip(&results) {
                    match plain {
                        Ok(_) => ran += 1,
                        Err(message) if message.starts_with("Syntax") => {
                            syntax.push(format!("{query}: {message}"));
                        }
                        Err(_) => {}
                    }
                    if plain != indexed {
                        differences.push(format!(
                            "{query}\n  without index: {plain:?}\n  with indexes:  {indexed:?}"
                        ));
                    }
                }
                for (form, result) in forms.iter().zip(&results).skip(1) {
                    if result.0 != results[0].0 {
                        differences.push(format!(
                            "{}\n  {:?}\n{form}\n  {:?}",
                            forms[0], results[0].0, result.0
                        ));
                    }
                }
            }
            Err(outcome) => fatal.check(&forms[0], Err::<(), _>(outcome)),
        }
    }
    assert!(
        ran * 10 >= total * 9,
        "only {ran} of {total} generated 2e queries ran"
    );
    assert!(
        syntax.is_empty(),
        "generated 2e queries that do not parse:\n{}",
        syntax.join("\n")
    );
    assert!(
        differences.is_empty(),
        "{} 2e queries whose rows depend on the form or an index:\n{}",
        differences.len(),
        differences.join("\n")
    );
    let mut texts: Vec<String> = generated.into_iter().flatten().collect();
    texts.extend(GQL_2E_CORPUS.iter().map(|s| s.to_string()));
    // Nesting, long bodies and unbounded loops: a result or a bounded error. (Every level of
    // nested comprehensions runs per item of the one around it: deep nesting is a parser
    // limit, shallow nesting runs.)
    for depth in [3, 300] {
        let mut exists = "(n)-->()".to_string();
        let mut comprehension = "1".to_string();
        for _ in 0..depth {
            exists = format!("(n) ((x WHERE EXISTS {{ {exists} }})-->(y)){{1,2}}");
            comprehension = format!("[(n) (()-->()){{1,2}} (m) | {comprehension}]");
        }
        texts.push(format!("MATCH (n) WHERE {exists} RETURN n"));
        texts.push(format!("MATCH (n) RETURN {comprehension} AS l"));
    }
    texts.push(format!(
        "MATCH (a) ({}){{1,3}} (b) RETURN count(*) AS c",
        (0..64)
            .map(|i| format!("(x{i})-[e{i}]-"))
            .collect::<String>()
            + "(x64)"
    ));
    texts.push("MATCH (a) ((x)-[e]-(y))* (b) ((u)-[f]-(v))* (c) RETURN count(*) AS c".into());
    texts.push("MATCH (a) WHERE (a) ((x)-->(y)){2} RETURN a".into());
    texts.push("MATCH (a) WHERE ((a)-->(b)) * 2 RETURN a".into());
    texts.push("MATCH (a) RETURN [((x)-->(y)){2} (a), ((a)-->(b))] AS l".into());
    for _ in 0..budget(100) {
        let base = rng.pick(GQL_2E_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(100) {
        let len = 1 + rng.below(20);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_2E_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (a) WHERE");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 2e quantified path patterns");
}

// ----- stage 3a: GQL writes (INSERT / CREATE) -----

/// One vertex as text for [`gql_3a_dump`]: its labels and its properties sorted by key.
#[cfg(feature = "gql")]
fn gql_3a_vertex<G: graphersal::storage::GraphStorage>(g: &G, v: G::VertexId) -> String {
    let mut props: Vec<String> = g
        .vertex_properties(v)
        .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
        .unwrap_or_default();
    props.sort();
    format!("{:?} {{{}}}", g.vertex_labels(v), props.join(", "))
}

/// The canonical state of `g`, independent of element ids: every vertex (labels, properties)
/// and every edge (label, properties, both ends by their content), sorted.
#[cfg(feature = "gql")]
fn gql_3a_dump<G: graphersal::storage::GraphStorage>(g: &G) -> Vec<String> {
    let mut lines: Vec<String> = g
        .vertices()
        .map(|v| format!("V {}", gql_3a_vertex(g, v)))
        .collect();
    for e in g.edges() {
        let mut props: Vec<String> = g
            .edge_properties(e)
            .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        props.sort();
        let ends = g
            .edge_endpoints(e)
            .map(|(from, to)| format!("{} -> {}", gql_3a_vertex(g, from), gql_3a_vertex(g, to)))
            .unwrap_or_default();
        lines.push(format!(
            "E {} {{{}}} {ends}",
            g.edge_label(e).unwrap_or_default(),
            props.join(", ")
        ));
    }
    lines.sort();
    lines
}

/// A random `INSERT` / `CREATE` query and its Gremlin twin (Rhai), which create the same
/// elements: the rows (one unit row, `MATCH (p:person)` or `FOR i IN [..]`), then a path of one
/// to three nodes, each new (labels and a constant or row-dependent property map) or the
/// bound `p`, joined by edges of either direction with one label. GQL creates the nodes in text
/// order, then the edges in text order (design W-d); the twin does the same with `add_v()` /
/// `add_e()` and `as()` labels. With `fail`, the rows come from `FOR x IN [1, .., 0]` and the
/// first new node stores `1 / x`, so the last row fails.
#[cfg(feature = "gql")]
fn random_gql_3a_write(rng: &mut Rng, fail: bool) -> (String, String) {
    let source = if fail { 3 } else { rng.below(3) };
    let (mut gql, mut twin) = match source {
        0 => (String::new(), "g.inject(0)".to_string()),
        1 => (
            "MATCH (p:person) ".to_string(),
            "g.v().has_label(\"person\").as(\"p\")".to_string(),
        ),
        2 => {
            let items = *rng.pick(&["[1]", "[1, 2]", "[1, 1, 2]", "[3, 2, 1]"]);
            (
                format!("FOR i IN {items} "),
                format!("g.inject({})", &items[1..items.len() - 1]) + ".as(\"i\")",
            )
        }
        _ => {
            let items = *rng.pick(&["[1, 0]", "[2, 1, 0]", "[1, 1, 0]"]);
            (
                format!("FOR x IN {items} "),
                format!("g.inject({})", &items[1..items.len() - 1]) + ".as(\"x\")",
            )
        }
    };
    // A failing query may also run on several rows per item: the last of them fails.
    let with_match = source == 1 || (source == 3 && rng.chance(50));
    if source == 3 && with_match {
        gql.push_str("MATCH (p:person) ");
        twin.push_str(".v().has_label(\"person\").as(\"p\")");
    }
    let nodes = 1 + rng.below(3);
    // Per node: `None` for the bound `p`, else its labels and its properties (GQL, Gremlin).
    let mut parts: Vec<Gql3aNode> = Vec::new();
    for at in 0..nodes {
        if with_match
            && nodes > 1
            && !(fail && at == 0)
            && rng.chance(30)
            && !parts.iter().any(Option::is_none)
        {
            parts.push(None);
            continue;
        }
        let labels: Vec<&str> = match rng.below(4) {
            0 => Vec::new(),
            1 => vec!["A"],
            2 => vec!["B"],
            _ => vec!["A", "B"],
        };
        let mut props: Vec<(String, String)> = Vec::new();
        if fail && at == 0 {
            props.push((
                "v: 1 / x".into(),
                "property(\"v\", __.select(\"x\").math(\"1 / _\"))".into(),
            ));
        }
        match rng.below(5) {
            0 => {}
            1 => props.push(("k: 1".into(), "property(\"k\", 1)".into())),
            2 => props.push(("s: 'a'".into(), "property(\"s\", \"a\")".into())),
            3 if with_match => props.push((
                "of: p.name".into(),
                "property(\"of\", __.select(\"p\").values(\"name\"))".into(),
            )),
            3 if source == 2 => {
                props.push(("i: i".into(), "property(\"i\", __.select(\"i\"))".into()))
            }
            _ => {
                props.push(("k: 2".into(), "property(\"k\", 2)".into()));
                props.push(("f: 1.5".into(), "property(\"f\", 1.5)".into()));
            }
        }
        parts.push(Some((labels, props)));
    }
    let name = |at: usize, part: &Gql3aNode| match part {
        None => "p".to_string(),
        Some(_) => format!("n{at}"),
    };
    let keyword = *rng.pick(&["INSERT", "CREATE"]);
    gql.push_str(keyword);
    gql.push(' ');
    let mut edges: Vec<(String, String, &str, bool)> = Vec::new();
    for (at, part) in parts.iter().enumerate() {
        if at > 0 {
            let label = *rng.pick(&["T", "U"]);
            let props = rng.chance(30);
            let map = if props { " {w: 1}" } else { "" };
            let previous = name(at - 1, &parts[at - 1]);
            let this = name(at, part);
            if rng.chance(50) {
                gql.push_str(&format!("-[:{label}{map}]->"));
                edges.push((previous, this, label, props));
            } else {
                gql.push_str(&format!("<-[:{label}{map}]-"));
                edges.push((this, previous, label, props));
            }
        }
        match part {
            None => gql.push_str("(p)"),
            Some((labels, props)) => {
                let written = if labels.is_empty() {
                    String::new()
                } else {
                    format!(":{}", labels.join(if rng.chance(50) { "&" } else { ":" }))
                };
                let map = if props.is_empty() {
                    String::new()
                } else {
                    let entries: Vec<&str> = props.iter().map(|(g, _)| g.as_str()).collect();
                    format!(" {{{}}}", entries.join(", "))
                };
                gql.push_str(&format!("(n{at}{written}{map})"));
                let quoted: Vec<String> = labels.iter().map(|l| format!("\"{l}\"")).collect();
                let add = if labels.is_empty() {
                    "add_v()".to_string()
                } else {
                    format!("add_v([{}])", quoted.join(", "))
                };
                twin.push('.');
                twin.push_str(&add);
                for (_, step) in props {
                    twin.push('.');
                    twin.push_str(step);
                }
                twin.push_str(&format!(".as(\"n{at}\")"));
            }
        }
    }
    for (from, to, label, props) in edges {
        twin.push_str(&format!(
            ".add_e(\"{label}\").from(\"{from}\").to(\"{to}\")"
        ));
        if props {
            twin.push_str(".property(\"w\", 1)");
        }
    }
    gql.push_str(rng.pick(&["", " FINISH", " RETURN count(*) AS c"]));
    twin.push_str(".iterate()");
    (gql, twin)
}

/// One node of [`random_gql_3a_write`]: `None` for the bound `p`, else its labels and its
/// properties (each as GQL and as a Gremlin step).
#[cfg(feature = "gql")]
type Gql3aNode = Option<(Vec<&'static str>, Vec<(String, String)>)>;

/// The canonical state of the modern graph after `script` ran on it through Rhai, or the
/// error text.
#[cfg(feature = "gql")]
fn gql_3a_script_state(script: &str) -> Result<Vec<String>, String> {
    let graph = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
    let _ = eval_value_with_limits(
        graph.clone(),
        script,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .map_err(|err| err.to_string())?;
    Ok(gql_3a_dump(&*graph.read()))
}

/// Runs `query` as a write on the modern graph: from Rust (rows, profile, the diagnostic in
/// both spellings) and from Rhai (a writing unit, then the same with a following Gremlin step).
/// Every run starts on a fresh modern graph: a write repeated on the graph it grew can grow it
/// quadratically per run (a cross product of all vertices), which tests nothing new.
#[cfg(feature = "gql")]
fn exercise_gql_write(query: String) -> Result<(), Outcome> {
    guarded(WATCHDOG, move || {
        let params = [
            ("n", ElementProperty::from(2)),
            ("name", ElementProperty::from("x")),
        ];
        for camel in [false, true] {
            let mut graph = TraversalGraph::tinkerpop_modern();
            let mut g = graph.traversal_mut();
            if camel {
                g = g.with("render.spelling", "camel");
            }
            if let Err(err) = g.gql_with(&query, params.clone()).to_list() {
                let _ = format_diagnostic(&err);
                let _ = err.help();
            }
        }
        let mut graph = TraversalGraph::tinkerpop_modern();
        match graph.traversal_mut().gql(&query).profile() {
            Ok(metrics) => {
                let _ = metrics.to_string();
            }
            Err(err) => {
                let _ = format_diagnostic(&err);
            }
        }
        for script in [
            "g.gql(q).toList();",
            r#"g.gql(q, #{n: 2, name: "x"}).count().next();"#,
            r#"g.with("render.spelling", "camel").gql(q).select("n0").out().toList();"#,
        ] {
            let graph = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
            let script_params: HashMap<String, ElementProperty> =
                [("q".to_string(), ElementProperty::from(query.as_str()))].into();
            let _ = eval_value_with_limits(graph, script, script_params, &ScriptLimits::default());
        }
    })
}

/// Adversarial and refused write queries of stage 3a: a write clause in every place, wrong
/// patterns (designs W5-W7), NULL and non-node endpoints, huge row counts.
#[cfg(feature = "gql")]
const GQL_3A_CORPUS: &[&str] = &[
    "INSERT (a:A {k: 1})-[:T {w: 2}]->(b:B)<-[:U]-(c)",
    "CREATE (a), (b), (a)-[:T]->(b)",
    "MATCH (p:person) INSERT (p)-[:T]->(p)",
    "MATCH (p:person) OPTIONAL MATCH (p)-[:zzz]->(q) INSERT (p)-[:T]->(q)",
    "FOR x IN [1, 'a', null] INSERT (:X {v: x})",
    "FOR x IN [1, 2] INSERT (a)-[:T]->(x)",
    "MATCH (p) INSERT (p:extra)",
    "MATCH (a)-[e]->(b) INSERT (a)-[e:T]->(b)",
    "INSERT (a)-[:T]-(b)",
    "INSERT (a)-[:T|U]->(b)",
    "INSERT (a:A|B)",
    "INSERT (a:!A)",
    "INSERT (a:%)",
    "INSERT p = (a)-[:T]->(b)",
    "INSERT ((a)-[:T]->(b)){2}",
    "INSERT (a)-[:T*2]->(b)",
    "INSERT (:X {k: count(*)})",
    "INSERT (:X {k: 1, k: 2})",
    "INSERT (:X {k: (:Y)})",
    "MATCH (p) INSERT (:X {k: p})",
    "MATCH (p) WHERE EXISTS { INSERT (:x) } RETURN p",
    "MATCH (p) RETURN COUNT { CREATE (:x) } AS c",
    "MATCH (p) RETURN [(p)-->(m) | CREATE (k)] AS l",
    "MATCH (p) RETURN 1 AS a UNION INSERT (:x) RETURN 1 AS a",
    "MATCH (p) CALL (p) { INSERT (p)-[:T]->(:X) } RETURN p",
    "MATCH (p) OPTIONAL CALL (p) { INSERT (:X {of: p.name}) } RETURN count(*) AS c",
    "INSERT (n:X) RETURN n NEXT MATCH (n)-->(m) RETURN m",
    "INSERT (n:X) WITH n MATCH (m:X) RETURN count(m) AS c",
    "INSERT (n:X) INSERT (n)-[:T]->(:Y) FINISH",
    "FOR i IN range(1, 200) INSERT (:Many {i: i})",
    "MATCH (a), (b) INSERT (a)-[:all]->(b)",
    "MATCH (a), (b), (c) INSERT (:triple {a: a.name, b: b.name, c: c.name})",
    "FOR x IN [1, 0] INSERT (:Z {v: 1 / x})",
    "MATCH (n) WITH n, n.age AS old INSERT (:Copy {age: old}) RETURN old",
    "INSERT (:X {m: {a: [1, 'b', {c: null}]}})",
    "INSERT (:X {v: $n, w: $missing})",
    "INSERT",
    "INSERT (",
    "CREATE INDEX FOR (n:X) ON (n.k)",
    "LOAD CSV FROM 'x' AS row INSERT (:X)",
    "MATCH (n) FOREACH (x IN [1] | CREATE (m))",
];

/// Tokens of stage 3a for the token soup (a new list: the existing ones stay as they are).
#[cfg(feature = "gql")]
const GQL_3A_TOKENS: &[&str] = &[
    "INSERT",
    "CREATE",
    "FINISH",
    "MATCH",
    "FOR",
    "IN",
    "[1, 2]",
    "x",
    "(n)",
    "(n:A)",
    "(n:A&B {k: 1})",
    "(:B)",
    "(p)",
    "-[:T]->",
    "<-[:U]-",
    "-[e:T {w: 1}]->",
    "-[:T]-",
    ",",
    "RETURN",
    "n",
    "count(*)",
    "OPTIONAL",
    "CALL",
    "{",
    "}",
    "WITH",
    "WHERE",
    "1 / 0",
    "NULL",
    "(m {v: n.k})",
    "AS",
];

#[test]
#[cfg(feature = "gql")]
fn random_gql_3a_writes_never_panic_equal_their_twins_and_fail_without_trace() {
    let mut fatal = Fatal::default();
    let mut differences = Vec::new();
    let mut traces = Vec::new();
    let mut rng = Rng::new(0x3A1A_5E27);
    let modern = gql_3a_dump(&TraversalGraph::tinkerpop_modern());
    let mut texts: Vec<String> = GQL_3A_CORPUS.iter().map(|s| s.to_string()).collect();
    let mut generated = Vec::new();
    for _ in 0..budget(40) {
        generated.push((random_gql_3a_write(&mut rng, false), false));
        generated.push((random_gql_3a_write(&mut rng, true), true));
    }
    let mut twins = 0;
    for ((query, twin), fail) in generated {
        texts.push(query.clone());
        let label = query.clone();
        let outcome = guarded(WATCHDOG, move || {
            let mut graph = TraversalGraph::tinkerpop_modern();
            let rows = graph
                .traversal_mut()
                .gql(&query)
                .to_list()
                .map_err(|e| e.to_string());
            let gql = rows.map(|_| gql_3a_dump(&graph));
            let rhai = gql_3a_script_state(&format!(
                "g.gql({}).iterate()",
                graphersal_rhai_string(&query)
            ));
            let gremlin = gql_3a_script_state(&twin);
            (query, twin, gql, rhai, gremlin, gql_3a_dump(&graph))
        });
        let Ok((query, twin, gql, rhai, gremlin, after)) = outcome else {
            fatal.check(&label, outcome.map(|_| ()));
            continue;
        };
        if fail {
            // The last row fails: an error, and nothing of the earlier rows stays.
            if gql.is_ok() || rhai.is_ok() || after != modern {
                traces.push(format!("{query}\n  {gql:?}\n  rhai: {rhai:?}"));
            }
            if gremlin.is_ok() {
                traces.push(format!("the twin of a failing query ran: {twin}"));
            }
            continue;
        }
        twins += 1;
        match (&gql, &gremlin) {
            (Ok(gql_state), Ok(gremlin_state)) if gql_state == gremlin_state && gql == rhai => {}
            _ => differences.push(format!(
                "{query}\n  GQL:     {gql:?}\n  Rhai:    {rhai:?}\n  {twin}\n  Gremlin: {gremlin:?}"
            )),
        }
    }
    assert!(twins >= budget(40) as usize, "only {twins} twins ran");
    assert!(
        differences.is_empty(),
        "{} INSERT queries whose final graph differs from their Gremlin twin:\n{}",
        differences.len(),
        differences.join("\n")
    );
    assert!(
        traces.is_empty(),
        "{} failing INSERT queries left a trace:\n{}",
        traces.len(),
        traces.join("\n")
    );
    for _ in 0..budget(40) {
        let base = rng.pick(GQL_3A_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(40) {
        let len = 1 + rng.below(16);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_3A_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "INSERT");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql_write(text.clone()));
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 3a writes");
}

/// `text` as a Rhai string literal.
#[cfg(feature = "gql")]
fn graphersal_rhai_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

// ----- stage 3c: GQL SET / REMOVE -----

/// One source of [`GQL_3C_SOURCES`]: the GQL `MATCH`, its Gremlin twin, its targets (name,
/// whether a node).
#[cfg(feature = "gql")]
type Gql3cSource = (&'static str, &'static str, &'static [(&'static str, bool)]);

/// The rows of [`random_gql_3c_set`]: the GQL `MATCH`, its Gremlin twin, and the targets it
/// binds (name, whether a node).
#[cfg(feature = "gql")]
const GQL_3C_SOURCES: &[Gql3cSource] = &[
    (
        "MATCH (p:person)",
        "g.v().has_label(\"person\").as(\"p\")",
        &[("p", true)],
    ),
    (
        "MATCH (p:person)-[e:knows]->(q)",
        "g.v().has_label(\"person\").as(\"p\").out_e(\"knows\").as(\"e\").in_v().as(\"q\")",
        &[("p", true), ("e", false), ("q", true)],
    ),
    (
        "MATCH (s:software)<-[c:created]-(p)",
        "g.v().has_label(\"software\").as(\"s\").in_e(\"created\").as(\"c\").out_v().as(\"p\")",
        &[("s", true), ("c", false), ("p", true)],
    ),
];

/// One `SET` / `REMOVE` item of [`random_gql_3c_set`] on `target`: its GQL text, its Gremlin
/// step on the selected target (`None` when the item reads the target's own name, which stays
/// valid only without a replacing item), whether it is a REMOVE item, whether it replaces
/// every property, whether it reads.
#[cfg(feature = "gql")]
fn random_gql_3c_item(
    rng: &mut Rng,
    target: &str,
    node: bool,
) -> (String, String, bool, bool, bool) {
    // `(v).k` is a property item form only (design D2).
    let t = if rng.chance(15) {
        format!("({target})")
    } else {
        target.to_string()
    };
    let v = target;
    let kinds = if node { 12 } else { 8 };
    match rng.below(kinds) {
        0 => (
            format!("{t}.k = 1"),
            "property(\"k\", 1)".into(),
            false,
            false,
            false,
        ),
        1 => (
            format!("{t}.s = 'a'"),
            "property(\"s\", \"a\")".into(),
            false,
            false,
            false,
        ),
        2 => (
            format!("{t}.k = 2.5"),
            "property(\"k\", 2.5)".into(),
            false,
            false,
            false,
        ),
        3 => (
            format!("{t}.k = NULL"),
            "remove_property(\"k\")".into(),
            false,
            false,
            false,
        ),
        4 => (
            format!("{v} += {{a: 1, b: 'x'}}"),
            "property(#{a: 1, b: \"x\"})".into(),
            false,
            false,
            false,
        ),
        5 => (
            format!("{v} += {{a: NULL, k: 3}}"),
            "remove_property(\"a\").property(\"k\", 3)".into(),
            false,
            false,
            false,
        ),
        6 => (
            format!("{v} = {{x: 1}}"),
            "side_effect(__.properties().drop()).property(\"x\", 1)".into(),
            false,
            true,
            false,
        ),
        7 => (
            format!("{t}.k"),
            "remove_property(\"k\")".into(),
            true,
            false,
            false,
        ),
        8 => {
            let (written, steps) = *rng.pick(&[
                (":A", "add_label(\"A\")"),
                (":A:B", "add_label(\"A\").add_label(\"B\")"),
                (":A&B", "add_label(\"A\").add_label(\"B\")"),
                (" IS B", "add_label(\"B\")"),
                (" :B :A", "add_label(\"B\").add_label(\"A\")"),
            ]);
            (format!("{v}{written}"), steps.into(), false, false, false)
        }
        9 => {
            let (written, steps) = *rng.pick(&[
                (":A", "drop_label(\"A\")"),
                (":A:B", "drop_label(\"A\").drop_label(\"B\")"),
                (":nope", "drop_label(\"nope\")"),
            ]);
            (format!("{v}{written}"), steps.into(), true, false, false)
        }
        10 => (
            format!("{t}.c = {target}.name"),
            "property(\"c\", __.values(\"name\"))".into(),
            false,
            false,
            true,
        ),
        _ => (format!("{t}.name"), String::new(), true, false, false),
    }
}

/// A random `SET` / `REMOVE` query and its Gremlin twin (Rhai), which leave the same graph:
/// the rows of one of [`GQL_3C_SOURCES`] (perhaps with an `OPTIONAL MATCH` binding `z` to
/// NULL, whose items are no-ops), then one to five items on the bound elements, in one or
/// several `SET` / `REMOVE` clauses, then nothing, `FINISH` or a `RETURN`. The twin applies the
/// items in text order with `select()` and the Gremlin write steps; it runs step at a time,
/// GQL row at a time, which leaves the same graph because every item writes a constant (or
/// the target's own name, never written): the last item touching a key decides it either way.
/// With `fail`, the rows come from `FOR x IN [1, .., 0]` and the first item stores `1 / x`, so
/// the last row fails.
#[cfg(feature = "gql")]
fn random_gql_3c_set(rng: &mut Rng, fail: bool) -> (String, String) {
    let (matched, twin, targets) = *rng.pick(GQL_3C_SOURCES);
    let mut gql = String::new();
    if fail {
        gql.push_str(rng.pick(&["FOR x IN [1, 0] ", "FOR x IN [2, 1, 0] "]));
    }
    gql.push_str(matched);
    let mut targets: Vec<(&str, bool)> = targets.to_vec();
    let null = rng.chance(30);
    if null {
        gql.push_str(" OPTIONAL MATCH (p)-[:nope]->(z)");
        targets.push(("z", true));
    }
    let mut twin = twin.to_string();
    let count = 1 + rng.below(5);
    let mut items: Vec<(String, &str, String, bool, bool, bool)> = Vec::new();
    if fail {
        let (target, _) = targets[0];
        items.push((
            format!("{target}.v = 1 / x"),
            target,
            String::new(),
            false,
            false,
            false,
        ));
    }
    while items.len() < count + usize::from(fail) {
        let (target, node) = *rng.pick(&targets);
        let (text, step, remove, replace, reads) = random_gql_3c_item(rng, target, node);
        // REMOVE of `name` only as a REMOVE item (its SET twin form is the empty step).
        if step.is_empty() && !remove {
            continue;
        }
        items.push((text, target, step, remove, replace, reads));
    }
    // A replacing item and a read of `name` do not mix (GQL reads row at a time).
    let replaces = items.iter().any(|item| item.4);
    let removes_name = items
        .iter()
        .any(|item| item.2.is_empty() && item.0.ends_with(".name"));
    if replaces || removes_name {
        items.retain(|item| !item.5);
    }
    let mut clause: Option<bool> = None;
    for (text, target, step, remove, _, _) in &items {
        let continues = clause == Some(*remove) && rng.chance(60);
        if continues {
            gql.push_str(", ");
        } else {
            gql.push_str(if *remove { " REMOVE " } else { " SET " });
        }
        clause = Some(*remove);
        gql.push_str(text);
        if *target == "z" || fail {
            continue;
        }
        let step = if step.is_empty() {
            "remove_property(\"name\")"
        } else {
            step.as_str()
        };
        twin.push_str(&format!(".select(\"{target}\").{step}"));
    }
    gql.push_str(rng.pick(&[
        "",
        " FINISH",
        " RETURN count(*) AS c",
        " RETURN p.name AS n",
    ]));
    twin.push_str(".iterate()");
    (gql, twin)
}

/// Adversarial and refused `SET` / `REMOVE` queries of stage 3c: every item form on nodes,
/// edges, NULL and non-element targets, D4 (`SET n = NULL`), label items on edges, complex
/// targets (not yet), writes in subqueries, aggregates, schema-free type mixes, huge row
/// counts.
#[cfg(feature = "gql")]
const GQL_3C_CORPUS: &[&str] = &[
    "MATCH (p:person) SET p.k = 1, (p).j = p.k, p:A:B, p IS C & D REMOVE p:A, p.age",
    "MATCH (p) SET p = NULL",
    "MATCH (p) SET p += NULL",
    "MATCH (p) SET p = $missing",
    "MATCH (p) SET p = 1",
    "MATCH (p) SET p += [1, 2]",
    "MATCH (p) SET p = p",
    "MATCH (a), (b) SET a = b",
    "MATCH (a)-[e]->(b) SET a += e, e = a",
    "MATCH (p) SET p.k = p",
    "MATCH (p) SET p.k = [p]",
    "MATCH (p) SET p += {k: p}",
    "MATCH p = (a)-->(b) SET a.k = p",
    "MATCH ()-[e]->() SET e:x",
    "MATCH ()-[e]->() REMOVE e:knows",
    "MATCH ()-[e]->() WITH collect(e) AS es FOR x IN es SET x:foo",
    "FOR x IN [1, 'a', null] SET x.k = 1",
    "FOR x IN [1] REMOVE x:A",
    "LET x = 1 SET x.k = 2",
    "MATCH p = (a)-->(b) SET p.k = 1",
    "MATCH (a) ((x)-->(y)){1,2} (b) SET x.k = 1",
    "FOR x IN [1] WITH ORDINALITY i SET i.k = 1",
    "MATCH (p) SET q.k = 1",
    "MATCH (p) SET p.a.b = 1",
    "MATCH (p) SET head([p]).k = 1",
    "MATCH (p) SET p[\"k\"] = 1",
    "MATCH (p) SET p:A|B",
    "MATCH (p) SET p:!A",
    "MATCH (p) SET p:%",
    "MATCH (p) SET p:A&B:C",
    "MATCH (p) SET p.k",
    "MATCH (p) SET",
    "MATCH (p) REMOVE p.k = 1",
    "MATCH (p) REMOVE p",
    "MATCH (p) SET p.k = count(*)",
    "MATCH (p) SET p += {k: 1, k: 2}",
    "MATCH (p) WHERE EXISTS { MATCH (p) SET p.x = 1 } RETURN p",
    "MATCH (p) RETURN COUNT { MATCH (p) REMOVE p.x RETURN p } AS c",
    "MATCH (p) RETURN [(p)-->(m) | 1] AS l SET p.k = 1",
    "MATCH (p) SET p.k = 1 UNION MATCH (p) RETURN p",
    "MATCH (p) CALL (p) { SET p.k = 1 } RETURN p.k",
    "MATCH (p) OPTIONAL CALL (p) { REMOVE p:person } RETURN count(*) AS c",
    "MATCH (p) SET p.k = 1 RETURN p NEXT MATCH (p) REMOVE p.k RETURN p",
    "MATCH (n) WITH n, n.age AS old SET n.age = 1 RETURN old",
    "MATCH (a), (b) SET a.n = coalesce(a.n, 0) + 1",
    "FOR i IN range(1, 300) MATCH (p) SET p.k = i",
    "MATCH (a), (b), (c) SET a.k = b.name, c:Triple",
    "FOR x IN [1, 0] MATCH (p) SET p.v = 1 / x",
    "MATCH (p) SET p.m = {a: [1, 'b', {c: null}]}, p.l = [1, [2, [3]]]",
    "MATCH (p) SET p.k = 1 LIMIT 0",
    "MATCH (p) SET p.k = 1 WITH p WHERE p.k > 0 RETURN count(*) AS c",
    "MATCH (p) SET p.k = 1 SET p.k = p.k + 1 REMOVE p.k SET p.k = 3",
    "INSERT (n:X) SET n.k = 1, n:Y REMOVE n:X RETURN n",
    "MATCH (p) OPTIONAL MATCH (p)-[:nope]->(z) SET z.k = 1, z:A, z = {}, z += {a: 1} REMOVE z.k, z:A",
    "MATCH (p) FOREACH (x IN [1] | SET p.k = x)",
    "MATCH (p) SET p.`we ird` = 1, p:`la bel` REMOVE p.`we ird`",
    "MATCH (p) SET p.k = $n, p += $name",
];

/// Tokens of stage 3c for the token soup (a new list: the existing ones stay as they are).
#[cfg(feature = "gql")]
const GQL_3C_TOKENS: &[&str] = &[
    "SET",
    "REMOVE",
    "MATCH",
    "(p)",
    "(p:person)",
    "-[e]->",
    "(q)",
    "p",
    "e",
    "(p)",
    ".",
    "k",
    "=",
    "+=",
    ":",
    "A",
    "IS",
    "&",
    "|",
    "{a: 1}",
    "{}",
    "NULL",
    "1",
    "p.name",
    ",",
    "RETURN",
    "FINISH",
    "OPTIONAL",
    "WITH",
    "FOR x IN [1, 0]",
    "1 / x",
    "CALL",
    "{",
    "}",
    "LIMIT 0",
];

#[test]
#[cfg(feature = "gql")]
fn random_gql_3c_set_never_panics_equals_its_twins_and_fails_without_trace() {
    let mut fatal = Fatal::default();
    let mut differences = Vec::new();
    let mut traces = Vec::new();
    let mut rng = Rng::new(0x3C5E_7AE0);
    let modern = gql_3a_dump(&TraversalGraph::tinkerpop_modern());
    let mut texts: Vec<String> = GQL_3C_CORPUS.iter().map(|s| s.to_string()).collect();
    let mut generated = Vec::new();
    for _ in 0..budget(40) {
        generated.push((random_gql_3c_set(&mut rng, false), false));
        generated.push((random_gql_3c_set(&mut rng, true), true));
    }
    let mut twins = 0;
    let mut changed = 0;
    for ((query, twin), fail) in generated {
        texts.push(query.clone());
        let label = query.clone();
        let outcome = guarded(WATCHDOG, move || {
            let mut graph = TraversalGraph::tinkerpop_modern();
            let rows = graph
                .traversal_mut()
                .gql(&query)
                .to_list()
                .map_err(|e| format_diagnostic(&e));
            let gql = rows.map(|_| gql_3a_dump(&graph));
            let rhai = gql_3a_script_state(&format!(
                "g.gql({}).iterate()",
                graphersal_rhai_string(&query)
            ));
            let gremlin = if fail {
                Err(String::new())
            } else {
                gql_3a_script_state(&twin)
            };
            (query, twin, gql, rhai, gremlin, gql_3a_dump(&graph))
        });
        let Ok((query, twin, gql, rhai, gremlin, after)) = outcome else {
            fatal.check(&label, outcome.map(|_| ()));
            continue;
        };
        if fail {
            // The last row fails: an error, and nothing of the earlier rows stays.
            if gql.is_ok() || rhai.is_ok() || after != modern {
                traces.push(format!("{query}\n  {gql:?}\n  rhai: {rhai:?}"));
            }
            continue;
        }
        twins += 1;
        if gql.as_ref().is_ok_and(|state| *state != modern) {
            changed += 1;
        }
        match (&gql, &gremlin) {
            (Ok(gql_state), Ok(gremlin_state)) if gql_state == gremlin_state && gql == rhai => {}
            _ => differences.push(format!(
                "{query}\n  GQL:     {gql:?}\n  Rhai:    {rhai:?}\n  {twin}\n  Gremlin: {gremlin:?}"
            )),
        }
    }
    assert!(twins >= budget(40) as usize, "only {twins} twins ran");
    // The twins compare changed graphs, not the modern graph again and again.
    assert!(
        changed * 2 > twins,
        "only {changed} of {twins} twins changed the graph"
    );
    assert!(
        differences.is_empty(),
        "{} SET / REMOVE queries whose final graph differs from their Gremlin twin:\n{}",
        differences.len(),
        differences.join("\n")
    );
    assert!(
        traces.is_empty(),
        "{} failing SET / REMOVE queries left a trace:\n{}",
        traces.len(),
        traces.join("\n")
    );
    for _ in 0..budget(40) {
        let base = rng.pick(GQL_3C_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(40) {
        let len = 1 + rng.below(16);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_3C_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (p) SET");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql_write(text.clone()));
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 3c SET / REMOVE");
}

// ----- stage 3d: GQL DELETE / DETACH DELETE -----

/// One source of [`GQL_3D_SOURCES`]: the GQL `MATCH`, its Gremlin twin, its node variables,
/// its edge variables, and its path variable (whose elements are exactly the node and edge
/// variables).
#[cfg(feature = "gql")]
type Gql3dSource = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    Option<&'static str>,
);

/// The rows of [`random_gql_3d_delete`]: every source binds its elements with `as()` labels
/// in the twin, so the twin can drop what the GQL query deletes.
#[cfg(feature = "gql")]
const GQL_3D_SOURCES: &[Gql3dSource] = &[
    (
        "MATCH (p:person)",
        "g.v().has_label(\"person\").as(\"p\")",
        &["p"],
        &[],
        None,
    ),
    (
        "MATCH (p:person)-[e:knows]->(q)",
        "g.v().has_label(\"person\").as(\"p\").out_e(\"knows\").as(\"e\").in_v().as(\"q\")",
        &["p", "q"],
        &["e"],
        None,
    ),
    (
        "MATCH (s:software)<-[c:created]-(p)",
        "g.v().has_label(\"software\").as(\"s\").in_e(\"created\").as(\"c\").out_v().as(\"p\")",
        &["s", "p"],
        &["c"],
        None,
    ),
    (
        "MATCH (s:software {name: 'ripple'})<-[c]-(p)",
        "g.v().has_label(\"software\").has(\"name\", \"ripple\").as(\"s\").in_e().as(\"c\")\
         .out_v().as(\"p\")",
        &["s", "p"],
        &["c"],
        None,
    ),
    (
        "MATCH t = (p:person {name: 'marko'})-[e]->(q)",
        "g.v().has_label(\"person\").has(\"name\", \"marko\").as(\"p\").out_e().as(\"e\")\
         .in_v().as(\"q\")",
        &["p", "q"],
        &["e"],
        Some("t"),
    ),
    (
        "MATCH ()-[e:created]->()",
        "g.v().out_e(\"created\").as(\"e\")",
        &[],
        &["e"],
        None,
    ),
];

/// How [`random_gql_3d_delete`] deletes.
#[cfg(feature = "gql")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gql3dMode {
    /// `DETACH DELETE`, in one or two clauses: always succeeds.
    Detach,
    /// `DELETE` / `NODETACH DELETE` in one clause: fails with G1001 when a node keeps an edge
    /// the clause does not delete.
    Plain,
    /// `FOR x IN [1, 0]` rows and `1 / x` before or after the `DETACH DELETE`: the last row
    /// fails.
    Fail,
}

/// A random DELETE query of stage 3d with its oracles: the GQL query; its Gremlin twin
/// (Rhai), which drops the same elements (selected through `union()`, `dedup()`, a
/// `barrier()`, then `drop()`); for `Detach`, the same deletion written as NODETACH with every
/// node's edges matched by `OPTIONAL MATCH` and deleted explicitly in the same clause; for
/// `Plain`, a GQL query telling whether a deleted node has an edge the clause does not delete
/// (G1001 exactly then).
#[cfg(feature = "gql")]
fn random_gql_3d_delete(rng: &mut Rng, mode: Gql3dMode) -> (String, String, String) {
    let (matched, source, nodes, edges, path) = *rng.pick(GQL_3D_SOURCES);
    // The targets: element variables, perhaps the path, perhaps inside a list, perhaps NULL.
    let mut vars: Vec<&str> = nodes.iter().chain(edges).copied().collect();
    if let Some(t) = path {
        vars.push(t);
    }
    let mut chosen: Vec<&str> = vars.iter().copied().filter(|_| rng.chance(60)).collect();
    if chosen.is_empty() {
        chosen.push(*rng.pick(&vars));
    }
    let null = rng.chance(25);
    let mut targets: Vec<String> = Vec::new();
    let mut pending: Vec<&str> = chosen.clone();
    while !pending.is_empty() {
        if pending.len() >= 2 && rng.chance(30) {
            let a = pending.remove(0);
            let b = pending.remove(0);
            targets.push(format!("[{a}, {b}]"));
        } else {
            targets.push(pending.remove(0).to_string());
        }
    }
    if null {
        targets.insert(rng.below(targets.len() + 1), "z".to_string());
    }
    if rng.chance(10) {
        targets.push("NULL".to_string());
    }
    // What the targets remove: the path stands for its elements.
    let mut removed: Vec<&str> = Vec::new();
    for var in &chosen {
        if Some(*var) == path {
            removed.extend(nodes.iter().chain(edges).copied());
        } else {
            removed.push(var);
        }
    }
    removed.sort_unstable();
    removed.dedup();
    let removed_nodes: Vec<&str> = removed
        .iter()
        .copied()
        .filter(|v| nodes.contains(v))
        .collect();
    let removed_edges: Vec<&str> = removed
        .iter()
        .copied()
        .filter(|v| edges.contains(v))
        .collect();
    let mut gql = String::new();
    if mode == Gql3dMode::Fail {
        gql.push_str(rng.pick(&["FOR x IN [1, 0] ", "FOR x IN [2, 1, 0] "]));
    }
    gql.push_str(matched);
    if null {
        gql.push_str(" OPTIONAL MATCH (z:nope)");
    }
    let prefix = gql.clone();
    let fail_before = mode == Gql3dMode::Fail && rng.chance(50);
    if fail_before {
        gql.push_str(" FILTER 1 / x > 0");
    }
    let keyword = match mode {
        Gql3dMode::Plain => *rng.pick(&["DELETE", "NODETACH DELETE", "delete"]),
        _ => *rng.pick(&["DETACH DELETE", "detach delete"]),
    };
    let split = mode == Gql3dMode::Detach && targets.len() >= 2 && rng.chance(30);
    if split {
        let at = 1 + rng.below(targets.len() - 1);
        gql.push_str(&format!(
            " {keyword} {} {keyword} {}",
            targets[..at].join(", "),
            targets[at..].join(", ")
        ));
    } else {
        gql.push_str(&format!(" {keyword} {}", targets.join(", ")));
    }
    if mode == Gql3dMode::Fail {
        if !fail_before {
            gql.push_str(" RETURN 1 / x AS y");
        }
    } else {
        let counted = removed.first().copied().unwrap_or("z");
        let tail = match rng.below(4) {
            0 => String::new(),
            1 => " FINISH".to_string(),
            2 => " RETURN count(*) AS c".to_string(),
            _ => format!(" RETURN count({counted}) AS c"),
        };
        gql.push_str(&tail);
    }
    // The Gremlin twin: edges first, each element once, all selected before the first drop.
    let selects: Vec<String> = removed_edges
        .iter()
        .chain(&removed_nodes)
        .map(|v| format!("__.select(\"{v}\")"))
        .collect();
    let twin = format!(
        "{source}.union({}).dedup().barrier().drop().iterate()",
        selects.join(", ")
    );
    let oracle = match mode {
        Gql3dMode::Detach => {
            // NODETACH with every node's edges deleted explicitly in the same clause.
            let mut text = prefix;
            let mut extra: Vec<String> = Vec::new();
            for (i, node) in removed_nodes.iter().enumerate() {
                text.push_str(&format!(" OPTIONAL MATCH ({node})-[x{i}]-()"));
                extra.push(format!("x{i}"));
            }
            extra.extend(targets.iter().cloned());
            let word = *rng.pick(&["DELETE", "NODETACH DELETE"]);
            text.push_str(&format!(" {word} {}", extra.join(", ")));
            text
        }
        Gql3dMode::Plain => {
            let es: Vec<String> = removed_edges
                .iter()
                .map(|e| format!("collect(DISTINCT {e})"))
                .collect();
            let ns: Vec<String> = removed_nodes
                .iter()
                .map(|n| format!("collect(DISTINCT {n})"))
                .collect();
            let or_empty = |parts: Vec<String>| {
                if parts.is_empty() {
                    "[]".to_string()
                } else {
                    parts.join(" + ")
                }
            };
            format!(
                "{prefix} WITH {} AS es, {} AS ns FOR n IN ns MATCH (n)-[w]-() WHERE NOT w IN \
                 es RETURN count(w) > 0 AS left",
                or_empty(es),
                or_empty(ns)
            )
        }
        Gql3dMode::Fail => String::new(),
    };
    (gql, twin, oracle)
}

/// Adversarial and refused DELETE queries of stage 3d: every target kind (nodes, edges,
/// paths, lists, map members, NULL, non-elements, label predicates), NODETACH on connected
/// nodes, deleted elements read, written, returned and walked from afterwards, deletes in
/// subqueries, CALL bodies, UNION operands and after NEXT, huge row counts.
#[cfg(feature = "gql")]
const GQL_3D_CORPUS: &[&str] = &[
    "MATCH (n) DETACH DELETE n",
    "MATCH (n) DELETE n",
    "MATCH (n) NODETACH DELETE n",
    "MATCH ()-[r]-() DELETE r",
    "MATCH (n)-[r]-(m) DELETE r, n, m RETURN count(*) AS c",
    "MATCH (a)-[*]-(b) DETACH DELETE a, b RETURN count(*) AS c",
    "MATCH p = (a)-[*1..3]->(b) DETACH DELETE p",
    "MATCH p = (a)-->(b) DELETE p",
    "MATCH (a) ((x)-[e]->(y)){1,2} (b) DETACH DELETE x, e",
    "MATCH (n) WITH collect(n) AS ns DETACH DELETE ns",
    "MATCH (n) WITH {k: n} AS m DETACH DELETE m.k",
    "MATCH (n) WITH {k: [n, [n]]} AS m DETACH DELETE m.k",
    "MATCH (n) DETACH DELETE [n, [n, [n, NULL]]]",
    "OPTIONAL MATCH (n:nope) DELETE n RETURN n",
    "DELETE NULL",
    "MATCH (n) DELETE n:person",
    "MATCH (n) DELETE n IS person",
    "MATCH ()-[r]->() DELETE r:knows",
    "MATCH (n) DELETE 1 + 1",
    "MATCH (n) DELETE n.name",
    "MATCH (n) WITH 1 AS x DELETE x",
    "MATCH (n) DELETE {a: n}",
    "MATCH (n) DELETE x",
    "MATCH (n) DELETE count(n)",
    "MATCH (n) DELETE",
    "MATCH (n) DETACH n",
    "MATCH (n) DELETE n,",
    "MATCH (n) DROP n",
    "MATCH (n) REMOVE n",
    "MATCH (n) DETACH DELETE n RETURN n",
    "MATCH (n) DETACH DELETE n RETURN n.name",
    "MATCH (n) DETACH DELETE n RETURN labels(n), element_id(n)",
    "MATCH (n) DETACH DELETE n RETURN count(n), n IS NULL",
    "MATCH ()-[r]->() DELETE r RETURN type(r), count(*)",
    "MATCH ()-[r]->() DELETE r RETURN r.weight",
    "MATCH ()-[r]->() DELETE r RETURN startNode(r)",
    "MATCH (n) DETACH DELETE n SET n.k = 1",
    "MATCH (n) DETACH DELETE n REMOVE n:person",
    "MATCH (n) DETACH DELETE n INSERT (n)-[:k]->(:x)",
    "MATCH (n) DETACH DELETE n WITH n MATCH (n)-->(m) RETURN m",
    "MATCH (n) DETACH DELETE n WITH n ORDER BY n.name RETURN count(*) AS c",
    "MATCH (n) DETACH DELETE n RETURN n LIMIT 0",
    "MATCH (n) DETACH DELETE n RETURN properties(n)",
    "MATCH (n) WITH n, n.name AS name DETACH DELETE n RETURN name",
    "MATCH (n) WITH n, properties(n) AS p DETACH DELETE n RETURN p",
    "MATCH (n) DETACH DELETE n DETACH DELETE n",
    "MATCH (n) DETACH DELETE n WITH * LIMIT 1 RETURN 1 AS one",
    "MATCH (n) DETACH DELETE n SKIP 1",
    "MATCH (n) RETURN COUNT { MATCH (n) DELETE n } AS c",
    "MATCH (n) WHERE EXISTS { MATCH (n) DETACH DELETE n } RETURN n",
    "MATCH (n) RETURN [x IN [1] WHERE EXISTS { MATCH (n) NODETACH DELETE n }] AS l",
    "MATCH (n) CALL (n) { DETACH DELETE n } RETURN count(*) AS c",
    "MATCH (n) CALL (n) { DELETE n RETURN 1 AS y } RETURN y",
    "MATCH (n) OPTIONAL CALL (n) { DETACH DELETE n RETURN n.name AS m } RETURN m",
    "MATCH (n) DETACH DELETE n RETURN 1 AS x UNION MATCH (m) RETURN 1 AS x",
    "MATCH (n) RETURN n NEXT DETACH DELETE n",
    "MATCH (n) FOREACH (x IN [n] | DELETE x)",
    "FOR i IN range(1, 300) MATCH (n) DETACH DELETE n",
    "MATCH (a), (b), (c) DETACH DELETE a, b, c",
    "FOR x IN [1, 0] MATCH (n) DETACH DELETE n RETURN 1 / x AS y",
    "INSERT (n:X)-[:T]->(m:Y) DELETE n",
    "INSERT (n:X) DELETE n RETURN count(*) AS c",
    "MATCH (n) SET n.k = 1 DETACH DELETE n",
    "MATCH (n) DETACH DELETE $n",
    "MATCH (n:`we ird`) DETACH DELETE n",
];

/// Tokens of stage 3d for the token soup (a new list: the existing ones stay as they are).
#[cfg(feature = "gql")]
const GQL_3D_TOKENS: &[&str] = &[
    "DELETE",
    "DETACH",
    "NODETACH",
    "MATCH",
    "OPTIONAL",
    "(n)",
    "(n:person)",
    "-[r]->",
    "-[r]-",
    "(m)",
    "p =",
    "n",
    "r",
    "p",
    "[n, r]",
    "NULL",
    ":",
    "person",
    "IS",
    ",",
    "1",
    "RETURN",
    "n.name",
    "count(*)",
    "FINISH",
    "WITH",
    "SET n.k = 1",
    "REMOVE",
    "INSERT (x)",
    "CALL",
    "{",
    "}",
    "LIMIT 0",
    "FOR x IN [1, 0]",
    "1 / x",
];

#[test]
#[cfg(feature = "gql")]
fn random_gql_3d_delete_never_panics_equals_its_twins_and_fails_without_trace() {
    let mut fatal = Fatal::default();
    let mut differences = Vec::new();
    let mut traces = Vec::new();
    let mut rng = Rng::new(0x3D_DE1E_7E00);
    let modern = gql_3a_dump(&TraversalGraph::tinkerpop_modern());
    let mut texts: Vec<String> = GQL_3D_CORPUS.iter().map(|s| s.to_string()).collect();
    let mut generated = Vec::new();
    for _ in 0..budget(30) {
        for mode in [Gql3dMode::Detach, Gql3dMode::Plain, Gql3dMode::Fail] {
            generated.push((random_gql_3d_delete(&mut rng, mode), mode));
        }
    }
    let (mut twins, mut changed, mut refused) = (0usize, 0usize, 0usize);
    for ((query, twin, oracle), mode) in generated {
        texts.push(query.clone());
        if !oracle.is_empty() {
            texts.push(oracle.clone());
        }
        let label = query.clone();
        let outcome = guarded(WATCHDOG, move || {
            let run = |text: &str| {
                let mut graph = TraversalGraph::tinkerpop_modern();
                let rows = graph.traversal_mut().gql(text).to_list();
                let status = match &rows {
                    Err(err) => match err.root_cause() {
                        TraverserError::Gql { gqlstatus, .. } => gqlstatus.clone(),
                        other => format!("{other}"),
                    },
                    Ok(_) => String::new(),
                };
                let rows = rows.map_err(|e| format_diagnostic(&e));
                (rows, status, gql_3a_dump(&graph))
            };
            let (rows, status, after) = run(&query);
            let rhai = gql_3a_script_state(&format!(
                "g.gql({}).iterate()",
                graphersal_rhai_string(&query)
            ));
            let gremlin = gql_3a_script_state(&twin);
            let oracle = match mode {
                Gql3dMode::Detach => {
                    let (rows, _, after) = run(&oracle);
                    rows.map(|_| after.join("\n"))
                }
                Gql3dMode::Plain => {
                    let mut graph = TraversalGraph::tinkerpop_modern();
                    graph
                        .traversal_mut()
                        .gql(&oracle)
                        .to_list()
                        .map(|rows| format!("{rows:?}"))
                        .map_err(|e| format_diagnostic(&e))
                }
                Gql3dMode::Fail => Ok(String::new()),
            };
            (
                query,
                twin,
                rows.map(|_| ()),
                status,
                after,
                rhai,
                gremlin,
                oracle,
            )
        });
        let Ok((query, twin, rows, status, after, rhai, gremlin, oracle)) = outcome else {
            fatal.check(&label, outcome.map(|_| ()));
            continue;
        };
        match mode {
            Gql3dMode::Fail => {
                // The last row fails: an error, and nothing the DELETE removed stays removed.
                if rows.is_ok() || rhai.is_ok() || after != modern {
                    traces.push(format!("{query}\n  {rows:?}\n  rhai: {rhai:?}"));
                }
            }
            Gql3dMode::Detach => {
                twins += 1;
                if after != modern {
                    changed += 1;
                }
                let same = rows.is_ok()
                    && gremlin.as_ref() == Ok(&after)
                    && rhai.as_ref() == Ok(&after)
                    && oracle.as_ref() == Ok(&after.join("\n"));
                if !same {
                    differences.push(format!(
                        "{query}\n  GQL:     {rows:?} {after:?}\n  Rhai:    {rhai:?}\n  {twin}\n  \
                         Gremlin: {gremlin:?}\n  NODETACH form: {oracle:?}"
                    ));
                }
            }
            Gql3dMode::Plain => {
                // The oracle counts the edges the clause leaves on its nodes.
                let left = oracle.as_ref().map(|rows| rows.contains("true"));
                let ok = match (&rows, left) {
                    (Ok(()), Ok(false)) => {
                        twins += 1;
                        if after != modern {
                            changed += 1;
                        }
                        gremlin.as_ref() == Ok(&after) && rhai.as_ref() == Ok(&after)
                    }
                    (Err(_), Ok(true)) => {
                        refused += 1;
                        status == "G1001" && after == modern && rhai.is_err()
                    }
                    _ => false,
                };
                if !ok {
                    differences.push(format!(
                        "{query}\n  GQL:     {rows:?} {status} {after:?}\n  Rhai:    {rhai:?}\n  \
                         {twin}\n  Gremlin: {gremlin:?}\n  edges left: {oracle:?}"
                    ));
                }
            }
        }
    }
    assert!(twins >= budget(30) as usize, "only {twins} twins ran");
    // The twins compare changed graphs, and plain DELETE both succeeds and refuses.
    assert!(
        changed * 2 > twins,
        "only {changed} of {twins} twins changed the graph"
    );
    assert!(refused > 0, "no plain DELETE was refused with G1001");
    assert!(
        differences.is_empty(),
        "{} DELETE queries whose final graph differs from their twins:\n{}",
        differences.len(),
        differences.join("\n")
    );
    assert!(
        traces.is_empty(),
        "{} failing DELETE queries left a trace:\n{}",
        traces.len(),
        traces.join("\n")
    );
    for _ in 0..budget(30) {
        let base = rng.pick(GQL_3D_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(30) {
        let len = 1 + rng.below(16);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_3D_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (n)-[r]->(m) DETACH DELETE");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql_write(text.clone()));
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 3d DELETE");
}

// ----- stage 3e: GQL MERGE -----

/// A random `MERGE` query of stage 3e with its pure-GQL twin: the rows (a unit row, `FOR x IN
/// [..]`, `MATCH (p:person)` or both), then `MERGE` of a path of one to three nodes, each new
/// (named `n<i>`, labels from a pool without `person`, so the rows of `MATCH (p:person)` never
/// grow, and a constant or row-dependent property map) or the bound `p`, joined by edges
/// (named `e<i>`) of either direction or undirected, optionally followed by `ON CREATE SET`
/// items. No `ON MATCH SET`: a second run would apply it to every match (also an undirected
/// edge matched both ways, or older elements a created edge now connects), and its twin (a
/// second unit `CALL`) runs step-at-a-time, all rows before the inserting `CALL`. The twin is
/// the conditional INSERT form: per row, a unit `CALL` that inserts the pattern (an undirected
/// edge from left to right) and sets the items only when `NOT EXISTS` finds no match. With
/// `fail`, the rows are `FOR x IN [1, .., 0]` and the first new node stores `1 / x`, so the
/// last row fails (its check, before the match).
#[cfg(feature = "gql")]
fn random_gql_3e_merge(rng: &mut Rng, fail: bool) -> (String, String) {
    let source = if fail { 1 } else { rng.below(4) };
    let rows = match source {
        0 => String::new(),
        1 if fail => format!("FOR x IN {} ", rng.pick(&["[0]", "[1, 0]", "[2, 1, 0]"])),
        1 => format!(
            "FOR x IN {} ",
            rng.pick(&["[1]", "[1, 1, 2]", "[2, 1, 2, 1]", "[3, 2, 1]"])
        ),
        2 => "MATCH (p:person) ".to_string(),
        _ => "MATCH (p:person) FOR x IN [1, 2, 1] ".to_string(),
    };
    let (has_x, has_p) = (source == 1 || source == 3, source >= 2);
    let imports = match (has_p, has_x) {
        (true, true) => "p, x",
        (true, false) => "p",
        (false, true) => "x",
        (false, false) => "",
    };
    let len = 1 + rng.below(3);
    let mut nodes: Vec<Option<String>> = Vec::new();
    for i in 0..len {
        // A lone bound node merges nothing (E2): a one-node pattern is always new.
        if has_p && len > 1 && rng.chance(40) {
            nodes.push(None);
            continue;
        }
        let labels = *rng.pick(&["", ":A", ":B", ":A&B", ":software"]);
        let mut props: Vec<&str> = Vec::new();
        if fail && nodes.iter().all(Option::is_none) {
            props.push("v: 1 / x");
        }
        match rng.below(6) {
            0 => props.push("k: 1"),
            1 if has_x => props.push("k: x"),
            2 if has_p => props.push("name: p.name"),
            3 if labels == ":software" => props.push("name: 'lop'"),
            _ => {}
        }
        let map = if props.is_empty() {
            String::new()
        } else {
            format!(" {{{}}}", props.join(", "))
        };
        nodes.push(Some(format!("n{i}{labels}{map}")));
    }
    if nodes.iter().all(Option::is_none) {
        nodes[0] = Some("n0:A".to_string());
    }
    let node = |n: &Option<String>| match n {
        Some(text) => format!("({text})"),
        None => "(p)".to_string(),
    };
    let (mut merge_pattern, mut insert_pattern) = (node(&nodes[0]), node(&nodes[0]));
    let mut names: Vec<String> = Vec::new();
    if nodes[0].is_some() {
        names.push("n0".to_string());
    }
    for (i, next) in nodes.iter().enumerate().skip(1) {
        let label = *rng.pick(&["knows", "created", "r"]);
        let props = *rng.pick(&["", "", " {w: 1}", " {weight: 0.5}"]);
        let edge = format!("e{i}:{label}{props}");
        let (merged, inserted) = match rng.below(3) {
            0 => (format!("-[{edge}]->"), format!("-[{edge}]->")),
            1 => (format!("<-[{edge}]-"), format!("<-[{edge}]-")),
            // Undirected: matched either way, created from left to right (W33).
            _ => (format!("-[{edge}]-"), format!("-[{edge}]->")),
        };
        merge_pattern.push_str(&format!("{merged}{}", node(next)));
        insert_pattern.push_str(&format!("{inserted}{}", node(next)));
        names.push(format!("e{i}"));
        if next.is_some() {
            names.push(format!("n{i}"));
        }
    }
    let items = if rng.chance(50) {
        let target = rng.pick(&names).clone();
        let value = *rng.pick(&["1", "'on'", "[1, 2]"]);
        let mut items = format!("{target}.c = {value}");
        if rng.chance(30) && target.starts_with('n') {
            items.push_str(&format!(", {target}:Seen"));
        }
        Some(items)
    } else {
        None
    };
    let mut merge = format!("{rows}MERGE {merge_pattern}");
    // The matching pattern of the twin: the MERGE pattern without its variables' names would
    // be as good; the names are local to the EXISTS body.
    let mut twin = rows.clone();
    if let Some(items) = &items {
        merge.push_str(&format!(" ON CREATE SET {items}"));
    }
    twin.push_str(&format!(
        "CALL ({imports}) {{ FILTER NOT EXISTS {{ MATCH {merge_pattern} }} INSERT \
         {insert_pattern}{} }}",
        items
            .as_ref()
            .map(|items| format!(" SET {items}"))
            .unwrap_or_default()
    ));
    (merge, twin)
}

/// Adversarial and refused MERGE queries of stage 3e: every refused form (W30, W36, E2),
/// NULL values and ends (E1), ON items reading and failing, MERGE in subqueries and bodies,
/// large row counts, saved-query parameters.
#[cfg(feature = "gql")]
const GQL_3E_CORPUS: &[&str] = &[
    "MERGE (n:A {k: 1}) ON CREATE SET n.c = 1 ON MATCH SET n.m = true RETURN n.c, n.m",
    "FOR x IN [1, 1, 2] MERGE (n:A {k: x}) RETURN count(*) AS c",
    "MATCH (p:person) MERGE (p)-[:likes]->(f:fruit {of: p.name}) RETURN f.of",
    "MATCH (a:person), (b:person) MERGE (a)-[:knows]-(b) RETURN count(*) AS c",
    "MATCH (p:person) MERGE (p)-[:self]->(p)",
    "MERGE p = (:a)-[:r]->(:b) RETURN length(p) AS l, nodes(p) AS ns",
    "MERGE (n) RETURN count(*) AS c",
    "MERGE (n:A:B {k: 1}) MERGE (n)-[:r]->(m:C) RETURN m",
    "MATCH (a) MERGE (a)",
    "MERGE (a:A) MERGE (a)",
    "MATCH (a:person) MERGE (a:extra)-[:r]->(:B)",
    "MATCH (a:person) MERGE (a {k: 1})-[:r]->(:B)",
    "MATCH (a)-[e]->(b) MERGE (a)-[e:knows]->(b)",
    "MERGE (a:A), (b:B)",
    "MERGE (a:A) WHERE a.k = 1",
    "MERGE (a:A)-[:r*1..2]->(b)",
    "MERGE (a:A)-[:r|s]->(b)",
    "MERGE (a:A)-[]->(b)",
    "MERGE (a:A|B)",
    "MERGE (a:!A)",
    "MERGE ((a)-[:r]->(b)){2}",
    "MERGE (a:A {k: NULL})",
    "FOR x IN [1, NULL] MERGE (:A {k: x})",
    "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) MERGE (p)-[:made]->(s)",
    "FOR x IN [1] MATCH (p:person) WITH p, x MERGE (p)-[:k]->(x)",
    "MATCH (p:person) MERGE (q:A {k: p})",
    "MERGE (a:A {k: 1}) ON CREATE SET a.k = a",
    "MERGE (a:A {k: 1}) ON CREATE SET b.k = 1",
    "MERGE (a:A {k: 1}) ON DELETE SET a.k = 1",
    "MERGE (a:A {k: 1}) ON CREATE SET a = {k: 2} ON MATCH SET a += {m: 1}",
    "FOR x IN [1, 0] MERGE (a:A {k: 1}) ON MATCH SET a.v = 1 / x",
    "FOR x IN [1, 0] MERGE (a:A {k: 1 / x})",
    "MERGE (a:A {k: COUNT { MATCH (n) }})",
    "MERGE (a:A {k: [(n)-->(m) | m.name]})",
    "MATCH (p) WHERE EXISTS { MERGE (:x) } RETURN p",
    "MATCH (p) RETURN COUNT { MERGE (:x) } AS c",
    "MATCH (p:person) CALL (p) { MERGE (p)-[:tag]->(:T {v: p.age % 2}) } RETURN count(*) AS c",
    "MATCH (p:person) OPTIONAL CALL (p) { MERGE (p)-[:tag]->(t:T) RETURN t } RETURN count(t)",
    "MERGE (n:A) RETURN n NEXT MERGE (n)-[:r]->(:B) RETURN count(*) AS c",
    "MATCH (n:software) DETACH DELETE n MERGE (:software {name: 'lop'})",
    "MERGE (n:A {k: 1}) SET n.k = 2 MERGE (m:A {k: 1}) RETURN count(*) AS c",
    "FOR i IN range(1, 300) MERGE (:Many {i: i % 7})",
    "MATCH (a), (b) MERGE (a)-[:all]->(b)",
    "MERGE (n:A {k: $n, name: $name})",
    "MERGE (n:A {k: $missing})",
    "MERGE",
    "MERGE (",
    "MERGE (n) ON",
    "MERGE (n) ON CREATE",
    "MERGE (n) ON MATCH SET",
    "merge (n:a) on create set n.k = 1",
];

/// Tokens of stage 3e for the token soup (a new list: the existing ones stay as they are).
#[cfg(feature = "gql")]
const GQL_3E_TOKENS: &[&str] = &[
    "MERGE",
    "ON",
    "CREATE",
    "MATCH",
    "SET",
    "(n)",
    "(n:A {k: 1})",
    "(p)",
    "(:B)",
    "-[:r]->",
    "<-[e:r]-",
    "-[:r]-",
    "p =",
    "n.k = 1",
    "n:Seen",
    "n += {m: 1}",
    ",",
    "NULL",
    "FOR x IN [1, 0]",
    "1 / x",
    "x",
    "RETURN",
    "count(*)",
    "WITH",
    "CALL",
    "{",
    "}",
    "OPTIONAL",
    "DETACH DELETE n",
    "INSERT (m)",
];

/// The canonical state of the modern graph after `queries` ran on it one after the other
/// (each its own unit), or the diagnostic of the first that failed.
#[cfg(feature = "gql")]
fn gql_3e_state(queries: &[&str]) -> Result<Vec<String>, String> {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for query in queries {
        graph
            .traversal_mut()
            .gql(query)
            .to_list()
            .map_err(|e| format_diagnostic(&e))?;
    }
    Ok(gql_3a_dump(&graph))
}

#[test]
#[cfg(feature = "gql")]
fn random_gql_3e_merge_never_panics_is_idempotent_equals_its_twin_and_fails_without_trace() {
    let mut fatal = Fatal::default();
    let mut differences = Vec::new();
    let mut traces = Vec::new();
    let mut rng = Rng::new(0x3E_3E26_E000);
    let modern = gql_3a_dump(&TraversalGraph::tinkerpop_modern());
    let mut texts: Vec<String> = GQL_3E_CORPUS.iter().map(|s| s.to_string()).collect();
    let mut generated = Vec::new();
    for _ in 0..budget(30) {
        for fail in [false, false, true] {
            generated.push((random_gql_3e_merge(&mut rng, fail), fail));
        }
    }
    let (mut twins, mut changed) = (0usize, 0usize);
    for ((merge, twin), fail) in generated {
        texts.push(merge.clone());
        texts.push(twin.clone());
        let label = merge.clone();
        // A fresh modern graph for every run.
        let outcome = guarded(WATCHDOG, move || {
            let once = gql_3e_state(&[&merge]);
            let rhai = gql_3a_script_state(&format!(
                "g.gql({}).iterate()",
                graphersal_rhai_string(&merge)
            ));
            let (twice, twin_state) = if fail {
                (Ok(Vec::new()), Ok(Vec::new()))
            } else {
                (gql_3e_state(&[&merge, &merge]), gql_3e_state(&[&twin]))
            };
            (merge, twin, once, rhai, twice, twin_state)
        });
        let Ok((merge, twin, once, rhai, twice, twin_state)) = outcome else {
            fatal.check(&label, outcome.map(|_| ()));
            continue;
        };
        if fail {
            // The last row fails: an error, and nothing an earlier row wrote stays.
            let mut graph = TraversalGraph::tinkerpop_modern();
            let failed = graph.traversal_mut().gql(&merge).to_list().is_err();
            if !failed || once.is_ok() || rhai.is_ok() || gql_3a_dump(&graph) != modern {
                traces.push(format!("{merge}\n  {once:?}\n  rhai: {rhai:?}"));
            }
            continue;
        }
        twins += 1;
        let Ok(once) = once else {
            differences.push(format!("{merge}\n  failed: {once:?}"));
            continue;
        };
        if once != modern {
            changed += 1;
        }
        let same = twice.as_ref() == Ok(&once)
            && twin_state.as_ref() == Ok(&once)
            && rhai.as_ref() == Ok(&once);
        if !same {
            differences.push(format!(
                "{merge}\n  once:  {once:?}\n  twice: {twice:?}\n  Rhai:  {rhai:?}\n  {twin}\n  \
                 twin:  {twin_state:?}"
            ));
        }
    }
    assert!(twins >= 2 * budget(30) as usize, "only {twins} twins ran");
    assert!(
        changed * 2 > twins,
        "only {changed} of {twins} MERGE queries changed the graph"
    );
    assert!(
        differences.is_empty(),
        "{} MERGE queries differ from a second run or their twin:\n{}",
        differences.len(),
        differences.join("\n")
    );
    assert!(
        traces.is_empty(),
        "{} failing MERGE queries left a trace:\n{}",
        traces.len(),
        traces.join("\n")
    );
    for _ in 0..budget(30) {
        let base = rng.pick(GQL_3E_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(30) {
        let len = 1 + rng.below(16);
        let mut soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_3E_TOKENS)).collect();
        if rng.chance(50) {
            soup.insert(0, "MATCH (p:person) MERGE");
        }
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql_write(text.clone()));
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL 3e MERGE");
}

/// One random temporal value of stage 3g for [`random_gql_temporal_queries_never_panic_and_equal_their_twins`]:
/// its GQL map constructor, its GQL text constructor, the same value as a Rhai literal, its
/// Cypher type name and the `X.truncate()` units that type takes.
#[cfg(feature = "gql")]
struct GqlTemporal {
    map: String,
    text: String,
    rhai: String,
    of: &'static str,
    units: &'static [&'static str],
}

#[cfg(feature = "gql")]
const GQL_TEMPORAL_DATE_UNITS: &[&str] = &[
    "millennium",
    "century",
    "decade",
    "year",
    "weekYear",
    "quarter",
    "month",
    "week",
    "day",
];

#[cfg(feature = "gql")]
const GQL_TEMPORAL_DATETIME_UNITS: &[&str] = &[
    "millennium",
    "Century",
    "decade",
    "year",
    "weekyear",
    "quarter",
    "month",
    "week",
    "day",
    "hour",
    "minute",
    "second",
    "millisecond",
    "MICROSECOND",
    "nanosecond",
];

#[cfg(feature = "gql")]
const GQL_TEMPORAL_TIME_UNITS: &[&str] = &[
    "day",
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];

/// A signed four-digit ISO year (`-0500`, `2015`).
#[cfg(feature = "gql")]
fn gql_temporal_year(year: i64) -> String {
    if year < 0 {
        format!("-{:04}", -year)
    } else {
        format!("{year:04}")
    }
}

/// A random temporal value (years within -2000..9000 so that the arithmetic twins stay in
/// range; the day at most 28, so every map is a valid date).
#[cfg(feature = "gql")]
fn random_gql_temporal(rng: &mut Rng) -> GqlTemporal {
    let year = if rng.chance(15) {
        rng.below(2001) as i64 - 2000
    } else {
        1 + rng.below(9000) as i64
    };
    let (month, day) = (1 + rng.below(12), 1 + rng.below(28));
    let (hour, minute, second) = (rng.below(24), rng.below(60), rng.below(60));
    let micro = if rng.chance(50) {
        0
    } else if rng.chance(50) {
        rng.below(1000) * 1000
    } else {
        rng.below(1_000_000)
    };
    let date = format!("{}-{month:02}-{day:02}", gql_temporal_year(year));
    let date_map = format!("year: {year}, month: {month}, day: {day}");
    let time = if micro == 0 {
        format!("{hour:02}:{minute:02}:{second:02}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}.{micro:06}")
    };
    // The sub-second fields: one microsecond field, or milliseconds plus microseconds.
    let fraction = if micro == 0 {
        String::new()
    } else if rng.chance(50) {
        format!(", microsecond: {micro}")
    } else {
        format!(
            ", millisecond: {}, microsecond: {}",
            micro / 1000,
            micro % 1000
        )
    };
    let time_map = format!("hour: {hour}, minute: {minute}, second: {second}{fraction}");
    match rng.below(5) {
        0 => GqlTemporal {
            map: format!("date({{{date_map}}})"),
            text: format!("date('{date}')"),
            rhai: format!("date(\"{date}\")"),
            of: "date",
            units: GQL_TEMPORAL_DATE_UNITS,
        },
        1 => GqlTemporal {
            map: format!("localtime({{{time_map}}})"),
            text: format!("local_time('{time}')"),
            rhai: format!("time(\"{time}\")"),
            of: "localtime",
            units: GQL_TEMPORAL_TIME_UNITS,
        },
        2 => GqlTemporal {
            map: format!("localdatetime({{{date_map}, {time_map}}})"),
            text: format!("local_datetime('{date}T{time}')"),
            rhai: format!("localdatetime(\"{date}T{time}\")"),
            of: "localdatetime",
            units: GQL_TEMPORAL_DATETIME_UNITS,
        },
        3 => {
            let offset = if rng.chance(20) {
                "Z".to_string()
            } else {
                let sign = if rng.chance(50) { '+' } else { '-' };
                format!(
                    "{sign}{:02}:{:02}",
                    rng.below(15),
                    *rng.pick(&[0usize, 15, 30, 45])
                )
            };
            let zone = if offset == "Z" { "+00:00" } else { &offset };
            GqlTemporal {
                map: format!("datetime({{{date_map}, {time_map}, timezone: '{zone}'}})"),
                text: format!("zoned_datetime('{date}T{time}{offset}')"),
                rhai: format!("datetime(\"{date}T{time}{offset}\")"),
                of: "datetime",
                units: GQL_TEMPORAL_DATETIME_UNITS,
            }
        }
        _ => {
            let months = rng.below(400) as i64 - 200;
            let days = rng.below(2000) as i64 - 1000;
            let seconds = rng.below(400_000) as i64 - 200_000;
            let micros = if rng.chance(50) {
                0
            } else {
                rng.below(1_000_000) as i64
            };
            // The engine's own reading of the parts: months, days and signed seconds.
            let total = seconds * 1_000_000 + micros;
            let sign = if total < 0 { "-" } else { "" };
            let (whole, fraction) = (total.abs() / 1_000_000, total.abs() % 1_000_000);
            let text = format!("P{months}M{days}DT{sign}{whole}.{fraction:06}S");
            GqlTemporal {
                map: format!(
                    "duration({{months: {months}, days: {days}, seconds: {seconds}, \
                     microseconds: {micros}}})"
                ),
                text: format!("duration('{text}')"),
                rhai: format!("duration(\"{text}\")"),
                of: "duration",
                units: &[],
            }
        }
    }
}

/// A random duration without months (so `x + d - d = x` is exact for every instant type).
#[cfg(feature = "gql")]
fn random_gql_exact_duration(rng: &mut Rng) -> String {
    format!(
        "duration({{days: {}, hours: {}, seconds: {}, microseconds: {}}})",
        rng.below(800) as i64 - 400,
        rng.below(100) as i64 - 50,
        rng.below(200_000) as i64 - 100_000,
        rng.below(1_000_000) as i64 - 500_000
    )
}

/// The twin conditions of one random temporal value: each must be TRUE.
#[cfg(feature = "gql")]
fn random_gql_temporal_twins(rng: &mut Rng, value: &GqlTemporal) -> Vec<String> {
    let (x, of) = (&value.map, value.of);
    let mut twins = vec![
        // The text constructor equals the map constructor.
        format!("{} = {x}", value.text),
        // Cypher toString (the Neo4j text) and ISO CAST (the engine text) read back.
        format!("{of}(toString({x})) = {x}"),
        format!("{of}(CAST({x} AS STRING)) = {x}"),
    ];
    if of == "duration" {
        let other = random_gql_temporal(rng);
        let d = if other.of == "duration" {
            other.map
        } else {
            random_gql_exact_duration(rng)
        };
        twins.push(format!("{x} + {d} - {d} = {x}"));
        twins.push(format!("{x} * 2 - {x} = {x}"));
        twins.push(format!("-(-{x}) = {x}"));
    } else {
        let d = random_gql_exact_duration(rng);
        twins.push(format!("{x} + {d} - {d} = {x}"));
        twins.push(format!("{d} + {x} - {d} = {x}"));
        let unit = rng.pick(value.units);
        let once = format!("{of}.truncate('{unit}', {x})");
        twins.push(format!("{of}.truncate('{unit}', {once}) = {once}"));
        if of != "localtime" && of != "date" && rng.chance(50) {
            // A date unit of a value with a date, as a date.
            let unit = rng.pick(GQL_TEMPORAL_DATE_UNITS);
            let once = format!("date.truncate('{unit}', {x})");
            twins.push(format!("date.truncate('{unit}', {once}) = {once}"));
        }
        // duration.between gives the duration that leads back to the value.
        if of != "localtime" {
            let other = random_gql_temporal(rng);
            if other.of == of {
                let y = &other.map;
                twins.push(format!(
                    "duration.inSeconds({x}, {y}) = duration.inSeconds({x}, {x} + \
                     duration.inSeconds({x}, {y}))"
                ));
            }
        }
    }
    twins
}

/// The rows of `query` on a fresh empty graph, or the diagnostic.
#[cfg(feature = "gql")]
fn gql_temporal_rows(query: &str) -> Result<Vec<ElementProperty>, String> {
    let graph = TraversalGraph::new();
    graph
        .traversal()
        .gql(query)
        .to_list()
        .map_err(|e| format_diagnostic(&e))
}

#[cfg(feature = "gql")]
const GQL_TEMPORAL_CORPUS: &[&str] = &[
    "RETURN date('2015-W30-2') AS d, date('2015202') AS o, date('2015-Q3-21') AS q",
    "RETURN localtime('214032.142') AS t, localdatetime('2015-07-21T21:40') AS l",
    "RETURN datetime('2015-07-21T21:40:32.142[Europe/London]') AS z",
    "RETURN datetime({year: 1984, month: 10, day: 11, hour: 12, timezone: 'Europe/Berlin'}) AS z",
    "RETURN date({year: 1984, week: 1, dayOfWeek: 7}).weekYear AS w",
    "RETURN duration({years: 0.5, months: 1.25, days: -0.75}) AS d",
    "RETURN duration('P1Y2M3W4DT5H6M7.5S').monthsOfYear AS m",
    "RETURN date.truncate('millennium', date('2017-10-11')) AS d",
    "RETURN datetime.truncate('week', datetime('2015-07-21T21:40+01:00'), {dayOfWeek: 7, timezone: 'Europe/Paris'}) AS d",
    "RETURN duration.between(localtime('12:00'), datetime('2015-07-21T21:40+01:00')) AS d",
    "RETURN duration.inMonths(date('2015-07-21'), localdatetime('2016-07-21T00:00')) AS d",
    "RETURN date('2020-02-29') + duration('P1Y') AS d, localtime('23:00') + duration('PT2H') AS t",
    "RETURN duration('P1M') < duration('P30D') AS lt, duration('P1D') / 0 AS z",
    "RETURN datetime.fromepoch(9223372036854775807, 0) AS e",
    "RETURN datetime.fromepochmillis(-9223372036854775808) AS e",
    "RETURN duration({seconds: 9.3e18}) AS d",
    "RETURN duration('P9223372036854775807Y') AS d",
    "RETURN date('+999999999-12-31') AS d",
    "RETURN localtime({hour: 12, nanosecond: 999999999}) AS t",
    "RETURN time('12:00+01:00') AS t, localtime.realtime() AS r",
    "RETURN date.statement() AS a, localdatetime.transaction('Europe/Berlin') AS b, datetime({timezone: '+05:00'}) AS c",
    "RETURN toString(duration('-PT1M0.001S')) AS s, CAST(localtime('12:00') AS STRING) AS c",
    "MATCH (p:person) RETURN p.name, date({year: p.age + 1950}) AS born ORDER BY born",
    "FOR x IN [date('2015-01-01'), localtime('12:00'), duration('P1D'), NULL] RETURN x.year, x.hour, x.days",
];

#[cfg(feature = "gql")]
const GQL_TEMPORAL_TOKENS: &[&str] = &[
    "RETURN",
    "date(",
    "localtime(",
    "local_datetime(",
    "datetime(",
    "duration(",
    "date.truncate(",
    "duration.between(",
    "datetime.fromepoch(",
    "'2015-07-21'",
    "'12:31:14.645876'",
    "'P1Y2M'",
    "'+01:00'",
    "'Europe/Berlin'",
    "{year: 2015",
    "month: 2",
    "week: 53",
    "timezone:",
    "nanosecond: 1",
    "})",
    ")",
    "(",
    ",",
    "+",
    "-",
    "*",
    "/",
    "<",
    "=",
    ".year",
    ".epochMillis",
    ".monthsOfYear",
    "'millennium'",
    "toString(",
    "AS x",
    "NULL",
    "9223372036854775807",
    "-0.5",
];

#[test]
#[cfg(feature = "gql")]
fn random_gql_temporal_queries_never_panic_and_equal_their_twins() {
    let mut fatal = Fatal::default();
    let mut differences = Vec::new();
    let mut rng = Rng::new(0x3_6DA7_E000);
    let truth = gql_temporal_rows("RETURN true AS same").expect("a constant row");
    let mut texts: Vec<String> = GQL_TEMPORAL_CORPUS.iter().map(|s| s.to_string()).collect();
    let mut checked = 0usize;
    for _ in 0..budget(60) {
        let value = random_gql_temporal(&mut rng);
        let twins = random_gql_temporal_twins(&mut rng, &value);
        let literal = format!("RETURN {} AS x", value.map);
        texts.push(literal.clone());
        texts.extend(twins.iter().map(|twin| format!("RETURN {twin} AS same")));
        let label = literal.clone();
        let rhai = value.rhai.clone();
        let truth = truth.clone();
        // A fresh graph for every case.
        let outcome = guarded(WATCHDOG, move || {
            let mut failed = Vec::new();
            for twin in &twins {
                let query = format!("RETURN {twin} AS same");
                match gql_temporal_rows(&query) {
                    Ok(rows) if rows == truth => {}
                    other => failed.push(format!("{query}\n  {other:?}")),
                }
            }
            // The constructor equals the Rhai literal of the same value.
            let graph = Arc::new(Graph::new(TraversalGraph::new()));
            let params: HashMap<String, ElementProperty> =
                [("q".to_string(), ElementProperty::from(literal.as_str()))].into();
            let same = eval_value_with_limits(
                graph,
                &format!("g.gql(q).next()[\"x\"] == {rhai}"),
                params,
                &ScriptLimits::default(),
            );
            if !matches!(same.as_ref().map(|v| v.as_bool()), Ok(Ok(true))) {
                failed.push(format!("{literal} vs Rhai {rhai}: {same:?}"));
            }
            (twins.len() + 1, failed)
        });
        match outcome {
            Ok((count, failed)) => {
                checked += count;
                differences.extend(failed);
            }
            Err(_) => fatal.check(&label, outcome.map(|_| ())),
        }
    }
    assert!(
        checked >= 5 * budget(60) as usize,
        "only {checked} twins ran"
    );
    assert!(
        differences.is_empty(),
        "{} temporal twins differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
    for _ in 0..budget(30) {
        let base = rng.pick(GQL_TEMPORAL_CORPUS).as_bytes();
        let bytes = mutate(&mut rng, base, GQL_SNIPPETS);
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    for _ in 0..budget(30) {
        let len = 1 + rng.below(14);
        let soup: Vec<&str> = (0..len).map(|_| *rng.pick(GQL_TEMPORAL_TOKENS)).collect();
        texts.push(soup.join(" "));
    }
    for text in texts {
        let label = text.clone();
        fatal.check(&label, exercise_gql(text));
    }
    fatal.assert_empty("GQL temporal");
}
