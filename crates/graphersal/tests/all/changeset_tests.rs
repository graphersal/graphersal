//! Change capture (`book/src/users_guide/transactions.md`): the `ChangeSet` of a committed unit,
//! built from the undo log while a commit hook is registered.
//!
//! One test per mutation kind (before and after values), the cascade order of a vertex drop,
//! jpath writes, schema changes in the stream, `compact()`, `commit_seq`, units that change
//! nothing, the traversal level, and a replay check: applying the captured mutations (raw and
//! compacted) to a copy of the starting graph gives the same graph.

use std::cell::RefCell;

use graphersal::changes::{ChangeSet, Mutation};
use graphersal::changes::{CommitHook, CommitMetadata, ElementRef, HookError, RollbackReason};
use graphersal::prelude::*;
use graphersal::storage::ElementKind;

// ---------------------------------------------------------------------------------------------
// Recording hook
// ---------------------------------------------------------------------------------------------

thread_local! {
    /// The change sets the recording hook of this test's graph received (hooks run on the
    /// committing thread, and every test runs on its own thread).
    static RECORDED: RefCell<Vec<ChangeSet>> = const { RefCell::new(Vec::new()) };
}

/// A commit hook that records every change set it accepts.
struct Recorder;

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        RECORDED.with(|recorded| recorded.borrow_mut().push(changes.clone()));
        Ok(())
    }
}

/// Turns change capture on by registering [`Recorder`], and reads what it recorded.
trait Record {
    fn record_changes(&mut self);
    fn recorded(&mut self) -> Vec<ChangeSet>;
}

impl<S: GraphStorage> Record for S {
    fn record_changes(&mut self) {
        self.register_commit_hook(Box::new(Recorder))
            .expect("TraversalGraph supports hooks");
    }

    fn recorded(&mut self) -> Vec<ChangeSet> {
        RECORDED.with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
    }
}

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut out = PropertyMap::default();
    for (key, value) in entries {
        out.insert(*key, value.clone());
    }
    out
}

fn path(text: &str) -> JsonPath {
    text.parse().expect("valid jpath")
}

fn v(graph: &TraversalGraph, id: &str) -> graphersal::storage::VertexHandle {
    graph.get_vertex_by_id(id).expect("vertex")
}

fn e(graph: &TraversalGraph, id: &str) -> graphersal::storage::EdgeHandle {
    graph.get_edge_by_id(id).expect("edge")
}

/// The modern graph with capture on.
fn modern() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.record_changes();
    graph
}

/// Runs `change` as one unit and returns the one change set it produced.
fn capture(
    graph: &mut TraversalGraph,
    change: impl FnOnce(&mut TraversalGraph) -> Result<(), GraphError>,
) -> ChangeSet {
    let mark = graph.begin_unit().unwrap();
    change(graph).expect("the change succeeds");
    graph.commit_unit(mark).expect("commit");
    let mut sets = graph.recorded();
    assert_eq!(sets.len(), 1, "one committed unit, one change set");
    sets.remove(0)
}

/// A compact, comparable text form of a mutation for order assertions.
fn op(mutation: &Mutation) -> String {
    match mutation {
        Mutation::AddVertex { id, .. } => format!("+v{id}"),
        Mutation::DropVertex { id, .. } => format!("-v{id}"),
        Mutation::AddEdge { id, .. } => format!("+e{id}"),
        Mutation::DropEdge { id, .. } => format!("-e{id}"),
        Mutation::SetProperty { element, key, .. } => format!("set {}.{key}", element.id),
        Mutation::RemoveProperty { element, key, .. } => format!("rm {}.{key}", element.id),
        Mutation::AddLabel { id, label, .. } => format!("+l{id}:{label}"),
        Mutation::RemoveLabel { id, label, .. } => format!("-l{id}:{label}"),
        Mutation::SetSchema { .. } => "schema".to_string(),
        _ => "unknown".to_string(),
    }
}

fn ops(set: &ChangeSet) -> Vec<String> {
    set.mutations().iter().map(op).collect()
}

/// `(id, labels, sorted properties)` per vertex and `(id, label, out, in, sorted properties)`
/// per edge, both sorted by id: the content a replay must reproduce (property order and
/// iteration order are not part of it).
type Content = (
    Vec<(String, Vec<String>, Vec<(String, ElementProperty)>)>,
    Vec<(
        String,
        Option<String>,
        String,
        String,
        Vec<(String, ElementProperty)>,
    )>,
    Option<GraphSchema>,
);

