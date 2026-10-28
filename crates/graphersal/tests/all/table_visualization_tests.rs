#[cfg(feature = "display")]
use graphersal::exec::RenderOptions;
use graphersal::prelude::*;

#[test]
fn test_vertex_to_table_default() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let table = g.v(None::<()>).has_label("person").to_table().unwrap();
    assert!(!table.is_empty());
    // Columns should include id, label, and sorted properties: age, name
    assert!(table.contains("id"));
    assert!(table.contains("label"));
    assert!(table.contains("age"));
    assert!(table.contains("name"));
    assert!(table.contains("marko"));
    assert!(table.contains("vadas"));
}

#[test]
fn test_vertex_to_markdown() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let md = g.v(None::<()>).has_label("person").to_markdown().unwrap();
    assert!(!md.is_empty());
    assert!(md.contains("id"));
    assert!(md.contains("label"));
    assert!(md.contains("age"));
    assert!(md.contains("name"));
    assert!(md.contains("marko"));
}

#[test]
fn test_edge_to_table_default() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let table = g.e(None::<()>).to_table().unwrap();
    assert!(!table.is_empty());
    // Columns for edge: id, label, outV, inV, weight
    assert!(table.contains("id"));
    assert!(table.contains("label"));
    assert!(table.contains("outV"));
    assert!(table.contains("inV"));
    assert!(table.contains("weight"));
    assert!(table.contains("knows"));
    assert!(table.contains("created"));
}

#[cfg(feature = "display")]
#[test]
fn test_column_inclusion() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let options = RenderOptions::new().with_columns(vec!["name", "age"]);
    let md = g
        .v(None::<()>)
        .has_label("person")
        .to_markdown_with(&options)
        .unwrap()
        .text;

    assert!(md.contains("name"));
    assert!(md.contains("age"));
    assert!(!md.contains("id"));
    assert!(!md.contains("label"));
}

#[cfg(feature = "display")]
#[test]
fn test_column_exclusion() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let options = RenderOptions::new().with_columns(vec!["!id", "!label"]);
    let md = g
        .v(None::<()>)
        .has_label("person")
        .to_markdown_with(&options)
        .unwrap()
        .text;

    assert!(!md.contains("id"));
    assert!(!md.contains("label"));
    assert!(md.contains("age"));
    assert!(md.contains("name"));

    // Check with minus sign prefix
    let options_minus = RenderOptions::new().with_columns(vec!["-id", "-label"]);
    let md_minus = tg
        .traversal()
        .v(None::<()>)
        .has_label("person")
        .to_markdown_with(&options_minus)
        .unwrap()
        .text;
    assert_eq!(md, md_minus);
}

#[cfg(feature = "display")]
#[test]
fn test_missing_property_handling() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    // All vertices: person has age, software has lang
    let md = g.v(None::<()>).to_markdown().unwrap();
    assert!(md.contains("age"));
    assert!(md.contains("lang"));
    // Marko has age 29, lang "-"
    assert!(md.contains("marko"));
    assert!(md.contains("29"));
    // Lop (software) has age "-", lang "java"
    assert!(md.contains("lop"));
    assert!(md.contains("java"));
    assert!(md.contains("-"));
}

#[cfg(feature = "display")]
#[test]
fn test_row_numbers_option() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let options = RenderOptions::new().with_row_numbers(true);
    let md = g
        .v(None::<()>)
        .has_label("person")
        .to_markdown_with(&options)
        .unwrap()
        .text;

    assert!(md.contains("#"));
    assert!(md.contains("id"));
    assert!(md.contains("label"));
    assert!(md.contains("age"));
    assert!(md.contains("name"));
    assert!(md.contains("marko"));
}

#[cfg(feature = "display")]
#[test]
fn test_truncate_and_max_width() {
    let graph = GraphSource::empty();
    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "marko")
        .property("age", 29)
        .property("comment", "very long comment for testing truncate option")
        .next()
        .unwrap();

    let options = RenderOptions::new()
        .with_truncate(25)
        .with_row_numbers(true);
    let table = graph
        .read()
        .traversal()
        .v(None::<()>)
        .to_table_with(&options)
        .unwrap()
        .text;
    // Headers and other columns should NOT be collapsed or emptied
    assert!(table.contains("id"));
    assert!(table.contains("label"));
    assert!(table.contains("comment"));
    assert!(table.contains("name"));
    assert!(table.contains("marko"));
    assert!(table.contains("..."));

    let options_wrap = RenderOptions::new().with_max_width(20);
    let table_wrap = graph
        .read()
        .traversal()
        .v(None::<()>)
        .to_table_with(&options_wrap)
        .unwrap()
        .text;
    assert!(table_wrap.contains("marko"));
    assert!(table_wrap.contains("comment"));
}

#[test]
fn test_record_object_table() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let md = g
        .v(None::<()>)
        .has_label("person")
        .project(vec!["name", "age"])
        .by("name")
        .by("age")
        .to_markdown()
        .unwrap();
    assert!(md.contains("age"));
    assert!(md.contains("name"));
    assert!(md.contains("29"));
    assert!(md.contains("marko"));
}

#[cfg(feature = "display")]
#[test]
fn test_scalar_table() {
    let tg = TraversalGraph::tinkerpop_modern();
    let g = tg.traversal();

    let md = g
        .v(None::<()>)
        .has_label("person")
        .values("name")
        .to_markdown()
        .unwrap();
    assert!(md.contains("value"));
    assert!(md.contains("marko"));
}
