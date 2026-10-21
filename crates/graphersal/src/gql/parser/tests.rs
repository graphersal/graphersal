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
        ExprKind::Index(index) => match &index.subscript {
            Subscript::At(at) => format!("(at {} {})", sexp(&index.target), sexp(at)),
            Subscript::Slice { from, to } => {
                let bound = |b: &Option<Expr>| b.as_ref().map_or("_".to_string(), sexp);
                format!(
                    "(slice {} {} {})",
                    sexp(&index.target),
                    bound(from),
                    bound(to)
                )
            }
        },
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
                BinaryOp::Pow => "^",
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
        ExprKind::Subquery(subquery) => {
            let form = match subquery.form {
                SubqueryForm::Braces => "exists{}",
                SubqueryForm::Parens => "exists()",
                SubqueryForm::PatternPredicate => "pattern",
                SubqueryForm::Comprehension => "comprehension",
            };
            let clauses = subquery
                .matches
                .iter()
                .map(|clause| {
                    let mut out = format!("(match {}", clause.patterns.len());
                    if let Some(w) = &clause.where_clause {
                        out.push_str(&format!(" (where {})", sexp(w)));
                    }
                    out.push(')');
                    out
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("({form} {clauses})")
        }
        ExprKind::MapProjection(projection) => {
            let items = projection
                .items
                .iter()
                .map(|item| match item {
                    ProjectionItem::Property(key) => format!(".{}", key.name),
                    ProjectionItem::AllProperties(_) => ".*".to_string(),
                    ProjectionItem::Variable(v) => v.name.clone(),
                    ProjectionItem::Entry(entry) => {
                        format!("({} {})", entry.key.name, sexp(&entry.value))
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("(project {} {{{items}}})", sexp(&projection.target))
        }
        ExprKind::ListComprehension(c) => {
            let head = match &c.form {
                ComprehensionForm::List => "for".to_string(),
                ComprehensionForm::Predicate(predicate) => predicate.name().to_string(),
                ComprehensionForm::Reduce(accumulator) => format!(
                    "reduce {} {}",
                    accumulator.variable.name,
                    sexp(&accumulator.init)
                ),
            };
            let mut out = format!("({head} {} {}", c.variable.name, sexp(&c.list));
            if let Some(filter) = &c.filter {
                out.push_str(&format!(" (where {})", sexp(filter)));
            }
            if let Some(map) = &c.map {
                out.push_str(&format!(" (map {})", sexp(map)));
            }
            out.push(')');
            out
        }
        ExprKind::PatternComprehension(c) => {
            let pattern = &c.body.patterns[0];
            let mut out = format!(
                "(collect {}{} edges",
                pattern
                    .variable
                    .as_ref()
                    .map_or(String::new(), |v| format!("{} = ", v.name)),
                pattern.steps.len()
            );
            if let Some(w) = &c.body.where_clause {
                out.push_str(&format!(" (where {})", sexp(w)));
            }
            out.push_str(&format!(" (map {}))", sexp(&c.map)));
            out
        }
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
    assert_eq!(f.expected, ["UNION", "NEXT", "end of input"]);
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
            "CALL",
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
            "CALL",
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
            "CALL",
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
    // Stage 3f: `^` is the power operator; `@` stays an unexpected character.
    let f = fail("MATCH (n) RETURN n @ 2");
    assert_eq!(f.text, "@");
}

// ----- refused and not yet supported constructs -----

#[test]
fn writes_not_yet_supported_are_refused() {
    for (query, text, step) in [
        (
            "MATCH (n) FOREACH (x IN [1] | SET n.x = x)",
            "FOREACH",
            "FOR x IN",
        ),
        (
            "match (n) foreach (x in [1] | set n.x = x)",
            "foreach",
            "FOR x IN",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}");
        assert_eq!(f.gqlstatus, "0A000");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains("is not yet supported"), "{}", f.message);
        let alternative = f.alternative.unwrap_or_default();
        assert!(alternative.contains(step), "{query}: {alternative}");
    }
    // A write clause after RETURN: RETURN ends the query.
    let f = fail("MATCH (n) RETURN n CREATE (m)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "CREATE");
    assert_eq!(f.message, "CREATE after RETURN: RETURN ends the query");
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
            "MATCH (((a)-->(b)){1,2}) RETURN a",
            "(",
            Some("((x)-->(y)-->(z))"),
        ),
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
        ("MATCH (n) RETURN n ORDER BY n.x NULLS FIRST", "NULLS", None),
        ("MATCH (n) WHERE n.x IS TRUE RETURN n", "TRUE", None),
        ("CYPHER 25 MATCH (n) RETURN n", "CYPHER", None),
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
            Stage::Call(_) => "call",
            Stage::Insert(_) => "insert",
            Stage::Set(_) => "set",
            Stage::Remove(_) => "remove",
            Stage::Delete(_) => "delete",
            Stage::Merge(_) => "merge",
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
            "CALL",
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
            "CALL",
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
            "CALL",
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
            "CALL",
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
    // Quantified path patterns parse since stage 2e (their E17 refusals:
    // `parenthesized_path_pattern_refusals`).
    let q = ok("MATCH ((a)-->(b)){1,2} RETURN a");
    assert_eq!(q.matches[0].patterns[0].groups.len(), 1);
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
        ("elements(p)", GqlErrorKind::NotYet, "nodes(p)"),
        // Stage 3f: percentileCont() is supported; stDev() stays not yet.
        ("stDev(n.x)", GqlErrorKind::NotYet, "collect_list"),
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
    // Stage 3f: `^` is supported (it was "not yet" naming power(x, y)).
    assert!(parse("MATCH (n) RETURN 2 ^ 10").is_ok());
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
            "CALL",
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
            "CALL",
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
            "CALL",
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

// ----- stage 2b: EXISTS and pattern predicates -----

/// The subquery of the first RETURN item of `MATCH (n) RETURN <src>`.
fn subquery(src: &str) -> SubqueryExpr {
    let query = ok(&format!("MATCH (n) RETURN {src}"));
    match &query.ret.items[0].expr.kind {
        ExprKind::Subquery(subquery) => (**subquery).clone(),
        other => panic!("{src}: not a subquery: {other:?}"),
    }
}

#[test]
fn exists_forms() {
    for (src, tree) in [
        ("EXISTS { MATCH (n)-->(m) }", "(exists{} (match 1))"),
        (
            "EXISTS { MATCH (n)-->(m) WHERE m.x > 1 MATCH (m)-->(k), (k)-->() }",
            "(exists{} (match 1 (where (> m.x 1))) (match 2))",
        ),
        (
            "EXISTS { (n)-->(m), (m)-->(k) WHERE m.x > 1 }",
            "(exists{} (match 2 (where (> m.x 1))))",
        ),
        ("exists { p = (n)-[:T*1..2]->() }", "(exists{} (match 1))"),
        ("EXISTS ( (n)-->() )", "(exists() (match 1))"),
        (
            "EXISTS ((n)-->() WHERE n.a = 1)",
            "(exists() (match 1 (where (= n.a 1))))",
        ),
        (
            "EXISTS ( MATCH (n)-->() MATCH ()-->(n) )",
            "(exists() (match 1) (match 1))",
        ),
        ("EXISTS ( p = (n)-->() )", "(exists() (match 1))"),
        (
            "EXISTS { MATCH (n)-->(m) WHERE EXISTS { (m)-->() } }",
            "(exists{} (match 1 (where (exists{} (match 1)))))",
        ),
        (
            "NOT EXISTS { (n)-->() } AND n.a = 1",
            "(and (not (exists{} (match 1))) (= n.a 1))",
        ),
    ] {
        assert_eq!(expr(src), tree, "{src}");
    }
    let s = subquery("EXISTS { MATCH (n)-[e:T]->(m:L {a: 1}) WHERE e.w > 0 }");
    assert_eq!(s.kind, SubqueryKind::Exists);
    assert_eq!(s.form, SubqueryForm::Braces);
    let clause = &s.matches[0];
    assert!(!clause.optional);
    assert_eq!(clause.patterns[0].steps.len(), 1);
    assert_eq!(
        clause.patterns[0].steps[0].edge.direction,
        EdgeDirection::Right
    );
    // A subquery is an ordinary operand: in WHERE, RETURN, CASE, function arguments.
    let q = ok("MATCH (n) WHERE EXISTS { (n)-->() } OR n.a = 1 RETURN n");
    assert!(matches!(
        &q.matches[0].where_clause,
        Some(Expr {
            kind: ExprKind::Binary {
                op: BinaryOp::Or,
                ..
            },
            ..
        })
    ));
    assert_eq!(
        expr("CASE WHEN (n)-->() THEN 1 ELSE 0 END"),
        "(case (when (pattern (match 1)) 1) (else 0))"
    );
    assert_eq!(
        expr("coalesce((n)-->(), false)"),
        "coalesce((pattern (match 1)) false)"
    );
}

#[test]
fn pattern_predicates() {
    for (src, edges) in [
        ("(n)-->()", vec![EdgeDirection::Right]),
        ("(n)<--()", vec![EdgeDirection::Left]),
        ("(n)--()", vec![EdgeDirection::Any]),
        ("(n)->()", vec![EdgeDirection::Right]),
        ("(n)<->()", vec![EdgeDirection::Any]),
        ("(n)<-->()", vec![EdgeDirection::Any]),
        ("(n)-[:T]->()", vec![EdgeDirection::Right]),
        ("(n)<-[:T]-()", vec![EdgeDirection::Left]),
        ("(n)-[e]-()", vec![EdgeDirection::Any]),
        ("(n)<-[:T]->()", vec![EdgeDirection::Any]),
        ("(n:L {a: 1})-[:T*1..3]->(:M)", vec![EdgeDirection::Right]),
        ("(n)-->{1,3}()", vec![EdgeDirection::Right]),
        ("(n)--{2}()", vec![EdgeDirection::Any]),
        // A weak edge first, a strong one later: the whole chain is the pattern.
        (
            "(n)-(m)-->(k)",
            vec![EdgeDirection::Any, EdgeDirection::Right],
        ),
        (
            "(n)<-(m)--(k)",
            vec![EdgeDirection::Left, EdgeDirection::Any],
        ),
        (
            "(n)-->(m)<--(k)-(j)",
            vec![
                EdgeDirection::Right,
                EdgeDirection::Left,
                EdgeDirection::Any,
            ],
        ),
    ] {
        let s = subquery(src);
        assert_eq!(s.form, SubqueryForm::PatternPredicate, "{src}");
        assert_eq!(s.matches.len(), 1, "{src}");
        let clause = &s.matches[0];
        assert!(clause.where_clause.is_none(), "{src}");
        assert_eq!(clause.patterns.len(), 1, "{src}");
        let directions: Vec<_> = clause.patterns[0]
            .steps
            .iter()
            .map(|step| step.edge.direction)
            .collect();
        assert_eq!(directions, edges, "{src}");
    }
    // Operator precedence: a pattern predicate is a primary.
    assert_eq!(
        expr("NOT (n)-[:T]->() AND (n)<--()"),
        "(and (not (pattern (match 1))) (pattern (match 1)))"
    );
    assert_eq!(expr("(n)-->() = true"), "(= (pattern (match 1)) true)");
    assert_eq!(expr("(n)-->() IS NULL"), "(null? (pattern (match 1)))");
    assert_eq!(expr("[(n)-->(), 1]"), "[(pattern (match 1)) 1]");
    // Nested in an inline WHERE of the pattern itself, and of MATCH.
    let q = ok("MATCH (n WHERE (n)-->()) RETURN (n)-[:T]->(m WHERE (m)-->())");
    assert!(matches!(
        &q.matches[0].patterns[0].start.where_clause,
        Some(Expr {
            kind: ExprKind::Subquery(_),
            ..
        })
    ));
}

#[test]
fn pattern_predicate_lookahead_keeps_arithmetic() {
    for (src, tree) in [
        ("(n) - (m)", "(- n m)"),
        ("(n)-(m)", "(- n m)"),
        ("(n.a)-(1)", "(- n.a 1)"),
        ("(n)-1", "(- n 1)"),
        ("(n) - 1", "(- n 1)"),
        ("(n)--1", "(- n -1)"),
        ("(n) - - (m)", "(- n (- m))"),
        ("(n) - -(m)", "(- n (- m))"),
        ("(n)<-(m)", "(< n (- m))"),
        ("(n) <- (m)", "(< n (- m))"),
        ("(n) - (m) - (k)", "(- (- n m) k)"),
        ("(n) - [1]", "(- n [1])"),
        ("(n) <-[1]", "(< n (- [1]))"),
        ("(n.a + 1) * 2", "(* (+ n.a 1) 2)"),
        ("((n))", "n"),
        ("(n) - (m) < (k)", "(< (- n m) k)"),
    ] {
        assert_eq!(expr(src), tree, "{src}");
    }
    // A long chain of weak edges stays arithmetic (the lookahead gives up after a bound).
    let chain = vec!["(n)"; 200].join(" - ");
    let parsed = expr(&chain);
    assert!(parsed.starts_with("(- (- "), "{parsed}");
    // Past the bound, the chain splits: the operands closer to the strong edge than the bound
    // read as a pattern (longer than the binder's edge limit anyway).
    let chain = format!("{} --> (m)", vec!["(n)"; 70].join("-"));
    let q = ok(&format!("MATCH (n) RETURN {chain}"));
    let ExprKind::Binary {
        op: BinaryOp::Sub,
        right,
        ..
    } = &q.ret.items[0].expr.kind
    else {
        panic!("not arithmetic first")
    };
    assert!(matches!(&right.kind, ExprKind::Subquery(_)));
    // Within the bound, the whole chain is the pattern.
    let chain = format!("{} --> (m)", vec!["(n)"; 60].join("-"));
    let q = ok(&format!("MATCH (n) RETURN {chain}"));
    assert!(matches!(
        &q.ret.items[0].expr.kind,
        ExprKind::Subquery(s) if s.matches[0].patterns[0].steps.len() == 60
    ));
}

#[test]
fn subquery_spans() {
    let query = "MATCH (n) WHERE NOT (n)-[:T]->(:L) AND EXISTS {\n MATCH (n)-->(m)\n} RETURN n";
    let q = ok(query);
    let Some(Expr {
        kind: ExprKind::Binary { left, right, .. },
        ..
    }) = &q.matches[0].where_clause
    else {
        panic!("not an AND")
    };
    let ExprKind::Unary { operand, .. } = &left.kind else {
        panic!("not a NOT")
    };
    assert_eq!(q.text(operand.span), "(n)-[:T]->(:L)");
    let ExprKind::Subquery(s) = &operand.kind else {
        panic!("not a pattern predicate")
    };
    assert_eq!(q.text(s.span), "(n)-[:T]->(:L)");
    assert_eq!(q.text(s.matches[0].span), "(n)-[:T]->(:L)");
    assert_eq!(q.text(right.span), "EXISTS {\n MATCH (n)-->(m)\n}");
    let ExprKind::Subquery(s) = &right.kind else {
        panic!("not EXISTS")
    };
    assert_eq!(q.text(s.matches[0].span), "MATCH (n)-->(m)");
    assert_eq!((s.matches[0].span.line, s.matches[0].span.column), (2, 2));
    let q = ok("MATCH (n) RETURN EXISTS ( (n)-->(m), (m)-->() WHERE m.a > 1 ) AS x");
    let ExprKind::Subquery(s) = &q.ret.items[0].expr.kind else {
        panic!("not EXISTS")
    };
    assert_eq!(
        q.text(s.span),
        "EXISTS ( (n)-->(m), (m)-->() WHERE m.a > 1 )"
    );
    assert_eq!(
        q.text(s.matches[0].span),
        "(n)-->(m), (m)-->() WHERE m.a > 1"
    );
    assert_eq!(
        q.ret.items[0].alias.as_ref().map(|a| a.name.as_str()),
        Some("x")
    );
}

#[test]
fn subquery_errors() {
    // Cypher 4 exists(value): refused for good, with the IS NOT NULL test.
    for (query, text, here) in [
        ("MATCH (n) WHERE exists(n.x) RETURN n", "exists(n.x)", "n.x"),
        ("MATCH (n) RETURN EXISTS(n) AS x", "EXISTS(n)", "n"),
        ("MATCH (n) RETURN exists($p) AS x", "exists($p)", "$p"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Refused, "{query}");
        assert_eq!(f.gqlstatus, "0A000", "{query}");
        assert_eq!(f.text, text, "{query}");
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.starts_with("test a property with n.p IS NOT NULL"),
            "{alternative}"
        );
        assert!(
            alternative.contains(&format!("here {here} IS NOT NULL")),
            "{alternative}"
        );
    }
    // Clauses other than MATCH inside a body that is not a whole query (stage 2d parses
    // braced bodies ending in RETURN): not yet.
    for (query, text) in [
        (
            "MATCH (n) WHERE EXISTS { MATCH (n)-->(m) WITH m } RETURN n",
            "WITH",
        ),
        (
            "MATCH (n) WHERE EXISTS ( MATCH (n)-->(m) LIMIT 1 ) RETURN n",
            "LIMIT",
        ),
        (
            "MATCH (n) WHERE EXISTS { (n)-->(m) ORDER BY m } RETURN n",
            "ORDER",
        ),
        (
            "MATCH (n) WHERE EXISTS { OPTIONAL MATCH (n)-->() } RETURN n",
            "OPTIONAL MATCH",
        ),
        (
            "MATCH (n) WHERE EXISTS { MATCH (n)-->() UNION MATCH (n) } RETURN n",
            "UNION",
        ),
        (
            "MATCH (n) WHERE EXISTS { ((n)-->{2}()){2} } RETURN n",
            "{2}",
        ),
        (
            "MATCH (n) WHERE EXISTS { ANY SHORTEST (n)-->() } RETURN n",
            "ANY SHORTEST",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
    }
    let f = fail("MATCH (n) WHERE EXISTS { MATCH (n)-->(m) WITH m } RETURN n");
    assert!(
        f.message
            .contains("WITH in the body of EXISTS { } without RETURN"),
        "{}",
        f.message
    );
    // Whole-query bodies and COUNT { } parse since stage 2d (D23); the binder refuses them
    // "not yet" until they lower (bind tests).
    let s = subquery("EXISTS { MATCH (n)-->(m) RETURN m }");
    assert!(s.matches.is_empty() && s.query.is_some());
    let s = subquery("COUNT { (n)-->() }");
    assert_eq!(s.kind, SubqueryKind::Count);
    // Syntax errors name what may stand there.
    for (query, text, expected) in [
        ("MATCH (n) WHERE EXISTS x RETURN n", "x", vec!["`{`", "`(`"]),
        (
            "MATCH (n) WHERE EXISTS {} RETURN n",
            "}",
            vec!["MATCH", "`(`"],
        ),
        (
            "MATCH (n) WHERE EXISTS () RETURN n",
            ")",
            vec!["MATCH", "`(`"],
        ),
        (
            "MATCH (n) WHERE EXISTS { (n)-->() MATCH (m) } RETURN n",
            "MATCH",
            vec!["`,`", "WHERE", "`}`"],
        ),
        (
            "MATCH (n) WHERE EXISTS { MATCH (n)-->() WHERE n.a = 1 (m) } RETURN n",
            "(",
            vec!["MATCH", "`}`"],
        ),
        (
            "MATCH (n) WHERE EXISTS ( (n)-->() } RETURN n",
            "}",
            vec!["`,`", "WHERE", "`)`"],
        ),
        (
            "MATCH (n) WHERE EXISTS { (n)-->( } RETURN n",
            "}",
            vec!["a variable", "`:`", "IS", "`{`", "WHERE", "`)`"],
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.expected, expected, "{query}");
    }
    // A pattern predicate ends at its last node: a decisive edge without a node is a pattern
    // error, not arithmetic.
    let f = fail("MATCH (n) WHERE (n)--> RETURN n");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    // `exists` is a keyword: as a variable it is back-quoted.
    let f = fail("MATCH (n) RETURN exists");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    ok("MATCH (`exists`) RETURN `exists`");
}

#[test]
fn subqueries_are_bounded() {
    // Nesting: the parser's depth limit, never a stack overflow.
    let mut nested = "(n)-->()".to_string();
    for _ in 0..200 {
        nested = format!("EXISTS {{ MATCH (n) WHERE {nested} }}");
    }
    let f = fail(&format!("MATCH (n) WHERE {nested} RETURN n"));
    assert_eq!(f.kind, GqlErrorKind::Limit);
    let mut nested = "(n)-->()".to_string();
    for _ in 0..200 {
        nested = format!("(n WHERE {nested})-->()");
    }
    let f = fail(&format!("MATCH (n) WHERE {nested} RETURN n"));
    assert_eq!(f.kind, GqlErrorKind::Limit);
    // Height: a subquery is one level taller than the tallest tree in its body, so a tall
    // chain inside EXISTS counts toward the expression it stands in.
    // `n.a OR n.a ...` of k items is k + 1 high; the subquery one more, each AND one more.
    let tall = vec!["n.a"; MAX_HEIGHT - 4].join(" OR ");
    ok(&format!(
        "MATCH (n) WHERE n.b AND (n.c AND EXISTS {{ MATCH (n) WHERE {tall} }}) RETURN n"
    ));
    let f = fail(&format!(
        "MATCH (n) WHERE n.d AND (n.b AND (n.c AND EXISTS {{ MATCH (n) WHERE {tall} }})) \
         RETURN n"
    ));
    assert_eq!(f.kind, GqlErrorKind::Limit);
    let f = fail(&format!(
        "MATCH (n) WHERE n.d AND (n.b AND (n.c AND EXISTS {{ MATCH (n {{a: 1}})-[:T]->(:L) \
         WHERE {tall} }})) RETURN n"
    ));
    assert_eq!(f.kind, GqlErrorKind::Limit);
    let f = fail(&format!(
        "MATCH (n) WHERE n.d AND (n.b AND (n.c AND (n)-[e WHERE {tall}]->())) RETURN n"
    ));
    assert_eq!(f.kind, GqlErrorKind::Limit);
    ok(&format!(
        "MATCH (n) WHERE n.b AND (n.c AND (n)-[e WHERE {tall}]->()) RETURN n"
    ));
}

/// Random expressions over the tokens that pattern predicates and arithmetic share: the
/// parser never panics, and every accepted subquery has a non-empty body.
#[test]
fn random_subquery_expressions_never_panic() {
    const TOKENS: &[&str] = &[
        "(", "(", "(", ")", ")", ")", "-", "-", "->", "<-", "--", "-->", "<--", "<->", "[", "]",
        "{", "}", "n", "m", "e", ":T", ":L", "1", "n.a", "*", "+", "{1,2}", ",", "WHERE", "EXISTS",
        "EXISTS {", "EXISTS (", "MATCH", "NOT", "AND", "OR", "=", "<", "p =", "RETURN", "OPTIONAL",
        "COUNT {", "exists(", "[e]", "-[e]->", "<-[:T]-", "()",
    ];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut accepted = 0;
    for _ in 0..10_000 {
        let len = 1 + (next() % 16) as usize;
        let body: Vec<&str> = (0..len)
            .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
            .collect();
        let query = format!("MATCH (n) WHERE {} RETURN n", body.join(" "));
        if let Ok(q) = parse(&query) {
            accepted += 1;
            let mut stack: Vec<&Expr> = q.matches[0].where_clause.iter().collect();
            while let Some(e) = stack.pop() {
                match &e.kind {
                    ExprKind::Subquery(s) => assert!(!s.matches.is_empty(), "{query}"),
                    ExprKind::Unary { operand, .. } => stack.push(operand),
                    ExprKind::Binary { left, right, .. } => {
                        stack.push(left);
                        stack.push(right);
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(accepted > 0);
}

// ----- stage 2c: map projections, list and pattern comprehensions -----

#[test]
fn map_projection_forms() {
    assert_eq!(
        expr("n {.name, .*, m, `k`: n.age + 1}"),
        "(project n {.name .* m (k (+ n.age 1))})"
    );
    assert_eq!(expr("n {}"), "(project n {})");
    // A keyword is a name after `.` and before `:`; a back-quoted keyword is a variable item.
    assert_eq!(
        expr("n {.match, return: 1, `order`}"),
        "(project n {.match (return 1) order})"
    );
    // Nested, inside other expressions, and followed by postfix operators.
    assert_eq!(
        expr("n {.a, f: m {.b}}"),
        "(project n {.a (f (project m {.b}))})"
    );
    assert_eq!(expr("[n {.a}]"), "[(project n {.a})]");
    assert_eq!(expr("n {.a}.a"), "(project n {.a}).a");
    // Spans: the expression from the variable to `}`, the braces on their own.
    let q = ok("MATCH (n) RETURN n {.name, k: 1} AS m");
    let item = &q.ret.items[0].expr;
    assert_eq!(q.text(item.span), "n {.name, k: 1}");
    let ExprKind::MapProjection(p) = &item.kind else {
        panic!("{item:?}");
    };
    assert_eq!(q.text(p.braces), "{.name, k: 1}");
    let ProjectionItem::Property(key) = &p.items[0] else {
        panic!("{p:?}");
    };
    assert_eq!(q.text(key.span), "name");
    let q = ok("MATCH (n) RETURN n {.*} AS m");
    let ExprKind::MapProjection(p) = &q.ret.items[0].expr.kind else {
        panic!("not a projection");
    };
    let ProjectionItem::AllProperties(star) = &p.items[0] else {
        panic!("{p:?}");
    };
    assert_eq!(q.text(*star), ".*");
    // Only a variable is projected: after another expression `{` ends it.
    let f = fail("MATCH (n) RETURN n.a {.b}");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "{");
}

#[test]
fn record_constructor_is_the_map_literal() {
    // `RECORD {..}` (any case) is the map literal; nested, empty, keyword keys.
    assert_eq!(expr("RECORD {a: 1, b: 'x'}"), "{a: 1, b: 'x'}");
    assert_eq!(expr("record {}"), "{}");
    assert_eq!(expr("Record {a: RECORD {match: [1]}}"), "{a: {match: [1]}}");
    assert_eq!(expr("[RECORD {a: 1}]"), "[{a: 1}]");
    // The span covers the keyword.
    let q = ok("RETURN RECORD {a: 1} AS m");
    let item = &q.ret.items[0].expr;
    assert_eq!(q.text(item.span), "RECORD {a: 1}");
    assert!(matches!(item.kind, ExprKind::Map(_)), "{item:?}");
    // Contextual: elsewhere `record` stays a variable, also before a projection that does not
    // start with `}` or `key:`, and back-quoted always.
    assert_eq!(expr("record"), "record");
    assert_eq!(expr("record.name"), "record.name");
    assert_eq!(expr("record {.name}"), "(project record {.name})");
    assert_eq!(expr("record {.*}"), "(project record {.*})");
    assert_eq!(expr("record {n, k: 1}"), "(project record {n (k 1)})");
    assert_eq!(expr("`record` {k: 1}"), "(project record {(k 1)})");
    assert_eq!(expr("`RECORD` {}"), "(project RECORD {})");
    // A malformed record is a syntax error of the map literal.
    let f = fail("RETURN RECORD {a: 1 b: 2} AS m");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "b");
}

#[test]
fn map_projection_errors() {
    for (query, text, expected) in [
        (
            "MATCH (n) RETURN n {.}",
            "}",
            &["a property name", "`*`"][..],
        ),
        ("MATCH (n) RETURN n {.a b}", "b", &["`,`", "`}`"]),
        (
            "MATCH (n) RETURN n {.a,}",
            "}",
            &["`.`", "a variable", "a key"],
        ),
        ("MATCH (n) RETURN n {match}", "}", &["`:`"]),
        ("MATCH (n) RETURN n {k: }", "}", &["an expression"]),
        (
            "MATCH (n) RETURN n {1}",
            "1",
            &["`.`", "a variable", "a key"],
        ),
        ("MATCH (n) RETURN n {.a", "", &["`,`", "`}`"]),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        for e in expected {
            assert!(
                f.expected.iter().any(|x| x == e),
                "{query}: {:?}",
                f.expected
            );
        }
    }
}

#[test]
fn list_comprehension_forms() {
    assert_eq!(
        expr("[x IN n.l WHERE x > 1 | x * 2]"),
        "(for x n.l (where (> x 1)) (map (* x 2)))"
    );
    assert_eq!(
        expr("[x IN n.l WHERE x > 1]"),
        "(for x n.l (where (> x 1)))"
    );
    assert_eq!(expr("[x IN n.l | x.a]"), "(for x n.l (map x.a))");
    assert_eq!(expr("[x IN [1, 2]]"), "(for x [1 2])");
    assert_eq!(expr("[`x y` IN [1] | `x y`]"), "(for x y [1] (map x y))");
    // Nested comprehensions, and one inside a list literal.
    assert_eq!(
        expr("[x IN [y IN n.l | y + 1] WHERE x IN [z IN [1] | z]]"),
        "(for x (for y n.l (map (+ y 1))) (where (in x (for z [1] (map z)))))"
    );
    // A list whose first item is a membership test is written with parentheses.
    assert_eq!(expr("[(x IN n.l), 1]"), "[(in x n.l) 1]");
    assert_eq!(expr("[1, x IN n.l]"), "[1 (in x n.l)]");
    // Span: from `[` to `]`.
    let q = ok("MATCH (n) RETURN [x IN n.l WHERE x > 1 | x] AS l");
    assert_eq!(
        q.text(q.ret.items[0].expr.span),
        "[x IN n.l WHERE x > 1 | x]"
    );
    let ExprKind::ListComprehension(c) = &q.ret.items[0].expr.kind else {
        panic!("not a comprehension");
    };
    assert_eq!(q.text(c.variable.span), "x");
    assert_eq!(q.text(c.list.span), "n.l");
}

/// A `|` at the comprehension's own bracket level ends its list or `WHERE`; label
/// alternatives are written in parentheses there and need none after the `|`.
#[test]
fn comprehension_bar_ends_label_expressions() {
    assert_eq!(
        expr("[x IN n.l WHERE x:A | x]"),
        "(for x n.l (where (labeled x A)) (map x))"
    );
    assert_eq!(
        expr("[x IN n.l WHERE x:(A|B) | x]"),
        "(for x n.l (where (labeled x (| A B))) (map x))"
    );
    assert_eq!(
        expr("[x IN n.l WHERE (x:A|B) | x]"),
        "(for x n.l (where (labeled x (| A B))) (map x))"
    );
    assert_eq!(
        expr("[x IN n.l | x:A|B]"),
        "(for x n.l (map (labeled x (| A B))))"
    );
    assert_eq!(
        expr("[x IN n.l WHERE x IS LABELED A | x]"),
        "(for x n.l (where (labeled x A)) (map x))"
    );
    // Label alternatives outside a comprehension, and in a nested list, are unchanged.
    assert_eq!(expr("n:A|B"), "(labeled n (| A B))");
    assert_eq!(
        expr("[x IN n.l WHERE [x:A|B] | x]"),
        "(for x n.l (where [(labeled x (| A B))]) (map x))"
    );
    assert_eq!(
        expr("[(a)-[:A|B]->(b) WHERE b:C | b]"),
        "(collect 1 edges (where (labeled b C)) (map b))"
    );
    // An unparenthesized alternative in the WHERE: the error names the fix.
    let f = fail("MATCH (n) RETURN [x IN n.l WHERE x:A|B | x]");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "|");
    assert!(
        f.alternative.unwrap_or_default().contains("x:(A|B)"),
        "the hint names the parentheses"
    );
}

#[test]
fn list_comprehension_errors() {
    for (query, text, expected) in [
        (
            "MATCH (n) RETURN [x IN n.l WHERE x > 1 x]",
            "x",
            &["`|`", "`]`"][..],
        ),
        (
            "MATCH (n) RETURN [x IN n.l 1]",
            "1",
            &["WHERE", "`|`", "`]`"],
        ),
        ("MATCH (n) RETURN [x IN n.l | x x]", "x", &["`]`"]),
        ("MATCH (n) RETURN [x IN | x]", "|", &["an expression"]),
        (
            "MATCH (n) RETURN [x IN n.l WHERE | x]",
            "|",
            &["an expression"],
        ),
        ("MATCH (n) RETURN [x IN n.l | ]", "]", &["an expression"]),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        for e in expected {
            assert!(
                f.expected.iter().any(|x| x == e),
                "{query}: {:?}",
                f.expected
            );
        }
    }
}

#[test]
fn pattern_comprehension_forms() {
    assert_eq!(
        expr("[(n)-[:knows]->(f) | f.name]"),
        "(collect 1 edges (map f.name))"
    );
    assert_eq!(
        expr("[(n)-->(f)<-[e]-(g) WHERE f.age > 30 | g]"),
        "(collect 2 edges (where (> f.age 30)) (map g))"
    );
    assert_eq!(expr("[p = (n)-->() | p]"), "(collect p = 1 edges (map p))");
    assert_eq!(
        expr("[(n)-[:knows]->{1,2}(f) | f]"),
        "(collect 1 edges (map f))"
    );
    assert_eq!(expr("[(n)-(m)-->(f) | f]"), "(collect 2 edges (map f))");
    assert_eq!(
        expr("[(n)-->(f) | [(f)-->(g) | g.name]]"),
        "(collect 1 edges (map (collect 1 edges (map g.name))))"
    );
    // The body is one non-optional MATCH clause; spans.
    let q = ok("MATCH (n) RETURN [p = (n)-[:knows]->(f) WHERE f.age > 1 | f.name] AS l");
    let item = &q.ret.items[0].expr;
    assert_eq!(
        q.text(item.span),
        "[p = (n)-[:knows]->(f) WHERE f.age > 1 | f.name]"
    );
    let ExprKind::PatternComprehension(c) = &item.kind else {
        panic!("{item:?}");
    };
    assert!(!c.body.optional);
    assert_eq!(c.body.patterns.len(), 1);
    assert_eq!(q.text(c.body.span), "p = (n)-[:knows]->(f) WHERE f.age > 1");
    assert_eq!(q.text(c.body.patterns[0].span), "p = (n)-[:knows]->(f)");
    assert_eq!(q.text(c.map.span), "f.name");
}

/// Without `WHERE` or `|` after the pattern, a bracket keeps its stage 2b reading: a list
/// literal whose items are pattern predicates and expressions.
#[test]
fn a_list_of_pattern_predicates_stays_a_list() {
    assert_eq!(expr("[(n)-->()]"), "[(pattern (match 1))]");
    assert_eq!(expr("[(n)-->(), 1]"), "[(pattern (match 1)) 1]");
    assert_eq!(
        expr("[(n)-->() AND n.a]"),
        "[(and (pattern (match 1)) n.a)]"
    );
    assert_eq!(expr("[p = (n)-->()]"), "[(= p (pattern (match 1)))]");
    // Arithmetic in parentheses stays arithmetic (the pattern-predicate lookahead).
    assert_eq!(expr("[(n) - (m)]"), "[(- n m)]");
    let f = fail("MATCH (n) RETURN [(n) - (m) | 1]");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "|");
}

#[test]
fn pattern_comprehension_errors() {
    for (query, text, expected) in [
        ("MATCH (n) RETURN [(n)-->(f) WHERE f.a]", "]", &["`|`"][..]),
        ("MATCH (n) RETURN [(n)-->(f) | ]", "]", &["an expression"]),
        ("MATCH (n) RETURN [(n)-->(f) | f f]", "f", &["`]`"]),
        (
            "MATCH (n) RETURN [(n)-->(f) WHERE | f]",
            "|",
            &["an expression"],
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        for e in expected {
            assert!(
                f.expected.iter().any(|x| x == e),
                "{query}: {:?}",
                f.expected
            );
        }
    }
    // A pattern comprehension declares its own names: they do not feed the missing-RETURN
    // alternative.
    let f = fail("MATCH (a) WITH a, [(a)-->(zz) | zz] AS l");
    assert!(f.message.starts_with("missing RETURN"), "{}", f.message);
    let alternative = f.alternative.unwrap_or_default();
    assert!(alternative.contains("RETURN a"), "{alternative}");
    assert!(!alternative.contains("zz"), "{alternative}");
}

/// Cypher's list predicates and `reduce` (2d.T5d, D23) parse into the comprehension node with
/// their form, in any case; `reduce`'s list ends at a `|` of its own bracket level.
#[test]
fn list_functions_parse_into_comprehensions() {
    assert_eq!(
        expr("all(x IN n.l WHERE x > 1)"),
        "(all x n.l (where (> x 1)))"
    );
    assert_eq!(
        expr("ANY(x IN n.l WHERE x > 1)"),
        "(any x n.l (where (> x 1)))"
    );
    assert_eq!(expr("None(x IN [1] WHERE x)"), "(none x [1] (where x))");
    assert_eq!(
        expr("single(`a b` IN n.l WHERE `a b` IS NULL)"),
        "(single a b n.l (where (null? a b)))"
    );
    assert_eq!(
        expr("reduce(s = 0, x IN n.l | s + x)"),
        "(reduce s 0 x n.l (map (+ s x)))"
    );
    // A label alternative in reduce's list is written in parentheses; in its `|` part a `|`
    // continues the label expression.
    assert_eq!(
        expr("reduce(s = 0, x IN [n:(A|B)] | s + x)"),
        "(reduce s 0 x [(labeled n (| A B))] (map (+ s x)))"
    );
    // Nested, and inside a list comprehension.
    assert_eq!(
        expr("[y IN n.l WHERE any(x IN y WHERE x > 0) | reduce(t = y, z IN [1] | t)]"),
        "(for y n.l (where (any x y (where (> x 0)))) (map (reduce t y z [1] (map t))))"
    );
    // Span: from the name to `)`.
    let q = ok("MATCH (n) RETURN reduce(s = 0, x IN n.l | s + x) AS r");
    assert_eq!(
        q.text(q.ret.items[0].expr.span),
        "reduce(s = 0, x IN n.l | s + x)"
    );
    // Deep nesting is the depth limit, a tall part the height limit, never a stack overflow.
    for (open, close) in [
        ("reduce(s = 0, x IN ", " | s)"),
        ("reduce(s = ", ", x IN [1] | s)"),
        ("reduce(s = 0, x IN [1] | ", ")"),
        ("all(x IN [1] WHERE ", ")"),
        ("any(x IN ", " WHERE true)"),
    ] {
        let query = format!(
            "MATCH (n) RETURN {}1{}",
            open.repeat(300),
            close.repeat(300)
        );
        assert_eq!(fail(&query).kind, GqlErrorKind::Limit, "{open}");
    }
    let tall = vec!["n.a"; MAX_HEIGHT - 2].join(" OR ");
    ok(&format!("MATCH (n) RETURN all(x IN [1] WHERE {tall})"));
    ok(&format!(
        "MATCH (n) RETURN reduce(s = {tall}, x IN [1] | s)"
    ));
    for query in [
        format!("MATCH (n) RETURN NOT all(x IN [1] WHERE {tall})"),
        format!("MATCH (n) RETURN NOT reduce(s = {tall}, x IN [1] | s)"),
    ] {
        assert_eq!(fail(&query).kind, GqlErrorKind::Limit, "{}", &query[..40]);
    }
    // A missing part is a syntax error naming what is expected.
    for (call, expected) in [
        ("all(x IN n.l)", "WHERE"),
        ("all(x IN n.l | x)", "WHERE"),
        ("any(1 IN n.l WHERE true)", "a variable"),
        ("none(x n.l WHERE true)", "IN"),
        ("reduce(s, x IN n.l | s)", "`=`"),
        ("reduce(s = 0 x IN n.l | s)", "`,`"),
        ("reduce(s = 0, x IN n.l)", "`|`"),
        ("reduce(s = 0, x IN n.l WHERE x > 0 | s)", "`|`"),
        ("single(x IN n.l WHERE true", "`)`"),
    ] {
        let f = fail(&format!("MATCH (n) RETURN {call}"));
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{call}");
        assert!(
            f.expected.iter().any(|e| e == expected),
            "{call}: {:?}",
            f.expected
        );
    }
}

#[test]
fn comprehensions_and_projections_are_bounded() {
    // Nesting: the parser's depth limit, never a stack overflow.
    for (open, close) in [
        ("[x IN ", " | x]"),
        ("[x IN [1] WHERE ", " | x]"),
        ("[(n)-->(m) | ", "]"),
        ("[(n WHERE ", ")-->(m) | m]"),
        ("n {k: ", "}"),
    ] {
        let query = format!(
            "MATCH (n) RETURN {}1{}",
            open.repeat(300),
            close.repeat(300)
        );
        let f = fail(&query);
        assert_eq!(f.kind, GqlErrorKind::Limit, "{open}");
    }
    // Height: a comprehension and a projection are one level taller than their tallest part.
    let tall = vec!["n.a"; MAX_HEIGHT - 2].join(" OR ");
    ok(&format!("MATCH (n) RETURN [x IN [1] WHERE {tall}]"));
    ok(&format!("MATCH (n) RETURN n {{k: {tall}}}"));
    ok(&format!("MATCH (n) RETURN [(n)-->(m) WHERE {tall} | m]"));
    for query in [
        format!("MATCH (n) RETURN NOT [x IN [1] WHERE {tall}]"),
        format!("MATCH (n) RETURN NOT n {{k: {tall}}}"),
        format!("MATCH (n) RETURN NOT [(n)-->(m) WHERE {tall} | m]"),
        format!("MATCH (n) RETURN NOT [(n)-->(m {{k: {tall}}}) | m]"),
    ] {
        let f = fail(&query);
        assert_eq!(f.kind, GqlErrorKind::Limit, "{}", &query[..60]);
    }
}

/// Random expressions over the tokens of the stage 2c forms: the parser never panics, and every
/// accepted pattern comprehension holds a pattern with an edge.
#[test]
fn random_comprehensions_never_panic() {
    const TOKENS: &[&str] = &[
        "[",
        "[",
        "]",
        "]",
        "(",
        ")",
        "{",
        "}",
        "x",
        "n",
        "IN",
        "WHERE",
        "|",
        "|",
        ".",
        ".*",
        ":",
        ",",
        "-->",
        "-",
        "<-",
        "-[e]->",
        "p =",
        "1",
        "n.l",
        ":A",
        "AND",
        ">",
        "x {",
        "{.a}",
        "k:",
        "[x IN",
        "[(n)-->(m)",
    ];
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut accepted = 0;
    for _ in 0..10_000 {
        let len = 1 + (next() % 16) as usize;
        let body: Vec<&str> = (0..len)
            .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
            .collect();
        let query = format!("MATCH (n) RETURN {}", body.join(" "));
        if let Ok(q) = parse(&query) {
            accepted += 1;
            let mut stack: Vec<&Expr> = q.ret.items.iter().map(|i| &i.expr).collect();
            while let Some(e) = stack.pop() {
                match &e.kind {
                    ExprKind::PatternComprehension(c) => {
                        assert!(!c.body.patterns[0].steps.is_empty(), "{query}");
                        stack.push(&c.map);
                    }
                    ExprKind::ListComprehension(c) => {
                        stack.push(&c.list);
                        stack.extend(c.filter.as_ref());
                        stack.extend(c.map.as_ref());
                    }
                    ExprKind::List(items) => stack.extend(items),
                    _ => {}
                }
            }
        }
    }
    assert!(accepted > 0);
}

// ----- stage 2d: UNION, NEXT, CALL { }, COUNT / COLLECT { }, whole-query bodies -----

/// The column texts of a linear query's `RETURN` (alias or expression text).
fn returned(q: &Query, linear: &LinearQuery) -> Vec<String> {
    linear
        .ret
        .items
        .iter()
        .map(|item| match &item.alias {
            Some(alias) => alias.name.clone(),
            None => q.text(item.expr.span).to_string(),
        })
        .collect()
}

/// The `CALL` clause of the stage at `i` of `linear`.
fn call_at(linear: &LinearQuery, i: usize) -> &CallClause {
    match linear.stages.get(i) {
        Some(Stage::Call(call)) => call,
        other => panic!("stage {i} is no CALL: {other:?}"),
    }
}

#[test]
fn a_linear_query_is_one_block_of_one_operand() {
    let q = ok("MATCH (a) RETURN a");
    assert!(q.is_linear());
    assert!(q.body.first.unions.is_empty() && q.body.next.is_empty());
    // The query dereferences to its only linear query.
    assert_eq!(stage_kinds(&q), ["match"]);
    assert_eq!(q.text(q.span), "MATCH (a) RETURN a");
    assert_eq!(q.text(q.body.span), "MATCH (a) RETURN a");
    assert!(q.importing.is_none());
}

#[test]
fn union_operands_and_their_kinds() {
    let q = ok("MATCH (a:person) RETURN a.name AS n \
                UNION MATCH (b:software) RETURN b.name AS n \
                UNION all MATCH (c) RETURN c.name AS n \
                UNION DISTINCT RETURN 'x' AS n;");
    assert!(!q.is_linear());
    let block = &q.body.first;
    let kinds: Vec<UnionKind> = block.unions.iter().map(|u| u.kind).collect();
    assert_eq!(
        kinds,
        [UnionKind::Distinct, UnionKind::All, UnionKind::Distinct]
    );
    let keywords: Vec<&str> = block.unions.iter().map(|u| q.text(u.keyword)).collect();
    assert_eq!(keywords, ["UNION", "UNION all", "UNION DISTINCT"]);
    assert_eq!(returned(&q, &block.first), ["n"]);
    assert_eq!(
        q.text(block.unions[0].query.span),
        "MATCH (b:software) RETURN b.name AS n"
    );
    // Each operand holds its own MATCH clauses; a later one may start without MATCH.
    assert_eq!(block.unions[1].query.matches.len(), 1);
    assert!(block.unions[2].query.stages.is_empty());
    assert!(q.text(q.body.span).ends_with("RETURN 'x' AS n"));
}

#[test]
fn order_by_offset_and_limit_belong_to_their_operand() {
    let q = ok("MATCH (a) RETURN a.x AS x ORDER BY x LIMIT 1 \
                UNION ALL MATCH (b) RETURN b.x AS x SKIP 1");
    let block = &q.body.first;
    assert!(block.first.order_by.is_some() && block.first.limit.is_some());
    assert!(block.first.offset.is_none());
    let second = &block.unions[0].query;
    assert!(second.order_by.is_none() && second.offset.is_some() && second.limit.is_none());
}

#[test]
fn next_binds_looser_than_union() {
    let q = ok("MATCH (a) RETURN a.x AS x UNION MATCH (b) RETURN b.x AS x \
                NEXT RETURN x ORDER BY x \
                NEXT FILTER x > 1 RETURN x UNION ALL RETURN 0 AS x");
    assert_eq!(q.body.first.unions.len(), 1);
    assert_eq!(q.body.next.len(), 2);
    let second = &q.body.next[0];
    assert_eq!(q.text(second.keyword), "NEXT");
    assert!(second.block.unions.is_empty());
    assert!(second.block.first.stages.is_empty());
    assert!(second.block.first.order_by.is_some());
    let third = &q.body.next[1].block;
    assert_eq!(stage_kinds_of(&third.first), ["filter"]);
    assert_eq!(third.unions.len(), 1);
    assert_eq!(third.unions[0].kind, UnionKind::All);
    // Every clause may start the statement after NEXT.
    for after in [
        "MATCH (b) RETURN b",
        "OPTIONAL MATCH (b) RETURN b",
        "WITH a RETURN a",
        "LET b = a RETURN b",
        "FILTER a IS NOT NULL RETURN a",
        "FOR x IN [1] RETURN x",
        "UNWIND [1] AS x RETURN x",
        "CALL { RETURN 1 AS one } RETURN one",
        "OPTIONAL CALL (a) { RETURN a AS b } RETURN b",
        "RETURN a",
    ] {
        let q = ok(&format!("MATCH (a) RETURN a NEXT {after}"));
        assert_eq!(q.body.next.len(), 1, "{after}");
    }
}

/// The stage kinds of one linear query.
fn stage_kinds_of(linear: &LinearQuery) -> Vec<&'static str> {
    linear
        .stages
        .iter()
        .map(|stage| match stage {
            Stage::Match(i) if linear.matches[*i].optional => "optional",
            Stage::Match(_) => "match",
            Stage::With(_) => "with",
            Stage::Let(_) => "let",
            Stage::Filter(_) => "filter",
            Stage::For(_) => "for",
            Stage::Call(_) => "call",
            Stage::Insert(_) => "insert",
            Stage::Set(_) => "set",
            Stage::Remove(_) => "remove",
            Stage::Delete(_) => "delete",
            Stage::Merge(_) => "merge",
        })
        .collect()
}

#[test]
fn call_forms_and_scopes() {
    let q = ok("MATCH (p:person), (s) \
                CALL (p, `s`) { MATCH (p)-[:created]->(x) RETURN count(x) AS made } \
                OPTIONAL CALL () { RETURN 1 AS one } \
                CALL (*) { RETURN p.name AS name } \
                CALL { MATCH (z) RETURN z } \
                RETURN p, made, one, name, z");
    assert_eq!(stage_kinds(&q), ["match", "call", "call", "call", "call"]);
    let first = call_at(&q, 1);
    assert!(first.optional.is_none());
    match &first.scope {
        CallScope::Variables { variables, span } => {
            let names: Vec<&str> = variables.iter().map(|v| v.name.as_str()).collect();
            assert_eq!(names, ["p", "s"]);
            assert_eq!(q.text(*span), "(p, `s`)");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        q.text(first.span),
        "CALL (p, `s`) { MATCH (p)-[:created]->(x) RETURN count(x) AS made }"
    );
    // The body holds its own MATCH clauses; the outer query only its own.
    assert_eq!(first.body.first.first.matches.len(), 1);
    assert_eq!(q.matches.len(), 1);
    let second = call_at(&q, 2);
    assert_eq!(second.optional.map(|s| q.text(s)), Some("OPTIONAL"));
    assert!(q.text(second.span).starts_with("OPTIONAL CALL ()"));
    assert!(
        matches!(&second.scope, CallScope::Variables { variables, .. } if variables.is_empty())
    );
    assert!(matches!(call_at(&q, 3).scope, CallScope::All(span) if q.text(span) == "(*)"));
    assert!(matches!(call_at(&q, 4).scope, CallScope::Implicit));
    assert!(call_at(&q, 4).body.first.first.importing.is_none());
}

#[test]
fn call_bodies_are_whole_queries_and_may_start_a_query() {
    let q = ok("CALL { MATCH (a:person) RETURN a.name AS n \
                UNION MATCH (b:software) RETURN b.name AS n \
                NEXT RETURN n ORDER BY n LIMIT 1 } \
                RETURN n");
    let call = call_at(&q, 0);
    assert_eq!(call.body.first.unions.len(), 1);
    assert_eq!(call.body.next.len(), 1);
    // Nested CALLs.
    let q = ok("MATCH (a) CALL (a) { CALL (a) { RETURN a AS b } RETURN b AS c } RETURN c");
    let inner = call_at(&call_at(&q, 1).body.first.first, 0);
    assert!(
        matches!(&inner.scope, CallScope::Variables { variables, .. } if variables[0].name == "a")
    );
    // A CALL inside an OPTIONAL MATCH flow and after WITH / FOR.
    let q = ok("MATCH (a) OPTIONAL MATCH (a)-->(b) WITH a, b FOR x IN [1] \
                CALL (b) { RETURN b AS c } RETURN c");
    assert_eq!(
        stage_kinds(&q),
        ["match", "optional", "with", "for", "call"]
    );
}

#[test]
fn cypher_importing_with_becomes_the_scope() {
    let q = ok("MATCH (a), (b) CALL { WITH a, b MATCH (a)-->(c) RETURN c \
                UNION WITH * RETURN a AS c UNION RETURN 1 AS c } RETURN c");
    let body = &call_at(&q, 1).body;
    let importing = body
        .first
        .first
        .importing
        .as_ref()
        .unwrap_or_else(|| panic!("no import"));
    let names: Vec<&str> = importing
        .variables
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(names, ["a", "b"]);
    assert!(importing.star.is_none());
    assert_eq!(q.text(importing.span), "WITH a, b");
    // The importing WITH is no clause of the body any more.
    assert_eq!(stage_kinds_of(&body.first.first), ["match"]);
    let star = body.first.unions[0].query.importing.as_ref();
    assert!(star.is_some_and(|i| i.star.is_some() && i.variables.is_empty()));
    assert!(body.first.unions[0].query.stages.is_empty());
    assert!(body.first.unions[1].query.importing.is_none());
    // Not an importing WITH: an alias, an expression, a tail, DISTINCT, `*` with items, a
    // scope clause written, or a WITH after NEXT. Each stays a WITH clause of the body.
    for body in [
        "WITH a AS x RETURN x",
        "WITH a, 1 AS one RETURN a",
        "WITH a WHERE a.x > 1 RETURN a",
        "WITH a ORDER BY a.x LIMIT 1 RETURN a",
        "WITH DISTINCT a RETURN a",
        "WITH *, a.x AS x RETURN x",
        "RETURN a AS b NEXT WITH b RETURN b",
    ] {
        let q = ok(&format!("MATCH (a) CALL {{ {body} }} RETURN 1 AS z"));
        let call = call_at(&q, 1);
        assert!(call.body.first.first.importing.is_none(), "{body}");
        let first = call
            .body
            .next
            .first()
            .map_or(&call.body.first.first, |n| &n.block.first);
        assert_eq!(stage_kinds_of(first), ["with"], "{body}");
    }
    let q = ok("MATCH (a) CALL (a) { WITH a RETURN a AS b } RETURN b");
    let call = call_at(&q, 1);
    assert!(call.body.first.first.importing.is_none());
    assert_eq!(stage_kinds_of(&call.body.first.first), ["with"]);
}

#[test]
fn count_and_collect_subqueries_parse() {
    for (src, kind, matches, query) in [
        ("COUNT { (n)-->() }", SubqueryKind::Count, 1, false),
        (
            "count { MATCH (n)-->(m) WHERE m.x > 1 }",
            SubqueryKind::Count,
            1,
            false,
        ),
        (
            "COUNT { MATCH (n)-->(m) RETURN m }",
            SubqueryKind::Count,
            0,
            true,
        ),
        (
            "COLLECT { MATCH (n)-->(m) RETURN m.name }",
            SubqueryKind::Collect,
            0,
            true,
        ),
        (
            "collect { MATCH (n)-->(m) }",
            SubqueryKind::Collect,
            1,
            false,
        ),
        (
            "COLLECT { MATCH (n)-->(m) RETURN m.x AS x UNION MATCH (n)<--(m) RETURN m.x AS x }",
            SubqueryKind::Collect,
            0,
            true,
        ),
        (
            "EXISTS { MATCH (n)-->(m) WITH m WHERE m.x > 1 RETURN m }",
            SubqueryKind::Exists,
            0,
            true,
        ),
        (
            "EXISTS { OPTIONAL MATCH (n)-->(m) RETURN m }",
            SubqueryKind::Exists,
            0,
            true,
        ),
        ("EXISTS { RETURN 1 }", SubqueryKind::Exists, 0, true),
        (
            "EXISTS { MATCH (n) CALL (n) { RETURN 1 AS one } RETURN one }",
            SubqueryKind::Exists,
            0,
            true,
        ),
        // `WITH` after STARTS / ENDS is an operator, not a clause: a body of MATCH clauses.
        (
            "EXISTS { MATCH (n) WHERE n.name STARTS WITH 'a' }",
            SubqueryKind::Exists,
            1,
            false,
        ),
        (
            "EXISTS { MATCH (n) WHERE n.name ENDS WITH 'a' }",
            SubqueryKind::Exists,
            1,
            false,
        ),
        // A nested body's clauses do not make the outer body a whole query.
        (
            "EXISTS { MATCH (n) WHERE EXISTS { MATCH (n) RETURN n } }",
            SubqueryKind::Exists,
            1,
            false,
        ),
    ] {
        let s = subquery(src);
        assert_eq!(s.kind, kind, "{src}");
        assert_eq!(s.form, SubqueryForm::Braces, "{src}");
        assert_eq!(s.matches.len(), matches, "{src}");
        assert_eq!(s.query.is_some(), query, "{src}");
    }
    let q = ok("MATCH (n) RETURN COUNT { (n)-->() } AS c");
    let ExprKind::Subquery(s) = &q.ret.items[0].expr.kind else {
        panic!("not a subquery");
    };
    assert_eq!(q.text(s.span), "COUNT { (n)-->() }");
    // `count(..)` stays the aggregate, `count` a variable elsewhere.
    let q = ok("MATCH (n) RETURN count(n) AS c");
    assert!(matches!(
        q.ret.items[0].expr.kind,
        ExprKind::FunctionCall { .. }
    ));
    let q = ok("MATCH (count) RETURN count.x");
    assert!(matches!(
        q.ret.items[0].expr.kind,
        ExprKind::Property { .. }
    ));
}

#[test]
fn whole_query_body_errors() {
    // A body of other clauses than MATCH needs RETURN, named at its first such clause.
    for (query, text) in [
        (
            "MATCH (n) WHERE COUNT { MATCH (n) WITH n } > 1 RETURN n",
            "WITH",
        ),
        (
            "MATCH (n) RETURN COLLECT { OPTIONAL MATCH (n)-->(m) } AS l",
            "OPTIONAL MATCH",
        ),
        (
            "MATCH (n) WHERE EXISTS { MATCH (n) FOR x IN [1] } RETURN n",
            "FOR",
        ),
        (
            "MATCH (n) WHERE EXISTS { CALL { RETURN 1 AS o } } RETURN n",
            "CALL",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains("without RETURN"), "{}", f.message);
    }
    let f = fail("MATCH (n) WHERE COUNT { MATCH (n) WITH n } > 1 RETURN n");
    assert!(f.message.contains("body of COUNT { }"), "{}", f.message);
    // After RETURN inside a body: the tail, then `}`.
    let f = fail("MATCH (n) WHERE EXISTS { MATCH (n) RETURN n x } RETURN n");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "x");
    assert!(f.expected.contains(&"`}`".to_string()), "{:?}", f.expected);
    assert!(!f.expected.contains(&"end of input".to_string()));
    // A later operand of a body needs RETURN.
    let f = fail("MATCH (n) WHERE EXISTS { RETURN 1 AS x UNION MATCH (m) } RETURN n");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert!(f.message.contains("missing RETURN"), "{}", f.message);
    // COUNT ( ) is the function, and COUNT { needs a body.
    let f = fail("MATCH (n) WHERE COUNT { } > 1 RETURN n");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
}

#[test]
fn composite_and_call_errors() {
    // UNION / NEXT before a RETURN.
    for (query, text) in [
        ("MATCH (p) UNION MATCH (q) RETURN q", "UNION"),
        ("UNION MATCH (q) RETURN q", "UNION"),
        ("MATCH (p) NEXT MATCH (q) RETURN q", "NEXT"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("must follow a complete query"),
            "{}",
            f.message
        );
        assert!(f.alternative.unwrap_or_default().contains("RETURN"));
    }
    // A missing operand.
    let f = fail("MATCH (p) RETURN p UNION");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.expected, NEXT_CLAUSES);
    // NEXT YIELD, EXCEPT, INTERSECT: not yet.
    for (query, text) in [
        ("MATCH (p) RETURN p NEXT YIELD p RETURN p", "NEXT YIELD"),
        ("MATCH (n) RETURN n EXCEPT MATCH (m) RETURN m", "EXCEPT"),
        (
            "MATCH (n) RETURN n INTERSECT MATCH (m) RETURN m",
            "INTERSECT",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}");
        assert_eq!(f.text, text, "{query}");
    }
    // IN TRANSACTIONS: refused, a write form.
    for (query, text) in [
        (
            "MATCH (p) CALL (p) { RETURN 1 AS x } IN TRANSACTIONS RETURN p",
            "IN TRANSACTIONS",
        ),
        (
            "MATCH (p) CALL { RETURN 1 AS x } IN 3 CONCURRENT TRANSACTIONS OF 10 ROWS RETURN p",
            "IN 3 CONCURRENT TRANSACTIONS",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Refused, "{query}");
        assert_eq!(f.gqlstatus, "0A000");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains("IN TRANSACTIONS"), "{}", f.message);
    }
    // A body without RETURN: refused (a unit subquery writes); an empty body: syntax.
    let f = fail("MATCH (p) CALL (p) { MATCH (p)-->(x) } RETURN p");
    assert_eq!(f.kind, GqlErrorKind::Refused);
    assert_eq!(f.text, "}");
    assert!(f.message.contains("without RETURN"), "{}", f.message);
    let f = fail("MATCH (p) CALL { } RETURN p");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "}");
    assert!(f.message.contains("empty"), "{}", f.message);
    // Procedure calls stay refused, OPTIONAL CALL included.
    for query in [
        "CALL db.labels()",
        "MATCH (n) OPTIONAL CALL db.x(n) RETURN n",
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Refused, "{query}");
        assert_eq!(f.text, "CALL", "{query}");
    }
    // The scope clause takes variables or `*`.
    for (query, text, expected) in [
        (
            "MATCH (p) CALL (1) { RETURN 1 AS x } RETURN x",
            "1",
            vec!["a variable", "`*`", "`)`"],
        ),
        (
            "MATCH (p) CALL (p,) { RETURN 1 AS x } RETURN x",
            ")",
            vec!["a variable"],
        ),
        (
            "MATCH (p) CALL (p q) { RETURN 1 AS x } RETURN x",
            "q",
            vec!["`,`", "`)`"],
        ),
        (
            "MATCH (p) CALL (*, p) { RETURN 1 AS x } RETURN x",
            ",",
            vec!["`)`"],
        ),
        ("MATCH (p) CALL (p) RETURN p", "RETURN", vec!["`{`"]),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.expected, expected, "{query}");
    }
    // An unclosed body; a token after the body's RETURN.
    let f = fail("MATCH (p) CALL { RETURN 1 AS x");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert!(f.expected.contains(&"`}`".to_string()), "{:?}", f.expected);
    // A CALL after RETURN.
    let f = fail("MATCH (p) RETURN p CALL { RETURN 1 AS x }");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert!(f.message.contains("CALL after RETURN"), "{}", f.message);
    // Inside an expression, CALL is no subquery.
    let f = fail("MATCH (p) RETURN CALL { RETURN 1 AS x }");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "CALL");
}

