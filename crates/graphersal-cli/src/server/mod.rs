//! `graphersal --server`: the DEV server. **Unstable, local only, temporary.**
//!
//! The hosting core is the crate `graphersal_serve`: the session thread and its job queue, the
//! run registry, the playground HTTP API (one endpoint per [`graphersal_session::Session`]
//! method, so the answers are the structures the WebAssembly build returns), the `Host`/`Origin`
//! checks, the embedded playground UI (a file of `--ui-dir` or `./playground` winning over the
//! embedded copy) and the MCP endpoint's host. This module is its host over `tiny_http`: it
//! parses nothing itself (`Cli` did), decides where to listen ([`bind`]), loads the graph,
//! prints the start banner, runs the accept loops, answers the health probes `GET /healthz` /
//! `/readyz` ([`health`]) and the 503s while loading or stopping, converts each request to a
//! `graphersal_serve::Request` and its answer back, and handles SIGINT/SIGTERM.
//!
//! Threads: one session thread (64 MiB stack, like the CLI's script thread) owns the
//! [`Session`] and runs the jobs one after the other; every HTTP request gets a thread of its
//! own that sends a job and waits for its answer. `POST /api/cancel`, `GET /api/info` and the
//! health probes never wait for the session thread, so Stop works while a query runs.
//!
//! Safety for a local endpoint without authentication: the sockets bind the loopback
//! (127.0.0.1, and `[::1]` on the same port when the system has IPv6) unless `--host` names
//! another address or the process runs in a container (then `0.0.0.0`, with a warning on
//! stderr: [`bind`]); the header checks, the JSON content type of writes and the body limit are
//! `graphersal_serve`'s. A request that declares a body beyond [`MAX_DECLARED_BODY_BYTES`] is
//! never answered (see there).
//!
//! Limits: the limit options of the command line (`--timeout`, `--safe-limits`,
//! `--max-operations`, `--max-string-size`, `--max-array-size`, `--max-map-size`,
//! `--max-traversers`, `--max-value-depth`) become the session's [`SessionPresets`]
//! ([`session_presets`]): every query of the server runs under them, as every script of the
//! one-shot runner does. Without them the server runs like the playground: no Rhai limits, the
//! engine's traversal defaults, no time budget. `--memory-limit` and `--memory-headroom` are not
//! presets: they set the process memory ceiling (on by default) before the server starts, so it
//! covers every session, import and load of this process.
//!
//! The read-only serving mode (`--server --read-only`) queues the session jobs on the reader
//! pool ([`pool`], a `graphersal_serve::JobQueue`). MCP (`--mcp`): `POST /mcp` is a Model
//! Context Protocol endpoint on the same session ([`mcp`]). AI Chat (`--ai FILE`):
//! `/api/ai/conversations*`, an LLM agent loop on the same tools (`ai`), mounted through
//! `graphersal_serve::Hooks` with the CLI's other additions to `GET /api/info` and the
//! statistics (the embedding models it lists).

use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};

use graphersal::GraphSource;
use graphersal::exec::CancelToken;
use graphersal::persist::Store;
use graphersal_serve::{
    Busy, BusyReason, Context, Hooks, HttpError, Job, JobQueue, Runner, SESSION_STACK_BYTES,
};
use graphersal_session::{Session, SessionPresets};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Server};

use crate::cli::{Cli, DEFAULT_UI_DIR, EXIT_OK, EXIT_USAGE_ERROR, GraphSpec, ScriptBudget};

#[cfg(feature = "ai")]
mod ai;
mod bind;
mod cost;
mod mcp;
mod pool;
mod start_stats;

pub use cost::run as mcp_cost;

/// The answer of an endpoint (`graphersal_serve`'s response).
type Answer = graphersal_serve::Response;

/// An answer as `tiny_http` writes it.
type TinyAnswer = tiny_http::Response<Cursor<Vec<u8>>>;

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

