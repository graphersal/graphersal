//! The internal `gql:*` steps on hand-built pipelines over the modern graph (the lowering that
//! builds them comes later): NULL semantics, column order and NULL columns, the marker error
//! before the run, the `ORDER BY` order, bulk propagation and the profile.

use std::sync::Arc;

use super::*;
use crate::gql::ast::{Expr, Query, SortDirection};
use crate::gql::eval::compile;
use crate::gql::parser::parse;
use crate::traversal::step::flatbarrier_steps::step_gql_order::GqlSortKey;
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::{TraversalGraph, TraverserResult};

/// The modern graph plus persons with a stored `null` age, a string age and no age.
fn graph() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "nullage")
        .property("age", ElementProperty::Null)
        .iterate()
        .unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "strage")
        .property("age", "x")
        .iterate()
        .unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "noage")
        .iterate()
        .unwrap();
    graph
}

fn query(text: &str) -> (Arc<str>, Query) {
    match parse(text) {
        Ok(parsed) => (Arc::from(text), parsed),
        Err(error) => panic!("{text:?} does not parse: {error}"),
    }
}

/// The variable names of `bindings`, then the bindings as the steps hold them.
fn slots(bindings: &[(&str, Binding)]) -> (Vec<CompactString>, Arc<[Binding]>) {
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

fn compiled<S: GraphStorage>(expr: &Expr, names: &[CompactString]) -> RowExpr<S> {
    let scope = BindingScope {
        names,
        parameter: |name: &str| (name == "min").then(|| ElementProperty::from(30)),
    };
    match compile(expr, &scope) {
        Ok(compiled) => compiled,
        Err(error) => panic!("does not compile: {error:?}"),
    }
}

/// `g.v().has_label("person")` filtered by the first `RETURN` item of `text` (the condition)
/// on the current vertex `n`: the names that pass, sorted.
fn passing(text: &str) -> Vec<String> {
    let graph = graph();
    let (query, parsed) = query(text);
    let (names, bindings) = slots(&[("n", Binding::Current)]);
    let condition = compiled(&parsed.ret.items[0].expr, &names);
    let result = graph
        .traversal()
        .v(None)
        .has_label("person")
        .gql_filter(query, condition, bindings)
        .values("name")
        .to_list();
    let mut names: Vec<String> = match result {
        Ok(values) => values
            .iter()
            .filter_map(|value| value.as_string().map(str::to_string))
            .collect(),
        Err(error) => panic!("{text}: {error}"),
    };
    names.sort();
    names
}

fn gql_error(result: TraverserResult<impl std::fmt::Debug>) -> (GqlErrorKind, String, String) {
    let error = match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    };
    match error.root_cause() {
        TraverserError::Gql {
            kind,
            message,
            gqlstatus,
            ..
        } => (*kind, message.clone(), gqlstatus.clone()),
        other => panic!("not a GQL error: {other}"),
    }
}

#[test]
fn filter_passes_only_on_true() {
    // A missing property, a stored null and a string age are NULL / incomparable: never TRUE.
    assert_eq!(passing("MATCH (n) RETURN n.age > 30"), ["josh", "peter"]);
    assert_eq!(passing("MATCH (n) RETURN n.age >= $min"), ["josh", "peter"]);
    assert_eq!(
        passing("MATCH (n) RETURN NOT (n.age > 30)"),
        ["marko", "vadas"]
    );
    // Cross-type `<>` is NULL, so the string age passes neither `=` nor `<>`.
    assert_eq!(
        passing("MATCH (n) RETURN n.age <> 29"),
        ["josh", "peter", "vadas"]
    );
    assert_eq!(passing("MATCH (n) RETURN n.age = 'x'"), ["strage"]);
    // Missing and stored null both read as NULL.
    assert_eq!(
        passing("MATCH (n) RETURN n.age IS NULL"),
        ["noage", "nullage"]
    );
    assert_eq!(
        passing("MATCH (n) RETURN n.age = NULL"),
        Vec::<String>::new()
    );
    assert_eq!(
        passing("MATCH (n) RETURN n.age IS NULL OR n.age < 28"),
        ["noage", "nullage", "vadas"]
    );
    assert_eq!(
        passing("MATCH (n) RETURN n:person AND element_id(n) = '1'"),
        ["marko"]
    );
}