#[test]
fn nested_bodies_are_bounded() {
    // Nesting: the parser's depth limit, never a stack overflow.
    let mut call = "RETURN 1 AS x".to_string();
    let mut exists = "(n)-->()".to_string();
    let mut count = "(n)-->()".to_string();
    for _ in 0..300 {
        call = format!("CALL {{ {call} }} RETURN x");
        exists = format!("EXISTS {{ MATCH (n) WHERE {exists} RETURN n }}");
        count = format!("COUNT {{ MATCH (n) WITH n WHERE {count} > 0 RETURN n }}");
    }
    let deep = format!("{}1{}", "[x IN ".repeat(300), " | x]".repeat(300));
    for query in [
        call,
        format!("MATCH (n) WHERE {exists} RETURN n"),
        format!("MATCH (n) WHERE {count} > 0 RETURN n"),
        format!("RETURN 1 AS x UNION RETURN {deep} AS x"),
        format!("RETURN 1 AS x NEXT RETURN {deep} AS x"),
        format!("CALL {{ RETURN {deep} AS x }} RETURN x"),
    ] {
        let f = fail(&query);
        assert_eq!(f.kind, GqlErrorKind::Limit, "{}", &query[..40]);
    }
    // A few levels parse.
    ok("CALL { CALL { CALL { RETURN 1 AS x } RETURN x } RETURN x } RETURN x");
}

