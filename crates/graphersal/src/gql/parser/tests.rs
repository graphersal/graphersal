use super::*;
use crate::error::DiagnosticError;

// ----- helpers -----

fn ok(query: &str) -> Query {
    match parse(query) {
        Ok(parsed) => parsed,
        Err(e) => panic!("{query:?} failed: {e} ({e:?})"),
    }
}

/// The parts of a GQL error a test looks at.
#[derive(Debug)]
struct Failure {
    kind: GqlErrorKind,
    message: String,
    /// The underlined text.
    text: String,
    line: usize,
    column: usize,
    gqlstatus: String,
    expected: Vec<String>,
    suggestion: Option<String>,
    alternative: Option<String>,
}

fn fail(query: &str) -> Failure {
    match parse(query) {
        Ok(parsed) => panic!("{query:?} parsed: {parsed:?}"),
        Err(TraverserError::Gql {
            kind,
            message,
            offset,
            line,
            column,
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
            line,
            column,
            gqlstatus,
            expected: context.expected.clone(),
            suggestion: context.suggestion.clone(),
            alternative: context.alternative.clone(),
        },
        Err(other) => panic!("{query:?}: not a GQL error: {other:?}"),
    }
}

fn label(e: &LabelExpr) -> String {
    match &e.kind {
        LabelExprKind::Label(name) => name.name.clone(),
        LabelExprKind::Wildcard => "%".into(),
        LabelExprKind::Not(inner) => format!("(! {})", label(inner)),
        LabelExprKind::And(a, b) => format!("(& {} {})", label(a), label(b)),
        LabelExprKind::Or(a, b) => format!("(| {} {})", label(a), label(b)),
    }
}

/// An S-expression of `e`, to check the tree shape (precedence).
fn sexp(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Literal(Literal::Null) => "null".into(),
        ExprKind::Literal(Literal::Bool(b)) => b.to_string(),
        ExprKind::Literal(Literal::Integer(i)) => i.to_string(),
        ExprKind::Literal(Literal::Float(f)) => format!("{f:?}"),
        ExprKind::Literal(Literal::String(s)) => format!("'{s}'"),
        ExprKind::List(items) => {
            format!("[{}]", items.iter().map(sexp).collect::<Vec<_>>().join(" "))
        }
        ExprKind::Map(map) => format!(
            "{{{}}}",
            map.entries
                .iter()
                .map(|entry| format!("{}: {}", entry.key.name, sexp(&entry.value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ExprKind::Parameter(name) => format!("${name}"),
        ExprKind::Variable(name) => name.name.clone(),
        ExprKind::Property { target, key } => format!("{}.{}", sexp(target), key.name),
        ExprKind::Unary { op, operand } => {
            let op = match op {
                UnaryOp::Not => "not",
                UnaryOp::Minus => "-",
                UnaryOp::Plus => "+",
            };
            format!("({op} {})", sexp(operand))
        }
        ExprKind::Binary { op, left, right } => {
            let op = match op {
                BinaryOp::Or => "or",
                BinaryOp::Xor => "xor",
                BinaryOp::And => "and",
                BinaryOp::Eq => "=",
                BinaryOp::Ne => "<>",
                BinaryOp::Lt => "<",
                BinaryOp::Le => "<=",
                BinaryOp::Gt => ">",
                BinaryOp::Ge => ">=",
                BinaryOp::Add => "+",
                BinaryOp::Sub => "-",
                BinaryOp::Mul => "*",
                BinaryOp::Div => "/",
                BinaryOp::Mod => "%",
                BinaryOp::Concat => "||",
                BinaryOp::In => "in",
                BinaryOp::StartsWith => "starts",
                BinaryOp::EndsWith => "ends",
                BinaryOp::Contains => "contains",
            };
            format!("({op} {} {})", sexp(left), sexp(right))
        }
        ExprKind::IsNull { operand, negated } => format!(
            "({} {})",
            if *negated { "not-null" } else { "null?" },
            sexp(operand)
        ),
        ExprKind::IsLabeled {
            operand,
            labels,
            negated,
        } => format!(
            "({} {} {})",
            if *negated { "unlabeled" } else { "labeled" },
            sexp(operand),
            label(labels)
        ),
        ExprKind::Case(case) => {
            let mut out = "(case".to_string();
            if let Some(operand) = &case.operand {
                out.push_str(&format!(" {}", sexp(operand)));
            }
            for arm in &case.whens {
                out.push_str(&format!(" (when {} {})", sexp(&arm.when), sexp(&arm.then)));
            }
            if let Some(e) = &case.else_result {
                out.push_str(&format!(" (else {})", sexp(e)));
            }
            out.push(')');
            out
        }
        ExprKind::FunctionCall { name, args, .. } => match args {
            CallArgs::Star(_) => format!("{}(*)", name.name),
            CallArgs::Exprs(args) => format!(
                "{}({})",
                name.name,
                args.iter().map(sexp).collect::<Vec<_>>().join(" ")
            ),
        },
    }
}

/// The tree of the first RETURN item of `MATCH (n) RETURN <src>`.
fn expr(src: &str) -> String {
    let query = ok(&format!("MATCH (n) RETURN {src}"));
    sexp(&query.ret.items[0].expr)
}

// ----- positive cases -----

#[test]
fn full_query_structure() {
    let q = ok(
        "MATCH (a:person {name: 'marko'})-[e:knows]->(b) WHERE b.age > 30 \
         RETURN DISTINCT a, b.name AS name ORDER BY name DESC, a.age OFFSET 1 LIMIT $n;",
    );
    assert_eq!(q.matches.len(), 1);
    let m = &q.matches[0];
    assert_eq!(
        q.text(m.span),
        "MATCH (a:person {name: 'marko'})-[e:knows]->(b) WHERE b.age > 30"
    );
    assert_eq!(m.patterns.len(), 1);
    let path = &m.patterns[0];
    assert_eq!(
        q.text(path.span),
        "(a:person {name: 'marko'})-[e:knows]->(b)"
    );
    assert_eq!(
        path.start.variable.as_ref().map(|v| v.name.as_str()),
        Some("a")
    );
    assert_eq!(path.start.labels.as_ref().map(label), Some("person".into()));
    let props = path.start.properties.as_ref().unwrap();
    assert_eq!(props.entries[0].key.name, "name");
    assert_eq!(sexp(&props.entries[0].value), "'marko'");
    assert_eq!(path.steps.len(), 1);
    let edge = &path.steps[0].edge;
    assert_eq!(edge.direction, EdgeDirection::Right);
    assert!(!edge.abbreviated);
    assert_eq!(q.text(edge.span), "-[e:knows]->");
    assert_eq!(edge.labels.as_ref().map(label), Some("knows".into()));
    assert_eq!(sexp(m.where_clause.as_ref().unwrap()), "(> b.age 30)");
    assert!(q.ret.distinct);
    assert!(q.ret.star.is_none());
    assert_eq!(q.ret.items.len(), 2);
    assert_eq!(q.text(q.ret.items[1].expr.span), "b.name");
    assert_eq!(q.ret.items[1].alias.as_ref().unwrap().name, "name");
    assert_eq!(q.text(q.ret.items[1].span), "b.name AS name");
    let order = q.order_by.as_ref().unwrap();
    assert_eq!(order.items.len(), 2);
    assert_eq!(order.items[0].direction, SortDirection::Desc);
    assert_eq!(order.items[1].direction, SortDirection::Asc);
    assert_eq!(q.text(order.span), "ORDER BY name DESC, a.age");
    assert_eq!(sexp(&q.offset.as_ref().unwrap().value), "1");
    assert_eq!(q.text(q.limit.as_ref().unwrap().span), "LIMIT $n");
    assert_eq!(sexp(&q.limit.as_ref().unwrap().value), "$n");
    assert!(q.text(q.span).ends_with("LIMIT $n"));
}

#[test]
fn several_match_clauses_and_comma_patterns() {
    let q = ok("MATCH (a)-->(b), (b)<--(c) MATCH (c)--(d) WHERE d.x = 1 RETURN *");
    assert_eq!(q.matches.len(), 2);
    assert_eq!(q.matches[0].patterns.len(), 2);
    assert!(q.matches[0].where_clause.is_none());
    assert!(q.matches[1].where_clause.is_some());
    assert!(q.ret.star.is_some());
    assert!(q.ret.items.is_empty());
    let q = ok("MATCH (a) RETURN *, a.name AS n");
    assert!(q.ret.star.is_some());
    assert_eq!(q.ret.items.len(), 1);
    let q = ok("MATCH (a) RETURN DISTINCT *");
    assert!(q.ret.distinct && q.ret.star.is_some());
}

#[test]
fn multi_line_comments_quoted_identifiers_and_keyword_case() {
    let q = ok("match (`my node`:`Person Type`)\n\
         // a comment\n\
         where `my node`.`first name` starts with 'm' /* inline */\n\
         -- a GQL comment\n\
         return `my node`.`first name` as `the name` order by `the name` asc skip 0 limit 3");
    let node = &q.matches[0].patterns[0].start;
    assert_eq!(node.variable.as_ref().unwrap().name, "my node");
    assert_eq!(node.labels.as_ref().map(label), Some("Person Type".into()));
    assert_eq!(
        sexp(q.matches[0].where_clause.as_ref().unwrap()),
        "(starts my node.first name 'm')"
    );
    let item = &q.ret.items[0];
    assert_eq!(item.alias.as_ref().unwrap().name, "the name");
    assert_eq!(item.expr.span.line, 5);
    assert!(q.offset.is_some() && q.limit.is_some());
    // Keywords as names where only a name can stand: labels, keys, aliases, map keys.
    let q = ok("MATCH (n:Match {order: 1}) RETURN n.limit AS return, {where: 1} AS m");
    assert_eq!(
        q.matches[0].patterns[0].start.labels.as_ref().map(label),
        Some("Match".into())
    );
    assert_eq!(sexp(&q.ret.items[0].expr), "n.limit");
    assert_eq!(q.ret.items[0].alias.as_ref().unwrap().name, "return");
    assert_eq!(sexp(&q.ret.items[1].expr), "{where: 1}");
    // MiXeD case keywords.
    ok("MaTcH (n) WhErE n.x Is NoT nUlL ReTuRn n");
}

#[test]
fn edge_forms_and_directions() {
    let cases = [
        ("(a)-[e]->(b)", EdgeDirection::Right, false),
        ("(a)<-[e]-(b)", EdgeDirection::Left, false),
        ("(a)-[e]-(b)", EdgeDirection::Any, false),
        ("(a)<-[e]->(b)", EdgeDirection::Any, false),
        ("(a)-->(b)", EdgeDirection::Right, true),
        ("(a)<--(b)", EdgeDirection::Left, true),
        ("(a)--(b)", EdgeDirection::Any, true),
        ("(a) -- (b)", EdgeDirection::Any, true),
        ("(a)<-->(b)", EdgeDirection::Any, true),
        ("(a)<->(b)", EdgeDirection::Any, true),
        ("(a)->(b)", EdgeDirection::Right, true),
        ("(a)<-(b)", EdgeDirection::Left, true),
        ("(a)-(b)", EdgeDirection::Any, true),
        ("(a)- ->(b)", EdgeDirection::Right, true),
    ];
    for (pattern, direction, abbreviated) in cases {
        let q = ok(&format!("MATCH {pattern} RETURN a"));
        let path = &q.matches[0].patterns[0];
        assert_eq!(path.steps.len(), 1, "{pattern}");
        let edge = &path.steps[0].edge;
        assert_eq!(edge.direction, direction, "{pattern}");
        assert_eq!(edge.abbreviated, abbreviated, "{pattern}");
        assert_eq!(path.steps[0].node.variable.as_ref().unwrap().name, "b");
    }
    let q = ok(
        "MATCH (a)-[e:knows|created {weight: 0.5} WHERE e.weight > 0.1]->(b)<-[:x]-(c) RETURN e",
    );
    let path = &q.matches[0].patterns[0];
    assert_eq!(path.steps.len(), 2);
    let edge = &path.steps[0].edge;
    assert_eq!(
        edge.labels.as_ref().map(label),
        Some("(| knows created)".into())
    );
    assert_eq!(
        sexp(edge.where_clause.as_ref().unwrap()),
        "(> e.weight 0.1)"
    );
    assert_eq!(edge.properties.as_ref().unwrap().entries.len(), 1);
    assert!(path.steps[1].edge.variable.is_none());
    assert_eq!(q.text(path.steps[1].edge.span), "<-[:x]-");
    // A node with every part, and an empty node.
    let q = ok("MATCH (v:A {k: 1} WHERE v.k > 0)-[]->() RETURN v");
    let start = &q.matches[0].patterns[0].start;
    assert!(start.labels.is_some() && start.properties.is_some() && start.where_clause.is_some());
    assert!(q.matches[0].patterns[0].steps[0].node.variable.is_none());
}

#[test]
fn label_expressions() {
    let node_labels = |pattern: &str| {
        let q = ok(&format!("MATCH {pattern} RETURN *"));
        label(q.matches[0].patterns[0].start.labels.as_ref().unwrap())
    };
    assert_eq!(node_labels("(v:A&B|C)"), "(| (& A B) C)");
    assert_eq!(node_labels("(v:A|B&C)"), "(| A (& B C))");
    assert_eq!(node_labels("(v:!A&%)"), "(& (! A) %)");
    assert_eq!(node_labels("(v:!(A|B))"), "(! (| A B))");
    assert_eq!(node_labels("(v:!!A)"), "(! (! A))");
    assert_eq!(node_labels("(:A:B:C)"), "(& (& A B) C)");
    assert_eq!(node_labels("(v IS A&B)"), "(& A B)");
    assert_eq!(node_labels("(IS %)"), "%");
    let q = ok("MATCH ()-[e IS knows]->() RETURN e");
    assert_eq!(
        q.matches[0].patterns[0].steps[0]
            .edge
            .labels
            .as_ref()
            .map(label),
        Some("knows".into())
    );
    // `:A:B` is plain names only.
    let f = fail("MATCH (v:A|B:C) RETURN v");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert!(f.alternative.unwrap().contains('&'));
    let f = fail("MATCH (v:A:!B) RETURN v");
    assert_eq!(f.text, "!B");
}

#[test]
fn precedence() {
    let cases = [
        ("a OR b XOR c AND d", "(or a (xor b (and c d)))"),
        ("a AND b OR c", "(or (and a b) c)"),
        ("NOT a AND b", "(and (not a) b)"),
        ("NOT a = b", "(not (= a b))"),
        ("NOT NOT a", "(not (not a))"),
        ("a AND NOT b", "(and a (not b))"),
        ("a = b IS NULL", "(= a (null? b))"),
        ("a IS NOT NULL AND b", "(and (not-null a) b)"),
        ("a + b = c * d", "(= (+ a b) (* c d))"),
        ("a + b * c - d", "(- (+ a (* b c)) d)"),
        ("a - b - c", "(- (- a b) c)"),
        ("a / b % c", "(% (/ a b) c)"),
        ("a || b + c", "(+ (|| a b) c)"),
        ("-a * b", "(* (- a) b)"),
        ("- a.x", "(- a.x)"),
        ("+a", "(+ a)"),
        ("a IN [1, 2] OR b", "(or (in a [1 2]) b)"),
        ("a + 1 IN b", "(in (+ a 1) b)"),
        (
            "a STARTS WITH 'x' AND b ENDS WITH 'y'",
            "(and (starts a 'x') (ends b 'y'))",
        ),
        ("a CONTAINS b || c", "(contains a (|| b c))"),
        ("n.name <> 'x'", "(<> n.name 'x')"),
        ("n.name != 'x'", "(<> n.name 'x')"),
        ("a <= b", "(<= a b)"),
        ("a >= b", "(>= a b)"),
        ("a<-1", "(< a -1)"),
        ("a <- b", "(< a (- b))"),
        ("(a OR b) AND c", "(and (or a b) c)"),
        ("n:A", "(labeled n A)"),
        ("n:A|B AND m:C", "(and (labeled n (| A B)) (labeled m C))"),
        ("n IS LABELED A&B", "(labeled n (& A B))"),
        ("n IS NOT LABELED A", "(unlabeled n A)"),
        ("NOT n:A", "(not (labeled n A))"),
        ("a.b.c", "a.b.c"),
        ("{k: 1}.k", "{k: 1}.k"),
    ];
    for (src, tree) in cases {
        assert_eq!(expr(src), tree, "{src}");
    }
    // Spans of nested nodes cover their text (column names).
    let q = ok("MATCH (n) RETURN (n.a + 1) * 2, -n.b");
    assert_eq!(q.text(q.ret.items[0].expr.span), "(n.a + 1) * 2");
    assert_eq!(q.text(q.ret.items[1].expr.span), "-n.b");
}

#[test]
fn literals_and_parameters() {
    assert_eq!(expr("-9223372036854775808"), i64::MIN.to_string());
    assert_eq!(expr("9223372036854775807"), i64::MAX.to_string());
    assert_eq!(expr("-5 * 2"), "(* -5 2)");
    assert_eq!(expr("- 5"), "-5");
    assert_eq!(expr("-(5)"), "(- 5)");
    assert_eq!(expr("1.5e3"), "1500.0");
    assert_eq!(expr("-.5"), "-0.5");
    assert_eq!(expr("0x1F"), "31");
    assert_eq!(expr("'it''s'"), "'it's'");
    assert_eq!(expr("\"a\\nb\""), "'a\nb'");
    assert_eq!(expr("TRUE"), "true");
    assert_eq!(expr("false"), "false");
    assert_eq!(expr("null"), "null");
    assert_eq!(expr("[]"), "[]");
    assert_eq!(expr("[1, [2, 'x'], $p]"), "[1 [2 'x'] $p]");
    assert_eq!(expr("{}"), "{}");
    assert_eq!(expr("{a: 1, b: {c: [true]}}"), "{a: 1, b: {c: [true]}}");
    assert_eq!(expr("$`my param`"), "$my param");
    assert_eq!(expr("$0"), "$0");
    let q = ok("MATCH (n) RETURN -9223372036854775808 AS m");
    assert_eq!(q.text(q.ret.items[0].expr.span), "-9223372036854775808");
    // 2^63 only with the minus folded in.
    let f = fail("MATCH (n) RETURN 9223372036854775808");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "9223372036854775808");
    assert!(f.message.contains("out of range"), "{}", f.message);
    let f = fail("MATCH (n) RETURN -(9223372036854775808)");
    assert_eq!(f.text, "9223372036854775808");
}

#[test]
fn case_expressions() {
    assert_eq!(
        expr("CASE n.x WHEN 1 THEN 'a' WHEN 2 THEN 'b' ELSE 'c' END"),
        "(case n.x (when 1 'a') (when 2 'b') (else 'c'))"
    );
    assert_eq!(
        expr("CASE WHEN n.x > 1 THEN 'big' END"),
        "(case (when (> n.x 1) 'big'))"
    );
    assert_eq!(
        expr("CASE WHEN a THEN CASE b WHEN 1 THEN 2 END END"),
        "(case (when a (case b (when 1 2))))"
    );
    let f = fail("MATCH (n) RETURN CASE n.x ELSE 1 END");
    assert_eq!(f.expected, ["WHEN"]);
    let f = fail("MATCH (n) RETURN CASE WHEN a THEN 1 RETURN");
    assert_eq!(f.expected, ["WHEN", "ELSE", "END"]);
}

#[test]
fn functions() {
    assert_eq!(expr("element_id(n)"), "element_id(n)");
    assert_eq!(expr("elementId(n)"), "elementId(n)");
    assert_eq!(expr("UPPER(toLower(n.name))"), "UPPER(toLower(n.name))");
    assert_eq!(expr("coalesce(n.a, n.b, 'x')"), "coalesce(n.a n.b 'x')");
    assert_eq!(expr("labels(n)"), "labels(n)");
    assert_eq!(
        expr("size(n.name) + char_length('ab')"),
        "(+ size(n.name) char_length('ab'))"
    );
    assert_eq!(
        lookup_function("TOUPPER").map(|s| s.function),
        Some(Function::Upper)
    );
    assert_eq!(
        lookup_function("collect").map(|s| s.name()),
        Some("collect_list")
    );
    assert!(lookup_function("nope").is_none());

    let f = fail("MATCH (n) RETURN upper(n.a, n.b)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.message, "upper() takes 1 argument, got 2");
    assert_eq!(f.text, "upper(n.a, n.b)");
    let f = fail("MATCH (n) RETURN coalesce()");
    assert_eq!(f.message, "coalesce() takes at least 1 argument, got 0");
    let f = fail("MATCH (n) RETURN uper(n.name)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "uper");
    assert_eq!(f.suggestion.as_deref(), Some("upper()"));
    let f = fail("MATCH (n) RETURN elementid2(n)");
    assert_eq!(f.suggestion.as_deref(), Some("elementId()"));
    let f = fail("MATCH (n) RETURN frobnicate(n)");
    assert_eq!(f.suggestion, None);
    let f = fail("MATCH (n) RETURN upper(DISTINCT n.name)");
    assert!(f.message.contains("DISTINCT"), "{}", f.message);
    let f = fail("MATCH (n) RETURN labels(*)");
    assert!(f.message.contains("count(*)"), "{}", f.message);
}

#[test]
fn keyword_variables_must_be_back_quoted() {
    let f = fail("MATCH (order) RETURN 1");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "order");
    assert!(f.alternative.unwrap().contains("`order`"));
    let f = fail("MATCH (n) RETURN end");
    assert_eq!(f.text, "end");
    assert!(f.alternative.unwrap().contains("`end`"));
    let q = ok("MATCH (`order`) RETURN `order`.x");
    assert_eq!(sexp(&q.ret.items[0].expr), "order.x");
}

#[test]
fn trailing_semicolon_and_text_after_the_query() {
    ok("MATCH (n) RETURN n;");
    let f = fail("MATCH (n) RETURN n; MATCH (m) RETURN m");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "MATCH");
    let f = fail("MATCH (n) RETURN n MATCH (m)");
    assert!(f.message.contains("RETURN ends the query"), "{}", f.message);
}

// ----- result tail order -----

#[test]
fn order_offset_limit_follow_gql_order() {
    let q = ok("MATCH (n) RETURN n SKIP 2");
    assert!(q.offset.is_some() && q.limit.is_none());
    let q = ok("MATCH (n) RETURN n LIMIT 2");
    assert!(q.offset.is_none() && q.limit.is_some());
    ok("MATCH (n) RETURN n ORDER BY n.x ASCENDING, n.y DESCENDING OFFSET 1 LIMIT 1");

    let f = fail("MATCH (n) RETURN n LIMIT 1 OFFSET 2");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "OFFSET");
    assert_eq!(f.message, "OFFSET must come before LIMIT");
    assert!(f.alternative.unwrap().contains("ORDER BY, OFFSET, LIMIT"));
    let f = fail("MATCH (n) RETURN n LIMIT 1 SKIP 2");
    assert_eq!(f.message, "SKIP must come before LIMIT");
    let f = fail("MATCH (n) RETURN n SKIP 1 ORDER BY n.x");
    assert_eq!(
        f.message,
        "ORDER BY must come before OFFSET / SKIP and LIMIT"
    );
    let f = fail("MATCH (n) RETURN n LIMIT 1 LIMIT 2");
    assert_eq!(f.expected, ["end of input"]);
    let f = fail("MATCH (n) RETURN n ORDER n.x");
    assert_eq!(f.expected, ["BY"]);
    let f = fail("MATCH (n) ORDER BY n.x RETURN n");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "ORDER");
}

// ----- errors: missing RETURN, expected tokens, did you mean -----

#[test]
fn missing_return_suggests_return_star_and_the_variables() {
    let query = "MATCH (a:person)-[e]->(b)\nWHERE a.age > 30";
    let f = fail(query);
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.gqlstatus, "42001");
    assert!(f.message.starts_with("missing RETURN"), "{}", f.message);
    assert_eq!((f.line, f.column), (2, 17));
    assert_eq!(f.expected, ["RETURN"]);
    let alternative = f.alternative.unwrap();
    assert!(alternative.contains("RETURN *"), "{alternative}");
    assert!(alternative.contains("RETURN a, e, b"), "{alternative}");
    let f = fail("MATCH ()-->();");
    assert_eq!(f.text, ";");
    assert!(f.alternative.unwrap().contains("RETURN a, b"));
    // The help() of the error leads with the alternative.
    let err = parse("MATCH (a)").unwrap_err();
    let help = err.help().unwrap_or_default();
    assert!(
        help.starts_with("end the query with a RETURN statement"),
        "{help}"
    );
}

