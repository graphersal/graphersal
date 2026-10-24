//! Temporal v2, stage 2b: the duration range of `java.time.Duration` / Neo4j (an i64 of seconds
//! plus the nanoseconds of one second, months and days i32), the fallible
//! `Duration::micros()`, text round trips at the edges, arithmetic that leaves the range as an
//! error (Rust, Rhai, GQL), and the clock in nanoseconds (`GraphStorage::unit_started_at`,
//! `CreateContext::for_storage`, `now()`).

use graphersal::prelude::*;
use graphersal::storage::{CreateContext, GraphStorage, Transactional};
use graphersal::temporal::{DT, DateTime, Duration, Temporal};

const NANOS_PER_SECOND: i128 = 1_000_000_000;

fn dur(text: &str) -> Duration {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn dt(text: &str) -> DateTime {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// The system clock in nanoseconds (the test's own reading, for bounds).
fn system_nanos() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

#[test]
fn the_time_part_spans_an_i64_of_seconds_plus_nanoseconds() {
    assert_eq!(Duration::MIN_NANOS, i64::MIN as i128 * NANOS_PER_SECOND);
    assert_eq!(
        Duration::MAX_NANOS,
        i64::MAX as i128 * NANOS_PER_SECOND + 999_999_999
    );
    assert_eq!(
        Duration::MAX_NANOS.to_string(),
        "9223372036854775807999999999"
    );
    let max = Duration::with_nanos(i32::MAX, i32::MAX, Duration::MAX_NANOS).unwrap();
    let min = Duration::with_nanos(i32::MIN, i32::MIN, Duration::MIN_NANOS).unwrap();
    assert_eq!(max.nanos(), Duration::MAX_NANOS);
    assert_eq!(min.nanos(), Duration::MIN_NANOS);
    assert!(Duration::with_nanos(0, 0, Duration::MAX_NANOS + 1).is_err());
    assert!(Duration::with_nanos(0, 0, Duration::MIN_NANOS - 1).is_err());
    assert!(Duration::from_nanos(Duration::MAX_NANOS + 1).is_err());
    assert!(Duration::from_nanos(i128::MAX).is_err());
    assert!(Duration::from_nanos(i128::MIN).is_err());
    // Every i64 of microseconds is in range: `new` stays infallible.
    assert_eq!(Duration::new(0, 0, i64::MAX).micros(), Some(i64::MAX));
    assert_eq!(Duration::new(0, 0, i64::MIN).micros(), Some(i64::MIN));
    assert_eq!(
        Duration::from_micros(i64::MIN).nanos(),
        i64::MIN as i128 * 1000
    );
    // Ordering and equality over the whole range.
    assert!(min < Duration::default() && Duration::default() < max);
    assert!(
        Duration::from_nanos(Duration::MAX_NANOS - 1).unwrap()
            < Duration::from_nanos(Duration::MAX_NANOS).unwrap()
    );
}

#[test]
fn micros_is_none_beyond_an_i64_and_truncates_toward_zero_below_it() {
    let edge = Duration::from_nanos(i64::MAX as i128 * 1000 + 999).unwrap();
    assert_eq!(edge.micros(), Some(i64::MAX));
    let past = Duration::from_nanos(i64::MAX as i128 * 1000 + 1000).unwrap();
    assert_eq!(past.micros(), None);
    let low = Duration::from_nanos(i64::MIN as i128 * 1000 - 999).unwrap();
    assert_eq!(low.micros(), Some(i64::MIN));
    let below = Duration::from_nanos(i64::MIN as i128 * 1000 - 1000).unwrap();
    assert_eq!(below.micros(), None);
    assert_eq!(dur("PT1.000001999S").micros(), Some(1_000_001));
    assert_eq!(dur("-PT1.000001999S").micros(), Some(-1_000_001));
    assert_eq!(dur("PT9223372036854775807S").micros(), None);
    // The nanoseconds stay exact where the microseconds do not fit.
    assert_eq!(
        dur("PT9223372036854775807S").nanos(),
        i64::MAX as i128 * NANOS_PER_SECOND
    );
}

#[test]
fn text_at_the_edges_round_trips() {
    for (text, printed) in [
        ("PT9223372036854775807S", "PT2562047788015215H30M7S"),
        (
            "PT9223372036854775807.999999999S",
            "PT2562047788015215H30M7.999999999S",
        ),
        ("-PT9223372036854775808S", "-PT2562047788015215H30M8S"),
        (
            "PT2562047788015215H30M7.999999999S",
            "PT2562047788015215H30M7.999999999S",
        ),
        (
            "P-178956970Y-8M-2147483648DT-9223372036854775808S",
            "-P178956970Y8M2147483648DT2562047788015215H30M8S",
        ),
    ] {
        let value = dur(text);
        assert_eq!(value.to_string(), printed, "{text}");
        assert_eq!(dur(printed), value, "{printed}");
    }
    assert_eq!(
        dur("PT9223372036854775807.999999999S").nanos(),
        Duration::MAX_NANOS
    );
    assert_eq!(dur("-PT9223372036854775808S").nanos(), Duration::MIN_NANOS);
    for text in [
        "PT9223372036854775808S",
        "-PT9223372036854775808.000000001S",
        "PT2562047788015216H",
        "PT18446744073709551616S",
        "PT99999999999999999999999S",
    ] {
        assert!(text.parse::<Duration>().is_err(), "{text}");
    }
    // Through the strict reader of files and schemas as well.
    let max = Temporal::Duration(Duration::from_nanos(Duration::MAX_NANOS).unwrap());
    assert_eq!(
        Temporal::parse_strict(max.kind(), &max.to_string()).unwrap(),
        max
    );
}

#[test]
fn arithmetic_leaving_the_range_is_an_error() {
    let max = Duration::from_nanos(Duration::MAX_NANOS).unwrap();
    let min = Duration::from_nanos(Duration::MIN_NANOS).unwrap();
    let one = Duration::from_nanos(1).unwrap();
    assert!(max.checked_add(one).is_err());
    assert!(min.checked_add(one.checked_neg().unwrap()).is_err());
    assert!(min.checked_neg().is_err());
    assert_eq!(max.checked_neg().unwrap().nanos(), -Duration::MAX_NANOS);
    assert_eq!(max.checked_add(min).unwrap().nanos(), -1);
    // Beyond what the old range (an i64 of microseconds) held, below the new bound.
    let long = dur("PT9223372036854775S").checked_add(dur("PT9223372036854775S"));
    assert_eq!(
        long.unwrap().nanos(),
        2 * 9_223_372_036_854_775 * NANOS_PER_SECOND
    );
    // Steps over a datetime: beyond ±9999 years is an error, never a panic or a wrap.
    let graph = TraversalGraph::new();
    let at = Temporal::DateTime(dt("2020-01-01T00:00:00Z"));
    for duration in [max, min, dur("PT9223372036854775S")] {
        let result = graph
            .traversal()
            .inject(vec![ElementProperty::Temporal(at)])
            .date_add_duration(duration)
            .to_list();
        assert!(result.is_err(), "{duration}");
    }
    // A duration's own steps at the edge.
    let edge = ElementProperty::Temporal(Temporal::Duration(max));
    let added = graph
        .traversal()
        .inject(vec![edge.clone()])
        .date_add(DT::Second, 1)
        .to_list();
    assert!(added.is_err());
    let truncated = graph
        .traversal()
        .inject(vec![edge.clone()])
        .date_trunc(DT::Hour)
        .to_list()
        .unwrap();
    assert_eq!(
        truncated,
        vec![ElementProperty::Temporal(Temporal::Duration(dur(
            "PT2562047788015215H"
        )))]
    );
}

#[cfg(feature = "script")]
mod script {
    use std::sync::Arc;

    use graphersal::prelude::*;

    use crate::script_host::{debug_value, eval_value};

    fn run(script: &str) -> String {
        match eval_value(Arc::new(GraphSource::empty()), script) {
            Ok(value) => debug_value(&value),
            Err(err) => panic!("{script}: {err}"),
        }
    }

    fn fails(script: &str) -> String {
        match eval_value(Arc::new(GraphSource::empty()), script) {
            Ok(value) => panic!("{script} must fail, got {}", debug_value(&value)),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn rhai_reads_and_refuses_at_the_edges() {
        assert_eq!(
            run(r#"duration("PT9223372036854775807.999999999S")"#),
            r#"duration("PT2562047788015215H30M7.999999999S")"#
        );
        assert_eq!(
            run(r#"duration("PT9223372036854775807S").as_string()"#),
            r#""PT2562047788015215H30M7S""#
        );
        assert_eq!(
            run(r#"duration("PT9223372036854775.807S").as_string()"#),
            r#""PT2562047788015H12M55.807S""#
        );
        // The getters hold an integer: beyond it a range error, never a cut value.
        assert!(fails(r#"duration("PT9223372036854775807S").micros"#).contains("range"));
        assert!(fails(r#"duration("PT9223372036854775807S").nanos"#).contains("range"));
        assert_eq!(
            run(r#"duration("PT9223372036854.775807S").micros"#),
            "9223372036854775807"
        );
        assert!(fails(r#"duration("PT9223372036854.775808S").micros"#).contains("range"));
        // Arithmetic.
        assert!(
            fails(r#"duration("PT9223372036854775807S") + duration("PT1S")"#).contains("range")
        );
        assert!(
            fails(r#"datetime("2020-01-01T00:00Z") + duration("PT9223372036854775807S")"#)
                .contains("range")
        );
        assert_eq!(
            run(r#"g.inject(duration("PT9223372036854775807S")).date_part(DT.hour).next()"#),
            "2562047788015215"
        );
        assert!(fails(r#"duration("PT9223372036854775808S")"#).contains("too large"));
    }

    /// `now()` of an unpinned engine is the system clock at the evaluation's start, with the
    /// digits the platform gives (nanoseconds at most).
    #[test]
    fn now_reads_the_system_clock_in_nanoseconds() {
        let before = super::system_nanos();
        let value = eval_value(Arc::new(GraphSource::empty()), "now()").unwrap();
        let after = super::system_nanos();
        let now = value
            .try_cast::<graphersal::temporal::DateTime>()
            .expect("a datetime");
        // The test's clock and the engine's are the same system clock; allow its resolution.
        assert!(
            now.epoch_nanos() >= before - 1_000_000 && now.epoch_nanos() <= after + 1_000_000,
            "{before} <= {} <= {after}",
            now.epoch_nanos()
        );
    }
}

#[cfg(feature = "gql")]
mod gql {
    use graphersal::prelude::*;
    use graphersal::temporal::Temporal;

    fn run(query: &str) -> Result<Vec<ElementProperty>, Box<TraverserError>> {
        TraversalGraph::new().traversal().gql(query).to_list()
    }

    fn single(query: &str) -> ElementProperty {
        let rows = run(query).unwrap_or_else(|e| panic!("{query}: {e}"));
        match rows.as_slice() {
            [ElementProperty::Map(map)] => map.values().next().cloned(),
            [ElementProperty::Object(map)] => map.values().next().cloned(),
            _ => panic!("{query}: {rows:?}"),
        }
        .unwrap_or(ElementProperty::Null)
    }

    fn text(query: &str) -> String {
        match single(query) {
            ElementProperty::Temporal(Temporal::Duration(d)) => d.to_string(),
            ElementProperty::String(s) => s.to_string(),
            other => format!("{other:?}"),
        }
    }

    fn status(query: &str) -> String {
        match run(query) {
            Ok(rows) => panic!("{query} gave {rows:?}"),
            Err(error) => match error.root_cause() {
                TraverserError::Gql { gqlstatus, .. } => gqlstatus.clone(),
                other => panic!("{query}: not a GQL error: {other}"),
            },
        }
    }

    #[test]
    fn gql_durations_reach_an_i64_of_seconds() {
        for (query, expected) in [
            (
                "RETURN duration({seconds: 9223372036854775807}) AS x",
                "PT2562047788015215H30M7S",
            ),
            (
                "RETURN duration({seconds: 9223372036854775807, nanoseconds: 999999999}) AS x",
                "PT2562047788015215H30M7.999999999S",
            ),
            (
                "RETURN duration({hours: 2562047788015215}) AS x",
                "PT2562047788015215H",
            ),
            (
                "RETURN duration('PT9223372036854775807S') AS x",
                "PT2562047788015215H30M7S",
            ),
            (
                "RETURN duration({seconds: 9223372036854775807}) * 0.5 AS x",
                "PT1281023894007607H45M3.500S",
            ),
            (
                "RETURN duration({seconds: 9223372036854775807}) - duration('PT7S') AS x",
                "PT2562047788015215H30M",
            ),
            (
                "RETURN toString(duration('PT-9223372036854775808S')) AS x",
                "PT-2562047788015215H-30M-8S",
            ),
        ] {
            assert_eq!(text(query), expected, "{query}");
        }
        assert_eq!(
            single("RETURN duration({seconds: 9223372036854775807}).seconds AS x"),
            ElementProperty::from(i64::MAX)
        );
    }

    #[test]
    fn gql_leaving_the_range_is_a_data_error() {
        for query in [
            "RETURN duration({seconds: 9223372036854775807}) + duration('PT1S') AS x",
            "RETURN duration({seconds: 9223372036854775807}) * 2 AS x",
            "RETURN duration({hours: 2562047788015216}) AS x",
            "RETURN date('2020-01-01') + duration({seconds: 9223372036854775807}) AS x",
            "RETURN datetime('2020-01-01T00:00Z') + duration({seconds: 9223372036854775807}) AS x",
        ] {
            assert_eq!(status(query), "22008", "{query}");
        }
        // A total that does not fit an integer.
        assert_eq!(
            status("RETURN duration({seconds: 9223372036854775807}).nanoseconds AS x"),
            "22003"
        );
    }
}

/// The unit clock: `unit_started_at` in nanoseconds, the same instant for `CreateContext`, never
/// earlier than the previous unit's start.
#[test]
fn the_unit_clock_is_in_nanoseconds() {
    let mut graph = TraversalGraph::new();
    assert_eq!(graph.unit_started_at(), None);
    let before = system_nanos();
    let (first, context) = graph
        .transaction(|g| {
            let started = g.unit_started_at().expect("inside a unit");
            Ok::<_, GraphError>((started, CreateContext::for_storage(&*g)))
        })
        .unwrap();
    let after = system_nanos();
    assert!(
        first >= before - 1_000_000 && first <= after + 1_000_000,
        "{before} <= {first} <= {after}"
    );
    assert_eq!(context.now().epoch_nanos(), first);
    assert_eq!(context.now().offset_seconds(), 0);
    let second = graph
        .transaction(|g| Ok::<_, GraphError>(g.unit_started_at().expect("inside a unit")))
        .unwrap();
    assert!(second >= first);
    assert_eq!(graph.unit_started_at(), None);
    // Outside a unit a create reads the system clock.
    let outside = CreateContext::for_storage(&graph).now().epoch_nanos();
    assert!(outside >= before - 1_000_000);
}

#[cfg(feature = "persist")]
mod persist {
    use graphersal::persist::{MemDir, Store, StoreOptions};
    use graphersal::prelude::*;
    use graphersal::temporal::{Duration, Temporal};

    fn add(store: &Store, id: &str, value: Duration) -> Result<(), GraphError> {
        let mut map = PropertyMap::default();
        map.insert("d", ElementProperty::Temporal(Temporal::Duration(value)));
        store.graph().write().transaction(|g| {
            g.add_vertex(Some(id.into()), vec!["e".into()], map)?;
            Ok::<_, GraphError>(())
        })
    }

    /// File format version 3 holds a duration's whole time part (`svar` seconds and the
    /// nanoseconds of one second): the extremes of the range are stored and read back (stage
    /// 2b refused one beyond an i64 of microseconds under version 2).
    #[test]
    fn a_store_keeps_a_duration_beyond_an_i64_of_microseconds() {
        let dir = MemDir::new();
        let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
        let edge = Duration::new(0, 0, i64::MAX);
        let long = Duration::from_nanos(i64::MAX as i128 * 1000 + 1000).unwrap();
        let max = Duration::with_nanos(i32::MAX, i32::MIN, Duration::MAX_NANOS).unwrap();
        let min = Duration::with_nanos(i32::MIN, i32::MAX, Duration::MIN_NANOS).unwrap();
        for (id, value) in [("ok", edge), ("long", long), ("max", max), ("min", min)] {
            add(&store, id, value).unwrap();
        }
        let read = |store: &Store, id: &str| {
            let lock = store.graph().read();
            let v = lock.get_vertex_by_id(id).unwrap();
            match lock.vertex_properties(v).unwrap().get("d") {
                Some(ElementProperty::Temporal(Temporal::Duration(d))) => {
                    (d.months(), d.days(), d.nanos())
                }
                other => panic!("{other:?}"),
            }
        };
        let parts = |d: Duration| (d.months(), d.days(), d.nanos());
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        for (id, value) in [("ok", edge), ("long", long), ("max", max), ("min", min)] {
            assert_eq!(read(&store, id), parts(value), "{id}: WAL replay");
        }
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        for (id, value) in [("ok", edge), ("long", long), ("max", max), ("min", min)] {
            assert_eq!(read(&store, id), parts(value), "{id}: snapshot");
        }
    }
}
