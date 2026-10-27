//! The Graph panel's filter (`Session::graph_view` with `filter`, `Session::neighbourhood` with
//! `filter`, `Session::graph_filter_query`) and the degree columns.

use std::collections::BTreeSet;

use graphersal_session::{RunOptions, Session};
use serde_json::{Value, json};

fn modern() -> Session {
    let mut s = Session::new();
    s.load_sample("modern").unwrap();
    s
}

fn run(s: &mut Session, script: &str) -> Value {
    let out: Value = serde_json::from_str(&s.execute(script, &RunOptions::default())).unwrap();
    assert_eq!(out["ok"], true, "{script}: {out}");
    out
}

fn strings(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
        .collect()
}

/// The filtered drawing with limits large enough to draw everything: (vertex ids, edge ids).
fn drawn(s: &Session, filter: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
    let view = view(
        s,
        json!({"maxNodes": 100_000, "maxEdges": 100_000, "filter": filter}),
    );
    assert_eq!(view["truncated"], false, "{view}");
    assert_eq!(
        view["matchedNodes"],
        view["vertexIds"].as_array().unwrap().len()
    );
    assert_eq!(
        view["matchedEdges"],
        view["edgeIds"].as_array().unwrap().len()
    );
    (strings(&view["vertexIds"]), strings(&view["edgeIds"]))
}

fn view(s: &Session, request: Value) -> Value {
    s.graph_view(&request.to_string()).unwrap()
}

/// The element ids the filter's query text selects, run as a script.
fn queried(s: &mut Session, filter: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
    let text = s.graph_filter_query(&filter.to_string()).unwrap();
    let ids = text.replace(".to_list()", ".id().to_list()");
    let out = run(s, &ids);
    let r = &out["execution"]["results"][0];
    (strings(&r["vertices"]), strings(&r["edges"]))
}

/// The drawing equals what the query text selects.
fn check_equivalent(s: &mut Session, filter: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
    let d = drawn(s, filter);
    assert_eq!(d, queried(s, filter), "filter {filter}");
    d
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn persons(conditions: Value) -> Value {
    json!({"vertices": {"person": conditions}})
}

#[test]
fn every_operator_on_modern() {
    let mut s = modern();
    // marko 1 (29), vadas 2 (27), josh 4 (32), peter 6 (35); software lop 3, ripple 5.
    let cases = [
        (
            json!([{"key": "name", "op": "eq", "value": "marko"}]),
            vec!["1"],
        ),
        (
            json!([{"key": "name", "op": "neq", "value": "marko"}]),
            vec!["2", "4", "6"],
        ),
        (json!([{"key": "age", "op": "lt", "value": 29}]), vec!["2"]),
        (
            json!([{"key": "age", "op": "lte", "value": 29}]),
            vec!["1", "2"],
        ),
        (json!([{"key": "age", "op": "gt", "value": 32}]), vec!["6"]),
        (
            json!([{"key": "age", "op": "gte", "value": 32}]),
            vec!["4", "6"],
        ),
        // Low inclusive, high exclusive: josh (32) is out.
        (
            json!([{"key": "age", "op": "between", "value": [29, 32]}]),
            vec!["1"],
        ),
        (
            json!([{"key": "age", "op": "gt", "value": 28.5}]),
            vec!["1", "4", "6"],
        ),
        (
            json!([{"key": "name", "op": "contains", "value": "a"}]),
            vec!["1", "2"],
        ),
        (
            json!([{"key": "name", "op": "starts_with", "value": "p"}]),
            vec!["6"],
        ),
        (
            json!([{"key": "name", "op": "ends_with", "value": "h"}]),
            vec!["4"],
        ),
        (
            json!([{"key": "name", "op": "regex", "value": "^(m|v)a"}]),
            vec!["1", "2"],
        ),
        (
            json!([{"key": "age", "op": "exists"}]),
            vec!["1", "2", "4", "6"],
        ),
        (
            json!([{"key": "lang", "op": "missing"}]),
            vec!["1", "2", "4", "6"],
        ),
        (json!([{"key": "lang", "op": "exists"}]), vec![]),
        // A number never matches a string (no sniffing).
        (json!([{"key": "age", "op": "eq", "value": "29"}]), vec![]),
    ];
    for (conditions, expected) in cases {
        let filter = persons(conditions);
        let (vertices, _) = check_equivalent(&mut s, &filter);
        assert_eq!(vertices, set(&expected), "{filter}");
    }
    // Edge conditions: knows edges with weight >= 0.5 (0: 1->2 0.5, 2: 1->4 1.0).
    let filter = json!({"edges": {"knows": [{"key": "weight", "op": "gte", "value": 0.5}]}});
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert_eq!(edges, set(&["0", "2"]));
    assert_eq!(vertices.len(), 6, "a hidden edge hides no vertex");
}

#[test]
fn and_within_a_label_or_across_labels() {
    let mut s = modern();
    let filter = json!({"vertices": {
        "person": [{"key": "age", "op": "gt", "value": 28}, {"key": "name", "op": "contains", "value": "o"}],
        "software": [{"key": "lang", "op": "eq", "value": "java"}],
    }});
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["1", "3", "4", "5"]));
    // Edges between visible vertices only (1->3, 1->4, 4->3, 4->5).
    assert_eq!(edges, set(&["1", "2", "3", "4"]));
    // A label listed without conditions shows all of it; an absent label is hidden.
    let (vertices, edges) = check_equivalent(&mut s, &json!({"vertices": {"software": []}}));
    assert_eq!(vertices, set(&["3", "5"]));
    assert!(edges.is_empty());
    // An empty object hides every vertex; `null` fields show everything.
    let (vertices, _) = check_equivalent(&mut s, &json!({"vertices": {}}));
    assert!(vertices.is_empty());
    let (vertices, edges) = check_equivalent(&mut s, &json!({"vertices": null, "edges": null}));
    assert_eq!((vertices.len(), edges.len()), (6, 6));
}

