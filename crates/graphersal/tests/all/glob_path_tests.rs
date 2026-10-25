//! Integration tests for the `glob_path(pattern)` step (a Gremlin extension), all on the
//! `file_tree` sample graph. Results are asserted by vertex id; ids are paths from `root`.
//!
//! Every query runs through the Rust API and, with the `script` feature, through Rhai in both
//! DSL (`glob_path`; a Graphersal extension, snake_case only), which must return the same ids.

use graphersal::prelude::*;

/// The ids of a result list, sorted.
fn sorted_ids(values: Vec<ElementProperty>) -> Vec<String> {
    let mut ids: Vec<String> = values
        .into_iter()
        .map(|value| match value {
            ElementProperty::String(id) => id.to_string(),
            other => panic!("expected a string id, got {other:?}"),
        })
        .collect();
    ids.sort();
    ids
}

/// Runs a Rhai query ending on a traversal against `file_tree` and returns its sorted ids.
#[cfg(feature = "script")]
fn rhai_ids(script: &str) -> Vec<String> {
    use std::sync::Arc;
    let graph = Arc::new(GraphSource::file_tree());
    let engine = crate::script_host::engine();
    let mut scope = crate::script_host::graph_scope(graph.clone());
    let gts = engine
        .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
        .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
    let lock = graph.read();
    let values = gts
        .ast_convert_to_gts(&*lock)
        .to_list()
        .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
    sorted_ids(values)
}

/// Asserts that the Rust query and the Rhai `script` (written with `glob_path`, and run again
/// all return exactly `expected` (as a set of ids,
/// with multiplicity).
fn dual<F>(rust: F, script: &str, expected: &[&str])
where
    F: for<'b> FnOnce(
        GraphTraversalSource<'b, &'b TraversalGraph>,
    ) -> TraverserResult<Vec<ElementProperty>>,
{
    let mut expected: Vec<String> = expected.iter().map(|id| id.to_string()).collect();
    expected.sort();

    let graph = TraversalGraph::file_tree();
    let rust_ids =
        sorted_ids(rust(graph.traversal()).unwrap_or_else(|e| panic!("Rust query failed: {e}")));
    assert_eq!(rust_ids, expected, "Rust API result");

    #[cfg(feature = "script")]
    {
        assert_eq!(rhai_ids(script), expected, "Rhai result of `{script}`");
    }
    #[cfg(not(feature = "script"))]
    let _ = script;
}

/// Every vertex of `file_tree` except `root`, with or without the symlinks.
fn all_below_root(with_symlinks: bool) -> Vec<String> {
    let graph = TraversalGraph::file_tree();
    let mut ids = sorted_ids(graph.traversal().v(None).id().to_list().unwrap());
    ids.retain(|id| {
        id != "root" && (with_symlinks || (id != "root/latest" && id != "root/y/loop"))
    });
    ids
}

/// Asserts that `err` is rooted in `InvalidModulator` for `glob_path` at `position`.
fn assert_invalid_modulator(err: &TraverserError, position: usize) {
    match err.root_cause() {
        TraverserError::InvalidModulator {
            step,
            position: actual,
            ..
        } => {
            assert_eq!(step, "glob_path", "wrong step in: {err}");
            assert_eq!(*actual, position, "wrong position in: {err}");
        }
        other => panic!("expected InvalidModulator, got: {other}"),
    }
}

const RS_BELOW_AUTHOR: [&str; 3] = [
    "root/author/x/some/f.rs",
    "root/author/q/author/w/some/h.rs",
    "root/y/author/z/some/g.rs",
];

