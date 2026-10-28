//! File format version 3 (temporal v2, stage 3; format spec 3 and 15): the nanosecond temporal
//! value tags 15 to 20 (`localdatetime`, `datetime`, `datetime` with a zone name, `time`,
//! `offsettime`, `duration`). Every file this build writes is of version 3; every temporal kind
//! at its range edges with nanoseconds, zone names, offset times and the longest durations goes
//! through every persistence path (packed snapshot, WAL replay, checkpoint, verify, backup,
//! ZIP backup, rollback and attic fork, fork, repair, packed export, read-only views); the
//! golden fixtures of Graphersal 0.2.3 (file format version 2) are read exactly and continued
//! in version 3; the microsecond tags 9 to 13 stay valid in a version 3 file; a nanosecond tag
//! in a version 1 or 2 file is damage; a zone name this build does not know is unsupported
//! content, never damage; damaged payloads never panic; `now()` on a pinned nanosecond clock
//! is stored exactly.
#![cfg(feature = "persist")]

use std::sync::Arc;

use graphersal::persist::{
    self, Durability, Journal, JournalRecord, MemDir, PersistError, ReadOptions, RecoveryTarget,
    RollbackMode, SharedBuffer, Store, StoreDir, StoreOptions, WriteOptions,
};
use graphersal::prelude::*;
use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, OffsetTime, Temporal, Time};

use crate::store_tests::crc32c;

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn t(value: Temporal) -> ElementProperty {
    ElementProperty::Temporal(value)
}

fn dt(text: &str) -> ElementProperty {
    t(Temporal::DateTime(
        text.parse::<DateTime>()
            .unwrap_or_else(|err| panic!("{text}: {err}")),
    ))
}

fn local(text: &str) -> ElementProperty {
    t(Temporal::LocalDateTime(
        text.parse::<LocalDateTime>().unwrap(),
    ))
}

fn time(text: &str) -> ElementProperty {
    t(Temporal::Time(text.parse::<Time>().unwrap()))
}

fn offset_time(text: &str) -> ElementProperty {
    t(Temporal::OffsetTime(text.parse::<OffsetTime>().unwrap()))
}

fn date(text: &str) -> ElementProperty {
    t(Temporal::Date(text.parse::<Date>().unwrap()))
}

fn duration(months: i32, days: i32, nanos: i128) -> ElementProperty {
    t(Temporal::Duration(
        Duration::with_nanos(months, days, nanos).unwrap(),
    ))
}

/// Every field of a value, recursively: a `datetime` with its offset and zone name (equality
/// compares only the instant), an `offsettime` with its offset, containers item by item.
fn exact(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Temporal(Temporal::Duration(d)) => {
            format!("duration({}, {}, {})", d.months(), d.days(), d.nanos())
        }
        ElementProperty::Temporal(t) => format!("{}({t})", t.kind()),
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(exact).collect();
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{key}: {}", exact(item)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        other => format!("{other:?}"),
    }
}

fn exact_map(map: &PropertyMap) -> Vec<String> {
    map.iter()
        .map(|(key, value)| format!("{key} = {}", exact(value)))
        .collect()
}

/// Every temporal kind of file format version 3 at the edges of its range, with digits below
/// the microsecond, zone names (one longer than an inline string), offset times and the
/// longest durations; nested in a list and an object too.
fn v3_values() -> Vec<(&'static str, ElementProperty)> {
    vec![
        ("date_min", date("-9999-01-01")),
        ("date_max", date("9999-12-31")),
        ("local_min", local("-9999-01-01T00:00:00")),
        ("local_min_ns", local("-9999-01-01T00:00:00.000000001")),
        ("local_max", local("9999-12-31T23:59:59.999999999")),
        ("local_before_epoch", local("1969-12-31T23:59:59.999999999")),
        ("dt_min", dt("-9999-01-01T00:00:00-18:00")),
        ("dt_max", dt("9999-12-31T23:59:59.999999999+18:00")),
        ("dt_before_epoch", dt("1969-12-31T23:59:59.000000001Z")),
        ("dt_offset_seconds", dt("1890-01-01T00:00:00.5+00:19:32")),
        (
            "zoned",
            dt("2024-03-31T03:30:00.000000001+02:00[Europe/Berlin]"),
        ),
        ("zoned_winter", dt("2024-01-01T10:00[Europe/Berlin]")),
        (
            "zoned_long_name",
            dt("2024-07-01T10:00:00.123456789[America/Argentina/ComodRivadavia]"),
        ),
        ("zoned_max", dt("9999-12-30T12:00:00.999999999[Asia/Tokyo]")),
        (
            "zoned_min",
            dt("-9999-01-03T00:00:00.000000001[America/New_York]"),
        ),
        ("zoned_utc", dt("2024-01-01T00:00:00Z[UTC]")),
        ("time_min", time("00:00:00")),
        ("time_max", time("23:59:59.999999999")),
        ("time_ns", time("12:00:00.000000001")),
        ("ot_min", offset_time("00:00:00+18:00")),
        ("ot_max", offset_time("23:59:59.999999999-18:00")),
        ("ot_ns", offset_time("10:00:00.000000001+05:45")),
        ("dur_max", duration(i32::MAX, i32::MAX, Duration::MAX_NANOS)),
        ("dur_min", duration(i32::MIN, i32::MIN, Duration::MIN_NANOS)),
        ("dur_minus_ns", duration(0, 0, -1)),
        ("dur_ns", duration(0, 0, 1)),
        ("dur_mixed", duration(1, -2, -1_500_000_001)),
        (
            "dur_beyond_micros",
            duration(0, 0, i64::MAX as i128 * 1000 + 1000),
        ),
        ("dur_zero", duration(0, 0, 0)),
        (
            "nested",
            ElementProperty::Array(vec![
                dt("2024-01-01T10:00:00.000000001[Europe/Berlin]"),
                ElementProperty::Object(Box::new({
                    let mut map = PropertyMap::default();
                    map.insert("at", offset_time("10:00:00.5+02:00"));
                    map.insert("when", dt("2024-07-01T10:00[Europe/Berlin]"));
                    map.insert("span", duration(0, 0, 7));
                    map
                })),
            ]),
        ),
    ]
}