/// Random token sequences over the stage 2d forms: the parser never panics.
#[test]
fn random_composite_queries_never_panic() {
    const TOKENS: &[&str] = &[
        "MATCH (a)",
        "MATCH (a)-->(b)",
        "OPTIONAL",
        "CALL",
        "CALL (a)",
        "CALL (*)",
        "CALL ()",
        "{",
        "}",
        "(",
        ")",
        "RETURN a",
        "RETURN 1 AS x",
        "RETURN *",
        "UNION",
        "UNION ALL",
        "UNION DISTINCT",
        "NEXT",
        "YIELD",
        "WITH a",
        "WITH *",
        "FILTER a",
        "LET b = 1",
        "FOR x IN [1]",
        "IN",
        "TRANSACTIONS",
        "CONCURRENT",
        "EXISTS",
        "COUNT",
        "COLLECT",
        "WHERE",
        "ORDER BY x",
        "LIMIT 1",
        ",",
        ";",
        "a",
        "1",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for i in 0..4_000 {
        let len = rng.below(40);
        let mut query: Vec<&str> = (0..len).map(|_| TOKENS[rng.below(TOKENS.len())]).collect();
        if i % 2 == 0 {
            query.insert(0, "MATCH (a)");
        }
        let text = query.join(" ");
        let _ = parse(&text);
        if let Ok(parsed) = parse(&text) {
            // Every operand of an accepted query ends in RETURN.
            let block = &parsed.body.first;
            assert!(
                std::iter::once(&block.first)
                    .chain(block.unions.iter().map(|u| &u.query))
                    .all(|q| !q.ret.items.is_empty() || q.ret.star.is_some()),
                "{text}"
            );
        }
    }
}

/// `OTHERWISE` (ISO GQL's conditional query) is "not yet" like `EXCEPT` / `INTERSECT`, in any
/// case, and stays a name where a name stands.
#[test]
fn otherwise_is_not_yet_supported() {
    for (query, text) in [
        (
            "MATCH (n) RETURN n OTHERWISE MATCH (m) RETURN m",
            "OTHERWISE",
        ),
        (
            "MATCH (n) RETURN n otherwise MATCH (m) RETURN m",
            "otherwise",
        ),
        (
            "MATCH (n:a) RETURN n.x AS x OTHERWISE RETURN 1 AS x",
            "OTHERWISE",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}");
        assert_eq!(f.gqlstatus, "0A000", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("OTHERWISE (a conditional query)"),
            "{}",
            f.message
        );
        assert!(
            f.alternative.unwrap_or_default().contains("coalesce()"),
            "{query}"
        );
    }
    ok("MATCH (otherwise) RETURN otherwise.name AS otherwise");
}

// ----- stage 2e: parenthesized path patterns -----

/// `(min, max)` of a parenthesized path pattern's quantifier.
fn bounds(group: &ParenthesizedPath) -> Option<(u64, Option<u64>)> {
    group.quantifier.map(|q| (q.min, q.max))
}

/// The variable names of a path's nodes and connectors in text order (`_` anonymous, `.`
/// implicit, a parenthesized path pattern as `(body){min,max}`).
fn path_shape(path: &PathPattern) -> String {
    fn node(n: &NodePattern) -> String {
        if n.is_implicit() {
            return ".".into();
        }
        n.variable.as_ref().map_or("_".into(), |v| v.name.clone())
    }
    fn chain(out: &mut String, steps: &[PatternStep]) {
        for step in steps {
            out.push('-');
            out.push_str(step.edge.variable.as_ref().map_or("_", |v| v.name.as_str()));
            out.push('-');
            out.push_str(&node(&step.node));
        }
    }
    let mut out = node(&path.start);
    chain(&mut out, &path.steps);
    for group in &path.groups {
        out.push_str(" (");
        out.push_str(&node(&group.group.start));
        chain(&mut out, &group.group.steps);
        if group.group.where_clause.is_some() {
            out.push_str(" WHERE");
        }
        out.push(')');
        match bounds(&group.group) {
            Some((min, Some(max))) => out.push_str(&format!("{{{min},{max}}}")),
            Some((min, None)) => out.push_str(&format!("{{{min},}}")),
            None => {}
        }
        out.push(' ');
        out.push_str(&node(&group.node));
        chain(&mut out, &group.steps);
    }
    out
}

#[test]
fn parenthesized_path_pattern_forms() {
    let q = ok("MATCH (a) ((x)-[e:knows]->(y) WHERE x.age < y.age){1,3} (b) RETURN b");
    let path = &q.matches[0].patterns[0];
    assert_eq!(path_shape(path), "a (x-e-y WHERE){1,3} b");
    let group = &path.groups[0];
    assert_eq!(
        q.text(group.group.span),
        "((x)-[e:knows]->(y) WHERE x.age < y.age)"
    );
    assert_eq!(q.text(group.group.quantifier.unwrap().span), "{1,3}");
    assert!(!group.group.quantifier.unwrap().cypher);
    assert_eq!(
        group.group.steps[0].edge.labels.as_ref().map(label),
        Some("knows".into())
    );
    assert_eq!(q.text(group.node.span), "(b)");
    assert_eq!(
        q.text(path.span),
        "(a) ((x)-[e:knows]->(y) WHERE x.age < y.age){1,3} (b)"
    );
    // Every quantifier form, `?` and none.
    for (quantifier, expected) in [
        ("{2}", Some((2, Some(2)))),
        ("{2,}", Some((2, None))),
        ("{,3}", Some((0, Some(3)))),
        ("{,}", Some((0, None))),
        ("{0}", Some((0, Some(0)))),
        ("*", Some((0, None))),
        ("+", Some((1, None))),
        ("?", Some((0, Some(1)))),
        ("", None),
    ] {
        let query = format!("MATCH (a) ((x)-->(y)){quantifier} (b) RETURN b");
        let q = ok(&query);
        let group = &q.matches[0].patterns[0].groups[0].group;
        assert_eq!(bounds(group), expected, "{query}");
        if let Some(written) = group.quantifier {
            assert_eq!(q.text(written.span), quantifier, "{query}");
        }
    }
    // Implicit nodes where the text has none: at either end, between an edge and a group,
    // between two groups.
    for (pattern, shape) in [
        ("((x)-->(y)){2}", ". (x-_-y){2,2} ."),
        ("(a)-->((x)-->(y))+", "a-_-. (x-_-y){1,} ."),
        ("((x)-->(y))+-->(b)", ". (x-_-y){1,} .-_-b"),
        (
            "((a)-->(b))* ((c)-->(d))? (e)",
            ". (a-_-b){0,} . (c-_-d){0,1} e",
        ),
        (
            "(a) ((x)-[:k]->(m)<-[f]-(y)){1,2} (b)-[:k]->(c)",
            "a (x-_-m-f-y){1,2} b-_-c",
        ),
        ("(a)-[e]->{1,3}((x)-->(y))", "a-e-. (x-_-y) ."),
        ("(a) ((x)-->(y) WHERE x.v > 1) (b)", "a (x-_-y WHERE) b"),
    ] {
        let q = ok(&format!("MATCH {pattern} RETURN 1 AS one"));
        let path = &q.matches[0].patterns[0];
        assert_eq!(path_shape(path), shape, "{pattern}");
        assert_eq!(q.text(path.span), pattern);
        // An implicit node spans nothing, at the start of the token after it.
        for n in std::iter::once(&path.start).chain(path.groups.iter().map(|g| &g.node)) {
            assert_eq!(n.is_implicit(), q.text(n.span).is_empty(), "{pattern}");
        }
    }
    let q = ok("MATCH ((x)-->(y)){2} RETURN x");
    let path = &q.matches[0].patterns[0];
    assert_eq!(
        (
            path.start.span.start,
            path.start.span.line,
            path.start.span.column
        ),
        (6, 1, 7)
    );
    assert_eq!(path.groups[0].node.span.start, 21);
    // A path variable on the whole path, several patterns, OPTIONAL MATCH, EXISTS bodies.
    let q = ok("MATCH p = (a) ((x)-->(y)){1,2} (b), (b)-->(c) RETURN p");
    let paths = &q.matches[0].patterns;
    assert_eq!(q.text(paths[0].span), "p = (a) ((x)-->(y)){1,2} (b)");
    assert!(paths[1].groups.is_empty());
    let q = ok("MATCH (a) OPTIONAL MATCH (a) ((x)-->(y))+ (b) RETURN b");
    assert_eq!(q.matches[1].patterns[0].groups.len(), 1);
    for body in [
        "EXISTS { ((x)-->(y)){2} }",
        "EXISTS { MATCH (n) ((x)-->(y)){2} (m) }",
        "EXISTS { MATCH (n) ((x)-->(y)){2} (m) RETURN m }",
        "COUNT { (n) ((x)-->(y))* (m) }",
    ] {
        ok(&format!("MATCH (n) WHERE {body} RETURN n"));
    }
    // Node patterns that start like a prefix or a path variable stay node patterns.
    let q = ok("MATCH (trail:person), (any WHERE any.x > 1), (walk), (`all` IS x) RETURN trail");
    assert!(q.matches[0].patterns.iter().all(|p| p.groups.is_empty()));
    // The 1b quantified edge and plain paths keep their tree.
    let q = ok("MATCH (a)-[:knows]->{1,3}(b)-[e*2]->(c) RETURN b");
    let path = &q.matches[0].patterns[0];
    assert!(path.groups.is_empty());
    assert_eq!(path.steps.len(), 2);
    assert!(path.steps.iter().all(|s| s.edge.quantifier.is_some()));
}

#[test]
fn parenthesized_path_pattern_refusals() {
    // E17: each "not yet supported" (0A000) at the construct, with an alternative.
    let cases: &[(&str, &str, &str)] = &[
        (
            "MATCH (((a)-->(b)){2}) RETURN a",
            "(",
            "MATCH (a) ((x)-->(y)-->(z)){1,3} (b)",
        ),
        (
            "MATCH ((a)-->(b) ((c)-->(d)){2}) RETURN a",
            "(",
            "outer one",
        ),
        ("MATCH ((a)-->((c)-->(d)){2}) RETURN a", "(", "outer one"),
        (
            "MATCH ((a)-->{2}(b)){2} RETURN a",
            "{2}",
            "((x)-->()-->(y)){1,3}",
        ),
        (
            "MATCH ((a)-[*2]->(b)){2} RETURN a",
            "*2",
            "-[:knows]->{1,3}",
        ),
        ("MATCH ((a)-->+(b)) RETURN a", "+", "((x)-->()-->(y)){1,3}"),
        (
            "MATCH (p = (a)-->(b)){2} RETURN a",
            "p =",
            "MATCH p = (a) ((x)-->(y)){1,3} (b) RETURN p",
        ),
        ("MATCH (`p` = (a)-->(b)){2} RETURN a", "`p` =", "MATCH p ="),
        (
            "MATCH (TRAIL (a)-->(b)){2} RETURN a",
            "TRAIL (",
            "DIFFERENT EDGES",
        ),
        (
            "MATCH (any shortest (a)-->(b)) RETURN a",
            "any shortest",
            "without it",
        ),
        (
            "MATCH (ACYCLIC PATH (a)-->(b)) RETURN a",
            "ACYCLIC PATH",
            "without it",
        ),
        (
            "MATCH ((a)-->(b), (c)-->(d)){2} RETURN a",
            ",",
            "(b), (b)-->(c)",
        ),
        ("MATCH ((a)-->(b) | (c)-->(d)){2} RETURN a", "|", "UNION"),
        (
            "MATCH ((a)-->(b) |+| (c)-->(d)){2} RETURN a",
            "|",
            "[:knows|created]",
        ),
        (
            "MATCH ((a) WHERE a.x = 1){2} RETURN a",
            "((a) WHERE a.x = 1)",
            "(a WHERE a.age > 30)",
        ),
        ("MATCH (n) ((a)) (m) RETURN n", "((a))", "((x)-->(y)){1,3}"),
        (
            "MATCH (n) WHERE EXISTS { ((n)-[*1..2]->()){2} } RETURN n",
            "*1..2",
            "{1,3}",
        ),
    ];
    for (query, text, alternative) in cases {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "0A000", "{query}");
        assert_eq!(&f.text, text, "{query}");
        assert!(
            f.message.contains("parenthesized path pattern")
                && f.message.contains("not yet supported"),
            "{query}: {}",
            f.message
        );
        let given = f.alternative.clone().unwrap_or_default();
        assert!(given.contains(alternative), "{query}: {given}");
    }
}

