//! Bounded rendering: `RenderOptions::max_rows` / `max_items` limit what a visualizer renders,
//! never the data, and report every cut in `RenderedOutput::truncation`, never in the text.

#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(all(feature = "script", feature = "display"))]
use crate::script_host::rhai::Dynamic;
use graphersal::exec::RenderOptions;
use graphersal::prelude::*;

/// Rows of a rounded `tabled` table: every line that starts with a cell border, minus the header.
#[cfg(feature = "display")]
fn table_rows(table: &str) -> usize {
    table
        .lines()
        .filter(|line| line.starts_with('│'))
        .count()
        .saturating_sub(1)
}

fn rows(n: usize) -> RenderOptions {
    RenderOptions::new().with_max_rows(n)
}

#[cfg(feature = "display")]
#[test]
fn max_rows_on_a_large_graph_renders_exactly_max_rows() {
    let graph = GraphSource::large_110k();
    let rendered = graph
        .read()
        .traversal()
        .v(None::<()>)
        .visualize_with(VisualizeFormat::Table, &rows(100))
        .unwrap();
    assert_eq!(table_rows(&rendered.text), 100);
    let truncation = rendered.truncation.expect("111k vertices, 100 shown");
    assert_eq!(truncation.rows_shown, 100);
    assert!(truncation.more_rows);
    assert!(!truncation.items_cut);
    assert!(
        !rendered.text.contains('…'),
        "the notice is never part of the text"
    );
}

#[cfg(feature = "display")]
#[test]
fn exactly_max_rows_results_are_not_truncated() {
    let graph = TraversalGraph::tinkerpop_modern();
    let rendered = graph
        .traversal()
        .v(None::<()>)
        .visualize_with(VisualizeFormat::Table, &rows(6))
        .unwrap();
    assert_eq!(table_rows(&rendered.text), 6);
    assert_eq!(rendered.truncation, None);

    let rendered = graph
        .traversal()
        .v(None::<()>)
        .visualize_with(VisualizeFormat::Table, &rows(5))
        .unwrap();
    assert_eq!(table_rows(&rendered.text), 5);
    let truncation = rendered.truncation.expect("one more exists");
    assert!(truncation.more_rows);
    assert_eq!(truncation.rows_shown, 5);
}

#[cfg(feature = "display")]
#[test]
fn a_mutating_traversal_updates_every_element_and_only_the_output_is_cut() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rendered = graph
        .traversal_mut()
        .v(None::<()>)
        .property("seen", true)
        .visualize_with(VisualizeFormat::Table, &rows(2))
        .unwrap();
    assert_eq!(table_rows(&rendered.text), 2);
    assert!(rendered.truncation.expect("cut").more_rows);

    let updated = graph
        .traversal()
        .v(None::<()>)
        .has("seen", true)
        .count()
        .next()
        .unwrap();
    assert_eq!(updated.and_then(|c| c.as_i64()), Some(6));
}

#[cfg(feature = "display")]
#[cfg(feature = "script")]
#[test]
fn a_nested_mutation_in_a_script_updates_every_element() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let engine = crate::script_host::engine();
    let mut scope = crate::script_host::graph_scope(graph.clone());
    let value = engine
        .eval_with_scope::<Dynamic>(&mut scope, r#"g.v().local(__.property("seen", true))"#)
        .unwrap();
    let rendered = crate::script_host::render_value(value, VisualizeFormat::Table, &rows(2));
    assert_eq!(table_rows(&rendered.text), 2);
    let count = engine
        .eval_with_scope::<Dynamic>(&mut scope, r#"g.v().has("seen", true).count().next()"#)
        .unwrap();
    assert_eq!(count.as_int().unwrap(), 6);
}

#[cfg(feature = "display")]
#[cfg(feature = "script")]
#[test]
fn max_items_cuts_nested_collections_at_every_depth() {
    let graph = TraversalGraph::tinkerpop_modern();
    let options = RenderOptions::new().with_max_items(2);

    // Depth 1: one fold() row holding every name.
    let rendered = graph
        .traversal()
        .v(None::<()>)
        .values("name")
        .fold()
        .visualize_with(VisualizeFormat::Table, &options)
        .unwrap();
    assert!(
        rendered.text.contains("[marko, vadas, …]"),
        "{}",
        rendered.text
    );
    let truncation = rendered.truncation.expect("cut");
    assert!(truncation.items_cut);
    assert!(!truncation.more_rows);
    assert_eq!(truncation.max_items, Some(2));

    // Depth 2: a list inside a group() entry.
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let value = crate::script_host::eval_value(
        graph,
        r#"g.v().has_label("person").group().by(By::Label).by(__.values("name").fold())"#,
    )
    .unwrap();
    let rendered = crate::script_host::render_value(value, VisualizeFormat::Tree, &options);
    assert!(
        rendered.text.contains("person: [marko, vadas, …]"),
        "{}",
        rendered.text
    );
    assert!(rendered.truncation.expect("cut").items_cut);
}

#[cfg(feature = "serde")]
#[test]
fn max_items_cuts_a_huge_single_group_row() {
    let graph = GraphSource::large_110k();
    let rendered = graph
        .read()
        .traversal()
        .v(None::<()>)
        .id()
        .fold()
        .visualize_with(
            VisualizeFormat::Json,
            &RenderOptions::new().with_max_rows(10).with_max_items(100),
        )
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let rows = json.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].as_array().expect("fold").len(), 100);
    let truncation = rendered.truncation.expect("cut");
    assert!(truncation.items_cut && !truncation.more_rows);
}