/// Runs the dev server until the process is stopped; returns the exit code of a failed start.
pub fn run(cli: &Cli) -> i32 {
    // `--server --read-only`: the read-only serving mode, a pool of workers under safe default
    // limits (each overridable by its flag).
    let serving_defaults;
    let cli = if cli.read_only_serving() {
        serving_defaults = cli.with_serving_defaults();
        &serving_defaults
    } else {
        cli
    };
    let pool = cli
        .read_only_serving()
        .then(|| Arc::new(pool::Pool::new(pool_config(cli))));
    // The stateless MCP session ids of the serving mode: signed with the operator's key (the
    // same on every replica) or a random key of this process. The key is never printed.
    let session_key = match (&pool, std::env::var_os(SESSION_KEY_VAR)) {
        (None, _) => None,
        (Some(_), None) => Some(graphersal_mcp::SessionKey::random()),
        (Some(_), Some(raw)) => match graphersal_mcp::SessionKey::new(raw.as_encoded_bytes()) {
            Ok(key) => Some(key),
            Err(err) => {
                errln!(
                    "Error: {SESSION_KEY_VAR}: {err}\nHelp: set it to a random secret of at \
                     least {} characters, the same on every replica (e.g. openssl rand -hex 32), \
                     or leave it unset for a random key of this process.",
                    graphersal_mcp::SessionKey::MIN_BYTES
                );
                return EXIT_USAGE_ERROR;
            }
        },
    };
    // --host (or the container default): an address that does not resolve is a usage error.
    // --allowed-host and GRAPHERSAL_ALLOWED_HOSTS: the public names behind a reverse proxy.
    let env_hosts = std::env::var(bind::ALLOWED_HOSTS_VAR).ok();
    let plan =
        match bind::BindPlan::new(cli.host.as_deref(), bind::in_container()).and_then(|plan| {
            bind::allowed_hosts(&cli.allowed_hosts, env_hosts.as_deref())
                .map(|names| plan.with_allowed_hosts(names))
        }) {
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
                Some(Arc::new(ai::Ai::new(loaded.config)))
            }
            Err(message) => {
                errln!("{message}");
                return EXIT_USAGE_ERROR;
            }
        },
        None => None,
    };
    let ui = match graphersal_serve::Ui::resolve(cli.ui_dir.as_deref(), DEFAULT_UI_DIR) {
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
        max_connections: cli.max_connections.or(pool
            .is_some()
            .then_some(crate::cli::DEFAULT_MAX_CONNECTIONS)),
        connections: AtomicUsize::new(0),
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
    // A Store directory is opened here (its lock held for the life of the server); a `.gsnap`
    // is loaded here too, so the graph serves the embedding models the file carries.
    // The models (and the notes about them) are listed in the start banner's own section,
    // not as `note:` lines before it.
    crate::cli::defer_model_notes();
    // With --schema the graph is loaded here, like the one-shot runner does (schema first,
    // then the data as one unit), so a violation stops the start with exit code 1; with
    // --read-only too (the graph gets the hook that refuses writes), and a Store opened
    // read-only (--read-only, or a store this process may not write) is a loaded graph
    // without a Store.
    let opens = matches!(cli.graph, GraphSpec::Store(_))
        || matches!(&cli.graph, GraphSpec::File(path) if graphersal::prelude::GraphSource::is_snapshot_path(path))
        || cli.schema.is_some()
        || cli.read_only;
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
    let (jobs, cancel, commit_seq, statistics) = match start_session(
        &cli.graph,
        source.as_deref(),
        presets,
        shared,
        preloaded,
        pool.clone(),
    ) {
        Ok(started) => started,
        Err(message) => {
            errln!("{message}");
            return EXIT_USAGE_ERROR;
        }
    };
    let load_time = load_started.elapsed();
    // Ctrl+C / SIGTERM: the running queries are cancelled and the store closed cleanly; in the
    // read-only serving mode the running queries get the drain timeout first.
    if let Some(store) = &store {
        stop.set_store(store.clone());
    }
    let runs = Arc::new(graphersal_serve::Runs::default());
    match &pool {
        None => {
            stop.set_cancel(cancel.clone());
            let runs = runs.clone();
            stop.set_on_stop(Arc::new(move || {
                runs.cancel_all();
            }));
        }
        Some(pool) => {
            let pool = pool.clone();
            let runs = runs.clone();
            let answering = serving.clone();
            let timeout = std::time::Duration::from_secs(
                cli.drain_timeout_s
                    .unwrap_or(crate::cli::DEFAULT_DRAIN_TIMEOUT_S),
            );
            stop.set_drain(Arc::new(move || {
                errln!(
                    "Stopping: no new queries; the running ones get {} s to finish.",
                    timeout.as_secs()
                );
                let drained = pool.drain(timeout, || runs.cancel_all());
                if drained.cancelled > 0 {
                    errln!(
                        "{} running queries were cancelled after the drain timeout.",
                        drained.cancelled
                    );
                }
                if drained.still_running > 0 {
                    errln!(
                        "warning: {} queries did not stop within 2 s of the cancel; exiting \
                         under them.",
                        drained.still_running
                    );
                }
                // The answers of the jobs that ended are still being written: give the request
                // threads a moment (new requests are answered 503 at once meanwhile).
                let grace = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while answering.connections.load(Ordering::SeqCst) > 0
                    && std::time::Instant::now() < grace
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }));
        }
    }
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
    let mut store_section = crate::sections::Section::new("Store");
    let mut graph_section = crate::sections::Section::new("Graph");
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
        if let Some(marker) = store.backup_marker() {
            store_section.note(
                "state",
                &crate::out::Note::new(format!(
                    "a BACKUP (up to commit {}): READ-ONLY, writes are refused",
                    marker.commit_seq
                ))
                .command(
                    "make it the live store (after stopping the server)",
                    format!("graphersal store restore {location}"),
                )
                .text(),
            );
        } else if store.maintenance().is_some() {
            store_section.note(
                "state",
                &crate::out::Note::new(
                    "MAINTENANCE MODE (damage found; read-only): the UI shows the damage report",
                )
                .command(
                    "build a repaired copy in a new directory",
                    format!("graphersal store repair {location} --to <new_dir>"),
                )
                .text(),
            );
        } else {
            store_section.row(
                "state",
                "durable: every commit is in the write-ahead log before the query answers; \
                 Ctrl+C stops the server and closes the store cleanly",
            );
        }
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
    let mut models_section = crate::sections::Section::new("Embedding models");
    let served = crate::cli::served_models();
    for model in &served.models {
        let state = match &model.state {
            graphersal::persist::ModelState::Loaded => "loaded".to_string(),
            graphersal::persist::ModelState::NotLoaded => "loaded at its first use".to_string(),
            graphersal::persist::ModelState::Missing => {
                "not served: its blob is missing".to_string()
            }
            graphersal::persist::ModelState::Damaged { reason }
            | graphersal::persist::ModelState::Unsupported { reason } => {
                format!("not served: {reason}")
            }
            _ => model.state.name().to_string(),
        };
        models_section.row(
            "model",
            format!(
                "{}{}, licence {}\n{}; {}\n{}",
                model.repo,
                if model.default { " (the default)" } else { "" },
                crate::cli::license_text(&model.license),
                served.source,
                state,
                model.identity
            ),
        );
    }
    for note in &served.notes {
        models_section.note("note", note);
    }
    // A read-only graph offers the agent no writing tool either.
    let mcp_read_only = cli.mcp_read_only || read_only;
    if let Some(config) = cli.mcp.then_some(cli.mcp_config.as_ref()).flatten()
        && let Err(err) = config.check(mcp_read_only)
    {
        // A Store that turned out read-only at open (no write permission).
        errln!("Error: --mcp-config: {err}");
        return EXIT_USAGE_ERROR;
    }
    let mcp = cli.mcp.then(|| {
        let mcp = match &cli.mcp_config {
            // The configuration carries the shorthand flags already (`Cli::parse`).
            Some(config) => mcp::Mcp::new(mcp_read_only, cli.mcp_token.clone(), false)
                .with_config(config.clone()),
            None => mcp::Mcp::new(mcp_read_only, cli.mcp_token.clone(), cli.mcp_query_tools)
                .with_operator_notes(cli.mcp_instructions.clone())
                .with_max_chars(cli.mcp_max_chars),
        };
        match session_key.clone() {
            Some(key) => mcp.with_session_key(key),
            None => mcp,
        }
    });
    let mut server_section = crate::sections::Section::new("Server");
    server_section.row("limits", presets_summary(&presets_json));
    if let Some(pool) = &pool {
        let config = pool.config();
        server_section.row(
            "serving",
            format!(
                "read-only serving mode: {} workers in parallel, at most {} queries waiting \
                 (up to {} s), {} per client, {} requests at once; SIGTERM drains for {} s",
                config.workers,
                config.queue,
                config.queue_timeout.as_secs(),
                config.max_inflight_per_client,
                serving.max_connections.unwrap_or_default(),
                cli.drain_timeout_s
                    .unwrap_or(crate::cli::DEFAULT_DRAIN_TIMEOUT_S)
            ),
        );
    }
    server_section.unwrapped("UI", ui.banner());
    if let Some(mcp) = &mcp {
        let (endpoint, connect) = mcp::banner(mcp, &plan.url_host(), port);
        server_section.row("MCP", endpoint);
        server_section.unwrapped("connect", connect);
    }
    #[cfg(feature = "ai")]
    if let Some(ai) = &ai {
        server_section.row("AI Chat", ai.banner());
    }
    if !plan.allowed().is_empty() {
        server_section.row("allowed hosts", plan.allowed().join(", "));
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
        crate::sections::render(&[store_section, graph_section, models_section, server_section])
    );
    let url = plan.url(port);
    let runner = match &pool {
        Some(pool) => Runner::Queue(pool.clone()),
        None => Runner::Session(jobs),
    };
    let hooks = Arc::new(CliHooks {
        #[cfg(feature = "ai")]
        ai: ai.clone(),
    });
    let context = Context::builder(runner, plan.host_check(port))
        .runs(runs)
        .ui(ui)
        .source(source)
        .schema_source(schema_source)
        .presets(presets_json)
        .read_only(read_only)
        .store(store)
        .commit_seq(commit_seq)
        .mcp(mcp)
        .hooks(hooks)
        .build();
    // The dev server's own parts of every memory report (MCP sessions, AI Chat
    // conversations), held weakly by the engine: registered for as long as this function runs.
    let server_memory: Arc<dyn graphersal::storage::MemorySource> = Arc::new(ServerMemory {
        context: Arc::downgrade(&context),
        #[cfg(feature = "ai")]
        ai,
    });
    graphersal::storage::register_memory_source(&server_memory);
    // No scheduled jobs in the read-only serving mode: they belong to a Store, which it does
    // not open.
    if pool.is_none()
        && let Err(err) =
            graphersal_serve::start_job_ticker(&context, crate::cli::local_time_zone())
    {
        errln!("warning: cannot start the scheduled-jobs thread: {err}");
    }
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

/// The dev server's parts of a memory report (book page "Memory Usage"): `mcp` (the client
/// sessions of `--mcp`) and `ai_chat` (the conversations of `--ai`), both estimated. Read
/// without waiting on a lock a request may hold.
struct ServerMemory {
    context: std::sync::Weak<Context>,
    #[cfg(feature = "ai")]
    ai: Option<Arc<ai::Ai>>,
}

impl graphersal::storage::MemorySource for ServerMemory {
    fn memory_components(&self, components: &mut graphersal::storage::MemoryComponents) {
        use graphersal::storage::MemoryComponent;
        let Some(context) = self.context.upgrade() else {
            return;
        };
        if let Some(mcp) = context.mcp() {
            let (sessions, bytes) = mcp.memory_bytes();
            components.push(
                MemoryComponent::estimated("mcp", bytes)
                    .with_detail(format!("{sessions} client session(s)")),
            );
        }
        #[cfg(feature = "ai")]
        if let Some(ai) = &self.ai {
            let (conversations, bytes) = ai.memory_bytes();
            components.push(
                MemoryComponent::estimated("ai_chat", bytes)
                    .with_detail(format!("{conversations} conversation(s)")),
            );
        }
    }
}

/// What the dev server adds to `graphersal_serve`'s API: the AI Chat (its endpoints, `ai` of
/// `GET /api/info`, `process.aiChat` of the statistics) and the embedding models the start found
/// (`embedders` and `modelsNote` of `GET /api/info`, `process.models` without an open Store).
struct CliHooks {
    #[cfg(feature = "ai")]
    ai: Option<Arc<ai::Ai>>,
}

impl Hooks for CliHooks {
    fn add_info(&self, context: &Context, info: &mut Value) {
        // The AI Chat (`--ai`): `{provider, model, readOnly, sendsDataTo, maxToolCallsPerTurn}`
        // (never the key), or null.
        info["ai"] = self.ai_status();
        // The embedders queries can call: the built-in hash-ngram/1 and the models of the
        // served database (`[{name, model, dimension, default, license, state}]`), with the
        // note when it holds models but none is the default (`modelsNote`).
        info["embedders"] = embedders_value(context);
        info["modelsNote"] = json!(
            models_of(context)
                .and_then(|(models, place)| crate::cli::no_default_note(&models, &place))
        );
    }

    /// `process.aiChat {conversations, historyBytes}` when the AI Chat is on
    /// ([`ai::process_memory`]), and without an open Store the database's loaded models in
    /// `process.models` ([`served_model_values`]).
    fn add_process_statistics(&self, context: &Context, statistics: &mut Value) {
        // Without an open Store (`--read-only`, a `.gsnap`) the session knows no store models,
        // but the graph serves the database's models: list the ones the memory bar counts.
        if context.store().is_none()
            && statistics["process"]["models"]
                .as_array()
                .is_some_and(Vec::is_empty)
        {
            let models = served_model_values(statistics);
            if !models.is_empty()
                && let Some(process) = statistics.get_mut("process").and_then(Value::as_object_mut)
            {
                process.insert("models".to_string(), Value::Array(models));
            }
        }
        #[cfg(feature = "ai")]
        if let Some(chat) = self.ai.as_deref().and_then(ai::process_memory)
            && let Some(process) = statistics.get_mut("process").and_then(Value::as_object_mut)
        {
            process.insert("aiChat".to_string(), chat);
        }
    }

    /// `/api/ai/conversations*` ([`ai::route`]); `404` in a build without the `ai` feature.
    fn route(
        &self,
        context: &Arc<Context>,
        request: &mut graphersal_serve::Request<'_>,
        endpoint: &str,
        query: &str,
        client: &str,
    ) -> Option<Result<Answer, HttpError>> {
        if endpoint != "ai/conversations" && !endpoint.starts_with("ai/conversations/") {
            return None;
        }
        #[cfg(feature = "ai")]
        {
            let method = request.method().clone();
            Some(ai::route(
                context,
                self.ai.as_ref(),
                request,
                &method,
                endpoint,
                query,
                client,
            ))
        }
        #[cfg(not(feature = "ai"))]
        {
            let _ = (context, request, query, client);
            Some(Err(HttpError::new(
                404,
                "the AI Chat is not available: this graphersal was built without the `ai` feature",
            )))
        }
    }
}

impl CliHooks {
    /// [`ai::status`], or null without `--ai` or in a build without the `ai` feature.
    fn ai_status(&self) -> Value {
        #[cfg(feature = "ai")]
        if let Some(ai) = &self.ai {
            return ai::status(ai);
        }
        Value::Null
    }
}

/// The embedding models of the served database (the open Store's current ones, else those the
/// start found: a read-only store, a `.gsnap`), with where they are kept.
fn models_of(context: &Context) -> Option<(Vec<graphersal::persist::ModelInfo>, String)> {
    match context.store() {
        Some(store) => store.models().ok().map(|models| (models, store.location())),
        None => {
            let served = crate::cli::served_models();
            let place = served
                .source
                .split_once(' ')
                .map(|(_, rest)| rest.trim_start_matches("in ").trim_start_matches("by "))
                .unwrap_or_default()
                .to_string();
            (!served.models.is_empty()).then_some((served.models, place))
        }
    }
}

/// The embedders queries can call, for `GET /api/info`: the built-in `hash-ngram/1` (the
/// default unless the database names one) and the database's models by their repository.
fn embedders_value(context: &Context) -> Value {
    let models = models_of(context)
        .map(|(models, _)| models)
        .unwrap_or_default();
    let any_default = models.iter().any(|m| m.default);
    let mut out = vec![json!({
        "name": "hash-ngram/1",
        "model": "hash-ngram/1",
        "dimension": 256,
        "default": !any_default,
        "license": "built-in",
        "state": "loaded",
    })];
    out.extend(models.iter().map(|model| {
        json!({
            "name": model.repo,
            "model": model.identity,
            "dimension": model.dimension,
            "default": model.default,
            "license": crate::cli::license_text(&model.license),
            "state": model.state.name(),
        })
    }));
    Value::Array(out)
}

/// The `process.models` entries (`{name, model, dimension, bytes, measured, default}`, as
/// `Session::process_memory` writes them for a Store) of the models the start found
/// ([`crate::cli::served_models`]: a read-only store, a `.gsnap`) that the memory bar counts as
/// loaded (an `embedding_model` part of its `models` segment, named by the model's identity).
fn served_model_values(statistics: &Value) -> Vec<Value> {
    let parts: Vec<&Value> = statistics["process"]["memory"]["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|segment| segment["name"] == "models")
        .flat_map(|segment| segment["parts"].as_array().into_iter().flatten())
        .filter(|part| part["name"] == "embedding_model")
        .collect();
    if parts.is_empty() {
        return Vec::new();
    }
    crate::cli::served_models()
        .models
        .iter()
        .filter_map(|model| {
            let part = parts
                .iter()
                .find(|part| part["detail"].as_str() == Some(model.identity.as_str()))?;
            Some(json!({
                "name": model.repo,
                "model": model.identity,
                "dimension": model.dimension,
                "bytes": part["bytes"],
                "measured": true,
                "default": model.default,
            }))
        })
        .collect()
}

/// The reader pool of the read-only serving mode queues the session jobs of
/// `graphersal_serve`'s context.
impl JobQueue for pool::Pool {
    fn submit(&self, key: &str, job: Job) -> Result<(), Busy> {
        self.run(key, job).map_err(|refused| {
            let reason = match refused {
                pool::Refused::QueueFull => BusyReason::QueueFull,
                pool::Refused::Expired => BusyReason::QueueTimeout,
                pool::Refused::ClientBusy => BusyReason::ClientLimit,
                pool::Refused::Stopping => BusyReason::Stopping,
            };
            Busy::new(reason, refused.message(self.config()))
        })
    }

    fn status(&self) -> Value {
        pool::Pool::status(self)
    }
}

/// What the accept loops share: the API's [`Context`] once the graph is loaded, and the
/// signal state (readiness turns 503 at the first SIGINT/SIGTERM).
struct Serving {
    context: OnceLock<Arc<Context>>,
    stop: Arc<crate::signals::Stop>,
    /// `--max-connections` (the read-only serving mode's default without it): the requests
    /// handled at once; `None`: no cap.
    max_connections: Option<usize>,
    /// The requests being handled.
    connections: AtomicUsize,
}

/// Accepts the requests of `server`, each on a thread of its own; every listener (IPv4 and
/// IPv6 loopback) feeds the same handler and session queue.
///
/// With a connection cap (`--max-connections`), a request beyond it is answered 503 with
/// `Retry-After: 1` here, on the accept thread, without a thread of its own; the health probes
/// are answered here too then (they never wait).
fn accept(server: &Server, serving: &Arc<Serving>) {
    for request in server.incoming_requests() {
        if let Some(max) = serving.max_connections
            && serving.connections.load(Ordering::SeqCst) >= max
        {
            if request.url().starts_with("/mcp")
                && let Some(mcp) = serving.context.get().and_then(|c| c.mcp())
            {
                mcp.stats()
                    .record_refusal(graphersal_mcp::Refusal::TooManyConnections);
            }
            let response = health(serving, &request).unwrap_or_else(|| {
                Answer::json(
                    503,
                    &json!({ "error": format!(
                        "the server is busy: it handles {max} requests at once \
                         (--max-connections); retry in a moment"
                    ) }),
                )
                .with_header("Retry-After", "1")
            });
            let _ = request.respond(tiny_answer(response));
            continue;
        }
        serving.connections.fetch_add(1, Ordering::SeqCst);
        let serving = serving.clone();
        let counted = serving.clone();
        let spawned = std::thread::Builder::new()
            .name("graphersal-http".to_string())
            .spawn(move || {
                let _done = Counted(serving.clone());
                handle(&serving, request)
            });
        if let Err(err) = spawned {
            counted.connections.fetch_sub(1, Ordering::SeqCst);
            errln!("warning: cannot start a request thread: {err}");
        }
    }
}

/// A request being handled: counted down however its handler ends.
struct Counted(Arc<Serving>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The pool settings of the read-only serving mode from the command line and the defaults.
fn pool_config(cli: &Cli) -> pool::PoolConfig {
    let workers = cli.workers.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4)
    });
    pool::PoolConfig {
        workers,
        queue: cli.queue.unwrap_or(workers.saturating_mul(4)),
        queue_timeout: std::time::Duration::from_secs(
            cli.queue_timeout_s
                .unwrap_or(crate::cli::DEFAULT_QUEUE_TIMEOUT_S),
        ),
        max_inflight_per_client: cli
            .max_inflight_per_client
            .unwrap_or(crate::cli::DEFAULT_MAX_INFLIGHT_PER_CLIENT),
    }
}

