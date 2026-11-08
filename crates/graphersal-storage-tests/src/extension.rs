//! Capability-conditional contract items (`book/src/users_guide/transactions.md`).
//!
//! Each function reads [`GraphStorage::capabilities`] first and checks only what the storage
//! claims: the rollback items run for an `atomic` storage, the hook items for one with
//! `change_capture`; a storage without the capability gets the negative side of the contract
//! instead (a hook registration is refused). The macros generate them for every storage, so a
//! storage that starts claiming a capability is tested for it automatically.

use graphersal::prelude::PropertyMap;
use graphersal::schema::{GraphSchema, SchemaMode};
use graphersal::{ElementProperty, error::GraphError};
use graphersal::{
    catalog::{Definition, DefinitionKind, QueryDefinition},
    changes::{
        ChangeSet, CommitHook, CommitMetadata, EdgeState, HookError, Mutation, RollbackReason,
        VertexState,
    },
    storage::{GraphStorage, StorageCapabilities, Transactional},
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::contract::{EventCounter, assert_consistent, edge, props, sample_query, vertex};

/// Everything observable through the trait, sorted, for before/after comparisons.
type Snapshot = (
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

fn snapshot<S: GraphStorage>(storage: &S) -> Snapshot {
    let sorted = |element: Option<&graphersal::prelude::PropertyMap>| {
        let mut out: Vec<(String, ElementProperty)> = element
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    };
    let mut vertices: Vec<_> = storage
        .vertices()
        .unwrap()
        .map(|v| {
            (
                storage
                    .vertex_id(v)
                    .unwrap()
                    .unwrap_or_default()
                    .to_string(),
                storage
                    .vertex_labels(v)
                    .unwrap()
                    .iter()
                    .map(|l| l.to_string())
                    .collect(),
                sorted(storage.vertex_properties(v).unwrap()),
            )
        })
        .collect();
    vertices.sort_by(|a, b| a.0.cmp(&b.0));
    let mut edges: Vec<_> = storage
        .edges()
        .unwrap()
        .map(|e| {
            let (from, to) = storage
                .edge_endpoints(e)
                .unwrap()
                .unwrap_or_else(|| panic!("edge {e:?} has no endpoints"));
            (
                storage.edge_id(e).unwrap().unwrap_or_default().to_string(),
                storage.edge_label(e).unwrap().map(str::to_string),
                storage
                    .vertex_id(from)
                    .unwrap()
                    .unwrap_or_default()
                    .to_string(),
                storage
                    .vertex_id(to)
                    .unwrap()
                    .unwrap_or_default()
                    .to_string(),
                sorted(storage.edge_properties(e).unwrap()),
            )
        })
        .collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    (vertices, edges, storage.get_schema().unwrap())
}

/// A small populated graph: `a -knows-> b`, both `person`.
fn populate<S: GraphStorage>(storage: &mut S) -> (S::VertexId, S::VertexId, S::EdgeId) {
    let a = vertex(
        storage,
        Some("a"),
        &["person"],
        props(&[("name", "ann".into())]),
    );
    let b = vertex(storage, Some("b"), &["person"], props(&[("age", 3.into())]));
    let e = edge(
        storage,
        Some("ab"),
        Some("knows"),
        a,
        b,
        props(&[("w", 1.into())]),
    );
    (a, b, e)
}

/// The claims fit together: `change_capture` requires `atomic`, `writable` excludes
/// `immutable`.
pub fn capabilities_are_consistent<S: GraphStorage>(storage: &mut S) {
    let caps = storage.capabilities().unwrap();
    assert!(
        caps.is_consistent(),
        "inconsistent capabilities {caps:?}: change_capture requires atomic"
    );
}

/// `atomic`: rolling a unit back restores everything observable, schema included (when the
/// storage supports schemas); the indexes stay consistent.
pub fn rollback_restores_everything<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().atomic {
        return;
    }
    let (a, b, e) = populate(storage);
    let before = snapshot(storage);
    let mark = storage.begin_unit().unwrap();
    let c = vertex(storage, Some("c"), &["x"], props(&[]));
    edge(storage, Some("ca"), None, c, a, props(&[]));
    storage
        .set_vertex_property(a, "name".into(), "ANN".into())
        .expect("set");
    storage.drop_vertex_property("age", b).expect("drop prop");
    storage.add_vertex_label(a, "admin".into()).expect("label");
    storage
        .set_edge_property(e, "w".into(), 2.into())
        .expect("set edge prop");
    // A storage without schema support (`Unsupported`, the trait default) skips this write.
    match storage.set_schema(GraphSchema::new().with_mode(SchemaMode::Open)) {
        Ok(()) | Err(GraphError::Unsupported { .. }) => {}
        Err(other) => panic!("set_schema in a unit failed: {other:?}"),
    }
    storage.drop_vertex(b).expect("drop vertex");
    assert_consistent(storage);
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_eq!(snapshot(storage), before, "a rollback restores everything");
    assert_consistent(storage);
}

/// `atomic`: a nested unit is a savepoint; rolling it back keeps what the outer unit did before
/// it, and committing the outer unit keeps that.
pub fn savepoint_rollback_keeps_the_outer_unit<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().atomic {
        return;
    }
    let outer = storage.begin_unit().unwrap();
    vertex(storage, Some("kept"), &[], props(&[]));
    let after_outer = snapshot(storage);
    let inner = storage.begin_unit().unwrap();
    vertex(storage, Some("undone"), &[], props(&[]));
    storage
        .rollback_unit(inner, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_eq!(snapshot(storage), after_outer);
    storage.commit_unit(outer).expect("commit");
    assert!(storage.get_vertex_by_id("kept").unwrap().is_some());
    assert!(storage.get_vertex_by_id("undone").unwrap().is_none());
    assert_consistent(storage);
}

/// `atomic`: `unit_changes` is part of the atomic contract. It reports a change made in an
/// open unit (also for every enclosing unit), never a change that is not there (a fresh unit, a
/// read, a savepoint that was rolled back), and not for a fresh savepoint; a schema change is
/// reported as `schema`, a data change as `data`. An atomic storage that keeps the trait's
/// default (nothing) fails here.
pub fn unit_changes_never_claims_an_undone_change<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().atomic {
        return;
    }
    let outer = storage.begin_unit().unwrap();
    assert!(
        !storage.unit_changes(outer).unwrap().any(),
        "a fresh unit has no changes"
    );
    let _ = storage.vertex_count().unwrap();
    assert!(
        !storage.unit_changes(outer).unwrap().any(),
        "a read is not a change"
    );
    let inner = storage.begin_unit().unwrap();
    vertex(storage, Some("undone"), &[], props(&[]));
    assert!(
        storage.unit_changes(inner).unwrap().any(),
        "an atomic storage must report a change of an open unit (the trait default `false` \
         does not meet the atomic contract)"
    );
    assert!(
        storage.unit_changes(outer).unwrap().any(),
        "a change of a savepoint is a change of the enclosing unit"
    );
    storage
        .rollback_unit(inner, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert!(
        !storage.unit_changes(outer).unwrap().any(),
        "a rolled-back savepoint leaves no change"
    );
    vertex(storage, Some("kept"), &[], props(&[]));
    assert!(
        storage.unit_changes(outer).unwrap().data,
        "a kept data change is reported as data"
    );
    let fresh = storage.begin_unit().unwrap();
    assert!(
        !storage.unit_changes(fresh).unwrap().any(),
        "a fresh savepoint has no changes"
    );
    storage.commit_unit(fresh).expect("commit");
    storage.commit_unit(outer).expect("commit");
    assert_consistent(storage);
}

/// A hook for the contract: records accepted change sets and rollbacks, vetoes while told to.
struct ContractHook {
    accepted: Arc<Mutex<Vec<ChangeSet>>>,
    rollbacks: Arc<Mutex<usize>>,
    veto: Arc<Mutex<bool>>,
}

impl CommitHook for ContractHook {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        if *self.veto.lock().map_err(|_| "poisoned")? {
            return Err("contract veto".into());
        }
        self.accepted
            .lock()
            .map_err(|_| "poisoned")?
            .push(changes.clone());
        Ok(())
    }

    fn after_rollback(&mut self, _reason: &RollbackReason) {
        if let Ok(mut count) = self.rollbacks.lock() {
            *count += 1;
        }
    }
}