#[test]
fn expected_tokens_and_did_you_mean() {
    let f = fail("MATHC (a) RETURN a");
    assert_eq!(f.text, "MATHC");
    assert_eq!(f.expected, ["MATCH"]);
    assert_eq!(f.suggestion.as_deref(), Some("MATCH"));
    let f = fail("MATCH (a) RETRUN a");
    assert_eq!(f.message, "unexpected `RETRUN`");
    assert_eq!(f.expected, ["`,`", "WHERE", "MATCH", "RETURN"]);
    assert_eq!(f.suggestion.as_deref(), Some("RETURN"));
    let f = fail("MATCH (a) WHER a.x = 1 RETURN a");
    assert_eq!(f.suggestion.as_deref(), Some("WHERE"));
    let f = fail("MATCH (a) RETURN a LIMT 1");
    assert!(
        f.expected.contains(&"LIMIT".to_string()),
        "{:?}",
        f.expected
    );
    assert_eq!(f.suggestion.as_deref(), Some("LIMIT"));
    let f = fail("MATCH (a) RETURN a ODER BY a.x");
    assert_eq!(f.suggestion.as_deref(), Some("ORDER BY"));
    let f = fail("MATCH (a) RETURN a.x DESC");
    assert_eq!(f.text, "DESC");
    let f = fail("MATCH (a RETURN a");
    assert_eq!(f.text, "RETURN");
    assert_eq!(f.expected, ["`:`", "IS", "`{`", "WHERE", "`)`"]);
    let f = fail("MATCH (a)-[e:x RETURN a");
    assert_eq!(f.expected, ["`{`", "WHERE", "`]`"]);
    let f = fail("MATCH (a)-[e]>(b) RETURN a");
    assert_eq!(f.expected, ["`->`", "`-`"]);
    let f = fail("MATCH (a) WHERE RETURN a");
    assert_eq!(f.text, "RETURN");
    assert_eq!(f.expected, ["an expression"]);
    assert!(f.alternative.is_none());
    let f = fail("MATCH (a) RETURN");
    assert_eq!(f.message, "unexpected end of input");
    assert_eq!(f.expected, ["`*`", "an expression"]);
    let f = fail("");
    assert_eq!(f.expected, ["MATCH"]);
    let f = fail("MATCH (a) RETURN [1, 2");
    assert_eq!(f.expected, ["`,`", "`]`"]);
    let f = fail("MATCH (a) WHERE a.x IS 5 RETURN a");
    assert_eq!(f.expected, ["NOT", "NULL", "LABELED"]);
    let f = fail("MATCH (a) WHERE a < b < c RETURN a");
    assert_eq!(f.message, "comparisons cannot be chained");
    assert_eq!(f.text, "<");
    let f = fail("MATCH (a) WHERE a = NOT b RETURN a");
    assert_eq!(f.text, "NOT");
    assert!(f.alternative.unwrap().contains("(NOT x)"));
    let f = fail("MATCH (a {k: 1, }) RETURN a");
    assert_eq!(f.expected, ["a property name"]);
    let f = fail("MATCH (a $props) RETURN a");
    assert!(f.alternative.unwrap().contains("{name: $name}"));
}

