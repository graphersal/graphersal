//! `graphersal --server`: the DEV server. **Unstable, local only, temporary.**
//!
//! It serves the playground UI (embedded in the binary, a file of `--ui-dir` or `./playground`
//! winning over the embedded copy: [`ui`]) and an HTTP API on
//! `127.0.0.1:<port>` (`--host` binds another address: [`bind`]); the UI detects the server through `GET /api/info` and sends every engine
//! call here instead of to its WebAssembly worker (`playground/js/engine/http-engine.js`). The
//! API is the playground's engine boundary over HTTP: every endpoint calls one method of the
//! shared [`graphersal_session::Session`], so the answers are the structures the WebAssembly
//! build returns.
//!
//! Threads: one session thread (64 MiB stack, like the CLI's script thread) owns the
//! [`Session`] and runs the jobs one after the other; every HTTP request gets a thread of its
//! own that sends a job and waits for its answer. `POST /api/cancel`, `GET /api/info` and the
//! health probes `GET /healthz` / `/readyz` ([`health`]) never wait for the session thread, so Stop works while a query runs: it cancels the session's
//! [`CancelToken`], the query stops at its next check and its unit rolls back.
//!
//! Safety for a local endpoint without authentication: the sockets bind the loopback
//! (127.0.0.1, and `[::1]` on the same port when the system has IPv6) unless `--host` names
//! another address or the process runs in a container (then `0.0.0.0`, with a warning on
//! stderr: [`bind`]); a request whose `Host` is not this server (DNS rebinding) or whose
//! `Origin` is another site is refused (403; the rules of a network bind are in [`bind`]); a write needs `Content-Type: application/json`, which a cross-site page cannot
//! send without a CORS preflight that this server never answers; scripts cannot touch files
//! (the playground's authorizer); bodies are limited to [`MAX_BODY_BYTES`]; a static path
//! cannot leave the UI directory (canonicalized and checked).
//!
//! Limits: the limit options of the command line (`--timeout`, `--memory-limit`,
//! `--memory-headroom`, `--safe-limits`, `--max-operations`, `--max-string-size`,
//! `--max-array-size`, `--max-map-size`, `--max-traversers`, `--max-value-depth`) become the
//! session's [`SessionPresets`] ([`session_presets`]): every query of the server runs under them,
//! as every script of the one-shot runner does. Without them the server runs like the
//! playground: no Rhai limits, the engine's traversal defaults, no time budget.
//!
//! Change feed: every job that changes the commit position of the served graph (or swaps the
//! graph: a view, a rollback) is recorded with the client that sent it (`X-Graphersal-Client`
//! of a browser tab, `mcp` for the MCP endpoint, `ai` for the AI Chat); `GET /api/changes`
//! answers it without the session thread, so a tab polls it cheaply and reloads when somebody
//! else changed the graph.
//!
//! MCP (`--mcp`): `POST /mcp` is a Model Context Protocol endpoint on the same session ([`mcp`]).
//! AI Chat (`--ai FILE`): `/api/ai/conversations*`, an LLM agent loop on the same tools (`ai`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use graphersal::persist::Store;
use graphersal::{GraphSource, exec::CancelToken};
use graphersal_session::store::{backup_value, damage_value, found_damage_value};
use graphersal_session::{RunOptions, Session, SessionPresets, views};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::cli::{Cli, DEFAULT_UI_DIR, EXIT_OK, EXIT_USAGE_ERROR, GraphSpec, ScriptBudget};

#[cfg(feature = "ai")]
mod ai;
mod bind;
mod layout;
mod mcp;
mod start_banner;
mod start_stats;
mod store;
mod ui;

/// Largest request body the server reads (scripts, schemas, patches).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Stack of the session thread: the scripting engine recurses on deeply nested values.
const SESSION_STACK_BYTES: usize = 64 * 1024 * 1024;

/// The `server` value of `GET /api/info` the UI recognizes.
pub const SERVER_ID: &str = "graphersal-dev";

/// The stderr banner.
const BANNER: &str = "\
==============================================================================
 graphersal DEV SERVER - dev mode, unstable, local only, NOT for production
 No authentication. Loopback only. The graph lives in memory only: nothing
 is saved unless you click \"Save to file\" (a --graph file) or download it.
 The HTTP API is internal to the playground and changes without notice.
==============================================================================";

/// The stderr banner of a server on a Store (its durability is a row of the Store section).
const STORE_BANNER: &str = "\
==============================================================================
 graphersal DEV SERVER - dev mode, unstable, local only, NOT for production
 No authentication. Loopback only. The graph is a Store on disk.
 The HTTP API is internal to the playground and changes without notice.
==============================================================================";

/// The stderr banner of a read-only server (`--read-only`, or a Store it may not write).
const READ_ONLY_BANNER: &str = "\
==============================================================================
 graphersal DEV SERVER - dev mode, unstable, local only, NOT for production
 No authentication. Loopback only. The graph is served READ-ONLY: every
 write is refused and nothing is written to its file or store.
 The HTTP API is internal to the playground and changes without notice.
==============================================================================";

type Job = Box<dyn FnOnce(&mut Session) + Send>;

/// What every request handler shares.
struct Context {
    jobs: Mutex<Sender<Job>>,
    cancel: CancelToken,
    running: AtomicUsize,
    /// The playground UI: a directory over the embedded copy.
    ui: ui::Ui,
    /// The `--graph` file, when the graph came from one ("Save to file" writes it back).
    source: Option<PathBuf>,
    /// The `--schema` file the graph was loaded with, when one was given.
    schema_source: Option<PathBuf>,
    port: u16,
    /// Where the server listens, and which `Host`/`Origin` headers it answers.
    bind: bind::BindPlan,
    /// The session presets, as `GET /api/info` reports them.
    presets: Value,
    /// `--read-only` (or a Store opened read-only): the graph refuses every write and
    /// "Save to file" is refused.
    read_only: bool,
    /// The Store of a `--graph <dir>`: checkpoints, marks, snapshot downloads, views, forks,
    /// rollbacks, the attic ([`store`]).
    store: Option<Arc<Store>>,
    /// The read-only view of a past state the session serves instead of the store's live
    /// graph (`POST /api/store/view`), as `GET /api/info` reports it; `None`: the live graph.
    view: Mutex<Option<Value>>,
    /// The change feed (`GET /api/changes`), updated by the session jobs.
    feed: Arc<Mutex<Feed>>,
    /// The MCP endpoint (`--mcp`), or `None`.
    mcp: Option<mcp::Mcp>,
    /// The AI Chat (`--ai FILE`), or `None`.
    #[cfg(feature = "ai")]
    ai: Option<ai::Ai>,
}

/// How many recent changes the feed keeps (a poller that missed more reloads anyway).
const FEED_RECENT: usize = 32;

