//! Host events in the system journal (issue #35, book page "System Journal", section "Host
//! events"): a host declares its own kinds once per process and records them beside the
//! engine's; malformed declarations, undeclared kinds and fields that do not match are errors,
//! never silent writes. Declarations are process-wide, so every test uses its own codes.

use std::sync::{Arc, Mutex};

use graphersal::prelude::*;
use graphersal::system_journal::{
    HostEvent, HostEventKind, JournalError, JournalQuery, JournalRecord, JournalSink, Level,
    declare, declared_kinds,
};

fn records(graph: &Graph, prefix: &str) -> Vec<JournalRecord> {
    graph
        .system_journal()
        .unwrap()
        .query(&JournalQuery::new().source_prefix(prefix))
        .unwrap()
        .records
}

fn job_failed(code: &str, source: &str) -> HostEventKind {
    HostEventKind::new(
        code,
        source,
        Level::Error,
        "A background job of the host failed.",
    )
    .field("job", "the job's name")
    .optional_field("cause", "the error")
}

#[test]
fn a_declared_host_event_is_recorded_beside_the_engines() {
    declare(job_failed("hosta.job_failed", "hosta.jobs")).unwrap();
    let graph = Graph::new(TraversalGraph::new());
    let journal = graph.system_journal().unwrap();
    // Fields given out of order are kept in declaration order.
    let seq = journal
        .record(
            HostEvent::new("hosta.job_failed", "the job nightly failed: timeout")
                .field("cause", "timeout")
                .field("job", "nightly"),
        )
        .unwrap();
    let found = records(&graph, "hosta");
    assert_eq!(found.len(), 1);
    let record = &found[0];
    assert_eq!(seq, Some(record.seq));
    assert_eq!(record.kind, "hosta.job_failed");
    assert_eq!(record.source, "hosta.jobs");
    assert_eq!(record.level, Level::Error);
    assert_eq!(record.message, "the job nightly failed: timeout");
    assert_eq!(record.principal.as_deref(), Some("host"));
    let names: Vec<&str> = record
        .fields
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["job", "cause"]);
    assert_eq!(record.field("job"), Some(&ElementProperty::from("nightly")));
    // The kind filter and the level filter see it like an engine record.
    let by_kind = journal
        .query(
            &JournalQuery::new()
                .kind("hosta.job_failed")
                .min_level(Level::Error),
        )
        .unwrap();
    assert_eq!(by_kind.records.len(), 1);
}

#[test]
fn the_envelope_options_and_an_optional_field_left_out() {
    declare(
        HostEventKind::new(
            "hostb.restart",
            "hostb",
            Level::Notice,
            "The host restarted.",
        )
        .field("reason", "why")
        .optional_field("attempt", "the attempt"),
    )
    .unwrap();
    let graph = Graph::new(TraversalGraph::new());
    let journal = graph.system_journal().unwrap();
    let start = journal
        .record(HostEvent::new("hostb.restart", "restarting").field("reason", "update"))
        .unwrap()
        .unwrap();
    journal
        .record(
            HostEvent::new("hostb.restart", "restarted")
                .field("reason", "update")
                .field("attempt", 2i64)
                .level(Level::Warn)
                .op(start)
                .principal("operator"),
        )
        .unwrap();
    let found = records(&graph, "hostb");
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].level, Level::Notice);
    assert_eq!(found[0].fields.len(), 1);
    assert_eq!(found[1].level, Level::Warn);
    assert_eq!(found[1].op, Some(start));
    assert_eq!(found[1].principal.as_deref(), Some("operator"));
}

#[test]
fn repeats_collapse_by_the_declared_key() {
    declare(
        HostEventKind::new("hostc.item_failed", "hostc", Level::Warn, "An item failed.")
            .field("queue", "the queue")
            .field("item", "the item")
            .collapse_by(["queue"]),
    )
    .unwrap();
    let graph = Graph::new(TraversalGraph::new());
    let journal = graph.system_journal().unwrap();
    let event = |item: &str| {
        HostEvent::new("hostc.item_failed", format!("item {item} failed"))
            .field("queue", "in")
            .field("item", item)
    };
    assert!(journal.record(event("1")).unwrap().is_some());
    assert_eq!(journal.record(event("2")).unwrap(), None);
    assert_eq!(journal.record(event("3")).unwrap(), None);
    assert_eq!(records(&graph, "hostc").len(), 1);
}

#[test]
fn declaring_again_is_a_no_op_and_a_different_declaration_is_refused() {
    declare(job_failed("hostd.job_failed", "hostd.jobs")).unwrap();
    declare(job_failed("hostd.job_failed", "hostd.jobs")).unwrap();
    let listed = declared_kinds()
        .into_iter()
        .filter(|kind| kind.code() == "hostd.job_failed")
        .count();
    assert_eq!(listed, 1);
    let other = HostEventKind::new("hostd.job_failed", "hostd.jobs", Level::Warn, "Else.");
    let err = declare(other).unwrap_err();
    assert!(
        matches!(&err, JournalError::InvalidDeclaration { kind, reason, .. }
            if kind == "hostd.job_failed" && reason.contains("already declared")),
        "{err}"
    );
}

