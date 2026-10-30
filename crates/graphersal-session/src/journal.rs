//! The system journal of the served graph (book page "System Journal"): one page of records
//! as JSON, for the dev server (`GET /api/journal`), the MCP tool `journal` and the wasm build.

use std::sync::Arc;

use graphersal::auth::{AccessRequest, Action, Authorizer, Resource};
use graphersal::system_journal::{JournalQuery, Level};
use serde_json::Value;

use crate::session::{Session, playground_authorizer};
use crate::{SessionError, store::parse_time};

/// The keys a journal request may have.
const KEYS: [&str; 9] = [
    "after", "level", "since", "until", "source", "kind", "op", "origin", "limit",
];

/// The [`JournalQuery`] of a request object: `after` (the cursor), `level` (`error`, `warn`,
/// `notice`, `info`: that level and the more severe ones), `since`/`until` (µs since the Unix
/// epoch, or an RFC 3339 date-time with a zone), `source` (a source and the ones below it),
/// `kind`, `op`, `origin` (an instance id, canonical UUID), `limit` (default 1000, at most
/// 10000). A missing or `null` key is no filter; an unknown key or a bad value is
/// [`SessionError::Invalid`].
pub fn journal_query(spec: &Value) -> Result<JournalQuery, SessionError> {
    let mut query = JournalQuery::new();
    let empty = serde_json::Map::new();
    let object = match spec {
        Value::Null => &empty,
        Value::Object(object) => object,
        _ => return Err(SessionError::invalid("a journal request is a JSON object")),
    };
    if let Some(key) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(SessionError::invalid(format!(
            "unknown journal request key '{key}': use {}",
            KEYS.join(", ")
        )));
    }
    let number = |key: &str| -> Result<Option<u64>, SessionError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => n
                .as_u64()
                .map(Some)
                .ok_or_else(|| SessionError::invalid(format!("'{key}' is a non-negative integer"))),
            Some(Value::String(text)) => text.trim().parse().map(Some).map_err(|_| {
                SessionError::invalid(format!("'{key}' is a non-negative integer, not {text:?}"))
            }),
            Some(_) => Err(SessionError::invalid(format!(
                "'{key}' is a non-negative integer"
            ))),
        }
    };
    let text = |key: &str| -> Result<Option<String>, SessionError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(_) => Err(SessionError::invalid(format!("'{key}' is a string"))),
        }
    };
    if let Some(after) = number("after")? {
        query = query.after(after);
    }
    if let Some(level) = text("level")? {
        let level = Level::from_name(&level).ok_or_else(|| {
            SessionError::invalid(format!(
                "unknown level '{level}': use error, warn, notice or info"
            ))
        })?;
        query = query.min_level(level);
    }
    if let Some(since) = text("since")? {
        query = query.since(parse_time(&since)?);
    }
    if let Some(until) = text("until")? {
        query = query.until(parse_time(&until)?);
    }
    if let Some(source) = text("source")? {
        query = query.source_prefix(source);
    }
    if let Some(kind) = text("kind")? {
        query = query.kind(kind);
    }
    if let Some(op) = number("op")? {
        query = query.op(op);
    }
    if let Some(origin) = text("origin")? {
        let id = parse_uuid(&origin).ok_or_else(|| {
            SessionError::invalid(format!(
                "'origin' is an instance id (a UUID), not {origin:?}"
            ))
        })?;
        query = query.origin(id);
    }
    if let Some(limit) = number("limit")? {
        query = query.limit(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    Ok(query)
}

/// A canonical (hyphenated or plain hex) UUID.
fn parse_uuid(text: &str) -> Option<u128> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    (hex.len() == 32)
        .then(|| u128::from_str_radix(&hex, 16).ok())
        .flatten()
}

impl Session {
    /// One page of the served graph's system journal (`{records, next_after, truncated, more,
    /// notes}`, each record `{seq, origin, origin_seq, time, level, source, kind, message,
    /// fields, ...}`) for the request `spec` ([`journal_query`]), under the playground's
    /// authorizer (reading the journal is `Read` on `Journal`).
    pub fn journal(&self, spec: &Value) -> Result<Value, SessionError> {
        self.journal_with_authorizer(spec, playground_authorizer())
    }

    /// [`Self::journal`] under `authorizer`: a denial of `Read` on
    /// [`Resource::Journal`] is [`SessionError::Failed`] with its reason.
    pub fn journal_with_authorizer(
        &self,
        spec: &Value,
        authorizer: Arc<dyn Authorizer>,
    ) -> Result<Value, SessionError> {
        let query = journal_query(spec)?;
        let request = AccessRequest::new(Action::Read, Resource::Journal);
        authorizer.authorize(&request).map_err(|denied| {
            SessionError::failed(format!(
                "reading the system journal is not allowed: {}",
                denied.reason()
            ))
        })?;
        let journal = self.graph().system_journal()?;
        let page = journal
            .query(&query)
            .map_err(|err| SessionError::failed(format!("{err}\nHelp: {}", err.help())))?;
        serde_json::from_str(&page.to_json())
            .map_err(|err| SessionError::failed(format!("the journal page is not JSON: {err}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_becomes_a_query_and_bad_ones_are_refused() {
        let query = journal_query(&json!({
            "after": 3, "level": "warn", "source": "store", "kind": "store.opened", "limit": 5,
            "since": "2026-10-30T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(query.after, 3);
        assert_eq!(query.min_level, Some(Level::Warn));
        assert_eq!(query.limit, 5);
        assert!(query.since.is_some());
        assert!(journal_query(&json!({"levle": "warn"})).is_err());
        assert!(journal_query(&json!({"level": "loud"})).is_err());
        assert!(journal_query(&json!({"after": -1})).is_err());
        assert!(journal_query(&json!([])).is_err());
        assert_eq!(journal_query(&Value::Null).unwrap(), JournalQuery::new());
    }

    #[test]
    fn the_session_shows_its_graphs_journal_and_asks_the_authorizer() {
        let session = Session::default();
        let page = session.journal(&json!({})).unwrap();
        assert!(page["records"].is_array(), "{page}");
        assert_eq!(page["truncated"], json!(false));
        let denied = session.journal_with_authorizer(
            &json!({}),
            Arc::new(graphersal::auth::AccessPolicy::deny_all()),
        );
        assert!(denied.unwrap_err().to_string().contains("not allowed"));
        let allowed = session.journal_with_authorizer(
            &json!({}),
            Arc::new(graphersal::auth::AccessPolicy::read_only()),
        );
        assert!(allowed.is_ok());
    }

    #[test]
    fn a_denied_query_is_recorded_without_its_text_and_repeats_collapse() {
        let mut session = Session::default();
        for _ in 0..3 {
            let answer: Value = serde_json::from_str(&session.execute_with_authorizer(
                r#"g.add_v("secret_label").next()"#,
                &crate::RunOptions::default(),
                crate::read_only_authorizer(),
            ))
            .unwrap();
            assert_eq!(answer["ok"], false, "{answer}");
        }
        let page = session.journal(&json!({"kind": "auth.denied"})).unwrap();
        let records = page["records"].as_array().unwrap();
        assert_eq!(records.len(), 1, "repeats are counted, not written: {page}");
        let record = &records[0];
        assert_eq!(record["level"], "warn");
        assert_eq!(record["fields"]["action"], "Create");
        assert_eq!(record["fields"]["resource_kind"], "Data");
        assert!(!record.to_string().contains("secret_label"), "{record}");
    }
}
