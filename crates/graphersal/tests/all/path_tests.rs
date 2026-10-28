//! Integration tests for path tracking: `path()`, `simple_path()`/`cyclic_path()`, label steps
//! and the path requirement analysis that decides what each step records.

#[cfg(feature = "script")]
use std::sync::Arc;

use graphersal::prelude::*;

/// Names of the objects of a materialized path (`path().by("name")`).
fn strings(value: &ElementProperty) -> Vec<String> {
    match value {
        ElementProperty::Array(items) => items
            .iter()
            .map(|item| match item {
                ElementProperty::Number(n) => n.to_string(),
                other => other.as_string().unwrap_or("?").to_string(),
            })
            .collect(),
        other => panic!("expected a path, got {other:?}"),
    }
}

/// Ids of the objects of a materialized path of elements.
fn ids(value: &ElementProperty) -> Vec<String> {
    match value {
        ElementProperty::Array(items) => items
            .iter()
            .map(|item| match item {
                ElementProperty::Vertex(v) => v.id().to_string(),
                ElementProperty::Edge(e) => e.id().to_string(),
                other => format!("{other:?}"),
            })
            .collect(),
        other => panic!("expected a path, got {other:?}"),
    }
}

/// Runs a Rhai query that ends on a traversal and returns its materialized results.
#[cfg(feature = "script")]
fn rhai(graph: &Arc<Graph>, script: &str) -> Vec<ElementProperty> {
    let engine = crate::script_host::engine();
    let mut scope = crate::script_host::graph_scope(graph.clone());
    let gts = engine
        .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
        .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
    let lock = graph.read();
    gts.ast_convert_to_gts(&*lock)
        .to_list()
        .unwrap_or_else(|e| panic!("`{script}` failed to execute: {e}"))
}

/// Asserts that every Rhai spelling returns exactly the Rust result.
#[cfg(feature = "script")]
fn assert_rhai(graph: &Arc<Graph>, rust: &[ElementProperty], scripts: &[&str]) {
    for script in scripts {
        assert_eq!(
            rust,
            rhai(graph, script).as_slice(),
            "Rhai `{script}` differs from Rust"
        );
    }
}

/// a -> b -> c -> a
fn cycle_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let mut nodes = Vec::new();
    for id in ["a", "b", "c"] {
        let mut props = graphersal::prelude::PropertyMap::default();
        props.insert("name", ElementProperty::String(id.into()));
        nodes.push(
            graph
                .add_vertex(Some(id.into()), vec!["node".into()], props)
                .unwrap(),
        );
    }
    for (from, to) in [(0, 1), (1, 2), (2, 0)] {
        graph
            .add_edge(
                None,
                Some("next".into()),
                nodes[from],
                nodes[to],
                Default::default(),
            )
            .unwrap();
    }
    graph
}

// T1: `path()` shape.

#[test]
fn path_holds_every_object_including_the_current_one() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let paths = lock
        .traversal()
        .v("1")
        .out(None)
        .out(None)
        .path()
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 2);
    assert_eq!(ids(&paths[0]), vec!["1", "4", "5"]);
    assert_eq!(ids(&paths[1]), vec!["1", "4", "3"]);
    drop(lock);

    #[cfg(feature = "script")]
    assert_rhai(
        &Arc::new(graph),
        &paths,
        &[
            r#"g.V("1").out().out().path()"#,
            r#"g.v("1").out().out().path()"#,
        ],
    );
}

#[test]
fn path_by_name() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let paths = lock
        .traversal()
        .v("1")
        .out(None)
        .out(None)
        .path()
        .by("name")
        .to_list()
        .unwrap();
    assert_eq!(strings(&paths[0]), vec!["marko", "josh", "ripple"]);
    assert_eq!(strings(&paths[1]), vec!["marko", "josh", "lop"]);
    drop(lock);

    #[cfg(feature = "script")]
    assert_rhai(
        &Arc::new(graph),
        &paths,
        &[
            r#"g.V("1").out().out().path().by("name")"#,
            r#"g.v("1").out().out().path().by("name")"#,
        ],
    );
}