/// The environment variable of the key that signs the stateless MCP session ids of the
/// read-only serving mode (the same on every replica behind a load balancer).
const SESSION_KEY_VAR: &str = "GRAPHERSAL_MCP_SESSION_KEY";

/// The session presets of `--server`: the limit options the command line gave. Only what was
/// given is set, so a server started without them runs like the playground (no Rhai limits, the
/// engine's traversal defaults, no time budget). `--safe-limits` and the `--max-*` script limits
/// set the engine's Rhai limits exactly as for a one-shot script; `--timeout` is the default
/// `evaluationTimeout` of every traversal and the budget of a run as a whole.
pub fn session_presets(cli: &Cli) -> SessionPresets {
    // The memory ceiling is not a preset: `--memory-limit` set it for the whole process
    // (`Cli::apply_memory_limit`, before the server started).
    let mut budget = ScriptBudget::new(cli.timeout_ms).with_max_value_depth(cli.max_value_depth);
    if cli.max_traversers.is_some() || cli.safe_limits {
        budget = budget.with_max_traversers(cli.traversal_max_traversers());
    }
    budget = budget.with_max_materialized_bytes(cli.traversal_max_materialized_bytes());
    if cli.max_string_size.is_some() || cli.safe_limits {
        budget = budget.with_max_string_bytes(cli.traversal_max_string_bytes());
    }
    let mut presets = SessionPresets::new()
        .with_limits(cli.script_limits())
        .with_script_timeout_ms(cli.timeout_ms);
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
        "none (start with --timeout, --safe-limits, --max-* to bound queries; the process memory \
         ceiling applies, --memory-limit)"
            .to_string()
    } else {
        format!("of every query: {}", parts.join(", "))
    }
}

