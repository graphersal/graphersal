//! `defaultFunction` `now` and `today` (book page "Schema Enforcement", "Defaults on create"):
//! the `datetime` of the unit's start and its UTC `date`, one value for every element created in
//! one unit; in a script the evaluation's `now()` (pinnable by the host); in Rust the unit's
//! start or `GraphTraversalSource::with_now`. Stored as plain values: change sets, replay and
//! imports carry them and never compute them again.
#![cfg(all(feature = "script", feature = "io"))]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::io::GraphImport;
use graphersal::prelude::*;
use graphersal::prelude::{Graph, PropertyMap};
use graphersal::script::rhai::Dynamic;
use graphersal::storage::{AnyGraph, CreateContext, GraphStorage, IntoAnyGraph, Transactional};
use graphersal::temporal::{Date, DateTime, Temporal};

use crate::forwarding::Forwarding;
use crate::script_host::{ScriptLimits, eval_value_with_limits};

/// `event` vertices stamped with `now` and `today`, `at` edges stamped with `now`.
const SCHEMA: &str = r#"{
  "mode": "open",
  "vertices": {
    "event": {"schema": {"type": "object", "properties": {
      "name": {"type": "string"},
      "at": {"type": "string", "format": "date-time", "defaultFunction": "now"},
      "day": {"type": "string", "format": "date", "defaultFunction": "today"}
    }}}
  },
  "edges": {
    "next": {"schema": {"type": "object", "properties": {
      "at": {"type": "string", "format": "date-time", "defaultFunction": "now"}
    }}}
  }
}"#;

fn schema(json: &str) -> GraphSchema {
    GraphSchema::from_json(json).unwrap_or_else(|err| panic!("{}", format_diagnostic(&err)))
}

/// An empty shared graph of storage `S` with `schema` set through the front end.
fn graph_of<S: GraphStorage + Default + Send + Sync + 'static>(storage: S) -> Arc<Graph<S>> {
    let graph = Arc::new(Graph::new(storage));
    AnyGraph::set_schema(&*graph, schema(SCHEMA)).expect("the schema applies");
    graph
}

fn graph() -> Arc<Graph<TraversalGraph>> {
    graph_of(TraversalGraph::new())
}

fn tg() -> TraversalGraph {
    let mut tg = TraversalGraph::new();
    tg.traversal_mut().set_schema(schema(SCHEMA)).unwrap();
    tg
}

fn dt(text: &str) -> DateTime {
    text.parse().unwrap()
}

/// Runs a script on `graph`; its value.
fn run(graph: impl IntoAnyGraph, script: &str) -> Dynamic {
    eval_value_with_limits(graph, script, HashMap::new(), &ScriptLimits::default())
        .unwrap_or_else(|err| panic!("`{script}`: {err}"))
}

/// The `at`/`day` values of every element of `kind` ("vertex"/"edge") with them, in order.
fn stamps<S: GraphStorage>(g: &S, edges: bool) -> Vec<(Option<DateTime>, Option<Date>)> {
    let maps: Vec<PropertyMap> = if edges {
        g.edges()
            .unwrap()
            .filter_map(|e| g.edge_properties(e).unwrap().cloned())
            .collect()
    } else {
        g.vertices()
            .unwrap()
            .filter_map(|v| g.vertex_properties(v).unwrap().cloned())
            .collect()
    };
    maps.iter()
        .map(|props| (datetime(props.get("at")), date(props.get("day"))))
        .collect()
}

fn datetime(value: Option<&ElementProperty>) -> Option<DateTime> {
    match value {
        Some(ElementProperty::Temporal(Temporal::DateTime(at))) => Some(*at),
        None => None,
        other => panic!("expected a datetime, found {other:?}"),
    }
}

fn date(value: Option<&ElementProperty>) -> Option<Date> {
    match value {
        Some(ElementProperty::Temporal(Temporal::Date(day))) => Some(*day),
        None => None,
        other => panic!("expected a date, found {other:?}"),
    }
}

/// The UTC date of `at` (the DSL's `today()`).
fn utc_date(at: DateTime) -> Date {
    at.with_offset(0).unwrap().local().date()
}

