//! Scheduled jobs of users as catalog definitions (kind `job`, book page "Scheduled Jobs"): the
//! DSL `g.define_job` / `g.jobs()` / `g.drop_job`, the definer's rights checked and stored as
//! the job's effects, and the definition surviving every store path; a newer version's job is
//! kept byte for byte and never run.
#![cfg(all(feature = "persist", feature = "script"))]

use std::sync::Arc;

use graphersal::auth::{AccessPolicy, ActionKind, ResourceKind};
use graphersal::catalog::{Definition, DefinitionKind, JobDefinition, JobTask};
use graphersal::persist::{MemDir, Store, StoreOptions};
use graphersal::prelude::*;
use graphersal::script::rhai::{Array, Dynamic, Map};
use graphersal::script::{self, ScriptLimits};

use crate::script_host::eval_value;

/// A query that writes (drops the vertices labelled `label`) and one that only reads.
const QUERIES: &str = r#"
    g.define_query(#{name: "purge", params: #{label: "string"},
        body: "fn purge(label) { g.v().has_label(label).drop() }"});
    g.define_query(#{name: "names", body: "fn names() { g.v().values(\"name\") }"});
"#;

fn modern_with_queries() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = eval_value(graph.clone(), QUERIES).unwrap();
    graph
}

fn jobs(graph: &Arc<Graph>) -> Array {
    eval_value(graph.clone(), "g.jobs()")
        .unwrap()
        .into_array()
        .unwrap()
}

fn field(job: &Dynamic, key: &str) -> Dynamic {
    job.clone_cast::<Map>()
        .get(key)
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_query_job_stores_the_effects_its_definer_was_allowed() {
    let graph = modern_with_queries();
    let _ = eval_value(
        graph.clone(),
        r#"g.define_job(#{name: "nightly_purge", schedule: "0 3 * * *", action: "query",
            query: "purge", args: #{label: "software"}, timeout_ms: 30000,
            description: "drops the software"})"#,
    )
    .unwrap();
    let listed = jobs(&graph);
    assert_eq!(listed.len(), 1);
    let job = &listed[0];
    assert_eq!(field(job, "name").to_string(), "nightly_purge");
    assert_eq!(field(job, "action").to_string(), "query");
    assert_eq!(field(job, "query").to_string(), "purge");
    assert_eq!(field(job, "timeout_ms").as_int().unwrap(), 30000);
    assert!(field(job, "served").as_bool().unwrap());
    let effects: Vec<String> = field(job, "effects")
        .into_array()
        .unwrap()
        .iter()
        .map(|e| {
            let e = e.clone_cast::<Map>();
            format!("{} {}", e["action"], e["resource"])
        })
        .collect();
    // `Execute` on the query, then the query's data write.
    assert_eq!(
        effects,
        ["Execute definition", "Delete data"],
        "{effects:?}"
    );
    // A reading query stores only the `Execute`.
    let _ = eval_value(
        graph.clone(),
        r#"g.define_job(#{name: "list", schedule: "*/5 * * * *", action: "query", query: "names"})"#,
    )
    .unwrap();
    let listed = jobs(&graph);
    let list = listed
        .iter()
        .find(|j| field(j, "name").to_string() == "list")
        .unwrap();
    assert_eq!(field(list, "effects").into_array().unwrap().len(), 1);
    // Defining a job changes no data and runs nothing.
    assert_eq!(graph.read().vertex_count(), 6);
    let drop = || {
        eval_value(graph.clone(), r#"g.drop_job("list")"#)
            .unwrap()
            .as_bool()
            .unwrap()
    };
    assert!(drop());
    assert!(!drop());
}

