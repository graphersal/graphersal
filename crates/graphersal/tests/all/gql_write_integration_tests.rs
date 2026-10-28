//! GQL writes in the engine's machinery (stage 3a, feature `gql`): one `gql()` traversal is one
//! unit (a failure on the last row leaves nothing), `Transactional::dry_run`, commit hooks (the
//! `ChangeSet` of an `INSERT`, a veto leaves nothing), exact statistics, the schema modes
//! (`none`, `open` with write-time coercion, `closed` with `required` and `default`), the error
//! of a read source (the same as a Gremlin write on it), the Store (reopen and recovery after
//! GQL writes, feature `persist`) and, with feature `script`, per-traversal units in a Rhai
//! script and saved queries whose body calls `g.gql("INSERT ...")`. Stage 3d: the change set,
//! dry run, veto and statistics of a `DELETE`, and its journal (WAL replay, reopen). Stage 3e:
//! the change set (creation, then ON items), dry run, veto, statistics, schema defaults and
//! violations, and the journal of a `MERGE`.
#![cfg(feature = "gql")]

use std::sync::{Arc, Mutex};

use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::prelude::*;
use graphersal::storage::{GraphStatistics, GraphStorage};

// ----- helpers -----

/// The canonical state of `g`: every vertex (labels, properties) and every edge (label,
/// properties, both ends by their content), sorted; independent of element ids.
fn dump<G: GraphStorage>(g: &G) -> Vec<String> {
    let text = |v: G::VertexId| {
        let mut props: Vec<String> = g
            .vertex_properties(v)
            .unwrap()
            .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        props.sort();
        format!("{:?} {{{}}}", g.vertex_labels(v).unwrap(), props.join(", "))
    };
    let mut lines: Vec<String> = g
        .vertices()
        .unwrap()
        .map(|v| format!("V {}", text(v)))
        .collect();
    for e in g.edges().unwrap() {
        let mut props: Vec<String> = g
            .edge_properties(e)
            .unwrap()
            .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        props.sort();
        let (from, to) = g.edge_endpoints(e).unwrap().expect("an edge has ends");
        lines.push(format!(
            "E {} {{{}}} from {} to {}",
            g.edge_label(e).unwrap().unwrap_or_default(),
            props.join(", "),
            text(from),
            text(to)
        ));
    }
    lines.sort();
    lines
}

fn modern_dump() -> Vec<String> {
    dump(&TraversalGraph::tinkerpop_modern())
}

/// Runs the GQL `query` on `graph` through the writing source.
fn gql(graph: &mut TraversalGraph, query: &str) -> TraverserResult<Vec<ElementProperty>> {
    graph.traversal_mut().gql(query).to_list()
}

fn ok(graph: &mut TraversalGraph, query: &str) -> Vec<ElementProperty> {
    gql(graph, query).unwrap_or_else(|e| panic!("{query}: {}", format_diagnostic(&e)))
}

/// The schema error at the root of a failed write.
fn schema_error(err: &TraverserError) -> SchemaError {
    match err.root_cause() {
        TraverserError::Graph(GraphError::Schema(err)) | TraverserError::Schema(err) => err.clone(),
        other => panic!("expected a schema error, got {other:?}"),
    }
}

/// A hook that keeps every change set it is asked about and vetoes while `veto` is set.
#[derive(Clone, Default)]
struct Probe {
    seen: Arc<Mutex<Vec<ChangeSet>>>,
    veto: Arc<Mutex<bool>>,
}

#[derive(Debug)]
struct Refused;

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("refused by the probe")
    }
}

impl std::error::Error for Refused {}

impl CommitHook for Probe {
    fn name(&self) -> &str {
        "probe"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.seen.lock().unwrap().push(changes.clone());
        if *self.veto.lock().unwrap() {
            return Err(Box::new(Refused));
        }
        Ok(())
    }
}

/// The mutations of `changes` as short texts: `+V [labels] {props}`, `+E label`.
fn mutation_texts(changes: &ChangeSet) -> Vec<String> {
    changes
        .mutations()
        .iter()
        .map(|mutation| match mutation {
            Mutation::AddVertex {
                labels, properties, ..
            } => {
                let mut props: Vec<String> = properties
                    .iter()
                    .map(|(k, v)| format!("{k}={v:?}"))
                    .collect();
                props.sort();
                format!("+V {labels:?} {{{}}}", props.join(", "))
            }
            Mutation::AddEdge { label, .. } => format!("+E {}", label.as_deref().unwrap_or("")),
            other => format!("{other:?}"),
        })
        .collect()
}

// ----- units -----

