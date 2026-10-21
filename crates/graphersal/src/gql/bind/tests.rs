use super::*;
use crate::gql::parser::parse;

// ----- helpers -----

fn ok(query: &str) -> BoundQuery {
    let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
    bind(&parsed).unwrap_or_else(|e| panic!("{query:?} does not bind: {e} ({e:?})"))
}

/// The parts of a binder error a test looks at.
#[derive(Debug)]
struct Failure {
    kind: GqlErrorKind,
    message: String,
    /// The underlined text.
    text: String,
    gqlstatus: String,
    suggestion: Option<String>,
    alternative: Option<String>,
}

fn failure(query: &str, result: Result<BoundQuery, TraverserError>) -> Failure {
    match result {
        Ok(bound) => panic!("{query:?} bound: {bound:?}"),
        Err(TraverserError::Gql {
            kind,
            message,
            offset,
            width,
            gqlstatus,
            context,
            ..
        }) => Failure {
            kind,
            message,
            text: query
                .get(offset..)
                .unwrap_or("")
                .chars()
                .take(width)
                .collect(),
            gqlstatus,
            suggestion: context.suggestion.clone(),
            alternative: context.alternative.clone(),
        },
        Err(other) => panic!("{query:?}: not a GQL error: {other:?}"),
    }
}

fn fail(query: &str) -> Failure {
    let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
    let f = failure(query, bind(&parsed));
    assert_eq!(f.kind, GqlErrorKind::Semantic, "{query:?}: {f:?}");
    f
}

fn id(q: &BoundQuery, name: &str) -> BindingId {
    q.lookup(name)
        .unwrap_or_else(|| panic!("no binding `{name}`"))
}

fn names(q: &BoundQuery, ids: &[BindingId]) -> Vec<String> {
    ids.iter()
        .map(|i| {
            q.binding(*i)
                .and_then(|b| b.name.as_ref())
                .map_or_else(|| format!("#{}", i.0), |n| n.name.clone())
        })
        .collect()
}

fn column_names(q: &BoundQuery) -> Vec<&str> {
    q.columns.iter().map(|c| c.name.as_str()).collect()
}

/// The text of each conjunct with its binding names: `"a.age > 30 [a]"`.
fn conjuncts(query: &str, q: &BoundQuery) -> Vec<String> {
    q.conjuncts
        .iter()
        .map(|c| {
            format!(
                "{} {:?}",
                query.get(c.span.start..c.span.end).unwrap_or("?"),
                names(q, &c.bindings)
            )
        })
        .collect()
}

// ----- bindings and joins -----

#[test]
fn bindings_follow_first_appearance_and_repeats_join() {
    let src = "MATCH (a:person)-[e:knows]->(b), (b)-[:created]->(c) MATCH (a)-->(c:software) \
               RETURN c";
    let q = ok(src);
    let named: Vec<_> = q
        .bindings
        .iter()
        .map(|b| (b.name.as_ref().map(|n| n.name.clone()), b.kind))
        .collect();
    assert_eq!(
        named,
        vec![
            (Some("a".into()), BindingKind::Node),
            (Some("e".into()), BindingKind::Edge),
            (Some("b".into()), BindingKind::Node),
            (None, BindingKind::Edge),
            (Some("c".into()), BindingKind::Node),
            (None, BindingKind::Edge),
        ]
    );
    for (i, b) in q.bindings.iter().enumerate() {
        assert_eq!(b.id, BindingId(i));
    }
    let (a, b, c) = (id(&q, "a"), id(&q, "b"), id(&q, "c"));
    assert!(q.binding(a).is_some_and(Binding::joined));
    assert!(q.binding(b).is_some_and(Binding::joined));
    assert!(q.binding(c).is_some_and(Binding::joined));
    assert!(!q.binding(id(&q, "e")).is_some_and(Binding::joined));
    // c's labels come from its second occurrence; a's from its first.
    assert_eq!(q.binding(c).map(|b| b.labels.len()), Some(1));
    assert_eq!(q.binding(c).map(|b| b.clause), Some(0));
    // The structure refers to the same ids.
    assert_eq!(q.matches.len(), 2);
    assert_eq!(q.matches[0].paths[1].start, b);
    assert_eq!(q.matches[1].paths[0].start, a);
    assert_eq!(q.matches[1].paths[0].steps[0].node, c);
    assert_eq!(
        q.matches[0].paths[0].steps[0].direction,
        EdgeDirection::Right
    );
    // A cycle in one pattern is a join as well.
    let q = ok("MATCH (a)-[:knows]->(b)-[:knows]->(a) RETURN b");
    assert_eq!(q.bindings.len(), 4);
    assert_eq!(q.matches[0].paths[0].steps[1].node, id(&q, "a"));
}

#[test]
fn kind_conflicts_are_semantic_errors() {
    let f = fail("MATCH (a)-[a]->(b) RETURN b");
    assert_eq!(f.text, "a");
    assert_eq!(f.gqlstatus, "42002");
    assert!(f.message.contains("used as an edge"), "{}", f.message);
    let f = fail("MATCH (x)-[r]->(y) MATCH (r) RETURN x");
    assert!(f.message.contains("used as a node"), "{}", f.message);
}

#[test]
fn an_edge_variable_twice_in_one_match_is_refused_but_joins_across_matches() {
    let src = "MATCH (a)-[r]->(b), (c)-[r]->(d) RETURN a";
    let f = fail(src);
    assert!(f.message.contains("more than one edge"), "{}", f.message);
    assert_eq!(f.text, "r");
    assert!(f.alternative.is_some());
    let f = fail("MATCH (a)-[r]->(b)-[r]->(c) RETURN a");
    assert!(f.message.contains("`r`"));
    let q = ok("MATCH (a)-[r]->(b) MATCH (b)<-[r]-(a) RETURN r");
    assert!(q.binding(id(&q, "r")).is_some_and(Binding::joined));
}

#[test]
fn undefined_variables_get_a_suggestion_from_the_scope() {
    let f = fail("MATCH (person)-->(friend) WHERE persn.age > 3 RETURN friend");
    assert_eq!(f.text, "persn");
    assert_eq!(f.suggestion.as_deref(), Some("person"));
    assert_eq!(f.gqlstatus, "42002");
    let f = fail("MATCH (n) RETURN m");
    assert_eq!(f.suggestion.as_deref(), Some("n"));
    let f = fail("MATCH (Node) RETURN node");
    assert_eq!(f.suggestion.as_deref(), Some("Node"));
    let f = fail("MATCH (n) RETURN n ORDER BY zzz");
    assert_eq!(f.text, "zzz");
    assert_eq!(f.suggestion, None);
    // ORDER BY suggests aliases too.
    let f = fail("MATCH (n) RETURN n.name AS title ORDER BY titel");
    assert_eq!(f.suggestion.as_deref(), Some("title"));
    // A variable of a later MATCH is not in scope in an earlier WHERE.
    let f = fail("MATCH (a) WHERE a.x = b.x MATCH (b) RETURN a");
    assert_eq!(f.text, "b");
    assert!(f.message.contains("later MATCH"), "{}", f.message);
    // Inside every expression form.
    for src in [
        "MATCH (a) WHERE [1, q] = [] RETURN a",
        "MATCH (a) WHERE {k: q} = {} RETURN a",
        "MATCH (a) WHERE NOT q RETURN a",
        "MATCH (a) WHERE q IS NULL RETURN a",
        "MATCH (a) WHERE q IS LABELED x RETURN a",
        "MATCH (a) RETURN CASE q WHEN 1 THEN 2 END",
        "MATCH (a) RETURN CASE WHEN a.x THEN q END",
        "MATCH (a) RETURN CASE WHEN a.x THEN 1 ELSE q END",
        "MATCH (a) RETURN coalesce(a.x, q)",
        "MATCH (a {k: q}) RETURN a",
        "MATCH (a WHERE q.k = 1) RETURN a",
        "MATCH (a)-[e {k: q.k}]->(b) RETURN a",
    ] {
        assert_eq!(fail(src).text, "q", "{src}");
    }
}

// ----- read flags (D11) -----

#[test]
fn read_flags_mark_bindings_read_away_from_their_position() {
    let q = ok("MATCH (a:person {name: 'x'})-[e]->(b)-->(c) WHERE a.age > 30 RETURN b.name");
    let read = |n: &str| q.binding(id(&q, n)).is_some_and(|b| b.read);
    // a: only single-binding filters; e, c: never read; b: RETURN.
    assert!(!read("a"));
    assert!(!read("e"));
    assert!(read("b"));
    assert!(!read("c"));
    // Cross-binding WHERE, inline WHERE reading another variable, ORDER BY.
    let q = ok(
        "MATCH (a)-->(b)-->(c WHERE c.x = a.x), (d), (f) WHERE a.k < b.k RETURN 1 AS one \
         ORDER BY d.k",
    );
    let read = |n: &str| q.binding(id(&q, n)).is_some_and(|b| b.read);
    assert!(read("a") && read("b") && read("c") && read("d"));
    assert!(!read("f"));
    // A join alone does not set `read` (the planner decides), but `joined`.
    let q = ok("MATCH (a)-->(b) MATCH (b)-->(c) RETURN c");
    let b = q.binding(id(&q, "b"));
    assert!(b.is_some_and(|b| !b.read && b.joined()));
}

// ----- conjuncts -----

