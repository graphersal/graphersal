//! Tests of the effect inference (`effects.rs`): the table covers registered names only, the
//! inference is exactly the authorization check's mapping on curated chains, and it covers
//! every write any documented example makes.

use std::sync::{Arc, Mutex};

use super::effects::{self, BodyEffects};
#[cfg(all(feature = "serde", feature = "io", feature = "persist"))]
use super::effects::{CLASSES, StepClass};
use crate::graph::{AccessRequest, AccessRequestBuf, Action, Authorizer, Denied, Resource};
#[cfg(all(feature = "serde", feature = "io", feature = "persist"))]
use crate::script::registered_step_names;
use crate::script::{ScriptLimits, engine, eval_value_with_limits};

/// An authorizer that allows everything and records every write it is asked about.
#[derive(Default)]
struct Recorder(Mutex<Vec<AccessRequestBuf>>);

impl Authorizer for Recorder {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        if matches!(
            request.action,
            Action::Create | Action::Update | Action::Delete
        ) && let Ok(mut seen) = self.0.lock()
        {
            let request = request.to_owned_request();
            if !seen.contains(&request) {
                seen.push(request);
            }
        }
        Ok(())
    }
}

/// The writes running `script` on a fresh modern graph asks for, and how it ended.
fn recorded(script: &str) -> (Vec<AccessRequestBuf>, Result<(), String>) {
    let recorder = Arc::new(Recorder::default());
    let outcome = eval_value_with_limits(
        Arc::new(crate::GraphSource::tinkerpop_modern()),
        script,
        Default::default(),
        &ScriptLimits::default(),
        recorder.clone(),
    )
    .map(|_| ())
    .map_err(|err| err.to_string());
    let writes = recorder
        .0
        .lock()
        .map(|seen| seen.clone())
        .unwrap_or_default();
    (writes, outcome)
}

/// The inferred effects of `script`.
fn inferred(script: &str) -> Result<BodyEffects, String> {
    let ast = engine().compile(script).map_err(|err| err.to_string())?;
    effects::analyze(&ast)
}

fn texts(requests: &[AccessRequestBuf]) -> Vec<String> {
    let mut texts: Vec<String> = requests.iter().map(ToString::to_string).collect();
    texts.sort();
    texts
}

// The classified names include steps that only `serde`, `io` and `persist` register.
#[cfg(all(feature = "serde", feature = "io", feature = "persist"))]
#[test]
fn every_classified_name_is_registered() {
    let registered = registered_step_names();
    let mut unknown = Vec::new();
    for (names, class) in CLASSES {
        for name in *names {
            // Rhai's own functions behind `Fn(..)` / `call` / `curry` / `eval`.
            if *class == StepClass::Dynamic {
                continue;
            }
            if !registered.iter().any(|known| known == name) {
                unknown.push(*name);
            }
        }
    }
    assert!(unknown.is_empty(), "not registered: {unknown:?}");
}