#[test]
fn lexer_errors_are_gql_syntax_errors() {
    let f = fail("MATCH (n)\nRETURN 'abc");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!((f.line, f.column), (2, 8));
    assert!(f.message.contains("unterminated string"), "{}", f.message);
    let f = fail("MATCH (n) RETURN 007");
    assert!(f.message.contains("leading zero"), "{}", f.message);
    let f = fail("MATCH (n) RETURN n ^ 2");
    assert_eq!(f.text, "^");
}

// ----- refused and not yet supported constructs -----

#[test]
fn writes_are_refused() {
    for (query, text) in [
        ("CREATE (n:person)", "CREATE"),
        ("INSERT (n:person)", "INSERT"),
        ("MATCH (n) SET n.x = 1", "SET"),
        ("MATCH (n) REMOVE n.x", "REMOVE"),
        ("MATCH (n) DELETE n", "DELETE"),
        ("MATCH (n) DETACH DELETE n", "DETACH DELETE"),
        ("MATCH (n) NODETACH DELETE n", "NODETACH DELETE"),
        ("MERGE (n:person {name: 'x'})", "MERGE"),
        ("MATCH (n) RETURN n CREATE (m)", "CREATE"),
        ("match (n) delete n", "delete"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Refused, "{query}");
        assert_eq!(f.gqlstatus, "0A000");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains("read-only"), "{}", f.message);
        assert_eq!(
            f.alternative.as_deref(),
            Some(
                "gql() is read-only; use the Gremlin mutation steps (add_v(), add_e(), \
                 property(), drop(), merge_v())"
            )
        );
    }
}

