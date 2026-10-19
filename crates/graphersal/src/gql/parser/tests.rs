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
        ExprKind::IsEndpoint {
            operand,
            edge,
            end,
            negated,
        } => format!(
            "({}{} {} {})",
            if *negated { "not-" } else { "" },
            match end {
                Endpoint::Source => "source",
                Endpoint::Destination => "destination",
            },
            sexp(operand),
            sexp(edge)
        ),
        ExprKind::FunctionCall { name, args, .. } => match args {
            CallArgs::Star(_) => format!("{}(*)", name.name),
            CallArgs::Exprs(args) => format!(
                "{}({})",
                name.name,
                args.iter().map(sexp).collect::<Vec<_>>().join(" ")
            ),
            CallArgs::Cast {
                operand, target, ..
            } => format!("{}({} as {})", name.name, sexp(operand), target.name()),
            CallArgs::Trim {
                side,
                character,
                source,
            } => format!(
                "{}({:?} {} from {})",
                name.name,
                side,
                character.as_deref().map_or("-".to_string(), sexp),
                sexp(source)
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
    assert_eq!(
        f.expected,
        [
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    assert_eq!(f.suggestion.as_deref(), Some("MATCH"));
    let f = fail("MATCH (a) RETRUN a");
    assert_eq!(f.message, "unexpected `RETRUN`");
    assert_eq!(
        f.expected,
        [
            "`,`",
            "WHERE",
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
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
    assert_eq!(f.expected, ["`*`", "`{`", "WHERE", "`]`"]);
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
    assert_eq!(
        f.expected,
        [
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    let f = fail("MATCH (a) RETURN [1, 2");
    assert_eq!(f.expected, ["`,`", "`]`"]);
    let f = fail("MATCH (a) WHERE a.x IS 5 RETURN a");
    assert_eq!(
        f.expected,
        ["NOT", "NULL", "LABELED", "SOURCE", "DESTINATION"]
    );
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

// ----- stage 1b: the clause pipeline -----

/// The kind of every stage, `match`/`optional` for a MATCH clause.
fn stage_kinds(q: &Query) -> Vec<&'static str> {
    q.stages
        .iter()
        .map(|stage| match stage {
            Stage::Match(i) if q.matches[*i].optional => "optional",
            Stage::Match(_) => "match",
            Stage::With(_) => "with",
            Stage::Let(_) => "let",
            Stage::Filter(_) => "filter",
            Stage::For(_) => "for",
        })
        .collect()
}

#[test]
fn a_1a_query_is_a_pipeline_of_plain_match_stages() {
    let q = ok("MATCH (a)-->(b), (b)<--(c) MATCH (c)--(d) RETURN *");
    assert_eq!(q.stages, [Stage::Match(0), Stage::Match(1)]);
    assert!(q.matches.iter().all(|m| !m.optional));
    let path = &q.matches[0].patterns[0];
    assert!(path.variable.is_none());
    assert!(path.steps.iter().all(|s| s.edge.quantifier.is_none()));
}

#[test]
fn optional_with_let_and_filter_form_the_pipeline() {
    let q = ok("MATCH (a) OPTIONAL MATCH (a)-->(b) WHERE b.x > 1 \
         WITH DISTINCT a, count(b) AS n ORDER BY n DESC SKIP 1 LIMIT 2 WHERE n > 0 \
         LET x = n + 1, y = 2 FILTER WHERE x > 1 FILTER y = 2 \
         MATCH (a)-->(c) RETURN a, x");
    assert_eq!(
        stage_kinds(&q),
        [
            "match", "optional", "with", "let", "filter", "filter", "match"
        ]
    );
    assert_eq!(q.matches.len(), 3);
    assert_eq!(q.stages[6], Stage::Match(2));
    let optional = &q.matches[1];
    assert_eq!(
        q.text(optional.span),
        "OPTIONAL MATCH (a)-->(b) WHERE b.x > 1"
    );
    assert_eq!(sexp(optional.where_clause.as_ref().unwrap()), "(> b.x 1)");
    let Stage::With(with) = &q.stages[2] else {
        panic!("{:?}", q.stages[2]);
    };
    assert!(with.distinct && with.star.is_none());
    assert_eq!(with.items.len(), 2);
    assert_eq!(with.items[1].alias.as_ref().unwrap().name, "n");
    assert_eq!(sexp(&with.items[1].expr), "count(b)");
    assert_eq!(
        q.text(with.order_by.as_ref().unwrap().span),
        "ORDER BY n DESC"
    );
    assert_eq!(q.text(with.offset.as_ref().unwrap().span), "SKIP 1");
    assert_eq!(q.text(with.limit.as_ref().unwrap().span), "LIMIT 2");
    assert_eq!(sexp(with.where_clause.as_ref().unwrap()), "(> n 0)");
    assert_eq!(
        q.text(with.span),
        "WITH DISTINCT a, count(b) AS n ORDER BY n DESC SKIP 1 LIMIT 2 WHERE n > 0"
    );
    let Stage::Let(let_clause) = &q.stages[3] else {
        panic!("{:?}", q.stages[3]);
    };
    assert_eq!(q.text(let_clause.span), "LET x = n + 1, y = 2");
    assert_eq!(let_clause.bindings.len(), 2);
    assert_eq!(let_clause.bindings[0].variable.name, "x");
    assert_eq!(sexp(&let_clause.bindings[0].value), "(+ n 1)");
    assert_eq!(q.text(let_clause.bindings[1].span), "y = 2");
    let Stage::Filter(filter) = &q.stages[4] else {
        panic!("{:?}", q.stages[4]);
    };
    assert_eq!(q.text(filter.span), "FILTER WHERE x > 1");
    assert_eq!(sexp(&filter.condition), "(> x 1)");
    let Stage::Filter(filter) = &q.stages[5] else {
        panic!("{:?}", q.stages[5]);
    };
    assert_eq!(q.text(filter.span), "FILTER y = 2");
    // The result statement is unchanged.
    assert_eq!(q.ret.items.len(), 2);
}

#[test]
fn with_forms_and_a_leading_optional_match() {
    let q = ok("OPTIONAL MATCH (a) RETURN a");
    assert_eq!(stage_kinds(&q), ["optional"]);
    assert_eq!(q.text(q.matches[0].span), "OPTIONAL MATCH (a)");
    let q = ok("optional match (a) with * return a");
    assert_eq!(stage_kinds(&q), ["optional", "with"]);
    let Stage::With(with) = &q.stages[1] else {
        panic!()
    };
    assert!(with.star.is_some() && with.items.is_empty());
    let q = ok("MATCH (a) WITH *, a.name AS name WITH name LIMIT $n RETURN name");
    let Stage::With(with) = &q.stages[1] else {
        panic!()
    };
    assert!(with.star.is_some());
    assert_eq!(with.items[0].alias.as_ref().unwrap().name, "name");
    let Stage::With(with) = &q.stages[2] else {
        panic!()
    };
    assert_eq!(sexp(&with.limit.as_ref().unwrap().value), "$n");
    assert!(with.order_by.is_none() && with.offset.is_none() && with.where_clause.is_none());
    // `WITH ... WHERE` with no other tail.
    let q = ok("MATCH (a) WITH a WHERE a.x = 1 RETURN a");
    let Stage::With(with) = &q.stages[1] else {
        panic!()
    };
    assert_eq!(sexp(with.where_clause.as_ref().unwrap()), "(= a.x 1)");
    // Back-quoted LET variable.
    let q = ok("MATCH (a) LET `the x` = 1 RETURN `the x`");
    let Stage::Let(clause) = &q.stages[1] else {
        panic!()
    };
    assert_eq!(clause.bindings[0].variable.name, "the x");
}

#[test]
fn pipeline_errors() {
    // A pipeline clause may start the query (stage 2a, FILTER included).
    let q = ok("FILTER true MATCH (a) RETURN a");
    assert_eq!(stage_kinds(&q), ["filter", "match"]);
    // OPTIONAL blocks.
    let f = fail("MATCH (a) OPTIONAL { MATCH (a)-->(b) } RETURN a");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "OPTIONAL {");
    let f = fail("MATCH (a) OPTIONAL (a)-->(b) RETURN a");
    assert_eq!(f.text, "OPTIONAL (");
    let f = fail("MATCH (a) OPTIONAL RETURN a");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.expected, ["MATCH"]);
    // The WITH tail in order ORDER BY, OFFSET, LIMIT, WHERE.
    for (query, text) in [
        ("MATCH (a) WITH a LIMIT 1 ORDER BY a.x RETURN a", "ORDER"),
        ("MATCH (a) WITH a WHERE a.x = 1 LIMIT 1 RETURN a", "LIMIT"),
        ("MATCH (a) WITH a LIMIT 1 SKIP 1 RETURN a", "SKIP"),
        ("MATCH (a) WITH a LIMIT 1 LIMIT 2 RETURN a", "LIMIT"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains("out of place"), "{}", f.message);
        assert!(
            f.alternative
                .unwrap()
                .contains("ORDER BY, OFFSET, LIMIT, WHERE")
        );
    }
    // Expected tokens after each kind of clause.
    let f = fail("MATCH (a) WITH a b RETURN a");
    assert_eq!(f.text, "b");
    assert_eq!(
        f.expected,
        [
            "`,`",
            "AS",
            "ORDER BY",
            "OFFSET",
            "SKIP",
            "LIMIT",
            "WHERE",
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    let f = fail("MATCH (a) WITH a WHERE a.x = 1 WHERE a RETURN a");
    assert_eq!(
        f.expected,
        [
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    let f = fail("MATCH (a) LET x = 1 y = 2 RETURN a");
    assert_eq!(
        f.expected,
        [
            "`,`",
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    let f = fail("MATCH (a) FILTER a.x = 1 b RETURN a");
    assert_eq!(
        f.expected,
        [
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    let f = fail("MATCH (a) WITH");
    assert_eq!(f.expected, ["`*`", "an expression"]);
    let f = fail("MATCH (a) WITH a");
    assert!(f.message.starts_with("missing RETURN"), "{}", f.message);
    // LET definitions.
    let f = fail("MATCH (a) LET = 1 RETURN a");
    assert_eq!(f.expected, ["a variable"]);
    let f = fail("MATCH (a) LET x 1 RETURN a");
    assert_eq!(f.expected, ["`=`"]);
    let f = fail("MATCH (a) LET order = 1 RETURN a");
    assert_eq!(f.text, "order");
    assert!(f.alternative.unwrap().contains("`order`"));
    // Misspelt pipeline keywords are suggested; RETURN stays the first suggestion.
    let f = fail("MATCH (a) WITHH a RETURN a");
    assert_eq!(f.suggestion.as_deref(), Some("WITH"));
    let f = fail("MATCH (a) OPTINAL MATCH (a)-->(b) RETURN a");
    assert_eq!(f.suggestion.as_deref(), Some("OPTIONAL MATCH"));
    let f = fail("MATCH (a) FILTR a.x = 1 RETURN a");
    assert_eq!(f.suggestion.as_deref(), Some("FILTER"));
    let f = fail("MATCH (a) RETRUN a");
    assert_eq!(f.suggestion.as_deref(), Some("RETURN"));
    // After RETURN, a pipeline clause is out of place.
    for (query, text) in [
        ("MATCH (n) RETURN n WITH n", "WITH"),
        ("MATCH (n) RETURN n LET x = 1", "LET"),
        ("MATCH (n) RETURN n FILTER true", "FILTER"),
        ("MATCH (n) RETURN n OPTIONAL MATCH (m)", "OPTIONAL"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains("RETURN ends the query"), "{}", f.message);
    }
}

// ----- stage 1b: quantified edges -----

/// The quantifier of the first edge of `MATCH <pattern> RETURN *`: min, max, Cypher form and
/// its text, with the edge's own text.
fn quantified(pattern: &str) -> (u64, Option<u64>, bool, String, String) {
    let q = ok(&format!("MATCH {pattern} RETURN *"));
    let path = &q.matches[0].patterns[0];
    let step = &path.steps[0];
    assert_eq!(
        step.node.variable.as_ref().map(|v| v.name.as_str()),
        Some("b"),
        "{pattern}"
    );
    let quantifier = step
        .edge
        .quantifier
        .unwrap_or_else(|| panic!("{pattern}: no quantifier"));
    (
        quantifier.min,
        quantifier.max,
        quantifier.cypher,
        q.text(quantifier.span).to_string(),
        q.text(step.edge.span).to_string(),
    )
}

#[test]
fn gql_quantifiers_after_an_edge() {
    let cases: &[(&str, u64, Option<u64>, &str, &str)] = &[
        ("(a)-[:knows]->{1,3}(b)", 1, Some(3), "{1,3}", "-[:knows]->"),
        ("(a)-[e]->{2}(b)", 2, Some(2), "{2}", "-[e]->"),
        ("(a)-[e]->{2,}(b)", 2, None, "{2,}", "-[e]->"),
        ("(a)-[e]->{,4}(b)", 0, Some(4), "{,4}", "-[e]->"),
        ("(a)-[e]->{,}(b)", 0, None, "{,}", "-[e]->"),
        ("(a)-[e]->{ 0 , 0 }(b)", 0, Some(0), "{ 0 , 0 }", "-[e]->"),
        ("(a)-[e]->*(b)", 0, None, "*", "-[e]->"),
        ("(a)-[e]->+(b)", 1, None, "+", "-[e]->"),
        ("(a)<-[e]-{1,2}(b)", 1, Some(2), "{1,2}", "<-[e]-"),
        ("(a)-[e]-{1,2}(b)", 1, Some(2), "{1,2}", "-[e]-"),
        ("(a)-->{1,3}(b)", 1, Some(3), "{1,3}", "-->"),
        ("(a)<--*(b)", 0, None, "*", "<--"),
        ("(a)--+(b)", 1, None, "+", "--"),
        ("(a)->{2}(b)", 2, Some(2), "{2}", "->"),
        ("(a)-[e]->{0x10}(b)", 16, Some(16), "{0x10}", "-[e]->"),
    ];
    for (pattern, min, max, text, edge) in cases {
        let (got_min, got_max, cypher, got_text, got_edge) = quantified(pattern);
        assert_eq!((got_min, got_max), (*min, *max), "{pattern}");
        assert!(!cypher, "{pattern}");
        assert_eq!(got_text, *text, "{pattern}");
        assert_eq!(got_edge, *edge, "{pattern}");
    }
    // Several quantified and plain edges in one path, with directions kept.
    let q = ok("MATCH (a)-[:knows]->{1,2}(b)-[:created]->(c)<-[x]-*(d) RETURN d");
    let steps = &q.matches[0].patterns[0].steps;
    assert_eq!(steps.len(), 3);
    assert!(steps[0].edge.quantifier.is_some());
    assert!(steps[1].edge.quantifier.is_none());
    assert_eq!(steps[2].edge.direction, EdgeDirection::Left);
    assert_eq!(
        steps[2].edge.quantifier.map(|q| (q.min, q.max)),
        Some((0, None))
    );
}

#[test]
fn cypher_variable_length_edges() {
    let cases: &[(&str, u64, Option<u64>, &str)] = &[
        ("(a)-[*]->(b)", 1, None, "*"),
        ("(a)-[e*]->(b)", 1, None, "*"),
        ("(a)-[e*2]->(b)", 2, Some(2), "*2"),
        ("(a)-[:knows*1..3]->(b)", 1, Some(3), "*1..3"),
        ("(a)-[e:knows *1 .. 3]->(b)", 1, Some(3), "*1 .. 3"),
        ("(a)-[*..3]->(b)", 1, Some(3), "*..3"),
        ("(a)-[*2..]->(b)", 2, None, "*2.."),
        ("(a)-[*..]->(b)", 1, None, "*.."),
        ("(a)-[*0..2]->(b)", 0, Some(2), "*0..2"),
        ("(a)<-[*1..2]-(b)", 1, Some(2), "*1..2"),
        ("(a)-[*3]-(b)", 3, Some(3), "*3"),
    ];
    for (pattern, min, max, text) in cases {
        let (got_min, got_max, cypher, got_text, _) = quantified(pattern);
        assert_eq!((got_min, got_max), (*min, *max), "{pattern}");
        assert!(cypher, "{pattern}");
        assert_eq!(got_text, *text, "{pattern}");
    }
    // The rest of the filler after the length.
    let q = ok("MATCH (a)-[e:knows*1..2 {weight: 1} WHERE e.weight > 0]->(b) RETURN b");
    let edge = &q.matches[0].patterns[0].steps[0].edge;
    assert_eq!(edge.labels.as_ref().map(label), Some("knows".into()));
    assert!(edge.properties.is_some() && edge.where_clause.is_some());
    assert_eq!(
        q.text(edge.span),
        "-[e:knows*1..2 {weight: 1} WHERE e.weight > 0]->"
    );
}

#[test]
fn quantifier_errors() {
    // Lower bound above the upper bound.
    let f = fail("MATCH (a)-[e]->{3,1}(b) RETURN a");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "{3,1}");
    assert!(f.message.contains("greater than"), "{}", f.message);
    assert!(f.alternative.unwrap().contains("{1,3}"));
    let f = fail("MATCH (a)-[*3..1]->(b) RETURN a");
    assert_eq!(f.text, "*3..1");
    assert!(f.alternative.unwrap().contains("*1..3"));
    // A quantifier on a node.
    for (query, text) in [
        ("MATCH (a){1,2}-->(b) RETURN a", "{"),
        ("MATCH (a)-->(b)* RETURN a", "*"),
        ("MATCH (a)+ RETURN a", "+"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains("node pattern"), "{}", f.message);
    }
    // Both forms on one edge.
    let f = fail("MATCH (a)-[e*1..2]->{1,3}(b) RETURN a");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "{1,3}");
    assert!(f.alternative.unwrap().contains("`*1..2`"));
    let f = fail("MATCH (a)-[*]->+(b) RETURN a");
    assert_eq!(f.text, "+");
    // Bounds are unsigned integer literals.
    let f = fail("MATCH (a)-[e]->{1.5}(b) RETURN a");
    assert_eq!(f.expected, ["an integer", "`,`"]);
    let f = fail("MATCH (a)-[e]->{$n}(b) RETURN a");
    assert_eq!(f.text, "$n");
    let f = fail("MATCH (a)-[e]->{1,x}(b) RETURN a");
    assert_eq!(f.expected, ["an integer", "`}`"]);
    let f = fail("MATCH (a)-[e]->{1,2(b) RETURN a");
    assert_eq!(f.expected, ["`}`"]);
    let f = fail("MATCH (a)-[e]->{1 2}(b) RETURN a");
    assert_eq!(f.expected, ["`,`", "`}`"]);
    // Vendor path searches stay refused.
    let f = fail("MATCH (a)-[*BFS]->(b) RETURN a");
    assert_eq!(f.kind, GqlErrorKind::Refused);
    // Quantified path patterns stay "not yet".
    let f = fail("MATCH ((a)-->(b)){1,2} RETURN a");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
}

// ----- stage 1b: path variables -----

#[test]
fn path_variables() {
    let q = ok("MATCH p = (a)-->(b), `the q` = (c) RETURN p");
    let paths = &q.matches[0].patterns;
    assert_eq!(paths[0].variable.as_ref().unwrap().name, "p");
    assert_eq!(q.text(paths[0].variable.as_ref().unwrap().span), "p");
    assert_eq!(q.text(paths[0].span), "p = (a)-->(b)");
    assert_eq!(paths[1].variable.as_ref().unwrap().name, "the q");
    assert_eq!(q.text(paths[1].span), "`the q` = (c)");
    // With a quantified edge and in an OPTIONAL MATCH.
    let q = ok("MATCH (a) OPTIONAL MATCH p = (a)-[:knows]->{1,3}(b) RETURN path_length(p)");
    let path = &q.matches[1].patterns[0];
    assert_eq!(path.variable.as_ref().unwrap().name, "p");
    assert!(path.steps[0].edge.quantifier.is_some());
    // A path search mode word is a path variable when `=` follows.
    let q = ok("MATCH trail = (a)-->(b) RETURN trail");
    assert_eq!(
        q.matches[0].patterns[0].variable.as_ref().unwrap().name,
        "trail"
    );
    // The missing-RETURN alternative names the path variable.
    let f = fail("MATCH p = (a)-->(b)");
    assert!(f.alternative.unwrap().contains("RETURN p, a, b"));
    // A keyword as a path variable must be back-quoted; `p =` without a pattern is an error.
    let f = fail("MATCH p = RETURN p");
    assert_eq!(f.text, "RETURN");
    assert_eq!(f.expected, ["`(`"]);
}

// ----- stage 1b: aggregates and path functions -----

#[test]
fn aggregate_calls() {
    assert_eq!(expr("count(*)"), "count(*)");
    assert_eq!(expr("COUNT(n)"), "COUNT(n)");
    assert_eq!(expr("sum(n.age) + 1"), "(+ sum(n.age) 1)");
    assert_eq!(expr("collect(n.name)"), "collect(n.name)");
    assert_eq!(expr("collect_list(n.name)"), "collect_list(n.name)");
    assert_eq!(expr("avg(n.age)"), "avg(n.age)");
    assert_eq!(expr("min(n.age)"), "min(n.age)");
    assert_eq!(expr("max(n.age)"), "max(n.age)");
    // DISTINCT and `*` are kept in the tree, with spans.
    let q = ok("MATCH (n) RETURN count(DISTINCT n.x), count( * ), max(n.x)");
    let ExprKind::FunctionCall { distinct, args, .. } = &q.ret.items[0].expr.kind else {
        panic!()
    };
    assert!(*distinct);
    assert!(matches!(args, CallArgs::Exprs(a) if a.len() == 1));
    assert_eq!(q.text(q.ret.items[0].expr.span), "count(DISTINCT n.x)");
    let ExprKind::FunctionCall { distinct, args, .. } = &q.ret.items[1].expr.kind else {
        panic!()
    };
    assert!(!*distinct);
    let CallArgs::Star(star) = args else { panic!() };
    assert_eq!(q.text(*star), "*");
    assert_eq!(q.text(q.ret.items[1].expr.span), "count( * )");
    let ExprKind::FunctionCall { distinct, .. } = &q.ret.items[2].expr.kind else {
        panic!()
    };
    assert!(!*distinct);
    // Nesting is the binder's check, not the parser's.
    assert_eq!(expr("count(sum(n.x))"), "count(sum(n.x))");
    // Errors.
    let f = fail("MATCH (n) RETURN sum(*)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "*");
    assert_eq!(f.message, "only count(*) takes `*`, not sum()");
    let f = fail("MATCH (n) RETURN count(DISTINCT *)");
    assert_eq!(f.text, "*");
    assert!(f.message.contains("count(DISTINCT *)"), "{}", f.message);
    let f = fail("MATCH (n) RETURN count()");
    assert_eq!(f.message, "count() takes 1 argument, got 0");
    let f = fail("MATCH (n) RETURN count(DISTINCT)");
    assert_eq!(f.message, "count() takes 1 argument, got 0");
    let f = fail("MATCH (n) RETURN collect(n.a, n.b)");
    assert_eq!(f.message, "collect_list() takes 1 argument, got 2");
    assert_eq!(f.text, "collect(n.a, n.b)");
    let f = fail("MATCH (n) RETURN count(* 1)");
    assert_eq!(f.expected, ["`)`"]);
    // Scalars still refuse DISTINCT and `*`.
    let f = fail("MATCH (n) RETURN upper(DISTINCT n.name)");
    assert!(f.message.contains("DISTINCT"), "{}", f.message);
}

#[test]
fn path_length_and_its_cypher_alias() {
    assert_eq!(expr("path_length(n)"), "path_length(n)");
    assert_eq!(expr("length(n)"), "length(n)");
    assert_eq!(
        lookup_function("LENGTH").map(|s| (s.function, s.name(), s.kind)),
        Some((Function::PathLength, "path_length", FunctionKind::Scalar))
    );
    let f = fail("MATCH (n) RETURN length(n, n)");
    assert_eq!(f.message, "path_length() takes 1 argument, got 2");
    let f = fail("MATCH (n) RETURN path_lenght(n)");
    assert_eq!(f.suggestion.as_deref(), Some("path_length()"));
}

#[test]
fn never_panics_on_stage_1b_queries() {
    // Prefixes of 1b queries and their mutations hit every new error path.
    let queries = [
        "MATCH (a) OPTIONAL MATCH p = (a)-[e:knows*1..3 {w: 1}]->{1,2}(b) \
         WITH DISTINCT a, count(DISTINCT b) AS n ORDER BY n SKIP 1 LIMIT 2 WHERE n > 0 \
         LET x = n, y = count(*) FILTER WHERE x > 1 RETURN a, x",
        "MATCH (a)-->{,}(b)<-[*..]-+(c) RETURN collect(c)",
    ];
    for query in queries {
        for end in 0..=query.len() {
            if let Some(prefix) = query.get(..end) {
                check(prefix);
            }
            if let (Some(head), Some(tail)) = (query.get(..end), query.get(end.min(query.len())..))
            {
                for insert in ["{", "*", "+", "..", ",", "=", "WITH", "OPTIONAL", "("] {
                    check(&format!("{head}{insert}{tail}"));
                }
            }
        }
    }
}

// ----- stage 1b: robustness of the new grammar -----

/// The stage 1b tokens and seeds (lists of their own: the 1a lists above stay as they are).
#[test]
fn never_panics_on_1b_tokens_and_mutated_1b_queries() {
    const SEEDS: &[&str] = &[
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name, count(s)",
        "MATCH (p) WITH DISTINCT p, count(*) AS c ORDER BY c DESC SKIP 1 LIMIT 2 WHERE c > 0 \
         LET d = c + 1 FILTER d > 1 RETURN p, d",
        "MATCH p = (a)-[e:knows WHERE e.w > 1]->{1,3}(b)<-[f*..2]-(c)-->+(d) RETURN p, e, \
         path_length(p)",
        "MATCH (a)-[e]-{,2}(b), (c)<-[:x|y]-*(d) RETURN collect_list(DISTINCT a.x), avg(b.y), \
         sum(c.z), min(d), max(e) GROUP BY a",
        "OPTIONAL MATCH (a) WITH a WHERE a IS NOT NULL RETURN nodes(p), relationships(p)",
    ];
    const TOKENS: &[&str] = &[
        "MATCH",
        "OPTIONAL",
        "WITH",
        "LET",
        "FILTER",
        "RETURN",
        "GROUP",
        "BY",
        "DISTINCT",
        "AS",
        "ORDER",
        "SKIP",
        "LIMIT",
        "WHERE",
        "=",
        ",",
        "(a)",
        "(b:L)",
        "-[e]->",
        "<-[e:T]-",
        "-[",
        "]-",
        "]->",
        "*",
        "+",
        "..",
        "{",
        "}",
        "{1,3}",
        "{,2}",
        "{0}",
        "{2,}",
        "1",
        "18446744073709551616",
        "p",
        "p =",
        "e",
        "count(*)",
        "count(",
        "sum(",
        "avg(",
        "collect_list(",
        "collect(",
        "min(",
        "max(",
        "path_length(",
        "nodes(",
        "DISTINCT a",
        ")",
        "a.x",
        "$n",
    ];
    let mut rng = Rng(0x1B1B_5EED_0000_0001);
    for seed in SEEDS {
        check(seed);
        for _ in 0..300 {
            let mut words: Vec<&str> = seed.split(' ').collect();
            for _ in 0..1 + rng.below(3) {
                let at = rng.below(words.len() + 1);
                match rng.below(3) {
                    0 if at < words.len() => {
                        words.remove(at);
                    }
                    1 if at < words.len() => words[at] = TOKENS[rng.below(TOKENS.len())],
                    _ => words.insert(at, TOKENS[rng.below(TOKENS.len())]),
                }
            }
            let separator = if rng.below(4) == 0 { "" } else { " " };
            check(&words.join(separator));
        }
    }
    for i in 0..2_000 {
        let len = rng.below(40);
        let mut query: Vec<&str> = (0..len).map(|_| TOKENS[rng.below(TOKENS.len())]).collect();
        if i % 2 == 0 {
            query.insert(0, "MATCH (n)");
        }
        check(&query.join(" "));
    }
}

/// `GROUP BY` on `WITH` (no ISO counterpart) names implicit grouping (stage 1c accepts it on
/// `RETURN`: `group_by_follows_the_return_items`).
#[test]
fn group_by_is_explained() {
    for query in [
        "MATCH (a) WITH a.x AS x, count(*) AS c GROUP BY x RETURN x",
        "MATCH (a) WHERE a.x > 1 GROUP BY a.x RETURN a.x",
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, "GROUP BY", "{query}");
        assert!(f.message.contains("grouping is implicit"), "{}", f.message);
        assert!(f.alternative.unwrap().contains("RETURN p.name, count(s)"));
    }
}

/// ISO GQL `GROUP BY` after the `RETURN` items: keys, the empty grouping set, the place.
#[test]
fn group_by_follows_the_return_items() {
    let query = ok("MATCH (a) RETURN a.x AS x, count(*) group by x, a.y ORDER BY x");
    let group_by = query.ret.group_by.as_ref().unwrap();
    let keys: Vec<String> = group_by.keys.iter().map(sexp).collect();
    assert_eq!(keys, ["x", "a.y"]);
    assert_eq!(query.text(group_by.span), "group by x, a.y");
    assert_eq!(query.text(query.ret.span), "RETURN a.x AS x, count(*)");
    assert!(query.order_by.is_some());
    let query = ok("MATCH (a) RETURN count(*) GROUP BY ()");
    assert!(query.ret.group_by.as_ref().unwrap().keys.is_empty());
    assert!(ok("MATCH (a) RETURN a").ret.group_by.is_none());
    // `group` stays a name elsewhere.
    assert!(ok("MATCH (group) RETURN group").ret.group_by.is_none());
    // After ORDER BY: the place is named.
    let f = fail("MATCH (a) RETURN a.x, count(*) ORDER BY a.x GROUP BY a.x");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "GROUP BY");
    assert!(
        f.message.contains("right after the RETURN items"),
        "{}",
        f.message
    );
    // No key.
    let f = fail("MATCH (a) RETURN a GROUP BY");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert!(f.expected.iter().any(|e| e == "an expression"), "{f:?}");
}

// ----- stage 1c: function forms -----

#[test]
fn cast_takes_one_or_two_word_type_names() {
    assert_eq!(expr("CAST(n.age AS INTEGER)"), "CAST(n.age as INTEGER)");
    assert_eq!(expr("cast(n.age as int)"), "cast(n.age as INTEGER)");
    assert_eq!(expr("CAST(1 AS SIGNED INTEGER)"), "CAST(1 as INTEGER)");
    assert_eq!(expr("CAST('1' AS DOUBLE PRECISION)"), "CAST('1' as FLOAT)");
    assert_eq!(expr("CAST(1 AS double)"), "CAST(1 as FLOAT)");
    assert_eq!(expr("CAST(1 AS CHAR VARYING)"), "CAST(1 as STRING)");
    assert_eq!(expr("CAST(1 AS varchar)"), "CAST(1 as STRING)");
    assert_eq!(expr("CAST('true' AS BOOL)"), "CAST('true' as BOOLEAN)");
    assert_eq!(
        expr("CAST(n.a + 1 AS STRING) || 'x'"),
        "(|| CAST((+ n.a 1) as STRING) 'x')"
    );
    // The span of the call covers the whole form; the target span the written type name.
    let q = ok("MATCH (n) RETURN CAST(n.age AS DOUBLE PRECISION) AS x");
    let item = &q.ret.items[0].expr;
    assert_eq!(q.text(item.span), "CAST(n.age AS DOUBLE PRECISION)");
    let ExprKind::FunctionCall {
        args: CallArgs::Cast { target_span, .. },
        ..
    } = &item.kind
    else {
        panic!("not a CAST: {item:?}");
    };
    assert_eq!(q.text(*target_span), "DOUBLE PRECISION");
}

#[test]
fn cast_errors_name_the_supported_types() {
    let f = fail("MATCH (n) RETURN CAST(n.age AS INTEGR)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.gqlstatus, "42001");
    assert_eq!(f.text, "INTEGR");
    assert_eq!(f.suggestion.as_deref(), Some("INTEGER"));
    assert_eq!(f.expected, ["INTEGER", "FLOAT", "STRING", "BOOLEAN"]);
    for (query, text) in [
        ("MATCH (n) RETURN CAST(n.age AS DATE)", "DATE"),
        ("MATCH (n) RETURN CAST(n.age AS INT32)", "INT32"),
        ("MATCH (n) RETURN CAST(n.tags AS LIST<STRING>)", "LIST"),
        (
            "MATCH (n) RETURN CAST(n.t AS ZONED DATETIME)",
            "ZONED DATETIME",
        ),
        ("MATCH (n) RETURN CAST(n.x AS DECIMAL)", "DECIMAL"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}");
        assert_eq!(f.gqlstatus, "0A000", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.alternative
                .as_deref()
                .unwrap_or("")
                .contains("INTEGER, FLOAT"),
            "{query}: {f:?}"
        );
    }
    let f = fail("MATCH (n) RETURN CAST(n.age INTEGER)");
    assert_eq!(f.expected, ["AS"]);
    assert_eq!(f.text, "INTEGER");
    let f = fail("MATCH (n) RETURN CAST(n.age AS INTEGER, 1)");
    assert_eq!(f.expected, ["`)`"]);
    let f = fail("MATCH (n) RETURN CAST(n.age AS 5)");
    assert_eq!(f.expected, ["a type name"]);
    let f = fail("MATCH (n) RETURN CAST(DISTINCT n.age AS INTEGER)");
    assert!(
        f.message.contains("DISTINCT is only allowed"),
        "{}",
        f.message
    );
}

#[test]
fn trim_sql_forms_and_plain_calls() {
    assert_eq!(expr("TRIM(n.name)"), "TRIM(n.name)");
    assert_eq!(expr("trim(n.list, 2)"), "trim(n.list 2)");
    assert_eq!(
        expr("TRIM(LEADING 'x' FROM n.name)"),
        "TRIM(Leading 'x' from n.name)"
    );
    assert_eq!(
        expr("trim(trailing FROM n.name)"),
        "trim(Trailing - from n.name)"
    );
    assert_eq!(
        expr("TRIM(BOTH 'x' || 'y' FROM n.name)"),
        "TRIM(Both (|| 'x' 'y') from n.name)"
    );
    assert_eq!(expr("TRIM('x' FROM n.name)"), "TRIM(Both 'x' from n.name)");
    assert_eq!(expr("TRIM(FROM n.name)"), "TRIM(Both - from n.name)");
    // A side word before `)`, `,`, `.` or an operator is a variable.
    assert_eq!(expr("trim(leading)"), "trim(leading)");
    assert_eq!(expr("trim(both.name, 1)"), "trim(both.name 1)");
    assert_eq!(expr("trim(trailing || 'x')"), "trim((|| trailing 'x'))");
    assert_eq!(expr("trim(from)"), "trim(from)");
    let f = fail("MATCH (n) RETURN TRIM(LEADING n.name)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.expected, ["FROM"]);
    assert_eq!(f.text, ")");
    assert!(f.alternative.unwrap_or_default().contains("ltrim(s)"));
    let f = fail("MATCH (n) RETURN TRIM(LEADING 'x' FROM n.name, 1)");
    assert_eq!(f.expected, ["`)`"]);
    let f = fail("MATCH (n) RETURN trim(n.name, 1, 2)");
    assert_eq!(f.message, "trim() takes 1 to 2 arguments, got 3");
}

#[test]
fn sql_substring_is_refused_with_the_alternative() {
    for (query, text) in [
        ("MATCH (n) RETURN SUBSTRING(n.name FROM 1 FOR 2)", "FROM"),
        ("MATCH (n) RETURN substring(n.name FOR 2)", "FOR"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains("substring(s, start, length)"),
            "{alternative}"
        );
        assert!(alternative.contains("left(s, n)"), "{alternative}");
    }
    assert_eq!(expr("substring(n.name, 1, 2)"), "substring(n.name 1 2)");
    assert_eq!(expr("substring(n.name, 1)"), "substring(n.name 1)");
    let f = fail("MATCH (n) RETURN substring(n.name)");
    assert_eq!(f.message, "substring() takes 2 to 3 arguments, got 1");
}

#[test]
fn is_source_of_and_is_destination_of() {
    assert_eq!(expr("n IS SOURCE OF e"), "(source n e)");
    assert_eq!(expr("n is not destination of e"), "(not-destination n e)");
    // A predicate: tighter than comparison and AND, its right operand like the one of IN.
    assert_eq!(
        expr("n IS SOURCE OF e AND m IS DESTINATION OF e"),
        "(and (source n e) (destination m e))"
    );
    assert_eq!(expr("n IS SOURCE OF e = TRUE"), "(= (source n e) true)");
    let q = ok("MATCH (n) WHERE n IS NOT SOURCE OF e RETURN n");
    let Some(condition) = &q.matches[0].where_clause else {
        panic!("no WHERE");
    };
    assert_eq!(q.text(condition.span), "n IS NOT SOURCE OF e");
    let f = fail("MATCH (a) WHERE a IS NOT 5 RETURN a");
    assert_eq!(f.expected, ["NULL", "LABELED", "SOURCE", "DESTINATION"]);
    // `SOURCE` without `OF` is no predicate.
    let f = fail("MATCH (a) WHERE a IS SOURCE e RETURN a");
    assert_eq!(f.text, "SOURCE");
    let f = fail("MATCH (a) WHERE a IS SOURCE OF RETURN a");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
}

#[test]
fn stage_1c_functions_parse_with_their_arity() {
    for call in [
        "toInteger(1)",
        "TOINTEGER(1)",
        "toIntegerOrNull(1)",
        "toFloat(1)",
        "toFloatOrNull(1)",
        "toBoolean(1)",
        "toBooleanOrNull(1)",
        "toString(1)",
        "toStringOrNull(1)",
        "left('a', 1)",
        "right('a', 1)",
        "substring('a', 1, 2)",
        "btrim('a')",
        "btrim('a', 'b')",
        "ltrim('a', 'b')",
        "rtrim('a')",
        "replace('a', 'b', 'c')",
        "replace('a', 'b', 'c', 1)",
        "split('a', ',')",
        "reverse('a')",
        "byte_length('a')",
        "octet_length('a')",
        "isEmpty('a')",
        "nullif(1, 2)",
        "NULLIF(1, 2)",
        "abs(1)",
        "floor(1)",
        "ceil(1)",
        "ceiling(1)",
        "round(1)",
        "round(1, 2)",
        "round(1, 2, 'HALF_UP')",
        "sign(1)",
        "sqrt(1)",
        "exp(1)",
        "ln(1)",
        "log(1)",
        "log(2, 8)",
        "log10(1)",
        "power(2, 3)",
        "mod(5, 2)",
        "head(n.l)",
        "last(n.l)",
        "LAST(n.l)",
        "tail(n.l)",
        "range(1, 5)",
        "range(1, 5, 2)",
        "keys(n)",
        "properties(n)",
        "startNode(e)",
        "endNode(e)",
        "nodes(p)",
        "relationships(p)",
    ] {
        let tree = expr(call);
        assert!(tree.ends_with(')'), "{call}: {tree}");
    }
    for (call, message) in [
        ("left('a')", "left() takes 2 arguments, got 1"),
        (
            "replace('a', 'b', 'c', 1, 2)",
            "replace() takes 3 to 4 arguments, got 5",
        ),
        (
            "round(1, 2, 'UP', 4)",
            "round() takes 1 to 3 arguments, got 4",
        ),
        ("range(1)", "range() takes 2 to 3 arguments, got 1"),
        ("log(1, 2, 3)", "log() takes 1 to 2 arguments, got 3"),
        ("nullif(1)", "nullif() takes 2 arguments, got 1"),
        ("startNode()", "startNode() takes 1 argument, got 0"),
        ("toInteger(1, 2)", "toInteger() takes 1 argument, got 2"),
    ] {
        let f = fail(&format!("MATCH (n) RETURN {call}"));
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{call}");
        assert_eq!(f.message, message, "{call}");
    }
    let f = fail("MATCH (n) RETURN abs(DISTINCT n.age)");
    assert!(
        f.message.contains("DISTINCT is only allowed"),
        "{}",
        f.message
    );
}

#[test]
fn not_yet_functions_name_the_reason_and_the_alternative() {
    for (call, kind, needle) in [
        ("rand()", GqlErrorKind::Refused, "sample(1)"),
        ("randomUUID()", GqlErrorKind::Refused, "sample(1)"),
        ("sin(n.x)", GqlErrorKind::NotYet, "math(\"sin(_)\")"),
        ("pi()", GqlErrorKind::NotYet, "math("),
        ("e()", GqlErrorKind::NotYet, "math("),
        ("date('2024-01-01')", GqlErrorKind::NotYet, "0.2.0"),
        ("timestamp()", GqlErrorKind::NotYet, "0.2.0"),
        ("toIntegerList(n.l)", GqlErrorKind::NotYet, "unfold()"),
        ("normalize(n.s)", GqlErrorKind::NotYet, "lower()"),
        ("reduce(n.l)", GqlErrorKind::NotYet, "unfold()"),
        ("elements(p)", GqlErrorKind::NotYet, "nodes(p)"),
        (
            "percentileCont(n.x, 0.5)",
            GqlErrorKind::NotYet,
            "collect_list",
        ),
    ] {
        let f = fail(&format!("MATCH (n) RETURN {call}"));
        assert_eq!(f.kind, kind, "{call}");
        assert_eq!(f.gqlstatus, "0A000", "{call}");
        let name = call.split('(').next().unwrap_or("");
        assert_eq!(f.text, name, "{call}");
        assert!(f.message.contains(&format!("{name}()")), "{}", f.message);
        let alternative = f.alternative.unwrap_or_default();
        assert!(alternative.contains(needle), "{call}: {alternative}");
    }
    let f = fail("MATCH (n) WHERE exists(n.x) RETURN n");
    assert!(
        f.alternative
            .unwrap_or_default()
            .contains("n.p IS NOT NULL"),
        "exists(n.x)"
    );
    let f = fail("MATCH (n) RETURN 2 ^ 10");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "^");
    assert!(f.alternative.unwrap_or_default().contains("power(x, y)"));
}

#[test]
fn stage_1c_rows_keep_the_older_suggestions() {
    for (name, suggestion) in [
        ("uper", "upper()"),
        ("lowr", "lower()"),
        ("sizee", "size()"),
        ("coalese", "coalesce()"),
        ("lenght", "length()"),
        ("colect", "collect()"),
    ] {
        let f = fail(&format!("MATCH (n) RETURN {name}(n)"));
        assert_eq!(f.suggestion.as_deref(), Some(suggestion), "{name}");
    }
    for (name, suggestion) in [
        ("toInt", "toInteger()"),
        ("tostrin", "toString()"),
        ("substr", "substring()"),
        ("startnod", "startNode()"),
    ] {
        let f = fail(&format!("MATCH (n) RETURN {name}(n)"));
        assert_eq!(f.suggestion.as_deref(), Some(suggestion), "{name}");
    }
}

// ----- stage 2a: FOR / UNWIND and queries without MATCH -----

/// The `FOR` clause at `stages[i]`.
fn for_clause(q: &Query, i: usize) -> &ForClause {
    match q.stages.get(i) {
        Some(Stage::For(clause)) => clause,
        other => panic!("stage {i} is not FOR: {other:?}"),
    }
}

#[test]
fn for_and_unwind_forms() {
    let q = ok("FOR x IN [1, 2, 3] RETURN x");
    assert_eq!(stage_kinds(&q), ["for"]);
    assert!(q.matches.is_empty());
    let f = for_clause(&q, 0);
    assert_eq!(f.variable.name, "x");
    assert_eq!(q.text(f.variable.span), "x");
    assert_eq!(q.text(f.list.span), "[1, 2, 3]");
    assert!(matches!(f.list.kind, ExprKind::List(ref items) if items.len() == 3));
    assert_eq!(f.ordinal, None);
    assert!(!f.unwind);
    assert_eq!(q.text(f.span), "FOR x IN [1, 2, 3]");

    // The Cypher alias: the same clause, marked.
    let q = ok("UNWIND $ids AS id RETURN id");
    let f = for_clause(&q, 0);
    assert!(f.unwind);
    assert_eq!(f.variable.name, "id");
    assert_eq!(f.list.kind, ExprKind::Parameter("ids".into()));
    assert_eq!(q.text(f.span), "UNWIND $ids AS id");

    // Ordinals; keywords in any case, back-quoted names.
    let q = ok("for x in ['a', 'b'] with ordinality i RETURN x, i");
    let ordinal = for_clause(&q, 0).ordinal.as_ref().unwrap();
    assert_eq!(ordinal.kind, OrdinalKind::Ordinality);
    assert_eq!(ordinal.variable.name, "i");
    assert_eq!(q.text(ordinal.span), "with ordinality i");
    assert_eq!(
        q.text(for_clause(&q, 0).span),
        "for x in ['a', 'b'] with ordinality i"
    );
    let q = ok("FOR `x y` IN $xs WITH OFFSET `match` RETURN `x y`, `match`");
    let f = for_clause(&q, 0);
    assert_eq!(f.variable.name, "x y");
    let ordinal = f.ordinal.as_ref().unwrap();
    assert_eq!(ordinal.kind, OrdinalKind::Offset);
    assert_eq!(ordinal.variable.name, "match");
    assert_eq!(q.text(ordinal.span), "WITH OFFSET `match`");

    // `IN` inside the list expression is the operator; the clause's `IN` comes first.
    let q = ok("FOR b IN [2 IN [2], 3] RETURN b");
    assert_eq!(q.text(for_clause(&q, 0).list.span), "[2 IN [2], 3]");
    let q = ok("FOR b IN $xs WITH OFFSET k FILTER k > 1 RETURN b");
    assert_eq!(q.text(for_clause(&q, 0).list.span), "$xs");
    // Any expression is a list expression (its type is checked later).
    let q = ok("MATCH p = (a)-->(b) FOR n IN nodes(p) RETURN n");
    assert_eq!(q.text(for_clause(&q, 1).list.span), "nodes(p)");
}

#[test]
fn for_stands_anywhere_in_the_pipeline() {
    let q = ok("MATCH (a) FOR x IN a.list UNWIND [1] AS y WITH a, x, y \
                FOR z IN [x] WITH ORDINALITY i OPTIONAL MATCH (a)-->(b) LET w = z FILTER w > 0 \
                RETURN a, i");
    assert_eq!(
        stage_kinds(&q),
        [
            "match", "for", "for", "with", "for", "optional", "let", "filter"
        ]
    );
    assert_eq!(
        q.text(for_clause(&q, 4).span),
        "FOR z IN [x] WITH ORDINALITY i"
    );
    // A WITH clause after a FOR clause stays a WITH clause.
    let q = ok("FOR x IN [1] WITH x AS y RETURN y");
    assert_eq!(stage_kinds(&q), ["for", "with"]);
    assert_eq!(for_clause(&q, 0).ordinal, None);
    // `ordinality` is a plain variable: `WITH ordinality AS o` / `WITH ordinality, x` project it.
    let q = ok("LET ordinality = 1 FOR x IN [1] WITH ordinality AS o RETURN o");
    assert_eq!(stage_kinds(&q), ["let", "for", "with"]);
    let q = ok("LET ordinality = 1 FOR x IN [1] WITH ordinality, x RETURN x");
    assert_eq!(stage_kinds(&q), ["let", "for", "with"]);
    // ... and a declared `ordinality` before RETURN is a WITH clause too.
    let q = ok("LET ordinality = 1 FOR x IN [1] WITH ordinality RETURN ordinality");
    assert_eq!(stage_kinds(&q), ["let", "for", "with"]);
    assert_eq!(for_clause(&q, 1).ordinal, None);
    // A name after ORDINALITY always makes the ordinal.
    let q = ok("LET ordinality = 1 FOR x IN [1] WITH ORDINALITY ordinality RETURN x");
    assert_eq!(stage_kinds(&q), ["let", "for"]);
}

#[test]
fn queries_may_start_without_match() {
    let q = ok("RETURN 1 + 2 AS x");
    assert!(q.stages.is_empty() && q.matches.is_empty());
    assert_eq!(q.text(q.ret.span), "RETURN 1 + 2 AS x");
    let q = ok("RETURN $a AS a ORDER BY a LIMIT 1;");
    assert!(q.stages.is_empty());
    assert!(q.limit.is_some());
    let q = ok("LET x = $a RETURN x");
    assert_eq!(stage_kinds(&q), ["let"]);
    let q = ok("WITH 1 AS x MATCH (a) RETURN a, x");
    assert_eq!(stage_kinds(&q), ["with", "match"]);
    let q = ok("LET x = 1 MATCH (a) RETURN a");
    assert_eq!(stage_kinds(&q), ["let", "match"]);
    let q = ok("FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN n");
    assert_eq!(stage_kinds(&q), ["for", "match"]);
    assert_eq!(q.matches.len(), 1);
    // A leading FILTER is accepted too; an unknown first token expects every clause a query
    // may start with.
    let q = ok("FILTER $flag RETURN 1");
    assert_eq!(stage_kinds(&q), ["filter"]);
    let f = fail("FOO 1");
    assert_eq!(
        f.expected,
        [
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
}

#[test]
fn for_and_unwind_errors() {
    // FOR without IN.
    let f = fail("FOR x [1, 2] RETURN x");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "[");
    assert_eq!(f.expected, ["IN"]);
    assert!(f.message.starts_with("FOR needs IN"), "{}", f.message);
    assert!(f.alternative.unwrap().contains("FOR x IN [1, 2, 3]"));
    let f = fail("FOR x");
    assert_eq!(f.expected, ["IN"]);
    assert!(f.message.contains("end of input"), "{}", f.message);
    // The Cypher order written with FOR: the variable is missing.
    let f = fail("FOR [1, 2] AS x RETURN x");
    assert_eq!(f.text, "[");
    assert_eq!(f.expected, ["a variable"]);
    // A keyword as the variable: back-quote it.
    let f = fail("FOR match IN [1] RETURN 1");
    assert_eq!(f.expected, ["a variable"]);
    assert!(f.alternative.unwrap().contains("`match`"));
    // UNWIND without AS.
    let f = fail("UNWIND [1, 2] x RETURN x");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "x");
    assert_eq!(f.expected, ["AS"]);
    assert!(f.message.starts_with("UNWIND needs AS"), "{}", f.message);
    assert!(f.alternative.unwrap().contains("UNWIND [1, 2, 3] AS x"));
    let f = fail("UNWIND [1] AS RETURN 1");
    assert_eq!(f.text, "RETURN");
    assert_eq!(f.expected, ["a variable"]);
    // GQL's IN with UNWIND: a syntax error naming AS.
    let f = fail("UNWIND x IN [1] RETURN x");
    assert_eq!(f.text, "RETURN");
    assert_eq!(f.expected, ["AS"]);
    // An ordinal without a name.
    for (query, text) in [
        ("FOR x IN [1] WITH ORDINALITY RETURN x", "WITH ORDINALITY"),
        ("FOR x IN [1] WITH ORDINALITY", "WITH ORDINALITY"),
        ("FOR x IN [1] WITH OFFSET RETURN x", "WITH OFFSET"),
        ("FOR x IN [1] WITH OFFSET 1 RETURN x", "WITH OFFSET"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.expected, ["a variable"], "{query}");
        assert!(f.message.contains("needs a variable name"), "{}", f.message);
        assert!(f.alternative.unwrap().contains(" i RETURN x, i"), "{query}");
    }
    let f = fail("FOR x IN [1] WITH OFFSET limit RETURN x");
    assert!(f.alternative.unwrap().contains("`limit`"));
    // An ordinal after UNWIND.
    let f = fail("UNWIND [1] AS x WITH ORDINALITY i RETURN x");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "WITH ORDINALITY i");
    assert!(
        f.alternative
            .unwrap()
            .contains("FOR x IN list WITH ORDINALITY i")
    );
    // An unexpected token after the clause names the ordinal forms and the next clauses.
    let f = fail("FOR x IN [1] y RETURN x");
    assert_eq!(f.text, "y");
    assert_eq!(
        f.expected,
        [
            "WITH ORDINALITY",
            "WITH OFFSET",
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    let f = fail("FOR x IN [1] WITH OFFSET i y RETURN x");
    assert_eq!(
        f.expected,
        [
            "MATCH",
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "FOR",
            "UNWIND",
            "RETURN"
        ]
    );
    let f = fail("FOR x IN [1]");
    assert!(f.message.starts_with("missing RETURN"), "{}", f.message);
    assert!(f.alternative.unwrap().contains("RETURN x"));
    // After RETURN.
    for query in [
        "MATCH (a) RETURN a FOR x IN [1]",
        "MATCH (a) RETURN a UNWIND [1] AS x",
    ] {
        let f = fail(query);
        assert!(f.message.contains("after RETURN"), "{query}: {}", f.message);
    }
    // A misplaced FOR clause is still found by position: line and column.
    let f = fail("FOR x IN [1]\nWITH OFFSET\nRETURN x");
    assert_eq!((f.line, f.column), (2, 1));
}