#[test]
fn parenthesized_path_pattern_syntax_errors() {
    let f = fail("MATCH ((a)-->(b)){2}{3} RETURN a");
    assert_eq!((f.kind, f.text.as_str()), (GqlErrorKind::Syntax, "{"));
    assert!(f.message.contains("takes one quantifier"), "{}", f.message);
    let f = fail("MATCH ((a)-->(b))+? RETURN a");
    assert_eq!((f.kind, f.text.as_str()), (GqlErrorKind::Syntax, "?"));
    let f = fail("MATCH ((a)-->(b)){3,1} RETURN a");
    assert_eq!((f.kind, f.text.as_str()), (GqlErrorKind::Syntax, "{3,1}"));
    assert_eq!(
        f.alternative.as_deref(),
        Some("write the smaller bound first, for example {1,3}")
    );
    let f = fail("MATCH ((a)-->(b)){1 RETURN a");
    assert_eq!(f.expected, ["`,`", "`}`"]);
    let f = fail("MATCH ((a)-->(b) x) RETURN a");
    assert_eq!(f.expected, ["an edge", "WHERE", "`)`"]);
    let f = fail("MATCH ((a)-->(b) WHERE a.x = 1 x) RETURN a");
    assert_eq!(f.expected, ["`)`"]);
    let f = fail("MATCH ((a)-->(b) RETURN a");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    let f = fail("MATCH ((a)-->) RETURN a");
    assert_eq!(f.expected, ["`{`", "`*`", "`+`", "`(`"]);
    // `?` quantifies a parenthesized path pattern only.
    let f = fail("MATCH (a)?-->(b) RETURN a");
    assert!(
        f.message
            .contains("a quantifier cannot follow a node pattern"),
        "{}",
        f.message
    );
    let f = fail("MATCH (a)-[e]->?(b) RETURN a");
    assert_eq!(
        (f.text.as_str(), f.expected.clone()),
        (
            "?",
            vec!["`{`".into(), "`*`".into(), "`+`".into(), "`(`".into()]
        )
    );
    let f = fail("RETURN 1 ? 2");
    assert_eq!((f.kind, f.text.as_str()), (GqlErrorKind::Syntax, "?"));
}