/// The change feed: a version that grows with every change of the served graph, the commit
/// position after it, and who made the recent ones.
#[derive(Default)]
struct Feed {
    version: u64,
    commit_seq: u64,
    /// The last [`FEED_RECENT`] changes, oldest first.
    recent: VecDeque<FeedEntry>,
}

/// One change of the feed.
struct FeedEntry {
    version: u64,
    client: Client,
    commit_seq: u64,
}

/// Who sent a session job, for the change feed: the client (`X-Graphersal-Client` of a browser
/// tab, `mcp`, `ai`) and an optional detail (the AI Chat's conversation id).
#[derive(Clone, Default)]
pub(crate) struct Client {
    name: String,
    detail: Option<String>,
}

impl Client {
    /// A client with a detail.
    pub(crate) fn with_detail(name: &str, detail: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            detail: detail.map(str::to_string),
        }
    }
}

impl Feed {
    fn record(&mut self, client: Client, commit_seq: u64) {
        self.version += 1;
        self.commit_seq = commit_seq;
        if self.recent.len() == FEED_RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(FeedEntry {
            version: self.version,
            client,
            commit_seq,
        });
    }

    fn to_value(&self) -> Value {
        json!({
            "version": self.version,
            "commitSeq": self.commit_seq,
            "recent": self.recent.iter().map(|entry| {
                let mut change = json!({
                    "version": entry.version, "by": entry.client.name,
                    "commitSeq": entry.commit_seq,
                });
                if let Some(detail) = &entry.client.detail {
                    change["detail"] = json!(detail);
                }
                change
            }).collect::<Vec<_>>(),
        })
    }
}

thread_local! {
    /// The client of the request this thread answers (every request has a thread of its own):
    /// the `X-Graphersal-Client` id of a browser tab, `mcp`, or empty; `ai` (with the
    /// conversation id as detail) on an AI Chat turn's thread.
    static CLIENT: RefCell<Client> = RefCell::new(Client::default());
}

/// Runs `run` with `client` as this thread's client (restored afterwards).
pub(crate) fn as_client<T>(client: Client, run: impl FnOnce() -> T) -> T {
    let previous = CLIENT.with(|c| c.replace(client));
    struct Restore(Option<Client>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                let _ = CLIENT.try_with(|c| *c.borrow_mut() = previous);
            }
        }
    }
    let _restore = Restore(Some(previous));
    run()
}

/// The commit position of the graph the session serves, and which graph it is (a view or a
/// rollback swaps it). Fails when the graph cannot be read.
fn position(session: &Session) -> Result<(usize, u64), graphersal::error::GraphError> {
    let graph = session.graph();
    Ok((
        Arc::as_ptr(graph) as *const () as usize,
        graph.last_commit_seq()?,
    ))
}

/// The `X-Graphersal-Client` id of a request: up to 64 letters, digits, `-` and `_`.
fn client_id(request: &Request) -> String {
    header(request, "X-Graphersal-Client")
        .map(|id| {
            id.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .take(64)
                .collect()
        })
        .unwrap_or_default()
}

/// A failed request: status and message, answered as `{"error": message}`.
#[derive(Debug)]
struct HttpError(u16, String);

impl HttpError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self(400, message.into())
    }
}