/// `change_capture`: a committed unit reaches the hook as one change set with a monotonic
/// commit sequence number; a unit that changed nothing emits nothing; a veto rolls the unit back
/// and reaches `after_rollback`. Without `change_capture` a registration is refused.
pub fn commit_hooks_follow_the_capability<S: GraphStorage>(storage: &mut S) {
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let rollbacks = Arc::new(Mutex::new(0));
    let veto = Arc::new(Mutex::new(false));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: rollbacks.clone(),
        veto: veto.clone(),
    };
    let registered = storage.register_commit_hook(Box::new(hook));
    if !storage.capabilities().unwrap().change_capture {
        assert!(
            registered.is_err(),
            "a storage without change_capture must refuse commit hooks"
        );
        return;
    }
    registered.expect("change_capture: hooks are supported");

    let start = storage.last_commit_seq().unwrap();
    let mark = storage.begin_unit().unwrap();
    let (a, _, _) = populate(storage);
    storage.commit_unit(mark).expect("commit");
    let mark = storage.begin_unit().unwrap();
    storage
        .set_vertex_property(a, "name".into(), "ANN".into())
        .expect("set");
    storage.commit_unit(mark).expect("commit");
    // A unit that changes nothing.
    let mark = storage.begin_unit().unwrap();
    storage.commit_unit(mark).expect("commit");
    let sets = accepted.lock().map(|s| s.clone()).unwrap_or_default();
    let seqs: Vec<u64> = sets.iter().map(ChangeSet::commit_seq).collect();
    assert_eq!(
        seqs,
        [start + 1, start + 2],
        "one change set per changing unit"
    );
    assert_eq!(storage.last_commit_seq().unwrap(), start + 2);
    assert_eq!(sets[0].len(), 3, "two vertices and one edge");
    assert!(matches!(
        sets[1].mutations(),
        [Mutation::SetProperty { .. }]
    ));

    // A veto rolls back.
    if let Ok(mut v) = veto.lock() {
        *v = true;
    }
    let before = snapshot(storage);
    let mark = storage.begin_unit().unwrap();
    vertex(storage, Some("vetoed"), &[], props(&[]));
    assert!(matches!(
        storage.commit_unit(mark),
        Err(GraphError::CommitRejected { .. })
    ));
    assert_eq!(
        snapshot(storage),
        before,
        "a vetoed unit leaves nothing behind"
    );
    assert_eq!(storage.last_commit_seq().unwrap(), start + 2);
    assert_eq!(rollbacks.lock().map(|c| *c).unwrap_or_default(), 1);
    assert_consistent(storage);
}

/// `change_capture`: applying a captured change set to the state it was recorded against
/// reproduces the recorded end state.
pub fn applied_change_set_reproduces_the_unit<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().change_capture {
        return;
    }
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: Arc::default(),
        veto: Arc::default(),
    };
    storage
        .register_commit_hook(Box::new(hook))
        .expect("change_capture: hooks are supported");
    let mark = storage.begin_unit().unwrap();
    let (a, b, _) = populate(storage);
    storage
        .set_vertex_property(a, "k".into(), 1.into())
        .expect("set");
    storage.drop_vertex(b).expect("drop");
    storage.commit_unit(mark).expect("commit");
    let recorded = snapshot(storage);
    let set = accepted
        .lock()
        .ok()
        .and_then(|s| s.first().cloned())
        .expect("one change set");
    // Undo by hand through the trait, then re-apply.
    if let Some(v) = storage.get_vertex_by_id("a").unwrap() {
        storage.drop_vertex(v).expect("drop");
    }
    assert_eq!(storage.vertex_count().unwrap(), 0);
    storage.apply_change_set(&set).expect("apply");
    assert_eq!(snapshot(storage), recorded);
    assert_consistent(storage);
}

/// `writable`: after [`GraphStorage::apply_change_set`] a new auto id never reuses an id the
/// change set's source handed out, also one whose add and drop were compacted away or that a
/// rolled-back unit of the source took: the storage raises its sequences to the change set's
/// ([`ChangeSet::node_sequence`], [`ChangeSet::edge_sequence`]) through
/// [`GraphStorage::raise_id_sequences`]. A storage without numeric auto ids passes as long as
/// its auto ids avoid the source's.
pub fn applied_change_set_raises_the_id_sequences<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().writable {
        return;
    }
    // The source: a reference graph whose unit hands out vertex ids 1-3 and edge ids 1-2, of
    // which only vertex 1 survives the compaction.
    let mut source = graphersal::TraversalGraph::new();
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: Arc::default(),
        veto: Arc::default(),
    };
    source
        .register_commit_hook(Box::new(hook))
        .expect("the reference graph captures changes");
    let mark = source.begin_unit().unwrap();
    let one = vertex(&mut source, None, &[], props(&[]));
    let two = vertex(&mut source, None, &[], props(&[]));
    let three = vertex(&mut source, None, &[], props(&[]));
    edge(&mut source, None, Some("e"), one, two, props(&[]));
    edge(&mut source, None, Some("e"), two, three, props(&[]));
    source.drop_vertex(two).expect("drop");
    source.drop_vertex(three).expect("drop");
    source.commit_unit(mark).expect("commit");
    let set = accepted
        .lock()
        .ok()
        .and_then(|s| s.first().cloned())
        .expect("one change set")
        .compact();
    assert_eq!(set.len(), 1, "only vertex 1 survives the compaction");

    storage.apply_change_set(&set).expect("apply");
    let used_vertices = ["1", "2", "3"];
    let used_edges = ["1", "2"];
    let mark = storage.begin_unit().unwrap();
    let first = vertex(storage, None, &[], props(&[]));
    let second = vertex(storage, None, &[], props(&[]));
    let link = edge(storage, None, Some("e"), first, second, props(&[]));
    storage.commit_unit(mark).expect("commit");
    for v in [first, second] {
        let id = storage
            .vertex_id(v)
            .unwrap()
            .unwrap_or_default()
            .to_string();
        assert!(
            !used_vertices.contains(&id.as_str()),
            "auto vertex id {id} reuses an id of the applied change set's source"
        );
    }
    let id = storage
        .edge_id(link)
        .unwrap()
        .unwrap_or_default()
        .to_string();
    assert!(
        !used_edges.contains(&id.as_str()),
        "auto edge id {id} reuses an id of the applied change set's source"
    );
    assert_consistent(storage);
}

