//! `merge()`: the union of two lists, or two maps merged.

use super::list_function::{ListArg, ListFunction, list_function_builders, list_function_step};
use crate::graph::GraphStorage;
use crate::*;

list_function_builders!(
    ListFunction::Merge,
    merge,
    merge_traversal,
    "`merge(list | map)` (TinkerPop `MergeStep`): for a list, the union of the list a traverser \
     holds and `list`, each element at most once, incoming elements first (`[\"a\", null, \"b\"]` \
     and `[\"a\", \"c\"]` give `[\"a\", null, \"b\", \"c\"]`). For a map (`project()`, \
     `elementMap()`, a map constant), the incoming entries with the argument's entries added or \
     overriding them; the argument must then be a map, and a map argument for a list is an error.",
    "let names = g.v(None).values(\"name\").fold().merge(vec![\"marko\", \"dave\"]).next().unwrap().unwrap();\n\
     assert_eq!(names.as_array().map(|a| a.len()), Some(7));"
);

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> ElementProperty {
        ElementProperty::Object(Box::new(
            entries
                .iter()
                .map(|(k, v)| ((*k), ElementProperty::from(*v)))
                .collect(),
        ))
    }

    #[test]
    fn list_union() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Array(vec![
                    ElementProperty::from("a"),
                    ElementProperty::Null,
                    ElementProperty::from("b"),
                ])])
                .merge(vec!["a", "c"])
                .to_list(),
            rhai: [r#"g.inject(["a", (), "b"]).merge(["a", "c"])"#],
            assert: |res| assert_eq!(
                res,
                &[ElementProperty::Array(vec![
                    ElementProperty::from("a"),
                    ElementProperty::Null,
                    ElementProperty::from("b"),
                    ElementProperty::from("c"),
                ])]
            ),
        );
    }

    #[test]
    fn map_merge_overrides_with_the_argument() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .project(vec!["name", "kind"])
                .by("name")
                .by(__::constant("person"))
                .merge(map(&[("kind", "friend"), ("other", "blueprint")]))
                .to_list(),
            rhai: [
                r#"g.V("1").project("name", "kind").by("name").by(__.constant("person")).merge(#{kind: "friend", other: "blueprint"})"#,
            ],
            assert: |res| assert_eq!(
                res,
                &[map(&[("name", "marko"), ("kind", "friend"), ("other", "blueprint")])]
            ),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .element_map(None)
                .merge_traversal(__::v("3").element_map(None))
                .to_list(),
            rhai: [
                r#"g.V("1").elementMap().merge(__.V("3").elementMap())"#,
            ],
            assert: |res| {
                let m = res[0].as_object().expect("a map");
                assert_eq!(m.get("id"), Some(&ElementProperty::from("3")));
                assert_eq!(m.get("name"), Some(&ElementProperty::from("lop")));
                assert_eq!(m.get("age"), Some(&ElementProperty::from(29)));
                assert_eq!(m.get("lang"), Some(&ElementProperty::from("java")));
            },
        );
    }

    #[test]
    fn list_and_map_do_not_mix() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().merge(map(&[("k", "v")])).to_list(),
            rhai: [r#"g.V().fold().merge(#{k: "v"})"#],
            assert: |err| assert!(err.contains("takes a list as its argument, but got map"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .element_map(None)
                .merge_traversal(__::constant("a"))
                .to_list(),
            rhai: [r#"g.V().elementMap().merge(__.constant("a"))"#],
            assert: |err| assert!(err.contains("takes a map as its traversal argument, but got string"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").merge(vec!["a"]).to_list(),
            rhai: [r#"g.V().values("name").merge(["a"])"#],
            assert: |err| assert!(err.contains("takes a list or map as its incoming value, but got string"), "{err}"),
        );
    }
}
