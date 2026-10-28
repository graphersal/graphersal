//! `difference()`: the distinct elements of the incoming list that are not in a second list.

use super::list_function::{ListArg, ListFunction, list_function_builders, list_function_step};
use crate::graph::GraphStorage;
use crate::*;

list_function_builders!(
    ListFunction::Difference,
    difference,
    difference_traversal,
    "`difference(list)` (TinkerPop `DifferenceStep`): the elements of the list a traverser holds \
     that are not in `list`, each at most once, in first-occurrence order (a set: \
     `[\"a\", null, \"b\", \"a\"]` minus `[\"a\", \"c\"]` is `[null, \"b\"]`).",
    "let rest = g.v(None).values(\"age\").fold().difference(vec![27, 29]).next().unwrap().unwrap();\n\
     assert_eq!(rest, ElementProperty::from(vec![32, 35]));"
);

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn removes_the_argument_and_duplicates() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Array(vec![
                    ElementProperty::from("a"),
                    ElementProperty::Null,
                    ElementProperty::from("b"),
                    ElementProperty::from("b"),
                ])])
                .difference(vec!["a", "c"])
                .to_list(),
            rhai: [r#"g.inject(["a", (), "b", "b"]).difference(["a", "c"])"#],
            assert: |res| assert_eq!(
                res,
                &[ElementProperty::Array(vec![ElementProperty::Null, ElementProperty::from("b")])]
            ),
        );
    }

    #[test]
    fn traversal_argument_and_vertex_identity() {
        // marko's friends (vadas, josh) minus josh: the elements stay vertices and compare by
        // identity.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .out("knows")
                .fold()
                .difference_traversal(__::v("4").fold())
                .unfold()
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V("1").out("knows").fold().difference(__.V("4").fold()).unfold().values("name")"#,
                r#"g.v("1").out("knows").fold().difference(__.v("4").fold()).unfold().values("name")"#,
            ],
            assert: |res| assert_eq!(res, &["vadas".into()]),
        );
    }

    #[test]
    fn a_map_argument_is_rejected() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .fold()
                .difference(one_entry_map())
                .to_list(),
            rhai: [r#"g.V().fold().difference(#{k: "v"})"#],
            assert: |err| assert!(err.contains("takes a list as its argument, but got map"), "{err}"),
        );
    }

    fn one_entry_map() -> ElementProperty {
        ElementProperty::Object(Box::new(
            [("k", ElementProperty::from("v"))].into_iter().collect(),
        ))
    }
}
