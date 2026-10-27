//! Forward compatibility (format spec 16): what a NEWER Graphersal writes into a store is
//! never read as damage by this version. An unknown value of a known key in a catalog
//! definition (a compression `codec`, `element` or `path`, an index `element`, a saved query's
//! parameter schema) keeps the definition: it is listed, not applied (`served: false` with a
//! reason) and written back byte for byte through every store path. A known kind whose payload
//! fields this version cannot read is kept opaque. A graph schema this version cannot parse
//! opens the store read-only (never maintenance mode, never a repair). An unknown interrupted
//! operation or store state is refused as unsupported, never damage.
#![cfg(all(feature = "persist", feature = "script"))]

use std::sync::Arc;

use graphersal::catalog::{
    CompressionDefinition, Definition, DefinitionFlags, DefinitionKind, PropertyIndexDefinition,
    QueryDefinition, encode_payload,
};
use graphersal::persist::{MemDir, PersistError, RecoveryTarget, Store, StoreDir, StoreOptions};
use graphersal::prelude::*;
use graphersal::storage::ElementKind;

use crate::script_host::*;

fn options() -> StoreOptions {
    StoreOptions::new().with_checkpoint_after_bytes(0)
}

fn shared(dir: &MemDir) -> Arc<dyn StoreDir> {
    Arc::new(dir.clone())
}

fn text(key: &str, value: &str) -> (String, ElementProperty) {
    (key.to_string(), ElementProperty::from(value))
}

