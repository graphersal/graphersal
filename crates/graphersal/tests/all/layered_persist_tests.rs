//! `PersistentStorage` on a `LayeredGraph` (layered-graph plan S3.1): the layer's parts
//! (`layer_parts`: the base patch by element id and the added elements) loaded over the same
//! base (`load_base_patch` + the ordinary load sink) give the same layer, and the change sets
//! of a live layer replayed into a fresh layer over the same base give the same layer: data,
//! property order, `layer_diff`, origins, statistics, index overlays, schema, catalog, position
//! and id sequences. Every delta kind is exercised: added vertices and edges, cross edges,
//! images of vertices and edges (label changes included), tombstones with cascade, a base
//! element changed back to clean, a layer-defined index, vector-rule-derived values
//! (`hash-ngram/1`), a schema change, a removed base definition, a reused base id.
#![cfg(feature = "persist")]

use std::sync::{Arc, Mutex};

use graphersal::catalog::{Definition, DefinitionKind, PropertyIndexDefinition, QueryDefinition};
use graphersal::changes::{ChangeSet, CommitHook, HookError, RollbackReason};
use graphersal::prelude::*;
use graphersal::storage::{
    BasePatch, ElementKind, GraphStorage, LayerDiffRequest, LayeredGraph, PersistentStorage,
};

/// The vertex label `doc` derives `emb` from `text` with the built-in `hash-ngram/1`.
const RULES: &str = r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
    "text": {"type": "string"},
    "emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "text"}}}}}}"#;

/// [`RULES`] plus a declaration of `person` (the layer's schema change).
const RULES_AND_PERSON: &str = r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
    "text": {"type": "string"},
    "emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "text"}}}},
  "person": {"schema": {"type": "object", "properties": {"age": {"type": "integer"}}}}}}"#;

fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in pairs {
        map.insert(*key, value.clone());
    }
    map
}

fn query(name: &str) -> Definition {
    Definition::query(
        name,
        QueryDefinition::new(format!("fn {name}() {{ g.v() }}")),
    )
}

/// The base: persons p1-p3 (indexed `age`), docs d1-d2 (vector rule), software s1; `knows`
/// edges k1-k3 (indexed `weight`), `created` c1, `cites` w1-w2; saved queries q1 and q2.
fn base() -> Arc<TraversalGraph> {
    let mut base = TraversalGraph::new();
    base.set_schema(GraphSchema::from_json(RULES).unwrap())
        .unwrap();
    base.set_definition(Definition::property_index(
        "age",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
    ))
    .unwrap();
    base.set_definition(Definition::property_index(
        "weight",
        PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight"),
    ))
    .unwrap();
    base.set_definition(query("q1")).unwrap();
    base.set_definition(query("q2")).unwrap();
    let mark = base.begin_unit().unwrap();
    let mut vertex = |id: &str, label: &str, pairs: &[(&str, ElementProperty)]| {
        base.insert_vertex(Some(id.into()), vec![label.into()], props(pairs))
            .unwrap()
    };
    let p1 = vertex(
        "p1",
        "person",
        &[("name", "ann".into()), ("age", 30.into())],
    );
    let p2 = vertex(
        "p2",
        "person",
        &[("name", "bob".into()), ("age", 40.into())],
    );
    let p3 = vertex("p3", "person", &[("name", "cy".into()), ("age", 50.into())]);
    let d1 = vertex("d1", "doc", &[("text", "graph databases".into())]);
    let d2 = vertex("d2", "doc", &[("text", "vector search".into())]);
    let s1 = vertex("s1", "software", &[("name", "lop".into())]);
    let mut edge = |id: &str, label: &str, from, to, pairs: &[(&str, ElementProperty)]| {
        base.insert_edge(Some(id.into()), Some(label.into()), from, to, props(pairs))
            .unwrap();
    };
    edge("k1", "knows", p1, p2, &[("weight", 0.5.into())]);
    edge("k2", "knows", p2, p3, &[("weight", 1.0.into())]);
    edge("k3", "knows", p1, p3, &[("weight", 0.2.into())]);
    edge("c1", "created", p3, s1, &[]);
    edge("w1", "cites", d1, d2, &[]);
    edge("w2", "cites", d2, d1, &[]);
    base.commit_unit(mark).unwrap();
    Arc::new(base)
}

