//! The catalog of definitions in a Store on every backend (a directory, memory, one file;
//! format spec 6.3, 9.3, 16): existing stores accept definitions without an upgrade, WAL
//! replay and checkpoints keep the catalog, unknown definitions are written back byte for
//! byte (checkpoint, fork, full and incremental backup, ZIP, repair), a rollback removes a
//! later define, and a critical definition of an unknown kind opens the store read-only
//! (from a manifest and from a WAL record).
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
use graphersal::catalog::{DefinitionFlags, QueryParam};
use graphersal::persist::{
    FsDir, MemDir, PersistError, RecoveryTarget, RollbackMode, SingleFileDir, Store, StoreDir,
    StoreOptions,
};
use graphersal::prelude::PropertyMap;
use graphersal::prelude::*;
use graphersal::schema::PropertySchema;

use crate::store_tests::crc32c;

/// A fresh directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-defs-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, Copy)]
enum Backend {
    Dir,
    Memory,
    File,
}

const BACKENDS: [Backend; 3] = [Backend::Dir, Backend::Memory, Backend::File];

impl Backend {
    fn make(self, temp: &Temp, name: &str) -> Arc<dyn StoreDir> {
        match self {
            Backend::Dir => Arc::new(FsDir::new(temp.0.join(name))),
            Backend::Memory => Arc::new(MemDir::named(name)),
            Backend::File => Arc::new(SingleFileDir::new(temp.0.join(format!("{name}.gstore")))),
        }
    }
}

fn options() -> StoreOptions {
    StoreOptions::new()
        .with_wal_segment_bytes(600)
        .with_checkpoint_after_bytes(0)
}

fn query(name: &str) -> Definition {
    Definition::query(
        name,
        QueryDefinition::new(format!("fn {name}(limit) {{ g.v().limit(limit) }}"))
            .with_folder("reports/sales")
            .with_description("the first vertices")
            .with_param(
                "limit",
                QueryParam::new(PropertySchema::integer()).with_default(10),
            ),
    )
}

/// A definition of a kind this version does not know (kept opaque).
fn unknown(name: &str, critical: bool) -> Definition {
    Definition::opaque(
        DefinitionKind::from_code(77),
        DefinitionFlags::NONE.with_critical(critical),
        name,
        vec![9, 8, 7, 0, 255, 1],
    )
}