#[test]
fn a_multi_label_vertex_shows_when_any_label_matches() {
    let mut s = modern();
    run(&mut s, r#"g.v("1").add_label("employee").iterate()"#);
    let (vertices, _) = check_equivalent(&mut s, &json!({"vertices": {"employee": []}}));
    assert_eq!(vertices, set(&["1"]));
    // Listed under its second label with a condition that fails, under its first that holds.
    let filter = json!({"vertices": {
        "employee": [{"key": "age", "op": "gt", "value": 100}],
        "person": [{"key": "name", "op": "eq", "value": "marko"}],
    }});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["1"]));
}

#[test]
fn hide_isolated_hides_vertices_without_a_visible_edge() {
    let mut s = modern();
    let filter = json!({"edges": {"created": []}, "hideIsolated": true});
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert_eq!(edges, set(&["1", "3", "4", "5"]));
    assert_eq!(
        vertices,
        set(&["1", "3", "4", "5", "6"]),
        "vadas has no created edge"
    );
    // Applied after the vertex filter: josh's created edges lead to visible software only.
    let filter = json!({
        "vertices": {"person": [{"key": "name", "op": "eq", "value": "josh"}], "software": []},
        "edges": {"created": []},
        "hideIsolated": true,
    });
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["3", "4", "5"]));
    assert_eq!(edges, set(&["3", "4"]));
}

#[test]
fn the_limits_cut_after_filtering() {
    let mut s = Session::new();
    s.load_sample("empty").unwrap();
    run(
        &mut s,
        r#"for i in 0..300 { g.add_v("filler").property("n", i).iterate(); }
           for i in 0..20 { g.add_v("target").property("n", i).iterate(); }
           let ts = g.v().has_label("target").id().to_list();
           for i in 1..20 { g.v(ts[i - 1]).add_e("next").to(ts[i]).iterate(); }"#,
    );
    let filter = json!({"vertices": {"target": [{"key": "n", "op": "gte", "value": 5}]}});
    let cut = view(&s, json!({"maxNodes": 10, "maxEdges": 4, "filter": filter}));
    let ids = cut["vertexIds"].as_array().unwrap();
    assert_eq!(ids.len(), 10, "{cut}");
    assert_eq!(cut["matchedNodes"], 15);
    assert_eq!(cut["matchedEdges"], 14);
    assert_eq!(cut["edgeIds"].as_array().unwrap().len(), 4);
    assert_eq!(cut["truncated"], true);
    assert_eq!(cut["totalNodes"], 320);
    // Every drawn vertex is a matching target (the first 10 vertices of the graph are fillers).
    let (all, _) = check_equivalent(&mut s, &filter);
    for id in ids {
        assert!(all.contains(id.as_str().unwrap()));
    }
    // Without the filter the same limits draw only fillers.
    let plain = view(&s, json!({"maxNodes": 10, "maxEdges": 4}));
    assert!(plain.get("matchedNodes").is_none());
    let plain_ids = strings(&plain["vertexIds"]);
    assert!(plain_ids.iter().all(|id| !all.contains(id)));
    assert_eq!(
        plain,
        serde_json::to_value(s.graph().graph_view(10, 4).unwrap()).unwrap(),
        "no filter: exactly the plain view"
    );
}

