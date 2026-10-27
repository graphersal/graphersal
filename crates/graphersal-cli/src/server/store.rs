//! The `/api/store/...` endpoints of a dev server on a Store directory (`--graph <dir>`): thin
//! HTTP wrappers of the session's Store operations (`graphersal_session::store`), which the
//! WebAssembly playground runs on its stores in memory as well. Each one runs on the session
//! thread; the actions that swap the session's graph (view, back to live, rollback, attic
//! restore) cancel a running query first. The server adds only what is about disks: a fork goes
//! to a NEW directory next to the store (`<store>-fork-<target>` by default, `dir` in the body;
//! a single-file store forks into `<name>-fork-<target>.gstore`), it is not opened; a backup
//! goes to `<store>-backup` next to the store by default (an increment when it holds a backup
//! of the store; `dir`, `full`, `from_memory` in the body).
//!
//! A target is a JSON object: `{"commit": N}`, `{"mark": "name"}` or `{"time": T}` with `T` an
//! RFC 3339 date-time (`"2026-10-08T02:43:00Z"`, the CLI's `--at-time`) or microseconds.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use graphersal::persist::{SINGLE_FILE_EXTENSION, Store, store_dir_at};
use graphersal_session::store::{target_label, target_of};
use graphersal_session::{ForkInto, Session, StoreError};
use serde_json::{Value, json};
use tiny_http::{Method, Request};

use super::{Answer, Context, HttpError, download, json_body, json_response, query_value, value};

