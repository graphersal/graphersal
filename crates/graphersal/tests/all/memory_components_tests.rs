//! The process side of the memory report (book page "Memory Usage", section "Where the process
//! memory goes"): the components a report names (the graph, a Store's buffers and in-memory
//! files, a Store's live graph beside a view, a host's own source), their sum, the value form
//! and the DSL.
//!
//! Sources are process-wide and tests run in parallel: other tests' stores and sources may be
//! listed too, so these tests look their own components up by name and detail.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{
    AnyGraph, Graph, MemoryComponent, MemoryComponents, MemoryReportRequest, MemorySource,
    register_memory_source,
};

struct Fixed(&'static str, u64);

impl MemorySource for Fixed {
    fn memory_components(&self, components: &mut MemoryComponents) {
        components.push(MemoryComponent::exact(self.0, self.1).with_detail("test"));
    }
}

fn report(graph: &Graph) -> graphersal::storage::MemoryReport {
    AnyGraph::memory_report(graph, &MemoryReportRequest::new()).unwrap()
}

#[test]
fn the_graph_is_a_component_and_the_components_add_up() {
    let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    let report = report(&graph);
    let part = report
        .component("graph", None)
        .expect("the graph component");
    assert_eq!(Some(part.bytes), report.estimated_total_bytes());
    assert!(!part.exact);
    let sum: u64 = report.components.iter().map(|c| c.bytes).sum();
    assert_eq!(report.attributed_bytes(), sum);
    // No tracking allocator in this test binary: nothing to attribute against.
    assert_eq!(report.tracked_live_bytes, None);
    assert_eq!(report.unattributed_bytes(), None);
}

#[test]
fn a_registered_source_is_asked_while_it_lives() {
    let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    let source: Arc<dyn MemorySource> = Arc::new(Fixed("components_test_cache", 12_345));
    register_memory_source(&source);
    let with = report(&graph);
    let part = with
        .component("components_test_cache", Some("test"))
        .expect("the source's component");
    assert_eq!((part.bytes, part.exact), (12_345, true));
    // The bare storage path asks the sources too.
    let bare = graph
        .read()
        .traversal()
        .memory_report(&MemoryReportRequest::new())
        .unwrap();
    assert!(bare.component("components_test_cache", None).is_some());
    drop(source);
    let without = report(&graph);
    assert!(without.component("components_test_cache", None).is_none());
}

#[test]
fn a_host_adds_a_part_it_alone_knows() {
    let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    let mut report = report(&graph);
    let before = report.attributed_bytes();
    report.add_component(MemoryComponent::estimated("host_buffer", 1000));
    assert_eq!(report.attributed_bytes(), before + 1000);
    assert!(!report.component("host_buffer", None).unwrap().exact);
}

/// The process's parts without a report: what a report adds beside the graph, never the graph
/// itself, a registered source while it lives.
#[test]
fn the_process_components_come_without_a_report() {
    let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    let source: Arc<dyn MemorySource> = Arc::new(Fixed("components_test_direct", 777));
    register_memory_source(&source);
    let parts = AnyGraph::process_memory_components(&graph).unwrap();
    assert!(
        parts
            .iter()
            .all(|c| c.name != "graph" && c.name != "workload_statistics")
    );
    let part = parts
        .iter()
        .find(|c| c.name == "components_test_direct")
        .expect("the source's component");
    assert_eq!(
        (part.bytes, part.exact, part.detail.as_deref()),
        (777, true, Some("test"))
    );
    drop(source);
    let parts = AnyGraph::process_memory_components(&graph).unwrap();
    assert!(parts.iter().all(|c| c.name != "components_test_direct"));
}

#[cfg(feature = "persist")]
mod store {
    use super::*;
    use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreOptions};

    fn location(store: &Store) -> String {
        store.location()
    }

    #[test]
    fn a_store_in_memory_counts_its_files_and_buffers() {
        let dir = MemDir::named("memory-components-files");
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        graphersal::script::eval(
            store.graph().clone(),
            "g.add_v(\"p\").property(\"n\", 1).iterate()",
        )
        .unwrap();
        let report = report(store.graph());
        let at = location(&store);
        let files = report
            .component("store_files", Some(&at))
            .expect("the store's files in memory");
        assert!(files.exact);
        assert_eq!(files.bytes, dir.total_bytes());
        assert!(files.bytes > 0);
        assert!(report.component("store", Some(&at)).is_some());
        // The report is about the store's own live graph: it is not listed twice.
        assert!(report.component("store_graph", Some(&at)).is_none());
        store.close().unwrap();
    }

    #[test]
    fn a_view_lists_the_live_graph_of_its_store() {
        let dir = MemDir::named("memory-components-view");
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        for i in 0..50 {
            graphersal::script::eval(
                store.graph().clone(),
                &format!("g.add_v(\"p\").property(\"n\", {i}).iterate()"),
            )
            .unwrap();
        }
        let view = Arc::new(
            store
                .read_only_at(RecoveryTarget::CommitSeq(10))
                .unwrap()
                .into_graph(),
        );
        let report = report(&view);
        let at = location(&store);
        let live = report
            .component("store_graph", Some(&at))
            .expect("the live graph beside the view");
        let live_estimate = AnyGraph::memory_usage(&**store.graph()).unwrap().unwrap();
        assert!(!live.exact);
        assert!(live.bytes >= live_estimate.total_bytes(), "{live:?}");
        // The view itself is the `graph` component, smaller than the live graph.
        assert!(report.component("graph", None).unwrap().bytes < live.bytes);
        store.close().unwrap();
    }

    /// Without the walk too: a view's process parts name its store's live graph, the live
    /// graph's own do not.
    #[test]
    fn the_process_components_of_a_view_name_the_live_graph() {
        let dir = MemDir::named("memory-components-view-direct");
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        for i in 0..20 {
            graphersal::script::eval(
                store.graph().clone(),
                &format!("g.add_v(\"p\").property(\"n\", {i}).iterate()"),
            )
            .unwrap();
        }
        let view = store
            .read_only_at(RecoveryTarget::CommitSeq(5))
            .unwrap()
            .into_graph();
        let at = location(&store);
        let named = |parts: &[MemoryComponent], name: &str| {
            parts
                .iter()
                .find(|c| c.name == name && c.detail.as_deref() == Some(at.as_str()))
                .cloned()
        };
        let parts = AnyGraph::process_memory_components(&view).unwrap();
        let live = named(&parts, "store_graph").expect("the live graph beside the view");
        let live_estimate = AnyGraph::memory_usage(&**store.graph()).unwrap().unwrap();
        assert!(live.bytes >= live_estimate.total_bytes(), "{live:?}");
        assert!(named(&parts, "store").is_some());
        let own = AnyGraph::process_memory_components(&**store.graph()).unwrap();
        assert!(named(&own, "store_graph").is_none());
        store.close().unwrap();
    }

    #[test]
    fn a_closed_store_is_no_longer_listed() {
        let dir = MemDir::named("memory-components-closed");
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        let at = location(&store);
        let graph = Graph::new(TraversalGraph::tinkerpop_modern());
        assert!(report(&graph).component("store", Some(&at)).is_some());
        store.close().unwrap();
        assert!(report(&graph).component("store", Some(&at)).is_none());
    }
}

#[test]
fn the_value_form_and_the_dsl_carry_the_components() {
    let graph = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
    let report = report(&graph);
    let json = report.to_json();
    assert!(json.contains(r#""components":[{"#), "{json}");
    assert!(json.contains(r#""name":"graph""#), "{json}");
    assert!(json.contains(r#""attributed_bytes":"#), "{json}");
    let out = graphersal::script::eval(
        graph.clone(),
        "let r = g.memory_report(); \
         let part = r.components.filter(|c| c.name == \"graph\")[0]; \
         [part.bytes == r.estimated_total_bytes, part.exact, r.attributed_bytes >= part.bytes]",
    )
    .unwrap();
    assert_eq!(out, "true\nfalse\ntrue");
    // The text form names the parts.
    assert!(report.to_string().contains("Process memory: where it goes"));
}
