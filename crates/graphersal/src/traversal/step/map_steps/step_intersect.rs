//! `intersect()`: the distinct elements of the incoming list that are also in a second list.

use super::list_function::{ListArg, ListFunction, list_function_builders, list_function_step};
use crate::graph::GraphStorage;
use crate::*;

list_function_builders!(
    ListFunction::Intersect,
    intersect,
    intersect_traversal,
    "`intersect(list)` (TinkerPop `IntersectStep`): the elements of the list a traverser holds \
     that are also in `list`, each at most once, in first-occurrence order (`null` is an element \
     like any other: `[\"a\", null, \"b\"]` and `[\"a\", null, \"c\"]` give `[\"a\", null]`).",
    "let common = g.v(None).values(\"age\").fold().intersect(vec![27, 40]).next().unwrap().unwrap();\n\
     assert_eq!(common, ElementProperty::from(vec![27]));"
);

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn keeps_common_elements_once() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Array(vec![
                    ElementProperty::from("a"),
                    ElementProperty::Null,
                    ElementProperty::from("a"),
                    ElementProperty::from("b"),
                ])])
                .intersect(ElementProperty::Array(vec![
                    ElementProperty::from("a"),
                    ElementProperty::Null,
                    ElementProperty::from("c"),
                ]))
                .to_list(),
            rhai: [r#"g.inject(["a", (), "a", "b"]).intersect(["a", (), "c"])"#],
            assert: |res| assert_eq!(
                res,
                &[ElementProperty::Array(vec![ElementProperty::from("a"), ElementProperty::Null])]
            ),
        );
    }

    #[test]
    fn with_a_traversal_argument_inside_local() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![vec!["marko", "dave"]])
                .local(__::intersect_traversal(__::v(None).values("name").fold()))
                .to_list(),
            rhai: [
                r#"g.inject(["marko", "dave"]).local(__.intersect(__.V().values("name").fold()))"#,
            ],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec!["marko"])]),
        );
    }
}