/// The catalog as `(kind, name, definition)`, for before/after comparisons.
fn catalog<S: GraphStorage>(storage: &S) -> Vec<Definition> {
    storage.definitions().unwrap().cloned().collect()
}

/// Whether the storage keeps a catalog (its `set_definition` is not the unsupported default).
fn has_catalog<S: GraphStorage>(storage: &mut S) -> bool {
    let mark = storage.begin_unit().unwrap();
    let supported = !matches!(
        storage.set_definition(sample_query("probe")),
        Err(GraphError::Unsupported { .. })
    );
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract probe"))
        .unwrap();
    supported
}

/// The ids of the vertices a lookup answered (`None`: not served), sorted.
fn vertex_ids<S: GraphStorage>(
    storage: &S,
    found: Option<Box<dyn Iterator<Item = S::VertexId> + '_>>,
) -> Option<Vec<String>> {
    let mut ids: Vec<String> = found?
        .map(|v| {
            storage
                .vertex_id(v)
                .unwrap()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    ids.sort();
    Some(ids)
}

/// `property_indexes` with a catalog: a LABEL-LESS index (`PropertyIndexDefinition::label_less`,
/// kind `property_index_ext`, critical) holds every vertex of the kind, unlabeled ones
/// included, and follows writes and rollbacks; `vertices_by_property_index(None, ..)` reads it
/// (a superset, each vertex once), a per-label lookup of the same key is not answered by it, and
/// `count_by_property_index`, when the storage answers, is the exact count. `property_indexes`
/// lists it with `label: None`. A label-less edge index the same for edges. Skipped without the
/// capability.
pub fn label_less_property_index_serves_every_element<S: GraphStorage>(storage: &mut S) {
    use graphersal::catalog::PropertyIndexDefinition;
    use graphersal::storage::{ElementKind, PropertyIndexRange};
    let caps = storage.capabilities().unwrap();
    if !caps.property_indexes || !caps.atomic || !has_catalog(storage) {
        return;
    }
    let a = vertex(storage, Some("ll_a"), &["a"], props(&[("k", "x".into())]));
    let _b = vertex(storage, Some("ll_b"), &["b"], props(&[("k", "y".into())]));
    let _bare = vertex(storage, Some("ll_bare"), &[], props(&[("k", "x".into())]));
    let _none = vertex(storage, Some("ll_none"), &["a"], PropertyMap::default());
    let e = edge(
        storage,
        Some("ll_e"),
        Some("r"),
        a,
        a,
        props(&[("o", "x".into())]),
    );
    let index = Definition::property_index(
        "k_any",
        PropertyIndexDefinition::label_less(ElementKind::Vertex, "k"),
    );
    assert_eq!(index.kind(), DefinitionKind::PROPERTY_INDEX_EXT);
    assert!(index.flags().is_critical());
    storage.set_definition(index).expect("a label-less index");
    storage
        .set_definition(Definition::property_index(
            "o_any",
            PropertyIndexDefinition::label_less(ElementKind::Edge, "o"),
        ))
        .expect("a label-less edge index");
    let x = PropertyIndexRange::equal("x".into());
    let listed = storage.property_indexes().unwrap();
    assert!(
        listed
            .iter()
            .any(|info| info.name == "k_any" && info.label.is_none() && info.key == "k"),
        "{listed:?}"
    );
    let found = |storage: &S| {
        vertex_ids(
            storage,
            storage.vertices_by_property_index(None, "k", &x).unwrap(),
        )
    };
    let mut expected = vec!["ll_a".to_string(), "ll_bare".to_string()];
    assert_eq!(
        found(storage),
        Some(expected.clone()),
        "unlabeled ones included"
    );
    assert!(
        storage
            .vertices_by_property_index(Some("a"), "k", &x)
            .unwrap()
            .is_none(),
        "no per-label index on a.k"
    );
    if let Some(count) = storage
        .count_by_property_index(ElementKind::Vertex, None, "k", None, &x)
        .unwrap()
    {
        assert_eq!(count, 2);
    }
    let edges: Option<Vec<S::EdgeId>> = storage
        .edges_by_property_index(None, "o", &x)
        .unwrap()
        .map(Iterator::collect);
    assert_eq!(edges, Some(vec![e]));
    // A write moves the entry; a rollback restores it.
    let mark = storage.begin_unit().unwrap();
    storage
        .set_vertex_property(a, "k".into(), "y".into())
        .unwrap();
    assert_eq!(found(storage), Some(vec!["ll_bare".to_string()]));
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_eq!(found(storage), Some(expected.clone()));
    let late = vertex(
        storage,
        Some("ll_late"),
        &["c", "d"],
        props(&[("k", "x".into())]),
    );
    expected.push("ll_late".to_string());
    assert_eq!(found(storage), Some(expected), "a multi-label vertex once");
    storage.drop_vertex(late).unwrap();
    assert!(
        storage
            .remove_definition(DefinitionKind::PROPERTY_INDEX_EXT, "k_any")
            .unwrap()
    );
    assert!(
        storage
            .vertices_by_property_index(None, "k", &x)
            .unwrap()
            .is_none(),
        "a dropped index serves nothing"
    );
}

/// `property_indexes` with a catalog: an index with the transform `each` (kind
/// `property_index_ext`, critical, also per label) holds one entry per array element and a
/// scalar value as itself; `vertices_by_transformed_property_index` reads it with each vertex
/// once (D15: an array holding several values of a range, or `1` and `1.0`, is one candidate),
/// follows writes and rollbacks, and `property_indexes` lists the transform and the skipped
/// elements (a nested array, an object). `count_by_property_index`, when the storage answers
/// for a one-value slice, is the exact count. Skipped without the capability.
pub fn each_property_index_holds_array_elements<S: GraphStorage>(storage: &mut S) {
    use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexTransform};
    use graphersal::storage::{ElementKind, PropertyIndexRange};
    use std::ops::Bound;
    let caps = storage.capabilities().unwrap();
    if !caps.property_indexes || !caps.atomic || !has_catalog(storage) {
        return;
    }
    let list = |items: &[ElementProperty]| ElementProperty::Array(items.to_vec());
    let mut object = PropertyMap::default();
    object.insert("o", "x".into());
    let a = vertex(
        storage,
        Some("each_a"),
        &["t"],
        props(&[("tags", list(&["x".into(), "y".into(), "x".into()]))]),
    );
    let _b = vertex(
        storage,
        Some("each_b"),
        &["t"],
        props(&[("tags", "x".into())]),
    );
    let _c = vertex(
        storage,
        Some("each_c"),
        &["t"],
        props(&[(
            "tags",
            list(&[
                list(&["x".into()]),
                ElementProperty::Object(Box::new(object)),
                1.into(),
                1.5.into(),
            ]),
        )]),
    );
    let _other = vertex(
        storage,
        Some("each_u"),
        &["u"],
        props(&[("tags", list(&["x".into()]))]),
    );
    let each = PropertyIndexTransform::Each;
    let index = Definition::property_index(
        "t_tags_each",
        PropertyIndexDefinition::new(ElementKind::Vertex, "t", "tags")
            .with_transform(Some(each.clone())),
    );
    assert_eq!(index.kind(), DefinitionKind::PROPERTY_INDEX_EXT);
    assert!(index.flags().is_critical());
    storage.set_definition(index).expect("an each index");
    let listed = storage.property_indexes().unwrap();
    let info = listed
        .iter()
        .find(|info| info.name == "t_tags_each")
        .unwrap_or_else(|| panic!("{listed:?}"));
    assert_eq!(info.transform, Some(each.clone()));
    assert_eq!(info.stats.skipped, 2, "the nested array and the object");
    let found = |storage: &S, range: &PropertyIndexRange| {
        vertex_ids(
            storage,
            storage
                .vertices_by_transformed_property_index(Some("t"), "tags", &each, range)
                .unwrap(),
        )
    };
    let x = PropertyIndexRange::equal("x".into());
    assert_eq!(
        found(storage, &x),
        Some(vec!["each_a".to_string(), "each_b".to_string()])
    );
    // A range over two values of one array: the vertex once.
    let a_to_z = PropertyIndexRange::new(Bound::Included("a".into()), Bound::Unbounded);
    assert_eq!(
        found(storage, &a_to_z),
        Some(vec!["each_a".to_string(), "each_b".to_string()])
    );
    let numbers = PropertyIndexRange::new(Bound::Included(0.into()), Bound::Included(2.into()));
    assert_eq!(found(storage, &numbers), Some(vec!["each_c".to_string()]));
    if let Some(count) = storage
        .count_by_property_index(ElementKind::Vertex, Some("t"), "tags", Some(&each), &x)
        .unwrap()
    {
        assert_eq!(count, 2);
    }
    assert!(
        storage
            .vertices_by_property_index(Some("t"), "tags", &x)
            .unwrap()
            .is_none(),
        "an each index is not the plain index of the key"
    );
    // A write moves the entries; a rollback restores them.
    let mark = storage.begin_unit().unwrap();
    storage
        .set_vertex_property(a, "tags".into(), list(&["z".into()]))
        .unwrap();
    assert_eq!(found(storage, &x), Some(vec!["each_b".to_string()]));
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_eq!(
        found(storage, &x),
        Some(vec!["each_a".to_string(), "each_b".to_string()])
    );
    assert!(
        storage
            .remove_definition(DefinitionKind::PROPERTY_INDEX_EXT, "t_tags_each")
            .unwrap()
    );
}

/// `exact_property_index_lookups`: for an equality slice of a string, boolean or uuid value,
/// the lookups of a per-label and a label-less index without a transform return exactly the
/// vertices whose value at the key equals it, and of an index with the transform `each`
/// exactly those whose value is it or an array holding it; each vertex once, never more (the
/// optimizer drops the re-check of such a condition). The data mixes the look-alikes a
/// superset could hold: other kinds (`"1"`, `1`, `1.0`), other case, arrays (with the value
/// twice, nested, beside objects), `null`, a missing key, multi-label and unlabeled vertices.
/// Checked after writes and a rollback too. Skipped without the capability.
pub fn exact_property_index_lookups_return_exactly<S: GraphStorage>(storage: &mut S) {
    use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexTransform};
    use graphersal::storage::{ElementKind, PropertyIndexRange};
    let caps = storage.capabilities().unwrap();
    if !caps.exact_property_index_lookups
        || !caps.property_indexes
        || !caps.atomic
        || !has_catalog(storage)
    {
        return;
    }
    let list = |items: &[ElementProperty]| ElementProperty::Array(items.to_vec());
    let mut object = PropertyMap::default();
    object.insert("x", "x".into());
    let uuid = ElementProperty::Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
    let values: Vec<(&[&str], Option<ElementProperty>)> = vec![
        (&["p"], Some("x".into())),
        (&["p", "q"], Some("x".into())),
        (&[], Some("x".into())),
        (&["q"], Some("x".into())),
        (&["p"], Some("X".into())),
        (&["p"], Some("1".into())),
        (&["p"], Some(1.into())),
        (&["p"], Some(1.0.into())),
        (&["p"], Some(true.into())),
        (&["p"], Some("true".into())),
        (&["p"], Some(uuid.clone())),
        (&["p"], Some(list(&["x".into(), "x".into(), "y".into()]))),
        (&["q"], Some(list(&[list(&["x".into()]), true.into()]))),
        (
            &["p"],
            Some(list(&[ElementProperty::Object(Box::new(object.clone()))])),
        ),
        (&["p"], Some(ElementProperty::Object(Box::new(object)))),
        (&["p"], Some(list(&[uuid.clone(), "1".into()]))),
        (&["p"], Some(ElementProperty::Null)),
        (&["p"], None),
    ];
    let mut handles = Vec::new();
    for (n, (labels, value)) in values.into_iter().enumerate() {
        let id = format!("ex_{n:02}");
        let properties = match value {
            Some(value) => props(&[("ex", value)]),
            None => PropertyMap::default(),
        };
        handles.push(vertex(storage, Some(&id), labels, properties));
    }
    let each = PropertyIndexTransform::Each;
    for (name, definition) in [
        (
            "ex_p",
            PropertyIndexDefinition::new(ElementKind::Vertex, "p", "ex"),
        ),
        (
            "ex_any",
            PropertyIndexDefinition::label_less(ElementKind::Vertex, "ex"),
        ),
        (
            "ex_any_each",
            PropertyIndexDefinition::label_less(ElementKind::Vertex, "ex")
                .with_transform(Some(each.clone())),
        ),
    ] {
        storage
            .set_definition(Definition::property_index(name, definition))
            .expect("an index");
    }
    // Exactly what the conditions accept, from a scan.
    let expected = |storage: &S, label: Option<&str>, by_each: bool, wanted: &ElementProperty| {
        let mut ids: Vec<String> = Vec::new();
        for v in storage.vertices().unwrap() {
            if let Some(label) = label
                && !storage
                    .vertex_labels(v)
                    .unwrap()
                    .iter()
                    .any(|l| l.as_str() == label)
            {
                continue;
            }
            let properties = storage.vertex_properties(v).unwrap();
            let Some(value) = properties.as_ref().and_then(|p| p.get("ex")) else {
                continue;
            };
            let holds = value == wanted
                || (by_each
                    && matches!(value, ElementProperty::Array(items) if items.contains(wanted)));
            if holds {
                ids.push(
                    storage
                        .vertex_id(v)
                        .unwrap()
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        ids.sort();
        ids
    };
    let check = |storage: &S, stage: &str| {
        for wanted in [
            ElementProperty::from("x"),
            ElementProperty::from("1"),
            ElementProperty::from(true),
            uuid.clone(),
            ElementProperty::from("absent"),
        ] {
            let range = PropertyIndexRange::equal(wanted.clone());
            let plain = vertex_ids(
                storage,
                storage
                    .vertices_by_property_index(Some("p"), "ex", &range)
                    .unwrap(),
            );
            assert_eq!(
                plain,
                Some(expected(storage, Some("p"), false, &wanted)),
                "{stage}: per-label lookup of {wanted:?}"
            );
            let any = vertex_ids(
                storage,
                storage
                    .vertices_by_property_index(None, "ex", &range)
                    .unwrap(),
            );
            assert_eq!(
                any,
                Some(expected(storage, None, false, &wanted)),
                "{stage}: label-less lookup of {wanted:?}"
            );
            let by_each = vertex_ids(
                storage,
                storage
                    .vertices_by_transformed_property_index(None, "ex", &each, &range)
                    .unwrap(),
            );
            assert_eq!(
                by_each,
                Some(expected(storage, None, true, &wanted)),
                "{stage}: each lookup of {wanted:?}"
            );
        }
    };
    check(storage, "built");
    let mark = storage.begin_unit().unwrap();
    storage
        .set_vertex_property(handles[0], "ex".into(), list(&["x".into(), "z".into()]))
        .unwrap();
    storage
        .set_vertex_property(handles[6], "ex".into(), "x".into())
        .unwrap();
    storage.drop_vertex(handles[2]).unwrap();
    check(storage, "after writes");
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    check(storage, "after the rollback");
    for name in ["ex_p", "ex_any", "ex_any_each"] {
        let kind = if name == "ex_p" {
            DefinitionKind::PROPERTY_INDEX
        } else {
            DefinitionKind::PROPERTY_INDEX_EXT
        };
        assert!(storage.remove_definition(kind, name).unwrap(), "{name}");
    }
}

/// `property_indexes` with a catalog: a UNIQUE index (kind `property_index_ext`, critical) is
/// listed with `unique`; every write that would give a second element of its label a key it
/// holds fails with `GraphError::UniqueViolation` and changes nothing (an insert, a property
/// write, a label added); keys compare as the index keys them (`to_lower`: case-insensitive);
/// `null` and values the index does not hold (a number under `to_lower`) are exempt; an
/// element may write its own key again; a swap through an intermediate value passes; an index
/// stored over repeated values is neither listed nor enforced, and `unique_index_duplicates`
/// reports them. Skipped without the capability.
pub fn unique_property_index_refuses_repeated_keys<S: GraphStorage>(storage: &mut S) {
    use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexTransform};
    use graphersal::storage::ElementKind;
    let caps = storage.capabilities().unwrap();
    if !caps.property_indexes || !caps.atomic || !has_catalog(storage) {
        return;
    }
    let a = vertex(
        storage,
        Some("uq_a"),
        &["uq"],
        props(&[("email", "a@x".into())]),
    );
    let b = vertex(
        storage,
        Some("uq_b"),
        &["uq"],
        props(&[("email", "b@x".into())]),
    );
    vertex(
        storage,
        Some("uq_n1"),
        &["uq"],
        props(&[("email", ElementProperty::Null)]),
    );
    let other = vertex(
        storage,
        Some("uq_o"),
        &["uq_other"],
        props(&[("email", "A@x".into())]),
    );
    let index = Definition::property_index(
        "uq_email",
        PropertyIndexDefinition::new(ElementKind::Vertex, "uq", "email")
            .with_transform(Some(PropertyIndexTransform::ToLower))
            .with_unique(true),
    );
    assert_eq!(index.kind(), DefinitionKind::PROPERTY_INDEX_EXT);
    assert!(index.flags().is_critical());
    storage
        .set_definition(index.clone())
        .expect("a unique index");
    let listed = storage.property_indexes().unwrap();
    let info = listed
        .iter()
        .find(|info| info.name == "uq_email")
        .unwrap_or_else(|| panic!("{listed:?}"));
    assert!(info.unique, "{info:?}");
    assert_eq!(storage.unique_index_duplicates("uq_email").unwrap(), None);
    let email = |storage: &S, v: S::VertexId| {
        storage
            .vertex_properties(v)
            .unwrap()
            .and_then(|map| map.get("email"))
            .map(|value| value.to_logical())
    };
    let violation = |result: Result<(), GraphError>, what: &str| match result {
        Err(GraphError::UniqueViolation { index, .. }) => assert_eq!(index, "uq_email"),
        other => panic!("{what}: expected a unique violation, got {other:?}"),
    };
    // An insert repeating a key (case-insensitively) fails and adds nothing.
    violation(
        storage
            .insert_vertex(
                Some("uq_dup".into()),
                vec!["uq".into()],
                props(&[("email", "A@X".into())]),
            )
            .map(|_| ()),
        "insert",
    );
    assert!(storage.get_vertex_by_id("uq_dup").unwrap().is_none());
    // A property write repeating a key fails and keeps the old value.
    violation(
        storage.set_vertex_property(b, "email".into(), "a@X".into()),
        "set",
    );
    assert_eq!(email(storage, b), Some("b@x".into()));
    // A label that brings the vertex into the index fails, the labels stay.
    violation(
        storage.add_vertex_label(other, "uq".into()).map(|_| ()),
        "add label",
    );
    let labels: Vec<&str> = storage
        .vertex_labels(other)
        .unwrap()
        .iter()
        .map(|label| label.as_str())
        .collect();
    assert_eq!(labels, vec!["uq_other"]);
    // Exempt: its own key again, null, values a to_lower index does not hold.
    storage
        .set_vertex_property(b, "email".into(), "B@x".into())
        .expect("its own key");
    for id in ["uq_n2", "uq_5a", "uq_5b"] {
        let value = if id == "uq_n2" {
            ElementProperty::Null
        } else {
            5.into()
        };
        vertex(storage, Some(id), &["uq"], props(&[("email", value)]));
    }
    // A swap needs an intermediate value (the check is immediate).
    let mark = storage.begin_unit().unwrap();
    storage
        .set_vertex_property(a, "email".into(), "tmp@x".into())
        .unwrap();
    storage
        .set_vertex_property(b, "email".into(), "a@x".into())
        .unwrap();
    storage
        .set_vertex_property(a, "email".into(), "b@x".into())
        .unwrap();
    storage.commit_unit(mark).unwrap();
    assert_eq!(email(storage, a), Some("b@x".into()));
    // Stored over repeated values (no create check): neither listed nor enforced.
    storage
        .remove_definition(DefinitionKind::PROPERTY_INDEX_EXT, "uq_email")
        .unwrap();
    storage
        .set_vertex_property(b, "email".into(), "B@X".into())
        .unwrap();
    storage.set_definition(index).unwrap();
    assert!(
        storage
            .property_indexes()
            .unwrap()
            .iter()
            .all(|info| info.name != "uq_email")
    );
    let report = storage
        .unique_index_duplicates("uq_email")
        .unwrap()
        .expect("the repeated values");
    assert_eq!(report.total, 1);
    assert_eq!(report.values[0].count, 2);
    assert_eq!(report.values[0].value, "\"b@x\"");
    vertex(
        storage,
        Some("uq_c"),
        &["uq"],
        props(&[("email", "b@x".into())]),
    );
    assert!(
        storage
            .remove_definition(DefinitionKind::PROPERTY_INDEX_EXT, "uq_email")
            .unwrap()
    );
}

/// `set_definitions` stores a list in order exactly like one `set_definition` per item (a name
/// given twice: the last one wins); a refused item fails the call with its error, and inside a
/// unit an atomic storage's rollback keeps nothing of the list. Needs a catalog and `atomic`.
pub fn a_definition_list_is_stored_like_single_calls<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().atomic || !has_catalog(storage) {
        return;
    }
    let before = catalog(storage);
    let replaced = Definition::query("listed_x", QueryDefinition::new("fn listed_x() { 2 }"));
    storage
        .set_definitions(vec![
            sample_query("listed_x"),
            sample_query("listed_y"),
            replaced.clone(),
        ])
        .expect("a valid list is stored");
    assert_eq!(
        storage
            .definition(DefinitionKind::QUERY, "listed_x")
            .unwrap(),
        Some(&replaced),
        "the last definition of a name wins"
    );
    assert!(
        storage
            .definition(DefinitionKind::QUERY, "listed_y")
            .unwrap()
            .is_some()
    );
    assert_eq!(catalog(storage).len(), before.len() + 2);
    let stored = catalog(storage);
    let mark = storage.begin_unit().unwrap();
    let refused = storage.set_definitions(vec![
        sample_query("listed_z"),
        Definition::query("not valid", QueryDefinition::new("fn x() {}")),
    ]);
    assert!(
        matches!(refused, Err(GraphError::Definition(_))),
        "a refused item fails the list: {refused:?}"
    );
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_eq!(
        catalog(storage),
        stored,
        "the rollback keeps nothing of the list"
    );
}

/// `atomic` with a catalog: rolling a unit back restores the catalog (a stored, a replaced and
/// a removed definition); a savepoint undoes only its own change; `unit_changes` reports a
/// catalog change as `definitions`, not as data or schema.
pub fn rollback_restores_definitions<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().atomic || !has_catalog(storage) {
        return;
    }
    storage
        .set_definition(sample_query("kept"))
        .expect("define");
    storage
        .set_definition(sample_query("gone"))
        .expect("define");
    let before = catalog(storage);
    let mark = storage.begin_unit().unwrap();
    storage
        .set_definition(sample_query("added"))
        .expect("define");
    storage
        .set_definition(Definition::query(
            "kept",
            QueryDefinition::new("fn kept() { 2 }"),
        ))
        .expect("replace");
    storage
        .remove_definition(DefinitionKind::QUERY, "gone")
        .expect("remove");
    let changes = storage.unit_changes(mark).unwrap();
    assert!(
        changes.definitions && !changes.data && !changes.schema,
        "a catalog change is reported as definitions: {changes:?}"
    );
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_eq!(catalog(storage), before, "a rollback restores the catalog");

    let outer = storage.begin_unit().unwrap();
    storage
        .set_definition(sample_query("outer"))
        .expect("define");
    let inner = storage.begin_unit().unwrap();
    storage
        .remove_definition(DefinitionKind::QUERY, "outer")
        .expect("remove");
    storage
        .rollback_unit(inner, &RollbackReason::failed(&"contract"))
        .unwrap();
    storage.commit_unit(outer).expect("commit");
    assert!(
        storage
            .definition(DefinitionKind::QUERY, "outer")
            .unwrap()
            .is_some()
    );
}

