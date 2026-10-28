//! Integration tests for TinkerPop 3.8's `to(Direction, labels)`, `toE(Direction, labels)` and
//! `toV(Direction)`, in the Rust API (`to_direction`/`to_e`/`to_v`) and both DSL spellings.

#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(feature = "script")]
use crate::script_host::eval_value;
use graphersal::prelude::*;

#[cfg(feature = "script")]
fn sorted(script: &str) -> Vec<String> {
    let mut out: Vec<String> = eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
        .unwrap()
        .into_array()
        .unwrap()
        .into_iter()
        .map(|v| v.to_string())
        .collect();
    out.sort();
    out
}

#[test]
fn rust_equals_the_direction_specific_steps() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = || lock.traversal().v(None::<()>);
    let count = |list: Vec<ElementProperty>| list.len();
    assert_eq!(
        count(g().to_direction(Direction::Out, "knows").to_list().unwrap()),
        count(g().out("knows").to_list().unwrap())
    );
    assert_eq!(
        count(g().to_direction(Direction::In, None).to_list().unwrap()),
        count(g().in_(None).to_list().unwrap())
    );
    assert_eq!(
        count(g().to_direction(Direction::Both, None).to_list().unwrap()),
        12
    );
    assert_eq!(count(g().to_e(Direction::Out, None).to_list().unwrap()), 6);
    assert_eq!(
        count(g().to_e(Direction::In, "created").to_list().unwrap()),
        4
    );
    assert_eq!(
        count(g().to_e(Direction::Both, "knows").to_list().unwrap()),
        4
    );
    let e = || lock.traversal().e(None::<()>);
    assert_eq!(
        e().to_v(Direction::Out).values("name").to_list().unwrap(),
        e().out_v().values("name").to_list().unwrap()
    );
    assert_eq!(
        e().to_v(Direction::In).values("name").to_list().unwrap(),
        e().in_v().values("name").to_list().unwrap()
    );
    assert_eq!(count(e().to_v(Direction::Both).to_list().unwrap()), 12);
    let anon = lock
        .traversal()
        .v("1")
        .local(__::to_direction(Direction::Out, "knows"))
        .to_list()
        .unwrap();
    assert_eq!(anon.len(), 2);
}

#[cfg(feature = "script")]
#[test]
fn dsl_to() {
    assert_eq!(
        sorted(r#"g.V("1").to(Direction.OUT, "knows").values("name").toList()"#),
        vec!["josh", "vadas"]
    );
    assert_eq!(
        sorted(r#"g.V("1").to(Direction::OUT, ["knows", "created"]).values("name").to_list()"#),
        vec!["josh", "lop", "vadas"]
    );
    assert_eq!(
        sorted(r#"g.V("3").to(Direction.IN).values("name").toList()"#),
        sorted(r#"g.V("3").in().values("name").toList()"#)
    );
    assert_eq!(
        sorted(r#"g.V("4").to(Direction.BOTH, "knows", "created").values("name").toList()"#),
        vec!["lop", "marko", "ripple"]
    );
    assert_eq!(
        sorted(r#"g.V("1").to(Direction.from).values("name").toList()"#),
        sorted(r#"g.V("1").out().values("name").toList()"#)
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_to_e_both_spellings() {
    for step in ["toE", "to_e"] {
        assert_eq!(
            sorted(&format!(
                r#"[g.V("1").{step}(Direction.OUT).count().next()]"#
            )),
            vec!["3"]
        );
        assert_eq!(
            sorted(&format!(
                r#"[g.V("3").{step}(Direction.IN, "created").count().next()]"#
            )),
            vec!["3"]
        );
        assert_eq!(
            sorted(&format!(
                r#"[g.V("4").{step}(Direction.BOTH, ["knows"]).count().next()]"#
            )),
            vec!["1"]
        );
    }
}

#[cfg(feature = "script")]
#[test]
fn dsl_to_v_both_spellings() {
    let out = sorted(r#"g.E().outV().values("name").toList()"#);
    let inn = sorted(r#"g.E().inV().values("name").toList()"#);
    for step in ["toV", "to_v"] {
        assert_eq!(
            sorted(&format!(
                r#"g.E().{step}(Direction.OUT).values("name").toList()"#
            )),
            out
        );
        assert_eq!(
            sorted(&format!(
                r#"g.E().{step}(Direction::IN).values("name").toList()"#
            )),
            inn
        );
        assert_eq!(
            sorted(&format!(r#"[g.E().{step}(Direction.BOTH).count().next()]"#)),
            vec!["12"]
        );
    }
}

#[cfg(feature = "script")]
#[test]
fn dsl_to_still_modulates_add_e() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let count = eval_value(
        graph.clone(),
        r#"g.V("1").addE("likes").to("2").count().next()"#,
    )
    .unwrap();
    assert_eq!(count.as_int().unwrap(), 1);
    let names = eval_value(graph, r#"g.V("1").out("likes").values("name").toList()"#)
        .unwrap()
        .into_array()
        .unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].to_string(), "vadas");
}

#[cfg(feature = "script")]
#[test]
fn dsl_to_with_a_non_direction_first_argument_is_a_missing_overload() {
    let err = eval_value(
        Arc::new(GraphSource::tinkerpop_modern()),
        r#"g.V("1").to("a", "b")"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Function not found"), "{err}");
}
