//! The resolution of calls of saved queries (stage 3h): the synthesized `CALL` clause, the
//! standalone `RETURN`, the deferred mode, and the bounds.

use super::*;
use crate::gql::ast::QueryEnd;
use crate::gql::planner::NoFacts;
use crate::graph::schema::PropertySchema;

/// A catalog of saved GQL queries.
struct Catalog(Vec<(String, QueryDefinition)>);

/// One saved query of a [`Catalog`]: name, folder, body, parameters in declaration order.
type Saved<'a> = (&'a str, Option<&'a str>, &'a str, &'a [(&'a str, JsonType)]);

impl Catalog {
    fn new(queries: &[Saved<'_>]) -> Self {
        Catalog(
            queries
                .iter()
                .map(|(name, folder, body, params)| {
                    let mut query = QueryDefinition::new(*body).with_dialect(GQL_QUERY_DIALECT);
                    if let Some(folder) = folder {
                        query = query.with_folder(*folder);
                    }
                    for (p, ty) in params.iter() {
                        query = query.with_param(*p, QueryParam::new(PropertySchema::of(*ty)));
                    }
                    (name.to_string(), query)
                })
                .collect(),
        )
    }
}

impl Procedures for Catalog {
    fn saved_query(&self, name: &str) -> Option<QueryDefinition> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, q)| q.clone())
    }
}

fn catalog() -> Catalog {
    Catalog::new(&[
        (
            "pair",
            Some("t"),
            "RETURN $a AS a, $a + 1 AS b",
            &[("a", JsonType::Integer)],
        ),
        ("nothing", Some("t"), "FOR x IN [] FINISH", &[]),
    ])
}

fn resolved(text: &str, catalog: &Catalog) -> Result<(Query, Vec<ResolvedCall>), TraverserError> {
    let mut query = parse(text)?;
    let calls = resolve(&mut query, Some(&[]), &Resolver::new(catalog, &NoFacts))?;
    Ok((query, calls))
}

fn message(error: TraverserError) -> String {
    match error {
        TraverserError::Gql { message, .. } => message,
        other => other.to_string(),
    }
}

#[test]
fn a_call_becomes_a_call_clause_of_its_shape() {
    let (query, calls) =
        resolved("CALL t.pair(1) YIELD b AS x, a RETURN x, a", &catalog()).unwrap();
    let Some(Stage::Call(clause)) = query.stages.first() else {
        panic!("{:?}", query.stages)
    };
    let procedure = clause.procedure.as_ref().expect("a resolved call");
    assert_eq!((procedure.index, procedure.writes), (0, false));
    assert_eq!(procedure.dotted, "t.pair");
    let ret = &clause.body.first.first.ret;
    let names: Vec<&str> = ret
        .items
        .iter()
        .filter_map(|item| item.alias.as_ref().map(|a| a.name.as_str()))
        .collect();
    assert_eq!(names, ["x", "a"]);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].path, "t/pair");
    assert_eq!(
        calls[0].outputs,
        [(CompactString::new("x"), 1), (CompactString::new("a"), 0)]
    );
    let callee = calls[0].callee.as_ref().expect("planned");
    assert_eq!(
        callee.params,
        [(CompactString::new("a"), ElementProperty::from(1))]
    );
}

#[test]
fn a_standalone_call_gets_the_return_of_its_columns() {
    let (query, _) = resolved("CALL t.pair(1)", &catalog()).unwrap();
    let names: Vec<&str> = query
        .ret
        .items
        .iter()
        .filter_map(|item| item.alias.as_ref().map(|a| a.name.as_str()))
        .collect();
    assert_eq!(names, ["a", "b"]);
    let (query, calls) = resolved("CALL t.nothing", &catalog()).unwrap();
    assert!(
        matches!(query.end(), QueryEnd::Finish(_)),
        "{:?}",
        query.ret
    );
    assert!(calls[0].unit);
    let Some(Stage::Call(clause)) = query.stages.first() else {
        panic!()
    };
    assert!(matches!(clause.body.first.first.end(), QueryEnd::Finish(_)));
}

