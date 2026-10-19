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
            filler.push_str(*self.rng.pick(&["*", "*2", "*1..2", "*..3", "*0.."]));
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
            tail.push_str(*self.rng.pick(&[" OFFSET 1", " SKIP $o"]));
        }
        if self.rng.chance(20) {
            tail.push_str(*self.rng.pick(&[" LIMIT 2", " LIMIT $n"]));
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