/// Runs the dev server until the process is stopped; returns the exit code of a failed start.
pub fn run(cli: &Cli) -> i32 {
    // --host (or the container default): an address that does not resolve is a usage error.
    let plan = match bind::BindPlan::new(cli.host.as_deref(), bind::in_container()) {
        Ok(plan) => plan,
        Err(message) => {
            errln!("Error: {message}");
            return EXIT_USAGE_ERROR;
        }
    };
    // --ai: the file is read and checked before anything starts (a bad file is a usage error).
    #[cfg(feature = "ai")]
    let ai = match &cli.ai {
        Some(path) => match crate::ai::config::AiConfig::load(path) {
            Ok(loaded) => {
                for warning in &loaded.warnings {
                    errln!("{warning}");
                }
                Some(ai::Ai::new(loaded.config))
            }
            Err(message) => {
                errln!("{message}");
                return EXIT_USAGE_ERROR;
            }
        },
        None => None,
    };
    let ui = match ui::Ui::resolve(cli.ui_dir.as_deref(), DEFAULT_UI_DIR) {
        Ok(ui) => ui,
        Err(message) => {
            errln!("{message}");
            return EXIT_USAGE_ERROR;
        }
    };
    let source = match &cli.graph {
        GraphSpec::File(path) => match std::fs::canonicalize(path) {
            Ok(path) => Some(path),
            Err(err) => {
                errln!("Error: cannot open the graph file '{path}': {err}");
                return EXIT_USAGE_ERROR;
            }
        },
        _ => None,
    };
    let schema_source = match &cli.schema {
        Some(schema) => match std::fs::canonicalize(&schema.path) {
            Ok(path) => Some(path),
            Err(err) => {
                errln!(
                    "Error: cannot read the schema file '{}': {err}",
                    schema.path
                );
                return EXIT_USAGE_ERROR;
            }
        },
        None => None,
    };
    // SIGINT/SIGTERM always end the server (as PID 1 of a container a process without a
    // handler ignores SIGTERM): the running query is cancelled, `/readyz` answers 503, and a
    // Store (added once it is open) is closed cleanly before the exit.
    let stop = Arc::new(crate::signals::Stop::default());
    stop.install(EXIT_OK, "Stopping: the dev server exits.");
    // The listeners bind before the graph loads, so the health endpoints answer during a long
    // Store open (`/healthz` 200, `/readyz` 503) and a taken port fails the start at once.
    let server = match Server::http((plan.ip, cli.port)) {
        Ok(server) => Arc::new(server),
        Err(err) => {
            errln!(
                "Error: cannot listen on {}: {err}\nHelp: choose another port with \
                 --port <N> (0 picks a free one){}.",
                plan.address(cli.port),
                if cli.host.is_some() {
                    " or another address with --host <ADDR>"
                } else {
                    ""
                }
            );
            return EXIT_USAGE_ERROR;
        }
    };
    let port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .unwrap_or(cli.port);
    // The same port on the IPv6 loopback, when the system has one (a browser may resolve
    // `localhost` to `::1` first); without it the server stays on IPv4 only, silently.
    let server_v6 = plan
        .also_v6_loopback
        .then(|| Server::http(("::1", port)).ok())
        .flatten();
    let serving = Arc::new(Serving {
        context: OnceLock::new(),
        stop: stop.clone(),
    });
    let has_v6 = server_v6.is_some();
    if let Some(server_v6) = server_v6 {
        let serving = serving.clone();
        let spawned = std::thread::Builder::new()
            .name("graphersal-accept-v6".to_string())
            .spawn(move || accept(&server_v6, &serving));
        if let Err(err) = spawned {
            errln!("warning: cannot start the IPv6 accept thread: {err}");
        }
    }
    let accepting = {
        let serving = serving.clone();
        let server = server.clone();
        std::thread::Builder::new()
            .name("graphersal-accept".to_string())
            .spawn(move || accept(&server, &serving))
            .ok()
    };
    // The load time printed at start: Store open (snapshot load + WAL replay), file import
    // (with --schema) or sample build, up to the session holding the graph.
    let load_started = std::time::Instant::now();
    // A Store directory is opened here (its lock held for the life of the server); the open
    // registers the embedding models the store keeps and its schema names, so the presets
    // are built after it.
    // The models the open loads (and the ones the schema names that nobody provides) are
    // listed in the start banner's own section, not as `note:` lines before it.
    crate::cli::defer_model_notes();
    // With --schema the graph is loaded here, like the one-shot runner does (schema first,
    // then the data as one unit), so a violation stops the start with exit code 1; with
    // --read-only too (the graph gets the hook that refuses writes), and a Store opened
    // read-only (--read-only, or a store this process may not write) is a loaded graph
    // without a Store.
    let opens = matches!(cli.graph, GraphSpec::Store(_)) || cli.schema.is_some() || cli.read_only;
    let (store, preloaded) = if opens {
        match cli.graph.open(
            cli.create_store,
            cli.on_damage,
            cli.schema.as_ref(),
            cli.read_only,
        ) {
            Ok(opened) => match opened.store {
                Some(store) => (Some(store), None),
                None => (None, Some((opened.graph, graph_title(&cli.graph)))),
            },
            Err(err) => {
                errln!("{}", err.message);
                return err.code;
            }
        }
    } else {
        (None, None)
    };
    // The whole server refuses writes: --read-only, or a Store opened read-only.
    let read_only = cli.read_only || (matches!(cli.graph, GraphSpec::Store(_)) && store.is_none());
    let presets = session_presets(cli);
    let presets_json = presets.to_value();
    let shared = store
        .as_ref()
        .map(|store| (store.clone(), store.location()));
    let (jobs, cancel, commit_seq, statistics) =
        match start_session(&cli.graph, source.as_deref(), presets, shared, preloaded) {
            Ok(started) => started,
            Err(message) => {
                errln!("{message}");
                return EXIT_USAGE_ERROR;
            }
        };
    let load_time = load_started.elapsed();
    // Ctrl+C / SIGTERM: the running query is cancelled and the store closed cleanly.
    if let Some(store) = &store {
        stop.set_store(store.clone());
    }
    stop.set_cancel(cancel.clone());
    let banner = if store.is_some() {
        STORE_BANNER
    } else if read_only {
        READ_ONLY_BANNER
    } else {
        BANNER
    };
    errln!(
        "{}",
        match plan.exposure {
            bind::Exposure::Loopback => banner.to_string(),
            bind::Exposure::Network => banner.replacen(
                " No authentication. Loopback only. ",
                " REACHABLE FROM THE NETWORK, see the warning below.\n No authentication. ",
                1
            ),
        }
    );
    if let Some(notice) = plan.network_notice(port) {
        if read_only {
            errln!(
                "{}",
                notice.replacen("read and change the graph", "read the graph", 1)
            );
        } else {
            errln!("{notice}");
        }
    }
    let replayed = store
        .as_ref()
        .map(|store| store.open_report().commits_replayed);
    let loaded = start_stats::loaded(load_time, replayed);
    let mut store_section = start_banner::Section::new("Store");
    let mut graph_section = start_banner::Section::new("Graph");
    if let Some(store) = &store {
        let location = store.location();
        store_section.row("path", location.clone());
        let snapshots = store.snapshots().map(|s| s.len()).unwrap_or(0);
        store_section.row(
            "commit",
            format!(
                "{}, {snapshots} snapshot{}",
                store.open_report().commit_seq,
                if snapshots == 1 { "" } else { "s" }
            ),
        );
        let state = if let Some(marker) = store.backup_marker() {
            format!(
                "a BACKUP (up to commit {}): READ-ONLY, writes are refused; make it the live \
                 store with `graphersal store restore {location}` (after stopping the server)",
                marker.commit_seq
            )
        } else if store.maintenance().is_some() {
            format!(
                "MAINTENANCE MODE (damage found; read-only): the UI shows the damage report; \
                 repair with `graphersal store repair {location} --to <new_dir>`"
            )
        } else {
            "durable: every commit is in the write-ahead log before the query answers; \
             Ctrl+C stops the server and closes the store cleanly"
                .to_string()
        };
        store_section.row("state", state);
        store_section.row("loaded", loaded);
    } else {
        graph_section.row("source", graph_title(&cli.graph));
        graph_section.row("loaded", loaded);
    }
    if read_only {
        graph_section.row(
            "access",
            if matches!(cli.graph, GraphSpec::Store(_)) {
                "READ-ONLY: every write is refused; the store was loaded without its lock and \
                 nothing in it is written (no Store panel, no history)"
            } else {
                "READ-ONLY: every write is refused"
            },
        );
    }
    graph_section.rows(start_stats::rows(
        &statistics,
        graphersal::alloc_tracking::global_live_bytes(),
    ));
    let mut models_section = start_banner::Section::new("Embedding models");
    let (models, notes) = crate::cli::served_models();
    for model in models {
        // The registry name queries call it by, when it is not the repository itself.
        let name = if model.name == model.label {
            String::new()
        } else {
            format!("{} = ", model.name)
        };
        models_section.row(
            "model",
            format!(
                "{name}{}, licence {}\n{}",
                model.label,
                crate::cli::license_text(&model.license),
                model.source
            ),
        );
    }
    for note in notes {
        models_section.row("missing", note);
    }
    let mcp = cli.mcp.then(|| {
        mcp::Mcp::new(
            // A read-only graph offers the agent no writing tool either.
            cli.mcp_read_only || read_only,
            cli.mcp_token.clone(),
            cli.mcp_query_tools,
        )
        .with_operator_notes(cli.mcp_instructions.clone())
        .with_max_chars(cli.mcp_max_chars)
    });
    let mut server_section = start_banner::Section::new("Server");
    server_section.row("limits", presets_summary(&presets_json));
    server_section.unwrapped("UI", ui.banner());
    if let Some(mcp) = &mcp {
        let (endpoint, connect) = mcp.banner(&plan.url_host(), port);
        server_section.row("MCP", endpoint);
        server_section.unwrapped("connect", connect);
    }
    #[cfg(feature = "ai")]
    if let Some(ai) = &ai {
        server_section.row("AI Chat", ai.banner());
    }
    server_section.row(
        "listening",
        if has_v6 {
            format!("http://127.0.0.1:{port}/ and http://[::1]:{port}/ (IPv6 loopback)")
        } else if plan.is_wildcard() {
            format!(
                "{} (all interfaces: {})",
                plan.url(port),
                plan.address(port)
            )
        } else {
            plan.url(port)
        },
    );
    errln!(
        "\n{}\n",
        start_banner::render(&[store_section, graph_section, models_section, server_section])
    );
    let url = plan.url(port);
    let context = Arc::new(Context {
        jobs: Mutex::new(jobs),
        cancel,
        running: AtomicUsize::new(0),
        ui,
        source,
        schema_source,
        port,
        bind: plan,
        presets: presets_json,
        read_only,
        store,
        view: Mutex::new(None),
        feed: Arc::new(Mutex::new(Feed {
            commit_seq,
            ..Feed::default()
        })),
        mcp,
        #[cfg(feature = "ai")]
        ai,
    });
    start_job_ticker(&context);
    // From here on the requests reach the API (before: health endpoints and 503 answers).
    let _ = serving.context.set(context);
    // The address is the only line on stdout (tests and scripts read it), printed once the
    // API serves. `outln!` flushes (so `docker logs` shows it at once); a closed stdout loses
    // the line and the server keeps serving.
    outln!("Graphersal dev server: {url}");
    match accepting {
        Some(accepting) => {
            let _ = accepting.join();
        }
        // No accept thread could be started: accept here (no early answers then).
        None => accept(&server, &serving),
    }
    EXIT_OK
}