fn v<S: GraphStorage>(s: &S, id: &str) -> S::VertexId {
    s.get_vertex_by_id(id).unwrap().unwrap()
}

fn e<S: GraphStorage>(s: &S, id: &str) -> S::EdgeId {
    s.get_edge_by_id(id).unwrap().unwrap()
}

/// Runs `writes` as one unit of the layer.
fn unit(
    layer: &mut LayeredGraph,
    writes: impl FnOnce(&mut LayeredGraph) -> Result<(), GraphError>,
) {
    let mark = layer.begin_unit().unwrap();
    writes(layer).unwrap();
    layer.commit_unit(mark).unwrap();
}

/// The units of the history, in order; each one exercises a delta kind (its comment).
fn history() -> Vec<Box<dyn Fn(&mut LayeredGraph) -> Result<(), GraphError>>> {
    vec![
        // Added vertices (an auto id, explicit ids, a doc whose `emb` the rule derives), an
        // added edge, cross edges (added -> base, base -> added, a base self-loop, base -> base).
        Box::new(|l| {
            let auto = l.insert_vertex(
                None,
                vec!["person".into()],
                props(&[("name", "dee".into()), ("age", 22.into())]),
            )?;
            let x1 = l.insert_vertex(
                Some("x1".into()),
                vec!["person".into()],
                props(&[("age", 30.into()), ("name", "xavier".into())]),
            )?;
            l.insert_vertex(
                Some("n1".into()),
                vec!["doc".into()],
                props(&[("text", "a fresh document".into())]),
            )?;
            l.insert_edge(
                Some("a1".into()),
                Some("knows".into()),
                x1,
                auto,
                props(&[("weight", 0.9.into())]),
            )?;
            let (p1, p2, p3, s1) = (v(l, "p1"), v(l, "p2"), v(l, "p3"), v(l, "s1"));
            l.insert_edge(
                Some("x2".into()),
                Some("knows".into()),
                x1,
                p1,
                props(&[("weight", 0.3.into())]),
            )?;
            l.insert_edge(None, Some("knows".into()), p2, x1, props(&[]))?;
            l.insert_edge(Some("loop".into()), Some("self".into()), p3, p3, props(&[]))?;
            l.insert_edge(Some("u1".into()), Some("uses".into()), p1, s1, props(&[]))?;
            Ok(())
        }),
        // Vertex images: a property, a gained and a lost label (index overlays), a doc text
        // whose derived vector the rule recomputes in the image.
        Box::new(|l| {
            let (p1, p2, d1) = (v(l, "p1"), v(l, "p2"), v(l, "d1"));
            l.set_vertex_property(p1, "age".into(), 31.into())?;
            l.add_vertex_label(p1, "admin".into())?;
            l.add_vertex_label(p2, "admin".into())?;
            l.remove_vertex_label(p2, "person")?;
            l.set_vertex_property(d1, "text".into(), "graph layers".into())?;
            Ok(())
        }),
        // Edge images: a property, relabels in another order than the slots, a new key.
        Box::new(|l| {
            let (k1, k2, k3, w1) = (e(l, "k1"), e(l, "k2"), e(l, "k3"), e(l, "w1"));
            l.set_edge_property(k1, "weight".into(), 0.75.into())?;
            l.set_edge_label(k3, "met".into())?;
            l.set_edge_label(k2, "met".into())?;
            l.set_edge_property(w1, "note".into(), "see also".into())?;
            Ok(())
        }),
        // Tombstones: a base vertex with its base edge and a cross edge (cascade), a base edge.
        Box::new(|l| {
            let s1 = v(l, "s1");
            l.drop_vertex(s1)?;
            let w2 = e(l, "w2");
            l.drop_edge(w2)
        }),
        // A base element changed (its derived vector with it) ...
        Box::new(|l| {
            let d2 = v(l, "d2");
            l.set_vertex_property(d2, "text".into(), "temporary".into())?;
            let k1 = e(l, "k1");
            l.set_edge_property(k1, "since".into(), 2020.into())
        }),
        // ... and changed back to clean (the vertex), the edge keeps an image.
        Box::new(|l| {
            let d2 = v(l, "d2");
            l.set_vertex_property(d2, "text".into(), "vector search".into())?;
            let k1 = e(l, "k1");
            l.drop_edge_property("since", k1)
        }),
        // A layer-defined index, a removed base definition, a new one.
        Box::new(|l| {
            l.set_definition(Definition::property_index(
                "name",
                PropertyIndexDefinition::label_less(ElementKind::Vertex, "name"),
            ))?;
            l.remove_definition(DefinitionKind::QUERY, "q1")?;
            l.set_definition(query("q3"))
        }),
        // A schema change on the layer.
        Box::new(|l| l.set_schema(GraphSchema::from_json(RULES_AND_PERSON).unwrap())),
        // The id of the dropped base vertex reused; an added vertex with its edge dropped and a
        // new auto id handed out.
        Box::new(|l| {
            l.insert_vertex(
                Some("s1".into()),
                vec!["software".into()],
                props(&[("name", "ripple".into())]),
            )?;
            let dee = l
                .vertices()?
                .find(|node| {
                    l.vertex_properties(*node)
                        .ok()
                        .flatten()
                        .and_then(|map| map.get("name").cloned())
                        == Some("dee".into())
                })
                .ok_or_else(|| GraphError::unsupported("dee"))?;
            l.drop_vertex(dee)?;
            l.insert_vertex(
                None,
                vec!["person".into()],
                props(&[("name", "eve".into())]),
            )?;
            Ok(())
        }),
    ]
}