/// Starts the session thread with the graph of `spec` loaded and `presets` applied (or the
/// Store `shared`, or the graph `preloaded` with its title); returns its job queue, its
/// cancellation token, the commit position of the loaded graph and its statistics
/// ([`Session::statistics`] with `process.memory` added, printed at start), or the diagnostic
/// of a graph that could not be loaded.
///
/// With a `pool` (the read-only serving mode) the thread loads the graph, then serves as the
/// pool's first worker; the other workers start here, each with a session of its own over the
/// same graph ([`Session::template`]).
fn start_session(
    spec: &GraphSpec,
    source: Option<&Path>,
    presets: SessionPresets,
    shared: Option<(Arc<Store>, String)>,
    preloaded: Option<(Arc<graphersal::prelude::Graph>, String)>,
    pool: Option<Arc<pool::Pool>>,
) -> Result<(Sender<Job>, CancelToken, u64, Value), String> {
    type Started = (
        CancelToken,
        u64,
        Value,
        Option<graphersal_session::SessionTemplate>,
    );
    let spec = spec.clone();
    let source = source.map(Path::to_path_buf);
    let first_worker = pool.clone();
    let setup = move |session: &mut Session| -> Result<Started, String> {
        match (shared, preloaded) {
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
            (None, None) => load_graph(session, &spec, source.as_deref()),
        }?;
        let seq = session
            .graph()
            .last_commit_seq()
            .map_err(|err| err.to_string())?;
        // The banner's memory row shows the process against its ceiling.
        let statistics = session
            .statistics_with_process()
            .map_err(|err| err.to_string())?;
        // The pool's other workers serve the same graph; the force-layout jobs are shared by
        // every page.
        let template = match &first_worker {
            None => None,
            Some(pool) => {
                // The graph refuses every write (--read-only): runs need no whole-run unit,
                // which would let one run at a time.
                session
                    .set_read_only_runs(true)
                    .map_err(|err| err.to_string())?;
                let template = session.template().map_err(|err| err.to_string())?;
                session
                    .set_layout_job_limit(SHARED_LAYOUT_JOBS.max(pool.config().workers))
                    .map_err(|err| err.to_string())?;
                Some(template)
            }
        };
        Ok((session.cancel_token(), seq, statistics, template))
    };
    // The pool's first worker is the session thread itself, once the graph is loaded.
    let takeover = pool
        .clone()
        .map(|pool| Box::new(move |session: &mut Session| pool.work(session)) as Job);
    let (jobs, (cancel, commit_seq, statistics, template)) =
        graphersal_serve::spawn_session(presets, setup, takeover).map_err(|err| err.to_string())?;
    if let (Some(pool), Some(template)) = (pool, template) {
        for worker in 1..pool.config().workers {
            let pool = pool.clone();
            let template = template.clone();
            std::thread::Builder::new()
                .name(format!("graphersal-worker-{worker}"))
                .stack_size(SESSION_STACK_BYTES)
                .spawn(move || {
                    graphersal::workload::set_thread_default(
                        graphersal::workload::Channel::Playground,
                        true,
                    );
                    match template.session() {
                        Ok(mut session) => pool.work(&mut session),
                        Err(err) => errln!("warning: a worker cannot start its session: {err}"),
                    }
                })
                .map_err(|err| format!("Error: cannot start a worker thread: {err}"))?;
        }
    }
    Ok((jobs, cancel, commit_seq, statistics))
}

