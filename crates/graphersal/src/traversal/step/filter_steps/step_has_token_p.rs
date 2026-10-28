//! `hasId(P)`, `hasLabel(P)` and `hasKey(P)`: the predicate overloads of the token filters.
//!
//! The literal forms stay in `StepHasId`/`StepHasLabel`/`StepHasKey` so the optimizer can fold
//! them into a source step. A predicate other than `eq`/`within` cannot be folded and lands here.
//! String operands are always literal data (an id, a label, a key), never a step label.

use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::*;

/// What a [`StepHasTokenP`] reads from the stream value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Id,
    Label,
    Key,
}

impl Token {
    fn name(self) -> &'static str {
        match self {
            Token::Id => "has_id",
            Token::Label => "has_label",
            Token::Key => "has_key",
        }
    }

    /// The `GraphStep::Filter` name; distinct from the literal steps so no optimizer rule that
    /// folds `has_id`/`has_label` mistakes this step for one.
    fn step_name(self) -> &'static str {
        match self {
            Token::Id => "has_id_p",
            Token::Label => "has_label_p",
            Token::Key => "has_key_p",
        }
    }
}

/// `hasId(P)`, `hasLabel(P)` and `hasKey(P)`: one filter over the id, the labels or the key of
/// the stream value.
///
/// **Deviations from TinkerPop 3.8.2** (user's guide: "Predicates: Comparison and Resolution"):
/// the operand is always literal data. A string operand is never resolved against a step label
/// or a side effect, so a label named like an id cannot shadow it. Over a label set or key set a
/// positive predicate (`eq`, `within`, ...) holds if ANY candidate satisfies it, a negated one
/// (`neq`, `without`, `notX`) only if ALL do.
///
/// Ids are strings: for `has_id(P)` a number operand of an equality predicate (`eq`, `neq`,
/// `within`, `without`, also inside `and`/`or`/`not`) is compared as its literal text, once when
/// the step is built (`3` matches the id `"3"`, `3.0` the id `"3.0"`, like `has_id(3.0)`); an
/// order predicate on a number is refused at plan time ([`ValueError::NumericIdComparison`]).
#[derive(Clone)]
struct StepHasTokenP<B: Into<PValue> + Debug + Clone + Send + Sync> {
    token: Token,
    /// The predicate as written: rendered by the profile and checked by `plan_check`.
    predicate: P<B>,
    /// The predicate the step tests: `predicate` with the numbers of the id equality forms
    /// already turned into text (no per-traverser conversion).
    compare: P<PValue>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> StepHasTokenP<B> {
    fn new(token: Token, predicate: P<B>) -> Self {
        let compare = numbers_as_text(&predicate, token == Token::Id);
        Self {
            token,
            predicate,
            compare,
        }
    }
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepHasTokenP<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}({})",
            self.token.name(),
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

fn string_value<S: GraphStorage>(text: &str) -> TraverserValue<S> {
    TraverserValue::Computed(ElementProperty::String(CompactString::new(text)), None)
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> StepHasTokenP<B> {
    /// Tests the predicate over a set of candidate strings (`P::test_set`).
    fn test_set<S: GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        candidates: &[&str],
    ) -> TraverserResult<bool> {
        self.compare.test_set(context, candidates)
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepHasTokenP<B>
{
    /// `has_id(P)` with an order predicate (`gt`, `gte`, `lt`, `lte`, `between`, `inside`,
    /// `outside`) on a number is an error at plan time: ids are strings, so the comparison could
    /// never hold and would silently filter everything out. Equality forms keep a number.
    fn plan_check(&self) -> TraverserResult<()> {
        if self.token == Token::Id && orders_by_number(&self.predicate) {
            let fixed = numbers_as_text(&self.predicate, false);
            return Err(Box::new(TraverserError::Value(
                ValueError::NumericIdComparison {
                    step: self.to_string(),
                    fix: format!(
                        "{}({})",
                        self.token.name(),
                        crate::value::predicate::render_predicate(&fixed)
                    ),
                },
            )));
        }
        Ok(())
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match (self.token, &source.value) {
            (Token::Id, TraverserValue::Vertex(node)) => {
                let Some(id) = crate::graph::storage::filter_vertex_id(&*context.storage, *node)?
                else {
                    return Ok(false);
                };
                let id = string_value::<S>(id);
                self.compare.test_literal(context, &id)
            }
            (Token::Id, TraverserValue::Edge(edge)) => {
                let Some(id) = crate::graph::storage::filter_edge_id(&*context.storage, *edge)?
                else {
                    return Ok(false);
                };
                let id = string_value::<S>(id);
                self.compare.test_literal(context, &id)
            }
            (Token::Label, TraverserValue::Vertex(node)) => {
                // The label set is borrowed from storage while the predicate needs `context`
                // mutably, so the (short) set is copied once per traverser.
                let labels: Vec<CompactString> = context.storage.vertex_labels(*node)?.to_vec();
                self.test_set(
                    context,
                    &labels.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
                )
            }
            (Token::Label, TraverserValue::Edge(edge)) => {
                let label = context.storage.edge_label(*edge)?.map(CompactString::new);
                self.test_set(
                    context,
                    &label.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
                )
            }
            (
                Token::Key,
                TraverserValue::VertexProperty(key, _, _) | TraverserValue::EdgeProperty(key, _, _),
            ) => self.test_set(context, &[key.as_str()]),
            (Token::Key, TraverserValue::PathProperty(prop)) => {
                self.test_set(context, &[prop.0.as_str()])
            }
            (Token::Key, TraverserValue::Vertex(node)) => {
                let keys: Vec<CompactString> = match context.storage.vertex_properties(*node)? {
                    Some(properties) => properties
                        .keys()
                        .map(|k| CompactString::new(k.as_str()))
                        .collect(),
                    None => Vec::new(),
                };
                self.test_set(
                    context,
                    &keys.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                )
            }
            (Token::Key, TraverserValue::Edge(edge)) => {
                let keys: Vec<CompactString> = match context.storage.edge_properties(*edge)? {
                    Some(properties) => properties
                        .keys()
                        .map(|k| CompactString::new(k.as_str()))
                        .collect(),
                    None => Vec::new(),
                };
                self.test_set(
                    context,
                    &keys.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                )
            }
            (Token::Key, TraverserValue::Record { keys, .. }) => {
                let keys: Vec<CompactString> = keys
                    .iter()
                    .map(|k| CompactString::new(k.as_str()))
                    .collect();
                self.test_set(
                    context,
                    &keys.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                )
            }
            (Token::Key, TraverserValue::Object(object)) => {
                let mut keys: Vec<CompactString> = Vec::new();
                for key in object.keys() {
                    match key {
                        TraverserValue::VertexProperty(prop, _, _)
                        | TraverserValue::EdgeProperty(prop, _, _) => keys.push(prop.clone()),
                        TraverserValue::Computed(ElementProperty::String(value), _) => {
                            keys.push(value.clone())
                        }
                        // Not a property key: it matches nothing, as in `hasKey(literal)`.
                        _ => {}
                    }
                }
                self.test_set(
                    context,
                    &keys.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                )
            }
            (Token::Id | Token::Label, other) => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge"],
                other,
            ))),
            (Token::Key, other) => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "vertexProperty", "edge", "edgeProperty", "object"],
                other,
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// Whether `predicate` (or a part of a composite) is an order comparison with a number operand.
fn orders_by_number<B: Into<PValue> + Clone>(predicate: &P<B>) -> bool {
    let number = |value: &B| matches!(value.clone().into(), PValue::Number(_));
    match predicate {
        P::Lt(v) | P::Gt(v) | P::Lte(v) | P::Gte(v) => number(v),
        P::Inside(a, b) | P::Outside(a, b) | P::Between(a, b) => number(a) || number(b),
        P::And(a, b) | P::Or(a, b) => orders_by_number(a) || orders_by_number(b),
        P::Not(inner) => orders_by_number(inner),
        P::Eq(_)
        | P::Neq(_)
        | P::WithIn(_)
        | P::WithOut(_)
        | P::StartingWith(_)
        | P::NotStartingWith(_)
        | P::EndingWith(_)
        | P::NotEndingWith(_)
        | P::Containing(_)
        | P::NotContaining(_)
        | P::Regex(_)
        | P::NotRegex(_)
        | P::TypeOf(_) => false,
    }
}

/// `predicate` as a `P<PValue>` with number operands replaced by their literal text
/// (`P.gt(3)` -> `P.gt("3")`): with `equality_only`, only the operands of `eq`/`neq`/`within`/
/// `without` (the comparisons an id step runs), else every operand (the fix the `help()` of
/// [`ValueError::NumericIdComparison`] shows).
fn numbers_as_text<B: Into<PValue> + Clone>(predicate: &P<B>, equality_only: bool) -> P<PValue> {
    let text = |value: &B| match value.clone().into() {
        PValue::Number(n) => PValue::String(CompactString::new(
            crate::value::literal::rhai_pvalue(&PValue::Number(n)),
        )),
        other => other,
    };
    let order = |value: &B| {
        if equality_only {
            value.clone().into()
        } else {
            text(value)
        }
    };
    let texts = |values: &[B]| values.iter().map(text).collect::<Vec<_>>();
    match predicate {
        P::Eq(v) => P::Eq(text(v)),
        P::Neq(v) => P::Neq(text(v)),
        P::WithIn(v) => P::WithIn(texts(v)),
        P::WithOut(v) => P::WithOut(texts(v)),
        P::Lt(v) => P::Lt(order(v)),
        P::Gt(v) => P::Gt(order(v)),
        P::Lte(v) => P::Lte(order(v)),
        P::Gte(v) => P::Gte(order(v)),
        P::Inside(a, b) => P::Inside(order(a), order(b)),
        P::Outside(a, b) => P::Outside(order(a), order(b)),
        P::Between(a, b) => P::Between(order(a), order(b)),
        P::StartingWith(v) => P::StartingWith(order(v)),
        P::NotStartingWith(v) => P::NotStartingWith(order(v)),
        P::EndingWith(v) => P::EndingWith(order(v)),
        P::NotEndingWith(v) => P::NotEndingWith(order(v)),
        P::Containing(v) => P::Containing(order(v)),
        P::NotContaining(v) => P::NotContaining(order(v)),
        P::Regex(r) => P::Regex(r.clone()),
        P::NotRegex(r) => P::NotRegex(r.clone()),
        P::TypeOf(t) => P::TypeOf(t.clone()),
        P::And(a, b) => P::And(
            Box::new(numbers_as_text(a, equality_only)),
            Box::new(numbers_as_text(b, equality_only)),
        ),
        P::Or(a, b) => P::Or(
            Box::new(numbers_as_text(a, equality_only)),
            Box::new(numbers_as_text(b, equality_only)),
        ),
        P::Not(inner) => P::Not(Box::new(numbers_as_text(inner, equality_only))),
    }
}

macro_rules! token_p_api {
    ($method:ident, $token:expr_2021, $doc:literal) => {
        impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
            #[doc = $doc]
            pub fn $method<
                B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
                T: Into<P<B>> + Debug,
            >(
                self,
                predicate: T,
            ) -> Self {
                let token: Token = $token;
                self.push_step(GraphStep::Filter(
                    token.step_name(),
                    Box::new(StepHasTokenP::new(token, predicate.into())),
                ))
            }
        }

        impl __ {
            #[doc = $doc]
            pub fn $method<
                'g,
                S: GraphStorage + 'g,
                B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
                T: Into<P<B>> + Debug,
            >(
                predicate: T,
            ) -> AnonymousTraversal<'g, S> {
                AnonymousTraversal::new().$method(predicate)
            }
        }

        impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
            #[doc = $doc]
            pub fn $method<
                B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
                T: Into<P<B>> + Debug,
            >(
                self,
                predicate: T,
            ) -> Self {
                let token: Token = $token;
                let step = GraphStep::Filter(
                    token.step_name(),
                    Box::new(StepHasTokenP::new(token, predicate.into())),
                );
                self.push_step(step)
            }
        }
    };
}