#[test]
fn chains_infer_exactly_the_requests_of_the_authorization_check() {
    let cases: &[(&str, &[&str])] = &[
        (
            r#"g.add_v("person").property("name", "x").iterate()"#,
            &[
                r#"Create Data(vertex "person")"#,
                r#"Update Data(vertex "person")"#,
            ],
        ),
        (
            r#"g.add_v(["a", "b"]).iterate()"#,
            &[r#"Create Data(vertex "a")"#, r#"Create Data(vertex "b")"#],
        ),
        (r#"g.add_v().iterate()"#, &["Create Data(vertex)"]),
        (
            r#"g.e().has_label("knows").drop().iterate()"#,
            &[r#"Delete Data(edge "knows")"#],
        ),
        (
            r#"g.e().has_label("knows", "created").limit(1).property("w", 1).iterate()"#,
            &[
                r#"Update Data(edge "created")"#,
                r#"Update Data(edge "knows")"#,
            ],
        ),
        // A vertex has a label set: has_label states the kind only.
        (
            r#"g.v().has_label("person").property("x", 1).iterate()"#,
            &["Update Data(vertex)"],
        ),
        (
            r#"g.v().out_e("knows").order().property("w", 1).iterate()"#,
            &[r#"Update Data(edge "knows")"#],
        ),
        (
            r#"g.v().limit(2).drop().iterate()"#,
            &["Delete Data(vertex)"],
        ),
        (
            r#"g.v().values("name").limit(1).drop().iterate()"#,
            &["Delete Data"],
        ),
        (
            r#"g.v().side_effect(__.property("x", 1)).iterate()"#,
            &["Update Data"],
        ),
        (
            r#"g.add_e("knows").from(__.v("1")).to(__.v("2")).property("w", 1).iterate()"#,
            &[
                r#"Create Data(edge "knows")"#,
                r#"Update Data(edge "knows")"#,
            ],
        ),
        (
            r#"g.v().add_label("x").property("a", 1).iterate()"#,
            &["Update Data(vertex)"],
        ),
        (
            r#"g.e().has_label("knows").set_label("x").property("a", 1).iterate()"#,
            &[r#"Update Data(edge "knows")"#, "Update Data(edge)"],
        ),
        (
            r#"g.merge_v(#{name: "x"}).iterate()"#,
            &["Create Data(vertex)", "Update Data(vertex)"],
        ),
        (
            r#"let t = g.e().has_label("knows"); t.drop().iterate()"#,
            &["Delete Data"],
        ),
    ];
    for (script, expected) in cases {
        let (writes, outcome) = recorded(script);
        assert!(outcome.is_ok(), "{script}: {outcome:?}");
        let analysis = inferred(script).unwrap();
        let mut expected: Vec<String> = expected.iter().map(ToString::to_string).collect();
        expected.sort();
        assert_eq!(texts(&analysis.writes), expected, "inferred for {script}");
        // A variable's chain is folded by the engine, never by the inference: it may be
        // coarser, never finer.
        if !script.starts_with("let ") {
            assert_eq!(texts(&writes), expected, "asked for {script}");
        }
        for write in &writes {
            assert!(
                analysis
                    .writes
                    .iter()
                    .any(|effect| effects::covers(effect, &write.as_request())),
                "{script}: {write} not covered"
            );
        }
    }
}

#[test]
fn the_inference_covers_every_write_of_every_documented_example() {
    let mut problems = Vec::new();
    let mut writing = 0;
    for (receiver, name, example, result) in crate::script::doc_examples() {
        if result.is_none() {
            continue;
        }
        let (writes, _) = recorded(example);
        if writes.is_empty() {
            continue;
        }
        writing += 1;
        match inferred(example) {
            // A refused body may write what it likes: it is never a saved query.
            Err(_) => {}
            Ok(analysis) => {
                for write in &writes {
                    let request = write.as_request();
                    let covered = match request.resource {
                        Resource::Data { .. } => {
                            analysis.dynamic.is_some()
                                || !analysis.calls.is_empty()
                                || analysis
                                    .writes
                                    .iter()
                                    .any(|effect| effects::covers(effect, &request))
                        }
                        // A write of the schema, the catalog or a file is refused.
                        _ => false,
                    };
                    if !covered {
                        problems.push(format!(
                            "{receiver}.{name}: `{example}` asks {write}, inferred {:?}",
                            texts(&analysis.writes)
                        ));
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // The catalogue documents dozens of writing functions; far fewer means the run broke.
    assert!(writing > 30, "only {writing} documented examples write");
}

#[test]
fn constructs_that_hide_calls_make_a_body_dynamic() {
    for (script, construct) in [
        (r#"fn q() { let f = Fn("drop"); 1 }"#, r#"Fn("drop")"#),
        (r#"fn q(f) { f.call(1) }"#, "call"),
        (r#"fn q(f) { call(f, 1) }"#, "call"),
        (r#"fn q(f) { f.curry(1) }"#, "curry"),
        (r#"fn q() { eval("1") }"#, "eval"),
        (
            r#"fn q(n) { g.query(n) }"#,
            "g.query(name) with a computed name",
        ),
        (r#"fn q() { import "m" as m; 1 }"#, "import"),
    ] {
        let analysis = inferred(script).unwrap();
        assert_eq!(analysis.dynamic.as_deref(), Some(construct), "{script}");
    }
    let plain = inferred(r#"fn q() { g.query("other", #{a: 1}).count() }"#).unwrap();
    assert_eq!(plain.dynamic, None);
    assert_eq!(plain.calls, ["other"]);
}

#[test]
fn writes_of_the_schema_the_catalog_and_files_are_refused() {
    for (script, part) in [
        (
            r#"fn q() { g.set_schema(#{}) }"#,
            "set_schema(), and it changes the schema",
        ),
        (
            r#"fn q() { g.drop_query("x") }"#,
            "drop_query(), and it changes the catalog",
        ),
        (
            r#"fn q() { g.export_graphml("x.xml") }"#,
            "export_graphml(), and it reads or writes a file",
        ),
        (r#"fn q() { g.mark("m") }"#, "mark()"),
    ] {
        let reason = inferred(script).unwrap_err();
        assert!(reason.contains(part), "{script}: {reason}");
    }
}

#[test]
fn soundness_cases_over_approximate() {
    // A label from a variable: any label of the kind.
    let analysis = inferred(r#"fn q(l) { g.add_v(l).iterate() }"#).unwrap();
    assert_eq!(texts(&analysis.writes), ["Create Data(vertex)"]);
    // A method called like a function: nothing about its receiver.
    let analysis = inferred(r#"fn q() { drop(g.e().has_label("knows")).iterate() }"#).unwrap();
    assert_eq!(texts(&analysis.writes), ["Delete Data"]);
    // A same-named method on a map counts as a write.
    let analysis = inferred(r#"fn q() { let m = #{}; m.drop() }"#).unwrap();
    assert_eq!(texts(&analysis.writes), ["Delete Data"]);
    // A subgraph into another graph creates data.
    let analysis =
        inferred(r#"fn q(t) { g.with_side_effect("sg", t).e().subgraph("sg").iterate() }"#)
            .unwrap();
    assert_eq!(texts(&analysis.writes), ["Create Data"]);
    let analysis = inferred(r#"fn q() { g.with_side_effect("x", [1]).v().iterate() }"#).unwrap();
    assert!(analysis.writes.is_empty());
    // A read-only body.
    let analysis = inferred(r#"fn q() { g.v().has_label("person").values("name") }"#).unwrap();
    assert_eq!(analysis, BodyEffects::default());
}

#[test]
fn transitive_effects_follow_calls_that_count_once() {
    let node = |writes: &[&str], calls: &[&str], counts: bool| effects::Node {
        effects: BodyEffects {
            writes: writes
                .iter()
                .map(|label| {
                    AccessRequest::new(Action::Create, Resource::data(None, Some(label)))
                        .to_owned_request()
                })
                .collect(),
            calls: calls.iter().map(ToString::to_string).collect(),
            dynamic: None,
        },
        counts,
    };
    let mut lookup = |name: &str| match name {
        "a" => Some(node(&["a"], &["b", "c", "a"], true)),
        "b" => Some(node(&["b"], &["a"], true)),
        "c" => Some(node(&["c"], &[], false)),
        _ => None,
    };
    let (writes, dynamic) = effects::transitive("a", &mut lookup);
    assert_eq!(
        texts(&writes),
        [r#"Create Data("a")"#, r#"Create Data("b")"#]
    );
    assert_eq!(dynamic, None);
}

#[cfg(feature = "gql")]
#[test]
fn gql_calls_infer_exactly_the_requests_of_the_authorization_check() {
    let cases: &[(&str, &[&str])] = &[
        (
            r#"g.gql("INSERT (:person {name: 'x'})").iterate()"#,
            &[r#"Create Data(vertex "person")"#],
        ),
        (
            r#"g.gql("INSERT (:a&b)-[:r]->(:c)").iterate()"#,
            &[
                r#"Create Data(vertex "a")"#,
                r#"Create Data(vertex "b")"#,
                r#"Create Data(vertex "c")"#,
                r#"Create Data(edge "r")"#,
            ],
        ),
        (
            r#"g.gql("MATCH (p:person) INSERT (p)-[:owns]->(:pet {owner: p.name})").iterate()"#,
            &[
                r#"Create Data(vertex "pet")"#,
                r#"Create Data(edge "owns")"#,
            ],
        ),
        (
            r#"g.gql("MATCH (p:person) CALL (p) { INSERT (p)-[:owns]->(:pet) } RETURN p").iterate()"#,
            &[
                r#"Create Data(vertex "pet")"#,
                r#"Create Data(edge "owns")"#,
            ],
        ),
        (
            r#"g.gql("FOR x IN $xs INSERT (:f {x: x})", #{xs: [1, 2]}).iterate()"#,
            &[r#"Create Data(vertex "f")"#],
        ),
        // The flow after the query continues into Gremlin steps: the last created element.
        (
            r#"g.gql("INSERT (:a)-[e:r]->(:b) RETURN e").select("e").property("w", 1).iterate()"#,
            &[
                r#"Create Data(vertex "a")"#,
                r#"Create Data(vertex "b")"#,
                r#"Create Data(edge "r")"#,
                "Update Data",
            ],
        ),
        (r#"g.gql("MATCH (p) RETURN p").iterate()"#, &[]),
        (
            r#"g.gql("MATCH (p) RETURN p LIMIT $k", #{k: 1}).iterate()"#,
            &[],
        ),
    ];
    for (script, expected) in cases {
        let (writes, outcome) = recorded(script);
        assert!(outcome.is_ok(), "{script}: {outcome:?}");
        let analysis = inferred(script).unwrap();
        let mut expected: Vec<String> = expected.iter().map(ToString::to_string).collect();
        expected.sort();
        assert_eq!(texts(&analysis.writes), expected, "inferred for {script}");
        assert_eq!(texts(&writes), expected, "asked for {script}");
    }
    // A query text the AST does not give claims every data write; it covers what runs.
    let script = r#"let q = "INSERT (:x)"; g.gql(q).iterate()"#;
    let (writes, outcome) = recorded(script);
    assert!(outcome.is_ok(), "{outcome:?}");
    let analysis = inferred(script).unwrap();
    assert_eq!(
        texts(&analysis.writes),
        ["Create Data", "Delete Data", "Update Data"]
    );
    for write in &writes {
        assert!(
            analysis
                .writes
                .iter()
                .any(|effect| effects::covers(effect, &write.as_request())),
            "{write} not covered"
        );
    }
    // A query that does not parse writes nothing (it fails before it runs).
    assert!(
        inferred(r#"g.gql("INSERT (:x").iterate()"#)
            .unwrap()
            .writes
            .is_empty()
    );
}