#[test]
fn refused_constructs() {
    // `~` forms.
    for (query, text) in [
        ("MATCH (a)~[e]~(b) RETURN a", "~"),
        ("MATCH (a)<~[e]~(b) RETURN a", "<~"),
        ("MATCH (a)~>(b) RETURN a", "~>"),
        ("MATCH (a)-[e]~(b) RETURN a", "~"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Refused, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.alternative.as_deref(),
            Some("edges in Graphersal are directed; use -[e]- for either direction")
        );
    }
    // Cypher id() (D15), naming the argument as written.
    let f = fail("MATCH (n) WHERE id(n) = 1 RETURN n");
    assert_eq!(f.kind, GqlErrorKind::Refused);
    assert_eq!(f.text, "id");
    assert_eq!(
        f.alternative.as_deref(),
        Some("use element_id(n), the element's id as a string")
    );
    let f = fail("MATCH (n) RETURN ID(  f(x) )");
    assert_eq!(
        f.alternative.as_deref(),
        Some("use element_id(f(x)), the element's id as a string")
    );
    // Vendor procedures and functions.
    for (query, text) in [
        ("CALL db.labels()", "CALL"),
        ("MATCH (n) CALL apoc.x(n) YIELD y RETURN y", "CALL"),
        ("SHOW INDEXES", "SHOW"),
        ("USE graph MATCH (n) RETURN n", "USE"),
        (
            "MATCH (n) RETURN apoc.text.join(n.a, ',')",
            "apoc.text.join",
        ),
        ("MATCH (n) RETURN db.labels()", "db.labels"),
        ("MATCH (n) RETURN n.x.y(1)", "n.x.y"),
        ("MATCH (a)-[*BFS]->(b) RETURN a", "*BFS"),
        ("MATCH (a)-[e *wShortest]->(b) RETURN a", "*wShortest"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Refused, "{query}");
        assert_eq!(f.text, text, "{query}");
    }
}

