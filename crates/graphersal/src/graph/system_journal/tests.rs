//! Crate unit tests of the system journal: collapsing with a fake clock, states and reminders,
//! the `tracing` mirror (a thread-local subscriber), lost records.

use std::sync::Mutex;
use std::sync::atomic::AtomicI64;

use super::*;

static FAKE_NOW: AtomicI64 = AtomicI64::new(0);
static FAKE_LOCK: Mutex<()> = Mutex::new(());

fn fake_now() -> i64 {
    FAKE_NOW.load(Ordering::SeqCst)
}

fn set_now(us: i64) {
    FAKE_NOW.store(us, Ordering::SeqCst);
}

fn journal() -> Arc<SystemJournal> {
    SystemJournal::in_memory(CommitClock::with_source(fake_now))
}

fn denied(journal: &SystemJournal, channel: &str) {
    journal.emit(
        &catalogue::AUTH_DENIED,
        Event::new("denied")
            .field("action", "Read")
            .field("resource_kind", "Data")
            .field("channel", channel),
    );
}

fn kinds(journal: &SystemJournal) -> Vec<String> {
    journal
        .query(&JournalQuery::new())
        .unwrap()
        .records
        .into_iter()
        .map(|record| record.kind)
        .collect()
}

#[test]
fn six_identical_events_are_one_record_and_a_repeated_one_at_flush() {
    let _guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_now(1_000_000_000);
    let journal = journal();
    for i in 0..6 {
        set_now(1_000_000_000 + i * 1000);
        denied(&journal, "mcp");
    }
    assert_eq!(kinds(&journal), ["auth.denied"]);
    journal.flush().unwrap();
    let page = journal.query(&JournalQuery::new()).unwrap();
    assert_eq!(page.records.len(), 2);
    let repeated = &page.records[1];
    assert_eq!(repeated.kind, "journal.repeated");
    assert_eq!(repeated.level, Level::Warn);
    assert_eq!(repeated.field("count"), Some(&ElementProperty::from(5i64)));
    assert_eq!(
        repeated.field("of_origin_seq"),
        Some(&ElementProperty::from(1i64))
    );
    assert_eq!(
        repeated.field("first_us"),
        Some(&ElementProperty::from(1_000_001_000i64))
    );
    assert_eq!(
        repeated.field("last_us"),
        Some(&ElementProperty::from(1_000_005_000i64))
    );
}

#[test]
fn key_fields_tell_events_apart_and_an_original_leaving_closes_its_group() {
    let _guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_now(2_000_000_000);
    let journal = journal();
    denied(&journal, "mcp");
    denied(&journal, "mcp");
    denied(&journal, "cli");
    assert_eq!(kinds(&journal), ["auth.denied", "auth.denied"]);
    // Five other records push the first one out of the last five: its group is closed.
    for _ in 0..4 {
        journal.emit(&catalogue::STORE_OPENED, Event::new("opened"));
    }
    let kinds = kinds(&journal);
    assert_eq!(kinds.len(), 7, "{kinds:?}");
    assert_eq!(kinds[6], "journal.repeated");
}

#[test]
fn a_group_is_closed_after_the_window_and_the_problem_shows_again() {
    let _guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_now(3_000_000_000);
    let journal = journal();
    denied(&journal, "mcp");
    set_now(3_000_000_000 + 1_000_000);
    denied(&journal, "mcp");
    set_now(3_000_000_000 + collapse::COLLAPSE_WINDOW_US + 1);
    denied(&journal, "mcp");
    assert_eq!(
        kinds(&journal),
        ["auth.denied", "journal.repeated", "auth.denied"]
    );
}

#[test]
fn a_state_is_written_once_reminded_per_window_and_cleared() {
    let _guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_now(4_000_000_000);
    let journal = journal();
    let damage = |journal: &SystemJournal| {
        journal.emit(
            &catalogue::DAMAGE_FOUND,
            Event::new("damage")
                .field("found_by", "verify")
                .count("problems", 1)
                .field("first_path", "wal/1.wal"),
        )
    };
    damage(&journal);
    damage(&journal);
    for _ in 0..6 {
        journal.emit(&catalogue::STORE_OPENED, Event::new("opened"));
    }
    // Still the same state: no new record, also after the original left the last five.
    damage(&journal);
    assert_eq!(
        kinds(&journal)
            .iter()
            .filter(|k| k.starts_with("store.damage"))
            .count(),
        1
    );
    set_now(4_000_000_000 + collapse::COLLAPSE_WINDOW_US);
    damage(&journal);
    let page = journal.query(&JournalQuery::new()).unwrap();
    let reminder = page.records.last().unwrap();
    assert_eq!(reminder.kind, "journal.state_reminder");
    assert_eq!(reminder.level, Level::Error);
    assert_eq!(
        reminder.field("state"),
        Some(&ElementProperty::from("store.damage_found"))
    );
    assert_eq!(reminder.field("count"), Some(&ElementProperty::from(3i64)));
    journal.emit(
        &catalogue::DAMAGE_CLEARED,
        Event::new("cleared").field("first_path", "wal/1.wal"),
    );
    damage(&journal);
    let kinds = kinds(&journal);
    assert_eq!(
        &kinds[kinds.len() - 2..],
        ["store.damage_cleared", "store.damage_found"]
    );
}