#[test]
fn filters_split_into_conjuncts_tagged_with_bindings() {
    let src = "MATCH (a:person {name: $n, age: 3})-[e:knows WHERE e.w > 0.5]->(b WHERE b.x = 1 \
               AND b.y = a.y) WHERE a.age > 30 AND (b.age < 20 OR a.age > 1) AND $p AND \
               a.k = b.k RETURN b";
    let q = ok(src);
    assert_eq!(
        conjuncts(src, &q),
        vec![
            r#"name: $n ["a"]"#,
            r#"age: 3 ["a"]"#,
            r#"e.w > 0.5 ["e"]"#,
            r#"b.x = 1 ["b"]"#,
            r#"b.y = a.y ["a", "b"]"#,
            r#"a.age > 30 ["a"]"#,
            r#"(b.age < 20 OR a.age > 1) ["a", "b"]"#,
            r#"$p []"#,
            r#"a.k = b.k ["a", "b"]"#,
        ]
    );
    let origins: Vec<_> = q.conjuncts.iter().map(|c| c.origin).collect();
    use ConjunctOrigin::*;
    assert_eq!(
        origins,
        vec![
            InlineProperty,
            InlineProperty,
            InlineWhere,
            InlineWhere,
            InlineWhere,
            Where,
            Where,
            Where,
            Where
        ]
    );
    // Per-binding lists and the cross list partition the conjuncts.
    let a = q.binding(id(&q, "a")).map(|b| b.predicates.clone());
    assert_eq!(a, Some(vec![0, 1, 5]));
    assert_eq!(
        q.binding(id(&q, "e")).map(|b| b.predicates.clone()),
        Some(vec![2])
    );
    assert_eq!(
        q.binding(id(&q, "b")).map(|b| b.predicates.clone()),
        Some(vec![3])
    );
    assert_eq!(q.cross, vec![4, 6, 7, 8]);
    // The inline property form keeps its binding, key and value.
    match &q.conjuncts[0].predicate {
        Predicate::Property {
            binding,
            key,
            value,
        } => {
            assert_eq!(*binding, id(&q, "a"));
            assert_eq!(key.name, "name");
            assert!(matches!(&value.kind, ExprKind::Parameter(p) if p == "n"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn inline_properties_of_anonymous_elements_bind_to_their_internal_ids() {
    let src = "MATCH (:person {name: 'marko'})-[:knows {weight: 1.0}]->(b) RETURN b";
    let q = ok(src);
    assert_eq!(
        conjuncts(src, &q),
        vec![r##"name: 'marko' ["#0"]"##, r##"weight: 1.0 ["#1"]"##]
    );
    assert_eq!(
        q.binding(BindingId(0)).map(|b| b.predicates.clone()),
        Some(vec![0])
    );
    assert!(q.cross.is_empty());
    let q = ok("MATCH (a)-->({k: a.k}) RETURN a");
    assert_eq!(q.cross, vec![0]);
    assert!(q.binding(id(&q, "a")).is_some_and(|b| b.read));
}

#[test]
fn a_long_and_chain_splits_without_deep_recursion() {
    let mut src = String::from("MATCH (a) WHERE a.k = 0");
    for i in 1..400 {
        src.push_str(&format!(" AND a.k = {i}"));
    }
    src.push_str(" RETURN a");
    let q = ok(&src);
    assert_eq!(q.conjuncts.len(), 400);
}

// ----- RETURN -----

#[test]
fn return_star_expands_named_variables_in_order_of_first_appearance() {
    let q = ok("MATCH (b)-[:x]->(a), (c)-[e]->(b) RETURN *");
    assert_eq!(column_names(&q), vec!["b", "a", "c", "e"]);
    assert!(q.columns.iter().all(|c| c.variable.is_some() && !c.aliased));
    assert_eq!(q.columns[0].variable, Some(id(&q, "b")));
    let q = ok("MATCH (a)-->(b) RETURN *, a.name AS n");
    assert_eq!(column_names(&q), vec!["a", "b", "n"]);
    let f = fail("MATCH ()-->() RETURN *");
    assert_eq!(f.text, "*");
    assert!(f.alternative.is_some());
    let f = fail("MATCH (a)-->(b) RETURN *, a");
    assert!(f.message.contains("`a`"), "{}", f.message);
}

#[test]
fn column_names_are_aliases_or_the_text_as_written() {
    let q = ok("MATCH (a) RETURN a.name, a . age, upper(a.name) AS up, 1+2, a");
    assert_eq!(
        column_names(&q),
        vec!["a.name", "a . age", "up", "1+2", "a"]
    );
    assert_eq!(
        q.columns.iter().map(|c| c.aliased).collect::<Vec<_>>(),
        vec![false, false, true, false, false]
    );
    assert_eq!(q.columns[4].variable, Some(id(&q, "a")));
    assert_eq!(q.columns[0].variable, None);
    assert_eq!(names(&q, &q.columns[0].bindings), vec!["a"]);
    assert!(q.columns[3].bindings.is_empty());
    assert!(!q.distinct);
    assert!(ok("MATCH (a) RETURN DISTINCT a").distinct);
    // Back-quoted alias: the unescaped name.
    assert_eq!(
        column_names(&ok("MATCH (a) RETURN a.x AS `my col`")),
        vec!["my col"]
    );
}

#[test]
fn duplicate_column_names_are_refused() {
    let f = fail("MATCH (a)-->(b) RETURN a.name AS n, b.name AS n");
    assert_eq!(f.text, "n");
    assert!(f.message.contains("used twice"), "{}", f.message);
    let f = fail("MATCH (a) RETURN a.name, a.name");
    assert_eq!(f.text, "a.name");
    let f = fail("MATCH (a) RETURN a.x AS `a.name`, a.name");
    assert_eq!(f.text, "a.name");
    ok("MATCH (a) RETURN a.name, a.name AS name");
}

// ----- ORDER BY -----

#[test]
fn order_by_resolves_aliases_columns_and_variables() {
    let q = ok(
        "MATCH (a)-->(b) RETURN a.name AS n, b.age ORDER BY n DESC, b.age, a.age, \
                n || 'x'",
    );
    let keys = &q.order_by;
    assert_eq!(keys.len(), 4);
    assert_eq!(keys[0].column, Some(0));
    assert_eq!(keys[0].direction, SortDirection::Desc);
    assert_eq!(names(&q, &keys[0].bindings), vec!["a"]);
    assert_eq!(keys[1].column, Some(1));
    assert_eq!(keys[2].column, None);
    assert_eq!(names(&q, &keys[2].bindings), vec!["a"]);
    assert!(keys[2].aliases.is_empty());
    assert_eq!(keys[3].column, None);
    assert_eq!(keys[3].aliases, vec![0]);
    // An alias shadows a variable of the same name.
    let q = ok("MATCH (a)-->(b) RETURN b.name AS a ORDER BY a");
    assert_eq!(q.order_by[0].column, Some(0));
    assert_eq!(names(&q, &q.order_by[0].bindings), vec!["b"]);
    // ORDER BY sets the read flag.
    let q = ok("MATCH (a)-->(b) RETURN 1 AS x ORDER BY b.k");
    assert!(q.binding(id(&q, "b")).is_some_and(|b| b.read));
    assert!(!q.binding(id(&q, "a")).is_some_and(|b| b.read));
}

#[test]
fn order_by_after_distinct_reads_only_returned_columns() {
    ok("MATCH (a) RETURN DISTINCT a.name ORDER BY a.name");
    ok("MATCH (a) RETURN DISTINCT a.name AS n ORDER BY n");
    ok("MATCH (a) RETURN DISTINCT a ORDER BY a.age");
    ok("MATCH (a)-->(b) RETURN DISTINCT a.name AS n ORDER BY n || 'x'");
    let f = fail("MATCH (a) RETURN DISTINCT a.name ORDER BY a.age");
    assert_eq!(f.text, "a.age");
    assert!(f.message.contains("DISTINCT"), "{}", f.message);
}

// ----- OFFSET / LIMIT -----

#[test]
fn offset_and_limit_take_non_negative_integers_or_parameters() {
    let q = ok("MATCH (a) RETURN a SKIP 2 LIMIT $n");
    assert_eq!(q.offset, Some(Count::Literal(2)));
    assert!(matches!(&q.limit, Some(Count::Parameter { name, .. }) if name == "n"));
    let q = ok("MATCH (a) RETURN a OFFSET 0 LIMIT 9223372036854775807");
    assert_eq!(q.offset, Some(Count::Literal(0)));
    assert_eq!(q.limit, Some(Count::Literal(i64::MAX as u64)));
    let q = ok("MATCH (a) RETURN a");
    assert_eq!((q.offset.clone(), q.limit.clone()), (None, None));
    for (src, text, status) in [
        ("MATCH (a) RETURN a LIMIT -1", "-1", "22G02"),
        ("MATCH (a) RETURN a OFFSET -5", "-5", "22G02"),
        ("MATCH (a) RETURN a LIMIT 1.5", "1.5", "22G03"),
        ("MATCH (a) RETURN a LIMIT 'x'", "'x'", "22G03"),
        ("MATCH (a) RETURN a LIMIT NULL", "NULL", "22G03"),
        ("MATCH (a) RETURN a LIMIT 1 + 1", "1 + 1", "22G03"),
        ("MATCH (a) RETURN a LIMIT a.k", "a.k", "22G03"),
    ] {
        let f = fail(src);
        assert_eq!(
            (f.text.as_str(), f.gqlstatus.as_str()),
            (text, status),
            "{src}"
        );
        assert!(f.message.contains("non-negative integer"), "{}", f.message);
    }
}

// ----- maps -----

#[test]
fn duplicate_map_keys_are_refused() {
    let f = fail("MATCH (a {name: 'x', age: 1, name: 'y'}) RETURN a");
    assert!(f.message.contains("`name`"), "{}", f.message);
    assert_eq!(f.text, "name");
    assert_eq!(f.gqlstatus, "42002");
    fail("MATCH (a)-[e {w: 1, w: 2}]->(b) RETURN a");
    fail("MATCH (a) WHERE {k: 1, k: 2} = {} RETURN a");
    fail("MATCH (a) RETURN [{x: 1, x: 1}]");
    ok("MATCH (a) RETURN {x: {x: 1}, y: 1} AS m");
    // Keys are case-sensitive.
    ok("MATCH (a {name: 1, Name: 2}) RETURN a");
}

// ----- parameters -----

#[test]
fn parameters_are_collected_in_text_order() {
    let src = "MATCH (a {name: $name})-->(b WHERE b.age > $min) WHERE a.k IN $list RETURN \
               a.x + $add AS v ORDER BY $sort SKIP $skip LIMIT $limit";
    let q = ok(src);
    let found: Vec<_> = q
        .parameters
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                src.get(p.span.start..p.span.end).unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            ("name", "$name"),
            ("min", "$min"),
            ("list", "$list"),
            ("add", "$add"),
            ("sort", "$sort"),
            ("skip", "$skip"),
            ("limit", "$limit"),
        ]
    );
}

#[test]
fn supplied_parameters_are_checked_with_a_suggestion() {
    let src = "MATCH (a {name: $name}) RETURN a LIMIT $limit";
    let parsed = parse(src).unwrap_or_else(|e| panic!("{e}"));
    assert!(bind_with_parameters(&parsed, ["name", "limit", "unused"]).is_ok());
    let f = failure(src, bind_with_parameters(&parsed, ["nme", "limit"]));
    assert_eq!(f.kind, GqlErrorKind::Semantic);
    assert_eq!(f.text, "$name");
    assert_eq!(f.suggestion.as_deref(), Some("$nme"));
    assert!(f.message.contains("`$name`"), "{}", f.message);
    let f = failure(src, bind_with_parameters(&parsed, ["name"]));
    assert_eq!(f.text, "$limit");
    assert_eq!(f.suggestion, None);
    // No parameters supplied at all.
    let f = failure(src, bind_with_parameters(&parsed, []));
    assert_eq!(f.text, "$name");
    // Without a supplied set nothing is checked.
    assert!(bind(&parsed).is_ok());
}

#[test]
fn binding_never_panics_on_parsed_queries() {
    let pieces = [
        "MATCH (a)",
        "MATCH (a)-[e]->(b)",
        "MATCH (a), (b)",
        "MATCH (a)-[a]->(b)",
        "MATCH ()-->()",
        "MATCH (x {k: $p, k: 1})",
    ];
    let tails = [
        "RETURN *",
        "RETURN a",
        "RETURN a, a",
        "RETURN DISTINCT a.k ORDER BY b.k",
        "RETURN a.k AS z ORDER BY z LIMIT $l",
        "RETURN e SKIP -1",
        "RETURN q",
    ];
    for p in pieces {
        for t in tails {
            let src = format!("{p} {t}");
            if let Ok(parsed) = parse(&src) {
                let _ = bind(&parsed);
                let _ = bind_with_parameters(&parsed, ["p"]);
            }
        }
    }
}

#[test]
fn a_match_clause_holds_at_most_max_match_edges_edge_patterns() {
    let path = |edges: usize| format!("(a){}-->(z)", "-->()".repeat(edges - 1));
    // At the limit, in one path or spread over comma-separated paths.
    ok(&format!("MATCH {} RETURN z", path(MAX_MATCH_EDGES)));
    ok(&format!(
        "MATCH {}, (z)-[e]->(y) RETURN y",
        path(MAX_MATCH_EDGES - 1)
    ));
    // Separate clauses are not limited together.
    ok(&format!(
        "MATCH {} MATCH (z){}-->(y) RETURN y",
        path(MAX_MATCH_EDGES),
        "-->()".repeat(MAX_MATCH_EDGES - 1)
    ));
    // One edge past the limit: the error is at that edge, also when a later path brings it.
    for query in [
        format!("MATCH {} RETURN z", path(MAX_MATCH_EDGES + 1)),
        format!("MATCH {}, (z)-[e]->(y) RETURN y", path(MAX_MATCH_EDGES)),
        format!("MATCH {} RETURN z", path(5_000)),
    ] {
        let parsed = parse(&query).unwrap_or_else(|e| panic!("does not parse: {e}"));
        let f = failure(&query, bind(&parsed));
        assert_eq!(f.kind, GqlErrorKind::Limit, "{f:?}");
        assert_eq!(f.gqlstatus, "54001");
        assert_eq!(
            f.message,
            format!("the MATCH clause has more than {MAX_MATCH_EDGES} edge patterns")
        );
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains("several MATCH clauses"),
            "{alternative}"
        );
        assert!(alternative.contains("repeat()"), "{alternative}");
    }
    let query = format!("MATCH {}, (z)-[e]->(y) RETURN y", path(MAX_MATCH_EDGES));
    let parsed = parse(&query).unwrap_or_else(|e| panic!("does not parse: {e}"));
    assert_eq!(failure(&query, bind(&parsed)).text, "-[e]->");
}

// ----- stage 1b: segments, substitutions, aggregates, groups, paths -----

/// A compact rendering of a stored expression: variables by name, properties, literals,
/// operators and calls (enough to see a substitution).
fn show(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Literal(Literal::Integer(n)) => n.to_string(),
        ExprKind::Literal(Literal::String(s)) => format!("'{s}'"),
        ExprKind::Literal(other) => format!("{other:?}"),
        ExprKind::Parameter(name) => format!("${name}"),
        ExprKind::Variable(var) => var.name.clone(),
        ExprKind::Property { target, key } => format!("{}.{}", show(target), key.name),
        ExprKind::Binary { op, left, right } => {
            format!("({} {op:?} {})", show(left), show(right))
        }
        ExprKind::IsLabeled { operand, .. } => format!("{} IS LABELED", show(operand)),
        ExprKind::FunctionCall { name, args, .. } => match args {
            CallArgs::Star(_) => format!("{}(*)", name.name),
            CallArgs::Exprs(args) => format!(
                "{}({})",
                name.name,
                args.iter().map(show).collect::<Vec<_>>().join(", ")
            ),
            CallArgs::Cast { .. } | CallArgs::Trim { .. } => format!(
                "{}({})",
                name.name,
                args.iter().map(show).collect::<Vec<_>>().join(", ")
            ),
        },
        other => format!("{other:?}"),
    }
}

fn conjunct_exprs(q: &BoundQuery) -> Vec<String> {
    q.conjuncts
        .iter()
        .map(|c| match &c.predicate {
            Predicate::Expr(e) => show(e),
            Predicate::Property {
                binding,
                key,
                value,
            } => {
                format!("#{}.{} = {}", binding.0, key.name, show(value))
            }
        })
        .collect()
}

fn segment_shapes(q: &BoundQuery) -> Vec<String> {
    q.segments
        .iter()
        .map(|s| match s {
            Segment::Match(clauses) => format!("match{clauses:?}"),
            Segment::Optional(clause) => format!("optional{clause}"),
            Segment::With(with) => format!("with{}", with.columns.len()),
            Segment::For(clause) => format!("for{}", 1 + usize::from(clause.ordinal.is_some())),
            Segment::Call(call) => format!("call{}", call.outputs.len()),
            Segment::Rows(rows) => format!("rows{}", rows.outputs.len()),
            Segment::Write(write) => match &**write {
                BoundWrite::Insert(insert) => {
                    format!("insert{}+{}", insert.nodes.len(), insert.edges.len())
                }
                BoundWrite::Set(set) => format!("set{}", set.items.len()),
                BoundWrite::Remove(remove) => format!("remove{}", remove.items.len()),
                BoundWrite::Delete(delete) => format!("delete{}", delete.targets.len()),
                BoundWrite::Merge(merge) => format!("merge{}", merge.outputs.len()),
            },
        })
        .collect()
}

fn failure_of(query: &str) -> Failure {
    let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
    failure(query, bind(&parsed))
}

#[test]
fn a_1a_query_is_one_match_segment_and_keys_are_names() {
    let q = ok("MATCH (a)-[e]->(b), (c) MATCH (b)-->(d) WHERE a.x = d.x RETURN *");
    assert_eq!(segment_shapes(&q), ["match[0, 1]"]);
    assert!(
        q.bindings
            .iter()
            .all(|b| b.segment == 0 && b.column.is_none() && !b.group)
    );
    assert!(
        q.bindings
            .iter()
            .all(|b| b.key == b.name.as_ref().map(|n| n.name.clone()))
    );
    assert!(q.conjuncts.iter().all(|c| c.segment == 0));
    assert!(q.matches.iter().all(|m| !m.optional && m.segment == 0));
    assert!(!q.aggregated());
}

#[test]
fn optional_match_and_materializing_with_cut_segments() {
    let q = ok(
        "MATCH (a) OPTIONAL MATCH (a)-->(b) MATCH (c) FILTER c.x = 1 \
                WITH a, count(b) AS n MATCH (a)-->(d) RETURN d",
    );
    assert_eq!(
        segment_shapes(&q),
        ["match[0]", "optional1", "match[2]", "with2", "match[3]"]
    );
    let b = q.binding(id(&q, "b")).unwrap();
    assert!(b.optional(&q.segments) && b.segment == 1);
    assert!(!q.binding(id(&q, "a")).unwrap().optional(&q.segments));
    assert!(q.matches[1].optional);
    // FILTER joins the open plain segment of `c`.
    let filter = q
        .conjuncts
        .iter()
        .find(|c| c.origin == ConjunctOrigin::Filter)
        .unwrap();
    assert_eq!(filter.segment, 2);
    // A FILTER right after an OPTIONAL MATCH opens a segment of its own, which a later plain
    // MATCH joins.
    let q = ok("MATCH (a) OPTIONAL MATCH (a)-->(b) FILTER b.x = 1 MATCH (b)-->(c) RETURN c");
    assert_eq!(segment_shapes(&q), ["match[0]", "optional1", "match[2]"]);
    // A conjunct on a binding of an earlier segment is cross and reads it away from its place.
    let filter = &q.conjuncts[0];
    assert_eq!((filter.segment, filter.origin), (2, ConjunctOrigin::Filter));
    assert_eq!(q.cross, [0]);
    assert!(q.binding(id(&q, "b")).unwrap().read);
    // A leading OPTIONAL MATCH.
    let q = ok("OPTIONAL MATCH (n:person) RETURN n");
    assert_eq!(segment_shapes(&q), ["optional0"]);
}

#[test]
fn conjuncts_of_an_optional_match_stay_in_its_segment() {
    let q = ok(
        "MATCH (a:person) OPTIONAL MATCH (a:software {lang: 'java'})-[e]->(b WHERE b.x = a.x) \
                WHERE e.w > 1 RETURN b",
    );
    let a = q.binding(id(&q, "a")).unwrap();
    // The label of `a` written in the optional part is a conjunct of that segment, not a
    // label of the binding.
    assert_eq!(a.labels.len(), 1);
    assert!(a.joined());
    assert_eq!(
        conjunct_exprs(&q),
        [
            "a IS LABELED",
            "#0.lang = 'java'",
            "(b.x Eq a.x)",
            "(e.w Gt 1)"
        ]
    );
    assert!(q.conjuncts.iter().all(|c| c.segment == 1));
    assert_eq!(q.conjuncts[0].origin, ConjunctOrigin::Label);
    // On `a` alone, but in another segment: cross; on `e` alone in its own segment: local.
    assert_eq!(q.cross, [0, 1, 2]);
    assert_eq!(q.binding(id(&q, "e")).unwrap().predicates, [3]);
    assert!(a.read);
}

#[test]
fn let_and_projecting_with_are_substitutions() {
    let q = ok(
        "MATCH (a)-->(b) LET n = a.name, s = b.age + 1 FILTER n = 'x' AND s > $min \
                RETURN n, s * 2 AS t ORDER BY s",
    );
    assert_eq!(segment_shapes(&q), ["match[0]"]);
    assert_eq!(
        conjunct_exprs(&q),
        ["(a.name Eq 'x')", "((b.age Add 1) Gt $min)"]
    );
    // A substituted conjunct over one binding is listed on it (pushdown stays possible).
    assert_eq!(q.binding(id(&q, "a")).unwrap().predicates, [0]);
    let columns: Vec<_> = q
        .columns
        .iter()
        .map(|c| (c.name.as_str(), show(&c.expr)))
        .collect();
    assert_eq!(
        columns,
        [
            ("n", "a.name".into()),
            ("t", "((b.age Add 1) Mul 2)".into())
        ]
    );
    assert_eq!(names(&q, &q.columns[0].bindings), ["a"]);
    assert_eq!(show(&q.order_by[0].expr), "(b.age Add 1)");
    assert_eq!(names(&q, &q.order_by[0].bindings), ["b"]);
    // A name for a variable is that binding: it joins and is the column's variable.
    let q = ok("MATCH (a) LET x = a MATCH (x)-->(c) RETURN x");
    assert!(q.binding(id(&q, "a")).unwrap().joined());
    assert_eq!(q.columns[0].variable, Some(id(&q, "a")));
    assert_eq!(show(&q.columns[0].expr), "a");
    assert_eq!(q.columns[0].name, "x");
    // Projecting WITH: renames, narrows the scope, its WHERE is a conjunct.
    let q = ok("MATCH (a)-->(b) WITH a AS p, b.name AS n WHERE n <> 'x' RETURN *");
    assert_eq!(segment_shapes(&q), ["match[0]"]);
    assert_eq!(column_names(&q), ["p", "n"]);
    assert_eq!(show(&q.columns[0].expr), "a");
    assert_eq!(show(&q.columns[1].expr), "b.name");
    assert_eq!(conjunct_exprs(&q), ["(b.name Ne 'x')"]);
    assert_eq!(q.conjuncts[0].origin, ConjunctOrigin::Filter);
    let f = fail("MATCH (a)-->(b) WITH a RETURN b");
    assert_eq!(f.text, "b");
    assert!(f.message.contains("not in scope"), "{}", f.message);
    assert!(f.alternative.unwrap_or_default().contains("WITH ..., b"));
    // WITH * keeps the scope.
    let q = ok("MATCH (a)-->(b) LET x = 1 WITH * RETURN *");
    assert_eq!(column_names(&q), ["a", "b", "x"]);
}

#[test]
fn let_errors() {
    let f = fail("MATCH (a) LET a = 1 RETURN a");
    assert!(f.message.contains("already defined"), "{}", f.message);
    let f = fail("MATCH (a) LET x = 1, x = 2 RETURN x");
    assert!(f.message.contains("defined twice"), "{}", f.message);
    let f = fail("MATCH (a) LET x = 1, y = x + 1 RETURN y");
    assert_eq!(f.text, "x");
    assert!(f.message.contains("same LET"), "{}", f.message);
    let f = fail("MATCH (a) LET x = a.name MATCH (x) RETURN x");
    assert!(
        f.message.contains("a value defined by LET or WITH"),
        "{}",
        f.message
    );
    let f = fail("MATCH (a) WITH a.name RETURN 1 AS one");
    assert!(f.message.contains("needs a name"), "{}", f.message);
    let f = fail("MATCH (a) WITH a, a.x AS a RETURN a");
    assert!(f.message.contains("used twice in WITH"), "{}", f.message);
    // Chained definitions that double the expression hit the substitution limit.
    let mut query = String::from("MATCH (a) LET x0 = a.v");
    for i in 1..40 {
        query.push_str(&format!(" LET x{i} = x{} + x{}", i - 1, i - 1));
    }
    query.push_str(" RETURN x39");
    let f = failure_of(&query);
    assert_eq!(
        (f.kind, f.gqlstatus.as_str()),
        (GqlErrorKind::Limit, "54001")
    );
    assert!(f.message.contains("substituted"), "{}", f.message);
    // Deep nesting through chained definitions is bounded too.
    let mut query = String::from("MATCH (a) LET x0 = a.v");
    for i in 1..600 {
        query.push_str(&format!(" LET x{i} = -x{}", i - 1));
    }
    query.push_str(" RETURN x599");
    let f = failure_of(&query);
    assert_eq!(f.kind, GqlErrorKind::Limit, "{f:?}");
}

#[test]
fn materializing_with_columns_become_bindings() {
    let q = ok(
        "MATCH (a)-[e]->(b) WITH a, e, count(b) AS n, collect(b.name) AS names \
                WHERE n > 1 MATCH (a)-->(c) RETURN a, n, c",
    );
    let Segment::With(with) = &q.segments[1] else {
        panic!("{:?}", q.segments)
    };
    assert_eq!(with.columns.len(), 4);
    assert_eq!(
        with.columns.iter().map(|c| c.aggregate).collect::<Vec<_>>(),
        [false, false, true, true]
    );
    let kinds: Vec<_> = with
        .outputs
        .iter()
        .map(|o| {
            let b = q.binding(*o).unwrap();
            (
                b.name.as_ref().unwrap().name.clone(),
                b.kind,
                b.key.clone().unwrap(),
                b.column,
            )
        })
        .collect();
    assert_eq!(kinds[0].1, BindingKind::Node);
    assert_eq!(kinds[1].1, BindingKind::Edge);
    assert_eq!(kinds[2].1, BindingKind::Value);
    assert_eq!(kinds[3].1, BindingKind::Value);
    // Names bound before get a key of their own; new names keep theirs.
    assert_eq!(kinds[0].2, format!("#{}", with.outputs[0].0));
    assert_eq!(kinds[2].2, "n");
    assert_eq!(
        kinds[2].3,
        Some(ColumnRef {
            segment: 1,
            column: 2
        })
    );
    // The later MATCH joins the column binding of `a`; RETURN reads the columns by key.
    let a = q.binding(with.outputs[0]).unwrap();
    assert!(a.joined());
    assert_eq!(show(&q.columns[0].expr), kinds[0].2);
    assert_eq!(q.columns[0].variable, Some(with.outputs[0]));
    assert_eq!(show(&q.columns[1].expr), "n");
    // The WHERE of the WITH is a conjunct of its segment over the columns.
    let c = &q.conjuncts[0];
    assert_eq!((c.segment, c.origin), (1, ConjunctOrigin::Filter));
    assert_eq!(c.bindings, [with.outputs[2]]);
    assert!(q.cross.contains(&0));
    // DISTINCT, ORDER BY, OFFSET and LIMIT materialize too; a plain projection does not.
    for (query, segments) in [
        ("MATCH (a) WITH DISTINCT a RETURN a", 2),
        ("MATCH (a) WITH a ORDER BY a.x RETURN a", 2),
        ("MATCH (a) WITH a SKIP 1 RETURN a", 2),
        ("MATCH (a) WITH a LIMIT $n RETURN a", 2),
        ("MATCH (a) WITH a RETURN a", 1),
    ] {
        assert_eq!(ok(query).segments.len(), segments, "{query}");
    }
    // ORDER BY of a WITH reads the old scope unless it aggregates or is DISTINCT.
    let q = ok("MATCH (a)-->(b) WITH a ORDER BY b.x LIMIT 1 RETURN a");
    let Segment::With(with) = &q.segments[1] else {
        panic!()
    };
    assert_eq!(names(&q, &with.order_by[0].bindings), ["b"]);
    let f = fail("MATCH (a)-->(b) WITH DISTINCT a ORDER BY b.x RETURN a");
    assert!(f.message.contains("after WITH DISTINCT"), "{}", f.message);
    // A value column is not a node.
    let f = fail("MATCH (a) WITH count(a) AS n MATCH (n) RETURN n");
    assert!(f.message.contains("holds a value"), "{}", f.message);
}

#[test]
fn a_name_dropped_by_with_and_bound_again_is_a_new_binding() {
    let q = ok("MATCH (a)-->(b) WITH a MATCH (a)-->(b) RETURN b");
    let bs: Vec<&Binding> = q
        .bindings
        .iter()
        .filter(|b| b.name.as_ref().is_some_and(|n| n.name == "b"))
        .collect();
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[0].key.as_deref(), Some("b"));
    let second = format!("#{}", bs[1].id.0);
    assert_eq!(bs[1].key.as_deref(), Some(second.as_str()));
    assert_eq!(show(&q.columns[0].expr), second);
    assert_eq!(q.columns[0].variable, Some(bs[1].id));
    // A projecting WITH keeps one segment; `a` joins across it.
    assert_eq!(segment_shapes(&q), ["match[0, 1]"]);
    assert!(q.binding(id(&q, "a")).unwrap().joined());
    // A back-quoted name never collides with a generated key.
    let q = ok("MATCH (b), (`#2`) WITH `#2` MATCH (b) RETURN b");
    let keys: Vec<_> = q.bindings.iter().filter_map(|b| b.key.clone()).collect();
    assert_eq!(keys, ["b", "#2", "##2"]);
}

#[test]
fn aggregates_group_by_the_other_items() {
    let q = ok("MATCH (a)-[e]->(b) RETURN a, a.name AS n, count(*) AS c, \
                count(DISTINCT b) + size(a.name) AS d, collect(e) AS es, 1 + max(b.age) AS m");
    assert!(q.aggregated());
    assert_eq!(
        q.columns.iter().map(|c| c.aggregate).collect::<Vec<_>>(),
        [false, false, true, true, true, true]
    );
    // Grouping-key expressions and properties of key variables may stand beside aggregates.
    ok("MATCH (a) RETURN a.name, a.name || count(*) AS x");
    ok("MATCH (a) RETURN a, a.age + count(*) AS x, $p + 1 + sum(a.age) AS y");
    ok("MATCH (a) LET x = a.age RETURN a, x + count(*) AS c");
    ok("MATCH (a) RETURN count(*) AS c ORDER BY c");
    ok("MATCH (a) RETURN a.name AS n, count(*) ORDER BY count(*) DESC, n");
    // Not grouped.
    let f = fail("MATCH (a)-->(b) RETURN a.name, b.age + count(*) AS x");
    assert_eq!(f.text, "b");
    assert!(f.message.contains("grouping key"), "{}", f.message);
    assert!(f.alternative.unwrap_or_default().contains("collect(b)"));
    let f = fail("MATCH (a) LET x = a.age RETURN x + count(*) AS c");
    assert_eq!(f.text, "x");
    // ORDER BY after aggregation reads only columns.
    let f = fail("MATCH (a) RETURN a.name AS n, count(*) AS c ORDER BY a.age");
    assert!(f.message.contains("after an aggregation"), "{}", f.message);
    let f = fail("MATCH (n) RETURN n ORDER BY max(n.age)");
    assert_eq!(f.text, "max");
    assert!(
        f.message.contains("ORDER BY can only use an aggregate"),
        "{}",
        f.message
    );
}

#[test]
fn misplaced_aggregates_are_refused() {
    for (query, place) in [
        ("MATCH (a) WHERE count(*) > 1 RETURN a", "in WHERE"),
        ("MATCH (a) FILTER sum(a.x) > 1 RETURN a", "in FILTER"),
        ("MATCH (a) LET c = count(a) RETURN c", "in LET"),
        ("MATCH (a {x: max(a.y)}) RETURN a", "in a pattern"),
        ("MATCH (a WHERE avg(a.y) > 1) RETURN a", "in a pattern"),
        ("MATCH (a) RETURN max(count(a))", "inside another aggregate"),
        (
            "MATCH (a) WITH a WHERE count(*) > 1 RETURN a",
            "in the WHERE of WITH",
        ),
    ] {
        let f = fail(query);
        assert!(
            f.message.ends_with(&format!("cannot be used {place}")),
            "{query}: {}",
            f.message
        );
        assert!(
            f.alternative.unwrap_or_default().contains("WITH"),
            "{query}"
        );
        assert_eq!(f.gqlstatus, "42002");
    }
}

#[test]
fn quantified_edges_bind_group_variables() {
    let q = ok("MATCH (a)-[e:knows WHERE e.w > 1]->{1,3}(b) WHERE size(e) > 1 RETURN e");
    let e = q.binding(id(&q, "e")).unwrap();
    assert!(e.group);
    let step = q.matches[0].paths[0].steps[0];
    let quantifier = step.quantifier.unwrap();
    assert_eq!((quantifier.min, quantifier.max), (1, Some(3)));
    // The inline WHERE is per iteration (local); the clause WHERE reads the list (cross).
    assert_eq!(e.predicates, [0]);
    assert_eq!(q.cross, [1]);
    // A group variable cannot be bound again, nor have a property read outside its element.
    let f = fail("MATCH (a)-[e]->{1,3}(b) MATCH (b)-[e]->(c) RETURN c");
    assert!(f.message.contains("group variable"), "{}", f.message);
    let f = fail("MATCH (a)-[e]->(b) MATCH (b)-[e]->{1,2}(c) RETURN c");
    assert!(f.message.contains("group variable"), "{}", f.message);
    let f = fail("MATCH (a)-[e]->{1,3}(b) WHERE e.w > 1 RETURN b");
    assert_eq!(f.text, "e");
    assert!(
        f.message.contains("list of the matched edges"),
        "{}",
        f.message
    );
    let f = fail("MATCH (a)-[e]->{1,3}(b) RETURN e.w");
    assert!(f.message.contains("`e.w`"), "{}", f.message);
    // An inline condition reading another variable is not yet supported.
    let f = failure_of("MATCH (a)-[e WHERE e.w > a.w]->{1,3}(b) RETURN b");
    assert_eq!(
        (f.kind, f.gqlstatus.as_str()),
        (GqlErrorKind::NotYet, "0A000")
    );
    assert_eq!(f.text, "e.w > a.w");
    // A quantified edge counts once toward the edge limit.
    let path = format!("(a){}-->(z)", "-->{1,2}()".repeat(MAX_MATCH_EDGES - 1));
    ok(&format!("MATCH {path} RETURN z"));
}

#[test]
fn path_variables_are_bindings_of_their_own_kind() {
    let q = ok("MATCH p = (a)-->(b), q = (b)-->(c) WHERE path_length(p) > 0 RETURN p, q");
    let p = q.binding(id(&q, "p")).unwrap();
    assert_eq!(p.kind, BindingKind::Path);
    assert_eq!(p.id, BindingId(0));
    assert_eq!(q.matches[0].paths[0].variable, Some(p.id));
    assert_eq!(q.matches[0].paths[1].variable, Some(id(&q, "q")));
    // A conjunct on a path is cross.
    assert_eq!(q.cross, [0]);
    assert_eq!(q.columns[0].variable, Some(p.id));
    for (query, needle) in [
        ("MATCH p = (a)-->(p) RETURN a", "path variable"),
        (
            "MATCH p = (a)-->(b) MATCH (a)-[p]->(b) RETURN a",
            "path variable",
        ),
        ("MATCH p = (a), p = (b) RETURN a", "already bound"),
        ("MATCH (p) MATCH p = (a) RETURN a", "already bound"),
        (
            "MATCH p = (a WHERE path_length(p) > 1)-->(b) RETURN a",
            "not complete",
        ),
    ] {
        let f = fail(query);
        assert!(f.message.contains(needle), "{query}: {}", f.message);
    }
    // A path variable of an earlier clause is complete in a later inline filter.
    ok("MATCH p = (a)-->(b) MATCH (c WHERE path_length(p) > 1) RETURN c");
}

#[test]
fn binding_never_panics_on_stage_1b_queries() {
    let pieces = [
        "MATCH (a)-[e]->{1,3}(b)",
        "OPTIONAL MATCH p = (a)-->(b)",
        "MATCH (a) LET x = a.k, y = 2",
        "MATCH (a)-->(b) WITH a, count(b) AS n",
        "MATCH (a) WITH * WHERE a.k > 1",
        "MATCH (a) FILTER a.k = 1",
        "MATCH (a) WITH a AS b ORDER BY b.k LIMIT 2",
    ];
    let tails = [
        "RETURN *",
        "RETURN a",
        "RETURN count(*) AS c ORDER BY c",
        "MATCH (a)-->(c) RETURN c",
        "RETURN x, collect(e) AS es",
        "RETURN n + count(*)",
        "RETURN DISTINCT p ORDER BY b",
        "WITH sum(x) AS s RETURN s",
    ];
    for p in pieces {
        for t in tails {
            let src = format!("{p} {t}");
            if let Ok(parsed) = parse(&src) {
                let _ = bind(&parsed);
                let _ = bind_with_parameters(&parsed, ["p"]);
            }
        }
    }
}

// ----- stage 2a: FOR / UNWIND and queries without MATCH -----

/// The `FOR` segment at `index`.
fn for_segment(q: &BoundQuery, index: usize) -> &BoundFor {
    match q.segments.get(index) {
        Some(Segment::For(clause)) => clause,
        other => panic!("segment {index} is not a FOR: {other:?}"),
    }
}

#[test]
fn for_and_unwind_bind_a_value_column_and_cut_the_pipeline() {
    let q = ok("FOR x IN [1, 2] RETURN x");
    assert!(q.unit);
    assert_eq!(segment_shapes(&q), ["for1"]);
    let x = q.binding(id(&q, "x")).unwrap();
    assert_eq!(x.kind, BindingKind::Value);
    assert_eq!(
        x.column,
        Some(ColumnRef {
            segment: 0,
            column: FOR_ITEM
        })
    );
    assert_eq!(x.key.as_deref(), Some("x"));
    let clause = for_segment(&q, 0);
    assert_eq!(
        (clause.variable, clause.ordinal, clause.unwind),
        (x.id, None, false)
    );
    assert!(clause.bindings.is_empty());
    assert_eq!(column_names(&q), ["x"]);
    assert_eq!(q.columns[0].variable, Some(x.id));

    // UNWIND is the same clause.
    let q = ok("UNWIND $ids AS id RETURN id");
    assert!(for_segment(&q, 0).unwind);
    assert_eq!(names(&q, &[for_segment(&q, 0).variable]), ["id"]);
    assert_eq!(q.parameters.len(), 1);

    // After MATCH: a cut between two plain segments; the list reads the row before the FOR.
    let q = ok("MATCH (a) FOR x IN a.tags MATCH (a)-->(b) WHERE b.k = x RETURN a, b, x");
    assert!(!q.unit);
    assert_eq!(segment_shapes(&q), ["match[0]", "for1", "match[1]"]);
    let clause = for_segment(&q, 1);
    assert_eq!(names(&q, &clause.bindings), ["a"]);
    assert_eq!(show(&clause.list), "a.tags");
    assert!(q.binding(id(&q, "a")).unwrap().read);
    let x = q.binding(id(&q, "x")).unwrap();
    assert_eq!((x.segment, x.clause), (1, 0));
    // A FOR column is never a local binding: the WHERE over it is cross.
    let conjunct = q.conjuncts.last().unwrap();
    assert_eq!(names(&q, &conjunct.bindings), ["x", "b"]);
    assert_eq!(conjunct.segment, 2);
    assert!(q.cross.contains(&(q.conjuncts.len() - 1)));
    assert!(x.read);

    // A FILTER after a FOR opens a plain segment of its own.
    let q = ok("MATCH (a) UNWIND [1, 2] AS x FILTER x > 1 RETURN x");
    assert_eq!(segment_shapes(&q), ["match[0]", "for1", "match[]"]);
    assert_eq!(q.conjuncts[0].segment, 2);
    assert_eq!(q.cross, [0]);

    // LET / WITH names in the list are substituted; FOR after FOR.
    let q = ok("MATCH (a) LET l = [a.x, 1] FOR x IN l FOR y IN [x, x] RETURN y");
    assert_eq!(segment_shapes(&q), ["match[0]", "for1", "for1"]);
    assert_eq!(names(&q, &for_segment(&q, 1).bindings), ["a"]);
    assert_eq!(names(&q, &for_segment(&q, 2).bindings), ["x"]);
}

#[test]
fn for_ordinals_are_a_second_column() {
    for (query, kind) in [
        (
            "FOR x IN [5, 6] WITH ORDINALITY i RETURN *",
            OrdinalKind::Ordinality,
        ),
        (
            "FOR x IN [5, 6] WITH OFFSET i RETURN *",
            OrdinalKind::Offset,
        ),
    ] {
        let q = ok(query);
        assert_eq!(segment_shapes(&q), ["for2"], "{query}");
        let i = id(&q, "i");
        assert_eq!(for_segment(&q, 0).ordinal, Some((kind, i)), "{query}");
        let binding = q.binding(i).unwrap();
        assert_eq!(binding.kind, BindingKind::Value);
        assert_eq!(
            binding.column,
            Some(ColumnRef {
                segment: 0,
                column: FOR_ORDINAL
            })
        );
        // `RETURN *` lists the item, then the ordinal.
        assert_eq!(column_names(&q), ["x", "i"], "{query}");
    }
    // A position variable named `ordinality` is an ordinary name.
    let q = ok("FOR x IN [1] WITH ORDINALITY ordinality RETURN ordinality");
    assert_eq!(column_names(&q), ["ordinality"]);
}

#[test]
fn for_names_must_be_new() {
    for (query, text, keyword) in [
        ("MATCH (a) FOR a IN [1] RETURN a", "a", "FOR"),
        ("MATCH (a) UNWIND [1] AS a RETURN a", "a", "UNWIND"),
        ("LET x = 1 FOR x IN [1] RETURN x", "x", "FOR"),
        ("FOR x IN [1] FOR x IN [2] RETURN x", "x", "FOR"),
        (
            "MATCH (a) FOR x IN [1] WITH ORDINALITY a RETURN a",
            "a",
            "FOR",
        ),
        (
            "FOR x IN [1] WITH OFFSET i FOR i IN [2] RETURN i",
            "i",
            "FOR",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.message,
            format!("the variable `{text}` is already defined; {keyword} defines a new variable"),
            "{query}"
        );
        let alternative = f.alternative.unwrap_or_default();
        let example = if keyword == "UNWIND" {
            format!("UNWIND <list> AS {text}2")
        } else {
            format!("FOR {text}2 IN <list>")
        };
        assert!(alternative.contains(&example), "{query}: {alternative}");
    }
    // The item and its position need two names.
    for (query, word) in [
        ("FOR x IN [1] WITH ORDINALITY x RETURN x", "ORDINALITY"),
        ("FOR x IN [1] WITH OFFSET x RETURN x", "OFFSET"),
    ] {
        let f = fail(query);
        assert_eq!((f.gqlstatus.as_str(), f.text.as_str()), ("42002", "x"));
        assert_eq!(
            f.message,
            format!(
                "`x` names both the item and its position; WITH {word} defines a second variable"
            )
        );
        assert!(
            f.alternative
                .unwrap_or_default()
                .contains(&format!("FOR x IN ... WITH {word} x_index")),
            "{query}"
        );
    }
    // The list reads the scope before the FOR.
    let f = fail("FOR x IN [x] RETURN x");
    assert_eq!(
        (f.text.as_str(), f.message.as_str()),
        ("x", "the variable `x` is not defined")
    );
    // A name dropped by WITH and bound again by FOR is a new binding with its own key (C14).
    let q = ok("MATCH (x) WITH 1 AS one FOR x IN [one] RETURN x");
    let item = for_segment(&q, 1).variable;
    let binding = q.binding(item).unwrap();
    assert_ne!(binding.key.as_deref(), Some("x"));
    assert_eq!(binding.name.as_ref().map(|n| n.name.as_str()), Some("x"));
}

#[test]
fn an_aggregate_in_the_for_list_is_refused() {
    for (query, place) in [
        ("MATCH (a) FOR x IN collect(a) RETURN x", "in FOR"),
        ("MATCH (a) UNWIND collect(a.x) AS x RETURN x", "in UNWIND"),
    ] {
        let f = fail(query);
        assert_eq!(
            (f.gqlstatus.as_str(), f.text.as_str()),
            ("42002", "collect")
        );
        assert_eq!(
            f.message,
            format!("the aggregate function collect() cannot be used {place}")
        );
        assert!(
            f.alternative
                .unwrap_or_default()
                .contains("AS fs FOR x IN fs"),
            "{query}"
        );
    }
}

#[test]
fn a_for_variable_written_as_a_node_or_edge_takes_that_kind() {
    // E3: a node of a later pattern; a label there is a conjunct of that segment.
    let q = ok("MATCH p = (a)-->(b) FOR n IN nodes(p) MATCH (n:person)-->(m) RETURN n, m");
    assert_eq!(segment_shapes(&q), ["match[0]", "for1", "match[1]"]);
    let n = q.binding(id(&q, "n")).unwrap();
    assert_eq!(n.kind, BindingKind::Node);
    assert_eq!(n.segment, 1);
    assert!(n.joined() && n.labels.is_empty());
    assert!(
        q.conjuncts
            .iter()
            .any(|c| c.origin == ConjunctOrigin::Label && c.bindings == [n.id] && c.segment == 2)
    );
    // An edge; then the same kind again joins.
    let q = ok("FOR e IN $edges MATCH (a)-[e]->(b) MATCH (c)-[e]->(d) RETURN a, e");
    let e = q.binding(id(&q, "e")).unwrap();
    assert_eq!((e.kind, e.occurrences.len()), (BindingKind::Edge, 3));
    // A projecting WITH keeps the binding, so it can still take a kind after it.
    let q = ok("FOR n IN $l WITH n MATCH (n)-->(m) RETURN m");
    assert_eq!(q.binding(id(&q, "n")).unwrap().kind, BindingKind::Node);
    // Once a node, never an edge.
    let f = fail("FOR n IN $l MATCH (n) MATCH ()-[n]->() RETURN n");
    assert!(
        f.message
            .contains("used as an edge here but is already bound to a node"),
        "{}",
        f.message
    );
    // The edge of a quantified edge is always a new group variable.
    let f = fail("FOR e IN $l MATCH (a)-[e]->{1,2}(b) RETURN a");
    assert!(f.message.contains("group variable"), "{}", f.message);
    // A materializing WITH column of a value stays a value.
    let f = fail("FOR n IN $l WITH n LIMIT 1 MATCH (n) RETURN n");
    assert!(f.message.contains("holds a value"), "{}", f.message);
    // The ordinal is a number, never an element.
    for query in [
        "FOR x IN [1] WITH ORDINALITY i MATCH (i) RETURN i",
        "FOR x IN [1] WITH OFFSET i MATCH ()-[i]->() RETURN i",
    ] {
        let f = fail(query);
        assert_eq!(
            (f.gqlstatus.as_str(), f.text.as_str()),
            ("42002", "i"),
            "{query}"
        );
        assert!(
            f.message
                .contains("is the position of a FOR item (a number)"),
            "{query}: {}",
            f.message
        );
    }
}

#[test]
fn queries_without_match_start_from_a_unit_row() {
    for (query, shapes, columns) in [
        ("RETURN 1 + 2 AS x", &[][..], &["x"][..]),
        ("RETURN $a AS a, 'k'", &[], &["a", "'k'"]),
        ("LET x = 1 RETURN x", &[], &["x"]),
        ("WITH 1 AS x RETURN x", &[], &["x"]),
        ("WITH 1 AS x WHERE x > 0 RETURN x", &["match[]"], &["x"]),
        (
            "WITH 1 AS x ORDER BY x LIMIT 1 RETURN x",
            &["with1"],
            &["x"],
        ),
        ("WITH count(*) AS n RETURN n", &["with1"], &["n"]),
        ("LET x = 1 MATCH (a {v: x}) RETURN a", &["match[0]"], &["a"]),
        (
            "FOR x IN [1] MATCH (a) RETURN *",
            &["for1", "match[0]"],
            &["x", "a"],
        ),
    ] {
        let q = ok(query);
        assert!(q.unit, "{query}");
        assert_eq!(segment_shapes(&q), shapes, "{query}");
        assert_eq!(column_names(&q), columns, "{query}");
        assert!(
            q.bindings
                .iter()
                .all(|b| b.name.is_some() || b.kind == BindingKind::Node)
        );
    }
    for query in ["MATCH (a) RETURN a", "OPTIONAL MATCH (a) RETURN a"] {
        assert!(!ok(query).unit, "{query}");
    }
    // `RETURN *` still needs a named variable.
    for query in ["RETURN *", "LET x = 1 WITH 2 AS y RETURN *"] {
        let parsed = parse(query).unwrap();
        let result = bind(&parsed);
        if query == "RETURN *" {
            let f = failure(query, result);
            assert!(
                f.message.contains("needs at least one named variable"),
                "{}",
                f.message
            );
        } else {
            assert_eq!(column_names(&result.unwrap()), ["y"]);
        }
    }
    // A name of a later MATCH is named as such before the first MATCH too.
    let f = fail("LET x = y MATCH (y) RETURN x");
    assert!(
        f.message.contains("bound by a later MATCH clause"),
        "{}",
        f.message
    );
    let f = fail("FOR x IN [z] MATCH (a) MATCH (z) RETURN x");
    assert!(
        f.message.contains("bound by a later MATCH clause"),
        "{}",
        f.message
    );
}

#[test]
fn binding_never_panics_on_stage_2a_queries() {
    let pieces = [
        "",
        "MATCH (a)",
        "MATCH (a)-[e]->(b)",
        "OPTIONAL MATCH (a)",
        "LET x = 1",
        "WITH 1 AS x",
        "MATCH p = (a)-->(b)",
    ];
    let fors = [
        "FOR x IN [1, 2]",
        "FOR a IN [1]",
        "UNWIND $p AS e",
        "FOR x IN a.l WITH ORDINALITY i",
        "FOR x IN [x] WITH OFFSET x",
        "FOR n IN nodes(p)",
        "UNWIND collect(a) AS y",
    ];
    let tails = [
        "RETURN *",
        "RETURN x",
        "MATCH (x)-->(c) RETURN c",
        "MATCH (i) RETURN i",
        "MATCH ()-[e]->() RETURN e",
        "MATCH (n)-[x]->{1,2}() RETURN n",
        "WITH x, count(*) AS c RETURN c",
        "FOR y IN [x] RETURN y",
    ];
    for p in pieces {
        for f in fors {
            for t in tails {
                let src = format!("{p} {f} {t}");
                if let Ok(parsed) = parse(&src) {
                    if let Ok(bound) = bind(&parsed) {
                        let _ =
                            crate::gql::planner::plan(&bound, &crate::gql::planner::NoFacts, None);
                    }
                    let _ = bind_with_parameters(&parsed, ["p"]);
                }
            }
        }
    }
}

#[test]
fn subqueries_bind_in_every_position() {
    // Stage 2b.T2 binds EXISTS and pattern predicates; since 2b.T5 every position runs (the
    // guard that refused them before the planner and the lowering supported them is gone).
    for (query, text) in [
        (
            "MATCH (n) WHERE EXISTS { MATCH (n)-->(m) } RETURN n",
            "EXISTS { MATCH (n)-->(m) }",
        ),
        ("MATCH (n) WHERE NOT (n)-[:T]->() RETURN n", "(n)-[:T]->()"),
        (
            "MATCH (n) RETURN CASE WHEN EXISTS ((n)-->()) THEN 1 END AS x",
            "EXISTS ((n)-->())",
        ),
        ("MATCH (n) LET x = (n)-->() RETURN x", "(n)-->()"),
        (
            "FOR x IN [EXISTS { (a)-->() }] RETURN x",
            "EXISTS { (a)-->() }",
        ),
    ] {
        let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
        let bound = bind(&parsed).unwrap_or_else(|e| panic!("{query:?} does not bind: {e}"));
        assert_eq!(bound.subqueries.len(), 1, "{query}");
        assert_eq!(parsed.text(subquery(&bound, 0).span), text, "{query}");
    }
}

// ----- subqueries (stage 2b.T2) -----

fn subquery(q: &BoundQuery, index: usize) -> &BoundSubquery {
    q.subqueries
        .get(index)
        .unwrap_or_else(|| panic!("no subquery {index}: {:?}", q.subqueries))
}

/// The binding of `name` declared inside a subquery body (the last of that name).
fn local<'q>(q: &'q BoundQuery, name: &str) -> &'q Binding {
    q.bindings
        .iter()
        .rev()
        .find(|b| b.name.as_ref().is_some_and(|n| n.name == name))
        .unwrap_or_else(|| panic!("no binding `{name}`"))
}

