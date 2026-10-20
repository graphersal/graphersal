//! The stage 1b internal steps on hand-built pipelines (the lowering that builds them comes
//! later): `gql:aggregate`, `gql:distinct`, `gql:bind`, `gql:trail` and the bindings
//! `RowColumn`, `Group` and `Path`.

use std::sync::Arc;

use super::*;
use crate::gql::aggregate::compile_aggregating;
use crate::gql::ast::Query;
use crate::gql::eval::compile;
use crate::gql::parser::parse;
use crate::traversal::step::flatbarrier_steps::step_gql_aggregate::GqlAggregateColumn;
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::{
    __, AnonymousTraversal, GraphTraversalSource, PropertyMap, TraversalGraph, TraverserResult,
};

/// The modern graph plus persons with a stored `null` age, a float age `32.0` (equal to josh's
/// `32` under GQL equality) and no age.
fn graph() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for (name, age) in [
        ("nullage", Some(ElementProperty::Null)),
        ("floatage", Some(ElementProperty::from(32.0))),
        ("noage", None),
    ] {
        let mut t = graph.traversal_mut().add_v("person").property("name", name);
        if let Some(age) = age {
            t = t.property("age", age);
        }
        t.iterate().unwrap();
    }
    graph
}

fn query(text: &str) -> (Arc<str>, Query) {
    match parse(text) {
        Ok(parsed) => (Arc::from(text), parsed),
        Err(error) => panic!("{text:?} does not parse: {error}"),
    }
}

fn names(bindings: &[(&str, Binding)]) -> (Vec<CompactString>, Arc<[Binding]>) {
    (
        bindings
            .iter()
            .map(|(name, _)| CompactString::from(*name))
            .collect(),
        bindings
            .iter()
            .map(|(_, binding)| binding.clone())
            .collect(),
    )
}

fn scope(names: &[CompactString]) -> BindingScope<'_, impl Fn(&str) -> Option<ElementProperty>> {
    BindingScope {
        names,
        parameter: |_: &str| None,
    }
}

/// The `RETURN` items of `text` compiled as an aggregating projection: the columns (an item
/// is a key when it holds no aggregate call) and the calls.
fn aggregation<S: GraphStorage>(
    parsed: &Query,
    names: &[CompactString],
) -> (
    Vec<GqlAggregateColumn<S>>,
    Vec<crate::gql::aggregate::AggregateCall<GqlElement<S>>>,
) {
    let items = &parsed.ret.items;
    let compiled = match compile_aggregating(items.iter().map(|item| &item.expr), &scope(names)) {
        Ok(compiled) => compiled,
        Err(error) => panic!("does not compile: {error:?}"),
    };
    // An item is a key when compiling it alone records no aggregate call.
    let columns = items
        .iter()
        .zip(compiled.outputs)
        .map(|(item, expr)| {
            let alone: crate::gql::aggregate::Aggregating<GqlElement<S>> =
                compile_aggregating([&item.expr], &scope(names)).unwrap();
            GqlAggregateColumn {
                name: match &item.alias {
                    Some(alias) => alias.name.as_str().into(),
                    None => parsed.text(item.expr.span).into(),
                },
                expr,
                key: alone.calls.is_empty(),
            }
        })
        .collect();
    (columns, compiled.calls)
}

/// Runs `MATCH (n) RETURN <items>` aggregated over `start` with `n` the current element:
/// the rows as (column, value) lists in output order.
fn aggregate<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    text: &str,
) -> TraverserResult<Vec<Vec<(String, ElementProperty)>>> {
    let (query, parsed) = query(text);
    let (names, bindings) = names(&[("n", Binding::Current)]);
    let (columns, calls) = aggregation(&parsed, &names);
    let rows = start
        .gql_aggregate(query, columns, calls, bindings)
        .to_list()?;
    Ok(rows.into_iter().map(row).collect())
}

fn row(value: ElementProperty) -> Vec<(String, ElementProperty)> {
    match value {
        ElementProperty::Object(map) => map
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect(),
        other => panic!("not a row map: {other:?}"),
    }
}

fn int(value: i64) -> ElementProperty {
    ElementProperty::from(value)
}