#[test]
fn not_yet_supported_constructs() {
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("MATCH (n) RETURN count(*)", "count", Some("count()")),
        (
            "MATCH (n) RETURN count(DISTINCT n.x)",
            "count",
            Some("count()"),
        ),
        ("MATCH (n) RETURN sum(n.age)", "sum", Some("sum()")),
        ("MATCH (n) RETURN AVG(n.age)", "AVG", Some("mean()")),
        ("MATCH (n) RETURN min(n.age)", "min", Some("min()")),
        ("MATCH (n) RETURN max(n.age)", "max", Some("max()")),
        (
            "MATCH (n) RETURN collect(n.name)",
            "collect",
            Some("fold()"),
        ),
        (
            "MATCH (n) RETURN collect_list(n.name)",
            "collect_list",
            Some("fold()"),
        ),
        ("OPTIONAL MATCH (n) RETURN n", "OPTIONAL", Some("optional(")),
        (
            "MATCH (n) OPTIONAL MATCH (n)-->(m) RETURN m",
            "OPTIONAL",
            Some("optional("),
        ),
        ("MATCH (n) WITH n RETURN n", "WITH", None),
        ("MATCH (n) LET x = 1 RETURN x", "LET", None),
        ("MATCH (n) FILTER n.x > 1 RETURN n", "FILTER", Some("WHERE")),
        ("MATCH (n) RETURN n NEXT MATCH (m) RETURN m", "NEXT", None),
        (
            "MATCH (n) RETURN n UNION MATCH (m) RETURN m",
            "UNION",
            Some("union("),
        ),
        (
            "MATCH (n) RETURN n EXCEPT MATCH (m) RETURN m",
            "EXCEPT",
            Some("union("),
        ),
        (
            "MATCH (n) RETURN n INTERSECT MATCH (m) RETURN m",
            "INTERSECT",
            Some("union("),
        ),
        (
            "MATCH (n) WHERE EXISTS { (n)-->() } RETURN n",
            "EXISTS",
            Some("where("),
        ),
        (
            "MATCH (n) WHERE exists(n.x) RETURN n",
            "exists",
            Some("where("),
        ),
        (
            "MATCH (n) WHERE COUNT { (n)-->() } > 1 RETURN n",
            "COUNT {",
            None,
        ),
        ("FOR x IN [1] RETURN x", "FOR", Some("unfold()")),
        ("UNWIND [1] AS x RETURN x", "UNWIND", Some("unfold()")),
        (
            "MATCH (a)-[:knows]->{1,3}(b) RETURN b",
            "{",
            Some("repeat("),
        ),
        ("MATCH (a)-->*(b) RETURN b", "*", Some("repeat(")),
        ("MATCH (a)-[e]->+(b) RETURN b", "+", Some("repeat(")),
        (
            "MATCH (a)-[:knows*1..3]->(b) RETURN b",
            "*",
            Some("repeat("),
        ),
        ("MATCH (a)-[*]->(b) RETURN b", "*", Some("repeat(")),
        ("MATCH p = (a)-->(b) RETURN p", "p =", Some("path()")),
        ("MATCH ((a)-->(b)){1,2} RETURN a", "((", Some("repeat(")),
        (
            "MATCH shortestPath((a)-->(b)) RETURN a",
            "shortestPath",
            Some("repeat("),
        ),
        (
            "MATCH ANY SHORTEST (a)-->(b) RETURN a",
            "ANY SHORTEST",
            None,
        ),
        ("MATCH TRAIL (a)-->(b) RETURN a", "TRAIL (", None),
        ("MATCH (n) RETURN [x IN n.list | x]", "[", Some("unfold()")),
        ("MATCH (n) RETURN n {.name}", "n {", Some("v.name")),
        ("MATCH (n) RETURN n.list[0]", "[", None),
        ("MATCH (n) RETURN n ORDER BY n.x NULLS FIRST", "NULLS", None),
        ("MATCH (n) WHERE n.x IS TRUE RETURN n", "TRUE", None),
        ("RETURN 1", "RETURN", Some("inject(")),
        ("CYPHER 25 MATCH (n) RETURN n", "CYPHER", None),
        (
            "MATCH (n) CALL { MATCH (m) RETURN m } RETURN n",
            "CALL",
            None,
        ),
    ];
    for (query, text, alternative) in cases {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "0A000");
        assert_eq!(&f.text, text, "{query}");
        assert!(
            f.message.contains("not yet supported"),
            "{query}: {}",
            f.message
        );
        if let Some(alternative) = alternative {
            let given = f.alternative.clone().unwrap_or_default();
            assert!(given.contains(alternative), "{query}: {given}");
        }
    }
}