/// The live layer after the whole history, with the change set of every unit.
fn live() -> (LayeredGraph, Vec<ChangeSet>) {
    let mut layer = LayeredGraph::over(base()).unwrap();
    let keeper = Keeper::on(&mut layer);
    for writes in history() {
        unit(&mut layer, |l| writes(l));
    }
    let changes = keeper.0.lock().unwrap().clone();
    (layer, changes)
}

/// A hook keeping every change set it accepted.
#[derive(Clone, Default)]
struct Keeper(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Keeper {
    fn name(&self) -> &str {
        "keeper"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }

    fn after_commit(&mut self, _: &ChangeSet) {}

    fn after_rollback(&mut self, _: &RollbackReason) {}
}

impl Keeper {
    fn on<S: GraphStorage>(storage: &mut S) -> Self {
        let keeper = Keeper::default();
        storage
            .register_commit_hook(Box::new(keeper.clone()))
            .unwrap();
        keeper
    }
}

/// Everything a layer shows, as sorted text rows: every element with its labels, properties
/// IN ORDER, endpoints, origin and adjacency; the label buckets; the diff; the statistics; the
/// index infos; the schema; the catalog; the commit sequence number and the id sequences (the
/// commit time is compared where it must be equal).
fn state(layer: &LayeredGraph) -> Vec<String> {
    let mut rows = Vec::new();
    let vid = |node| layer.vertex_id(node).unwrap().unwrap().to_string();
    let eid = |edge| layer.edge_id(edge).unwrap().unwrap().to_string();
    for node in layer.vertices().unwrap() {
        let mut out: Vec<String> = layer.out_edges(node).unwrap().map(eid).collect();
        let mut inc: Vec<String> = layer.in_edges(node).unwrap().map(eid).collect();
        out.sort();
        inc.sort();
        rows.push(format!(
            "v {} {:?} {:?} {:?} out {out:?} in {inc:?}",
            vid(node),
            layer.vertex_labels(node).unwrap(),
            layer.vertex_properties(node).unwrap(),
            layer.vertex_layer(node).unwrap(),
        ));
    }
    for edge in layer.edges().unwrap() {
        let (from, to) = layer.edge_endpoints(edge).unwrap().unwrap();
        rows.push(format!(
            "e {} {:?} {} -> {} {:?} {:?}",
            eid(edge),
            layer.edge_label(edge).unwrap(),
            vid(from),
            vid(to),
            layer.edge_properties(edge).unwrap(),
            layer.edge_layer(edge).unwrap(),
        ));
    }
    for label in ["person", "admin", "doc", "software"] {
        let mut members: Vec<String> = layer.vertices_by_label(label).unwrap().map(vid).collect();
        members.sort();
        rows.push(format!("label {label} {members:?}"));
    }
    for label in ["knows", "met", "cites", "self", "uses", "created"] {
        let mut members: Vec<String> = layer.edges_by_label(label).unwrap().map(eid).collect();
        members.sort();
        rows.push(format!("edge label {label} {members:?}"));
    }
    // The diff lists added elements in delta slot order, which a load compacts (plan P4):
    // compared as sets.
    let diff = layer.layer_diff(&LayerDiffRequest::new()).unwrap();
    let sorted = |entries: Vec<String>| {
        let mut entries = entries;
        entries.sort();
        entries
    };
    let debug = |entry: &dyn std::fmt::Debug| format!("{entry:?}");
    rows.push(format!(
        "diff added {:?} removed {:?} modified {:?} truncated {}",
        sorted(diff.added.iter().map(|x| debug(x)).collect()),
        sorted(diff.removed.iter().map(|x| debug(x)).collect()),
        sorted(diff.modified.iter().map(|x| debug(x)).collect()),
        diff.truncated
    ));
    rows.push(format!("statistics {:?}", layer.statistics().unwrap()));
    let mut indexes: Vec<String> = layer
        .property_indexes()
        .unwrap()
        .iter()
        .map(|info| format!("{info:?}"))
        .collect();
    indexes.sort();
    rows.push(format!("indexes {indexes:?}"));
    rows.push(format!(
        "schema {}",
        layer.get_schema().unwrap().unwrap_or_default().to_json()
    ));
    let mut catalog: Vec<String> = layer
        .definitions()
        .unwrap()
        .map(|d| format!("{:?}", d))
        .collect();
    catalog.sort();
    rows.push(format!("catalog {catalog:?}"));
    rows.push(format!(
        "position {} {:?}",
        layer.last_commit_seq().unwrap(),
        layer.id_sequences()
    ));
    rows.sort();
    rows
}

/// Asserts two [`state`]s are equal, naming only the rows that differ.
#[track_caller]
fn assert_same(left: &LayeredGraph, right: &LayeredGraph, what: &str) {
    let (left, right) = (state(left), state(right));
    if left != right {
        let only = |a: &[String], b: &[String]| -> Vec<String> {
            a.iter().filter(|row| !b.contains(row)).cloned().collect()
        };
        panic!(
            "{what}: the states differ\nonly left:\n{}\nonly right:\n{}",
            only(&left, &right).join("\n"),
            only(&right, &left).join("\n")
        );
    }
}

#[track_caller]
fn check(layer: &LayeredGraph) {
    if let Err(problem) = layer.check_layer_indexes() {
        panic!("{problem}");
    }
}

/// `layer`'s parts loaded into a fresh layer over its base, in a snapshot loader's order:
/// the patch, the schema, the catalog, the added vertices, the added edges, `finish_load`,
/// then the lineage id, the position and the sequences.
fn reload(layer: &LayeredGraph) -> LayeredGraph {
    let mut fresh = LayeredGraph::over(Arc::clone(layer.base())).unwrap();
    let parts = layer.layer_parts().unwrap().expect("a layer has parts");
    for entry in parts.patch {
        fresh.load_base_patch(entry).unwrap();
    }
    fresh
        .load_schema(layer.get_schema().unwrap().unwrap_or_default())
        .unwrap();
    for definition in layer.definitions().unwrap() {
        fresh.load_definition(definition.clone()).unwrap();
    }
    for node in parts.vertices {
        fresh
            .load_vertex(
                layer.vertex_id(node).unwrap().unwrap().into(),
                layer.vertex_labels(node).unwrap().to_vec(),
                layer.vertex_properties(node).unwrap().unwrap().clone(),
            )
            .unwrap();
    }
    for edge in parts.edges {
        let (from, to) = layer.edge_endpoints(edge).unwrap().unwrap();
        fresh
            .load_edge(
                layer.edge_id(edge).unwrap().unwrap().into(),
                layer.edge_label(edge).unwrap().map(Into::into),
                layer.vertex_id(from).unwrap().unwrap(),
                layer.vertex_id(to).unwrap().unwrap(),
                layer
                    .edge_properties(edge)
                    .unwrap()
                    .cloned()
                    .unwrap_or_default(),
            )
            .unwrap();
    }
    fresh.finish_load().unwrap();
    fresh.set_graph_id(layer.graph_id()).unwrap();
    fresh
        .raise_position(
            layer.last_commit_seq().unwrap(),
            PersistentStorage::last_committed_at(layer),
        )
        .unwrap();
    let (vertices, edges) = layer.id_sequences();
    fresh.raise_id_sequences(vertices, edges).unwrap();
    fresh
}

/// The live layer holds every delta kind the test is about (so the round trips below cover
/// them).
#[test]
fn the_history_makes_every_delta_kind() {
    let (layer, changes) = live();
    assert_eq!(changes.len(), history().len());
    check(&layer);
    let parts = layer.layer_parts().unwrap().unwrap();
    let patch = parts.patch.clone();
    let has = |want: fn(&BasePatch) -> bool| patch.iter().any(want);
    assert!(has(
        |p| matches!(p, BasePatch::RemovedVertex { id, .. } if id == "s1")
    ));
    assert!(has(
        |p| matches!(p, BasePatch::RemovedEdge { id, .. } if id == "w2")
    ));
    assert!(has(
        |p| matches!(p, BasePatch::RemovedEdge { id, .. } if id == "c1")
    ));
    assert!(has(
        |p| matches!(p, BasePatch::VertexImage { id, labels, .. } if id == "p2" && labels == &["admin"])
    ));
    assert!(has(
        |p| matches!(p, BasePatch::VertexImage { id, .. } if id == "d1")
    ));
    assert!(
        !has(|p| matches!(p, BasePatch::VertexImage { id, .. } if id == "d2")),
        "d2 is clean again"
    );
    assert!(has(
        |p| matches!(p, BasePatch::EdgeImage { id, label, .. } if id == "k2" && label.as_deref() == Some("met"))
    ));
    assert!(has(
        |p| matches!(p, BasePatch::RemovedDefinition { kind, name, .. } if *kind == DefinitionKind::QUERY && name == "q1")
    ));
    // The relabeled edges come in the order they were relabeled (k3, then k2).
    let relabeled: Vec<&str> = patch
        .iter()
        .filter_map(|p| match p {
            BasePatch::EdgeImage {
                id,
                label: Some(label),
                ..
            } if label == "met" => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(relabeled, ["k3", "k2"]);
    // The derived vector of the changed base doc is in its image, recomputed.
    let d1 = layer.vertex_properties(v(&layer, "d1")).unwrap().unwrap();
    let base_d1 = layer
        .base()
        .vertex_properties(v(&**layer.base(), "d1"))
        .unwrap()
        .unwrap();
    assert!(d1.get("emb").is_some());
    assert_ne!(d1.get("emb"), base_d1.get("emb"));
    // The added elements: x1, n1, s1 (new), eve; a1 is gone with dee.
    let added: Vec<String> = parts
        .vertices
        .map(|node| layer.vertex_id(node).unwrap().unwrap().to_string())
        .collect();
    assert_eq!(added.len(), 4, "{added:?}");
    assert!(added.contains(&"s1".to_string()));
    assert!(
        layer
            .vertex_properties(v(&layer, "n1"))
            .unwrap()
            .unwrap()
            .get("emb")
            .is_some(),
        "the rule derived the added doc's vector"
    );
    assert!(layer.id_sequences().0 > layer.base().id_sequences().0);
}

/// The parts of a live layer loaded into a fresh layer over the same base give the same layer;
/// writes continue alike (the next auto ids, a unique property of the next write).
#[test]
fn layer_parts_loaded_over_the_same_base_give_the_same_layer() {
    let (mut layer, _) = live();
    let mut loaded = reload(&layer);
    check(&loaded);
    assert_same(&loaded, &layer, "loaded");
    assert_eq!(loaded.graph_id(), layer.graph_id());
    assert_eq!(
        PersistentStorage::last_committed_at(&loaded),
        PersistentStorage::last_committed_at(&layer)
    );
    assert_eq!(loaded.statistics().unwrap(), layer.statistics().unwrap());
    // Loaded again from the loaded layer: still the same (the patch is stable).
    let again = reload(&loaded);
    assert_same(&again, &layer, "loaded twice");
    assert_eq!(
        again.layer_parts().unwrap().unwrap().patch,
        layer.layer_parts().unwrap().unwrap().patch
    );
    // The next auto ids are the same and never a base id.
    for target in [&mut layer, &mut loaded] {
        unit(target, |l| {
            let n = l.insert_vertex(None, vec!["person".into()], props(&[]))?;
            let p1 = v(l, "p1");
            l.insert_edge(None, Some("knows".into()), n, p1, props(&[]))?;
            l.set_vertex_property(v(l, "d2"), "text".into(), "after the load".into())
        });
        check(target);
    }
    assert_same(&loaded, &layer, "after more writes");
}

/// The change sets of a live layer, replayed (with and without verification) into a fresh
/// layer over the same base, give the same layer.
#[test]
fn replayed_change_sets_give_the_same_layer() {
    let (layer, changes) = live();
    for verify in [false, true] {
        let mut replayed = LayeredGraph::over(Arc::clone(layer.base())).unwrap();
        PersistentStorage::begin_replay(&mut replayed).unwrap();
        for change_set in &changes {
            PersistentStorage::replay(&mut replayed, change_set, verify).unwrap();
        }
        PersistentStorage::finish_replay(&mut replayed).unwrap();
        check(&replayed);
        assert_same(&replayed, &layer, &format!("replayed, verify: {verify}"));
        assert_eq!(
            PersistentStorage::last_committed_at(&replayed),
            PersistentStorage::last_committed_at(&layer)
        );
        // Nothing was recorded or committed by the replay itself.
        assert_eq!(replayed.last_commit_seq().unwrap(), changes.len() as u64);
    }
}

/// A snapshot of the first half of the history plus the replay of the rest gives the same
/// layer (recovery: snapshot plus journal).
#[test]
fn a_loaded_layer_continues_with_the_replayed_journal() {
    let base = base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let keeper = Keeper::on(&mut layer);
    let units = history();
    let half = units.len() / 2;
    for writes in &units[..half] {
        unit(&mut layer, |l| writes(l));
    }
    let mut recovered = reload(&layer);
    for writes in &units[half..] {
        unit(&mut layer, |l| writes(l));
    }
    let changes = keeper.0.lock().unwrap().clone();
    for change_set in &changes[half..] {
        PersistentStorage::replay(&mut recovered, change_set, true).unwrap();
    }
    check(&recovered);
    assert_same(&recovered, &layer, "recovered");
}

/// A replay into a layer that does not hold the state the journal continues fails with the
/// element named (verification), and a base patch naming an element the base lacks fails.
#[test]
fn mismatches_are_refused() {
    let (_, changes) = live();
    // The drop commit (s1 with c1 and the cross edge u1, then w2) needs the first commit (u1):
    // without it the verification names the missing edge; after it, a second replay of the
    // drops names an element removed already.
    let drops = &changes[3];
    let mut wrong = LayeredGraph::over(base()).unwrap();
    let err = PersistentStorage::replay(&mut wrong, drops, true).unwrap_err();
    assert!(err.contains("u1") && err.contains("missing"), "{err}");
    let mut wrong = LayeredGraph::over(base()).unwrap();
    PersistentStorage::replay(&mut wrong, &changes[0], true).unwrap();
    PersistentStorage::replay(&mut wrong, drops, true).unwrap();
    let err = PersistentStorage::replay(&mut wrong, drops, true).unwrap_err();
    assert!(err.contains("missing"), "{err}");
    let mut fresh = LayeredGraph::over(base()).unwrap();
    assert!(matches!(
        fresh.load_base_patch(BasePatch::removed_vertex("nope")),
        Err(GraphError::VertexNotFound { .. })
    ));
    assert!(matches!(
        fresh.load_base_patch(BasePatch::removed_edge("nope")),
        Err(GraphError::EdgeNotFound { .. })
    ));
    fresh
        .load_base_patch(BasePatch::removed_vertex("p3"))
        .unwrap();
    // Removed already (also its edges by the cascade): nothing more.
    fresh
        .load_base_patch(BasePatch::removed_vertex("p3"))
        .unwrap();
    fresh
        .load_base_patch(BasePatch::removed_edge("k2"))
        .unwrap();
    // An image of a removed element is refused.
    assert!(
        fresh
            .load_base_patch(BasePatch::vertex_image(
                "p3",
                vec![],
                PropertyMap::default()
            ))
            .is_err()
    );
    assert_eq!(fresh.vertex_count().unwrap(), 5);
    assert_eq!(fresh.edge_count().unwrap(), 3);
}

/// An edge image with an empty map over a base edge without one, an unlabeled image of a
/// labeled base edge... the states the property and label writes reach only indirectly load
/// as they were.
#[test]
fn edge_images_of_every_shape_load_as_they_were() {
    let mut base = TraversalGraph::new();
    let a = base
        .insert_vertex(Some("a".into()), vec![], PropertyMap::default())
        .unwrap();
    let b = base
        .insert_vertex(Some("b".into()), vec![], PropertyMap::default())
        .unwrap();
    let layer_over = |base: TraversalGraph| LayeredGraph::over(Arc::new(base)).unwrap();
    // A base edge without a map: `insert_edge` gives one, so build it through the load sink.
    base.load_edge(
        "e".into(),
        Some("l".into()),
        "a",
        "b",
        PropertyMap::default(),
    )
    .unwrap();
    base.insert_edge(
        Some("f".into()),
        Some("l".into()),
        a,
        b,
        props(&[("k", 1.into())]),
    )
    .unwrap();
    let mut layer = layer_over(base);
    unit(&mut layer, |l| {
        let ed = e(l, "e");
        l.set_edge_property(ed, "x".into(), 1.into())?;
        l.drop_edge_property("x", ed)?;
        let f = e(l, "f");
        l.set_edge_property(f, "k".into(), 2.into())?;
        l.set_edge_label(f, "m".into()).map(|_| ())
    });
    let loaded = reload(&layer);
    check(&loaded);
    assert_same(&loaded, &layer, "edge shapes");
    assert_eq!(
        loaded
            .edge_properties(e(&loaded, "e"))
            .unwrap()
            .map(PropertyMap::len),
        layer
            .edge_properties(e(&layer, "e"))
            .unwrap()
            .map(PropertyMap::len)
    );
    // A patch may also name an unlabeled image and a map in another key order.
    let mut fresh = LayeredGraph::over(Arc::clone(layer.base())).unwrap();
    fresh
        .load_base_patch(BasePatch::edge_image(
            "f",
            None,
            Some(props(&[("z", 0.into()), ("k", 1.into())])),
        ))
        .unwrap();
    let f = e(&fresh, "f");
    assert_eq!(fresh.edge_label(f).unwrap(), None);
    let keys: Vec<&str> = fresh
        .edge_properties(f)
        .unwrap()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(keys, ["z", "k"]);
    let loaded = reload(&fresh);
    assert_same(&loaded, &fresh, "patched");
}

/// `clear` gives an empty layer over the same base and keeps the hooks; a plain storage has no
/// parts and refuses a base patch.
#[test]
fn clear_keeps_the_base_and_the_hooks_and_a_plain_storage_is_no_layer() {
    let (mut layer, _) = live();
    let keeper = Keeper::on(&mut layer);
    let lineage = layer.graph_id();
    layer.clear().unwrap();
    let empty = LayeredGraph::over(Arc::clone(layer.base())).unwrap();
    assert_same(&layer, &empty, "cleared");
    assert_ne!(layer.graph_id(), lineage);
    assert!(layer.layer_parts().unwrap().unwrap().patch.is_empty());
    unit(&mut layer, |l| {
        l.insert_vertex(None, vec!["person".into()], props(&[]))
            .map(|_| ())
    });
    assert_eq!(keeper.0.lock().unwrap().len(), 1, "the hook was kept");

    let mut plain = TraversalGraph::tinkerpop_modern();
    assert!(plain.layer_parts().unwrap().is_none());
    assert!(matches!(
        plain.load_base_patch(BasePatch::removed_vertex("1")),
        Err(GraphError::Unsupported { .. })
    ));
}

/// A unique index the base keeps disabled (repeated values) stays disabled with the base's
/// report after a load, also when the layer removed the repetition (as on the live layer); a
/// unique index the layer defines over its own data is built again by the load.
#[test]
fn unique_indexes_load_as_the_live_layer_serves_them() {
    let mut base = TraversalGraph::new();
    for (id, name) in [("a", "ann"), ("b", "ann"), ("c", "cy")] {
        base.insert_vertex(
            Some(id.into()),
            vec!["person".into()],
            props(&[("name", name.into()), ("code", id.into())]),
        )
        .unwrap();
    }
    base.set_definition(Definition::property_index(
        "uname",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name").with_unique(true),
    ))
    .unwrap();
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    assert!(layer.unique_index_duplicates("uname").unwrap().is_some());
    unit(&mut layer, |l| {
        l.set_vertex_property(v(l, "b"), "name".into(), "bob".into())?;
        l.set_definition(Definition::property_index(
            "ucode",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "code").with_unique(true),
        ))
    });
    let loaded = reload(&layer);
    check(&loaded);
    assert_same(&loaded, &layer, "unique");
    assert_eq!(
        loaded.unique_index_duplicates("uname").unwrap(),
        layer.unique_index_duplicates("uname").unwrap()
    );
    assert!(loaded.unique_index_duplicates("uname").unwrap().is_some());
    // The layer's own unique index is enforced after the load.
    let mut loaded = loaded;
    let mark = loaded.begin_unit().unwrap();
    let err = loaded
        .set_vertex_property(v(&loaded, "c"), "code".into(), "a".into())
        .unwrap_err();
    loaded
        .rollback_unit(mark, &RollbackReason::failed(&err))
        .unwrap();
    assert!(matches!(err, GraphError::UniqueViolation { .. }), "{err}");
}