#[test]
fn aggregate_groups_by_gql_equality_in_first_arrival_order() {
    let graph = graph();
    let rows = aggregate(
        graph.traversal().v(None).has_label("person"),
        "MATCH (n) RETURN n.age AS age, count(*) AS rows, count(n.name) AS names",
    )
    .unwrap();
    // 29, 27, 32 (josh and floatage: `32 = 32.0`), 35, NULL (nullage and noage).
    let ages: Vec<ElementProperty> = rows.iter().map(|row| row[0].1.clone()).collect();
    assert_eq!(
        ages,
        [int(29), int(27), int(32), int(35), ElementProperty::Null]
    );
    let counts: Vec<ElementProperty> = rows.iter().map(|row| row[1].1.clone()).collect();
    assert_eq!(counts, [int(1), int(1), int(2), int(1), int(2)]);
    for row in &rows {
        assert_eq!(row[1].1, row[2].1, "{row:?}");
        let keys: Vec<&str> = row.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["age", "rows", "names"]);
    }
}

#[test]
fn aggregate_skips_null_and_reads_keys_in_aggregate_columns() {
    let graph = graph();
    let rows = aggregate(
        graph.traversal().v(None).has_label("person"),
        "MATCH (n) RETURN n.age > 30 AS old, count(n.age) + 100 AS n, sum(n.age) AS s, \
         avg(n.age) AS a, min(n.name) AS lo, max(n.name) AS hi",
    )
    .unwrap();
    let find = |old: ElementProperty| {
        rows.iter()
            .find(|row| row[0].1 == old)
            .unwrap_or_else(|| panic!("no group {old:?} in {rows:?}"))
    };
    let young = find(ElementProperty::from(false));
    assert_eq!(young[1].1, int(102));
    assert_eq!(young[2].1, int(56));
    assert_eq!(young[3].1, ElementProperty::from(28.0));
    assert_eq!(young[4].1, ElementProperty::from("marko"));
    assert_eq!(young[5].1, ElementProperty::from("vadas"));
    let old = find(ElementProperty::from(true));
    // 32 + 35 + 32.0: a float among the inputs makes the sum a float.
    assert_eq!(old[1].1, int(103));
    assert_eq!(old[2].1, ElementProperty::from(99.0));
    // nullage and noage: the NULL group; count / sum skip NULL.
    let null = find(ElementProperty::Null);
    assert_eq!(null[1].1, int(100));
    assert_eq!(null[2].1, int(0));
    assert_eq!(null[3].1, ElementProperty::Null);
}

#[test]
fn aggregate_on_empty_input() {
    let graph = graph();
    let text = "MATCH (n) RETURN count(*) AS c, count(n) AS cn, sum(n.age) AS s, avg(n.age) AS a, \
                min(n.age) AS lo, collect_list(n.name) AS names";
    let rows = aggregate(graph.traversal().v("nobody"), text).unwrap();
    assert_eq!(
        rows,
        [vec![
            ("c".to_string(), int(0)),
            ("cn".to_string(), int(0)),
            ("s".to_string(), int(0)),
            ("a".to_string(), ElementProperty::Null),
            ("lo".to_string(), ElementProperty::Null),
            ("names".to_string(), ElementProperty::Array(Vec::new())),
        ]]
    );
    // With a grouping key there is no group, so no row.
    let rows = aggregate(
        graph.traversal().v("nobody"),
        "MATCH (n) RETURN n.age AS age, count(*) AS c",
    )
    .unwrap();
    assert!(rows.is_empty(), "{rows:?}");
}

#[test]
fn aggregate_honours_bulk_without_expanding() {
    let graph = graph();
    let text = "MATCH (n) RETURN n.name AS name, count(*) AS c, sum(1) AS s, \
                collect_list(n.lang) AS langs, count(DISTINCT n) AS d";
    // lop arrives three times (one traverser of bulk 3 after the barrier), ripple once.
    let build = |merge: bool| {
        aggregate(
            graph
                .traversal()
                .with("bulk.merge", merge)
                .v(None)
                .out(None)
                .has_label("software")
                .barrier(),
            text,
        )
        .unwrap()
    };
    let merged = build(true);
    assert_eq!(merged, build(false));
    let lop = merged
        .iter()
        .find(|row| row[0].1 == ElementProperty::from("lop"))
        .unwrap();
    assert_eq!(lop[1].1, int(3));
    assert_eq!(lop[2].1, int(3));
    assert_eq!(lop[3].1, ElementProperty::Array(vec!["java".into(); 3]));
    assert_eq!(lop[4].1, int(1));
    // The merged run really fed one traverser of bulk 3.
    let (query, parsed) = query(text);
    let (names, bindings) = names(&[("n", Binding::Current)]);
    let (columns, calls) = aggregation(&parsed, &names);
    let profile = graph
        .traversal()
        .v(None)
        .out(None)
        .has_label("software")
        .barrier()
        .gql_aggregate(query, columns, calls, bindings)
        .profile()
        .unwrap();
    let metrics = profile
        .metrics()
        .iter()
        .find(|m| m.name().starts_with("gql:aggregate"))
        .unwrap_or_else(|| panic!("no gql:aggregate in {profile}"));
    assert_eq!(metrics.counts().traverser_count(), 2, "{profile}");
    assert_eq!(
        metrics.name(),
        r#"gql:aggregate(["n.name"], ["count(*)", "sum(1)", "collect_list(n.lang)", "count(DISTINCT n)"], ["name", "c", "s", "langs", "d"])"#
    );
}