#[test]
fn subquery_bodies_are_nested_scopes_with_imports() {
    let query = "MATCH (a)-->(b) WHERE EXISTS { MATCH (b)-->(c) WHERE c.x = a.x } RETURN a";
    let q = ok(query);
    assert_eq!(q.subqueries.len(), 1);
    let s = subquery(&q, 0);
    assert_eq!(
        (s.kind, s.form),
        (SubqueryKind::Exists, SubqueryForm::Braces)
    );
    assert_eq!(s.parent, None);
    // Imports: the outer names the body reads, in a pattern (b) or a condition (a).
    assert_eq!(names(&q, &s.imports), ["a", "b"]);
    // The body clause follows the query's own clauses; its segment lies past the pipeline.
    assert_eq!(s.clauses, [1]);
    assert_eq!(q.matches.len(), 2);
    assert_eq!(segment_shapes(&q), ["match[0]"]);
    assert_eq!(s.segment, 1);
    assert_eq!(q.matches.get(1).map(|m| m.segment), Some(1));
    // `b` in the body is the outer binding (a correlation), `c` a local of the body.
    let body = &q.matches[1].paths[0];
    assert_eq!(body.start, id(&q, "b"));
    let c = local(&q, "c");
    assert_eq!((c.subquery, c.segment, c.clause), (Some(0), 1, 1));
    assert_eq!(q.binding(id(&q, "b")).map(|b| b.subquery), Some(None));
    // The host conjunct reads the imports; the body conjunct runs in the body's segment.
    assert_eq!(
        conjuncts(query, &q),
        [
            "c.x = a.x [\"a\", \"c\"]",
            "EXISTS { MATCH (b)-->(c) WHERE c.x = a.x } [\"a\", \"b\"]"
        ]
    );
    assert_eq!(
        q.conjuncts.iter().map(|c| c.segment).collect::<Vec<_>>(),
        [1, 0]
    );
    // The host conjunct reads two bindings: a cross conjunct of the outer segment, both read.
    assert_eq!(q.cross, [0, 1]);
    assert!(q.binding(id(&q, "a")).is_some_and(|b| b.read));
    // The stored host expression names the subquery by its span.
    let Predicate::Expr(host) = &q.conjuncts[1].predicate else {
        panic!("not an expression")
    };
    assert_eq!(q.subquery_at(host.span), Some(0));
}

#[test]
fn a_subquery_on_one_element_is_listed_on_it_and_reads_nothing_away() {
    // "Who has no ...": the conjunct imports only the element being bound, so it sits on that
    // element (design 6: pushdown into the scan), and the element needs no label.
    // (`RETURN` reads nothing, so only the subquery could set the read flag.)
    let query = "MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN count(*) AS c";
    let q = ok(query);
    let p = q.binding(id(&q, "p")).unwrap_or_else(|| panic!("p"));
    assert_eq!(p.predicates, [0]);
    assert!(!p.read);
    assert!(q.cross.is_empty());
    let s = subquery(&q, 0);
    assert_eq!(s.form, SubqueryForm::PatternPredicate);
    assert_eq!(names(&q, &s.imports), ["p"]);
    // The anonymous elements of the pattern are locals of the body.
    let locals: Vec<_> = q
        .bindings
        .iter()
        .filter(|b| b.subquery == Some(0))
        .collect();
    assert_eq!(locals.len(), 2);
    assert!(
        locals
            .iter()
            .all(|b| b.name.is_none() && b.segment == s.segment)
    );
    // A label written on an import inside the body is a condition of the body.
    let query = "MATCH (p) WHERE EXISTS { (p:person)-->() } RETURN p";
    let q = ok(query);
    assert_eq!(
        conjuncts(query, &q),
        ["person [\"p\"]", "EXISTS { (p:person)-->() } [\"p\"]"]
    );
    assert_eq!(q.conjuncts[0].origin, ConjunctOrigin::Label);
    assert_eq!(q.conjuncts[0].segment, subquery(&q, 0).segment);
    assert!(q.binding(id(&q, "p")).is_some_and(|b| b.labels.is_empty()));
}

#[test]
fn nested_subqueries_import_through_their_parents() {
    let query = "MATCH (a) WHERE EXISTS { MATCH (a)-->(b) WHERE EXISTS { MATCH (b)-->(c) \
                 WHERE c.x = a.x } } RETURN a";
    let q = ok(query);
    assert_eq!(q.subqueries.len(), 2);
    let (outer, inner) = (subquery(&q, 0), subquery(&q, 1));
    assert_eq!((outer.parent, inner.parent), (None, Some(0)));
    // `b` is a local of the outer body, so only `a` crosses its border.
    assert_eq!(names(&q, &outer.imports), ["a"]);
    assert_eq!(names(&q, &inner.imports), ["a", "b"]);
    assert_eq!((outer.segment, inner.segment), (1, 2));
    assert_eq!(
        (outer.clauses.as_slice(), inner.clauses.as_slice()),
        (&[1][..], &[2][..])
    );
    assert_eq!(local(&q, "b").subquery, Some(0));
    assert_eq!(local(&q, "c").subquery, Some(1));
    // Several clauses in one body: each its own clause, one segment; joins across them.
    let q = ok("MATCH (a) WHERE EXISTS { MATCH (a)-[f]->(b) MATCH (b)-[f]->(c) } RETURN a");
    let s = subquery(&q, 0);
    assert_eq!(s.clauses, [1, 2]);
    assert!(q.matches[1..].iter().all(|m| m.segment == s.segment));
    assert!(local(&q, "f").joined());
}

#[test]
fn subquery_locals_do_not_leak_into_the_query() {
    for (query, text) in [
        ("MATCH (a) WHERE EXISTS { MATCH (a)-->(b) } RETURN b", "b"),
        (
            "MATCH (a) WHERE EXISTS { (a)-[e]->() } AND e.w > 1 RETURN a",
            "e",
        ),
        (
            "MATCH (a) WHERE EXISTS { MATCH p = (a)-->() } RETURN p",
            "p",
        ),
        (
            "MATCH (a) WHERE EXISTS { MATCH (a)-->(b) } AND EXISTS { MATCH (a) WHERE b.x = 1 } \
             RETURN a",
            "b",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("bound inside a subquery"),
            "{query}: {}",
            f.message
        );
        assert!(
            f.alternative
                .as_deref()
                .is_some_and(|a| a.contains("MATCH clause")),
            "{query}: {f:?}"
        );
    }
    // The same name bound later by the query is a new binding of its own.
    let q = ok("MATCH (a) WHERE EXISTS { MATCH (a)-->(b) } MATCH (b) RETURN b");
    let bs: Vec<_> = q
        .bindings
        .iter()
        .filter(|b| b.name.as_ref().is_some_and(|n| n.name == "b"))
        .collect();
    assert_eq!(bs.len(), 2);
    assert_eq!((bs[0].subquery, bs[1].subquery), (Some(0), None));
    assert_ne!(bs[0].key, bs[1].key);
    assert_eq!(column_names(&q), ["b"]);
    assert_eq!(q.columns[0].variable, Some(bs[1].id));
}

#[test]
fn pattern_predicates_introduce_no_variable() {
    for (query, text) in [
        ("MATCH (a) WHERE (a)-->(b) RETURN a", "b"),
        ("MATCH (a) WHERE NOT (a)-[r:T]->() RETURN a", "r"),
        ("MATCH (a) RETURN (x)-->(a) AS y", "x"),
    ] {
        let f = fail(query);
        assert_eq!(
            (f.text.as_str(), f.gqlstatus.as_str()),
            (text, "42002"),
            "{query}"
        );
        assert!(f.message.contains("new variable"), "{query}: {}", f.message);
        assert!(
            f.alternative
                .as_deref()
                .is_some_and(|a| a.contains("EXISTS { MATCH (")),
            "{query}: {f:?}"
        );
    }
    // A name a WITH dropped keeps its own message.
    let f = fail("MATCH (a)-->(b) WITH a WHERE (b)-->() RETURN a");
    assert!(f.message.contains("does not carry it"), "{}", f.message);
    // Names in scope, anonymous elements, labels and property maps are fine; braces may bind.
    ok("MATCH (a), (b) WHERE (a)-[:T]->(b) AND NOT (b {x: 1})-->(:L) RETURN a");
    ok("MATCH (a) WHERE EXISTS { (a)-->(b) WHERE b.x = 1 } RETURN a");
    ok("FOR x IN [1] RETURN EXISTS { (a)-->() } AS e");
}

#[test]
fn subquery_names_follow_the_pattern_rules() {
    // An edge bound outside may be bound again inside (design E8: trail per body clause).
    let q = ok("MATCH (a)-[e]->(b) WHERE EXISTS { MATCH (b)<-[e]-(a) } RETURN a");
    assert_eq!(names(&q, &subquery(&q, 0).imports), ["a", "e", "b"]);
    // Inside one body clause an edge variable is still bound once.
    let f = fail("MATCH (a) WHERE EXISTS { MATCH (a)-[f]->(b), (b)-[f]->(c) } RETURN a");
    assert!(f.message.contains("more than one edge"), "{}", f.message);
    // Kinds, group variables, path variables and LET values keep their rules.
    let f = fail("MATCH (a)-[e]->(b) WHERE EXISTS { (e)-->() } RETURN a");
    assert!(
        f.message.contains("already bound to an edge"),
        "{}",
        f.message
    );
    let f = fail("MATCH (a)-[e]->{1,2}(b) WHERE EXISTS { MATCH (a)-[e]->(b) } RETURN a");
    assert!(f.message.contains("group variable"), "{}", f.message);
    let f = fail("MATCH p = (a)-->(b) WHERE EXISTS { MATCH p = (a)-->() } RETURN a");
    assert!(f.message.contains("already bound"), "{}", f.message);
    let f = fail("MATCH (a) LET x = 1 FILTER EXISTS { MATCH (x)-->() } RETURN a");
    assert!(f.message.contains("value defined by LET"), "{}", f.message);
    // A path variable of the body is a local; one read inside its own MATCH is incomplete,
    // also from a subquery standing in that MATCH's inline filter.
    ok("MATCH (a) WHERE EXISTS { MATCH p = (a)-->() WHERE path_length(p) > 0 } RETURN a");
    let f = fail(
        "MATCH p = (a WHERE EXISTS { MATCH (a)-->(b) WHERE path_length(p) > 0 })-->() RETURN a",
    );
    assert!(f.message.contains("not complete"), "{}", f.message);
    // An undefined name inside a body gets the usual suggestion.
    let f = fail("MATCH (alpha) WHERE EXISTS { MATCH (alpha)-->(b) WHERE b.x = alpa.x } RETURN 1");
    assert_eq!(f.suggestion.as_deref(), Some("alpha"));
}

#[test]
fn subqueries_read_let_values_and_stand_anywhere_an_expression_does() {
    // A LET value read in a body is substituted there, and its bindings are imports.
    let query =
        "MATCH (n) LET y = n.age FILTER EXISTS { MATCH (n)-->(m) WHERE m.age > y } RETURN n";
    let q = ok(query);
    assert_eq!(names(&q, &subquery(&q, 0).imports), ["n"]);
    assert_eq!(conjunct_exprs(&q)[0], "(m.age Gt n.age)");
    // A LET holding a subquery: the substituted host keeps naming the same subquery.
    let q = ok("MATCH (n) LET has = (n)-->() FILTER has OR n.x = 1 RETURN n, has AS h");
    assert_eq!(q.subqueries.len(), 1);
    let Predicate::Expr(filter) = &q.conjuncts[0].predicate else {
        panic!("not an expression")
    };
    let ExprKind::Binary { left, .. } = &filter.kind else {
        panic!("{filter:?}")
    };
    assert_eq!(q.subquery_at(left.span), Some(0));
    assert_eq!(names(&q, &q.columns[1].bindings), ["n"]);
    // Columns, sort keys, FOR lists: the host reads the imports, so they are read.
    let q = ok("MATCH (a)-->(b) RETURN b, EXISTS { (a)-->(b) } AS e ORDER BY EXISTS { (b)-->() }");
    assert_eq!(names(&q, &q.columns[1].bindings), ["a", "b"]);
    assert_eq!(names(&q, &q.order_by[0].bindings), ["b"]);
    assert!(q.binding(id(&q, "a")).is_some_and(|b| b.read));
    let q = ok("MATCH (a) FOR x IN [EXISTS { (a)-->() }] RETURN x");
    let Some(Segment::For(f)) = q.segments.get(1) else {
        panic!("{:?}", q.segments)
    };
    assert_eq!(names(&q, &f.bindings), ["a"]);
    assert_eq!(subquery(&q, 0).segment, 2);
}