/// `change_capture` with a catalog: a catalog change reaches the hooks as a
/// `Mutation::SetDefinition` with before and after; applying the change sets to a storage at the
/// recorded state reproduces the catalog.
pub fn definition_changes_are_captured<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().change_capture || !has_catalog(storage) {
        return;
    }
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: Arc::default(),
        veto: Arc::default(),
    };
    storage
        .register_commit_hook(Box::new(hook))
        .expect("change_capture: hooks are supported");
    let mark = storage.begin_unit().unwrap();
    storage.set_definition(sample_query("q")).expect("define");
    storage.commit_unit(mark).expect("commit");
    let mark = storage.begin_unit().unwrap();
    storage
        .remove_definition(DefinitionKind::QUERY, "q")
        .expect("remove");
    storage.commit_unit(mark).expect("commit");
    let sets = accepted.lock().map(|s| s.clone()).unwrap_or_default();
    assert_eq!(sets.len(), 2, "one change set per catalog change");
    match sets[0].mutations() {
        [Mutation::SetDefinition { before, after, .. }] => {
            assert!(before.is_none());
            assert_eq!(after.as_deref(), Some(&sample_query("q")));
        }
        other => panic!("expected one SetDefinition, got {other:?}"),
    }
    match sets[1].mutations() {
        [Mutation::SetDefinition { before, after, .. }] => {
            assert_eq!(before.as_deref(), Some(&sample_query("q")));
            assert!(after.is_none());
        }
        other => panic!("expected one SetDefinition, got {other:?}"),
    }
    storage.apply_change_set(&sets[0]).expect("apply");
    assert_eq!(
        storage.definition(DefinitionKind::QUERY, "q").unwrap(),
        Some(&sample_query("q"))
    );
}