// ----- limits and robustness -----

#[test]
fn nesting_depth_is_bounded() {
    let wrap = |open: &str, inner: &str, close: &str, n: usize| {
        format!(
            "MATCH (n) RETURN {}{inner}{}",
            open.repeat(n),
            close.repeat(n)
        )
    };
    // Well inside the limit: parses (and proves the stack copes with deep nesting).
    let deep = MAX_DEPTH - 8;
    ok(&wrap("(", "1", ")", deep));
    ok(&wrap("[", "1", "]", deep));
    ok(&wrap("NOT ", "x", "", deep));
    ok(&wrap("- ", "x", "", deep));
    ok(&wrap("{a: ", "1", "}", deep));
    ok(&wrap("upper(", "'x'", ")", deep));
    ok(&wrap("CASE WHEN x THEN ", "1", " END", deep));
    ok(&format!(
        "MATCH (n:{}A{}) RETURN n",
        "(".repeat(deep),
        ")".repeat(deep)
    ));
    ok(&format!("MATCH (n:{}A) RETURN n", "!".repeat(deep)));
    // Past the limit: a Limit error, never a stack overflow, even far past it.
    for n in [MAX_DEPTH + 1, 100_000] {
        for query in [
            wrap("(", "1", ")", n),
            wrap("[", "1", "]", n),
            wrap("NOT ", "x", "", n),
            wrap("- ", "x", "", n),
            wrap("+", "x", "", n),
            wrap("{a: ", "1", "}", n),
            wrap("coalesce(", "1", ")", n),
            wrap("CASE WHEN x THEN ", "1", " END", n),
            format!("MATCH (n:{}A{}) RETURN n", "(".repeat(n), ")".repeat(n)),
            format!("MATCH (n:{}A) RETURN n", "!".repeat(n)),
            format!(
                "MATCH (n WHERE {}1{}) RETURN n",
                "(".repeat(n),
                ")".repeat(n)
            ),
        ] {
            let f = fail(&query);
            assert_eq!(f.kind, GqlErrorKind::Limit, "{}", &query[..40]);
            assert_eq!(f.gqlstatus, "54001");
            assert!(f.message.contains("128 levels"), "{}", f.message);
        }
    }
    // Flat operator chains grow the tree one level per operator: bounded by MAX_HEIGHT.
    let chain = |n: usize, op: &str| {
        let terms: Vec<String> = (0..n).map(|i| format!("n.a{i}")).collect();
        format!("MATCH (n) WHERE {} RETURN n", terms.join(op))
    };
    ok(&chain(MAX_HEIGHT - 8, " OR "));
    for query in [
        chain(MAX_HEIGHT + 1, " OR "),
        chain(50_000, " + "),
        format!("MATCH (n) RETURN n{}", ".a".repeat(50_000)),
        format!("MATCH (n:A{}) RETURN n", "|A".repeat(50_000)),
        format!("MATCH (n:A{}) RETURN n", ":A".repeat(50_000)),
    ] {
        let f = fail(&query);
        assert_eq!(f.kind, GqlErrorKind::Limit);
        assert!(f.message.contains("too large"), "{}", f.message);
        assert!(f.alternative.unwrap().contains("IN"));
    }
    // Wide but flat input is fine.
    let items: Vec<String> = (0..5_000).map(|i| i.to_string()).collect();
    ok(&format!("MATCH (n) RETURN [{}]", items.join(", ")));
    ok(&format!("MATCH (n){} RETURN n", "-->()".repeat(5_000)));
}