#[cfg(feature = "serde")]
#[test]
fn bounded_json_parses_and_has_max_rows_elements_without_markers() {
    let graph = TraversalGraph::tinkerpop_modern();
    let rendered = graph
        .traversal()
        .v(None::<()>)
        .value_map(None::<()>)
        .visualize_with(
            VisualizeFormat::Json,
            &RenderOptions::new().with_max_rows(3).with_max_items(1),
        )
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let rows = json.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row.as_object().expect("map").len(), 1, "{row}");
    }
    assert!(!rendered.text.contains('…'));
    let truncation = rendered.truncation.expect("cut");
    assert!(truncation.more_rows && truncation.items_cut);

    // The JSON-per-line fallback formats follow the same rules.
    let rendered = graph
        .traversal()
        .v(None::<()>)
        .visualize_with(
            VisualizeFormat::Mermaid,
            &RenderOptions::new().with_max_rows(2),
        )
        .unwrap();
    assert_eq!(rendered.text.lines().count(), 2);
    for line in rendered.text.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}

#[cfg(feature = "script")]
#[test]
fn a_schema_and_a_profile_are_not_bounded() {
    let graph = TraversalGraph::tinkerpop_modern();
    let limits = RenderOptions::new().with_max_rows(1).with_max_items(1);
    let bounded = graph
        .traversal()
        .v(None::<()>)
        .infer_schema_step()
        .visualize_with(VisualizeFormat::Table, &limits)
        .unwrap();
    let full = graph
        .traversal()
        .v(None::<()>)
        .infer_schema_step()
        .visualize(VisualizeFormat::Table)
        .unwrap();
    assert_eq!(bounded.text, full);
    assert_eq!(bounded.truncation, None);

    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let value = crate::script_host::eval_value(graph, "g.v().out().profile()").unwrap();
    let rendered = crate::script_host::render_value(value, VisualizeFormat::Auto, &limits);
    assert!(rendered.text.contains("out()"), "{}", rendered.text);
    assert!(rendered.text.lines().count() > 3);
    assert_eq!(rendered.truncation, None);
}

#[cfg(feature = "display")]
#[cfg(feature = "serde")]
#[test]
fn unlimited_output_is_identical_to_the_unbounded_visualizers() {
    let graph = TraversalGraph::tinkerpop_modern();
    for format in [
        VisualizeFormat::Auto,
        VisualizeFormat::Table,
        VisualizeFormat::Markdown,
        VisualizeFormat::Json,
        VisualizeFormat::Tree,
        VisualizeFormat::Mermaid,
    ] {
        let full = graph
            .traversal()
            .v(None::<()>)
            .value_map(None::<()>)
            .visualize(format)
            .unwrap();
        let rendered = graph
            .traversal()
            .v(None::<()>)
            .value_map(None::<()>)
            .visualize_with(format, &RenderOptions::unlimited())
            .unwrap();
        assert_eq!(rendered.text, full, "{format:?}");
        assert_eq!(rendered.truncation, None);
    }
    // A fixed rendering, so a change in the unlimited path shows up as a diff.
    let json = graph
        .traversal()
        .v("1")
        .values("name")
        .visualize(VisualizeFormat::Json)
        .unwrap();
    assert_eq!(json, "[\n  \"marko\"\n]");
}

#[test]
fn the_notice_is_one_function_for_every_front_end() {
    let graph = TraversalGraph::tinkerpop_modern();
    let rendered = graph
        .traversal()
        .v(None::<()>)
        .values("name")
        .fold()
        .visualize_with(
            VisualizeFormat::Table,
            &RenderOptions::new().with_max_items(2),
        )
        .unwrap();
    assert_eq!(
        rendered.notice().unwrap(),
        "… nested collections were cut to 2 items. Use #{max_items: n}, /set max-items <n> \
         (--max-items <n>), or /set no-limit (--no-limit)."
    );
    let rendered = graph
        .traversal()
        .v(None::<()>)
        .visualize_with(VisualizeFormat::Table, &rows(3))
        .unwrap();
    assert_eq!(
        rendered.notice().unwrap(),
        "… showing the first 3 rows; more exist. Use .limit(n), #{no_limit: true}, \
         /set no-limit (--no-limit), or to_list() for the data."
    );
}