/// Force-layout jobs the pages of the read-only serving mode share (at least one per worker).
const SHARED_LAYOUT_JOBS: usize = 64;

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

/// Largest declared body (`Content-Length`) a request may name. `tiny_http` 0.12 skips an
/// unread body when the request is dropped (after the answer) by allocating ONE zeroed buffer
/// of the remaining declared length (`EqualReader::drop`), so a request that declares, say,
/// 2^62 bytes would abort the whole process (an allocation failure is not a panic). Such a
/// request is never dropped: it gets no answer and its connection is leaked (no client of this
/// server sends one; the bodies it reads are at most `graphersal_serve::MAX_BODY_BYTES`, and a
/// declared body up to this size still gets its `413`).
const MAX_DECLARED_BODY_BYTES: usize = 4 * graphersal_serve::MAX_BODY_BYTES;

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
        let _ = request.respond(tiny_answer(response));
        return;
    }
    let Some(context) = serving.context.get() else {
        let _ = request.respond(tiny_answer(Answer::json(
            503,
            &json!({ "error": "the server is still loading its graph; GET /readyz answers \
                200 when it serves queries" }),
        )));
        return;
    };
    if serving.stop.stopping() {
        let _ = request.respond(tiny_answer(Answer::json(
            503,
            &json!({ "error": "the server is stopping" }),
        )));
        return;
    }
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
        .collect();
    let method = graphersal_serve::Method::parse(request.method().as_str());
    let url = request.url().to_string();
    let body_length = request.body_length();
    let response = {
        let mut converted = graphersal_serve::Request::new(method, &url, request.as_reader())
            .with_body_length(body_length);
        for (name, value) in &headers {
            converted = converted.with_header(name, value);
        }
        graphersal_serve::handle(context, converted)
    };
    let _ = request.respond(tiny_answer(response));
}

