//! `Fake(seed)`, the seeded test-data generator of the script DSL: the golden values of a seed
//! (the reproducibility contract), copies sharing one stream, independent forks, both
//! spellings, a graph built from it, and the argument errors.
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::eval_value;
use graphersal::prelude::*;

fn empty() -> Arc<Graph> {
    Arc::new(Graph::new(TraversalGraph::new()))
}

fn text(script: &str) -> String {
    eval_value(empty(), script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .to_string()
}

fn fail(script: &str) -> String {
    match eval_value(empty(), script) {
        Ok(v) => panic!("{script}: expected an error, got {v}"),
        Err(e) => e.to_string(),
    }
}

/// The reproducibility contract: these values are what seed 42 produces, on every platform and
/// in every version. A change here changes every generated data set; it needs a decision and a
/// release note, not just an updated expectation.
#[test]
fn seed_42_golden_values() {
    let script = r#"
        let f = Fake(42);
        [f.first_name(), f.last_name(), f.full_name(), f.username(), f.email(), f.phone(),
         f.job_title(), f.street_name(), f.street_address(), f.city(), f.state(),
         f.state_abbr(), f.zip_code(), f.country(), f.address(), f.company(), f.domain_name(),
         f.word(), f.sentence(), f.int(1, 6), f.float(), f.float(0, 10), f.chance(0.5),
         f.pick(["a", "b", "c"]), f.shuffle([1, 2, 3, 4, 5]), f.sample([1, 2, 3, 4, 5], 2),
         f.uuid().as_string(), f.seed(), f.fork("x").first_name()]
    "#;
    let expected = r#"["Lawrence", "Young", "Sharon Morgan", "lindap219", "joe.ortiz62@example.com", "+1-589-555-0151", "Lead Support Manager", "Washington Place", "935 Prospect Trail", "San Diego", "New Jersey", "NM", "08339", "Belgium", "7420 School Trail, Chandler, AZ 79162", "Prairie Slate Labs", "ramosbeacon.net", "shadow", "North coral frost sky.", 5, 0.32139128933780203, 0.8448177729996109, true, "b", [4, 3, 1, 2, 5], [2, 1], "d28f82f0-0d3c-444b-af08-0b41cdf27a01", 42, "Philip"]"#;
    assert_eq!(text(script), expected);
}

#[test]
fn the_same_seed_gives_the_same_values() {
    let script = r#"let a = Fake(7); let b = Fake(7);
        let x = []; let y = [];
        for i in 0..50 { x.push(a.full_name()); x.push(a.int(0, 1000)); }
        for i in 0..50 { y.push(b.full_name()); y.push(b.int(0, 1000)); }
        x == y"#;
    assert_eq!(text(script), "true");
    assert_eq!(
        text("Fake(1).full_name() == Fake(2).full_name() && Fake(1).email() == Fake(2).email()"),
        "false"
    );
}

#[test]
fn both_spellings_draw_the_same_values() {
    let snake = text(
        "let f = Fake(3); [f.first_name(), f.last_name(), f.full_name(), f.job_title(), f.street_name(), f.street_address(), f.state_abbr(), f.zip_code(), f.domain_name()]",
    );
    let camel = text(
        "let f = Fake(3); [f.first_name(), f.last_name(), f.full_name(), f.job_title(), f.street_name(), f.street_address(), f.state_abbr(), f.zip_code(), f.domain_name()]",
    );
    assert_eq!(snake, camel);
}

/// A copy (a variable, a function argument) shares the stream: a helper function advances the
/// caller's generator instead of repeating the same value.
#[test]
fn copies_share_one_stream() {
    let script = r#"
        fn name(f) { f.first_name() }
        let f = Fake(42);
        let via_fn = [name(f), name(f), name(f)];
        let g2 = Fake(42);
        let direct = [g2.first_name(), g2.first_name(), g2.first_name()];
        via_fn == direct && via_fn[0] != via_fn[1]
    "#;
    assert_eq!(text(script), "true");
    assert_eq!(
        text(
            "let a = Fake(5); let b = a; a.int(0, 1000000); let c = Fake(5); c.int(0, 1000000); b.int(0, 1000000) == c.int(0, 1000000)"
        ),
        "true"
    );
}

/// A fork depends on the seed and its name only, not on what was drawn before.
#[test]
fn forks_are_independent_streams() {
    let script = r#"
        let a = Fake(9); let b = Fake(9);
        for i in 0..100 { a.word(); }
        let x = a.fork("people"); let y = b.fork("people");
        [x.full_name() == y.full_name(), b.fork("people").full_name() == b.fork("places").full_name(),
         a.fork("p").fork("q").email() == Fake(9).fork("p").fork("q").email(), x.seed()]
    "#;
    assert_eq!(text(script), "[true, false, true, 9]");
}

#[test]
fn an_unseeded_fake_reports_its_seed_for_reproduction() {
    assert_eq!(
        text("let f = Fake(); let s = f.seed(); let a = f.email(); Fake(s).email() == a && s >= 0"),
        "true"
    );
    assert_eq!(text("type_of(Fake())"), "Fake");
    assert_eq!(text("Fake(12).to_string()"), "Fake(seed: 12)");
}

#[test]
fn an_email_for_a_name_uses_that_name() {
    let value = text(r#"Fake(4).email("Mary-Ann  O'Neil")"#);
    assert!(value.starts_with("maryann.oneil"), "{value}");
    assert!(
        value.ends_with("@example.com")
            || value.ends_with("@example.org")
            || value.ends_with("@example.net"),
        "{value}"
    );
    assert!(text(r#"Fake(4).email("  ")"#).starts_with("user"));
}

#[test]
fn a_negative_seed_is_valid() {
    assert_eq!(text("Fake(-1).seed()"), "-1");
    assert_eq!(text("Fake(-1).email() == Fake(-1).email()"), "true");
}

#[test]
fn the_values_feed_a_graph() {
    let script = r#"
        let f = Fake(42);
        for i in 0..20 {
            g.add_v("person").property("name", f.full_name()).property("uid", f.uuid()).property("age", f.int(18, 90)).to_list();
        }
        [g.v().count().next(), g.v().values("age").min().next() >= 18, type_of(g.v().values("uid").next())]
    "#;
    assert_eq!(text(script), r#"[20, true, "Uuid"]"#);
}

#[test]
fn invalid_arguments_are_script_errors() {
    for (script, expected) in [
        ("Fake(1).int(5, 1)", "Fake.int"),
        ("Fake(1).float(1.0, 0.0)", "Fake.float"),
        ("Fake(1).chance(1.5)", "Fake.chance"),
        ("Fake(1).pick([])", "Fake.pick"),
        ("Fake(1).sample([1, 2], 3)", "Fake.sample"),
        ("Fake(1).sample([1, 2], -1)", "Fake.sample"),
    ] {
        let message = fail(script);
        assert!(message.contains(expected), "{script}: {message}");
    }
}

/// The string `graphersal-wasm`'s `fake_values_match_the_native_build` expects in the browser
/// build: keep both in sync.
#[test]
fn the_string_the_wasm_test_pins() {
    assert_eq!(
        text(
            r#"let f = Fake(42); `${f.full_name()}|${f.int(1, 1000000)}|${f.float()}|${f.uuid()}|${f.fork("x").email()}`"#
        ),
        "Lawrence Young|278602|0.34419071652363753|09bc585a-2448-43f2-9e44-31fa3c80db06|philip.davis70@example.org"
    );
}
