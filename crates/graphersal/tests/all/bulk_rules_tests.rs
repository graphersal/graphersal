//! Bulk compression rules (stage V1b; book page "Compressed Properties", section "Several rules
//! at once", and page "Optimize", section "Applying several at once"): a LIST of ordinary rules
//! applied in one unit, one commit, one walk (`GraphStorage::set_definitions`,
//! `GraphTraversalSource::set_definitions`/`define_compressions`, the DSL's
//! `g.define_compressions([..])` with its `paths` form) and several advisor recommendations
//! applied at once (`Graph::apply_recommendations`). All or nothing; reads never change.
#![cfg(feature = "script")]

use std::sync::{Arc, Mutex};

use graphersal::advisor::{AdviceRequest, RecommendedAction};
use graphersal::auth::{AccessPolicy, ActionKind, Authorizer, ResourceKind};
use graphersal::catalog::{Codec, CompressionDefinition, Definition, DefinitionKind};
use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::error::{DefinitionError, GraphError};
use graphersal::prelude::*;
use graphersal::storage::ElementKind;
use graphersal::workload::{WorkloadLevel, WorkloadStats};

use crate::script_host::*;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// `n` `file` vertices with repeated `kind` (3), `owner` (5), `size` (4) and `tags` (2 lists),
/// plus `link` edges with a repeated `rel` (2).
fn files(n: usize) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    {
        for i in 0..n {
            graph
                .traversal_mut()
                .add_v("file")
                .property("id_n", i as i64)
                .property("kind", ["doc", "img", "src"][i % 3])
                .property("owner", format!("user-{}", i % 5).as_str())
                .property("size", (i % 4) as i64)
                .to_list()
                .unwrap();
        }
    }
    let ids: Vec<String> = graph
        .vertices()
        .unwrap()
        .map(|v| graph.vertex_id(v).unwrap().unwrap().to_string())
        .collect();
    let script = ids
        .windows(2)
        .enumerate()
        .map(|(i, pair)| {
            format!(
                "g.v(\"{}\").add_e(\"link\").to(__.v(\"{}\")).property(\"rel\", \"{}\").iterate();",
                pair[0],
                pair[1],
                ["near", "far"][i % 2]
            )
        })
        .collect::<String>();
    let shared = Arc::new(Graph::new(graph));
    let _ = eval_value(shared.clone(), &script).unwrap();
    Arc::try_unwrap(shared).ok().unwrap().into_inner()
}

fn dedup(element: ElementKind, label: &str, path: &str) -> CompressionDefinition {
    CompressionDefinition::new(element, label, path)
        .unwrap()
        .with_codec(Codec::Dedup)
}

fn three_rules() -> Vec<(String, CompressionDefinition)> {
    vec![
        (
            "file_kind".to_string(),
            dedup(ElementKind::Vertex, "file", "kind"),
        ),
        (
            "file_owner".to_string(),
            dedup(ElementKind::Vertex, "file", "owner"),
        ),
        (
            "link_rel".to_string(),
            dedup(ElementKind::Edge, "link", "rel"),
        ),
    ]
}

fn invariant(graph: &TraversalGraph) {
    if let Err(reason) = graph.check_compression_invariant() {
        panic!("invariant broken: {reason}");
    }
}

/// The logical content of a graph: every vertex's and edge's properties by id.
fn content(graph: &TraversalGraph) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in graph.vertices().unwrap() {
        let props = graph.vertex_properties(v).unwrap().unwrap();
        let mut props: Vec<String> = props
            .iter()
            .map(|(k, v)| format!("{k}={:?}", v.to_logical()))
            .collect();
        props.sort();
        out.push(format!(
            "v {} {props:?}",
            graph.vertex_id(v).unwrap().unwrap()
        ));
    }
    for e in graph.edges().unwrap() {
        let props = graph.edge_properties(e).unwrap().unwrap();
        let mut props: Vec<String> = props
            .iter()
            .map(|(k, v)| format!("{k}={:?}", v.to_logical()))
            .collect();
        props.sort();
        out.push(format!(
            "e {} {props:?}",
            graph.edge_id(e).unwrap().unwrap()
        ));
    }
    out.sort();
    out
}