#[test]
fn malformed_declarations_are_refused() {
    let refused = |kind: HostEventKind, part: &str| {
        let err = declare(kind).unwrap_err();
        match &err {
            JournalError::InvalidDeclaration { reason, .. } => {
                assert!(reason.contains(part), "{reason:?} should name {part:?}")
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(err.help().contains("Host events"), "{}", err.help());
    };
    let kind = |code: &str, source: &str| HostEventKind::new(code, source, Level::Info, "X.");
    // An engine code, an engine segment, the reserved `graphersal`.
    refused(kind("store.opened", "hoste"), "store");
    refused(kind("store.my_event", "hoste"), "engine");
    refused(kind("hoste.event", "journal.mine"), "source");
    refused(kind("graphersal.event", "hoste"), "graphersal");
    // Names.
    refused(kind("hoste", "hoste"), "dotted segments");
    refused(kind("hoste.Event", "hoste"), "segment `Event`");
    refused(kind("hoste.event", "hoste."), "source");
    refused(kind("hoste.", "hoste"), "code");
    refused(
        HostEventKind::new("hoste.event", "hoste", Level::Info, " "),
        "description",
    );
    // Fields and the collapse key.
    refused(
        kind("hoste.event", "hoste").field("Bad", "x"),
        "field `Bad`",
    );
    refused(
        kind("hoste.event", "hoste")
            .field("a", "x")
            .optional_field("a", "y"),
        "declared twice",
    );
    refused(
        kind("hoste.event", "hoste")
            .field("a", "x")
            .collapse_by(["b"]),
        "not a declared field",
    );
    // Nothing of these was declared.
    assert!(
        declared_kinds()
            .iter()
            .all(|kind| !kind.code().starts_with("hoste") && kind.code() != "store.opened")
    );
}

#[test]
fn events_that_do_not_match_a_declaration_are_refused_and_nothing_is_recorded() {
    declare(job_failed("hostf.job_failed", "hostf.jobs")).unwrap();
    let graph = Graph::new(TraversalGraph::new());
    let journal = graph.system_journal().unwrap();
    let before = journal.query(&JournalQuery::new()).unwrap().records.len();

    let err = journal
        .record(HostEvent::new("hostf.unknown", "x"))
        .unwrap_err();
    assert!(matches!(&err, JournalError::Undeclared { kind, .. } if kind == "hostf.unknown"));
    assert!(err.help().contains("declare"), "{}", err.help());

    let invalid = |event: HostEvent, part: &str| {
        let err = journal.record(event).unwrap_err();
        match &err {
            JournalError::InvalidEvent { reason, .. } => {
                assert!(reason.contains(part), "{reason:?} should name {part:?}")
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(err.help().contains("required field"), "{}", err.help());
    };
    invalid(HostEvent::new("store.opened", "x"), "engine");
    invalid(
        HostEvent::new("hostf.job_failed", "x").field("cause", "y"),
        "required field `job`",
    );
    invalid(
        HostEvent::new("hostf.job_failed", "x")
            .field("job", "a")
            .field("other", 1i64),
        "`other`",
    );
    invalid(
        HostEvent::new("hostf.job_failed", "x")
            .field("job", "a")
            .field("job", "b"),
        "given twice",
    );
    let after = journal.query(&JournalQuery::new()).unwrap().records.len();
    assert_eq!(before, after);
}

/// A sink recording the kinds it receives.
struct Kinds(Arc<Mutex<Vec<String>>>);

impl JournalSink for Kinds {
    fn record(&self, record: &JournalRecord) {
        self.0.lock().unwrap().push(record.kind.clone());
    }

    fn name(&self) -> &str {
        "kinds"
    }
}

#[test]
fn sinks_receive_host_records() {
    declare(job_failed("hostg.job_failed", "hostg.jobs")).unwrap();
    let graph = Graph::new(TraversalGraph::new());
    let journal = graph.system_journal().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    journal.add_sink(Box::new(Kinds(seen.clone()))).unwrap();
    journal
        .record(HostEvent::new("hostg.job_failed", "failed").field("job", "a"))
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), ["hostg.job_failed"]);
}

#[cfg(feature = "persist")]
mod stored {
    use super::*;
    use graphersal::persist::{MemDir, Store, StoreOptions};

    #[test]
    fn host_records_are_stored_with_the_stores_journal_and_read_after_close() {
        declare(job_failed("hosth.job_failed", "hosth.jobs")).unwrap();
        declare(
            HostEventKind::new("hosth.started", "hosth", Level::Info, "The host started.")
                .field("version", "the host's version"),
        )
        .unwrap();
        let dir = MemDir::new();
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        let journal = store.journal().unwrap();
        journal
            .record(HostEvent::new("hosth.started", "started").field("version", "1.2.3"))
            .unwrap();
        journal
            .record(
                HostEvent::new("hosth.job_failed", "the job a panicked")
                    .field("job", "a")
                    .field("cause", "panic"),
            )
            .unwrap();
        store.close().unwrap();

        let page = Store::journal_dir(&dir, &JournalQuery::new().source_prefix("hosth")).unwrap();
        let kinds: Vec<&str> = page.records.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, ["hosth.started", "hosth.job_failed"]);
        let failed = &page.records[1];
        assert_eq!(failed.level, Level::Error);
        assert_eq!(failed.field("cause"), Some(&ElementProperty::from("panic")));

        // And again after the store is opened anew.
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        let page = store
            .journal()
            .unwrap()
            .query(&JournalQuery::new().kind("hosth.started"))
            .unwrap();
        assert_eq!(page.records.len(), 1);
        store.close().unwrap();
    }
}