/// How often the dev server ticks the scheduled jobs of the active store.
const JOB_TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// Ticks the session's scheduled jobs (book page "Scheduled Jobs") from a thread of its own,
/// in the instance's local time zone; each tick is a session job like a browser call.
fn start_job_ticker(context: &Arc<Context>) {
    if let Some(zone) = crate::cli::local_time_zone() {
        let _ = context.call(move |session| session.set_time_zone(&zone));
    }
    let context = context.clone();
    let spawned = std::thread::Builder::new()
        .name("graphersal-jobs".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(JOB_TICK);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_micros() as i64);
                if context.call(move |session| session.tick_jobs(now)).is_err() {
                    // The session thread is gone: the server is stopping.
                    break;
                }
            }
        });
    if let Err(err) = spawned {
        errln!("warning: cannot start the scheduled-jobs thread: {err}");
    }
}

/// What the accept loops share: the API's [`Context`] once the graph is loaded, and the
/// signal state (readiness turns 503 at the first SIGINT/SIGTERM).
struct Serving {
    context: OnceLock<Arc<Context>>,
    stop: Arc<crate::signals::Stop>,
}

/// Accepts the requests of `server`, each on a thread of its own; every listener (IPv4 and
/// IPv6 loopback) feeds the same handler and session queue.
fn accept(server: &Server, serving: &Arc<Serving>) {
    for request in server.incoming_requests() {
        let serving = serving.clone();
        let spawned = std::thread::Builder::new()
            .name("graphersal-http".to_string())
            .spawn(move || handle(&serving, request));
        if let Err(err) = spawned {
            errln!("warning: cannot start a request thread: {err}");
        }
    }
}

/// The session presets of `--server`: the limit options the command line gave. Only what was
/// given is set, so a server started without them runs like the playground (no Rhai limits, the
/// engine's traversal defaults, no time budget). `--safe-limits` and the `--max-*` script limits
/// set the engine's Rhai limits exactly as for a one-shot script; `--timeout` is the default
/// `evaluationTimeout` of every traversal and the budget of a run as a whole.
pub fn session_presets(cli: &Cli) -> SessionPresets {
    let mut budget = ScriptBudget::new(cli.timeout_ms)
        .with_max_value_depth(cli.max_value_depth)
        .with_memory_limit(cli.memory_limit, cli.memory_headroom);
    if cli.max_traversers.is_some() || cli.safe_limits {
        budget = budget.with_max_traversers(cli.traversal_max_traversers());
    }
    if cli.max_string_size.is_some() || cli.safe_limits {
        budget = budget.with_max_string_bytes(cli.traversal_max_string_bytes());
    }
    let mut presets = SessionPresets::new()
        .with_limits(cli.script_limits())
        .with_script_timeout_ms(cli.timeout_ms)
        .with_embedders(crate::cli::embedders());
    for (key, value) in budget.traversal_options() {
        presets = presets.with_option(key, value);
    }
    presets
}

/// The `limits` row of the start banner: which limits the server applies.
fn presets_summary(presets: &Value) -> String {
    let mut parts = Vec::new();
    for section in ["options", "scriptLimits"] {
        if let Some(map) = presets[section].as_object() {
            parts.extend(map.iter().map(|(key, value)| format!("{key}={value}")));
        }
    }
    if parts.is_empty() {
        "none (start with --timeout, --memory-limit, --safe-limits, --max-* to bound queries)"
            .to_string()
    } else {
        format!("of every query: {}", parts.join(", "))
    }
}

/// Starts the session thread with the graph of `spec` loaded and `presets` applied (or the
/// Store `shared`, or the graph `preloaded` with its title); returns its job queue, its
/// cancellation token, the commit position of the loaded graph and its statistics
/// ([`Session::statistics`], printed at start), or the diagnostic of a graph that could not be
/// loaded.
fn start_session(
    spec: &GraphSpec,
    source: Option<&Path>,
    presets: SessionPresets,
    shared: Option<(Arc<Store>, String)>,
    preloaded: Option<(Arc<graphersal::prelude::Graph>, String)>,
) -> Result<(Sender<Job>, CancelToken, u64, Value), String> {
    let (jobs, queue) = channel::<Job>();
    let (ready, started) = channel::<Result<(CancelToken, u64, Value), String>>();
    let spec = spec.clone();
    let source = source.map(Path::to_path_buf);
    std::thread::Builder::new()
        .name("graphersal-session".to_string())
        .stack_size(SESSION_STACK_BYTES)
        .spawn(move || {
            // Work outside a request (loading at start) is technical; `Context::call` sets
            // each request's channel.
            graphersal::workload::set_thread_default(
                graphersal::workload::Channel::Playground,
                true,
            );
            let mut session = Session::with_presets(presets);
            let loaded = match (shared, preloaded) {
                (Some((store, name)), _) => session
                    .attach_store(store, &name)
                    .map(|_| ())
                    .map_err(|err| err.to_string()),
                (None, Some((graph, name))) => {
                    let kind = match &spec {
                        GraphSpec::File(_) => "file",
                        GraphSpec::Modern => "modern",
                        GraphSpec::Empty => "empty",
                        GraphSpec::Large => "large",
                        GraphSpec::FileTree => "tree",
                        GraphSpec::Store(_) => "store",
                    };
                    session
                        .load_shared(graph, &name, kind)
                        .map(|_| ())
                        .map_err(|err| err.to_string())
                }
                (None, None) => load_graph(&mut session, &spec, source.as_deref()),
            };
            if let Err(err) = loaded {
                let _ = ready.send(Err(err));
                return;
            }
            let started = position(&session)
                .map_err(|err| err.to_string())
                .and_then(|(_, seq)| {
                    let statistics = session.statistics().map_err(|err| err.to_string())?;
                    Ok((session.cancel_token(), seq, statistics))
                });
            let failed = started.is_err();
            let _ = ready.send(started);
            if failed {
                return;
            }
            for job in queue {
                // A panic is a bug; the session (its graph rolled back by the unit) serves on.
                if std::panic::catch_unwind(AssertUnwindSafe(|| job(&mut session))).is_err() {
                    errln!("warning: a request panicked in the session (a bug, please report)");
                }
            }
        })
        .map_err(|err| format!("Error: cannot start the session thread: {err}"))?;
    started
        .recv()
        .map_err(|_| "Error: the session thread stopped while loading the graph".to_string())?
        .map(|(cancel, commit_seq, statistics)| (jobs, cancel, commit_seq, statistics))
}

