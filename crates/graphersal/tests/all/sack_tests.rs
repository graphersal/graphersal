//! Sack storage and read (sack series, step 2): `withSack(initial)`, `sack()`, the sack carried by
//! every derived traverser, child traversal and merge point. The update operators arrive in
//! step 3, so every sack here is the initial one (or `null`).

use graphersal::prelude::*;

fn modern() -> Graph {
    GraphSource::tinkerpop_modern()
}

type Source<'b> = GraphTraversalSource<'b, &'b TraversalGraph>;

fn int(v: i64) -> ElementProperty {
    ElementProperty::Number(Number::from(v))
}

fn all_equal(values: &[ElementProperty], expected: &ElementProperty, count: usize) {
    assert_eq!(values.len(), count, "{values:?}");
    assert!(values.iter().all(|v| v == expected), "{values:?}");
}

fn run<F>(query: F) -> Vec<ElementProperty>
where
    F: for<'b> Fn(Source<'b>) -> Source<'b>,
{
    let graph = modern();
    let lock = graph.read();
    query(lock.traversal()).to_list().expect("query runs")
}

/// The generational handles (`VertexHandle`/`EdgeHandle`: slot + non-zero generation, 8 bytes,
/// `Option` uses the niche) must not grow the stream value (`book/src/users_guide/transactions.md`).
#[test]
fn traverser_value_stays_64_bytes() {
    use std::mem::size_of;
    type G = TraversalGraph;
    assert_eq!(size_of::<TraverserValue<G>>(), 64);
    assert_eq!(size_of::<graphersal::storage::VertexHandle>(), 8);
    assert_eq!(size_of::<Option<graphersal::storage::VertexHandle>>(), 8);
    assert_eq!(size_of::<graphersal::storage::EdgeHandle>(), 8);
}

/// The stored value stays 32 bytes (a `CompactString` is 24) and `CompactString` is re-exported at
/// the crate root and in the prelude, so a user does not depend on `compact_str` directly.
#[test]
fn element_property_stays_32_bytes_and_compact_string_is_reexported() {
    use std::mem::size_of;
    assert_eq!(size_of::<ElementProperty>(), 32);
    assert_eq!(size_of::<graphersal::CompactString>(), 24);
    let name: graphersal::CompactString = "marko".into();
    let prelude_name: graphersal::prelude::CompactString = name.clone();
    assert_eq!(
        ElementProperty::String(prelude_name),
        ElementProperty::String(name)
    );
}

// ---- read ----

#[test]
fn with_sack_reads_the_initial_value_on_every_traverser() {
    all_equal(&run(|g| g.with_sack(5).v(None).sack()), &int(5), 6);
}

#[test]
fn without_with_sack_the_sack_is_null() {
    all_equal(&run(|g| g.v(None).sack()), &ElementProperty::Null, 6);
}

#[test]
fn typed_sacks_round_trip() {
    all_equal(
        &run(|g| g.with_sack("x").inject(vec![1]).sack()),
        &ElementProperty::String("x".into()),
        1,
    );
    all_equal(
        &run(|g| g.with_sack(0.0).inject(vec![1]).sack()),
        &ElementProperty::Number(Number::from(0.0f64)),
        1,
    );
    all_equal(
        &run(|g| g.with_sack(true).inject(vec![1]).sack()),
        &ElementProperty::Boolean(true),
        1,
    );
    let id = 0x1234_u128;
    all_equal(
        &run(|g| {
            g.with_sack(ElementProperty::Uuid(id))
                .inject(vec![1])
                .sack()
        }),
        &ElementProperty::Uuid(id),
        1,
    );
}

#[test]
fn a_float_sack_is_not_an_int_sack() {
    let one = run(|g| g.with_sack(1).inject(vec![0]).sack());
    let one_f = run(|g| g.with_sack(1.0).inject(vec![0]).sack());
    assert_eq!(one, vec![int(1)]);
    assert_ne!(one, one_f);
}

#[test]
fn a_container_sack_is_carried() {
    let list = ElementProperty::Array(vec![int(1), int(2)]);
    all_equal(
        &run(|g| g.with_sack(list.clone()).v(None).out(None).sack()),
        &list,
        6,
    );
}