#[test]
fn design_example_matches_the_three_files() {
    dual(
        |g| {
            g.v("root")
                .glob_path("**/author/*/some/*.rs")
                .by("name")
                .by(["directory", "file"])
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("**/author/*/some/*.rs").by("name").by(["directory","file"]).id()"#,
        &RS_BELOW_AUTHOR,
    );
}

#[test]
fn double_star_leading_inner_and_trailing() {
    dual(
        |g| {
            g.v("root")
                .glob_path("**/f.rs")
                .by("name")
                .by(["directory", "file"])
                .id()
                .to_list()
        },
        r#"g.V("root").glob_path("**/f.rs").by("name").by(["directory", "file"]).id()"#,
        &["root/author/x/some/f.rs"],
    );
    dual(
        |g| {
            g.v("root")
                .glob_path("author/**/h.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.V("root").glob_path("author/**/h.rs").by("name").id()"#,
        &["root/author/q/author/w/some/h.rs"],
    );
    let src_below = [
        "root/src/main.rs",
        "root/src/lib.rs",
        "root/src/test_a.rs",
        "root/src/test_b.rs",
        "root/src/util",
        "root/src/util/mod.rs",
        "root/src/util/io.rs",
    ];
    dual(
        |g| g.v("root").glob_path("src/**").by("name").id().to_list(),
        r#"g.V("root").glob_path("src/**").by("name").id()"#,
        &src_below,
    );
    // `**/**` collapses into one `**`.
    dual(
        |g| g.v("root").glob_path("src/**/**").by("name").id().to_list(),
        r#"g.V("root").glob_path("src/**/**").by("name").id()"#,
        &src_below,
    );
    dual(
        |g| {
            g.v("root")
                .glob_path("**/**/f.rs")
                .by("name")
                .by(["directory", "file"])
                .id()
                .to_list()
        },
        r#"g.V("root").glob_path("**/**/f.rs").by("name").by(["directory", "file"]).id()"#,
        &["root/author/x/some/f.rs"],
    );
}

#[test]
fn double_star_alone_is_every_descendant_but_never_the_start() {
    let expected = all_below_root(false);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    dual(
        |g| {
            g.v("root")
                .glob_path("**")
                .by("name")
                .by(["directory", "file"])
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("**").by("name").by(["directory", "file"]).id()"#,
        &expected,
    );

    // Without the filter the walk follows `root/y/loop` back to `root`: it terminates, emits
    // every other vertex once, and still never emits the start.
    let expected = all_below_root(true);
    assert_eq!(expected.len(), 36);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    dual(
        |g| g.v("root").glob_path("**").by("name").id().to_list(),
        r#"g.v("root").glob_path("**").by("name").id()"#,
        &expected,
    );
}

#[test]
fn every_glob_construct() {
    let cases: [(&str, &[&str]); 8] = [
        (
            "src/test_?.rs",
            &["root/src/test_a.rs", "root/src/test_b.rs"],
        ),
        (
            "src/test_[ab].rs",
            &["root/src/test_a.rs", "root/src/test_b.rs"],
        ),
        ("src/test_[!a].rs", &["root/src/test_b.rs"]),
        (
            "src/{main,lib}.rs",
            &["root/src/main.rs", "root/src/lib.rs"],
        ),
        (
            "src/util/*.rs",
            &["root/src/util/mod.rs", "root/src/util/io.rs"],
        ),
        (r"docs/data\[1\].csv", &["root/docs/data[1].csv"]),
        ("docs/r?sum?.md", &["root/docs/résumé.md"]),
        (r"a\/b/c.rs", &["root/#slash/c.rs"]),
    ];
    for (pattern, expected) in cases {
        let rhai_pattern = pattern.replace('\\', "\\\\");
        dual(
            |g| g.v("root").glob_path(pattern).by("name").id().to_list(),
            &format!(r#"g.v("root").glob_path("{rhai_pattern}").by("name").id()"#),
            expected,
        );
    }
}

#[test]
fn missing_or_non_string_keys_never_match_a_named_segment() {
    // `root/#unnamed` has no name: `*` passes it through without reading the key ...
    dual(
        |g| {
            g.v("root")
                .glob_path("*/orphan.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("*/orphan.rs").by("name").id()"#,
        &["root/#unnamed/orphan.rs"],
    );
    // ... but a named segment, even one that matches every string, does not match it.
    dual(
        |g| {
            g.v("root")
                .glob_path("?*/orphan.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("?*/orphan.rs").by("name").id()"#,
        &[],
    );
    // `root/#42` has the int64 name 42: never stringified.
    dual(
        |g| g.v("root").glob_path("42/*").by("name").id().to_list(),
        r#"g.v("root").glob_path("42/*").by("name").id()"#,
        &[],
    );
    dual(
        |g| {
            g.v("root")
                .glob_path("*/answer.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("*/answer.rs").by("name").id()"#,
        &["root/#42/answer.rs"],
    );
}

#[test]
fn match_key_by_id() {
    // A segment compared with full path ids: `*` also matches '/' inside one value.
    dual(
        |g| {
            g.v("root")
                .glob_path(r"**/*\/util\/*")
                .by(By::Id)
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("**/*\\/util\\/*").by(By.Id).id()"#,
        &["root/src/util/mod.rs", "root/src/util/io.rs"],
    );
    dual(
        |g| {
            g.v("root")
                .glob_path(r"root\/src/root\/src\/main.rs")
                .by(By::Id)
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("root\\/src/root\\/src\\/main.rs").by(By.Id).id()"#,
        &["root/src/main.rs"],
    );
}

#[test]
fn match_key_by_label() {
    dual(
        |g| {
            g.v("root")
                .glob_path("**/symlink")
                .by(By::Label)
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("**/symlink").by(By.Label).id()"#,
        &["root/latest", "root/y/loop"],
    );
    dual(
        |g| {
            g.v("root")
                .glob_path("directory/file")
                .by(By::Label)
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("directory/file").by(By.Label).id()"#,
        &[
            "root/src/main.rs",
            "root/src/lib.rs",
            "root/src/test_a.rs",
            "root/src/test_b.rs",
            "root/docs/guide.md",
            "root/docs/notes.txt",
            "root/docs/data[1].csv",
            "root/docs/résumé.md",
            "root/#unnamed/orphan.rs",
            "root/#42/answer.rs",
            "root/#slash/c.rs",
        ],
    );
}

#[test]
fn a_named_label_segment_matches_any_of_several_labels() {
    let mut graph = TraversalGraph::new();
    let root = graph
        .add_vertex(Some("r".into()), vec!["dir".into()], PropertyMap::default())
        .unwrap();
    let child = graph
        .add_vertex(
            Some("r/c".into()),
            vec!["file".into(), "hidden".into()],
            PropertyMap::default(),
        )
        .unwrap();
    graph
        .add_edge(
            None,
            Some("contains".into()),
            root,
            child,
            PropertyMap::default(),
        )
        .unwrap();
    for pattern in ["file", "hidden", "{x,hidden}"] {
        let ids = sorted_ids(
            graph
                .traversal()
                .v("r")
                .glob_path(pattern)
                .by(By::Label)
                .id()
                .to_list()
                .unwrap(),
        );
        assert_eq!(ids, vec!["r/c".to_string()], "pattern {pattern}");
    }
    let ids = graph
        .traversal()
        .v("r")
        .glob_path("dir")
        .by(By::Label)
        .id()
        .to_list()
        .unwrap();
    assert!(ids.is_empty());
}

#[test]
fn label_filter_excludes_symlinks_and_keeps_the_walk_a_strict_tree() {
    dual(
        |g| {
            g.v("root")
                .glob_path("**/loop")
                .by("name")
                .by(["directory", "file"])
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("**/loop").by("name").by(["directory", "file"]).id()"#,
        &[],
    );
    dual(
        |g| g.v("root").glob_path("**/loop").by("name").id().to_list(),
        r#"g.v("root").glob_path("**/loop").by("name").id()"#,
        &["root/y/loop"],
    );
    // Through the `root/latest` symlink `root/author/*` is reachable a second time.
    dual(
        |g| {
            g.v("root")
                .glob_path("latest/*")
                .by("name")
                .by(["directory", "file"])
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("latest/*").by("name").by(["directory", "file"]).id()"#,
        &[],
    );
    dual(
        |g| g.v("root").glob_path("latest/*").by("name").id().to_list(),
        r#"g.v("root").glob_path("latest/*").by("name").id()"#,
        &["root/author"],
    );
}

#[test]
fn cycles_terminate_and_diamonds_yield_no_duplicates() {
    // `root/author` is reachable directly and via `root/latest`; `root` again via `root/y/loop`.
    dual(
        |g| g.v("root").glob_path("**/f.rs").by("name").id().to_list(),
        r#"g.v("root").glob_path("**/f.rs").by("name").id()"#,
        &["root/author/x/some/f.rs"],
    );
    dual(
        |g| {
            g.v("root")
                .glob_path("**/author/*/some/*.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("**/author/*/some/*.rs").by("name").id()"#,
        &RS_BELOW_AUTHOR,
    );
    // Different splits of two `**` reach the same vertex; it is still emitted once.
    dual(
        |g| {
            g.v("root")
                .glob_path("**/author/**/*.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("**/author/**/*.rs").by("name").id()"#,
        &RS_BELOW_AUTHOR,
    );
}

#[test]
fn several_incoming_traversers_are_independent() {
    dual(
        |g| {
            g.v(vec!["root/author", "root/y"])
                .glob_path("**/*.rs")
                .by("name")
                .by(["directory", "file"])
                .id()
                .to_list()
        },
        r#"g.v(["root/author", "root/y"]).glob_path("**/*.rs").by("name").by(["directory", "file"]).id()"#,
        &RS_BELOW_AUTHOR,
    );
    // Each start emits its own matches, so `f.rs` appears once per start.
    dual(
        |g| {
            g.v(vec!["root", "root/author"])
                .glob_path("**/f.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.v(["root", "root/author"]).glob_path("**/f.rs").by("name").id()"#,
        &["root/author/x/some/f.rs", "root/author/x/some/f.rs"],
    );
}

#[test]
fn chaining_and_child_traversals() {
    dual(
        |g| {
            g.v("root")
                .glob_path("src")
                .by("name")
                .glob_path("**/*.rs")
                .by("name")
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("src").by("name").glob_path("**/*.rs").by("name").id()"#,
        &[
            "root/src/main.rs",
            "root/src/lib.rs",
            "root/src/test_a.rs",
            "root/src/test_b.rs",
            "root/src/util/mod.rs",
            "root/src/util/io.rs",
        ],
    );
    dual(
        |g| {
            g.v("root")
                .glob_path("*")
                .by("name")
                .by(["directory"])
                .where_t(__::glob_path("*.md").by("name"))
                .id()
                .to_list()
        },
        r#"g.v("root").glob_path("*").by("name").by(["directory"]).where(__.glob_path("*.md").by("name")).id()"#,
        &["root/docs"],
    );
    dual(
        |g| {
            g.v(vec!["root/src", "root/docs"])
                .local(__::glob_path("{*.md,util}").by("name"))
                .id()
                .to_list()
        },
        r#"g.v(["root/src", "root/docs"]).local(__.glob_path("{*.md,util}").by("name")).id()"#,
        &["root/src/util", "root/docs/guide.md", "root/docs/résumé.md"],
    );
}

#[test]
fn path_is_start_and_match() {
    let graph = TraversalGraph::file_tree();
    let paths = graph
        .traversal()
        .v("root")
        .glob_path("src/util/io.rs")
        .by("name")
        .path()
        .by(By::Id)
        .to_list()
        .unwrap();
    assert_eq!(
        paths,
        vec![ElementProperty::Array(vec![
            "root".into(),
            "root/src/util/io.rs".into()
        ])]
    );
}

#[test]
fn profile_renders_the_step_with_its_modulators() {
    let graph = TraversalGraph::file_tree();
    let stats = graph
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by("name")
        .by(["directory", "file"])
        .profile()
        .unwrap();
    let node = stats
        .metrics()
        .iter()
        .find(|node| node.name().starts_with("glob_path("))
        .unwrap_or_else(|| panic!("no glob_path node in {stats}"));
    assert_eq!(
        node.name(),
        r#"glob_path("**/*.rs").by("name").by(["directory","file"])"#
    );
    assert_eq!(node.count_in(), 1);
    assert_eq!(node.counts().traverser_count(), 12);
    assert!(
        stats
            .to_string()
            .contains(r#"glob_path("**/*.rs").by("name")"#),
        "{stats}"
    );

    let stats = graph
        .traversal()
        .v("root")
        .glob_path("**")
        .by(By::Id)
        .profile()
        .unwrap();
    assert!(
        stats
            .metrics()
            .iter()
            .any(|node| node.name() == r#"glob_path("**").by(T.id)"#),
        "{stats}"
    );
}

#[test]
fn every_invalid_by_fails_with_invalid_modulator() {
    let graph = TraversalGraph::file_tree();
    let g = || graph.traversal().v("root");

    let err = g().glob_path("*").to_list().expect_err("no by()");
    assert_invalid_modulator(&err, 1);
    let err = g()
        .glob_path("*")
        .by(["directory"])
        .to_list()
        .expect_err("label list first");
    assert_invalid_modulator(&err, 1);
    let err = g()
        .glob_path("*")
        .by(__::values("name"))
        .to_list()
        .expect_err("traversal key");
    assert_invalid_modulator(&err, 1);
    let err = g()
        .glob_path("*")
        .by(graphersal::prelude::Order::Asc)
        .to_list()
        .expect_err("order key");
    assert_invalid_modulator(&err, 1);
    let err = g()
        .glob_path("*")
        .by(By::Value)
        .to_list()
        .expect_err("value key");
    assert_invalid_modulator(&err, 1);
    let err = g()
        .glob_path("*")
        .by(By::Count)
        .to_list()
        .expect_err("count key");
    assert_invalid_modulator(&err, 1);
    let err = g()
        .glob_path("*")
        .by("name")
        .by("name")
        .to_list()
        .expect_err("second by() is a key");
    assert_invalid_modulator(&err, 2);
    let err = g()
        .glob_path("*")
        .by("name")
        .by(By::Label)
        .to_list()
        .expect_err("second by() is T.label");
    assert_invalid_modulator(&err, 2);
    let err = g()
        .glob_path("*")
        .by("name")
        .by(__::values("name"))
        .to_list()
        .expect_err("second by() is a traversal");
    assert_invalid_modulator(&err, 2);
    let err = g()
        .glob_path("*")
        .by("name")
        .by(Vec::<&str>::new())
        .to_list()
        .expect_err("empty label list");
    assert_invalid_modulator(&err, 2);
    let err = g()
        .glob_path("*")
        .by("name")
        .by(["file"])
        .by("name")
        .to_list()
        .expect_err("third by()");
    assert_invalid_modulator(&err, 3);
}

#[test]
fn an_invalid_pattern_fails_with_invalid_glob_pattern() {
    let graph = TraversalGraph::file_tree();
    let err = graph
        .traversal()
        .v("root")
        .glob_path("a**b")
        .by("name")
        .to_list()
        .expect_err("'**' must be a whole segment");
    match err.root_cause() {
        TraverserError::Value(ValueError::InvalidGlobPattern {
            pattern, position, ..
        }) => {
            assert_eq!(pattern, "a**b");
            assert_eq!(*position, 1);
        }
        other => panic!("expected InvalidGlobPattern, got: {other}"),
    }
}

#[cfg(feature = "script")]
#[test]
fn rhai_errors_surface_as_the_same_variants() {
    use std::sync::Arc;
    let run = |script: &str| -> TraverserResult<Vec<ElementProperty>> {
        let graph = Arc::new(GraphSource::file_tree());
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
        let lock = graph.read();
        gts.ast_convert_to_gts(&*lock).to_list()
    };
    let cases = [
        (r#"g.v("root").glob_path("*")"#, 1),
        (r#"g.v("root").glob_path("*").by(["file"])"#, 1),
        (r#"g.v("root").glob_path("*").by(__.values("name"))"#, 1),
        (r#"g.v("root").glob_path("*").by("name").by("name")"#, 2),
        (r#"g.v("root").glob_path("*").by("name").by([])"#, 2),
        (
            r#"g.v("root").glob_path("*").by("name").by(["file"]).by(By.Id)"#,
            3,
        ),
    ];
    for (script, position) in cases {
        let err = run(script).expect_err(script);
        assert_invalid_modulator(&err, position);
    }
    let err = run(r#"g.v("root").glob_path("src/[z-a]").by("name")"#).expect_err("inverted range");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::InvalidGlobPattern { .. })
        ),
        "got {err}"
    );
}

#[test]
fn a_non_vertex_input_fails_with_the_cast_exception() {
    let graph = TraversalGraph::file_tree();
    let err = graph
        .traversal()
        .v("root")
        .out_e(None)
        .glob_path("*")
        .by("name")
        .to_list()
        .expect_err("an edge is no start vertex");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::CastException { .. })
        ),
        "got {err}"
    );
    let err = graph
        .traversal()
        .v("root")
        .values("name")
        .as_string()
        .glob_path("*")
        .by("name")
        .to_list()
        .expect_err("a computed string is no start vertex");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::CastException { .. })
        ),
        "got {err}"
    );
    // Like `out()`, a raw `values(key)` reference stands for its vertex.
    let ids = sorted_ids(
        graph
            .traversal()
            .v("root")
            .values("name")
            .glob_path("src")
            .by("name")
            .id()
            .to_list()
            .unwrap(),
    );
    assert_eq!(ids, vec!["root/src".to_string()]);
}

#[test]
fn evaluation_timeout_interrupts_a_long_walk() {
    // A chain of 200k vertices; `**/nothing` reads every name and matches none.
    const LEN: usize = 200_000;
    let mut graph = TraversalGraph::new();
    let mut previous = None;
    for i in 0..LEN {
        let mut props = PropertyMap::default();
        props.insert("name", format!("n{i}").into());
        let vertex = graph
            .add_vertex(
                Some(format!("c{i}").into()),
                vec!["directory".into()],
                props,
            )
            .unwrap();
        if let Some(previous) = previous {
            graph
                .add_edge(
                    None,
                    Some("contains".into()),
                    previous,
                    vertex,
                    PropertyMap::default(),
                )
                .unwrap();
        }
        previous = Some(vertex);
    }
    let err = graph
        .traversal()
        .with("evaluationTimeout", 1)
        .v("c0")
        .glob_path("**/nothing")
        .by("name")
        .to_list()
        .expect_err("1 ms is not enough for a 200k-vertex walk");
    assert!(
        matches!(err.root_cause(), TraverserError::Timeout { .. }),
        "expected a timeout, got {err:?}"
    );
    // Without a budget the same walk completes, with no match.
    let ids = graph
        .traversal()
        .v("c0")
        .glob_path("**/nothing")
        .by("name")
        .to_list()
        .unwrap();
    assert!(ids.is_empty());
}

/// The match key of a count-parity case.
#[derive(Clone, Copy, Debug)]
enum Key {
    Name,
    Id,
    Label,
}

/// Asserts that `glob_path(pattern).by(key)[.by(["directory","file"])].count()` from `starts`
/// equals `to_list().len()` of the same traversal, with and without the label filter, and that
/// the list holds no duplicate (D7). Returns the unfiltered count.
fn assert_count_equals_list_len(
    graph: &TraversalGraph,
    starts: &[&str],
    pattern: &str,
    key: Key,
) -> i64 {
    fn by<'g>(
        t: GraphTraversalSource<'g, &'g TraversalGraph>,
        key: Key,
    ) -> GraphTraversalSource<'g, &'g TraversalGraph> {
        match key {
            Key::Name => t.by("name"),
            Key::Id => t.by(By::Id),
            Key::Label => t.by(By::Label),
        }
    }
    let starts: Vec<&str> = starts.to_vec();
    let mut unfiltered = 0;
    for filtered in [false, true] {
        let build = || {
            let t = by(graph.traversal().v(starts.clone()).glob_path(pattern), key);
            if filtered {
                t.by(["directory", "file"])
            } else {
                t
            }
        };
        let list = build()
            .id()
            .to_list()
            .unwrap_or_else(|e| panic!("{pattern}: {e}"));
        let count = build()
            .count()
            .next()
            .unwrap_or_else(|e| panic!("{pattern}: {e}"))
            .and_then(|value| value.as_i64())
            .unwrap_or_else(|| panic!("{pattern}: count() returned no integer"));
        assert_eq!(
            count,
            list.len() as i64,
            "count() != to_list().len() for {pattern:?} by {key:?} from {starts:?} (filtered: {filtered})"
        );
        if starts.len() == 1 {
            let mut unique = sorted_ids(list.clone());
            unique.dedup();
            assert_eq!(
                unique.len(),
                list.len(),
                "duplicates for {pattern:?} (filtered: {filtered})"
            );
        }
        if !filtered {
            unfiltered = count;
        }
    }
    unfiltered
}

#[test]
fn count_equals_to_list_len_for_every_tested_pattern() {
    let graph = TraversalGraph::file_tree();
    let root = &["root"][..];
    let cases: &[(&[&str], &str, Key)] = &[
        (root, "**/author/*/some/*.rs", Key::Name),
        (root, "**/author/**/*.rs", Key::Name),
        (root, "**/f.rs", Key::Name),
        (root, "author/**/h.rs", Key::Name),
        (root, "src/**", Key::Name),
        (root, "src/**/**", Key::Name),
        (root, "**/**/f.rs", Key::Name),
        (root, "**", Key::Name),
        (root, "**/*.rs", Key::Name),
        (root, "**/loop", Key::Name),
        (root, "latest/*", Key::Name),
        (root, "latest/**", Key::Name),
        (root, "src/test_?.rs", Key::Name),
        (root, "src/test_[ab].rs", Key::Name),
        (root, "src/test_[!a].rs", Key::Name),
        (root, "src/{main,lib}.rs", Key::Name),
        (root, "src/util/*.rs", Key::Name),
        (root, r"docs/data\[1\].csv", Key::Name),
        (root, "docs/r?sum?.md", Key::Name),
        (root, r"a\/b/c.rs", Key::Name),
        (root, "*/orphan.rs", Key::Name),
        (root, "?*/orphan.rs", Key::Name),
        (root, "42/*", Key::Name),
        (root, "*/answer.rs", Key::Name),
        (root, "src", Key::Name),
        (root, "*", Key::Name),
        (root, "nothing/**", Key::Name),
        (root, r"**/*\/util\/*", Key::Id),
        (root, r"root\/src/root\/src\/main.rs", Key::Id),
        (root, "**", Key::Id),
        (root, "**/symlink", Key::Label),
        (root, "directory/file", Key::Label),
        (root, "**/directory", Key::Label),
        (&["root/author", "root/y"], "**/*.rs", Key::Name),
        (&["root", "root/author"], "**/f.rs", Key::Name),
        (&["root", "root/author", "root/y"], "**", Key::Name),
        (&["root/src", "root/docs"], "{*.md,util}", Key::Name),
    ];
    for &(starts, pattern, key) in cases {
        assert_count_equals_list_len(&graph, starts, pattern, key);
    }
    // The cycle and the diamond are only walked without the label filter.
    assert_eq!(
        assert_count_equals_list_len(&graph, root, "**", Key::Name),
        36
    );
    assert_eq!(
        assert_count_equals_list_len(&graph, root, "**/author/**/*.rs", Key::Name),
        3
    );
}

/// A diamond where the second arrival at `a2` brings a new automaton state, so the walk
/// re-expands `a2` and reaches `a2/c/d` a second time with an accepting state: `process()` drops
/// the second emission, and `count()` (whose first pass sees 3 matches) takes its second,
/// set-collecting pass. Both must agree.
#[test]
fn count_after_a_re_expanded_diamond_counts_each_vertex_once() {
    let mut graph = TraversalGraph::new();
    let vertex = |graph: &mut TraversalGraph, id: &str, name: &str| {
        let mut props = PropertyMap::default();
        props.insert("name", name.into());
        graph
            .add_vertex(Some(id.into()), vec!["directory".into()], props)
            .unwrap()
    };
    let r = vertex(&mut graph, "r", "r");
    let a1 = vertex(&mut graph, "r/a1", "a");
    let a2 = vertex(&mut graph, "r/a1/a2", "a");
    let c = vertex(&mut graph, "r/a1/a2/c", "c");
    let d = vertex(&mut graph, "r/a1/a2/c/d", "d");
    for (from, to) in [(r, a1), (a1, a2), (r, a2), (a2, c), (c, d)] {
        graph
            .add_edge(
                None,
                Some("contains".into()),
                from,
                to,
                PropertyMap::default(),
            )
            .unwrap();
    }
    // Directly below `r`, `a2` is the `a` segment, so `d` is `a/*/**`; below `a1`, `a2` is the
    // `*` segment, so both `c` and `d` are `a/*/**`.
    let ids = sorted_ids(
        graph
            .traversal()
            .v("r")
            .glob_path("a/*/**")
            .by("name")
            .id()
            .to_list()
            .unwrap(),
    );
    assert_eq!(
        ids,
        vec!["r/a1/a2/c".to_string(), "r/a1/a2/c/d".to_string()]
    );
    assert_eq!(
        assert_count_equals_list_len(&graph, &["r"], "a/*/**", Key::Name),
        2
    );
    assert_eq!(
        assert_count_equals_list_len(&graph, &["r"], "**", Key::Name),
        4
    );
}

#[test]
fn profile_of_a_fused_count_shows_the_step_and_its_count() {
    let graph = TraversalGraph::file_tree();
    let stats = graph
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by("name")
        .by(["directory", "file"])
        .count()
        .profile()
        .unwrap();
    let node = stats
        .metrics()
        .iter()
        .find(|node| node.name().starts_with("glob_path("))
        .unwrap_or_else(|| panic!("no glob_path node in {stats}"));
    assert_eq!(
        node.name(),
        r#"glob_path("**/*.rs").by("name").by(["directory","file"])"#
    );
    assert_eq!(node.count_in(), 1);
    assert_eq!(node.counts().traverser_count(), 12);
    let count = stats
        .metrics()
        .iter()
        .find(|node| node.name() == "count()")
        .unwrap_or_else(|| panic!("no count() node in {stats}"));
    assert_eq!(count.counts().traverser_count(), 1);

    let count = graph
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by("name")
        .by(["directory", "file"])
        .count()
        .next()
        .unwrap();
    assert_eq!(count.and_then(|value| value.as_i64()), Some(12));
}

#[cfg(feature = "script")]
#[test]
fn rhai_count_matches_in_both_spellings() {
    use std::sync::Arc;
    let graph = Arc::new(GraphSource::file_tree());
    let engine = crate::script_host::engine();
    for script in [
        r#"g.v("root").glob_path("**/*.rs").by("name").by(["directory","file"]).count().next()"#,
        r#"g.V("root").glob_path("**/*.rs").by("name").by(["directory","file"]).count().next()"#,
    ] {
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let count = engine
            .eval_with_scope::<i64>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
        assert_eq!(count, 12, "{script}");
    }
}

// ---------------------------------------------------------------------------------------------
// Options: `glob_path_with(pattern, GlobPathOptions)` / Rhai `glob_path(pattern, #{...})`
// ---------------------------------------------------------------------------------------------

/// A traversal builder over `file_tree`, for [`options_case`].
type Build<'a> = dyn for<'b> Fn(
        GraphTraversalSource<'b, &'b TraversalGraph>,
    ) -> GraphTraversalSource<'b, &'b TraversalGraph>
    + 'a;

/// Asserts that `build(g).id()` (Rust) and `{script}.id()` (Rhai, in both spellings) return
/// exactly `expected`, and that `count()` of the same traversal returns its length.
fn options_case(build: &Build<'_>, script: &str, expected: &[&str]) {
    let mut expected_ids: Vec<String> = expected.iter().map(|id| id.to_string()).collect();
    expected_ids.sort();
    let graph = TraversalGraph::file_tree();
    let ids = sorted_ids(
        build(graph.traversal())
            .id()
            .to_list()
            .unwrap_or_else(|e| panic!("`{script}`: {e}")),
    );
    assert_eq!(ids, expected_ids, "Rust API result of `{script}`");
    let count = build(graph.traversal())
        .count()
        .next()
        .unwrap_or_else(|e| panic!("`{script}`: {e}"))
        .and_then(|value| value.as_i64());
    assert_eq!(
        count,
        Some(expected.len() as i64),
        "Rust count() of `{script}`"
    );

    #[cfg(feature = "script")]
    {
        use std::sync::Arc;
        {
            assert_eq!(
                rhai_ids(&format!("{script}.id()")),
                expected_ids,
                "Rhai result of `{script}`"
            );
            let graph = Arc::new(GraphSource::file_tree());
            let mut scope = crate::script_host::graph_scope(graph);
            let count = crate::script_host::engine()
                .eval_with_scope::<i64>(&mut scope, &format!("{script}.count().next()"))
                .unwrap_or_else(|e| panic!("`{script}.count()` failed: {e}"));
            assert_eq!(count, expected.len() as i64, "Rhai count() of `{script}`");
        }
    }
}

/// The `.rs` files directly below the top-level directories (depth 2).
const RS_AT_DEPTH_2: [&str; 7] = [
    "root/#42/answer.rs",
    "root/#slash/c.rs",
    "root/#unnamed/orphan.rs",
    "root/src/lib.rs",
    "root/src/main.rs",
    "root/src/test_a.rs",
    "root/src/test_b.rs",
];

#[test]
fn max_depth_bounds_the_walk() {
    // `max_depth: 1` visits exactly the children of the start.
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with("**", GlobPathOptions::default().max_depth(1))
                .by("name")
        },
        r#"g.v("root").glob_path("**", #{max_depth: 1}).by("name")"#,
        &[
            "root/#42",
            "root/#slash",
            "root/#unnamed",
            "root/author",
            "root/docs",
            "root/latest",
            "root/src",
            "root/y",
        ],
    );
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with("**/*.rs", GlobPathOptions::default().max_depth(2))
                .by("name")
        },
        r#"g.v("root").glob_path("**/*.rs", #{max_depth: 2}).by("name")"#,
        &RS_AT_DEPTH_2,
    );
    // Every match of this pattern is four levels deep.
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "author/*/some/*.rs",
                    GlobPathOptions::default().max_depth(3),
                )
                .by("name")
        },
        r#"g.v("root").glob_path("author/*/some/*.rs", #{max_depth: 3}).by("name")"#,
        &[],
    );
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "author/*/some/*.rs",
                    GlobPathOptions::default().max_depth(4),
                )
                .by("name")
        },
        r#"g.v("root").glob_path("author/*/some/*.rs", #{max_depth: 4}).by("name")"#,
        &["root/author/x/some/f.rs"],
    );
    // A bound larger than the tree changes nothing.
    let every: Vec<String> = all_below_root(true);
    let every: Vec<&str> = every.iter().map(String::as_str).collect();
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with("**", GlobPathOptions::default().max_depth(100))
                .by("name")
        },
        r#"g.v("root").glob_path("**", #{max_depth: 100}).by("name")"#,
        &every,
    );
    // A literal pattern shorter than the bound is unaffected.
    for depth in [3, 5] {
        options_case(
            &move |g| {
                g.v("root")
                    .glob_path_with(
                        "src/util/io.rs",
                        GlobPathOptions::default().max_depth(depth),
                    )
                    .by("name")
            },
            &format!(
                r#"g.v("root").glob_path("src/util/io.rs", #{{max_depth: {depth}}}).by("name")"#
            ),
            &["root/src/util/io.rs"],
        );
    }
}

#[test]
fn max_depth_keeps_a_match_reachable_within_the_bound_over_a_diamond() {
    // `root/author` is reached directly (depth 1) and via `root/latest` (depth 2); `some` below
    // it is within the bound only via the direct edge. The walk records every child of `root`
    // before it descends, so it sees the direct edge first here;
    // `max_depth_re_expands_a_vertex_reached_shallower_later` covers the deep route first.
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with("**/some", GlobPathOptions::default().max_depth(3))
                .by("name")
        },
        r#"g.v("root").glob_path("**/some", #{max_depth: 3}).by("name")"#,
        &["root/author/x/some"],
    );
}

#[test]
fn max_depth_re_expands_a_vertex_reached_shallower_later() {
    // Two routes to `m`: `r -> a -> x -> m` (depth 3) and `r -> b -> m` (depth 2). The walk
    // descends into `a` before `b` (it pops the oldest edge of a vertex first), so it reaches `m`
    // at depth 3 first, where `c` below it (depth 4) is out of reach; via `b` it must expand `m`
    // again, and `c` is at depth 3.
    let mut graph = TraversalGraph::new();
    let vertex = |graph: &mut TraversalGraph, id: &str, name: &str| {
        let mut props = PropertyMap::default();
        props.insert("name", name.into());
        graph
            .add_vertex(Some(id.into()), vec!["directory".into()], props)
            .unwrap()
    };
    let r = vertex(&mut graph, "r", "r");
    let a = vertex(&mut graph, "r/a", "a");
    let x = vertex(&mut graph, "r/a/x", "x");
    let m = vertex(&mut graph, "r/a/x/m", "m");
    let c = vertex(&mut graph, "r/a/x/m/c", "c");
    let b = vertex(&mut graph, "r/b", "b");
    for (from, to) in [(r, a), (a, x), (x, m), (m, c), (r, b), (b, m)] {
        graph
            .add_edge(
                None,
                Some("contains".into()),
                from,
                to,
                PropertyMap::default(),
            )
            .unwrap();
    }
    let build = || {
        graph
            .traversal()
            .v("r")
            .glob_path_with("**/c", GlobPathOptions::default().max_depth(3))
            .by("name")
    };
    assert_eq!(
        sorted_ids(build().id().to_list().unwrap()),
        vec!["r/a/x/m/c".to_string()]
    );
    assert_eq!(
        build()
            .count()
            .next()
            .unwrap()
            .and_then(|value| value.as_i64()),
        Some(1)
    );
}

/// The ids `glob_path(pattern).by(key)[.by(filter)]` with `max_depth` must return from `start`,
/// computed by brute force: every walk of at most `max_depth` edges from `start` becomes its own
/// vertex of a tree (a copy of the walk's last vertex, with its labels and properties), so the
/// unbounded walk over that tree has no cycle, no diamond and no depth bound to get wrong; the
/// copies it matches are mapped back to the vertices they copy.
fn brute_force_ids(
    graph: &TraversalGraph,
    start: &str,
    pattern: &str,
    max_depth: u32,
    by_label: bool,
    filter: bool,
) -> Vec<String> {
    let mut tree = TraversalGraph::new();
    let mut next = 0usize;
    let mut copy = |tree: &mut TraversalGraph, original| {
        next += 1;
        let id = format!("{}#{next}", graph.vertex_id(original).unwrap_or_default());
        let labels = graph.vertex_labels(original).to_vec();
        let properties = graph
            .vertex_properties(original)
            .cloned()
            .unwrap_or_default();
        tree.add_vertex(Some(id.into()), labels, properties)
            .unwrap()
    };
    let original_start = graph.get_vertex_by_id(start).expect("start vertex");
    let tree_start = copy(&mut tree, original_start);
    let mut frontier = vec![(original_start, tree_start)];
    for _ in 0..max_depth {
        let mut deeper = Vec::new();
        for (original, copied) in frontier {
            let children: Vec<_> = graph
                .out_edges(original)
                .filter_map(|edge| graph.edge_endpoints(edge).map(|(_, child)| child))
                .collect();
            for child in children {
                let child_copy = copy(&mut tree, child);
                tree.add_edge(
                    None,
                    Some("contains".into()),
                    copied,
                    child_copy,
                    PropertyMap::default(),
                )
                .unwrap();
                deeper.push((child, child_copy));
            }
        }
        frontier = deeper;
    }
    let start_copy = tree.vertex_id(tree_start).unwrap_or_default().to_string();
    let walk = tree.traversal().v(start_copy.as_str()).glob_path(pattern);
    let walk = if by_label {
        walk.by(By::Label)
    } else {
        walk.by("name")
    };
    let walk = if filter {
        walk.by(["directory", "file"])
    } else {
        walk
    };
    let mut ids: Vec<String> = sorted_ids(walk.id().to_list().unwrap())
        .into_iter()
        .map(|id| {
            id.rsplit_once('#')
                .map_or(id.clone(), |(original, _)| original.to_string())
        })
        .filter(|id| id != start)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Asserts that `glob_path` with `max_depth` equals the brute-force reference for every
/// pattern and bound, with and without the label filter, and that `count()` agrees.
fn assert_matches_brute_force(
    graph: &TraversalGraph,
    start: &str,
    patterns: &[(&str, bool)],
    depths: std::ops::RangeInclusive<u32>,
) {
    for &(pattern, by_label) in patterns {
        for depth in depths.clone() {
            for filter in [false, true] {
                let build = || {
                    let walk = graph
                        .traversal()
                        .v(start)
                        .glob_path_with(pattern, GlobPathOptions::default().max_depth(depth));
                    let walk = if by_label {
                        walk.by(By::Label)
                    } else {
                        walk.by("name")
                    };
                    if filter {
                        walk.by(["directory", "file"])
                    } else {
                        walk
                    }
                };
                let list = sorted_ids(build().id().to_list().unwrap());
                let expected = brute_force_ids(graph, start, pattern, depth, by_label, filter);
                assert_eq!(
                    list, expected,
                    "{pattern:?} with max_depth {depth} (filter: {filter})"
                );
                let count = build()
                    .count()
                    .next()
                    .unwrap()
                    .and_then(|value| value.as_i64());
                assert_eq!(
                    count,
                    Some(list.len() as i64),
                    "count() of {pattern:?} with max_depth {depth}"
                );
            }
        }
    }
}

#[test]
fn max_depth_equals_a_brute_force_walk_on_the_file_tree() {
    // Without the label filter the walk crosses the `root/y/loop` cycle and the `root/latest`
    // diamond; with it, `file_tree` is a strict tree.
    let graph = TraversalGraph::file_tree();
    let patterns = [
        ("**", false),
        ("**/some", false),
        ("**/*.rs", false),
        ("**/author/**", false),
        ("**/author/*/some/*.rs", false),
        ("*/*", false),
        ("latest/*/**", false),
        ("**/loop/**", false),
        ("y/*/*/**", false),
        ("**/directory/file", true),
    ];
    assert_matches_brute_force(&graph, "root", &patterns, 1..=7);
    assert_matches_brute_force(
        &graph,
        "root/y",
        &[("**", false), ("**/src/**", false)],
        1..=6,
    );
}

#[test]
fn max_depth_equals_a_brute_force_walk_on_diamonds_in_both_edge_orders() {
    // `r -> a1 -> a2` and a shortcut `r -> a2`, inserted before and after the long route, so
    // either route can be walked first; `a2 -> r` closes a cycle.
    for shortcut_first in [false, true] {
        let mut graph = TraversalGraph::new();
        let vertex = |graph: &mut TraversalGraph, id: &str, name: &str| {
            let mut props = PropertyMap::default();
            props.insert("name", name.into());
            graph
                .add_vertex(Some(id.into()), vec!["directory".into()], props)
                .unwrap()
        };
        let r = vertex(&mut graph, "r", "r");
        let a1 = vertex(&mut graph, "r/a1", "a");
        let a2 = vertex(&mut graph, "r/a1/a2", "a");
        let c = vertex(&mut graph, "r/a1/a2/c", "c");
        let d = vertex(&mut graph, "r/a1/a2/c/d", "d");
        let mut edges = vec![(r, a1), (a1, a2), (a2, c), (c, d), (a2, r)];
        if shortcut_first {
            edges.insert(0, (r, a2));
        } else {
            edges.push((r, a2));
        }
        for (from, to) in edges {
            graph
                .add_edge(
                    None,
                    Some("contains".into()),
                    from,
                    to,
                    PropertyMap::default(),
                )
                .unwrap();
        }
        let patterns = [
            ("**", false),
            ("a/*/**", false),
            ("**/c", false),
            ("**/d", false),
            ("**/a/**", false),
            ("*/*/*", false),
        ];
        assert_matches_brute_force(&graph, "r", &patterns, 1..=6);
    }
}

#[test]
fn prune_stops_the_descent_but_not_the_emission() {
    // Nothing below the three `author` directories (also not through `root/latest`).
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "**/*.rs",
                    GlobPathOptions::default().prune(__::has("name", "author")),
                )
                .by("name")
        },
        r#"g.v("root").glob_path("**/*.rs", #{prune: __.has("name", "author")}).by("name")"#,
        &[
            "root/#42/answer.rs",
            "root/#slash/c.rs",
            "root/#unnamed/orphan.rs",
            "root/src/lib.rs",
            "root/src/main.rs",
            "root/src/test_a.rs",
            "root/src/test_b.rs",
            "root/src/util/io.rs",
            "root/src/util/mod.rs",
        ],
    );
    // A pruned vertex is still emitted when it matches; the nested `root/author/q/author` is
    // never reached.
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "**/author",
                    GlobPathOptions::default().prune(__::has("name", "author")),
                )
                .by("name")
        },
        r#"g.v("root").glob_path("**/author", #{prune: __.has("name", "author")}).by("name")"#,
        &["root/author", "root/y/author"],
    );
}

#[test]
fn prune_reads_a_step_label_set_before_the_step() {
    // The probe carries the incoming path, so `select("a")` is the start vertex: every child
    // is emitted, none is descended into.
    options_case(
        &|g| {
            g.v("root/src")
                .as_("a")
                .glob_path_with(
                    "**",
                    GlobPathOptions::default().prune(__::select("a").has("name", "src")),
                )
                .by("name")
        },
        r#"g.v("root/src").as("a").glob_path("**", #{prune: __.select("a").has("name", "src")}).by("name")"#,
        &[
            "root/src/lib.rs",
            "root/src/main.rs",
            "root/src/test_a.rs",
            "root/src/test_b.rs",
            "root/src/util",
        ],
    );
}

#[test]
fn prune_combines_with_the_label_filter_and_max_depth() {
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "**/*.rs",
                    GlobPathOptions::default().prune(__::has("name", "author")),
                )
                .by("name")
                .by(["directory", "file"])
        },
        r#"g.v("root").glob_path("**/*.rs", #{prune: __.has("name", "author")}).by("name").by(["directory","file"])"#,
        &[
            "root/#42/answer.rs",
            "root/#slash/c.rs",
            "root/#unnamed/orphan.rs",
            "root/src/lib.rs",
            "root/src/main.rs",
            "root/src/test_a.rs",
            "root/src/test_b.rs",
            "root/src/util/io.rs",
            "root/src/util/mod.rs",
        ],
    );
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "**/*.rs",
                    GlobPathOptions::default()
                        .max_depth(2)
                        .prune(__::has("name", "author")),
                )
                .by("name")
        },
        r#"g.v("root").glob_path("**/*.rs", #{prune: __.has("name", "author"), max_depth: 2}).by("name")"#,
        &RS_AT_DEPTH_2,
    );
}