#[test]
fn a_failure_on_the_last_row_leaves_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let seq = graph.last_commit_seq();
    // The third row divides by zero after two rows inserted their nodes and edges.
    let err = gql(
        &mut graph,
        "FOR x IN [1, 2, 0] MATCH (p:person {name: 'marko'}) INSERT (p)-[:R]->(:F {v: 10 / x})",
    )
    .unwrap_err();
    let TraverserError::Gql { gqlstatus, .. } = err.root_cause() else {
        panic!("{err:?}");
    };
    assert_eq!(gqlstatus, "22012");
    assert_eq!(dump(&graph), modern_dump());
    assert_eq!(graph.last_commit_seq(), seq, "no commit");
    // The endpoint of the last row is missing: the same.
    let err = gql(
        &mut graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) INSERT (s)-[:BY]->(:Mark)",
    )
    .unwrap_err();
    assert!(
        matches!(err.root_cause(), TraverserError::Gql { gqlstatus, .. } if gqlstatus == "22004"),
        "{err:?}"
    );
    assert_eq!(dump(&graph), modern_dump());
    // Each successful query is one commit.
    ok(&mut graph, "FOR x IN [1, 2, 3] INSERT (:F {v: x})");
    assert_eq!(graph.last_commit_seq(), seq + 1);
}

#[test]
fn a_dry_run_previews_a_gql_write_and_leaves_the_graph() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let seq = graph.last_commit_seq();
    let (rows, changes) = graph
        .dry_run(|g| {
            g.traversal_mut()
                .gql("INSERT (a:A {k: 1})-[:R]->(b:B) RETURN a.k AS k")
                .to_list()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(dump(&graph), modern_dump());
    assert_eq!(graph.last_commit_seq(), seq);
    let mut texts = mutation_texts(&changes);
    texts.sort();
    assert_eq!(texts, ["+E R", "+V [\"A\"] {k=1}", "+V [\"B\"] {}"]);
    // The body sees its own writes; the graph after the dry run does not.
    let (count, _) = graph
        .dry_run(|g| {
            g.traversal_mut().gql("INSERT (:A), (:A)").to_list()?;
            g.traversal().v(None::<()>).has_label("A").count().next()
        })
        .unwrap();
    assert_eq!(count, Some(ElementProperty::from(2)));
    assert_eq!(dump(&graph), modern_dump());
}

#[test]
fn commit_hooks_see_the_change_set_of_an_insert_and_a_veto_leaves_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let probe = Probe::default();
    graph.register_commit_hook(Box::new(probe.clone())).unwrap();
    ok(
        &mut graph,
        "MATCH (p:person {name: 'marko'}) INSERT (p)-[:OWNS {since: 2020}]->(:Pet&Cat {name: 'tom'})",
    );
    let seen = std::mem::take(&mut *probe.seen.lock().unwrap());
    assert_eq!(seen.len(), 1, "one unit, one change set");
    assert_eq!(
        mutation_texts(&seen[0]),
        ["+V [\"Pet\", \"Cat\"] {name=\"tom\"}", "+E OWNS"]
    );
    let before = dump(&graph);
    let seq = graph.last_commit_seq();
    *probe.veto.lock().unwrap() = true;
    let err = gql(&mut graph, "FOR x IN range(1, 5) INSERT (:V {x: x})").unwrap_err();
    assert!(
        format_diagnostic(&err).contains("refused by the probe"),
        "{err}"
    );
    assert_eq!(dump(&graph), before);
    assert_eq!(graph.last_commit_seq(), seq);
    assert_eq!(probe.seen.lock().unwrap().len(), 1, "the hook was asked");
}

#[test]
fn the_statistics_stay_exact_after_inserts() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    ok(
        &mut graph,
        "MATCH (p:person) INSERT (p)-[:OWNS]->(:Pet&Cat {owner: p.name}), (:Toy)",
    );
    let kept = graph
        .statistics()
        .unwrap()
        .expect("TraversalGraph keeps statistics")
        .clone();
    assert_eq!(kept, GraphStatistics::from_storage(&graph).unwrap());
    assert_eq!((kept.vertex_count(), kept.edge_count()), (14, 10));
    assert_eq!(kept.vertex_label_count("Pet"), 4);
    assert_eq!(kept.vertex_label_count("Cat"), 4);
    assert_eq!(kept.primary_vertex_label_count("Pet"), 4);
    assert_eq!(kept.primary_vertex_label_count("Cat"), 0);
    assert_eq!(kept.vertex_label_count("Toy"), 4);
    assert_eq!(kept.edge_label_count("OWNS"), 4);
    // A failed write leaves the statistics as they were.
    let _ = gql(&mut graph, "FOR x IN [1, 0] INSERT (:Pet {v: 1 / x})").unwrap_err();
    assert_eq!(graph.statistics().unwrap(), Some(&kept));
}

// ----- stage 3d: DELETE in the unit machinery -----