#[test]
fn the_neighbourhood_filter_drops_what_it_hides() {
    let s = modern();
    let all = s.neighbourhood(r#"{"ids": ["1"], "hops": 1}"#).unwrap();
    assert_eq!(all["vertexIds"].as_array().unwrap().len(), 4);
    assert!(all.get("matchedNodes").is_none());
    let request = json!({
        "ids": ["1"], "hops": 1,
        "filter": {"vertices": {"person": []}, "edges": {"knows": [{"key": "weight", "op": "gt", "value": 0.6}]}},
    });
    let view = s.neighbourhood(&request.to_string()).unwrap();
    assert_eq!(strings(&view["vertexIds"]), set(&["1", "2", "4"]));
    assert_eq!(strings(&view["edgeIds"]), set(&["2"]));
    assert_eq!(view["matchedNodes"], 3);
    assert_eq!(view["matchedEdges"], 1);
    assert_eq!(view["seeds"], json!([0]));
    let request = json!({
        "ids": ["1"], "hops": 1,
        "filter": {"vertices": {"person": []}, "edges": {"knows": [{"key": "weight", "op": "gt", "value": 0.6}]}, "hideIsolated": true},
    });
    let view = s.neighbourhood(&request.to_string()).unwrap();
    assert_eq!(strings(&view["vertexIds"]), set(&["1", "4"]));
    // A bad filter is an error, like a bad request.
    let bad =
        json!({"ids": ["1"], "filter": {"vertices": {"person": [{"key": "age", "op": "near"}]}}});
    let err = s.neighbourhood(&bad.to_string()).unwrap_err();
    assert!(
        err.to_string().contains("person") && err.to_string().contains("near"),
        "{err}"
    );
}

#[test]
fn values_of_declared_keys_are_coerced_like_writes() {
    let mut s = Session::new();
    s.load_sample("empty").unwrap();
    run(
        &mut s,
        r#"g.set_schema(#{mode: "open", vertices: #{ev: #{schema: #{type: "object", properties: #{
               d: #{type: "string", format: "date"}, u: #{type: "string", format: "uuid"},
               x: #{type: "number"}}}}}});
           g.add_v("ev").property("name", "a").property("d", "2024-01-05")
               .property("u", "910a2dec-8902-4cc1-beeb-8da1658eec67").property("x", 2).iterate();
           g.add_v("ev").property("name", "b").property("d", "2023-01-05").property("x", 1.5).iterate();
           g.add_v("other").property("d", "2024-02-01").iterate();"#,
    );
    let filter = json!({"vertices": {"ev": [{"key": "d", "op": "gt", "value": "2024-01-01"}]}});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices.len(), 1);
    let text = s.graph_filter_query(&filter.to_string()).unwrap();
    assert!(text.contains(r#"P.gt(date("2024-01-01"))"#), "{text}");
    let filter = json!({"vertices": {"ev": [{"key": "u", "op": "eq", "value": "910a2dec-8902-4cc1-beeb-8da1658eec67"}]}});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices.len(), 1);
    let filter = json!({"vertices": {"ev": [{"key": "x", "op": "between", "value": [1, 2]}]}});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices.len(), 1, "the int64 bounds are coerced to float64");
    // A label that does not declare the key compares as given (a string with a string).
    let filter = json!({"vertices": {"other": [{"key": "d", "op": "eq", "value": "2024-02-01"}]}});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices.len(), 1);
    // A value the declared type cannot take is an error naming the place.
    let bad = json!({"vertices": {"ev": [{"key": "d", "op": "gt", "value": "05.01.2024"}]}});
    let err = s
        .graph_view(&json!({"filter": bad}).to_string())
        .unwrap_err();
    assert!(
        err.to_string().contains("vertices.ev[0]") && err.to_string().contains("key \"d\""),
        "{err}"
    );
    assert!(s.graph_filter_query(&bad.to_string()).is_err());
}