#[test]
fn path_by_modulators_apply_round_robin() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    // marko (name), josh (age), ripple (name)
    let paths = lock
        .traversal()
        .v("1")
        .out("knows")
        .path()
        .by("name")
        .by("age")
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 2);
    assert_eq!(strings(&paths[0]), vec!["marko", "32"]);
    assert_eq!(strings(&paths[1]), vec!["marko", "27"]);
    drop(lock);

    #[cfg(feature = "script")]
    assert_rhai(
        &Arc::new(graph),
        &paths,
        &[r#"g.V("1").out("knows").path().by("name").by("age")"#],
    );
}

#[test]
fn path_by_a_missing_key_drops_the_traverser() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    // lop is software and has no age.
    let paths = lock
        .traversal()
        .v("1")
        .out(None)
        .path()
        .by("age")
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 2);
    let count = lock
        .traversal()
        .v("1")
        .out(None)
        .path()
        .by("age")
        .count()
        .next()
        .unwrap();
    assert_eq!(count.and_then(|c| c.as_i64()), Some(2));
}

#[test]
fn edge_paths() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let paths = lock.traversal().e(None).path().to_list().unwrap();
    assert_eq!(paths.len(), 6);
    assert!(paths.iter().all(|p| ids(p).len() == 1));

    let paths = lock
        .traversal()
        .v("1")
        .out_e(None)
        .in_v()
        .path()
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 3);
    for path in &paths {
        let ElementProperty::Array(objects) = path else {
            panic!("expected a path");
        };
        assert_eq!(objects.len(), 3);
        assert!(matches!(objects[0], ElementProperty::Vertex(_)));
        assert!(matches!(objects[1], ElementProperty::Edge(_)));
        assert!(matches!(objects[2], ElementProperty::Vertex(_)));
    }
}

#[test]
fn path_length_counts_objects() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let lengths = lock
        .traversal()
        .v("1")
        .out(None)
        .out(None)
        .path()
        .count_scoped(Scope::Local)
        .to_list()
        .unwrap();
    assert_eq!(lengths.len(), 2);
    assert!(lengths.iter().all(|l| l.as_i64() == Some(3)));
}

#[test]
fn a_barrier_starts_a_new_path() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let paths = lock
        .traversal()
        .v(None)
        .out(None)
        .fold()
        .path()
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 1);
    let ElementProperty::Array(objects) = &paths[0] else {
        panic!("expected a path");
    };
    assert_eq!(objects.len(), 1);
}

#[cfg(feature = "display")]
#[test]
fn table_view_renders_one_row_per_path() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let table = lock
        .traversal()
        .v("1")
        .out(None)
        .out(None)
        .path()
        .to_table()
        .unwrap();
    assert!(table.contains("path"), "{table}");
    assert!(table.contains("[v[1], v[4], v[5]]"), "{table}");
    assert!(table.contains("[v[1], v[4], v[3]]"), "{table}");
}

// T2: labels.

#[test]
fn select_reads_labels() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v("1")
        .as_("a")
        .out(None)
        .as_("b")
        .select("a")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names.len(), 3);
    assert!(names.iter().all(|n| n.as_string() == Some("marko")));

    let names = lock
        .traversal()
        .v("1")
        .as_("a")
        .out(None)
        .as_("a")
        .select("a")
        .values("name")
        .to_list()
        .unwrap();
    // The last label wins: "a" now names the neighbour, not marko.
    let mut names: Vec<_> = names.iter().filter_map(|n| n.as_string()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["josh", "lop", "vadas"]);
}

#[test]
fn where_label_predicate() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let back = lock
        .traversal()
        .v("1")
        .as_("a")
        .out(None)
        .in_(None)
        .where_p(P::Eq("a"))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(back.len(), 3);
    assert!(back.iter().all(|n| n.as_string() == Some("marko")));

    let others = lock
        .traversal()
        .v("1")
        .as_("a")
        .out(None)
        .in_(None)
        .where_p(P::Neq("a"))
        .values("name")
        .to_list()
        .unwrap();
    let mut others: Vec<_> = others.iter().filter_map(|n| n.as_string()).collect();
    others.sort_unstable();
    assert_eq!(others, vec!["josh", "peter"]);
}