/// `atomic` / `change_capture` with `set_edge_label`: a rollback restores the old label (and
/// the label index and counts); a committed change reaches the hooks as a
/// `Mutation::SetEdgeLabel` with before and after, and applying it reproduces the label. A
/// storage that keeps the default (`GraphError::Unsupported`) passes.
pub fn edge_label_change_rolls_back_and_is_captured<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().atomic {
        return;
    }
    let (_, _, e) = populate(storage);
    let before = snapshot(storage);
    let mark = storage.begin_unit().unwrap();
    match storage.set_edge_label(e, "likes".into()) {
        Err(GraphError::Unsupported { .. }) => {
            storage
                .rollback_unit(mark, &RollbackReason::failed(&"contract"))
                .unwrap();
            return;
        }
        result => assert!(result.expect("relabel")),
    }
    assert_consistent(storage);
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_eq!(
        snapshot(storage),
        before,
        "a rollback restores the edge label"
    );
    assert_eq!(storage.edges_by_label("likes").unwrap().count(), 0);
    assert_consistent(storage);
    if !storage.capabilities().unwrap().change_capture {
        return;
    }
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: Arc::default(),
        veto: Arc::default(),
    };
    storage
        .register_commit_hook(Box::new(hook))
        .expect("change_capture: hooks are supported");
    let mark = storage.begin_unit().unwrap();
    let e = storage.get_edge_by_id("ab").unwrap().expect("edge ab");
    storage.set_edge_label(e, "likes".into()).expect("relabel");
    storage.commit_unit(mark).expect("commit");
    let sets = accepted.lock().map(|s| s.clone()).unwrap_or_default();
    let set = match sets.as_slice() {
        [set] => set.clone(),
        other => panic!("expected one change set, got {other:?}"),
    };
    match set.mutations() {
        [
            Mutation::SetEdgeLabel {
                id, before, after, ..
            },
        ] => {
            assert_eq!(id, "ab");
            assert_eq!(before.as_deref(), Some("knows"));
            assert_eq!(after, "likes");
        }
        other => panic!("expected one SetEdgeLabel, got {other:?}"),
    }
    let recorded = snapshot(storage);
    let e = storage.get_edge_by_id("ab").unwrap().expect("edge ab");
    storage.set_edge_label(e, "knows".into()).expect("back");
    storage.apply_change_set(&set).expect("apply");
    assert_eq!(snapshot(storage), recorded);
    assert_consistent(storage);
}

