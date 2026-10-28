//! `combine()`: the incoming list followed by a second list, duplicates kept.

use super::list_function::{ListArg, ListFunction, list_function_builders, list_function_step};
use crate::graph::GraphStorage;
use crate::*;

list_function_builders!(
    ListFunction::Combine,
    combine,
    combine_traversal,
    "`combine(list)` (TinkerPop `CombineStep`): the list a traverser holds followed by `list`, \
     duplicates and order kept (`[\"a\", \"b\"]` combined with `[\"b\", \"c\"]` is \
     `[\"a\", \"b\", \"b\", \"c\"]`).",
    "let all = g.v(None).values(\"name\").fold().combine(vec![\"dave\"]).next().unwrap().unwrap();\n\
     assert_eq!(all.as_array().map(|a| a.len()), Some(7));"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn appends_a_constant_and_keeps_duplicates() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![vec!["a", "b"]]).combine(vec!["b", "c"]).to_list(),
            rhai: [
                r#"g.inject(["a", "b"]).combine(["b", "c"])"#,
            ],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec!["a", "b", "b", "c"])]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_traversal_argument_is_its_first_result() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .values("age")
                .order()
                .fold()
                .combine_traversal(__::constant(27).fold())
                .to_list(),
            rhai: [
                r#"g.V().values("age").order().fold().combine(__.constant(27).fold())"#,
                r#"g.v().values("age").order().fold().combine(__.constant(27).fold())"#,
            ],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec![27, 29, 32, 35, 27])]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_path_is_a_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out("created").path().by("name").combine(vec!["x"]).to_list(),
            rhai: [r#"g.V("1").out("created").path().by("name").combine(["x"])"#],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec!["marko", "lop", "x"])]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn vertices_stay_vertices() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .fold()
                .combine_traversal(__::v("2").fold())
                .unfold()
                .values("name")
                .to_list(),
            rhai: [r#"g.V("1").fold().combine(__.V("2").fold()).unfold().values("name")"#],
            assert: |res| assert_eq!(res, &["marko".into(), "vadas".into()]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn rejects_null_and_non_lists() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![ElementProperty::Null]).combine(vec![1]).to_list(),
            rhai: [r#"g.inject(()).combine([1])"#],
            assert: |err| assert!(err.contains("incoming value") && err.contains("null"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").combine(vec![1]).count().to_list(),
            rhai: [r#"g.V().values("name").combine([1]).count()"#],
            assert: |err| assert!(err.contains("takes a list as its incoming value"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().combine(2).to_list(),
            rhai: [r#"g.V().fold().combine(2)"#],
            assert: |err| assert!(err.contains("as its argument, but got int64"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().combine_traversal(__::v(None)).to_list(),
            rhai: [r#"g.V().fold().combine(__.V())"#],
            assert: |err| assert!(err.contains("traversal argument, but got vertex"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().combine_traversal(__::constant(ElementProperty::Null)).to_list(),
            rhai: [r#"g.V().fold().combine(__.constant(()))"#],
            assert: |err| assert!(err.contains("but got null"), "{err}"),
        );
    }

    #[test]
    fn profile_names_the_step() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let metrics = lock
            .traversal()
            .v(None)
            .values("name")
            .fold()
            .combine_traversal(__::v(None).values("name").fold())
            .profile()
            .unwrap();
        assert!(
            metrics.to_string().contains("combine(__.v().values"),
            "{metrics}"
        );
    }
}