/// The title the session shows for the graph of `spec`: the file name of a file, the sample's
/// title (`Session::sample_graphs`) of a sample.
fn graph_title(spec: &GraphSpec) -> String {
    let kind = match spec {
        GraphSpec::File(path) | GraphSpec::Store(path) => {
            return Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
        }
        GraphSpec::Modern => "modern",
        GraphSpec::Empty => "empty",
        GraphSpec::Large => "large",
        GraphSpec::FileTree => "tree",
    };
    Session::sample_graphs()
        .as_array()
        .into_iter()
        .flatten()
        .find(|sample| sample["kind"] == kind)
        .and_then(|sample| sample["title"].as_str())
        .unwrap_or(kind)
        .to_string()
}

/// Loads the `--graph` of the server into `session`.
fn load_graph(
    session: &mut Session,
    spec: &GraphSpec,
    source: Option<&Path>,
) -> Result<(), String> {
    let info = match (spec, source) {
        (GraphSpec::File(_), Some(path)) => {
            let bytes = std::fs::read(path)
                .map_err(|err| format!("Error: cannot read '{}': {err}", path.display()))?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if GraphSource::is_snapshot_path(path) {
                session.load_snapshot(&bytes, &name)?
            } else if GraphSource::is_graphson_path(path) {
                session.load_graphson(&bytes, &name, None)?
            } else {
                session.load_graphml(&bytes, &name, None)?
            }
        }
        (GraphSpec::Modern, _) => session.load_sample("modern")?,
        (GraphSpec::Empty, _) => session.load_sample("empty")?,
        (GraphSpec::Large, _) => session.load_sample("large")?,
        (GraphSpec::FileTree, _) => session.load_sample("tree")?,
        (GraphSpec::File(path), None) => return Err(format!("Error: cannot open '{path}'")),
        (GraphSpec::Store(path), _) => {
            return Err(format!("Error: the store '{path}' was not opened"));
        }
    };
    if let Some(warnings) = info["warnings"].as_array() {
        for warning in warnings {
            errln!("note: {}", warning.as_str().unwrap_or_default());
        }
    }
    Ok(())
}

impl Context {
    /// Runs `job` on the session thread and waits for its answer. A job that changes the
    /// served graph's commit position (or swaps the graph) is recorded in the change feed with
    /// the client of this request.
    fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Session) -> T + Send + 'static,
    ) -> Result<T, HttpError> {
        let (reply, answer) = channel();
        let feed = self.feed.clone();
        let client = CLIENT.with(|c| c.borrow().clone());
        // The workload statistics count only the queries a client runs (`Session::execute*`)
        // under its channel; the page's drawing and panels are technical.
        let channel = match client.name.as_str() {
            "mcp" | "ai" => graphersal::workload::Channel::Mcp,
            _ => graphersal::workload::Channel::Playground,
        };
        let job: Job = Box::new(move |session| {
            let before = position(session);
            let out = graphersal::workload::scope(channel, true, || job(session));
            let after = position(session);
            // The change feed is a notification for the other clients: a position that cannot
            // be read before or after counts as a change (they reload and see for themselves).
            let changed = match (&before, &after) {
                (Ok(before), Ok(after)) => before != after,
                _ => true,
            };
            if changed && let Ok(mut feed) = feed.lock() {
                let seq = match (&after, &before) {
                    (Ok(after), _) | (Err(_), Ok(after)) => after.1,
                    (Err(_), Err(_)) => 0,
                };
                feed.record(client, seq);
            }
            let _ = reply.send(out);
        });
        let sent = match self.jobs.lock() {
            Ok(jobs) => jobs.send(job).is_ok(),
            Err(_) => false,
        };
        if !sent {
            return Err(HttpError(500, "the session is not running".to_string()));
        }
        answer
            .recv()
            .map_err(|_| HttpError(500, "the request failed inside the session".to_string()))
    }

    /// A session method that fails with a diagnostic: the failure is a 422 answer.
    fn call_fallible<T: Send + 'static, E: std::fmt::Display + Send + 'static>(
        &self,
        job: impl FnOnce(&mut Session) -> Result<T, E> + Send + 'static,
    ) -> Result<T, HttpError> {
        self.call(job)?
            .map_err(|message| HttpError(422, message.to_string()))
    }

    /// The hosts a request to this server may name (`Host`, and the origin of `Origin`).
    fn is_own_host(&self, host: &str) -> bool {
        self.bind.is_own_host(host, self.port)
    }
}

fn header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

/// Largest declared body (`Content-Length`) a request may name. `tiny_http` 0.12 skips an
/// unread body when the request is dropped (after the answer) by allocating ONE zeroed buffer
/// of the remaining declared length (`EqualReader::drop`), so a request that declares, say,
/// 2^62 bytes would abort the whole process (an allocation failure is not a panic). Such a
/// request is never dropped: it gets no answer and its connection is leaked (no client of this
/// server sends one; the bodies it reads are at most [`MAX_BODY_BYTES`], and a declared body
/// up to this size still gets its `413`).
const MAX_DECLARED_BODY_BYTES: usize = 4 * MAX_BODY_BYTES;

