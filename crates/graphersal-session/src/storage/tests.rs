//! The session over two storages: [`TraversalGraph`] ([`Session::new`]) and the `Forwarding`
//! proof storage (not `TraversalGraph`, own handle types). Every operation must answer the
//! same JSON on both; the differences a foreign storage may show (compression without
//! compressed properties, no persistence) are tested on their own.

use std::sync::Arc;

use graphersal::persist::{MemDir, StoreDir};
use graphersal::storage::{AnyGraph, Graph, TraversalGraph};
use graphersal_storage_tests::forwarding::Forwarding;
use serde_json::{Value, json};

use super::no_compression::NoCompression;
use super::{GraphFactory, StorageKind};
use crate::{ForkInto, RunOptions, Session};

/// A session over `Forwarding` with stores.
fn forwarding() -> Session {
    Session::with_storage(Arc::new(StorageKind::<Forwarding>::persistent()))
}

/// Both sessions, the modern graph loaded (`stores`: every loaded graph a store in memory).
fn both(stores: bool) -> [Session; 2] {
    [Session::new(), forwarding()].map(|mut s| {
        s.set_memory_stores(stores);
        s.load_sample("modern").expect("modern");
        s
    })
}

/// The answer of a run, without what depends on time.
fn run(session: &mut Session, script: &str) -> Value {
    let mut answer: Value =
        serde_json::from_str(&session.execute(script, &RunOptions::default())).unwrap();
    answer["elapsed_ms"] = Value::Null;
    answer
}

/// The same operation on both sessions answers the same JSON.
fn same(sessions: &mut [Session; 2], op: impl Fn(&mut Session) -> Value) -> Value {
    let [a, b] = sessions;
    let (left, right) = (op(a), op(b));
    assert_eq!(left, right);
    left
}

#[test]
fn the_storage_is_the_factorys() {
    let [tg, fw] = both(false);
    assert!(tg.graph().storage_type_name().ends_with("TraversalGraph"));
    assert!(fw.graph().storage_type_name().ends_with("Forwarding"));
    assert!(fw.factory().storage_name().ends_with("Forwarding"));
    let any: Arc<dyn AnyGraph> = fw.graph().clone();
    assert!(any.as_any().downcast_ref::<Graph<Forwarding>>().is_some());
}

#[test]
fn execute_commits_and_rolls_back_alike() {
    let mut s = both(false);
    let added = same(&mut s, |s| {
        run(
            s,
            r#"g.add_v("person").property("name", "ann").values("name").to_list()"#,
        )
    });
    assert_eq!(added["ok"], true, "{added}");
    assert_eq!(added["mutated"], true);
    assert_eq!(added["graph"]["vertexCount"], 7);
    // A failing script after a write: the whole run is rolled back.
    let failed = same(&mut s, |s| {
        run(
            s,
            r#"g.add_v("person").next(); g.v().fail("stop").to_list()"#,
        )
    });
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["mutated"], false);
    assert_eq!(failed["graph"]["vertexCount"], 7);
    // Implicit execution, a subgraph, a profile.
    let traversal = same(&mut s, |s| {
        run(s, r#"g.v().has_label("person").out_e("knows")"#)
    });
    assert_eq!(traversal["implicit"], true);
    assert_eq!(
        traversal["subgraph"]["edgeIds"].as_array().unwrap().len(),
        2
    );
    same(&mut s, |s| run(s, "g.v().count()"));
}