#[test]
fn invalid_filters_are_errors_naming_the_condition() {
    let s = modern();
    let cases = [
        (
            json!({"vertices": {"person": [{"key": "age", "op": "near", "value": 1}]}}),
            "unknown operator",
        ),
        (
            json!({"vertices": {"person": [{"key": "age", "op": "gt"}]}}),
            "needs a \"value\"",
        ),
        (
            json!({"vertices": {"person": [{"key": "age", "op": "exists", "value": 1}]}}),
            "takes no",
        ),
        (
            json!({"vertices": {"person": [{"key": "age", "op": "between", "value": 3}]}}),
            "[low, high]",
        ),
        (
            json!({"vertices": {"person": [{"key": "age", "op": "between", "value": [1]}]}}),
            "[low, high]",
        ),
        (
            json!({"vertices": {"person": [{"key": "name", "op": "regex", "value": "("}]}}),
            "regex",
        ),
        (
            json!({"vertices": {"person": [{"key": "name", "op": "contains", "value": 3}]}}),
            "string",
        ),
        (
            json!({"vertices": {"person": [{"key": "age", "op": "eq", "value": [1, 2]}]}}),
            "string, number",
        ),
        (
            json!({"vertices": {"person": [{"op": "eq", "value": 1}]}}),
            "\"key\"",
        ),
        (
            json!({"vertices": {"person": [{"key": "a", "op": "eq", "value": 1, "x": 2}]}}),
            "unknown field",
        ),
        (
            json!({"vertices": {"person": {"key": "age"}}}),
            "list of conditions",
        ),
        (json!({"vertices": ["person"]}), "object"),
        (json!({"hideIsolated": "yes"}), "hideIsolated"),
        (json!({"labels": {}}), "unknown filter field"),
        (json!([1]), "JSON object"),
    ];
    for (filter, needle) in cases {
        let err = s
            .graph_view(&json!({"filter": filter}).to_string())
            .unwrap_err();
        assert!(err.to_string().contains(needle), "{filter}: {err}");
        assert!(
            s.graph_filter_query(&filter.to_string()).is_err(),
            "{filter}"
        );
    }
    // The condition's place, key and operator are named.
    let err = s
        .graph_view(
            &json!({"filter": {"edges": {"knows": [{"key": "weight", "op": "gt", "value": {}}]}}})
                .to_string(),
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("edges.knows[0] (key \"weight\", op \"gt\")"),
        "{err}"
    );
    assert!(s.graph_view(r#"{"maxNodes": 1, "colour": 1}"#).is_err());
}

#[test]
fn a_filter_never_writes_and_its_query_text_is_escaped() {
    let mut s = modern();
    let nasty = r#"x"); g.v().drop(); ("\"#;
    run(
        &mut s,
        &format!(
            "g.add_v({label}).property({key}, {value}).iterate()",
            label = serde_json::to_string(nasty).unwrap(),
            key = serde_json::to_string(nasty).unwrap(),
            value = serde_json::to_string(&format!("{nasty}\n\t\u{1}end")).unwrap(),
        ),
    );
    let filter = json!({"vertices": {
        nasty: [{"key": nasty, "op": "eq", "value": format!("{nasty}\n\t\u{1}end")}],
        "person": [{"key": nasty, "op": "missing"}, {"key": "name", "op": "regex", "value": "\"; g.v().drop(); \""}],
    }, "edges": {nasty: [{"key": nasty, "op": "contains", "value": nasty}]}, "hideIsolated": false});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices.len(), 1, "the nasty vertex itself");
    let count = run(&mut s, "g.v().count().next()");
    assert_eq!(count["execution"]["results"][0], 7, "nothing was dropped");
    let text = s.graph_filter_query(&filter.to_string()).unwrap();
    assert!(!text.contains("x\"); g.v().drop()"), "{text}");
}

#[test]
fn query_text_is_readable() {
    let s = modern();
    let text = s
        .graph_filter_query(
            &json!({"vertices": {"person": [{"key": "age", "op": "gt", "value": 30}]}}).to_string(),
        )
        .unwrap();
    assert_eq!(
        text,
        "#{\n  vertices: g.v().has_label(\"person\").has(\"age\", P.gt(30)).to_list(),\n  \
         edges: g.e().filter(__.out_v().has_label(\"person\").has(\"age\", P.gt(30)))\
         .filter(__.in_v().has_label(\"person\").has(\"age\", P.gt(30))).to_list()\n}"
    );
    let text = s.graph_filter_query("{}").unwrap();
    assert_eq!(
        text,
        "#{\n  vertices: g.v().to_list(),\n  edges: g.e().to_list()\n}"
    );
}

#[test]
fn equivalence_on_a_generated_graph() {
    let mut s = Session::new();
    s.load_sample("empty").unwrap();
    run(
        &mut s,
        r#"let f = Fake(7);
           for i in 0..200 {
               let l = if i % 3 == 0 { "a" } else if i % 3 == 1 { "b" } else { "c" };
               g.add_v(l).property("n", i).property("name", f.first_name()).iterate();
           }
           let ids = g.v().id().to_list();
           for i in 0..400 {
               let s = ids[(i * 7) % 200]; let t = ids[(i * 13 + 5) % 200];
               let l = if i % 2 == 0 { "x" } else { "y" };
               g.v(s).add_e(l).to(t).property("w", i % 10).iterate();
           }"#,
    );
    let filters = [
        json!({"vertices": {"a": [{"key": "n", "op": "lt", "value": 100}], "b": []}}),
        json!({"vertices": {"c": [{"key": "name", "op": "regex", "value": "^[A-M]"}]}, "edges": {"x": []}}),
        json!({"edges": {"y": [{"key": "w", "op": "between", "value": [2, 5]}]}, "hideIsolated": true}),
        json!({"vertices": {"a": [], "b": [{"key": "n", "op": "gte", "value": 50}]},
               "edges": {"x": [{"key": "w", "op": "neq", "value": 3}], "y": []}, "hideIsolated": true}),
    ];
    for filter in filters {
        check_equivalent(&mut s, &filter);
    }
}

#[test]
fn degrees_are_whole_graph_counts() {
    let mut s = modern();
    let expected = |s: &mut Session, id: &str| {
        let r = run(
            s,
            &format!(r#"[g.v("{id}").in_e().count().next(), g.v("{id}").out_e().count().next()]"#),
        );
        let pair = &r["execution"]["results"];
        (pair[0].as_u64().unwrap(), pair[1].as_u64().unwrap())
    };
    let check = |s: &mut Session, view: &Value| {
        let ids = view["vertexIds"].as_array().unwrap();
        assert_eq!(view["vertexInDegree"].as_array().unwrap().len(), ids.len());
        for (i, id) in ids.iter().enumerate() {
            let (inn, out) = expected(s, id.as_str().unwrap());
            assert_eq!(view["vertexInDegree"][i], inn, "{id}");
            assert_eq!(view["vertexOutDegree"][i], out, "{id}");
        }
    };
    // A self-loop counts once in and once out.
    run(&mut s, r#"g.v("1").add_e("self").to("1").iterate()"#);
    let plain = view(&s, json!({"degrees": true}));
    check(&mut s, &plain);
    // Unaffected by the filter and by the cut.
    let filtered = view(
        &s,
        json!({"maxNodes": 2, "degrees": true, "filter": {"vertices": {"person": []}, "edges": {"self": []}}}),
    );
    assert_eq!(filtered["vertexIds"].as_array().unwrap().len(), 2);
    check(&mut s, &filtered);
    let marko = filtered["vertexIds"]
        .as_array()
        .unwrap()
        .iter()
        .position(|id| id == "1")
        .unwrap();
    assert_eq!(filtered["vertexOutDegree"][marko], 4);
    assert_eq!(filtered["vertexInDegree"][marko], 1);
    // The neighbourhood, a label sample and a result's subgraph.
    let n = s
        .neighbourhood(
            r#"{"ids": ["4"], "degrees": true, "filter": {"vertices": {"software": []}}}"#,
        )
        .unwrap();
    check(&mut s, &n);
    let sample = s
        .label_sample(r#"{"vertices": {"labels": ["person"]}, "degrees": true}"#)
        .unwrap();
    check(&mut s, &sample);
    let options = RunOptions {
        subgraph_degrees: true,
        ..RunOptions::default()
    };
    let out: Value =
        serde_json::from_str(&s.execute("g.v().has_label(\"software\")", &options)).unwrap();
    check(&mut s, &out["subgraph"]);
    // Absent when not asked.
    let out: Value = serde_json::from_str(&s.execute("g.v()", &RunOptions::default())).unwrap();
    assert!(out["subgraph"].get("vertexInDegree").is_none());
    assert!(view(&s, json!({})).get("vertexInDegree").is_none());
    assert!(
        s.neighbourhood(r#"{"ids": ["4"]}"#)
            .unwrap()
            .get("vertexOutDegree")
            .is_none()
    );
}

/// The generic helpers answer the same over another storage (`Forwarding`: own handle types).
#[test]
fn a_foreign_storage_draws_the_same() {
    use graphersal::prelude::*;
    use graphersal_session::views::{GraphViewRequest, graph_filter_query, graph_view_request};
    use graphersal_storage_tests::forwarding::Forwarding;
    let tg = TraversalGraph::tinkerpop_modern();
    let fw = Forwarding::new(TraversalGraph::tinkerpop_modern());
    let filters = [
        json!({"vertices": {"person": [{"key": "age", "op": "gt", "value": 28}], "software": []}}),
        json!({"edges": {"created": [{"key": "weight", "op": "lt", "value": 0.5}]}, "hideIsolated": true}),
    ];
    for filter in filters {
        let request = GraphViewRequest {
            max_nodes: Some(3),
            degrees: true,
            filter: Some(filter.clone()),
            ..GraphViewRequest::default()
        };
        let a = serde_json::to_value(graph_view_request(&tg, &request).unwrap()).unwrap();
        let b = serde_json::to_value(graph_view_request(&fw, &request).unwrap()).unwrap();
        assert_eq!(a, b, "{filter}");
        assert_eq!(
            graph_filter_query(&tg, &filter).unwrap(),
            graph_filter_query(&fw, &filter).unwrap()
        );
    }
}

// ------------------------------------------------------------------ others visible, hidden labels

#[test]
fn others_visible_shows_every_label_not_hidden_or_restricted() {
    let mut s = modern();
    // Hidden labels only: everything else shows, with no condition.
    let filter = json!({"otherVertices": "visible", "hiddenVertices": ["software"]});
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["1", "2", "4", "6"]));
    assert_eq!(
        edges,
        set(&["0", "2"]),
        "knows edges only (both ends persons)"
    );
    // A condition on one label restricts that label; every other label is visible unconditionally.
    let filter = json!({"otherVertices": "visible",
        "vertices": {"person": [{"key": "age", "op": "gt", "value": 30}]}});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["3", "4", "5", "6"]));
    // A listed label without conditions restricts nothing; a hidden label wins over a listing.
    let filter = json!({"otherVertices": "visible", "hiddenVertices": ["person"],
        "vertices": {"person": [{"key": "age", "op": "gt", "value": 30}], "software": []}});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["3", "5"]));
    // Edges: the same rules, endpoints still checked.
    let filter = json!({"otherEdges": "visible", "hiddenEdges": ["created"],
        "edges": {"knows": [{"key": "weight", "op": "gte", "value": 0.5}]}});
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert_eq!(edges, set(&["0", "2"]));
    assert_eq!(vertices.len(), 6);
    let filter = json!({"otherEdges": "visible", "hiddenEdges": ["knows"],
        "otherVertices": "visible", "vertices": {"software": [{"key": "lang", "op": "eq", "value": "nope"}]},
        "hideIsolated": true});
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert!(edges.is_empty(), "created edges end at hidden software");
    assert!(vertices.is_empty());
    // Nothing hidden and no condition: nothing is restricted.
    let filter =
        json!({"otherVertices": "visible", "otherEdges": "visible", "vertices": {"person": []}});
    let (vertices, edges) = check_equivalent(&mut s, &filter);
    assert_eq!((vertices.len(), edges.len()), (6, 6));
}

