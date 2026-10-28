//! Integration tests for reading maps generically: `unfold()` of a map into entries and
//! `select(Column::Keys)` / `select(Column::Values)` (TinkerPop `select(Column)`).

use graphersal::prelude::*;

/// The modern graph's vertex names in insertion order, which is the order `group()` keeps.
const NAMES: [&str; 6] = ["marko", "vadas", "lop", "josh", "ripple", "peter"];
/// `out().count()` per vertex, in the same order as [`NAMES`].
const OUT_COUNTS: [i64; 6] = [3, 0, 0, 2, 0, 1];

fn strings(values: &[ElementProperty]) -> Vec<String> {
    values
        .iter()
        .map(|v| {
            v.as_string()
                .unwrap_or_else(|| panic!("not a string: {v:?}"))
                .to_string()
        })
        .collect()
}

#[test]
fn unfold_of_a_map_emits_one_single_key_entry_per_key_in_map_order() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let entries = lock
        .traversal()
        .v(None::<()>)
        .group()
        .by("name")
        .by(__::out(None::<()>).count())
        .unfold()
        .to_list()
        .unwrap();

    assert_eq!(entries.len(), 6);
    for ((entry, name), count) in entries.iter().zip(NAMES).zip(OUT_COUNTS) {
        match entry {
            ElementProperty::Object(obj) => {
                assert_eq!(
                    obj.len(),
                    1,
                    "an entry materializes as a single-key map: {obj:?}"
                );
                assert_eq!(obj.get(name).and_then(|v| v.as_i64()), Some(count));
            }
            other => panic!("expected a single-key map, got {other:?}"),
        }
    }
}

#[test]
fn unfold_select_keys_and_values_read_each_entry_as_a_scalar() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let keys = lock
        .traversal()
        .v(None::<()>)
        .group()
        .by("name")
        .by(__::out(None::<()>).count())
        .unfold()
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(strings(&keys), NAMES);

    let values = lock
        .traversal()
        .v(None::<()>)
        .group()
        .by("name")
        .by(__::out(None::<()>).count())
        .unfold()
        .select(Column::Values)
        .to_list()
        .unwrap();
    let counts: Vec<Option<i64>> = values.iter().map(|v| v.as_i64()).collect();
    assert_eq!(counts, OUT_COUNTS.map(Some));
}

#[test]
fn select_keys_on_a_whole_map_emits_one_list() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let keys = lock
        .traversal()
        .v(None::<()>)
        .group()
        .by("name")
        .by(__::out(None::<()>).count())
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(keys.len(), 1, "one traverser for the whole map: {keys:?}");
    match &keys[0] {
        ElementProperty::Array(items) => assert_eq!(strings(items), NAMES),
        other => panic!("expected a list of keys, got {other:?}"),
    }

    let unfolded = lock
        .traversal()
        .v(None::<()>)
        .group()
        .by("name")
        .by(__::out(None::<()>).count())
        .select(Column::Keys)
        .unfold()
        .to_list()
        .unwrap();
    assert_eq!(strings(&unfolded), NAMES);

    let values = lock
        .traversal()
        .v(None::<()>)
        .group_count()
        .by(By::Label)
        .select(Column::Values)
        .to_list()
        .unwrap();
    assert_eq!(values.len(), 1);
    match &values[0] {
        ElementProperty::Array(items) => {
            // The pushed-down `group_count().by(T.label)` does not guarantee a key order.
            let mut counts: Vec<Option<i64>> = items.iter().map(|v| v.as_i64()).collect();
            counts.sort_unstable();
            assert_eq!(counts, vec![Some(2), Some(4)]);
        }
        other => panic!("expected a list of values, got {other:?}"),
    }
}

/// The map-versus-entry rule: a one-group `group_count()` result is still a map, so
/// `select(Column::Keys)` yields a one-element list, while the entry `unfold()` makes of it
/// yields the bare key.
#[test]
fn a_single_key_map_is_a_map_not_an_entry() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let from_map = lock
        .traversal()
        .v("1")
        .group_count()
        .by("name")
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(
        from_map,
        vec![ElementProperty::Array(vec![ElementProperty::String(
            "marko".into()
        )])]
    );

    let from_entry = lock
        .traversal()
        .v("1")
        .group_count()
        .by("name")
        .unfold()
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(from_entry, vec![ElementProperty::String("marko".into())]);
}

#[test]
fn records_are_maps_too() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let keys = lock
        .traversal()
        .v("1")
        .element_map(None::<()>)
        .select(Column::Keys)
        .unfold()
        .to_list()
        .unwrap();
    let keys = strings(&keys);
    for expected in ["id", "label", "name", "age"] {
        assert!(
            keys.iter().any(|k| k == expected),
            "missing {expected} in {keys:?}"
        );
    }

    let entries = lock
        .traversal()
        .v("1")
        .element_map(None::<()>)
        .unfold()
        .count()
        .next()
        .unwrap()
        .and_then(|c| c.as_i64());
    assert_eq!(entries, Some(keys.len() as i64));
}

#[test]
fn anonymous_select_column_filters_entries() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v(None::<()>)
        .group()
        .by("name")
        .by(__::out(None::<()>).count())
        .unfold()
        .where_t(__::select(Column::Values).is(P::Gt(1)))
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(strings(&names), vec!["marko", "josh"]);
}

#[test]
fn unfold_keeps_lists_and_scalars_and_passes_entries_through() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let from_list = lock
        .traversal()
        .v(None::<()>)
        .fold()
        .unfold()
        .count()
        .next()
        .unwrap();
    assert_eq!(from_list.and_then(|c| c.as_i64()), Some(6));

    let scalars = lock
        .traversal()
        .v(None::<()>)
        .values("name")
        .unfold()
        .to_list()
        .unwrap();
    assert_eq!(strings(&scalars), NAMES);

    let entries_twice = lock
        .traversal()
        .v(None::<()>)
        .group_count()
        .by("name")
        .unfold()
        .unfold()
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(strings(&entries_twice), NAMES);
}

