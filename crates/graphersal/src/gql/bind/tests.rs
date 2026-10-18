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
