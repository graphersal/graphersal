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

// ----- stage 2a: parsed, not bound yet -----

#[test]
fn for_and_queries_without_match_are_not_yet_bound() {
    for (query, text, what) in [
        ("RETURN 1", "RETURN", "a query without MATCH"),
        ("RETURN $a AS a", "RETURN", "a query without MATCH"),
        ("LET x = 1 RETURN x", "LET", "a query that starts with LET"),
        (
            "with 1 AS x RETURN x",
            "with",
            "a query that starts with WITH",
        ),
        ("FOR x IN [1] RETURN x", "FOR", "FOR"),
        ("UNWIND [1] AS x RETURN x", "UNWIND", "UNWIND"),
        ("MATCH (a) FOR x IN [1] RETURN a", "FOR", "FOR"),
        (
            "MATCH (a) WITH a UNWIND [1] AS x RETURN a",
            "UNWIND",
            "UNWIND",
        ),
        (
            "FOR x IN [1] WITH ORDINALITY i MATCH (a) RETURN a",
            "FOR",
            "FOR",
        ),
    ] {
        let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
        let f = failure(query, bind(&parsed));
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "0A000", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.message,
            format!("{what} is not yet supported in this version"),
            "{query}"
        );
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains("inject(") || alternative.contains("unfold()"),
            "{query}: {alternative}"
        );
    }
}