/// An answer as `tiny_http` writes it (a header `tiny_http` cannot carry is left out).
fn tiny_answer(answer: Answer) -> TinyAnswer {
    let (status, headers, body) = answer.into_parts();
    let mut response = tiny_http::Response::from_data(body).with_status_code(status);
    for (name, value) in &headers {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response = response.with_header(header);
        }
    }
    response
}

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
        return Some(Answer::json(
            405,
            &json!({ "error": format!("method {method} is not allowed on {path}") }),
        ));
    }
    // Probes come every few seconds: never above trace.
    tracing::trace!(target: "graphersal::server", "{method} {path}");
    if path == "/healthz" {
        return Some(Answer::json(200, &json!({ "status": "ok" })));
    }
    let (status, state) = if serving.stop.stopping() {
        (503, "stopping")
    } else if serving.context.get().is_none() {
        (503, "loading")
    } else {
        (200, "ready")
    };
    Some(Answer::json(status, &json!({ "status": state })))
}

/// [`graphersal_serve::Request::json_body`], for the AI Chat's endpoints.
#[cfg(feature = "ai")]
fn json_body(request: &mut graphersal_serve::Request) -> Result<Value, HttpError> {
    request.json_body()
}

/// A JSON answer, for the AI Chat's endpoints.
#[cfg(feature = "ai")]
fn json_response(status: u16, body: &Value) -> Answer {
    Answer::json(status, body)
}

/// A `200` JSON answer, for the AI Chat's endpoints.
#[cfg(feature = "ai")]
fn value(body: Value) -> Result<Answer, HttpError> {
    Ok(Answer::json(200, &body))
}

/// A number query parameter, for the AI Chat's endpoints.
#[cfg(feature = "ai")]
fn query_number(query: &str, key: &str) -> Option<usize> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
}

/// The run registry's path of the pool's tests (`crate::server::runs::Runs`), now
/// `graphersal_serve`'s.
#[cfg(test)]
mod runs {
    pub(crate) use graphersal_serve::Runs;
}