/// The single time every vertex carries (and checks `day` is its UTC date).
fn the_one_time<S: GraphStorage>(g: &S) -> DateTime {
    let all = stamps(g, false);
    assert!(!all.is_empty());
    let (first, _) = all[0];
    let first = first.expect("an `at`");
    for (at, day) in &all {
        assert_eq!(*at, Some(first), "one time for every element of the unit");
        assert_eq!(*day, Some(utc_date(first)));
    }
    first
}

#[test]
fn a_hundred_vertices_of_one_traversal_get_the_same_now() {
    let mut tg = tg();
    let seeds: Vec<i64> = (0..100).collect();
    tg.traversal_mut()
        .inject(seeds)
        .add_v("event")
        .iterate()
        .unwrap();
    assert_eq!(tg.vertex_count(), 100);
    the_one_time(&tg);
}

#[test]
fn a_hundred_vertices_of_one_script_get_its_now() {
    let g = graph();
    let t = run(
        g.clone(),
        "let t = now(); let xs = []; for i in 0..100 { xs.push(i) } \
         g.inject(xs).unfold().addV(\"event\").iterate(); t",
    );
    let t: DateTime = t.try_cast().expect("now() is a datetime");
    let read = g.read();
    assert_eq!(read.vertex_count(), 100);
    assert_eq!(the_one_time(&*read), t, "the evaluation's now()");
}

#[test]
fn every_creating_step_of_a_script_and_its_child_traversals_share_the_time() {
    let g = graph();
    let script = r#"
        let t = now();
        g.addV("event").property("name", "a").iterate();
        g.V().has("name", "a").coalesce(__.out("next"), __.addV("event").property("name", "b"))
            .iterate();
        g.mergeV(#{"T.label": "event", name: "c"}).iterate();
        g.V().has("name", "a").addE("next").to(__.V().has("name", "b")).iterate();
        g.mergeE(#{"T.label": "next", "Direction.OUT": g.V().has("name", "b").id().next(),
                   "Direction.IN": g.V().has("name", "c").id().next()}).iterate();
        t
    "#;
    let t: DateTime = run(g.clone(), script).try_cast().unwrap();
    let read = g.read();
    assert_eq!(read.vertex_count(), 3);
    assert_eq!(the_one_time(&*read), t);
    let edges = stamps(&*read, true);
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().all(|(at, _)| *at == Some(t)), "{edges:?}");
}

#[test]
fn two_units_get_non_decreasing_times() {
    let mut tg = tg();
    let mut previous: Option<DateTime> = None;
    for round in 0..20 {
        tg.traversal_mut()
            .add_v("event")
            .property("name", round.to_string())
            .iterate()
            .unwrap();
        let at = tg
            .get_vertex_by_id(
                &tg.vertices()
                    .unwrap()
                    .last()
                    .map(|v| tg.vertex_id(v).unwrap().unwrap().to_string())
                    .unwrap(),
            )
            .unwrap()
            .and_then(|v| datetime(tg.vertex_properties(v).unwrap().unwrap().get("at")))
            .unwrap();
        if let Some(previous) = previous {
            assert!(at >= previous, "{at} after {previous}");
        }
        previous = Some(at);
    }
    // Two separate script evaluations too.
    let g = graph();
    let first: DateTime = run(g.clone(), "g.addV(\"event\").iterate(); now()")
        .try_cast()
        .unwrap();
    let second: DateTime = run(g.clone(), "g.addV(\"event\").iterate(); now()")
        .try_cast()
        .unwrap();
    assert!(second >= first);
    let read = g.read();
    let times: Vec<DateTime> = stamps(&*read, false)
        .iter()
        .filter_map(|(at, _)| *at)
        .collect();
    let mut sorted = times.clone();
    sorted.sort();
    assert_eq!(times.len(), 2);
    assert!(
        sorted.contains(&first) && sorted.contains(&second),
        "{times:?}"
    );
}

