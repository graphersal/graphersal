//! A non-finite float in value JSON (`to_json()`, the `json` visualizer, `execute().to_json()`)
//! is GraphSON's typed double `{"@type":"g:Double","@value":"NaN"}` (`"Infinity"`,
//! `"-Infinity"`), never an error; the plain serde of a value stays strict (`test_serde.rs`).
#![cfg(all(feature = "script", feature = "serde"))]

use graphersal::prelude::*;

use crate::script_host::*;

fn text(script: &str) -> String {
    let graph = std::sync::Arc::new(Graph::new(TraversalGraph::new()));
    let value = eval_value(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"));
    match value.try_cast_result::<RenderedText>() {
        Ok(rendered) => rendered.to_string(),
        Err(value) => value.to_string(),
    }
}

fn json(script: &str) -> serde_json::Value {
    let text = text(script);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{script}: {err}: {text}"))
}

#[test]
fn to_json_writes_typed_doubles() {
    let nan = serde_json::json!({"@type": "g:Double", "@value": "NaN"});
    let inf = serde_json::json!({"@type": "g:Double", "@value": "Infinity"});
    let ninf = serde_json::json!({"@type": "g:Double", "@value": "-Infinity"});
    assert_eq!(
        json("g.inject(0.0 / 0.0, 1.0 / 0.0, -1.0 / 0.0, 1.5).to_json()"),
        serde_json::json!([nan, inf, ninf, 1.5])
    );
    assert_eq!(
        json(r#"g.inject(#{xs: [0.0 / 0.0], s: "NaN"}).visualize(V.Json)"#),
        serde_json::json!([{"xs": [nan], "s": "NaN"}])
    );
    let execution = json("g.inject(1.0 / 0.0).execute().to_json()");
    assert_eq!(
        execution["results"],
        serde_json::json!([inf]),
        "{execution}"
    );
}

#[test]
fn a_stored_non_finite_property_is_typed_in_json() {
    let value = json(
        r#"g.add_v("x").property(T.id, "a").property("w", 0.0 / 0.0).iterate();
           g.v("a").value_map("w").to_json()"#,
    );
    assert_eq!(
        value,
        serde_json::json!([{"w": [{"@type": "g:Double", "@value": "NaN"}]}])
    );
}