#[test]
fn a_wrong_job_is_refused_with_the_field_named() {
    let graph = modern_with_queries();
    let refused = |spec: &str| {
        eval_value(graph.clone(), &format!("g.define_job(#{{{spec}}})"))
            .expect_err(spec)
            .to_string()
    };
    let err = refused(r#"name: "j", schedule: "61 * * * *", action: "checkpoint""#);
    assert!(err.contains("schedule"), "{err}");
    let err = refused(r#"name: "j", schedule: "* * * * *", action: "query", query: "nowhere""#);
    assert!(err.contains("nowhere"), "{err}");
    let err = refused(
        r#"name: "j", schedule: "* * * * *", action: "query", query: "purge", args: #{lbl: "x"}"#,
    );
    assert!(err.contains("lbl"), "{err}");
    let err = refused(
        r#"name: "j", schedule: "* * * * *", action: "query", query: "purge", args: #{label: 1}"#,
    );
    assert!(err.contains("label"), "{err}");
    let err = refused(r#"name: "j", schedule: "* * * * *", action: "compact""#);
    assert!(err.contains("checkpoint"), "{err}");
    let err = refused(r#"name: "j", schedule: "* * * * *", action: "backup""#);
    assert!(err.contains("path"), "{err}");
    let err = refused(r#"name: "j", schedule: "* * * * *", action: "checkpoint", every: 1"#);
    assert!(err.contains("every"), "{err}");
    assert!(jobs(&graph).is_empty());
}

#[test]
fn the_definers_rights_are_checked_when_the_job_is_defined() {
    let graph = modern_with_queries();
    let run = |script: &str, policy: AccessPolicy| {
        script::eval_value_with_limits(
            graph.clone(),
            script,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(policy),
        )
        .map_err(|err| err.to_string())
    };
    // The query deletes data: a definer who may not delete data cannot schedule it.
    let err = run(
        r#"g.define_job(#{name: "j", schedule: "0 3 * * *", action: "query", query: "purge"})"#,
        AccessPolicy::allow_all().deny(ActionKind::Delete, ResourceKind::Data),
    )
    .unwrap_err();
    assert!(
        err.contains("Permission denied for 'define_job' (Delete Data"),
        "{err}"
    );
    // A backup writes files.
    let err = run(
        r#"g.define_job(#{name: "b", schedule: "0 4 * * *", action: "backup", path: "/tmp/b"})"#,
        AccessPolicy::allow_all().deny_resource(ResourceKind::File),
    )
    .unwrap_err();
    assert!(
        err.contains("Permission denied for 'define_job' (Create File("),
        "{err}"
    );
    // Defining a job at all needs `Create` on the job.
    let err = run(
        r#"g.define_job(#{name: "c", schedule: "0 4 * * *", action: "checkpoint"})"#,
        AccessPolicy::allow_all().deny_resource(ResourceKind::Definition),
    )
    .unwrap_err();
    assert!(err.contains("Permission denied for 'define_job'"), "{err}");
    assert!(jobs(&graph).is_empty());
}

#[test]
fn a_job_survives_every_store_path() {
    let dir = MemDir::named("memory:jobs-definitions");
    let store = Store::create_with(
        &dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .unwrap();
    let _ = eval_value(store.graph().clone(), QUERIES).unwrap();
    let _ = eval_value(
        store.graph().clone(),
        r#"g.define_job(#{name: "nightly", schedule: "0 3 * * *", action: "query",
            query: "purge", args: #{label: "software"}, enabled: false})"#,
    )
    .unwrap();
    let expected = store
        .graph()
        .read()
        .definition(DefinitionKind::JOB, "nightly")
        .unwrap()
        .cloned()
        .unwrap();
    let job_of = |graph: &Arc<Graph>| {
        graph
            .read()
            .definition(DefinitionKind::JOB, "nightly")
            .unwrap()
            .cloned()
    };
    // The journal (no checkpoint yet): reopen replays it.
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(job_of(store.graph()).as_ref(), Some(&expected));
    // A checkpoint, then reopen from the snapshot.
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(job_of(store.graph()).as_ref(), Some(&expected));
    // A backup carries it.
    let backup = MemDir::named("memory:jobs-definitions-backup");
    store.backup(&backup).unwrap();
    let opened = Store::open_backup(&backup, StoreOptions::new()).unwrap();
    assert_eq!(job_of(opened.graph()).as_ref(), Some(&expected));
    opened.close().unwrap();
    // The catalog file carries it too.
    let text = graphersal::catalog::catalog_to_json(store.graph().read().definitions().unwrap());
    let back = graphersal::catalog::catalog_from_json(&text).unwrap();
    assert!(back.iter().any(|d| d == &expected), "{text}");
    store.close().unwrap();
}

#[test]
fn a_job_of_a_newer_version_is_kept_byte_for_byte_and_not_run() {
    let dir = MemDir::named("memory:jobs-newer");
    let store = Store::create_with(
        &dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .unwrap();
    // A newer version's action and an unknown key.
    let mut payload = JobDefinition::new("0 3 * * *", JobTask::Checkpoint).to_payload();
    payload.insert("action", "compact_vectors".into());
    payload.insert("tier", "cold".into());
    let job = JobDefinition::from_payload(&payload).unwrap();
    assert!(job.unserved_reason().is_some());
    store
        .graph()
        .write()
        .set_definition(Definition::job("future", job))
        .unwrap();
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let kept = store
        .graph()
        .read()
        .definition(DefinitionKind::JOB, "future")
        .unwrap()
        .and_then(|d| d.as_job().cloned())
        .unwrap();
    assert_eq!(kept.to_payload(), payload);
    let listed = jobs(store.graph());
    assert!(!field(&listed[0], "served").as_bool().unwrap());
    assert!(
        field(&listed[0], "reason")
            .to_string()
            .contains("compact_vectors")
    );
    store.close().unwrap();
}