#[test]
fn a_vertex_is_not_a_sack_value() {
    let graph = modern();
    let lock = graph.read();
    let vertex = ElementProperty::Vertex(Box::default());
    let err = lock.traversal().with_sack(vertex).v(None).sack().to_list();
    let err = err.expect_err("a vertex sack is refused");
    let help = graphersal::error::DiagnosticError::help(&err).unwrap_or_default();
    assert!(help.contains("by(T.id)"), "{help}");
}

// ---- carried by derived traversers ----

#[test]
fn the_sack_survives_derivation_steps() {
    let five = int(5);
    all_equal(&run(|g| g.with_sack(5).v(None).out(None).sack()), &five, 6);
    all_equal(&run(|g| g.with_sack(5).v(None).in_(None).sack()), &five, 6);
    all_equal(
        &run(|g| g.with_sack(5).v(None).both(None).sack()),
        &five,
        12,
    );
    all_equal(
        &run(|g| g.with_sack(5).v("1").out_e(None).in_v().sack()),
        &five,
        3,
    );
    all_equal(
        &run(|g| g.with_sack(5).v(None).has_label("person").sack()),
        &five,
        4,
    );
    all_equal(
        &run(|g| g.with_sack(5).v(None).values("age").sack()),
        &five,
        4,
    );
    all_equal(
        &run(|g| g.with_sack(5).v(None).out(None).dedup().sack()),
        &five,
        4,
    );
    all_equal(&run(|g| g.with_sack(5).v(None).limit(2).sack()), &five, 2);
    all_equal(
        &run(|g| g.with_sack(5).v(None).repeat(__::out(None)).times(2).sack()),
        &five,
        2,
    );
    all_equal(
        &run(|g| g.with_sack(5).v("1").as_("a").out(None).select("a").sack()),
        &five,
        3,
    );
    all_equal(&run(|g| g.with_sack(5).v(None).barrier().sack()), &five, 6);
}

#[test]
fn the_sack_changes_nothing_about_the_value_stream() {
    let plain = run(|g| g.v(None).values("age"));
    let sacked = run(|g| g.with_sack(5).v(None).values("age"));
    assert_eq!(plain, sacked);
}

#[test]
fn a_reducing_barrier_emits_the_initial_sack() {
    all_equal(&run(|g| g.with_sack(9).v(None).count().sack()), &int(9), 1);
    all_equal(
        &run(|g| g.with_sack(9).v(None).values("age").sum().sack()),
        &int(9),
        1,
    );
    all_equal(&run(|g| g.with_sack(9).v(None).fold().sack()), &int(9), 1);
    all_equal(
        &run(|g| g.with_sack(9).v(None).fold().unfold().sack()),
        &int(9),
        6,
    );
}

#[test]
fn start_steps_get_the_initial_sack() {
    all_equal(
        &run(|g| g.with_sack(9).inject(vec![1, 2]).sack()),
        &int(9),
        2,
    );
    all_equal(&run(|g| g.with_sack(9).e(None).sack()), &int(9), 6);
}

#[test]
fn the_sack_is_absent_from_a_path_unless_read() {
    let paths = run(|g| g.with_sack(7).v("1").sack().path());
    match &paths[0] {
        ElementProperty::Array(items) => assert_eq!(items.last(), Some(&int(7))),
        other => panic!("{other:?}"),
    }
}

#[test]
fn profile_keeps_sack_as_its_own_row() {
    let graph = modern();
    let lock = graph.read();
    let text = lock
        .traversal()
        .with_sack(1)
        .v(None)
        .out(None)
        .sack()
        .count()
        .profile()
        .unwrap()
        .to_string();
    assert!(text.contains("sack()"), "{text}");
    assert!(text.contains("count()"), "{text}");
}

// ---- merging ----

