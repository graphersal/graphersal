//! The memory report of a `LayeredGraph` (S4.7, design §7): with the delta scope
//! (`MemoryReportRequest::with_scope(MemoryReportScope::Delta)`, DSL `#{scope: "delta"}`) the
//! rows cover only what the layer holds of its own (added and modified elements), the structure
//! is the layer's own (`memory_structure`: its slots, stubs, removed base elements, the totals of
//! `memory_usage()`), and the base is one line, `shared_bytes`. The default scope (`all`) walks
//! everything the layer shows. A plain graph reports the same rows in both scopes and shares
//! nothing.

use std::collections::BTreeSet;
use std::sync::Arc;

use graphersal::catalog::{Definition, PropertyIndexDefinition};
use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, Graph, GraphStorage, LayeredGraph, MemoryReport, MemoryReportRequest,
    MemoryReportScope,
};

/// The base: 2 000 people (a long name, an age, indexed by age), 1 000 items (a sku) and
/// 2 000 `bought` edges (a quantity).
fn base() -> Arc<TraversalGraph> {
    let mut graph = TraversalGraph::new();
    let mut people = Vec::new();
    for i in 0..2_000i64 {
        let mut map = PropertyMap::default();
        map.insert(
            "name",
            format!("person number {i:05} with a long name").into(),
        );
        map.insert("age", (20 + i % 50).into());
        people.push(
            graph
                .insert_vertex(Some(format!("p{i}").into()), vec!["person".into()], map)
                .unwrap(),
        );
    }
    let mut items = Vec::new();
    for i in 0..1_000i64 {
        let mut map = PropertyMap::default();
        map.insert("sku", format!("sku-{i}").into());
        items.push(
            graph
                .insert_vertex(Some(format!("i{i}").into()), vec!["item".into()], map)
                .unwrap(),
        );
    }
    for (i, person) in people.iter().enumerate() {
        let mut map = PropertyMap::default();
        map.insert("qty", (i as i64 % 7).into());
        graph
            .insert_edge(
                Some(format!("b{i}").into()),
                Some("bought".into()),
                *person,
                items[i % items.len()],
                map,
            )
            .unwrap();
    }
    graph
        .set_definitions(vec![Definition::property_index(
            "person_age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        )])
        .unwrap();
    Arc::new(graph)
}

/// A layer touching about 1 % of the base: 20 people get a score (images), 10 `new` vertices
/// (a tag) are added and linked to 10 other people (stubs only), 10 `bought` edges are dropped.
fn touched(base: &Arc<TraversalGraph>) -> LayeredGraph {
    let mut layer = LayeredGraph::over(Arc::clone(base)).unwrap();
    for i in 0..20 {
        let person = layer.get_vertex_by_id(&format!("p{i}")).unwrap().unwrap();
        layer
            .set_vertex_property(person, "score".into(), (i as i64).into())
            .unwrap();
    }
    for i in 0..10 {
        let mut map = PropertyMap::default();
        map.insert("tag", format!("t{i}").into());
        let new = layer
            .insert_vertex(Some(format!("n{i}").into()), vec!["new".into()], map)
            .unwrap();
        let person = layer
            .get_vertex_by_id(&format!("p{}", 100 + i))
            .unwrap()
            .unwrap();
        let mut map = PropertyMap::default();
        map.insert("w", 0.5.into());
        layer
            .insert_edge(None, Some("linked".into()), new, person, map)
            .unwrap();
    }
    for i in 500..510 {
        let edge = layer.get_edge_by_id(&format!("b{i}")).unwrap().unwrap();
        layer.drop_edge(edge).unwrap();
    }
    layer
}

fn report<S: GraphStorage>(graph: &S, request: MemoryReportRequest) -> MemoryReport {
    GraphTraversalSource::new(graph)
        .memory_report(&request.with_limit(usize::MAX))
        .unwrap()
}

fn delta() -> MemoryReportRequest {
    MemoryReportRequest::new().with_scope(MemoryReportScope::Delta)
}

/// `(kind, label, elements)` of every label row.
fn labels(report: &MemoryReport) -> BTreeSet<(String, String, u64)> {
    report
        .labels
        .iter()
        .map(|row| {
            (
                row.element.as_str().to_string(),
                row.label.as_deref().unwrap_or("-").to_string(),
                row.elements,
            )
        })
        .collect()
}

/// `(label, key, elements)` of every property row.
fn rows(report: &MemoryReport) -> BTreeSet<(String, String, u64)> {
    report
        .properties
        .iter()
        .map(|row| {
            (
                row.target.label.as_deref().unwrap_or("-").to_string(),
                row.target.key().unwrap_or("?").to_string(),
                row.elements,
            )
        })
        .collect()
}

fn set(entries: &[(&str, &str, u64)]) -> BTreeSet<(String, String, u64)> {
    entries
        .iter()
        .map(|(a, b, n)| (a.to_string(), b.to_string(), *n))
        .collect()
}

/// The delta report of a 1 % layer lists only what the layer touched; its structure is the
/// layer's own (`memory_usage()`, whose total leaves the base out) and the base is
/// `shared_bytes`.
#[test]
fn the_delta_report_lists_only_what_the_layer_touched() {
    let base = base();
    let base_total = base.memory_usage().unwrap().unwrap().total_bytes();
    let layer = touched(&base);
    let usage = layer.memory_usage().unwrap().unwrap();
    let report = report(&layer, delta());

    assert_eq!(report.scope, MemoryReportScope::Delta);
    assert_eq!(
        labels(&report),
        set(&[
            ("vertex", "person", 20),
            ("vertex", "new", 10),
            ("edge", "linked", 10),
        ])
    );
    assert_eq!(
        rows(&report),
        set(&[
            ("person", "name", 20),
            ("person", "age", 20),
            ("person", "score", 20),
            ("new", "tag", 10),
            ("linked", "w", 10),
        ])
    );
    assert_eq!(report.visited_elements, 40);
    assert!(!report.sampled);

    // The structure: the layer's own figures, the base beside them.
    let structure = report.structure.unwrap();
    assert_eq!(structure.totals, usage);
    assert_eq!(report.estimated_total_bytes(), Some(usage.total_bytes()));
    assert_eq!(report.shared_bytes, base_total);
    assert_eq!(report.shared_bytes, usage.shared_bytes);
    assert_eq!(structure.base_stubs, 30, "20 images and 10 linked people");
    assert_eq!(
        structure.vertex_slots.live, 40,
        "10 added vertices and 30 stubs"
    );
    assert_eq!(structure.edge_slots.live, 10);
    assert_eq!(
        (
            structure.removed_base_vertices,
            structure.removed_base_edges
        ),
        (0, 10)
    );
    // The rows account for the layer's property bytes exactly, as for a `TraversalGraph`.
    assert_eq!(report.property_bytes(), usage.property_bytes);
    assert!(report.property_bytes() < base_total / 50, "{report:?}");
    // The base is one component of the process, beside the layer's own.
    let graph = report.component("graph", None).unwrap();
    assert_eq!(graph.bytes, usage.total_bytes());
    let shared = report.component("graph", Some("shared base")).unwrap();
    assert_eq!(shared.bytes, base_total);

    // The value and text forms carry the scope and the shared line.
    let json = report.to_json();
    for key in [
        r#""scope":"delta""#,
        &format!(r#""shared_bytes":{base_total}"#),
        r#""removed_base_edges":10"#,
        r#""base_stubs":30"#,
    ] {
        assert!(json.contains(key), "{key} in {json}");
    }
    let text = report.to_string();
    assert!(text.contains("shared base"), "{text}");
    assert!(text.contains("Scope: delta"), "{text}");
    assert!(text.contains("10 edges removed"), "{text}");
}

/// The default scope walks everything the layer shows (the report is the same as on the graph
/// it shows); the structure stays the layer's own and the base `shared_bytes`.
#[test]
fn the_default_scope_reports_everything_the_layer_shows() {
    let base = base();
    let base_total = base.memory_usage().unwrap().unwrap().total_bytes();
    let layer = touched(&base);
    let report = report(&layer, MemoryReportRequest::new());
    assert_eq!(report.scope, MemoryReportScope::All);
    assert_eq!(
        labels(&report),
        set(&[
            ("vertex", "person", 2_000),
            ("vertex", "item", 1_000),
            ("vertex", "new", 10),
            ("edge", "bought", 1_990),
            ("edge", "linked", 10),
        ])
    );
    assert_eq!(report.shared_bytes, base_total);
    assert_eq!(
        report.structure.unwrap().totals,
        layer.memory_usage().unwrap().unwrap()
    );
}

/// An untouched layer owns about nothing: no rows, no live slots, its dirty bits only; the base
/// is all shared. A base index costs the layer nothing; an index the layer defines shows its
/// owned bytes in the report's index rows and in the layer's own total.
#[test]
fn an_untouched_layer_owns_nothing_but_an_index_it_defines() {
    let base = base();
    let base_total = base.memory_usage().unwrap().unwrap().total_bytes();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let untouched = report(&layer, delta());
    assert!(untouched.properties.is_empty());
    assert!(untouched.labels.is_empty());
    assert_eq!(untouched.visited_elements, 0);
    assert_eq!(untouched.property_bytes(), 0);
    let structure = untouched.structure.unwrap();
    assert_eq!(structure.vertex_slots.live + structure.edge_slots.live, 0);
    assert_eq!(structure.base_stubs, 0);
    assert!(
        untouched.estimated_total_bytes().unwrap() < 1_024,
        "{structure:?}"
    );
    assert_eq!(untouched.shared_bytes, base_total);
    let inherited = untouched
        .indexes
        .iter()
        .find(|index| index.name == "person_age")
        .unwrap();
    assert_eq!(inherited.stats.bytes, 0, "the base's index is shared");

    let mut definitions: Vec<Definition> = layer.definitions().unwrap().cloned().collect();
    definitions.push(Definition::property_index(
        "item_sku",
        PropertyIndexDefinition::new(ElementKind::Vertex, "item", "sku"),
    ));
    layer.set_definitions(definitions).unwrap();
    let indexed = report(&layer, delta());
    let owned = indexed
        .indexes
        .iter()
        .find(|index| index.name == "item_sku")
        .unwrap();
    assert_eq!(owned.stats.entries, 1_000);
    assert!(owned.stats.bytes > 0, "the layer owns the index it defines");
    let totals = indexed.structure.unwrap().totals;
    assert_eq!(totals.property_index_bytes, owned.stats.bytes);
    assert!(indexed.estimated_total_bytes().unwrap() >= owned.stats.bytes);
    assert_eq!(indexed.shared_bytes, base_total);
    // Still no rows: defining an index changes no element.
    assert!(indexed.properties.is_empty());
}

/// Sampling with an element cap is computed over the layer's own elements, not the base's.
#[test]
fn a_sampled_delta_report_samples_the_layer_own_elements() {
    let base = base();
    let layer = touched(&base);
    let sampled = report(&layer, delta().with_max_elements(10));
    // 40 own elements, at most 10 read: every 4th per label.
    assert_eq!(sampled.sample_every, 4);
    assert!(sampled.sampled);
    assert_eq!(
        labels(&sampled),
        set(&[
            ("vertex", "person", 20),
            ("vertex", "new", 10),
            ("edge", "linked", 10),
        ])
    );
}

/// A plain graph owns all it shows: the delta scope reports the same rows and structure as the
/// default, and nothing is shared.
#[test]
fn a_plain_graph_reports_the_same_in_both_scopes() {
    let graph = TraversalGraph::tinkerpop_modern();
    let all = report(&graph, MemoryReportRequest::new());
    let own = report(&graph, delta());
    assert_eq!(own.scope, MemoryReportScope::Delta);
    assert_eq!(own.properties, all.properties);
    assert_eq!(own.labels, all.labels);
    assert_eq!(own.structure, all.structure);
    assert_eq!((own.shared_bytes, all.shared_bytes), (0, 0));
    assert_eq!(own.property_bytes(), all.property_bytes());
    assert!(own.component("graph", Some("shared base")).is_none());
    let structure = own.structure.unwrap();
    assert_eq!(
        (
            structure.base_stubs,
            structure.removed_base_vertices,
            structure.removed_base_edges
        ),
        (0, 0, 0)
    );
}

#[cfg(feature = "script")]
mod dsl {
    use std::collections::HashMap;

    use graphersal::storage::IntoAnyGraph;

    use super::*;

    fn render(graph: impl IntoAnyGraph, script: &str) -> Result<String, String> {
        crate::script_host::eval_value_with_limits(
            graph,
            &format!("({{ {script} }}).to_string()"),
            HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
    }

    /// `g.memory_report(#{scope: "delta"})` on a layer and on a plain graph; a bad scope fails
    /// naming the two values.
    #[test]
    fn the_dsl_scope_option() {
        let base = base();
        let base_total = base.memory_usage().unwrap().unwrap().total_bytes();
        let layer = Arc::new(Graph::new(touched(&base)));
        assert_eq!(
            render(
                Arc::clone(&layer),
                r#"let r = g.memory_report(#{scope: "delta"});
                   [r.scope, r.shared_bytes, r.labels.len(), r.structure.removed_base_edges]"#,
            )
            .unwrap(),
            format!(r#"["delta", {base_total}, 3, 10]"#)
        );
        assert_eq!(
            render(
                Arc::clone(&layer),
                "let r = g.memory_report(); [r.scope, r.labels.len()]"
            )
            .unwrap(),
            r#"["all", 5]"#
        );
        let plain = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
        assert_eq!(
            render(
                Arc::clone(&plain),
                r#"let r = g.memory_report(#{scope: "delta"});
                   [r.scope, r.shared_bytes, r.properties.len(), r.structure.totals.shared_bytes]"#,
            )
            .unwrap(),
            r#"["delta", 0, 6, 0]"#
        );
        assert_eq!(
            render(Arc::clone(&plain), r#"g.memory_report(#{scope: ()}).scope"#).unwrap(),
            "all"
        );
        for script in [
            r#"g.memory_report(#{scope: "base"})"#,
            r#"g.memory_report(#{scope: 1})"#,
        ] {
            let err = render(Arc::clone(&plain), script).unwrap_err();
            assert!(
                err.contains(r#"scope as "all" or "delta""#),
                "{script}: {err}"
            );
        }
        let err = render(Arc::clone(&plain), "g.memory_report(5)").unwrap_err();
        assert!(err.contains("scope?"), "{err}");
    }
}
