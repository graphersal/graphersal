//! Vector rules (book page "Vector Rules"): the schema keyword `from` on a vector property
//! (parse, print, placement, chains, models), the derived write at the end of the outermost
//! unit (once per element, the five removal cases, multi-label vertices, edges, labels added,
//! compressed and deduplicated sources), the refusal of direct writes, the exempt paths
//! (`apply_change_set`), failures that roll the unit back (a model this build cannot run, a
//! cancel, the memory ceiling), the first fill in the schema-changing unit, rule changes and
//! removals, the preview (`validate_schema`, `diff_schema`, the one warning text), the journal
//! records of a fill, and a model replacement that re-embeds (`model2vec`). The counting and
//! failing embedder cases are unit tests of `graph/vector_rules.rs`.

use std::sync::{Arc, Mutex};

use graphersal::catalog::{Codec, CompressionDefinition};
use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use graphersal::schema::{FillReason, SchemaChangeKind, fill_warning};
use graphersal::storage::ElementKind;
use graphersal::vector::{EmbedContext, Embedder, HashNgram, Vector};

const RULE: &str =
    r#"{"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "text"}"#;

/// A schema whose vertex label `doc` (mode `mode`) has `text` and the rule `emb` from `text`.
fn rule_schema(mode: &str) -> GraphSchema {
    GraphSchema::from_json(&format!(
        r#"{{"mode": "{mode}", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{
            "text": {{"type": "string"}}, "emb": {RULE}}}}}}}}}}}"#
    ))
    .unwrap()
}

fn ruled() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .set_schema(rule_schema("none"))
        .unwrap();
    graph
}

fn embedded(text: &str) -> Vector {
    HashNgram::new()
        .embed(&[text], &EmbedContext::new())
        .unwrap()
        .remove(0)
}

fn add_doc(graph: &mut TraversalGraph, id: &str, text: ElementProperty) {
    graph
        .traversal_mut()
        .add_v("doc")
        .property("name", id)
        .property("text", text)
        .to_list()
        .unwrap();
}

/// The `emb` of the vertex named `id`, if any.
fn emb(graph: &TraversalGraph, id: &str) -> Option<Vector> {
    let values = graph
        .traversal()
        .v(None)
        .has("name", id)
        .values("emb")
        .to_list()
        .unwrap();
    values.into_iter().next().map(|value| match value {
        ElementProperty::Vector(v) => v,
        other => panic!("not a vector: {other:?}"),
    })
}

fn set_text(graph: &mut TraversalGraph, id: &str, value: ElementProperty) {
    graph
        .traversal_mut()
        .v(None)
        .has("name", id)
        .property("text", value)
        .to_list()
        .unwrap();
}

// ---------------------------------------------------------------------------------------------
// The keyword
// ---------------------------------------------------------------------------------------------

#[test]
fn a_rule_parses_prints_and_round_trips() {
    let schema = rule_schema("open");
    let vector = schema.vertices["doc"].schema.properties["emb"]
        .vector
        .as_ref()
        .unwrap();
    assert_eq!(vector.from.as_deref(), Some("text"));
    assert_eq!(vector.embedder.as_deref(), Some("hash-ngram/1"));
    let json = schema.to_json();
    assert!(
        json.contains(r#""from":"text""#) || json.contains(r#""from": "text""#),
        "{json}"
    );
    assert_eq!(GraphSchema::from_json(&json).unwrap(), schema);
    // Inference never invents a rule.
    let graph = ruled();
    let mut graph = graph;
    add_doc(&mut graph, "1", "graph databases".into());
    let inferred = graph.traversal().infer_schema().unwrap();
    assert!(
        !inferred.to_json().contains("\"from\""),
        "{}",
        inferred.to_json()
    );
}

fn refused(json: &str) -> String {
    let mut graph = TraversalGraph::new();
    let schema = match GraphSchema::from_json(json) {
        Ok(schema) => schema,
        Err(err) => return err.to_string(),
    };
    match graph.traversal_mut().set_schema(schema) {
        Ok(()) => panic!("accepted: {json}"),
        Err(err) => format!("{err} {}", err.help().unwrap_or_default()),
    }
}

#[test]
fn from_without_embedder_is_refused() {
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "emb": {"type": "array", "format": "vector", "from": "text"}}}}}}"#,
    );
    assert!(err.contains("from needs \"embedder\""), "{err}");
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "text": {"type": "string", "from": "x"}}}}}}"#,
    );
    assert!(err.contains("from applies to array values"), "{err}");
}