/// Routes `/api/store` and `/api/store/...` (`endpoint` without the `/api/` prefix).
pub(super) fn route(
    context: &Context,
    request: &mut Request,
    method: &Method,
    endpoint: &str,
    query: &str,
) -> Result<Answer, HttpError> {
    match (method, endpoint) {
        (Method::Get, "store") => match &context.store {
            None => value(json!({ "store": null })),
            Some(_) => value(json!({ "store": session_call(context, |s| s.store_info())? })),
        },
        (Method::Post, "store/checkpoint") => {
            own_store(context)?;
            let name = body_text(&json_body(request)?, "name");
            value(session_call(context, move |s| s.store_checkpoint(&name))?)
        }
        (Method::Post, "store/mark") => {
            own_store(context)?;
            let name = body_text(&json_body(request)?, "name");
            value(session_call(context, move |s| s.store_mark(&name))?)
        }
        (Method::Get, "store/snapshot") => {
            own_store(context)?;
            let commit = match query_value(query, "commit") {
                None => None,
                Some(raw) => Some(raw.parse::<u64>().map_err(|_| {
                    HttpError::bad_request("commit must be a commit sequence number")
                })?),
            };
            // `models=without`: the download without the store's embedding models.
            let models = match query_value(query, "models").as_deref() {
                None | Some("with") => graphersal::persist::ModelCopy::Include,
                Some("without") => graphersal::persist::ModelCopy::Exclude,
                Some(_) => {
                    return Err(HttpError::bad_request(
                        "models must be \"with\" or \"without\"",
                    ));
                }
            };
            let (bytes, commit) =
                session_call(context, move |s| s.store_snapshot_with(commit, models))?;
            Ok(download(bytes, &format!("graph-{commit}.gsnap")))
        }
        (Method::Post, "store/view") => {
            own_store(context)?;
            let body = json_body(request)?;
            // The graph changes under the session: a running query is cancelled.
            stop_running_query(context);
            let answer = session_call(context, move |s| s.store_open_view(&body))?;
            set_view(context, Some(answer["view"].clone()));
            value(answer)
        }
        (Method::Post, "store/live") => {
            own_store(context)?;
            stop_running_query(context);
            let answer = session_call(context, |s| s.store_close_view())?;
            set_view(context, None);
            value(answer)
        }
        (Method::Post, "store/fork") => {
            let store = own_store(context)?;
            let body = json_body(request)?;
            let target = target_of(&body).map_err(http)?;
            let new_dir = fork_dir(
                store_path(&store)?,
                is_single_file(&store),
                &body,
                &target_label(&target),
            )?;
            let into = ForkInto::Dir(store_dir_at(new_dir));
            value(session_call(context, move |s| s.store_fork(&body, into))?)
        }
        (Method::Post, "store/rollback") => {
            own_store(context)?;
            let body = json_body(request)?;
            stop_running_query(context);
            let answer = session_call(context, move |s| s.store_rollback(&body));
            set_view(context, None);
            value(answer?)
        }
        (Method::Get, "store/attic/changes") => {
            own_store(context)?;
            let id = query_value(query, "id")
                .and_then(|id| percent_decode_component(&id))
                .ok_or_else(|| HttpError::bad_request("expected ?id=<attic entry id>"))?;
            let offset = query_number(query, "offset", "offset")?.unwrap_or(0);
            let limit = query_number(query, "limit", "limit")?;
            value(session_call(context, move |s| {
                s.store_attic_changes(&id, offset, limit)
            })?)
        }
        (Method::Post, "store/attic/restore") => {
            own_store(context)?;
            let id = body_id(request)?;
            stop_running_query(context);
            let answer = session_call(context, move |s| s.store_attic_restore(&id));
            set_view(context, None);
            value(answer?)
        }
        (Method::Post, "store/attic/fork") => {
            let store = own_store(context)?;
            let body = json_body(request)?;
            let id = body_text(&body, "id");
            if id.is_empty() {
                return Err(HttpError::bad_request("expected {\"id\": \"...\"}"));
            }
            let new_dir = fork_dir(
                store_path(&store)?,
                is_single_file(&store),
                &body,
                &format!("attic-{id}"),
            )?;
            let into = ForkInto::Dir(store_dir_at(new_dir));
            value(session_call(context, move |s| {
                s.store_attic_fork(&id, into)
            })?)
        }
        (Method::Post, "store/attic/remove") => {
            own_store(context)?;
            let id = body_id(request)?;
            value(session_call(context, move |s| s.store_attic_remove(&id))?)
        }
        (Method::Post, "store/verify") => {
            own_store(context)?;
            value(session_call(context, |s| s.store_verify())?)
        }
        (Method::Post, "store/backup") => {
            let store = own_store(context)?;
            let body = json_body(request)?;
            let full = body.get("full").and_then(Value::as_bool).unwrap_or(false);
            let from_memory = body
                .get("from_memory")
                .or_else(|| body.get("fromMemory"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let target = backup_dir(store_path(&store)?, is_single_file(&store), &body)?;
            let into = store_dir_at(target);
            value(session_call(context, move |s| {
                s.store_backup(into, full, from_memory)
            })?)
        }
        (Method::Get, "store/compaction") => {
            own_store(context)?;
            value(session_call(context, |s| s.store_compaction_advice())?)
        }
        (Method::Post, "store/compact") => {
            own_store(context)?;
            // An empty body is `{}` (a bare `curl -X POST`).
            let body = if request.body_length() == Some(0) {
                json!({})
            } else {
                json_body(request)?
            };
            let drop_marks = body
                .get("dropMarks")
                .or_else(|| body.get("drop_marks"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // Marks that would become unreachable without `dropMarks`: 409 with their list
            // (`{"error", "marksUnreachable"}`), nothing changed.
            match context.call(move |s| s.store_compact(drop_marks))? {
                Ok(answer) => value(answer),
                Err(err) => match err.data {
                    Some(Value::Object(mut fields)) => {
                        fields.insert("error".into(), Value::String(err.message));
                        Ok(json_response(err.status, &Value::Object(fields)))
                    }
                    _ => Err(http(err)),
                },
            }
        }
        (
            _,
            "store"
            | "store/checkpoint"
            | "store/mark"
            | "store/snapshot"
            | "store/view"
            | "store/live"
            | "store/fork"
            | "store/rollback"
            | "store/attic/changes"
            | "store/attic/restore"
            | "store/attic/fork"
            | "store/attic/remove"
            | "store/verify"
            | "store/backup"
            | "store/compaction"
            | "store/compact",
        ) => Err(HttpError(
            405,
            format!("method {method} is not allowed on /api/{endpoint}"),
        )),
        _ => Err(HttpError(404, format!("unknown endpoint /api/{endpoint}"))),
    }
}

fn http(err: StoreError) -> HttpError {
    HttpError(err.status, err.message)
}

/// Runs a session store operation on the session thread; its failure is an HTTP error.
fn session_call<T: Send + 'static>(
    context: &Context,
    job: impl FnOnce(&mut Session) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, HttpError> {
    context.call(job)?.map_err(http)
}

/// The store of the server, or 409.
fn own_store(context: &Context) -> Result<Arc<Store>, HttpError> {
    context.store.clone().ok_or_else(|| {
        HttpError(
            409,
            "the server does not run on a Store (start it with --graph <dir> or --graph <file>.gstore)".into(),
        )
    })
}

/// The directory of the server's store.
fn store_path(store: &Store) -> Result<&Path, HttpError> {
    store
        .dir()
        .local_path()
        .ok_or_else(|| HttpError(409, "the store is not a directory".into()))
}

fn body_text(body: &Value, key: &str) -> String {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn body_id(request: &mut Request) -> Result<String, HttpError> {
    let id = body_text(&json_body(request)?, "id");
    if id.is_empty() {
        return Err(HttpError::bad_request("expected {\"id\": \"...\"}"));
    }
    Ok(id)
}

fn query_number(query: &str, key: &str, what: &str) -> Result<Option<usize>, HttpError> {
    match query_value(query, key) {
        None => Ok(None),
        Some(raw) => raw
            .parse()
            .map(Some)
            .map_err(|_| HttpError::bad_request(format!("{what} must be a number"))),
    }
}

/// The read-only view the session serves, or `null` for the live graph (mirrored here so
/// `GET /api/info` answers without the session thread).
pub(super) fn current_view(context: &Context) -> Value {
    context
        .view
        .lock()
        .ok()
        .and_then(|view| view.clone())
        .unwrap_or(Value::Null)
}

pub(super) fn set_view(context: &Context, view: Option<Value>) {
    if let Ok(mut current) = context.view.lock() {
        *current = view.filter(|v| !v.is_null());
    }
}

/// Cancels the query that runs now (it stops at its next check and rolls back), so an action
/// queued behind it on the session thread does not wait for it.
pub(super) fn stop_running_query(context: &Context) {
    if context.running.load(Ordering::SeqCst) > 0 {
        context.cancel.cancel();
    }
}

/// Whether the store is a single file (its backend keeps garbage to compact).
fn is_single_file(store: &Store) -> bool {
    store.dir().space_usage().unwrap().is_some()
}

/// Keeps letters, digits, `.`, `_` and `-` (the rest becomes `-`).
fn file_safe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The directory of a fork: the body's `dir` (a relative path is next to the store; it must
/// not exist, its parent must; a `.gstore` name is a single file), or by default
/// `<store>-fork-<label>` next to the store, with `-2`, `-3`, ... when that exists (a
/// single-file store forks into `<name>-fork-<label>.gstore`).
fn fork_dir(
    store_dir: &Path,
    single_file: bool,
    body: &Value,
    label: &str,
) -> Result<PathBuf, HttpError> {
    let parent = store_dir.parent().unwrap_or(store_dir).to_path_buf();
    let requested = body_text(body, "dir");
    if !requested.is_empty() {
        let path = Path::new(&requested);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            parent.join(path)
        };
        if path.exists() {
            return Err(HttpError(
                409,
                format!(
                    "'{}' exists already: a fork needs a new directory",
                    path.display()
                ),
            ));
        }
        match path.parent() {
            Some(up) if up.is_dir() => {}
            _ => {
                return Err(HttpError(
                    422,
                    format!(
                        "the parent directory of '{}' does not exist",
                        path.display()
                    ),
                ));
            }
        }
        return Ok(path);
    }
    let name = if single_file {
        store_dir.file_stem()
    } else {
        store_dir.file_name()
    }
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_else(|| "store".to_string());
    let base = format!("{name}-fork-{}", file_safe(label));
    let extension = if single_file {
        format!(".{SINGLE_FILE_EXTENSION}")
    } else {
        String::new()
    };
    for n in 1..1000 {
        let candidate = if n == 1 {
            parent.join(format!("{base}{extension}"))
        } else {
            parent.join(format!("{base}-{n}{extension}"))
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(HttpError(
        409,
        format!("no free directory name like '{base}' next to the store: give one"),
    ))
}

/// The directory of a backup: the body's `dir` (a relative path is next to the store; when it
/// exists it must be a backup of the store, which the backup checks; its parent must exist; a
/// `.gstore` name is a single file), or by default `<store>-backup` next to the store
/// (`<name>-backup.gstore` for a single-file store), which takes increments from then on.
fn backup_dir(store_dir: &Path, single_file: bool, body: &Value) -> Result<PathBuf, HttpError> {
    let parent = store_dir.parent().unwrap_or(store_dir).to_path_buf();
    let requested = body_text(body, "dir");
    if !requested.is_empty() {
        let path = Path::new(&requested);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            parent.join(path)
        };
        if path == store_dir {
            return Err(HttpError(
                409,
                "a backup needs another directory than the store's".into(),
            ));
        }
        match path.parent() {
            Some(up) if up.is_dir() => {}
            _ => {
                return Err(HttpError(
                    422,
                    format!(
                        "the parent directory of '{}' does not exist",
                        path.display()
                    ),
                ));
            }
        }
        return Ok(path);
    }
    let name = if single_file {
        store_dir.file_stem()
    } else {
        store_dir.file_name()
    }
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_else(|| "store".to_string());
    Ok(if single_file {
        parent.join(format!("{name}-backup.{SINGLE_FILE_EXTENSION}"))
    } else {
        parent.join(format!("{name}-backup"))
    })
}

/// Decodes a `%XX`-escaped query value (`+` is a space).
fn percent_decode_component(text: &str) -> Option<String> {
    super::percent_decode(&text.replace('+', " "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal::persist::RecoveryTarget;

    #[test]
    fn targets_and_labels() {
        let target = |body: Value| target_of(&json!({ "target": body }));
        assert_eq!(
            target(json!({"commit": 5})).ok(),
            Some(RecoveryTarget::CommitSeq(5))
        );
        assert_eq!(
            target(json!({"mark": " m1 "})).ok(),
            Some(RecoveryTarget::Mark("m1".into()))
        );
        assert_eq!(
            target(json!({"time": "1970-01-01T00:00:01Z"})).ok(),
            Some(RecoveryTarget::Time(1_999_999))
        );
        assert_eq!(
            target(json!({"time": 42})).ok(),
            Some(RecoveryTarget::Time(42))
        );
        for bad in [
            json!({"commit": -1}),
            json!({"mark": ""}),
            json!({"time": "noon"}),
            json!({}),
        ] {
            assert_eq!(
                target(bad.clone()).err().map(|e| e.status),
                Some(400),
                "{bad}"
            );
        }
        assert!(target_of(&json!({})).is_err());
        // A bare date: refused with the explicit forms (the same text as the CLI's --at-time).
        let bare = target(json!({"time": "2026-10-08"})).unwrap_err();
        assert_eq!(bare.status, 400);
        assert!(
            bare.message.contains("2026-10-08T23:59:59Z")
                && bare.message.contains("2026-10-08T00:00:00+02:00"),
            "{}",
            bare.message
        );
        assert_eq!(target_label(&RecoveryTarget::CommitSeq(5)), "commit-5");
        assert_eq!(
            target_label(&RecoveryTarget::Time(1_760_000_000_000_000)),
            "time-20251009T085320Z"
        );
        assert_eq!(file_safe("mark-a b/c"), "mark-a-b-c");
    }

    #[test]
    fn fork_directories_default_next_to_the_store() {
        let base = std::env::temp_dir().join(format!("graphersal_forkdir_{}", std::process::id()));
        let store = base.join("data");
        std::fs::create_dir_all(&store).unwrap();
        let first = fork_dir(&store, false, &json!({}), "commit-3").unwrap();
        assert_eq!(first, base.join("data-fork-commit-3"));
        std::fs::create_dir_all(&first).unwrap();
        assert_eq!(
            fork_dir(&store, false, &json!({}), "commit-3").unwrap(),
            base.join("data-fork-commit-3-2")
        );
        assert_eq!(
            fork_dir(&store, false, &json!({"dir": "branch"}), "x").unwrap(),
            base.join("branch")
        );
        assert_eq!(
            fork_dir(&store, false, &json!({"dir": "data-fork-commit-3"}), "x")
                .err()
                .map(|e| e.0),
            Some(409)
        );
        assert_eq!(
            fork_dir(&store, false, &json!({"dir": "missing/deeper"}), "x")
                .err()
                .map(|e| e.0),
            Some(422)
        );
        // A single-file store forks into a .gstore file next to it.
        let file = base.join("graph.gstore");
        assert_eq!(
            fork_dir(&file, true, &json!({}), "mark-m").unwrap(),
            base.join("graph-fork-mark-m.gstore")
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