#[test]
fn filter_reads_path_labels_and_declares_them() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN a.name = 'marko' AND n.lang = 'java'");
    let (names, bindings) = slots(&[("n", Binding::Current), ("a", Binding::Label("a".into()))]);
    let condition = compiled(&parsed.ret.items[0].expr, &names);
    let build = |condition| {
        graph
            .traversal()
            .v(None)
            .has_label("person")
            .as_("a")
            .out(None)
            .gql_filter(Arc::clone(&query), condition, Arc::clone(&bindings))
    };
    let names = build(condition.clone()).values("name").to_list().unwrap();
    assert_eq!(names, [ElementProperty::from("lop")]);

    let profile = build(condition).profile().unwrap().to_string();
    assert!(
        profile.contains(r#"gql:filter("a.name = 'marko' AND n.lang = 'java'")"#),
        "{profile}"
    );
    // The `as("a")` records its label because the filter reads it (rule 13).
    assert!(profile.contains("[path: labels(a)]"), "{profile}");
}

#[test]
fn filter_errors_carry_the_gqlstatus() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN 100 / (n.age - 29) > 1");
    let (names, bindings) = slots(&[("n", Binding::Current)]);
    let condition = compiled(&parsed.ret.items[0].expr, &names);
    let result = graph
        .traversal()
        .v("1")
        .gql_filter(query, condition, bindings)
        .to_list();
    let (kind, message, status) = gql_error(result);
    assert_eq!(kind, GqlErrorKind::Data);
    assert_eq!(status, "22012");
    assert_eq!(message, "division by zero");
}

/// `MATCH (n:person) RETURN <text items>` as row maps: (keys in order, values) per row, sorted.
fn rows(text: &str) -> Vec<Vec<(String, ElementProperty)>> {
    let graph = graph();
    let (query, parsed) = query(text);
    let (names, bindings) = slots(&[("n", Binding::Current)]);
    let columns = parsed
        .ret
        .items
        .iter()
        .map(|item| GqlColumn {
            name: match &item.alias {
                Some(alias) => alias.name.as_str().into(),
                None => parsed.text(item.expr.span).into(),
            },
            expr: compiled(&item.expr, &names),
        })
        .collect();
    let result = graph
        .traversal()
        .v(None)
        .has_label("person")
        .gql_project(query, columns, bindings)
        .to_list()
        .unwrap();
    let mut rows: Vec<Vec<(String, ElementProperty)>> = result
        .into_iter()
        .map(|row| match row {
            ElementProperty::Object(map) => map
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
            other => panic!("not a row map: {other:?}"),
        })
        .collect();
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    rows
}

#[test]
fn project_keeps_written_order_and_null_columns() {
    let rows = rows(
        "MATCH (n) RETURN n.name AS name, n.age, \
         CASE WHEN n.age = 'x' THEN 0 ELSE n.age * 2 END AS twice, $min AS p",
    );
    assert_eq!(rows.len(), 7);
    for row in &rows {
        let keys: Vec<&str> = row.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["name", "n.age", "twice", "p"]);
        assert_eq!(row[3].1, ElementProperty::from(30));
    }
    let row = |name: &str| {
        rows.iter()
            .find(|row| row[0].1 == ElementProperty::from(name))
            .unwrap_or_else(|| panic!("no row {name}"))
    };
    assert_eq!(row("marko")[1].1, ElementProperty::from(29));
    assert_eq!(row("marko")[2].1, ElementProperty::from(58));
    // A missing and a stored null value are both a null column, never a dropped key.
    assert_eq!(row("noage")[1].1, ElementProperty::Null);
    assert_eq!(row("noage")[2].1, ElementProperty::Null);
    assert_eq!(row("nullage")[1].1, ElementProperty::Null);
}

#[test]
fn project_errors_fail_the_traversal() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN n.age + 1");
    let (names, bindings) = slots(&[("n", Binding::Current)]);
    let columns = vec![GqlColumn {
        name: "x".into(),
        expr: compiled(&parsed.ret.items[0].expr, &names),
    }];
    let result = graph
        .traversal()
        .v(None)
        .has("name", "strage")
        .gql_project(query, columns, bindings)
        .to_list();
    let (kind, _, status) = gql_error(result);
    assert_eq!(kind, GqlErrorKind::Data);
    assert_eq!(status, "22G03");
}

#[test]
fn projected_elements_are_live_for_gremlin() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN n AS p, labels(n) AS l, [n] AS list");
    let (names, bindings) = slots(&[("n", Binding::Current)]);
    let column = |index: usize, name: &str| GqlColumn {
        name: name.into(),
        expr: compiled(&parsed.ret.items[index].expr, &names),
    };
    let build = || {
        graph.traversal().v("1").gql_project(
            Arc::clone(&query),
            vec![column(0, "p"), column(1, "l"), column(2, "list")],
            Arc::clone(&bindings),
        )
    };
    // `select("p")` yields the vertex itself: Gremlin walks on from it.
    let mut created = build()
        .select("p")
        .out(None)
        .values("name")
        .to_list()
        .unwrap();
    created.sort_by_key(|value| value.as_string().map(str::to_string));
    assert_eq!(
        created,
        ["josh", "lop", "vadas"].map(ElementProperty::from).to_vec()
    );
    let row = build().next().unwrap();
    let Some(ElementProperty::Object(row)) = row else {
        panic!("not a row: {row:?}");
    };
    // The element column materializes like `project()`'s: the vertex value.
    assert!(matches!(row.get("p"), Some(ElementProperty::Vertex(_))));
    assert_eq!(
        row.get("l"),
        Some(&ElementProperty::Array(vec!["person".into()]))
    );
    assert!(
        matches!(row.get("list"), Some(ElementProperty::Array(items)) if matches!(items[..], [ElementProperty::Vertex(_)]))
    );
}