#[test]
fn equal_sacks_merge_and_results_do_not_change() {
    let graph = modern();
    let lock = graph.read();
    let count = |sack: bool, merge: bool| {
        let g = lock.traversal().with("bulk.merge", merge);
        let g = if sack { g.with_sack(0) } else { g };
        g.v(None)
            .repeat(__::out(None))
            .times(3)
            .count()
            .to_list()
            .unwrap()
    };
    let baseline = count(false, true);
    assert_eq!(count(true, true), baseline);
    assert_eq!(count(true, false), baseline);
    let rows = |merge: bool| {
        let mut r = lock
            .traversal()
            .with("bulk.merge", merge)
            .with_sack(0)
            .v(None)
            .both(None)
            .both(None)
            .barrier()
            .values("name")
            .to_list()
            .unwrap();
        r.sort_by_key(|v| format!("{v:?}"));
        r
    };
    assert_eq!(rows(true), rows(false));
}

#[test]
fn the_frontier_still_merges_with_a_sack() {
    let graph = modern();
    let lock = graph.read();
    let text = lock
        .traversal()
        .with_sack(0)
        .v(None)
        .both(None)
        .both(None)
        .barrier()
        .profile()
        .unwrap()
        .to_string();
    let header = text.lines().find(|l| l.starts_with("Step")).unwrap();
    assert!(header.contains("Bulk"), "{text}");
}

// ---- Rhai ----

#[cfg(feature = "script")]
mod dsl {
    use super::*;
    use std::sync::Arc;

    fn eval(script: &str) -> Result<Vec<ElementProperty>, String> {
        let graph = Arc::new(modern());
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .map_err(|e| format!("{script}: {e}"))?;
        let lock = graph.read();
        gts.ast_convert_to_gts(&*lock)
            .to_list()
            .map_err(|e| format!("{e:?}"))
    }

