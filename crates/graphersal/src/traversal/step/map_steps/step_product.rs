//! `product()`: the Cartesian product of two lists, as `[a, b]` pairs.

use super::list_function::{ListArg, ListFunction, list_function_builders, list_function_step};
use crate::graph::GraphStorage;
use crate::*;

list_function_builders!(
    ListFunction::Product,
    product,
    product_traversal,
    "`product(list)` (TinkerPop `ProductStep`): the Cartesian product of the list a traverser \
     holds and `list`: one two-element list `[a, b]` per pair, `a` from the incoming list (outer \
     loop), `b` from `list` (inner loop), duplicates kept (`[\"a\", \"b\"]` and `[1, 2]` give \
     `[[\"a\", 1], [\"a\", 2], [\"b\", 1], [\"b\", 2]]`). An empty side gives an empty list.",
    "let pairs = g.v(None).values(\"age\").fold().product(vec![1, 2]).next().unwrap().unwrap();\n\
     assert_eq!(pairs.as_array().map(|a| a.len()), Some(8));"
);

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn pairs_in_order() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![vec!["a", "b"]]).product(vec![1, 2]).unfold().to_list(),
            rhai: [r#"g.inject(["a", "b"]).product([1, 2]).unfold()"#],
            assert: |res| assert_eq!(
                res,
                &[
                    ElementProperty::Array(vec!["a".into(), 1.into()]),
                    ElementProperty::Array(vec!["a".into(), 2.into()]),
                    ElementProperty::Array(vec!["b".into(), 1.into()]),
                    ElementProperty::Array(vec!["b".into(), 2.into()]),
                ]
            ),
        );
    }

    #[test]
    fn traversal_argument_and_empty_side() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .values("nonexistent")
                .fold()
                .product_traversal(__::v(None).values("name").fold())
                .to_list(),
            rhai: [
                r#"g.V().values("nonexistent").fold().product(__.V().values("name").fold())"#,
                r#"g.v().values("nonexistent").fold().product(__.v().values("name").fold())"#,
            ],
            assert: |res| assert_eq!(res, &[ElementProperty::Array(vec![])]),
        );
    }
}