/// A small deterministic generator (xorshift64*), no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Parses `query`; every outcome is fine except a panic or a non-GQL error.
fn check(query: &str) {
    match parse(query) {
        Ok(parsed) => assert_eq!(parsed.source, query),
        Err(TraverserError::Gql { .. }) => {}
        Err(other) => panic!("{query:?}: {other:?}"),
    }
}

#[test]
fn never_panics_on_mutated_queries() {
    const SEEDS: &[&str] = &[
        "MATCH (a:person {name: 'marko'})-[e:knows]->(b) WHERE b.age > 30 RETURN a, b.name",
        "MATCH (a)--(b), (b)<-[:created]-(c:A&!B|%) RETURN DISTINCT * ORDER BY a.x DESC LIMIT 2",
        "MATCH (n WHERE n.v IN [1, 2.5e3, 0x1F, $p, `q``x`]) RETURN CASE n.v WHEN 1 THEN \
         'a\\n' ELSE \"b\" END AS c /* c */ OFFSET 1 LIMIT 2 -- end",
        "match (a)<-->(b)-[e IS x|y WHERE e.w <-1]-(c) return -9223372036854775808, a:x:y, \
         coalesce(a.k, upper(b.k)) skip 3",
        "MATCH (a) WHERE NOT a.x IS NULL AND a.y STARTS WITH 'q' OR a.z CONTAINS 'r' XOR \
         a IS NOT LABELED B RETURN a.x || 'y' AS `the x`",
        "MATCH (n) RETURN count(*), id(n), apoc.x(n), [x IN l | x], n {.a}",
        "MATCH (a)-[:k*1..3]->(b) OPTIONAL MATCH (b)~[e]~(c) WITH b CREATE (d) RETURN b",
        "MATCH (a)\n  -[e]->\n  (b)\nWHERE a.x = $p\nRETURN a;",
    ];
    const ALPHABET: &[char] = &[
        '(', ')', '[', ']', '{', '}', '-', '>', '<', '~', '/', '*', '\'', '"', '`', '\\', '$', '.',
        '0', '9', 'e', 'x', '_', ' ', '\n', ':', ',', '|', '&', '!', '%', '=', '+', ';', 'ä', '😀',
    ];
    const WORDS: &[&str] = &[
        " MATCH ",
        " RETURN ",
        " WHERE ",
        " AND ",
        " NOT ",
        " IS ",
        " NULL ",
        " CASE ",
        " END ",
        " ORDER BY ",
        " LIMIT ",
        " AS ",
        " -> ",
        " <- ",
        " -- ",
        "(",
        ")",
        "[",
        "]",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut runs = 0;
    for seed in SEEDS {
        check(seed);
        for _ in 0..400 {
            let mut chars: Vec<char> = seed.chars().collect();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(chars.len() + 1);
                match rng.below(5) {
                    0 if at < chars.len() => {
                        chars.remove(at);
                    }
                    1 if at < chars.len() => chars[at] = ALPHABET[rng.below(ALPHABET.len())],
                    2 => {
                        let word = WORDS[rng.below(WORDS.len())];
                        for (i, c) in word.chars().enumerate() {
                            chars.insert(at + i, c);
                        }
                    }
                    3 if at < chars.len() => {
                        // Duplicate a slice (repeats clauses, brackets, operators).
                        let end = (at + 1 + rng.below(12)).min(chars.len());
                        let slice: Vec<char> = chars[at..end].to_vec();
                        for (i, c) in slice.into_iter().enumerate() {
                            chars.insert(end + i, c);
                        }
                    }
                    _ => chars.insert(at, ALPHABET[rng.below(ALPHABET.len())]),
                }
            }
            let mutated: String = chars.into_iter().collect();
            check(&mutated);
            let cut = rng.below(mutated.len() + 1);
            if let Some(prefix) = mutated.get(..cut) {
                check(prefix);
            }
            runs += 1;
        }
    }
    assert_eq!(runs, SEEDS.len() * 400);
}