    #[test]
    fn both_spellings_and_the_read_step() {
        for script in [
            "g.withSack(5).V().sack()",
            "g.with_sack(5).V().sack()",
            "g.withSack(5).v().sack()",
        ] {
            all_equal(&eval(script).unwrap(), &int(5), 6);
        }
        all_equal(&eval("g.V().sack()").unwrap(), &ElementProperty::Null, 6);
        all_equal(
            &eval(r#"g.withSack("x").inject(1).sack()"#).unwrap(),
            &ElementProperty::String("x".into()),
            1,
        );
        all_equal(
            &eval("g.withSack(0.5).inject(1).sack()").unwrap(),
            &ElementProperty::Number(Number::from(0.5f64)),
            1,
        );
        all_equal(
            &eval(r#"g.withSack(UUID.fromString("00000000-0000-0000-0000-000000000001")).inject(1).sack()"#).unwrap(),
            &ElementProperty::Uuid(1),
            1,
        );
    }

    /// One query per child-input site of the construction audit: each child sees the sack of
    /// the traverser it runs for (here the initial one, 7).
    #[test]
    fn every_child_traversal_sees_the_sack() {
        let seven = int(7);
        all_equal(
            &eval("g.withSack(7).V().where(__.sack().is(P.eq(7))).sack()").unwrap(),
            &seven,
            6,
        );
        all_equal(
            &eval("g.withSack(7).V().filter(__.sack().is(P.eq(7))).sack()").unwrap(),
            &seven,
            6,
        );
        all_equal(
            &eval("g.withSack(7).V().local(__.sack())").unwrap(),
            &seven,
            6,
        );
        all_equal(
            &eval("g.withSack(7).V().union(__.sack(), __.sack())").unwrap(),
            &seven,
            12,
        );
        all_equal(
            &eval("g.withSack(7).V().coalesce(__.constant(1).sack())").unwrap(),
            &seven,
            6,
        );
        all_equal(
            &eval(r#"g.withSack(7).V().choose(__.has("age"), __.sack(), __.sack())"#).unwrap(),
            &seven,
            6,
        );
        all_equal(
            &eval("g.withSack(7).V().optional(__.out().sack()).sack()").unwrap(),
            &seven,
            9,
        );
        all_equal(
            &eval("g.withSack(7).V().order().by(__.sack()).sack()").unwrap(),
            &seven,
            6,
        );
        all_equal(
            &eval(r#"g.withSack(7).V().project("s").by(__.sack()).select("s")"#).unwrap(),
            &seven,
            6,
        );
        all_equal(
            &eval(
                r#"g.withSack(7).V().has("name","marko").as("a").out().select("a").by(__.sack())"#,
            )
            .unwrap(),
            &seven,
            3,
        );
        all_equal(
            &eval(r#"g.withSack(7).V().values("age").math("_ + 1").by(__.sack()).sack()"#).unwrap(),
            &seven,
            4,
        );
        all_equal(
            &eval(r#"g.withSack(7).V().has("name","marko").path().by(__.sack()).unfold()"#)
                .unwrap(),
            &seven,
            1,
        );
        all_equal(
            &eval("g.withSack(7).V().group().by(__.sack()).by(__.count()).unfold().select(keys)")
                .unwrap(),
            &seven,
            1,
        );
        all_equal(
            &eval(r#"g.withSack(7).V().has("name","marko").repeat(__.out().sack()).times(1)"#)
                .unwrap(),
            &seven,
            3,
        );
        all_equal(&eval(r#"g.withSack(7).V().has("name","marko").until(__.sack().is(P.eq(7))).repeat(__.out()).sack()"#).unwrap(), &seven, 1);
        all_equal(&eval(r#"g.withSack(7).V().has("name","marko").out().emit().repeat(__.out()).times(1).sack()"#).unwrap(), &seven, 5);
    }

    fn eval_on(graph: Arc<Graph>, script: &str) -> Vec<ElementProperty> {
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("{script}: {e}"));
        let lock = graph.read();
        gts.ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{script}: {e:?}"))
    }

    /// Child-input sites that need a filtering child to be observable: if the child did not see
    /// the sack its `is(P.eq(7))` would be empty and the traverser (or the list item) dropped.
    #[test]
    fn filtering_children_prove_the_sack_reaches_every_site() {
        let ages = eval(r#"g.withSack(7).V().values("age").fold().order(Scope.local).by(__.sack().is(P.eq(7)))"#).unwrap();
        assert_eq!(ages.len(), 1);
        match &ages[0] {
            ElementProperty::Array(items) => assert_eq!(items.len(), 4, "{items:?}"),
            other => panic!("{other:?}"),
        }
        // where(P).by(traversal): the by() runs on the compared objects.
        all_equal(
            &eval(r#"g.withSack(7).V().has("name","marko").as("a").out().where(P.eq("a")).by(__.sack().is(P.eq(7))).sack()"#).unwrap(),
            &int(7),
            3,
        );
        // property(key, traversal) stores what the child read from the sack.
        let graph = Arc::new(modern());
        let stored = crate::script_host::eval_value(
            graph,
            r#"g.withSack(7).V().has("name","marko").property("s", __.sack()).values("s").toList()"#,
        )
        .unwrap();
        assert_eq!(format!("{stored:?}"), "[7]");
        // glob_path(prune): a pruning child that sees the sack (7) stops the descent below the
        // start; one that never matches (8) walks the whole tree.
        let tree = Arc::new(GraphSource::file_tree());
        let count = |sack: i64| {
            let script = format!(
                r#"g.withSack(7).V("root").glob_path("**/*.rs", #{{prune: __.sack().is(P.eq({sack}))}}).by("name").count()"#
            );
            match eval_on(tree.clone(), &script).as_slice() {
                [ElementProperty::Number(n)] => n.as_i64().unwrap(),
                other => panic!("{other:?}"),
            }
        };
        assert!(count(7) < count(8), "{} vs {}", count(7), count(8));
    }

    #[test]
    fn nested_start_steps_get_the_initial_sack() {
        let seven = int(7);
        let result =
            eval("g.withSack(7).V().limit(2).union(__.inject(1).sack(), __.sack())").unwrap();
        // A mid-traversal inject() passes the incoming traverser through and adds the injected
        // value: (incoming + 1, then the sack read) per input, two inputs.
        all_equal(&result, &seven, 6);
        all_equal(
            &eval("g.withSack(7).V().limit(1).local(__.inject(1, 2).sack())").unwrap(),
            &seven,
            3,
        );
    }

    #[test]
    fn the_new_steps_do_not_mutate() {
        let graph = Arc::new(modern());
        let before = eval("g.V().count()").unwrap();
        let _ = eval("g.withSack(1).V().sack()").unwrap();
        assert_eq!(eval("g.V().count()").unwrap(), before);
        let _ = graph;
    }
}