#[test]
fn aggregate_errors_and_limits() {
    let graph = graph();
    let error = aggregate(
        graph.traversal().v(None).has_label("person"),
        "MATCH (n) RETURN sum(n.name) AS s",
    )
    .unwrap_err();
    match error.root_cause() {
        TraverserError::Gql { gqlstatus, .. } => assert_eq!(gqlstatus, "22G03"),
        other => panic!("not a GQL error: {other}"),
    }
    // The lists collect_list builds are checked before they are built.
    let error = aggregate(
        graph
            .traversal()
            .with("traversal.max_materialized_bytes", 64)
            .v(None),
        "MATCH (n) RETURN collect_list(n) AS all",
    )
    .unwrap_err();
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::ResourceLimitExceeded { limit, .. }
                if limit == "traversal.max_materialized_bytes"
        ),
        "{error}"
    );
}

#[test]
fn aggregate_reads_labels_and_starts_fresh_paths() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN a.name AS person, count(n) AS made");
    let (names, bindings) = names(&[("n", Binding::Current), ("a", Binding::Label("a".into()))]);
    let (columns, calls) = aggregation(&parsed, &names);
    let build = || {
        graph
            .traversal()
            .v(None)
            .has_label("person")
            .as_("a")
            .as_("unused")
            .out("created")
            .gql_aggregate(
                Arc::clone(&query),
                columns
                    .iter()
                    .map(|c| GqlAggregateColumn {
                        name: c.name.clone(),
                        expr: c.expr.clone(),
                        key: c.key,
                    })
                    .collect(),
                calls.clone(),
                Arc::clone(&bindings),
            )
    };
    let mut rows: Vec<Vec<(String, ElementProperty)>> =
        build().to_list().unwrap().into_iter().map(row).collect();
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    let made: Vec<(ElementProperty, ElementProperty)> = rows
        .iter()
        .map(|row| (row[0].1.clone(), row[1].1.clone()))
        .collect();
    assert_eq!(
        made,
        [
            ("josh".into(), int(2)),
            ("marko".into(), int(1)),
            ("peter".into(), int(1)),
        ]
    );
    let profile = build().profile().unwrap().to_string();
    assert!(profile.contains("[path: labels(a)]"), "{profile}");
    assert!(!profile.contains("unused)]"), "{profile}");
    assert!(!profile.contains("[path: full]"), "{profile}");
    // A later step reads only what the aggregate's rows carry: its output paths start fresh,
    // so a full path after it holds the row map only.
    let paths = build().path().to_list().unwrap();
    for path in paths {
        let ElementProperty::Array(objects) = path else {
            panic!("not a path: {path:?}");
        };
        assert_eq!(objects.len(), 1, "{objects:?}");
    }
}

#[test]
fn distinct_uses_gql_equality() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN n.age AS age, n.lang AS lang");
    let (names, bindings) = names(&[("n", Binding::Current)]);
    let columns = parsed
        .ret
        .items
        .iter()
        .map(|item| GqlColumn {
            name: item.alias.as_ref().map_or("", |a| a.name.as_str()).into(),
            expr: compile(&item.expr, &scope(&names)).unwrap(),
        })
        .collect();
    let rows: Vec<Vec<(String, ElementProperty)>> = graph
        .traversal()
        .v(None)
        .gql_project(query, columns, bindings)
        .gql_distinct()
        .to_list()
        .unwrap()
        .into_iter()
        .map(row)
        .collect();
    let ages: Vec<ElementProperty> = rows.iter().map(|row| row[0].1.clone()).collect();
    // Persons 29, 27, 32 (and floatage's 32.0), 35, NULL (nullage, noage); the software rows
    // (NULL, "java") are one row; first arrival kept.
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert_eq!(
        ages.iter()
            .filter(|age| **age == ElementProperty::Null)
            .count(),
        2
    );
    assert!(ages.contains(&int(32)));
    assert!(!ages.contains(&ElementProperty::from(32.0)));
}