#[test]
fn a_clock_step_back_is_one_record_not_one_per_reading() {
    let _guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_now(5_000_000_000);
    let clock = CommitClock::with_source(fake_now);
    let journal = SystemJournal::in_memory(clock.clone());
    assert_eq!(clock.now(), 5_000_000_000);
    set_now(4_000_000_000);
    for _ in 0..100 {
        assert_eq!(clock.now(), 5_000_000_000);
    }
    assert_eq!(kinds(&journal), ["clock.step_back"]);
    let record = &journal.query(&JournalQuery::new()).unwrap().records[0];
    assert_eq!(record.time, 5_000_000_000);
    assert_eq!(
        record.field("behind_ms"),
        Some(&ElementProperty::from(1_000_000i64))
    );
}

struct FailingWriter {
    fail: Arc<std::sync::atomic::AtomicBool>,
    written: Arc<Mutex<Vec<String>>>,
}

impl JournalWriter for FailingWriter {
    fn append(&mut self, record: &JournalRecord, _urgent: bool) -> Result<(), WriteFailure> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(WriteFailure {
                reason: "disk full".into(),
                lost: 1,
            });
        }
        self.written.lock().unwrap().push(record.kind.clone());
        Ok(())
    }

    fn sync(&mut self) -> Result<(), WriteFailure> {
        Ok(())
    }
}

#[test]
fn a_failed_write_is_counted_and_reported_by_the_next_record() {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let written = Arc::new(Mutex::new(Vec::new()));
    let journal = SystemJournal::build(
        InstanceId::generate(Some("test")),
        CommitClock::new(),
        1,
        Vec::new(),
        Some(Box::new(FailingWriter {
            fail: fail.clone(),
            written: written.clone(),
        })),
        None,
    );
    journal.emit(&catalogue::STORE_OPENED, Event::new("a"));
    journal.emit(&catalogue::STORE_OPENED, Event::new("b"));
    assert_eq!(journal.lost_records(), 2);
    fail.store(false, Ordering::SeqCst);
    journal.emit(&catalogue::STORE_OPENED, Event::new("c"));
    assert_eq!(
        *written.lock().unwrap(),
        ["store.opened", "journal.records_lost"]
    );
    let page = journal.query(&JournalQuery::new()).unwrap();
    assert_eq!(page.records.len(), 4);
    assert_eq!(
        page.records[3].field("count"),
        Some(&ElementProperty::from(2i64))
    );
}

/// A `tracing` subscriber recording the target and level of every event (installed for one
/// thread with `with_default`, never globally).
struct Capture(Arc<Mutex<Vec<(String, tracing::Level)>>>);