#[test]
fn add_e_reads_labels() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let edges = graph
        .traversal_mut()
        .v("1")
        .as_("a")
        .out("knows")
        .as_("b")
        .add_e("pal")
        .from("a")
        .to("b")
        .to_list()
        .unwrap();
    assert_eq!(edges.len(), 2);
    for edge in &edges {
        let ElementProperty::Edge(edge) = edge else {
            panic!("expected an edge");
        };
        assert_eq!(edge.out_v().id(), "1");
    }
}

// T3: nested propagation.

#[test]
fn union_children_record_their_paths() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let paths = lock
        .traversal()
        .v("1")
        .union(vec![__::out("knows").path(), __::out("created").path()])
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 3);
    assert!(paths.iter().all(|p| ids(p).len() == 2 && ids(p)[0] == "1"));
    drop(lock);

    #[cfg(feature = "script")]
    assert_rhai(
        &Arc::new(graph),
        &paths,
        &[r#"g.V("1").union([__.out("knows").path(), __.out("created").path()])"#],
    );
}

#[test]
fn a_nested_full_requirement_reaches_the_parent() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v("1")
        .out(None)
        .where_t(__::in_(None).simple_path())
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, vec![ElementProperty::String("lop".into())]);
    drop(lock);

    #[cfg(feature = "script")]
    assert_rhai(
        &Arc::new(graph),
        &names,
        &[
            r#"g.V("1").out().where(__.in().simplePath()).values("name")"#,
            r#"g.v("1").out().where(__.in().simple_path()).values("name")"#,
        ],
    );
}

#[test]
fn a_nested_select_reads_the_parent_label() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v("1")
        .as_("a")
        .out(None)
        .where_t(__::select("a").has("name", "marko"))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names.len(), 3);
}

#[test]
fn optional_passthrough_keeps_labels() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v("2")
        .as_("a")
        .optional(__::out(None).as_("a"))
        .select("a")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, vec![ElementProperty::String("vadas".into())]);
}

// T4: analysis modes, observed through profile annotations.

fn profile_lines(stats: &TraversalMetrics) -> Vec<String> {
    stats
        .metrics()
        .iter()
        .map(|n| match n.path_recording() {
            Some(recording) => format!("{} [path: {recording}]", n.name()),
            None => n.name().to_string(),
        })
        .collect()
}

#[test]
fn a_query_without_path_consumer_records_nothing() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v(None)
        .out(None)
        .values("name")
        .profile()
        .unwrap();
    assert!(profile_lines(&stats).iter().all(|l| !l.contains("[path:")));
}

#[test]
fn tracking_stops_after_the_last_consumer() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .out(None)
        .path()
        .count_scoped(Scope::Local)
        .profile()
        .unwrap();
    let lines = profile_lines(&stats);
    assert_eq!(lines[0], "v(\"1\") [path: full]");
    assert_eq!(lines[1], "out() [path: full]");
    assert_eq!(lines[2], "path()");
    assert_eq!(lines[3], "count(Scope.local)");
}

#[test]
fn only_read_labels_are_tracked() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .as_("a")
        .out(None)
        .as_("b")
        .select("a")
        .profile()
        .unwrap();
    let lines = profile_lines(&stats);
    assert_eq!(lines[0], "v(\"1\")");
    assert_eq!(lines[1], "as(\"a\") [path: labels(a)]");
    assert_eq!(lines[2], "out() [path: labels(a)]");
    assert_eq!(lines[3], "as(\"b\") [path: labels(a)]");
    assert_eq!(lines[4], "select(\"a\")");
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("labels(a,b)") && !l.contains("labels(b)"))
    );
}

#[test]
fn a_path_before_a_fused_count_is_skipped() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v(None)
        .out(None)
        .path()
        .count()
        .execute_with(ExecOptions::profile(ProfileType::none()));
    let profile = stats.profile().unwrap();
    assert!(profile_lines(profile).iter().all(|l| !l.contains("[path:")));
    assert_eq!(stats.results()[0].as_i64(), Some(6));
}

#[test]
fn a_string_predicate_is_not_mistaken_for_a_label() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v(None)
        .values("name")
        .is("marko")
        .profile()
        .unwrap();
    assert!(profile_lines(&stats).iter().all(|l| !l.contains("[path:")));
}

// T5: simple_path() / cyclic_path().