/// The encoded definition (format spec 6.3) of a small opaque one: kind, flags, name,
/// length, payload (every length below 128 is one byte).
fn record_bytes(definition: &Definition) -> Vec<u8> {
    let payload = definition.opaque_payload().unwrap();
    let mut out = vec![
        definition.kind().code(),
        definition.flags().bits(),
        definition.name().len() as u8,
    ];
    out.extend_from_slice(definition.name().as_bytes());
    out.push(payload.len() as u8);
    out.extend_from_slice(payload);
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn catalog(store: &Store) -> Vec<Definition> {
    store
        .graph()
        .read()
        .definitions()
        .unwrap()
        .cloned()
        .collect()
}

/// The manifest of the store's newest snapshot.
fn latest_manifest(dir: &dyn StoreDir) -> Vec<u8> {
    let mut names: Vec<String> = dir
        .list("snapshots")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .filter(|n| !n.starts_with("tmp"))
        .collect();
    names.sort();
    let newest = names.last().expect("a snapshot");
    dir.read(&format!("snapshots/{newest}/manifest"), u64::MAX)
        .unwrap()
}

fn set(store: &Store, definition: Definition) {
    store.graph().write().set_definition(definition).unwrap();
}

#[test]
fn the_catalog_round_trips_on_every_backend_and_unknown_definitions_stay_byte_identical() {
    for backend in BACKENDS {
        let temp = Temp::new("roundtrip");
        let main = backend.make(&temp, "main");
        // An existing store: written with an empty catalog (definitions count 0), closed.
        Store::create(&main, options()).unwrap().close().unwrap();
        // It opens unchanged and accepts definitions right away (WAL op 10).
        let store = Store::open(&main, options()).unwrap();
        set(&store, query("top"));
        set(&store, unknown("future", false));
        let expected = catalog(&store);
        assert_eq!(expected.len(), 2, "{backend:?}");
        store.close().unwrap();

        // Reopen: the catalog comes from the WAL replay.
        let store = Store::open(&main, options()).unwrap();
        assert!(store.open_report().commits_replayed >= 2, "{backend:?}");
        assert_eq!(catalog(&store), expected, "{backend:?}");
        assert!(store.read_only_reason().unwrap().is_none());
        // A checkpoint writes the table; the unknown definition is written back byte for byte.
        store.checkpoint(None).unwrap();
        assert!(
            contains(&latest_manifest(main.as_ref()), &record_bytes(&expected[1])),
            "{backend:?}"
        );
        store.close().unwrap();

        // Reopen: the catalog comes from the manifest.
        let store = Store::open(&main, options()).unwrap();
        assert_eq!(store.open_report().commits_replayed, 0, "{backend:?}");
        assert_eq!(catalog(&store), expected, "{backend:?}");

        // Change, move, describe and drop go through the WAL too.
        let before_define = store.graph().read().last_commit_seq();
        set(&store, query("later"));
        {
            let mut graph = store.graph().write();
            let mut moved = query("top");
            moved
                .as_query_mut()
                .unwrap()
                .set_folder(Some("archive".into()));
            moved
                .as_query_mut()
                .unwrap()
                .set_description(Some("moved".into()));
            graph.set_definition(moved).unwrap();
        }
        let expected = catalog(&store);
        assert_eq!(expected.len(), 3);
        // Verify reads every manifest and WAL record, op 10 included.
        assert!(store.verify().unwrap().is_ok(), "{backend:?}");

        // Fork, full backup, incremental backup, repair: the catalog is kept.
        let fork = backend.make(&temp, "fork");
        store.fork(RecoveryTarget::Latest, &fork).unwrap();
        let forked = Store::open(&fork, options()).unwrap();
        assert_eq!(catalog(&forked), expected, "{backend:?} fork");
        assert!(contains(
            &latest_manifest(fork.as_ref()),
            &record_bytes(&unknown("future", false))
        ));
        forked.close().unwrap();

        let backup = backend.make(&temp, "backup");
        store.backup(&backup).unwrap();
        set(&store, query("after_backup"));
        store.backup(&backup).unwrap();
        let expected = catalog(&store);
        let opened = Store::open_backup(&backup, options()).unwrap();
        assert_eq!(catalog(&opened), expected, "{backend:?} incremental backup");
        drop(opened);

        #[cfg(feature = "persist-zip")]
        {
            let mut zip = std::io::Cursor::new(Vec::new());
            store.backup_zip(&mut zip).unwrap();
            let zip = zip.into_inner();
            let restored = backend.make(&temp, "zip");
            Store::restore_zip(zip.as_slice(), &restored).unwrap();
            let opened = Store::open(&restored, options()).unwrap();
            assert_eq!(catalog(&opened), expected, "{backend:?} zip");
        }

        let repaired = backend.make(&temp, "repaired");
        store.repair_to(&repaired).unwrap();
        let opened = Store::open(&repaired, options()).unwrap();
        assert_eq!(catalog(&opened), expected, "{backend:?} repair");
        assert!(contains(
            &latest_manifest(repaired.as_ref()),
            &record_bytes(&unknown("future", false))
        ));
        drop(opened);

        // A rollback to before a define removes it.
        store
            .rollback_to(RecoveryTarget::CommitSeq(before_define), RollbackMode::Move)
            .unwrap();
        let names: Vec<String> = catalog(&store)
            .iter()
            .map(|d| d.name().to_string())
            .collect();
        assert_eq!(names, ["top", "future"], "{backend:?}");
        assert_eq!(
            store
                .graph()
                .read()
                .definition(DefinitionKind::QUERY, "top")
                .unwrap()
                .and_then(|d| d.as_query())
                .and_then(|q| q.folder()),
            Some("reports/sales")
        );
        store.close().unwrap();
    }
}

/// Writes are refused with a read-only error, the GRAPH file stays as it was.
fn assert_read_only(main: &Arc<dyn StoreDir>, backend: Backend) {
    let graph_file = main.read("GRAPH", u64::MAX).unwrap();
    let store = Store::open(main, options()).unwrap();
    let reason = store.read_only_reason().unwrap().expect("read-only");
    assert!(reason.contains("critical"), "{reason}");
    assert_eq!(
        store.open_report().read_only.as_deref(),
        Some(reason.as_str())
    );
    assert!(store.maintenance().is_none());
    // The data is readable, every write is refused.
    assert_eq!(catalog(&store).len(), 1);
    let err = store
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .to_list()
        .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{backend:?}: {err}");
    // Its own help (a newer version), not the generic commit-hook help.
    let text = graphersal::error::format_diagnostic(&err);
    assert!(text.contains("newer Graphersal"), "{text}");
    assert!(!text.contains("write-ahead"), "{text}");
    let err = store.checkpoint(None).unwrap_err();
    assert!(matches!(err, PersistError::ReadOnly { .. }), "{err}");
    assert!(err.help().unwrap().contains("newer"), "{err:?}");
    assert!(matches!(
        store.mark("m"),
        Err(PersistError::ReadOnly { .. })
    ));
    // A fork keeps the definition and is read-only for this version as well.
    let fork_temp = Temp::new("readonly-fork");
    let fork = backend.make(&fork_temp, "fork");
    store.fork(RecoveryTarget::Latest, &fork).unwrap();
    store.close().unwrap();
    assert_eq!(main.read("GRAPH", u64::MAX).unwrap(), graph_file);
    let forked = Store::open(&fork, options()).unwrap();
    assert!(forked.read_only_reason().unwrap().is_some(), "{backend:?}");
}

#[test]
fn a_critical_unknown_definition_in_a_manifest_opens_the_store_read_only() {
    for backend in BACKENDS {
        let temp = Temp::new("critical-manifest");
        let main = backend.make(&temp, "main");
        // A graph without a journal holds it (an in-memory graph is not a store).
        let mut graph = TraversalGraph::new();
        graph.set_definition(unknown("index", true)).unwrap();
        Store::create_with(&main, graph, options())
            .unwrap()
            .close()
            .unwrap();
        assert_read_only(&main, backend);
    }
}

#[test]
fn a_critical_unknown_definition_in_a_wal_record_opens_the_store_read_only() {
    for backend in BACKENDS {
        let temp = Temp::new("critical-wal");
        let main = backend.make(&temp, "main");
        let store = Store::create(&main, options()).unwrap();
        set(&store, unknown("index", false));
        store.close().unwrap();
        // A newer version wrote it as critical: set the flags byte and the record's body CRC
        // (format spec 9.2) as that writer would have.
        let needle = record_bytes(&unknown("index", false));
        let mut patched = false;
        for entry in main.list("wal").unwrap() {
            let name = format!("wal/{}", entry.name);
            let mut bytes = main.read(&name, u64::MAX).unwrap();
            let mut at = 64;
            while at + 25 <= bytes.len() {
                let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
                let payload = at + 25..at + 25 + len;
                if let Some(found) = bytes[payload.clone()]
                    .windows(needle.len())
                    .position(|w| w == needle)
                {
                    bytes[payload.start + found + 1] = DefinitionFlags::CRITICAL;
                    let crc = crc32c(&bytes[payload.clone()]);
                    bytes[at + 21..at + 25].copy_from_slice(&crc.to_le_bytes());
                    patched = true;
                }
                at = payload.end;
            }
            main.write_atomic(&name, &bytes).unwrap();
        }
        assert!(patched, "{backend:?}");
        assert_read_only(&main, backend);
    }
}

/// A host-kind definition (kinds 128..=255, payload encoded with the public payload codec) is
/// kept byte for byte through the WAL, a checkpoint, a fork and full and incremental backups
/// on every backend, is never damage for verify, and a critical one opens read-only like any
/// critical kind the library does not decode.
#[test]
fn a_host_kind_round_trips_through_every_store_path() {
    use graphersal::catalog::{Definition, DefinitionKind};
    use graphersal::catalog::{decode_payload, encode_payload};

    let kind = DefinitionKind::host(200).unwrap();
    let mut payload = PropertyMap::default();
    payload.insert("label", "person".into());
    payload.insert("keys", ElementProperty::from(vec!["name", "age"]));
    payload.insert("id", ElementProperty::Uuid(42));
    let host = |name: &str, critical: bool| {
        Definition::opaque(
            kind,
            DefinitionFlags::NONE.with_critical(critical),
            name,
            encode_payload(&payload).unwrap(),
        )
    };
    let decoded = |store: &Store| {
        let graph = store.graph().read();
        let definition = graph
            .definition(kind, "person_name")
            .unwrap()
            .expect("host definition");
        decode_payload(definition.opaque_payload().unwrap()).unwrap()
    };
    for backend in BACKENDS {
        let temp = Temp::new("host-kind");
        let main = backend.make(&temp, "main");
        let store = Store::create(&main, options()).unwrap();
        set(&store, host("person_name", false));
        set(&store, query("top"));
        let expected = catalog(&store);
        store.close().unwrap();

        // WAL replay.
        let store = Store::open(&main, options()).unwrap();
        assert!(store.open_report().commits_replayed >= 2, "{backend:?}");
        assert!(store.read_only_reason().unwrap().is_none(), "{backend:?}");
        assert_eq!(catalog(&store), expected, "{backend:?}");
        assert_eq!(decoded(&store), payload, "{backend:?}");
        // Checkpoint: the record is written back byte for byte.
        store.checkpoint(None).unwrap();
        assert!(
            contains(
                &latest_manifest(main.as_ref()),
                &record_bytes(&host("person_name", false))
            ),
            "{backend:?}"
        );
        assert!(store.verify().unwrap().is_ok(), "{backend:?}");
        store.close().unwrap();
        let store = Store::open(&main, options()).unwrap();
        assert_eq!(store.open_report().commits_replayed, 0, "{backend:?}");
        assert_eq!(catalog(&store), expected, "{backend:?}");

        // Fork, full backup, incremental backup.
        let fork = backend.make(&temp, "fork");
        store.fork(RecoveryTarget::Latest, &fork).unwrap();
        let forked = Store::open(&fork, options()).unwrap();
        assert_eq!(catalog(&forked), expected, "{backend:?} fork");
        assert_eq!(decoded(&forked), payload, "{backend:?} fork");
        forked.close().unwrap();

        let backup = backend.make(&temp, "backup");
        store.backup(&backup).unwrap();
        set(&store, host("later", false));
        store.backup(&backup).unwrap();
        let expected = catalog(&store);
        let opened = Store::open_backup(&backup, options()).unwrap();
        assert_eq!(catalog(&opened), expected, "{backend:?} backup");
        assert_eq!(decoded(&opened), payload, "{backend:?} backup");
        drop(opened);
        store.close().unwrap();

        // A critical host definition: the store opens read-only (format spec 16 rule 3).
        let critical = backend.make(&temp, "critical");
        let mut graph = TraversalGraph::new();
        graph.set_definition(host("index", true)).unwrap();
        Store::create_with(&critical, graph, options())
            .unwrap()
            .close()
            .unwrap();
        assert_read_only(&critical, backend);
    }
}

/// `StoreInfo::definitions`: the open store counts its catalog; `Store::inspect` counts it from
/// the files (the latest snapshot's table, then the WAL's catalog changes after it), on every
/// backend.
#[test]
fn store_info_counts_the_definitions() {
    for backend in BACKENDS {
        let temp = Temp::new("info-count");
        let main = backend.make(&temp, "main");
        let store = Store::create(&main, options()).unwrap();
        assert_eq!(store.info().unwrap().definitions, 0, "{backend:?}");
        set(&store, query("a"));
        set(&store, query("b"));
        assert_eq!(store.info().unwrap().definitions, 2, "{backend:?}");
        store.checkpoint(None).unwrap();
        // After the checkpoint: one more define, one drop, one replace (WAL only).
        set(&store, query("c"));
        store
            .graph()
            .write()
            .remove_definition(DefinitionKind::QUERY, "a")
            .unwrap();
        set(&store, query("b"));
        assert_eq!(store.info().unwrap().definitions, 2, "{backend:?}");
        store.close().unwrap();
        assert_eq!(Store::inspect(&main).unwrap().definitions, 2, "{backend:?}");
    }
}

/// A compression rule on every backend (`book/src/users_guide/compressed_properties.md`): the
/// files hold plain values, every load (WAL replay, checkpoint, fork, backups, ZIP, repair,
/// rollback, attic restore, read-only open) compresses the rule paths again and keeps the
/// invariant.
#[test]
fn compression_rules_survive_every_store_path() {
    use graphersal::catalog::Definition;
    use graphersal::{catalog::CompressionDefinition, storage::ElementKind};

    fn text(i: usize) -> String {
        format!(
            "CV {i}: {}",
            "Senior engineer, led the platform team in Rust. ".repeat(12)
        )
    }
    fn check(graph: &TraversalGraph, expected_values: usize, what: &str) {
        graph
            .check_compression_invariant()
            .unwrap_or_else(|reason| panic!("{what}: {reason}"));
        assert_eq!(
            graph.memory_usage().unwrap().unwrap().compressed_values,
            expected_values as u64,
            "{what}"
        );
        let read = graph.traversal().v(None).values("cv").to_list().unwrap();
        assert_eq!(read.len(), expected_values, "{what}");
        assert!(read.iter().all(|v| !v.holds_compressed()), "{what}");
    }
    let add = |store: &Store, i: usize| {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("customer")
            .property("cv", text(i).as_str())
            .to_list()
            .unwrap();
    };
    for backend in BACKENDS {
        let temp = Temp::new("compression");
        let main = backend.make(&temp, "main");
        let store = Store::create(&main, options()).unwrap();
        add(&store, 0);
        set(
            &store,
            Definition::compression(
                "cv",
                CompressionDefinition::new(ElementKind::Vertex, "customer", "cv").unwrap(),
            ),
        );
        add(&store, 1);
        check(&store.graph().read(), 2, "live");
        let before_third = store.graph().read().last_commit_seq();
        add(&store, 2);
        store.close().unwrap();

        // WAL replay, then a checkpoint and a load from the manifest.
        let store = Store::open(&main, options()).unwrap();
        check(&store.graph().read(), 3, "wal replay");
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&main, options()).unwrap();
        check(&store.graph().read(), 3, "manifest");
        assert!(store.verify().unwrap().is_ok(), "{backend:?}");

        let fork = backend.make(&temp, "fork");
        store.fork(RecoveryTarget::Latest, &fork).unwrap();
        let forked = Store::open(&fork, options()).unwrap();
        check(&forked.graph().read(), 3, "fork");
        forked.close().unwrap();
        let backup = backend.make(&temp, "backup");
        store.backup(&backup).unwrap();
        let opened = Store::open_backup(&backup, options()).unwrap();
        check(&opened.graph().read(), 3, "backup");
        drop(opened);
        #[cfg(feature = "persist-zip")]
        {
            let mut zip = std::io::Cursor::new(Vec::new());
            store.backup_zip(&mut zip).unwrap();
            let restored = backend.make(&temp, "zip");
            Store::restore_zip(zip.into_inner().as_slice(), &restored).unwrap();
            let opened = Store::open(&restored, options()).unwrap();
            check(&opened.graph().read(), 3, "zip");
            opened.close().unwrap();
        }
        let repaired = backend.make(&temp, "repaired");
        store.repair_to(&repaired).unwrap();
        let opened = Store::open(&repaired, options()).unwrap();
        check(&opened.graph().read(), 3, "repair");
        opened.close().unwrap();
        let read_only = Store::open_read_only(&main, RecoveryTarget::Latest).unwrap();
        check(&read_only.graph, 3, "read-only open");

        // Rollback to before the third value, then the attic restore brings it back.
        store
            .rollback_to(RecoveryTarget::CommitSeq(before_third), RollbackMode::Move)
            .unwrap();
        check(&store.graph().read(), 2, "rollback");
        let entry = store.attic_list().unwrap().remove(0);
        store.attic_restore(&entry.id).unwrap();
        check(&store.graph().read(), 3, "attic restore");
        store.close().unwrap();
    }
}