#[test]
fn a_glob_path_nested_in_prune_gets_its_own_buffers() {
    // Prune every directory with a Markdown file: `docs` and `author/x/some`.
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "**/*.rs",
                    GlobPathOptions::default().prune(__::glob_path("*.md").by("name")),
                )
                .by("name")
        },
        r#"g.v("root").glob_path("**/*.rs", #{prune: __.glob_path("*.md").by("name")}).by("name")"#,
        &[
            "root/#42/answer.rs",
            "root/#slash/c.rs",
            "root/#unnamed/orphan.rs",
            "root/author/q/author/w/some/h.rs",
            "root/src/lib.rs",
            "root/src/main.rs",
            "root/src/test_a.rs",
            "root/src/test_b.rs",
            "root/src/util/io.rs",
            "root/src/util/mod.rs",
            "root/y/author/z/some/g.rs",
        ],
    );
    // Two levels of nesting, each with options.
    options_case(
        &|g| {
            g.v("root")
                .glob_path_with(
                    "**",
                    GlobPathOptions::default().max_depth(2).prune(
                        __::glob_path_with(
                            "**/*.md",
                            GlobPathOptions::default().prune(__::glob_path("*.csv").by("name")),
                        )
                        .by("name"),
                    ),
                )
                .by("name")
                .by(["directory"])
        },
        r#"g.v("root").glob_path("**", #{max_depth: 2, prune: __.glob_path("**/*.md", #{prune: __.glob_path("*.csv").by("name")}).by("name")}).by("name").by(["directory"])"#,
        // `root/y` is pruned: through `root/y/loop` the walk below it reaches
        // `root/author/x/some/readme.md` (`root/docs` is pruned inside, by its `.csv`).
        &[
            "root/#42",
            "root/#slash",
            "root/#unnamed",
            "root/author",
            "root/docs",
            "root/src",
            "root/src/util",
            "root/y",
        ],
    );
}

