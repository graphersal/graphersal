//! A bare-string operand of a predicate names a path label only under `where(P)`
//! (TinkerPop's `WherePredicateStep`). The value filters `has(key, P)`, `is(P)`, `hasValue(P)`,
//! `any(P)`/`all(P)`/`none(P)`, `has(jpath, P)` and `has(key, traversal)` compare with the
//! literal, as TinkerPop's `HasContainer`/`IsStep` do, even when an `as()` of the same spelling
//! labels the path. Before, `as("vadas")..has("name", P.eq("vadas"))` compared the property with
//! the labelled vertex and found nothing.
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;

use crate::script_host::{ScriptLimits, eval_value_with_limits};

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn run(graph: &Arc<Graph>, script: &str) -> Dynamic {
    eval_value_with_limits(
        graph.clone(),
        script,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .unwrap_or_else(|err| panic!("{script}: {err}"))
}

/// The rendered items of a list result, in order.
fn items(graph: &Arc<Graph>, script: &str) -> Vec<String> {
    run(graph, script)
        .into_array()
        .expect("a list")
        .into_iter()
        .map(|item| item.to_string())
        .collect()
}

fn count(graph: &Arc<Graph>, script: &str) -> i64 {
    run(graph, script).as_int().expect("an integer")
}

#[test]
fn has_with_a_predicate_compares_the_literal_not_a_same_named_label() {
    let g = modern();
    for query in [
        r#"g.V().has("name", "marko").as("vadas").out("knows").has("name", P.eq("vadas")).values("name").to_list()"#,
        r#"g.v("1").as("vadas").out("knows").has("name", P.eq("vadas")).values("name").to_list()"#,
        r#"g.v("1").as("a").out("knows").has("name", P.eq("a").or(P.eq("vadas"))).values("name").to_list()"#,
        r#"g.v("1").as("vadas").out("knows").has("name", __.is(P.eq("vadas"))).values("name").to_list()"#,
        r#"g.v("1").as("vadas").out("knows").values("name").is(P.eq("vadas")).to_list()"#,
        r#"g.v("1").as("vadas").out("knows").properties("name").has_value(P.eq("vadas")).value().to_list()"#,
    ] {
        assert_eq!(items(&g, query), vec!["vadas"], "{query}");
    }
    // `neq` keeps every other value (josh), and drops vadas.
    assert_eq!(
        items(
            &g,
            r#"g.v("1").as("vadas").out("knows").has("name", P.neq("vadas")).values("name").to_list()"#
        ),
        vec!["josh"]
    );
    // A map and a jpath key compare the literal too.
    assert_eq!(
        count(
            &g,
            r#"g.inject(#{name: "vadas"}).as("vadas").has("name", P.eq("vadas")).count().next()"#
        ),
        1
    );
    assert_eq!(
        count(
            &g,
            r#"g.inject(#{a: #{name: "vadas"}}).as("vadas").has(jpath("a.name"), P.eq("vadas")).count().next()"#
        ),
        1
    );
}

#[test]
fn list_predicates_compare_the_literal_not_a_same_named_label() {
    let g = modern();
    let list = r#"g.v("1").as("josh").out("knows").values("name").fold()"#;
    assert_eq!(
        count(&g, &format!(r#"{list}.any(P.eq("josh")).count().next()"#)),
        1
    );
    assert_eq!(
        count(&g, &format!(r#"{list}.none(P.eq("josh")).count().next()"#)),
        0
    );
    assert_eq!(
        count(&g, &format!(r#"{list}.all(P.neq("josh")).count().next()"#)),
        0
    );
}

#[test]
fn where_still_resolves_path_labels() {
    let g = modern();
    let base = r#"g.v("1").as("A").out("created").in("created")"#;
    assert_eq!(
        items(&g, &format!(r#"{base}.where(P.eq("A")).id().to_list()"#)),
        vec!["1"]
    );
    assert_eq!(
        items(&g, &format!(r#"{base}.where(P.neq("A")).id().to_list()"#)),
        vec!["6", "4"]
    );
    assert_eq!(
        items(
            &g,
            &format!(r#"{base}.where(P.eq("A")).by("name").id().to_list()"#)
        ),
        vec!["1"]
    );
    assert_eq!(
        items(
            &g,
            &format!(r#"{base}.as("B").where("A", P.eq("B")).id().to_list()"#)
        ),
        vec!["1"]
    );
    // `is()` is a value filter: the same label is never read, the vertex never equals "A".
    assert!(items(&g, &format!(r#"{base}.is(P.eq("A")).id().to_list()"#)).is_empty());
    assert_eq!(
        items(
            &g,
            &format!(r#"{base}.is(P.neq("A")).id().order().to_list()"#)
        ),
        vec!["1", "4", "6"]
    );
}

#[test]
fn a_value_filter_records_no_path_label() {
    let g = modern();
    let metrics = run(
        &g,
        r#"g.v("1").as("vadas").out("knows").has("name", P.eq("vadas")).values("name").profile()"#,
    )
    .try_cast::<graphersal::profile::TraversalMetrics>()
    .expect("metrics");
    let text = metrics.to_string();
    assert!(!text.contains("[path:"), "{text}");
}

#[test]
fn the_rust_api_compares_the_literal() {
    let graph = GraphSource::tinkerpop_modern();
    let names = graph
        .read()
        .traversal()
        .v("1")
        .as_("vadas")
        .out("knows")
        .has_p("name", P::Eq("vadas"))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, vec![ElementProperty::String("vadas".into())]);
}