#[test]
fn subquery_bodies_refuse_aggregates_and_check_grouping() {
    let f = fail("MATCH (a) WHERE EXISTS { MATCH (a)-->(b) WHERE count(b) > 1 } RETURN a");
    assert!(f.message.contains("in WHERE"), "{}", f.message);
    let f = fail("MATCH (a) WHERE EXISTS { MATCH (a)-->(b {x: max(1)}) } RETURN a");
    assert!(f.message.contains("in a pattern"), "{}", f.message);
    // Beside an aggregate, a subquery may import only grouping keys.
    let f = fail(
        "MATCH (a)-->(b) RETURN count(b) + CASE WHEN EXISTS { (a)-->() } THEN 1 ELSE 0 END AS x",
    );
    assert_eq!(f.text, "EXISTS { (a)-->() }");
    assert!(f.message.contains("`a`"), "{}", f.message);
    ok("MATCH (a)-->(b) RETURN a, count(b) + CASE WHEN EXISTS { (a)-->() } THEN 1 ELSE 0 END AS x");
    // A subquery inside an aggregate argument is fine.
    ok("MATCH (a) RETURN count(CASE WHEN (a)-->() THEN 1 END) AS c");
    // After DISTINCT, ORDER BY reads only returned columns, imports included.
    let f = fail("MATCH (a)-->(b) RETURN DISTINCT b ORDER BY EXISTS { (a)-->() }");
    assert!(f.message.contains("`a`"), "{}", f.message);
}

#[test]
fn subqueries_are_limited() {
    // At most MAX_SUBQUERIES per query (nested ones included), kind Limit.
    let many = |n: usize| {
        let preds = vec!["(a)-->()"; n].join(" AND ");
        format!("MATCH (a) WHERE {preds} RETURN a")
    };
    assert_eq!(ok(&many(MAX_SUBQUERIES)).subqueries.len(), MAX_SUBQUERIES);
    let query = many(MAX_SUBQUERIES + 1);
    let f = failure_of(&query);
    assert_eq!(
        (f.kind, f.gqlstatus.as_str()),
        (GqlErrorKind::Limit, "54001")
    );
    assert_eq!(f.text, "(a)-->()");
    let offset = query.rfind("(a)-->()").unwrap_or(0);
    let parsed = parse(&query).unwrap_or_else(|e| panic!("{e}"));
    let Err(TraverserError::Gql { offset: at, .. }) = bind(&parsed) else {
        panic!("bound")
    };
    assert_eq!(at, offset);
    // A body clause holds at most MAX_MATCH_EDGES edge patterns, like a clause of the query.
    let chain = |n: usize| "-->()".repeat(n);
    ok(&format!(
        "MATCH (a) WHERE EXISTS {{ MATCH (a){} }} RETURN a",
        chain(MAX_MATCH_EDGES)
    ));
    let f = failure_of(&format!(
        "MATCH (a) WHERE EXISTS {{ MATCH (a){} }} RETURN a",
        chain(MAX_MATCH_EDGES + 1)
    ));
    assert_eq!(f.kind, GqlErrorKind::Limit);
    assert!(f.message.contains("edge patterns"), "{}", f.message);
}

#[test]
fn deeply_nested_subqueries_bind_without_overflow() {
    // The deepest nesting the parser accepts binds (recursion: one level per subquery).
    let mut deepest = None;
    let mut nested = "(n)-->()".to_string();
    for _ in 0..200 {
        nested = format!("EXISTS {{ MATCH (n) WHERE {nested} }}");
        let query = format!("MATCH (n) WHERE {nested} RETURN n");
        match parse(&query) {
            Ok(parsed) => deepest = Some(parsed),
            Err(_) => break,
        }
    }
    let parsed = deepest.unwrap_or_else(|| panic!("nothing parsed"));
    let bound = bind(&parsed).unwrap_or_else(|e| panic!("{e}"));
    assert!(bound.subqueries.len() > 10, "{}", bound.subqueries.len());
    assert!(
        bound
            .subqueries
            .iter()
            .enumerate()
            .skip(1)
            .all(|(k, s)| s.parent == Some(k - 1) && s.imports.len() == 1)
    );
}

#[test]
fn binding_never_panics_on_stage_2b_queries() {
    let heads = [
        "MATCH (a)-[e]->(b)",
        "OPTIONAL MATCH (a)",
        "MATCH (a)-[e]->{1,2}(b)",
        "MATCH p = (a)-->(b)",
        "FOR a IN [1]",
        "MATCH (a) WITH a, count(*) AS c",
    ];
    let preds = [
        "EXISTS { MATCH (a)-->(c) }",
        "EXISTS { (a)-[e]->(b) WHERE b.x = a.x }",
        "NOT (a)-->(b)",
        "(a)-[x]->()",
        "EXISTS ((c)-->())",
        "EXISTS { MATCH (a)-->(b) MATCH (b)-->(c) WHERE EXISTS { (c)-->(a) } }",
        "EXISTS { MATCH q = (a)-->() WHERE path_length(p) > 1 }",
        "count(a) > 1 OR (a)-->()",
    ];
    let tails = [
        "RETURN a",
        "RETURN c",
        "RETURN a, (a)-->() AS h ORDER BY h",
        "RETURN count(*) + CASE WHEN (a)-->() THEN 1 END AS k",
        "MATCH (c) RETURN c",
    ];
    for h in heads {
        for p in preds {
            for t in tails {
                for src in [format!("{h} WHERE {p} {t}"), format!("{h} FILTER {p} {t}")] {
                    if let Ok(parsed) = parse(&src) {
                        let _ = bind(&parsed);
                        let _ = bind_with_parameters(&parsed, ["p"]);
                    }
                }
            }
        }
    }
}

#[test]
fn body_conditions_mark_only_the_bodys_own_locals_read() {
    // A body condition reading an import does not set its read flag (the planner decides
    // where the body reads it); the body's own locals read away are marked as usual.
    let q = ok(
        "MATCH (p) WHERE EXISTS { MATCH (p)-->(q), (r) WHERE q.x = p.x AND r.y = 1 } \
                RETURN 1 AS x",
    );
    let read = |name: &str| local(&q, name).read;
    assert!(!read("p"));
    assert!(read("q"));
    assert!(!read("r"));
    // Nested: the inner host reads the outer body's local `b` (a cross conjunct of the outer
    // body) and the query's `a`, which only the planner labels.
    let q = ok(
        "MATCH (a) WHERE EXISTS { MATCH (a)-->(b) WHERE EXISTS { MATCH (b)-->(c) \
                WHERE c.x = a.x } } RETURN 1 AS x",
    );
    assert!(!local(&q, "a").read);
    assert!(local(&q, "b").read);
    assert!(local(&q, "c").read);
    // The host expression still reads the imports where it stands.
    let q = ok(
        "MATCH (a), (b) WHERE EXISTS { MATCH (a)-->(c) WHERE c.x = 1 } AND \
                EXISTS { (b)-->(a) } RETURN 1 AS x",
    );
    assert!(local(&q, "a").read && local(&q, "b").read);
}

// ----- stage 2c.T3: pattern comprehensions -----

/// A pattern comprehension binds wherever an expression stands (it was refused "not yet"
/// before 2c.T3).
#[test]
fn pattern_comprehensions_bind_everywhere() {
    for query in [
        "MATCH (n) RETURN [(n)-->(m) | m.name]",
        "MATCH (n) LET l = [(n)-->() | 1] RETURN l",
        "MATCH (n) RETURN CASE WHEN true THEN [(n)-->(m) | m] END",
        "MATCH (n) RETURN n {k: [(n)-->(m) | m]}",
        "MATCH (n) RETURN [x IN [1] | [(n)-->(m) | m]]",
        "MATCH (n) WHERE size([(n)-->(m) | m]) > 1 RETURN n",
        "MATCH (n) WITH n, [(n)-->(m) | m] AS ms RETURN n, ms",
        "MATCH (n) RETURN n ORDER BY size([(n)-->(m) | m])",
        "MATCH (n) RETURN collect_list([(n)-->(m) | m.name]) AS l",
        "MATCH (n) RETURN n, count(*) AS c, [(n)-->(m) | m] AS ms",
        "MATCH (n) RETURN [p = (n)-->(m) WHERE m.age > 1 | p] AS ps",
        "MATCH (n) RETURN [(n)-->(m) | [(m)-->(o) | o.name]] AS l",
        "MATCH (n) RETURN [(n)-->(m) WHERE EXISTS { (m)-->() } | m] AS l",
        "FOR x IN [(a)-->(b) | b] RETURN x",
        "MATCH (n) WHERE EXISTS { MATCH (n)-->(m) WHERE size([(m)-->(o) | o]) > 0 } RETURN n",
    ] {
        ok(query);
    }
}

#[test]
fn a_pattern_comprehension_is_a_collecting_subquery() {
    let q = ok("MATCH (a)-->(b) RETURN [(a)-[e]->(c) WHERE c.age > b.age | c.name] AS l");
    assert_eq!(q.subqueries.len(), 1);
    let s = subquery(&q, 0);
    assert_eq!(s.kind, SubqueryKind::Collect);
    assert_eq!(s.form, SubqueryForm::Comprehension);
    // The span is the whole expression (the stored expression names it by it).
    assert_eq!(
        &"MATCH (a)-->(b) RETURN [(a)-[e]->(c) WHERE c.age > b.age | c.name] AS l"
            [s.span.start..s.span.end],
        "[(a)-[e]->(c) WHERE c.age > b.age | c.name]"
    );
    assert_eq!(names(&q, &s.imports), ["a", "b"]);
    assert_eq!(names(&q, &s.map_bindings), ["c"]);
    assert!(s.map.is_some());
    // The column reads the imports; the locals are read (they are labelled when needed).
    assert_eq!(names(&q, &q.columns[0].bindings), ["a", "b"]);
    assert!(q.binding(id(&q, "c")).is_some_and(|b| b.read));
    assert!(q.binding(id(&q, "e")).is_some_and(|b| !b.read));
    // An import read only in the `|` part is an import too.
    let q = ok("MATCH (a), (b) RETURN [(a)-->(c) | b.name] AS l");
    assert_eq!(names(&q, &subquery(&q, 0).imports), ["a", "b"]);
    // A LET value read in the `|` part is substituted there.
    let q = ok("MATCH (a) LET k = a.name RETURN [(a)-->(c) | k] AS l");
    let s = subquery(&q, 0);
    assert_eq!(names(&q, &s.imports), ["a"]);
    assert!(matches!(
        s.map.as_ref().map(|m| &m.kind),
        Some(ExprKind::Property { .. })
    ));
}

#[test]
fn pattern_comprehension_names_are_local() {
    // A new name is invisible after the comprehension.
    let f = fail("MATCH (a) RETURN [(a)-->(c) | c] AS l, c");
    assert!(f.message.contains("`c`"), "{f:?}");
    // An aggregate in the `|` part is refused, naming the list's size or OPTIONAL MATCH.
    let f = fail("MATCH (a) RETURN [(a)-->(c) | count(c)] AS l");
    assert!(f.message.contains("pattern comprehension"), "{f:?}");
    assert!(
        f.alternative.unwrap_or_default().contains("OPTIONAL MATCH"),
        "alternative"
    );
    // A list comprehension variable read in the `|` part runs in the child: not yet.
    let query = "MATCH (a) RETURN [x IN [1] | [(a)-->(c) | x]] AS l";
    let parsed = parse(query).unwrap_or_else(|e| panic!("{e}"));
    let f = failure(query, bind(&parsed));
    assert_eq!(f.kind, GqlErrorKind::NotYet, "{f:?}");
    assert_eq!(f.text, "x");
    // Beside an aggregate it may import only grouping keys.
    let f = fail("MATCH (a)-->(b) RETURN a, count(*) + size([(b)-->(x) | x]) AS c");
    assert!(f.message.contains("`b`"), "{f:?}");
    assert_eq!(f.text, "[(b)-->(x) | x]");
    ok("MATCH (a)-->(b) RETURN a, count(*) + size([(a)-->(x) | x]) AS c");
    // A column of its own is a grouping key.
    ok("MATCH (a)-->(b) RETURN a, count(*) AS c, [(b)-->(x) | x] AS l");
}

// ----- stage 2c.T2: map projections and list comprehensions -----

#[test]
fn map_projections_and_list_comprehensions_bind_everywhere() {
    for query in [
        "MATCH (n) RETURN n {.name, k: 1}",
        "MATCH (n) RETURN n {.*, n}",
        "MATCH (n) RETURN [x IN [1, 2] WHERE x > 1 | x]",
        "MATCH (n) WHERE size([x IN n.l | x]) > 0 RETURN n",
        "MATCH (n {k: [x IN [1] | x]}) RETURN n",
        "MATCH (n WHERE n {.a} IS NULL) RETURN n",
        "MATCH (n)-[e WHERE e {.a} IS NULL]->(m) RETURN n",
        "MATCH (n) WITH n {.a} AS m RETURN m",
        "FOR x IN [y IN [1] | y] RETURN x",
        "MATCH (n) RETURN n ORDER BY n {.a}",
        "MATCH (n) FILTER [x IN [1] WHERE x = 1] = [1] RETURN n",
        "MATCH (n) WHERE EXISTS { MATCH (n)-->(m) WHERE [x IN [m] | x] = [] } RETURN n",
        "MATCH (n) RETURN collect_list(n {.a})",
        "MATCH (n) RETURN [x IN collect_list(n.age) WHERE x > 1] AS l",
        "MATCH (n) RETURN [x IN [1] | [y IN [x] | x + y]] AS l",
        "MATCH (n) RETURN [x IN [1] WHERE EXISTS { MATCH (n)-->() } | x] AS l",
        "MATCH (n) RETURN [x IN [1] | x] AS a, [x IN [2] | x] AS b",
        "MATCH (n) RETURN n.a AS a ORDER BY [x IN [a] | x]",
    ] {
        ok(query);
    }
}

#[test]
fn a_comprehension_reads_the_bindings_of_its_parts_not_its_variable() {
    let q = ok("MATCH (n)-->(m) RETURN [x IN n.l WHERE x > m.age | x] AS l");
    assert_eq!(names(&q, &q.columns[0].bindings), ["n", "m"]);
    let q = ok("MATCH (n)-->(m) RETURN n {.a, m} AS p");
    assert_eq!(names(&q, &q.columns[0].bindings), ["n", "m"]);
    // An aggregate in the list part aggregates the item.
    let q = ok("MATCH (n) RETURN [x IN collect_list(n.age) WHERE x > 1] AS l");
    assert!(q.columns[0].aggregate);
}

#[test]
fn a_comprehension_variable_shadows_nothing() {
    for (query, text, message) in [
        (
            "MATCH (n) RETURN [n IN [1] | n]",
            "n",
            "the comprehension variable `n` shadows the variable `n` in scope",
        ),
        (
            "MATCH (n) RETURN [x IN [1] | [x IN [2] | x]]",
            "x",
            "the comprehension variable `x` shadows the variable `x` in scope",
        ),
        (
            "LET y = 1 RETURN [y IN [1] | y]",
            "y",
            "the comprehension variable `y` shadows the variable `y` in scope",
        ),
        (
            "MATCH (n) RETURN n.a AS a ORDER BY [a IN [1] | a]",
            "a",
            "the comprehension variable `a` shadows the variable `a` in scope",
        ),
        // A name the query used before: a substituted expression refers to bindings by it.
        (
            "MATCH (n) WITH n AS m RETURN [n IN [1] | m]",
            "n",
            "the comprehension variable `n` is a name this query already uses",
        ),
        (
            "MATCH (p) WHERE EXISTS { MATCH (p)-->(q) } RETURN [q IN [1] | q]",
            "q",
            "the comprehension variable `q` is a name this query already uses",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.starts_with(message), "{query}: {}", f.message);
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains(&format!("[{text}2 IN list")),
            "{query}: {alternative}"
        );
    }
}

#[test]
fn a_comprehension_variable_is_invisible_outside_its_parts() {
    for (query, text) in [
        ("MATCH (n) RETURN [x IN [1] | x] AS l, x", "x"),
        ("MATCH (n) WHERE [x IN [1] | x] = [1] RETURN x", "x"),
        // The list part is outside the variable's scope.
        ("MATCH (n) RETURN [x IN [x] | x] AS l", "x"),
        ("MATCH (n) RETURN [x IN [1] | x] AS l ORDER BY x", "x"),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("is not defined"),
            "{query}: {}",
            f.message
        );
    }
}

#[test]
fn no_aggregate_inside_the_parts_of_a_comprehension() {
    for query in [
        "MATCH (n) RETURN [x IN [1] WHERE count(*) > 1] AS l",
        "MATCH (n) RETURN [x IN [1] | sum(x)] AS l",
    ] {
        let f = fail(query);
        assert!(
            f.message
                .contains("cannot be used inside the WHERE or | part of a list comprehension"),
            "{query}: {}",
            f.message
        );
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains("use its column in the comprehension"),
            "{query}: {alternative}"
        );
    }
}

#[test]
fn a_comprehension_variable_inside_a_subquery_is_not_yet_read() {
    for (query, text) in [
        (
            "MATCH (n) RETURN [x IN [1] WHERE EXISTS { MATCH (n)-->(m) WHERE m.a = x }] AS l",
            "x",
        ),
        // Named in a body pattern it would be a new variable: refused too.
        (
            "MATCH (n) RETURN [x IN [1] WHERE EXISTS { MATCH (n)-->(x) }] AS l",
            "x",
        ),
        ("MATCH (n) RETURN [x IN [1] WHERE (n)-->(x)] AS l", "x"),
    ] {
        let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
        let f = failure(query, bind(&parsed));
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "0A000", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message
                .contains("reading the comprehension variable `x` inside a subquery"),
            "{query}: {}",
            f.message
        );
    }
}

#[test]
fn a_projection_variable_item_keeps_its_name_when_substituted() {
    let q = ok("MATCH (n) LET k = n.age RETURN n {.name, k} AS p");
    let ExprKind::MapProjection(projection) = &q.columns[0].expr.kind else {
        panic!("not a projection: {:?}", q.columns[0].expr);
    };
    match &projection.items[1] {
        ProjectionItem::Entry(entry) => {
            assert_eq!(entry.key.name, "k");
            assert!(matches!(entry.value.kind, ExprKind::Property { .. }));
        }
        other => panic!("not an entry: {other:?}"),
    }
    // A variable that is a binding's own name stays a variable item.
    let q = ok("MATCH (n)-->(m) RETURN n {m} AS p");
    let ExprKind::MapProjection(projection) = &q.columns[0].expr.kind else {
        panic!("not a projection");
    };
    assert!(matches!(&projection.items[0], ProjectionItem::Variable(v) if v.name == "m"));
    // A substituted target.
    let q = ok("MATCH (n) WITH n AS m RETURN m {.name} AS p");
    let ExprKind::MapProjection(projection) = &q.columns[0].expr.kind else {
        panic!("not a projection");
    };
    assert!(matches!(&projection.target.kind, ExprKind::Variable(v) if v.name == "n"));
    // A comprehension keeps its variable through a substitution of its parts.
    let q = ok("MATCH (n) LET k = n.age RETURN [x IN [1] | x + k] AS l");
    let ExprKind::ListComprehension(comprehension) = &q.columns[0].expr.kind else {
        panic!("not a comprehension");
    };
    assert_eq!(comprehension.variable.name, "x");
    assert_eq!(names(&q, &q.columns[0].bindings), ["n"]);
}

#[test]
fn a_projection_beside_an_aggregate_reads_only_grouping_keys() {
    ok("MATCH (n)-->(m) RETURN n, n {.a, c: count(*)} AS p");
    ok("MATCH (n)-->(m) RETURN n, m, n {m, c: count(*)} AS p");
    for (query, text) in [
        ("MATCH (n)-->(m) RETURN n {.a, c: count(*)} AS p", "n"),
        ("MATCH (n)-->(m) RETURN n, n {m, c: count(*)} AS p", "m"),
        (
            "MATCH (n)-->(m) RETURN n, n {k: m.a, c: count(*)} AS p",
            "m",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("must be a grouping key"),
            "{query}: {}",
            f.message
        );
    }
    // A comprehension variable is no grouping key and needs none.
    ok("MATCH (n) RETURN [x IN collect_list(n.a) WHERE x > 1 | x * 2] AS l");
}

/// Stage 2d.T1: the composite queries, `CALL { }`, `COUNT { }`, `COLLECT { }` and whole-query
/// bodies parse; stage 2d.T2 binds them, and since stage 2d.T5c every one lowers (no "not yet"
/// guard is left between planning and lowering).
#[test]
fn stage_2d_constructs_parse_and_bind() {
    for query in [
        "MATCH (a) RETURN a.x AS x UNION ALL MATCH (b) RETURN b.x AS x",
        "MATCH (a) RETURN a NEXT MATCH (a)-->(b) RETURN b",
        "MATCH (a) CALL (a) { MATCH (a)-->(b) RETURN b } RETURN b",
        "MATCH (a) OPTIONAL CALL { WITH a RETURN a AS b } RETURN b",
        "MATCH (a) WHERE COUNT { (a)-->() } > 1 RETURN a",
        "MATCH (a) RETURN COLLECT { MATCH (a)-->(b) RETURN b.name } AS names",
        "MATCH (a) WHERE EXISTS { MATCH (a)-->(b) RETURN b } RETURN a",
    ] {
        let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
        assert!(bind(&parsed).is_ok(), "{query}");
    }
    // A pattern comprehension and EXISTS with a body of MATCH clauses bind as before.
    ok("MATCH (a) WHERE EXISTS { MATCH (a)-->(b) WHERE b.x > 1 } RETURN [(a)-->(c) | c.x] AS l");
}

// ----- stage 2d: composite queries, CALL, whole-query subquery bodies (2d.T2) -----

/// The column names of a linear query.
fn linear_columns(linear: &BoundLinear) -> Vec<&str> {
    linear.columns.iter().map(|c| c.name.as_str()).collect()
}

/// The segment shapes of one pipeline.
fn pipeline_shapes(q: &BoundQuery, linear: &BoundLinear) -> Vec<String> {
    let shapes = segment_shapes(q);
    linear
        .pipeline
        .iter()
        .map(|&s| shapes.get(s).cloned().unwrap_or_else(|| format!("?{s}")))
        .collect()
}

/// The only `CALL` segment of `q`.
fn the_call(q: &BoundQuery) -> &BoundCall {
    let mut calls = q.segments.iter().filter_map(|s| match s {
        Segment::Call(call) => Some(call),
        _ => None,
    });
    let call = calls.next().expect("a CALL segment");
    assert!(calls.next().is_none(), "one CALL segment");
    call
}

#[test]
fn a_linear_query_is_one_pipeline_of_every_segment() {
    let q =
        ok("MATCH (a) OPTIONAL MATCH (a)-->(b) WITH a, count(b) AS n FOR x IN [1] RETURN a, n, x");
    assert!(q.body.is_linear());
    assert_eq!(q.pipeline, (0..q.segments.len()).collect::<Vec<_>>());
    assert!(!q.unit);
    assert!(ok("RETURN 1 AS x").unit);
}

#[test]
fn union_operands_return_the_same_names_in_the_first_operands_order() {
    let q = ok("MATCH (a:person) RETURN a.name AS name, a.age AS age \
         UNION MATCH (b:software) RETURN b.lang AS age, b.name AS name ORDER BY name LIMIT 1");
    assert!(!q.body.is_linear());
    let block = &q.body.first;
    assert!(block.distinct());
    assert_eq!(linear_columns(&block.first), ["name", "age"]);
    let second = &block.unions[0].operand;
    assert_eq!(linear_columns(second), ["name", "age"]);
    assert_eq!(show(&second.columns[0].expr), "b.name");
    assert_eq!(show(&second.columns[1].expr), "b.lang");
    // ORDER BY and LIMIT belong to their operand, read after the reordering.
    assert_eq!(second.order_by.len(), 1);
    assert_eq!(second.order_by[0].column, Some(0));
    assert_eq!(second.limit, Some(Count::Literal(1)));
    assert!(block.first.order_by.is_empty() && block.first.limit.is_none());
    // Each operand is a pipeline of its own.
    assert_eq!(pipeline_shapes(&q, &block.first), ["match[0]"]);
    assert_eq!(pipeline_shapes(&q, second), ["match[1]"]);
    // UNION ALL keeps duplicates; a chain of one kind binds.
    let q = ok("RETURN 1 AS x UNION ALL RETURN 2 AS x UNION ALL RETURN 3 AS x");
    assert!(!q.body.first.distinct());
    assert_eq!(q.body.first.unions.len(), 2);
    let q = ok("RETURN 1 AS x UNION DISTINCT RETURN 2 AS x UNION RETURN 3 AS x");
    assert!(q.body.first.distinct());
}

#[test]
fn union_operands_with_other_column_names_are_refused_naming_both_sets() {
    for (query, text, detail) in [
        (
            "MATCH (p) RETURN p UNION MATCH (q) RETURN q",
            "RETURN q",
            "`p` is missing",
        ),
        (
            "RETURN 1 AS a, 2 AS b UNION RETURN 1 AS a",
            "RETURN 1 AS a",
            "`b` is missing",
        ),
        (
            "RETURN 1 AS a UNION RETURN 1 AS a, 2 AS b",
            "RETURN 1 AS a, 2 AS b",
            "`b` is not a column of the first",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains(detail), "{query}: {}", f.message);
        assert!(f.message.contains("the first returns `"), "{}", f.message);
        assert!(
            f.alternative
                .as_deref()
                .is_some_and(|a| a.contains(" AS name")),
            "{f:?}"
        );
    }
    let f = fail("RETURN 1 AS a, 2 AS b UNION RETURN 1 AS a, 2 AS c");
    assert!(
        f.message
            .contains("returns `a`, `b`, this one returns `a`, `c`"),
        "{}",
        f.message
    );
}