#[test]
fn parenthesized_path_patterns_are_bounded() {
    // Each parenthesized path pattern is one MAX_DEPTH level: nesting through body conditions
    // and EXISTS stops at the depth limit, never with a stack overflow.
    let nest = |n: usize| {
        let mut inner = "(n)-->()".to_string();
        for _ in 0..n {
            inner = format!("(n) ((x WHERE EXISTS {{ {inner} }})-->(y)){{1,2}} (m)");
        }
        format!("MATCH {inner} RETURN 1 AS one")
    };
    // Three levels per round: the group, the EXISTS and the pattern of its body.
    ok(&nest(MAX_DEPTH / 3 - 1));
    for n in [MAX_DEPTH / 3 + 1, 300] {
        let f = fail(&nest(n));
        assert_eq!(f.kind, GqlErrorKind::Limit, "{n}");
        assert_eq!(f.gqlstatus, "54001");
    }
    // Through the body's own WHERE.
    let mut inner = "(n)-->()".to_string();
    for _ in 0..300 {
        inner = format!("((x)-->(y) WHERE EXISTS {{ {inner} }})+");
    }
    let f = fail(&format!("MATCH {inner} RETURN 1 AS one"));
    assert_eq!(f.kind, GqlErrorKind::Limit);
}

#[test]
fn never_panics_on_mutated_parenthesized_path_patterns() {
    const SEEDS: &[&str] = &[
        "MATCH (a) ((x)-[e:knows]->(y) WHERE x.age < y.age){1,3} (b) RETURN b",
        "MATCH p = ((x)-->(y))+ (b)-[f]->((c)<-[:k]-(d))? RETURN p",
        "MATCH (n) WHERE EXISTS { (n) ((x)-->(y)){,2} } RETURN n",
        "MATCH ((a) WHERE a.x)* , (p = (a)-->(b)){2} RETURN a",
    ];
    const PIECES: &[&str] = &[
        "(", ")", "((", "))", "{", "}", "?", "*", "+", "{1,2}", "{0}", "-->", "<-[e]-", "|", ",",
        "WHERE", "p =", "TRAIL", "(x)", " ",
    ];
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for seed in SEEDS {
        check(seed);
        for _ in 0..500 {
            let mut text = seed.to_string();
            for _ in 0..1 + rng.below(3) {
                let mut at = rng.below(text.len() + 1);
                while !text.is_char_boundary(at) {
                    at -= 1;
                }
                match rng.below(3) {
                    0 if at < text.len() => {
                        let mut end = at + 1;
                        while !text.is_char_boundary(end) {
                            end += 1;
                        }
                        text.replace_range(at..end, "");
                    }
                    _ => text.insert_str(at, PIECES[rng.below(PIECES.len())]),
                }
            }
            check(&text);
        }
    }
}

// ----- stage 2e.T5: parenthesized path patterns where an expression stands -----

/// The path pattern of the subquery `src` stands for (a pattern predicate or a pattern
/// comprehension, the first RETURN item of `MATCH (n) RETURN <src>`), as [`path_shape`].
fn expression_path_shape(src: &str) -> String {
    let query = ok(&format!("MATCH (n) RETURN {src}"));
    match &query.ret.items[0].expr.kind {
        ExprKind::Subquery(s) => path_shape(&s.matches[0].patterns[0]),
        ExprKind::PatternComprehension(c) => path_shape(&c.body.patterns[0]),
        other => panic!("{src}: neither a subquery nor a comprehension: {other:?}"),
    }
}

#[test]
fn parenthesized_path_patterns_in_pattern_predicates() {
    // A juxtaposed group after a node, a leading group, a group after an edge, every
    // quantifier form: a pattern predicate.
    for (src, shape) in [
        ("(n) ((x)-->(y)){2}", "n (x-_-y){2,2} ."),
        ("(n) (()-[:knows]->()){1,3} (m)", "n (_-_-_){1,3} m"),
        ("(n) ((x)-->(y))", "n (x-_-y) ."),
        ("(n) ((x)-->(y))? (m)", "n (x-_-y){0,1} m"),
        ("((x)-->(y)){2} (n)", ". (x-_-y){2,2} n"),
        ("((x)-->(y))? (n)", ". (x-_-y){0,1} n"),
        ("((x)-->(y) WHERE x.a > 1){1,2}", ". (x-_-y WHERE){1,2} ."),
        ("((x)-[e]-(y)) (n)", ". (x-e-y) n"),
        ("((x)-->(y))-->(n)", ". (x-_-y) .-_-n"),
        ("((x)-->(y))+ -->(n)", ". (x-_-y){1,} .-_-n"),
        ("((x)-->(y))* (n)", ". (x-_-y){0,} n"),
        ("(n)-->((x)-->(y)){2}", "n-_-. (x-_-y){2,2} ."),
        ("(n)-((x)-->(y)){2}", "n-_-. (x-_-y){2,2} ."),
        (
            "(n) ((x)-->(y)){2} ((u)<--(v))+ (m)",
            "n (x-_-y){2,2} . (u-_-v){1,} m",
        ),
    ] {
        assert_eq!(expression_path_shape(src), shape, "{src}");
        assert!(expr(src).starts_with("(pattern "), "{src}: {}", expr(src));
    }
    // In a condition, with the other operators around it.
    assert_eq!(
        expr("(n) (()-->()){2} AND NOT ((a)-->(b))? (n) OR true"),
        "(or (and (pattern (match 1)) (not (pattern (match 1)))) true)"
    );
    assert_eq!(
        expr("((x)-->(y))+ AND true"),
        "(and (pattern (match 1)) true)"
    );
    // A parenthesized pattern predicate without what only a group continues with stays one, and
    // arithmetic on parentheses stays arithmetic.
    for (src, tree) in [
        ("((n)-->(m))", "(pattern (match 1))"),
        ("((n)-->(m)) * 2", "(* (pattern (match 1)) 2)"),
        ("((n)-->(m)) + [1]", "(+ (pattern (match 1)) [1])"),
        ("((n)-->(m)) + x", "(+ (pattern (match 1)) x)"),
        ("((n)-->(m)) + -1", "(+ (pattern (match 1)) -1)"),
        ("((n)-->(m)) = true", "(= (pattern (match 1)) true)"),
        ("((n) - (m)) * (k)", "(* (- n m) k)"),
        ("((n)) * (m)", "(* n m)"),
        ("((1 + 2)) * 3", "(* (+ 1 2) 3)"),
        ("(n) - ((m) - (k))", "(- n (- m k))"),
    ] {
        assert_eq!(expr(src), tree, "{src}");
    }
    // Pattern after the operator: `+` / `*` followed by a node reads as a quantifier.
    assert_eq!(
        expression_path_shape("((n)-->(m)) + (k)"),
        ". (n-_-m){1,} k"
    );
    // Errors stay where they were: a body without an edge, a nested group (E17).
    let f = fail("MATCH (n) WHERE (n) ((x)){2} RETURN n");
    assert_eq!(f.gqlstatus, "0A000", "{}", f.message);
    let f = fail("MATCH (n) WHERE (n) (((x)-->(y)){2}){2} RETURN n");
    assert_eq!(f.gqlstatus, "0A000", "{}", f.message);
}

#[test]
fn parenthesized_path_patterns_in_pattern_comprehensions() {
    for (src, shape) in [
        ("[(n) ((x)-->(y)){1,2} | y]", "n (x-_-y){1,2} ."),
        (
            "[(n) ((x)-->(y)){1,2} (m) WHERE m.a > 1 | m]",
            "n (x-_-y){1,2} m",
        ),
        ("[((x)-->(y)){2} | x]", ". (x-_-y){2,2} ."),
        ("[((x)-->(y))? (m) | m]", ". (x-_-y){0,1} m"),
        ("[((x)-->(y))+ | 1]", ". (x-_-y){1,} ."),
        ("[((x)-->(y))* WHERE true | 1]", ". (x-_-y){0,} ."),
        ("[(n)-->((x)-->(y)){2} | y]", "n-_-. (x-_-y){2,2} ."),
        (
            "[(n) ((x)-->(y)) ((u)<--(v)){2} | v]",
            "n (x-_-y) . (u-_-v){2,2} .",
        ),
        ("[p = (n) ((x)-->(y)){1,2} | p]", "n (x-_-y){1,2} ."),
        ("[p = ((x)-->(y)){1,2} (m) | p]", ". (x-_-y){1,2} m"),
    ] {
        assert_eq!(expression_path_shape(src), shape, "{src}");
        assert!(expr(src).starts_with("(collect "), "{src}: {}", expr(src));
    }
    // Without `WHERE` or `|` the bracket stays a list holding a pattern predicate.
    assert_eq!(expr("[(n) ((x)-->(y)){2}]"), "[(pattern (match 1))]");
    assert_eq!(expr("[((x)-->(y)){2} (n), 1]"), "[(pattern (match 1)) 1]");
    assert_eq!(expr("[((n)-->(m))]"), "[(pattern (match 1))]");
}

#[test]
fn parenthesized_path_patterns_in_expressions_are_bounded() {
    // Nesting through a pattern predicate in a body node's WHERE and through comprehensions:
    // the depth limit, never a stack overflow, and the lookahead stays a forward scan.
    let mut predicate = "(n)-->()".to_string();
    let mut comprehension = "1".to_string();
    for _ in 0..300 {
        predicate = format!("(n) ((x WHERE {predicate})-->(y)){{1,2}}");
        comprehension = format!("[(n) ((x)-->(y)){{2}} | {comprehension}]");
    }
    for query in [
        format!("MATCH (n) WHERE {predicate} RETURN n"),
        format!("MATCH (n) RETURN {comprehension} AS l"),
    ] {
        let f = fail(&query);
        assert_eq!(f.kind, GqlErrorKind::Limit, "{}", &query[..60]);
    }
    // Long unclosed or unbalanced input ends the scan.
    for query in [
        format!("MATCH (n) WHERE {} RETURN n", "((".repeat(5000)),
        format!("MATCH (n) WHERE (n) {}", "((x)-->(y)){2} ".repeat(2000)),
        format!("MATCH (n) RETURN [{}", "((x)-->(y))+ ".repeat(2000)),
    ] {
        let _ = fail(&query);
    }
}

// ----- stage 3a: INSERT / CREATE, FINISH, queries ending in a write -----

/// The only stage of `q` as an INSERT clause.
fn insert_of(linear: &LinearQuery, index: usize) -> &InsertClause {
    match linear.stages.get(index) {
        Some(Stage::Insert(clause)) => clause,
        other => panic!("stage {index} is no INSERT: {other:?}"),
    }
}

/// The label names of a conjunction, in written order.
fn conjunction(labels: &LabelExpr) -> Vec<&str> {
    match &labels.kind {
        LabelExprKind::Label(name) => vec![name.name.as_str()],
        LabelExprKind::And(left, right) => {
            let mut names = conjunction(left);
            names.extend(conjunction(right));
            names
        }
        other => panic!("not a conjunction: {other:?}"),
    }
}

#[test]
fn insert_and_create_forms() {
    let q = ok("INSERT (n:person {name: 'ann', age: 30})");
    assert_eq!(stage_kinds(&q), ["insert"]);
    assert_eq!(q.end(), QueryEnd::Write);
    let insert = insert_of(&q, 0);
    assert!(!insert.cypher);
    assert_eq!(q.text(insert.keyword), "INSERT");
    assert_eq!(
        q.text(insert.span),
        "INSERT (n:person {name: 'ann', age: 30})"
    );
    let node = &insert.patterns[0].start;
    assert_eq!(node.variable.as_ref().map(|v| v.name.as_str()), Some("n"));
    assert_eq!(conjunction(node.labels.as_ref().unwrap()), ["person"]);
    let keys: Vec<&str> = node
        .properties
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .map(|e| e.key.name.as_str())
        .collect();
    assert_eq!(keys, ["name", "age"]);
    // CREATE is the Cypher alias: the same tree, marked.
    let q = ok("create (n:person {name: 'ann', age: 30})");
    let create = insert_of(&q, 0);
    assert!(create.cypher);
    assert_eq!(q.text(create.keyword), "create");
    // Label conjunctions: `&`, Cypher `:A:B`, the IS form, parentheses.
    let q = ok("INSERT (a:A&B), (b IS C & D), (c:E:F), (d:(G&H)), ()");
    let insert = insert_of(&q, 0);
    assert_eq!(insert.patterns.len(), 5);
    let names: Vec<Vec<&str>> = insert.patterns[..4]
        .iter()
        .map(|p| conjunction(p.start.labels.as_ref().unwrap()))
        .collect();
    assert_eq!(names, [["A", "B"], ["C", "D"], ["E", "F"], ["G", "H"]]);
    assert_eq!(q.text(insert.patterns[4].span), "()");
    assert!(insert.patterns[4].start.labels.is_none());
    // Edges: both directions, a variable, one label, properties; a chain.
    let q = ok("CREATE (a:person)-[:knows {since: 2001}]->(b)<-[e:likes]-(c), (c)-[:T]->(a)");
    let insert = insert_of(&q, 0);
    let steps = &insert.patterns[0].steps;
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].edge.direction, EdgeDirection::Right);
    assert_eq!(steps[1].edge.direction, EdgeDirection::Left);
    assert_eq!(
        conjunction(steps[0].edge.labels.as_ref().unwrap()),
        ["knows"]
    );
    assert_eq!(q.text(steps[0].edge.span), "-[:knows {since: 2001}]->");
    assert_eq!(q.text(steps[1].edge.span), "<-[e:likes]-");
    assert_eq!(steps[1].edge.variable.as_ref().unwrap().name, "e");
    assert_eq!(q.text(insert.patterns[1].span), "(c)-[:T]->(a)");
}