#[test]
fn never_panics_on_random_token_sequences() {
    const TOKENS: &[&str] = &[
        "MATCH",
        "RETURN",
        "WHERE",
        "OPTIONAL",
        "WITH",
        "ORDER",
        "BY",
        "ASC",
        "DESC",
        "SKIP",
        "OFFSET",
        "LIMIT",
        "DISTINCT",
        "AS",
        "AND",
        "OR",
        "XOR",
        "NOT",
        "IS",
        "NULL",
        "LABELED",
        "TRUE",
        "FALSE",
        "IN",
        "STARTS",
        "ENDS",
        "CONTAINS",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "CREATE",
        "DETACH",
        "DELETE",
        "CALL",
        "EXISTS",
        "NULLS",
        "UNION",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        ":",
        ",",
        ".",
        "..",
        ";",
        "|",
        "&",
        "!",
        "%",
        "*",
        "+",
        "-",
        "/",
        "=",
        "<>",
        "!=",
        "<",
        "<=",
        ">",
        ">=",
        "||",
        "->",
        "<-",
        "~",
        "~>",
        "<~",
        "a",
        "b",
        "n",
        "person",
        "`q`",
        "count",
        "id",
        "upper",
        "coalesce",
        "apoc",
        "1",
        "0",
        "2.5",
        "'s'",
        "$p",
        "9223372036854775808",
        "-9223372036854775808",
    ];
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for i in 0..3_000 {
        let len = rng.below(48);
        let mut query: Vec<&str> = (0..len).map(|_| TOKENS[rng.below(TOKENS.len())]).collect();
        // Half of the runs start like a query, to get past the first token.
        if i % 2 == 0 {
            query.insert(0, "MATCH (n)");
        }
        let separator = if i % 3 == 0 { "" } else { " " };
        check(&query.join(separator));
    }
}