#[test]
fn mixing_union_all_and_union_distinct_in_one_block_is_refused() {
    let f = fail("RETURN 1 AS x UNION RETURN 2 AS x UNION ALL RETURN 3 AS x");
    assert_eq!(f.text, "UNION ALL");
    assert!(
        f.message.contains("UNION ALL is mixed with UNION"),
        "{}",
        f.message
    );
    let f = fail("RETURN 1 AS x UNION ALL RETURN 2 AS x UNION DISTINCT RETURN 3 AS x");
    assert_eq!(f.text, "UNION DISTINCT");
    // Different kinds in different blocks (separated by NEXT) bind.
    ok("RETURN 1 AS x UNION ALL RETURN 1 AS x NEXT RETURN x UNION RETURN 2 AS x");
}

#[test]
fn union_operands_have_scopes_of_their_own() {
    let f = fail("MATCH (a) RETURN a.x AS x UNION RETURN a AS x");
    assert_eq!(f.text, "a");
    assert!(f.message.contains("`a` is not defined"), "{}", f.message);
    // The same name in two operands is two bindings.
    let q = ok("MATCH (a:person) RETURN a UNION MATCH (a:software) RETURN a");
    let named: Vec<_> = q
        .bindings
        .iter()
        .filter(|b| b.name.as_ref().is_some_and(|n| n.name == "a"))
        .collect();
    assert_eq!(named.len(), 2);
    assert_ne!(named[0].segment, named[1].segment);
    assert_eq!(named[1].key.as_deref(), Some("#1"));
}

#[test]
fn next_after_a_linear_query_is_the_with_it_desugars_to() {
    // Design E19: `RETURN .. NEXT` binds like `WITH ..`: a projecting one is a substitution.
    for (next, with) in [
        (
            "MATCH (a)-->(b) RETURN a, b.name AS n NEXT MATCH (a)-->(c) WHERE c.name = n RETURN n, c",
            "MATCH (a)-->(b) WITH a, b.name AS n MATCH (a)-->(c) WHERE c.name = n RETURN n, c",
        ),
        (
            "MATCH (a)-->(b) RETURN a, count(b) AS k ORDER BY k LIMIT 3 NEXT FILTER k > 1 RETURN a",
            "MATCH (a)-->(b) WITH a, count(b) AS k ORDER BY k LIMIT 3 FILTER k > 1 RETURN a",
        ),
        (
            "MATCH (a) RETURN DISTINCT a.x AS x NEXT RETURN x",
            "MATCH (a) WITH DISTINCT a.x AS x RETURN x",
        ),
    ] {
        let n = ok(next);
        let w = ok(with);
        assert!(n.body.is_linear(), "{next}");
        assert_eq!(segment_shapes(&n), segment_shapes(&w), "{next}");
        assert_eq!(conjunct_exprs(&n), conjunct_exprs(&w), "{next}");
        assert_eq!(column_names(&n), column_names(&w), "{next}");
        assert_eq!(n.pipeline, (0..n.segments.len()).collect::<Vec<_>>());
        assert_eq!(n.bindings.len(), w.bindings.len(), "{next}");
    }
    // A RETURN item without AS is named by its text (unlike a WITH item) and read back-quoted.
    let q = ok("MATCH (a) RETURN a.name NEXT RETURN `a.name` AS n");
    assert_eq!(column_names(&q), ["n"]);
    assert_eq!(show(&q.columns[0].expr), "a.name");
    // GROUP BY before NEXT: grouping keys only, a DISTINCT row per group.
    let q = ok("MATCH (a) RETURN a.x AS x GROUP BY x NEXT RETURN x");
    assert_eq!(segment_shapes(&q), ["match[0]", "with1"]);
}

#[test]
fn a_name_the_return_before_next_does_not_return_is_out_of_scope() {
    let f = fail("MATCH (a)-->(b) RETURN a NEXT RETURN b");
    assert_eq!(f.text, "b");
    assert!(
        f.message
            .contains("the RETURN before NEXT does not return it"),
        "{}",
        f.message
    );
    assert!(
        f.alternative
            .as_deref()
            .is_some_and(|a| a.contains("RETURN ..., b NEXT")),
        "{f:?}"
    );
    // The WITH message stays.
    let f = fail("MATCH (a)-->(b) WITH a RETURN b");
    assert!(
        f.message.contains("the WITH before does not carry it"),
        "{}",
        f.message
    );
}

#[test]
fn a_block_after_a_union_reads_its_records_as_columns() {
    let q = ok(
        "MATCH (a:person) RETURN a AS x UNION MATCH (b:software) RETURN b AS x \
         NEXT MATCH (x)-->(y) RETURN x, y ORDER BY x",
    );
    assert_eq!(q.body.next.len(), 1);
    let next = &q.body.next[0].first;
    assert_eq!(pipeline_shapes(&q, next), ["rows1", "match[2]"]);
    let Some(Segment::Rows(rows)) = next.pipeline.first().and_then(|&s| q.segments.get(s)) else {
        panic!("rows first")
    };
    let x = q.binding(rows.outputs[0]).expect("x");
    // Both operands return a node there: a node column, continued by the later pattern.
    assert_eq!(x.kind, BindingKind::Node);
    assert_eq!(x.column.map(|c| c.column), Some(0));
    assert!(x.joined());
    assert!(!next.unit);
    // A column that is a node in one operand and a value in the other is a value.
    let q = ok("MATCH (a) RETURN a AS x UNION RETURN 1 AS x NEXT RETURN x");
    let Some(Segment::Rows(rows)) = q.body.next[0]
        .first
        .pipeline
        .first()
        .and_then(|&s| q.segments.get(s))
    else {
        panic!("rows first")
    };
    assert_eq!(
        q.binding(rows.outputs[0]).map(|b| b.kind),
        Some(BindingKind::Value)
    );
    // Chained single blocks after the union are one pipeline (E19).
    let q = ok("RETURN 1 AS x UNION RETURN 2 AS x NEXT RETURN x + 1 AS y NEXT RETURN y");
    assert_eq!(q.body.next.len(), 1);
    assert_eq!(pipeline_shapes(&q, &q.body.next[0].first), ["rows1"]);
}

#[test]
fn a_union_block_after_next_starts_each_operand_from_the_rows_before() {
    let q = ok(
        "MATCH (a:person) RETURN a NEXT MATCH (a)-[:created]->(b) RETURN b \
         UNION MATCH (a)-[:knows]->(b) RETURN b",
    );
    assert_eq!(q.body.next.len(), 1);
    let block = &q.body.next[0];
    for operand in block.operands() {
        assert_eq!(pipeline_shapes(&q, operand)[0], "rows1");
    }
    // Each operand reads its own column binding of `a`.
    let a: Vec<_> = q
        .bindings
        .iter()
        .filter(|b| b.name.as_ref().is_some_and(|n| n.name == "a"))
        .collect();
    assert_eq!(a.len(), 3, "{a:?}");
}

#[test]
fn call_imports_the_variables_of_its_scope_clause_and_returns_new_columns() {
    let q = ok(
        "MATCH (a:person) CALL (a) { MATCH (a)-[:created]->(s) RETURN s.name AS sn } \
         RETURN a.name AS name, sn",
    );
    let call = the_call(&q);
    assert_eq!(names(&q, &call.imports), ["a"]);
    assert_eq!(names(&q, &call.outputs), ["sn"]);
    assert!(!call.optional);
    let sn = q.binding(call.outputs[0]).expect("sn");
    assert_eq!(sn.kind, BindingKind::Value);
    assert!(q.binding(id(&q, "a")).is_some_and(|a| a.read && a.joined()));
    // The body is a pipeline of its own; the query's pipeline holds the CALL.
    assert_eq!(pipeline_shapes(&q, &q), ["match[0]", "call1"]);
    assert_eq!(pipeline_shapes(&q, &call.body.first.first), ["match[1]"]);
    assert_eq!(q.matches.len(), 2);
    // A node returned by the body is a node binding a later pattern continues.
    let q = ok("MATCH (a) CALL (a) { MATCH (a)-->(b) RETURN b } MATCH (b)-->(c) RETURN c");
    let b = q.binding(the_call(&q).outputs[0]).expect("b");
    assert_eq!(b.kind, BindingKind::Node);
    assert!(b.joined());
}

#[test]
fn call_scope_forms() {
    // `()` imports nothing: a name written inside is new.
    let q = ok("MATCH (a) CALL () { MATCH (a)-->(b) RETURN b } RETURN b");
    assert!(the_call(&q).imports.is_empty());
    // `(*)` imports every name in scope (read ones are the imports).
    let q = ok(
        "MATCH (a)-->(c) LET k = c.x CALL (*) { MATCH (a)-->(b) WHERE b.x = k RETURN b } RETURN b",
    );
    assert_eq!(names(&q, &the_call(&q).imports), ["a", "c"]);
    // No scope clause: nothing, except what a Cypher importing WITH names.
    let q = ok("MATCH (a) CALL { MATCH (a)-->(b) RETURN b } RETURN b");
    assert!(the_call(&q).imports.is_empty());
    let q = ok("MATCH (a) CALL { WITH a MATCH (a)-->(b) RETURN b } RETURN b");
    assert_eq!(names(&q, &the_call(&q).imports), ["a"]);
    let q = ok("MATCH (a) CALL { WITH * MATCH (a)-->(b) RETURN b } RETURN b");
    assert_eq!(names(&q, &the_call(&q).imports), ["a"]);
    // Each operand of a union body imports through its own importing WITH.
    let q = ok("MATCH (a)-->(c) CALL { WITH a MATCH (a)-->(b) RETURN b \
         UNION WITH c MATCH (c)-->(b) RETURN b } RETURN b");
    assert_eq!(names(&q, &the_call(&q).imports), ["a", "c"]);
    // OPTIONAL CALL: the returned names may be NULL.
    let q = ok("MATCH (a) OPTIONAL CALL (a) { MATCH (a)-->(b) RETURN b } RETURN b");
    let call = the_call(&q);
    assert!(call.optional);
    assert!(
        q.binding(call.outputs[0])
            .is_some_and(|b| b.optional(&q.segments))
    );
}

#[test]
fn a_call_body_reads_only_what_it_imports() {
    for (query, text) in [
        ("MATCH (a) CALL () { RETURN a.name AS n } RETURN n", "a"),
        ("MATCH (a) CALL { WITH a AS b RETURN b } RETURN b", "a"),
        (
            "MATCH (a), (c) CALL (c) { MATCH (c)-->(b) WHERE b.x = a.x RETURN b } RETURN b",
            "a",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("is not imported into the CALL body"),
            "{query}: {}",
            f.message
        );
        assert!(
            f.alternative
                .as_deref()
                .is_some_and(|a| a.contains("CALL (a) {")),
            "{f:?}"
        );
    }
    // A scope clause naming an unknown variable, or one twice.
    let f = fail("MATCH (abc) CALL (abd) { RETURN 1 AS x } RETURN x");
    assert_eq!(
        (f.text.as_str(), f.suggestion.as_deref()),
        ("abd", Some("abc"))
    );
    let f = fail("MATCH (a) CALL (a, a) { RETURN 1 AS x } RETURN x");
    assert!(f.message.contains("imported twice"), "{}", f.message);
    let f = fail("MATCH (a) CALL { WITH a, a RETURN 1 AS x } RETURN x");
    assert!(f.message.contains("imported twice"), "{}", f.message);
}

#[test]
fn a_returned_name_must_be_new_and_body_names_stay_inside() {
    for (query, text) in [
        ("MATCH (a) CALL (a) { RETURN a } RETURN a", "a"),
        (
            "MATCH (a) CALL { MATCH (b) RETURN b AS a } RETURN a",
            "b AS a",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("already defined outside"),
            "{query}: {}",
            f.message
        );
    }
    let f = fail("MATCH (a) CALL (a) { MATCH (a)-->(s) RETURN s.name AS n } RETURN s");
    assert_eq!(f.text, "s");
    assert!(
        f.message.contains("bound inside a CALL body"),
        "{}",
        f.message
    );
    // A later MATCH binding the name makes a new binding.
    let q = ok("MATCH (a) CALL (a) { MATCH (a)-->(s) RETURN s.name AS n } MATCH (s) RETURN s, n");
    let s: Vec<_> = q
        .bindings
        .iter()
        .filter(|b| b.name.as_ref().is_some_and(|n| n.name == "s"))
        .collect();
    assert_eq!(s.len(), 2);
}

#[test]
fn a_call_body_may_be_a_union_and_nest() {
    let q = ok(
        "MATCH (a) CALL (a) { MATCH (a)-->(b) RETURN b UNION ALL MATCH (a)<--(b) RETURN b } \
         RETURN b",
    );
    let call = the_call(&q);
    assert_eq!(call.body.first.unions.len(), 1);
    assert_eq!(names(&q, &call.imports), ["a"]);
    assert_eq!(
        q.binding(call.outputs[0]).map(|b| b.kind),
        Some(BindingKind::Node)
    );
    // Nested: the outer body imports what the inner one reads from outside.
    let q =
        ok("MATCH (a) CALL (a) { CALL (a) { MATCH (a)-->(b) RETURN b } RETURN b AS c } RETURN c");
    let outer = q
        .segments
        .iter()
        .filter_map(|s| match s {
            Segment::Call(call) => Some(call),
            _ => None,
        })
        .next_back()
        .expect("outer CALL");
    assert_eq!(names(&q, &outer.imports), ["a"]);
    assert_eq!(names(&q, &outer.outputs), ["c"]);
}

#[test]
fn clauses_of_match_only_bodies_follow_every_pipelines_clauses() {
    let q = ok("MATCH (a) WHERE EXISTS { MATCH (a)-->(x) } \
         CALL (a) { MATCH (a)-->(c) RETURN c } RETURN c");
    assert_eq!(q.matches.len(), 3);
    assert_eq!(q.subqueries[0].clauses, [2]);
    assert!(q.subqueries[0].segment >= q.segments.len());
    let x = q.binding(id(&q, "x")).expect("x");
    assert_eq!((x.clause, x.segment), (2, q.subqueries[0].segment));
}

#[test]
fn whole_query_bodies_bind_with_every_outer_name_in_scope() {
    let q = ok("MATCH (a) WHERE EXISTS { MATCH (a)-->(b) WITH b WHERE b.x > 1 RETURN b } RETURN a");
    let sub = &q.subqueries[0];
    assert!(sub.query.is_some() && sub.clauses.is_empty());
    assert_eq!(names(&q, &sub.imports), ["a"]);
    assert!(
        q.bindings
            .iter()
            .filter(|b| b.name.as_ref().is_some_and(|n| n.name == "b"))
            .all(|b| b.subquery == Some(0))
    );
    // Its names stay inside.
    let f = fail("MATCH (a) WHERE EXISTS { MATCH (a)-->(b) RETURN b } RETURN b");
    assert!(
        f.message.contains("bound inside a subquery"),
        "{}",
        f.message
    );
    // COUNT { } of MATCH clauses binds like EXISTS; COLLECT { } returns exactly one column.
    let q = ok("MATCH (a) WHERE COUNT { (a)-->() } > 1 RETURN a");
    assert_eq!(q.subqueries[0].kind, SubqueryKind::Count);
    assert_eq!(q.subqueries[0].clauses.len(), 1);
    let q = ok("MATCH (a) RETURN COLLECT { MATCH (a)-->(b) RETURN b.name } AS names");
    assert_eq!(q.subqueries[0].kind, SubqueryKind::Collect);
    assert_eq!(names(&q, &q.subqueries[0].imports), ["a"]);
    for query in [
        "MATCH (a) RETURN COLLECT { MATCH (a)-->(b) } AS names",
        "MATCH (a) RETURN COLLECT { MATCH (a)-->(b) RETURN b.name, b.x } AS names",
    ] {
        let f = fail(query);
        assert!(
            f.message.contains("exactly one column"),
            "{query}: {}",
            f.message
        );
        assert!(f.text.starts_with("COLLECT {"), "{query}: {}", f.text);
    }
    // A comprehension variable may not be bound again inside a whole-query body (not yet).
    let f = failure_of("RETURN [x IN [1] | EXISTS { MATCH (x) RETURN x }] AS l");
    assert_eq!(f.kind, GqlErrorKind::NotYet, "{f:?}");
}

#[test]
fn bodies_nest_at_most_max_body_depth_levels() {
    let calls = |levels: usize| {
        let mut q = "RETURN 1 AS x".to_string();
        for _ in 0..levels {
            q = format!("CALL {{ {q} }} RETURN x");
        }
        q
    };
    ok(&calls(MAX_BODY_DEPTH));
    let f = failure_of(&calls(MAX_BODY_DEPTH + 1));
    assert_eq!(
        (f.kind, f.gqlstatus.as_str()),
        (GqlErrorKind::Limit, "54001")
    );
    assert!(f.message.contains("deeper than 16 levels"), "{}", f.message);
    let bodies = |levels: usize| {
        let mut q = "(n)-->()".to_string();
        for _ in 0..levels {
            q = format!("EXISTS {{ MATCH (n) WITH n WHERE {q} RETURN n }}");
        }
        format!("MATCH (n) WHERE {q} RETURN n")
    };
    ok(&bodies(MAX_BODY_DEPTH));
    assert_eq!(
        failure_of(&bodies(MAX_BODY_DEPTH + 1)).kind,
        GqlErrorKind::Limit
    );
}