#[test]
fn select_column_on_a_non_map_is_a_cast_error() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();

    for (err, actual) in [
        (
            lock.traversal()
                .v(None::<()>)
                .select(Column::Keys)
                .next()
                .unwrap_err(),
            "vertex",
        ),
        (
            lock.traversal()
                .v(None::<()>)
                .values("name")
                .select(Column::Values)
                .next()
                .unwrap_err(),
            "string",
        ),
        (
            lock.traversal()
                .v(None::<()>)
                .fold()
                .select(Column::Keys)
                .next()
                .unwrap_err(),
            "array",
        ),
    ] {
        match err.root_cause() {
            TraverserError::Value(ValueError::CastException {
                expected,
                actual: got,
                ..
            }) => {
                assert_eq!(expected, "map or entry");
                assert_eq!(got, actual);
            }
            other => panic!("expected a cast exception, got {other:?}"),
        }
    }
}

/// `select(Column)` reads nothing from the path: an `as()` label upstream records nothing, and
/// the step shows under its own name in the profile.
#[test]
fn select_column_needs_no_path_and_profiles_under_its_name() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let profile = lock
        .traversal()
        .v(None::<()>)
        .as_("keys")
        .group_count()
        .by("name")
        .unfold()
        .select(Column::Keys)
        .profile()
        .unwrap()
        .to_string();
    assert!(profile.contains("select(Column.keys)"), "{profile}");
    assert!(
        !profile.contains("[path:"),
        "select(Column) must not record a path: {profile}"
    );
}

/// `select(Column)` over the other map shapes: an object-valued vertex/edge property read
/// through `values(key)`, an injected map and a path's labels; a scalar is a cast error.
#[cfg(feature = "script")]
mod other_shapes {
    use super::*;
    use crate::script_host::eval_value;
    use std::sync::Arc;

    fn docs() -> Arc<Graph> {
        let graph = Arc::new(GraphSource::empty());
        let _ = eval_value(
            graph.clone(),
            r#"
            g.addV("doc").property("meta", #{a: 1, b: 2}).property("n", 5).next();
            g.V().addE("self").to(__.V()).property("info", #{w: 3}).next();
            "#,
        )
        .expect("fixture");
        graph
    }

    fn rendered(graph: &Arc<Graph>, script: &str) -> String {
        eval_value(graph.clone(), script)
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .to_string()
    }

    #[test]
    fn object_properties_injected_maps_and_path_labels() {
        let g = docs();
        assert_eq!(
            rendered(&g, r#"g.V().values("meta").select(Column.keys).next()"#),
            r#"["a", "b"]"#
        );
        assert_eq!(
            rendered(&g, r#"g.v().values("meta").select(Column::values).next()"#),
            "[1, 2]"
        );
        assert_eq!(
            rendered(&g, r#"g.E().values("info").select(Column.values).next()"#),
            "[3]"
        );
        assert_eq!(
            rendered(&g, r#"g.inject(#{x: 1}).select(Column.keys).next()"#),
            r#"["x"]"#
        );
        // a path's keys are its label sets, one per step
        assert_eq!(
            rendered(
                &g,
                r#"g.V().as("p").out().as("q", "r").path().select(Column.keys).next()"#
            ),
            r#"[["p"], ["q", "r"]]"#
        );
    }

    #[test]
    fn a_scalar_property_is_a_cast_error() {
        let g = docs();
        let err = eval_value(
            g.clone(),
            r#"g.V().values("n").select(Column.keys).toList()"#,
        )
        .expect_err("an int has no keys")
        .to_string();
        assert!(
            err.contains("expected type [map or entry], got type 'int64'"),
            "{err}"
        );
    }

    /// TinkerPop's `Column.values` applied to a `Path` is its objects, `Column.keys` its labels;
    /// both line up by position. A `by()`-modulated path yields the modulated values.
    #[test]
    fn a_path_s_values_are_its_objects_in_rhai() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        for script in [
            r#"g.V("1").out("knows").path().by("name").select(Column.values).toList()"#,
            r#"g.v("1").out("knows").path().by("name").select(Column::values).to_list()"#,
            r#"g.V("1").out("knows").path().by("name").select(values).toList()"#,
        ] {
            assert_eq!(
                rendered(&graph, script),
                r#"[["marko", "josh"], ["marko", "vadas"]]"#,
                "{script}"
            );
        }
        assert_eq!(
            rendered(
                &graph,
                r#"g.V("1").as("a").out("knows").has("name", "vadas").as("b").path().by("name").project("k", "v").by(__.select(Column.keys)).by(__.select(Column.values)).next()"#
            ),
            r#"#{"k": [["a"], ["b"]], "v": ["marko", "vadas"]}"#
        );
        // by(Column.values) over a path is the same projection
        assert_eq!(
            rendered(
                &graph,
                r#"g.V("1").out("knows").path().by("name").order().by(Column.values, Order.desc).limit(1).select(Column.values).next()"#
            ),
            r#"["marko", "vadas"]"#
        );
    }

    /// TinkerPop's `Column.values` applied to a `Path` is its objects (`Column.keys` its labels).
    #[test]
    fn a_path_s_values_are_its_objects() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let names = lock
            .traversal()
            .v("1")
            .out("knows")
            .path()
            .select(Column::Values)
            .unfold()
            .values("name")
            .to_list()
            .unwrap();
        // the out() order of this modern graph: josh before vadas
        assert_eq!(strings(&names), ["marko", "josh", "marko", "vadas"]);
    }
}
