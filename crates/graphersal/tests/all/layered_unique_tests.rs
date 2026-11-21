//! Unique property indexes on a `LayeredGraph` (layered-graph plan S4.3): a base with unique
//! indexes carries a layer, and every write of the layer is checked over base + delta through
//! the index overlays, with the error a `TraversalGraph` holding the layer's data returns for
//! the same write (same variant, same text, same precedence among the write checks). A value a
//! base element dropped or changed is free again; a unique index the base keeps disabled stays
//! disabled; a unit failing a unique check leaves nothing.

use std::sync::Arc;

use graphersal::catalog::{Definition, PropertyIndexDefinition, PropertyIndexTransform};
use graphersal::prelude::*;
use graphersal::storage::{
    CreateContext, ElementKind, GraphStorage, JsonPath, LayeredGraph, RollbackReason,
};

const OPEN: &str = r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
    "properties": {"name": {"type": "string"}, "age": {"type": "integer"}}}}}}"#;

fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in pairs {
        map.insert(*key, value.clone());
    }
    map
}

/// The unique indexes of the fixture: person `name` (`to_lower`), the label-less vertex
/// `code`, `knows` `weight` and the label-less edge `tag`; plus `dup_name`, a unique index over
/// the repeated names of the `dup` vertices (stored, disabled).
fn unique_definitions() -> Vec<Definition> {
    [
        (
            "person_name",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name")
                .with_transform(Some(PropertyIndexTransform::ToLower)),
        ),
        (
            "code",
            PropertyIndexDefinition::label_less(ElementKind::Vertex, "code"),
        ),
        (
            "knows_weight",
            PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight"),
        ),
        (
            "tag",
            PropertyIndexDefinition::label_less(ElementKind::Edge, "tag"),
        ),
        (
            "dup_name",
            PropertyIndexDefinition::new(ElementKind::Vertex, "dup", "name"),
        ),
    ]
    .into_iter()
    .map(|(name, definition)| Definition::property_index(name, definition.with_unique(true)))
    .collect()
}

/// The base: three people, a software, a `thing` named like a person, two `dup` vertices with
/// the same name, three edges; the unique indexes of [`unique_definitions`]; with `schema`
/// first when given.
fn unique_base(schema: Option<&str>) -> Arc<TraversalGraph> {
    let mut base = TraversalGraph::new();
    if let Some(schema) = schema {
        base.set_schema(GraphSchema::from_json(schema).unwrap())
            .unwrap();
    }
    let mut vertex = |id: &str, label: &str, pairs: &[(&str, ElementProperty)]| {
        base.insert_vertex(Some(id.into()), vec![label.into()], props(pairs))
            .unwrap()
    };
    let p1 = vertex(
        "p1",
        "person",
        &[("name", "Ann".into()), ("code", "c1".into())],
    );
    let p2 = vertex(
        "p2",
        "person",
        &[("name", "Bob".into()), ("code", "c2".into())],
    );
    let p3 = vertex("p3", "person", &[("name", "Cy".into())]);
    let s1 = vertex(
        "s1",
        "software",
        &[("name", "lop".into()), ("code", "c4".into())],
    );
    vertex("t1", "thing", &[("name", "ann".into())]);
    vertex("u1", "dup", &[("name", "same".into())]);
    vertex("u2", "dup", &[("name", "same".into())]);
    let mut edge = |id: &str, label: &str, from, to, pairs: &[(&str, ElementProperty)]| {
        base.insert_edge(Some(id.into()), Some(label.into()), from, to, props(pairs))
            .unwrap();
    };
    edge(
        "k1",
        "knows",
        p1,
        p2,
        &[("weight", 1_i64.into()), ("tag", "t1".into())],
    );
    edge(
        "k2",
        "knows",
        p2,
        p3,
        &[("weight", 0.5.into()), ("tag", "t2".into())],
    );
    edge(
        "c1",
        "created",
        p1,
        s1,
        &[("weight", 0.5.into()), ("tag", "t3".into())],
    );
    for definition in unique_definitions() {
        base.set_definition(definition).unwrap();
    }
    base.check_property_indexes().unwrap();
    let names: Vec<String> = base
        .property_indexes()
        .unwrap()
        .into_iter()
        .map(|info| info.name)
        .collect();
    assert_eq!(names, ["person_name", "code", "knows_weight", "tag"]);
    assert!(base.unique_index_duplicates("dup_name").unwrap().is_some());
    Arc::new(base)
}