/// `change_capture`: [`GraphStorage::discard_unit`] leaves the data, the schema and the catalog
/// as they were before [`GraphStorage::begin_unit`], calls no hook, does not move
/// [`GraphStorage::last_commit_seq`], and returns the unit's changes as a never-committed
/// change set (`commit_seq` 0, empty metadata). The auto-id sequences keep their progress: an
/// auto id handed out inside the discarded unit is not handed out again. A storage without
/// `change_capture` is not checked (`Transactional::dry_run` is unsupported there).
pub fn discard_unit_restores_state_keeps_sequences<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().change_capture {
        return;
    }
    let catalog_kept = has_catalog(storage);
    let (a, b, e) = populate(storage);
    if catalog_kept {
        storage
            .set_definition(sample_query("kept"))
            .expect("define");
    }
    let (hook, events) = EventCounter::new();
    storage
        .register_commit_hook(Box::new(hook))
        .expect("change_capture: hooks are supported");
    let before = snapshot(storage);
    let catalog_before = catalog(storage);
    let seq = storage.last_commit_seq().unwrap();

    let mark = storage.begin_unit().unwrap();
    let fresh = vertex(storage, None, &["x"], props(&[]));
    let fresh_id = storage
        .vertex_id(fresh)
        .unwrap()
        .unwrap_or_default()
        .to_string();
    let link = edge(storage, None, Some("e"), fresh, a, props(&[]));
    let link_id = storage
        .edge_id(link)
        .unwrap()
        .unwrap_or_default()
        .to_string();
    storage
        .set_vertex_property(a, "name".into(), "ANN".into())
        .expect("set");
    storage
        .set_edge_property(e, "w".into(), 2.into())
        .expect("set edge prop");
    storage.drop_vertex(b).expect("drop vertex");
    match storage.set_schema(GraphSchema::new().with_mode(SchemaMode::Open)) {
        Ok(()) | Err(GraphError::Unsupported { .. }) => {}
        Err(other) => panic!("set_schema in a unit failed: {other:?}"),
    }
    if catalog_kept {
        storage
            .set_definition(sample_query("added"))
            .expect("define");
        storage
            .remove_definition(DefinitionKind::QUERY, "kept")
            .expect("remove");
    }
    let changes = storage.discard_unit(mark).expect("discard_unit");

    assert_eq!(snapshot(storage), before, "a discarded unit leaves nothing");
    assert_eq!(catalog(storage), catalog_before, "the catalog is restored");
    assert_eq!(
        storage.last_commit_seq().unwrap(),
        seq,
        "a discard is not a commit"
    );
    assert_eq!(events.load(Ordering::SeqCst), 0, "a discard calls no hook");
    assert_eq!(changes.commit_seq(), 0, "never committed");
    assert_eq!(changes.committed_at(), 0, "never committed");
    assert_eq!(changes.metadata(), &CommitMetadata::new());
    assert!(
        !changes.is_empty(),
        "the change set lists what the unit changed"
    );
    assert_consistent(storage);

    // The sequences kept their progress.
    let mark = storage.begin_unit().unwrap();
    let next = vertex(storage, None, &[], props(&[]));
    let next_edge = edge(storage, None, Some("e"), next, a, props(&[]));
    storage.commit_unit(mark).expect("commit");
    assert_ne!(
        storage.vertex_id(next).unwrap().unwrap_or_default(),
        fresh_id,
        "an auto vertex id handed out in a discarded unit is not handed out again"
    );
    assert_ne!(
        storage.edge_id(next_edge).unwrap().unwrap_or_default(),
        link_id,
        "an auto edge id handed out in a discarded unit is not handed out again"
    );
    assert_consistent(storage);
}