fn rule_names(graph: &TraversalGraph) -> Vec<String> {
    graph
        .definitions()
        .unwrap()
        .filter(|d| d.kind() == DefinitionKind::COMPRESSION)
        .map(|d| d.name().to_string())
        .collect()
}

/// The id of the first vertex.
fn first_vertex(graph: &TraversalGraph) -> String {
    let v = graph.vertices().unwrap().next().unwrap();
    graph.vertex_id(v).unwrap().unwrap().to_string()
}

/// Keeps the change set of every commit it sees.
struct Commits(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Commits {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The Rust API: one unit, one commit, all or nothing
// ---------------------------------------------------------------------------------------------

#[test]
fn a_list_of_rules_is_one_commit_and_codes_every_key_with_reads_unchanged() {
    let mut graph = files(60);
    let oracle = content(&graph);
    let commits: Arc<Mutex<Vec<ChangeSet>>> = Arc::default();
    graph
        .register_commit_hook(Box::new(Commits(commits.clone())))
        .unwrap();
    let seq = graph.last_commit_seq();
    graph
        .traversal_mut()
        .define_compressions(three_rules())
        .unwrap();
    assert_eq!(graph.last_commit_seq(), seq + 1, "one commit");
    let seen = commits.lock().unwrap();
    assert_eq!(seen.len(), 1, "a hook sees one unit");
    let set: Vec<&str> = seen[0]
        .mutations()
        .iter()
        .filter_map(|m| match m {
            Mutation::SetDefinition { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(set, ["file_kind", "file_owner", "link_rel"]);
    drop(seen);
    invariant(&graph);
    assert_eq!(content(&graph), oracle, "reads never change");
    let first = first_vertex(&graph);
    assert!(graph.is_dedup_coded(ElementKind::Vertex, &first, "kind"));
    assert!(graph.is_dedup_coded(ElementKind::Vertex, &first, "owner"));
    assert!(!graph.is_dedup_coded(ElementKind::Vertex, &first, "size"));
    let stats = graph.compression_stats("link_rel").unwrap();
    assert_eq!(stats.compressed_values, 59);
    assert_eq!(stats.distinct_values, 2);
}

#[test]
fn a_refused_rule_fails_the_whole_list_and_names_itself() {
    let mut graph = files(30);
    let oracle = content(&graph);
    let seq = graph.last_commit_seq();
    let mut rules = three_rules();
    // `dictionary: true` is a field error of a dedup rule.
    rules.insert(
        1,
        (
            "file_size".to_string(),
            dedup(ElementKind::Vertex, "file", "size").with_dictionary(true),
        ),
    );
    let err = graph
        .traversal_mut()
        .define_compressions(rules)
        .unwrap_err()
        .to_string();
    assert!(err.contains("file_size"), "{err}");
    assert!(rule_names(&graph).is_empty(), "nothing stays");
    assert_eq!(graph.last_commit_seq(), seq, "nothing committed");
    invariant(&graph);
    assert_eq!(content(&graph), oracle);
    assert!(!graph.is_dedup_coded(ElementKind::Vertex, &first_vertex(&graph), "kind"));
}

#[test]
fn two_rules_of_one_list_on_the_same_key_conflict_and_nothing_is_stored() {
    let mut graph = files(30);
    let err = graph
        .traversal_mut()
        .define_compressions([
            ("a".to_string(), dedup(ElementKind::Vertex, "file", "kind")),
            ("b".to_string(), dedup(ElementKind::Vertex, "file", "owner")),
            ("c".to_string(), dedup(ElementKind::Vertex, "file", "kind")),
        ])
        .unwrap_err();
    let TraverserError::Graph(GraphError::Definition(DefinitionError::Conflict { name, .. })) =
        err.root_cause()
    else {
        panic!("{err}");
    };
    assert_eq!(name, "c");
    assert!(rule_names(&graph).is_empty());
    invariant(&graph);
    // A conflict with a stored rule refuses the list as well.
    graph
        .traversal_mut()
        .define_compression("kept", dedup(ElementKind::Vertex, "file", "kind"))
        .unwrap();
    assert!(
        graph
            .traversal_mut()
            .define_compressions(three_rules())
            .is_err()
    );
    assert_eq!(rule_names(&graph), ["kept"]);
    invariant(&graph);
}

#[test]
fn an_outer_rollback_takes_the_whole_list_back() {
    let mut graph = files(40);
    let oracle = content(&graph);
    let result: Result<(), GraphError> = graph.transaction(|unit| {
        GraphTraversalSource::new(&mut *unit)
            .define_compressions(three_rules())
            .map_err(|err| GraphError::unsupported(Box::leak(err.to_string().into_boxed_str())))?;
        Err(GraphError::unsupported("rolled back on purpose"))
    });
    assert!(result.is_err());
    assert!(rule_names(&graph).is_empty());
    invariant(&graph);
    assert_eq!(content(&graph), oracle);
    // Applied again afterwards: the state is clean.
    graph
        .traversal_mut()
        .define_compressions(three_rules())
        .unwrap();
    invariant(&graph);
    assert_eq!(content(&graph), oracle);
}

#[test]
fn a_list_on_a_foreign_storage_is_stored_by_the_default_loop() {
    // `&mut TraversalGraph` forwards; a `Graph<S>` front end reaches the override through
    // `AnyGraph::set_definitions` too.
    let graph = Arc::new(Graph::new(files(20)));
    let any: Arc<dyn graphersal::storage::AnyGraph> = graph.clone();
    any.set_definitions(
        three_rules()
            .into_iter()
            .map(|(name, rule)| Definition::compression(name, rule))
            .collect(),
    )
    .unwrap();
    assert_eq!(rule_names(&graph.read()).len(), 3);
    invariant(&graph.read());
}

#[cfg(feature = "persist")]
#[test]
fn a_list_is_one_wal_commit_and_reopens_coded() {
    use graphersal::persist::{MemDir, Store, StoreOptions};
    let dir = MemDir::new();
    let store = Store::create_with(&dir, files(50), StoreOptions::new()).unwrap();
    let oracle = content(&store.graph().read());
    let seq = store.graph().read().last_commit_seq();
    store
        .graph()
        .write()
        .traversal_mut()
        .define_compressions(three_rules())
        .unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), seq + 1);
    let stats: Vec<_> = ["file_kind", "file_owner", "link_rel"]
        .iter()
        .map(|name| store.graph().read().compression_stats(name).unwrap())
        .collect();
    store.close().unwrap();
    // WAL replay: the one commit holds the three rules.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    invariant(&store.graph().read());
    assert_eq!(content(&store.graph().read()), oracle);
    let reopened: Vec<_> = ["file_kind", "file_owner", "link_rel"]
        .iter()
        .map(|name| store.graph().read().compression_stats(name).unwrap())
        .collect();
    assert_eq!(reopened, stats);
    assert_eq!(store.graph().read().last_commit_seq(), seq + 1);
    store.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// The DSL: `g.define_compressions([..])`
// ---------------------------------------------------------------------------------------------

#[test]
fn the_dsl_defines_a_list_with_paths_in_one_commit() {
    let graph = Arc::new(Graph::new(files(30)));
    let oracle = content(&graph.read());
    let seq = graph.read().last_commit_seq();
    let stored = eval_value(
        graph.clone(),
        r#"g.define_compressions([
            #{label: "file", paths: ["kind", "owner"], codec: "dedup"},
            #{label: "link", paths: ["rel"], element: "edge", codec: "dedup"},
            #{name: "sizes", label: "file", path: "size", codec: "dedup"},
        ])"#,
    )
    .unwrap();
    assert_eq!(stored.as_int().unwrap(), 4);
    assert_eq!(graph.read().last_commit_seq(), seq + 1);
    assert_eq!(
        rule_names(&graph.read()),
        ["file_kind", "file_owner", "link_rel_edge", "sizes"]
    );
    invariant(&graph.read());
    assert_eq!(content(&graph.read()), oracle);
    let listed = eval_value(graph.clone(), "g.compressions().map(|r| r.codec)").unwrap();
    assert_eq!(
        format!("{listed}"),
        r#"["dedup", "dedup", "dedup", "dedup"]"#
    );
}

#[test]
fn the_dsl_refuses_a_bad_list_and_stores_nothing() {
    let graph = Arc::new(Graph::new(files(10)));
    let refused = |script: &str| -> String {
        let err = eval_value(graph.clone(), script).unwrap_err().to_string();
        assert!(rule_names(&graph.read()).is_empty(), "{script}: {err}");
        err
    };
    let err = refused(r#"g.define_compressions(#{label: "file", path: "kind"})"#);
    assert!(err.contains("a list of rule maps"), "{err}");
    let err = refused(r#"g.define_compressions([#{name: "x", label: "file", paths: ["kind"]}])"#);
    assert!(err.contains("name with paths"), "{err}");
    let err = refused(r#"g.define_compressions([#{label: "file", paths: []}])"#);
    assert!(err.contains("an empty list"), "{err}");
    let err = refused(r#"g.define_compressions([#{label: "file", paths: ["kind"], colour: 1}])"#);
    assert!(err.contains("the key 'colour'"), "{err}");
    let err = refused(r#"g.define_compressions([#{label: "file", path: "kind"}])"#);
    assert!(err.contains("no name"), "{err}");
    // A valid rule before a refused one: all or nothing.
    let err = refused(
        r#"g.define_compressions([
            #{name: "ok", label: "file", path: "kind", codec: "dedup"},
            #{name: "bad", label: "file", path: "size", codec: "dedup", dictionary: true},
        ])"#,
    );
    assert!(err.contains("bad"), "{err}");
    // Two rules of the list on one key.
    let err = refused(
        r#"g.define_compressions([
            #{name: "one", label: "file", path: "kind", codec: "dedup"},
            #{name: "two", label: "file", path: "kind"},
        ])"#,
    );
    assert!(err.contains("two"), "{err}");
    // `define_compression` keeps refusing `paths`.
    let err = refused(r#"g.define_compression(#{label: "file", paths: ["kind"]})"#);
    assert!(err.contains("the key 'paths'"), "{err}");
}

#[test]
fn the_dsl_list_is_refused_in_a_saved_query() {
    let graph = Arc::new(Graph::new(files(5)));
    let err = eval_value(
        graph.clone(),
        r#"g.define_query(#{name: "bulk", body: "fn bulk() { g.define_compressions([]) }"})"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("define_compressions"), "{err}");
}

#[test]
fn the_dsl_list_asks_for_each_rule_before_anything_changes() {
    let graph = Arc::new(Graph::new(files(10)));
    graph
        .write()
        .traversal_mut()
        .define_compression("file_kind", dedup(ElementKind::Vertex, "file", "kind"))
        .unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    struct Recorder(Arc<Mutex<Vec<String>>>);
    impl Authorizer for Recorder {
        fn authorize(
            &self,
            request: &graphersal::auth::AccessRequest<'_>,
        ) -> Result<(), graphersal::auth::Denied> {
            self.0.lock().unwrap().push(request.to_string());
            Ok(())
        }
    }
    let _ = graphersal::script::eval_value_with_limits(
        graph.clone(),
        r#"g.define_compressions([#{label: "file", paths: ["kind", "owner"], codec: "dedup"}])"#,
        Default::default(),
        &ScriptLimits::default(),
        Arc::new(Recorder(seen.clone())),
    )
    .unwrap();
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|r| r.starts_with(r#"Update Definition(compression "file_kind")"#)),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|r| r.starts_with(r#"Create Definition(compression "file_owner")"#)),
        "{seen:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Several advisor recommendations at once
// ---------------------------------------------------------------------------------------------

fn advised(n: usize) -> Arc<Graph> {
    let graph = Graph::new(files(n));
    graph.attach_workload_stats(Arc::new(WorkloadStats::new(WorkloadLevel::Memory)));
    Arc::new(graph)
}

#[test]
fn the_advice_groups_dedup_recommendations_and_applies_a_group_in_one_commit() {
    let graph = advised(300);
    let oracle = content(&graph.read());
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    let group = advice
        .groups
        .iter()
        .find(|g| g.key == "add_compression:dedup")
        .unwrap_or_else(|| panic!("{:?}", advice.groups));
    assert!(group.ids.len() >= 3, "{group:?}");
    assert!(
        group
            .title
            .starts_with(&format!("dedup: {} keys, total -", group.ids.len())),
        "{}",
        group.title
    );
    let promised: i64 = advice
        .recommendations
        .iter()
        .filter(|r| group.ids.contains(&r.id))
        .map(|r| r.memory_bytes)
        .sum();
    assert_eq!(group.memory_bytes, promised);
    let cost = &advice
        .recommendations
        .iter()
        .find(|r| r.id == group.ids[0])
        .unwrap()
        .cost;
    assert!(
        cost.contains("about 30 % more") && cost.contains("5-9 %"),
        "{cost}"
    );
    let json = advice.to_json();
    assert!(json.contains("\"groups\":[{"), "{json}");

    let seq = graph.read().last_commit_seq();
    let before = graph.read().memory_usage().unwrap().unwrap().property_bytes;
    let applied = graph
        .apply_recommendations(&group.ids, &AdviceRequest::everything(), None)
        .unwrap();
    assert_eq!(
        applied.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        group.ids
    );
    assert_eq!(graph.read().last_commit_seq(), seq + 1, "one commit");
    invariant(&graph.read());
    assert_eq!(content(&graph.read()), oracle);
    let saved = before - graph.read().memory_usage().unwrap().unwrap().property_bytes;
    // The same commands one by one end in the same memory as the batch (each estimate is made
    // alone, so the sum of the promises is near, not equal to, what the rules save together).
    let one_by_one = advised(300);
    for id in &group.ids {
        let r = advice.recommendations.iter().find(|r| &r.id == id).unwrap();
        let _ = eval_value(one_by_one.clone(), &r.action.command()).unwrap();
    }
    // At most what one by one leaves: the steps between keep row layouts of their own (the
    // layout table drops them only when it doubles), the batch builds only the final ones.
    let batch_bytes = graph.read().memory_usage().unwrap().unwrap().property_bytes;
    let single_bytes = one_by_one
        .read()
        .memory_usage()
        .unwrap()
        .unwrap()
        .property_bytes;
    assert!(
        batch_bytes <= single_bytes && single_bytes - batch_bytes <= single_bytes / 20,
        "batch {batch_bytes} vs one by one {single_bytes}"
    );
    for id in &group.ids {
        let r = advice.recommendations.iter().find(|r| &r.id == id).unwrap();
        let RecommendedAction::AddCompression { name, .. } = &r.action else {
            panic!("{r:?}");
        };
        assert_eq!(
            graph.read().compression_stats(name).unwrap(),
            one_by_one.read().compression_stats(name).unwrap(),
            "{name}"
        );
    }
    assert_eq!(content(&one_by_one.read()), oracle);
    assert!(
        (saved as f64 - promised as f64).abs() <= promised as f64 / 4.0,
        "saved {saved} vs promised {promised}"
    );
    // Gone from the advice, each one listed as applied with its own share: the batch's
    // deviation from its promises lands on each outcome alike.
    let after = graph.advise(&AdviceRequest::everything()).unwrap();
    let mut deviations = Vec::new();
    for id in &group.ids {
        assert!(!after.recommendations.iter().any(|r| &r.id == id), "{id}");
        let outcome = after.applied.iter().find(|a| &a.id == id).unwrap();
        deviations.push(outcome.measured_bytes - outcome.promised_bytes);
    }
    assert!(
        deviations.windows(2).all(|w| w[0] == w[1]),
        "{deviations:?}"
    );
    assert_eq!(deviations[0], saved as i64 - promised, "{deviations:?}");
}

#[test]
fn applying_recommendations_is_all_or_nothing() {
    let graph = advised(120);
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    let mut ids: Vec<String> = advice
        .recommendations
        .iter()
        .filter(|r| matches!(r.action, RecommendedAction::AddCompression { .. }))
        .map(|r| r.id.clone())
        .collect();
    assert!(ids.len() >= 2, "{ids:?}");
    // An id no longer advised: nothing is applied.
    ids.push("add_compression:vertex:file:nope".to_string());
    let seq = graph.read().last_commit_seq();
    let err = graph
        .apply_recommendations(&ids, &AdviceRequest::everything(), None)
        .unwrap_err();
    let TraverserError::Graph(GraphError::Definition(DefinitionError::NotFound {
        kind, name, ..
    })) = err.root_cause()
    else {
        panic!("{err}");
    };
    assert_eq!(
        (kind.as_str(), name.as_str()),
        ("recommendation", "add_compression:vertex:file:nope")
    );
    assert_eq!(graph.read().last_commit_seq(), seq);
    assert!(rule_names(&graph.read()).is_empty());
    ids.pop();
    // A suggested name already taken by another rule: refused, nothing applied.
    let taken = advice
        .recommendations
        .iter()
        .find(|r| r.id == ids[1])
        .unwrap();
    let RecommendedAction::AddCompression { name, .. } = &taken.action else {
        panic!("{taken:?}");
    };
    graph
        .write()
        .traversal_mut()
        .define_compression(name, dedup(ElementKind::Vertex, "file", "id_n"))
        .unwrap();
    let seq = graph.read().last_commit_seq();
    let err = graph
        .apply_recommendations(&ids, &AdviceRequest::everything(), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains(name.as_str()), "{err}");
    assert_eq!(graph.read().last_commit_seq(), seq);
    assert_eq!(rule_names(&graph.read()), [name.clone()]);
    invariant(&graph.read());
    // No ids: nothing to do, no commit.
    let none: [&str; 0] = [];
    assert!(
        graph
            .apply_recommendations(&none, &AdviceRequest::everything(), None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(graph.read().last_commit_seq(), seq);
}

#[test]
fn applying_recommendations_asks_the_authorizer_first() {
    let graph = advised(120);
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    let ids: Vec<String> = advice
        .recommendations
        .iter()
        .map(|r| r.id.clone())
        .collect();
    let seq = graph.read().last_commit_seq();
    for policy in [
        AccessPolicy::read_only(),
        AccessPolicy::allow_all().deny(ActionKind::Read, ResourceKind::Data),
        AccessPolicy::allow_all().deny(ActionKind::Create, ResourceKind::Definition),
    ] {
        let err = graph
            .apply_recommendations(&ids, &AdviceRequest::everything(), Some(Arc::new(policy)))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Permission denied"), "{err}");
        assert_eq!(graph.read().last_commit_seq(), seq);
        assert!(rule_names(&graph.read()).is_empty());
    }
    graph
        .apply_recommendations(
            &ids,
            &AdviceRequest::everything(),
            Some(Arc::new(AccessPolicy::allow_all())),
        )
        .unwrap();
    assert!(!rule_names(&graph.read()).is_empty());
}