/// The deepest accepted bodies with the tallest expression inside bind on a test thread's
/// 2 MiB stack (debug build; measured in 2d.T2: about 1.8 MB at most, see its report).
#[test]
fn the_deepest_bodies_bind_within_the_stack() {
    let ors = |n: usize| {
        (0..n)
            .map(|i| format!("x = {i}"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let mut calls = format!("WITH 1 AS x RETURN {} AS y", ors(500));
    for _ in 0..MAX_BODY_DEPTH {
        calls = format!("CALL {{ {calls} UNION ALL RETURN 2 AS y }} RETURN y");
    }
    ok(&calls);
    let mut bodies = format!("EXISTS {{ LET x = 1 FILTER {} RETURN x }}", ors(440));
    for _ in 1..MAX_BODY_DEPTH {
        bodies = format!("EXISTS {{ MATCH (n) WITH n WHERE {bodies} RETURN n }}");
    }
    ok(&format!("MATCH (n) WHERE {bodies} RETURN n"));
    // A substituted tall expression on its own (13 MB before 2d.T2 split `substitute`).
    ok(&format!("LET x = 1 RETURN {} AS y", ors(500)));
}

/// Stage 2d.T5c removed the "not yet" guard: the constructs it named first bind together.
#[test]
fn the_former_not_yet_constructs_bind_together() {
    for query in [
        "MATCH (a) WHERE COUNT { (a)-->() } > 1 CALL (a) { RETURN 1 AS x } RETURN x",
        "MATCH (a) CALL (a) { RETURN 1 AS x } RETURN x, COUNT { (a)-->() } AS c",
        "MATCH (a) RETURN COLLECT { MATCH (a)-->(b) RETURN b.name } AS l, COUNT { (a)-->() } AS c",
        "MATCH (a) WHERE EXISTS { (a)-->() } RETURN a",
    ] {
        let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
        assert!(bind(&parsed).is_ok(), "{query}");
    }
}

/// A name bound inside `COUNT { }` / `COLLECT { }` and read outside: the message names the
/// construct (the `EXISTS` text stays for `EXISTS` and pattern predicates).
#[test]
fn count_and_collect_locals_name_their_construct() {
    for (query, construct, fix) in [
        (
            "MATCH (a) WHERE COUNT { MATCH (a)-->(b) } > 1 RETURN b",
            "COUNT { }",
            "inside the COUNT { } body",
        ),
        (
            "MATCH (a) RETURN COUNT { MATCH (a)-->(b) RETURN b } AS n, b",
            "COUNT { }",
            "inside the COUNT { } body",
        ),
        (
            "MATCH (a) RETURN COLLECT { MATCH (a)-->(b) RETURN b.name } AS l, b",
            "COLLECT { }",
            "return what you need of it from the body",
        ),
        (
            "MATCH (a) WITH a, COLLECT { MATCH (a)-->(b) RETURN b } AS l RETURN b",
            "COLLECT { }",
            "return what you need of it from the body",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.text, "b", "{query}");
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert!(
            f.message.contains(&format!(
                "it is bound inside a {construct} subquery, whose variables are visible only \
                 inside it"
            )),
            "{query}: {}",
            f.message
        );
        assert!(!f.message.contains("EXISTS"), "{query}: {}", f.message);
        let alternative = f.alternative.unwrap_or_default();
        assert!(alternative.contains(fix), "{query}: {alternative}");
        assert!(
            alternative.contains("MATCH clause"),
            "{query}: {alternative}"
        );
    }
    let f = fail("MATCH (a) WHERE EXISTS { MATCH (a)-->(b) } RETURN b");
    assert!(
        f.message
            .contains("bound inside a subquery (EXISTS or a pattern predicate)"),
        "{}",
        f.message
    );
}

/// A first `WITH` of a `CALL` body without a scope clause that is no importing `WITH` (a
/// `WHERE`, an alias, an expression, ...) imports nothing: an outer name it reads gets the
/// targeted message, also when a later `MATCH` of the body binds the same name.
#[test]
fn call_body_with_that_is_not_importing_says_why() {
    for query in [
        "MATCH (p) CALL { WITH p WHERE p.age > 30 MATCH (p)-->(x) RETURN x } RETURN x",
        "MATCH (p) CALL { WITH p WHERE p.age > 30 RETURN p.name AS n } RETURN n",
        "MATCH (p) CALL { WITH p.name AS n RETURN n AS m } RETURN m",
        "MATCH (p) CALL { WITH p AS q RETURN q AS m } RETURN m",
        "MATCH (p) CALL { WITH DISTINCT p RETURN p.name AS m } RETURN m",
        "MATCH (p) CALL { WITH p ORDER BY p.age LIMIT 1 RETURN p.name AS m } RETURN m",
    ] {
        let f = fail(query);
        assert_eq!(f.text, "p", "{query}");
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert!(
            f.message.contains(
                "the variable `p` is not imported into the CALL body: its first WITH is not \
                 an importing WITH, which lists plain variables only"
            ),
            "{query}: {}",
            f.message
        );
        assert!(!f.message.contains("later MATCH"), "{query}: {}", f.message);
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains("CALL (p) { MATCH ... WHERE ... }")
                && alternative.contains("CALL { WITH p MATCH ... WHERE ... }"),
            "{query}: {alternative}"
        );
    }
    // The importing forms and a scope clause stay valid; a name no outer scope holds keeps
    // its own message; a later WITH of the body is no first WITH.
    for query in [
        "MATCH (p) CALL { WITH p MATCH (p)-->(x) WHERE p.age > 30 RETURN x } RETURN x",
        "MATCH (p) CALL (p) { WITH p WHERE p.age > 30 RETURN p.name AS n } RETURN n",
        "MATCH (p) CALL { WITH 1 AS one RETURN one } RETURN one",
    ] {
        ok(query);
    }
    let f = fail("MATCH (p) CALL { WITH q WHERE q.age > 30 RETURN q.name AS n } RETURN n");
    assert!(!f.message.contains("importing WITH"), "{}", f.message);
    let f = fail("MATCH (p) CALL { MATCH (x) WITH x WHERE p.age > 1 RETURN x } RETURN x");
    assert!(!f.message.contains("importing WITH"), "{}", f.message);
    assert!(
        f.message.contains("not imported into the CALL body"),
        "{}",
        f.message
    );
}

// ----- stage 2e: parenthesized path patterns, wherever a pattern stands -----

#[test]
fn parenthesized_path_patterns_bind_wherever_a_pattern_stands() {
    // Since stage 2e.T2 they bind wherever a pattern stands; since stage 2e.T4 they lower
    // (`gql_qpp_tests::the_former_not_yet_forms_lower_and_run` runs these queries).
    for (query, text) in [
        (
            "MATCH ((a)-[:knows]->(b)){1,3} RETURN b",
            "((a)-[:knows]->(b))",
        ),
        ("MATCH (a) ((x)-->(y))? (b) RETURN x", "((x)-->(y))"),
        (
            "MATCH (a) ((x)-->(y) WHERE x.v > 1) (b) RETURN b",
            "((x)-->(y) WHERE x.v > 1)",
        ),
        (
            "MATCH (a) OPTIONAL MATCH (a)-->((x)-->(y))+ RETURN a",
            "((x)-->(y))",
        ),
        (
            "MATCH (n) WHERE EXISTS { (n) ((x)-->(m)){2} } RETURN n",
            "((x)-->(m))",
        ),
        (
            "MATCH (n) WHERE COUNT { MATCH (n) ((x)-->(y))* (m) RETURN m } > 0 RETURN n",
            "((x)-->(y))",
        ),
        ("MATCH (n) WHERE (n)-->(()-->()){2} RETURN n", "(()-->())"),
        (
            "CALL { MATCH ((x)-->(y)){2} RETURN x } RETURN x",
            "((x)-->(y))",
        ),
        (
            "MATCH (n) RETURN [(n)-->(m) WHERE EXISTS { (m) ((x)-->(y)){2} } | m]",
            "((x)-->(y))",
        ),
    ] {
        let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
        let bound = bind(&parsed).unwrap_or_else(|e| panic!("{query:?} does not bind: {e}"));
        let group = bound
            .matches
            .iter()
            .flat_map(|m| &m.paths)
            .flat_map(|path| &path.groups)
            .next()
            .unwrap_or_else(|| panic!("{query:?}: no parenthesized path pattern bound"));
        assert_eq!(
            query.get(group.group.span.start..group.group.span.end),
            Some(text),
            "{query}"
        );
    }
}

// ----- stage 2e: parenthesized path patterns -----

/// The kind of a binding by name and whether it is a group variable.
fn shape_of(q: &BoundQuery, name: &str) -> (BindingKind, bool) {
    let b = q.binding(id(q, name)).unwrap();
    (b.kind, b.group)
}

fn not_yet_of(query: &str) -> Failure {
    let f = failure_of(query);
    assert_eq!(
        (f.kind, f.gqlstatus.as_str()),
        (GqlErrorKind::NotYet, "0A000"),
        "{query}: {f:?}"
    );
    f
}

#[test]
fn quantified_path_patterns_bind_group_variables() {
    let q = ok("MATCH (a) ((x:person)-[e:knows]->(y)){1,3} (b) RETURN a, x, e, y, b");
    let path = &q.matches[0].paths[0];
    assert_eq!(path.start, id(&q, "a"));
    assert!(path.steps.is_empty());
    let [g] = &path.groups[..] else {
        panic!("{path:?}")
    };
    assert_eq!(g.group.start, id(&q, "x"));
    assert_eq!(g.group.steps.len(), 1);
    assert_eq!(
        (g.group.steps[0].edge, g.group.steps[0].node),
        (id(&q, "e"), id(&q, "y"))
    );
    assert_eq!(g.group.steps[0].quantifier, None);
    assert_eq!(g.group.steps[0].direction, EdgeDirection::Right);
    let quantifier = g.group.quantifier.unwrap();
    assert_eq!((quantifier.min, quantifier.max), (1, Some(3)));
    assert_eq!(g.node, id(&q, "b"));
    assert!(g.steps.is_empty());
    // The merge rule: `a` is the first iteration's `x`, `b` the last one's `y`.
    assert_eq!(path.node_before(0), Some(id(&q, "a")));
    assert_eq!(path.node_before(1), Some(id(&q, "b")));
    assert_eq!(path.node_before(2), None);
    // Body variables are group variables (lists outside the body), the outer nodes are not.
    assert_eq!(shape_of(&q, "x"), (BindingKind::Node, true));
    assert_eq!(shape_of(&q, "e"), (BindingKind::Edge, true));
    assert_eq!(shape_of(&q, "y"), (BindingKind::Node, true));
    assert_eq!(shape_of(&q, "a"), (BindingKind::Node, false));
    assert_eq!(shape_of(&q, "b"), (BindingKind::Node, false));
    // Ids follow the text; the body's labels stay on its bindings.
    assert_eq!(
        names(&q, &path.elements().collect::<Vec<_>>()),
        ["a", "x", "e", "y", "b"]
    );
    assert_eq!(q.binding(id(&q, "x")).unwrap().labels.len(), 1);
    // A group column is a value (the list), read like any column.
    assert_eq!(column_names(&q), ["a", "x", "e", "y", "b"]);
    assert!(q.binding(id(&q, "x")).unwrap().read);
    // `?` is {0,1} with conditional singletons (ISO, 2e P0 "After T2"); `{0,1}` written out,
    // `*` {0,}, `+` {1,} make group variables too.
    for (query, min, max, group) in [
        ("MATCH (a) ((x)-->(y))? (b) RETURN x", 0, Some(1), false),
        ("MATCH (a) ((x)-->(y)){0,1} (b) RETURN x", 0, Some(1), true),
        ("MATCH (a) ((x)-->(y))* (b) RETURN x", 0, None, true),
        ("MATCH (a) ((x)-->(y))+ (b) RETURN x", 1, None, true),
        ("MATCH (a) ((x)-->(y)){0} (b) RETURN x", 0, Some(0), true),
    ] {
        let q = ok(query);
        let g = &q.matches[0].paths[0].groups[0].group;
        let quantifier = g.quantifier.unwrap();
        assert_eq!((quantifier.min, quantifier.max), (min, max), "{query}");
        assert_eq!(g.conditional, !group, "{query}");
        assert_eq!(shape_of(&q, "x"), (BindingKind::Node, group), "{query}");
    }
}

#[test]
fn a_parenthesized_path_pattern_without_quantifier_binds_singletons() {
    // ISO: no quantifier, exactly one iteration; its variables keep a single value.
    let q = ok("MATCH (a) ((x)-[e]->(y) WHERE x.age < y.age) (b) RETURN x.name, e, y");
    assert_eq!(q.matches[0].paths[0].groups[0].group.quantifier, None);
    assert_eq!(shape_of(&q, "x"), (BindingKind::Node, false));
    assert_eq!(shape_of(&q, "e"), (BindingKind::Edge, false));
    // A later pattern may join a singleton like any variable.
    let q = ok("MATCH ((x)-->(y) WHERE x.v = 1) MATCH (y)-->(z) RETURN z");
    assert!(q.binding(id(&q, "y")).unwrap().joined());
}

#[test]
fn a_question_mark_binds_conditional_singletons() {
    // ISO (2e P0 "After T2"): `?` keeps single elements, NULL after zero iterations.
    let q = ok("MATCH (a) ((x)-[e]->(y) WHERE x.v < y.v)? (b) WHERE y.k = 1 RETURN x.name, e, y");
    assert!(q.matches[0].paths[0].groups[0].group.conditional);
    assert_eq!(shape_of(&q, "x"), (BindingKind::Node, false));
    assert_eq!(shape_of(&q, "e"), (BindingKind::Edge, false));
    // The outer condition runs after the body (y may be NULL there): a cross conjunct; the
    // body's own condition stays per iteration.
    let y = id(&q, "y");
    assert!(q.binding(y).unwrap().predicates.is_empty());
    assert_eq!(q.cross.len(), 1);
    assert_eq!(q.conjuncts[q.cross[0]].bindings, [y]);
    assert_eq!(q.matches[0].paths[0].groups[0].group.conjuncts.len(), 1);
    // An inline condition is the element's own.
    let q = ok("MATCH (a) ((x {k: 1})-->(y))? (b) RETURN x");
    assert_eq!(q.binding(id(&q, "x")).unwrap().predicates.len(), 1);
    assert!(q.cross.is_empty());
    // A singleton may be joined after its body; an outer name inside the body is not yet.
    let q = ok("MATCH ((x)-->(y))? MATCH (y)-->(z) RETURN z");
    assert!(q.binding(id(&q, "y")).unwrap().joined());
    let f = failure_of("MATCH (x) ((x)-->(y))? RETURN y");
    assert_eq!(f.kind, GqlErrorKind::NotYet, "{f:?}");
}

#[test]
fn implicit_nodes_around_parenthesized_path_patterns_are_anonymous_bindings() {
    let q = ok("MATCH ((x)-->(y)){2} RETURN x");
    let path = &q.matches[0].paths[0];
    let anonymous = |id: BindingId| q.binding(id).unwrap().name.is_none();
    assert!(anonymous(path.start));
    assert!(anonymous(path.groups[0].node));
    assert_eq!(path.node_before(0), Some(path.start));
    // After an edge, between two groups, and with plain steps after a group.
    let q = ok("MATCH (a)-->((x)-->(y))+ ((u)-[f]->(v)){2} (b)<--(c) RETURN a, b, c");
    let path = &q.matches[0].paths[0];
    let anonymous = |id: BindingId| q.binding(id).unwrap().name.is_none();
    assert_eq!(path.steps.len(), 1);
    assert_eq!(path.groups.len(), 2);
    let [first, second] = &path.groups[..] else {
        panic!("{path:?}")
    };
    assert_eq!(path.node_before(0), Some(path.steps[0].node));
    assert!(anonymous(path.steps[0].node));
    assert!(anonymous(first.node));
    assert_eq!(path.node_before(1), Some(first.node));
    assert_eq!(second.node, id(&q, "b"));
    assert_eq!(second.steps[0].node, id(&q, "c"));
    assert_eq!(second.steps[0].direction, EdgeDirection::Left);
    assert_eq!(path.node_before(2), Some(id(&q, "c")));
    assert_eq!(
        names(&q, &path.elements().collect::<Vec<_>>())
            .iter()
            .filter(|n| !n.starts_with('#'))
            .collect::<Vec<_>>(),
        ["a", "x", "y", "u", "f", "v", "b", "c"]
    );
    // A path variable spans the whole pattern.
    let q = ok("MATCH p = (a) ((x)-->(y)){1,2} (b) WHERE path_length(p) > 1 RETURN p");
    assert_eq!(q.matches[0].paths[0].variable, Some(id(&q, "p")));
    assert_eq!(q.cross.len(), 1);
}

#[test]
fn body_conditions_run_per_iteration() {
    let query = "MATCH (a) ((x {name: 'n'})-[e WHERE e.w > x.w]->(y WHERE y.v > 1) WHERE x.age < \
                 y.age AND e.w > $min AND $flag AND x.k = 2){1,3} (b) WHERE size(e) > 1 RETURN b";
    let q = ok(query);
    assert_eq!(
        conjuncts(query, &q),
        [
            "name: 'n' [\"x\"]",
            "e.w > x.w [\"x\", \"e\"]",
            "y.v > 1 [\"y\"]",
            "x.age < y.age [\"x\", \"y\"]",
            "e.w > $min [\"e\"]",
            "$flag []",
            "x.k = 2 [\"x\"]",
            "size(e) > 1 [\"e\"]",
        ]
    );
    let g = &q.matches[0].paths[0].groups[0].group;
    // On one body element: listed on it (run where it is matched in each iteration).
    assert_eq!(q.binding(id(&q, "x")).unwrap().predicates, [0, 6]);
    assert_eq!(q.binding(id(&q, "e")).unwrap().predicates, [4]);
    assert_eq!(q.binding(id(&q, "y")).unwrap().predicates, [2]);
    // On several body elements or none: the group's, never cross.
    assert_eq!(g.conjuncts, [1, 3, 5]);
    assert_eq!(q.conjuncts[3].origin, ConjunctOrigin::BodyWhere);
    // The clause WHERE reads the list: cross.
    assert_eq!(q.cross, [7]);
    // A body condition marks nothing read; reading the list outside does.
    assert!(!q.binding(id(&q, "x")).unwrap().read);
    assert!(!q.binding(id(&q, "y")).unwrap().read);
    assert!(q.binding(id(&q, "e")).unwrap().read);
    // A LET constant and a comprehension inside the body read no variable outside it.
    ok("LET k = 2 MATCH ((x)-->(y) WHERE x.v = k AND all(i IN [1] WHERE i < y.v)){2} RETURN y");
}

#[test]
fn body_conditions_read_only_the_body() {
    for (query, text) in [
        (
            "MATCH (a) ((x)-->(y) WHERE x.age < a.age){1,3} RETURN y",
            "x.age < a.age",
        ),
        (
            "MATCH (a) ((x {name: a.name})-->(y)){2} RETURN y",
            "name: a.name",
        ),
        (
            "MATCH (a) ((x)-[e WHERE e.w > a.w]->(y)){2} RETURN y",
            "e.w > a.w",
        ),
        (
            "MATCH (a) LET n = a.name MATCH ((x)-->(y) WHERE y.name = n){2} RETURN y",
            "y.name = n",
        ),
        // A singleton body too (P0: one scope rule for every body).
        (
            "MATCH (a) ((x)-->(y) WHERE x.v = a.v) RETURN y",
            "x.v = a.v",
        ),
        // A variable of a later pattern of the clause is outside too.
        (
            "MATCH ((x)-->(y) WHERE x.v = c.v){2}, (c) RETURN y",
            "x.v = c.v",
        ),
    ] {
        let f = not_yet_of(query);
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.message,
            "a condition inside a parenthesized path pattern that reads a variable declared \
             outside it is not yet supported in this version",
            "{query}"
        );
        assert!(
            f.alternative
                .as_deref()
                .is_some_and(|a| a.contains("WHERE of the MATCH clause")),
            "{query}: {f:?}"
        );
    }
    // A subquery inside a body is not yet supported (its body would run per iteration).
    for (query, text) in [
        (
            "MATCH ((x)-->(y) WHERE EXISTS { (y)-->() }){2} RETURN y",
            "EXISTS { (y)-->() }",
        ),
        ("MATCH ((x)-->(y WHERE (y)-->())){2} RETURN y", "(y)-->()"),
        (
            "MATCH ((x)-->(y) WHERE size([(y)-->(z) | z]) > 1){2} RETURN y",
            "[(y)-->(z) | z]",
        ),
        (
            "MATCH ((x)-->(y) WHERE COUNT { MATCH (y)-->(z) RETURN z } > 1){2} RETURN y",
            "COUNT { MATCH (y)-->(z) RETURN z }",
        ),
    ] {
        let f = not_yet_of(query);
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message
                .starts_with("a subquery inside a parenthesized path pattern"),
            "{query}"
        );
    }
    // An aggregate is refused as in any pattern.
    let f = fail("MATCH ((x)-->(y) WHERE count(x) > 1){2} RETURN y");
    assert!(f.message.contains("in a pattern"), "{}", f.message);
}

#[test]
fn body_variables_are_new_names() {
    // A name bound outside: a group variable must be new (42002); in a body without a
    // quantifier, a join is not yet supported.
    let f = fail("MATCH (a) ((a)-->(y)){2} RETURN y");
    assert_eq!(f.text, "a");
    assert!(f.message.contains("must be a new name"), "{}", f.message);
    assert!(
        f.alternative
            .as_deref()
            .is_some_and(|a| a.contains("first node of its first iteration"))
    );
    let f = fail("MATCH (a)-[e]->(b) MATCH ((x)-[e]->(y)){2} RETURN y");
    assert_eq!(f.text, "e");
    let f = fail("LET a = 1 MATCH ((a)-->(y)){2} RETURN y");
    assert_eq!(f.text, "a");
    let f = fail("MATCH ((x)-->(y)){2} ((x)-->(z)){2} RETURN z");
    assert!(f.message.contains("must be a new name"), "{}", f.message);
    let f = not_yet_of("MATCH (a) ((a)-->(y) WHERE y.v = 1) RETURN y");
    assert_eq!(f.text, "a");
    assert!(
        f.message
            .starts_with("a variable bound outside a parenthesized path pattern")
    );
    // Twice inside one body: not yet (the alternative compares two variables).
    let f = not_yet_of("MATCH ((x)-->(x)){2} RETURN x");
    assert_eq!(
        (f.text.as_str(), f.message.as_str()),
        (
            "x",
            "a variable written twice inside a parenthesized path pattern is not yet supported in this version"
        )
    );
    assert!(
        f.alternative
            .as_deref()
            .is_some_and(|a| a.contains("element_id(x) = element_id(x2)"))
    );
    let f = not_yet_of("MATCH ((x)-[e]->(y)-[e]->(z)){2} RETURN x");
    assert_eq!(f.text, "e");
    // A group variable bound again after its body: in the same clause, a later clause, a
    // subquery, another body.
    for (query, text) in [
        ("MATCH ((x)-->(y)){2}, (x)-->(z) RETURN z", "x"),
        ("MATCH ((x)-->(y)){2} (b) MATCH (y)-->(b) RETURN b", "y"),
        ("MATCH ((x)-[e]->(y)){2} (b)-[e]->(c) RETURN c", "e"),
        (
            "MATCH ((x)-->(y)){2} WHERE EXISTS { (x)-->() } RETURN y",
            "x",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message
                .contains("group variable of the quantified path pattern"),
            "{query}: {}",
            f.message
        );
    }
    // A singleton body variable read again later is a join (above); a path variable inside
    // the body is refused by the parser (E17).
}

#[test]
fn group_variables_of_a_body_are_lists_outside_it() {
    // A property of a group variable outside its body has no single value.
    for (query, text) in [
        ("MATCH ((x)-->(y)){2} RETURN x.name", "x"),
        ("MATCH ((x)-->(y)){2} WHERE y.age > 1 RETURN x", "y"),
        ("MATCH ((x)-[e]->(y)){2} (b WHERE b.v = e.w) RETURN b", "e"),
        ("MATCH ((x)-->(y)){2} RETURN x ORDER BY y.age", "y"),
    ] {
        let f = fail(query);
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message
                .contains("is a group variable of the quantified path pattern ((x)-")
                && f.message.contains("has no single value"),
            "{query}: {}",
            f.message
        );
    }
    // The list itself may be read anywhere.
    ok("MATCH ((x)-->(y)){1,3} (b) WHERE size(x) > 1 RETURN [v IN y | v.name] AS names, x");
    let q = ok("MATCH ((x)-->(y)){1,3} WITH x, count(*) AS c RETURN x, c");
    let with = q.segments.iter().find_map(|s| match s {
        Segment::With(w) => Some(w),
        _ => None,
    });
    let x = with.unwrap().outputs[0];
    assert_eq!(q.binding(x).unwrap().kind, BindingKind::Value);
}

#[test]
fn parenthesized_path_patterns_in_subqueries_and_bodies() {
    // A body's group variables are locals of the subquery; the node before is an import.
    let q = ok("MATCH (n) WHERE EXISTS { (n) ((x)-->(y)){2} (m) } RETURN n");
    let sub = &q.subqueries[0];
    assert_eq!(names(&q, &sub.imports), ["n"]);
    assert_eq!(q.binding(id(&q, "x")).unwrap().subquery, Some(0));
    assert_eq!(shape_of(&q, "x"), (BindingKind::Node, true));
    let f = fail("MATCH (n) WHERE EXISTS { (n) ((x)-->(y)){2} } RETURN x");
    assert_eq!(f.text, "x");
    // OPTIONAL MATCH, a CALL body (its imports), inside a pattern comprehension's body (the
    // pattern of a comprehension does not parse a parenthesized path pattern yet).
    ok("MATCH (a) OPTIONAL MATCH (a) ((x)-->(y)){1,2} (b) RETURN a, x, b");
    let q = ok("MATCH (a) CALL (a) { MATCH (a) ((x)-->(y)){2} (b) RETURN b } RETURN b");
    let call = q.segments.iter().find_map(|s| match s {
        Segment::Call(c) => Some(c),
        _ => None,
    });
    assert_eq!(names(&q, &call.unwrap().imports), ["a"]);
    ok("MATCH (a) RETURN [(a)-->(m) WHERE EXISTS { (m) ((x)-->(y)){2} } | m] AS l");
    // A pattern predicate may not introduce a body variable.
    let f = fail("MATCH (n) WHERE (n)-->((x)-->(y)){2} RETURN n");
    assert_eq!(f.text, "x");
    assert!(
        f.message.contains("introduces the new variable `x`"),
        "{}",
        f.message
    );
    ok("MATCH (n) WHERE (n)-->(()-->()){2} RETURN n");
    // A body variable named by a later MATCH clause is not defined before it.
    let f = fail("MATCH (a) WHERE x IS NOT NULL MATCH (a) ((x)-->(y)){2} RETURN a");
    assert_eq!(f.text, "x");
    // A comprehension variable may not be named in a body inside a subquery (E14).
    let f = failure_of("MATCH (a) RETURN [x IN [1] WHERE EXISTS { (a) ((x)-->(y)){2} }] AS l");
    assert_eq!(f.text, "x");
}

#[test]
fn every_body_edge_counts_toward_the_edge_limit() {
    // 64 edges with the body's: accepted; one more: refused at the 65th.
    let body = format!("((x){}){{1,3}}", "-->()".repeat(4));
    let plain = "-->()".repeat(MAX_MATCH_EDGES - 4);
    ok(&format!("MATCH (a){plain} {body} RETURN a"));
    let query = format!("MATCH (a){plain} {body}-[z]->() RETURN a");
    let f = failure_of(&query);
    assert_eq!((f.kind, f.text.as_str()), (GqlErrorKind::Limit, "-[z]->"));
    // In a subquery body too, and a long body alone.
    let query = format!(
        "MATCH (n) WHERE EXISTS {{ (n) ((x){}) }} RETURN n",
        "-->()".repeat(65)
    );
    let f = failure_of(&query);
    assert_eq!(f.kind, GqlErrorKind::Limit, "{f:?}");
}