#[test]
fn simple_and_cyclic_paths_on_a_cycle() {
    let graph = cycle_graph();
    let simple = graph
        .traversal()
        .v("a")
        .out(None)
        .out(None)
        .out(None)
        .simple_path()
        .to_list()
        .unwrap();
    assert!(simple.is_empty());
    let cyclic = graph
        .traversal()
        .v("a")
        .out(None)
        .out(None)
        .out(None)
        .cyclic_path()
        .path()
        .by("name")
        .to_list()
        .unwrap();
    assert_eq!(cyclic.len(), 1);
    assert_eq!(strings(&cyclic[0]), vec!["a", "b", "c", "a"]);
    let two_steps = graph
        .traversal()
        .v("a")
        .out(None)
        .out(None)
        .simple_path()
        .count()
        .next()
        .unwrap();
    assert_eq!(two_steps.and_then(|c| c.as_i64()), Some(1));
}

#[test]
fn simple_path_excludes_returns_to_the_start() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let paths = lock
        .traversal()
        .v("1")
        .both(None)
        .both(None)
        .simple_path()
        .path()
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 4);
    for path in &paths {
        let ids = ids(path);
        assert_eq!(ids[0], "1");
        assert!(!ids[1..].contains(&"1".to_string()));
    }
    let cyclic = lock
        .traversal()
        .v("1")
        .both(None)
        .both(None)
        .cyclic_path()
        .count()
        .next()
        .unwrap();
    assert_eq!(cyclic.and_then(|c| c.as_i64()), Some(3));
    drop(lock);

    #[cfg(feature = "script")]
    assert_rhai(
        &Arc::new(graph),
        &paths,
        &[
            r#"g.V("1").both().both().simplePath().path()"#,
            r#"g.v("1").both().both().simple_path().path()"#,
        ],
    );
}

#[test]
fn simple_path_compares_values_by_value() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    // marko -> "marko": the name value differs from the vertex, so the path is simple.
    let simple = lock
        .traversal()
        .v("1")
        .values("name")
        .simple_path()
        .count()
        .next()
        .unwrap();
    assert_eq!(simple.and_then(|c| c.as_i64()), Some(1));
    // "marko" -> constant "marko": the same value twice.
    let cyclic = lock
        .traversal()
        .v("1")
        .values("name")
        .constant("marko")
        .cyclic_path()
        .count()
        .next()
        .unwrap();
    assert_eq!(cyclic.and_then(|c| c.as_i64()), Some(1));
}