#[test]
fn hidden_labels_apply_to_the_allow_list_form_too() {
    let mut s = modern();
    // The default of `otherVertices` with a map is "hidden": the old form, unchanged.
    let old =
        json!({"vertices": {"person": [{"key": "age", "op": "gt", "value": 28}], "software": []}});
    let explicit = json!({"otherVertices": "hidden",
        "vertices": {"person": [{"key": "age", "op": "gt", "value": 28}], "software": []}});
    assert_eq!(
        check_equivalent(&mut s, &old),
        check_equivalent(&mut s, &explicit)
    );
    let filter =
        json!({"vertices": {"person": [], "software": []}, "hiddenVertices": ["software"]});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["1", "2", "4", "6"]));
    // Without a map the default is "visible": hidden labels alone hide just those.
    let (vertices, _) = check_equivalent(&mut s, &json!({"hiddenVertices": ["person"]}));
    assert_eq!(vertices, set(&["3", "5"]));
    // "hidden" without a map lists nothing: every vertex hidden.
    let (vertices, _) = check_equivalent(&mut s, &json!({"otherVertices": "hidden"}));
    assert!(vertices.is_empty());
}

#[test]
fn others_visible_with_several_labels_needs_every_restriction() {
    let mut s = modern();
    run(&mut s, r#"g.v("1").add_label("employee").iterate()"#);
    // marko carries person (condition holds) and employee (condition fails): hidden.
    let filter = json!({"otherVertices": "visible", "vertices": {
        "employee": [{"key": "age", "op": "gt", "value": 100}],
        "person": [{"key": "name", "op": "eq", "value": "marko"}],
    }});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["3", "5"]));
    // A hidden second label hides the vertex.
    let filter = json!({"otherVertices": "visible", "hiddenVertices": ["employee"]});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices, set(&["2", "3", "4", "5", "6"]));
}