#[test]
fn a_query_without_calls_is_left_alone() {
    let mut query = parse("MATCH (n) CALL (n) { RETURN 1 AS x } RETURN x").unwrap();
    let before = query.clone();
    let calls = resolve(&mut query, None, &Resolver::new(&NoProcedures, &NoFacts)).unwrap();
    assert!(calls.is_empty());
    assert_eq!(query, before);
}

#[test]
fn calls_in_call_bodies_and_union_operands_are_resolved() {
    let (query, calls) = resolved(
        "CALL () { CALL t.pair(2) YIELD a RETURN a } RETURN a UNION CALL t.pair(3) YIELD a RETURN a",
        &catalog(),
    )
    .unwrap();
    assert_eq!(calls.len(), 2);
    let Some(Stage::Call(outer)) = query.stages.first() else {
        panic!()
    };
    assert!(outer.procedure.is_none());
    assert!(matches!(
        outer.body.first.first.stages.first(),
        Some(Stage::Call(inner)) if inner.procedure.is_some()
    ));
}

#[test]
fn deferred_resolution_needs_no_catalog() {
    let mut query = parse("CALL a.b.c($x, 1) YIELD p AS q, r RETURN q, r").unwrap();
    let mut resolver = Resolver::new(&NoProcedures, &NoFacts);
    resolver.deferred = true;
    let calls = resolve(&mut query, None, &resolver).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (calls[0].name.as_str(), calls[0].path.as_str()),
        ("c", "a/b/c")
    );
    assert!(calls[0].callee.is_none());
    let Some(Stage::Call(clause)) = query.stages.first() else {
        panic!()
    };
    let procedure = clause.procedure.as_ref().unwrap();
    assert_eq!(procedure.parameters.len(), 1);
    assert_eq!(procedure.parameters[0].0, "x");
    // Aggregates and reads of the row are refused there too.
    let mut query = parse("MATCH (n) CALL a.b(count(n)) YIELD p RETURN p").unwrap();
    let error = resolve(&mut query, None, &resolver).err().expect("refused");
    assert!(message(error).contains("cannot aggregate"));
}

#[test]
fn a_call_outside_its_place_is_refused() {
    // A standalone-looking call in a NEXT block is a part of a larger query.
    let error = resolved("RETURN 1 AS x NEXT CALL t.pair(1)", &catalog())
        .err()
        .expect("refused");
    assert!(message(error).contains("without RETURN is a whole query"));
}

#[test]
fn the_number_of_resolved_calls_is_bounded() {
    // q1 calls q0 32 times; the query calls q1 40 times: far more than MAX_RESOLVED_CALLS.
    let q1: String = (0..32)
        .map(|i| format!("CALL q0() YIELD x AS x{i} "))
        .collect::<String>()
        + "RETURN 1 AS x";
    let catalog = Catalog::new(&[
        ("q0", None, "RETURN 1 AS x", &[]),
        ("q1", None, q1.as_str(), &[]),
    ]);
    let query: String = (0..40)
        .map(|i| format!("CALL q1() YIELD x AS y{i} "))
        .collect::<String>()
        + "RETURN 1 AS z";
    let error = resolved(&query, &catalog).err().expect("refused");
    match error {
        TraverserError::Gql {
            gqlstatus, message, ..
        } => {
            assert_eq!(gqlstatus, "54001", "{message}");
            assert!(
                message.contains(&MAX_RESOLVED_CALLS.to_string()),
                "{message}"
            );
        }
        other => panic!("{other}"),
    }
}

#[test]
fn type_text_names_the_json_types() {
    let nullable = QueryParam::new(PropertySchema::of_types(&[
        JsonType::Integer,
        JsonType::Null,
    ]));
    assert_eq!(type_text(&nullable), "integer or null");
    assert_eq!(
        type_text(&QueryParam::new(PropertySchema::of(JsonType::String))),
        "string"
    );
    assert_eq!(qualified(Some("my reports/x"), "q"), "`my reports`.x.q");
    assert_eq!(qualified(None, "q"), "q");
}