#[test]
fn insert_stands_wherever_a_clause_stands() {
    // After reads, before RETURN, before MATCH (W4), twice, first after NEXT.
    for (query, kinds) in [
        (
            "MATCH (n) INSERT (n)-[:T]->(:X) RETURN n",
            &["match", "insert"][..],
        ),
        ("INSERT (a:A) MATCH (b:A) RETURN b", &["insert", "match"]),
        ("INSERT (a) INSERT (b) RETURN a, b", &["insert", "insert"]),
        ("FOR x IN [1, 2] INSERT (:N {v: x})", &["for", "insert"]),
        (
            "MATCH (n) WITH n LET k = 1 INSERT (:N {k: k})",
            &["match", "with", "let", "insert"],
        ),
        (
            "MATCH (n) OPTIONAL MATCH (n)-->(m) INSERT (:N)",
            &["match", "optional", "insert"],
        ),
    ] {
        let q = ok(query);
        assert_eq!(stage_kinds(&q), kinds, "{query}");
    }
    let q = ok("MATCH (n) RETURN n NEXT INSERT (:copy) RETURN 1 AS one");
    assert_eq!(stage_kinds_of(&q.body.next[0].block.first), ["insert"]);
    // With RETURN the query ends as before.
    let q = ok("INSERT (n) RETURN n ORDER BY n LIMIT 1");
    assert_eq!(q.end(), QueryEnd::Return);
    assert!(q.limit.is_some());
}

#[test]
fn a_query_may_end_in_a_write_clause() {
    for query in [
        "INSERT (n)",
        "INSERT (n);",
        "MATCH (n) INSERT (n)-[:T]->(m)",
        "MATCH (p) CALL (p) { INSERT (p)-[:T]->(:X) }",
        "MATCH (p) OPTIONAL CALL (p) { INSERT (:X) } ",
    ] {
        let q = ok(query);
        assert_eq!(q.end(), QueryEnd::Write, "{query}");
        assert!(q.ret.items.is_empty() && q.ret.star.is_none(), "{query}");
        assert_eq!(q.ret.span.start, q.ret.span.end, "{query}");
        assert!(q.order_by.is_none() && q.offset.is_none() && q.limit.is_none());
    }
    // The span of the linear query ends at its last clause.
    let q = ok("MATCH (n) INSERT (m) ;");
    assert_eq!(q.text(q.span), "MATCH (n) INSERT (m)");
    // A CALL body may end in a write; the body of a CALL whose body writes may end the query.
    let q = ok("MATCH (p) CALL (p) { MATCH (p)-->(x) INSERT (x)-[:seen]->(:log) } RETURN p");
    let Some(Stage::Call(call)) = q.stages.get(1) else {
        panic!("no CALL: {q:?}");
    };
    assert_eq!(call.body.first.first.end(), QueryEnd::Write);
    assert_eq!(stage_kinds_of(&call.body.first.first), ["match", "insert"]);
    // A clause that reads after the write needs RETURN again.
    let f = fail("MATCH (n) INSERT (m) MATCH (k)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert!(f.message.starts_with("missing RETURN"), "{}", f.message);
    // A read-only CALL body without RETURN stays refused.
    let f = fail("MATCH (p) CALL (p) { MATCH (p)-->(x) } RETURN p");
    assert_eq!(f.kind, GqlErrorKind::Refused);
    assert_eq!(
        f.message,
        "a CALL { } body without RETURN (a unit subquery) only makes sense for writes, and this \
         body writes nothing"
    );
    // UNION and NEXT need a RETURN before them.
    for (query, text) in [
        ("INSERT (a) NEXT MATCH (b) RETURN b", "NEXT"),
        ("INSERT (a) UNION MATCH (b) RETURN b", "UNION"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("must follow a complete query"),
            "{}",
            f.message
        );
    }
}

#[test]
fn finish_ends_a_query_without_a_result() {
    for (query, finish) in [
        ("MATCH (n) FINISH", "FINISH"),
        ("finish", "finish"),
        ("MATCH (n) INSERT (m) FINISH;", "FINISH"),
    ] {
        let q = ok(query);
        let QueryEnd::Finish(span) = q.end() else {
            panic!("{query}: {:?}", q.end());
        };
        assert_eq!(q.text(span), finish, "{query}");
    }
    let q = ok("MATCH (p) CALL (p) { MATCH (p)-->(x) Finish } RETURN p");
    let Some(Stage::Call(call)) = q.stages.get(1) else {
        panic!("no CALL: {q:?}");
    };
    assert!(matches!(call.body.first.first.end(), QueryEnd::Finish(_)));
    // `finish` stays a name everywhere else.
    let q = ok("MATCH (finish) RETURN finish.x AS finish");
    assert_eq!(q.end(), QueryEnd::Return);
    // Nothing follows FINISH.
    for (query, text, expected) in [
        ("MATCH (n) FINISH RETURN n", "RETURN", "end of input"),
        ("MATCH (n) FINISH LIMIT 1", "LIMIT", "end of input"),
        (
            "MATCH (p) CALL (p) { FINISH MATCH (x) } RETURN p",
            "MATCH",
            "`}`",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.expected, [expected], "{query}");
        assert!(
            f.message.ends_with("after FINISH: FINISH ends the query"),
            "{}",
            f.message
        );
    }
}

#[test]
fn insert_expected_tokens() {
    // After an INSERT pattern: another pattern or a clause (K9: no write clause is named).
    let f = fail("MATCH (n) INSERT (m) foo");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "foo");
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
            "CALL",
            "RETURN"
        ]
    );
    // A pattern starts with a node.
    let f = fail("INSERT");
    assert_eq!(f.expected, ["`(`"]);
    let f = fail("INSERT x");
    assert_eq!(f.text, "x");
    assert_eq!(f.expected, ["`(`"]);
    // An edge leads to a node.
    let f = fail("INSERT (a)-[:T]->");
    assert_eq!(f.expected, ["`{`", "`*`", "`+`", "`(`"]);
    // Inside a node pattern.
    let f = fail("INSERT (a:person name)");
    assert_eq!(f.text, "name");
    assert_eq!(f.expected, ["`{`", "WHERE", "`)`"]);
    // After the whole query only its end may follow.
    let f = fail("INSERT (a); MATCH (n)");
    assert_eq!(f.text, "MATCH");
    assert_eq!(f.expected, ["end of input"]);
}

#[test]
fn insert_form_errors() {
    // W6: edges need a direction.
    for (query, text) in [
        ("INSERT (a)-[:T]-(b)", "-[:T]-"),
        ("INSERT (a)--(b)", "--"),
        ("INSERT (a)-(b)", "-"),
        ("INSERT (a)<-[:T]->(b)", "<-[:T]->"),
        ("INSERT (a)<-->(b)", "<-->"),
        ("CREATE (a)-[:T]-(b)", "-[:T]-"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.gqlstatus, "42001");
        assert_eq!(f.text, text, "{query}");
        let word = &query[..6];
        assert_eq!(
            f.message,
            format!("{word} needs a direction; write -[e:T]-> or <-[e:T]-"),
            "{query}"
        );
        assert!(
            f.alternative
                .as_deref()
                .unwrap_or("")
                .contains("-[:knows]->"),
            "{query}: {f:?}"
        );
    }
    // W6: exactly one edge label.
    for (query, text, message) in [
        ("INSERT (a)-[e]->(b)", "-[e]->", "needs exactly one label"),
        ("INSERT (a)-->(b)", "-->", "needs exactly one label"),
        ("INSERT (a)<-[]-(b)", "<-[]-", "needs exactly one label"),
        ("INSERT (a)-[:A|B]->(b)", "A|B", "has exactly one label"),
        ("INSERT (a)-[:A&B]->(b)", "A&B", "has exactly one label"),
        ("INSERT (a)-[:A:B]->(b)", "A:B", "has exactly one label"),
        ("INSERT (a)-[:!A]->(b)", "!A", "has exactly one label"),
        ("INSERT (a)-[:%]->(b)", "%", "has exactly one label"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains(message), "{query}: {}", f.message);
    }
    // W6: no quantifier; no WHERE in an edge.
    for (query, text, message) in [
        (
            "INSERT (a)-[:T]->{2}(b)",
            "{2}",
            "a quantifier cannot stand in INSERT",
        ),
        (
            "INSERT (a)-[:T*1..2]->(b)",
            "*1..2",
            "a quantifier cannot stand in INSERT",
        ),
        (
            "INSERT (a)-[:T]->+(b)",
            "+",
            "a quantifier cannot stand in INSERT",
        ),
        (
            "INSERT (a)-[e:T WHERE e.x = 1]->(b)",
            "e.x = 1",
            "a WHERE inside an edge pattern cannot stand in INSERT",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.starts_with(message), "{query}: {}", f.message);
    }
    // W7: node labels are a conjunction of names.
    for (query, text, operator) in [
        ("INSERT (a:A|B)", "A|B", "`|`"),
        ("CREATE (a:!A)", "!A", "`!`"),
        ("INSERT (a:%)", "%", "`%`"),
        ("INSERT (a:A&(B|C))", "(B|C)", "`|`"),
        ("INSERT (a IS A & !B)", "!B", "`!`"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.starts_with(operator), "{query}: {}", f.message);
        assert!(
            f.message.contains("a new node gets the labels written"),
            "{}",
            f.message
        );
    }
    // No WHERE in a node, no parenthesized path pattern.
    let f = fail("INSERT (a WHERE a.x = 1)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "a.x = 1");
    assert!(
        f.message.starts_with("a WHERE inside a node pattern"),
        "{}",
        f.message
    );
    let f = fail("INSERT (x) ((a)-[:T]->(b)){2}");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "((a)-[:T]->(b))");
    assert!(
        f.message.starts_with("a parenthesized path pattern"),
        "{}",
        f.message
    );
    // W5: a path variable is not yet supported.
    let f = fail("INSERT p = (a)-[:T]->(b)");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "p");
    assert_eq!(
        f.message,
        "a path variable in INSERT is not yet supported in this version"
    );
    // The second pattern is checked too.
    let f = fail("INSERT (a), (b)--(c)");
    assert_eq!(f.text, "--");
}

#[test]
fn writes_inside_subqueries_and_comprehensions_are_refused() {
    for (query, text, place) in [
        (
            "MATCH (n) WHERE EXISTS { CREATE (m) } RETURN n",
            "CREATE",
            "EXISTS { }",
        ),
        (
            "MATCH (n) WHERE EXISTS { MATCH (n) INSERT (m) } RETURN n",
            "INSERT",
            "EXISTS { }",
        ),
        (
            "MATCH (n) WHERE EXISTS { MATCH (n) INSERT (m) RETURN m } RETURN n",
            "INSERT",
            "EXISTS { }",
        ),
        (
            "MATCH (n) WHERE EXISTS ( MATCH (n) SET n.x = 1 ) RETURN n",
            "SET",
            "EXISTS ( )",
        ),
        (
            "MATCH (n) RETURN COUNT { MATCH (n) INSERT (m) RETURN m }",
            "INSERT",
            "COUNT { }",
        ),
        (
            "MATCH (n) RETURN COLLECT { MATCH (n) DELETE n RETURN n }",
            "DELETE",
            "COLLECT { }",
        ),
        (
            "MATCH (n) RETURN COUNT { MATCH (n) DETACH DELETE n RETURN n }",
            "DETACH DELETE",
            "COUNT { }",
        ),
        (
            "MATCH (n) RETURN EXISTS { CALL { INSERT (m) } RETURN 1 }",
            "INSERT",
            "EXISTS { }",
        ),
        (
            "RETURN [x IN [1] | CREATE (m)]",
            "CREATE",
            "a list comprehension",
        ),
        (
            "RETURN [x IN [1] WHERE MERGE (m) | x]",
            "MERGE",
            "a list comprehension",
        ),
        (
            "MATCH (n) RETURN [(n)-->(m) | CREATE (k)]",
            "CREATE",
            "a pattern comprehension",
        ),
        (
            "RETURN reduce(a = 0, x IN [1] | INSERT (m))",
            "INSERT",
            "reduce( )",
        ),
        ("RETURN all(x IN [1] WHERE CREATE (m))", "CREATE", "all( )"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "42001");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.message,
            format!("{text} inside {place}: a subquery expression or a comprehension cannot write"),
            "{query}"
        );
        assert!(f.alternative.unwrap_or_default().contains("INSERT (:maker"));
    }
    // A write clause where an expression stands, outside a subquery.
    let f = fail("MATCH (n) WHERE CREATE (m) RETURN n");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "CREATE");
    assert_eq!(
        f.message,
        "CREATE is a clause and cannot stand inside an expression"
    );
}

#[test]
fn union_operands_that_write_are_not_yet_supported() {
    for (query, text) in [
        (
            "MATCH (n) INSERT (m) RETURN m UNION MATCH (k) RETURN k",
            "INSERT",
        ),
        ("MATCH (k) RETURN k UNION ALL CREATE (m) RETURN m", "CREATE"),
        (
            "MATCH (k) RETURN k UNION CALL { INSERT (m) } RETURN 1 AS k",
            "INSERT",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.message,
            format!("{text} in an operand of UNION is not yet supported in this version")
        );
    }
    // A writing CALL body may hold its own UNION of reads.
    ok("CALL { MATCH (a) RETURN a UNION MATCH (a) RETURN a } INSERT (:copy)");
}

#[test]
fn catalog_and_vendor_commands_are_refused() {
    for (query, text, message, alternative) in [
        (
            "CREATE INDEX person_age FOR (n:person) ON (n.age)",
            "CREATE INDEX",
            "CREATE INDEX is a catalog command, not part of a gql() query",
            Some("g.create_index("),
        ),
        (
            "CREATE TEXT INDEX FOR (n:person) ON (n.name)",
            "CREATE TEXT",
            "CREATE TEXT is a catalog command, not part of a gql() query",
            Some("g.create_index("),
        ),
        (
            "CREATE CONSTRAINT FOR (n:person) REQUIRE n.name IS UNIQUE",
            "CREATE CONSTRAINT",
            "CREATE CONSTRAINT is a schema command, not part of a gql() query",
            Some("g.set_schema("),
        ),
        (
            "CREATE GRAPH social",
            "CREATE GRAPH",
            "CREATE GRAPH is a catalog command: gql() runs on the graph of its traversal source",
            Some("host application"),
        ),
        (
            "create or replace graph social",
            "create or replace",
            "CREATE OR REPLACE is a catalog command: gql() runs on the graph of its traversal \
             source",
            Some("host application"),
        ),
        (
            "CREATE DATABASE social",
            "CREATE DATABASE",
            "CREATE DATABASE is a vendor administration command, not part of GQL",
            None,
        ),
        (
            "MATCH (n) CREATE INDEX FOR (m:x) ON (m.k)",
            "CREATE INDEX",
            "CREATE INDEX is a catalog command, not part of a gql() query",
            Some("g.create_index("),
        ),
        (
            "LOAD CSV FROM 'file:///people.csv' AS row RETURN row",
            "LOAD CSV",
            "LOAD CSV is a vendor extension, not part of GQL",
            Some("FOR row IN $rows INSERT"),
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Refused, "{query}");
        assert_eq!(f.gqlstatus, "0A000");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.message, message, "{query}");
        match alternative {
            Some(part) => assert!(
                f.alternative.as_deref().unwrap_or("").contains(part),
                "{query}: {f:?}"
            ),
            None => assert!(f.alternative.is_none(), "{query}: {f:?}"),
        }
    }
    // A path variable after CREATE is a pattern, not a command.
    let f = fail("CREATE p = (a)-[:T]->(b)");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "p");
    // FOREACH: not yet, naming FOR with a write clause.
    let f = fail("MATCH (n) FOREACH (x IN [1] | CREATE (m))");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "FOREACH");
    assert!(f.alternative.unwrap_or_default().contains("FOR name IN"));
}

#[test]
fn not_yet_write_clauses_build_the_gremlin_example_from_the_clause() {
    let (query, example) = ("MATCH (p) FOREACH (x IN [1] | SET p.k = x)", "FOR name IN");
    let f = fail(query);
    assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}");
    let alternative = f.alternative.unwrap_or_default();
    assert!(alternative.contains(example), "{query}: {alternative}");
}

// ----- stage 3c.T1: SET / REMOVE -----

/// Stage `index` of `linear` as a SET clause.
fn set_of(linear: &LinearQuery, index: usize) -> &SetClause {
    match linear.stages.get(index) {
        Some(Stage::Set(clause)) => clause,
        other => panic!("stage {index} is no SET: {other:?}"),
    }
}

/// Stage `index` of `linear` as a REMOVE clause.
fn remove_of(linear: &LinearQuery, index: usize) -> &RemoveClause {
    match linear.stages.get(index) {
        Some(Stage::Remove(clause)) => clause,
        other => panic!("stage {index} is no REMOVE: {other:?}"),
    }
}

fn names_of(labels: &LabelItem) -> Vec<&str> {
    labels.labels.iter().map(|l| l.name.as_str()).collect()
}