/// The change set of a GQL `DETACH DELETE` is the one of its `drop()` twin (every incident edge
/// first, then the vertex); a dry run previews it, a veto leaves nothing.
#[test]
fn a_gql_delete_records_the_change_set_of_its_drop_twin() {
    let changes = |gql: bool| {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let (_, changes) = graph
            .dry_run(|g| {
                if gql {
                    g.traversal_mut()
                        .gql("MATCH (n:person {name: 'marko'}) DETACH DELETE n")
                        .to_list()
                } else {
                    g.traversal_mut().v("1").drop().to_list()
                }
            })
            .unwrap();
        assert_eq!(dump(&graph), modern_dump(), "a dry run leaves the graph");
        changes
            .mutations()
            .iter()
            .map(|m| match m {
                Mutation::DropVertex { id, .. } => format!("-V {id}"),
                Mutation::DropEdge { id, .. } => format!("-E {id}"),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let deleted = changes(true);
    assert_eq!(deleted, changes(false));
    assert_eq!(deleted, ["-E 2", "-E 1", "-E 0", "-V 1"]);

    let mut graph = TraversalGraph::tinkerpop_modern();
    let probe = Probe::default();
    graph.register_commit_hook(Box::new(probe.clone())).unwrap();
    *probe.veto.lock().unwrap() = true;
    let seq = graph.last_commit_seq();
    let err = gql(&mut graph, "MATCH (n:software) DETACH DELETE n").unwrap_err();
    assert!(
        format_diagnostic(&err).contains("refused by the probe"),
        "{err}"
    );
    assert_eq!(dump(&graph), modern_dump());
    assert_eq!(graph.last_commit_seq(), seq);
    *probe.veto.lock().unwrap() = false;
    ok(&mut graph, "MATCH (n:software) DETACH DELETE n");
    assert_eq!(graph.last_commit_seq(), seq + 1);
    let seen = probe.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].mutations().len(), 6, "4 edges, 2 vertices");
}

#[test]
fn the_statistics_stay_exact_after_deletes() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    ok(&mut graph, "MATCH (n:software) DETACH DELETE n");
    ok(&mut graph, "MATCH ()-[r:knows]->() DELETE r");
    let kept = graph
        .statistics()
        .unwrap()
        .expect("TraversalGraph keeps statistics")
        .clone();
    assert_eq!(kept, GraphStatistics::from_storage(&graph).unwrap());
    assert_eq!((kept.vertex_count(), kept.edge_count()), (4, 0));
    assert_eq!(kept.vertex_label_count("software"), 0);
    assert_eq!(kept.vertex_label_count("person"), 4);
    assert_eq!(kept.edge_label_count("created"), 0);
    assert_eq!(kept.edge_label_count("knows"), 0);
    // The same as after the Gremlin twin.
    let mut twin = TraversalGraph::tinkerpop_modern();
    twin.traversal_mut()
        .v(None::<()>)
        .has_label("software")
        .drop()
        .to_list()
        .unwrap();
    twin.traversal_mut()
        .e(None::<()>)
        .has_label("knows")
        .drop()
        .to_list()
        .unwrap();
    assert_eq!(twin.statistics().unwrap(), Some(&kept));
    // A failed delete (NODETACH) leaves the statistics as they were.
    ok(&mut graph, "INSERT (:A)-[:R]->(:B)");
    let kept = graph.statistics().unwrap().unwrap().clone();
    let _ = gql(&mut graph, "MATCH (a:A) DELETE a").unwrap_err();
    assert_eq!(graph.statistics().unwrap(), Some(&kept));
}

// ----- stage 3e: MERGE in the unit machinery -----

/// The mutations of `changes` as short texts, property writes included: `+V [labels] {props}`,
/// `+E label`, `set key=value`, `+label`.
fn merge_texts(changes: &ChangeSet) -> Vec<String> {
    changes
        .mutations()
        .iter()
        .map(|mutation| match mutation {
            Mutation::SetProperty { key, after, .. } => format!("set {key}={after:?}"),
            Mutation::AddLabel { label, .. } => format!("+{label}"),
            Mutation::AddVertex {
                labels, properties, ..
            } => {
                let mut props: Vec<String> = properties
                    .iter()
                    .map(|(k, v)| format!("{k}={v:?}"))
                    .collect();
                props.sort();
                format!("+V {labels:?} {{{}}}", props.join(", "))
            }
            Mutation::AddEdge { label, .. } => format!("+E {}", label.as_deref().unwrap_or("")),
            _ => "other".to_string(),
        })
        .collect()
}