token_p_api!(
    has_id_p,
    Token::Id,
    "`hasId(P)`: keeps a vertex/edge whose id (a string) satisfies the predicate, e.g. `has_id_p(P::Neq(\"1\"))`.\n\n**Deviation from TinkerPop:** ids are strings. A number operand of an equality predicate (`Eq`, `Neq`, `WithIn`, `WithOut`, also inside `And`/`Or`/`Not`) is compared as its literal text (`P::Eq(3)` matches the id `\"3\"`, `P::Neq(3)` keeps every other element; `3.0` is the text `\"3.0\"`). An order predicate (`Gt`, `Gte`, `Lt`, `Lte`, `Between`, `Inside`, `Outside`) with a number operand fails before the run with `ValueError::NumericIdComparison`; compare with a string (`P::Gt(\"3\")`, string order: `\"10\"` sorts before `\"9\"`)."
);
token_p_api!(
    has_label_p,
    Token::Label,
    "`hasLabel(P)`: keeps a vertex/edge whose label satisfies the predicate. A vertex with several labels satisfies a positive predicate through any label and a negated one (`neq`, `without`) only if all labels do."
);
token_p_api!(
    has_key_p,
    Token::Key,
    "`hasKey(P)`: keeps a property (or a map) whose key satisfies the predicate, e.g. `properties().has_key_p(P::Neq(\"name\"))`."
);

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn has_id_p_on_edges_compares_the_id_text() {
        // modern edge ids are "0".."5": a string comparison keeps "4" and "5"
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_id_p(P::Gt("3")).id().to_list(),
            rhai: [
                r#"g.E().hasId(P.gt("3")).id()"#,
                r#"g.e().has_id(P::gt("3")).id()"#,
            ],
            assert: |res| assert_eq!(res, &["4".into(), "5".into()]),
        );
    }

    #[test]
    fn has_label_p_on_edges_and_in_a_child_traversal() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_label_p(P::Neq("knows")).count().to_list(),
            rhai: [
                r#"g.E().hasLabel(P.neq("knows")).count()"#,
                r#"g.e().has_label(P::neq("knows")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .where_t(__::has_label_p(P::StartingWith("so")))
                .count()
                .to_list(),
            rhai: [r#"g.V().where(__.hasLabel(P.startingWith("so"))).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn has_key_p_reads_the_key_set_of_every_keyed_shape() {
        // elements: any of their keys
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_key_p(P::Eq("weight")).count().to_list(),
            rhai: [r#"g.E().hasKey(P.eq("weight")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_key_p(P::Eq("lang")).count().to_list(),
            rhai: [r#"g.V().hasKey(P.eq("lang")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        // properties: their own key
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .properties(None)
                .where_t(__::has_key_p(P::Neq("name")))
                .count()
                .to_list(),
            rhai: [r#"g.V().properties().where(__.hasKey(P.neq("name"))).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).properties(None).has_key_p(P::Eq("weight")).count().to_list(),
            rhai: [r#"g.E().properties().hasKey(P.eq("weight")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        // a jpath property's key is its materialized path text
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .properties("name".parse::<JsonPath>().unwrap())
                .has_key_p(P::Eq("$.name"))
                .count()
                .to_list(),
            rhai: [r#"g.V("1").properties(jpath("name")).hasKey(P.eq("$.name")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        // maps: any of their keys (a record from valueMap(), an object from project())
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .value_map(None)
                .has_key_p(P::StartingWith("la"))
                .count()
                .to_list(),
            rhai: [r#"g.V().valueMap().hasKey(P.startingWith("la")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .project(vec!["age", "n"])
                .by("age")
                .by("name")
                .has_key_p(P::Eq("n"))
                .count()
                .to_list(),
            rhai: [r#"g.V("1").project("age", "n").by("age").by("name").hasKey(P.eq("n")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn non_matching_shapes_are_cast_errors() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").has_id_p(P::Neq("1")).to_list(),
            rhai: [r#"g.V("1").values("name").hasId(P.neq("1"))"#],
            assert: |err| assert!(err.contains("expected type [vertex or edge]"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").has_label_p(P::Neq("x")).to_list(),
            rhai: [r#"g.V("1").values("name").hasLabel(P.neq("x"))"#],
            assert: |err| assert!(err.contains("expected type [vertex or edge]"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").fold().has_key_p(P::Eq("a")).to_list(),
            rhai: [r#"g.V("1").fold().hasKey(P.eq("a"))"#],
            assert: |err| {
                assert!(err.contains("got type 'array'"), "{err}");
                assert!(err.contains("Scope.local"), "the help names the list remedy: {err}");
            },
        );
    }
}