/// `change_capture`: the metadata of `Transactional::transaction_with` reaches the hooks with
/// the commit's change set; a nested `transaction_with` that commits adds what the enclosing
/// one does not set (the enclosing one wins), a nested one that fails adds nothing.
pub fn transaction_with_metadata_reaches_hooks<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().change_capture {
        return;
    }
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: Arc::default(),
        veto: Arc::default(),
    };
    storage
        .register_commit_hook(Box::new(hook))
        .expect("change_capture: hooks are supported");
    let outer = CommitMetadata::new()
        .with_principal("outer")
        .with_attribute("ticket", 1);
    let inner = CommitMetadata::new()
        .with_principal("inner")
        .with_attribute("ticket", 2)
        .with_attribute("step", "inner");
    let failed = CommitMetadata::new().with_attribute("failed", true);
    storage
        .transaction_with(outer, |s| {
            vertex(s, Some("a"), &[], props(&[]));
            s.transaction_with(inner, |s| {
                vertex(s, Some("b"), &[], props(&[]));
                Ok::<_, GraphError>(())
            })?;
            let refused: Result<(), GraphError> = s.transaction_with(failed, |s| {
                vertex(s, Some("c"), &[], props(&[]));
                Err(GraphError::unsupported("contract"))
            });
            assert!(refused.is_err());
            Ok::<_, GraphError>(())
        })
        .expect("transaction");
    let sets = accepted.lock().map(|s| s.clone()).unwrap_or_default();
    let set = match sets.as_slice() {
        [set] => set,
        other => panic!("expected one change set for the transaction, got {other:?}"),
    };
    assert_eq!(set.len(), 2, "a and b; the failed savepoint left nothing");
    let metadata = set.metadata();
    assert_eq!(
        metadata.principal(),
        Some("outer"),
        "the enclosing one wins"
    );
    assert_eq!(
        metadata.attributes().get("ticket"),
        Some(&ElementProperty::from(1))
    );
    assert_eq!(
        metadata.attributes().get("step"),
        Some(&ElementProperty::from("inner")),
        "a committed nested transaction adds what is missing"
    );
    assert!(
        metadata.attributes().get("failed").is_none(),
        "a failed nested transaction adds nothing"
    );
    assert!(storage.get_vertex_by_id("c").unwrap().is_none());
    assert_consistent(storage);
}

