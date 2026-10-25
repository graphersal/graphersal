//! The storage advisor in the session (book page "Optimize", [`graphersal::advisor`]): the
//! operations behind the playground's Optimize panel, the dev server's `/api/advise*` endpoints
//! and the MCP tool `advise`.
//!
//! JSON forms:
//!
//! - a **spec** (`advise`, `apply_recommendation`): `{everything?, min_filter_runs?,
//!   min_index_elements?, min_observed_days?, min_compression_bytes?, min_saving_bytes?,
//!   min_saving_percent?, hot_reads?, max_estimates?}` (or `null`): the thresholds of
//!   [`AdviceRequest`], from its defaults, or from [`AdviceRequest::everything`] (every
//!   candidate, for small graphs) with `everything: true`; a given threshold wins either way;
//! - the **advice** (`advise`): `{ok: true, advice}` with [`graphersal::advisor::Advice::to_property`]
//!   as JSON (`{workload_level, observed_since, commit_seq, property_bytes, estimates,
//!   recommendations: [{id, kind, element, label, key, title, evidence, cost, memory_bytes,
//!   measured, confidence, reversible, command, action}]}`), or `{ok: false, error}`;
//! - **apply** (`apply_recommendation`): recomputes the advice with the same spec, finds the
//!   recommendation `id` and runs its catalog command through the session's own operation
//!   (`create_index`, `drop_index`, `define_compression`, `drop_compression`: one commit, the
//!   change feed records it); answers `{ok: true, id, kind, result}` (the operation's answer)
//!   or `{ok: false, error}` (gone meanwhile, refused).

use graphersal::advisor::{AdviceRequest, RecommendedAction};
use graphersal::storage::ElementKind;
use serde_json::{Value, json};

use crate::Session;

/// The keys a spec may have.
const KEYS: [&str; 9] = [
    "everything",
    "min_filter_runs",
    "min_index_elements",
    "min_observed_days",
    "min_compression_bytes",
    "min_saving_bytes",
    "min_saving_percent",
    "hot_reads",
    "max_estimates",
];

/// The request of a spec (module docs).
pub fn advice_request(spec: &Value) -> Result<AdviceRequest, String> {
    let object = match spec {
        Value::Null => return Ok(AdviceRequest::new()),
        Value::Object(object) => object,
        _ => return Err("the advice spec is a JSON object of thresholds".to_string()),
    };
    if let Some(unknown) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!(
            "unknown key '{unknown}' (the keys are {})",
            KEYS.join(", ")
        ));
    }
    let mut request = match object.get("everything") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => AdviceRequest::new(),
        Some(Value::Bool(true)) => AdviceRequest::everything(),
        Some(_) => return Err("everything is true or false".to_string()),
    };
    let number = |key: &str| -> Result<Option<u64>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| format!("{key} is a non-negative integer")),
        }
    };
    if let Some(n) = number("min_filter_runs")? {
        request.min_filter_runs = n;
    }
    if let Some(n) = number("min_index_elements")? {
        request.min_index_elements = n;
    }
    if let Some(n) = number("min_observed_days")? {
        request.min_observed_days = n;
    }
    if let Some(n) = number("min_compression_bytes")? {
        request.min_compression_bytes = n;
    }
    if let Some(n) = number("min_saving_bytes")? {
        request.min_saving_bytes = n;
    }
    if let Some(n) = number("min_saving_percent")? {
        request.min_saving_percent = n;
    }
    if let Some(n) = number("hot_reads")? {
        request.hot_reads = n;
    }
    if let Some(n) = number("max_estimates")? {
        request.max_estimates = usize::try_from(n).unwrap_or(usize::MAX);
    }
    Ok(request)
}

impl Session {
    /// The storage advisor's recommendations for the served graph (module docs); cancellable
    /// through [`Self::cancel_token`]. Read-only.
    pub fn advise(&self, spec: &Value) -> Value {
        match self.advice(spec) {
            Ok(advice) => json!({
                "ok": true,
                "advice": serde_json::from_str::<Value>(&advice.to_json()).unwrap_or(Value::Null),
            }),
            Err(error) => json!({"ok": false, "error": error}),
        }
    }

    fn advice(&self, spec: &Value) -> Result<graphersal::advisor::Advice, String> {
        let request = advice_request(spec)?;
        let cancel = self.cancel_token();
        cancel.reset();
        cancel
            .scope(|| self.graph.advise(&request))
            .map_err(|err| graphersal::error::format_diagnostic(&*err))
    }