/// Many labels (more than the statistics list and than a variadic step takes): the counts cover
/// every label, and the query text stays runnable.
#[test]
fn others_visible_covers_labels_beyond_any_list() {
    let mut s = Session::new();
    s.load_sample("empty").unwrap();
    run(
        &mut s,
        r#"for i in 0..300 { g.add_v("l" + i).property("n", i).iterate(); }
           let ids = g.v().id().to_list();
           for i in 1..300 { g.v(ids[i - 1]).add_e("e" + (i % 20)).to(ids[i]).property("w", i).iterate(); }"#,
    );
    let hidden: Vec<String> = (0..12).map(|i| format!("l{i}")).collect();
    let filter = json!({"otherVertices": "visible", "hiddenVertices": hidden,
        "vertices": {"l50": [{"key": "n", "op": "lt", "value": 0}]}});
    let cut = view(
        &s,
        json!({"maxNodes": 10, "maxEdges": 10, "filter": filter}),
    );
    assert_eq!(cut["matchedNodes"], 300 - 12 - 1, "{cut}");
    assert_eq!(cut["totalNodes"], 300);
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices.len(), 287);
    let text = s.graph_filter_query(&filter.to_string()).unwrap();
    assert!(
        text.contains(".not(__.has_label(P.within([\"l0\", \"l1\", "),
        "{text}"
    );
    // The allow-list form with many plain labels is runnable too (one P.within argument).
    let listed: serde_json::Map<String, Value> =
        (0..20).map(|i| (format!("l{i}"), json!([]))).collect();
    let filter = json!({"vertices": listed, "otherEdges": "visible",
        "edges": {"e3": [{"key": "w", "op": "gt", "value": 100}]}});
    let (vertices, _) = check_equivalent(&mut s, &filter);
    assert_eq!(vertices.len(), 20);
}