/// `atomic`: a panic inside a `Transactional::transaction` (or `dry_run`) closure rolls the unit
/// back before it continues to the caller; nothing stays, no commit is counted, and the storage
/// keeps working (the unit is closed).
pub fn transactional_panic_rolls_back<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().atomic {
        return;
    }
    populate(storage);
    let before = snapshot(storage);
    let seq = storage.last_commit_seq().unwrap();
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let _: Result<(), GraphError> = storage.transaction(|s| {
            vertex(s, Some("ghost"), &[], props(&[]));
            panic!("contract: a panic inside a transaction");
        });
    }));
    assert!(unwound.is_err(), "the panic continues to the caller");
    assert_eq!(
        snapshot(storage),
        before,
        "the panic rolled everything back"
    );
    assert_eq!(storage.last_commit_seq().unwrap(), seq);
    if storage.capabilities().unwrap().change_capture {
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let _: Result<((), _), GraphError> = storage.dry_run(|s| {
                vertex(s, Some("ghost"), &[], props(&[]));
                panic!("contract: a panic inside a dry run");
            });
        }));
        assert!(unwound.is_err());
        assert_eq!(snapshot(storage), before);
    }
    // The unit was closed: a new one opens and commits normally.
    storage
        .transaction(|s| {
            vertex(s, Some("after"), &[], props(&[]));
            Ok::<_, GraphError>(())
        })
        .expect("a transaction after the panic");
    assert!(storage.get_vertex_by_id("after").unwrap().is_some());
    assert_consistent(storage);
}

/// `change_capture` with a [`GraphStorage::commit_clock`]: the clock handed out is the one the
/// storage times its commits with (the instance clock the system journal shares), so a commit
/// is never earlier than its floor, also when the system clock is behind it (simulated by
/// raising the floor an hour ahead). A storage that keeps no clock of its own (`Ok(None)`, the
/// default) or does not capture changes is not checked.
pub fn commit_clock_is_shared_with_hooks<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().change_capture {
        return;
    }
    let Some(clock) = storage.commit_clock().expect("commit_clock") else {
        return;
    };
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: Arc::default(),
        veto: Arc::default(),
    };
    storage
        .register_commit_hook(Box::new(hook))
        .expect("change_capture: hooks are supported");
    let floor = clock.now() + 3_600_000_000;
    clock.raise(floor);
    let mark = storage.begin_unit().unwrap();
    vertex(storage, Some("clocked"), &[], props(&[]));
    storage.commit_unit(mark).expect("commit");
    let sets = accepted.lock().map(|s| s.clone()).unwrap_or_default();
    let times: Vec<i64> = sets.iter().map(ChangeSet::committed_at).collect();
    assert_eq!(
        times,
        [floor],
        "the commit is timed by the clock handed out"
    );
    let again = storage.commit_clock().unwrap().expect("the clock stays");
    assert!(again.floor() >= floor, "clones share one floor");
}

/// The capabilities a reference `TraversalGraph` reports (for a backend's own comparisons).
pub fn reference_capabilities() -> StorageCapabilities {
    graphersal::TraversalGraph::new().capabilities().unwrap()
}

/// Rebuilds `mutation` through its public constructor (the shape a storage that captures
/// changes itself produces).
fn rebuilt(mutation: &Mutation) -> Mutation {
    match mutation {
        Mutation::AddVertex {
            id,
            labels,
            properties,
            ..
        } => Mutation::add_vertex(id.as_str(), labels.iter().cloned(), properties.clone()),
        Mutation::DropVertex { id, before, .. } => Mutation::drop_vertex(
            id.as_str(),
            VertexState::new(before.labels.iter().cloned(), before.properties.clone()),
        ),
        Mutation::AddEdge {
            id,
            label,
            out_id,
            in_id,
            properties,
            ..
        } => Mutation::add_edge(
            id.as_str(),
            label.as_deref(),
            out_id.as_str(),
            in_id.as_str(),
            properties.clone(),
        ),
        Mutation::DropEdge { id, before, .. } => Mutation::drop_edge(
            id.as_str(),
            EdgeState::new(
                before.label.as_deref(),
                before.out_id.as_str(),
                before.in_id.as_str(),
                before.properties.clone(),
            ),
        ),
        Mutation::SetProperty {
            element,
            key,
            before,
            after,
            ..
        } => Mutation::set_property(element.clone(), key.as_str(), before.clone(), after.clone()),
        Mutation::RemoveProperty {
            element,
            key,
            before,
            ..
        } => Mutation::remove_property(element.clone(), key.as_str(), before.clone()),
        Mutation::AddLabel { id, label, .. } => Mutation::add_label(id.as_str(), label.as_str()),
        Mutation::RemoveLabel { id, label, .. } => {
            Mutation::remove_label(id.as_str(), label.as_str())
        }
        Mutation::SetEdgeLabel {
            id, before, after, ..
        } => Mutation::set_edge_label(id.as_str(), before.as_deref(), after.as_str()),
        Mutation::SetSchema { before, after, .. } => {
            Mutation::set_schema((**before).clone(), (**after).clone())
        }
        Mutation::SetDefinition {
            kind,
            name,
            before,
            after,
            ..
        } => Mutation::set_definition(
            *kind,
            name.as_str(),
            before.as_deref().cloned(),
            after.as_deref().cloned(),
        ),
        other => other.clone(),
    }
}

/// `change_capture`: a change set rebuilt with the public constructors
/// ([`ChangeSet::builder`], one constructor per [`Mutation`] kind) from a captured one is equal
/// to it, and applying it to the state the unit started from reproduces the recorded end state.
pub fn change_set_builder_round_trip<S: GraphStorage>(storage: &mut S) {
    if !storage.capabilities().unwrap().change_capture {
        return;
    }
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let hook = ContractHook {
        accepted: accepted.clone(),
        rollbacks: Arc::default(),
        veto: Arc::default(),
    };
    storage
        .register_commit_hook(Box::new(hook))
        .expect("change_capture: hooks are supported");
    let mark = storage.begin_unit().unwrap();
    let (a, b, _) = populate(storage);
    storage
        .set_vertex_property(a, "k".into(), 1.into())
        .expect("set");
    storage.drop_vertex(b).expect("drop");
    storage.commit_unit(mark).expect("commit");
    let recorded = snapshot(storage);
    let captured: ChangeSet = accepted
        .lock()
        .ok()
        .and_then(|s| s.first().cloned())
        .expect("one change set");
    let mut builder = ChangeSet::builder(captured.commit_seq(), captured.committed_at())
        .with_sequences(captured.node_sequence(), captured.edge_sequence())
        .with_metadata(captured.metadata().clone());
    for mutation in captured.mutations() {
        builder.push(rebuilt(mutation));
    }
    let built = builder.build().expect("a captured change set is valid");
    assert_eq!(
        built, captured,
        "the constructors rebuild the captured change set"
    );
    if let Some(v) = storage.get_vertex_by_id("a").unwrap() {
        storage.drop_vertex(v).expect("drop");
    }
    assert_eq!(storage.vertex_count().unwrap(), 0);
    storage.apply_change_set(&built).expect("apply");
    assert_eq!(snapshot(storage), recorded);
    assert_consistent(storage);
}