#[test]
fn a_property_index_with_a_transform_survives_every_store_path() {
    use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexTransform};
    use graphersal::storage::ElementKind;

    let definition = Definition::property_index(
        "person_name_to_lower",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name")
            .with_transform(Some(PropertyIndexTransform::ToLower)),
    );
    // The definition is stored, and the index is rebuilt from the data with `entries` entries.
    let check = |graph: &TraversalGraph, entries: u64, what: &str| {
        assert_eq!(
            graph
                .definition(DefinitionKind::PROPERTY_INDEX, "person_name_to_lower")
                .unwrap(),
            Some(&definition),
            "{what}"
        );
        graph
            .check_property_indexes()
            .unwrap_or_else(|reason| panic!("{what}: {reason}"));
        let infos = graph.property_indexes().unwrap();
        assert_eq!(infos.len(), 1, "{what}");
        assert_eq!(
            infos[0].transform,
            Some(PropertyIndexTransform::ToLower),
            "{what}"
        );
        assert_eq!(infos[0].stats.entries, entries, "{what}");
    };
    let add = |store: &Store, name: &str| {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("person")
            .property("name", name)
            .to_list()
            .unwrap();
    };
    for backend in BACKENDS {
        let temp = Temp::new("lower-index");
        let main = backend.make(&temp, "main");
        let store = Store::create(&main, options()).unwrap();
        add(&store, "ADA");
        set(&store, definition.clone());
        add(&store, "Grace");
        check(&store.graph().read(), 2, "live");
        let before_third = store.graph().read().last_commit_seq();
        add(&store, "ALAN");
        store.close().unwrap();

        let store = Store::open(&main, options()).unwrap();
        check(&store.graph().read(), 3, "wal replay");
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&main, options()).unwrap();
        check(&store.graph().read(), 3, "manifest");
        assert!(store.verify().unwrap().is_ok(), "{backend:?}");

        let fork = backend.make(&temp, "fork");
        store.fork(RecoveryTarget::Latest, &fork).unwrap();
        let forked = Store::open(&fork, options()).unwrap();
        check(&forked.graph().read(), 3, "fork");
        forked.close().unwrap();
        let backup = backend.make(&temp, "backup");
        store.backup(&backup).unwrap();
        let opened = Store::open_backup(&backup, options()).unwrap();
        check(&opened.graph().read(), 3, "backup");
        drop(opened);
        #[cfg(feature = "persist-zip")]
        {
            let mut zip = std::io::Cursor::new(Vec::new());
            store.backup_zip(&mut zip).unwrap();
            let restored = backend.make(&temp, "zip");
            Store::restore_zip(zip.into_inner().as_slice(), &restored).unwrap();
            let opened = Store::open(&restored, options()).unwrap();
            check(&opened.graph().read(), 3, "zip");
            opened.close().unwrap();
        }
        let repaired = backend.make(&temp, "repaired");
        store.repair_to(&repaired).unwrap();
        let opened = Store::open(&repaired, options()).unwrap();
        check(&opened.graph().read(), 3, "repair");
        opened.close().unwrap();
        let read_only = Store::open_read_only(&main, RecoveryTarget::Latest).unwrap();
        check(&read_only.graph, 3, "read-only open");

        store
            .rollback_to(RecoveryTarget::CommitSeq(before_third), RollbackMode::Move)
            .unwrap();
        check(&store.graph().read(), 2, "rollback");
        let entry = store.attic_list().unwrap().remove(0);
        store.attic_restore(&entry.id).unwrap();
        check(&store.graph().read(), 3, "attic restore");
        store.close().unwrap();
    }
}