#[test]
fn set_item_forms_and_spans() {
    let query = "MATCH (n)-[r]->(m) SET n.k = 1 + 2, (r).w = n.k, n = {a: 1}, m += {b: NULL}, \
                 n:Foo:Bar, m :A :B, n IS C & D, n:E&F&G RETURN n";
    let q = ok(query);
    assert_eq!(stage_kinds(&q), ["match", "set"]);
    assert_eq!(q.end(), QueryEnd::Return);
    let set = set_of(&q, 1);
    assert_eq!(q.text(set.keyword), "SET");
    assert!(q.text(set.span).starts_with("SET n.k = 1 + 2, "));
    assert!(q.text(set.span).ends_with("n:E&F&G"));
    let items: Vec<&str> = set.items.iter().map(|i| q.text(i.span)).collect();
    assert_eq!(
        items,
        [
            "n.k = 1 + 2",
            "(r).w = n.k",
            "n = {a: 1}",
            "m += {b: NULL}",
            "n:Foo:Bar",
            "m :A :B",
            "n IS C & D",
            "n:E&F&G"
        ]
    );
    let targets: Vec<&str> = set.items.iter().map(|i| i.target.name.as_str()).collect();
    assert_eq!(targets, ["n", "r", "n", "m", "n", "m", "n", "n"]);
    assert_eq!(q.text(set.items[1].target.span), "r");
    match &set.items[0].kind {
        SetItemKind::Property { key, value } => {
            assert_eq!(key.name, "k");
            assert_eq!(q.text(value.span), "1 + 2");
        }
        other => panic!("{other:?}"),
    }
    match &set.items[1].kind {
        SetItemKind::Property { key, value } => {
            assert_eq!(key.name, "w");
            assert!(matches!(value.kind, ExprKind::Property { .. }), "{value:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        &set.items[2].kind,
        SetItemKind::Replace { value } if matches!(value.kind, ExprKind::Map(_))
    ));
    assert!(matches!(
        &set.items[3].kind,
        SetItemKind::Merge { value } if q.text(value.span) == "{b: NULL}"
    ));
    let labels: Vec<(Vec<&str>, &str)> = set.items[4..]
        .iter()
        .map(|i| match &i.kind {
            SetItemKind::Labels(labels) => (names_of(labels), q.text(labels.span)),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        labels,
        [
            (vec!["Foo", "Bar"], ":Foo:Bar"),
            (vec!["A", "B"], ":A :B"),
            (vec!["C", "D"], "IS C & D"),
            (vec!["E", "F", "G"], ":E&F&G"),
        ]
    );
    // A label written twice is added once; `SET n = $p` and `SET n = m` are expression forms.
    let q = ok("MATCH (n), (m) SET n:A:A, n = $p, n += m");
    let set = set_of(&q, 1);
    match &set.items[0].kind {
        SetItemKind::Labels(labels) => assert_eq!(names_of(labels), ["A"]),
        other => panic!("{other:?}"),
    }
    assert!(matches!(&set.items[1].kind, SetItemKind::Replace { value }
        if matches!(value.kind, ExprKind::Parameter(_))));
    assert!(matches!(&set.items[2].kind, SetItemKind::Merge { value }
        if matches!(value.kind, ExprKind::Variable(_))));
    // Case-insensitive keyword, back-quoted names.
    let q = ok("match (`my n`) set `my n`.`the key` = 'x'");
    let set = set_of(&q, 1);
    assert_eq!(q.text(set.keyword), "set");
    assert_eq!(set.items[0].target.name, "my n");
    assert!(
        matches!(&set.items[0].kind, SetItemKind::Property { key, .. } if key.name == "the key")
    );
}

#[test]
fn remove_item_forms_and_spans() {
    let query = "MATCH (n)-[r]->() REMOVE n.k, (r).w, n:L1:L3, n IS A & B, n :X";
    let q = ok(query);
    assert_eq!(stage_kinds(&q), ["match", "remove"]);
    assert_eq!(q.end(), QueryEnd::Write);
    let remove = remove_of(&q, 1);
    assert_eq!(q.text(remove.keyword), "REMOVE");
    assert_eq!(q.text(remove.span), &query[18..]);
    let items: Vec<&str> = remove.items.iter().map(|i| q.text(i.span)).collect();
    assert_eq!(items, ["n.k", "(r).w", "n:L1:L3", "n IS A & B", "n :X"]);
    let kinds: Vec<String> = remove
        .items
        .iter()
        .map(|i| match &i.kind {
            RemoveItemKind::Property { key } => format!("{}.{}", i.target.name, key.name),
            RemoveItemKind::Labels(labels) => {
                format!("{}:{}", i.target.name, names_of(labels).join(":"))
            }
        })
        .collect();
    assert_eq!(kinds, ["n.k", "r.w", "n:L1:L3", "n:A:B", "n:X"]);
}

#[test]
fn set_and_remove_stand_where_insert_stands() {
    // First clause, between reads, a write ending, `FINISH`, after `NEXT`, in CALL bodies.
    for (query, kinds) in [
        ("SET n.k = 1", vec!["set"]),
        (
            "MATCH (n) SET n.k = 1 MATCH (m) REMOVE m.k RETURN m",
            vec!["match", "set", "match", "remove"],
        ),
        ("MATCH (n) SET n.k = 1;", vec!["match", "set"]),
        ("MATCH (n) REMOVE n:A FINISH", vec!["match", "remove"]),
        (
            "FOR x IN [1] SET x.k = 1 SET x.j = 2",
            vec!["for", "set", "set"],
        ),
        (
            "MATCH (n) INSERT (m) SET m.k = n.k",
            vec!["match", "insert", "set"],
        ),
    ] {
        assert_eq!(stage_kinds(&ok(query)), kinds, "{query}");
    }
    assert!(matches!(
        ok("MATCH (n) REMOVE n:A FINISH").end(),
        QueryEnd::Finish(_)
    ));
    assert_eq!(ok("MATCH (n) SET n.k = 1;").end(), QueryEnd::Write);
    let q = ok("MATCH (n) RETURN n NEXT MATCH (n) SET n.k = 1");
    assert_eq!(q.body.next.len(), 1);
    // A CALL body may end in SET / REMOVE (a unit subquery) or return after it.
    for query in [
        "MATCH (n) CALL (n) { SET n.k = 1 } RETURN n",
        "MATCH (n) CALL (n) { REMOVE n:A } RETURN n",
        "MATCH (n) CALL (n) { SET n.k = 1 RETURN n.k AS k } RETURN k",
        "MATCH (n) OPTIONAL CALL { WITH n REMOVE n.k } RETURN n",
    ] {
        let q = ok(query);
        let Some(Stage::Call(call)) = q.stages.get(1) else {
            panic!("{query}: {:?}", q.stages);
        };
        let kinds = stage_kinds_of(&call.body.first.first);
        assert!(
            kinds.contains(&"set") || kinds.contains(&"remove"),
            "{query}: {kinds:?}"
        );
    }
    // A UNION operand that writes: not yet, at its keyword.
    let f = fail("MATCH (n) SET n.k = 1 RETURN n UNION MATCH (n) RETURN n");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "SET");
    let f = fail("MATCH (n) RETURN n UNION MATCH (n) REMOVE n.k RETURN n");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "REMOVE");
    // A write clause after RETURN: RETURN ends the query.
    for (query, text) in [
        ("MATCH (n) RETURN n SET n.k = 1", "SET"),
        ("MATCH (n) RETURN n REMOVE n.k", "REMOVE"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text);
        assert_eq!(
            f.message,
            format!("{text} after RETURN: RETURN ends the query")
        );
    }
    // Inside subquery expressions and comprehensions: the write ban (design W3).
    for (query, text, place) in [
        (
            "MATCH (n) WHERE EXISTS { MATCH (n) SET n.k = 1 RETURN n } RETURN n",
            "SET",
            "EXISTS",
        ),
        (
            "MATCH (n) RETURN COUNT { MATCH (n) REMOVE n.k RETURN n } AS c",
            "REMOVE",
            "COUNT",
        ),
        (
            "MATCH (n) RETURN [x IN [n] | SET x.k = 1] AS l",
            "SET",
            "list comprehension",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains(place), "{query}: {}", f.message);
        assert!(f.message.contains("cannot write"), "{}", f.message);
    }
    // Inside an expression: a clause, not a function.
    let f = fail("MATCH (n) RETURN SET(n)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "SET");
    assert!(f.message.contains("is a clause"), "{}", f.message);
}

#[test]
fn set_and_remove_errors() {
    // Label items take a conjunction of names only (design D1).
    for (query, text, operator) in [
        ("MATCH (n) SET n:A|B", "A|B", "`|`"),
        ("MATCH (n) SET n IS !A", "!A", "`!`"),
        ("MATCH (n) REMOVE n:%", "%", "`%`"),
        ("MATCH (n) REMOVE n IS A&(B|C)", "(B|C)", "`|`"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "42001");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.starts_with(operator), "{query}: {}", f.message);
        assert!(
            f.alternative.as_deref().unwrap_or("").contains("as in "),
            "{query}: {f:?}"
        );
    }
    // A target that is no variable (design D2): not yet, the target underlined.
    for (query, text) in [
        ("MATCH (n) SET head([n]).k = 1", "head([n]).k"),
        ("MATCH (n) SET n.a.b = 1", "n.a.b"),
        ("MATCH (n) SET (n.a).k = 1", "(n.a).k"),
        ("MATCH (n) REMOVE n.a.b", "n.a.b"),
        ("MATCH (n) REMOVE head([n]).k", "head([n]).k"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "0A000");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(
            f.message,
            format!(
                "`{text}` as a {} target is not yet supported in this version",
                if query.contains("SET") {
                    "SET"
                } else {
                    "REMOVE"
                }
            )
        );
        assert!(f.alternative.unwrap_or_default().contains("AS x"));
    }
    // A dynamic key `n[k]`: not yet, as a target that is no variable.
    let f = fail("MATCH (n) SET n['k'] = 1");
    assert_eq!(f.kind, GqlErrorKind::NotYet);
    assert_eq!(f.text, "n['k']");
    // Missing or wrong operators: the expected tokens.
    for (query, text, expected) in [
        (
            "MATCH (n) SET n",
            "",
            vec!["`.`", "`=`", "`+=`", "`:`", "IS"],
        ),
        (
            "MATCH (n) SET n + = {}",
            "+",
            vec!["`.`", "`=`", "`+=`", "`:`", "IS"],
        ),
        ("MATCH (n) SET n.k", "", vec!["`=`"]),
        ("MATCH (n) SET n.k += 1", "+", vec!["`=`"]),
        ("MATCH (n) REMOVE n", "", vec!["`.`", "`:`", "IS"]),
        ("MATCH (n) REMOVE n = 1", "=", vec!["`.`", "`:`", "IS"]),
        ("MATCH (n) SET 1 = 2", "1", vec!["a variable"]),
        ("MATCH (n) SET", "", vec!["a variable"]),
        ("MATCH (n) SET n.k = 1,", "", vec!["a variable"]),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.expected, expected, "{query}");
    }
    // After an item: `,` and the clauses (K9).
    let f = fail("MATCH (n) SET n.k = 1 n.j = 2");
    assert_eq!(f.text, "n");
    assert_eq!(f.expected.first().map(String::as_str), Some("`,`"));
    let f = fail("MATCH (n) REMOVE n:A )");
    assert_eq!(f.expected.first().map(String::as_str), Some("`,`"));
}

// ----- stage 3d.T1: DELETE -----

/// Stage `index` of `linear` as a DELETE clause.
fn delete_of(linear: &LinearQuery, index: usize) -> &DeleteClause {
    match linear.stages.get(index) {
        Some(Stage::Delete(clause)) => clause,
        other => panic!("stage {index} is no DELETE: {other:?}"),
    }
}

#[test]
fn delete_forms_and_spans() {
    for (query, keyword, detach, nodetach) in [
        ("MATCH (n) DELETE n", "DELETE", false, false),
        ("MATCH (n) DETACH DELETE n", "DETACH DELETE", true, false),
        (
            "MATCH (n) NODETACH DELETE n",
            "NODETACH DELETE",
            false,
            true,
        ),
        ("match (n) detach  delete n", "detach  delete", true, false),
    ] {
        let q = ok(query);
        assert_eq!(stage_kinds(&q), ["match", "delete"], "{query}");
        assert_eq!(q.end(), QueryEnd::Write, "{query}");
        let delete = delete_of(&q, 1);
        assert_eq!(q.text(delete.keyword), keyword, "{query}");
        assert_eq!(delete.detach, detach, "{query}");
        assert_eq!(delete.explicit_nodetach, nodetach, "{query}");
        assert_eq!(q.text(delete.span), format!("{keyword} n"), "{query}");
    }
    // Expression targets (design W29): variables, paths, lists, map members, NULL, calls.
    let q = ok("MATCH p = (n)-[r]->(m) DELETE r, n, p, [m], NULL, {a: n}.a, nodes(p), $x");
    let delete = delete_of(&q, 1);
    let targets: Vec<&str> = delete.targets.iter().map(|t| q.text(t.span)).collect();
    assert_eq!(
        targets,
        ["r", "n", "p", "[m]", "NULL", "{a: n}.a", "nodes(p)", "$x"]
    );
    assert!(matches!(delete.targets[0].kind, ExprKind::Variable(_)));
    assert!(matches!(delete.targets[3].kind, ExprKind::List(_)));
    assert!(matches!(
        delete.targets[4].kind,
        ExprKind::Literal(Literal::Null)
    ));
    assert!(matches!(delete.targets[5].kind, ExprKind::Property { .. }));
    // A computed target parses (the binder refuses it, design E2).
    let q = ok("MATCH (n) DELETE 1 + 1");
    assert_eq!(q.text(delete_of(&q, 1).targets[0].span), "1 + 1");
}

#[test]
fn delete_stands_where_set_stands() {
    for (query, kinds) in [
        (
            "MATCH (n) DELETE n RETURN count(*)",
            vec!["match", "delete"],
        ),
        (
            "MATCH (n) DETACH DELETE n MATCH (m) DELETE m RETURN 1",
            vec!["match", "delete", "match", "delete"],
        ),
        ("MATCH (n) DELETE n;", vec!["match", "delete"]),
        ("MATCH (n) DELETE n FINISH", vec!["match", "delete"]),
        ("DELETE NULL", vec!["delete"]),
        (
            "MATCH (n) SET n.k = 1 DELETE n DELETE n",
            vec!["match", "set", "delete", "delete"],
        ),
        (
            "MATCH (n) WITH n, n.k AS k DELETE n RETURN k",
            vec!["match", "with", "delete"],
        ),
    ] {
        assert_eq!(stage_kinds(&ok(query)), kinds, "{query}");
    }
    assert!(matches!(
        ok("MATCH (n) DELETE n FINISH").end(),
        QueryEnd::Finish(_)
    ));
    assert_eq!(ok("MATCH (n) DELETE n RETURN 1").end(), QueryEnd::Return);
    let q = ok("MATCH (n) RETURN n NEXT MATCH (n) DETACH DELETE n");
    assert_eq!(q.body.next.len(), 1);
    // A CALL body may end in DELETE (a unit subquery) or return after it.
    for query in [
        "MATCH (n) CALL (n) { DETACH DELETE n } RETURN count(*)",
        "MATCH (n) CALL (n) { NODETACH DELETE n RETURN 1 AS one } RETURN one",
        "MATCH (n) OPTIONAL CALL { WITH n DELETE n } RETURN 1",
    ] {
        let q = ok(query);
        let Some(Stage::Call(call)) = q.stages.get(1) else {
            panic!("{query}: {:?}", q.stages);
        };
        let kinds = stage_kinds_of(&call.body.first.first);
        assert!(kinds.contains(&"delete"), "{query}: {kinds:?}");
    }
    // A UNION operand that writes: not yet, at its keywords.
    for (query, text) in [
        (
            "MATCH (n) DELETE n RETURN 1 AS x UNION RETURN 2 AS x",
            "DELETE",
        ),
        (
            "RETURN 2 AS x UNION MATCH (n) DETACH DELETE n RETURN 1 AS x",
            "DETACH DELETE",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.contains("in an operand of UNION"),
            "{}",
            f.message
        );
    }
    // After RETURN: RETURN ends the query.
    for (query, text) in [
        ("MATCH (n) RETURN n DELETE n", "DELETE"),
        ("MATCH (n) RETURN n DETACH DELETE n", "DETACH"),
        ("MATCH (n) RETURN n NODETACH DELETE n", "NODETACH"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.ends_with("after RETURN: RETURN ends the query"),
            "{query}: {}",
            f.message
        );
    }
    // Inside subquery expressions and comprehensions: the write ban (design W3), the keywords
    // underlined.
    for (query, text, place) in [
        (
            "MATCH (n) WHERE EXISTS { MATCH (n) DELETE n RETURN n } RETURN n",
            "DELETE",
            "EXISTS",
        ),
        (
            "MATCH (n) RETURN COUNT { MATCH (n) NODETACH DELETE n RETURN n } AS c",
            "NODETACH DELETE",
            "COUNT",
        ),
        (
            "MATCH (n) RETURN [x IN [n] | DETACH DELETE x] AS l",
            "DETACH DELETE",
            "list comprehension",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.contains(place), "{query}: {}", f.message);
        assert!(f.message.contains("cannot write"), "{}", f.message);
    }
    // Inside an expression: a clause, not a function.
    let f = fail("MATCH (n) RETURN DELETE(n)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "DELETE");
    assert!(f.message.contains("is a clause"), "{}", f.message);
}

#[test]
fn delete_errors() {
    // A label predicate as a target (design E1, TCK Delete1 [8], Delete2 [5]): a syntax error
    // at the target naming REMOVE.
    for (query, text, element, remove) in [
        (
            "MATCH (n) DELETE n:Person",
            "n:Person",
            "n",
            "REMOVE n:Person",
        ),
        ("MATCH ()-[r]->() DELETE r:T", "r:T", "r", "REMOVE r:T"),
        (
            "MATCH (n) DETACH DELETE n IS Person",
            "n IS Person",
            "n",
            "REMOVE n:Person",
        ),
        (
            "MATCH (n) DELETE n IS LABELED Person",
            "n IS LABELED Person",
            "n",
            "REMOVE n:Person",
        ),
        (
            "MATCH (n), (m) DELETE m, n:A:B",
            "n:A:B",
            "n",
            "REMOVE n:A:B",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "42001");
        assert_eq!(f.text, text, "{query}");
        assert!(
            f.message.starts_with(&format!(
                "`{text}` is a label test (a boolean), not an element"
            )),
            "{query}: {}",
            f.message
        );
        let alternative = f.alternative.unwrap_or_default();
        assert!(alternative.contains(remove), "{query}: {alternative}");
        assert!(
            alternative.contains(&format!("DELETE {element}")),
            "{query}: {alternative}"
        );
    }
    // Missing or wrong pieces: the expected tokens.
    for (query, text, expected) in [
        ("MATCH (n) DETACH n", "n", vec!["DELETE"]),
        ("MATCH (n) NODETACH", "", vec!["DELETE"]),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.expected, expected, "{query}");
    }
    for query in ["MATCH (n) DELETE", "MATCH (n) DELETE n,"] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, "", "{query}");
    }
    // After a target: `,` and the clauses (K9).
    let f = fail("MATCH (n) DELETE n )");
    assert_eq!(f.text, ")");
    assert_eq!(f.expected.first().map(String::as_str), Some("`,`"));
    assert!(f.expected.iter().any(|e| e == "RETURN"), "{:?}", f.expected);
}

// ----- stage 3e.T1: MERGE -----

/// Stage `index` of `linear` as a MERGE clause.
fn merge_of(linear: &LinearQuery, index: usize) -> &MergeClause {
    match linear.stages.get(index) {
        Some(Stage::Merge(clause)) => clause,
        other => panic!("stage {index} is no MERGE: {other:?}"),
    }
}