impl tracing::Subscriber for Capture {
    // Asked at every event: other threads' subscribers must not decide for this one.
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        self.0
            .lock()
            .unwrap()
            .push((meta.target().to_string(), *meta.level()));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Every written record goes to `tracing` at its level, a collapsed one never.
///
/// Deterministic under parallel tests: what `mirror` hands to `tracing` is read from its
/// thread-local log (exact records and levels), and the subscriber check rebuilds `tracing`'s
/// process-wide callsite interest after installing the thread's subscriber (another thread may
/// have registered the mirror's callsite while only the global no-op subscriber existed, which
/// caches "never"), retrying a bounded number of times for a registration racing that rebuild.
#[test]
fn every_written_record_is_mirrored_to_tracing_at_its_level() {
    MIRRORED.with(|mirrored| mirrored.borrow_mut().clear());
    let journal = SystemJournal::in_memory(CommitClock::new());
    journal.emit(
        &catalogue::CHECKPOINT_FAILED,
        Event::new("failed")
            .field("error", "x")
            .field("automatic", false),
    );
    journal.emit(&catalogue::STORE_OPENED, Event::new("opened"));
    // Collapsed: not mirrored.
    journal.emit(
        &catalogue::CHECKPOINT_FAILED,
        Event::new("failed")
            .field("error", "x")
            .field("automatic", false),
    );
    let written = journal.query(&JournalQuery::new()).unwrap().records;
    assert_eq!(written.len(), 2, "the repeat is collapsed: {written:?}");
    let mirrored = MIRRORED.with(|mirrored| mirrored.borrow().clone());
    assert_eq!(
        mirrored,
        [
            (written[0].seq, tracing::Level::ERROR),
            (written[1].seq, tracing::Level::INFO)
        ],
        "each written record once, at its level; the collapsed one never"
    );
    assert_eq!(
        Level::ALL.map(tracing_level),
        [
            tracing::Level::ERROR,
            tracing::Level::WARN,
            tracing::Level::INFO,
            tracing::Level::INFO
        ]
    );

    // And `tracing` really receives them: a thread-local subscriber sees the error record.
    let seen = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::with_default(Capture(seen.clone()), || {
        for _ in 0..20 {
            tracing::callsite::rebuild_interest_cache();
            let journal = SystemJournal::in_memory(CommitClock::new());
            journal.emit(
                &catalogue::CHECKPOINT_FAILED,
                Event::new("failed")
                    .field("error", "x")
                    .field("automatic", false),
            );
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|(target, _)| target == "graphersal::journal")
            {
                break;
            }
        }
    });
    let seen: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(target, _)| target == "graphersal::journal")
        .map(|(_, level)| *level)
        .collect();
    assert_eq!(seen.first(), Some(&tracing::Level::ERROR), "{seen:?}");
}

#[test]
fn every_catalogue_kind_is_described_and_unique() {
    let mut codes = std::collections::HashSet::new();
    for kind in catalogue::EVENT_KINDS {
        assert!(codes.insert(kind.code), "duplicate kind {}", kind.code);
        assert!(!kind.description.is_empty(), "{}", kind.code);
        assert!(kind.code.contains('.'), "{}", kind.code);
        assert!(
            kind.code
                .starts_with(kind.source.split('.').next().unwrap()),
            "{}",
            kind.code
        );
        for field in kind.fields {
            assert!(
                !field.description.is_empty(),
                "{}.{}",
                kind.code,
                field.name
            );
        }
        match &kind.form {
            catalogue::Form::State { cleared, .. } => {
                assert!(catalogue::kind(cleared).is_some(), "{cleared}")
            }
            catalogue::Form::Cleared { state } => {
                assert!(catalogue::kind(state).is_some(), "{state}")
            }
            catalogue::Form::Event(catalogue::Collapse::By(fields)) => {
                for name in *fields {
                    assert!(kind.fields.iter().any(|field| field.name == *name));
                }
            }
            catalogue::Form::Event(catalogue::Collapse::Never) => {}
        }
    }
}

#[test]
fn the_in_memory_journal_keeps_the_newest_records() {
    let journal = SystemJournal::in_memory(CommitClock::new());
    for i in 0..(MEMORY_CAPACITY + 5) {
        journal.emit(&catalogue::STORE_OPENED, Event::new(format!("{i}")));
    }
    let page = journal
        .query(&JournalQuery::new().limit(MAX_QUERY_LIMIT))
        .unwrap();
    assert!(page.truncated);
    assert_eq!(page.records.len(), MEMORY_CAPACITY);
    assert_eq!(page.records[0].seq, 6);
    let later = journal
        .query(&JournalQuery::new().after(MEMORY_CAPACITY as u64))
        .unwrap();
    assert!(!later.truncated);
    assert_eq!(later.records.len(), 5);
}

#[test]
fn the_book_shows_the_event_catalogue() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../book/src/persistence/system_journal.md"
    );
    let Ok(page) = std::fs::read_to_string(path) else {
        // The book is not part of the published crate.
        return;
    };
    let (begin, end) = (
        "<!-- journal-events:begin -->\n",
        "<!-- journal-events:end -->",
    );
    let start = page.find(begin).expect("the begin marker") + begin.len();
    let stop = page.find(end).expect("the end marker");
    let generated = catalogue::catalogue_markdown();
    if page[start..stop] == generated {
        return;
    }
    if std::env::var_os("GRAPHERSAL_UPDATE_BOOK").is_some() {
        let updated = format!("{}{}{}", &page[..start], generated, &page[stop..]);
        std::fs::write(path, updated).expect("writing the book page");
        return;
    }
    panic!(
        "the event catalogue of book/src/persistence/system_journal.md is stale: run \
         GRAPHERSAL_UPDATE_BOOK=1 cargo test -p graphersal --lib catalogue"
    );
}

