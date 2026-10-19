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
    assert_eq!((q.offset, q.limit), (None, None));
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
