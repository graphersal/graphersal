//! Layers in the session (`graphersal_session::layer`): `info.layer`, the origin columns of the
//! drawing and the inspector's `origin`, `layer_diff(spec)`, and `start_layer` / `drop_layer`;
//! each on a plain graph (no change, nothing paid) and on a layer (the dev server's `--layer`
//! hands the session a `Graph<LayeredGraph>`; the browser playground starts one itself).

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::LayerBase;
use graphersal_session::{RunOptions, Session};
use serde_json::{Value, json};

/// A session on a plain `modern` graph.
fn plain() -> Session {
    let mut session = Session::new();
    session.load_sample("modern").unwrap();
    session
}

/// A session on an empty layer over `modern`, as the dev server's `--layer` serves it.
fn layered() -> Session {
    let base = LayerBase::from_graph(Graph::new(TraversalGraph::tinkerpop_modern()))
        .unwrap()
        .with_name("modern");
    let mut session = Session::new();
    session
        .load_shared(Arc::new(base.new_layer().unwrap()), "modern", "layer")
        .unwrap();
    session
}

fn run(session: &mut Session, script: &str) -> Value {
    let answer: Value =
        serde_json::from_str(&session.execute(script, &RunOptions::default())).unwrap();
    assert_eq!(answer["ok"], true, "{script}: {answer}");
    answer
}

/// One element of each kind: vertex "ada" added, vertex 1 modified, vertex 6 removed (with
/// its edge 5), edge 13 added between base vertices 2 and 4.
fn change(session: &mut Session) {
    run(
        session,
        r#"g.add_v("person").property(T.id, "ada").property("name", "ada").iterate();
           g.v(1).property("age", 30).iterate();
           g.v(6).drop().iterate();
           g.add_e("knows").property(T.id, "13").from("2").to("4").iterate();"#,
    );
}