#[test]
fn a_newest_first_query_pages_backwards_and_follows_forwards() {
    let journal = SystemJournal::in_memory(CommitClock::new());
    for i in 0..25 {
        journal.emit(&catalogue::STORE_OPENED, Event::new(format!("{i}")));
    }
    let seqs = |page: &JournalPage| page.records.iter().map(|r| r.seq).collect::<Vec<_>>();
    let newest = journal
        .query(&JournalQuery::new().newest_first().limit(10))
        .unwrap();
    assert_eq!(seqs(&newest), (16..=25).rev().collect::<Vec<_>>());
    assert!(newest.more);
    assert_eq!(newest.next_after, 25, "a reader follows from the newest");
    assert_eq!(newest.next_before, Some(16));
    let older = journal
        .query(&JournalQuery::new().newest_first().before(16).limit(10))
        .unwrap();
    assert_eq!(seqs(&older), (6..=15).rev().collect::<Vec<_>>());
    let oldest = journal
        .query(&JournalQuery::new().newest_first().before(6).limit(10))
        .unwrap();
    assert_eq!(seqs(&oldest), [5, 4, 3, 2, 1]);
    assert!(!oldest.more);
    assert_eq!(oldest.next_before, Some(1));
    // `after` bounds it from below; an oldest-first page has no `next_before`.
    let window = journal
        .query(&JournalQuery::new().newest_first().after(20).limit(100))
        .unwrap();
    assert_eq!(seqs(&window), [25, 24, 23, 22, 21]);
    assert!(!window.more);
    let forward = journal.query(&JournalQuery::new().after(20)).unwrap();
    assert_eq!(forward.next_before, None);
    assert!(!forward.to_json().contains("next_before"));
    assert!(newest.to_json().contains("\"next_before\":16"));
    // `before` also bounds an oldest-first query.
    let bounded = journal
        .query(&JournalQuery::new().before(4).limit(100))
        .unwrap();
    assert_eq!(seqs(&bounded), [1, 2, 3]);
    assert_eq!(bounded.next_after, 3);
    // Filters apply before the limit.
    let none = journal
        .query(&JournalQuery::new().newest_first().min_level(Level::Error))
        .unwrap();
    assert!(none.records.is_empty() && !none.more);
}

#[test]
fn a_newest_first_query_of_a_full_ring_says_older_records_are_gone() {
    let journal = SystemJournal::in_memory(CommitClock::new());
    for i in 0..(MEMORY_CAPACITY + 5) {
        journal.emit(&catalogue::STORE_OPENED, Event::new(format!("{i}")));
    }
    let newest = journal
        .query(&JournalQuery::new().newest_first().limit(3))
        .unwrap();
    assert_eq!(newest.records[0].seq, (MEMORY_CAPACITY + 5) as u64);
    assert!(!newest.truncated, "a full page needs nothing older");
    let past = journal
        .query(&JournalQuery::new().newest_first().before(7).limit(10))
        .unwrap();
    assert_eq!(past.records.len(), 1, "only seq 6 is still held");
    assert!(past.truncated);
}

/// `to_line` keeps one header line per record; a field of several lines (a `recommendation` in
/// the note layout) follows below it, indented, and the one-line fields stay in parentheses.
#[test]
fn a_multi_line_field_follows_the_record_line_indented() {
    let journal = SystemJournal::in_memory(CommitClock::new());
    let recommendation = crate::graph::note_text::NoteText::new("bring the model back")
        .command(
            "download the model's files again",
            "graphersal embed-model download acme/tiny",
        )
        .finish();
    journal.emit(
        &catalogue::MODEL_UNAVAILABLE,
        Event::new("the model acme/tiny is missing: queries with it fail")
            .field("repo", "acme/tiny")
            .field("identity", "model2vec/acme/tiny@0123456789ab")
            .field("cause", "missing")
            .field("recommendation", recommendation),
    );
    let records = journal.query(&JournalQuery::new()).unwrap().records;
    let line = records[0].to_line();
    let lines: Vec<&str> = line.lines().collect();
    assert!(
        lines[0].ends_with(
            "model.unavailable the model acme/tiny is missing: queries with it fail \
             (repo=acme/tiny, identity=model2vec/acme/tiny@0123456789ab, cause=missing)"
        ),
        "{line}"
    );
    assert_eq!(
        lines[1..],
        [
            "  recommendation: bring the model back",
            "    # download the model's files again",
            "    graphersal embed-model download acme/tiny",
        ],
        "{line}"
    );
    // A record without a field of several lines stays one line.
    journal.emit(&catalogue::STORE_OPENED, Event::new("opened"));
    let records = journal.query(&JournalQuery::new()).unwrap().records;
    assert!(!records[1].to_line().contains('\n'));
}