/// Asserts that `err` is rooted in `InvalidOption` for `glob_path.max_depth` with `reason`.
fn assert_invalid_max_depth(err: &TraverserError, reason: &str) {
    match err.root_cause() {
        TraverserError::InvalidOption {
            key,
            reason: actual,
            ..
        } => {
            assert_eq!(key, "glob_path.max_depth", "wrong key in: {err}");
            assert_eq!(actual, reason, "wrong reason in: {err}");
        }
        other => panic!("expected InvalidOption, got: {other}"),
    }
}

#[test]
fn an_out_of_range_max_depth_fails_on_execution_after_the_other_checks() {
    let graph = TraversalGraph::file_tree();
    let run = |pattern: &str, depth: u32, by: bool| {
        let t = graph
            .traversal()
            .v("root")
            .glob_path_with(pattern, GlobPathOptions::default().max_depth(depth));
        let t = if by { t.by("name") } else { t };
        t.to_list().expect_err("max_depth 0")
    };
    assert_invalid_max_depth(&run("**", 0, true), "must be at least 1, got 0");
    // count() validates the same way.
    let err = graph
        .traversal()
        .v("root")
        .glob_path_with("**", GlobPathOptions::default().max_depth(0))
        .by("name")
        .count()
        .next()
        .expect_err("max_depth 0");
    assert_invalid_max_depth(&err, "must be at least 1, got 0");
    // The pattern is checked first, then the by() modulators.
    assert!(
        matches!(
            run("[z-a]", 0, true).root_cause(),
            TraverserError::Value(ValueError::InvalidGlobPattern { .. })
        ),
        "the pattern is checked before the options"
    );
    assert_invalid_modulator(&run("**", 0, false), 1);
}