/// A MERGE that creates records the creation, then its ON CREATE items; one that only matches
/// records its ON MATCH items only, one without ON items commits nothing. A dry run previews it, a veto
/// leaves nothing.
#[test]
fn a_gql_merge_records_its_creations_and_on_items_in_one_change_set() {
    let q = "MATCH (p:person {name: 'marko'}) MERGE (p)-[:OWNS]->(c:Pet {name: 'tom'}) \
             ON CREATE SET c.age = 1 ON MATCH SET c:Old";
    let mut graph = TraversalGraph::tinkerpop_modern();
    let seq = graph.last_commit_seq();
    let (_, changes) = graph
        .dry_run(|g| g.traversal_mut().gql(q).to_list())
        .unwrap();
    assert_eq!(
        merge_texts(&changes),
        ["+V [\"Pet\"] {name=\"tom\"}", "+E OWNS", "set age=1"]
    );
    assert_eq!(dump(&graph), modern_dump(), "a dry run leaves the graph");
    assert_eq!(graph.last_commit_seq(), seq);

    let probe = Probe::default();
    graph.register_commit_hook(Box::new(probe.clone())).unwrap();
    ok(&mut graph, q);
    ok(&mut graph, q);
    let seen = std::mem::take(&mut *probe.seen.lock().unwrap());
    assert_eq!(seen.len(), 2, "one unit, one change set each");
    assert_eq!(
        merge_texts(&seen[0]),
        ["+V [\"Pet\"] {name=\"tom\"}", "+E OWNS", "set age=1"]
    );
    assert_eq!(merge_texts(&seen[1]), ["+Old"]);
    assert_eq!(graph.last_commit_seq(), seq + 2);
    // A MERGE that only matches and has no ON items writes nothing: no change set, no commit.
    ok(&mut graph, "MERGE (p:person {name: 'marko'})");
    assert!(probe.seen.lock().unwrap().is_empty());
    assert_eq!(graph.last_commit_seq(), seq + 2);

    let before = dump(&graph);
    *probe.veto.lock().unwrap() = true;
    let err = gql(&mut graph, "FOR x IN range(1, 5) MERGE (:V {x: x})").unwrap_err();
    assert!(
        format_diagnostic(&err).contains("refused by the probe"),
        "{err}"
    );
    assert_eq!(dump(&graph), before);
    assert_eq!(graph.last_commit_seq(), seq + 2);
}

#[test]
fn the_statistics_stay_exact_after_merges() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = "MATCH (p:person) MERGE (p)-[:OWNS]->(:Pet&Cat {owner: p.name}) \
             MERGE (t:Toy {k: 1}) ON MATCH SET t:Used";
    ok(&mut graph, q);
    ok(&mut graph, q);
    let kept = graph
        .statistics()
        .unwrap()
        .expect("TraversalGraph keeps statistics")
        .clone();
    assert_eq!(kept, GraphStatistics::from_storage(&graph).unwrap());
    assert_eq!((kept.vertex_count(), kept.edge_count()), (11, 10));
    assert_eq!(kept.vertex_label_count("Pet"), 4);
    assert_eq!(kept.primary_vertex_label_count("Cat"), 0);
    assert_eq!(kept.vertex_label_count("Toy"), 1);
    assert_eq!(kept.vertex_label_count("Used"), 1);
    assert_eq!(kept.edge_label_count("OWNS"), 4);
    // A failed MERGE (a NULL value on the last row) leaves the statistics as they were.
    let _ = gql(&mut graph, "FOR x IN [1, NULL] MERGE (:Pet {v: x})").unwrap_err();
    assert_eq!(graph.statistics().unwrap(), Some(&kept));
}

// ----- the schema -----