/// One write, by ids.
#[derive(Debug, Clone)]
enum Write {
    InsertV(Option<&'static str>, Vec<&'static str>, PropertyMap),
    AddV(Vec<&'static str>, PropertyMap),
    InsertE(
        Option<&'static str>,
        &'static str,
        &'static str,
        &'static str,
        PropertyMap,
    ),
    AddE(&'static str, &'static str, &'static str, PropertyMap),
    SetVP(&'static str, &'static str, ElementProperty),
    SetEP(&'static str, &'static str, ElementProperty),
    SetVPath(&'static str, &'static str, ElementProperty),
    SetEPath(&'static str, &'static str, ElementProperty),
    RemoveVPath(&'static str, &'static str),
    AddLabel(&'static str, &'static str),
    RemoveLabel(&'static str, &'static str),
    Relabel(&'static str, &'static str),
    DropV(&'static str),
    DropE(&'static str),
    DropVP(&'static str, &'static str),
}

fn apply<S: GraphStorage>(storage: &mut S, write: &Write) -> Result<(), GraphError> {
    let vertex = |storage: &S, id: &str| storage.get_vertex_by_id(id).unwrap().unwrap();
    let edge = |storage: &S, id: &str| storage.get_edge_by_id(id).unwrap().unwrap();
    let labels = |labels: &[&str]| labels.iter().map(|l| (*l).into()).collect::<Vec<_>>();
    let path = |text: &str| text.parse::<JsonPath>().unwrap();
    let context = CreateContext::for_storage(&*storage).unwrap();
    match write {
        Write::InsertV(id, l, map) => storage
            .insert_vertex(id.map(Into::into), labels(l), map.clone())
            .map(drop),
        Write::AddV(l, map) => storage
            .add_vertex_with(None, labels(l), map.clone(), &context)
            .map(drop),
        Write::InsertE(id, label, from, to, map) => {
            let (from, to) = (vertex(storage, from), vertex(storage, to));
            storage
                .insert_edge(
                    id.map(Into::into),
                    Some((*label).into()),
                    from,
                    to,
                    map.clone(),
                )
                .map(drop)
        }
        Write::AddE(label, from, to, map) => {
            let (from, to) = (vertex(storage, from), vertex(storage, to));
            storage
                .add_edge_with(None, Some((*label).into()), from, to, map.clone(), &context)
                .map(drop)
        }
        Write::SetVP(id, key, value) => {
            let node = vertex(storage, id);
            storage.set_vertex_property(node, (*key).into(), value.clone())
        }
        Write::SetEP(id, key, value) => {
            let e = edge(storage, id);
            storage.set_edge_property(e, (*key).into(), value.clone())
        }
        Write::SetVPath(id, at, value) => {
            let node = vertex(storage, id);
            storage.set_vertex_property_path(node, &path(at), value.clone())
        }
        Write::SetEPath(id, at, value) => {
            let e = edge(storage, id);
            storage.set_edge_property_path(e, &path(at), value.clone())
        }
        Write::RemoveVPath(id, at) => {
            let node = vertex(storage, id);
            storage.remove_vertex_property_path(node, &path(at))
        }
        Write::AddLabel(id, label) => {
            let node = vertex(storage, id);
            storage.add_vertex_label(node, (*label).into()).map(drop)
        }
        Write::RemoveLabel(id, label) => {
            let node = vertex(storage, id);
            storage.remove_vertex_label(node, label).map(drop)
        }
        Write::Relabel(id, label) => {
            let e = edge(storage, id);
            storage.set_edge_label(e, (*label).into()).map(drop)
        }
        Write::DropV(id) => {
            let node = vertex(storage, id);
            storage.drop_vertex(node)
        }
        Write::DropE(id) => {
            let e = edge(storage, id);
            storage.drop_edge(e)
        }
        Write::DropVP(id, key) => {
            let node = vertex(storage, id);
            storage.drop_vertex_property(key, node)
        }
    }
}

/// What a write is expected to do.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Expect {
    Ok,
    /// A unique violation naming the index and the other element's id.
    Unique(&'static str, &'static str),
    /// Another error (a check that comes before the unique check).
    Other,
    /// With a schema [`Expect::Other`] (the schema check comes first), without it the
    /// unique violation.
    SchemaFirst(&'static str, &'static str),
}

/// The layer's changes before the cases: Bob renamed (frees "bob"), Cy dropped (frees "cy";
/// its edge k2 goes with it, freeing weight 0.5 and tag "t2"), lop's code changed (frees
/// "c4"), an added person Dee with code "c9" and an added edge d1 -> p1 (weight 2, tag "t9").
fn setup() -> Vec<Write> {
    vec![
        Write::SetVP("p2", "name", "Robert".into()),
        Write::DropV("p3"),
        Write::SetVP("s1", "code", "c5".into()),
        Write::InsertV(
            Some("d1"),
            vec!["person"],
            props(&[("name", "Dee".into()), ("code", "c9".into())]),
        ),
        Write::InsertE(
            Some("de"),
            "knows",
            "d1",
            "p1",
            props(&[("weight", 2_i64.into()), ("tag", "t9".into())]),
        ),
    ]
}

/// Every write kind `TraversalGraph` checks against unique indexes, against base values,
/// added values, values the layer freed, and with other checks that come first.
fn cases() -> Vec<(&'static str, Expect, Write)> {
    use Expect::{Ok, Other, SchemaFirst, Unique};
    let name = |text: &str| props(&[("name", text.into())]);
    vec![
        // Creates.
        (
            "insert, base name (lowered)",
            Unique("person_name", "p1"),
            Write::InsertV(None, vec!["person"], name("ANN")),
        ),
        (
            "insert, added name",
            Unique("person_name", "d1"),
            Write::InsertV(None, vec!["person"], name("dee")),
        ),
        (
            "insert, changed base name",
            Unique("person_name", "p2"),
            Write::InsertV(None, vec!["person"], name("robert")),
        ),
        (
            "insert, name freed by a change",
            Ok,
            Write::InsertV(Some("n1"), vec!["person"], name("bob")),
        ),
        (
            "insert, name freed by a drop",
            Ok,
            Write::InsertV(Some("n2"), vec!["person"], name("CY")),
        ),
        (
            "insert, another label",
            Ok,
            Write::InsertV(None, vec!["software"], name("Ann")),
        ),
        (
            "insert, label-less base code",
            Unique("code", "p1"),
            Write::InsertV(None, vec!["thing"], props(&[("code", "c1".into())])),
        ),
        (
            "insert, label-less image code",
            Unique("code", "s1"),
            Write::InsertV(None, vec![], props(&[("code", "c5".into())])),
        ),
        (
            "insert, label-less added code",
            Unique("code", "d1"),
            Write::InsertV(None, vec!["person"], props(&[("code", "c9".into())])),
        ),
        (
            "insert, label-less code freed",
            Ok,
            Write::InsertV(Some("n3"), vec!["thing"], props(&[("code", "c4".into())])),
        ),
        (
            "insert, label-less before per-label",
            Unique("code", "p2"),
            Write::InsertV(
                None,
                vec!["person"],
                props(&[("name", "ann".into()), ("code", "c2".into())]),
            ),
        ),
        (
            "insert, unique before the id",
            Unique("person_name", "p1"),
            Write::InsertV(Some("p2"), vec!["person"], name("ann")),
        ),
        (
            "insert, the id after a free value",
            Other,
            Write::InsertV(Some("p2"), vec!["person"], name("zed")),
        ),
        (
            "insert, the label check first",
            Other,
            Write::InsertV(None, vec!["a::b"], name("ann")),
        ),
        (
            "insert, null exempt",
            Ok,
            Write::InsertV(
                Some("z1"),
                vec!["thing"],
                props(&[("code", ElementProperty::Null)]),
            ),
        ),
        (
            "insert, null exempt twice",
            Ok,
            Write::InsertV(
                Some("z2"),
                vec!["thing"],
                props(&[("code", ElementProperty::Null)]),
            ),
        ),
        (
            "add, base name",
            Unique("person_name", "p1"),
            Write::AddV(vec!["person"], name("aNN")),
        ),
        (
            "edge insert, int equals float",
            Unique("knows_weight", "k1"),
            Write::InsertE(None, "knows", "p1", "s1", props(&[("weight", 1.0.into())])),
        ),
        (
            "edge insert, weight freed by a cascade",
            Ok,
            Write::InsertE(
                Some("n4"),
                "knows",
                "p1",
                "s1",
                props(&[("weight", 0.5.into())]),
            ),
        ),
        (
            "edge insert, added weight",
            Unique("knows_weight", "de"),
            Write::InsertE(None, "knows", "s1", "p1", props(&[("weight", 2.0.into())])),
        ),
        (
            "edge insert, label-less base tag",
            Unique("tag", "k1"),
            Write::InsertE(None, "created", "p2", "s1", props(&[("tag", "t1".into())])),
        ),
        (
            "edge insert, unique before the id",
            Unique("tag", "de"),
            Write::InsertE(
                Some("k1"),
                "created",
                "p2",
                "s1",
                props(&[("tag", "t9".into())]),
            ),
        ),
        (
            "edge insert, tag freed by a cascade",
            Ok,
            Write::InsertE(
                Some("n5"),
                "created",
                "p2",
                "s1",
                props(&[("tag", "t2".into())]),
            ),
        ),
        (
            "edge add, base tag",
            Unique("tag", "c1"),
            Write::AddE("likes", "p2", "p1", props(&[("tag", "t3".into())])),
        ),
        // Property writes.
        (
            "set, added name on a base vertex",
            Unique("person_name", "d1"),
            Write::SetVP("p1", "name", "DEE".into()),
        ),
        (
            "set, base name on an added vertex",
            Unique("person_name", "p1"),
            Write::SetVP("d1", "name", "ann".into()),
        ),
        (
            "set, its own name again",
            Ok,
            Write::SetVP("p1", "name", "ANN".into()),
        ),
        (
            "set, its own base value back",
            Ok,
            Write::SetVP("s1", "code", "c5".into()),
        ),
        (
            "set, a name an inserted vertex took",
            Unique("person_name", "n1"),
            Write::SetVP("p2", "name", "Bob".into()),
        ),
        (
            "insert, the schema check first",
            SchemaFirst("person_name", "p1"),
            Write::InsertV(
                None,
                vec!["person"],
                props(&[("name", "ann".into()), ("age", "x".into())]),
            ),
        ),
        (
            "edge set, base weight",
            Unique("knows_weight", "k1"),
            Write::SetEP("de", "weight", 1_i64.into()),
        ),
        (
            "edge set, label-less added tag",
            Unique("tag", "de"),
            Write::SetEP("c1", "tag", "t9".into()),
        ),
        (
            "edge set, another label's weight",
            Ok,
            Write::SetEP("c1", "weight", 2_i64.into()),
        ),
        (
            "path, base code",
            Unique("code", "p2"),
            Write::SetVPath("d1", "$.code", "c2".into()),
        ),
        (
            "path, added name",
            Unique("person_name", "d1"),
            Write::SetVPath("p2", "$.name", "dee".into()),
        ),
        (
            "path, an object is skipped",
            Ok,
            Write::SetVPath("t1", "$.code.inner", "c1".into()),
        ),
        (
            "path, the path check first",
            Other,
            Write::SetVPath("p1", "$.name.x", "dee".into()),
        ),
        (
            "edge path, base tag",
            Unique("tag", "k1"),
            Write::SetEPath("de", "$.tag", "t1".into()),
        ),
        (
            "path removal frees nothing new",
            Ok,
            Write::RemoveVPath("t1", "$.code.inner"),
        ),
        // Labels.
        (
            "label, a base name under the new label",
            Unique("person_name", "p1"),
            Write::AddLabel("t1", "person"),
        ),
        (
            "label, the label check first",
            Other,
            Write::AddLabel("t1", "x::y"),
        ),
        ("label, a free name", Ok, Write::AddLabel("s1", "person")),
        (
            "relabel, another label's weight now under the index",
            Unique("knows_weight", "de"),
            Write::Relabel("c1", "knows"),
        ),
        ("relabel away", Ok, Write::Relabel("de", "likes")),
        (
            "relabel back, the weight still free",
            Ok,
            Write::Relabel("de", "knows"),
        ),
        // Drops free values.
        (
            "label removal frees the name",
            Ok,
            Write::RemoveLabel("s1", "person"),
        ),
        (
            "insert, the name freed by the label removal",
            Ok,
            Write::InsertV(Some("n6"), vec!["person"], name("LOP")),
        ),
        ("drop an added edge", Ok, Write::DropE("de")),
        (
            "edge set, a tag freed by an edge drop",
            Ok,
            Write::SetEP("n4", "tag", "t9".into()),
        ),
        ("drop a base vertex", Ok, Write::DropV("p1")),
        (
            "insert, a name freed by the drop",
            Ok,
            Write::InsertV(Some("n7"), vec!["person"], name("ann")),
        ),
        (
            "label, now a name an inserted vertex holds",
            Unique("person_name", "n7"),
            Write::AddLabel("t1", "person"),
        ),
        ("drop a property", Ok, Write::DropVP("d1", "code")),
        (
            "set, a code freed by a property drop",
            Ok,
            Write::SetVP("p2", "code", "c9".into()),
        ),
        // A unique index the base keeps disabled is not enforced.
        (
            "disabled, a repeated value",
            Ok,
            Write::InsertV(None, vec!["dup"], name("same")),
        ),
    ]
}

/// The outcome of a write as compared: `Ok`, or the error's variant and text.
fn outcome(
    result: Result<(), GraphError>,
) -> Result<(), (std::mem::Discriminant<GraphError>, String)> {
    result.map_err(|err| (std::mem::discriminant(&err), err.to_string()))
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

/// A `TraversalGraph` with the layer's data, its index definitions and its schema.
fn materialize(layer: &LayeredGraph) -> TraversalGraph {
    let mut graph = crate::layered_index_tests::materialize(layer);
    if let Some(schema) = layer.get_schema().unwrap() {
        graph.set_schema(schema).unwrap();
    }
    graph
}

#[track_caller]
fn check(layer: &LayeredGraph) {
    if let Err(problem) = layer.check_layer_indexes() {
        panic!("{problem}");
    }
}

/// A layer over the unique base (with `schema`), the SETUP applied.
fn touched_layer(schema: Option<&str>) -> LayeredGraph {
    let mut layer = LayeredGraph::over(unique_base(schema)).expect("unique indexes are accepted");
    for write in setup() {
        apply(&mut layer, &write).unwrap();
    }
    check(&layer);
    layer
}

/// Every case on the touched layer and on a `TraversalGraph` with the layer's data (the
/// same indexes): the same outcome (variant and text) as expected, and the same data after
/// each; the overlays stay exact. Schema mode none (in-place path writes in
/// `TraversalGraph`) and open.
#[test]
fn every_write_kind_fails_like_the_traversal_graph() {
    for schema in [None, Some(OPEN)] {
        let mut layer = touched_layer(schema);
        let mut reference = materialize(&layer);
        reference.check_property_indexes().unwrap();
        for (what, expect, write) in cases() {
            let what = format!("{what} (schema {})", schema.is_some());
            let expect = match expect {
                Expect::SchemaFirst(..) if schema.is_some() => Expect::Other,
                Expect::SchemaFirst(index, existing) => Expect::Unique(index, existing),
                expect => expect,
            };
            let on_layer = outcome(apply(&mut layer, &write));
            let on_reference = outcome(apply(&mut reference, &write));
            assert_eq!(on_layer, on_reference, "{what}");
            match (expect, &on_layer) {
                (Expect::Ok, Ok(())) => {}
                (Expect::Other, Err((_, text))) => {
                    assert!(!text.contains("unique index"), "{what}: {text}");
                }
                (Expect::Unique(index, existing), Err((_, text))) => {
                    let Err(GraphError::UniqueViolation {
                        index: got_index,
                        existing: got_existing,
                        ..
                    }) = apply(&mut layer, &write)
                    else {
                        panic!("{what}: {text}");
                    };
                    assert_eq!(
                        (got_index.as_str(), got_existing.as_str()),
                        (index, existing)
                    );
                }
                (expect, got) => panic!("{what}: expected {expect:?}, got {got:?}"),
            }
            check(&layer);
            assert_eq!(snapshot(&layer), snapshot(&reference), "{what}");
        }
        reference.check_property_indexes().unwrap();
        layer.base().check_property_indexes().unwrap();
    }
}

/// An untouched layer accepts the base's unique indexes, lists them as the base does (the
/// disabled one neither listed nor enforced, its report forwarded) and enforces them.
#[test]
fn a_base_with_unique_indexes_carries_a_layer() {
    let base = unique_base(None);
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let of = |infos: Vec<graphersal::storage::PropertyIndexInfo>| {
        infos
            .into_iter()
            .map(|info| (info.name, info.unique))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of(layer.property_indexes().unwrap()),
        of(base.property_indexes().unwrap())
    );
    assert_eq!(
        layer.unique_index_duplicates("dup_name").unwrap(),
        base.unique_index_duplicates("dup_name").unwrap()
    );
    assert_eq!(layer.unique_index_duplicates("person_name").unwrap(), None);
    let mut reference = materialize(&layer);
    let write = Write::InsertV(None, vec!["person"], props(&[("name", "bob".into())]));
    let err = apply(&mut layer, &write).unwrap_err();
    assert_eq!(
        err.to_string(),
        apply(&mut reference, &write).unwrap_err().to_string()
    );
    assert_eq!(
        err.to_string(),
        "The unique index 'person_name' already holds \"bob\" at 'name' for the vertex 'p2'"
    );
    assert!(matches!(err, GraphError::UniqueViolation { .. }));
    check(&layer);
}

/// A unit that fails a unique check mid-way leaves nothing: the data, the overlays and the
/// values it freed before the failure are as before the unit (a value freed inside the unit
/// is taken again after the rollback).
#[test]
fn a_unit_failing_a_unique_check_leaves_nothing() {
    let mut layer = touched_layer(None);
    let before = snapshot(&layer);
    let mark = layer.begin_unit().unwrap();
    for write in [
        Write::DropV("p1"),
        Write::InsertV(Some("x1"), vec!["person"], props(&[("name", "ann".into())])),
        Write::SetVP("p2", "name", "Cy".into()),
        Write::SetVP("s1", "code", "c4".into()),
        Write::InsertE(
            Some("x2"),
            "knows",
            "x1",
            "p2",
            props(&[("weight", 1_i64.into())]),
        ),
    ] {
        apply(&mut layer, &write).unwrap();
    }
    check(&layer);
    let failed = apply(&mut layer, &Write::SetVP("d1", "name", "ANN".into()));
    assert!(
        matches!(&failed, Err(GraphError::UniqueViolation { existing, .. }) if existing == "x1"),
        "{failed:?}"
    );
    layer
        .rollback_unit(mark, &RollbackReason::failed(&"unique"))
        .unwrap();
    check(&layer);
    assert_eq!(snapshot(&layer), before);
    // The values the unit freed are held again, the ones it took are free.
    for (write, existing) in [
        (
            Write::InsertV(None, vec!["person"], props(&[("name", "ann".into())])),
            "p1",
        ),
        (Write::SetEP("de", "weight", 1_i64.into()), "k1"),
    ] {
        match apply(&mut layer, &write) {
            Err(GraphError::UniqueViolation { existing: got, .. }) => assert_eq!(got, existing),
            other => panic!("{write:?}: {other:?}"),
        }
    }
    match apply(&mut layer, &Write::SetVP("d1", "code", "c5".into())) {
        Err(GraphError::UniqueViolation { existing, .. }) => assert_eq!(existing, "s1"),
        other => panic!("{other:?}"),
    }
    apply(&mut layer, &Write::SetVP("d1", "name", "Cy".into())).unwrap();
    check(&layer);
}

/// A nested savepoint failing a unique check rolls back its part only; the outer unit
/// commits what it wrote before.
#[test]
fn a_nested_savepoint_failing_a_unique_check_rolls_back_its_part() {
    let mut layer = touched_layer(None);
    let outer = layer.begin_unit().unwrap();
    apply(&mut layer, &Write::SetVP("p1", "name", "Ada".into())).unwrap();
    let after_outer = snapshot(&layer);
    let inner = layer.begin_unit().unwrap();
    apply(&mut layer, &Write::SetVP("p2", "name", "ann".into())).unwrap();
    apply(&mut layer, &Write::DropV("d1")).unwrap();
    let failed = apply(&mut layer, &Write::SetVP("s1", "name", "ADA".into()));
    assert!(failed.is_ok(), "software is not under person_name");
    let failed = apply(&mut layer, &Write::AddLabel("s1", "person"));
    assert!(
        matches!(&failed, Err(GraphError::UniqueViolation { existing, .. }) if existing == "p1"),
        "{failed:?}"
    );
    layer
        .rollback_unit(inner, &RollbackReason::failed(&"unique"))
        .unwrap();
    assert_eq!(snapshot(&layer), after_outer);
    check(&layer);
    layer.commit_unit(outer).unwrap();
    // "ann" is free (p1 is Ada now), "dee" is held again by d1.
    apply(
        &mut layer,
        &Write::InsertV(None, vec!["person"], props(&[("name", "ANN".into())])),
    )
    .unwrap();
    match apply(&mut layer, &Write::SetVP("p2", "name", "dee".into())) {
        Err(GraphError::UniqueViolation { existing, .. }) => assert_eq!(existing, "d1"),
        other => panic!("{other:?}"),
    }
    check(&layer);
}

/// Through the DSL: a traversal that conflicts mid-way fails with the text a
/// `TraversalGraph` with the same data gives, and leaves nothing on the layer.
#[cfg(feature = "script")]
#[test]
fn dsl_conflicts_read_like_the_traversal_graph() {
    use graphersal::storage::Graph;
    let layer = touched_layer(Some(OPEN));
    let reference = materialize(&layer);
    let before = snapshot(&layer);
    let layer = Arc::new(Graph::new(layer));
    let reference = Arc::new(Graph::new(reference));
    let scripts = [
        r#"g.add_v("person").property("name", "Zed").iterate(); g.add_v("person").property("name", "ann").iterate()"#,
        r#"g.add_v("person").property("name", "Zed").add_v("person").property("name", "zed").iterate()"#,
        r#"g.v("d1").property("name", "ROBERT").iterate()"#,
        r#"g.v("d1").property("code", "c1").iterate()"#,
        r#"g.v("t1").add_label("person").iterate()"#,
        r#"g.e("de").property("tag", "t1").iterate()"#,
        r#"g.v("p1").add_e("knows").to(__.v("s1")).property("weight", 1.0).iterate()"#,
        r#"g.v().has_label("person").property("code", "same").iterate()"#,
    ];
    for script in scripts {
        let on_layer = crate::script_host::eval_value_atomic(
            Arc::clone(&layer),
            script,
            std::collections::HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .map(|_| ())
        .map_err(|err| err.to_string());
        let on_reference = crate::script_host::eval_value_atomic(
            Arc::clone(&reference),
            script,
            std::collections::HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .map(|_| ())
        .map_err(|err| err.to_string());
        assert_eq!(on_layer, on_reference, "{script}");
        let text = on_layer.expect_err(script);
        assert!(text.contains("unique index"), "{script}: {text}");
        check(&layer.read());
        assert_eq!(snapshot(&*layer.read()), before, "{script}");
    }
}

/// A base whose uniquely indexed values are stored compressed (an lz4 rule on `doc.body`) and
/// coded (a `dedup` rule on `doc.kind`), each key under a unique index (S4.3 note): the layer
/// compares the logical keys, so every write conflicts, or not, exactly as on a `TraversalGraph`
/// built the same way (same data, rules and indexes) given the same writes: same outcome
/// (variant and text, the other element's id resolved), same logical data after each.
#[test]
fn compressed_and_coded_values_compare_by_their_logical_key() {
    use graphersal::catalog::{Codec, CompressionDefinition};
    fn long(seed: usize) -> ElementProperty {
        (0..120)
            .map(|i| format!("word{} ", (seed * 7 + i) % 13))
            .collect::<String>()
            .into()
    }
    fn graph() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        for (id, kind, body) in [
            ("d1", Some("alpha"), Some(1)),
            ("d2", Some("beta"), Some(2)),
            ("d3", None, None),
        ] {
            let mut map = PropertyMap::default();
            if let Some(kind) = kind {
                map.insert("kind", kind.into());
            }
            if let Some(seed) = body {
                map.insert("body", long(seed));
            }
            graph
                .insert_vertex(Some(id.into()), vec!["doc".into()], map)
                .unwrap();
        }
        graph
            .insert_vertex(
                Some("n1".into()),
                vec!["note".into()],
                props(&[("kind", "alpha".into()), ("body", long(2))]),
            )
            .unwrap();
        let mut g = graph.traversal_mut();
        g.define_compression(
            "kind",
            CompressionDefinition::new(ElementKind::Vertex, "doc", "kind")
                .unwrap()
                .with_codec(Codec::Dedup),
        )
        .unwrap();
        g.define_compression(
            "body",
            CompressionDefinition::new(ElementKind::Vertex, "doc", "body")
                .unwrap()
                .with_dictionary(false),
        )
        .unwrap();
        drop(g);
        for (name, key) in [("doc_kind", "kind"), ("doc_body", "body")] {
            graph
                .set_definition(Definition::property_index(
                    name,
                    PropertyIndexDefinition::new(ElementKind::Vertex, "doc", key).with_unique(true),
                ))
                .unwrap();
        }
        graph.check_property_indexes().unwrap();
        graph
    }
    /// Every vertex (id, labels, logical properties), sorted.
    fn logical<S: GraphStorage>(storage: &S) -> Vec<String> {
        let mut rows: Vec<String> = storage
            .vertices()
            .unwrap()
            .map(|node| {
                let mut properties: Vec<String> = storage
                    .vertex_properties(node)
                    .unwrap()
                    .map(|map| {
                        map.iter()
                            .map(|(k, v)| format!("{k}={:?}", v.to_logical()))
                            .collect()
                    })
                    .unwrap_or_default();
                properties.sort();
                format!(
                    "{} {:?} {properties:?}",
                    storage.vertex_id(node).unwrap().unwrap(),
                    storage.vertex_labels(node).unwrap()
                )
            })
            .collect();
        rows.sort();
        rows
    }
    let base = Arc::new(graph());
    let stored = |id: &str| {
        let node = base.get_vertex_by_id(id).unwrap().unwrap();
        base.vertex_properties(node).unwrap().unwrap().clone()
    };
    assert!(
        matches!(
            stored("d1").get("body"),
            Some(ElementProperty::Compressed(_))
        ),
        "the base stores the body compressed"
    );
    assert!(base.is_dedup_coded(ElementKind::Vertex, "d1", "kind"));
    let mut layer = LayeredGraph::over(Arc::clone(&base)).expect("accepted");
    let mut reference = graph();
    let cases: Vec<(&str, Expect, Write)> = vec![
        (
            "a compressed base value taken",
            Expect::Unique("doc_body", "d1"),
            Write::SetVP("d3", "body", long(1)),
        ),
        (
            "a coded base value taken",
            Expect::Unique("doc_kind", "d1"),
            Write::SetVP("d3", "kind", "alpha".into()),
        ),
        (
            "an added element with a compressed base value",
            Expect::Unique("doc_body", "d2"),
            Write::InsertV(Some("d4"), vec!["doc"], props(&[("body", long(2))])),
        ),
        (
            "the same value written again on its holder",
            Expect::Ok,
            Write::SetVP("d1", "body", long(1)),
        ),
        (
            "a value only another label holds",
            Expect::Ok,
            Write::SetVP("d3", "body", long(5)),
        ),
        (
            "the holder changes its value",
            Expect::Ok,
            Write::SetVP("d1", "body", long(9)),
        ),
        (
            "the freed compressed value",
            Expect::Ok,
            Write::InsertV(Some("d5"), vec!["doc"], props(&[("body", long(1))])),
        ),
        (
            "the new value of the base holder, now a layer image",
            Expect::Unique("doc_body", "d1"),
            Write::SetVP("d3", "body", long(9)),
        ),
        (
            "a label added over a coded base value",
            Expect::Unique("doc_kind", "d1"),
            Write::AddLabel("n1", "doc"),
        ),
        ("the coded holder dropped", Expect::Ok, Write::DropV("d1")),
        (
            "its coded value free again",
            Expect::Ok,
            Write::SetVP("d3", "kind", "alpha".into()),
        ),
        (
            "an added element taking a coded value of the layer",
            Expect::Unique("doc_kind", "d3"),
            Write::InsertV(Some("d6"), vec!["doc"], props(&[("kind", "alpha".into())])),
        ),
        (
            "the coded base value still held",
            Expect::Unique("doc_kind", "d2"),
            Write::SetVP("d5", "kind", "beta".into()),
        ),
    ];
    for (what, expect, write) in cases {
        let on_layer = outcome(apply(&mut layer, &write));
        let on_reference = outcome(apply(&mut reference, &write));
        assert_eq!(on_layer, on_reference, "{what}");
        match (expect, &on_layer) {
            (Expect::Ok, Ok(())) => {}
            (Expect::Unique(index, existing), Err((_, text))) => {
                assert!(text.contains("unique index"), "{what}: {text}");
                assert!(text.contains(index), "{what}: {text}");
                assert!(text.contains(existing), "{what}: {text}");
            }
            (expect, got) => panic!("{what}: expected {expect:?}, got {got:?}"),
        }
        check(&layer);
        assert_eq!(logical(&layer), logical(&reference), "{what}");
    }
    reference.check_property_indexes().unwrap();
    layer.base().check_property_indexes().unwrap();
}