fn view(session: &Session) -> Value {
    session
        .graph_view(r#"{"maxNodes": 100, "maxEdges": 100}"#)
        .unwrap()
}

/// The origin column `key` of a drawing, by the id in `ids`.
fn origins(view: &Value, ids: &str, key: &str) -> Vec<(String, Value)> {
    let ids = view[ids].as_array().unwrap();
    let origins = view[key].as_array().unwrap();
    assert_eq!(ids.len(), origins.len(), "{view}");
    ids.iter()
        .map(|id| id.as_str().unwrap().to_string())
        .zip(origins.iter().cloned())
        .collect()
}

fn origin_of(view: &Value, ids: &str, key: &str, id: &str) -> Value {
    origins(view, ids, key)
        .into_iter()
        .find(|(each, _)| each == id)
        .map(|(_, origin)| origin)
        .unwrap_or_else(|| panic!("{id} not drawn: {view}"))
}

fn element(session: &Session, kind: &str, id: &str) -> Value {
    serde_json::from_str(&session.element(kind, id).unwrap()).unwrap()
}

#[test]
fn a_plain_graph_has_no_layer_block_and_no_origins() {
    let session = plain();
    let info = session.info().unwrap();
    assert!(info.get("layer").is_none(), "{info}");
    assert_eq!(session.layer_info().unwrap(), None);
    let drawn = view(&session);
    assert!(drawn.get("vertexOrigin").is_none(), "{drawn}");
    assert!(drawn.get("edgeOrigin").is_none(), "{drawn}");
    let near = session.neighbourhood(r#"{"ids": ["1"]}"#).unwrap();
    assert!(near.get("vertexOrigin").is_none(), "{near}");
    for (kind, id) in [("vertex", "1"), ("edge", "0")] {
        let inspected = element(&session, kind, id);
        assert!(inspected.get("origin").is_none(), "{inspected}");
    }
    let answer: Value = serde_json::from_str(
        &plain().execute(r#"g.v().has_label("person")"#, &RunOptions::default()),
    )
    .unwrap();
    assert!(answer["subgraph"]["vertexIds"].is_array(), "{answer}");
    assert!(answer["subgraph"].get("vertexOrigin").is_none(), "{answer}");
}

#[test]
fn a_plain_graph_answers_an_empty_layer_diff() {
    let session = plain();
    for spec in [Value::Null, json!({}), json!({"kinds": "edge", "limit": 3})] {
        let answer = session.layer_diff(&spec);
        assert_eq!(
            answer,
            json!({"ok": true, "diff": {"added": [], "removed": [], "modified": [], "truncated": false}}),
            "{spec}"
        );
    }
}

#[test]
fn a_layer_diff_spec_is_checked() {
    let session = plain();
    for (spec, needle) in [
        (json!([]), "the layer diff spec is a JSON object"),
        (json!({"kind": "vertex"}), "unknown key 'kind'"),
        (json!({"kinds": "node"}), "kinds must be"),
        (json!({"kinds": []}), "kinds must be"),
        (json!({"kinds": 1}), "kinds must be"),
        (
            json!({"labels": [1]}),
            "labels must be a label or a list of labels",
        ),
        (json!({"limit": 0}), "limit must be a positive integer"),
        (json!({"limit": "3"}), "limit must be a positive integer"),
    ] {
        let answer = session.layer_diff(&spec);
        assert_eq!(answer["ok"], false, "{spec}: {answer}");
        let error = answer["error"].as_str().unwrap();
        assert!(error.contains(needle), "{spec}: {error}");
    }
    for spec in [
        json!({"kinds": null, "labels": null, "limit": null}),
        json!({"kinds": ["vertex", "edge"], "labels": "person", "limit": 1}),
    ] {
        assert_eq!(session.layer_diff(&spec)["ok"], true, "{spec}");
    }
}

#[test]
fn a_layer_shows_its_base_counts_and_memory_in_the_info() {
    let mut session = layered();
    let layer = session.info().unwrap()["layer"].clone();
    assert_eq!(layer["base"], "modern", "{layer}");
    let base_ref = &layer["baseRef"];
    assert_eq!(base_ref["graphId"].as_str().unwrap().len(), 36, "{layer}");
    assert!(base_ref["commitSeq"].is_u64(), "{layer}");
    assert!(base_ref["committedAt"].is_i64(), "{layer}");
    for key in ["added", "modified", "removed"] {
        assert_eq!(layer[key], json!({"vertices": 0, "edges": 0}), "{layer}");
    }
    let shared = layer["sharedBytes"].as_u64().unwrap();
    assert!(shared > 0, "{layer}");
    assert!(layer["ownBytes"].as_u64().unwrap() > 0, "{layer}");

    change(&mut session);
    let layer = session.info().unwrap()["layer"].clone();
    assert_eq!(
        layer["added"],
        json!({"vertices": 1, "edges": 1}),
        "{layer}"
    );
    assert_eq!(
        layer["modified"],
        json!({"vertices": 1, "edges": 0}),
        "{layer}"
    );
    assert_eq!(
        layer["removed"],
        json!({"vertices": 1, "edges": 1}),
        "{layer}"
    );
    assert_eq!(layer["sharedBytes"].as_u64().unwrap(), shared, "{layer}");
    // The memory figures are the graph's own `memory` and its `shared_bytes`.
    let statistics = session.statistics().unwrap();
    assert_eq!(
        statistics["memory"]["shared_bytes"].as_u64(),
        Some(shared),
        "{statistics}"
    );
}

#[test]
fn a_layer_draws_and_inspects_each_elements_origin() {
    let mut session = layered();
    let drawn = view(&session);
    assert!(
        origins(&drawn, "vertexIds", "vertexOrigin")
            .iter()
            .all(|(_, origin)| origin == "base"),
        "{drawn}"
    );
    change(&mut session);
    let drawn = view(&session);
    assert_eq!(
        origin_of(&drawn, "vertexIds", "vertexOrigin", "ada"),
        "added"
    );
    assert_eq!(
        origin_of(&drawn, "vertexIds", "vertexOrigin", "1"),
        "modified"
    );
    assert_eq!(origin_of(&drawn, "vertexIds", "vertexOrigin", "2"), "base");
    assert_eq!(origin_of(&drawn, "edgeIds", "edgeOrigin", "13"), "added");
    assert_eq!(origin_of(&drawn, "edgeIds", "edgeOrigin", "0"), "base");
    assert!(
        !origins(&drawn, "vertexIds", "vertexOrigin")
            .iter()
            .any(|(id, _)| id == "6"),
        "a removed vertex is not drawn: {drawn}"
    );

    let near = session.neighbourhood(r#"{"ids": ["2"]}"#).unwrap();
    assert_eq!(
        origin_of(&near, "vertexIds", "vertexOrigin", "1"),
        "modified"
    );
    assert_eq!(origin_of(&near, "edgeIds", "edgeOrigin", "13"), "added");
    let sample = session
        .label_sample(r#"{"vertices": {"labels": ["person"]}, "maxNodes": 10}"#)
        .unwrap();
    assert_eq!(
        origin_of(&sample, "vertexIds", "vertexOrigin", "ada"),
        "added"
    );

    let answer = run(&mut session, r#"g.v("1", "ada", "3")"#);
    let subgraph = &answer["subgraph"];
    assert_eq!(
        origin_of(subgraph, "vertexIds", "vertexOrigin", "ada"),
        "added"
    );
    assert_eq!(
        origin_of(subgraph, "vertexIds", "vertexOrigin", "3"),
        "base"
    );
    assert!(answer["graph"]["layer"].is_object(), "{answer}");

    assert_eq!(element(&session, "vertex", "1")["origin"], "modified");
    assert_eq!(element(&session, "vertex", "ada")["origin"], "added");
    assert_eq!(element(&session, "vertex", "3")["origin"], "base");
    assert_eq!(element(&session, "edge", "13")["origin"], "added");
    assert_eq!(element(&session, "edge", "1")["origin"], "base");
}

#[test]
fn a_layer_diff_lists_what_the_layer_changed() {
    let mut session = layered();
    change(&mut session);
    let answer = session.layer_diff(&Value::Null);
    assert_eq!(answer["ok"], true, "{answer}");
    let diff = &answer["diff"];
    let ids = |list: &str| -> Vec<String> {
        diff[list]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids("added"), ["ada", "13"], "{diff}");
    assert_eq!(ids("removed"), ["6", "5"], "{diff}");
    assert_eq!(ids("modified"), ["1"], "{diff}");
    assert_eq!(diff["truncated"], false);
    assert_eq!(
        diff["modified"][0]["properties"]["age"],
        json!({"base": 29, "layer": 30}),
        "{diff}"
    );
    assert_eq!(diff["removed"][1]["out"], "6", "{diff}");
    assert_eq!(diff["added"][1]["in"], "4", "{diff}");

    // The info's counts are the lengths of the unlimited diff's lists.
    let layer = session.info().unwrap()["layer"].clone();
    let total = |key: &str| {
        layer[key]["vertices"].as_u64().unwrap() + layer[key]["edges"].as_u64().unwrap()
    };
    assert_eq!(total("added"), 2);
    assert_eq!(total("removed"), 2);
    assert_eq!(total("modified"), 1);

    let vertices = session.layer_diff(&json!({"kinds": "vertex"}));
    assert_eq!(vertices["diff"]["added"].as_array().unwrap().len(), 1);
    let software = session.layer_diff(&json!({"labels": ["software"]}));
    assert_eq!(software["diff"]["removed"], json!([]), "{software}");
    let first = session.layer_diff(&json!({"limit": 1}));
    assert_eq!(first["diff"]["added"].as_array().unwrap().len(), 1);
    assert_eq!(first["diff"]["truncated"], true, "{first}");
}

#[test]
fn start_layer_freezes_the_graph_and_drop_layer_goes_back() {
    for memory_stores in [false, true] {
        let mut session = plain();
        session.set_memory_stores(memory_stores);
        assert_eq!(session.has_store(), memory_stores);
        let started = session.start_layer();
        assert_eq!(started["ok"], true, "{started}");
        assert_eq!(
            started["info"]["layer"]["base"], "Modern (TinkerPop)",
            "{started}"
        );
        assert_eq!(started["info"]["vertexCount"], 6, "{started}");
        assert!(!session.has_store(), "a layer is not kept as a store");
        change(&mut session);
        assert_eq!(session.info().unwrap()["vertexCount"], 6);
        assert_eq!(
            session.layer_diff(&Value::Null)["diff"]["added"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let again = session.start_layer();
        assert_eq!(again["ok"], false);
        assert!(
            again["error"].as_str().unwrap().contains("already a layer"),
            "{again}"
        );

        let dropped = session.drop_layer();
        assert_eq!(dropped["ok"], true, "{dropped}");
        assert!(dropped["info"].get("layer").is_none(), "{dropped}");
        assert_eq!(session.has_store(), memory_stores);
        // The base is back as it was: the layer's changes are gone.
        let ages = run(&mut session, r#"g.v(1).values("age")"#);
        assert_eq!(ages["execution"]["results"], json!([29]), "{ages}");
        assert_eq!(session.info().unwrap()["vertexCount"], 6);
        // And it is writable again.
        run(&mut session, r#"g.add_v("person").iterate()"#);
        assert_eq!(session.info().unwrap()["vertexCount"], 7);
        let none = session.drop_layer();
        assert_eq!(none["ok"], false);
        assert!(
            none["error"].as_str().unwrap().contains("no layer to drop"),
            "{none}"
        );
    }
}

#[test]
fn a_layer_the_host_gave_is_not_dropped_or_layered_again() {
    let mut session = layered();
    let dropped = session.drop_layer();
    assert_eq!(dropped["ok"], false, "{dropped}");
    assert!(session.info().unwrap()["layer"].is_object());
    assert_eq!(session.start_layer()["ok"], false);
}

#[test]
fn start_layer_refuses_a_graph_shared_with_another_owner() {
    let mut session = plain();
    let other = session.graph().clone();
    let refused = session.start_layer();
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("shared with another owner"),
        "{refused}"
    );
    assert!(session.info().unwrap().get("layer").is_none());
    assert!(Arc::ptr_eq(session.graph(), &other));
    drop(other);
    assert_eq!(session.start_layer()["ok"], true);
    // Loading another graph forgets the layer's base.
    session.load_sample("modern").unwrap();
    assert!(session.info().unwrap().get("layer").is_none());
    assert_eq!(session.drop_layer()["ok"], false);
}

#[test]
fn a_layer_cannot_drop_while_another_owner_holds_it() {
    let mut session = plain();
    assert_eq!(session.start_layer()["ok"], true);
    let other = session.graph().clone();
    let refused = session.drop_layer();
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(session.info().unwrap()["layer"].is_object());
    drop(other);
    assert_eq!(session.drop_layer()["ok"], true);
}