#[test]
fn without_a_schema_or_in_mode_none_every_value_is_stored() {
    for schema in [None, Some(r#"{"mode": "none", "vertices": {"P": {}}}"#)] {
        let mut graph = TraversalGraph::new();
        if let Some(schema) = schema {
            graph
                .set_schema(GraphSchema::from_json(schema).unwrap())
                .unwrap();
        }
        ok(
            &mut graph,
            "INSERT (:P {name: 5, list: [1, 'a'], map: {k: 1}})-[:ANY {w: 'x'}]->(:Q)",
        );
        assert_eq!(graph.vertex_count(), 2);
        assert_eq!(graph.edge_count(), 1);
    }
}

#[test]
fn open_mode_coerces_declared_values_on_write_and_refuses_wrong_ones() {
    const UID: &str = "550e8400-e29b-41d4-a716-446655440000";
    let mut graph = TraversalGraph::new();
    graph
        .set_schema(
            GraphSchema::from_json(
                r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
                    "properties": {"uid": {"type": "string", "format": "uuid"},
                                   "w": {"type": "number"}, "name": {"type": "string"}}}}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
    ok(
        &mut graph,
        &format!("INSERT (:person {{uid: '{UID}', w: 2, other: 'free'}}), (:other {{x: 1}})"),
    );
    let person = graph
        .vertices()
        .unwrap()
        .find(|&v| graph.vertex_has_label(v, "person").unwrap())
        .unwrap();
    let props = graph.vertex_properties(person).unwrap().unwrap();
    assert_eq!(
        props.get("uid"),
        Some(&ElementProperty::Uuid(
            0x550e8400_e29b_41d4_a716_446655440000
        ))
    );
    assert_eq!(props.get("w"), Some(&ElementProperty::from(2.0)));
    assert_eq!(graph.vertex_count(), 2);
    // A value of the wrong type fails the query; the rows before it are gone too.
    let before = dump(&graph);
    let err = gql(
        &mut graph,
        "FOR n IN ['a', 'b', 3] INSERT (:person {name: n})",
    )
    .unwrap_err();
    let _ = schema_error(&err);
    assert_eq!(dump(&graph), before);
}

#[test]
fn closed_mode_refuses_undeclared_writes_and_fills_defaults_at_create() {
    let mut graph = TraversalGraph::new();
    graph
        .set_schema(
            GraphSchema::from_json(
                r#"{"mode": "closed",
                  "vertices": {"person": {"schema": {"type": "object", "properties": {
                      "name": {"type": "string"},
                      "active": {"type": "boolean", "default": true}},
                    "required": ["name"]}}},
                  "edges": {"knows": {"connections": [{"from": "person", "to": "person"}]}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
    ok(
        &mut graph,
        "INSERT (:person {name: 'ann'})-[:knows]->(:person {name: 'bob', active: false})",
    );
    assert_eq!(
        dump(&graph),
        [
            "E knows {} from [\"person\"] {active=true, name=\"ann\"} to [\"person\"] {active=false, name=\"bob\"}",
            "V [\"person\"] {active=false, name=\"bob\"}",
            "V [\"person\"] {active=true, name=\"ann\"}",
        ]
    );
    let before = dump(&graph);
    // `required` sees every property of the INSERT at once: a missing one fails.
    let err = gql(
        &mut graph,
        "FOR n IN ['cy', NULL] INSERT (:person {name: n})",
    )
    .unwrap_err();
    assert!(
        matches!(schema_error(&err), SchemaError::MissingRequiredProperty { ref property_name, .. } if property_name == "name"),
        "{err:?}"
    );
    // An undeclared label, property or edge label.
    let err = gql(&mut graph, "INSERT (:alien {name: 'x'})").unwrap_err();
    assert!(
        matches!(schema_error(&err), SchemaError::UnknownVertexLabel { ref label, .. } if label == "alien"),
        "{err:?}"
    );
    for query in [
        "INSERT (:person {name: 'x', extra: 1})",
        "MATCH (a:person {name: 'ann'}) INSERT (a)-[:likes]->(:person {name: 'x'})",
    ] {
        let err = gql(&mut graph, query).unwrap_err();
        let _ = schema_error(&err);
    }
    assert_eq!(dump(&graph), before);
}

/// MERGE creates with the schema's defaults (W37) and coerces like INSERT; the match child
/// finds what the create stored, so a second run matches. ON CREATE items are a second write
/// after the create (C8): a `required` property they alone would give fails the create; any
/// violation leaves nothing.
#[test]
fn merge_creates_with_schema_defaults_and_a_violation_leaves_nothing() {
    let mut graph = TraversalGraph::new();
    graph
        .set_schema(
            GraphSchema::from_json(
                r#"{"mode": "closed",
                  "vertices": {"person": {"schema": {"type": "object", "properties": {
                      "name": {"type": "string"},
                      "w": {"type": "number"},
                      "active": {"type": "boolean", "default": true}},
                    "required": ["name"]}}},
                  "edges": {"knows": {"connections": [{"from": "person", "to": "person"}]}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
    let q = "FOR n IN ['ann', 'bob', 'ann'] MERGE (p:person {name: n, w: 2}) \
             ON MATCH SET p.active = false RETURN p.w AS w";
    let rows = ok(&mut graph, q);
    assert_eq!(rows.len(), 3);
    ok(
        &mut graph,
        "MATCH (a:person {name: 'ann'}), (b:person {name: 'bob'}) MERGE (a)-[:knows]->(b)",
    );
    assert_eq!(
        dump(&graph),
        [
            "E knows {} from [\"person\"] {active=false, name=\"ann\", w=2} to [\"person\"] {active=true, name=\"bob\", w=2}",
            "V [\"person\"] {active=false, name=\"ann\", w=2}",
            "V [\"person\"] {active=true, name=\"bob\", w=2}",
        ]
    );
    // Coerced to the declared `number` (as INSERT, open mode test above).
    for v in graph.vertices().unwrap() {
        assert_eq!(
            graph
                .vertex_properties(v)
                .unwrap()
                .and_then(|p| p.get("w"))
                .cloned(),
            Some(ElementProperty::from(2.0))
        );
    }
    let before = dump(&graph);
    // `required` sees the pattern's properties only: ON CREATE comes after the create.
    let err = gql(
        &mut graph,
        "FOR k IN [1, 2] MERGE (p:person {w: k}) ON CREATE SET p.name = 'x'",
    )
    .unwrap_err();
    assert!(
        matches!(schema_error(&err), SchemaError::MissingRequiredProperty { ref property_name, .. } if property_name == "name"),
        "{err:?}"
    );
    // Undeclared labels, properties, edge labels and wrong types, created or set by an ON
    // item, on a later row after earlier rows wrote.
    for query in [
        "FOR n IN ['cy', 'dan'] MERGE (:person {name: n}) MERGE (:alien {name: n})",
        "FOR n IN ['cy', 'dan'] MERGE (:person {name: n, extra: 1})",
        "FOR n IN ['cy', 'ann'] MERGE (p:person {name: n}) ON MATCH SET p.extra = 1",
        "FOR n IN ['cy', 'ann'] MERGE (p:person {name: n}) ON MATCH SET p.w = 'heavy'",
        "MATCH (a:person {name: 'ann'}) FOR n IN ['cy', 'dan'] \
         MERGE (a)-[:likes]->(:person {name: n})",
    ] {
        let err = gql(&mut graph, query).unwrap_err();
        let _ = schema_error(&err);
        assert_eq!(dump(&graph), before, "{query}");
    }
}

// ----- the read source -----

#[test]
fn the_read_source_refuses_a_gql_write_with_the_error_of_a_gremlin_write() {
    let graph = TraversalGraph::tinkerpop_modern();
    let gremlin = graph.traversal().add_v("A").to_list().unwrap_err();
    for query in ["INSERT (:A)", "MATCH (p:person) INSERT (p)-[:R]->(:A)"] {
        let err = graph.traversal().gql(query).to_list().unwrap_err();
        assert_eq!(
            format!("{:?}", err.root_cause()),
            format!("{:?}", gremlin.root_cause()),
            "{query}"
        );
        assert_eq!(
            err.root_cause().to_string(),
            "Invalid element state: Cannot mutate immutable graph reference"
        );
        let text = format_diagnostic(&err);
        assert!(
            text.starts_with("Error: Step #")
                && text.contains(" 'gql:insert_node([\"A\"])' execution failed"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "Caused by: Invalid element state: Cannot mutate immutable graph reference\n\
                 Help: Ensure element properties and states are valid."
            ),
            "{text}"
        );
        // The Gremlin write ends with exactly the same lines.
        assert!(format_diagnostic(&gremlin).ends_with(
            "Caused by: Invalid element state: Cannot mutate immutable graph reference\n\
                 Help: Ensure element properties and states are valid."
        ));
    }
    assert_eq!(dump(&graph), modern_dump());
}

// ----- the Store -----

#[cfg(feature = "persist")]
mod store {
    use graphersal::persist::{MemDir, Store, StoreOptions};
    use graphersal::prelude::*;

    use super::dump;

    fn write(store: &Store, query: &str) -> TraverserResult<Vec<ElementProperty>> {
        store.graph().write().traversal_mut().gql(query).to_list()
    }

    #[test]
    fn gql_writes_survive_a_reopen_and_a_crash() {
        let dir = MemDir::named("memory:gql");
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        write(
            &store,
            "INSERT (a:person {name: 'ann'})-[:knows {w: 1}]->(:person {name: 'bob'})",
        )
        .unwrap();
        write(
            &store,
            "MATCH (p:person) INSERT (p)-[:OWNS]->(:Pet&Cat {owner: p.name})",
        )
        .unwrap();
        // A failed write appends nothing to the journal.
        let seq = store.graph().read().last_commit_seq();
        assert!(write(&store, "FOR x IN [1, 0] INSERT (:Bad {v: 1 / x})").is_err());
        assert_eq!(store.graph().read().last_commit_seq(), seq);
        let before = dump(&*store.graph().read());
        assert_eq!(before.len(), 7, "{before:?}");

        // A crash image: the WAL replays both units.
        let crashed = dir.deep_copy();
        let recovered = Store::open(&crashed, StoreOptions::new()).unwrap();
        assert!(!recovered.open_report().clean_close);
        assert_eq!(recovered.open_report().commit_seq, seq);
        assert_eq!(dump(&*recovered.graph().read()), before);
        drop(recovered);

        // A clean close and reopen.
        store.close().unwrap();
        let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
        assert!(reopened.open_report().clean_close);
        assert_eq!(dump(&*reopened.graph().read()), before);
        // The reopened store goes on writing.
        write(&reopened, "INSERT (:person {name: 'cy'})").unwrap();
        assert_eq!(reopened.graph().read().last_commit_seq(), seq + 1);
    }

    /// Stage 3e: a GQL MERGE is journaled with what it wrote: the WAL replays its creations
    /// and ON items after a crash and a reopen keeps them; a MERGE that only matches and a failed
    /// one append nothing.
    #[test]
    fn gql_merges_survive_a_reopen_and_a_crash() {
        let dir = MemDir::named("memory:gql-merge");
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        let q = "FOR n IN ['ann', 'bob', 'ann'] MERGE (p:person {name: n}) \
                 ON CREATE SET p.v = 1 ON MATCH SET p.v = p.v + 1";
        write(&store, q).unwrap();
        write(
            &store,
            "MATCH (a:person {name: 'ann'}), (b:person {name: 'bob'}) \
             MERGE (a)-[k:knows]->(b) ON CREATE SET k.w = 1",
        )
        .unwrap();
        write(&store, "MERGE (p:person {name: 'bob'})").unwrap();
        let seq = store.graph().read().last_commit_seq();
        assert!(write(&store, "FOR x IN [1, NULL] MERGE (:Bad {v: x})").is_err());
        assert_eq!(store.graph().read().last_commit_seq(), seq);
        let before = dump(&*store.graph().read());
        assert_eq!(
            before,
            [
                "E knows {w=1} from [\"person\"] {name=\"ann\", v=2} to [\"person\"] {name=\"bob\", v=1}",
                "V [\"person\"] {name=\"ann\", v=2}",
                "V [\"person\"] {name=\"bob\", v=1}",
            ]
        );

        let crashed = dir.deep_copy();
        let recovered = Store::open(&crashed, StoreOptions::new()).unwrap();
        assert!(!recovered.open_report().clean_close);
        assert_eq!(recovered.open_report().commit_seq, seq);
        assert_eq!(dump(&*recovered.graph().read()), before);
        drop(recovered);

        store.close().unwrap();
        let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
        assert!(reopened.open_report().clean_close);
        assert_eq!(dump(&*reopened.graph().read()), before);
        // The reopened store goes on merging: ann matches.
        write(&reopened, q).unwrap();
        let after = dump(&*reopened.graph().read());
        assert!(
            after.contains(&"V [\"person\"] {name=\"ann\", v=4}".to_string()),
            "{after:?}"
        );
    }

    /// Stage 3d: a GQL `DELETE` / `DETACH DELETE` is journaled like `drop()`: the WAL replays
    /// it after a crash and a reopen keeps it; a refused NODETACH delete appends nothing.
    #[test]
    fn gql_deletes_survive_a_reopen_and_a_crash() {
        let dir = MemDir::named("memory:gql-delete");
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        write(
            &store,
            "INSERT (a:person {name: 'ann'})-[:knows]->(b:person {name: 'bob'}), \
             (b)-[:knows]->(:person {name: 'cy'}), (:Tmp)",
        )
        .unwrap();
        write(&store, "MATCH (t:Tmp) DELETE t").unwrap();
        let seq = store.graph().read().last_commit_seq();
        assert!(write(&store, "MATCH (p {name: 'ann'}) DELETE p").is_err());
        assert_eq!(store.graph().read().last_commit_seq(), seq);
        write(&store, "MATCH (p {name: 'bob'}) DETACH DELETE p").unwrap();
        write(&store, "MATCH (p:person) DELETE p").unwrap();
        let seq = seq + 2;
        assert_eq!(store.graph().read().last_commit_seq(), seq);
        let before = dump(&*store.graph().read());
        assert!(before.is_empty(), "{before:?}");

        let crashed = dir.deep_copy();
        let recovered = Store::open(&crashed, StoreOptions::new()).unwrap();
        assert!(!recovered.open_report().clean_close);
        assert_eq!(recovered.open_report().commit_seq, seq);
        assert_eq!(dump(&*recovered.graph().read()), before);
        drop(recovered);

        store.close().unwrap();
        let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
        assert!(reopened.open_report().clean_close);
        assert_eq!(dump(&*reopened.graph().read()), before);
    }
}

// ----- the Rhai binding: units per traversal, saved queries -----

#[cfg(feature = "script")]
mod script {
    use std::sync::Arc;

    use graphersal::catalog::{QUERY_DIALECT, QueryDefinition, WRITING_QUERY_DIALECT};
    use graphersal::prelude::*;
    use graphersal::script::rhai;

    use super::{dump, modern_dump};
    use crate::script_host::eval_value;

    fn modern() -> Arc<Graph> {
        Arc::new(GraphSource::tinkerpop_modern())
    }

    fn run(graph: &Arc<Graph>, script: &str) -> Result<rhai::Dynamic, String> {
        eval_value(graph.clone(), script).map_err(|err| err.to_string())
    }

    fn ok(graph: &Arc<Graph>, script: &str) -> rhai::Dynamic {
        run(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"))
    }

    fn effects(graph: &Arc<Graph>, name: &str) -> Vec<String> {
        ok(graph, &format!(r#"g.get_query("{name}")["effects"]"#))
            .cast::<rhai::Array>()
            .into_iter()
            .map(|effect| effect.cast::<rhai::Map>()["text"].to_string())
            .collect()
    }

    fn get(graph: &Arc<Graph>, name: &str, field: &str) -> String {
        ok(graph, &format!(r#"g.get_query("{name}")["{field}"]"#)).to_string()
    }

    #[test]
    fn each_gql_traversal_of_a_script_is_its_own_unit() {
        let graph = modern();
        let err = run(
            &graph,
            r#"g.gql("INSERT (:A)").iterate();
               g.gql("FOR x IN [1, 0] INSERT (:B {v: 1 / x})").iterate()"#,
        )
        .unwrap_err();
        assert!(err.contains("division by zero"), "{err}");
        let lines = dump(&*graph.read());
        assert!(
            lines.iter().any(|l| l.starts_with("V [\"A\"]")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("V [\"B\"]")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_saved_query_calling_a_gql_insert_writes_and_names_its_labels() {
        let graph = modern();
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "adopt", params: #{owner: "string"}, body: `fn adopt(owner) {
                g.gql("MATCH (p:person {name: $owner}) INSERT (p)-[:OWNS]->(:Pet&Cat {owner: $owner})",
                      #{owner: owner}).iterate()
            }`})"#,
        );
        assert_eq!(get(&graph, "adopt", "writes"), "true");
        assert_eq!(get(&graph, "adopt", "dialect"), WRITING_QUERY_DIALECT);
        assert_eq!(
            effects(&graph, "adopt"),
            [
                r#"Create Data(vertex "Pet")"#,
                r#"Create Data(vertex "Cat")"#,
                r#"Create Data(edge "OWNS")"#,
            ]
        );
        let _ = ok(&graph, r#"g.query("adopt", #{owner: "marko"})"#);
        let lines = dump(&*graph.read());
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("E OWNS {} from [\"person\"]") && l.contains("marko")),
            "{lines:?}"
        );
        // A GQL read stays a read-only query.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "names", body: `fn names() {
                g.gql("MATCH (p:person) RETURN p.name AS n").to_list()
            }`})"#,
        );
        assert_eq!(get(&graph, "names", "writes"), "false");
        assert_eq!(get(&graph, "names", "dialect"), QUERY_DIALECT);
        // A parameter the plan needs (LIMIT $k) keeps a read read-only too.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "top", params: #{k: "integer"}, body: `fn top(k) {
                g.gql("MATCH (p:person) RETURN p.name AS n LIMIT $k", #{k: k}).to_list()
            }`})"#,
        );
        assert_eq!(get(&graph, "top", "writes"), "false");
    }

    #[test]
    fn a_computed_gql_text_may_write_anything() {
        let graph = modern();
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "any", params: #{text: "string"},
                body: "fn any(text) { g.gql(text).to_list() }"})"#,
        );
        assert_eq!(get(&graph, "any", "writes"), "true");
        assert_eq!(
            effects(&graph, "any"),
            ["Create Data", "Update Data", "Delete Data"]
        );
        let _ = ok(&graph, r#"g.query("any", #{text: "INSERT (:Free)"})"#);
        assert!(
            dump(&*graph.read())
                .iter()
                .any(|l| l.starts_with("V [\"Free\"]"))
        );
        // A body that hides its calls and runs a computed GQL text is refused at define.
        let err = run(
            &graph,
            r#"g.define_query(#{name: "hidden", params: #{n: "string", t: "string"},
                body: "fn hidden(n, t) { g.query(n); g.gql(t).to_list() }"})"#,
        )
        .unwrap_err();
        assert!(err.contains("g.query(name) with a computed name"), "{err}");
    }

    #[test]
    fn a_read_only_body_calling_a_gql_insert_is_denied_and_changes_nothing() {
        let graph = modern();
        // Stored through the Rust API, which keeps the read-only dialect as given.
        graph
            .write()
            .traversal_mut()
            .define_query(
                "sneak",
                QueryDefinition::new(r#"fn sneak() { g.gql("INSERT (:Sneak)").iterate() }"#),
            )
            .unwrap();
        assert_eq!(get(&graph, "sneak", "dialect"), QUERY_DIALECT);
        let err = run(&graph, r#"g.query("sneak")"#).unwrap_err();
        assert!(
            err.contains(
                r#"Permission denied for 'gql:insert_node' (Create Data(vertex "Sneak"))"#
            ),
            "{err}"
        );
        assert!(err.contains("a saved query is read-only"), "{err}");
        assert_eq!(dump(&*graph.read()), modern_dump());
        // What a read-only query returns cannot be continued into a GQL write either: a
        // `gql()` starts a traversal, so a returned traversal is no start for it.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})"#,
        );
        assert!(run(&graph, r#"g.query("people").gql("INSERT (:X)").iterate()"#).is_err());
        assert_eq!(dump(&*graph.read()), modern_dump());
    }
}