#[test]
fn an_explicit_transaction_is_one_unit_with_one_time() {
    let mut tg = tg();
    tg.transaction(|g| -> Result<(), GraphError> {
        g.traversal_mut()
            .add_v("event")
            .iterate()
            .map_err(|_| GraphError::unsupported("x"))?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        g.traversal_mut()
            .add_v("event")
            .iterate()
            .map_err(|_| GraphError::unsupported("x"))?;
        // The trait's create path reads the same unit start.
        let create = CreateContext::for_storage(&*g).unwrap();
        g.add_vertex_with(None, vec!["event".into()], PropertyMap::default(), &create)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(tg.vertex_count(), 3);
    the_one_time(&tg);
    // Outside a unit there is no unit start.
    assert_eq!(tg.unit_started_at().unwrap(), None);
}

#[test]
fn a_pinned_clock_is_stored_exactly() {
    // Rust: `with_now`.
    let mut tg = tg();
    let pinned = dt("2023-08-02T23:30:00-07:00");
    tg.traversal_mut()
        .with_now(pinned)
        .inject(vec![1i64, 2])
        .add_v("event")
        .iterate()
        .unwrap();
    let at = the_one_time(&tg);
    assert_eq!(
        at.to_string(),
        "2023-08-02T23:30:00-07:00",
        "kept with its offset"
    );
    // `today` is the UTC date: 2023-08-03 at UTC.
    let (_, day) = stamps(&tg, false)[0];
    assert_eq!(day.unwrap().to_string(), "2023-08-03");

    // Rust: a pinned `CreateContext` on the trait's create path.
    let mut tg = self::tg();
    let at = dt("2001-02-03T04:05:06Z");
    tg.transaction(|g| {
        g.add_vertex_with(
            None,
            vec!["event".into()],
            PropertyMap::default(),
            &CreateContext::at(at),
        )
        .map(|_| ())
    })
    .unwrap();
    assert_eq!(the_one_time(&tg), at);

    // A script host: `pin_now`, the same clock as the DSL's `now()`.
    let graph = graph();
    let mut engine = crate::script_host::engine_with_options(&Default::default(), &[]);
    graphersal::script::pin_now(&mut engine, Some(pinned));
    let mut scope = crate::script_host::graph_scope_with_options(graph.clone(), &[]);
    let returned: Dynamic = engine
        .eval_with_scope(
            &mut scope,
            r#"g.inject(1, 2, 3).addV("event").iterate(); g.addV("event").property("name", "later")"#,
        )
        .unwrap();
    {
        let read = graph.read();
        assert_eq!(the_one_time(&*read), pinned);
    }
    // A traversal returned by the script runs after the evaluation with the evaluation's time
    // (here, after the clock was unpinned again).
    graphersal::script::pin_now(&mut engine, None);
    let mut later: graphersal::script::ScriptGraphTraversalSource = returned.try_cast().unwrap();
    later.iterate().unwrap();
    let read = graph.read();
    assert_eq!(read.vertex_count(), 4);
    assert_eq!(the_one_time(&*read), pinned);
}

#[test]
fn today_is_the_utc_date_of_the_unit_start() {
    let mut tg = tg();
    tg.traversal_mut()
        .with_now(dt("2024-02-29T00:30:00+01:00"))
        .add_v("event")
        .iterate()
        .unwrap();
    let (_, day) = stamps(&tg, false)[0];
    assert_eq!(day.unwrap().to_string(), "2024-02-28");
    // Unpinned: the date of the stored `now`.
    let mut tg = self::tg();
    tg.traversal_mut().add_v("event").iterate().unwrap();
    the_one_time(&tg);
}

/// A schema with one `event` property `at` declared as `node`.
fn with_at(node: &str) -> String {
    format!(
        r#"{{"mode": "open", "vertices": {{"event": {{"schema": {{"type": "object",
            "properties": {{"at": {node}}}}}}}}}}}"#
    )
}

fn set(json: &str) -> Result<(), Box<TraverserError>> {
    TraversalGraph::new()
        .traversal_mut()
        .set_schema(schema(json))
}