fn handle(serving: &Serving, mut request: Request) {
    if request
        .body_length()
        .is_some_and(|len| len > MAX_DECLARED_BODY_BYTES)
    {
        errln!(
            "warning: a request declared a body of {} bytes; it is ignored (no answer)",
            request.body_length().unwrap_or_default()
        );
        std::mem::forget(request);
        return;
    }
    // The probes: no Host/Origin check, no lock, no session (see `health`).
    if let Some(response) = health(serving, &request) {
        let _ = request.respond(response);
        return;
    }
    let Some(context) = serving.context.get() else {
        let _ = request.respond(json_response(
            503,
            &json!({ "error": "the server is still loading its graph; GET /readyz answers \
                200 when it serves queries" }),
        ));
        return;
    };
    if serving.stop.stopping() {
        let _ = request.respond(json_response(
            503,
            &json!({ "error": "the server is stopping" }),
        ));
        return;
    }
    let client = if request.url().split('?').next() == Some("/mcp") {
        "mcp".to_string()
    } else {
        client_id(&request)
    };
    CLIENT.with(|c| *c.borrow_mut() = Client::with_detail(&client, None));
    let response = match route(context, &mut request) {
        Ok(response) => response,
        Err(HttpError(status, message)) => json_response(status, &json!({ "error": message })),
    };
    let _ = request.respond(response);
}

type Answer = Response<std::io::Cursor<Vec<u8>>>;

/// `GET`/`HEAD /healthz` (liveness: 200 while the process answers HTTP) and `/readyz`
/// (readiness: 200 when the graph is loaded and no stop signal arrived, else 503), or `None`
/// for every other path. Answered before the Host/Origin checks, so a Kubernetes probe
/// (`Host: <pod IP>:<port>`) and a Docker `HEALTHCHECK` work in every bind mode: they read
/// nothing of the graph, take no lock and never wait for the session thread (a long query
/// cannot fail liveness), and have no side effect.
fn health(serving: &Serving, request: &Request) -> Option<Answer> {
    let path = request.url().split('?').next().unwrap_or_default();
    if path != "/healthz" && path != "/readyz" {
        return None;
    }
    let method = request.method();
    if !matches!(method, Method::Get | Method::Head) {
        return Some(json_response(
            405,
            &json!({ "error": format!("method {method} is not allowed on {path}") }),
        ));
    }
    // Probes come every few seconds: never above trace.
    tracing::trace!(target: "graphersal::server", "{method} {path}");
    if path == "/healthz" {
        return Some(json_response(200, &json!({ "status": "ok" })));
    }
    let (status, state) = if serving.stop.stopping() {
        (503, "stopping")
    } else if serving.context.get().is_none() {
        (503, "loading")
    } else {
        (200, "ready")
    };
    Some(json_response(status, &json!({ "status": state })))
}