#[test]
fn distinct_keeps_one_row_of_a_merged_traverser() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN n.name AS name");
    let (names, bindings) = names(&[("n", Binding::Current)]);
    let columns = vec![GqlColumn {
        name: "name".into(),
        expr: compile(&parsed.ret.items[0].expr, &scope(&names)).unwrap(),
    }];
    let count = graph
        .traversal()
        .v(None)
        .out(None)
        .barrier()
        .gql_project(query, columns, bindings)
        .gql_distinct()
        .count()
        .next()
        .unwrap();
    // lop, vadas, josh, ripple.
    assert_eq!(count, Some(int(4)));
}

/// The rows of `aggregate` over every person by name with the count of software they created,
/// labelled `#w`.
fn made_rows(graph: &TraversalGraph) -> GraphTraversalSource<'_, &TraversalGraph> {
    let (query, parsed) = query("MATCH (n) RETURN n AS person, count(*) AS made, NULL AS nothing");
    let (names, bindings) = names(&[("n", Binding::Current)]);
    let (columns, calls) = aggregation(&parsed, &names);
    graph
        .traversal()
        .v(None)
        .has_label("person")
        .gql_aggregate(query, columns, calls, bindings)
        .as_("#w")
}

#[test]
fn bind_continues_from_a_row_column() {
    let graph = graph();
    let created = |column: usize, kind: ElementKind| {
        let mut names: Vec<String> = made_rows(&graph)
            .gql_bind(
                "a".into(),
                Binding::RowColumn {
                    label: "#w".into(),
                    column,
                },
                kind,
            )
            .out("created")
            .values("name")
            .to_list()
            .unwrap()
            .iter()
            .map(|value| value.as_string().unwrap_or("?").to_string())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        created(0, ElementKind::Vertex),
        ["lop", "lop", "lop", "ripple"]
    );
    // Not an element of the pattern's kind, a number, NULL, a missing column: no row.
    assert!(created(0, ElementKind::Edge).is_empty());
    assert!(created(1, ElementKind::Vertex).is_empty());
    assert!(created(2, ElementKind::Vertex).is_empty());
    assert!(created(9, ElementKind::Vertex).is_empty());

    let profile = made_rows(&graph)
        .gql_bind(
            "a".into(),
            Binding::RowColumn {
                label: "#w".into(),
                column: 0,
            },
            ElementKind::Vertex,
        )
        .out(None)
        .profile()
        .unwrap()
        .to_string();
    assert!(profile.contains(r#"gql:bind("a")"#), "{profile}");
    assert!(profile.contains("[path: labels(#w)]"), "{profile}");
    assert!(!profile.contains("[path: full]"), "{profile}");
}

#[test]
fn row_column_reads_a_labelled_row_in_later_steps() {
    let graph = graph();
    let column = |column: usize| Binding::RowColumn {
        label: "#w".into(),
        column,
    };
    let (names, bindings) = names(&[("p", column(0)), ("made", column(1))]);
    let (query, parsed) = query("MATCH (n) RETURN p.name AS name, made * 10 AS m");
    let columns = parsed
        .ret
        .items
        .iter()
        .map(|item| GqlColumn {
            name: item.alias.as_ref().map_or("", |a| a.name.as_str()).into(),
            expr: compile(&item.expr, &scope(&names)).unwrap(),
        })
        .collect();
    // After the row, the pattern walks on: the row is read back through its label.
    let rows: Vec<Vec<(String, ElementProperty)>> = made_rows(&graph)
        .select("person")
        .out(None)
        .gql_project(query, columns, bindings)
        .to_list()
        .unwrap()
        .into_iter()
        .map(row)
        .collect();
    // marko: 3 neighbours, josh: 2, peter: 1.
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert!(rows.contains(&vec![
        ("name".to_string(), ElementProperty::from("marko")),
        ("m".to_string(), int(10)),
    ]));
}

/// A graph a -> b -> a (edges `x1`, `x2`) and a self-loop c -> c (`x3`).
fn cycle() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let mut vertex = |id: &str| {
        graph
            .add_vertex(Some(id.into()), vec!["node".into()], PropertyMap::default())
            .unwrap()
    };
    let (a, b, c) = (vertex("a"), vertex("b"), vertex("c"));
    for (id, from, to) in [("x1", a, b), ("x2", b, a), ("x3", c, c)] {
        graph
            .add_edge(
                Some(id.into()),
                Some("next".into()),
                from,
                to,
                PropertyMap::default(),
            )
            .unwrap();
    }
    graph
}

/// The trail body of a quantified edge: one hop that uses no edge twice.
fn hop<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
    __::out_e(None)
        .gql_trail(vec!["#g".into()])
        .as_("#g")
        .in_v()
        .as_("#gv")
}