/// The `help()` of the parse error of `query`.
fn help_of(query: &str) -> String {
    match parse(query) {
        Ok(parsed) => panic!("{query:?} parsed: {parsed:?}"),
        Err(e) => e.help().unwrap_or_default(),
    }
}

#[test]
fn merge_forms_and_spans() {
    let query = "MATCH (a) MERGE (a)-[r:KNOWS {since: 1}]->(b:person:admin {name: 'x'})";
    let q = ok(query);
    assert_eq!(stage_kinds(&q), ["match", "merge"]);
    let merge = merge_of(&q, 1);
    assert_eq!(q.text(merge.keyword), "MERGE");
    assert_eq!(
        q.text(merge.span),
        "MERGE (a)-[r:KNOWS {since: 1}]->(b:person:admin {name: 'x'})"
    );
    assert!(merge.actions.is_empty());
    assert!(merge.pattern.variable.is_none());
    assert_eq!(merge.pattern.steps.len(), 1);
    // A path variable (P0 E3) and an undirected edge (W33) are allowed; lower case keywords.
    let q = ok("merge p = (a:A)-[:T]-(b:B) return p");
    let merge = merge_of(&q, 0);
    assert_eq!(
        merge.pattern.variable.as_ref().map(|v| v.name.as_str()),
        Some("p")
    );
    assert_eq!(merge.pattern.steps[0].edge.direction, EdgeDirection::Any);
    assert_eq!(q.text(merge.span), "merge p = (a:A)-[:T]-(b:B)");
    // ON parts: several, any order, written order kept, items as in SET (TCK Merge4 [1]:
    // ON MATCH first).
    let query = "MERGE (a:A {k: 1}) ON MATCH SET a.seen = true, a:B ON CREATE SET a += {c: 1} \
                 on create set a.n = 2 ON MATCH SET a = {k: 1} RETURN a";
    let q = ok(query);
    let merge = merge_of(&q, 0);
    let parts: Vec<(bool, usize, &str)> = merge
        .actions
        .iter()
        .map(|a| (a.on_create, a.items.len(), q.text(a.keyword)))
        .collect();
    assert_eq!(
        parts,
        [
            (false, 2, "ON MATCH SET"),
            (true, 1, "ON CREATE SET"),
            (true, 1, "on create set"),
            (false, 1, "ON MATCH SET"),
        ]
    );
    assert!(matches!(
        merge.actions[0].items[1].kind,
        SetItemKind::Labels(_)
    ));
    assert!(matches!(
        merge.actions[1].items[0].kind,
        SetItemKind::Merge { .. }
    ));
    assert_eq!(q.text(merge.actions[3].span), "ON MATCH SET a = {k: 1}");
    assert!(q.text(merge.span).ends_with("ON MATCH SET a = {k: 1}"));
    // `ON` is a contextual word: a variable may be named `on`.
    let q = ok("FOR on IN [1] MERGE (a {k: on}) ON CREATE SET a.x = on RETURN a");
    assert_eq!(merge_of(&q, 1).actions.len(), 1);
}

#[test]
fn merge_stands_where_set_stands() {
    for (query, kinds) in [
        ("MERGE (a:A)", vec!["merge"]),
        ("MERGE (a:A);", vec!["merge"]),
        ("MERGE (a:A) FINISH", vec!["merge"]),
        ("MERGE (a:A) RETURN a", vec!["merge"]),
        (
            "MATCH (n) MERGE (n)-[:T]->(m) MERGE (m)-[:T]->(o) RETURN o",
            vec!["match", "merge", "merge"],
        ),
        (
            "FOR x IN [1, 1] MERGE (a:N {x: x}) ON CREATE SET a.new = true SET a.k = 1",
            vec!["for", "merge", "set"],
        ),
        (
            "CREATE (a) MERGE (b) DELETE a MATCH (c) RETURN c",
            vec!["insert", "merge", "delete", "match"],
        ),
    ] {
        assert_eq!(stage_kinds(&ok(query)), kinds, "{query}");
    }
    assert!(matches!(ok("MERGE (a) FINISH").end(), QueryEnd::Finish(_)));
    let q = ok("MATCH (n) RETURN n NEXT MERGE (n)-[:T]->(:X)");
    assert_eq!(q.body.next.len(), 1);
    // A CALL body may end in MERGE (a unit subquery) or return after it.
    for query in [
        "MATCH (n) CALL (n) { MERGE (n)-[:T]->(:X) } RETURN count(*)",
        "MATCH (n) CALL (n) { MERGE (n)-[:T]->(x:X) RETURN x } RETURN x",
        "MATCH (n) OPTIONAL CALL { WITH n MERGE (n)-[:T]->(:X) ON MATCH SET n.k = 1 } RETURN 1",
    ] {
        let q = ok(query);
        let Some(Stage::Call(call)) = q.stages.get(1) else {
            panic!("{query}: {:?}", q.stages);
        };
        let kinds = stage_kinds_of(&call.body.first.first);
        assert!(kinds.contains(&"merge"), "{query}: {kinds:?}");
    }
    // A UNION operand that writes: not yet, at its keyword.
    let f = fail("MERGE (n) RETURN 1 AS x UNION RETURN 2 AS x");
    assert_eq!(f.kind, GqlErrorKind::NotYet, "{f:?}");
    assert_eq!(f.text, "MERGE");
    assert!(
        f.message.contains("in an operand of UNION"),
        "{}",
        f.message
    );
    // After RETURN: RETURN ends the query.
    let f = fail("MATCH (n) RETURN n MERGE (m)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "MERGE");
    assert_eq!(f.message, "MERGE after RETURN: RETURN ends the query");
    // Inside subquery expressions and comprehensions: the write ban (design W3).
    for (query, text, place) in [
        (
            "MATCH (n) WHERE EXISTS { MATCH (n) MERGE (n)-[:T]->(m) RETURN m } RETURN n",
            "MERGE",
            "EXISTS",
        ),
        (
            "MATCH (n) RETURN COUNT { merge (m:X) RETURN m } AS c",
            "MERGE",
            "COUNT",
        ),
        (
            "MATCH (n) RETURN [x IN [n] WHERE MERGE (m) | x] AS l",
            "MERGE",
            "list comprehension",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text.to_ascii_uppercase(), text, "{query}");
        assert!(f.message.contains(place), "{query}: {}", f.message);
        assert!(f.message.contains("cannot write"), "{}", f.message);
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains("INSERT (:maker"),
            "{query}: {alternative}"
        );
        assert!(help_of(query).contains("MERGE looks for"), "{query}");
    }
    // Inside an expression: a clause, not a function.
    let f = fail("MATCH (n) RETURN MERGE(n)");
    assert_eq!(f.kind, GqlErrorKind::Syntax);
    assert_eq!(f.text, "MERGE");
    assert!(f.message.contains("is a clause"), "{}", f.message);
}

#[test]
fn merge_form_errors() {
    // Design W30: the INSERT rules except the path variable and the direction, worded for
    // MERGE, with the MERGE advice.
    for (query, text, message) in [
        (
            "MERGE (a)-[:T*2]->(b)",
            "*2",
            "a quantifier cannot stand in MERGE: each edge pattern finds or creates one edge",
        ),
        (
            "MERGE (a)-[:T]->{2}(b)",
            "{2}",
            "a quantifier cannot stand in MERGE",
        ),
        (
            "MERGE (a WHERE a.k = 1)",
            "a.k = 1",
            "a WHERE inside a node pattern cannot stand in MERGE: it finds or creates the node",
        ),
        (
            "MERGE (a)-[e:T WHERE e.k = 1]->(b)",
            "e.k = 1",
            "a WHERE inside an edge pattern cannot stand in MERGE",
        ),
        (
            "MERGE (a:A|B)",
            "A|B",
            "`|` (a label alternative) cannot stand in MERGE",
        ),
        (
            "MERGE (a:!A)",
            "!A",
            "`!` (a label negation) cannot stand in MERGE",
        ),
        ("MERGE (a:%)", "%", "`%` (any label) cannot stand in MERGE"),
        (
            "MERGE (a)-->(b)",
            "-->",
            "an edge in MERGE needs exactly one label, its type",
        ),
        (
            "MERGE (a)-[NO_COLON]->(b)",
            "-[NO_COLON]->",
            "an edge in MERGE needs exactly one label, its type",
        ),
        (
            "MERGE (a)-[:A|B]->(b)",
            "A|B",
            "an edge in MERGE has exactly one label",
        ),
        (
            "MERGE (a) ((x)-[:T]->(y)){2} (b)",
            "((x)-[:T]->(y))",
            "a parenthesized path pattern cannot stand in MERGE",
        ),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "42001", "{query}");
        assert_eq!(f.text, text, "{query}");
        assert!(f.message.starts_with(message), "{query}: {}", f.message);
        let alternative = f.alternative.unwrap_or_default();
        assert!(
            alternative.contains("g.gql(\"MERGE ("),
            "{query}: {alternative}"
        );
        let help = help_of(query);
        assert!(
            help.contains("MERGE looks for its whole pattern"),
            "{query}: {help}"
        );
    }
    // Cypher's `:A|:B` (TCK Merge5 [25]) is the label expression `A|B`: a syntax error under it.
    let f = fail("MERGE (a)-[:A|:B]->(b)");
    assert_eq!(f.kind, GqlErrorKind::Syntax, "{f:?}");
    assert_eq!(f.text, "A|:B");
    // A parameter as the property map (TCK Merge1 [16], Merge5 [27]): the existing 42001.
    for query in [
        "MERGE (n $param)",
        "MATCH (a), (b) MERGE (a)-[r:FOO $param]->(b)",
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "42001", "{query}");
    }
    // Several patterns (W30): 42001 at the comma, one MERGE per pattern.
    let query = "MERGE (a:A), (b:B)";
    let f = fail(query);
    assert_eq!(f.kind, GqlErrorKind::Syntax, "{f:?}");
    assert_eq!(f.gqlstatus, "42001");
    assert_eq!(f.text, ",");
    assert!(
        f.message.starts_with("MERGE takes exactly one pattern"),
        "{}",
        f.message
    );
    let alternative = f.alternative.unwrap_or_default();
    assert!(
        alternative.contains("g.gql(\"MERGE (a:A) MERGE (b:B)\")"),
        "{alternative}"
    );
    assert!(help_of(query).contains("MERGE looks for"));
    // Missing or wrong pieces of an ON part: the expected tokens.
    for (query, text, expected) in [
        (
            "MERGE (a) ON DELETE SET a.k = 1",
            "DELETE",
            vec!["CREATE", "MATCH"],
        ),
        ("MERGE (a) ON", "", vec!["CREATE", "MATCH"]),
        ("MERGE (a) ON CREATE a.k = 1", "a", vec!["SET"]),
        ("MERGE (a) ON MATCH", "", vec!["SET"]),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
        assert_eq!(f.expected, expected, "{query}");
        assert!(help_of(query).contains("MERGE looks for"), "{query}");
    }
    for query in [
        "MERGE (a) ON CREATE SET",
        "MERGE (a) ON CREATE SET a.k = 1,",
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.text, "", "{query}");
    }
    // After the pattern: the ON parts and the clauses; after an item also `,` (K9).
    let f = fail("MERGE (a) )");
    assert_eq!(f.text, ")");
    assert_eq!(
        f.expected.get(..2).unwrap_or_default(),
        ["ON CREATE SET", "ON MATCH SET"]
    );
    assert!(f.expected.iter().any(|e| e == "RETURN"), "{:?}", f.expected);
    let query = "MERGE (a) ON CREATE SET a.k = 1 )";
    let f = fail(query);
    assert_eq!(f.text, ")");
    assert_eq!(
        f.expected.get(..3).unwrap_or_default(),
        ["`,`", "ON CREATE SET", "ON MATCH SET"]
    );
    assert!(help_of(query).contains("MERGE looks for"));
    // A missing pattern.
    let f = fail("MERGE RETURN 1");
    assert_eq!(f.kind, GqlErrorKind::Syntax, "{f:?}");
    assert_eq!(f.text, "RETURN");
}

// ----- indexing and slicing (stage 3f) -----

#[test]
fn subscripts_bind_like_property_access() {
    for (src, tree) in [
        ("x[0]", "(at x 0)"),
        ("x[-1]", "(at x -1)"),
        ("x[i + 1]", "(at x (+ i 1))"),
        ("x[1..3]", "(slice x 1 3)"),
        ("x[1..]", "(slice x 1 _)"),
        ("x[..3]", "(slice x _ 3)"),
        ("x[..]", "(slice x _ _)"),
        ("x[$a..$b]", "(slice x $a $b)"),
        ("x[-3..-1]", "(slice x -3 -1)"),
        ("x[null..]", "(slice x null _)"),
        ("[[1]][0][0]", "(at (at [[1]] 0) 0)"),
        ("[1, 2][0] + 3", "(+ (at [1 2] 0) 3)"),
        ("[1] + [2, 3][1..]", "(+ [1] (slice [2 3] 1 _))"),
        ("-x[0]", "(- (at x 0))"),
        ("x[0].name", "(at x 0).name"),
        ("x.list[0]", "(at x.list 0)"),
        ("x[0] IN y[1..]", "(in (at x 0) (slice y 1 _))"),
        ("n[k]", "(at n k)"),
        ("n['na' + 'me']", "(at n (+ 'na' 'me'))"),
        ("m[$key]", "(at m $key)"),
        // A string literal key is the property access (one plan with `n.name`).
        ("n['name']", "n.name"),
        ("{null: 1}['null']", "{null: 1}.null"),
        ("x[0]['a']", "(at x 0).a"),
    ] {
        assert_eq!(expr(src), tree, "{src}");
    }
    // The span covers the whole subscript (the default column name).
    let q = ok("MATCH (n) RETURN [1, 2][0..1], n['name']");
    assert_eq!(q.text(q.ret.items[0].expr.span), "[1, 2][0..1]");
    assert_eq!(q.text(q.ret.items[1].expr.span), "n['name']");
}

#[test]
fn malformed_subscripts_are_syntax_errors() {
    for (query, text) in [
        ("RETURN [1][]", "]"),
        ("RETURN [1][0", ""),
        ("RETURN [1][0..1..2]", ".."),
        ("RETURN [1][0, 1]", ","),
        // TCK Literals7 [21]: a nested list with missing commas.
        ("RETURN [[','[]',']]", "]"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::Syntax, "{query}: {f:?}");
        assert_eq!(f.gqlstatus, "42001", "{query}");
        assert_eq!(f.text, text, "{query}");
    }
    // Each subscript is one level of the expression tree: a long chain reaches MAX_HEIGHT.
    let f = fail(&format!("MATCH (n) RETURN n{}", "[0]".repeat(50_000)));
    assert_eq!(f.kind, GqlErrorKind::Limit);
    assert!(f.message.contains("too large"), "{}", f.message);
    // Nested inside its brackets, the depth bound holds.
    let n = 50_000;
    let f = fail(&format!(
        "MATCH (n) RETURN {}0{}",
        "n[".repeat(n),
        "]".repeat(n)
    ));
    assert_eq!(f.kind, GqlErrorKind::Limit);
}

#[test]
fn non_reserved_words_name_variables() {
    // ISO GQL non-reserved words (FIRST, LAST, LABELED, SHORTEST) are names wherever a
    // variable can stand (TCK Unwind1 [3], Match4 [8]); their keyword meaning stays.
    for query in [
        "WITH [1, 2] AS first RETURN first",
        "MATCH (first)-[last]->(shortest) RETURN first, last, shortest",
        "MATCH labeled = (a)-->(b) RETURN labeled",
        "LET first = 1 RETURN first + 1 AS x",
        "FOR last IN [1, 2] WITH ORDINALITY first RETURN last, first",
        "UNWIND [1] AS first RETURN [first IN [first] | first] AS l",
        "MATCH (n) RETURN all(last IN [n] WHERE last IS NOT NULL) AS b",
        "MATCH (first) CALL (first) { RETURN 1 AS one } RETURN first, one",
        "MATCH (first) RETURN first { .name } AS m",
    ] {
        ok(query);
    }
    let q = ok("WITH [1, 2] AS first RETURN first");
    assert!(matches!(
        &q.ret.items[0].expr.kind,
        ExprKind::Variable(var) if var.name == "first"
    ));
    // The keyword forms keep their meaning.
    for (query, text) in [
        ("MATCH (n) RETURN n ORDER BY n.x NULLS FIRST", "NULLS"),
        ("MATCH ANY SHORTEST (a)-->(b) RETURN a", "ANY SHORTEST"),
    ] {
        let f = fail(query);
        assert_eq!(f.kind, GqlErrorKind::NotYet, "{query}: {f:?}");
        assert_eq!(f.text, text, "{query}");
    }
    ok("MATCH (n) WHERE n IS LABELED person RETURN n");
}

#[test]
fn a_colon_after_a_label_bar_is_the_cypher_disjunction() {
    // TCK Match3 [8]: `[:T|:T]` reads as `[:T|T]`.
    for (query, expected) in [
        ("MATCH (a)-[:T|:U]->(b) RETURN b", "(| T U)"),
        ("MATCH (a)-[e:T|:U|V]->(b) RETURN b", "(| (| T U) V)"),
    ] {
        let q = ok(query);
        let edge = &q.matches[0].patterns[0].steps[0].edge;
        assert_eq!(
            edge.labels.as_ref().map(label),
            Some(expected.to_string()),
            "{query}"
        );
    }
    // Mixed with the `:A:B` conjunction it stays an error.
    let f = fail("MATCH (a:A|:B:C) RETURN a");
    assert_eq!(f.kind, GqlErrorKind::Syntax, "{f:?}");
}

#[test]
fn power_binds_between_multiplication_and_unary_minus() {
    // Stage 3f: Cypher `^`, left-associative (the openCypher TCK `Precedence2` [2]-[4]).
    for (src, tree) in [
        ("2 ^ 3", "(^ 2 3)"),
        ("4 ^ 3 * 2 ^ 3", "(* (^ 4 3) (^ 2 3))"),
        ("4 ^ 3 + 2 ^ 3", "(+ (^ 4 3) (^ 2 3))"),
        ("2 ^ 3 ^ 2", "(^ (^ 2 3) 2)"),
        ("-3 ^ 2", "(^ -3 2)"),
        ("-x ^ 2", "(^ (- x) 2)"),
        ("x[0] ^ 2", "(^ (at x 0) 2)"),
        ("x.a ^ 2 = 4", "(= (^ x.a 2) 4)"),
    ] {
        assert_eq!(expr(src), tree, "{src}");
    }
    assert!(parse("RETURN 2 ^").is_err());
}
