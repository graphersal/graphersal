//! Temporal values in every format and in the schema (issue #3, phase A2): the binary codec
//! (packed snapshot, Store WAL replay, checkpoint, backup, ZIP backup, rollback; file format
//! version 2, a version 1 file still loads, a temporal tag in a version 1 file is damage,
//! damaged temporal payloads never panic, a store of store format version 2 is refused), the
//! serialized `ChangeSet` tags, GraphSON 3.0 import and export of the `java.time` types,
//! GraphML with a declared schema, and the schema's temporal formats and format bounds.

#![cfg_attr(
    not(all(feature = "persist", feature = "io", feature = "serde")),
    allow(dead_code)
)]

use graphersal::prelude::*;
use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, Time};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn dt(text: &str) -> ElementProperty {
    ElementProperty::from(text.parse::<DateTime>().unwrap())
}

fn date(text: &str) -> ElementProperty {
    ElementProperty::from(text.parse::<Date>().unwrap())
}

fn local(text: &str) -> ElementProperty {
    ElementProperty::from(text.parse::<LocalDateTime>().unwrap())
}

fn time(text: &str) -> ElementProperty {
    ElementProperty::from(text.parse::<Time>().unwrap())
}

fn duration(text: &str) -> ElementProperty {
    ElementProperty::from(text.parse::<Duration>().unwrap())
}