#[test]
fn views_and_selections_are_equal() {
    let mut s = both(false);
    same(&mut s, |s| s.graph().schema_view().unwrap());
    same(&mut s, |s| {
        serde_json::to_value(s.graph().graph_view(100, 100).unwrap()).unwrap()
    });
    same(&mut s, |s| {
        let mut statistics = s.statistics().unwrap();
        // The footprint is the storage's own business.
        statistics["memory"] = Value::Null;
        statistics
    });
    same(&mut s, |s| json!(s.element("vertex", "1").unwrap()));
    same(&mut s, |s| json!(s.element("edge", "9").unwrap()));
    same(&mut s, |s| {
        s.neighbourhood(r#"{"ids": ["1"], "hops": 2}"#).unwrap()
    });
    same(&mut s, |s| {
        s.label_sample(r#"{"vertices": {"labels": ["person"]}}"#)
            .unwrap()
    });
    same(&mut s, |s| s.meta_graph().unwrap());
    same(&mut s, |s| s.infer_schema().unwrap());
    same(&mut s, |s| s.info().unwrap());
}

#[test]
fn edits_and_creations_preview_and_commit_alike() {
    let mut s = both(false);
    same(&mut s, |s| s.element_for_edit("vertex", "1").unwrap());
    let preview = json!({"kind": "vertex", "id": "1", "validateOnly": true,
        "set": [["age", {"type": "int64", "value": "30"}]]});
    let previewed = same(&mut s, |s| s.update_element(&preview));
    assert_eq!(previewed["changed"], false, "{previewed}");
    let update = json!({"kind": "vertex", "id": "1", "remove": ["age"],
        "set": [["nick", {"type": "string", "value": "m"}]]});
    let updated = same(&mut s, |s| s.update_element(&update));
    assert_eq!(updated["changed"], true, "{updated}");
    let bad = json!({"kind": "vertex", "id": "1", "set": [["x", {"type": "int64", "value": "x"}]]});
    assert_eq!(same(&mut s, |s| s.update_element(&bad))["ok"], false);

    same(&mut s, |s| {
        s.label_choices(&json!({"kind": "edge", "from": "1", "to": "2"}))
            .unwrap()
    });
    let vertex = json!({"kind": "vertex", "labels": ["person"], "id": "v9",
        "properties": [["name", {"type": "string", "value": "zoe"}]]});
    let mut dry = vertex.clone();
    dry["validateOnly"] = json!(true);
    assert_eq!(same(&mut s, |s| s.create_element(&dry))["created"], false);
    let created = same(&mut s, |s| s.create_element(&vertex));
    assert_eq!(created["created"], true, "{created}");
    assert_eq!(created["drawn"]["caption"], "zoe");
    let edge = json!({"kind": "edge", "label": "knows", "from": "v9", "to": "1",
        "properties": []});
    assert_eq!(same(&mut s, |s| s.create_element(&edge))["created"], true);
    let delete = json!({"kind": "vertex", "id": "v9", "validateOnly": true});
    assert_eq!(same(&mut s, |s| s.delete_element(&delete))["edges"], 1);
    let delete = json!({"kind": "vertex", "id": "v9"});
    assert_eq!(same(&mut s, |s| s.delete_element(&delete))["deleted"], true);
    same(&mut s, |s| s.info().unwrap());
}

#[test]
fn saved_queries_and_the_catalog_file_are_equal() {
    let mut s = both(false);
    let spec = json!({
        "name": "older_than", "folder": "people",
        "params": [{"name": "age", "schema": {"type": "integer"}, "default": 30}],
        "body": "fn older_than(age) { g.v().has(\"age\", P.gt(age)).values(\"name\") }",
    });
    assert_eq!(same(&mut s, |s| s.define_query(&spec))["ok"], true);
    same(&mut s, |s| s.list_queries(None).unwrap());
    let ran = same(&mut s, |s| {
        run(s, r#"g.query("older_than", #{age: 31}).to_list()"#)
    });
    assert_eq!(
        ran["execution"]["results"],
        json!(["josh", "peter"]),
        "{ran}"
    );
    let text = same(&mut s, |s| json!(s.save_catalog_text().unwrap()));
    let text = text.as_str().unwrap().to_string();
    assert!(
        same(&mut s, |s| json!(s.drop_query("older_than").unwrap()))
            .as_bool()
            .unwrap()
    );
    let loaded = same(&mut s, |s| s.load_catalog(&text, false).unwrap());
    assert_eq!(loaded["loaded"], 1);
    assert_eq!(
        same(&mut s, |s| s.get_query("older_than").unwrap())["folder"],
        "people"
    );
    // A GraphML load keeps the catalog, on both storages.
    let saved = same(&mut s, |s| s.save_graphml().unwrap());
    let graphml = saved["text"].as_str().unwrap().to_string();
    let info = same(&mut s, |s| {
        s.load_graphml(graphml.as_bytes(), "saved", None).unwrap()
    });
    assert_eq!(info["savedQueries"], 1);
}

#[test]
fn compression_rules_work_and_a_storage_without_compression_says_so() {
    let cv = "Senior engineer, led the platform team in Rust. ".repeat(20);
    let add = format!(r#"g.add_v("customer").property("cv", "{cv}").to_list()"#);
    let rule = json!({"name": "cv", "label": "customer", "path": "cv", "minBytes": 64});

    let mut s = both(false);
    same(&mut s, |s| run(s, &add));
    let defined = same(&mut s, |s| s.define_compression(&rule));
    assert_eq!(defined["rule"]["stats"]["compressedValues"], 1, "{defined}");
    same(&mut s, |s| s.recompress("cv").unwrap());
    same(&mut s, |s| s.list_compressions().unwrap());

    // A storage that keeps rules but has no compressed properties: list, define and drop work
    // and persist (a fork of its store has the rule); the stats are `null`; recompress is the
    // storage's "not supported" error.
    let mut plain = Session::with_storage(Arc::new(StorageKind::<NoCompression>::persistent()));
    plain.set_memory_stores(true);
    plain.load_sample("modern").unwrap();
    assert_eq!(run(&mut plain, &add)["ok"], true);
    let defined = plain.define_compression(&rule);
    assert_eq!(defined["ok"], true, "{defined}");
    assert_eq!(defined["rule"]["stats"], Value::Null);
    assert_eq!(plain.list_compressions().unwrap()[0]["name"], "cv");
    let err = plain.recompress("cv").unwrap_err();
    assert!(
        err.to_string()
            .contains("does not support compressed properties"),
        "{err}"
    );
    let fork = plain
        .store_fork(&json!({"target": {"commit": 2}}), ForkInto::Memory)
        .unwrap();
    plain
        .store_switch(fork["fork"]["database"].as_str().unwrap())
        .unwrap();
    assert_eq!(plain.compression_count().unwrap(), 1);
    assert!(plain.drop_compression("cv").unwrap());
    assert_eq!(plain.compression_count().unwrap(), 0);
    // The value was never compressed: it reads back as it was written.
    let read = run(
        &mut plain,
        r#"g.v().has_label("customer").values("cv").to_list()"#,
    );
    assert_eq!(read["execution"]["results"][0], json!(cv));
}

#[test]
fn the_store_menu_works_on_a_memory_store_of_either_storage() {
    let mut s = both(true);
    for session in &mut s {
        assert_eq!(run(session, r#"g.add_v("person").next()"#)["ok"], true);
    }
    let positions = |info: &Value| {
        json!([
            info["commitSeq"],
            info["memory"],
            info["kind"],
            info["databases"].as_array().unwrap().len()
        ])
    };
    let info = same(&mut s, |s| positions(&s.store_info().unwrap()));
    assert_eq!(info, json!([1, true, "memory", 1]));
    same(&mut s, |s| {
        s.store_checkpoint("one").unwrap()["snapshot"]["commitSeq"].clone()
    });
    same(&mut s, |s| {
        s.store_mark("m").unwrap()["mark"]["commitSeq"].clone()
    });
    let backup = same(&mut s, |s| {
        let dir: Arc<dyn StoreDir> = Arc::new(MemDir::named("memory:backup"));
        let answer = s.store_backup(dir, false, false).unwrap();
        json!([
            answer["backup"]["commitSeq"],
            answer["backup"]["incremental"]
        ])
    });
    assert_eq!(backup, json!([1, false]));
    // A view of the past, refused writes, back to live.
    let view = same(&mut s, |s| {
        s.store_open_view(&json!({"target": {"commit": 0}}))
            .unwrap()["graph"]
            .clone()
    });
    assert_eq!(view["vertexCount"], 6);
    same(&mut s, |s| run(s, r#"g.add_v("x").next()"#)["ok"].clone());
    same(&mut s, |s| s.store_close_view().unwrap()["graph"].clone());
    // Fork into the database list, switch to it, write there, back.
    let fork = same(&mut s, |s| {
        let mut fork = s
            .store_fork(&json!({"target": {"commit": 0}}), ForkInto::Memory)
            .unwrap()["fork"]
            .clone();
        fork["graphId"] = Value::Null;
        fork["parentGraphId"] = Value::Null;
        fork
    });
    let id = fork["database"].as_str().unwrap().to_string();
    same(&mut s, |s| s.store_switch(&id).unwrap()["graph"].clone());
    same(&mut s, |s| {
        run(s, r#"g.add_v("software").next()"#)["graph"].clone()
    });
    same(&mut s, |s| s.store_switch("main").unwrap()["graph"].clone());
    // Rollback to the loaded state, attic restore, verify.
    let rolled = same(&mut s, |s| {
        let answer = s.store_rollback(&json!({"target": {"commit": 0}})).unwrap();
        json!([answer["graph"], answer["rollback"]["previousCommitSeq"]])
    });
    assert_eq!(rolled[0]["vertexCount"], 6);
    same(&mut s, |s| {
        let id = s.store_info().unwrap()["attic"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.store_attic_restore(&id).unwrap()["graph"].clone()
    });
    same(&mut s, |s| {
        s.store_verify().unwrap()["verify"]["ok"].clone()
    });
    same(&mut s, |s| s.store_close(&id).unwrap()["closed"].clone());
}

#[test]
fn loads_go_through_the_factory() {
    let [tg, _] = both(false);
    let graphson = tg.save_graphson().unwrap();
    let text = graphson["text"].as_str().unwrap().to_string();
    let snapshot = tg.save_snapshot().unwrap();
    let mut s = both(false);
    same(&mut s, |s| {
        s.load_graphson(text.as_bytes(), "file", None).unwrap()
    });
    same(&mut s, |s| s.load_snapshot(&snapshot, "snapshot").unwrap());
    let [_, fw] = &s;
    assert!(fw.graph().storage_type_name().ends_with("Forwarding"));
    // A snapshot of the foreign storage reads back on the default one.
    let mut back = Session::new();
    back.load_snapshot(&fw.save_snapshot().unwrap(), "back")
        .unwrap();
    assert_eq!(back.info().unwrap()["vertexCount"], 6);
}

#[test]
fn load_shared_serves_a_graph_of_any_storage() {
    let mut session = Session::new();
    let graph = Arc::new(Graph::new(Forwarding::new(
        TraversalGraph::tinkerpop_modern(),
    )));
    let info = session
        .load_shared(graph.clone(), "host graph", "host")
        .unwrap();
    assert_eq!(info["vertexCount"], 6);
    assert_eq!(run(&mut session, r#"g.add_v("person").next()"#)["ok"], true);
    // The host's own handle sees the commit.
    assert_eq!(
        graphersal::storage::GraphStorage::vertex_count(&*graph.read()),
        7
    );
}

#[test]
fn a_storage_without_persistence_serves_graphs_without_stores() {
    let factory = Arc::new(StorageKind::<Forwarding>::new());
    assert!(!factory.supports_stores());
    let mut s = Session::with_storage(factory);
    s.set_memory_stores(true);
    let info = s.load_sample("modern").unwrap();
    assert_eq!(info["vertexCount"], 6);
    let warnings = info["warnings"].to_string();
    assert!(warnings.contains("keeps no stores"), "{warnings}");
    let refused = s.store_info().unwrap_err();
    assert_eq!(refused.status, 409);
    assert!(
        refused.message.contains("does not support stores"),
        "{refused}"
    );
    // `.gsnap` is refused; everything else works.
    let snapshot = Session::new().save_snapshot().unwrap();
    let err = s.load_snapshot(&snapshot, "x").unwrap_err();
    assert!(
        err.to_string()
            .contains("does not support packed snapshots"),
        "{err}"
    );
    let [mut tg, _] = both(false);
    let ran = run(
        &mut s,
        r#"g.v().has_label("person").values("name").to_list()"#,
    );
    assert_eq!(
        ran["execution"],
        run(
            &mut tg,
            r#"g.v().has_label("person").values("name").to_list()"#
        )["execution"]
    );
}

#[test]
fn property_indexes_work_and_a_storage_without_them_says_so() {
    let spec = json!({"label": "person", "key": "age"});
    let mut s = both(false);
    let created = same(&mut s, |s| s.create_index(&spec));
    assert_eq!(created["index"]["stats"]["entries"], 4, "{created}");
    same(&mut s, |s| s.list_indexes().unwrap());
    same(&mut s, |s| {
        run(
            s,
            r#"g.v().has_label("person").has("age", P.gt(30)).values("name").to_list()"#,
        )
    });
    let text = s[0].save_catalog_text().unwrap();
    assert_eq!(text, s[1].save_catalog_text().unwrap());

    // A storage that keeps index definitions but serves none: create is its "not supported"
    // error; a catalog file brings the definition in, listed as not served.
    let mut plain = Session::with_storage(Arc::new(StorageKind::<NoCompression>::persistent()));
    plain.load_sample("modern").unwrap();
    let refused = plain.create_index(&spec);
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["errors"][0]["field"], "general");
    assert!(
        refused["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("property indexes"),
        "{refused}"
    );
    assert_eq!(plain.index_count().unwrap(), 0);
    plain.load_catalog(&text, false).unwrap();
    assert_eq!(plain.index_count().unwrap(), 1);
    let listed = plain.list_indexes().unwrap();
    assert_eq!(listed[0]["served"], false, "{listed}");
    assert_eq!(listed[0]["stats"], Value::Null);
    assert!(plain.drop_index("person_age").unwrap());
}

#[test]
fn an_estimate_is_the_same_on_both_storages_and_unsupported_without_compression() {
    let cv = "Senior engineer, led the platform team in Rust. ".repeat(20);
    let add = format!(r#"g.add_v("customer").property("cv", "{cv}").to_list()"#);
    let spec = json!({"label": "customer", "path": "cv"});
    let without_timings = |mut answer: Value| {
        answer["estimate"]["elapsedMs"] = Value::Null;
        for candidate in answer["estimate"]["candidates"].as_array_mut().unwrap() {
            candidate["encodeBytesPerSec"] = Value::Null;
            candidate["decodeBytesPerSec"] = Value::Null;
        }
        answer
    };
    let mut s = both(false);
    same(&mut s, |s| run(s, &add));
    let estimate = same(&mut s, |s| without_timings(s.estimate_compression(&spec)));
    assert_eq!(estimate["ok"], true, "{estimate}");
    assert_eq!(estimate["estimate"]["stringValues"], 1);

    let mut plain = Session::with_storage(Arc::new(StorageKind::<NoCompression>::persistent()));
    plain.load_sample("modern").unwrap();
    assert_eq!(run(&mut plain, &add)["ok"], true);
    let refused = plain.estimate_compression(&spec);
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["errors"][0]["field"], "general");
    assert!(
        refused["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("does not support compressed properties"),
        "{refused}"
    );
}

/// On the thread that holds the graph's unit beside a live write view (a commit hook of that
/// unit), every read of the session's graph is `GraphViewHeld`: formerly `0`, `null`, empty or
/// `None`. A whole session answers it as its error too.
#[test]
fn session_graph_reads_answer_a_forbidden_view_with_its_error() {
    use super::SessionGraph;
    use crate::SessionError;
    use graphersal::catalog::DefinitionKind;
    use graphersal::storage::{ElementKind, GraphError};

    fn held<T: std::fmt::Debug>(answer: Result<T, GraphError>) {
        assert!(
            matches!(answer, Err(GraphError::GraphViewHeld { .. })),
            "{answer:?}"
        );
    }
    let shared = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
    let mut session = Session::new();
    session.load_shared(shared.clone(), "held", "host").unwrap();
    let graph: &dyn SessionGraph = &*shared;
    let mark = shared.begin_unit().unwrap();
    {
        let _write = shared.write();
        held(graph.vertex_count());
        held(graph.edge_count());
        held(graph.inferred_schema());
        held(graph.schema_view());
        held(graph.statistics_view());
        held(graph.memory_view());
        held(graph.element(ElementKind::Vertex, "1"));
        held(graph.queries(None));
        held(graph.query("anything"));
        held(graph.count_kind(DefinitionKind::QUERY));
        held(graph.catalog_text());
        held(graph.compressions());
        held(graph.indexes());
        assert!(matches!(
            session.info(),
            Err(SessionError::Graph(err)) if matches!(*err, GraphError::GraphViewHeld { .. })
        ));
        assert!(session.statistics().is_err());
        assert!(session.list_queries(None).is_err());
        assert!(session.get_schema_text().is_err());
    }
    assert_eq!(graph.vertex_count().unwrap(), 6);
    assert_eq!(session.info().unwrap()["vertexCount"], 6);
    graph
        .rollback_run(mark, &graphersal::changes::RollbackReason::failed(&"test"))
        .unwrap();
}