#[test]
fn an_error_inside_prune_propagates() {
    let graph = TraversalGraph::file_tree();
    let err = graph
        .traversal()
        .v("root")
        .glob_path_with(
            "**",
            GlobPathOptions::default().prune(__::glob_path("[z-a]").by("name")),
        )
        .by("name")
        .to_list()
        .expect_err("invalid pattern in prune");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::InvalidGlobPattern { .. })
        ),
        "got {err}"
    );
    let err = graph
        .traversal()
        .v("root")
        .glob_path_with(
            "**",
            GlobPathOptions::default()
                .prune(__::glob_path_with("*", GlobPathOptions::default().max_depth(0)).by("name")),
        )
        .by("name")
        .count()
        .next()
        .expect_err("invalid max_depth in prune");
    assert_invalid_max_depth(&err, "must be at least 1, got 0");
}

#[cfg(feature = "script")]
#[test]
fn rhai_option_errors() {
    use crate::script_host::rhai::EvalAltResult;
    use std::sync::Arc;
    let eval = |script: &str| {
        let graph = Arc::new(GraphSource::file_tree());
        let mut scope = crate::script_host::graph_scope(graph);
        crate::script_host::engine()
            .eval_with_scope::<crate::script_host::rhai::Dynamic>(&mut scope, script)
    };
    // Range errors surface on execution, as InvalidOption.
    for (depth, reason) in [
        ("0", "must be at least 1, got 0"),
        ("-1", "must be at least 1, got -1"),
        ("4294967296", "must be at most 4294967295, got 4294967296"),
    ] {
        {
            let script =
                format!(r#"g.v("root").glob_path("**", #{{max_depth: {depth}}}).by("name")"#);
            let graph = Arc::new(GraphSource::file_tree());
            let mut scope = crate::script_host::graph_scope(graph.clone());
            let gts = crate::script_host::engine()
                .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(
                    &mut scope, &script,
                )
                .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
            let lock = graph.read();
            let err = gts.ast_convert_to_gts(&*lock).to_list().expect_err(&script);
            assert_invalid_max_depth(&err, reason);
        }
    }
    // Key and type errors surface when the step is built, as ArgumentMismatch.
    let keys = "expected an options map with the keys max_depth, prune";
    let cases = [
        (r#"#{foo: 1}"#, format!("{keys}, got the unknown key `foo`")),
        (r#"#{edges: ["contains"]}"#, format!("{keys}, got the reserved key `edges`, which is not implemented yet")),
        (r#"#{path: "full"}"#, format!("{keys}, got the reserved key `path`, which is not implemented yet")),
        (
            r#"#{case_insensitive: true}"#,
            format!("{keys}, got the reserved key `case_insensitive`, which is not implemented yet"),
        ),
        (
            r#"#{max_depth: "3"}"#,
            "expected max_depth as an integer, for example #{max_depth: 3}, got a value of type string (`3`)".to_string(),
        ),
        (
            r#"#{max_depth: 2.5}"#,
            "expected max_depth as an integer, for example #{max_depth: 3}, got a value of type float64 (`2.5`)".to_string(),
        ),
        (
            r#"#{prune: 5}"#,
            r#"expected prune as an anonymous traversal, for example #{prune: __.has("name", "target")}, got a value of type int64 (`5`)"#
                .to_string(),
        ),
    ];
    for (options, message) in &cases {
        {
            let script = format!(r#"g.v("root").glob_path("**", {options}).by("name").to_list()"#);
            let err = eval(&script).expect_err(&script);
            let EvalAltResult::ErrorRuntime(diagnostic, ..) = *err else {
                panic!("`{script}`: not a runtime error: {err}");
            };
            let diagnostic = diagnostic.to_string();
            assert!(
                diagnostic.starts_with(&format!(
                    "Error: Argument mismatch for step 'glob_path': {message}\n"
                )),
                "`{script}`:\n{diagnostic}"
            );
            assert!(
                diagnostic.contains(
                    r#"Help: glob_path(pattern, #{...}) accepts the options max_depth (an integer from 1: the deepest level below the start vertex the walk visits) and prune (an anonymous traversal: a visited vertex for which it yields a result is not descended into, but is still emitted when it matches); the keys edges, path and case_insensitive are reserved. For example g.v("root").glob_path("**/*.rs", #{max_depth: 3, prune: __.has("name", "target")}).by("name")."#
                ),
                "`{script}`:\n{diagnostic}"
            );
        }
    }
}

#[test]
fn profile_renders_the_options_and_the_prune_child() {
    let graph = TraversalGraph::file_tree();
    let stats = graph
        .traversal()
        .v("root")
        .glob_path_with(
            "**/*.rs",
            GlobPathOptions::default()
                .max_depth(3)
                .prune(__::has("name", "author")),
        )
        .by("name")
        .by(["directory", "file"])
        .profile()
        .unwrap();
    let node = stats
        .metrics()
        .iter()
        .find(|node| node.name().starts_with("glob_path("))
        .unwrap_or_else(|| panic!("no glob_path node in {stats}"));
    assert_eq!(
        node.name(),
        r#"glob_path("**/*.rs", {max_depth: 3, prune: __.has("name", "author")}).by("name").by(["directory","file"])"#
    );
    assert_eq!(node.count_in(), 1);
    assert_eq!(node.counts().traverser_count(), 9);
    // The probe runs once for every vertex the walk would expand: the 7 filtered children of
    // `root` and the 13 grandchildren below the 6 that were not pruned; 2 of them are pruned.
    let prune = node
        .nested()
        .iter()
        .find(|child| child.name() == r#"has("name", "author")"#)
        .unwrap_or_else(|| panic!("no prune child row in {stats}"));
    assert_eq!(
        (prune.count_in(), prune.counts().traverser_count()),
        (20, 2),
        "{stats}"
    );
    assert!(
        stats.to_string().contains(r#"  \> has("name", "author")"#),
        "{stats}"
    );

    // Without options the rendering is unchanged.
    let stats = graph
        .traversal()
        .v("root")
        .glob_path_with("*", GlobPathOptions::default())
        .by("name")
        .profile()
        .unwrap();
    assert!(
        stats
            .metrics()
            .iter()
            .any(|node| node.name() == r#"glob_path("*").by("name")"#),
        "{stats}"
    );
}

// ---------------------------------------------------------------------------------------------
// Book: `book/src/traversal/steps/glob_path.md`
//
// Every example on that page, with the output the page shows (copied from
// `graphersal --graph tree`). A change here must be mirrored on the page, and vice versa.
// ---------------------------------------------------------------------------------------------

/// Runs a page example as `graphersal --graph tree` does and returns what it prints: one line per
/// item of a list result (strings quoted, lists as `[a, b]`), the value itself otherwise, or the
/// full diagnostic of a failed traversal.
#[cfg(feature = "script")]
fn book_run(script: &str) -> String {
    use crate::script_host::rhai::{Dynamic, EvalAltResult};
    use std::sync::Arc;
    let graph = Arc::new(GraphSource::file_tree());
    let engine = crate::script_host::engine();
    let mut scope = crate::script_host::graph_scope(graph);
    match engine.eval_with_scope::<Dynamic>(&mut scope, script) {
        // `{:?}` is how the REPL renders plain Rhai values (strings quoted, arrays bracketed).
        Ok(value) if value.is_array() => value
            .cast::<crate::script_host::rhai::Array>()
            .iter()
            .map(|item| format!("{item:?}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Ok(value) => format!("{value:?}"),
        Err(err) => match *err {
            EvalAltResult::ErrorRuntime(diagnostic, ..) if diagnostic.is_string() => {
                diagnostic.to_string()
            }
            other => panic!("`{script}` failed outside a traversal: {other}"),
        },
    }
}

/// The code blocks of the page, each followed by the output block the page shows for it.
#[cfg(feature = "script")]
#[test]
fn book_glob_path_query_examples() {
    let cases: &[(&str, &str)] = &[
        // Quick start
        (
            r#"g.v("root").glob_path("**/author/*/some/*.rs").by("name").by(["directory","file"]).order().by(T.id).id().to_list()"#,
            "\"root/author/q/author/w/some/h.rs\"\n\"root/author/x/some/f.rs\"\n\"root/y/author/z/some/g.rs\"",
        ),
        (
            r#"g.v("root").glob_path("src").by("name").glob_path("util/*").by("name").order().by(T.id).id().to_list()"#,
            "\"root/src/util/io.rs\"\n\"root/src/util/mod.rs\"",
        ),
        (
            r#"g.v("root").glob_path("*").by("name").by(["directory"]).where(__.glob_path("*.md").by("name")).id().to_list()"#,
            "\"root/docs\"",
        ),
        // Pattern syntax
        (
            r#"g.v("root").glob_path("docs/data\\[1\\].csv").by("name").id().to_list()"#,
            "\"root/docs/data[1].csv\"",
        ),
        // The by() modulators
        (
            r#"g.v("root").glob_path("**/*\\/util\\/*").by(T.id).order().by(T.id).id().to_list()"#,
            "\"root/src/util/io.rs\"\n\"root/src/util/mod.rs\"",
        ),
        (
            r#"g.v("root").glob_path("**/symlink").by(T.label).order().by(T.id).id().to_list()"#,
            "\"root/latest\"\n\"root/y/loop\"",
        ),
        (
            r#"g.v("root").glob_path("src/*").by("name").by(["directory"]).id().to_list()"#,
            "\"root/src/util\"",
        ),
        // Options
        (
            r#"g.v("root").glob_path("**", #{max_depth: 1}).by("name").order().by(T.id).id().to_list()"#,
            "\"root/#42\"\n\"root/#slash\"\n\"root/#unnamed\"\n\"root/author\"\n\"root/docs\"\n\"root/latest\"\n\"root/src\"\n\"root/y\"",
        ),
        (
            r#"g.v("root").glob_path("**/*.rs", #{max_depth: 2}).by("name").order().by(T.id).id().to_list()"#,
            "\"root/#42/answer.rs\"\n\"root/#slash/c.rs\"\n\"root/#unnamed/orphan.rs\"\n\"root/src/lib.rs\"\n\"root/src/main.rs\"\n\"root/src/test_a.rs\"\n\"root/src/test_b.rs\"",
        ),
        (
            r#"g.v("root").glob_path("**/*.rs", #{prune: __.has("name", "author")}).by("name").order().by(T.id).id().to_list()"#,
            "\"root/#42/answer.rs\"\n\"root/#slash/c.rs\"\n\"root/#unnamed/orphan.rs\"\n\"root/src/lib.rs\"\n\"root/src/main.rs\"\n\"root/src/test_a.rs\"\n\"root/src/test_b.rs\"\n\"root/src/util/io.rs\"\n\"root/src/util/mod.rs\"",
        ),
        (
            r#"g.v("root").glob_path("**/author", #{prune: __.has("name", "author")}).by("name").order().by(T.id).id().to_list()"#,
            "\"root/author\"\n\"root/y/author\"",
        ),
        (
            r#"g.v("root/src").as("a").glob_path("**", #{prune: __.select("a").has("name", "src")}).by("name").order().by(T.id).id().to_list()"#,
            "\"root/src/lib.rs\"\n\"root/src/main.rs\"\n\"root/src/test_a.rs\"\n\"root/src/test_b.rs\"\n\"root/src/util\"",
        ),
        (
            r#"g.v("root").glob_path("**/*.rs", #{max_depth: 3, prune: __.has("name", "author")}).by("name").by(["directory","file"]).count().next()"#,
            "9",
        ),
        // Semantics
        (
            r#"g.v("root").glob_path("root").by("name").count().next()"#,
            "0",
        ),
        (
            r#"g.v("root").glob_path("**/root").by("name").count().next()"#,
            "0",
        ),
        (
            r#"g.v("root/latest").glob_path("*").by("name").by(["directory"]).id().to_list()"#,
            "\"root/author\"",
        ),
        (
            r#"g.v("root").glob_path("latest/*").by("name").order().by(T.id).id().to_list()"#,
            "\"root/author\"",
        ),
        (
            r#"g.v("root").glob_path("latest/*").by("name").by(["directory","file"]).count().next()"#,
            "0",
        ),
        (
            r#"g.v("root").glob_path("**/loop").by("name").order().by(T.id).id().to_list()"#,
            "\"root/y/loop\"",
        ),
        (
            r#"g.v("root").glob_path("**/loop").by("name").by(["directory","file"]).count().next()"#,
            "0",
        ),
        (
            r#"g.v("root").glob_path("**/author").by("name").order().by(T.id).id().to_list()"#,
            "\"root/author\"\n\"root/author/q/author\"\n\"root/y/author\"",
        ),
        (
            r#"g.v("root").glob_path("**/author/**/*.rs").by("name").order().by(T.id).id().to_list()"#,
            "\"root/author/q/author/w/some/h.rs\"\n\"root/author/x/some/f.rs\"\n\"root/y/author/z/some/g.rs\"",
        ),
        (
            r#"g.v(["root","root/author"]).glob_path("**/f.rs").by("name").id().to_list()"#,
            "\"root/author/x/some/f.rs\"\n\"root/author/x/some/f.rs\"",
        ),
        (
            r#"g.v("root").glob_path("src/util/io.rs").by("name").path().by(T.id).to_list()"#,
            "[\"root\", \"root/src/util/io.rs\"]",
        ),
        (
            r#"g.v("root").glob_path("*/orphan.rs").by("name").id().to_list()"#,
            "\"root/#unnamed/orphan.rs\"",
        ),
        (
            r#"g.v("root").glob_path("?*/orphan.rs").by("name").count().next()"#,
            "0",
        ),
        (
            r#"g.v("root").glob_path("42/*").by("name").count().next()"#,
            "0",
        ),
        (
            r#"g.v("root").glob_path("*/answer.rs").by("name").id().to_list()"#,
            "\"root/#42/answer.rs\"",
        ),
        // Performance
        (
            r#"g.v("root").glob_path("**").by("name").count().next()"#,
            "36",
        ),
        (
            r#"g.v("root").glob_path("**").by("name").by(["directory","file"]).count().next()"#,
            "34",
        ),
        (
            r#"g.v("root").glob_path("**/*.rs").by("name").by(["directory","file"]).count().next()"#,
            "12",
        ),
        // Errors
        (
            r#"g.v("root").glob_path("src/a**").by("name").to_list()"#,
            r#"Error: Step #1 'glob_path("src/a**").by("name")' execution failed
  at #1: v("root").glob_path("src/a**").by("name")
                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
Caused by: Invalid glob pattern 'src/a**' at position 5: '**' must be a whole segment
Help: '**' spans whole levels, so it must stand alone between slashes. Write glob_path("src/a*/**") instead, or plain glob_path("**") to match every level below the start vertex. A literal '*' is written '\*'.
      A pattern is a '/'-separated list of segments matched level by level below the start vertex: '*' (any value), '?' (one character), '**' (any number of levels, a whole segment only), '[a-z]' / '[!a-z]' (character classes), '{a,b}' (alternatives within one segment) and '\' to escape any of them, for example glob_path("**/src/*.{rs,toml}")."#,
        ),
        (
            r#"g.v("root").glob_path("**", #{max_depth: 0}).by("name").to_list()"#,
            r#"Error: Step #1 'glob_path("**", {max_depth: 0}).by("name")' execution failed
  at #1: v("root").glob_path("**", {max_depth: 0}).by("name")
                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
Caused by: Invalid value for option 'glob_path.max_depth': must be at least 1, got 0
Help: max_depth is the deepest level below the start vertex that glob_path() visits: the start vertex is level 0 and its children are level 1, so max_depth is an integer from 1 to 4294967295. Leave it out to walk without a bound. For example g.v("root").glob_path("**", #{max_depth: 1}).by("name")."#,
        ),
        (
            r#"g.v("root").glob_path("**", #{edges: ["contains"]}).by("name").to_list()"#,
            r#"Error: Argument mismatch for step 'glob_path': expected an options map with the keys max_depth, prune, got the reserved key `edges`, which is not implemented yet
Help: glob_path(pattern, #{...}) accepts the options max_depth (an integer from 1: the deepest level below the start vertex the walk visits) and prune (an anonymous traversal: a visited vertex for which it yields a result is not descended into, but is still emitted when it matches); the keys edges, path and case_insensitive are reserved. For example g.v("root").glob_path("**/*.rs", #{max_depth: 3, prune: __.has("name", "target")}).by("name")."#,
        ),
        // Equivalents
        (
            r#"g.V("root").emit().repeat(__.out().hasLabel("directory","file")).out().hasLabel("directory","file").has("name","author").out().hasLabel("directory","file").out().hasLabel("directory","file").has("name","some").out().hasLabel("directory","file").has("name", TextP.endingWith(".rs")).dedup().order().by(T.id).id().to_list()"#,
            "\"root/author/q/author/w/some/h.rs\"\n\"root/author/x/some/f.rs\"\n\"root/y/author/z/some/g.rs\"",
        ),
        (
            r#"g.V("root").repeat(__.out().hasLabel("directory","file")).until(__.has("name","author")).out().hasLabel("directory","file").out().hasLabel("directory","file").has("name","some").out().hasLabel("directory","file").has("name", TextP.endingWith(".rs")).dedup().order().by(T.id).id().to_list()"#,
            "\"root/author/x/some/f.rs\"\n\"root/y/author/z/some/g.rs\"",
        ),
    ];
    for &(script, expected) in cases {
        assert_eq!(book_run(script), expected, "page example `{script}`");
    }
}

/// The introductory example (shown without a terminal) returns the three files.
#[cfg(feature = "script")]
#[test]
fn book_glob_path_intro_example() {
    let intro =
        r#"g.v("root").glob_path("**/author/*/some/*.rs").by("name").by(["directory","file"])"#;
    let mut expected: Vec<String> = RS_BELOW_AUTHOR.iter().map(|id| id.to_string()).collect();
    expected.sort();
    assert_eq!(rhai_ids(&format!("{intro}.id()")), expected);
}

/// The "Pattern syntax" table: each row is
/// `g.v("root").glob_path(PATTERN).by("name").order().by(T.id).id().to_list()`, run through the Rust API
/// and through Rhai (where the pattern's backslashes are doubled in the string literal).
#[test]
fn book_glob_path_syntax_table() {
    let rows: &[(&str, &[&str])] = &[
        ("src/main.rs", &["root/src/main.rs"]),
        (
            "src/*",
            &[
                "root/src/lib.rs",
                "root/src/main.rs",
                "root/src/test_a.rs",
                "root/src/test_b.rs",
                "root/src/util",
            ],
        ),
        (
            "src/util/*.rs",
            &["root/src/util/io.rs", "root/src/util/mod.rs"],
        ),
        (
            "src/test_?.rs",
            &["root/src/test_a.rs", "root/src/test_b.rs"],
        ),
        ("docs/r?sum?.md", &["root/docs/résumé.md"]),
        ("**/src", &["root/src"]),
        ("**/util", &["root/src/util"]),
        ("author/**/h.rs", &["root/author/q/author/w/some/h.rs"]),
        (
            "src/**",
            &[
                "root/src/lib.rs",
                "root/src/main.rs",
                "root/src/test_a.rs",
                "root/src/test_b.rs",
                "root/src/util",
                "root/src/util/io.rs",
                "root/src/util/mod.rs",
            ],
        ),
        (
            "src/test_[ab].rs",
            &["root/src/test_a.rs", "root/src/test_b.rs"],
        ),
        ("src/test_[!a].rs", &["root/src/test_b.rs"]),
        ("src/[a-m]*.rs", &["root/src/lib.rs", "root/src/main.rs"]),
        (
            "src/{main,lib}.rs",
            &["root/src/lib.rs", "root/src/main.rs"],
        ),
        (
            "docs/{guide,notes}.{md,txt}",
            &["root/docs/guide.md", "root/docs/notes.txt"],
        ),
        ("docs/guide{,s}.md", &["root/docs/guide.md"]),
        (
            "{src,docs}/{m,g}*",
            &["root/docs/guide.md", "root/src/main.rs"],
        ),
        (r"docs/data\[1\].csv", &["root/docs/data[1].csv"]),
        (r"a\/b/c.rs", &["root/#slash/c.rs"]),
        ("docs/*.MD", &[]),
    ];
    let graph = TraversalGraph::file_tree();
    for &(pattern, expected) in rows {
        let rust = graph
            .traversal()
            .v("root")
            .glob_path(pattern)
            .by("name")
            .order()
            .by(By::Id)
            .id()
            .to_list()
            .unwrap_or_else(|e| panic!("{pattern}: {e}"));
        let expected_values: Vec<ElementProperty> = expected
            .iter()
            .map(|&id| ElementProperty::from(id))
            .collect();
        assert_eq!(rust, expected_values, "Rust API, pattern {pattern}");
        #[cfg(feature = "script")]
        {
            let script = format!(
                r#"g.v("root").glob_path("{}").by("name").order().by(T.id).id().to_list()"#,
                pattern.replace('\\', "\\\\")
            );
            let printed: Vec<String> = expected.iter().map(|id| format!("{id:?}")).collect();
            assert_eq!(book_run(&script), printed.join("\n"), "Rhai, `{script}`");
        }
    }
}

/// The "Invalid combinations" table of the `by()` section: the `Caused by:` line and the Fix
/// column (the help text, quoted verbatim) of each row.
#[cfg(feature = "script")]
#[test]
fn book_glob_path_invalid_modulator_table() {
    let rows: &[(&str, &str, &str)] = &[
        (
            r#"glob_path("*")"#,
            "`glob_path()` cannot use its by() #1: glob_path() needs a first by() naming the match key",
            "Add the match key as the first by().",
        ),
        (
            r#"glob_path("*").by(["directory"])"#,
            "`glob_path()` cannot use its by() #1: the first by() is the match key; the vertex-label list goes into the second by()",
            r#"The first by() is the key and the label list goes second: put a key such as by("name") before the label list."#,
        ),
        (
            r#"glob_path("*").by(__.values("name"))"#,
            "`glob_path()` cannot use its by() #1: the match key must be a property key, T.id or T.label",
            "A traversal, an order, a JSON path, Value or Count cannot be the match key.",
        ),
        (
            r#"glob_path("*").by("name").by("name")"#,
            "`glob_path()` cannot use its by() #2: the second by() must be a list of vertex labels",
            r#"The second by() only filters by vertex label: pass a list such as by(["directory", "file"]), or drop it to visit every vertex."#,
        ),
        (
            r#"glob_path("*").by("name").by([])"#,
            "`glob_path()` cannot use its by() #2: the vertex-label list is empty",
            "An empty label list would admit no vertex: list at least one label, or drop the second by() to visit every vertex.",
        ),
        (
            r#"glob_path("*").by("name").by(["file"]).by(T.id)"#,
            "`glob_path()` cannot use its by() #3: glob_path() takes at most two by() modulators",
            "There is no third by(): drop it.",
        ),
    ];
    for &(written, caused_by, fix) in rows {
        let output = book_run(&format!(r#"g.v("root").{written}.to_list()"#));
        assert!(
            output.contains(&format!("\nCaused by: {caused_by}\n")),
            "{written}:\n{output}"
        );
        assert!(
            output.contains(fix),
            "{written}: fix `{fix}` not in\n{output}"
        );
        assert!(
            output.contains(r#"For example g.v("root").glob_path("**/*.rs").by("name").by(["directory","file"])."#),
            "{written}:\n{output}"
        );
    }
}

/// The "Invalid glob pattern" table: the `Caused by:` line and the Fix column (the help text,
/// quoted verbatim; the page wraps parts of it in backticks) of each row.
#[cfg(feature = "script")]
#[test]
fn book_glob_path_invalid_pattern_table() {
    let segments_script = r#"let p = "a"; for i in 0..63 { p += "/a"; } g.v("root").glob_path(p).by("name").to_list()"#;
    let segments_pattern = vec!["a"; 64].join("/");
    let segments_caused_by = format!(
        "Invalid glob pattern '{segments_pattern}' at position 126: the pattern has more than 63 segments"
    );
    let relative = "The pattern is relative to the start vertex: drop the leading and trailing `'/'` and the empty segments";
    // (Rhai pattern literal, `Caused by:` line, Fix cell as on the page)
    let rows: Vec<(String, String, &str)> = vec![
        (
            String::new(),
            "Invalid glob pattern '' at position 0: the pattern is empty".into(),
            r#"Pass a non-empty pattern, for example `glob_path("*")` for the children of the start vertex or `glob_path("**")` for all of its descendants."#,
        ),
        (
            "/src".into(),
            "Invalid glob pattern '/src' at position 0: a pattern cannot start with '/'".into(),
            relative,
        ),
        (
            "src/".into(),
            "Invalid glob pattern 'src/' at position 3: a pattern cannot end with '/'".into(),
            relative,
        ),
        (
            "src//a".into(),
            "Invalid glob pattern 'src//a' at position 4: empty segment between two '/'".into(),
            relative,
        ),
        (
            "src/..".into(),
            "Invalid glob pattern 'src/..' at position 4: '.' and '..' are reserved as whole segments".into(),
            r#"The start vertex is already the current level: `glob_path("docs/*")` matches its children directly. To walk back up use `.in()` or `repeat(__.in())` before the step, for example `g.v("x").in().glob_path("docs/*")`. A value that is literally `'.'` is matched with `'\.'`."#,
        ),
        (
            "src/a**".into(),
            "Invalid glob pattern 'src/a**' at position 5: '**' must be a whole segment".into(),
            r#"Write `glob_path("src/a*/**")` instead, or plain `glob_path("**")` to match every level below the start vertex. A literal `'*'` is written `'\*'`."#,
        ),
        (
            r"src\\".into(),
            r"Invalid glob pattern 'src\' at position 3: a trailing '\' escapes nothing".into(),
            r"`'\'` escapes the character after it; match a literal backslash with `'\\'`",
        ),
        (
            "src/[ab".into(),
            "Invalid glob pattern 'src/[ab' at position 4: unclosed character class '['".into(),
            r#"Close the character class with `']'` in the same segment, for example `glob_path("file[0-9].txt")`; match a literal `'['` with `'\['`."#,
        ),
        (
            "src/[]".into(),
            "Invalid glob pattern 'src/[]' at position 4: empty character class".into(),
            r#"A character class needs at least one character; a `']'` right after `'['` or `'[!'` is taken literally, so `glob_path("[]]")` matches `']'` and `glob_path("[!]]")` anything else."#,
        ),
        (
            "src/[z-a]".into(),
            "Invalid glob pattern 'src/[z-a]' at position 5: inverted character range".into(),
            r#"A range goes from the lower to the higher character, for example `glob_path("[a-z]*")` instead of `glob_path("[z-a]*")`."#,
        ),
        (
            "src/{a,b".into(),
            "Invalid glob pattern 'src/{a,b' at position 4: unclosed alternatives '{'".into(),
            r#"Close the alternatives with `'}'`, for example `glob_path("*.{rs,toml}")`; match a literal `'{'` with `'\{'`."#,
        ),
        (
            "src/{a,{b}}".into(),
            "Invalid glob pattern 'src/{a,{b}}' at position 7: alternatives '{..}' cannot be nested".into(),
            r#"Alternatives cannot be nested; flatten them into one group, for example `glob_path("{a,b1,b2}")` instead of `glob_path("{a,b{1,2}}")`, or place two groups side by side: `glob_path("{a,b}{1,2}")`."#,
        ),
        (
            "src/{a/b,c}".into(),
            "Invalid glob pattern 'src/{a/b,c}' at position 6: alternatives '{..}' cannot contain '/'".into(),
            r#"Split the alternative into two queries, for example `glob_path("src/a")` and `glob_path("lib/b")` instead of `glob_path("{src/a,lib/b}")`, or use `'**'` to span levels, for example `glob_path("**/{a,b}")`. A value that itself contains `'/'` is matched with `'\/'`."#,
        ),
        (
            "{a,b}".repeat(9),
            "Invalid glob pattern '{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}' at position 40: a segment expands to more than 256 alternatives".into(),
            r#"Replace a group with a class or a wildcard, for example `glob_path("file[0-9][0-9]")` instead of `glob_path("file{0,1,...,9}{0,1,...,9}")`, or split the query."#,
        ),
    ];
    let segments_fix = r#"Split the walk into two consecutive steps, each matching below the vertices the previous one emits, for example `glob_path("a/b/c").glob_path("d/e/f")` instead of `glob_path("a/b/c/d/e/f")`."#;

    let check = |script: &str, caused_by: &str, fix: &str| {
        let output = book_run(script);
        assert!(
            output.contains(&format!("\nCaused by: {caused_by}\n")),
            "`{script}`:\n{output}"
        );
        let fix = fix.replace('`', "");
        assert!(
            output.contains(&fix),
            "`{script}`: fix `{fix}` not in\n{output}"
        );
    };
    for (literal, caused_by, fix) in &rows {
        check(
            &format!(r#"g.v("root").glob_path("{literal}").by("name").to_list()"#),
            caused_by,
            fix,
        );
    }
    check(segments_script, &segments_caused_by, segments_fix);
}

/// The "Rust API" code block, verbatim.
#[test]
fn book_glob_path_rust_api() -> TraverserResult<()> {
    let graph = TraversalGraph::file_tree();

    let rust_files = graph
        .traversal()
        .v("root")
        .glob_path("**/author/*/some/*.rs")
        .by("name")
        .by(["directory", "file"])
        .id()
        .order()
        .to_list()?;
    assert_eq!(
        rust_files,
        vec![
            ElementProperty::from("root/author/q/author/w/some/h.rs"),
            ElementProperty::from("root/author/x/some/f.rs"),
            ElementProperty::from("root/y/author/z/some/g.rs"),
        ]
    );

    let symlinks = graph
        .traversal()
        .v("root")
        .glob_path("**/symlink")
        .by(By::Label)
        .count()
        .next()?;
    assert_eq!(symlinks.and_then(|count| count.as_i64()), Some(2));

    let docs_with_markdown = graph
        .traversal()
        .v("root")
        .glob_path("*")
        .by("name")
        .by(["directory"])
        .where_t(__::glob_path("*.md").by("name"))
        .id()
        .to_list()?;
    assert_eq!(docs_with_markdown, vec![ElementProperty::from("root/docs")]);

    let outside_author = graph
        .traversal()
        .v("root")
        .glob_path_with(
            "**/*.rs",
            GlobPathOptions::default()
                .max_depth(3)
                .prune(__::has("name", "author")),
        )
        .by("name")
        .by(["directory", "file"])
        .count()
        .next()?;
    assert_eq!(outside_author.and_then(|count| count.as_i64()), Some(9));
    Ok(())
}