#[test]
fn now_and_today_need_a_property_admitting_their_type() {
    for (node, needed) in [
        (
            r#"{"type": "string", "defaultFunction": "now"}"#,
            "date-time",
        ),
        (
            r#"{"type": "string", "format": "date", "defaultFunction": "now"}"#,
            "date-time",
        ),
        (
            r#"{"type": "integer", "defaultFunction": "now"}"#,
            "date-time",
        ),
        (
            r#"{"type": "string", "format": "local-date-time", "defaultFunction": "now"}"#,
            "date-time",
        ),
        (
            r#"{"type": "string", "defaultFunction": "today"}"#,
            "\"format\": \"date\"",
        ),
        (
            r#"{"type": "string", "format": "date-time", "defaultFunction": "today"}"#,
            "\"format\": \"date\"",
        ),
        (
            r#"{"type": "string", "format": "date-time", "defaultFunction": "uuidv4"}"#,
            "uuid",
        ),
    ] {
        let err = set(&with_at(node)).expect_err(node);
        let text = format_diagnostic(&err);
        assert!(
            text.contains("/vertices/event/schema/properties/at/defaultFunction"),
            "{text}"
        );
        assert!(text.contains("does not fit the property"), "{text}");
        assert!(text.contains(needed), "{node}: {text}");
    }
    // Admitting the type is enough: nullable, or one member of an anyOf.
    for node in [
        r#"{"type": "string", "format": "date-time", "defaultFunction": "now"}"#,
        r#"{"type": ["string", "null"], "format": "date-time", "defaultFunction": "now"}"#,
        r#"{"anyOf": [{"type": "integer"}, {"type": "string", "format": "date-time"}], "defaultFunction": "now"}"#,
        r#"{"type": "string", "format": "date", "defaultFunction": "today"}"#,
    ] {
        set(&with_at(node)).unwrap_or_else(|err| panic!("{node}: {}", format_diagnostic(&err)));
    }
    // Nested, it is misplaced as for every function.
    let err = set(&with_at(
        r#"{"type": "object", "properties": {"t": {"type": "string", "format": "date-time", "defaultFunction": "now"}}}"#,
    ))
    .unwrap_err();
    assert!(format_diagnostic(&err).contains("only to a label's top-level"));
    // The DSL reports the same.
    let text = eval_value_with_limits(
        Arc::new(GraphSource::empty()),
        r#"g.set_schema(#{mode: "open", vertices: #{event: #{schema: #{type: "object",
            properties: #{at: #{type: "string", defaultFunction: "now"}}}}}})"#,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("date-time"), "{text}");
}

#[test]
fn the_schema_round_trips_now_and_today() {
    let parsed = schema(SCHEMA);
    let again = schema(&parsed.to_json());
    assert_eq!(parsed, again);
    let json = parsed.to_json();
    assert!(json.contains("today") && json.contains("now"), "{json}");
}

thread_local! {
    static RECORDED: RefCell<Vec<ChangeSet>> = const { RefCell::new(Vec::new()) };
}

struct Recorder;

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        RECORDED.with(|recorded| recorded.borrow_mut().push(changes.clone()));
        Ok(())
    }
}

