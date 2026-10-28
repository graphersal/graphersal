//! `drop()` over every stream shape: elements and property handles are removed, anything else
//! fails with a cast error and removes nothing (the failing traversal is rolled back).

#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(feature = "script")]
use crate::script_host::eval_value;
use graphersal::prelude::*;

#[cfg(feature = "script")]
fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

#[cfg(feature = "script")]
fn int(graph: &Arc<Graph>, script: &str) -> i64 {
    eval_value(graph.clone(), script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .as_int()
        .unwrap_or_else(|t| panic!("{script}: not an int but {t}"))
}

#[cfg(feature = "script")]
#[test]
fn edge_properties_are_dropped_in_both_spellings() {
    let g = modern();
    let _ = eval_value(
        g.clone(),
        r#"g.E().hasLabel("knows").properties("weight").drop().toList()"#,
    )
    .unwrap();
    assert_eq!(int(&g, r#"g.E().properties("weight").count().next()"#), 4);
    let _ = eval_value(g.clone(), r#"g.e().properties("weight").drop().to_list()"#).unwrap();
    assert_eq!(int(&g, r#"g.E().properties().count().next()"#), 0);
    assert_eq!(int(&g, r#"g.E().count().next()"#), 6, "the edges stay");
}

#[test]
fn rust_api_drops_edge_properties_and_edges() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .e("0")
        .properties("weight")
        .drop()
        .to_list()
        .unwrap();
    assert!(
        graph
            .traversal()
            .e("0")
            .values("weight")
            .to_list()
            .unwrap()
            .is_empty()
    );
    graph.traversal_mut().e("0").drop().to_list().unwrap();
    assert_eq!(graph.edge_count(), 5);
}

#[cfg(feature = "script")]
#[test]
fn non_removable_values_are_cast_errors_and_nothing_is_removed() {
    for (script, got) in [
        (r#"g.inject(1).drop().toList()"#, "int64"),
        (
            r#"g.V("1").properties(jpath("name")).drop().toList()"#,
            "pathProperty",
        ),
        (r#"g.V("1").valueMap().unfold().drop().toList()"#, "entry"),
        (r#"g.V().infer_schema().drop().toList()"#, "schema"),
        (r#"g.E().subgraph("sg").cap("sg").drop().toList()"#, "graph"),
        // a vertex dropped before the failing value is restored with the failure
        (
            r#"g.V("1").sideEffect(__.drop()).constant(1).drop().toList()"#,
            "int64",
        ),
    ] {
        let g = modern();
        let err = eval_value(g.clone(), script).expect_err(script).to_string();
        assert!(
            err.contains(&format!("got type '{got}'")),
            "{script}: {err}"
        );
        assert!(
            err.contains("expected type [vertex or vertexProperty or edge or edgeProperty]"),
            "{script}: {err}"
        );
        assert_eq!(int(&g, "g.V().count().next()"), 6, "{script}");
        assert_eq!(int(&g, "g.V().properties().count().next()"), 12, "{script}");
    }
}

/// A path, a list, a map and a computed value are reported by their real kind, never as the
/// internal `computed`.
#[cfg(feature = "script")]
#[test]
fn non_removable_containers_are_named_by_kind() {
    for (script, got) in [
        (r#"g.V("1").path().drop().toList()"#, "path"),
        (r#"g.v("1").path().drop().to_list()"#, "path"),
        (r#"g.V("1").fold().drop().toList()"#, "array"),
        (r#"g.V("1").valueMap().drop().toList()"#, "object"),
        (
            r#"g.V("1").project("n").by("name").drop().toList()"#,
            "object",
        ),
        (r#"g.inject([1, 2]).drop().toList()"#, "array"),
        (
            r#"g.V("1").values("name").as_string().drop().toList()"#,
            "string",
        ),
    ] {
        let err = eval_value(modern(), script).expect_err(script).to_string();
        assert!(
            err.contains(&format!("got type '{got}'")),
            "{script}: {err}"
        );
    }
}