#[cfg(feature = "script")]
#[test]
fn cyclic_path_in_both_spellings() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let snake = rhai(&graph, r#"g.V("1").both().both().cyclic_path().path()"#);
    let camel = rhai(&graph, r#"g.V("1").both().both().cyclicPath().path()"#);
    assert_eq!(snake, camel);
    assert_eq!(snake.len(), 3);
    let anonymous = rhai(
        &graph,
        r#"g.V("1").out().where(__.in().cyclicPath()).values("name")"#,
    );
    assert_eq!(anonymous.len(), 3);
}

// T6: differential test, analysis on and off.

/// Queries using `as/select/where/path/simplePath/cyclicPath/addE/math`, run as `.toList()`.
#[cfg(feature = "script")]
const CORPUS: &[&str] = &[
    r#"g.V().out().out().path()"#,
    r#"g.V("1").out().out().path().by("name")"#,
    r#"g.V("1").outE().inV().path()"#,
    r#"g.E().path()"#,
    r#"g.V().as("a").out().select("a")"#,
    r#"g.V().as("a").out().as("b").select(["a", "b"])"#,
    r#"g.V().as("a").out().as("b").select("b").values("name")"#,
    r#"g.V("1").as("a").out().in().where(P.neq("a")).values("name")"#,
    r#"g.V("1").as("a").out().in().where(P.eq("a")).values("name")"#,
    r#"g.V("1").both().both().simplePath().path()"#,
    r#"g.V("1").both().both().cyclicPath().path()"#,
    r#"g.V().both().both().simplePath().count()"#,
    r#"g.V("1").out().where(__.in().simplePath()).values("name")"#,
    r#"g.V("1").as("a").out().where(__.select("a").has("name", "marko")).values("name")"#,
    r#"g.V("1").union([__.out("knows").path(), __.out("created").path()])"#,
    r#"g.V("1").union([__.out("knows"), __.out("created")]).path()"#,
    r#"g.V("1").coalesce([__.out("nothing"), __.out("knows")]).path()"#,
    r#"g.V("1").optional(__.out("nothing")).path()"#,
    r#"g.V("1").optional(__.out("knows")).path().by("name")"#,
    r#"g.V().hasKey("age").as("x").out("knows").as("y").math("x - y").by("age")"#,
    r#"g.V().as("a").out("created").as("b").select(["a", "b"]).by("name")"#,
    r#"g.V().as("a").out().out().select("a").values("name")"#,
    r#"g.V().out().path().count()"#,
    r#"g.V().out().fold().path()"#,
    r#"g.V().as("a").out().dedup().select("a")"#,
    r#"g.V().as("a").out().limit(2).path()"#,
    r#"g.V().values("name").as("n").select("n")"#,
    r#"g.V("1").as("a").out().as("a").select("a").values("name")"#,
    r#"g.V("1").out().as("x").in().as("y").select(["x", "y"]).by("name")"#,
    r#"g.V().as("a").project(["n"]).by(__.select("a").values("name"))"#,
    r#"g.V("1").out().path().count(Scope.local)"#,
    r#"g.V("1").as("a").out().out().path().by("name").by(__.label())"#,
    r#"g.V("1").as("a").out("knows").as("b").addE("pal").from("a").to("b").path()"#,
    r#"g.V().as("a").both().both().where(P.eq("a")).path()"#,
    r#"g.V("2").as("a").optional(__.out().as("a")).select("a").values("name")"#,
    r#"g.V("1").as("a").out().as("b").in().where(P.eq("a")).select(["a", "b"]).by("name")"#,
];

#[cfg(feature = "script")]
fn run_corpus_query(script: &str) -> Vec<String> {
    // Each run gets a fresh graph, so mutating queries compare like with like.
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let value = crate::script_host::eval_value(graph, &format!("{script}.toList()"))
        .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
    let mut rows: Vec<String> = value
        .into_array()
        .unwrap_or_else(|_| panic!("`{script}` did not return a list"))
        .into_iter()
        .map(|row| format!("{row:?}"))
        .collect();
    // Compare as multisets: some steps (`dedup()`) do not guarantee an output order.
    rows.sort_unstable();
    rows
}

#[cfg(feature = "script")]
#[test]
fn analysis_on_and_off_return_identical_results() {
    assert!(CORPUS.len() >= 30);
    for script in CORPUS {
        let with_analysis = run_corpus_query(script);
        let without = run_corpus_query(&format!(
            "g.with(\"path.analysis\", false){}",
            script.strip_prefix('g').unwrap_or(script)
        ));
        assert_eq!(with_analysis, without, "results differ for `{script}`");
    }
}

#[test]
fn kill_switch_records_full_paths() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .with("path.analysis", false)
        .v("1")
        .out(None)
        .values("name")
        .profile()
        .unwrap();
    assert!(
        profile_lines(&stats)
            .iter()
            .all(|l| l.ends_with("[path: full]"))
    );
}

/// A mid-traversal `V()`/`E()` maps every incoming traverser to every element it finds and
/// extends that traverser's path, as in TinkerPop; it does not append to the stream.
#[test]
fn mid_traversal_v_runs_per_traverser_and_extends_the_path() {
    let graph = TraversalGraph::tinkerpop_modern();
    let paths = graph
        .traversal()
        .v("1")
        .as_("x")
        .v("2")
        .path()
        .to_list()
        .unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(ids(&paths[0]), ["1", "2"]);
    let names = graph
        .traversal()
        .v(None)
        .has_label("person")
        .v(None)
        .has_label("software")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names.len(), 8);
    let selected = graph
        .traversal()
        .v("1")
        .as_("x")
        .v("2")
        .select("x")
        .id()
        .to_list()
        .unwrap();
    assert_eq!(selected, [ElementProperty::from("1")]);
    // An empty upstream stays empty.
    let none = graph
        .traversal()
        .v(None)
        .has_label("nothing")
        .v(None)
        .count()
        .next()
        .unwrap();
    assert_eq!(none, Some(ElementProperty::from(0)));
    let edges = graph
        .traversal()
        .v(vec!["1", "2"])
        .e(None)
        .count()
        .next()
        .unwrap();
    assert_eq!(edges, Some(ElementProperty::from(12)));
}