#[test]
fn others_visible_equivalence_on_a_generated_graph() {
    let mut s = Session::new();
    s.load_sample("empty").unwrap();
    run(
        &mut s,
        r#"let f = Fake(11);
           for i in 0..150 {
               let l = if i % 3 == 0 { "a" } else if i % 3 == 1 { "b" } else { "c" };
               g.add_v(l).property("n", i).property("name", f.first_name()).iterate();
           }
           let ids = g.v().id().to_list();
           for i in 0..300 {
               let s = ids[(i * 7) % 150]; let t = ids[(i * 13 + 5) % 150];
               let l = if i % 2 == 0 { "x" } else { "y" };
               g.v(s).add_e(l).to(t).property("w", i % 10).iterate();
           }"#,
    );
    let filters = [
        json!({"otherVertices": "visible", "hiddenVertices": ["c"],
               "vertices": {"a": [{"key": "n", "op": "lt", "value": 60}]}}),
        json!({"otherVertices": "visible", "vertices": {"b": [{"key": "name", "op": "contains", "value": "a"}]},
               "otherEdges": "visible", "edges": {"y": [{"key": "w", "op": "between", "value": [2, 6]}]},
               "hideIsolated": true}),
        json!({"otherEdges": "visible", "hiddenEdges": ["x"], "hideIsolated": true}),
        json!({"vertices": {"a": [], "b": []}, "hiddenVertices": ["b"], "hiddenEdges": ["y"]}),
    ];
    for filter in filters {
        check_equivalent(&mut s, &filter);
    }
}

#[test]
fn invalid_new_fields_are_errors() {
    let s = modern();
    let cases = [
        (json!({"otherVertices": "maybe"}), "otherVertices"),
        (json!({"otherEdges": true}), "otherEdges"),
        (json!({"hiddenVertices": "person"}), "hiddenVertices"),
        (json!({"hiddenEdges": [1]}), "hiddenEdges"),
    ];
    for (filter, needle) in cases {
        let err = s
            .graph_view(&json!({"filter": filter}).to_string())
            .unwrap_err();
        assert!(err.to_string().contains(needle), "{filter}: {err}");
        assert!(s.graph_filter_query(&filter.to_string()).is_err());
    }
}