#[test]
fn from_below_the_top_level_is_refused() {
    let err = refused(&format!(
        r#"{{"vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{
            "meta": {{"type": "object", "properties": {{"emb": {RULE}}}}}}}}}}}}}}}"#
    ));
    assert!(
        err.contains("applies only to a label's top-level vector properties"),
        "{err}"
    );
}

#[test]
fn a_chain_of_rules_is_refused() {
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "a": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "text"},
            "b": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "a"}}}}}}"#,
    );
    assert!(err.contains("rules cannot be chained"), "{err}");
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "emb"}}}}}}"#,
    );
    assert!(err.contains("names its own property"), "{err}");
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "n": {"type": "integer"},
            "emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "n"}}}}}}"#,
    );
    assert!(err.contains("would always remove emb"), "{err}");
}

#[test]
fn a_rule_naming_an_unknown_model_is_refused_with_the_import_command() {
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "emb": {"type": "array", "format": "vector", "embedder": "acme/minilm", "from": "text"}}}}}}"#,
    );
    assert!(err.contains("names the model acme/minilm"), "{err}");
    assert!(
        err.contains("graphersal embed-model import <repo> --into <store>"),
        "{err}"
    );
    // An identity with a normalization override is no stored model: the help says to drop it.
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "emb": {"type": "array", "format": "vector",
                    "embedder": "model2vec/acme/tiny@0123456789ab?normalize=false", "from": "text"}}}}}}"#,
    );
    assert!(err.contains("drop the override normalize=false"), "{err}");
    assert!(
        err.contains("\"embedder\": \"model2vec/acme/tiny@0123456789ab\""),
        "{err}"
    );
    // A declared dimension that the model does not make.
    let err = refused(
        r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
            "emb": {"type": "array", "format": "vector", "dimension": 8,
                    "embedder": "hash-ngram/1", "from": "text"}}}}}}"#,
    );
    assert!(err.contains("makes vectors of dimension 256"), "{err}");
}

#[test]
fn the_rule_declares_the_dimension_of_its_model() {
    let graph = ruled();
    let schema = graph.traversal().get_schema().unwrap().unwrap();
    let vector = schema.vertices["doc"].schema.properties["emb"]
        .vector
        .clone()
        .unwrap();
    assert_eq!(vector.dimension, Some(256));
}

// ---------------------------------------------------------------------------------------------
// The derived write
// ---------------------------------------------------------------------------------------------

#[test]
fn writing_from_fills_to_in_the_same_unit() {
    let mut graph = ruled();
    add_doc(&mut graph, "1", "graph databases".into());
    assert_eq!(emb(&graph, "1"), Some(embedded("graph databases")));
    set_text(&mut graph, "1", "vector search".into());
    assert_eq!(emb(&graph, "1"), Some(embedded("vector search")));
    // Inside the unit the old value is read; the vector appears at the end of the unit.
    let seen = graph
        .transaction(|g| -> Result<Option<Vector>, GraphError> {
            set_text(g, "1", "later text".into());
            Ok(emb(g, "1"))
        })
        .unwrap();
    assert_eq!(seen, Some(embedded("vector search")));
    assert_eq!(emb(&graph, "1"), Some(embedded("later text")));
}

struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

fn sets_emb(changes: &ChangeSet) -> bool {
    changes
        .mutations()
        .iter()
        .any(|m| matches!(m, Mutation::SetProperty { key, .. } if key == "emb"))
}

#[test]
fn the_vector_is_in_the_change_set_and_the_dry_run() {
    let mut graph = ruled();
    let seen = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    add_doc(&mut graph, "1", "graph databases".into());
    assert!(sets_emb(seen.lock().unwrap().last().unwrap()));
    let ((), changes) = graph
        .dry_run(|g| -> Result<(), GraphError> {
            set_text(g, "1", "dry text".into());
            Ok(())
        })
        .unwrap();
    assert!(sets_emb(&changes), "{changes:?}");
    assert_eq!(emb(&graph, "1"), Some(embedded("graph databases")));
}

