//! Vector rules on a `LayeredGraph` (layered-graph plan S4.5b): a layer over a base whose schema
//! has vector rules runs the rule pass on its own units. After every write the layer's data
//! (the derived vectors included) and the committed change set equal those of a
//! `TraversalGraph` holding the same data after the same writes; a vector recomputed to the
//! value the base element holds copies nothing up; a failing embedder rolls the whole unit back
//! (data, index overlays, nothing in `layer_diff`); a rule the layer adds on a base label fills
//! every element of the label, and the fill preview counts the base elements it copies up;
//! direct writes of a derived property are refused with the same error; dry runs and
//! `apply_change_set` behave as on a `TraversalGraph`.

use std::sync::{Arc, Mutex};

use graphersal::catalog::{Definition, PropertyIndexDefinition};
use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, GraphStorage, JsonPath, LayerDiffRequest, LayeredGraph, RollbackReason,
};

/// The rule of the vertex label `doc` (`emb` from `text`) and of the edge label `rel` (`nemb`
/// from `note`), with the built-in `hash-ngram/1`.
const RULES: &str = r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
    "text": {"type": "string"},
    "emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "text"}}}}},
  "edges": {"rel": {"schema": {"type": "object", "properties": {
    "note": {"type": "string"},
    "nemb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "note"}}}}}}"#;

fn rules() -> GraphSchema {
    GraphSchema::from_json(RULES).unwrap()
}

fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in pairs {
        map.insert(*key, value.clone());
    }
    map
}

/// The base data: docs d1-d4 with texts, d5 without one, a `page` p1 with a text (no rule), an
/// `other` vertex o1, and the `rel` edges r1 (d1 -> d2, a note) and r2 (d3 -> d4, no note).
/// Written in one unit (`schema` set first, when given), so the base's own rule pass derives
/// its vectors.
fn base_with(schema: Option<GraphSchema>) -> TraversalGraph {
    let mut base = TraversalGraph::new();
    if let Some(schema) = schema {
        base.set_schema(schema).unwrap();
    }
    base.transaction(|g| -> Result<(), GraphError> {
        let mut vertex = |id: &str, label: &str, pairs: &[(&str, ElementProperty)]| {
            g.insert_vertex(Some(id.into()), vec![label.into()], props(pairs))
        };
        let d1 = vertex("d1", "doc", &[("text", "graph databases".into())])?;
        let d2 = vertex("d2", "doc", &[("text", "vector search".into())])?;
        let d3 = vertex("d3", "doc", &[("text", "layered storage".into())])?;
        let d4 = vertex("d4", "doc", &[("text", "frozen bases".into())])?;
        vertex("d5", "doc", &[("name", "no text".into())])?;
        vertex("p1", "page", &[("text", "a page of text".into())])?;
        vertex("o1", "other", &[("text", "other things".into())])?;
        g.insert_edge(
            Some("r1".into()),
            Some("rel".into()),
            d1,
            d2,
            props(&[("note", "related work".into())]),
        )?;
        g.insert_edge(Some("r2".into()), Some("rel".into()), d3, d4, props(&[]))?;
        Ok(())
    })
    .unwrap();
    base
}

/// The base with the rules.
fn ruled_base() -> Arc<TraversalGraph> {
    Arc::new(base_with(Some(rules())))
}

/// A `TraversalGraph` with the layer's data and schema: the schema set on the empty graph
/// first (no fill), then every element copied as it is, outside any unit (no rule pass).
fn materialize(layer: &LayeredGraph) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    if let Some(schema) = layer.get_schema().unwrap() {
        graph.set_schema(schema).unwrap();
    }
    let mut vertices = std::collections::HashMap::new();
    for node in layer.vertices().unwrap() {
        let id = layer.vertex_id(node).unwrap().map(Into::into);
        let labels = layer.vertex_labels(node).unwrap().to_vec();
        let properties = layer.vertex_properties(node).unwrap().cloned();
        let copy = graph
            .insert_vertex(id, labels, properties.unwrap_or_default())
            .unwrap();
        vertices.insert(node, copy);
    }
    for edge in layer.edges().unwrap() {
        let id = layer.edge_id(edge).unwrap().map(Into::into);
        let label = layer.edge_label(edge).unwrap().map(Into::into);
        let (from, to) = layer.edge_endpoints(edge).unwrap().unwrap();
        let properties = layer.edge_properties(edge).unwrap().cloned();
        graph
            .insert_edge(
                id,
                label,
                vertices[&from],
                vertices[&to],
                properties.unwrap_or_default(),
            )
            .unwrap();
    }
    graph
}

/// Every vertex and edge (id, labels, properties) of a storage, sorted by id.
fn snapshot<S: GraphStorage>(storage: &S) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for node in storage.vertices().unwrap() {
        let mut properties: Vec<String> = storage
            .vertex_properties(node)
            .unwrap()
            .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        properties.sort();
        rows.push(format!(
            "v {} {:?} {properties:?}",
            storage.vertex_id(node).unwrap().unwrap(),
            storage.vertex_labels(node).unwrap()
        ));
    }
    for edge in storage.edges().unwrap() {
        let mut properties: Vec<String> = storage
            .edge_properties(edge)
            .unwrap()
            .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        properties.sort();
        rows.push(format!(
            "e {} {:?} {properties:?}",
            storage.edge_id(edge).unwrap().unwrap(),
            storage.edge_label(edge).unwrap()
        ));
    }
    rows.sort();
    rows
}

fn v<S: GraphStorage>(s: &S, id: &str) -> S::VertexId {
    s.get_vertex_by_id(id).unwrap().unwrap()
}

fn e<S: GraphStorage>(s: &S, id: &str) -> S::EdgeId {
    s.get_edge_by_id(id).unwrap().unwrap()
}

/// The vector property `key` of the vertex `id`.
fn vector<S: GraphStorage>(s: &S, id: &str, key: &str) -> Option<ElementProperty> {
    s.vertex_properties(v(s, id))
        .unwrap()
        .and_then(|map| map.get(key).cloned())
}

#[track_caller]
fn check(layer: &LayeredGraph) {
    if let Err(problem) = layer.check_layer_indexes() {
        panic!("{problem}");
    }
}

/// The number of base elements the layer holds an image of, and of added elements.
fn delta_counts(layer: &LayeredGraph) -> (usize, usize, usize) {
    let diff = layer.layer_diff(&LayerDiffRequest::new()).unwrap();
    (diff.added.len(), diff.removed.len(), diff.modified.len())
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

    fn last(&self) -> ChangeSet {
        self.0.lock().unwrap().last().cloned().expect("a commit")
    }
}

// ---------------------------------------------------------------------------------------------
// Writes on the layer and on a TraversalGraph with the same data
// ---------------------------------------------------------------------------------------------

/// The write kinds, each one unit.
const KINDS: &[&str] = &[
    "base_text",
    "base_same_text",
    "base_drop_text",
    "base_other_key",
    "base_label",
    "base_edge_note",
    "base_edge_drop_note",
    "added_doc",
    "added_doc_written_twice",
    "added_edge",
    "added_then_relabeled",
    "dropped_after_write",
];

fn write<S: GraphStorage>(s: &mut S, kind: &str) -> Result<(), GraphError> {
    match kind {
        "base_text" => s.set_vertex_property(v(s, "d1"), "text".into(), "a new text".into()),
        "base_same_text" => {
            s.set_vertex_property(v(s, "d2"), "text".into(), "vector search".into())
        }
        "base_drop_text" => s.drop_vertex_property("text", v(s, "d3")),
        "base_other_key" => s.set_vertex_property(v(s, "d4"), "name".into(), "renamed".into()),
        "base_label" => s.add_vertex_label(v(s, "p1"), "doc".into()).map(|_| ()),
        "base_edge_note" => s.set_edge_property(e(s, "r1"), "note".into(), "new note".into()),
        "base_edge_drop_note" => s.drop_edge_property("note", e(s, "r1")),
        "added_doc" => s
            .insert_vertex(
                Some("n1".into()),
                vec!["doc".into()],
                props(&[("text", "a fresh document".into())]),
            )
            .map(|_| ()),
        "added_doc_written_twice" => {
            let n = s.insert_vertex(
                Some("n2".into()),
                vec!["doc".into()],
                props(&[("text", "first".into())]),
            )?;
            s.set_vertex_property(n, "text".into(), "second".into())
        }
        "added_edge" => {
            let (from, to) = (v(s, "d5"), v(s, "d1"));
            s.insert_edge(
                Some("n3".into()),
                Some("rel".into()),
                from,
                to,
                props(&[("note", "a new relation".into())]),
            )
            .map(|_| ())
        }
        "added_then_relabeled" => {
            let n = s.insert_vertex(
                Some("n4".into()),
                vec!["page".into()],
                props(&[("text", "becomes a doc".into())]),
            )?;
            s.add_vertex_label(n, "doc".into()).map(|_| ())
        }
        "dropped_after_write" => {
            let d4 = v(s, "d4");
            s.set_vertex_property(d4, "text".into(), "gone soon".into())?;
            s.drop_vertex(d4)
        }
        other => panic!("unknown write kind {other}"),
    }
}

/// Runs `kind` in one unit under a [`Keeper`] and returns the committed change set.
fn committed<S: GraphStorage>(s: &mut S, kind: &str) -> ChangeSet {
    let keeper = Keeper::on(s);
    let mark = s.begin_unit().unwrap();
    write(s, kind).unwrap();
    s.commit_unit(mark).unwrap();
    keeper.last()
}

/// Creating a layer over a base with rules embeds nothing: the layer is empty, and reads the
/// base's vectors in place.
#[test]
fn a_layer_over_a_ruled_base_is_free() {
    let base = ruled_base();
    let layer = LayeredGraph::over(Arc::clone(&base)).expect("a base with vector rules");
    assert_eq!(delta_counts(&layer), (0, 0, 0));
    assert_eq!(snapshot(&layer), snapshot(&*base));
    assert!(vector(&layer, "d1", "emb").is_some());
    assert!(layer.capabilities().unwrap().layered);
    assert!(!base.capabilities().unwrap().layered);
}

/// Every write kind on a layer over the ruled base and on a `TraversalGraph` with the same
/// data: the same data after the unit (the derived vectors included) and the same change set.
#[test]
fn every_write_embeds_like_a_traversal_graph() {
    let base = ruled_base();
    let mut differences = Vec::new();
    for kind in KINDS {
        let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
        let mut reference = materialize(&layer);
        assert_eq!(snapshot(&layer), snapshot(&reference), "{kind}: start");
        let on_layer = committed(&mut layer, kind);
        let on_reference = committed(&mut reference, kind);
        check(&layer);
        assert_eq!(snapshot(&layer), snapshot(&reference), "{kind}: the data");
        if on_layer.mutations() != on_reference.mutations() {
            differences.push(format!(
                "{kind}\n  TraversalGraph: {:#?}\n  LayeredGraph:   {:#?}",
                on_reference.mutations(),
                on_layer.mutations()
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// The kinds that embed write the vector a `TraversalGraph` writes; the kinds that remove the
/// source remove the stale vector.
#[test]
fn derived_values_follow_the_sources() {
    let base = ruled_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let before = vector(&layer, "d1", "emb");
    committed(&mut layer, "base_text");
    assert_ne!(vector(&layer, "d1", "emb"), before);
    assert!(vector(&layer, "d1", "emb").is_some());
    committed(&mut layer, "base_drop_text");
    assert_eq!(vector(&layer, "d3", "emb"), None);
    committed(&mut layer, "base_label");
    assert!(vector(&layer, "p1", "emb").is_some());
    committed(&mut layer, "added_doc");
    assert!(vector(&layer, "n1", "emb").is_some());
    // The base keeps its own vectors.
    assert_eq!(vector(&*base, "d1", "emb"), before);
    assert!(vector(&*base, "d3", "emb").is_some());
    assert_eq!(vector(&*base, "p1", "emb"), None);
}

/// A write of an identical text re-derives an identical vector: nothing is copied up, the
/// layer stays empty; a write of another key copies up only the element written, and the
/// pass adds nothing to it.
#[test]
fn an_identical_text_copies_nothing_up() {
    let base = ruled_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let changes = committed(&mut layer, "base_same_text");
    assert_eq!(delta_counts(&layer), (0, 0, 0));
    // The change set reports both writes (as a TraversalGraph does), with equal values.
    let keys: Vec<&str> = changes
        .mutations()
        .iter()
        .filter_map(|m| match m {
            Mutation::SetProperty { key, .. } => Some(key.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(keys, ["text", "emb"]);
    committed(&mut layer, "base_other_key");
    assert_eq!(delta_counts(&layer), (0, 0, 1));
    let diff = layer.layer_diff(&LayerDiffRequest::new()).unwrap();
    let names: Vec<&str> = diff.modified[0]
        .properties
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["name"]);
}

/// The embedder fails (a cancelled scope): the whole unit is rolled back on the layer exactly
/// as on a `TraversalGraph`: the data, the index overlays, nothing in the diff, no commit.
#[test]
fn a_failing_embedder_rolls_the_unit_back() {
    let base = {
        let mut base = base_with(Some(rules()));
        base.set_definition(Definition::property_index(
            "doc_text",
            PropertyIndexDefinition::new(ElementKind::Vertex, "doc", "text"),
        ))
        .unwrap();
        Arc::new(base)
    };
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = materialize(&layer);
    let before = snapshot(&layer);
    let seq = layer.last_commit_seq().unwrap();
    let mut errors = Vec::new();
    for storage in [&mut layer as &mut dyn FnMutUnit, &mut reference] {
        let token = CancelToken::new();
        token.cancel();
        errors.push(token.scope(|| storage.unit_of("base_text")).unwrap_err());
    }
    assert_eq!(errors[0].to_string(), errors[1].to_string());
    assert!(errors[0].to_string().contains("cancelled"), "{}", errors[0]);
    assert_eq!(snapshot(&layer), before);
    assert_eq!(snapshot(&reference), before);
    assert_eq!(delta_counts(&layer), (0, 0, 0));
    assert_eq!(layer.last_commit_seq().unwrap(), seq);
    check(&layer);
    // An added element written in the failing unit is gone too.
    let token = CancelToken::new();
    token.cancel();
    token.scope(|| layer.unit_of("added_doc")).unwrap_err();
    assert_eq!(delta_counts(&layer), (0, 0, 0));
    check(&layer);
}

/// One unit of a write kind, over any storage (to loop over a layer and a `TraversalGraph`).
trait FnMutUnit {
    fn unit_of(&mut self, kind: &str) -> Result<(), GraphError>;
}

impl<S: GraphStorage> FnMutUnit for S {
    fn unit_of(&mut self, kind: &str) -> Result<(), GraphError> {
        let mark = self.begin_unit()?;
        match write(self, kind) {
            Ok(()) => self.commit_unit(mark),
            Err(error) => {
                self.rollback_unit(mark, &RollbackReason::failed(&error))?;
                Err(error)
            }
        }
    }
}

/// A direct write or removal of a derived property is refused on the layer with the error a
/// `TraversalGraph` returns, for base and added elements, vertices and edges, plain and path
/// writes and creates; another label's property of the same name is free. Outside a unit
/// (rules not live) both accept the write.
#[test]
fn direct_writes_of_a_derived_property_are_refused_alike() {
    type Attempt<S> = fn(&mut S) -> Result<(), GraphError>;
    fn attempts<S: GraphStorage>() -> Vec<(&'static str, Attempt<S>)> {
        vec![
            ("set base", |s: &mut S| {
                s.set_vertex_property(v(s, "d1"), "emb".into(), "x".into())
            }),
            ("drop base", |s: &mut S| {
                s.drop_vertex_property("emb", v(s, "d1"))
            }),
            ("drop absent on d5", |s: &mut S| {
                s.drop_vertex_property("emb", v(s, "d5"))
            }),
            ("set path", |s: &mut S| {
                let path: JsonPath = "emb[0]".parse().unwrap();
                s.set_vertex_property_path(v(s, "d1"), &path, 0.5.into())
            }),
            ("remove path", |s: &mut S| {
                let path: JsonPath = "emb[0]".parse().unwrap();
                s.remove_vertex_property_path(v(s, "d1"), &path)
            }),
            ("set edge", |s: &mut S| {
                s.set_edge_property(e(s, "r1"), "nemb".into(), "x".into())
            }),
            ("drop edge", |s: &mut S| {
                s.drop_edge_property("nemb", e(s, "r1"))
            }),
            ("insert", |s: &mut S| {
                s.insert_vertex(
                    None,
                    vec!["doc".into()],
                    props(&[("text", "t".into()), ("emb", "x".into())]),
                )
                .map(|_| ())
            }),
            ("insert edge", |s: &mut S| {
                let (a, b) = (v(s, "d1"), v(s, "d2"));
                s.insert_edge(
                    None,
                    Some("rel".into()),
                    a,
                    b,
                    props(&[("nemb", "x".into())]),
                )
                .map(|_| ())
            }),
            ("added element", |s: &mut S| {
                let n = s.insert_vertex(None, vec!["doc".into()], props(&[]))?;
                s.set_vertex_property(n, "emb".into(), "x".into())
            }),
            ("relabeled base", |s: &mut S| {
                let p1 = v(s, "p1");
                s.add_vertex_label(p1, "doc".into())?;
                s.set_vertex_property(p1, "emb".into(), "x".into())
            }),
            ("other label", |s: &mut S| {
                s.set_vertex_property(v(s, "o1"), "emb".into(), "x".into())
            }),
            ("bad value first", |s: &mut S| {
                s.set_vertex_property(v(s, "d1"), "emb".into(), ElementProperty::Null)
            }),
        ]
    }
    let base = ruled_base();
    let layer_attempts = attempts::<LayeredGraph>();
    let reference_attempts = attempts::<TraversalGraph>();
    let mut refused = 0;
    for ((what, on_layer), (_, on_reference)) in layer_attempts.iter().zip(&reference_attempts) {
        let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
        let mut reference = materialize(&layer);
        let mark = layer.begin_unit().unwrap();
        let l = on_layer(&mut layer);
        layer
            .rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        let mark = reference.begin_unit().unwrap();
        let r = on_reference(&mut reference);
        reference
            .rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        match (&l, &r) {
            (Ok(()), Ok(())) => {}
            (Err(a), Err(b)) => {
                assert_eq!(
                    std::mem::discriminant(a),
                    std::mem::discriminant(b),
                    "{what}: {a:?} vs {b:?}"
                );
                assert_eq!(a.to_string(), b.to_string(), "{what}");
                if matches!(a, GraphError::DerivedProperty { .. }) {
                    refused += 1;
                }
            }
            _ => panic!("{what}: layer {l:?}, TraversalGraph {r:?}"),
        }
        check(&layer);
        assert_eq!(delta_counts(&layer), (0, 0, 0), "{what}");
    }
    assert!(refused >= 10, "{refused} refusals");
    // Outside a unit the rules are not live: both store the write as given.
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = materialize(&layer);
    layer
        .set_vertex_property(v(&layer, "d1"), "emb".into(), "x".into())
        .unwrap();
    reference
        .set_vertex_property(v(&reference, "d1"), "emb".into(), "x".into())
        .unwrap();
    assert_eq!(snapshot(&layer), snapshot(&reference));
}

/// A dry run on the layer shows the vectors its writes would get (the same change set as a
/// `TraversalGraph`'s dry run) and leaves nothing; a failing embedder is the dry run's error.
#[test]
fn a_dry_run_shows_the_derived_vectors() {
    let base = ruled_base();
    for kind in ["base_text", "added_doc", "base_drop_text"] {
        let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
        let mut reference = materialize(&layer);
        let before = snapshot(&layer);
        let mark = layer.begin_unit().unwrap();
        write(&mut layer, kind).unwrap();
        let on_layer = layer.discard_unit(mark).unwrap();
        let mark = reference.begin_unit().unwrap();
        write(&mut reference, kind).unwrap();
        let on_reference = reference.discard_unit(mark).unwrap();
        assert_eq!(on_layer.mutations(), on_reference.mutations(), "{kind}");
        assert!(
            on_layer
                .mutations()
                .iter()
                .any(|m| matches!(m, Mutation::SetProperty { key, .. }
                    | Mutation::RemoveProperty { key, .. } if key == "emb")),
            "{kind}: {on_layer:?}"
        );
        assert_eq!(snapshot(&layer), before, "{kind}");
        assert_eq!(delta_counts(&layer), (0, 0, 0), "{kind}");
        check(&layer);
    }
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mark = layer.begin_unit().unwrap();
    write(&mut layer, "base_text").unwrap();
    let token = CancelToken::new();
    token.cancel();
    let err = token.scope(|| layer.discard_unit(mark)).unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert_eq!(delta_counts(&layer), (0, 0, 0));
    // The traversal-level dry run as well.
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let ((), changes) = layer
        .dry_run(|l| -> Result<(), GraphError> { write(l, "base_text") })
        .unwrap();
    assert!(
        changes
            .mutations()
            .iter()
            .any(|m| matches!(m, Mutation::SetProperty { key, .. } if key == "emb"))
    );
    assert_eq!(delta_counts(&layer), (0, 0, 0));
}

// ---------------------------------------------------------------------------------------------
// A rule added by a schema write on the layer: the full fill and its preview
// ---------------------------------------------------------------------------------------------

/// A base without rules (d5 holds a stale `emb`); the layer adds a doc of its own, then the
/// rules: the preview counts the base elements the fill copies up, the fill equals a
/// `TraversalGraph`'s, and the layer holds exactly that many images afterwards.
#[test]
fn a_rule_on_a_base_label_fills_like_a_traversal_graph_and_the_preview_counts_copies() {
    let base = {
        let mut base = base_with(None);
        base.set_vertex_property(
            GraphStorage::get_vertex_by_id(&base, "d5")
                .unwrap()
                .unwrap(),
            "emb".into(),
            ElementProperty::Array(vec![ElementProperty::from(1.0)]),
        )
        .unwrap();
        Arc::new(base)
    };
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    committed(&mut layer, "added_doc");
    assert_eq!(delta_counts(&layer), (1, 0, 0));
    let mut reference = materialize(&layer);

    // The preview: d1-d4 get a vector, d5 loses its stale one, r1 gets a vector (r2 has no
    // note): 6 base elements copied up; the added doc n1 is not counted.
    let validation = GraphTraversalSource::new(&layer)
        .validate_schema(&rules())
        .unwrap();
    let copies: Vec<(String, u64, u64, u64)> = validation
        .fill
        .iter()
        .map(|f| {
            (
                f.label().to_string(),
                f.elements(),
                f.removals(),
                f.base_copies(),
            )
        })
        .collect();
    assert_eq!(
        copies,
        [("doc".to_string(), 5, 1, 5), ("rel".to_string(), 1, 0, 1)]
    );
    let warning = validation.fill_warning().unwrap();
    assert!(
        warning.contains("This is a layer: the fill copies 6 base elements into it"),
        "{warning}"
    );
    println!("{warning}");
    // The same preview on the TraversalGraph counts no copies and says nothing about them.
    let reference_validation = reference.traversal().validate_schema(&rules()).unwrap();
    assert!(
        reference_validation
            .fill
            .iter()
            .all(|f| f.base_copies() == 0)
    );
    assert!(
        !reference_validation
            .fill_warning()
            .unwrap()
            .contains("This is a layer")
    );
    // The approximate preview (statistics): the label's base elements.
    let diff = GraphTraversalSource::new(&layer)
        .diff_schema(&rules())
        .unwrap();
    let approximate: Vec<u64> = diff.fill.iter().map(|f| f.base_copies()).collect();
    assert_eq!(approximate, [5, 2]);
    let warning = graphersal::schema::fill_warning(&diff.fill).unwrap();
    assert!(
        warning.contains("copies up to 7 base elements"),
        "{warning}"
    );

    // The fill.
    let on_layer_keeper = Keeper::on(&mut layer);
    let on_reference_keeper = Keeper::on(&mut reference);
    layer.set_schema(rules()).unwrap();
    reference.set_schema(rules()).unwrap();
    check(&layer);
    assert_eq!(snapshot(&layer), snapshot(&reference));
    let sorted = |changes: ChangeSet| {
        let mut rows: Vec<String> = changes
            .mutations()
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        rows.sort();
        rows
    };
    assert_eq!(
        sorted(on_layer_keeper.last()),
        sorted(on_reference_keeper.last())
    );
    assert_eq!(delta_counts(&layer), (1, 0, 6));
    // The base is untouched.
    assert_eq!(base.get_schema().unwrap(), None);
    assert!(vector(&*base, "d1", "emb").is_none());
}

/// A rule the layer adds is filled in a cancelled scope: the schema change and every vector
/// are rolled back, nothing is copied up.
#[test]
fn a_failing_fill_leaves_nothing() {
    let base = Arc::new(base_with(None));
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let token = CancelToken::new();
    token.cancel();
    let err = token.scope(|| layer.set_schema(rules())).unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert_eq!(layer.get_schema().unwrap(), None);
    assert_eq!(delta_counts(&layer), (0, 0, 0));
    check(&layer);
    // Later writes are plain again (the rules went with the schema).
    committed(&mut layer, "base_text");
    assert!(vector(&layer, "d1", "emb").is_none());
}

/// A rule naming a model the layer does not serve is refused as on a `TraversalGraph`.
#[test]
fn a_rule_without_its_model_is_refused_alike() {
    let schema = GraphSchema::from_json(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "text": {"type": "string"},
            "emb": {"type": "array", "format": "vector", "embedder": "acme/missing", "from": "text"}}}}}}"#,
    )
    .unwrap();
    let base = Arc::new(base_with(None));
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = materialize(&layer);
    let on_layer = layer.set_schema(schema.clone()).unwrap_err();
    let on_reference = reference.set_schema(schema).unwrap_err();
    assert_eq!(on_layer.to_string(), on_reference.to_string());
    assert!(
        matches!(on_layer, GraphError::VectorRuleModelMissing { .. }),
        "{on_layer:?}"
    );
    assert_eq!(layer.get_schema().unwrap(), None);
}

/// Removing the rules on the layer keeps the vectors and makes the property writable; a
/// rollback of the schema change brings the rules back.
#[test]
fn schema_changes_and_their_rollback_refresh_the_rules() {
    let base = ruled_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mark = layer.begin_unit().unwrap();
    layer.set_schema(GraphSchema::default()).unwrap();
    layer
        .set_vertex_property(v(&layer, "d1"), "emb".into(), "free".into())
        .unwrap();
    layer
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(layer.get_schema().unwrap(), base.get_schema().unwrap());
    let mark = layer.begin_unit().unwrap();
    let err = layer
        .set_vertex_property(v(&layer, "d1"), "emb".into(), "x".into())
        .unwrap_err();
    assert!(matches!(err, GraphError::DerivedProperty { .. }), "{err:?}");
    layer
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(delta_counts(&layer), (0, 0, 0));
}

// ---------------------------------------------------------------------------------------------
// apply_change_set and the journal
// ---------------------------------------------------------------------------------------------

/// A change set applied to a layer over a ruled base takes its vectors as given (no refusal,
/// no recomputation: the change set below carries for n9 a vector that is no embedding of its
/// text), as the same change set applied to a `TraversalGraph` with the layer's data does;
/// inside an outer unit its range is exempt from the pass, while the outer unit's own writes
/// are embedded.
#[test]
fn apply_change_set_is_exempt_like_a_traversal_graph() {
    let base = ruled_base();
    let carried = vector(&*base, "d2", "emb").expect("d2 has a vector");
    // The source: a graph without rules, whose unit adds n9 with that vector.
    let mut source = TraversalGraph::new();
    let keeper = Keeper::on(&mut source);
    let mark = source.begin_unit().unwrap();
    source
        .insert_vertex(
            Some("n9".into()),
            vec!["doc".into()],
            props(&[("text", "carried".into()), ("emb", carried.clone())]),
        )
        .unwrap();
    source.commit_unit(mark).unwrap();
    let changes = keeper.last();

    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = materialize(&layer);
    layer.apply_change_set(&changes).unwrap();
    reference.apply_change_set(&changes).unwrap();
    assert_eq!(snapshot(&layer), snapshot(&reference));
    assert_eq!(vector(&layer, "n9", "emb"), Some(carried.clone()));
    check(&layer);

    // Inside an outer unit: the applied range is exempt, the outer write is embedded.
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = materialize(&layer);
    for storage in [&mut layer as &mut dyn ApplyInUnit, &mut reference] {
        storage.apply_in_unit(&changes);
    }
    assert_eq!(snapshot(&layer), snapshot(&reference));
    assert_eq!(vector(&layer, "n9", "emb"), Some(carried));
    assert_ne!(vector(&layer, "d1", "emb"), vector(&*base, "d1", "emb"));
    check(&layer);
}

trait ApplyInUnit {
    fn apply_in_unit(&mut self, changes: &ChangeSet);
}

impl<S: GraphStorage> ApplyInUnit for S {
    fn apply_in_unit(&mut self, changes: &ChangeSet) {
        let mark = self.begin_unit().unwrap();
        self.apply_change_set(changes).unwrap();
        write(self, "base_text").unwrap();
        self.commit_unit(mark).unwrap();
    }
}

/// A fill on a `Graph<LayeredGraph>` journals its start and end into the layer graph's
/// journal, as on a `Graph<TraversalGraph>`; an ordinary write journals nothing.
#[test]
fn a_fill_on_a_layer_graph_is_journaled() {
    let layer = LayeredGraph::over(Arc::new(base_with(None))).unwrap();
    let graph = Graph::new(layer);
    GraphTraversalSource::new(&mut *graph.write())
        .set_schema(rules())
        .unwrap();
    let schema_commit = graph.read().last_commit_seq().unwrap();
    GraphTraversalSource::new(&mut *graph.write())
        .v(None)
        .has_id("d1")
        .property("text", "new text")
        .to_list()
        .unwrap();
    let records = graph
        .system_journal()
        .unwrap()
        .query(&graphersal::system_journal::JournalQuery::new())
        .unwrap()
        .records;
    let fills: Vec<_> = records
        .iter()
        .filter(|r| r.kind.starts_with("vector_fill."))
        .collect();
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_eq!(fills[0].kind, "vector_fill.started");
    assert_eq!(fills[1].kind, "vector_fill.done");
    assert_eq!(fills[1].op, Some(fills[0].seq));
    assert_eq!(fills[1].commit, Some(schema_commit));
    assert!(graph.vector_fill_progress().is_none());
}