fn v3_map() -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in v3_values() {
        map.insert(key, value);
    }
    map
}

const EDGE_VALUE: &str = "2001-02-03T04:05:06.000000007-03:30[America/St_Johns]";

/// A graph with one vertex `t` holding [`v3_values`] and an edge `e` holding a zoned datetime.
fn v3_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    graph
        .transaction(|g| {
            let t = g.add_vertex(Some("t".into()), vec!["temporal".into()], v3_map())?;
            let u = g.add_vertex(
                Some("u".into()),
                vec!["temporal".into()],
                PropertyMap::default(),
            )?;
            let mut edge = PropertyMap::default();
            edge.insert("since", dt(EDGE_VALUE));
            g.add_edge(Some("e".into()), Some("knows".into()), t, u, edge)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    graph
}

fn vertex_map(graph: &TraversalGraph, id: &str) -> PropertyMap {
    let v = graph
        .get_vertex_by_id(id)
        .unwrap()
        .unwrap_or_else(|| panic!("vertex {id}"));
    graph
        .vertex_properties(v)
        .unwrap()
        .cloned()
        .unwrap_or_default()
}

#[track_caller]
fn assert_holds_v3_values(graph: &TraversalGraph, id: &str) {
    assert_eq!(exact_map(&vertex_map(graph, id)), exact_map(&v3_map()));
}

#[track_caller]
fn assert_v3_graph(graph: &TraversalGraph) {
    assert_holds_v3_values(graph, "t");
    let e = graph.get_edge_by_id("e").unwrap().expect("edge");
    assert_eq!(
        exact(
            graph
                .edge_properties(e)
                .unwrap()
                .unwrap()
                .get("since")
                .unwrap()
        ),
        exact(&dt(EDGE_VALUE))
    );
}

fn add_vertex(store: &Store, id: &str, props: PropertyMap) {
    store
        .graph()
        .write()
        .transaction(|g| {
            g.add_vertex(Some(id.into()), vec!["temporal".into()], props)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
}

fn version_word(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[8..12].try_into().unwrap())
}

/// The file format version of every manifest, segment and WAL segment of `dir`.
fn file_versions(dir: &MemDir) -> Vec<(String, u32)> {
    dir.file_names()
        .into_iter()
        .filter(|name| {
            name.ends_with("/manifest") || name.ends_with(".seg") || name.ends_with(".wal")
        })
        .map(|name| {
            let bytes = dir.read(&name, u64::MAX).unwrap();
            let version = version_word(&bytes);
            (name, version)
        })
        .collect()
}

/// Copies a directory of the source tree into a new `MemDir`.
fn fixture_mem(path: &std::path::Path) -> MemDir {
    fn walk(root: &std::path::Path, at: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((relative.replace('\\', "/"), std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(path, path, &mut files);
    let dir = MemDir::new();
    for (relative, bytes) in files {
        if let Some((parent, _)) = relative.rsplit_once('/') {
            dir.create_dir_all(parent).unwrap();
        }
        dir.write_atomic(&relative, &bytes).unwrap();
    }
    dir
}

fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.2.3")
}

/// A journal stream with one commit that writes `value` to vertex `x` (an uncompressed record).
fn journal_with(value: ElementProperty) -> Vec<u8> {
    let mut graph = TraversalGraph::new();
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::EveryCommit)
        .attach(&mut graph)
        .unwrap();
    graph
        .transaction(|g| {
            let mut props = PropertyMap::default();
            props.insert("v", value);
            g.add_vertex(Some("x".into()), vec!["x".into()], props)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    wal.contents()
}

fn read_all(bytes: &[u8]) -> Result<Vec<JournalRecord>, PersistError> {
    persist::read_journal(bytes)?.collect()
}

/// The value written by the one commit of a [`journal_with`] stream.
fn journal_value(bytes: &[u8]) -> Result<ElementProperty, PersistError> {
    let mut graph = TraversalGraph::new();
    for record in read_all(bytes)? {
        if let JournalRecord::Commit(changes) = record {
            graph.apply_change_set(&changes).unwrap();
        }
    }
    let v = graph.get_vertex_by_id("x").unwrap().unwrap();
    Ok(graph
        .vertex_properties(v)
        .unwrap()
        .unwrap()
        .get("v")
        .unwrap()
        .clone())
}

const WAL_HEADER: usize = 64;
const FRAME: usize = 25;

/// Rewrites a journal header's version word (and its checksum).
fn with_header_version(mut bytes: Vec<u8>, version: u32) -> Vec<u8> {
    bytes[8..12].copy_from_slice(&version.to_le_bytes());
    let crc = crc32c(&bytes[..60]);
    bytes[60..64].copy_from_slice(&crc.to_le_bytes());
    bytes
}

/// The record of [`journal_with`] with its payload replaced (frame length and checksums fixed,
/// so the decoder itself meets the bytes).
fn with_payload(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = bytes[..WAL_HEADER + FRAME].to_vec();
    out[WAL_HEADER + 4..WAL_HEADER + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    let header_crc = crc32c(&out[WAL_HEADER..WAL_HEADER + 17]);
    out[WAL_HEADER + 17..WAL_HEADER + 21].copy_from_slice(&header_crc.to_le_bytes());
    out[WAL_HEADER + 21..WAL_HEADER + 25].copy_from_slice(&crc32c(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Replaces `from` by `to` (same length) inside the WAL records of `dir` and fixes each
/// patched record's body CRC (format spec 9.2), as a newer writer would have written it.
fn patch_wal(dir: &MemDir, from: &[u8], to: &[u8]) {
    assert_eq!(from.len(), to.len());
    let mut patched = false;
    for entry in dir.list("wal").unwrap() {
        let name = format!("wal/{}", entry.name);
        let mut bytes = dir.read(&name, u64::MAX).unwrap();
        let mut at = WAL_HEADER;
        while at + FRAME <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let payload = at + FRAME..at + FRAME + len;
            if let Some(found) = bytes[payload.clone()]
                .windows(from.len())
                .position(|w| w == from)
            {
                let start = payload.start + found;
                bytes[start..start + to.len()].copy_from_slice(to);
                let crc = crc32c(&bytes[payload.clone()]);
                bytes[at + 21..at + 25].copy_from_slice(&crc.to_le_bytes());
                patched = true;
            }
            at = payload.end;
        }
        dir.write_atomic(&name, &bytes).unwrap();
    }
    assert!(patched, "the WAL holds {:?}", String::from_utf8_lossy(from));
}

// ---------------------------------------------------------------------------------------------
// Version 3 everywhere
// ---------------------------------------------------------------------------------------------

#[test]
fn every_file_this_build_writes_is_of_file_format_version_3() {
    assert_eq!(persist::FORMAT_VERSION, 3);
    assert_eq!(persist::STORE_FORMAT_VERSION, 3, "the store version stays");
    let mut packed = Vec::new();
    persist::write_snapshot(&v3_graph(), &mut packed, &WriteOptions::new()).unwrap();
    assert_eq!(&packed[..8], b"GRSLPACK");
    assert_eq!(version_word(&packed), 3);

    let dir = MemDir::new();
    let store = Store::create_with(&dir, v3_graph(), StoreOptions::new()).unwrap();
    add_vertex(&store, "w", v3_map());
    store.checkpoint(None).unwrap();
    add_vertex(&store, "w2", v3_map());
    store.close().unwrap();
    let versions = file_versions(&dir);
    assert!(
        versions.iter().any(|(n, _)| n.ends_with(".wal")),
        "{versions:?}"
    );
    assert!(
        versions.iter().any(|(n, _)| n.ends_with(".seg")),
        "{versions:?}"
    );
    assert!(versions.iter().all(|(_, v)| *v == 3), "{versions:?}");
    let graph = dir.read("GRAPH", u64::MAX).unwrap();
    assert_eq!(version_word(&graph), 3, "the store format version");
}

#[test]
fn a_packed_snapshot_keeps_every_temporal_value_exactly() {
    let mut bytes = Vec::new();
    persist::write_snapshot(&v3_graph(), &mut bytes, &WriteOptions::new()).unwrap();
    let loaded = persist::read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
    assert_v3_graph(&loaded);
    // Written again from what was read: the same bytes but for the snapshot's time.
    let mut again = Vec::new();
    persist::write_snapshot(&loaded, &mut again, &WriteOptions::new()).unwrap();
    assert_eq!(bytes.len(), again.len());
}

#[test]
fn a_journal_keeps_every_temporal_value_exactly() {
    for (name, value) in v3_values() {
        let bytes = journal_with(value.clone());
        assert_eq!(version_word(&bytes), 3);
        assert_eq!(
            exact(&journal_value(&bytes).unwrap()),
            exact(&value),
            "{name}"
        );
    }
}

#[test]
fn a_store_keeps_every_temporal_value_through_every_path() {
    let dir = MemDir::new();
    let store = Store::create_with(&dir, v3_graph(), StoreOptions::new()).unwrap();
    add_vertex(&store, "w", v3_map());
    let after_w = store.graph().read().last_commit_seq();
    store.graph().write().mark("after-w").unwrap();
    add_vertex(&store, "later", v3_map());
    store.close().unwrap();

    // WAL replay.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    {
        let lock = store.graph().read();
        assert_v3_graph(&lock);
        assert_holds_v3_values(&lock, "w");
        assert_holds_v3_values(&lock, "later");
    }
    // A checkpoint (the merge of the WAL into the base snapshot), then a load from it.
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    {
        let lock = store.graph().read();
        assert_v3_graph(&lock);
        assert_holds_v3_values(&lock, "later");
    }
    let report = store.verify().unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
    assert!(report.notes.is_empty(), "{report:?}");
    // A checkpoint of the closed store too.
    add_vertex(&store, "closed", v3_map());
    store.close().unwrap();
    Store::checkpoint_dir(&dir, StoreOptions::new(), None).unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_holds_v3_values(&store.graph().read(), "closed");

    // A read-only view of a past state (the mark before `later`).
    let past = store
        .read_only_at(RecoveryTarget::Mark("after-w".into()))
        .unwrap();
    assert_holds_v3_values(&past.graph, "w");
    assert!(past.graph.get_vertex_by_id("later").unwrap().is_none());

    // A verified backup, opened read-only.
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    {
        let opened = Store::open_backup(&backup, StoreOptions::new()).unwrap();
        let lock = opened.graph().read();
        assert_v3_graph(&lock);
        assert_holds_v3_values(&lock, "closed");
    }
    assert!(file_versions(&backup).iter().all(|(_, v)| *v == 3));

    // A ZIP backup, unpacked into a live store.
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = std::io::Cursor::new(Vec::new());
        store.backup_zip(&mut zip).unwrap();
        let unpacked = MemDir::new();
        Store::restore_zip(zip.into_inner().as_slice(), &unpacked).unwrap();
        let restored = Store::open(&unpacked, StoreOptions::new()).unwrap();
        let lock = restored.graph().read();
        assert_v3_graph(&lock);
        assert_holds_v3_values(&lock, "closed");
    }

    // A packed export of the latest snapshot, and a new store from it.
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    assert_eq!(version_word(&packed), 3);
    let from_packed = MemDir::new();
    {
        let created =
            Store::create_from_packed(&from_packed, packed.as_slice(), StoreOptions::new())
                .unwrap();
        assert_v3_graph(&created.graph().read());
    }

    // A fork of the latest state, and a repair (a fresh copy of a healthy store).
    let forked = MemDir::new();
    store.fork(RecoveryTarget::Latest, &forked).unwrap();
    {
        let opened = Store::open(&forked, StoreOptions::new()).unwrap();
        assert_holds_v3_values(&opened.graph().read(), "closed");
    }
    let repaired = MemDir::new();
    store.repair_to(&repaired).unwrap();
    {
        let opened = Store::open(&repaired, StoreOptions::new()).unwrap();
        let lock = opened.graph().read();
        assert_v3_graph(&lock);
        assert_holds_v3_values(&lock, "closed");
    }

    // A rollback to the commit after `w`: reloaded from the files; the rolled-back history
    // goes into the attic, from which a fork rebuilds it.
    store
        .rollback_to(RecoveryTarget::CommitSeq(after_w), RollbackMode::Move)
        .unwrap();
    {
        let lock = store.graph().read();
        assert_v3_graph(&lock);
        assert_holds_v3_values(&lock, "w");
        assert!(lock.get_vertex_by_id("later").unwrap().is_none());
    }
    let entries = store.attic_list().unwrap();
    assert_eq!(entries.len(), 1);
    let attic_fork = MemDir::new();
    store.attic_fork(&entries[0].id, &attic_fork).unwrap();
    {
        let opened = Store::open(&attic_fork, StoreOptions::new()).unwrap();
        let lock = opened.graph().read();
        assert_holds_v3_values(&lock, "later");
        assert_holds_v3_values(&lock, "closed");
    }
    store.close().unwrap();
    assert!(Store::verify_dir(&dir).unwrap().is_ok());
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_holds_v3_values(&store.graph().read(), "w");
}

#[test]
fn a_single_file_store_keeps_every_temporal_value() {
    let temp = std::env::temp_dir().join(format!(
        "graphersal-temporal-v3-{}.gstore",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&temp);
    let store = Store::create_with(&temp, v3_graph(), StoreOptions::new()).unwrap();
    add_vertex(&store, "w", v3_map());
    store.checkpoint(None).unwrap();
    add_vertex(&store, "later", v3_map());
    store.close().unwrap();
    {
        let store = Store::open(&temp, StoreOptions::new()).unwrap();
        let lock = store.graph().read();
        assert_v3_graph(&lock);
        assert_holds_v3_values(&lock, "w");
        assert_holds_v3_values(&lock, "later");
    }
    let _ = std::fs::remove_file(&temp);
}

// ---------------------------------------------------------------------------------------------
// The golden fixtures of Graphersal 0.2.3 (file format version 2)
// ---------------------------------------------------------------------------------------------

/// The values `make.sh` wrote to vertex `snap` and `wal` (file format version 2 tags), parsed
/// by this build from the same text.
fn fixture_values() -> PropertyMap {
    let mut map = PropertyMap::default();
    map.insert("name", ElementProperty::from("x"));
    for (key, value) in [
        ("d_min", date("-9999-01-01")),
        ("d_max", date("9999-12-31")),
        ("ldt_min", local("-9999-01-01T00:00:00")),
        ("ldt_max", local("9999-12-31T23:59:59.999999")),
        ("dt_min", dt("-9999-01-01T00:00:00-18:00")),
        ("dt_max", dt("9999-12-31T23:59:59.999999+18:00")),
        ("dt", dt("2024-03-31T02:30:00.123456+02:00")),
        ("t_min", time("00:00:00")),
        ("t_max", time("23:59:59.999999")),
        (
            "dur",
            t(Temporal::Duration("P1Y2M3DT4H5M6.789012S".parse().unwrap())),
        ),
        ("dur_neg", duration(0, 0, -1000)),
        ("dur_max", duration(0, 0, i64::MAX as i128 * 1000)),
        ("dur_min", duration(0, 0, i64::MIN as i128 * 1000)),
        ("dur_cal", duration(i32::MAX, i32::MIN, 0)),
        (
            "nested",
            ElementProperty::Array(vec![
                date("2024-01-01"),
                ElementProperty::Object(Box::new({
                    let mut map = PropertyMap::default();
                    map.insert("at", time("12:00:00.5"));
                    map.insert("when", dt("2024-01-01T00:00:00Z"));
                    map
                })),
            ]),
        ),
    ] {
        map.insert(key, value);
    }
    map
}

fn thing(graph: &TraversalGraph, name: &str) -> PropertyMap {
    let v = graph
        .vertices()
        .unwrap()
        .find(|&v| {
            graph
                .vertex_properties(v)
                .unwrap()
                .and_then(|p| p.get("name"))
                == Some(&ElementProperty::from(name))
        })
        .unwrap_or_else(|| panic!("vertex {name}"));
    graph.vertex_properties(v).unwrap().cloned().unwrap()
}

/// The fixture graph as `make.sh` left it (commit 4).
#[track_caller]
fn assert_fixture_graph(graph: &TraversalGraph) {
    let mut expected = fixture_values();
    expected.insert("name", ElementProperty::from("snap"));
    expected.insert("dt", dt("2000-02-29T23:59:59.000001-05:30"));
    assert_eq!(exact_map(&thing(graph, "snap")), exact_map(&expected));
    expected = fixture_values();
    expected.insert("name", ElementProperty::from("wal"));
    assert_eq!(exact_map(&thing(graph, "wal")), exact_map(&expected));
    assert_eq!(graph.edge_count(), 1);
    let e = graph.edges().unwrap().next().unwrap();
    assert_eq!(
        exact(
            graph
                .edge_properties(e)
                .unwrap()
                .unwrap()
                .get("when")
                .unwrap()
        ),
        exact(&local("1969-12-31T23:59:59.999999"))
    );
}

#[test]
fn the_packed_snapshot_of_0_2_3_loads_exactly_and_a_store_from_it_continues_in_version_3() {
    let bytes = std::fs::read(fixtures().join("snapshot.gsnap")).unwrap();
    assert_eq!(version_word(&bytes), 2);
    let graph = persist::read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
    assert_fixture_graph(&graph);

    let dir = MemDir::new();
    let store = Store::create_from_packed(&dir, bytes.as_slice(), StoreOptions::new()).unwrap();
    assert_fixture_graph(&store.graph().read());
    add_vertex(&store, "v3", v3_map());
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    {
        let lock = store.graph().read();
        assert_fixture_graph(&lock);
        assert_holds_v3_values(&lock, "v3");
    }
    assert!(store.verify().unwrap().problems.is_empty());
    // The imported files keep version 2; everything this build wrote is version 3.
    let versions = file_versions(&dir);
    assert!(versions.iter().any(|(_, v)| *v == 2), "{versions:?}");
    assert!(versions.iter().any(|(_, v)| *v == 3), "{versions:?}");
}

#[test]
fn the_store_of_0_2_3_reads_exactly_and_continues_in_version_3() {
    let dir = fixture_mem(&fixtures().join("store"));
    let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
    let before = file_versions(&dir);
    assert!(before.iter().all(|(_, v)| *v == 2), "{before:?}");
    let old_wal = dir.read("wal/00000000000000000001.wal", u64::MAX).unwrap();
    assert!(Store::verify_dir(&shared).unwrap().is_ok());

    let store = Store::open(&shared, StoreOptions::new()).unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), 4);
    assert_fixture_graph(&store.graph().read());
    // Continued: version 3 values go into a NEW segment of version 3, never into the version 2
    // segment (format spec 15.1, 17).
    add_vertex(&store, "v3", v3_map());
    store
        .graph()
        .write()
        .traversal_mut()
        .v(None)
        .has("name", "snap")
        .property("dt", dt("2024-01-01T10:00:00.000000001[Europe/Berlin]"))
        .iterate()
        .unwrap();
    store.close().unwrap();
    assert_eq!(
        dir.read("wal/00000000000000000001.wal", u64::MAX).unwrap(),
        old_wal,
        "the version 2 segment is never appended to"
    );
    let new_wal = dir.read("wal/00000000000000000005.wal", u64::MAX).unwrap();
    assert_eq!(version_word(&new_wal), 3);

    // The mixed store (version 2 snapshot and segment, version 3 segment) reads back.
    assert!(Store::verify_dir(&shared).unwrap().is_ok());
    let store = Store::open(&shared, StoreOptions::new()).unwrap();
    let check = |graph: &TraversalGraph| {
        assert_holds_v3_values(graph, "v3");
        assert_eq!(
            exact(thing(graph, "snap").get("dt").unwrap()),
            exact(&dt("2024-01-01T10:00:00.000000001[Europe/Berlin]"))
        );
        assert_eq!(
            exact(thing(graph, "wal").get("ldt_max").unwrap()),
            exact(&local("9999-12-31T23:59:59.999999"))
        );
    };
    check(&store.graph().read());
    // The state at the old mark is still there, read from version 2 files only.
    let past = store
        .read_only_at(RecoveryTarget::Mark("last".into()))
        .unwrap();
    assert_fixture_graph(&past.graph);
    // A checkpoint merges across both file versions into a version 3 snapshot.
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&shared, StoreOptions::new()).unwrap();
    check(&store.graph().read());
    store.close().unwrap();
    assert!(Store::verify_dir(&shared).unwrap().is_ok());
    let after = file_versions(&dir);
    let newest = after
        .iter()
        .filter(|(name, _)| name.starts_with("snapshots/"))
        .max()
        .unwrap();
    assert_eq!(newest.1, 3, "{after:?}");
    // From now on the store holds version 3 files, which Graphersal 0.2.x refuses as written by
    // a newer version (its `refuse_newer_version`), never as damage (format spec 15).
    assert!(after.iter().any(|(_, v)| *v == 3));
}

#[cfg(feature = "persist-zip")]
#[test]
fn the_zip_backup_of_0_2_3_restores_and_continues_in_version_3() {
    let bytes = std::fs::read(fixtures().join("backup.zip")).unwrap();
    let dir = MemDir::new();
    Store::restore_zip(bytes.as_slice(), &dir).unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_fixture_graph(&store.graph().read());
    add_vertex(&store, "v3", v3_map());
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_holds_v3_values(&store.graph().read(), "v3");
    assert_fixture_graph(&store.graph().read());
}

#[test]
fn the_microsecond_tags_stay_valid_in_a_version_3_file() {
    // The fixture's version 2 WAL segment (tags 9 to 13) with its header set to version 3: the
    // records decode to the same values.
    let segment = std::fs::read(fixtures().join("store/wal/00000000000000000001.wal")).unwrap();
    assert_eq!(version_word(&segment), 2);
    let as_v2 = read_all(&segment).unwrap();
    let as_v3 = read_all(&with_header_version(segment, 3)).unwrap();
    assert_eq!(as_v2.len(), 5, "commits 1 to 4 and the checkpoint record");
    assert_eq!(format!("{as_v2:?}"), format!("{as_v3:?}"));
}

// ---------------------------------------------------------------------------------------------
// Damage and newer content
// ---------------------------------------------------------------------------------------------

#[test]
fn a_nanosecond_tag_in_a_version_1_or_2_file_is_damage() {
    for (name, value) in v3_values() {
        if name.starts_with("date_") {
            // A date keeps tag 9, valid since version 2.
            continue;
        }
        let bytes = journal_with(value);
        for version in [1, 2] {
            match read_all(&with_header_version(bytes.clone(), version)) {
                Err(PersistError::Corrupt { reason, .. }) => {
                    assert!(
                        reason.contains(&format!("format version {version}")),
                        "{name}: {reason}"
                    );
                }
                other => panic!("{name}: expected damage, got {other:?}"),
            }
        }
    }
    // In a store: a version 2 WAL segment holding a version 3 value is damage at open.
    let dir = MemDir::new();
    let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
    add_vertex(&store, "x", v3_map());
    store.close().unwrap();
    let wal = dir
        .file_names()
        .into_iter()
        .rfind(|name| name.starts_with("wal/"))
        .unwrap();
    let bytes = dir.read(&wal, u64::MAX).unwrap();
    dir.write_atomic(&wal, &with_header_version(bytes, 2))
        .unwrap();
    match Store::open(&dir, StoreOptions::new()) {
        Ok(store) => {
            assert!(store.maintenance().is_some(), "opened without damage");
            assert!(
                store
                    .graph()
                    .read()
                    .get_vertex_by_id("x")
                    .unwrap()
                    .is_none()
            );
        }
        Err(err) => assert!(matches!(err, PersistError::Corrupt { .. }), "{err}"),
    }
}

#[test]
fn a_zone_name_this_build_does_not_know_is_unsupported_content_never_damage() {
    // A journal record: a newer time zone database's name behind valid checksums.
    let bytes = journal_with(dt("2024-01-01T10:00[Europe/Berlin]"));
    let payload = bytes[WAL_HEADER + FRAME..].to_vec();
    let at = payload
        .windows(13)
        .position(|w| w == b"Europe/Berlin")
        .expect("the zone name inline");
    let mut newer = payload.clone();
    newer[at..at + 13].copy_from_slice(b"Europe/Xerlin");
    match read_all(&with_payload(&bytes, &newer)) {
        Err(err @ PersistError::UnsupportedContent { .. }) => {
            assert!(err.to_string().contains("Europe/Xerlin"), "{err}");
        }
        other => panic!("expected unsupported content, got {other:?}"),
    }
    // A case variant of a known name is that name.
    let mut lower = payload;
    lower[at..at + 13].copy_from_slice(b"europe/berlin");
    assert_eq!(
        exact(&journal_value(&with_payload(&bytes, &lower)).unwrap()),
        exact(&dt("2024-01-01T10:00[Europe/Berlin]"))
    );

    // In a store's WAL: the open refuses it as unsupported content, no maintenance mode; the
    // operations that would rewrite the state refuse it the same way; nothing is changed.
    let dir = MemDir::new();
    let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
    let store = Store::create_with(&shared, TraversalGraph::new(), StoreOptions::new()).unwrap();
    add_vertex(
        &store,
        "x",
        PropertyMap::from([("at", dt("2024-01-01T10:00[Europe/Berlin]"))]),
    );
    store.close().unwrap();
    patch_wal(&dir, b"Europe/Berlin", b"Europe/Xerlin");
    let files = dir.file_names();
    let graph_file = dir.read("GRAPH", u64::MAX).unwrap();
    match Store::open(&shared, StoreOptions::new()) {
        Err(PersistError::UnsupportedContent { .. }) => {}
        Err(other) => panic!("expected unsupported content, got {other}"),
        Ok(store) => panic!("opened (maintenance: {:?})", store.maintenance().is_some()),
    }
    for result in [
        Store::checkpoint_dir(&shared, StoreOptions::new(), None).map(|_| ()),
        Store::repair_dir(&shared, MemDir::new()).map(|_| ()),
    ] {
        match result {
            Err(err @ PersistError::UnsupportedContent { .. }) => {
                assert!(err.to_string().contains("Europe/Xerlin"), "{err}")
            }
            other => panic!("expected unsupported content, got {other:?}"),
        }
    }
    // Verify and a backup note it and go on (format spec 16 rule 9).
    let report = Store::verify_dir(&shared).unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
    assert!(
        report.notes.iter().any(|n| n.contains("Europe/Xerlin")),
        "{report:?}"
    );
    Store::backup_dir(&shared, MemDir::new()).unwrap();
    assert_eq!(dir.file_names(), files);
    assert_eq!(dir.read("GRAPH", u64::MAX).unwrap(), graph_file);
}

#[test]
fn damaged_nanosecond_payloads_are_errors_never_panics() {
    for (name, value) in v3_values() {
        let bytes = journal_with(value.clone());
        let payload = bytes[WAL_HEADER + FRAME..].to_vec();
        assert_eq!(
            exact(&journal_value(&with_payload(&bytes, &payload)).unwrap()),
            exact(&value),
            "{name}"
        );
        // Every cut and every flipped bit: an error or another valid value, never a panic.
        for cut in 0..payload.len() {
            let _ = read_all(&with_payload(&bytes, &payload[..cut]));
        }
        for index in 0..payload.len() {
            for bit in 0..8 {
                let mut flipped = payload.clone();
                flipped[index] ^= 1 << bit;
                let _ = read_all(&with_payload(&bytes, &flipped));
            }
        }
    }
}

#[test]
fn out_of_range_nanosecond_payloads_are_damage() {
    fn uvar(out: &mut Vec<u8>, mut raw: u64) {
        while raw >= 0x80 {
            out.push((raw as u8) | 0x80);
            raw >>= 7;
        }
        out.push(raw as u8);
    }
    fn svar(out: &mut Vec<u8>, value: i64) {
        uvar(out, ((value << 1) ^ (value >> 63)) as u64);
    }
    // The journal of a time (tag 18, `uvar` nanoseconds) whose value is replaced by each case.
    let good_value = 43_200_000_000_001u64;
    let bytes = journal_with(time("12:00:00.000000001"));
    let payload = bytes[WAL_HEADER + FRAME..].to_vec();
    let mut good = vec![18u8];
    uvar(&mut good, good_value);
    let at = payload
        .windows(good.len())
        .rposition(|w| w == good.as_slice())
        .expect("the encoded time");
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // localdatetime: nanoseconds of 10^9; a second beyond the range.
    let mut case = vec![15u8];
    svar(&mut case, 0);
    uvar(&mut case, 1_000_000_000);
    cases.push(("ldt nanos", case));
    let mut case = vec![15u8];
    svar(&mut case, 253_402_300_800); // 10000-01-01T00:00:00
    uvar(&mut case, 0);
    cases.push(("ldt beyond", case));
    // datetime: offset beyond ±18:00; a local time beyond the range.
    let mut case = vec![16u8];
    svar(&mut case, 0);
    uvar(&mut case, 0);
    svar(&mut case, 18 * 3600 + 1);
    cases.push(("dt offset", case));
    let mut case = vec![16u8];
    svar(&mut case, 253_402_300_799);
    uvar(&mut case, 0);
    svar(&mut case, 3600);
    cases.push(("dt local beyond", case));
    let mut case = vec![17u8];
    svar(&mut case, 0);
    uvar(&mut case, 1_000_000_000);
    svar(&mut case, 0);
    case.extend_from_slice(&[3, b'U', b'T', b'C']);
    cases.push(("zoned nanos", case));
    // time: a day or more.
    let mut case = vec![18u8];
    uvar(&mut case, 86_400_000_000_000);
    cases.push(("time", case));
    let mut case = vec![18u8];
    uvar(&mut case, u64::MAX);
    cases.push(("time u64", case));
    // offsettime: the time, the offset.
    let mut case = vec![19u8];
    uvar(&mut case, 86_400_000_000_000);
    svar(&mut case, 0);
    cases.push(("ot time", case));
    let mut case = vec![19u8];
    uvar(&mut case, 0);
    svar(&mut case, -(18 * 3600 + 1));
    cases.push(("ot offset", case));
    // duration: months beyond i32, nanoseconds of 10^9.
    let mut case = vec![20u8];
    svar(&mut case, i64::from(i32::MAX) + 1);
    svar(&mut case, 0);
    svar(&mut case, 0);
    uvar(&mut case, 0);
    cases.push(("dur months", case));
    let mut case = vec![20u8];
    svar(&mut case, 0);
    svar(&mut case, i64::from(i32::MIN) - 1);
    svar(&mut case, 0);
    uvar(&mut case, 0);
    cases.push(("dur days", case));
    let mut case = vec![20u8];
    svar(&mut case, 0);
    svar(&mut case, 0);
    svar(&mut case, i64::MAX);
    uvar(&mut case, 1_000_000_000);
    cases.push(("dur nanos", case));
    // An unknown tag after the last one.
    cases.push(("tag 21", vec![21u8]));
    for (name, case) in cases {
        let mut damaged = payload[..at].to_vec();
        damaged.extend_from_slice(&case);
        damaged.extend_from_slice(&payload[at + good.len()..]);
        match read_all(&with_payload(&bytes, &damaged)) {
            Err(PersistError::Corrupt { reason, .. }) => {
                assert!(
                    reason.contains("invalid") || reason.contains("unknown value tag"),
                    "{name}: {reason}"
                );
            }
            other => panic!("{name}: expected damage, got {other:?}"),
        }
    }
    // The extremes themselves are valid: the largest duration time part.
    let mut edge = vec![20u8];
    svar(&mut edge, 0);
    svar(&mut edge, 0);
    svar(&mut edge, i64::MAX);
    uvar(&mut edge, 999_999_999);
    let mut valid = payload[..at].to_vec();
    valid.extend_from_slice(&edge);
    valid.extend_from_slice(&payload[at + good.len()..]);
    assert_eq!(
        exact(&journal_value(&with_payload(&bytes, &valid)).unwrap()),
        exact(&duration(0, 0, Duration::MAX_NANOS))
    );
}

// ---------------------------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
#[test]
fn now_on_a_nanosecond_clock_is_stored_exactly() {
    let dir = MemDir::new();
    let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
    let pinned = DateTime::from_nanos(1_700_000_000_123_456_789, 3600).unwrap();
    let mut engine = crate::script_host::engine_with_options(&Default::default(), &[]);
    graphersal::script::pin_now(&mut engine, Some(pinned));
    let mut scope = crate::script_host::graph_scope_with_options(store.graph().clone(), &[]);
    let _ = engine
        .eval_with_scope::<graphersal::script::rhai::Dynamic>(
            &mut scope,
            r#"g.add_v("e").property("name", "now").property("at", now()).iterate()"#,
        )
        .unwrap();
    // The system clock too (whatever digits the platform gives).
    graphersal::script::pin_now(&mut engine, None);
    let mut scope = crate::script_host::graph_scope_with_options(store.graph().clone(), &[]);
    let _ = engine
        .eval_with_scope::<graphersal::script::rhai::Dynamic>(
            &mut scope,
            r#"g.add_v("e").property("name", "system").property("at", now()).iterate()"#,
        )
        .unwrap();
    let system = thing(&store.graph().read(), "system")
        .get("at")
        .cloned()
        .unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let lock = store.graph().read();
    assert_eq!(
        exact(thing(&lock, "now").get("at").unwrap()),
        exact(&t(Temporal::DateTime(pinned)))
    );
    match thing(&lock, "now").get("at") {
        Some(ElementProperty::Temporal(Temporal::DateTime(at))) => {
            assert_eq!(at.epoch_nanos(), 1_700_000_000_123_456_789);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        exact(thing(&lock, "system").get("at").unwrap()),
        exact(&system)
    );
}
