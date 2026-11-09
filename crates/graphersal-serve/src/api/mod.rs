//! The playground HTTP API: the playground's engine boundary over HTTP. Every endpoint calls one
//! method of the shared [`graphersal_session::Session`], so the answers are the structures the
//! WebAssembly build returns; the UI detects a server through `GET /api/info` and sends every
//! engine call to it instead of to its WebAssembly worker
//! (`playground/js/engine/http-engine.js`). Static paths serve the playground UI ([`Ui`]).
//!
//! `POST /api/cancel` and `GET /api/info` never wait for the session thread, so Stop works while
//! a query runs. Every run a client may cancel (a query, a layout slice, an MCP query, an AI Chat
//! tool call) has a [`CancelToken`] of its own, registered before its job is queued
//! ([`crate::runs`]): Stop cancels the page's run `runId` or all its runs, never another
//! client's; a run cancelled while it waits stops when it starts, a running one at its next
//! check, and its unit rolls back.
//!
//! Safety for a local endpoint without authentication: a request whose `Host` is not this
//! server (DNS rebinding) or whose `Origin` is another site is refused (403;
//! [`crate::HostCheck`]); a write needs `Content-Type: application/json`, which a cross-site
//! page cannot send without a CORS preflight that this server never answers; scripts cannot
//! touch files (the playground's authorizer); bodies are limited to
//! [`crate::MAX_BODY_BYTES`]; a static path cannot leave the UI directory (canonicalized and
//! checked).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

use graphersal::{GraphSource, exec::CancelToken};
use graphersal_session::store::{backup_value, damage_value, found_damage_value};
use graphersal_session::{RunOptions, playground_authorizer, views};
use serde_json::{Value, json};

use crate::context::{CLIENT, Client, Context, position};
use crate::http::{HttpError, Method, Request, Response};
use crate::runs::RunKind;
use crate::ui::Ui;
use crate::{job_workers, mcp, runs};

mod layout;
mod store;

#[cfg(feature = "mcp")]
pub(crate) use store::current_view;

#[cfg(test)]
mod boundary_tests;

/// The `server` value of `GET /api/info` the UI recognizes.
pub const SERVER_ID: &str = "graphersal-dev";

/// The `X-Graphersal-Client` id of a request: up to 64 letters, digits, `-` and `_`.
fn client_id(request: &Request) -> String {
    request
        .header("X-Graphersal-Client")
        .map(|id| {
            id.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .take(64)
                .collect()
        })
        .unwrap_or_default()
}

/// Answers one request of the API with the context that serves it: the request filter
/// ([`crate::RequestFilter`], default the `Host`/`Origin` checks), then the endpoint (`/mcp`,
/// `/openapi.json`, `/api/...`, the host's extra routes, else a static file of the UI). A
/// failure is answered as `{"error": message}`; a `503` (the host's job queue is busy) carries
/// `Retry-After: 1`. A host with a lifecycle (probes, loading, stopping) or several graphs calls
/// [`crate::Server::handle`], which ends here.
///
/// The client of the request (`X-Graphersal-Client`, `mcp` for `/mcp`) is this thread's client
/// from here on: the change feed and the run registry name it.
pub fn handle(context: &Arc<Context>, mut request: Request) -> Response {
    let client = if request.url().split('?').next() == Some("/mcp") {
        "mcp".to_string()
    } else {
        client_id(&request)
    };
    CLIENT.with(|c| *c.borrow_mut() = Client::with_detail(&client, None));
    let answer = context.filter.filter(&mut request).and_then(|()| {
        let identity = request.identity().map(str::to_string);
        CLIENT.with(|c| c.borrow_mut().identity = identity);
        route(context, &mut request)
    });
    answer.unwrap_or_else(error_response)
}

/// The answer of a failed request: `{"error": message}`; a `503` (the host's job queue is busy)
/// also with `Retry-After: 1`, so the client retries.
pub(crate) fn error_response(error: HttpError) -> Response {
    match error {
        HttpError(503, message) => {
            Response::json(503, &json!({ "error": message })).with_header("Retry-After", "1")
        }
        HttpError(status, message) => Response::json(status, &json!({ "error": message })),
    }
}