#[test]
fn query_text_uses_the_text_predicates_and_not_for_hidden_labels() {
    let s = modern();
    let text = s
        .graph_filter_query(
            &json!({"vertices": {"person": [
                {"key": "name", "op": "contains", "value": "ar"},
                {"key": "name", "op": "starts_with", "value": "m"},
                {"key": "name", "op": "ends_with", "value": "o"},
                {"key": "name", "op": "regex", "value": "^m"},
            ]}})
            .to_string(),
        )
        .unwrap();
    assert!(
        text.starts_with(
            "#{\n  vertices: g.v().has_label(\"person\").has(\"name\", TextP.containing(\"ar\"))\
             .has(\"name\", TextP.starting_with(\"m\")).has(\"name\", TextP.ending_with(\"o\"))\
             .has(\"name\", TextP.regex(\"^m\")).to_list(),"
        ),
        "{text}"
    );
    let text = s
        .graph_filter_query(
            &json!({"otherVertices": "visible", "hiddenVertices": ["software"],
                "vertices": {"person": [{"key": "age", "op": "gt", "value": 30}]}})
            .to_string(),
        )
        .unwrap();
    assert!(
        text.starts_with(
            "#{\n  vertices: g.v().not(__.has_label(\"software\"))\
             .or(__.not(__.has_label(\"person\")), __.has(\"age\", P.gt(30))).to_list(),"
        ),
        "{text}"
    );
}

#[test]
fn a_filtered_neighbourhood_reports_what_it_held_before() {
    let s = modern();
    let request = json!({
        "ids": ["1"], "hops": 1,
        "filter": {"otherVertices": "visible", "hiddenVertices": ["software"]},
    });
    let view = s.neighbourhood(&request.to_string()).unwrap();
    assert_eq!(strings(&view["vertexIds"]), set(&["1", "2", "4"]));
    assert_eq!(view["unfiltered"]["nodes"], 4);
    assert_eq!(view["unfiltered"]["edges"], 4);
    assert_eq!(
        strings(&view["unfiltered"]["vertexIds"]),
        set(&["1", "2", "3", "4"])
    );
    // Edges leading out of the filtered drawing count only what the filter would show: josh's
    // created edges end at hidden software (unfiltered: josh->ripple and peter->lop).
    assert_eq!(view["hiddenEdges"], 0, "{view}");
    let plain = s.neighbourhood(r#"{"ids": ["1"], "hops": 1}"#).unwrap();
    assert!(plain.get("unfiltered").is_none());
    assert_eq!(plain["hiddenEdges"], 2);
    let request = json!({"ids": ["1"], "hops": 1,
        "filter": {"hiddenEdges": ["knows"], "otherEdges": "visible"}});
    let view = s.neighbourhood(&request.to_string()).unwrap();
    assert_eq!(
        view["hiddenEdges"], 2,
        "josh->ripple and peter->lop are created edges"
    );
    // Redrawing the unfiltered set (`keep`, `hops: 0`) under another filter brings back what the
    // first one hid: every vertex drilled into, not only the centre's neighbours.
    let grown = s
        .neighbourhood(r#"{"ids": ["4"], "hops": 1, "keep": ["1", "2", "3", "4"]}"#)
        .unwrap();
    let all = strings(&grown["vertexIds"]);
    assert!(all.contains("5"));
    let filtered = s
        .neighbourhood(
            &json!({"ids": ["4"], "hops": 1, "keep": ["1", "2", "3", "4"],
                "filter": {"vertices": {"person": []}}})
            .to_string(),
        )
        .unwrap();
    let kept = filtered["unfiltered"]["vertexIds"].clone();
    let again = s
        .neighbourhood(
            &json!({"ids": [], "keep": kept, "hops": 0,
                "filter": {"vertices": {"software": []}}})
            .to_string(),
        )
        .unwrap();
    assert_eq!(strings(&again["vertexIds"]), set(&["3", "5"]));
    assert_eq!(strings(&again["unfiltered"]["vertexIds"]), all);
}

#[test]
fn a_foreign_storage_draws_the_new_form_the_same() {
    use graphersal::prelude::*;
    use graphersal_session::views::{GraphViewRequest, graph_filter_query, graph_view_request};
    use graphersal_storage_tests::forwarding::Forwarding;
    let tg = TraversalGraph::tinkerpop_modern();
    let fw = Forwarding::new(TraversalGraph::tinkerpop_modern());
    let filter = json!({"otherVertices": "visible", "hiddenVertices": ["software"],
        "vertices": {"person": [{"key": "age", "op": "gt", "value": 28}]},
        "otherEdges": "visible", "hiddenEdges": ["created"]});
    let request = GraphViewRequest {
        max_nodes: Some(3),
        filter: Some(filter.clone()),
        ..GraphViewRequest::default()
    };
    let a = serde_json::to_value(graph_view_request(&tg, &request).unwrap()).unwrap();
    let b = serde_json::to_value(graph_view_request(&fw, &request).unwrap()).unwrap();
    assert_eq!(a, b);
    assert_eq!(a["matchedNodes"], 3);
    assert_eq!(
        graph_filter_query(&tg, &filter).unwrap(),
        graph_filter_query(&fw, &filter).unwrap()
    );
}