    /// Applies the recommendation `id` of the advice for `spec` (module docs).
    pub fn apply_recommendation(&mut self, id: &str, spec: &Value) -> Value {
        let advice = match self.advice(spec) {
            Ok(advice) => advice,
            Err(error) => return json!({"ok": false, "error": error}),
        };
        let Some(recommendation) = advice.recommendations.into_iter().find(|r| r.id == id) else {
            return json!({
                "ok": false,
                "error": format!(
                    "the recommendation '{id}' is not advised any more (the data or the \
                     workload changed): compute the advice again"
                ),
            });
        };
        let kind = recommendation.action.kind();
        let element = |element: &ElementKind| element.as_str().to_string();
        let result = match &recommendation.action {
            RecommendedAction::CreateIndex {
                element: kind_of,
                label,
                key,
            } => {
                let answer = self.create_index(
                    &json!({"element": element(kind_of), "label": label, "key": key}),
                );
                if answer["ok"] != true {
                    return refused(&answer);
                }
                answer
            }
            RecommendedAction::DropIndex { name } => match self.drop_index(name) {
                Ok(dropped) => json!({"dropped": dropped}),
                Err(error) => return json!({"ok": false, "error": error}),
            },
            RecommendedAction::AddCompression {
                name,
                element: kind_of,
                label,
                key,
                candidate,
            } => {
                let answer = self.define_compression(&json!({
                    "name": name,
                    "element": element(kind_of),
                    "label": label,
                    "path": key,
                    "codec": candidate.codec.name(),
                    "minBytes": candidate.min_bytes,
                    "dictionary": candidate.dictionary,
                }));
                if answer["ok"] != true {
                    return refused(&answer);
                }
                answer
            }
            RecommendedAction::DropCompression { name } => match self.drop_compression(name) {
                Ok(dropped) => json!({"dropped": dropped}),
                Err(error) => return json!({"ok": false, "error": error}),
            },
            _ => {
                return json!({
                    "ok": false,
                    "error": format!("this version cannot apply a recommendation of kind {kind}"),
                });
            }
        };
        json!({"ok": true, "id": id, "kind": kind, "result": result})
    }
}

/// A refused catalog operation's field errors as one error text.
fn refused(answer: &Value) -> Value {
    let text = answer["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e["message"].as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    json!({"ok": false, "error": text})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recommendation_is_listed_and_applied_by_its_id() {
        let mut s = Session::new();
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        for _ in 0..2 {
            let answer = s.execute(
                r#"g.v().has_label("person").has("age", P.gt(30)).values("name").to_list()"#,
                &crate::RunOptions::default(),
            );
            assert!(answer.contains("josh"), "{answer}");
        }
        let spec = json!({"everything": true});
        let advice = s.advise(&spec);
        assert_eq!(advice["ok"], true, "{advice}");
        let advice = &advice["advice"];
        assert_eq!(advice["workload_level"], "persisted");
        let index = advice["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "create_index:vertex:person:age")
            .cloned()
            .unwrap_or_else(|| panic!("{advice}"));
        assert_eq!(index["kind"], "create_index");
        assert_eq!(index["action"]["key"], "age");
        assert!(
            index["command"]
                .as_str()
                .unwrap()
                .starts_with("g.create_index")
        );
        let applied = s.apply_recommendation("create_index:vertex:person:age", &spec);
        assert_eq!(applied["ok"], true, "{applied}");
        assert_eq!(applied["result"]["index"]["name"], "person_age");
        // Gone now; applying it again is refused with a reason.
        let again = s.apply_recommendation("create_index:vertex:person:age", &spec);
        assert_eq!(again["ok"], false);
        assert!(
            again["error"].as_str().unwrap().contains("not advised"),
            "{again}"
        );
    }

    #[test]
    fn a_wrong_spec_is_refused() {
        let s = Session::new();
        assert!(advice_request(&json!({"min_filter_runs": -1})).is_err());
        assert!(advice_request(&json!({"every": true})).is_err());
        let request = advice_request(&json!({"everything": true, "hot_reads": 5})).unwrap();
        assert_eq!(request.hot_reads, 5);
        assert_eq!(request.min_filter_runs, 1);
        assert_eq!(s.advise(&json!([]))["ok"], false);
        // Without a store: no workload statistics, the data alone.
        assert_eq!(s.advise(&Value::Null)["advice"]["workload_level"], "off");
    }
}