fn ids(values: Vec<ElementProperty>) -> Vec<String> {
    let mut ids: Vec<String> = values
        .iter()
        .map(|value| value.as_string().unwrap_or("?").to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn trail_drops_an_edge_used_before() {
    let graph = cycle();
    let reach = |times: u32, trail: bool| {
        let body = if trail {
            hop()
        } else {
            __::out_e(None).as_("#g").in_v()
        };
        ids(graph
            .traversal()
            .v(vec!["a", "c"])
            .repeat(body)
            .times(times)
            .id()
            .to_list()
            .unwrap())
    };
    assert_eq!(reach(2, false), ["a", "c"]);
    assert_eq!(reach(2, true), ["a"]);
    // a -x1-> b -x2-> a -x1-> b: the third hop reuses x1.
    assert_eq!(reach(3, false), ["b", "c"]);
    assert_eq!(reach(3, true), Vec::<String>::new());

    let profile = graph
        .traversal()
        .v("a")
        .repeat(hop())
        .times(2)
        .profile()
        .unwrap()
        .to_string();
    // The trail reads every earlier occurrence of `#g`: the `as("#g")` after it does not
    // retract the label, so the trail step itself keeps the path (labels only).
    assert!(
        profile.contains(r##"gql:trail("#g") [path: labels(#g)]"##),
        "{profile}"
    );
    assert!(profile.contains("[path: labels(#g)]"), "{profile}");
    assert!(!profile.contains("[path: full]"), "{profile}");
}

#[test]
fn group_and_path_bindings_assemble_in_text_order() {
    let graph = cycle();
    let (query, parsed) =
        query("MATCH (n) RETURN es AS es, size(es) AS k, path_length(p) AS len, p AS p");
    let (names, bindings) = names(&[
        (
            "es",
            Binding::Group {
                label: "#g".into(),
                anchor: Some("a".into()),
                reversed: false,
            },
        ),
        (
            "p",
            Binding::Path(Arc::from([
                PathPart::Element("a".into()),
                PathPart::Group {
                    edges: "#g".into(),
                    vertices: "#gv".into(),
                    reversed: false,
                },
                PathPart::Element("b".into()),
            ])),
        ),
    ]);
    let columns: Vec<GqlColumn<_>> = parsed
        .ret
        .items
        .iter()
        .map(|item| GqlColumn {
            name: item.alias.as_ref().map_or("", |a| a.name.as_str()).into(),
            expr: compile(&item.expr, &scope(&names)).unwrap(),
        })
        .collect();
    let build = |times: u32| {
        graph
            .traversal()
            .v("a")
            .as_("a")
            .emit()
            .repeat(hop())
            .times(times)
            .as_("b")
            .gql_project(
                Arc::clone(&query),
                columns
                    .iter()
                    .map(|c| GqlColumn {
                        name: c.name.clone(),
                        expr: c.expr.clone(),
                    })
                    .collect(),
                Arc::clone(&bindings),
            )
    };
    let rows: Vec<Vec<(String, ElementProperty)>> =
        build(2).to_list().unwrap().into_iter().map(row).collect();
    // Zero, one and two hops from a.
    assert_eq!(rows.len(), 3, "{rows:?}");
    for row in &rows {
        let ElementProperty::Array(edges) = &row[0].1 else {
            panic!("not a list: {row:?}");
        };
        let k = edges.len() as i64;
        assert_eq!(row[1].1, int(k), "{row:?}");
        assert_eq!(row[2].1, int(k), "{row:?}");
        let ElementProperty::Array(objects) = &row[3].1 else {
            panic!("not a path: {row:?}");
        };
        // a, then edge and vertex per hop; a zero-length match is the one vertex a.
        assert_eq!(objects.len() as i64, 2 * k + 1, "{row:?}");
        assert!(matches!(objects[0], ElementProperty::Vertex(_)));
        for (i, object) in objects.iter().enumerate() {
            if i % 2 == 1 {
                assert!(matches!(object, ElementProperty::Edge(_)), "{row:?}");
            } else {
                assert!(matches!(object, ElementProperty::Vertex(_)), "{row:?}");
            }
        }
    }
    let profile = build(2).profile().unwrap().to_string();
    assert!(!profile.contains("[path: full]"), "{profile}");
    assert!(profile.contains("#gv"), "{profile}");
}

#[test]
fn reversed_groups_and_missing_anchors() {
    let graph = cycle();
    // Walked from b back against x1 ... in walk order the group holds x2 then x1 with the
    // vertex a between; in text order (a)-[x1]->(b)-[x2]->(a) it is x1, b, x2.
    let (query, parsed) = query("MATCH (n) RETURN p AS p, es AS es");
    let (names, bindings) = names(&[
        (
            "p",
            Binding::Path(Arc::from([
                PathPart::Element("a".into()),
                PathPart::Group {
                    edges: "#g".into(),
                    vertices: "#gv".into(),
                    reversed: true,
                },
                PathPart::Element("b".into()),
            ])),
        ),
        (
            "es",
            Binding::Group {
                label: "#g".into(),
                anchor: Some("missing".into()),
                reversed: false,
            },
        ),
    ]);
    let columns: Vec<GqlColumn<_>> = parsed
        .ret
        .items
        .iter()
        .map(|item| GqlColumn {
            name: item.alias.as_ref().map_or("", |a| a.name.as_str()).into(),
            expr: compile(&item.expr, &scope(&names)).unwrap(),
        })
        .collect();
    // From b (= the text's end) walk in_e twice: x1 reaches a, then x2 reaches b.
    let back = __::in_e(None)
        .gql_trail(vec!["#g".into()])
        .as_("#g")
        .out_v()
        .as_("#gv");
    let rows: Vec<ElementProperty> = graph
        .traversal()
        .v("a")
        .as_("b")
        .repeat(back)
        .times(2)
        .as_("a")
        .gql_project(query, columns, bindings)
        .to_list()
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = row(rows[0].clone());
    let ElementProperty::Array(objects) = &row[0].1 else {
        panic!("not a path: {row:?}");
    };
    let ids: Vec<String> = objects
        .iter()
        .map(|object| match object {
            ElementProperty::Vertex(v) => v.id().to_string(),
            ElementProperty::Edge(e) => e.id().to_string(),
            other => panic!("not an element: {other:?}"),
        })
        .collect();
    // Walk: a <-x2- b <-x1- a, i.e. text order a -x1-> b -x2-> a.
    assert_eq!(ids, ["a", "x1", "b", "x2", "a"]);
    // The anchor label is absent: the group variable reads as NULL.
    assert_eq!(row[1].1, ElementProperty::Null);
}

#[test]
fn a_group_variable_read_after_the_loop_keeps_every_occurrence() {
    let graph = cycle();
    let (query, parsed) = query("MATCH (n) RETURN size(es) AS k");
    let (names, bindings) = names(&[(
        "es",
        Binding::Group {
            label: "#g".into(),
            anchor: None,
            reversed: false,
        },
    )]);
    let columns = [GqlColumn {
        name: "k".into(),
        expr: compile(&parsed.ret.items[0].expr, &scope(&names)).unwrap(),
    }];
    // No trail in the body: only the group variable after the loop reads `#g`, in every
    // occurrence, so no step before an `as("#g")` may drop the path.
    let build = || {
        graph
            .traversal()
            .v("a")
            .repeat(__::out_e(None).as_("#g").in_v())
            .times(3)
            .gql_project(
                Arc::clone(&query),
                columns
                    .iter()
                    .map(|c| GqlColumn {
                        name: c.name.clone(),
                        expr: c.expr.clone(),
                    })
                    .collect(),
                Arc::clone(&bindings),
            )
    };
    let rows: Vec<Vec<(String, ElementProperty)>> =
        build().to_list().unwrap().into_iter().map(row).collect();
    assert_eq!(rows, [vec![("k".to_string(), int(3))]]);
    let profile = build().profile().unwrap().to_string();
    assert!(profile.contains("out_e() [path: labels(#g)]"), "{profile}");
    assert!(!profile.contains("[path: full]"), "{profile}");
}
