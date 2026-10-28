//! `disjunct()`: the distinct elements that are in exactly one of two lists.

use super::list_function::{ListArg, ListFunction, list_function_builders, list_function_step};
use crate::graph::GraphStorage;
use crate::*;

list_function_builders!(
    ListFunction::Disjunct,
    disjunct,
    disjunct_traversal,
    "`disjunct(list)` (TinkerPop `DisjunctStep`): the symmetric difference of the list a \
     traverser holds and `list`, each element at most once: first the incoming elements that are \
     not in `list`, then the elements of `list` that are not in the incoming list \
     (`[\"a\", null, \"b\"]` and `[\"a\", \"c\"]` give `[null, \"b\", \"c\"]`).",
    "let odd = g.v(None).values(\"age\").fold().disjunct(vec![27, 40]).next().unwrap().unwrap();\n\
     assert_eq!(odd, ElementProperty::from(vec![29, 32, 35, 40]));"
);

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn symmetric_difference_with_a_constant() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Array(vec![
                    ElementProperty::from("a"),
                    ElementProperty::Null,
                    ElementProperty::from("b"),
                ])])
                .disjunct(ElementProperty::Array(vec![
                    ElementProperty::from("a"),
                    ElementProperty::Null,
                    ElementProperty::from("c"),
                    ElementProperty::from("c"),
                ]))
                .to_list(),
            rhai: [r#"g.inject(["a", (), "b"]).disjunct(["a", (), "c", "c"])"#],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec!["b", "c"])]),
        );
    }

    #[test]
    fn with_a_traversal_argument() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .values("age")
                .fold()
                .disjunct_traversal(__::v(None).values("age").fold())
                .to_list(),
            rhai: [
                r#"g.V().values("age").fold().disjunct(__.V().values("age").fold())"#,
                r#"g.v().values("age").fold().disjunct(__.v().values("age").fold())"#,
            ],
            assert: |res| assert_eq!(res, &[ElementProperty::Array(vec![])]),
        );
    }
}