fn sorted_props(element: Option<&PropertyMap>) -> Vec<(String, ElementProperty)> {
    let mut out: Vec<_> = element
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn content(graph: &TraversalGraph) -> Content {
    let mut vertices: Vec<_> = GraphStorage::vertices(graph)
        .map(|h| {
            (
                graph.vertex_id(h).unwrap().to_string(),
                graph
                    .vertex_labels(h)
                    .iter()
                    .map(|l| l.to_string())
                    .collect(),
                sorted_props(graph.vertex_properties(h)),
            )
        })
        .collect();
    vertices.sort_by(|a, b| a.0.cmp(&b.0));
    let mut edges: Vec<_> = GraphStorage::edges(graph)
        .map(|h| {
            let (from, to) = graph.edge_endpoints(h).unwrap();
            (
                graph.edge_id(h).unwrap().to_string(),
                graph.edge_label(h).map(str::to_string),
                graph.vertex_id(from).unwrap().to_string(),
                graph.vertex_id(to).unwrap().to_string(),
                sorted_props(graph.edge_properties(h)),
            )
        })
        .collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    (vertices, edges, graph.get_schema().unwrap())
}

/// Applies `mutations` through the storage API, checking every before-value against the graph
/// it is applied to (a test-side stand-in for task 04's `apply`).
fn replay(graph: &mut TraversalGraph, mutations: &[Mutation]) {
    for mutation in mutations {
        match mutation {
            Mutation::AddVertex {
                id,
                labels,
                properties,
                ..
            } => {
                assert!(graph.get_vertex_by_id(id).is_none(), "{id} is free");
                let labels = labels.iter().map(|l| l.as_str().into()).collect();
                graph
                    .add_vertex(Some(id.as_str().into()), labels, properties.clone())
                    .unwrap();
            }
            Mutation::DropVertex { id, before, .. } => {
                let h = v(graph, id);
                let labels: Vec<String> = graph
                    .vertex_labels(h)
                    .iter()
                    .map(|l| l.to_string())
                    .collect();
                assert_eq!(labels, before.labels, "labels of {id} before its drop");
                assert_eq!(
                    graph.vertex_properties(h),
                    Some(&before.properties),
                    "properties of {id} before its drop"
                );
                assert_eq!(
                    graph.out_edges(h).count() + graph.in_edges(h).count(),
                    0,
                    "the cascade was listed before the vertex drop"
                );
                graph.drop_vertex(h).unwrap();
            }
            Mutation::AddEdge {
                id,
                label,
                out_id,
                in_id,
                properties,
                ..
            } => {
                let (from, to) = (v(graph, out_id), v(graph, in_id));
                graph
                    .add_edge(
                        Some(id.as_str().into()),
                        label.as_deref().map(Into::into),
                        from,
                        to,
                        properties.clone(),
                    )
                    .unwrap();
            }
            Mutation::DropEdge { id, before, .. } => {
                let h = e(graph, id);
                assert_eq!(
                    graph.edge_label(h).map(str::to_string),
                    before.label,
                    "label of {id}"
                );
                let (from, to) = graph.edge_endpoints(h).unwrap();
                assert_eq!(graph.vertex_id(from), Some(before.out_id.as_str()));
                assert_eq!(graph.vertex_id(to), Some(before.in_id.as_str()));
                assert_eq!(
                    sorted_props(graph.edge_properties(h)),
                    sorted_props(Some(&before.properties))
                );
                graph.drop_edge(h).unwrap();
            }
            Mutation::SetProperty {
                element,
                key,
                before,
                after,
                ..
            } => match element.kind {
                ElementKind::Vertex => {
                    let h = v(graph, &element.id);
                    let current = graph.vertex_properties(h).and_then(|p| p.get(key.as_str()));
                    assert_eq!(current, before.as_ref(), "before of {}.{key}", element.id);
                    graph
                        .set_vertex_property(h, key.as_str().into(), after.clone())
                        .unwrap();
                }
                ElementKind::Edge => {
                    let h = e(graph, &element.id);
                    let current = graph.edge_properties(h).and_then(|p| p.get(key.as_str()));
                    assert_eq!(current, before.as_ref(), "before of {}.{key}", element.id);
                    graph
                        .set_edge_property(h, key.as_str().into(), after.clone())
                        .unwrap();
                }
            },
            Mutation::RemoveProperty {
                element,
                key,
                before,
                ..
            } => match element.kind {
                ElementKind::Vertex => {
                    let h = v(graph, &element.id);
                    let current = graph.vertex_properties(h).and_then(|p| p.get(key.as_str()));
                    assert_eq!(current, Some(before));
                    graph.drop_vertex_property(key, h).unwrap();
                }
                ElementKind::Edge => {
                    let h = e(graph, &element.id);
                    let current = graph.edge_properties(h).and_then(|p| p.get(key.as_str()));
                    assert_eq!(current, Some(before));
                    graph.drop_edge_property(key, h).unwrap();
                }
            },
            Mutation::AddLabel { id, label, .. } => {
                let h = v(graph, id);
                assert!(graph.add_vertex_label(h, label.as_str().into()).unwrap());
            }
            Mutation::RemoveLabel { id, label, .. } => {
                let h = v(graph, id);
                assert!(graph.remove_vertex_label(h, label).unwrap());
            }
            Mutation::SetSchema { before, after, .. } => {
                let current = graph.get_schema().unwrap().unwrap_or_default();
                assert_eq!(&current, before.as_ref());
                GraphStorage::set_schema(graph, (**after).clone()).unwrap();
            }
            _ => panic!("unknown mutation {mutation:?}"),
        }
    }
}

/// Runs `change` on the modern graph as one captured unit, then checks that replaying the raw
/// and the compacted change set on a fresh modern graph reproduces the result. Returns the
/// change set.
fn capture_and_replay(
    change: impl FnOnce(&mut TraversalGraph) -> Result<(), GraphError>,
) -> ChangeSet {
    let mut graph = modern();
    let set = capture(&mut graph, change);
    let expected = content(&graph);
    for (name, mutations) in [
        ("raw", set.mutations().to_vec()),
        ("compacted", set.compact().into_mutations()),
    ] {
        let mut replica = TraversalGraph::tinkerpop_modern();
        replay(&mut replica, &mutations);
        assert_eq!(content(&replica), expected, "{name} replay");
    }
    set
}

// ---------------------------------------------------------------------------------------------
// One test per mutation kind
// ---------------------------------------------------------------------------------------------

#[test]
fn add_vertex_carries_labels_and_stored_properties() {
    let set = capture_and_replay(|g| {
        g.add_vertex(
            Some("100".into()),
            vec!["person".into(), "admin".into()],
            props(&[("name", "x".into()), ("age", 3.into())]),
        )?;
        Ok(())
    });
    assert_eq!(set.len(), 1);
    match &set.mutations()[0] {
        Mutation::AddVertex {
            id,
            labels,
            properties,
            ..
        } => {
            assert_eq!(id, "100");
            assert_eq!(labels, &["person", "admin"]);
            assert_eq!(
                properties,
                &props(&[("name", "x".into()), ("age", 3.into())])
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn add_vertex_records_the_auto_id() {
    let mut graph = modern();
    let set = capture(&mut graph, |g| {
        g.add_vertex(None, vec![], PropertyMap::default())?;
        Ok(())
    });
    let Mutation::AddVertex { id, .. } = &set.mutations()[0] else {
        panic!("an add");
    };
    assert!(graph.get_vertex_by_id(id).is_some());
}

#[test]
fn add_edge_carries_label_endpoints_and_properties() {
    let set = capture_and_replay(|g| {
        let (a, b) = (v(g, "1"), v(g, "6"));
        g.add_edge(
            Some("50".into()),
            Some("likes".into()),
            a,
            b,
            props(&[("since", 2020.into())]),
        )?;
        Ok(())
    });
    match &set.mutations()[0] {
        Mutation::AddEdge {
            id,
            label,
            out_id,
            in_id,
            properties,
            ..
        } => {
            assert_eq!(
                (
                    id.as_str(),
                    label.as_deref(),
                    out_id.as_str(),
                    in_id.as_str()
                ),
                ("50", Some("likes"), "1", "6")
            );
            assert_eq!(properties, &props(&[("since", 2020.into())]));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn drop_vertex_lists_its_cascaded_edges_first() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.drop_vertex(marko)
    });
    let ops = ops(&set);
    assert_eq!(ops.len(), 4);
    assert_eq!(ops[3], "-v1", "the vertex drop comes after its edges");
    let mut edges = ops[..3].to_vec();
    edges.sort();
    assert_eq!(edges, ["-e0", "-e1", "-e2"]);
    match &set.mutations()[3] {
        Mutation::DropVertex { before, .. } => {
            assert_eq!(before.labels, ["person"]);
            assert_eq!(before.properties.get("name"), Some(&"marko".into()));
        }
        other => panic!("unexpected {other:?}"),
    }
    match &set.mutations()[0] {
        Mutation::DropEdge { before, .. } => {
            assert_eq!(before.out_id, "1");
            assert!(before.properties.contains_key("weight"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_self_loop_is_dropped_once() {
    let set = capture_and_replay(|g| {
        let josh = v(g, "4");
        g.add_edge(
            Some("loop".into()),
            Some("self".into()),
            josh,
            josh,
            PropertyMap::default(),
        )?;
        g.drop_vertex(josh)
    });
    let ops = ops(&set);
    assert_eq!(ops.first().map(String::as_str), Some("+eloop"));
    assert_eq!(ops.last().map(String::as_str), Some("-v4"));
    assert_eq!(ops.iter().filter(|o| *o == "-eloop").count(), 1);
    // josh: in 2, out 3 and 4, plus the loop.
    assert_eq!(ops.len(), 1 + 4 + 1);
}

#[test]
fn drop_edge_carries_its_before_image() {
    let set = capture_and_replay(|g| {
        let edge = e(g, "3");
        g.drop_edge(edge)
    });
    match set.mutations() {
        [Mutation::DropEdge { id, before, .. }] => {
            assert_eq!(id, "3");
            assert_eq!(before.label.as_deref(), Some("created"));
            assert_eq!((before.out_id.as_str(), before.in_id.as_str()), ("4", "3"));
            assert_eq!(before.properties.get("weight"), Some(&0.4.into()));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn vertex_property_writes_and_removals_carry_before_and_after() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "age".into(), 30.into())?; // overwrite
        g.set_vertex_property(marko, "nick".into(), "m".into())?; // new key
        g.set_vertex_property(marko, "gone".into(), ElementProperty::Null)?; // stored null
        g.drop_vertex_property("name", marko)?; // removal
        g.drop_vertex_property("missing", marko)?; // absent: nothing
        Ok(())
    });
    let m = set.mutations();
    assert_eq!(m.len(), 4);
    let expect_set = |m: &Mutation, k: &str, b: Option<ElementProperty>, a: ElementProperty| match m
    {
        Mutation::SetProperty {
            element,
            key,
            before,
            after,
            ..
        } => {
            assert_eq!(element, &ElementRef::vertex("1"));
            assert_eq!((key.as_str(), before, after), (k, &b, &a));
        }
        other => panic!("unexpected {other:?}"),
    };
    expect_set(&m[0], "age", Some(29.into()), 30.into());
    expect_set(&m[1], "nick", None, "m".into());
    expect_set(&m[2], "gone", None, ElementProperty::Null);
    match &m[3] {
        Mutation::RemoveProperty {
            element,
            key,
            before,
            ..
        } => {
            assert_eq!(element, &ElementRef::vertex("1"));
            assert_eq!((key.as_str(), before), ("name", &"marko".into()));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn edge_property_writes_and_removals_carry_before_and_after() {
    let set = capture_and_replay(|g| {
        let edge = e(g, "0");
        g.set_edge_property(edge, "weight".into(), 0.9.into())?;
        g.set_edge_property(edge, "note".into(), "n".into())?;
        g.drop_edge_property("note", edge)?;
        Ok(())
    });
    assert_eq!(ops(&set), ["set 0.weight", "set 0.note", "rm 0.note"]);
    match &set.mutations()[0] {
        Mutation::SetProperty {
            element,
            before,
            after,
            ..
        } => {
            assert_eq!(element, &ElementRef::edge("0"));
            assert_eq!(before, &Some(0.5.into()));
            assert_eq!(after, &0.9.into());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn label_changes_are_add_and_remove_label() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.add_vertex_label(marko, "admin".into())?;
        g.add_vertex_label(marko, "admin".into())?; // already there: nothing
        g.remove_vertex_label(marko, "person")?;
        Ok(())
    });
    assert_eq!(ops(&set), ["+l1:admin", "-l1:person"]);
}

#[test]
fn a_jpath_write_is_a_set_of_the_whole_top_level_value() {
    for mode in [SchemaMode::None, SchemaMode::Open] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let marko = v(&graph, "1");
        let mut meta = PropertyMap::default();
        meta.insert("k", 1.into());
        meta.insert("tags", ElementProperty::Array(vec!["a".into()]));
        graph
            .set_vertex_property(
                marko,
                "meta".into(),
                ElementProperty::Object(Box::new(meta.clone())),
            )
            .unwrap();
        graph
            .set_schema(GraphSchema::new().with_mode(mode))
            .unwrap();
        graph.record_changes();
        let set = capture(&mut graph, |g| {
            let marko = v(g, "1");
            g.set_vertex_property_path(marko, &path("meta.k"), 2.into())?;
            g.remove_vertex_property_path(marko, &path("meta.tags[0]"))?;
            g.remove_vertex_property_path(marko, &path("meta.nope"))?; // missing: nothing
            g.set_vertex_property_path(marko, &path("fresh.a"), 1.into())?;
            Ok(())
        });
        assert_eq!(
            ops(&set),
            ["set 1.meta", "set 1.meta", "set 1.fresh"],
            "{mode:?}"
        );
        let m = set.mutations();
        let mut after_k = meta.clone();
        after_k.insert("k", 2.into());
        let mut after_tags = after_k.clone();
        after_tags.insert("tags", ElementProperty::Array(vec![]));
        let obj = |e: &PropertyMap| ElementProperty::Object(Box::new(e.clone()));
        let check = |m: &Mutation, b: Option<ElementProperty>, a: ElementProperty| match m {
            Mutation::SetProperty { before, after, .. } => {
                assert_eq!(before, &b, "{mode:?}");
                assert_eq!(after, &a, "{mode:?}");
            }
            other => panic!("unexpected {other:?}"),
        };
        check(&m[0], Some(obj(&meta)), obj(&after_k));
        check(&m[1], Some(obj(&after_k)), obj(&after_tags));
        check(&m[2], None, obj(&props(&[("a", 1.into())])));
    }
}

#[test]
fn schema_changes_are_in_the_stream_in_order() {
    let set = capture_and_replay(|g| {
        g.add_vertex(Some("a".into()), vec!["n".into()], PropertyMap::default())?;
        GraphStorage::set_schema(g, GraphSchema::new().with_mode(SchemaMode::Open))?;
        g.add_vertex(Some("b".into()), vec!["n".into()], PropertyMap::default())?;
        Ok(())
    });
    assert_eq!(ops(&set), ["+va", "schema", "+vb"]);
    match &set.mutations()[1] {
        Mutation::SetSchema { before, after, .. } => {
            assert_eq!(before.mode, SchemaMode::None);
            assert_eq!(after.mode, SchemaMode::Open);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_mixed_unit_replays_raw_and_compacted() {
    capture_and_replay(|g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "age".into(), 1.into())?;
        g.add_vertex_label(marko, "x".into())?;
        let n = g.add_vertex(Some("n".into()), vec!["new".into()], PropertyMap::default())?;
        g.add_edge(
            Some("ne".into()),
            Some("to".into()),
            n,
            marko,
            PropertyMap::default(),
        )?;
        g.set_vertex_property(n, "p".into(), 1.into())?;
        g.drop_vertex(marko)?; // cascades ne and marko's own edges
        let marko = g.add_vertex(
            Some("1".into()),
            vec!["person".into()],
            PropertyMap::default(),
        )?;
        g.add_edge(
            Some("0".into()),
            Some("knows".into()),
            marko,
            n,
            PropertyMap::default(),
        )?;
        let josh = v(g, "4");
        g.set_vertex_property(josh, "age".into(), 33.into())?;
        g.drop_edge(e(g, "3"))?;
        Ok(())
    });
}

// ---------------------------------------------------------------------------------------------
// compact()
// ---------------------------------------------------------------------------------------------

#[test]
fn compact_folds_changes_of_an_added_vertex_into_the_add() {
    let set = capture_and_replay(|g| {
        let n = g.add_vertex(Some("n".into()), vec!["a".into()], PropertyMap::default())?;
        g.set_vertex_property(n, "p".into(), 1.into())?;
        g.set_vertex_property(n, "p".into(), 2.into())?;
        g.add_vertex_label(n, "b".into())?;
        Ok(())
    });
    let compact = set.compact();
    assert_eq!(compact.commit_seq(), set.commit_seq());
    match compact.mutations() {
        [
            Mutation::AddVertex {
                labels, properties, ..
            },
        ] => {
            assert_eq!(labels, &["a", "b"]);
            assert_eq!(properties, &props(&[("p", 2.into())]));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn compact_drops_an_element_added_and_dropped() {
    let set = capture_and_replay(|g| {
        let n = g.add_vertex(Some("n".into()), vec![], PropertyMap::default())?;
        let marko = v(g, "1");
        g.add_edge(Some("x".into()), None, n, marko, PropertyMap::default())?;
        g.drop_vertex(n)
    });
    assert_eq!(ops(&set), ["+vn", "+ex", "-ex", "-vn"]);
    assert!(set.compact().is_empty());
}

#[test]
fn compact_keeps_the_net_property_change() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "age".into(), 1.into())?;
        g.set_vertex_property(marko, "age".into(), 2.into())?;
        g.set_vertex_property(marko, "tmp".into(), 1.into())?;
        g.drop_vertex_property("tmp", marko)?; // set then removed: nothing
        g.set_vertex_property(marko, "name".into(), "x".into())?;
        g.set_vertex_property(marko, "name".into(), "marko".into())?; // back: nothing
        g.set_vertex_property(v(g, "2"), "age".into(), 27.into())?; // equal value: nothing
        g.drop_vertex_property("lang", v(g, "3"))?;
        Ok(())
    });
    let compact = set.compact();
    assert_eq!(ops(&compact), ["set 1.age", "rm 3.lang"]);
    match &compact.mutations()[0] {
        Mutation::SetProperty { before, after, .. } => {
            assert_eq!(before, &Some(29.into()));
            assert_eq!(after, &2.into());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn compact_restores_the_original_image_of_a_changed_then_dropped_vertex() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "age".into(), 1.into())?;
        g.drop_vertex_property("name", marko)?;
        g.add_vertex_label(marko, "x".into())?;
        g.set_edge_property(e(g, "0"), "weight".into(), 9.into())?;
        g.drop_vertex(marko)
    });
    let compact = set.compact();
    let mut edge_drops: Vec<_> = ops(&compact)[..3].to_vec();
    edge_drops.sort();
    assert_eq!(edge_drops, ["-e0", "-e1", "-e2"]);
    assert_eq!(ops(&compact)[3], "-v1");
    let original = TraversalGraph::tinkerpop_modern();
    match &compact.mutations()[3] {
        Mutation::DropVertex { before, .. } => {
            assert_eq!(before.labels, ["person"]);
            assert_eq!(
                sorted_props(Some(&before.properties)),
                sorted_props(original.vertex_properties(v(&original, "1")))
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    let e0 = compact
        .mutations()
        .iter()
        .find_map(|m| match m {
            Mutation::DropEdge { id, before, .. } if id == "0" => Some(before.clone()),
            _ => None,
        })
        .expect("drop of edge 0");
    assert_eq!(e0.properties.get("weight"), Some(&0.5.into()));
}

#[test]
fn compact_keeps_a_drop_and_a_re_add_of_the_same_id() {
    let set = capture_and_replay(|g| {
        g.drop_vertex(v(g, "2"))?;
        let vadas = g.add_vertex(
            Some("2".into()),
            vec!["robot".into()],
            PropertyMap::default(),
        )?;
        g.set_vertex_property(vadas, "p".into(), 1.into())?;
        Ok(())
    });
    let compact = set.compact();
    assert_eq!(ops(&compact), ["-e0", "-v2", "+v2"]);
    match &compact.mutations()[2] {
        Mutation::AddVertex {
            labels, properties, ..
        } => {
            assert_eq!(labels, &["robot"]);
            assert_eq!(properties, &props(&[("p", 1.into())]));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn compact_drops_a_label_added_and_removed_again() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.add_vertex_label(marko, "x".into())?;
        g.remove_vertex_label(marko, "x")?;
        g.add_vertex_label(marko, "y".into())?;
        Ok(())
    });
    assert_eq!(ops(&set.compact()), ["+l1:y"]);
}

#[test]
fn compact_treats_schema_changes_as_barriers() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "age".into(), 1.into())?;
        GraphStorage::set_schema(g, GraphSchema::new().with_mode(SchemaMode::Open))?;
        GraphStorage::set_schema(g, GraphSchema::new().with_mode(SchemaMode::None))?; // back
        g.set_vertex_property(marko, "age".into(), 2.into())?;
        GraphStorage::set_schema(g, GraphSchema::new().with_mode(SchemaMode::Open))?;
        g.add_vertex(Some("n".into()), vec![], PropertyMap::default())?;
        Ok(())
    });
    // The first two schema changes merge and cancel out; the segments around them stay apart.
    assert_eq!(
        ops(&set.compact()),
        ["set 1.age", "set 1.age", "schema", "+vn"]
    );
}

#[test]
fn compact_orders_a_segment_for_replay() {
    let set = capture_and_replay(|g| {
        let n = g.add_vertex(Some("n".into()), vec![], PropertyMap::default())?;
        let josh = v(g, "4");
        g.add_edge(Some("x".into()), None, n, josh, PropertyMap::default())?;
        g.set_vertex_property(josh, "age".into(), 1.into())?;
        g.set_edge_property(e(g, "5"), "weight".into(), 1.into())?;
        g.drop_vertex(v(g, "6"))?;
        Ok(())
    });
    assert_eq!(
        ops(&set.compact()),
        ["-e5", "-v6", "+vn", "set 4.age", "+ex"]
    );
}

// ---------------------------------------------------------------------------------------------
// commit_seq and units that change nothing
// ---------------------------------------------------------------------------------------------

#[test]
fn an_empty_unit_produces_no_change_set_and_no_commit() {
    let mut graph = modern();
    let seq = graph.last_commit_seq();
    let mark = graph.begin_unit().unwrap();
    graph.commit_unit(mark).expect("commit");
    // A read-only traversal, a drop over an empty set, removing an absent property.
    graph.traversal_mut().v(None::<()>).count().next().unwrap();
    graph.traversal_mut().v("nope").drop().to_list().unwrap();
    let mark = graph.begin_unit().unwrap();
    let marko = v(&graph, "1");
    graph.drop_vertex_property("missing", marko).unwrap();
    graph
        .remove_vertex_property_path(marko, &path("name.x"))
        .unwrap();
    graph.commit_unit(mark).expect("commit");
    assert!(graph.recorded().is_empty());
    assert_eq!(graph.last_commit_seq(), seq);
}

#[test]
fn a_rolled_back_unit_produces_nothing() {
    let mut graph = modern();
    let mark = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("x".into()), vec![], PropertyMap::default())
        .unwrap();
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    let failed = graph
        .traversal_mut()
        .add_v("n")
        .constant("x")
        .as_number(GType::LONG)
        .to_list();
    assert!(failed.is_err());
    assert!(graph.recorded().is_empty());
    assert_eq!(graph.last_commit_seq(), 0);
}

#[test]
fn commit_seq_is_monotonic_and_advances_also_without_capture() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    assert_eq!(graph.last_commit_seq(), 0);
    graph.traversal_mut().add_v("a").to_list().unwrap();
    assert_eq!(graph.last_commit_seq(), 1);
    assert!(graph.recorded().is_empty(), "no hook: nothing captured");
    graph.record_changes();
    graph.traversal_mut().add_v("b").to_list().unwrap();
    graph.traversal_mut().v(None::<()>).count().next().unwrap();
    graph.traversal_mut().add_v("c").to_list().unwrap();
    let seqs: Vec<u64> = graph.recorded().iter().map(ChangeSet::commit_seq).collect();
    assert_eq!(seqs, [2, 3]);
    assert_eq!(graph.last_commit_seq(), 3);
}

#[test]
fn nested_units_produce_one_change_set_without_rolled_back_savepoints() {
    let mut graph = modern();
    let outer = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("a".into()), vec![], PropertyMap::default())
        .unwrap();
    let inner = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("b".into()), vec![], PropertyMap::default())
        .unwrap();
    graph
        .rollback_unit(inner, &RollbackReason::failed(&"test"))
        .unwrap();
    let inner = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("c".into()), vec![], PropertyMap::default())
        .unwrap();
    graph.commit_unit(inner).expect("commit");
    assert!(
        graph.recorded().is_empty(),
        "nothing before the outer commit"
    );
    graph.commit_unit(outer).expect("commit");
    let sets = graph.recorded();
    assert_eq!(sets.len(), 1);
    assert_eq!(ops(&sets[0]), ["+va", "+vc"]);
    assert_eq!(sets[0].commit_seq(), 1);
    assert_eq!(sets[0].metadata(), &CommitMetadata::default());
}

// ---------------------------------------------------------------------------------------------
// Traversal level
// ---------------------------------------------------------------------------------------------

#[test]
fn a_traversal_is_one_change_set() {
    let mut graph = modern();
    graph
        .traversal_mut()
        .v("1")
        .property("age", 30)
        .out("knows")
        .drop()
        .to_list()
        .unwrap();
    let sets = graph.recorded();
    assert_eq!(sets.len(), 1);
    let ops = ops(&sets[0]);
    assert_eq!(ops[0], "set 1.age");
    assert!(ops.contains(&"-v2".to_string()) && ops.contains(&"-v4".to_string()));
    assert_eq!(ops.len(), 1 + 2 + 4, "two vertices and their four edges");
}

#[cfg(feature = "script")]
#[test]
fn a_rhai_statement_is_one_change_set() {
    use std::sync::Arc;
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    graph.write().record_changes();
    let _count = crate::script_host::eval_value(
        graph.clone(),
        r#"g.addV("a").property("k", 1).toList(); g.V("1").property("age", 1).toList(); g.V().count().next()"#,
    )
    .unwrap();
    let sets = graph.write().recorded();
    assert_eq!(sets.len(), 2);
    // `property()` right after `addV()` is folded into the add (AddPropertyFold).
    match sets[0].mutations() {
        [
            Mutation::AddVertex {
                labels, properties, ..
            },
        ] => {
            assert_eq!(labels, &["a"]);
            assert_eq!(properties, &props(&[("k", 1.into())]));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(ops(&sets[1]), ["set 1.age"]);
}

// ---------------------------------------------------------------------------------------------
// serde
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "serde")]
#[test]
fn a_change_set_round_trips_through_json() {
    let set = capture_and_replay(|g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "f".into(), 1.0.into())?;
        g.set_vertex_property(marko, "nil".into(), ElementProperty::Null)?;
        g.set_vertex_property(marko, "nil".into(), 2.into())?; // before: a stored null
        let mut nested = PropertyMap::default();
        nested.insert(
            "a",
            ElementProperty::Array(vec![1.into(), "x".into(), true.into()]),
        );
        g.set_vertex_property(
            marko,
            "obj".into(),
            ElementProperty::Object(Box::new(nested)),
        )?;
        g.add_vertex_label(marko, "x".into())?;
        g.remove_vertex_label(marko, "x")?;
        GraphStorage::set_schema(g, GraphSchema::new().with_mode(SchemaMode::Open))?;
        g.drop_vertex(marko)?;
        let n = g.add_vertex(
            Some("n".into()),
            vec!["a".into()],
            props(&[("k", 1.into())]),
        )?;
        g.add_edge(
            Some("ne".into()),
            None,
            n,
            v(g, "2"),
            PropertyMap::default(),
        )?;
        g.drop_edge(e(g, "3"))?;
        Ok(())
    });
    let text = serde_json::to_string(&set).unwrap();
    let back: ChangeSet = serde_json::from_str(&text).unwrap();
    assert_eq!(back, set);
    // A stored null before the write stays distinct from an absent property.
    let nil_writes: Vec<_> = back
        .mutations()
        .iter()
        .filter_map(|m| match m {
            Mutation::SetProperty { key, before, .. } if key == "nil" => Some(before.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(nil_writes, [None, Some(ElementProperty::Null)]);
    // The shape: an `op` tag per mutation.
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["commit_seq"], 1);
    assert_eq!(json["mutations"][0]["op"], "set_property");
    assert_eq!(json["mutations"][0]["element"]["kind"], "vertex");
}

#[cfg(feature = "serde")]
#[test]
fn a_uuid_survives_json_at_any_depth_and_a_schema_less_replay_is_identical() {
    let uuid = ElementProperty::Uuid(0x1234);
    let mut graph = modern();
    let start = content(&graph);
    let mut tricky = PropertyMap::default();
    // A user map that looks like a type tag must not be misread.
    tricky.insert("@type", "g:UUID".into());
    tricky.insert("@value", "00000000-0000-0000-0000-000000001234".into());
    tricky.insert("id", uuid.clone());
    let set = capture(&mut graph, |g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "u".into(), uuid.clone())?;
        g.set_vertex_property(
            marko,
            "nested".into(),
            ElementProperty::Object(Box::new(props(&[(
                "list",
                ElementProperty::Array(vec![uuid.clone(), "x".into()]),
            )]))),
        )?;
        g.set_vertex_property(
            marko,
            "tricky".into(),
            ElementProperty::Object(Box::new(tricky.clone())),
        )?;
        let n = g.add_vertex(
            Some("n".into()),
            vec!["a".into()],
            props(&[("u", uuid.clone())]),
        )?;
        g.add_edge(
            Some("ne".into()),
            None,
            n,
            marko,
            props(&[("u", uuid.clone())]),
        )?;
        g.drop_vertex(v(g, "2"))?;
        Ok(())
    });
    let text = serde_json::to_string(&set).unwrap();
    assert!(
        text.contains(r#"{"@type":"g:UUID","@value":"00000000-0000-0000-0000-000000001234"}"#),
        "{text}"
    );
    assert!(text.contains(r#""@type":"graphersal:Object""#), "{text}");
    let back: ChangeSet = serde_json::from_str(&text).unwrap();
    assert_eq!(back, set, "the round trip keeps every logical type");
    // Schema-less replay of the deserialized journal into a copy of the start state.
    let replica = TraversalGraph::tinkerpop_modern().replay(&back).unwrap();
    assert_ne!(content(&replica), start);
    assert_eq!(content(&replica), content(&graph));
}

#[cfg(feature = "serde")]
#[test]
fn a_malformed_type_tag_is_rejected_not_guessed() {
    for bad in [
        r#"{"@type":"g:UUID","@value":"not-a-uuid"}"#,
        r#"{"@type":"g:UUID","@value":"00000000-0000-0000-0000-000000001234","x":1}"#,
        r#"{"@type":"g:Date","@value":"2024-01-01"}"#,
        r#"{"@type":"graphersal:Object","@value":[1]}"#,
        r#"{"@type":1,"@value":"x"}"#,
        r#"{"@type":"g:UUID"}"#,
    ] {
        let text = format!(
            r#"{{"commit_seq":1,"mutations":[{{"op":"set_property","element":{{"kind":"vertex","id":"1"}},"key":"k","after":{bad}}}]}}"#
        );
        assert!(
            serde_json::from_str::<ChangeSet>(&text).is_err(),
            "{bad} must be rejected"
        );
    }
    // A plain object without "@type" stays a plain object.
    let text = r#"{"commit_seq":1,"mutations":[{"op":"set_property","element":{"kind":"vertex","id":"1"},"key":"k","after":{"@value":"x"}}]}"#;
    let set: ChangeSet = serde_json::from_str(text).unwrap();
    match &set.mutations()[0] {
        Mutation::SetProperty { after, .. } => {
            assert_eq!(
                after,
                &ElementProperty::Object(Box::new(props(&[("@value", "x".into())])))
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}
