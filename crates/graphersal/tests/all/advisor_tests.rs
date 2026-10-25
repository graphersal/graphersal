//! The storage advisor (book page "Optimize", `graphersal::advisor`): recommendations from the
//! memory report and the workload statistics, each applied by the command it names, after
//! which it is gone.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::advisor::{Advice, AdviceRequest, Confidence, RecommendedAction};
use graphersal::prelude::*;
use graphersal::workload::{WorkloadLevel, WorkloadStats};

use crate::script_host::eval_value;

fn run(graph: &Arc<Graph>, script: &str) {
    let _ = eval_value(graph.clone(), script).unwrap_or_else(|err| panic!("{script}: {err}"));
}

fn with_stats(graph: Graph) -> Arc<Graph> {
    graph.attach_workload_stats(Arc::new(WorkloadStats::new(WorkloadLevel::Memory)));
    Arc::new(graph)
}

fn ids(advice: &Advice) -> Vec<&str> {
    advice
        .recommendations
        .iter()
        .map(|r| r.id.as_str())
        .collect()
}

/// A graph whose `customer.cv` holds large repeated texts.
fn customers() -> Graph {
    let graph = Graph::new(TraversalGraph::new());
    let cv = "Senior engineer, led the platform team and the data migration. ".repeat(20);
    let script = format!(
        r#"for i in 0..300 {{ g.add_v("customer").property("n", i).property("cv", "{cv}" + (i % 7)).iterate(); }}"#
    );
    let graph = Arc::new(graph);
    run(&graph, &script);
    Arc::try_unwrap(graph).unwrap()
}

#[test]
fn a_frequent_full_scan_is_advised_an_index_and_applying_it_ends_the_advice() {
    let graph = with_stats(GraphSource::tinkerpop_modern());
    for _ in 0..3 {
        run(
            &graph,
            r#"g.v().has_label("person").has("age", P.gt(30)).values("name").to_list()"#,
        );
    }
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    let index = advice
        .recommendations
        .iter()
        .find(|r| r.id == "create_index:vertex:person:age")
        .unwrap_or_else(|| panic!("{:?}", ids(&advice)));
    assert!(matches!(
        index.action,
        RecommendedAction::CreateIndex { .. }
    ));
    assert!(
        index.evidence.contains("filtered 3 times"),
        "{}",
        index.evidence
    );
    assert!(index.memory_bytes < 0 && !index.measured);
    assert_eq!(index.confidence, Confidence::Medium);
    assert_eq!(
        index.action.command(),
        r#"g.create_index(#{label: "person", key: "age"})"#
    );
    // Below the default thresholds (a tiny graph, three runs): nothing.
    let quiet = graph.advise(&AdviceRequest::new()).unwrap();
    assert!(!ids(&quiet).iter().any(|id| id.starts_with("create_index")));
    run(&graph, &index.action.command());
    let after = graph.advise(&AdviceRequest::everything()).unwrap();
    assert!(
        !ids(&after).contains(&"create_index:vertex:person:age"),
        "{:?}",
        ids(&after)
    );
}

#[test]
fn an_index_no_query_used_is_advised_to_be_dropped() {
    let graph = with_stats(GraphSource::tinkerpop_modern());
    run(&graph, r#"g.create_index(#{label: "person", key: "name"})"#);
    run(
        &graph,
        r#"g.v().has_label("software").values("lang").to_list()"#,
    );
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    let drop = advice
        .recommendations
        .iter()
        .find(|r| r.id == "drop_index:person_name")
        .unwrap_or_else(|| panic!("{:?}", ids(&advice)));
    assert!(drop.measured && drop.memory_bytes > 0, "{drop:?}");
    // Used once: not advised any more.
    run(
        &graph,
        r#"g.v().has_label("person").has("name", "marko").to_list()"#,
    );
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    assert!(!ids(&advice).contains(&"drop_index:person_name"));
    // With the default thresholds an index is called unused only after days of statistics.
    run(
        &graph,
        r#"g.create_index(#{label: "software", key: "lang"})"#,
    );
    let advice = graph.advise(&AdviceRequest::new()).unwrap();
    assert!(!ids(&advice).iter().any(|id| id.starts_with("drop_index")));
}

#[test]
fn large_repeated_strings_are_advised_a_measured_compression() {
    let graph = with_stats(customers());
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    let rule = advice
        .recommendations
        .iter()
        .find(|r| r.id == "add_compression:vertex:customer:cv")
        .unwrap_or_else(|| panic!("{:?}", ids(&advice)));
    assert!(rule.measured && rule.memory_bytes > 0, "{rule:?}");
    assert!(advice.estimates >= 1);
    // Seven distinct texts: storing each once wins.
    let RecommendedAction::AddCompression {
        candidate, name, ..
    } = &rule.action
    else {
        panic!("{rule:?}");
    };
    assert_eq!(candidate.codec.name(), "dedup", "{rule:?}");
    assert_eq!(name, "customer_cv");
    let before = graph.read().memory_usage().unwrap().property_bytes;
    run(&graph, &rule.action.command());
    let saved = before - graph.read().memory_usage().unwrap().property_bytes;
    // What was promised is what the rule saves (the estimate measures with the rule's code;
    // the dedup table's overhead is counted per entry, so within 1 %).
    let promised = rule.memory_bytes as f64;
    assert!(
        (saved as f64 - promised).abs() <= promised / 100.0,
        "{saved} vs {rule:?}"
    );
    let after = graph.advise(&AdviceRequest::everything()).unwrap();
    assert!(
        !ids(&after)
            .iter()
            .any(|id| id.starts_with("add_compression"))
    );
}

#[test]
fn a_compressed_key_that_became_hot_is_advised_back_to_plain() {
    let graph = with_stats(customers());
    run(
        &graph,
        r#"g.define_compression(#{name: "cv", label: "customer", path: "cv", min_bytes: 64})"#,
    );
    for _ in 0..3 {
        run(
            &graph,
            r#"g.v().has_label("customer").values("cv").to_list()"#,
        );
    }
    let mut request = AdviceRequest::everything();
    request.hot_reads = 3;
    let advice = graph.advise(&request).unwrap();
    let drop = advice
        .recommendations
        .iter()
        .find(|r| r.id == "drop_compression:cv")
        .unwrap_or_else(|| panic!("{:?}", ids(&advice)));
    assert!(drop.memory_bytes < 0, "{drop:?}");
    assert_eq!(drop.action.command(), r#"g.drop_compression("cv")"#);
}

#[test]
fn without_workload_statistics_only_the_data_is_advised() {
    let graph = Arc::new(customers());
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    assert_eq!(advice.workload_level, WorkloadLevel::Off);
    assert_eq!(advice.observed_since, None);
    assert_eq!(ids(&advice), ["add_compression:vertex:customer:cv"]);
    let json = advice.to_json();
    for key in [
        "\"recommendations\"",
        "\"command\"",
        "\"memory_bytes\"",
        "\"confidence\"",
        "\"workload_level\":\"off\"",
    ] {
        assert!(json.contains(key), "{key}: {json}");
    }
}