/// Names in the order of `gql:order` over the projected rows `name, age` of every person.
fn ordered(text: &str) -> Vec<String> {
    let graph = graph();
    let (query, parsed) = query(text);
    let (names, project_bindings) = slots(&[("n", Binding::Current)]);
    let columns = parsed
        .ret
        .items
        .iter()
        .map(|item| GqlColumn {
            name: item.alias.as_ref().map_or("", |a| a.name.as_str()).into(),
            expr: compiled(&item.expr, &names),
        })
        .collect();
    // ORDER BY reads the columns by alias, and `n` through its step label.
    let (order_names, order_bindings) = slots(&[
        ("name", Binding::Column(0)),
        ("age", Binding::Column(1)),
        ("n", Binding::Label("n".into())),
    ]);
    let keys = parsed
        .order_by
        .as_ref()
        .map(|order| {
            order
                .items
                .iter()
                .map(|item| GqlSortKey {
                    expr: compiled(&item.expr, &order_names),
                    descending: item.direction == SortDirection::Desc,
                })
                .collect()
        })
        .unwrap_or_default();
    let result = graph
        .traversal()
        .v(None)
        .has_label("person")
        .as_("n")
        .gql_project(Arc::clone(&query), columns, project_bindings)
        .gql_order(query, keys, order_bindings)
        .select("name")
        .to_list()
        .unwrap();
    result
        .iter()
        .map(|name| name.as_string().unwrap_or("?").to_string())
        .collect()
}

#[test]
fn order_puts_nulls_last_ascending_and_first_descending() {
    // Ties (the two NULL ages) keep their input order (the sort is stable).
    assert_eq!(
        ordered("MATCH (n) RETURN n.name AS name, n.age AS age ORDER BY age"),
        [
            "strage", "vadas", "marko", "josh", "peter", "nullage", "noage"
        ]
    );
    assert_eq!(
        ordered("MATCH (n) RETURN n.name AS name, n.age AS age ORDER BY age DESC"),
        [
            "nullage", "noage", "peter", "josh", "marko", "vadas", "strage"
        ]
    );
    // A key over a path label not returned, then a tie breaker on a column.
    assert_eq!(
        ordered(
            "MATCH (n) RETURN n.name AS name, n.age AS age \
             ORDER BY n.age IS NULL DESC, name DESC"
        ),
        [
            "nullage", "noage", "vadas", "strage", "peter", "marko", "josh"
        ]
    );
}

#[test]
fn order_of_types_is_total_and_fixed() {
    let vertex = OrderValue::Element(ElementKind::Vertex, "2".into());
    let ascending = [
        OrderValue::Map(vec![("a".into(), OrderValue::Null)]),
        OrderValue::Element(ElementKind::Vertex, "1".into()),
        vertex.clone(),
        OrderValue::Element(ElementKind::Edge, "1".into()),
        OrderValue::List(vec![]),
        OrderValue::List(vec![OrderValue::Scalar(1.into())]),
        OrderValue::Scalar(ElementProperty::Uuid(7)),
        OrderValue::Scalar("a".into()),
        OrderValue::Scalar("b".into()),
        OrderValue::Scalar(false.into()),
        OrderValue::Scalar(true.into()),
        OrderValue::Scalar(1.into()),
        OrderValue::Scalar(1.5.into()),
        OrderValue::Scalar(2.into()),
        OrderValue::Null,
    ];
    for (i, a) in ascending.iter().enumerate() {
        for (j, b) in ascending.iter().enumerate() {
            assert_eq!(compare_order(a, b), i.cmp(&j), "{a:?} vs {b:?}");
        }
    }
    // Integers and floats of one value are equal.
    assert_eq!(
        compare_order(
            &OrderValue::Scalar(2.into()),
            &OrderValue::Scalar(2.0.into())
        ),
        Ordering::Equal
    );
}

