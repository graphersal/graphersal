//! The run administration (issue #32, off by default: `ContextBuilder::admin_runs`):
//! `GET /api/admin/runs` lists every queued or running run of the context's run registry
//! whatever its client, `POST /api/admin/runs/cancel` `{"id": n}` cancels one by its id. A
//! separate surface from a client's own `POST /api/cancel`, which reaches only that client's
//! runs. The router (`api/mod.rs`) keeps one arm per endpoint; the checks and answers are here.

use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::context::Context;
use crate::http::{HttpError, Request};
use crate::runs::RunEntry;

/// The header that carries the run administration's token.
pub(crate) const TOKEN_HEADER: &str = "X-Graphersal-Admin-Token";

/// The `404` text while the run administration is off (never `unknown endpoint`: the
/// endpoint exists, the host did not turn it on).
pub(crate) const OFF: &str = "the run administration is off: start the server with --admin-runs";

/// Whether `request` may use the run administration of `context`: on, and the token (if the
/// host set one) sent in [`TOKEN_HEADER`].
pub(crate) fn check(context: &Context, request: &Request) -> Result<(), HttpError> {
    let admin = context
        .admin_runs
        .as_ref()
        .ok_or_else(|| HttpError(404, OFF.to_string()))?;
    let Some(token) = &admin.token else {
        return Ok(());
    };
    match request.header(TOKEN_HEADER) {
        Some(sent) if same_token(sent.trim().as_bytes(), token.as_bytes()) => Ok(()),
        Some(_) => Err(HttpError(
            403,
            format!("the run administration refused the token in {TOKEN_HEADER}"),
        )),
        None => Err(HttpError(
            403,
            format!("the run administration needs its token: send the header {TOKEN_HEADER}"),
        )),
    }
}

/// Compares two tokens in a time that does not depend on where they differ.
fn same_token(sent: &[u8], token: &[u8]) -> bool {
    sent.len() == token.len()
        && sent
            .iter()
            .zip(token)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// `GET /api/admin/runs`: `{runs: [...], textHeadChars}`, the runs registered under this
/// context's mount path, oldest first.
pub(crate) fn list(context: &Context) -> Result<Value, HttpError> {
    let mount = context.mount_path.as_deref();
    let runs: Vec<Value> = context
        .runs
        .list()
        .map_err(|err| HttpError(500, err.to_string()))?
        .iter()
        .filter(|run| run.info().mount() == mount)
        .map(run_value)
        .collect();
    Ok(json!({
        "runs": runs,
        "textHeadChars": crate::RUN_TEXT_HEAD_CHARS,
    }))
}

/// One run of the list.
fn run_value(run: &RunEntry) -> Value {
    let info = run.info();
    let started_at_ms = info
        .started_at()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0);
    json!({
        "id": info.id(),
        "client": info.client(),
        "alias": info.alias(),
        "kind": info.kind().name(),
        "state": run.state().name(),
        "cancelled": run.is_cancelled(),
        "startedAtMs": started_at_ms,
        "durationMs": run.duration().as_millis() as u64,
        "text": info.text(),
        "identity": info.identity(),
        "mount": info.mount(),
    })
}

/// `POST /api/admin/runs/cancel` `{"id": n}`: `{cancelled}`, whether such a run (under this
/// context's mount path) was registered.
pub(crate) fn cancel(context: &Context, request: &mut Request) -> Result<Value, HttpError> {
    let body = request.json_body()?;
    let id = body
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| HttpError::bad_request("expected {\"id\": <run id>}"))?;
    let cancelled = context
        .runs
        .cancel_run_in(id, context.mount_path.as_deref());
    if cancelled {
        tracing::info!(target: "graphersal::server", run = id, "run cancelled by the run administration");
    }
    Ok(json!({ "cancelled": cancelled }))
}

#[cfg(test)]
mod tests {
    use super::same_token;

    #[test]
    fn tokens_compare_whole() {
        assert!(same_token(b"s3cret", b"s3cret"));
        assert!(!same_token(b"s3cre", b"s3cret"));
        assert!(!same_token(b"s3cret!", b"s3cret"));
        assert!(!same_token(b"s3creT", b"s3cret"));
        assert!(!same_token(b"", b"s3cret"));
    }
}