/// Binding never panics on combinations of parenthesized path patterns, and every bound body
/// keeps its invariants: consecutive new bindings, group variables exactly when quantified
/// (not by `?`),
/// its conditions on its own bindings only and never cross.
#[test]
fn binding_never_panics_on_parenthesized_path_patterns() {
    let befores = ["", "(a)", "(a)-->", "(a)-[r]->(c)", "p = (a)"];
    let bodies = [
        "((x)-[e]->(y))",
        "((x:person {v: 1})-[e:knows WHERE e.w > x.w]->(y)<--(z) WHERE x.v < z.v AND $p)",
        "(()-->())",
        "((x)-->(a))",
        "((x)-->(x))",
        "((x)-->(y) WHERE x.v = a.v)",
        "((x)-->(y) WHERE EXISTS { (y)-->() })",
    ];
    let quantifiers = ["", "{1,3}", "?", "*", "+", "{0}"];
    let afters = ["", " (b)", " (b)-->(d)", " ((u)-->(w)){2}", "-->(x)"];
    let tails = [
        "RETURN *",
        "WHERE size(x) > 0 RETURN x",
        "WHERE x.v > 1 RETURN 1",
        "MATCH (x)-->() RETURN 1",
        "RETURN [v IN y | v.name] AS l",
    ];
    let mut bound_bodies = 0;
    for before in befores {
        for body in bodies {
            for quantifier in quantifiers {
                for after in afters {
                    for tail in tails {
                        let query = format!("MATCH {before}{body}{quantifier}{after} {tail}");
                        let Ok(parsed) = parse(&query) else { continue };
                        let Ok(q) = bind(&parsed) else { continue };
                        for path in q.matches.iter().flat_map(|m| &m.paths) {
                            for g in &path.groups {
                                bound_bodies += 1;
                                let ids = std::iter::once(g.group.start)
                                    .chain(g.group.steps.iter().flat_map(|s| [s.edge, s.node]));
                                let first = g.group.start.0;
                                for (i, id) in ids.enumerate() {
                                    assert_eq!(id.0, first + i, "{query}");
                                    let b = q.binding(id).unwrap();
                                    assert_eq!(
                                        b.group,
                                        g.group.quantifier.is_some() && !g.group.conditional,
                                        "{query}"
                                    );
                                }
                                let range = g.group.bindings();
                                for c in &g.group.conjuncts {
                                    assert!(!q.cross.contains(c), "{query}");
                                    let bindings = &q.conjuncts[*c].bindings;
                                    assert!(
                                        bindings.iter().all(|id| range.contains(*id)),
                                        "{query}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(bound_bodies > 300, "{bound_bodies}");
}

// ----- stage 3a.T2: INSERT / CREATE, FINISH, queries ending in a write -----

fn insert_of(q: &BoundQuery, segment: usize) -> &BoundInsert {
    match q.segments.get(segment) {
        Some(Segment::Write(write)) => match &**write {
            BoundWrite::Insert(insert) => insert,
            other => panic!("segment {segment} is no INSERT: {other:?}"),
        },
        other => panic!("segment {segment} is no write: {other:?}"),
    }
}

fn label_texts(labels: &[Identifier]) -> Vec<&str> {
    labels.iter().map(|l| l.name.as_str()).collect()
}

#[test]
fn insert_binds_new_nodes_and_edges() {
    let query = "INSERT (a:person&admin&person {name: 'ann', age: 1 + 1})-[e:knows {since: \
                 2020}]->(b IS person), (:city)<-[:lives]-(b)";
    let q = ok(query);
    assert_eq!(segment_shapes(&q), ["insert3+2"]);
    assert!(q.unit, "a leading INSERT starts from the unit row");
    assert_eq!(q.end, QueryEnd::Write);
    assert!(q.columns.is_empty());
    assert!(q.writes());
    let insert = insert_of(&q, 0);
    assert!(!insert.cypher);
    assert_eq!(&query[insert.keyword.start..insert.keyword.end], "INSERT");
    let (a, e, b) = (id(&q, "a"), id(&q, "e"), id(&q, "b"));
    let nodes: Vec<BindingId> = insert.nodes.iter().map(|n| n.binding).collect();
    assert_eq!(nodes[..2], [a, b]);
    assert_eq!(label_texts(&insert.nodes[0].labels), ["person", "admin"]);
    assert_eq!(label_texts(&insert.nodes[1].labels), ["person"]);
    assert_eq!(label_texts(&insert.nodes[2].labels), ["city"]);
    let keys: Vec<&str> = insert.nodes[0]
        .properties
        .iter()
        .map(|p| p.key.name.as_str())
        .collect();
    assert_eq!(keys, ["name", "age"]);
    let age = &insert.nodes[0].properties[1];
    assert_eq!(&query[age.span.start..age.span.end], "age: 1 + 1");
    assert!(age.bindings.is_empty());
    let knows = &insert.edges[0];
    assert_eq!(
        (knows.binding, knows.source, knows.target),
        (e, a, b),
        "-[e]-> leaves a and enters b"
    );
    assert_eq!(knows.label.name, "knows");
    assert_eq!(knows.properties.len(), 1);
    // `<-[:lives]-(b)`: the edge leaves b and enters the city.
    let lives = &insert.edges[1];
    assert_eq!((lives.source, lives.target), (b, insert.nodes[2].binding));
    assert_eq!(lives.direction, EdgeDirection::Left);
    assert!(insert.reads.is_empty(), "nothing bound before the clause");
    for binding in [a, e, b] {
        let binding = q.binding(binding).unwrap();
        assert_eq!(binding.segment, 0);
        assert_eq!(binding.column, None);
    }
    assert_eq!(q.binding(e).unwrap().kind, BindingKind::Edge);
    // CREATE is the same clause.
    let q = ok("create (a:person)-[:knows]->(a)");
    let insert = insert_of(&q, 0);
    assert!(insert.cypher);
    assert_eq!(insert.nodes.len(), 1);
    let self_loop = &insert.edges[0];
    assert_eq!(self_loop.source, self_loop.target);
}

#[test]
fn insert_refers_to_bound_nodes_and_cuts_the_pipeline() {
    let query = "MATCH (n:person) WHERE n.age > 30 INSERT (n)-[:knows]->(m:person {age: n.age + 1}) \
                 RETURN m, n";
    let q = ok(query);
    assert_eq!(segment_shapes(&q), ["match[0]", "insert1+1"]);
    let (n, m) = (id(&q, "n"), id(&q, "m"));
    let insert = insert_of(&q, 1);
    assert_eq!(insert.nodes[0].binding, m);
    assert_eq!((insert.edges[0].source, insert.edges[0].target), (n, m));
    assert_eq!(insert.reads, [n]);
    assert_eq!(insert.nodes[0].properties[0].bindings, [n]);
    assert!(
        q.binding(n).unwrap().read,
        "the edge step and the property read n"
    );
    assert!(q.binding(m).unwrap().read, "an end of the new edge");
    assert_eq!(q.binding(m).unwrap().segment, 1);
    assert_eq!(q.end, QueryEnd::Return);
    assert_eq!(column_names(&q), ["m", "n"]);
    // A clause after the write opens a segment of its own (design W9).
    let q = ok("MATCH (n) INSERT (n)-[:T]->(m) MATCH (m)-->(x) WHERE x.k = n.k RETURN x");
    assert_eq!(segment_shapes(&q), ["match[0]", "insert1+1", "match[1]"]);
    let q = ok("INSERT (a:X) FILTER a.k IS NULL RETURN a");
    assert_eq!(segment_shapes(&q), ["insert1+0", "match[]"]);
    // RETURN * lists the named variables of the INSERT in the order they entered the scope.
    let q = ok("MATCH (n) INSERT (n)-[e:T]->(m), (:X) RETURN *");
    assert_eq!(column_names(&q), ["n", "e", "m"]);
    // A FOR item may be an end (design E3: it must hold a node when the edge is created).
    let q = ok("MATCH p = (a)-->(b) FOR x IN nodes(p) INSERT (x)-[:T]->(:Y)");
    assert_eq!(q.binding(id(&q, "x")).unwrap().kind, BindingKind::Node);
    // A WITH column holding a node is a node.
    let q = ok("MATCH (n) WITH DISTINCT n INSERT (n)-[:T]->(:Y)");
    let insert = insert_of(&q, 2);
    assert_eq!(insert.reads.len(), 1);
    // NEXT after a write: the RETURN before it is a WITH.
    let q = ok("INSERT (a:X) RETURN a NEXT MATCH (a)-[:T]->(b) RETURN b");
    assert_eq!(segment_shapes(&q), ["insert1+0", "match[0]"]);
}

#[test]
fn insert_reference_and_variable_rules() {
    for (query, text, message) in [
        (
            "MATCH (n) INSERT (n:X)-[:T]->(m)",
            "n",
            "the variable `n` is already bound; INSERT refers to a bound node only as (n), \
             without labels or properties",
        ),
        (
            "MATCH (n) CREATE (n {k: 1})-[:T]->(m)",
            "n",
            "the variable `n` is already bound; CREATE refers to a bound node only as (n), \
             without labels or properties",
        ),
        (
            "INSERT (a:X), (a:Y)-[:T]->(b)",
            "a",
            "the node `a` is created earlier in this INSERT; a later occurrence refers to it and \
             carries no labels or properties",
        ),
        (
            "MATCH ()-[e]->() INSERT (a)-[e:T]->(b)",
            "e",
            "the variable `e` is already bound; INSERT creates a new edge, whose variable must \
             be a new name",
        ),
        (
            "INSERT (a)-[e:T]->(b), (b)-[e:T]->(c)",
            "e",
            "the variable `e` is already bound; INSERT creates a new edge, whose variable must \
             be a new name",
        ),
        (
            "MATCH ()-[e]->() INSERT (e)-[:T]->(b)",
            "e",
            "the variable `e` is used as a node here but is already bound to an edge; a variable \
             is either a node or an edge",
        ),
        (
            "INSERT (a)-[e:T]->(e)",
            "e",
            "the variable `e` is used as a node here but is already bound to an edge; a variable \
             is either a node or an edge",
        ),
        (
            "MATCH p = (a)-->(b) INSERT (p)-[:T]->(b)",
            "p",
            "the variable `p` is used as a node here but is a path variable; a path variable \
             cannot name a node or an edge",
        ),
        (
            "LET x = 1 INSERT (x)-[:T]->(b)",
            "x",
            "`x` is a value defined by LET or WITH, not a node; a pattern variable is a new name \
             or a node or edge variable",
        ),
        (
            "FOR x IN [1] WITH ORDINALITY i INSERT (i)-[:T]->(b)",
            "i",
            "the variable `i` is the position of a FOR item (a number), not a node",
        ),
        (
            "MATCH (n) INSERT (n)",
            "(n)",
            "the pattern (n) creates nothing: `n` is already bound; a node pattern in INSERT \
             creates a node unless an edge connects it",
        ),
        (
            "INSERT (a:X), (a)",
            "(a)",
            "the pattern (a) creates nothing: `a` is already bound; a node pattern in INSERT \
             creates a node unless an edge connects it",
        ),
        (
            "MATCH (a) ((x)-[r]->(y)){1,2} (b) INSERT (x)-[:T]->(b)",
            "x",
            "`x` is a group variable (the list of the nodes of a quantified path pattern), not \
             one node; an edge in INSERT connects two nodes",
        ),
        (
            "INSERT ({a: 1, a: 2})",
            "a",
            "the key `a` appears twice in this map",
        ),
        ("INSERT (:X {v: y})", "y", "the variable `y` is not defined"),
        (
            "MATCH (n) WITH n.age AS a INSERT (:X {v: n.age})",
            "n",
            "the variable `n` is not in scope: the WITH before does not carry it",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.message, message, "{query}");
    }
    // An aggregate cannot stand in a property map (design W8).
    let f = fail("MATCH (n) INSERT (:X {c: count(n)})");
    assert_eq!(
        f.message,
        "the aggregate function count() cannot be used in INSERT"
    );
    assert!(
        f.alternative
            .as_deref()
            .is_some_and(|a| a.contains("WITH p, count(f.age) AS n INSERT")),
        "{f:?}"
    );
    let f = fail("MATCH (n) CREATE (:X {c: collect_list(n.k)})");
    assert_eq!(
        f.message,
        "the aggregate function collect_list() cannot be used in CREATE"
    );
    // Reading an element the same clause creates: not yet.
    for (query, text) in [
        ("INSERT (a {x: 1}), (b {y: a.x})", "a.x"),
        ("INSERT (a)-[e:T {w: b.k}]->(b)", "b.k"),
        ("INSERT (a {x: 1})-[:T {w: a.x}]->(b)", "a.x"),
    ] {
        let parsed = parse(query).unwrap();
        let f = failure(query, bind(&parsed));
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.message,
            "reading an element that the same INSERT creates is not yet supported in this version"
        );
    }
}

#[test]
fn substitutions_read_across_a_write_are_materialized_before_it() {
    // Design W10: `a` stands for `n.age`; read after the write, it is the value from before it.
    let query = "MATCH (n) WITH n, n.age AS a INSERT (:X {v: a})";
    let q = ok(query);
    assert_eq!(segment_shapes(&q), ["match[0]", "with2", "insert1+0"]);
    let Some(Segment::With(with)) = q.segments.get(1) else {
        panic!("{:?}", q.segments);
    };
    assert_eq!(
        with.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["n", "a"]
    );
    assert_eq!(with.columns[1].bindings, [id(&q, "n")]);
    let a = id(&q, "a");
    let column = q.binding(a).unwrap().column.unwrap();
    assert_eq!((column.segment, column.column), (1, 1));
    let value = &insert_of(&q, 2).nodes[0].properties[0];
    assert_eq!(value.bindings, [a], "the value reads the column, not n.age");
    // LET too; the RETURN reads the column.
    let q = ok("MATCH (n) LET a = n.age INSERT (n)-[:T]->(:X) RETURN a, n");
    assert_eq!(segment_shapes(&q), ["match[0]", "with2", "insert1+1"]);
    assert_eq!(q.columns[0].bindings, [id(&q, "a")]);
    let n_column = q.columns[1].variable.unwrap();
    assert_ne!(n_column, id(&q, "n"), "n continues as the WITH column");
    assert_eq!(insert_of(&q, 2).edges[0].source, n_column);
    // A value holding a subquery reads the graph too.
    // (`match[0]` is the pipeline of the COUNT body.)
    let q = ok("LET c = COUNT { MATCH (m) RETURN m } INSERT (:X) RETURN c");
    assert_eq!(segment_shapes(&q), ["match[0]", "with1", "insert1+0"]);
    // A constant stays a substitution: nothing to materialize.
    let q = ok("LET a = 1, b = $p INSERT (:X {v: a, w: b}) RETURN a");
    assert_eq!(segment_shapes(&q), ["insert1+0"]);
    let values: Vec<&Expr> = insert_of(&q, 0).nodes[0]
        .properties
        .iter()
        .map(|p| &p.value)
        .collect();
    assert!(matches!(values[0].kind, ExprKind::Literal(_)), "{values:?}");
    assert!(
        matches!(values[1].kind, ExprKind::Parameter(_)),
        "{values:?}"
    );
    // Materialized once: a second write reads the columns.
    let q = ok("MATCH (n) LET a = n.age INSERT (:X) INSERT (:Y {v: a}) RETURN a");
    assert_eq!(
        segment_shapes(&q),
        ["match[0]", "with2", "insert1+0", "insert1+0"]
    );
    // A CALL whose body writes is a write: the values are materialized before it.
    let q = ok("MATCH (n) LET a = n.age CALL (n) { INSERT (n)-[:T]->(:X) } RETURN a");
    assert_eq!(
        segment_shapes(&q),
        ["match[0]", "with2", "insert1+1", "call0"]
    );
    // Without a write a projecting WITH stays a substitution (read-only queries bind as before).
    let q = ok("MATCH (n) WITH n, n.age AS a RETURN a");
    assert_eq!(segment_shapes(&q), ["match[0]"]);
    assert!(!q.writes());
    let q = ok("MATCH (n) LET a = n.age CALL (n) { MATCH (n)-->(m) RETURN m } RETURN a, m");
    assert_eq!(segment_shapes(&q), ["match[0]", "match[1]", "call1"]);
}

#[test]
fn writes_inside_call_bodies() {
    let q = ok("MATCH (p) CALL (p) { INSERT (p)-[:T]->(:X) } RETURN p");
    assert_eq!(segment_shapes(&q), ["match[0]", "insert1+1", "call0"]);
    let Some(Segment::Call(call)) = q.segments.get(2) else {
        panic!("{:?}", q.segments);
    };
    assert_eq!(call.imports, [id(&q, "p")]);
    assert!(call.outputs.is_empty(), "a unit subquery returns no column");
    assert_eq!(call.body.first.first.end, QueryEnd::Write);
    assert!(
        call.body.first.first.unit,
        "the body starts from the outer row"
    );
    assert!(q.writes());
    // A body that writes and returns.
    let q = ok("MATCH (p) CALL (p) { INSERT (p)-[:T]->(x:X) RETURN x } RETURN p, x");
    let Some(Segment::Call(call)) = q.segments.get(2) else {
        panic!("{:?}", q.segments);
    };
    assert_eq!(call.outputs.len(), 1);
    assert_eq!(column_names(&q), ["p", "x"]);
    // Cypher's importing WITH, and OPTIONAL CALL.
    let q = ok("MATCH (p) OPTIONAL CALL { WITH p CREATE (p)-[:T]->(:X) } RETURN p");
    assert!(q.writes());
    // A name the body does not import is a new node of the body (as in Neo4j).
    let q = ok("MATCH (p) CALL { INSERT (p)-[:T]->(:X) } RETURN p");
    let Some(Segment::Call(call)) = q.segments.get(2) else {
        panic!("{:?}", q.segments);
    };
    assert!(call.imports.is_empty());
    assert_eq!(insert_of(&q, 1).nodes.len(), 2);
}

#[test]
fn finish_and_write_endings_return_no_row() {
    let q = ok("MATCH (n) FINISH");
    assert!(matches!(q.end, QueryEnd::Finish(_)));
    assert!(q.columns.is_empty() && q.order_by.is_empty());
    assert!(!q.writes());
    let q = ok("MATCH (n) INSERT (n)-[:T]->(:X) FINISH");
    assert!(matches!(q.end, QueryEnd::Finish(_)));
    assert!(q.writes());
    let q = ok("MATCH (n) INSERT (n)-[:T]->(:X);");
    assert_eq!(q.end, QueryEnd::Write);
    let q = ok("MATCH (n) RETURN n");
    assert_eq!(q.end, QueryEnd::Return);
}

#[test]
fn a_read_error_before_an_insert_comes_first() {
    // A read clause before the INSERT is bound first: its errors come first.
    let parsed = parse("MATCH (n) WHERE x > 1 INSERT (m)").unwrap();
    let f = failure("MATCH (n) WHERE x > 1 INSERT (m)", bind(&parsed));
    assert_eq!(f.kind, GqlErrorKind::Semantic, "{f:?}");
}

#[test]
fn insert_property_subqueries_bind_with_their_segment() {
    // A whole-query subquery body adds segments while the map is bound; the new bindings
    // still belong to the write segment.
    let q = ok("MATCH (n) INSERT (n)-[:T]->(x:X {c: COUNT { MATCH (n)-->(m) RETURN m }}) RETURN x");
    let write = q
        .segments
        .iter()
        .position(|s| matches!(s, Segment::Write(_)))
        .unwrap();
    assert_eq!(q.binding(id(&q, "x")).unwrap().segment, write);
    assert_eq!(insert_of(&q, write).reads, [id(&q, "n")]);
}

// ----- stage 3c.T1: SET / REMOVE -----

fn set_of(q: &BoundQuery, segment: usize) -> &BoundSet {
    match q.segments.get(segment) {
        Some(Segment::Write(write)) => match &**write {
            BoundWrite::Set(set) => set,
            other => panic!("segment {segment} is no SET: {other:?}"),
        },
        other => panic!("segment {segment} is no write: {other:?}"),
    }
}

fn remove_of(q: &BoundQuery, segment: usize) -> &BoundRemove {
    match q.segments.get(segment) {
        Some(Segment::Write(write)) => match &**write {
            BoundWrite::Remove(remove) => remove,
            other => panic!("segment {segment} is no REMOVE: {other:?}"),
        },
        other => panic!("segment {segment} is no write: {other:?}"),
    }
}

/// The edge labels of a target, `None` unless it is an edge restricted to names.
fn edge_labels(target: &WriteTarget) -> Option<Vec<&str>> {
    match &target.kind {
        TargetKind::Edge {
            labels: Some(labels),
        } => Some(label_texts(labels)),
        _ => None,
    }
}

#[test]
fn set_binds_targets_values_and_reads() {
    let query = "MATCH (n:person)-[e:knows|likes]->(m) SET n.age = m.age + 1, (e).w = $w, \
                 m = {a: 1}, m += n, n:A:B RETURN n";
    let q = ok(query);
    assert_eq!(segment_shapes(&q), ["match[0]", "set5"]);
    assert!(q.writes());
    assert_eq!(q.end, QueryEnd::Return);
    let (n, e, m) = (id(&q, "n"), id(&q, "e"), id(&q, "m"));
    let set = set_of(&q, 1);
    assert_eq!(&query[set.keyword.start..set.keyword.end], "SET");
    assert!(query[set.span.start..set.span.end].ends_with("n:A:B"));
    let targets: Vec<BindingId> = set.items.iter().map(|i| i.target.binding).collect();
    assert_eq!(targets, [n, e, m, m, n]);
    assert_eq!(set.items[0].target.kind, TargetKind::Node);
    assert_eq!(
        edge_labels(&set.items[1].target),
        Some(vec!["knows", "likes"])
    );
    assert_eq!(
        &query[set.items[1].target.span.start..set.items[1].target.span.end],
        "e"
    );
    match &set.items[0].kind {
        BoundSetKind::Property { key, value } => {
            assert_eq!(key.name, "age");
            assert_eq!(value.bindings, [m]);
        }
        other => panic!("{other:?}"),
    }
    match &set.items[1].kind {
        BoundSetKind::Property { value, .. } => {
            assert!(value.bindings.is_empty());
            assert!(matches!(value.expr.kind, ExprKind::Parameter(_)));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&set.items[2].kind, BoundSetKind::Replace(v) if v.bindings.is_empty()));
    assert!(matches!(&set.items[3].kind, BoundSetKind::Merge(v) if v.bindings == [n]));
    match &set.items[4].kind {
        BoundSetKind::Labels(labels) => assert_eq!(label_texts(labels), ["A", "B"]),
        other => panic!("{other:?}"),
    }
    let mut reads = vec![n, e, m];
    reads.sort_unstable();
    assert_eq!(set.reads, reads, "the targets and what the values read");
    assert!(reads.iter().all(|id| q.binding(*id).unwrap().read));
    assert_eq!(q.parameters.len(), 1);
    // Edge targets: the label names of the pattern when they are names; else any label.
    for (query, labels) in [
        ("MATCH ()-[e]->() SET e.w = 1", None),
        ("MATCH ()-[e:knows]->() SET e.w = 1", Some(vec!["knows"])),
        ("MATCH ()-[e:!knows]->() SET e.w = 1", None),
        ("MATCH ()-[e:%]->() SET e.w = 1", None),
        ("MATCH ()-[e]->() WITH DISTINCT e SET e.w = 1", None),
    ] {
        let q = ok(query);
        let write = q.segments.len() - 1;
        let target = &set_of(&q, write).items[0].target;
        assert!(
            matches!(target.kind, TargetKind::Edge { .. }),
            "{query}: {target:?}"
        );
        assert_eq!(edge_labels(target), labels, "{query}");
    }
    // Targets whose kind only the run knows: a FOR item, a column of a value, a CALL column.
    for query in [
        "FOR x IN [1, 2] SET x.k = 1",
        "MATCH (n) WITH head(collect(n)) AS x SET x.k = 1",
        "MATCH (n) CALL (n) { RETURN head([n]) AS x } SET x.k = 1",
        "MATCH (n) WITH collect(n) AS l UNWIND l AS x REMOVE x:A",
    ] {
        let q = ok(query);
        let write = q.segments.len() - 1;
        let target = match q.segments.get(write) {
            Some(Segment::Write(w)) => match &**w {
                BoundWrite::Set(set) => &set.items[0].target,
                BoundWrite::Remove(remove) => &remove.items[0].target,
                other => panic!("{other:?}"),
            },
            other => panic!("{query}: {other:?}"),
        };
        assert_eq!(target.kind, TargetKind::Unknown, "{query}");
    }
    // A column holding a node variable is a node.
    let q = ok("MATCH (n) CALL (n) { RETURN n AS x } SET x.k = 1");
    assert_eq!(set_of(&q, 2).items[0].target.kind, TargetKind::Node);
    // A LET / WITH alias of a variable is that variable.
    let q = ok("MATCH (p) LET q = p WITH q AS r SET r.k = 1");
    assert_eq!(set_of(&q, 1).items[0].target.binding, id(&q, "p"));
    assert_eq!(set_of(&q, 1).items[0].target.kind, TargetKind::Node);
}

#[test]
fn remove_binds_targets_and_reads() {
    let query = "MATCH (n)-[e]->(m) REMOVE n.age, (e).w, m:L1:L3, n IS A";
    let q = ok(query);
    assert_eq!(segment_shapes(&q), ["match[0]", "remove4"]);
    assert_eq!(q.end, QueryEnd::Write);
    assert!(q.columns.is_empty());
    let (n, e, m) = (id(&q, "n"), id(&q, "e"), id(&q, "m"));
    let remove = remove_of(&q, 1);
    assert_eq!(&query[remove.keyword.start..remove.keyword.end], "REMOVE");
    let items: Vec<String> = remove
        .items
        .iter()
        .map(|i| match &i.kind {
            BoundRemoveKind::Property(key) => format!("#{}.{}", i.target.binding.0, key.name),
            BoundRemoveKind::Labels(labels) => {
                format!("#{}:{}", i.target.binding.0, label_texts(labels).join(":"))
            }
        })
        .collect();
    assert_eq!(
        items,
        [
            format!("#{}.age", n.0),
            format!("#{}.w", e.0),
            format!("#{}:L1:L3", m.0),
            format!("#{}:A", n.0)
        ]
    );
    let mut reads = vec![n, e, m];
    reads.sort_unstable();
    assert_eq!(remove.reads, reads);
    assert!(reads.iter().all(|id| q.binding(*id).unwrap().read));
}

#[test]
fn set_and_remove_materialize_substitutions_first() {
    // Design W10: `old` stands for `n.age`; read after the SET, it is the value from before.
    let q = ok("MATCH (n) WITH n, n.age AS old SET n.age = 1 RETURN old");
    assert_eq!(segment_shapes(&q), ["match[0]", "with2", "set1"]);
    let old = id(&q, "old");
    assert_eq!(q.columns[0].bindings, [old]);
    let n_column = set_of(&q, 2).items[0].target.binding;
    assert_ne!(n_column, id(&q, "n"), "n continues as the WITH column");
    assert_eq!(set_of(&q, 2).items[0].target.kind, TargetKind::Node);
    let q = ok("MATCH (n) LET a = n.age REMOVE n.age RETURN a");
    assert_eq!(segment_shapes(&q), ["match[0]", "with2", "remove1"]);
    // The TCK List12 shape: a comprehension over the nodes, read after the SET.
    let q = ok(
        "MATCH (n) WITH collect(n) AS nodes WITH nodes, [x IN nodes | x.name] AS oldNames \
                UNWIND nodes AS m SET m.name = 'x' RETURN m.name AS name, oldNames",
    );
    let shapes = segment_shapes(&q);
    let set = shapes.iter().position(|s| s == "set1").unwrap();
    assert!(
        shapes[..set]
            .iter()
            .filter(|s| s.starts_with("with"))
            .count()
            >= 2,
        "{shapes:?}"
    );
    // A constant stays a substitution.
    let q = ok("MATCH (n) LET a = 1 SET n.k = a RETURN a");
    assert_eq!(segment_shapes(&q), ["match[0]", "set1"]);
    // A read-only query binds as before.
    let q = ok("MATCH (n) WITH n, n.age AS old RETURN old");
    assert_eq!(segment_shapes(&q), ["match[0]"]);
}

#[test]
fn set_and_remove_inside_call_bodies() {
    let q = ok("MATCH (p) CALL (p) { SET p.k = 1 } RETURN p");
    assert_eq!(segment_shapes(&q), ["match[0]", "set1", "call0"]);
    let Some(Segment::Call(call)) = q.segments.get(2) else {
        panic!("{:?}", q.segments);
    };
    assert!(call.unit(), "a body ending in SET is a unit subquery");
    assert_eq!(call.imports, [id(&q, "p")]);
    assert_eq!(set_of(&q, 1).items[0].target.binding, id(&q, "p"));
    let q = ok("MATCH (p) OPTIONAL CALL { WITH p REMOVE p:A } RETURN p");
    assert!(q.writes());
    // A CALL whose body sets is a write: values are materialized before it.
    let q = ok("MATCH (n) LET a = n.age CALL (n) { SET n.age = 1 } RETURN a");
    assert_eq!(segment_shapes(&q), ["match[0]", "with2", "set1", "call0"]);
    // A body that sets and returns.
    let q = ok("MATCH (p) CALL (p) { SET p.k = 1 RETURN p.k AS k } RETURN p, k");
    assert_eq!(column_names(&q), ["p", "k"]);
}

#[test]
fn set_and_remove_errors() {
    // Unbound targets: 42002 with the message of any unknown name.
    for (query, text) in [
        ("MATCH (p) SET q.k = 1", "q"),
        ("MATCH (p) REMOVE x:A", "x"),
        ("MATCH (p) WITH p.age AS a SET p.k = 1", "p"),
        ("SET n.k = 1", "n"),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
    }
    let f = fail("MATCH (person) SET persn.k = 1");
    assert_eq!(f.suggestion.as_deref(), Some("person"));
    // An unbound name in a value.
    let f = fail("MATCH (p) SET p.k = q.k");
    assert_eq!(f.text, "q");
    // Label items on an edge the binder knows (design W26, P0 C1).
    for (query, text) in [
        ("MATCH ()-[e:knows]->() SET e:x", ":x"),
        ("MATCH ()-[e]->() REMOVE e IS knows", "IS knows"),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.starts_with("`e` is an edge"), "{}", f.message);
        assert!(
            f.alternative.unwrap_or_default().contains(".set_label("),
            "{query}"
        );
    }
    // Targets that are no element.
    for (query, text, message) in [
        (
            "LET m = 1 SET m.k = 2",
            "m",
            "is a value defined by LET or WITH",
        ),
        (
            "MATCH (a) ((x)-->(y)){1,2} (b) SET x.k = 1",
            "x",
            "is a group variable",
        ),
        ("MATCH p = (a)-->(b) SET p.k = 1", "p", "is a path variable"),
        (
            "FOR x IN [1] WITH ORDINALITY i SET i.k = 1",
            "i",
            "is the position of a FOR item",
        ),
        (
            "MATCH ()-[e]->{1,2}() REMOVE e.k",
            "e",
            "is a group variable",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains(message), "{query}: {}", f.message);
    }
    // Aggregates and duplicate keys in a value (design W8).
    let f = fail("MATCH (p)-->(f) SET p.n = count(f)");
    assert_eq!(
        f.message,
        "the aggregate function count() cannot be used in SET"
    );
    assert!(
        f.alternative
            .unwrap_or_default()
            .contains("AS n SET p.value = n"),
        "the alternative names WITH"
    );
    let f = fail("MATCH (p) SET p += {a: 1, a: 2}");
    assert_eq!(f.message, "the key `a` appears twice in this map");
    // A read error before the SET comes first.
    let f = fail("MATCH (n) WHERE x > 1 SET n.k = 1");
    assert_eq!(f.text, "x");
}

// ----- stage 3d.T1: DELETE -----

fn delete_of(q: &BoundQuery, segment: usize) -> &BoundDelete {
    match q.segments.get(segment) {
        Some(Segment::Write(write)) => match &**write {
            BoundWrite::Delete(delete) => delete,
            other => panic!("segment {segment} is no DELETE: {other:?}"),
        },
        other => panic!("segment {segment} is no write: {other:?}"),
    }
}

/// The kind of the last segment's only DELETE target.
fn delete_kind_of(query: &str) -> DeleteKind {
    let q = ok(query);
    let delete = delete_of(&q, q.segments.len() - 1);
    assert_eq!(delete.targets.len(), 1, "{query}");
    delete.targets[0].kind.clone()
}

#[test]
fn delete_binds_targets_kinds_and_reads() {
    let query = "MATCH p = (n:person)-[e:knows|likes]->(m) DETACH DELETE e, n, p, [m], NULL, \
                 $x, {a: n}.a";
    let q = ok(query);
    assert_eq!(segment_shapes(&q), ["match[0]", "delete7"]);
    assert!(q.writes());
    assert_eq!(q.end, QueryEnd::Write);
    let (n, e, m, p) = (id(&q, "n"), id(&q, "e"), id(&q, "m"), id(&q, "p"));
    let delete = delete_of(&q, 1);
    assert!(delete.detach);
    assert_eq!(
        &query[delete.keyword.start..delete.keyword.end],
        "DETACH DELETE"
    );
    assert!(query[delete.span.start..delete.span.end].ends_with("{a: n}.a"));
    let kinds: Vec<&DeleteKind> = delete.targets.iter().map(|t| &t.kind).collect();
    match kinds[0] {
        DeleteKind::Edge {
            labels: Some(labels),
        } => assert_eq!(label_texts(labels), ["knows", "likes"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        kinds[1..],
        [
            &DeleteKind::Node,
            &DeleteKind::Path,
            &DeleteKind::List,
            &DeleteKind::Null,
            &DeleteKind::Unknown,
            &DeleteKind::Unknown
        ]
    );
    let spans: Vec<&str> = delete
        .targets
        .iter()
        .map(|t| &query[t.span.start..t.span.end])
        .collect();
    assert_eq!(spans, ["e", "n", "p", "[m]", "NULL", "$x", "{a: n}.a"]);
    assert_eq!(delete.targets[0].value.bindings, [e]);
    assert_eq!(delete.targets[3].value.bindings, [m]);
    assert!(delete.targets[4].value.bindings.is_empty());
    let mut reads = vec![n, e, m, p];
    reads.sort_unstable();
    assert_eq!(delete.reads, reads);
    // The default and the spelled-out NODETACH.
    for query in ["MATCH (n) DELETE n", "MATCH (n) NODETACH DELETE n"] {
        let q = ok(query);
        let delete = delete_of(&q, 1);
        assert!(!delete.detach, "{query}");
        assert_eq!(delete.targets[0].kind, DeleteKind::Node, "{query}");
    }
    // An edge without a label name: any label.
    assert_eq!(
        delete_kind_of("MATCH ()-[r]->() DELETE r"),
        DeleteKind::Edge { labels: None }
    );
    // Lists: a group variable, comprehensions, COLLECT { }.
    for query in [
        "MATCH (a) ((x)-->(y)){1,2} (b) DELETE x",
        "MATCH (n) DELETE [x IN [n] | x]",
        "MATCH (n) DELETE [(n)-->(m) | m]",
        "MATCH (n) DELETE COLLECT { MATCH (n)-->(m) RETURN m }",
    ] {
        assert_eq!(delete_kind_of(query), DeleteKind::List, "{query}");
    }
    // Kinds only the run knows: a FOR item, a WITH value column, a LET value name, a
    // function result, a CASE, a list concatenation, arithmetic with NULL.
    for query in [
        "FOR x IN [1, 2] DELETE x",
        "MATCH (n) WITH head(collect(n)) AS x DELETE x",
        "MATCH (n) LET l = [n] DELETE l",
        "LET c = 1 DELETE c",
        "MATCH (n) DELETE head([n])",
        "MATCH (n) DELETE CASE WHEN true THEN n END",
        "MATCH (n) DELETE [n] + [n]",
        "DELETE 1 + NULL",
        "MATCH (n) CALL (n) { RETURN head([n]) AS x } DELETE x",
    ] {
        assert_eq!(delete_kind_of(query), DeleteKind::Unknown, "{query}");
    }
    // A column holding a node variable is a node.
    assert_eq!(
        delete_kind_of("MATCH (n) CALL (n) { RETURN n AS x } DELETE x"),
        DeleteKind::Node
    );
    assert_eq!(
        delete_kind_of("MATCH (n) WITH n AS x DELETE x"),
        DeleteKind::Node
    );
}

#[test]
fn delete_materializes_substitutions_first() {
    // Design W10 (TCK Delete6 [5]): `num` stands for `n.num`; read after the DELETE, it is the
    // value from before.
    let q = ok("MATCH (n) WITH n, n.num AS num DELETE n RETURN num");
    assert_eq!(segment_shapes(&q), ["match[0]", "with2", "delete1"]);
    let num = id(&q, "num");
    assert_eq!(q.columns[0].bindings, [num]);
    let delete = delete_of(&q, 2);
    assert_ne!(delete.targets[0].value.bindings, [id(&q, "n")]);
    assert_eq!(delete.targets[0].kind, DeleteKind::Node);
    let q = ok("MATCH (n) LET a = n.age DETACH DELETE n RETURN sum(a)");
    assert_eq!(segment_shapes(&q), ["match[0]", "with2", "delete1"]);
    // A constant stays a substitution.
    let q = ok("MATCH (n) LET a = 1 DELETE n RETURN a");
    assert_eq!(segment_shapes(&q), ["match[0]", "delete1"]);
}

#[test]
fn delete_inside_call_bodies() {
    let q = ok("MATCH (p) CALL (p) { DETACH DELETE p } RETURN count(*)");
    assert_eq!(segment_shapes(&q), ["match[0]", "delete1", "call0"]);
    let Some(Segment::Call(call)) = q.segments.get(2) else {
        panic!("{:?}", q.segments);
    };
    assert!(call.unit(), "a body ending in DELETE is a unit subquery");
    assert_eq!(delete_of(&q, 1).targets[0].value.bindings, [id(&q, "p")]);
    let q = ok("MATCH (p) OPTIONAL CALL { WITH p DELETE p } RETURN 1");
    assert!(q.writes());
    // A CALL whose body deletes is a write: values are materialized before it.
    let q = ok("MATCH (n) LET a = n.age CALL (n) { DETACH DELETE n } RETURN a");
    assert_eq!(
        segment_shapes(&q),
        ["match[0]", "with2", "delete1", "call0"]
    );
    // After NEXT.
    let q = ok("MATCH (n) RETURN n NEXT DETACH DELETE n");
    assert!(q.writes());
}

#[test]
fn delete_errors() {
    // Targets the binder knows hold no element (design E2, TCK Delete5 [9]): 42002 at the
    // target.
    for (query, text, what) in [
        ("MATCH (n) DELETE 1 + 1", "1 + 1", "a computed value"),
        ("MATCH (n) DELETE n, 1", "1", "a number"),
        ("MATCH (n) DETACH DELETE 'n'", "'n'", "a text"),
        ("DELETE true", "true", "a boolean"),
        ("MATCH (n) DELETE n.k = 1", "n.k = 1", "a computed value"),
        ("MATCH (n) DELETE NOT n", "NOT n", "a computed value"),
        (
            "MATCH (n) DELETE n IS NULL",
            "n IS NULL",
            "a computed value",
        ),
        (
            "MATCH (n) DELETE 'a' || 'b'",
            "'a' || 'b'",
            "a computed value",
        ),
        ("MATCH (n) DELETE {a: n}", "{a: n}", "a map"),
        ("MATCH (n) DELETE n {.name}", "n {.name}", "a map"),
        (
            "MATCH (n) DELETE EXISTS { (n)-->() }",
            "EXISTS { (n)-->() }",
            "a boolean",
        ),
        (
            "MATCH (n) DELETE COUNT { (n)-->() }",
            "COUNT { (n)-->() }",
            "a number",
        ),
        (
            "FOR x IN [1] WITH ORDINALITY i DELETE i",
            "i",
            "the position of a FOR item (a number)",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
        let word = if query.contains("DETACH") {
            "DETACH DELETE"
        } else {
            "DELETE"
        };
        assert_eq!(
            f.message,
            format!(
                "`{text}` is {what}, not a node, an edge, a path or a list of them; {word} \
                 removes elements"
            ),
            "{query}"
        );
        assert!(
            f.alternative.unwrap_or_default().contains("g.gql("),
            "{query}"
        );
    }
    // Unbound names (TCK Delete5 [8]): 42002 with the message of any unknown name.
    for (query, text) in [
        ("MATCH (n) DELETE x", "x"),
        ("MATCH (n) DELETE n, [m]", "m"),
        ("DELETE n", "n"),
        ("MATCH (p) WITH p.age AS a DELETE p", "p"),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}");
    }
    // An aggregate in a target (design W8).
    let f = fail("MATCH (n) DELETE head(collect(n))");
    assert_eq!(f.text, "collect");
    assert!(f.message.contains("in DELETE"), "{}", f.message);
}

// ----- stage 3e.T2: MERGE -----

fn merge_of(q: &BoundQuery, segment: usize) -> &BoundMerge {
    match q.segments.get(segment) {
        Some(Segment::Write(write)) => match &**write {
            BoundWrite::Merge(merge) => merge,
            other => panic!("segment {segment} is no MERGE: {other:?}"),
        },
        other => panic!("segment {segment} is no write: {other:?}"),
    }
}

/// The last segment of `q` as a MERGE.
fn last_merge(q: &BoundQuery) -> &BoundMerge {
    merge_of(q, q.segments.len() - 1)
}

/// The column names of a MERGE's match child.
fn merge_columns(merge: &BoundMerge) -> Vec<&str> {
    merge
        .body
        .last()
        .first
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect()
}

/// The help of the binder error of `query`.
fn bind_help(query: &str) -> String {
    let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
    match bind(&parsed) {
        Ok(bound) => panic!("{query:?} bound: {bound:?}"),
        Err(e) => crate::error::DiagnosticError::help(&e).unwrap_or_default(),
    }
}

#[test]
fn merge_binds_the_template_the_match_child_and_the_on_items() {
    let query = "MATCH (a:person) MERGE (a)-[r:knows]->(b:person {name: a.name}) \
                 ON CREATE SET b.new = true ON MATCH SET r.seen = 1, b += {n: a.age} RETURN b, r";
    let q = ok(query);
    // The MATCH, the match child's MATCH, the MERGE (W9: a cut).
    assert_eq!(segment_shapes(&q), ["match[0]", "match[1]", "merge2"]);
    assert!(q.writes());
    assert_eq!(q.end, QueryEnd::Return);
    let (a, r, b) = (id(&q, "a"), id(&q, "r"), id(&q, "b"));
    let merge = merge_of(&q, 2);
    assert_eq!(query[merge.keyword.start..merge.keyword.end], *"MERGE");
    assert!(query[merge.span.start..merge.span.end].ends_with("b += {n: a.age}"));
    // The record: the new elements in text order; each a column of the MERGE segment.
    assert_eq!(merge.outputs, [r, b]);
    assert_eq!(merge_columns(merge), ["r", "b"]);
    for (column, output) in merge.outputs.iter().enumerate() {
        let binding = &q.bindings[output.0];
        assert_eq!(binding.segment, 2);
        assert_eq!(binding.column, Some(ColumnRef { segment: 2, column }));
    }
    assert_eq!(q.bindings[r.0].kind, BindingKind::Edge);
    assert_eq!(q.bindings[b.0].kind, BindingKind::Node);
    assert_eq!(merge.elements, [a, r, b]);
    assert_eq!(merge.path, None);
    // The bound node is imported by the match child and read by the clause.
    assert_eq!(merge.imports, [a]);
    assert_eq!(merge.reads, [a]);
    // The create template in INSERT terms.
    let create = &merge.create;
    assert_eq!(create.nodes.len(), 1);
    assert_eq!(create.nodes[0].binding, b);
    assert_eq!(label_texts(&create.nodes[0].labels), ["person"]);
    assert_eq!(create.nodes[0].properties[0].key.name, "name");
    assert_eq!(create.nodes[0].properties[0].bindings, [a]);
    assert_eq!(create.edges.len(), 1);
    let edge = &create.edges[0];
    assert_eq!((edge.binding, edge.source, edge.target), (r, a, b));
    assert_eq!(edge.label.name, "knows");
    assert_eq!(edge.direction, EdgeDirection::Right);
    assert_eq!(create.reads, [a]);
    assert!(!create.cypher);
    // The ON items in written order, over the pattern's variables and the earlier ones.
    assert_eq!(merge.on_create.len(), 1);
    assert_eq!(merge.on_create[0].target.binding, b);
    assert_eq!(merge.on_match.len(), 2);
    assert_eq!(merge.on_match[0].target.binding, r);
    assert!(matches!(
        merge.on_match[0].target.kind,
        TargetKind::Edge { .. }
    ));
    match &merge.on_match[1].kind {
        BoundSetKind::Merge(value) => assert_eq!(value.bindings, [a]),
        other => panic!("{other:?}"),
    }
    // The new variables are in scope after the clause.
    assert_eq!(q.columns.len(), 2);
}

#[test]
fn merge_names_anonymous_elements_and_binds_the_path() {
    // P0 E3: `p =` is a new binding, the last output; anonymous elements are `#m<k>` in the
    // match child. W33: an undirected edge is created from left to right.
    let q = ok("MERGE p = (:A {k: 1})-[:T]-(x) RETURN p, x");
    let merge = last_merge(&q);
    let (p, x) = (id(&q, "p"), id(&q, "x"));
    assert_eq!(merge.outputs.len(), 4);
    assert_eq!(merge.outputs[2..], [x, p]);
    assert_eq!(merge.path, Some(p));
    assert_eq!(q.bindings[p.0].kind, BindingKind::Path);
    assert_eq!(merge_columns(merge), ["#m0", "#m1", "x", "p"]);
    assert_eq!(merge.elements, merge.outputs[..3]);
    let edge = &merge.create.edges[0];
    assert_eq!(edge.direction, EdgeDirection::Any);
    assert_eq!((edge.source, edge.target), (merge.outputs[0], x));
    assert!(merge.imports.is_empty());
    // A name in scope is never reused for an anonymous element.
    let q = ok("WITH 1 AS `#m0` MERGE (:A)-[:T]->(:B) RETURN `#m0`");
    assert_eq!(merge_columns(last_merge(&q)), ["##m0", "#m1", "#m2"]);
    // Left direction: the edge enters the bound node.
    let q = ok("MATCH (a) MERGE (a)<-[e:T]-(b)");
    let merge = last_merge(&q);
    let edge = &merge.create.edges[0];
    assert_eq!((edge.source, edge.target), (id(&q, "b"), id(&q, "a")));
    assert_eq!(edge.direction, EdgeDirection::Left);
    assert_eq!(q.end, QueryEnd::Write);
    // A new node written twice is one output; the edge a self-loop.
    let q = ok("MERGE (a:A)-[:T]->(a)");
    let merge = last_merge(&q);
    let a = id(&q, "a");
    assert_eq!(merge.outputs.len(), 2);
    assert_eq!(merge.outputs[0], a);
    assert_eq!(merge.elements, [a, merge.outputs[1], a]);
    let edge = &merge.create.edges[0];
    assert_eq!((edge.source, edge.target), (a, a));
    // Both ends bound before (TCK Merge5 [1]-[21]): only the edge is new.
    let q = ok("MATCH (a:person), (b:software) MERGE (a)-[r:made]->(b) RETURN r");
    let merge = last_merge(&q);
    assert_eq!(merge.outputs, [id(&q, "r")]);
    assert_eq!(merge.imports, [id(&q, "a"), id(&q, "b")]);
    assert!(merge.create.nodes.is_empty());
}

#[test]
fn merge_scope_new_variables_and_earlier_values() {
    // After `WITH a AS x`, `a` is a new name (TCK Merge5 [18], [19]).
    // (`x` substitutes the first `a`, the binding the match child imports.)
    let q = ok("MATCH (a) WITH a AS x MERGE (a)-[:T]->(x) RETURN a, x");
    let merge = last_merge(&q);
    let first_a = id(&q, "a");
    assert_eq!(merge.imports, [first_a]);
    assert_eq!(merge.outputs.len(), 2);
    assert_ne!(merge.outputs[1], first_a);
    assert_eq!(merge.elements[2], first_a);
    // The values read FOR items and parameters; the ON items read them too.
    let q = ok("FOR v IN [1, 1] MERGE (n:N {v: v, p: $p}) ON CREATE SET n.w = v RETURN n");
    let merge = last_merge(&q);
    let v = id(&q, "v");
    assert_eq!(merge.imports, [v]);
    assert_eq!(merge.reads, [v]);
    assert_eq!(merge.create.reads, [v]);
    assert_eq!(q.parameters.len(), 2);
    // A FOR item may be a bound end (design E3 of stage 2a).
    let q = ok("MATCH (s:software) WITH collect_list(s) AS l FOR x IN l MERGE (x)-[:T]->(:Y)");
    assert_eq!(last_merge(&q).imports.len(), 1);
    // Several MERGEs: the second reads the first's records (TCK Merge5 [9], Merge9 [2]).
    let q = ok("MERGE (a:A) MERGE (b:B) MERGE (a)-[r:T]->(b) RETURN r");
    let merge = last_merge(&q);
    assert_eq!(merge.imports, [id(&q, "a"), id(&q, "b")]);
    // A MERGE in a CALL body imports what the body imports.
    let q = ok("MATCH (n) CALL (n) { MERGE (n)-[:T]->(m:X) RETURN m } RETURN m");
    let merge = q
        .segments
        .iter()
        .find_map(|s| match s {
            Segment::Write(write) => match &**write {
                BoundWrite::Merge(merge) => Some(merge),
                _ => None,
            },
            _ => None,
        })
        .expect("a MERGE");
    assert_eq!(merge.imports, [id(&q, "n")]);
}

#[test]
fn merge_materializes_values_that_read_the_graph() {
    // Design W10, TCK Merge1 [9]: `x` is materialized before the MERGE.
    let q = ok("MATCH (f) WITH f.x AS x MERGE (:N {x: x})");
    let shapes = segment_shapes(&q);
    assert_eq!(shapes.len(), 4, "{shapes:?}");
    assert!(shapes[1].starts_with("with"), "{shapes:?}");
    assert!(shapes[3].starts_with("merge"), "{shapes:?}");
    // A constant stays a substitution: nothing materialized.
    let q = ok("WITH 42 AS var MERGE (c:N {var: var}) RETURN c");
    assert_eq!(segment_shapes(&q), ["match[0]", "merge1"]);
}

#[test]
fn merge_binding_errors() {
    for (query, text, message) in [
        // P0 E2 (TCK Merge1 [15]).
        (
            "MATCH (a) MERGE (a)",
            "(a)",
            "MERGE (a) with `a` bound earlier has nothing to find or create",
        ),
        (
            "MATCH (a) MERGE p = (a)",
            "(a)",
            "MERGE p = (a) with `a` bound earlier has nothing to find or create",
        ),
        // W36 (TCK Merge5 [22], [26]).
        (
            "MATCH (a) MERGE (a:X)",
            "a",
            "the variable `a` is already bound; MERGE refers to a bound node only as (a)",
        ),
        (
            "MATCH (a) MERGE (a {k: 1})-[:T]->(b)",
            "a",
            "the variable `a` is already bound; MERGE refers to a bound node only as (a)",
        ),
        (
            "MATCH (a)-[r]->(b) MERGE (a)-[r:T]->(b)",
            "r",
            "the variable `r` is already bound; MERGE finds or creates the edge itself",
        ),
        (
            "MERGE (a:A)-[:T]->(a:B)",
            "a",
            "the node `a` is created earlier in this MERGE",
        ),
        (
            "MATCH p = (a)-->(b) MERGE p = (a)-[:T]->(b)",
            "p",
            "the path variable `p` is already bound",
        ),
        ("MATCH (a)-[r]->(b) MERGE (r)-[:T]->(b)", "r", "`r`"),
        ("WITH 1 AS x MERGE (x)-[:T]->(b)", "x", "`x`"),
        // ON items: an unknown name (TCK Merge2 [6], Merge3 [5]), an aggregate, edge labels.
        (
            "MERGE (a) ON CREATE SET x.k = 1",
            "x",
            "the variable `x` is not defined",
        ),
        (
            "MERGE (a) ON MATCH SET a.k = count(*)",
            "count",
            "the aggregate function count() cannot be used in SET",
        ),
        (
            "MERGE (a)-[r:T]->(b) ON CREATE SET r:X",
            ":X",
            "`r` is an edge",
        ),
        // Property values: no aggregate (W8).
        (
            "MERGE (a {k: count(*)})",
            "count",
            "the aggregate function count() cannot be used in MERGE",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.gqlstatus, "42002", "{query}");
        assert_eq!(f.text, text, "{query}: {f:?}");
        assert!(f.message.contains(message), "{query}: {}", f.message);
    }
    // The MERGE advice on the pattern errors.
    for query in [
        "MATCH (a) MERGE (a)",
        "MATCH (a) MERGE (a:X)",
        "MATCH (a)-[r]->(b) MERGE (a)-[r:T]->(b)",
        "MERGE (a) ON CREATE SET x.k = 1",
    ] {
        let help = bind_help(query);
        assert!(
            help.contains("MERGE looks for its whole pattern"),
            "{query}: {help}"
        );
    }
    // Not yet: a value reading an element the same MERGE creates, a subquery in a value.
    for (query, text) in [
        ("MERGE (a {x: 1})-[:T]->(b {y: a.x})", "a.x"),
        (
            "MERGE (a {k: COUNT { MATCH (b) RETURN b }})",
            "COUNT { MATCH (b) RETURN b }",
        ),
    ] {
        let f = failure_of(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
    }
    // A NULL property is no bind-time error (P0 E1: 22004 at run time, TCK Merge1 [17]).
    ok("MERGE (a {num: null})");
    ok("FOR x IN [NULL] MERGE (a)-[:T {k: x}]->(b)");
}

#[test]
fn a_quantified_edge_over_a_bound_list_of_edges_is_not_yet() {
    // TCK Match4 [8], Match9 [6] / [7]: Cypher walks exactly the edges of the list.
    for (query, text) in [
        (
            "MATCH ()-[r1]->()-[r2]->() WITH [r1, r2] AS rs LIMIT 1 \
             MATCH (first)-[rs*]->(second) RETURN first, second",
            "rs",
        ),
        (
            "MATCH ()-[r1]->()-[r2]->() WITH [r1, r2] AS rs \
             MATCH (a)-[rs*1..2]->(b) RETURN a",
            "rs",
        ),
        ("LET rs = [] MATCH (a)-[rs*]->(b) RETURN a", "rs"),
    ] {
        let parsed = parse(query).unwrap();
        let f = failure(query, bind(&parsed));
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "0A000", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message
                .contains("a quantified edge over a list of edges bound before"),
            "{}",
            f.message
        );
        assert!(
            f.alternative
                .as_deref()
                .is_some_and(|a| a.contains("WHERE rs2 = rs")),
            "{f:?}"
        );
    }
}