#[test]
fn marker_fails_before_the_run() {
    let graph = graph();
    let error = || match parse("MATCH (n) RETRUN n") {
        Err(error) => error,
        Ok(_) => panic!("parses"),
    };
    // No traverser reaches the marker: it fails anyway, in the plan-time pass.
    let result = graph
        .traversal()
        .v(None)
        .has("name", "nobody")
        .gql_error(error())
        .to_list();
    let (kind, message, status) = gql_error(result);
    assert_eq!(kind, GqlErrorKind::Syntax);
    assert_eq!(status, "42001");
    assert!(message.contains("RETRUN"), "{message}");
    // On an empty graph too, and nothing after it runs.
    let empty = TraversalGraph::new();
    let result = empty.traversal().v(None).gql_error(error()).count().next();
    assert_eq!(gql_error(result).0, GqlErrorKind::Syntax);
    // The located error names the marker step and keeps the caret excerpt.
    let Err(located) = graph.traversal().v(None).gql_error(error()).to_list() else {
        panic!("no error");
    };
    let report = crate::error::format_diagnostic(&located);
    assert!(report.contains("gql:error(\"42001\")"), "{report}");
    assert!(report.contains("RETRUN"), "{report}");
    assert!(report.contains('^'), "{report}");
}

#[test]
fn bulk_passes_through_every_step() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN n.lang = 'java', n.name AS name ORDER BY name");
    let (names, bindings) = slots(&[("n", Binding::Current)]);
    let condition = compiled(&parsed.ret.items[0].expr, &names);
    let columns = vec![GqlColumn {
        name: "name".into(),
        expr: compiled(&parsed.ret.items[1].expr, &names),
    }];
    let (order_names, order_bindings) = slots(&[("name", Binding::Column(0))]);
    let keys = vec![GqlSortKey {
        expr: compiled(
            &parsed.order_by.as_ref().unwrap().items[0].expr,
            &order_names,
        ),
        descending: true,
    }];
    let build = || {
        graph
            .traversal()
            .v(None)
            .out(None)
            .barrier()
            .gql_filter(Arc::clone(&query), condition.clone(), Arc::clone(&bindings))
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
            .gql_order(
                Arc::clone(&query),
                keys.iter()
                    .map(|k| GqlSortKey {
                        expr: k.expr.clone(),
                        descending: k.descending,
                    })
                    .collect(),
                Arc::clone(&order_bindings),
            )
    };
    // lop three times (merged into one traverser of bulk 3) and ripple once.
    let names: Vec<String> = build()
        .select("name")
        .to_list()
        .unwrap()
        .iter()
        .map(|v| v.as_string().unwrap_or("?").to_string())
        .collect();
    assert_eq!(names, ["ripple", "lop", "lop", "lop"]);
    let profile = build().profile().unwrap();
    for step in ["gql:filter", "gql:project", "gql:order"] {
        let metrics = profile
            .metrics()
            .iter()
            .find(|m| m.name().starts_with(step))
            .unwrap_or_else(|| panic!("no {step} in {profile}"));
        assert_eq!(metrics.counts().traverser_count(), 2, "{step}: {profile}");
        assert_eq!(metrics.counts().element_count(), 4, "{step}: {profile}");
    }
    let text = profile.to_string();
    assert!(text.contains(r#"gql:project("name")"#), "{text}");
    assert!(text.contains(r#"gql:order("name DESC")"#), "{text}");
}

#[test]
fn camel_case_respelling_keeps_the_gql_names_and_text() {
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN element_id(n) = '1'");
    let (names, bindings) = slots(&[("n", Binding::Current)]);
    let condition = compiled(&parsed.ret.items[0].expr, &names);
    let profile = graph
        .traversal()
        .v(None)
        .has_label("person")
        .gql_filter(query, condition, bindings)
        .out_e(None)
        .profile()
        .unwrap()
        .to_string();
    let camel = crate::traversal::spelling::respell(&profile, crate::traversal::Spelling::Camel);
    assert!(
        camel.contains(r#"gql:filter("element_id(n) = '1'")"#),
        "{camel}"
    );
    assert!(camel.contains("outE("), "{camel}");
}

#[test]
fn source_steps_stay_untouched_by_the_optimizer() {
    // `filter_reorder` puts a cheap `has_label` before the GQL filter of the same run, and the
    // GQL filter never moves across the `as()` that sets the label it reads.
    let graph = graph();
    let (query, parsed) = query("MATCH (n) RETURN a.age > 30");
    let (names, bindings) = slots(&[("a", Binding::Label("a".into()))]);
    let condition = compiled(&parsed.ret.items[0].expr, &names);
    let profile = graph
        .traversal()
        .v(None)
        .as_("a")
        .gql_filter(query, condition, bindings)
        .has_label("person")
        .count()
        .profile()
        .unwrap();
    let steps: Vec<&str> = profile.metrics().iter().map(|m| m.name()).collect();
    let position = |prefix: &str| {
        steps
            .iter()
            .position(|name| name.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {steps:?}"))
    };
    assert!(position("as(") < position("has_label("), "{steps:?}");
    assert!(
        position("has_label(") < position("gql:filter("),
        "{steps:?}"
    );
}