#[test]
fn apply_change_set_takes_to_as_given() {
    let mut source = ruled();
    let seen = Arc::new(Mutex::new(Vec::new()));
    source
        .register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    add_doc(&mut source, "1", "graph databases".into());
    let changes = seen.lock().unwrap().last().unwrap().clone();
    // The replica's rule names the same model; the change set's vector is stored as it is
    // (never recomputed: the unit tests count the embedder calls).
    let mut replica = ruled();
    replica.apply_change_set(&changes).unwrap();
    assert_eq!(emb(&replica, "1"), Some(embedded("graph databases")));
}

fn assert_removed_by(value: ElementProperty) {
    let mut graph = ruled();
    add_doc(&mut graph, "1", "graph databases".into());
    assert!(emb(&graph, "1").is_some());
    set_text(&mut graph, "1", value);
    assert_eq!(emb(&graph, "1"), None);
}

#[test]
fn removing_from_removes_to() {
    let mut graph = ruled();
    add_doc(&mut graph, "1", "graph databases".into());
    graph
        .traversal_mut()
        .v(None)
        .has("name", "1")
        .properties(vec!["text"])
        .drop()
        .to_list()
        .unwrap();
    assert_eq!(emb(&graph, "1"), None);
}

#[test]
fn null_from_removes_to() {
    // `property(k, null)` removes the key in TinkerPop; the stored null of mode none too.
    let mut graph = ruled();
    add_doc(&mut graph, "1", "graph databases".into());
    let _ = graph
        .traversal_mut()
        .v(None)
        .has("name", "1")
        .property("text", ElementProperty::Null)
        .to_list();
    assert_eq!(emb(&graph, "1"), None);
}

#[test]
fn empty_from_removes_to() {
    assert_removed_by("".into());
}

#[test]
fn non_string_from_removes_to() {
    assert_removed_by(ElementProperty::from(42i64));
}

#[test]
fn text_without_features_removes_to() {
    // hash-ngram/1 finds no word in separators only: `EmbedError::NoFeatures`.
    assert_removed_by(" .,;- ".into());
}