fn payload(pairs: &[(String, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in pairs {
        map.insert(key.as_str(), value.clone());
    }
    map
}

/// A long, compressible text.
fn body(i: usize) -> String {
    format!(
        "{i}: {}",
        "The quick brown fox jumps over the lazy dog. ".repeat(20)
    )
}

fn docs() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for i in 0..5 {
        graph
            .traversal_mut()
            .add_v("doc")
            .property("body", body(i))
            .to_list()
            .unwrap();
    }
    graph
}

/// Compression rules a newer version wrote: an unknown codec, element kind and path syntax,
/// with an unknown extra key.
fn newer_rules() -> Vec<(String, PropertyMap)> {
    let base = |codec: &str, element: &str, path: &str| {
        payload(&[
            text("element", element),
            text("label", "doc"),
            text("path", path),
            text("codec", codec),
            ("min_bytes".to_string(), ElementProperty::from(64i64)),
            ("level".to_string(), ElementProperty::from(19i64)),
        ])
    };
    vec![
        ("zstd_body".to_string(), base("zstd", "vertex", "body")),
        ("property_body".to_string(), base("lz4", "property", "body")),
        (
            "wildcard_body".to_string(),
            base("lz4", "vertex", "$.body[?(@.x)]"),
        ),
    ]
}

fn newer_index() -> PropertyIndexDefinition {
    PropertyIndexDefinition::from_payload(&payload(&[
        text("element", "property"),
        text("label", "doc"),
        text("key", "body"),
        text("type", "memory-ordered"),
    ]))
    .unwrap()
}

/// A saved query whose parameter schema uses a keyword this version does not know.
fn newer_query() -> QueryDefinition {
    QueryDefinition::from_payload(&payload(&[
        text("dialect", "graphersal-rhai/1"),
        text("source", "fn recent(limit) { g.v().limit(limit) }"),
        text(
            "params",
            r#"{"limit":{"schema":{"type":"integer","xFutureKeyword":true},"default":3}}"#,
        ),
    ]))
    .unwrap()
}

/// A compression rule whose payload this version cannot read as a rule at all (no `label`):
/// kept opaque.
fn unreadable_rule() -> Definition {
    let bytes = encode_payload(&payload(&[text("path", "body"), text("codec", "brotli")])).unwrap();
    Definition::opaque(
        DefinitionKind::COMPRESSION,
        DefinitionFlags::NONE,
        "unreadable",
        bytes,
    )
}

fn newer_catalog() -> Vec<Definition> {
    let mut definitions: Vec<Definition> = newer_rules()
        .into_iter()
        .map(|(name, map)| {
            Definition::compression(name, CompressionDefinition::from_payload(&map).unwrap())
        })
        .collect();
    definitions.push(Definition::property_index(
        "doc_body_property",
        newer_index(),
    ));
    definitions.push(Definition::query("recent", newer_query()));
    definitions.push(unreadable_rule());
    definitions
}

fn sorted(mut definitions: Vec<Definition>) -> Vec<Definition> {
    definitions.sort_by(|a, b| (a.kind().code(), a.name()).cmp(&(b.kind().code(), b.name())));
    definitions
}

fn catalog_of(store: &Store) -> Vec<Definition> {
    sorted(
        store
            .graph()
            .read()
            .definitions()
            .unwrap()
            .cloned()
            .collect(),
    )
}

#[test]
fn compression_rules_of_a_newer_version_are_kept_and_not_applied() {
    for (name, map) in newer_rules() {
        let rule = CompressionDefinition::from_payload(&map).unwrap();
        let reason = rule.unserved_reason().expect("not applied");
        assert!(reason.contains("newer Graphersal"), "{name}: {reason}");
        // Written back exactly as read, unknown key included.
        assert_eq!(rule.to_payload(), map, "{name}");

        let mut graph = docs();
        graph
            .set_definition(Definition::compression(name.as_str(), rule))
            .unwrap();
        // The values stay plain: nothing is compressed outside an applied rule.
        graph.check_compression_invariant().unwrap();
        assert_eq!(graph.compression_stats(&name), None, "{name}");
        let err = GraphTraversalSource::new(&mut graph)
            .recompress(&name)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not known to this version"), "{name}: {err}");
    }
    // The listing names the codec as read, not applied, with the reason.
    let mut graph = docs();
    let (name, map) = newer_rules().remove(0);
    graph
        .set_definition(Definition::compression(
            name,
            CompressionDefinition::from_payload(&map).unwrap(),
        ))
        .unwrap();
    let listed = eval_value(
        Arc::new(Graph::new(graph)),
        r#"let r = g.compressions()[0]; [r.codec, r.served, r.reason.contains("zstd")]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(listed, r#"["zstd", false, true]"#);
}

#[test]
fn a_known_rule_on_the_path_of_a_newer_rule_conflicts() {
    let mut graph = docs();
    let (name, map) = newer_rules().remove(0);
    graph
        .set_definition(Definition::compression(
            name,
            CompressionDefinition::from_payload(&map).unwrap(),
        ))
        .unwrap();
    let lz4 = CompressionDefinition::new(ElementKind::Vertex, "doc", "body").unwrap();
    let err = graph
        .set_definition(Definition::compression("lz4_body", lz4))
        .unwrap_err()
        .to_string();
    assert!(err.contains("zstd_body"), "{err}");
}

#[test]
fn an_index_of_a_newer_element_kind_is_kept_not_served_and_never_created() {
    let index = newer_index();
    assert_eq!(index.element_name(), "property");
    let reason = index.unserved_reason().expect("not served");
    assert!(reason.contains("'property'"), "{reason}");
    assert!(index.check_create("doc_body", None).is_err());
    let mut graph = docs();
    graph
        .set_definition(Definition::property_index("doc_body", index))
        .unwrap();
    assert!(graph.property_indexes().unwrap().is_empty());
    let listed = eval_value(
        Arc::new(Graph::new(graph)),
        r#"let i = g.indexes()[0]; [i.element, i.served, i.reason.contains("property")]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(listed, r#"["property", false, true]"#);
}

#[test]
fn a_saved_query_with_parameters_of_a_newer_version_is_kept_and_refused_to_run() {
    let query = newer_query();
    let reason = query.unsupported_reason().expect("not runnable");
    assert!(reason.contains("xFutureKeyword"), "{reason}");
    assert!(query.params().is_empty());
    // The exchange form a user writes stays strict.
    let json = Definition::query("recent", query.clone()).to_json();
    assert!(Definition::from_json(&json).is_err(), "{json}");
    let mut graph = docs();
    graph
        .set_definition(Definition::query("recent", query))
        .unwrap();
    let err = eval_value(
        Arc::new(Graph::new(graph)),
        r#"g.query("recent", #{limit: 1})"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("xFutureKeyword"), "{err}");
}

/// A saved query of a GQL dialect this version does not run (a newer Graphersal wrote it) is
/// listed as unavailable (`status: "error"`, its reason naming the dialects this version runs),
/// refused at a call, and kept byte for byte: its payload, the journal, a checkpoint, a fork and
/// a backup carry it unchanged.
#[test]
fn a_saved_query_of_a_newer_gql_dialect_is_kept_listed_and_refused_to_run() {
    let pairs = [
        text("dialect", "graphersal-gql/3"),
        text("source", "MATCH (p:person) RETURN p FUTURE CLAUSE"),
        text("params", "{}"),
        text("xFutureKey", "kept"),
    ];
    let query = QueryDefinition::from_payload(&payload(&pairs)).unwrap();
    assert_eq!(query.language(), "gql");
    assert_eq!(query.to_payload(), payload(&pairs));
    let bytes = encode_payload(&query.to_payload()).unwrap();

    let dir = MemDir::new();
    let main = shared(&dir);
    let store = Store::create_with(&main, docs(), options()).unwrap();
    store
        .graph()
        .write()
        .set_definition(Definition::query("future", query.clone()))
        .unwrap();
    let listed = eval_value(
        store.graph().clone(),
        r#"let q = g.get_query("future"); [q.status, q.language, q.dialect, q.writes, q.error]"#,
    )
    .unwrap()
    .cast::<rhai::Array>();
    assert_eq!(listed[0].to_string(), "error");
    assert_eq!(listed[1].to_string(), "gql");
    assert_eq!(listed[2].to_string(), "graphersal-gql/3");
    assert!(!listed[3].as_bool().unwrap());
    let reason = listed[4].to_string();
    assert!(
        reason.contains("its dialect is 'graphersal-gql/3', this version runs"),
        "{reason}"
    );
    let err = eval_value(store.graph().clone(), r#"g.query("future")"#)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Invalid body of saved query 'future'"),
        "{err}"
    );
    store.close().unwrap();

    let kept = |store: &Store| {
        let graph = store.graph().read();
        let definition = graph
            .definition(DefinitionKind::QUERY, "future")
            .unwrap()
            .expect("kept");
        let stored = definition.as_query().expect("a query").clone();
        assert_eq!(stored, query);
        assert_eq!(encode_payload(&stored.to_payload()).unwrap(), bytes);
    };
    let store = Store::open(&main, options()).unwrap();
    kept(&store);
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&main, options()).unwrap();
    kept(&store);
    let fork = MemDir::new();
    store.fork(RecoveryTarget::Latest, &fork).unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let forked = Store::open(shared(&fork), options()).unwrap();
    kept(&forked);
    forked.close().unwrap();
    let opened = Store::open_backup(shared(&backup), options()).unwrap();
    kept(&opened);
}

/// Every store path keeps the definitions of a newer version as they were: the WAL, a
/// checkpoint, a fork, a backup; the store opens normally (nothing critical is unknown).
#[test]
fn definitions_of_a_newer_version_survive_every_store_path() {
    let dir = MemDir::new();
    let main = shared(&dir);
    let store = Store::create_with(&main, docs(), options()).unwrap();
    for definition in newer_catalog() {
        store.graph().write().set_definition(definition).unwrap();
    }
    let expected = sorted(newer_catalog());
    assert_eq!(catalog_of(&store), expected);
    store.close().unwrap();

    // From the WAL.
    let store = Store::open(&main, options()).unwrap();
    assert_eq!(store.read_only_reason().unwrap(), None);
    assert!(store.maintenance().is_none());
    assert_eq!(catalog_of(&store), expected);
    assert!(
        catalog_of(&store)
            .iter()
            .any(|d| d.name() == "unreadable" && d.is_opaque())
    );
    store.graph().read().check_compression_invariant().unwrap();
    // From a checkpoint.
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&main, options()).unwrap();
    assert_eq!(catalog_of(&store), expected);
    // A fork and a backup.
    let fork = MemDir::new();
    store.fork(RecoveryTarget::Latest, &fork).unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let forked = Store::open(shared(&fork), options()).unwrap();
    assert_eq!(catalog_of(&forked), expected);
    forked.close().unwrap();
    let opened = Store::open_backup(shared(&backup), options()).unwrap();
    assert_eq!(catalog_of(&opened), expected);
    // A closed-store checkpoint (the merge) and verify.
    let report = Store::verify_dir(&main).unwrap();
    assert!(report.is_ok(), "{report:?}");
}

/// Replaces `from` by `to` (same length) inside the WAL records of `dir` and fixes each
/// patched record's body CRC (format spec 9.2), as the newer writer would have written it.
fn patch_wal(dir: &MemDir, from: &[u8], to: &[u8]) {
    assert_eq!(from.len(), to.len());
    let mut patched = false;
    for entry in dir.list("wal").unwrap() {
        let name = format!("wal/{}", entry.name);
        let mut bytes = dir.read(&name, u64::MAX).unwrap();
        let mut at = 64;
        while at + 25 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let payload = at + 25..at + 25 + len;
            if let Some(found) = bytes[payload.clone()]
                .windows(from.len())
                .position(|w| w == from)
            {
                let start = payload.start + found;
                bytes[start..start + to.len()].copy_from_slice(to);
                let crc = crate::store_tests::crc32c(&bytes[payload.clone()]);
                bytes[at + 21..at + 25].copy_from_slice(&crc.to_le_bytes());
                patched = true;
            }
            at = payload.end;
        }
        dir.write_atomic(&name, &bytes).unwrap();
    }
    assert!(patched, "the WAL holds {:?}", String::from_utf8_lossy(from));
}

#[test]
fn a_schema_of_a_newer_version_in_the_wal_opens_read_only_never_damage() {
    let dir = MemDir::new();
    let main = shared(&dir);
    let store = Store::create_with(&main, docs(), options()).unwrap();
    let schema = graphersal::schema::GraphSchema::from_json(
        r#"{"mode":"open","vertices":{"doc":{"schema":{"type":"object","description":"QQQQQQQQ"}}}}"#,
    )
    .unwrap();
    GraphTraversalSource::new(&mut *store.graph().write())
        .set_schema(schema)
        .unwrap();
    store.close().unwrap();
    patch_wal(
        &dir,
        br#""description":"QQQQQQQQ""#,
        br#""xFuture":"QQQQQQQQQQQQ""#,
    );
    let files = dir.file_names();
    let graph_file = dir.read("GRAPH", u64::MAX).unwrap();

    let store = Store::open(&main, options()).unwrap();
    let reason = store.read_only_reason().unwrap().expect("read-only");
    assert!(reason.contains("newer Graphersal"), "{reason}");
    assert!(reason.contains("xFuture"), "{reason}");
    assert!(store.maintenance().is_none());
    assert_eq!(store.graph().read().vertex_count(), 5);
    let err = store
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .to_list()
        .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
    let err = store.checkpoint(None).unwrap_err();
    assert!(matches!(err, PersistError::ReadOnly { .. }), "{err}");
    store.close().unwrap();

    // Nothing that would write a state without the schema runs; none of it is damage.
    let err = Store::checkpoint_dir(&main, options(), None).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    let err = Store::repair_dir(&main, MemDir::new()).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    let err = Store::fork_dir(&main, RecoveryTarget::Latest, MemDir::new()).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    let report = Store::verify_dir(&main).unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert!(
        report.notes.iter().any(|n| n.contains("xFuture")),
        "{report:?}"
    );
    let info = Store::inspect(&main).unwrap();
    assert!(info.commit_seq >= 1);
    // A backup copies the bytes as they are.
    Store::backup_dir(&main, MemDir::new()).unwrap();
    assert_eq!(dir.file_names(), files);
    assert_eq!(dir.read("GRAPH", u64::MAX).unwrap(), graph_file);
}

#[test]
fn an_interrupted_operation_of_a_newer_version_is_refused_not_damage() {
    let dir = MemDir::new();
    let main = shared(&dir);
    Store::create(&main, options()).unwrap().close().unwrap();
    dir.write_atomic("INTENT", br#"{"op":"future_operation","files":[]}"#)
        .unwrap();
    let Err(err) = Store::open(&main, options()) else {
        panic!("refused")
    };
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("future_operation"), "{err}");
}

/// The GRAPH file's state word (format spec 5.1) set to a value this version does not know,
/// both copies, checksums fixed as a newer writer would have written them.
fn set_graph_state(dir: &MemDir, state: u32) {
    for name in ["GRAPH", "GRAPH.copy"] {
        let mut bytes = dir.read(name, u64::MAX).unwrap();
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let fixed = bytes.len() - 4 - word(40) as usize * 32;
        bytes[12..16].copy_from_slice(&state.to_le_bytes());
        let crc = crate::store_tests::crc32c(&bytes[..fixed - 4]);
        bytes[fixed - 4..fixed].copy_from_slice(&crc.to_le_bytes());
        dir.write_atomic(name, &bytes).unwrap();
    }
}

#[test]
fn a_store_state_of_a_newer_version_is_refused_not_damage() {
    let dir = MemDir::new();
    let main = shared(&dir);
    Store::create(&main, options()).unwrap().close().unwrap();
    set_graph_state(&dir, 7);
    let Err(err) = Store::open(&main, options()) else {
        panic!("refused")
    };
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    let err = Store::verify_dir(&main).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
    let err = Store::repair_dir(&main, MemDir::new()).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedContent { .. }),
        "{err}"
    );
}