/// Every field of a value, recursively: a `datetime` with its offset (equality compares only
/// the instant), a `duration` field by field, containers item by item.
fn exact(value: &ElementProperty) -> String {
    match value {
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

/// The values at the edges of every temporal type's range, and a few in between.
fn edge_values() -> Vec<(&'static str, ElementProperty)> {
    vec![
        ("date_min", date("-9999-01-01")),
        ("date_max", date("9999-12-31")),
        ("date_epoch", date("1970-01-01")),
        ("date_leap_zero", date("0000-02-29")),
        ("local_min", local("-9999-01-01T00:00:00")),
        ("local_max", local("9999-12-31T23:59:59.999999")),
        ("dt_min", dt("-9999-01-01T00:00:00-18:00")),
        ("dt_max", dt("9999-12-31T23:59:59.999999+18:00")),
        ("dt_utc", dt("2023-08-02T12:00:00Z")),
        ("dt_offset", dt("2023-08-02T14:00:00.25+02:00")),
        ("dt_offset_seconds", dt("1890-01-01T00:00:00+00:19:32")),
        ("time_min", time("00:00:00")),
        ("time_max", time("23:59:59.999999")),
        (
            "dur_min",
            ElementProperty::from(Duration::new(i32::MIN, i32::MIN, i64::MIN)),
        ),
        (
            "dur_max",
            ElementProperty::from(Duration::new(i32::MAX, i32::MAX, i64::MAX)),
        ),
        ("dur_mixed", ElementProperty::from(Duration::new(1, -2, 3))),
        ("dur_zero", duration("PT0S")),
        (
            "nested",
            ElementProperty::Array(vec![
                dt("2023-08-02T14:00:00+02:00"),
                ElementProperty::Object(Box::new({
                    let mut map = PropertyMap::default();
                    map.insert("d", date("2024-02-29"));
                    map.insert("p", duration("P1Y2M3D"));
                    map
                })),
            ]),
        ),
    ]
}

fn edge_map() -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in edge_values() {
        map.insert(key, value);
    }
    map
}

/// A graph with one vertex `t` holding [`edge_values`] and an edge `e` holding a datetime.
fn temporal_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    graph
        .transaction(|g| {
            let t = g.add_vertex(Some("t".into()), vec!["temporal".into()], edge_map())?;
            let u = g.add_vertex(
                Some("u".into()),
                vec!["temporal".into()],
                PropertyMap::default(),
            )?;
            let mut edge = PropertyMap::default();
            edge.insert("since", dt("2001-02-03T04:05:06.007-03:30"));
            g.add_edge(Some("e".into()), Some("knows".into()), t, u, edge)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    graph
}

fn vertex_map(graph: &TraversalGraph, id: &str) -> PropertyMap {
    let v = graph.get_vertex_by_id(id).expect("vertex");
    graph.vertex_properties(v).cloned().unwrap_or_default()
}

fn edge_since(graph: &TraversalGraph) -> String {
    let e = graph.get_edge_by_id("e").expect("edge");
    exact(graph.edge_properties(e).unwrap().get("since").unwrap())
}

fn assert_holds_edge_values(graph: &TraversalGraph) {
    assert_eq!(exact_map(&vertex_map(graph, "t")), exact_map(&edge_map()));
    assert_eq!(edge_since(graph), "datetime(2001-02-03T04:05:06.007-03:30)");
}

/// CRC-32C (Castagnoli), bitwise.
#[cfg(feature = "persist")]
fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn the_range_edges_are_what_the_format_specification_states() {
    // Format spec 3: the normative day numbers of the date range.
    let first = "-9999-01-01".parse::<Date>().unwrap();
    let last = "9999-12-31".parse::<Date>().unwrap();
    assert_eq!(first.days_since_epoch(), -4_371_587);
    assert_eq!(last.days_since_epoch(), 2_932_896);
}

// ---------------------------------------------------------------------------------------------
// Binary format
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
mod binary {
    use super::*;
    use graphersal::persist::{
        self, Durability, Journal, JournalRecord, MemDir, PersistError, ReadOptions,
        RecoveryTarget, RollbackMode, SharedBuffer, Store, StoreDir, StoreOptions, WriteOptions,
    };

    fn packed(graph: &TraversalGraph) -> Vec<u8> {
        let mut bytes = Vec::new();
        persist::write_snapshot(graph, &mut bytes, &WriteOptions::new()).unwrap();
        bytes
    }

    #[test]
    fn a_packed_snapshot_keeps_every_temporal_value_exactly() {
        let graph = temporal_graph();
        let bytes = packed(&graph);
        // Every file this build writes is of file format version 3.
        assert_eq!(&bytes[..8], b"GRSLPACK");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);
        assert_eq!(persist::FORMAT_VERSION, 3);
        let loaded = persist::read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
        assert_holds_edge_values(&loaded);
    }

    #[test]
    fn a_packed_snapshot_of_graphersal_0_1_still_loads() {
        // Written by the 0.1.0 binary (file format version 1): the modern graph plus a vertex
        // with a uuid and a nested object.
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/graphersal-0.1-modern.gsnap"
        ))
        .unwrap();
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1);
        let graph = persist::read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
        assert_eq!(graph.vertex_count(), 7);
        assert_eq!(graph.edge_count(), 6);
        let thing = graph
            .vertices()
            .find(|&v| {
                graph.vertex_properties(v).and_then(|p| p.get("name"))
                    == Some(&ElementProperty::from("thing"))
            })
            .expect("the thing vertex");
        let props = graph.vertex_properties(thing).unwrap();
        assert_eq!(
            props.get("u"),
            Some(&ElementProperty::Uuid(
                0x550e8400_e29b_41d4_a716_446655440000
            ))
        );
        // A store created from it keeps working and takes temporal values (written in new
        // version 2 files next to the version 1 ones).
        let dir = MemDir::new();
        let store = Store::create_from_packed(&dir, bytes.as_slice(), StoreOptions::new())
            .expect("a store from a 0.1 packed snapshot");
        store
            .graph()
            .write()
            .transaction(|g| {
                let v = g.add_vertex(Some("t".into()), vec!["temporal".into()], edge_map())?;
                let _ = v;
                Ok::<_, GraphError>(())
            })
            .unwrap();
        store.checkpoint(None).unwrap();
        assert!(store.verify().unwrap().problems.is_empty());
        store.close().unwrap();
        let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
        let lock = reopened.graph().read();
        assert_eq!(exact_map(&vertex_map(&lock, "t")), exact_map(&edge_map()));
        assert_eq!(lock.vertex_count(), 8);
    }

    /// A journal stream with one commit that writes `value` (an uncompressed record).
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

    const WAL_HEADER: usize = 64;
    const FRAME: usize = 25;

    /// Rewrites the journal header's version word (and its checksum).
    fn with_header_version(mut bytes: Vec<u8>, version: u32) -> Vec<u8> {
        bytes[8..12].copy_from_slice(&version.to_le_bytes());
        let crc = crc32c(&bytes[..60]);
        bytes[60..64].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    #[test]
    fn a_temporal_tag_in_a_version_1_journal_is_damage() {
        let bytes = journal_with(dt("2023-08-02T14:00:00+02:00"));
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);
        let records = read_all(&bytes).unwrap();
        assert_eq!(records.len(), 1);
        let v1 = with_header_version(bytes.clone(), 1);
        match read_all(&v1) {
            Err(PersistError::Corrupt { reason, .. }) => {
                assert!(reason.contains("format version 1"), "{reason}");
            }
            other => panic!("expected damage, got {other:?}"),
        }
        // The same journal without a temporal value is a valid version 1 journal.
        let plain = with_header_version(journal_with("text".into()), 1);
        assert_eq!(read_all(&plain).unwrap().len(), 1);
        // A newer version is refused, never guessed at.
        assert!(read_all(&with_header_version(bytes, 4)).is_err());
    }

    /// The record of [`journal_with`] with its payload replaced (frame length and checksums
    /// fixed, so the decoder itself meets the bytes).
    fn with_payload(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = bytes[..WAL_HEADER + FRAME].to_vec();
        out[WAL_HEADER + 4..WAL_HEADER + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let header_crc = crc32c(&out[WAL_HEADER..WAL_HEADER + 17]);
        out[WAL_HEADER + 17..WAL_HEADER + 21].copy_from_slice(&header_crc.to_le_bytes());
        out[WAL_HEADER + 21..WAL_HEADER + 25].copy_from_slice(&crc32c(payload).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn damaged_temporal_payloads_are_errors_never_panics() {
        for (name, value) in edge_values() {
            let bytes = journal_with(value.clone());
            let payload = bytes[WAL_HEADER + FRAME..].to_vec();
            // The unchanged record decodes to the value.
            match &read_all(&with_payload(&bytes, &payload)).unwrap()[0] {
                JournalRecord::Commit(changes) => {
                    let text = format!("{:?}", changes.mutations());
                    assert!(text.contains("\"x\""), "{name}: {text}");
                }
                other => panic!("{name}: {other:?}"),
            }
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
    fn out_of_range_temporal_payloads_are_damage() {
        // The value tags and payloads of format spec 3, out of range (zigzag + LEB128).
        fn svar(out: &mut Vec<u8>, value: i64) {
            let mut raw = ((value << 1) ^ (value >> 63)) as u64;
            while raw >= 0x80 {
                out.push((raw as u8) | 0x80);
                raw >>= 7;
            }
            out.push(raw as u8);
        }
        let bytes = journal_with(date("2000-01-01"));
        let payload = bytes[WAL_HEADER + FRAME..].to_vec();
        let mut good = vec![9u8];
        svar(&mut good, 10_957);
        let at = payload
            .windows(good.len())
            .rposition(|w| w == good.as_slice())
            .expect("the encoded date");
        let mut cases: Vec<Vec<u8>> = Vec::new();
        let mut date_beyond = vec![9u8];
        svar(&mut date_beyond, 2_932_897);
        cases.push(date_beyond);
        let mut local_beyond = vec![10u8];
        svar(&mut local_beyond, i64::MAX);
        cases.push(local_beyond);
        let mut offset_beyond = vec![11u8];
        svar(&mut offset_beyond, 0);
        svar(&mut offset_beyond, 18 * 3600 + 1);
        cases.push(offset_beyond);
        let mut time_beyond = vec![12u8];
        // `uvar` 86_400_000_000: a zigzag-encoded value is twice as large.
        svar(&mut time_beyond, 43_200_000_000);
        cases.push(time_beyond);
        let mut months_beyond = vec![13u8];
        svar(&mut months_beyond, i64::from(i32::MAX) + 1);
        svar(&mut months_beyond, 0);
        svar(&mut months_beyond, 0);
        cases.push(months_beyond);
        for case in cases {
            let mut damaged = payload[..at].to_vec();
            damaged.extend_from_slice(&case);
            damaged.extend_from_slice(&payload[at + good.len()..]);
            match read_all(&with_payload(&bytes, &damaged)) {
                Err(PersistError::Corrupt { reason, .. }) => {
                    assert!(reason.contains("invalid"), "{case:?}: {reason}");
                }
                other => panic!("{case:?}: expected damage, got {other:?}"),
            }
        }
    }

    fn add_temporal_vertex(store: &Store, id: &str, props: PropertyMap) {
        store
            .graph()
            .write()
            .transaction(|g| {
                g.add_vertex(Some(id.into()), vec!["temporal".into()], props)?;
                Ok::<_, GraphError>(())
            })
            .unwrap();
    }

    fn one(key: &str, value: ElementProperty) -> PropertyMap {
        let mut map = PropertyMap::default();
        map.insert(key, value);
        map
    }

    #[test]
    fn a_store_keeps_temporal_values_through_wal_replay_checkpoint_backup_and_rollback() {
        let dir = MemDir::new();
        let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
        add_temporal_vertex(&store, "t", edge_map());
        let after_t = store.graph().read().last_commit_seq();
        add_temporal_vertex(&store, "later", one("at", dt("2024-01-01T00:00:00+05:45")));
        store.close().unwrap();

        // WAL replay.
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(
            exact_map(&vertex_map(&store.graph().read(), "t")),
            exact_map(&edge_map())
        );
        // Checkpoint (a merge of the WAL into a new snapshot), then a load from it.
        store.checkpoint(None).unwrap();
        add_temporal_vertex(&store, "after-checkpoint", one("span", duration("P1MT2H")));
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        {
            let lock = store.graph().read();
            assert_eq!(exact_map(&vertex_map(&lock, "t")), exact_map(&edge_map()));
            assert_eq!(
                exact_map(&vertex_map(&lock, "after-checkpoint")),
                ["span = duration(P1MT2H)"]
            );
        }
        assert!(store.verify().unwrap().problems.is_empty());

        // A verified backup, opened read-only.
        let backup = MemDir::new();
        store.backup(&backup).unwrap();
        {
            let opened = Store::open_backup(&backup, StoreOptions::new()).unwrap();
            let lock = opened.graph().read();
            assert_eq!(exact_map(&vertex_map(&lock, "t")), exact_map(&edge_map()));
            assert_eq!(
                exact_map(&vertex_map(&lock, "later")),
                ["at = datetime(2024-01-01T00:00:00+05:45)"]
            );
        }

        // A ZIP backup, unpacked into a live store.
        #[cfg(feature = "persist-zip")]
        {
            let mut zip = std::io::Cursor::new(Vec::new());
            store.backup_zip(&mut zip).unwrap();
            let unpacked = MemDir::new();
            Store::restore_zip(zip.into_inner().as_slice(), &unpacked).unwrap();
            let restored = Store::open(&unpacked, StoreOptions::new()).unwrap();
            let lock = restored.graph().read();
            assert_eq!(exact_map(&vertex_map(&lock, "t")), exact_map(&edge_map()));
        }

        // An in-place rollback to the commit after `t` reloads it from the files.
        store
            .rollback_to(RecoveryTarget::CommitSeq(after_t), RollbackMode::Move)
            .unwrap();
        {
            let lock = store.graph().read();
            assert_eq!(exact_map(&vertex_map(&lock, "t")), exact_map(&edge_map()));
            assert!(lock.get_vertex_by_id("later").is_none());
        }
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(
            exact_map(&vertex_map(&store.graph().read(), "t")),
            exact_map(&edge_map())
        );
    }

    #[test]
    fn a_temporal_value_in_a_version_1_wal_segment_of_a_store_is_damage() {
        let dir = MemDir::new();
        let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
        add_temporal_vertex(&store, "t", one("at", dt("2023-08-02T12:00:00Z")));
        store.close().unwrap();
        let wal = dir
            .file_names()
            .into_iter()
            .rfind(|name| name.starts_with("wal/"))
            .unwrap();
        let bytes = dir.read(&wal, 1 << 20).unwrap();
        dir.write_atomic(&wal, &with_header_version(bytes, 1))
            .unwrap();
        match Store::open(&dir, StoreOptions::new()) {
            Ok(store) => {
                assert!(store.maintenance().is_some(), "opened without damage");
                assert!(store.graph().read().get_vertex_by_id("t").is_none());
            }
            Err(err) => assert!(matches!(err, PersistError::Corrupt { .. }), "{err}"),
        }
    }

    #[test]
    fn a_store_of_store_format_version_2_is_refused_as_older() {
        assert_eq!(persist::STORE_FORMAT_VERSION, 3);
        let dir = MemDir::new();
        let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
        add_temporal_vertex(&store, "t", one("d", date("2024-01-01")));
        store.close().unwrap();
        for name in ["GRAPH", "GRAPH.copy"] {
            let mut bytes = dir.read(name, 1 << 20).unwrap();
            assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);
            bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
            let crc = crc32c(&bytes[..252]);
            bytes[252..256].copy_from_slice(&crc.to_le_bytes());
            dir.write_atomic(name, &bytes).unwrap();
        }
        for result in [
            Store::open(&dir, StoreOptions::new()).map(|_| ()),
            Store::open_read_only(&dir, RecoveryTarget::Latest).map(|_| ()),
            Store::verify_dir(&dir).map(|_| ()),
        ] {
            match result {
                Err(err @ PersistError::OlderFormat { .. }) => {
                    assert!(
                        matches!(
                            err,
                            PersistError::OlderFormat {
                                found: 2,
                                supported: 3,
                                ..
                            }
                        ),
                        "{err}"
                    );
                    let text = graphersal::error::format_diagnostic(&err);
                    assert!(text.contains("graphersal store upgrade"), "{text}");
                }
                other => panic!("expected OlderFormat, got {other:?}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// ChangeSet serde
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "serde")]
mod change_set_serde {
    use super::*;
    use graphersal::changes::{ChangeSet, Mutation};

    fn captured() -> ChangeSet {
        let mut graph = TraversalGraph::new();
        let ((), changes) = graph
            .dry_run(|g| {
                let t = g.add_vertex(Some("t".into()), vec!["temporal".into()], edge_map())?;
                g.set_vertex_property(t, "later".into(), dt("2024-01-01T00:00:00-07:00"))?;
                Ok::<_, GraphError>(())
            })
            .unwrap();
        changes
    }

    fn after_of(set: &ChangeSet) -> Vec<String> {
        set.mutations()
            .iter()
            .flat_map(|m| match m {
                Mutation::AddVertex { properties, .. } => exact_map(properties),
                Mutation::SetProperty { key, after, .. } => {
                    vec![format!("{key} = {}", exact(after))]
                }
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn every_temporal_type_round_trips_with_its_tag() {
        let set = captured();
        let text = serde_json::to_string(&set).unwrap();
        for tag in [
            r#"{"@type":"gx:LocalDate","@value":"-9999-01-01"}"#,
            r#"{"@type":"gx:LocalDateTime","@value":"9999-12-31T23:59:59.999999"}"#,
            r#"{"@type":"gx:OffsetDateTime","@value":"2023-08-02T14:00:00.250+02:00"}"#,
            r#"{"@type":"gx:LocalTime","@value":"23:59:59.999999"}"#,
            r#"{"@type":"gx:Duration","@value":"PT0S"}"#,
            r#"{"@type":"gx:Period","@value":"P1Y2M3D"}"#,
            r#"{"@type":"graphersal:Interval","@value":"P1M-2DT0.000003S"}"#,
        ] {
            assert!(text.contains(tag), "{tag} missing in {text}");
        }
        let back: ChangeSet = serde_json::from_str(&text).unwrap();
        assert_eq!(after_of(&back), after_of(&set));
        let mut expected = exact_map(&edge_map());
        expected.push("later = datetime(2024-01-01T00:00:00-07:00)".to_string());
        assert_eq!(after_of(&back), expected);
    }

    #[test]
    fn a_fixed_length_duration_is_a_java_duration_and_a_calendar_one_a_period() {
        let mut graph = TraversalGraph::new();
        let ((), set) = graph
            .dry_run(|g| {
                let mut props = PropertyMap::default();
                props.insert("hours", duration("PT26H"));
                props.insert("negative", duration("-PT1.5S"));
                props.insert("months", duration("P14M"));
                props.insert("days", duration("-P3D"));
                g.add_vertex(None, vec![], props)?;
                Ok::<_, GraphError>(())
            })
            .unwrap();
        let text = serde_json::to_string(&set).unwrap();
        for tag in [
            r#"{"@type":"gx:Duration","@value":"PT26H"}"#,
            r#"{"@type":"gx:Duration","@value":"-PT1.500S"}"#,
            r#"{"@type":"gx:Period","@value":"P1Y2M"}"#,
            r#"{"@type":"gx:Period","@value":"-P3D"}"#,
        ] {
            assert!(text.contains(tag), "{tag} missing in {text}");
        }
    }

    #[test]
    fn a_temporal_tag_is_read_strictly() {
        let read = |value: &str| {
            let text = format!(
                r#"{{"commit_seq":1,"mutations":[{{"op":"set_property","element":{{"kind":"vertex","id":"1"}},"key":"k","after":{value}}}]}}"#
            );
            serde_json::from_str::<ChangeSet>(&text).map(|set| match &set.mutations()[0] {
                Mutation::SetProperty { after, .. } => exact(after),
                other => panic!("{other:?}"),
            })
        };
        assert_eq!(
            read(r#"{"@type":"gx:OffsetDateTime","@value":"2023-08-02T12:00:00Z"}"#).unwrap(),
            "datetime(2023-08-02T12:00:00Z)"
        );
        assert_eq!(
            read(r#"{"@type":"graphersal:Interval","@value":"P1DT1H"}"#).unwrap(),
            "duration(P1DT1H)"
        );
        for bad in [
            // Text that is not of the tag's type.
            r#"{"@type":"gx:LocalDate","@value":"2023-13-01"}"#,
            r#"{"@type":"gx:LocalDate","@value":"2023-08-02T00:00:00"}"#,
            r#"{"@type":"gx:LocalTime","@value":"10:00:00+02:00"}"#,
            r#"{"@type":"gx:LocalDateTime","@value":"2023-08-02T10:00:00Z"}"#,
            r#"{"@type":"gx:OffsetDateTime","@value":"10000-01-01T00:00:00Z"}"#,
            // An offset date-time must carry its offset (never read as UTC).
            r#"{"@type":"gx:OffsetDateTime","@value":"2023-08-02T12:00:00"}"#,
            // Parts that do not fit the duration tag.
            r#"{"@type":"gx:Duration","@value":"P1D"}"#,
            r#"{"@type":"gx:Period","@value":"PT1H"}"#,
            r#"{"@type":"graphersal:Interval","@value":"PT1H"}"#,
            r#"{"@type":"graphersal:Interval","@value":"P1D"}"#,
            // A payload that is not text, or extra keys.
            r#"{"@type":"gx:LocalDate","@value":19000}"#,
            r#"{"@type":"gx:LocalDate","@value":"2023-08-02","x":1}"#,
        ] {
            assert!(read(bad).is_err(), "{bad} must be rejected");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// GraphSON
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "io")]
mod graphson {
    use super::*;
    use graphersal::io::{GraphSONImport, ImportNoteKind, ImportReport};

    fn load(text: &str) -> (TraversalGraph, ImportReport) {
        let mut graph = TraversalGraph::new();
        let report = GraphSONImport::new()
            .import_reader(&mut graph, text.as_bytes())
            .expect("import");
        (graph, report)
    }

    /// One vertex property `p` holding `value` (GraphSON 3.0 line form).
    fn one_value(value: &str) -> (Option<String>, ImportReport) {
        let line =
            format!(r#"{{"id":"v","label":"x","properties":{{"p":[{{"value":{value}}}]}}}}"#);
        let (graph, report) = load(&line);
        let props = vertex_map(&graph, "v");
        (props.get("p").map(exact), report)
    }

    #[test]
    fn every_java_time_tag_is_imported() {
        let cases = [
            (
                r#"{"@type":"g:Date","@value":1481750076295}"#,
                "datetime(2016-12-14T21:14:36.295Z)",
            ),
            (
                r#"{"@type":"g:Timestamp","@value":-1000}"#,
                "datetime(1969-12-31T23:59:59Z)",
            ),
            (
                r#"{"@type":"gx:Instant","@value":"2016-12-14T16:39:19.349Z"}"#,
                "datetime(2016-12-14T16:39:19.349Z)",
            ),
            (
                r#"{"@type":"gx:OffsetDateTime","@value":"2007-12-03T10:15:30+01:00"}"#,
                "datetime(2007-12-03T10:15:30+01:00)",
            ),
            // GraphSON 2.0 and other spellings of the same type.
            (
                r#"{"@type":"g:OffsetDateTime","@value":"2007-12-03T10:15+01:00"}"#,
                "datetime(2007-12-03T10:15:00+01:00)",
            ),
            (
                r#"{"@type":"gx:LocalDate","@value":"2016-01-01"}"#,
                "date(2016-01-01)",
            ),
            (
                r#"{"@type":"gx:LocalDateTime","@value":"2016-01-01T12:30"}"#,
                "localdatetime(2016-01-01T12:30:00)",
            ),
            (
                r#"{"@type":"gx:LocalTime","@value":"12:30:45"}"#,
                "time(12:30:45)",
            ),
            (
                r#"{"@type":"gx:Duration","@value":"PT120H"}"#,
                "duration(PT120H)",
            ),
            // `java.time.Duration` reads days as 24 hours.
            (
                r#"{"@type":"gx:Duration","@value":"P2DT3H4M"}"#,
                "duration(PT51H4M)",
            ),
            (
                r#"{"@type":"gx:Duration","@value":"PT-0.5S"}"#,
                "duration(-PT0.500S)",
            ),
            (
                r#"{"@type":"gx:Period","@value":"P1Y2M3D"}"#,
                "duration(P1Y2M3D)",
            ),
            (r#"{"@type":"gx:Period","@value":"P2W"}"#, "duration(P14D)"),
            (r#"{"@type":"gx:Period","@value":"P0D"}"#, "duration(PT0S)"),
            (
                r#"{"@type":"g:List","@value":[{"@type":"gx:LocalDate","@value":"2016-01-01"}]}"#,
                "[date(2016-01-01)]",
            ),
        ];
        for (value, expected) in cases {
            let (got, report) = one_value(value);
            assert_eq!(got.as_deref(), Some(expected), "{value}: {report}");
            assert!(!report.has_skips(), "{value}: {report}");
        }
    }

    #[test]
    fn a_zoned_date_time_keeps_its_offset_and_the_zone_is_reported() {
        let (got, report) = one_value(
            r#"{"@type":"gx:ZonedDateTime","@value":"2016-12-23T12:12:24.000000036+02:00[GMT+02:00]"}"#,
        );
        assert_eq!(
            got.as_deref(),
            Some("datetime(2016-12-23T12:12:24.000000036+02:00)")
        );
        let kinds: Vec<&ImportNoteKind> = report.notes.iter().map(|n| &n.kind).collect();
        assert!(
            kinds.contains(&&ImportNoteKind::ZoneDropped {
                type_tag: "gx:ZonedDateTime".into()
            }),
            "{report}"
        );
        assert!(
            !kinds.contains(&&ImportNoteKind::SubMicrosecondsCut {
                type_tag: "gx:ZonedDateTime".into()
            }),
            "{report}"
        );
        assert!(!report.has_skips(), "{report}");
        assert!(
            report.to_string().contains("zone id is not kept"),
            "{report}"
        );
        // A zone name the bundled database knows is kept (temporal v2), nothing is reported.
        let (got, report) = one_value(
            r#"{"@type":"gx:ZonedDateTime","@value":"2007-12-03T10:15:30+01:00[Europe/Paris]"}"#,
        );
        assert_eq!(
            got.as_deref(),
            Some("datetime(2007-12-03T10:15:30+01:00[Europe/Paris])")
        );
        assert_eq!(report.notes.len(), 0, "{report}");
    }

    #[test]
    fn the_types_without_a_twin_and_malformed_dates_are_skipped() {
        for tag in ["gx:Year", "gx:YearMonth", "gx:MonthDay", "gx:ZoneOffset"] {
            let (got, report) = one_value(&format!(r#"{{"@type":"{tag}","@value":"2016"}}"#));
            assert_eq!(got, None, "{tag}");
            assert!(
                report.notes.iter().any(|n| n.kind
                    == ImportNoteKind::SkippedDateTime {
                        type_tag: tag.to_string()
                    }),
                "{tag}: {report}"
            );
        }
        for value in [
            r#"{"@type":"g:Date","@value":"2016-01-01"}"#,
            r#"{"@type":"gx:LocalDate","@value":"2016-13-01"}"#,
            r#"{"@type":"gx:LocalDate","@value":20000}"#,
            r#"{"@type":"gx:OffsetDateTime","@value":"10000-01-01T00:00:00Z"}"#,
            // An offset date-time without its offset is not read as UTC.
            r#"{"@type":"gx:OffsetDateTime","@value":"2016-01-01T10:00"}"#,
            r#"{"@type":"g:Date","@value":9223372036854775807}"#,
            r#"{"@type":"gx:Duration","@value":"P1M"}"#,
            r#"{"@type":"gx:Period","@value":"PT1H"}"#,
            r#"{"@type":"gx:LocalTime","@value":"25:00"}"#,
        ] {
            let (got, report) = one_value(value);
            assert_eq!(got, None, "{value}");
            assert!(
                report
                    .notes
                    .iter()
                    .any(|n| n.kind == ImportNoteKind::SkippedMalformedValue),
                "{value}: {report}"
            );
        }
    }

    #[test]
    fn an_untyped_date_text_stays_a_string() {
        // Rule 7: no type tag, no date (GraphSON 1.0, `types=false`).
        let (got, report) = one_value(r#""2016-12-14T16:39:19.349Z""#);
        assert_eq!(got.as_deref(), Some(r#""2016-12-14T16:39:19.349Z""#));
        assert!(report.notes.is_empty(), "{report}");
    }

    #[test]
    fn edge_and_meta_property_dates_are_imported() {
        let text = concat!(
            r#"{"id":"a","label":"x","outE":{"knows":[{"id":"e","inV":"b","properties":{"since":{"@type":"gx:LocalDate","@value":"2001-02-03"}}}]},"#,
            r#""properties":{"p":[{"value":1,"properties":{"at":{"@type":"gx:Instant","@value":"2001-02-03T04:05:06Z"}}}]}}"#,
            "\n",
            r#"{"id":"b","label":"x"}"#
        );
        let (graph, report) = load(text);
        assert!(!report.has_skips(), "{report}");
        let e = graph.get_edge_by_id("e").unwrap();
        assert_eq!(
            exact(graph.edge_properties(e).unwrap().get("since").unwrap()),
            "date(2001-02-03)"
        );
        let meta = vertex_map(&graph, "a");
        assert_eq!(
            exact(meta.get("_meta").unwrap()),
            "{p: {at: datetime(2001-02-03T04:05:06Z)}}"
        );
    }

    #[test]
    fn an_export_writes_java_time_tags_and_imports_back_exactly() {
        let graph = Graph::new(temporal_graph());
        let mut out = Vec::new();
        graph.export_graphson_writer(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        for part in [
            r#"{"@type":"gx:LocalDate","@value":"-9999-01-01"}"#,
            r#"{"@type":"gx:OffsetDateTime","@value":"2023-08-02T14:00:00.250+02:00"}"#,
            r#"{"@type":"gx:OffsetDateTime","@value":"2023-08-02T12:00:00Z"}"#,
            r#"{"@type":"gx:LocalDateTime","@value":"9999-12-31T23:59:59.999999"}"#,
            r#"{"@type":"gx:LocalTime","@value":"00:00:00"}"#,
            r#"{"@type":"gx:Duration","@value":"PT0S"}"#,
            r#"{"@type":"gx:Period","@value":"P1Y2M3D"}"#,
            r#"{"@type":"graphersal:Interval","@value":"P1M-2DT0.000003S"}"#,
        ] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        // `graphersal:Interval` is not a GraphSON type (TinkerGraph cannot load it), but our own
        // reader takes it back.
        let (back, report) = load(&text);
        assert!(report.notes.is_empty(), "{report}");
        assert_holds_edge_values(&back);
        let (got, report) = one_value(r#"{"@type":"graphersal:Interval","@value":"PT1H"}"#);
        assert_eq!(got, None);
        assert!(report.has_skips(), "{report}");
    }
}

// ---------------------------------------------------------------------------------------------
// GraphML
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "io")]
mod graphml {
    use super::*;

    fn schema() -> GraphSchema {
        GraphSchema::from_json(
            r#"{"mode": "open", "vertices": {"temporal": {"schema": {"type": "object", "properties": {
                "date_max": {"type": "string", "format": "date"},
                "local_max": {"type": "string", "format": "local-date-time"},
                "dt_offset": {"type": "string", "format": "date-time"},
                "time_max": {"type": "string", "format": "time"},
                "dur_mixed": {"type": "string", "format": "duration"},
                "nested": {"type": "array", "items": {"anyOf": [
                    {"type": "string", "format": "date-time"},
                    {"type": "object", "properties": {"d": {"type": "string", "format": "date"},
                        "p": {"type": "string", "format": "duration"}}}]}}
            }}}}}"#,
        )
        .unwrap()
    }

    fn export(graph: &Graph) -> String {
        let mut out = Vec::new();
        graph.export_graphml_writer(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn graphml_restores_temporal_values_from_the_target_schema() {
        let graph = Graph::new(temporal_graph());
        let xml = export(&graph);
        // The canonical text of each value.
        assert!(xml.contains(">2023-08-02T14:00:00.250+02:00<"), "{xml}");
        assert!(xml.contains(">P1M-2DT0.000003S<"), "{xml}");

        let mut target = Graph::new(TraversalGraph::new());
        target.write().set_schema(schema()).unwrap();
        target
            .import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))
            .unwrap();
        let lock = target.read();
        let props = vertex_map(&lock, "t");
        for key in [
            "date_max",
            "local_max",
            "dt_offset",
            "time_max",
            "dur_mixed",
            "nested",
        ] {
            assert_eq!(
                exact(props.get(key).unwrap()),
                exact(edge_map().get(key).unwrap()),
                "{key}"
            );
        }
        // Undeclared: the text stays a string (rule 7).
        assert_eq!(exact(props.get("date_min").unwrap()), r#""-9999-01-01""#);
    }

    #[test]
    fn graphml_text_that_is_not_of_the_declared_type_is_refused() {
        let xml = r#"<?xml version="1.0"?><graphml xmlns="http://graphml.graphdrawing.org/xmlns">
            <key id="d" for="node" attr.name="date_max" attr.type="string"/>
            <key id="l" for="node" attr.name="labelV" attr.type="string"/>
            <graph edgedefault="directed"><node id="t"><data key="l">temporal</data><data key="d">2024-02-30</data></node></graph></graphml>"#;
        let mut target = Graph::new(TraversalGraph::new());
        target.write().set_schema(schema()).unwrap();
        let err = target
            .import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))
            .unwrap_err();
        assert!(err.to_string().contains("date"), "{err}");
        assert_eq!(target.read().vertex_count(), 0);
    }
}

// ---------------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------------

mod schema {
    use super::*;
    use graphersal::schema::{PropertySchema, SchemaChangeKind, SchemaFormat};
    use graphersal::temporal::TemporalKind;

    fn vertex_schema(mode: &str, properties: &str) -> String {
        format!(
            r#"{{"mode": "{mode}", "vertices": {{"e": {{"schema": {{"type": "object", "properties": {properties}}}}}}}}}"#
        )
    }

    fn parse(mode: &str, properties: &str) -> Result<GraphSchema, SchemaError> {
        GraphSchema::from_json(&vertex_schema(mode, properties))
    }

    fn graph_with(mode: &str, properties: &str) -> Graph {
        let graph = Graph::new(TraversalGraph::new());
        graph
            .write()
            .set_schema(parse(mode, properties).unwrap())
            .unwrap();
        graph
    }

    fn add(graph: &Graph, key: &str, value: ElementProperty) -> Result<String, GraphError> {
        let mut lock = graph.write();
        let mut props = PropertyMap::default();
        props.insert(key, value);
        lock.transaction(|g| {
            let v = g.add_vertex(None, vec!["e".into()], props)?;
            Ok(exact(g.vertex_properties(v).unwrap().get(key).unwrap()))
        })
    }

    #[test]
    fn every_temporal_format_parses_prints_and_names_its_type() {
        let properties = r#"{
            "a": {"type": "string", "format": "date-time", "formatMinimum": "2020-01-01T00:00:00+01:00", "formatExclusiveMaximum": "2030-01-01T00:00:00Z"},
            "b": {"type": "string", "format": "date", "formatExclusiveMinimum": "2020-01-01", "formatMaximum": "2020-12-31"},
            "c": {"type": ["string", "null"], "format": "local-date-time"},
            "d": {"type": "string", "format": "time", "formatMinimum": "08:00"},
            "e": {"type": "string", "format": "duration", "formatMaximum": "P1D"},
            "f": {"type": "array", "items": {"type": "string", "format": "date"}}
        }"#;
        let schema = parse("open", properties).unwrap();
        let text = schema.to_json();
        for part in [
            r#""format":"date-time","formatMinimum":"2020-01-01T00:00:00+01:00","formatExclusiveMaximum":"2030-01-01T00:00:00Z""#,
            r#""format":"date","formatExclusiveMinimum":"2020-01-01","formatMaximum":"2020-12-31""#,
            r#""format":"local-date-time""#,
            r#""format":"time","formatMinimum":"08:00:00""#,
            r#""format":"duration","formatMaximum":"P1D""#,
        ] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        // The printed form loads to the same schema.
        assert_eq!(GraphSchema::from_json(&text).unwrap(), schema);
        let node = |key: &str| schema.vertices["e"].schema.properties[key].clone();
        assert_eq!(node("a").type_name(), "datetime");
        assert_eq!(node("b").type_name(), "date");
        assert_eq!(node("c").type_name(), "union<localdatetime|null>");
        assert_eq!(node("d").type_name(), "time");
        assert_eq!(node("e").type_name(), "duration");
        assert_eq!(node("f").type_name(), "array<date>");
        // The builders give the same nodes.
        assert_eq!(
            PropertySchema::date(),
            node("f").items.as_deref().unwrap().clone()
        );
        for kind in [
            TemporalKind::Date,
            TemporalKind::LocalDateTime,
            TemporalKind::DateTime,
            TemporalKind::Time,
            TemporalKind::Duration,
        ] {
            let format = SchemaFormat::of_temporal(kind);
            assert_eq!(format.temporal_kind(), Some(kind));
            assert_eq!(SchemaFormat::from_name(format.as_str()), Some(format));
            assert_eq!(PropertySchema::temporal(kind).type_name(), kind.name());
        }
        // The visualizers name the types and bounds.
        let table = schema.visualize(VisualizeFormat::Table);
        assert!(table.contains("datetime"), "{table}");
        assert!(table.contains(">= 2020-01-01T00:00:00+01:00"), "{table}");
    }

    #[test]
    fn invalid_temporal_schemas_name_the_problem() {
        let invalid = |properties: &str| match parse("open", properties) {
            Err(err @ SchemaError::InvalidSchema { .. }) => {
                graphersal::error::format_diagnostic(&err)
            }
            other => panic!("{properties}: expected InvalidSchema, got {other:?}"),
        };
        let text = invalid(r#"{"a": {"type": "string", "format": "email"}}"#);
        assert!(text.contains("unsupported format 'email'"), "{text}");
        assert!(text.contains("local-date-time"), "{text}");
        let text = invalid(r#"{"a": {"type": "string", "formatMinimum": "2020-01-01"}}"#);
        assert!(text.contains("needs a date or time format"), "{text}");
        let text = invalid(r#"{"a": {"type": "string", "format": "uuid", "formatMaximum": "x"}}"#);
        assert!(text.contains("needs a date or time format"), "{text}");
        let text = invalid(r#"{"a": {"type": "integer", "formatMinimum": "2020-01-01"}}"#);
        assert!(text.contains("formatMinimum"), "{text}");
        let text = invalid(
            r#"{"a": {"type": "string", "format": "date", "formatMinimum": "2020-01-01T00:00:00Z"}}"#,
        );
        assert!(text.contains("formatMinimum is not a date"), "{text}");
        assert!(text.contains("2024-05-01"), "{text}");
        let text =
            invalid(r#"{"a": {"type": "string", "format": "date", "formatMinimum": 20200101}}"#);
        assert!(text.contains("formatMinimum"), "{text}");
        let text = invalid(r#"{"a": {"format": "date-time"}}"#);
        assert!(text.contains("format needs"), "{text}");
        let text = invalid(r#"{"a": {"type": "datetime"}}"#);
        assert!(
            text.contains(r#"\"format\": \"date-time\""#)
                || text.contains(r#""format": "date-time""#),
            "{text}"
        );
        // `minimum` stays a number keyword: it is not a temporal bound.
        let text =
            invalid(r#"{"a": {"type": "string", "format": "date", "minimum": "2020-01-01"}}"#);
        assert!(text.contains("minimum"), "{text}");
    }

    #[test]
    fn a_declared_temporal_property_coerces_its_text_on_write_and_is_checked_exactly() {
        let graph = graph_with(
            "open",
            r#"{"at": {"type": "string", "format": "date-time"}, "day": {"type": "string", "format": "date"},
                "span": {"type": "string", "format": "duration"}, "t": {"type": "string", "format": "time"},
                "l": {"type": "string", "format": "local-date-time"}, "s": {"type": "string"}}"#,
        );
        // Text the type's parser accepts is coerced (an explicit declaration, not sniffing).
        assert_eq!(
            add(&graph, "at", "2023-08-02T14:00:00+02:00".into()).unwrap(),
            "datetime(2023-08-02T14:00:00+02:00)"
        );
        assert_eq!(
            add(&graph, "at", "2023-08-02 14:00Z".into()).unwrap(),
            "datetime(2023-08-02T14:00:00Z)"
        );
        assert_eq!(
            add(&graph, "day", "2024-02-29".into()).unwrap(),
            "date(2024-02-29)"
        );
        assert_eq!(add(&graph, "span", "P2W".into()).unwrap(), "duration(P14D)");
        assert_eq!(add(&graph, "t", "09:30".into()).unwrap(), "time(09:30:00)");
        assert_eq!(
            add(&graph, "l", "2024-01-01T00:00".into()).unwrap(),
            "localdatetime(2024-01-01T00:00:00)"
        );
        // A temporal value of the declared type is stored as it is.
        assert_eq!(
            add(&graph, "day", date("2024-01-01")).unwrap(),
            "date(2024-01-01)"
        );
        // Text that is not of the type is a coercion failure naming the type.
        let err = add(&graph, "day", "2024-02-30".into()).unwrap_err();
        assert!(err.to_string().contains("not a date"), "{err}");
        let err = add(&graph, "t", "09:30:00+02:00".into()).unwrap_err();
        assert!(err.to_string().contains("not a time"), "{err}");
        // Another temporal type, a number or a plain string field is a type mismatch.
        let err = add(&graph, "day", dt("2024-01-01T00:00:00Z")).unwrap_err();
        assert!(err.to_string().contains("date"), "{err}");
        assert!(add(&graph, "at", 5i64.into()).is_err());
        assert!(add(&graph, "s", date("2024-01-01")).is_err());
        assert_eq!(graph.read().vertex_count(), 7);
    }

    #[test]
    fn a_declared_date_time_needs_an_explicit_offset_or_zone() {
        let graph = graph_with(
            "open",
            r#"{"at": {"type": "string", "format": "date-time"}}"#,
        );
        // An offset or a zone is explicit.
        assert_eq!(
            add(&graph, "at", "2024-01-01T10:00Z".into()).unwrap(),
            "datetime(2024-01-01T10:00:00Z)"
        );
        assert_eq!(
            add(&graph, "at", "2024-01-01T10:00[Europe/Berlin]".into()).unwrap(),
            "datetime(2024-01-01T10:00:00+01:00[Europe/Berlin])"
        );
        // Without either the text is not read as UTC (rule 7: no guessing).
        for text in ["2024-01-01T10:00", "2024-01-01"] {
            let err = add(&graph, "at", text.into()).unwrap_err();
            assert!(err.to_string().contains("UTC offset"), "{text}: {err}");
        }
        // A format bound is read the same way.
        let err = parse(
            "open",
            r#"{"at": {"type": "string", "format": "date-time", "formatMinimum": "2020-01-01T00:00"}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("formatMinimum"), "{err}");
        assert_eq!(graph.read().vertex_count(), 2);
    }

    #[test]
    fn a_closed_graph_checks_temporal_values_at_any_depth_and_through_jpath_writes() {
        let graph = graph_with(
            "closed",
            r#"{"meta": {"type": "object", "properties": {
                "seen": {"type": "array", "items": {"type": "string", "format": "date-time"}},
                "window": {"type": "object", "properties": {"from": {"type": "string", "format": "date"}}}}}}"#,
        );
        let mut nested = PropertyMap::default();
        nested.insert(
            "seen",
            ElementProperty::Array(vec![
                "2024-01-01T00:00:00Z".into(),
                dt("2024-01-02T00:00:00+01:00"),
            ]),
        );
        assert_eq!(
            add(&graph, "meta", ElementProperty::Object(Box::new(nested))).unwrap(),
            "{seen: [datetime(2024-01-01T00:00:00Z), datetime(2024-01-02T00:00:00+01:00)]}"
        );
        let mut bad = PropertyMap::default();
        bad.insert("seen", ElementProperty::Array(vec!["yesterday".into()]));
        let err = add(&graph, "meta", ElementProperty::Object(Box::new(bad))).unwrap_err();
        assert!(err.to_string().contains("not a datetime"), "{err}");
        // A jpath write coerces its leaf to the declared type.
        let mut lock = graph.write();
        let v = lock.vertices().next().unwrap();
        let path: JsonPath = "meta.window.from".parse().unwrap();
        lock.transaction(|g| g.set_vertex_property_path(v, &path, "2024-03-01".into()))
            .unwrap();
        let meta = lock.vertex_properties(v).unwrap().get("meta").unwrap();
        assert!(
            exact(meta).contains("window: {from: date(2024-03-01)}"),
            "{}",
            exact(meta)
        );
        let err = lock
            .transaction(|g| g.set_vertex_property_path(v, &path, "2024-03-01T00:00:00".into()))
            .unwrap_err();
        assert!(err.to_string().contains("not a date"), "{err}");
    }

    #[test]
    fn format_bounds_are_checked_with_the_types_ordering() {
        let graph = graph_with(
            "open",
            r#"{"at": {"type": "string", "format": "date-time", "formatMinimum": "2020-01-01T00:00:00Z", "formatExclusiveMaximum": "2021-01-01T00:00:00Z"},
                "span": {"type": "string", "format": "duration", "formatMaximum": "P1M"},
                "t": {"type": "string", "format": "time", "formatExclusiveMinimum": "08:00"}}"#,
        );
        // A datetime compares by its instant: 2020-01-01T01:00+01:00 is the minimum itself.
        assert!(add(&graph, "at", "2020-01-01T01:00:00+01:00".into()).is_ok());
        let err = add(&graph, "at", "2020-01-01T00:59:59+01:00".into()).unwrap_err();
        assert!(err.to_string().contains(">= 2020-01-01T00:00:00Z"), "{err}");
        assert!(add(&graph, "at", "2020-12-31T23:59:59.999999Z".into()).is_ok());
        let err = add(&graph, "at", "2021-01-01T01:00:00+01:00".into()).unwrap_err();
        assert!(err.to_string().contains("< 2021-01-01T00:00:00Z"), "{err}");
        // A duration by its length (a month counts 30 days): P30D is not above P1M, P31D is.
        assert!(add(&graph, "span", "P30D".into()).is_ok());
        assert!(add(&graph, "span", "P31D".into()).is_err());
        assert!(add(&graph, "t", "08:00:00.000001".into()).is_ok());
        assert!(add(&graph, "t", "08:00".into()).is_err());
        // Existing data is checked when a schema with bounds is set.
        let data = Graph::new(TraversalGraph::new());
        add(&data, "at", dt("2019-06-01T00:00:00Z")).unwrap();
        let err = data
            .write()
            .set_schema(
                parse(
                    "open",
                    r#"{"at": {"type": "string", "format": "date-time", "formatMinimum": "2020-01-01T00:00:00Z"}}"#,
                )
                .unwrap(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("violates a constraint"), "{err}");
    }

    #[test]
    fn inference_reports_the_exact_temporal_type() {
        let graph = Graph::new(TraversalGraph::new());
        add(&graph, "at", dt("2024-01-01T00:00:00Z")).unwrap();
        add(&graph, "mixed", dt("2024-01-01T00:00:00Z")).unwrap();
        add(&graph, "mixed", "a string".into()).unwrap();
        add(&graph, "two", date("2024-01-01")).unwrap();
        add(&graph, "two", duration("P1D")).unwrap();
        let mut list = PropertyMap::default();
        list.insert(
            "days",
            ElementProperty::Array(vec![date("2024-01-01"), ElementProperty::Null]),
        );
        add(&graph, "obj", ElementProperty::Object(Box::new(list))).unwrap();
        let inferred = graph.read().traversal().infer_schema().unwrap();
        let text = inferred.to_json();
        for part in [
            r#""at":{"type":"string","format":"date-time"}"#,
            r#""mixed":{"anyOf":[{"type":"string"},{"type":"string","format":"date-time"}]}"#,
            r#""two":{"anyOf":[{"type":"string","format":"date"},{"type":"string","format":"duration"}]}"#,
            r#""days":{"type":"array","items":{"type":["string","null"],"format":"date"}}"#,
        ] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        // Freezing the structure: the inferred schema admits its data in closed mode.
        graph
            .write()
            .set_schema(inferred.with_mode(SchemaMode::Closed))
            .unwrap();
        assert!(add(&graph, "two", local("2024-01-01T00:00:00")).is_err());
        assert!(add(&graph, "two", "P1D".into()).is_ok());
    }

    #[test]
    fn diff_classifies_format_and_bound_changes() {
        let diff = |old: &str, new: &str| {
            let graph = graph_with("open", old);
            let lock = graph.read();
            lock.traversal()
                .diff_schema(&parse("open", new).unwrap())
                .unwrap()
        };
        let a = |rest: &str| format!(r#"{{"a": {{"type": "string", "format": "date"{rest}}}}}"#);
        // Adding a bound is incompatible, removing one compatible.
        let added = diff(&a(""), &a(r#", "formatMinimum": "2020-01-01""#));
        assert!(!added.compatible(), "{added:?}");
        assert!(
            added
                .changes
                .iter()
                .any(|c| c.kind == SchemaChangeKind::ConstraintChanged
                    && c.after.as_deref() == Some("formatMinimum 2020-01-01")),
            "{added:?}"
        );
        assert!(diff(&a(r#", "formatMinimum": "2020-01-01""#), &a("")).compatible());
        // Relaxing (lower minimum, higher maximum) is compatible, tightening incompatible.
        assert!(
            diff(
                &a(r#", "formatMinimum": "2020-01-01""#),
                &a(r#", "formatMinimum": "2019-01-01""#)
            )
            .compatible()
        );
        assert!(
            !diff(
                &a(r#", "formatMinimum": "2020-01-01""#),
                &a(r#", "formatMinimum": "2021-01-01""#)
            )
            .compatible()
        );
        assert!(
            diff(
                &a(r#", "formatExclusiveMaximum": "2020-01-01""#),
                &a(r#", "formatExclusiveMaximum": "2021-01-01""#)
            )
            .compatible()
        );
        assert!(
            !diff(
                &a(r#", "formatMaximum": "2020-01-01""#),
                &a(r#", "formatMaximum": "2019-12-31""#)
            )
            .compatible()
        );
        // Changing, adding or removing the format is incompatible.
        let changed = diff(
            r#"{"a": {"type": "string", "format": "date"}}"#,
            r#"{"a": {"type": "string", "format": "date-time"}}"#,
        );
        assert!(!changed.compatible());
        assert!(
            changed
                .changes
                .iter()
                .any(|c| c.kind == SchemaChangeKind::TypeChanged
                    && c.before.as_deref() == Some("date")
                    && c.after.as_deref() == Some("datetime")),
            "{changed:?}"
        );
        assert!(
            !diff(
                r#"{"a": {"type": "string"}}"#,
                r#"{"a": {"type": "string", "format": "time"}}"#
            )
            .compatible()
        );
        assert!(
            !diff(
                r#"{"a": {"type": "string", "format": "time"}}"#,
                r#"{"a": {"type": "string"}}"#
            )
            .compatible()
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Randomized: every format keeps random temporal values; random text never panics a coercion
// ---------------------------------------------------------------------------------------------

#[cfg(all(feature = "persist", feature = "io", feature = "serde"))]
mod randomized {
    use super::*;
    use graphersal::changes::{ChangeSet, Mutation};
    use graphersal::io::GraphSONImport;
    use graphersal::persist::{self, ReadOptions, WriteOptions};

    /// SplitMix64: a seeded, dependency-free generator.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn range(&mut self, low: i64, high: i64) -> i64 {
            low + (self.next() % ((high - low) as u64 + 1)) as i64
        }
    }

    const DAY_MIN: i64 = -4_371_587;
    const DAY_MAX: i64 = 2_932_896;
    const MICROS_PER_DAY: i64 = 86_400_000_000;

    fn random_value(rng: &mut Rng) -> ElementProperty {
        let micros_min = DAY_MIN * MICROS_PER_DAY;
        let micros_max = (DAY_MAX + 1) * MICROS_PER_DAY - 1;
        match rng.next() % 5 {
            0 => Date::from_days_since_epoch(rng.range(DAY_MIN, DAY_MAX) as i32)
                .unwrap()
                .into(),
            1 => LocalDateTime::from_micros(rng.range(micros_min, micros_max))
                .unwrap()
                .into(),
            2 => {
                let offset = rng.range(-64_800, 64_800) as i32;
                let margin = 64_800 * 1_000_000;
                DateTime::from_micros(rng.range(micros_min + margin, micros_max - margin), offset)
                    .unwrap()
                    .into()
            }
            3 => Time::from_micros_of_day(rng.range(0, MICROS_PER_DAY - 1))
                .unwrap()
                .into(),
            _ => {
                let months = if rng.next().is_multiple_of(3) {
                    0
                } else {
                    rng.range(i32::MIN as i64, i32::MAX as i64) as i32
                };
                let days = if rng.next().is_multiple_of(3) {
                    0
                } else {
                    rng.range(i32::MIN as i64, i32::MAX as i64) as i32
                };
                let micros = if rng.next().is_multiple_of(3) {
                    0
                } else {
                    rng.next() as i64
                };
                Duration::new(months, days, micros).into()
            }
        }
    }

    #[test]
    fn random_temporal_values_survive_every_format() {
        let seed = 0x7E3D_0A2B;
        let mut rng = Rng(seed);
        for round in 0..20 {
            let mut props = PropertyMap::default();
            for key in 0..50 {
                props.insert(format!("k{key}").as_str(), random_value(&mut rng));
            }
            let mut graph = TraversalGraph::new();
            let ((), changes) = graph
                .dry_run(|g| {
                    g.add_vertex(Some("v".into()), vec!["x".into()], props.clone())?;
                    Ok::<_, GraphError>(())
                })
                .unwrap();
            graph
                .transaction(|g| {
                    g.add_vertex(Some("v".into()), vec!["x".into()], props.clone())?;
                    Ok::<_, GraphError>(())
                })
                .unwrap();
            let expected = exact_map(&props);
            // Binary.
            let mut bytes = Vec::new();
            persist::write_snapshot(&graph, &mut bytes, &WriteOptions::new()).unwrap();
            let loaded = persist::read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
            assert_eq!(
                exact_map(&vertex_map(&loaded, "v")),
                expected,
                "round {round}"
            );
            // ChangeSet serde.
            let text = serde_json::to_string(&changes).unwrap();
            let back: ChangeSet = serde_json::from_str(&text).unwrap();
            match &back.mutations()[0] {
                Mutation::AddVertex { properties, .. } => {
                    assert_eq!(exact_map(properties), expected, "round {round}")
                }
                other => panic!("{other:?}"),
            }
            // GraphSON.
            let shared = Graph::new(graph);
            let mut out = Vec::new();
            shared.export_graphson_writer(&mut out).unwrap();
            let mut imported = TraversalGraph::new();
            let report = GraphSONImport::new()
                .import_reader(&mut imported, out.as_slice())
                .unwrap();
            assert!(report.notes.is_empty(), "round {round}: {report}");
            assert_eq!(
                exact_map(&vertex_map(&imported, "v")),
                expected,
                "round {round}"
            );
        }
    }

    #[test]
    fn random_text_never_panics_a_declared_temporal_coercion() {
        let graph = Graph::new(TraversalGraph::new());
        graph
            .write()
            .set_schema(
                GraphSchema::from_json(
                    r#"{"mode": "open", "vertices": {"e": {"schema": {"type": "object", "properties": {
                        "a": {"type": "string", "format": "date-time"}, "b": {"type": "string", "format": "date"},
                        "c": {"type": "string", "format": "local-date-time"}, "d": {"type": "string", "format": "time"},
                        "f": {"type": "string", "format": "duration", "formatMaximum": "P1D"}}}}}}"#,
                )
                .unwrap(),
            )
            .unwrap();
        let alphabet = b"0123456789-+:.,TZPYMWDHS []tz!/Ea";
        let mut rng = Rng(42);
        let mut lock = graph.write();
        for _ in 0..20_000 {
            let len = (rng.next() % 32) as usize;
            let text: String = (0..len)
                .map(|_| alphabet[(rng.next() % alphabet.len() as u64) as usize] as char)
                .collect();
            for key in ["a", "b", "c", "d", "f"] {
                let mut props = PropertyMap::default();
                props.insert(key, text.as_str().into());
                let _ = lock.transaction(|g| {
                    g.add_vertex(None, vec!["e".into()], props)?;
                    Ok::<_, GraphError>(())
                });
            }
        }
    }
}