#[test]
fn a_multi_label_vertex_follows_the_first_declaring_label() {
    let mut graph = TraversalGraph::new();
    let schema = GraphSchema::from_json(
        r#"{"vertices": {
            "a": {"schema": {"type": "object", "properties": {"emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "title"}}}},
            "b": {"schema": {"type": "object", "properties": {"emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "body"}}}}}}"#,
    )
    .unwrap();
    graph.traversal_mut().set_schema(schema).unwrap();
    graph
        .traversal_mut()
        .add_v(vec!["b", "a"])
        .property("name", "1")
        .property("title", "the title")
        .property("body", "the body")
        .to_list()
        .unwrap();
    assert_eq!(emb(&graph, "1"), Some(embedded("the body")));
    // Writing the other label's source changes nothing.
    graph
        .traversal_mut()
        .v(None)
        .has("name", "1")
        .property("title", "a new title")
        .to_list()
        .unwrap();
    assert_eq!(emb(&graph, "1"), Some(embedded("the body")));
}

#[test]
fn edges_follow_their_rule() {
    let mut graph = TraversalGraph::new();
    let schema = GraphSchema::from_json(&format!(
        r#"{{"edges": {{"cites": {{"schema": {{"type": "object", "properties": {{"emb": {RULE}}}}}}}}}}}"#
    ))
    .unwrap();
    graph.traversal_mut().set_schema(schema).unwrap();
    graph
        .traversal_mut()
        .add_v("doc")
        .as_("a")
        .add_v("doc")
        .add_e("cites")
        .from("a")
        .property("text", "edge text")
        .to_list()
        .unwrap();
    let vectors = graph.traversal().e(None).values("emb").to_list().unwrap();
    assert_eq!(
        vectors,
        vec![ElementProperty::Vector(embedded("edge text"))]
    );
}

#[test]
fn adding_a_label_with_a_rule_embeds() {
    let mut graph = ruled();
    graph
        .traversal_mut()
        .add_v("note")
        .property("name", "1")
        .property("text", "graph databases")
        .to_list()
        .unwrap();
    assert_eq!(emb(&graph, "1"), None);
    graph
        .traversal_mut()
        .v(None)
        .has("name", "1")
        .add_label("doc")
        .to_list()
        .unwrap();
    assert_eq!(emb(&graph, "1"), Some(embedded("graph databases")));
}

#[test]
fn compressed_and_deduped_sources_are_read_as_text() {
    let long = "graph databases keep vertices and edges with their properties ".repeat(4);
    for codec in [Codec::Lz4, Codec::Dedup] {
        let mut graph = ruled();
        graph
            .traversal_mut()
            .define_compression(
                "text",
                CompressionDefinition::new(ElementKind::Vertex, "doc", "text")
                    .unwrap()
                    .with_codec(codec)
                    .with_min_bytes(25),
            )
            .unwrap();
        add_doc(&mut graph, "1", long.as_str().into());
        add_doc(&mut graph, "2", long.as_str().into());
        assert_eq!(emb(&graph, "1"), Some(embedded(&long)), "{codec:?}");
        assert_eq!(emb(&graph, "2"), Some(embedded(&long)), "{codec:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Direct writes
// ---------------------------------------------------------------------------------------------

#[test]
fn writing_to_directly_is_refused_with_help() {
    let mut graph = ruled();
    add_doc(&mut graph, "1", "graph databases".into());
    let before = emb(&graph, "1");
    let attempts: Vec<Box<dyn Fn(&mut TraversalGraph) -> Result<(), String>>> = vec![
        Box::new(|g| {
            g.traversal_mut()
                .v(None)
                .has("name", "1")
                .property("emb", ElementProperty::Vector(embedded("x")))
                .to_list()
                .map(|_| ())
                .map_err(|e| format!("{} {}", e.root_cause(), e.help().unwrap_or_default()))
        }),
        Box::new(|g| {
            g.traversal_mut()
                .v(None)
                .has("name", "1")
                .properties(vec!["emb"])
                .drop()
                .to_list()
                .map(|_| ())
                .map_err(|e| format!("{} {}", e.root_cause(), e.help().unwrap_or_default()))
        }),
        Box::new(|g| {
            g.traversal_mut()
                .add_v("doc")
                .property("text", "t")
                .property("emb", ElementProperty::Vector(embedded("x")))
                .to_list()
                .map(|_| ())
                .map_err(|e| format!("{} {}", e.root_cause(), e.help().unwrap_or_default()))
        }),
    ];
    for attempt in attempts {
        let err = attempt(&mut graph).expect_err("refused");
        assert!(
            err.contains("is computed by a vector rule from 'text'"),
            "{err}"
        );
        assert!(err.contains("write text instead"), "{err}");
        assert!(err.contains("g.v(id).property(\"text\", \"...\")"), "{err}");
    }
    assert_eq!(graph.vertex_count(), 1);
    assert_eq!(emb(&graph, "1"), before);
    // Another label's property of the same name is free.
    graph
        .traversal_mut()
        .add_v("other")
        .property("emb", ElementProperty::Vector(embedded("x")))
        .to_list()
        .unwrap();
}

// ---------------------------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------------------------

#[test]
fn cancel_rolls_the_fill_back() {
    let mut graph = TraversalGraph::new();
    add_doc(&mut graph, "1", "graph databases".into());
    let token = CancelToken::new();
    token.cancel();
    let err = token
        .scope(|| graph.traversal_mut().set_schema(rule_schema("none")))
        .unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert!(graph.traversal().get_schema().unwrap().is_none());
    assert_eq!(emb(&graph, "1"), None);
}

#[test]
fn the_memory_ceiling_stops_a_fill() {
    let mut graph = TraversalGraph::new();
    for i in 0..2_000 {
        add_doc(
            &mut graph,
            &i.to_string(),
            format!("text number {i}").into(),
        );
    }
    let footprint = graph.memory_usage().unwrap().unwrap().total_bytes();
    // Room for the graph, not for 2,000 vectors of 1 KiB each.
    let err = graphersal::exec::memory_limit_scope(footprint + 256 * 1024, || {
        graph.traversal_mut().set_schema(rule_schema("none"))
    })
    .unwrap_err();
    assert!(err.to_string().contains("vector rule fill"), "{err}");
    assert!(graph.traversal().get_schema().unwrap().is_none());
    assert_eq!(emb(&graph, "0"), None);
}

// ---------------------------------------------------------------------------------------------
// First fill, changes, removal
// ---------------------------------------------------------------------------------------------

#[test]
fn adding_a_rule_fills_every_element_in_the_schema_unit() {
    let mut graph = TraversalGraph::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    add_doc(&mut graph, "1", "graph databases".into());
    add_doc(&mut graph, "2", "".into());
    // An element that already holds a vector is re-embedded (no keep-existing option).
    graph
        .traversal_mut()
        .add_v("doc")
        .property("name", "3")
        .property("text", "vector search")
        .property("emb", ElementProperty::Vector(embedded("stale")))
        .to_list()
        .unwrap();
    graph
        .traversal_mut()
        .add_v("doc")
        .property("name", "4")
        .property("emb", ElementProperty::Vector(embedded("stale")))
        .to_list()
        .unwrap();
    let commits = seen.lock().unwrap().len();
    graph
        .traversal_mut()
        .set_schema(rule_schema("open"))
        .unwrap();
    let all = seen.lock().unwrap();
    assert_eq!(all.len(), commits + 1, "one unit");
    let unit = all.last().unwrap();
    assert!(
        unit.mutations()
            .iter()
            .any(|m| matches!(m, Mutation::SetSchema { .. }))
    );
    drop(all);
    assert_eq!(emb(&graph, "1"), Some(embedded("graph databases")));
    assert_eq!(emb(&graph, "2"), None);
    assert_eq!(emb(&graph, "3"), Some(embedded("vector search")));
    assert_eq!(emb(&graph, "4"), None, "no text: the vector is removed");
}

#[test]
fn changing_the_rule_refills() {
    let mut graph = ruled();
    graph
        .traversal_mut()
        .add_v("doc")
        .property("name", "1")
        .property("text", "graph databases")
        .property("title", "a title")
        .to_list()
        .unwrap();
    let patch = graphersal::schema::SchemaPatch::from_json(
        r#"{"vertices": {"doc": {"schema": {"properties": {"emb": {"from": "title"}}}}}}"#,
    )
    .unwrap();
    let validation = graph.traversal().validate_schema_patch(&patch).unwrap();
    assert_eq!(validation.fill.len(), 1);
    assert_eq!(validation.fill[0].reason(), FillReason::RuleChanged);
    assert!(
        validation
            .diff
            .changes
            .iter()
            .any(|c| c.kind == SchemaChangeKind::VectorRuleChanged && !c.compatible)
    );
    graph.traversal_mut().patch_schema(&patch).unwrap();
    assert_eq!(emb(&graph, "1"), Some(embedded("a title")));
}

#[test]
fn removing_a_rule_keeps_the_vectors() {
    let mut graph = ruled();
    add_doc(&mut graph, "1", "graph databases".into());
    let patch = graphersal::schema::SchemaPatch::from_json(
        r#"{"vertices": {"doc": {"schema": {"properties": {"emb": {"from": null}}}}}}"#,
    )
    .unwrap();
    let diff = graph
        .traversal()
        .validate_schema_patch(&patch)
        .unwrap()
        .diff;
    assert!(
        diff.changes
            .iter()
            .any(|c| c.kind == SchemaChangeKind::VectorRuleRemoved && c.compatible)
    );
    graph.traversal_mut().patch_schema(&patch).unwrap();
    assert_eq!(emb(&graph, "1"), Some(embedded("graph databases")));
    // Now a plain property: written directly, not recomputed.
    set_text(&mut graph, "1", "other".into());
    assert_eq!(emb(&graph, "1"), Some(embedded("graph databases")));
    graph
        .traversal_mut()
        .v(None)
        .has("name", "1")
        .property("emb", ElementProperty::Vector(embedded("by hand")))
        .to_list()
        .unwrap();
}

#[test]
fn a_rolled_back_schema_change_leaves_no_vector() {
    let mut graph = TraversalGraph::new();
    add_doc(&mut graph, "1", "graph databases".into());
    let result = graph.transaction(|g| -> Result<(), TraverserError> {
        g.traversal_mut()
            .set_schema(rule_schema("none"))
            .map_err(|e| *e)?;
        Err(TraverserError::Graph(GraphError::unsupported("abort")))
    });
    assert!(result.is_err());
    assert_eq!(emb(&graph, "1"), None);
    assert!(graph.traversal().get_schema().unwrap().is_none());
    // A later write of the source is no rule write either.
    set_text(&mut graph, "1", "again".into());
    assert_eq!(emb(&graph, "1"), None);
}

// ---------------------------------------------------------------------------------------------
// Preview and the warning
// ---------------------------------------------------------------------------------------------

#[test]
fn the_preview_reports_the_planned_fill() {
    let mut graph = TraversalGraph::new();
    add_doc(&mut graph, "1", "graph databases".into());
    add_doc(&mut graph, "2", "vector search".into());
    graph
        .traversal_mut()
        .add_v("doc")
        .property("name", "3")
        .property("emb", ElementProperty::Vector(embedded("stale")))
        .to_list()
        .unwrap();
    let schema = rule_schema("none");
    let validation = graph.traversal().validate_schema(&schema).unwrap();
    assert_eq!(validation.fill.len(), 1);
    let fill = &validation.fill[0];
    assert_eq!(
        (fill.label(), fill.from(), fill.to(), fill.model()),
        ("doc", "text", "emb", "hash-ngram/1")
    );
    assert_eq!(fill.reason(), FillReason::RuleAdded);
    assert_eq!((fill.elements(), fill.removals()), (2, 1));
    assert!(!fill.approximate());
    assert!(fill.estimate_ms().is_some());
    assert!(!fill.needs_confirmation());
    // The diff counts the label's elements from the statistics.
    let diff = graph.traversal().diff_schema(&schema).unwrap();
    assert_eq!(diff.fill.len(), 1);
    assert!(diff.fill[0].approximate());
    assert_eq!(diff.fill[0].elements(), 3);
    // Nothing is filled when the rule is already there.
    graph.traversal_mut().set_schema(schema.clone()).unwrap();
    assert!(
        graph
            .traversal()
            .validate_schema(&schema)
            .unwrap()
            .fill
            .is_empty()
    );
}

#[test]
fn the_warning_text_is_built_once() {
    let mut graph = TraversalGraph::new();
    add_doc(&mut graph, "1", "graph databases".into());
    let validation = graph
        .traversal()
        .validate_schema(&rule_schema("none"))
        .unwrap();
    let text = validation.fill_warning().unwrap();
    assert_eq!(Some(text.clone()), fill_warning(&validation.fill));
    assert!(text.starts_with(
        "Applying this schema fills vectors: 1 element of doc.emb from doc.text with the model \
         hash-ngram/1 (under a second)."
    ));
    assert!(
        text.contains("The whole database is LOCKED while this runs"),
        "{text}"
    );
    assert!(text.ends_with("If it fails, nothing is written."), "{text}");
    // The data forms carry the same text.
    let value = validation.to_value();
    let ElementProperty::Object(map) = value else {
        panic!("not a map")
    };
    assert_eq!(
        map.get("fill_warning").and_then(ElementProperty::as_str),
        Some(text.as_str())
    );
}

// ---------------------------------------------------------------------------------------------
// The journal
// ---------------------------------------------------------------------------------------------

#[test]
fn the_journal_records_start_and_end_of_a_fill() {
    let mut data = TraversalGraph::new();
    add_doc(&mut data, "1", "graph databases".into());
    add_doc(&mut data, "2", "".into());
    let graph = Graph::new(data);
    graph
        .write()
        .traversal_mut()
        .set_schema(rule_schema("none"))
        .unwrap();
    let schema_commit = graph.read().last_commit_seq();
    // An ordinary write is no fill: nothing journaled.
    graph
        .write()
        .traversal_mut()
        .v(None)
        .has("name", "1")
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
    let (started, done) = (fills[0], fills[1]);
    assert_eq!(started.kind, "vector_fill.started");
    assert_eq!(
        started.field("reason").and_then(ElementProperty::as_str),
        Some("rule_added")
    );
    assert_eq!(done.kind, "vector_fill.done");
    assert_eq!(done.op, Some(started.seq));
    assert_eq!(done.commit, Some(schema_commit));
    assert!(graph.vector_fill_progress().is_none());
}

// ---------------------------------------------------------------------------------------------
// A store's model (feature model2vec: the tiny synthetic model embeds)
// ---------------------------------------------------------------------------------------------

#[cfg(all(feature = "persist", not(target_arch = "wasm32")))]
mod store_models {
    use super::*;
    use crate::model_fixture::*;
    use graphersal::persist::{ImportModelOptions, ModelSource, PersistError, Store, StoreFailure};

    fn rule_naming(model: &str) -> GraphSchema {
        GraphSchema::from_json(&format!(
            r#"{{"vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{
                "text": {{"type": "string"}},
                "emb": {{"type": "array", "format": "vector", "embedder": "{model}", "from": "text"}}}}}}}}}}}}"#
        ))
        .unwrap()
    }

    fn add(store: &Store, text: &str) -> Result<(), String> {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("doc")
            .property("text", text)
            .to_list()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_rule_names_the_model_by_its_repository() {
        let store = Store::create(graphersal::persist::MemDir::new(), options()).unwrap();
        let identity = import(&store, 1, "acme/tiny").model.identity;
        let applied = store
            .graph()
            .write()
            .traversal_mut()
            .set_schema(rule_naming("acme/tiny"));
        let schema = store.graph().read().get_schema().unwrap();
        if cfg!(feature = "model2vec") {
            applied.unwrap();
            let schema = schema.unwrap();
            let vector = schema.vertices["doc"].schema.properties["emb"]
                .vector
                .clone()
                .unwrap();
            assert_eq!(vector.embedder.as_deref(), Some(identity.as_str()));
            assert_eq!(vector.dimension, Some(DIMENSION as u32));
            add(&store, "graph vector").unwrap();
            let count = store
                .graph()
                .read()
                .traversal()
                .v(None)
                .values("emb")
                .count()
                .next()
                .unwrap();
            assert_eq!(count, Some(ElementProperty::from(1i64)));
        } else {
            // Empty graph: nothing to fill, the schema applies; a write fails in this build.
            applied.unwrap();
            assert!(schema.is_some());
            let err = add(&store, "graph vector").unwrap_err();
            assert!(err.contains("vector rule"), "{err}");
            assert_eq!(store.graph().read().vertex_count(), 0);
        }
    }

    #[test]
    fn a_replace_refuses_vectors_without_a_rule() {
        let store = Store::create(graphersal::persist::MemDir::new(), options()).unwrap();
        let identity = import(&store, 1, "acme/tiny").model.identity;
        set_schema(&store, &identity);
        let err = store
            .import_model(
                ModelSource::Files(tiny_model(2, "acme/tiny")),
                ImportModelOptions::new().with_replace(true),
            )
            .unwrap_err();
        match &err {
            PersistError::Store { cause, .. } => {
                assert_eq!(*cause, StoreFailure::ModelInUse, "{err}")
            }
            other => panic!("{other}"),
        }
        assert!(err.to_string().contains("doc.emb"), "{err}");
        assert!(err.to_string().contains("no vector rule"), "{err}");
    }

    #[cfg(feature = "model2vec")]
    #[test]
    fn a_replace_re_embeds_the_rules_of_the_old_revision() {
        let store = Store::create(graphersal::persist::MemDir::new(), options()).unwrap();
        let old = import(&store, 1, "acme/tiny").model.identity;
        store
            .graph()
            .write()
            .traversal_mut()
            .set_schema(rule_naming("acme/tiny"))
            .unwrap();
        add(&store, "graph vector").unwrap();
        let before = store
            .graph()
            .read()
            .traversal()
            .v(None)
            .values("emb")
            .to_list()
            .unwrap();
        let report = store
            .import_model(
                ModelSource::Files(tiny_model(2, "acme/tiny")),
                ImportModelOptions::new().with_replace(true),
            )
            .unwrap();
        let new = report.model.identity;
        assert_ne!(old, new);
        let schema = store.graph().read().get_schema().unwrap().unwrap();
        assert_eq!(
            schema.vertices["doc"].schema.properties["emb"]
                .vector
                .as_ref()
                .unwrap()
                .embedder
                .as_deref(),
            Some(new.as_str())
        );
        let after = store
            .graph()
            .read()
            .traversal()
            .v(None)
            .values("emb")
            .to_list()
            .unwrap();
        assert_ne!(before, after, "re-embedded by the new revision");
        let records = store
            .graph()
            .system_journal()
            .unwrap()
            .query(&graphersal::system_journal::JournalQuery::new())
            .unwrap()
            .records;
        let started = records.iter().find(|r| {
            r.kind == "vector_fill.started"
                && r.field("reason").and_then(ElementProperty::as_str) == Some("model_replaced")
        });
        assert!(started.is_some(), "{records:?}");
    }
}