#[test]
fn the_change_set_carries_the_time_and_replay_and_import_never_compute_it() {
    let mut tg = TraversalGraph::new();
    tg.register_commit_hook(Box::new(Recorder)).unwrap();
    tg.traversal_mut().set_schema(schema(SCHEMA)).unwrap();
    let pinned = dt("2020-05-06T07:08:09Z");
    tg.traversal_mut()
        .with_now(pinned)
        .add_v("event")
        .iterate()
        .unwrap();
    let e1 = tg
        .vertices()
        .unwrap()
        .next()
        .and_then(|v| tg.vertex_id(v).unwrap().map(str::to_string))
        .unwrap();
    let recorded = RECORDED.with(|r| std::mem::take(&mut *r.borrow_mut()));
    assert_eq!(recorded.len(), 2, "set_schema, then add_v");
    let added = recorded[1]
        .mutations()
        .iter()
        .find_map(|m| match m {
            Mutation::AddVertex { properties, .. } => Some(properties.clone()),
            _ => None,
        })
        .expect("an AddVertex");
    assert_eq!(datetime(added.get("at")), Some(pinned));
    assert_eq!(date(added.get("day")).unwrap().to_string(), "2020-05-06");

    // Replay on a later day reproduces the recorded values.
    let mut replayed = TraversalGraph::new();
    for changes in &recorded {
        replayed = replayed.replay(changes).unwrap();
    }
    let props = replayed
        .get_vertex_by_id(&e1)
        .unwrap()
        .and_then(|v| replayed.vertex_properties(v).unwrap().cloned())
        .unwrap();
    assert_eq!(props, added);

    // A change set whose vertex lacks the properties stays without them.
    let mut target = self::tg();
    let mut builder = ChangeSet::builder(1, 0);
    builder.push(Mutation::add_vertex("x", ["event"], PropertyMap::default()));
    target.apply_change_set(&builder.build().unwrap()).unwrap();
    let props = target
        .get_vertex_by_id("x")
        .unwrap()
        .and_then(|v| target.vertex_properties(v).unwrap().cloned())
        .unwrap();
    assert!(
        !props.contains_key("at") && !props.contains_key("day"),
        "{props:?}"
    );

    // An import stores what it carries.
    let mut g = Graph::new(TraversalGraph::new());
    AnyGraph::set_schema(&g, schema(SCHEMA)).unwrap();
    g.import_graphson_reader(r#"{"id":"y","label":"event","properties":{}}"#.as_bytes())
        .unwrap();
    let read = g.read();
    assert_eq!(stamps(&*read, false), vec![(None, None)]);
}

#[test]
fn a_foreign_storage_gets_the_unit_time() {
    let g = graph_of(Forwarding::new(TraversalGraph::new()));
    let t: DateTime = run(
        g.clone(),
        "let t = now(); g.inject(1, 2, 3).addV(\"event\").iterate(); t",
    )
    .try_cast()
    .unwrap();
    {
        let read = g.read();
        assert_eq!(the_one_time(&*read), t);
    }
    // In Rust: the unit start the storage forwards, one time for the explicit transaction.
    let mut write = g.write();
    let storage: &mut Forwarding = &mut write;
    storage
        .transaction(|s| -> Result<(), GraphError> {
            assert!(
                s.unit_started_at().unwrap().is_some(),
                "Forwarding forwards the unit start"
            );
            for _ in 0..3 {
                graphersal::GraphTraversalSource::new(&mut *s)
                    .add_v("event")
                    .property("name", "rust")
                    .iterate()
                    .map_err(|_| GraphError::unsupported("x"))?;
            }
            Ok(())
        })
        .unwrap();
    let rust: Vec<DateTime> = storage
        .vertices()
        .unwrap()
        .filter(|v| {
            storage.vertex_properties(*v).unwrap().unwrap().get("name") == Some(&"rust".into())
        })
        .map(|v| datetime(storage.vertex_properties(v).unwrap().unwrap().get("at")).unwrap())
        .collect();
    assert_eq!(rust.len(), 3);
    assert!(rust.iter().all(|at| *at == rust[0]));
    assert!(rust[0] >= t);
}

#[test]
fn a_saved_query_parameter_rejects_default_function() {
    for function in ["now", "uuidv4"] {
        let script = format!(
            r#"g.define_query(#{{name: "q", body: "fn q(x) {{ g.V() }}",
                params: #{{x: #{{schema: #{{type: "string", format: "date-time",
                    defaultFunction: "{function}"}}}}}}}})"#
        );
        let text = eval_value_with_limits(
            Arc::new(GraphSource::empty()),
            &script,
            HashMap::new(),
            &ScriptLimits::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(text.contains("defaultFunction"), "{text}");
        assert!(text.contains("default beside its schema"), "{text}");
    }
    // Nested inside the parameter's schema too.
    let script = r#"g.define_query(#{name: "q", body: "fn q(x) { g.V() }",
        params: #{x: #{schema: #{type: "object", properties: #{id: #{type: "string",
            format: "uuid", defaultFunction: "uuidv7"}}}}}})"#;
    let text = eval_value_with_limits(
        Arc::new(GraphSource::empty()),
        script,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("/properties/id/defaultFunction"), "{text}");
}