fn route(context: &Arc<Context>, request: &mut Request) -> Result<Answer, HttpError> {
    // DNS rebinding and cross-site requests: only this server's own address is served.
    let host = match header(request, "Host") {
        Some(host) if context.is_own_host(host) => host,
        _ => {
            return Err(HttpError(
                403,
                match context.bind.exposure {
                    bind::Exposure::Loopback => {
                        format!("unknown Host: use {}", context.bind.url(context.port))
                    }
                    bind::Exposure::Network => "unknown Host: open this server by its IP \
                        address, localhost or a name without a dot, or start it with \
                        --host <that name>"
                        .to_string(),
                },
            ));
        }
    };
    if let Some(origin) = header(request, "Origin")
        && !context.bind.is_own_origin(origin, host, context.port)
    {
        return Err(HttpError(
            403,
            format!("cross-origin request from {origin} refused"),
        ));
    }
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let method = request.method().clone();
    if path == "/mcp" {
        return mcp::route(context, request, &method);
    }
    if path == "/openapi.json" || path == "/api/openapi.json" {
        return match method {
            Method::Get | Method::Head => Ok(json_response(200, &openapi())),
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
        (Method::Get, "info") => Ok(json_response(200, &info(context))),
        (Method::Get, "changes") => Ok(json_response(200, &changes(context))),
        (Method::Post, "cancel") => {
            let running = context.running.load(Ordering::SeqCst) > 0;
            if running {
                context.cancel.cancel();
            }
            Ok(json_response(200, &json!({ "cancelled": running })))
        }
        (Method::Get, "graph") => value(context.call_fallible(|s| s.info())?),
        (Method::Post, "execute") => {
            let body = json_body(request)?;
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
            context.running.fetch_add(1, Ordering::SeqCst);
            let answer = context.call(move |s| s.execute(&script, &options));
            context.running.fetch_sub(1, Ordering::SeqCst);
            Ok(text_response(200, answer?))
        }
        (Method::Get, "schema") => value(context.call_fallible(|s| s.graph().schema_view())?),
        (Method::Post, "schema/view") => {
            let body = text_body(request)?;
            value(context.call_fallible(move |s| s.schema_view_of(&body))?)
        }
        (Method::Get, "schema/stored") => Ok(text_response(
            200,
            context.call_fallible(|s| s.get_schema_text())?,
        )),
        (Method::Get, "schema/infer") => Ok(text_response(
            200,
            context.call_fallible(|s| s.infer_schema_text())?,
        )),
        (Method::Post, "schema/set") => {
            let body = text_body(request)?;
            let mode = query_value(query, "mode");
            value(context.call_fallible(move |s| s.set_schema(&body, mode.as_deref()))?)
        }
        (Method::Post, "schema/patch") => {
            let body = text_body(request)?;
            Ok(text_response(
                200,
                context.call_fallible(move |s| s.patch_schema_text(&body))?,
            ))
        }
        (Method::Post, "schema/validate") => {
            let body = text_body(request)?;
            value(context.call_fallible(move |s| s.validate_schema(&body))?)
        }
        (Method::Post, "schema/validate-patch") => {
            let body = text_body(request)?;
            value(context.call_fallible(move |s| s.validate_schema_patch(&body))?)
        }
        (Method::Post, "schema/diff") => {
            let body = text_body(request)?;
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
            let body = text_body(request)?;
            value(context.call_fallible(move |s| s.graph_view(&body))?)
        }
        // The Rhai query selecting what a filter shows: `{"query": "..."}`.
        (Method::Post, "graph-filter-query") => {
            let body = text_body(request)?;
            let query = context.call_fallible(move |s| s.graph_filter_query(&body))?;
            value(serde_json::json!({ "query": query }))
        }
        (Method::Post, "neighbourhood") => {
            let body = text_body(request)?;
            value(context.call_fallible(move |s| s.neighbourhood(&body))?)
        }
        (Method::Post, "sample") => {
            let body = text_body(request)?;
            value(context.call_fallible(move |s| s.label_sample(&body))?)
        }
        (Method::Get, "meta-graph") => value(context.call_fallible(|s| s.meta_graph())?),
        (Method::Get, "element") => {
            let kind = query_text(query, "kind").unwrap_or_default();
            let id = query_text(query, "id")
                .ok_or_else(|| HttpError::bad_request("expected ?kind=vertex|edge&id=<id>"))?;
            Ok(text_response(
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
            let body = json_body(request)?;
            value(context.call(move |s| s.update_element(&body))?)
        }
        // The Graph panel's edit mode (`graphersal_session::create`): label choices, creating a
        // vertex or an edge, deleting one; each write one unit, recorded in the change feed.
        (Method::Post, "element/labels") => {
            let body = json_body(request)?;
            value(context.call_fallible(move |s| s.label_choices(&body))?)
        }
        (Method::Post, "element/create") => {
            let body = json_body(request)?;
            value(context.call(move |s| s.create_element(&body))?)
        }
        (Method::Post, "element/delete") => {
            let body = json_body(request)?;
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
            let body = json_body(request)?;
            value(context.call(move |s| {
                s.define_job_with_authorizer(
                    &body,
                    Arc::new(graphersal::auth::AccessPolicy::allow_all()),
                )
            })?)
        }
        (Method::Post, "jobs/drop") => {
            let name = body_name(&json_body(request)?)?;
            let dropped = context.call_fallible(move |s| s.drop_job(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (Method::Post, "jobs/enable") => {
            let body = json_body(request)?;
            let name = body_name(&body)?;
            let enabled = body["enabled"].as_bool().ok_or_else(|| {
                HttpError(400, "the body needs \"enabled\": true or false".to_string())
            })?;
            value(context.call_fallible(move |s| s.set_job_enabled(&name, enabled))?)
        }
        (Method::Post, "jobs/schedule") => {
            let body = json_body(request)?;
            let name = body_name(&body)?;
            let schedule = body["schedule"].as_str().map(str::to_string);
            value(context.call_fallible(move |s| s.set_job_schedule(&name, schedule.as_deref()))?)
        }
        (Method::Post, "jobs/run") => {
            let name = body_name(&json_body(request)?)?;
            value(context.call_fallible(move |s| s.run_job_now(&name))?)
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
        // The storage advisor (`graphersal_session::advisor`): POST a spec of thresholds (`{}`:
        // the defaults); apply one recommendation by its id with the same spec.
        (Method::Post, "advise") => {
            let body = json_body(request)?;
            value(context.call(move |s| s.advise(&body))?)
        }
        (Method::Post, "advise/apply") => {
            let body = json_body(request)?;
            let id = body["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| HttpError(400, "the body needs \"id\"".to_string()))?;
            let spec = body.get("spec").cloned().unwrap_or(Value::Null);
            value(context.call(move |s| s.apply_recommendation(&id, &spec))?)
        }
        // `{id, days?}`: not recommended for `days` (without: for good); `{id}`: listed again.
        (Method::Post, "advise/dismiss") => {
            let body = json_body(request)?;
            let id = body["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| HttpError(400, "the body needs \"id\"".to_string()))?;
            let days = body["days"].as_u64();
            value(context.call(move |s| s.dismiss_recommendation(&id, days))?)
        }
        (Method::Post, "advise/restore") => {
            let body = json_body(request)?;
            let id = body["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| HttpError(400, "the body needs \"id\"".to_string()))?;
            value(context.call(move |s| s.restore_recommendation(&id))?)
        }
        // The memory report (`Session::memory_report`): POST a spec, `{}` for a full report.
        (Method::Post, "memory/report") => {
            let body = json_body(request)?;
            value(context.call(move |s| s.memory_report(&body))?)
        }
        (Method::Get, "statistics") => value(with_process_memory(
            context,
            context.call_fallible(|s| s.statistics())?,
        )),
        (Method::Get, "export") if query_value(query, "format").as_deref() == Some("gsnap") => {
            let bytes = context.call_fallible(|s| s.save_snapshot())?;
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
            let body = json_body(request)?;
            value(context.call(move |s| s.define_query(&body))?)
        }
        (Method::Post, "queries/drop") => {
            let name = body_name(&json_body(request)?)?;
            let dropped = context.call_fallible(move |s| s.drop_query(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (Method::Post, "queries/move") => {
            let body = json_body(request)?;
            let name = body_name(&body)?;
            let folder = body
                .get("folder")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            value(context.call_fallible(move |s| s.move_query(&name, Some(&folder)))?)
        }
        (Method::Post, "queries/preview") => {
            let body = json_body(request)?;
            let name = body_name(&body)?;
            let args = body.get("args").cloned().unwrap_or(Value::Null);
            value(context.call(move |s| s.preview_query(&name, &args))?)
        }
        (Method::Post, "queries/call-text") => {
            let body = json_body(request)?;
            let name = body_name(&body)?;
            let args = body.get("args").cloned().unwrap_or(Value::Null);
            let text = graphersal_session::queries::query_call_text(&name, &args)
                .map_err(HttpError::bad_request)?;
            value(json!({ "text": text }))
        }
        // The whole catalog (saved queries, compression rules, every other kind) as one file.
        (Method::Get, "catalog/export") => Ok(text_response(
            200,
            context.call_fallible(|s| s.save_catalog_text())?,
        )),
        (Method::Post, "catalog/load") => {
            let body = text_body(request)?;
            let replace = query_value(query, "replace").as_deref() == Some("true");
            value(context.call_fallible(move |s| s.load_catalog(&body, replace))?)
        }
        // Compression rules (`graphersal_session::compression`).
        (Method::Get, "compression") => value(context.call_fallible(|s| s.list_compressions())?),
        (Method::Post, "compression/define") => {
            let body = json_body(request)?;
            value(context.call(move |s| s.define_compression(&body))?)
        }
        (Method::Post, "compression/estimate") => {
            let body = json_body(request)?;
            value(context.call(move |s| s.estimate_compression(&body))?)
        }
        (Method::Post, "compression/drop") => {
            let name = body_name(&json_body(request)?)?;
            let dropped = context.call_fallible(move |s| s.drop_compression(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (Method::Post, "compression/recompress") => {
            let name = body_name(&json_body(request)?)?;
            value(context.call_fallible(move |s| s.recompress(&name))?)
        }
        // Property indexes (`graphersal_session::indexes`).
        (Method::Get, "indexes") => value(context.call_fallible(|s| s.list_indexes())?),
        (Method::Post, "indexes/create") => {
            let body = json_body(request)?;
            value(context.call(move |s| s.create_index(&body))?)
        }
        (Method::Post, "indexes/drop") => {
            let name = body_name(&json_body(request)?)?;
            let dropped = context.call_fallible(move |s| s.drop_index(&name))?;
            value(json!({ "dropped": dropped }))
        }
        (_, "layout" | "layout/step" | "layout/cancel") => {
            layout::route(context, request, &method, endpoint)
        }
        // The AI Chat (`--ai`): conversations, their turns and events.
        (_, e) if e == "ai/conversations" || e.starts_with("ai/conversations/") => {
            ai_route(context, request, &method, endpoint, query)
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
            | "compression/recompress"
            | "indexes"
            | "indexes/create"
            | "indexes/drop",
        ) => Err(HttpError(
            405,
            format!("method {method} is not allowed on {path}"),
        )),
        _ => Err(HttpError(404, format!("unknown endpoint {path}"))),
    }
}

/// The OpenAPI description of this HTTP API (`openapi.json` beside the crate's manifest,
/// EXPERIMENTAL), answered at `GET /openapi.json` and `GET /api/openapi.json` with this binary's
/// version in `info.version` (the file carries the same; `server_tests.rs` checks it and that
/// every route is described).
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
    json!({
        "server": SERVER_ID,
        "name": "server",
        "dev": true,
        "unstable": true,
        // The OpenAPI description of this API (EXPERIMENTAL).
        "openapi": "/openapi.json",
        "version": env!("CARGO_PKG_VERSION"),
        // The binary installs the counting allocator (main.rs) and queries run on one thread.
        "memoryTracked": true,
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
        // The AI Chat (`--ai`): `{provider, model, readOnly, sendsDataTo, maxToolCallsPerTurn}`
        // (never the key), or null.
        "ai": ai_status(context),
        // The embedders queries can call (the built-in hash-ngram/1 and every --embedder):
        // `[{name, model, dimension, default}]`.
        "embedders": embedders_value(),
    })
}

/// The registry of [`crate::cli::embedders`] for `GET /api/info`.
fn embedders_value() -> Value {
    let registry = crate::cli::embedders();
    let default = registry.default_embedder().info().name.clone();
    registry
        .infos()
        .map(|info| {
            json!({
                "name": info.name.as_str(),
                "model": info.model.as_str(),
                "dimension": info.dimension,
                "default": info.name == default,
            })
        })
        .collect()
}

/// [`ai::status`], or null in a build without the `ai` feature.
fn ai_status(context: &Context) -> Value {
    #[cfg(feature = "ai")]
    return ai::status(context);
    #[cfg(not(feature = "ai"))]
    {
        let _ = context;
        Value::Null
    }
}

/// The session's statistics (`Session::statistics`) with the dev server's own part of the
/// process memory added: `process.memory` (the whole process, [`graphersal_session::process`]) and `process.aiChat {conversations, historyBytes}` when the AI Chat is on
/// ([`ai::process_memory`]). Served by `GET /api/statistics` and the MCP `statistics` tool.
fn with_process_memory(context: &Context, mut statistics: Value) -> Value {
    graphersal_session::process::add_to_statistics(&mut statistics);
    #[cfg(feature = "ai")]
    if let Some(chat) = ai::process_memory(context)
        && let Some(process) = statistics.get_mut("process").and_then(Value::as_object_mut)
    {
        process.insert("aiChat".to_string(), chat);
    }
    #[cfg(not(feature = "ai"))]
    let _ = context;
    statistics
}

/// `/api/ai/conversations*` ([`ai::route`]); `404` in a build without the `ai` feature.
fn ai_route(
    context: &Arc<Context>,
    request: &mut Request,
    method: &Method,
    endpoint: &str,
    query: &str,
) -> Result<Answer, HttpError> {
    #[cfg(feature = "ai")]
    {
        let client = CLIENT.with(|c| c.borrow().name.clone());
        ai::route(context, request, method, endpoint, query, &client)
    }
    #[cfg(not(feature = "ai"))]
    {
        let _ = (context, request, method, endpoint, query);
        Err(HttpError(
            404,
            "the AI Chat is not available: this graphersal was built without the `ai` feature"
                .into(),
        ))
    }
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
fn download(bytes: Vec<u8>, file_name: &str) -> Answer {
    let response = Response::from_data(bytes).with_status_code(200);
    let response = with_header(response, "Content-Type", "application/octet-stream");
    let response = with_header(
        response,
        "Content-Disposition",
        &format!("attachment; filename=\"{file_name}\""),
    );
    let response = with_header(response, "Cache-Control", "no-store");
    with_header(response, "X-Content-Type-Options", "nosniff")
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
        "gsnap" => s.save_snapshot().map(|bytes| (bytes, json!({}))),
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

/// Reads the body (at most [`MAX_BODY_BYTES`]) of a `Content-Type: application/json` request.
fn text_body(request: &mut Request) -> Result<String, HttpError> {
    let json_type = header(request, "Content-Type")
        .map(|t| {
            t.trim_start()
                .to_ascii_lowercase()
                .starts_with("application/json")
        })
        .unwrap_or(false);
    if !json_type {
        return Err(HttpError(
            415,
            "send the body as Content-Type: application/json".into(),
        ));
    }
    if request
        .body_length()
        .is_some_and(|len| len > MAX_BODY_BYTES)
    {
        return Err(HttpError(
            413,
            format!("the body is larger than {MAX_BODY_BYTES} bytes"),
        ));
    }
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| HttpError::bad_request(format!("cannot read the body: {err}")))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(HttpError(
            413,
            format!("the body is larger than {MAX_BODY_BYTES} bytes"),
        ));
    }
    String::from_utf8(bytes).map_err(|_| HttpError::bad_request("the body is not UTF-8"))
}

/// [`text_body`] parsed as a JSON object.
fn json_body(request: &mut Request) -> Result<Value, HttpError> {
    let text = text_body(request)?;
    let body: Value = serde_json::from_str(&text)
        .map_err(|err| HttpError::bad_request(format!("malformed JSON body: {err}")))?;
    if !body.is_object() {
        return Err(HttpError::bad_request("the body must be a JSON object"));
    }
    Ok(body)
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

fn with_header(response: Answer, name: &str, value: &str) -> Answer {
    match Header::from_bytes(name.as_bytes(), value.as_bytes()) {
        Ok(header) => response.with_header(header),
        Err(()) => response,
    }
}

fn text_response(status: u16, text: String) -> Answer {
    let response = Response::from_data(text.into_bytes()).with_status_code(status);
    let response = with_header(response, "Content-Type", "application/json; charset=utf-8");
    let response = with_header(response, "Cache-Control", "no-store");
    with_header(response, "X-Content-Type-Options", "nosniff")
}

fn json_response(status: u16, body: &Value) -> Answer {
    text_response(status, body.to_string())
}

fn value(body: Value) -> Result<Answer, HttpError> {
    Ok(json_response(200, &body))
}

/// A static file of the UI directory. The path is percent-decoded, must not contain `..`, and
/// must canonicalize to a file inside `root`; a directory serves its `index.html`.
fn serve_static(ui: &ui::Ui, path: &str) -> Result<Answer, HttpError> {
    let not_found = || HttpError(404, format!("not found: {path}"));
    let decoded = percent_decode(path).ok_or_else(not_found)?;
    let (bytes, name) = ui
        .read(decoded.trim_start_matches('/'))
        .ok_or_else(not_found)?;
    let response = Response::from_data(bytes).with_status_code(200);
    let response = with_header(response, "Content-Type", content_type(Path::new(&name)));
    let response = with_header(response, "Cache-Control", "no-store");
    Ok(with_header(response, "X-Content-Type-Options", "nosniff"))
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