/// Routes a request the filter accepted (DNS rebinding and cross-site requests are the default
/// filter's: [`crate::HostCheck::check`]).
fn route(context: &Arc<Context>, request: &mut Request) -> Result<Response, HttpError> {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let method = request.method().clone();
    if path == "/mcp" {
        return mcp::route(context, request);
    }
    if path == "/openapi.json" || path == "/api/openapi.json" {
        return match method {
            Method::Get | Method::Head => Ok(Response::json(200, &openapi())),
            _ => Err(HttpError(
                405,
                format!("method {method} is not allowed on {path}"),
            )),
        };
    }
    if !path.starts_with("/api/") {
        return match method {
            Method::Get | Method::Head => serve_static(&context.ui, path),
            _ => Err(HttpError(405, "static files are read with GET".into())),
        };
    }
    let endpoint = &path[5..];
    match (&method, endpoint) {
        (Method::Get, "info") => Ok(Response::json(200, &info(context))),
        (Method::Get, "changes") => Ok(Response::json(200, &changes(context))),
        (Method::Get, "mcp/stats") => mcp::stats(context),
        // Stop of a page: its run `runId`, or all its runs; never another client's.
        (Method::Post, "cancel") => {
            let run_id = cancel_run_id(request)?;
            let key = CLIENT.with(|c| c.borrow().pool_key());
            let cancelled = match run_id {
                Some(id) => context.runs.cancel_alias(&key, &id),
                None => context.runs.cancel_client(&key),
            };
            Ok(Response::json(200, &json!({ "cancelled": cancelled })))
        }
        // Scheduled jobs belong to a Store, which the read-only serving mode does not open.
        (
            Method::Post,
            "jobs/define" | "jobs/drop" | "jobs/enable" | "jobs/schedule" | "jobs/run",
        ) if context.queue().is_some() => Err(HttpError(
            409,
            context
                .queue()
                .map(|queue| queue.scheduled_jobs_refusal())
                .unwrap_or_default(),
        )),
        (Method::Get, "graph") => value(context.call_fallible(|s| s.info())?),
        (Method::Post, "execute") => {
            let body = request.json_body()?;
            let script = body
                .get("script")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    HttpError::bad_request("expected {\"script\": \"...\", \"options\": {...}}")
                })?
                .to_string();
            let options: RunOptions = match body.get("options") {
                None | Some(Value::Null) => RunOptions::default(),
                Some(options) => serde_json::from_value(options.clone())
                    .map_err(|err| HttpError::bad_request(format!("invalid options: {err}")))?,
            };
            let run_id = run_id_of(&body)?;
            let cancel = CancelToken::new();
            let _run = context.register_run(
                run_id.as_deref(),
                cancel.clone(),
                RunKind::Query,
                Some(&script),
            );
            let answer = context.call(move |s| {
                s.execute_with_cancel(&script, &options, playground_authorizer(), &cancel)
            });
            Ok(Response::text(200, answer?))
        }
        (Method::Get, "schema") => value(context.call_fallible(|s| s.graph().schema_view())?),
        (Method::Post, "schema/view") => {
            let body = request.text_body()?;
            value(context.call_fallible(move |s| s.schema_view_of(&body))?)
        }
        (Method::Get, "schema/stored") => Ok(Response::text(
            200,
            context.call_fallible(|s| s.get_schema_text())?,
        )),
        (Method::Get, "schema/infer") => Ok(Response::text(
            200,
            context.call_fallible(|s| s.infer_schema_text())?,
        )),
        (Method::Post, "schema/set") => {
            let body = request.text_body()?;
            let mode = query_value(query, "mode");
            value(context.call_fallible(move |s| s.set_schema(&body, mode.as_deref()))?)
        }
        (Method::Post, "schema/patch") => {
            let body = request.text_body()?;
            Ok(Response::text(
                200,
                context.call_fallible(move |s| s.patch_schema_text(&body))?,
            ))
        }
        (Method::Post, "schema/validate") => {
            let body = request.text_body()?;
            value(context.call_fallible(move |s| s.validate_schema(&body))?)
        }
        (Method::Post, "schema/validate-patch") => {
            let body = request.text_body()?;
            value(context.call_fallible(move |s| s.validate_schema_patch(&body))?)
        }
        (Method::Post, "schema/diff") => {
            let body = request.text_body()?;
            value(context.call_fallible(move |s| s.diff_schema(&body))?)
        }
        (Method::Get, "graph-view") => {
            let max_nodes = query_number(query, "maxNodes").unwrap_or(500);
            let max_edges = query_number(query, "maxEdges").unwrap_or(1000);
            let view = context.call_fallible(move |s| {
                s.graph()
                    .graph_view(max_nodes, max_edges)
                    .map_err(|err| err.to_string())
            })?;
            value(serde_json::to_value(view).map_err(|err| HttpError(500, err.to_string()))?)
        }
        // The Graph panel with a filter and/or degree columns
        // (`graphersal_session::views::GraphViewRequest`); an invalid filter is a 400 naming
        // the label, key and operator.
        (Method::Post, "graph-view") => {
            let body = request.text_body()?;
            value(context.call_fallible(move |s| s.graph_view(&body))?)
        }
        // The Rhai query selecting what a filter shows: `{"query": "..."}`.
        (Method::Post, "graph-filter-query") => {
            let body = request.text_body()?;
            let query = context.call_fallible(move |s| s.graph_filter_query(&body))?;
            value(serde_json::json!({ "query": query }))
        }
        (Method::Post, "neighbourhood") => {
            let body = request.text_body()?;
            value(context.call_fallible(move |s| s.neighbourhood(&body))?)
        }
        (Method::Post, "sample") => {
            let body = request.text_body()?;
            value(context.call_fallible(move |s| s.label_sample(&body))?)
        }
        (Method::Get, "meta-graph") => value(context.call_fallible(|s| s.meta_graph())?),
        (Method::Get, "element") => {
            let kind = query_text(query, "kind").unwrap_or_default();
            let id = query_text(query, "id")
                .ok_or_else(|| HttpError::bad_request("expected ?kind=vertex|edge&id=<id>"))?;
            Ok(Response::text(
                200,
                context.call_fallible(move |s| s.element(&kind, &id))?,
            ))
        }
        // The Graph panel's property editor (`graphersal_session::edit`): the element in the
        // typed form, and a write of its changed and removed keys as one unit (a commit like
        // any other, recorded in the change feed).
        (Method::Get, "element/edit") => {
            let kind = query_text(query, "kind").unwrap_or_default();
            let id = query_text(query, "id")
                .ok_or_else(|| HttpError::bad_request("expected ?kind=vertex|edge&id=<id>"))?;
            value(context.call_fallible(move |s| s.element_for_edit(&kind, &id))?)
        }
        (Method::Post, "element/update") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.update_element(&body))?)
        }
        // The Graph panel's edit mode (`graphersal_session::create`): label choices, creating a
        // vertex or an edge, deleting one; each write one unit, recorded in the change feed.
        (Method::Post, "element/labels") => {
            let body = request.json_body()?;
            value(context.call_fallible(move |s| s.label_choices(&body))?)
        }
        (Method::Post, "element/create") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.create_element(&body))?)
        }
        (Method::Post, "element/delete") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.delete_element(&body))?)
        }
        (Method::Get, "completions") => value(context.call(|s| views::completions(s.engine()))?),
        (Method::Get, "token-values") => value(context.call(|s| s.token_values())?),
        (Method::Get, "memory") => value(context.call_fallible(|s| s.graph().memory_view())?),
        // The scheduled jobs of the active store (`graphersal_session::jobs`).
        (Method::Get, "jobs") => value(context.call_fallible(|s| s.jobs_view())?),
        // The dev server's user owns the machine's files (the Store menu writes backups
        // anywhere), so a backup job may name any directory.
        (Method::Post, "jobs/define") => {
            let body = request.json_body()?;
            value(context.call(move |s| {
                s.define_job_with_authorizer(
                    &body,
                    Arc::new(graphersal::auth::AccessPolicy::allow_all()),
                )
            })?)
        }
        (Method::Post, "jobs/drop") => {
            let name = body_name(&request.json_body()?)?;
            let dropped = context.call_fallible(move |s| s.drop_job(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (Method::Post, "jobs/enable") => {
            let body = request.json_body()?;
            let name = body_name(&body)?;
            let enabled = body["enabled"].as_bool().ok_or_else(|| {
                HttpError(400, "the body needs \"enabled\": true or false".to_string())
            })?;
            value(context.call_fallible(move |s| s.set_job_enabled(&name, enabled))?)
        }
        (Method::Post, "jobs/schedule") => {
            let body = request.json_body()?;
            let name = body_name(&body)?;
            let schedule = body["schedule"].as_str().map(str::to_string);
            value(context.call_fallible(move |s| s.set_job_schedule(&name, schedule.as_deref()))?)
        }
        (Method::Post, "jobs/run") => {
            let name = body_name(&request.json_body()?)?;
            // The caller waits for the run, the session does not.
            let job = name.clone();
            let (before, run) = context.call_fallible(move |s| {
                let before = position(s).ok();
                s.start_job_now(&job).map(|run| (before, run))
            })?;
            let (done, finished) = channel();
            job_workers::start(context, before, run, Some(done));
            finished
                .recv()
                .map_err(|_| HttpError(500, "the session is not running".to_string()))?;
            value(context.call_fallible(move |s| s.job_entry(&name))?)
        }
        // `?name=<job>&tier=minutes|hours|days` (default minutes): the job's run buckets.
        (Method::Get, "jobs/history") => {
            let name = query_text(query, "name").unwrap_or_default();
            let tier = query_text(query, "tier").unwrap_or_else(|| "minutes".to_string());
            value(context.call_fallible(move |s| s.job_history(&name, &tier))?)
        }
        // The workload statistics (`Session::workload_report`).
        // `?history=minutes|hours|days` adds every row's buckets of that tier (chart data).
        (Method::Get, "workload") => match query_value(query, "history") {
            None => value(context.call_fallible(|s| s.workload_report())?),
            Some(tier) => {
                value(context.call_fallible(move |s| s.workload_report_with_history(&tier))?)
            }
        },
        // The system journal (`Session::journal`): one page from the cursor `after` (or, with
        // `newest_first=true`, the newest records below `before`, newest first), filtered by
        // `level`, `since`, `until`, `source`, `kind`, `op`, `origin`, at most `limit`.
        (Method::Get, "journal") => {
            let mut spec = serde_json::Map::new();
            for key in [
                "after",
                "before",
                "newest_first",
                "level",
                "since",
                "until",
                "source",
                "kind",
                "op",
                "origin",
                "limit",
            ] {
                if let Some(text) = query_text(query, key) {
                    spec.insert(key.to_string(), Value::String(text));
                }
            }
            let spec = Value::Object(spec);
            value(context.call_fallible(move |s| s.journal(&spec))?)
        }
        // The storage advisor (`graphersal_session::advisor`): POST a spec of thresholds (`{}`:
        // the defaults); apply one recommendation by its id (`{id, spec}`), or several at once
        // in one commit (`{ids: [..], spec}`), with the same spec.
        (Method::Post, "advise") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.advise(&body))?)
        }
        (Method::Post, "advise/apply") => {
            let body = request.json_body()?;
            let spec = body.get("spec").cloned().unwrap_or(Value::Null);
            if let Some(ids) = body.get("ids") {
                let ids: Vec<String> = ids
                    .as_array()
                    .and_then(|ids| {
                        ids.iter()
                            .map(|id| id.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or_else(|| {
                        HttpError(400, "\"ids\" is a list of recommendation ids".to_string())
                    })?;
                return value(context.call(move |s| s.apply_recommendations(&ids, &spec))?);
            }
            let id = body["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| HttpError(400, "the body needs \"id\" or \"ids\"".to_string()))?;
            value(context.call(move |s| s.apply_recommendation(&id, &spec))?)
        }
        // `{id, days?}`: not recommended for `days` (without: for good); `{id}`: listed again.
        (Method::Post, "advise/dismiss") => {
            let body = request.json_body()?;
            let id = body["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| HttpError(400, "the body needs \"id\"".to_string()))?;
            let days = body["days"].as_u64();
            value(context.call(move |s| s.dismiss_recommendation(&id, days))?)
        }
        (Method::Post, "advise/restore") => {
            let body = request.json_body()?;
            let id = body["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| HttpError(400, "the body needs \"id\"".to_string()))?;
            value(context.call(move |s| s.restore_recommendation(&id))?)
        }
        // The memory report (`Session::memory_report`): POST a spec, `{}` for a full report.
        (Method::Post, "memory/report") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.memory_report(&body))?)
        }
        (Method::Get, "statistics") => value(
            context.with_process_memory(context.call_fallible(|s| s.statistics_with_process())?),
        ),
        (Method::Get, "export") if query_value(query, "format").as_deref() == Some("gsnap") => {
            let models = store::model_copy_of(query)?;
            let bytes = context.call_fallible(move |s| s.save_snapshot_with(models))?;
            Ok(download(bytes, "graph.gsnap"))
        }
        (Method::Get, "queries") => {
            let folder = query_text(query, "folder");
            value(context.call_fallible(move |s| s.list_queries(folder.as_deref()))?)
        }
        (Method::Get, "queries/get") => {
            let name = query_text(query, "name").unwrap_or_default();
            value(context.call_fallible(move |s| s.get_query(&name))?)
        }
        (Method::Post, "queries/define") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.define_query(&body))?)
        }
        (Method::Post, "queries/drop") => {
            let name = body_name(&request.json_body()?)?;
            let dropped = context.call_fallible(move |s| s.drop_query(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (Method::Post, "queries/move") => {
            let body = request.json_body()?;
            let name = body_name(&body)?;
            let folder = body
                .get("folder")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            value(context.call_fallible(move |s| s.move_query(&name, Some(&folder)))?)
        }
        (Method::Post, "queries/preview") => {
            let body = request.json_body()?;
            let name = body_name(&body)?;
            let args = body.get("args").cloned().unwrap_or(Value::Null);
            value(context.call(move |s| s.preview_query(&name, &args))?)
        }
        (Method::Post, "queries/call-text") => {
            let body = request.json_body()?;
            let name = body_name(&body)?;
            let args = body.get("args").cloned().unwrap_or(Value::Null);
            let text = graphersal_session::queries::query_call_text(&name, &args)
                .map_err(HttpError::bad_request)?;
            value(json!({ "text": text }))
        }
        // The whole catalog (saved queries, compression rules, every other kind) as one file.
        (Method::Get, "catalog/export") => Ok(Response::text(
            200,
            context.call_fallible(|s| s.save_catalog_text())?,
        )),
        (Method::Post, "catalog/load") => {
            let body = request.text_body()?;
            let replace = query_value(query, "replace").as_deref() == Some("true");
            value(context.call_fallible(move |s| s.load_catalog(&body, replace))?)
        }
        // Compression rules (`graphersal_session::compression`).
        (Method::Get, "compression") => value(context.call_fallible(|s| s.list_compressions())?),
        (Method::Post, "compression/define") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.define_compression(&body))?)
        }
        (Method::Post, "compression/estimate") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.estimate_compression(&body))?)
        }
        (Method::Post, "compression/drop") => {
            let name = body_name(&request.json_body()?)?;
            let dropped = context.call_fallible(move |s| s.drop_compression(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (Method::Post, "compression/drop-many") => {
            let body = request.json_body()?;
            let names: Vec<String> = body["names"]
                .as_array()
                .and_then(|names| {
                    names
                        .iter()
                        .map(|name| name.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| HttpError(400, "\"names\" is a list of rule names".to_string()))?;
            let dropped = context.call_fallible(move |s| s.drop_compressions(&names))?;
            value(json!({ "dropped": dropped }))
        }
        (Method::Post, "compression/recompress") => {
            let name = body_name(&request.json_body()?)?;
            value(context.call_fallible(move |s| s.recompress(&name))?)
        }
        // Property indexes (`graphersal_session::indexes`).
        (Method::Get, "indexes") => value(context.call_fallible(|s| s.list_indexes())?),
        (Method::Post, "indexes/create") => {
            let body = request.json_body()?;
            value(context.call(move |s| s.create_index(&body))?)
        }
        (Method::Post, "indexes/drop") => {
            let name = body_name(&request.json_body()?)?;
            let dropped = context.call_fallible(move |s| s.drop_index(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (_, "layout" | "layout/step" | "layout/cancel") => {
            layout::route(context, request, &method, endpoint)
        }
        (_, "store") => store::route(context, request, &method, endpoint, query),
        (_, e) if e.starts_with("store/") => {
            store::route(context, request, &method, endpoint, query)
        }
        (Method::Get, "export") => {
            let graphml = match query_value(query, "format").as_deref() {
                None | Some("graphson") => false,
                Some("graphml") => true,
                Some(other) => {
                    return Err(HttpError::bad_request(format!(
                        "unknown format {other:?}: graphson or graphml"
                    )));
                }
            };
            let mut saved = context.call_fallible(move |s| {
                if graphml {
                    s.save_graphml()
                } else {
                    s.save_graphson()
                }
            })?;
            saved["format"] = json!(if graphml { "graphml" } else { "graphson" });
            value(saved)
        }
        (Method::Post, "save") => {
            if context.read_only {
                return Err(HttpError(
                    409,
                    "the graph is served read-only: nothing is saved".into(),
                ));
            }
            let Some(path) = context.source.clone() else {
                return Err(HttpError(
                    409,
                    "the graph did not come from a file (--graph <file>): download it with Save graph"
                        .into(),
                ));
            };
            value(save_to_source(context, path)?)
        }
        (
            _,
            "info"
            | "changes"
            | "cancel"
            | "graph"
            | "execute"
            | "schema"
            | "schema/stored"
            | "schema/view"
            | "schema/infer"
            | "schema/set"
            | "schema/patch"
            | "schema/validate"
            | "schema/validate-patch"
            | "schema/diff"
            | "graph-view"
            | "element/edit"
            | "element/update"
            | "element/labels"
            | "element/create"
            | "element/delete"
            | "completions"
            | "token-values"
            | "memory"
            | "memory/report"
            | "advise"
            | "advise/apply"
            | "advise/dismiss"
            | "advise/restore"
            | "jobs"
            | "jobs/define"
            | "jobs/drop"
            | "jobs/enable"
            | "jobs/schedule"
            | "jobs/run"
            | "jobs/history"
            | "statistics"
            | "export"
            | "save"
            | "queries"
            | "queries/get"
            | "queries/define"
            | "queries/drop"
            | "queries/move"
            | "queries/call-text"
            | "catalog/export"
            | "catalog/load"
            | "compression"
            | "compression/define"
            | "compression/estimate"
            | "compression/drop"
            | "compression/drop-many"
            | "compression/recompress"
            | "indexes"
            | "indexes/create"
            | "indexes/drop",
        ) => Err(HttpError(
            405,
            format!("method {method} is not allowed on {path}"),
        )),
        // The host's own endpoints (the CLI's AI Chat `/api/ai/conversations*`), else 404.
        _ => {
            let client = CLIENT.with(|c| c.borrow().name.clone());
            match context
                .hooks
                .route(context, request, endpoint, query, &client)
            {
                Some(answer) => answer,
                None => Err(HttpError(404, format!("unknown endpoint {path}"))),
            }
        }
    }
}

/// The OpenAPI description of this HTTP API (`openapi.json` beside the crate's manifest,
/// EXPERIMENTAL), answered at `GET /openapi.json` and `GET /api/openapi.json` with this crate's
/// version in `info.version` (the file carries the same; the CLI's `server_tests.rs` checks it
/// and that every route is described).
fn openapi() -> Value {
    let mut spec: Value =
        serde_json::from_str(include_str!("../../openapi.json")).unwrap_or(Value::Null);
    if let Some(info) = spec.get_mut("info").and_then(Value::as_object_mut) {
        info.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    }
    spec
}

/// `GET /api/info`: what the UI needs to switch to this server; answered without the session.
fn info(context: &Context) -> Value {
    let mut info = info_fields(context);
    // The read-only serving mode: the queue's settings and load (only then, so the read-write
    // server's answer is unchanged).
    if let Some(queue) = context.queue() {
        info["serving"] = queue.status();
    }
    info
}

/// The fields of `GET /api/info` every server answers, plus the host's ([`crate::Hooks`]).
fn info_fields(context: &Context) -> Value {
    let mut info = json!({
        "server": SERVER_ID,
        "name": "server",
        "dev": true,
        "unstable": true,
        // The OpenAPI description of this API (EXPERIMENTAL).
        "openapi": "/openapi.json",
        "version": env!("CARGO_PKG_VERSION"),
        // The host installs the counting allocator (`ContextBuilder::memory_tracked`).
        "memoryTracked": context.memory_tracked,
        "source": context.source.as_ref().map(|p| p.display().to_string()),
        "saveable": context.source.is_some(),
        // The --schema file the graph was loaded with.
        "schemaSource": context.schema_source.as_ref().map(|p| p.display().to_string()),
        // A Store directory: the UI shows its panel (GET /api/store).
        "store": context.store.as_ref().map(|s| s.location()),
        // A damaged store opened read-only: the UI shows a banner and the damage report.
        "maintenance": context.store.as_ref().and_then(|s| s.maintenance()).map(damage_value),
        // Damage found on disk while the store is open (a backup, a checkpoint, verify): a
        // banner; read-only when the store's damage policy is `maintenance`.
        "damageFound": match context.store.as_ref() {
            None => Value::Null,
            Some(store) => found_damage_value(&**store)
                .unwrap_or_else(|err| json!({ "error": err.to_string() })),
        },
        // A backup directory opened read-only: the UI shows a banner and offers nothing that
        // writes (restore is a CLI operation).
        "backup": context.store.as_ref().and_then(|s| {
            s.backup_marker().map(|m| backup_value(&m, &s.location()))
        }),
        // Where the store lives: on disk here (the browser keeps its stores in memory).
        "storeMemory": false,
        // A read-only view of a past state of the store (POST /api/store/view): the UI shows a
        // banner with "Back to the live graph".
        "view": store::current_view(context),
        // The limits every query runs under (from the command line).
        "presets": context.presets,
        // --read-only (or a Store this process may not write): every write is refused.
        "readOnly": context.read_only,
        // The MCP endpoint (`--mcp`): `{readOnly, endpoint, client}`, or null.
        "mcp": mcp::status(context),
    });
    context.hooks.add_info(context, &mut info);
    info
}

/// `GET /api/changes`: the change feed (`{version, commitSeq, recent: [{version, by,
/// commitSeq}]}`) plus what the top bar shows live (`view`, `mcp`); answered without the
/// session, so a page polls it cheaply while a query runs.
fn changes(context: &Context) -> Value {
    let mut answer = context
        .feed
        .lock()
        .map(|feed| feed.to_value())
        .unwrap_or_else(|_| json!({}));
    answer["view"] = store::current_view(context);
    answer["mcp"] = mcp::status(context);
    // Damage found while the store is open: the banner appears without a reload.
    answer["damageFound"] = match context.store.as_ref() {
        None => Value::Null,
        Some(store) => {
            found_damage_value(&**store).unwrap_or_else(|err| json!({ "error": err.to_string() }))
        }
    };
    answer
}

/// A binary download.
fn download(bytes: Vec<u8>, file_name: &str) -> Response {
    Response::new(200, bytes)
        .with_header("Content-Type", "application/octet-stream")
        .with_header(
            "Content-Disposition",
            &format!("attachment; filename=\"{file_name}\""),
        )
        .with_header("Cache-Control", "no-store")
        .with_header("X-Content-Type-Options", "nosniff")
}

/// The `warning` of a "Save to file" that could not keep the stored schema: GraphML and GraphSON
/// do not carry it, and the server was started without `--schema FILE`.
const SCHEMA_NOT_SAVED_WARNING: &str = "the schema is not part of a GraphML/GraphSON file: \
     download it with Save schema, or start the server with --schema FILE";

/// "Save to file": writes the graph back to its `--graph` file in the file's format, and the
/// stored schema back to the `--schema` file when the server was started with one, each
/// atomically (a temporary file next to it, then a rename; both temporary files are written
/// before either is renamed). Without `--schema`, a GraphML/GraphSON save of a graph with a
/// schema answers a `warning` ([`SCHEMA_NOT_SAVED_WARNING`]); no file is written beside the
/// graph implicitly. A `.gsnap` carries its schema itself.
fn save_to_source(context: &Context, path: PathBuf) -> Result<Value, HttpError> {
    let format = if GraphSource::is_snapshot_path(&path) {
        "gsnap"
    } else if GraphSource::is_graphson_path(&path) {
        "graphson"
    } else {
        "graphml"
    };
    let (bytes, mut saved) = context.call_fallible(move |s| match format {
        // The snapshot is lossless: the schema and the saved queries travel with the data.
        // The embedding models it serves too: this file is where they are kept.
        "gsnap" => s
            .save_snapshot_noting(graphersal::persist::ModelCopy::Include)
            .map(|(bytes, not_carried)| {
                let warnings: Vec<String> = not_carried
                    .iter()
                    .map(|blob| format!("the embedding model blob {blob} is not in the file"))
                    .collect();
                (bytes, json!({ "warnings": warnings }))
            }),
        "graphson" => s.save_graphson().map(text_save),
        _ => s.save_graphml().map(text_save),
    })?;
    let schema = saved["schema"].as_str().map(str::to_string);
    let schema_file = match (&context.schema_source, format) {
        // A cleared schema is written as the empty schema (`{}` = no schema), so the file keeps
        // matching the saved graph.
        (Some(schema_path), "graphson" | "graphml") => Some((
            schema_path.clone(),
            schema.clone().unwrap_or_else(|| "{}".to_string()),
        )),
        _ => None,
    };
    let graph_bytes = bytes.len();
    let mut files = vec![(path.clone(), bytes)];
    if let Some((schema_path, text)) = &schema_file {
        let mut text = text.clone();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        files.push((schema_path.clone(), text.into_bytes()));
    }
    write_files_atomically(&files)?;
    if let Some(map) = saved.as_object_mut() {
        map.remove("schema");
    }
    if format != "gsnap" && schema_file.is_none() && schema.is_some() {
        saved["warning"] = json!(SCHEMA_NOT_SAVED_WARNING);
    }
    if !saved["warnings"].is_array() {
        saved["warnings"] = json!([]);
    }
    saved["path"] = json!(path.display().to_string());
    saved["format"] = json!(format);
    saved["bytes"] = json!(graph_bytes);
    if let Some((schema_path, _)) = &schema_file {
        saved["schemaPath"] = json!(schema_path.display().to_string());
    }
    Ok(saved)
}

/// Writes every `(path, bytes)` atomically: each to a temporary file next to it (synced), and
/// only when all are written, each renamed over its target. A failure removes the temporary
/// files that were not renamed yet.
fn write_files_atomically(files: &[(PathBuf, Vec<u8>)]) -> Result<(), HttpError> {
    let temp_of = |path: &Path| {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        path.with_file_name(format!(
            ".{file_name}.graphersal-{}.tmp",
            std::process::id()
        ))
    };
    let temps: Vec<PathBuf> = files.iter().map(|(path, _)| temp_of(path)).collect();
    let cleanup = |from: usize| {
        for temp in &temps[from..] {
            let _ = std::fs::remove_file(temp);
        }
    };
    for ((path, bytes), temp) in files.iter().zip(&temps) {
        let written = std::fs::File::create(temp).and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
        if let Err(err) = written {
            cleanup(0);
            return Err(HttpError(
                500,
                format!("cannot write '{}': {err}", path.display()),
            ));
        }
    }
    for (index, ((path, _), temp)) in files.iter().zip(&temps).enumerate() {
        if let Err(err) = std::fs::rename(temp, path) {
            cleanup(index);
            return Err(HttpError(
                500,
                format!("cannot write '{}': {err}", path.display()),
            ));
        }
    }
    Ok(())
}

/// Splits a session's text save (`{text, schema, warnings, ..}`) into the file bytes and the
/// rest of the answer.
fn text_save(mut saved: Value) -> (Vec<u8>, Value) {
    let text = saved["text"]
        .as_str()
        .unwrap_or_default()
        .as_bytes()
        .to_vec();
    if let Some(map) = saved.as_object_mut() {
        map.remove("text");
    }
    (text, saved)
}

/// The client's own id of a run (`runId` of `POST /api/execute` and `/api/cancel`): a string
/// of 1 to [`runs::MAX_ALIAS_CHARS`] characters, or absent.
fn run_id_of(body: &Value) -> Result<Option<String>, HttpError> {
    match body.get("runId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id))
            if !id.is_empty() && id.chars().count() <= runs::MAX_ALIAS_CHARS =>
        {
            Ok(Some(id.clone()))
        }
        Some(_) => Err(HttpError::bad_request(format!(
            "runId must be a string of 1 to {} characters",
            runs::MAX_ALIAS_CHARS
        ))),
    }
}

/// The `runId` of a `POST /api/cancel`: its body is optional (a Stop without one cancels all
/// the client's runs), and read only when sent as JSON.
fn cancel_run_id(request: &mut Request) -> Result<Option<String>, HttpError> {
    let json_type = request.header("Content-Type").is_some_and(|t| {
        t.trim_start()
            .to_ascii_lowercase()
            .starts_with("application/json")
    });
    if !json_type {
        return Ok(None);
    }
    let text = request.text_body()?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|err| HttpError::bad_request(format!("malformed JSON body: {err}")))?;
    if !body.is_object() {
        return Err(HttpError::bad_request("the body must be a JSON object"));
    }
    run_id_of(&body)
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
}

/// A text query parameter, percent-decoded (`+` is a space); `None` when absent or empty.
fn query_text(query: &str, key: &str) -> Option<String> {
    query_value(query, key)
        .and_then(|v| percent_decode(&v.replace('+', " ")))
        .filter(|v| !v.is_empty())
}

/// The `name` of a JSON body (a saved query's name).
fn body_name(body: &Value) -> Result<String, HttpError> {
    body.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| HttpError::bad_request("expected {\"name\": \"<saved query>\", ...}"))
}

fn query_number(query: &str, key: &str) -> Option<usize> {
    query_value(query, key).and_then(|v| v.parse().ok())
}

fn value(body: Value) -> Result<Response, HttpError> {
    Ok(Response::json(200, &body))
}

/// A static file of the UI directory. The path is percent-decoded, must not contain `..`, and
/// must canonicalize to a file inside `root`; a directory serves its `index.html`.
fn serve_static(ui: &Ui, path: &str) -> Result<Response, HttpError> {
    let not_found = || HttpError(404, format!("not found: {path}"));
    let decoded = percent_decode(path).ok_or_else(not_found)?;
    let (bytes, name) = ui
        .read(decoded.trim_start_matches('/'))
        .ok_or_else(not_found)?;
    Ok(Response::new(200, bytes)
        .with_header("Content-Type", content_type(Path::new(&name)))
        .with_header("Cache-Control", "no-store")
        .with_header("X-Content-Type-Options", "nosniff"))
}

fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = path.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("wasm") => "application/wasm",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("md") | Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_and_content_types() {
        assert_eq!(percent_decode("/a%20b.js").as_deref(), Some("/a b.js"));
        assert_eq!(percent_decode("/%2e%2e/x").as_deref(), Some("/../x"));
        assert_eq!(percent_decode("/bad%2"), None);
        assert_eq!(
            content_type(Path::new("x.mjs")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("x.unknown")),
            "application/octet-stream"
        );
    }

    #[test]
    fn query_parameters() {
        assert_eq!(
            query_number("maxNodes=30&maxEdges=40", "maxEdges"),
            Some(40)
        );
        assert_eq!(
            query_value("format=graphml", "format").as_deref(),
            Some("graphml")
        );
        assert_eq!(query_value("", "format"), None);
    }
}
