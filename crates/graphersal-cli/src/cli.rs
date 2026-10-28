//! Command-line front end for `graphersal`.
//!
//! Besides the interactive REPL, the binary runs as a one-shot query runner: a script arrives on
//! `stdin` or through `--eval`, the rendered result is written to `stdout`, the diagnostic of a
//! failing query to `stderr`, and nothing else is printed. That makes the output directly
//! assertable from a shell script, a CI job, or an AI agent verifying a Gremlin-like query.

use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use graphersal::schema::{GraphSchema, SchemaMode, VisualizeFormat};
use graphersal::script::ScriptLimits;
use graphersal::{
    ElementProperty, GraphSource,
    exec::{RenderOptions, Spelling},
    prelude::Graph,
};
use rhai::{Dynamic, Engine, Scope};

/// What a script run by the CLI may do: everything ([`AllowAll`](graphersal::auth::AllowAll),
/// the library's default). The CLI is a trusted local front end: the user runs their own scripts
/// on their own graph and files.
pub fn authorizer() -> Arc<dyn graphersal::auth::Authorizer> {
    Arc::new(graphersal::auth::AllowAll)
}

/// The registry [`install_embedders`] built at start, with the models of an opened Store added
/// ([`GraphSpec::open`]).
static EMBEDDERS: std::sync::RwLock<Option<Arc<graphersal::vector::Embedders>>> =
    std::sync::RwLock::new(None);

/// The embedders the CLI's scripts and its dev server call: the registry of the command line's
/// `--embedder`/`--default-embedder` ([`install_embedders`]) plus the models an opened Store
/// keeps for its schema, else the default registry, which holds the built-in `hash-ngram/1`.
pub fn embedders() -> Arc<graphersal::vector::Embedders> {
    EMBEDDERS
        .read()
        .ok()
        .and_then(|current| current.clone())
        .unwrap_or_else(|| Arc::new(graphersal::vector::Embedders::default()))
}

/// Replaces the registry [`embedders`] answers.
fn set_embedders(embedders: Arc<graphersal::vector::Embedders>) {
    if let Ok(mut current) = EMBEDDERS.write() {
        *current = Some(embedders);
    }
}

/// An embedding model the CLI serves besides the built-in `hash-ngram/1`: an `--embedder` model
/// or one an opened Store keeps. The start banners name it with its licence.
#[cfg_attr(
    not(any(feature = "dev-server", feature = "model2vec")),
    allow(dead_code)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedModel {
    /// The registry name queries call it by (`embed(text, name)`).
    pub name: String,
    /// The model identity (`model2vec/<repo>@<12 hex>`).
    pub identity: String,
    /// The repository (`owner/name`) when known, else the model identity.
    pub label: String,
    /// The licence: an SPDX id as the repository states it, or `unknown`.
    pub license: String,
    /// Where it comes from: `--embedder`, `stored in <store>`, or both (`--embedder; also
    /// stored in <store>`).
    pub source: String,
}

/// The models [`served_models`] lists, in the order they were registered.
#[cfg_attr(
    not(any(feature = "dev-server", feature = "model2vec")),
    allow(dead_code)
)]
static SERVED_MODELS: Mutex<Vec<ServedModel>> = Mutex::new(Vec::new());

/// The notes about models a Store's schema names that nobody provides.
static MODEL_NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Set by the dev server: the model notes of a Store open are kept for its start banner
/// instead of being printed at once.
static DEFER_MODEL_NOTES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The embedding models the CLI serves besides the built-in one, and the notes about models
/// the opened Store's schema names that nobody provides.
#[cfg_attr(not(feature = "dev-server"), allow(dead_code))]
pub fn served_models() -> (Vec<ServedModel>, Vec<String>) {
    let models = SERVED_MODELS.lock().map(|m| m.clone()).unwrap_or_default();
    let notes = MODEL_NOTES.lock().map(|n| n.clone()).unwrap_or_default();
    (models, notes)
}

/// Keeps the model notes of the next Store open for the caller's own output (the dev server's
/// start banner) instead of printing them as `note:` lines.
#[cfg_attr(not(feature = "dev-server"), allow(dead_code))]
pub fn defer_model_notes() {
    DEFER_MODEL_NOTES.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// An SPDX licence id as people write it (the Hub states ids in lower case: `mit`,
/// `apache-2.0`); an id it does not know stays as it is.
#[cfg_attr(
    not(any(feature = "dev-server", feature = "model2vec")),
    allow(dead_code)
)]
pub fn license_text(id: &str) -> String {
    match id.to_ascii_lowercase().as_str() {
        "mit" => "MIT".to_string(),
        "apache-2.0" => "Apache-2.0".to_string(),
        "bsd-2-clause" => "BSD-2-Clause".to_string(),
        "bsd-3-clause" => "BSD-3-Clause".to_string(),
        "cc-by-4.0" => "CC-BY-4.0".to_string(),
        "cc-by-sa-4.0" => "CC-BY-SA-4.0".to_string(),
        "cc-by-nc-4.0" => "CC-BY-NC-4.0".to_string(),
        "gpl-3.0" => "GPL-3.0".to_string(),
        "agpl-3.0" => "AGPL-3.0".to_string(),
        _ => id.to_string(),
    }
}

/// Records a served model.
#[cfg_attr(not(feature = "model2vec"), allow(dead_code))]
fn serve_model(model: ServedModel) {
    if let Ok(mut models) = SERVED_MODELS.lock() {
        models.push(model);
    }
}

/// Records a model note, and prints it unless the notes are deferred ([`defer_model_notes`]).
fn model_note(note: String) {
    if !DEFER_MODEL_NOTES.load(std::sync::atomic::Ordering::Relaxed) {
        errln!("note: {note}");
    }
    if let Ok(mut notes) = MODEL_NOTES.lock() {
        notes.push(note);
    }
}

/// The `@<12 hex>` part of a model identity (`model2vec/<repo>@<hex>`): the same files give the
/// same hash whatever repository name an identity records.
#[cfg(feature = "model2vec")]
fn identity_hash(identity: &str) -> &str {
    identity.rsplit_once('@').map_or(identity, |(_, hash)| hash)
}

/// Loads the models of `--embedder NAME=PATH|REPO` (once, at start) and registers them with
/// `--default-embedder` as the default; [`embedders`] answers this registry from then on. A
/// model that does not load is the error (with its help); the process then exits with
/// [`EXIT_USAGE_ERROR`].
pub fn install_embedders(cli: &Cli) -> Result<(), String> {
    if cli.embedders.is_empty() && cli.default_embedder.is_none() {
        return Ok(());
    }
    let registry_error =
        |err: graphersal::vector::EmbedError| format!("{err}\nHelp: {}", err.help());
    #[allow(unused_mut)]
    let mut builder = graphersal::vector::Embedders::builder();
    #[cfg(feature = "model2vec")]
    for (name, spec) in &cli.embedders {
        let model = crate::embed_model::load_embedder(name, spec)?;
        let (repo, license) = crate::embed_model::embedder_license(spec);
        serve_model(ServedModel {
            name: name.clone(),
            identity: graphersal::vector::Embedder::info(&model).model.to_string(),
            label: repo.unwrap_or_else(|| spec.clone()),
            license,
            source: "--embedder".to_string(),
        });
        builder = builder
            .with(Arc::new(model))
            .map_err(|err| format!("--embedder {name}={spec}: {}", registry_error(err)))?;
    }
    #[cfg(not(feature = "model2vec"))]
    if let Some((name, spec)) = cli.embedders.first() {
        return Err(format!(
            "--embedder {name}={spec}: this graphersal was built without the `model2vec` \
             feature (real embedding models); only the built-in hash-ngram/1 is available"
        ));
    }
    if let Some(name) = &cli.default_embedder {
        builder = builder
            .default_name(name)
            .map_err(|err| format!("--default-embedder {name}: {}", registry_error(err)))?;
    }
    set_embedders(Arc::new(builder.build()));
    Ok(())
}

/// Registers the embedding models the opened `store` keeps and its schema names (format spec
/// 4.5) beside the `--embedder` ones, which win; prints what it loaded and every model the
/// schema names that neither provides as a note on stderr.
#[cfg(feature = "model2vec")]
fn register_stored_models(
    path: &str,
    store: &graphersal::persist::Store,
) -> Result<(), graphersal::persist::PersistError> {
    register_found_models(path, store.stored_embedders(&embedders())?, store.models()?);
    Ok(())
}

/// [`register_stored_models`] of a Store loaded read-only (`--read-only`): the models are read
/// from `path` without opening the store, under the schema of `graph`.
#[cfg(feature = "model2vec")]
fn register_stored_models_read_only(path: &str, graph: &Graph) -> Result<(), String> {
    use graphersal::persist::Store;
    use graphersal::prelude::GraphStorage;
    let format =
        |err: graphersal::persist::PersistError| graphersal::error::format_diagnostic(&err);
    let schema = graph
        .read_checked()
        .and_then(|graph| graph.get_schema())
        .map_err(|err| graphersal::error::format_diagnostic(&err))?;
    let found = Store::stored_embedders_dir(path, schema.as_ref(), &embedders()).map_err(format)?;
    register_found_models(path, found, Store::models_dir(path).map_err(format)?);
    Ok(())
}

/// Without the `model2vec` feature no stored model can be loaded (a read-only open names none).
#[cfg(not(feature = "model2vec"))]
fn register_stored_models_read_only(_path: &str, _graph: &Graph) -> Result<(), String> {
    Ok(())
}

/// Registers what [`graphersal::persist::Store::stored_embedders`] found in the store at
/// `path` (`stored`: the models it keeps) and lists them for the start banner.
#[cfg(feature = "model2vec")]
fn register_found_models(
    path: &str,
    found: graphersal::persist::StoredEmbedders,
    stored: Vec<graphersal::persist::StoredModel>,
) {
    for (name, identity) in &found.loaded {
        let model = stored
            .iter()
            .find(|model| identity_hash(&model.identity) == identity_hash(identity));
        let license = model.map_or("unknown", |model| model.license.as_str());
        if !DEFER_MODEL_NOTES.load(std::sync::atomic::Ordering::Relaxed) {
            errln!(
                "note: loaded the embedding model {identity} from the store {path} as {name} \
                 (licence {})",
                license_text(license)
            );
        }
        serve_model(ServedModel {
            name: name.clone(),
            identity: identity.clone(),
            label: model
                .and_then(|model| model.repo.clone())
                .unwrap_or_else(|| identity.clone()),
            license: license.to_string(),
            source: format!("stored in {path}"),
        });
    }
    // A stored model another embedder serves (an `--embedder` of the same files) is named on
    // that embedder's row; one that could not be used gets a row of its own.
    for model in &stored {
        let hash = identity_hash(&model.identity);
        if found.loaded.iter().any(|(_, id)| identity_hash(id) == hash) {
            continue;
        }
        let also = format!("also stored in {path}");
        let covered = SERVED_MODELS.lock().ok().is_some_and(|mut served| {
            served
                .iter_mut()
                .find(|served| identity_hash(&served.identity) == hash)
                .map(|served| served.source = format!("{}; {also}", served.source))
                .is_some()
        });
        if !covered {
            serve_model(ServedModel {
                name: "(unused)".to_string(),
                identity: model.identity.clone(),
                label: model.repo.clone().unwrap_or_else(|| model.identity.clone()),
                license: model.license.clone(),
                source: format!(
                    "stored in {path}; not loaded: {}",
                    model.problem.as_deref().unwrap_or("it does not load")
                ),
            });
        }
    }
    for note in &found.notes {
        model_note(note.clone());
    }
    if !found.loaded.is_empty() {
        set_embedders(found.embedders);
    }
}

/// Without the `model2vec` feature no stored model can be loaded: the schema's models are
/// named in a note.
#[cfg(not(feature = "model2vec"))]
fn register_stored_models(
    path: &str,
    store: &graphersal::persist::Store,
) -> Result<(), graphersal::persist::PersistError> {
    let registered = embedders();
    for identity in store.info()?.schema_models {
        if registered.by_model(&identity).is_none() {
            model_note(format!(
                "the schema names the embedding model {identity}; this graphersal was built \
                 without the `model2vec` feature and cannot load it (from {path} or \
                 elsewhere)"
            ));
        }
    }
    Ok(())
}

/// The IANA name of the local time zone of this machine, for the scheduled jobs: the `TZ`
/// variable when it names one (optionally with a leading `:`), else the target of the
/// `/etc/localtime` link (`.../zoneinfo/Europe/Berlin`); `None` when neither tells (the jobs
/// then run in UTC).
#[cfg(feature = "dev-server")]
pub fn local_time_zone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let name = tz.trim_start_matches(':');
        if graphersal::jobs::Scheduler::new()
            .with_time_zone(name)
            .is_ok()
        {
            return Some(name.to_string());
        }
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let text = target.to_string_lossy();
    let name = text.split("zoneinfo/").nth(1)?;
    graphersal::jobs::Scheduler::new()
        .with_time_zone(name)
        .is_ok()
        .then(|| name.to_string())
}

/// Exit code returned when every script evaluated successfully.
pub const EXIT_OK: i32 = 0;
/// Exit code returned when a script failed to evaluate.
pub const EXIT_QUERY_ERROR: i32 = 1;
/// Exit code returned when the invocation itself is invalid: unknown flags or an unreadable graph.
pub const EXIT_USAGE_ERROR: i32 = 2;

/// Default number of top-level results a displayed result shows.
pub const DEFAULT_MAX_ROWS: usize = 100;
/// Default number of items a nested collection inside a displayed result shows.
pub const DEFAULT_MAX_ITEMS: usize = 100;

/// Usage text printed by `--help`.
pub const HELP: &str = r#"graphersal - Graphersal interactive REPL and one-shot Gremlin-like query runner

USAGE:
    graphersal [OPTIONS]                 # interactive REPL (/help lists its commands)
    graphersal [OPTIONS] <SCRIPT>        # evaluate SCRIPT, print the result, exit
    graphersal [OPTIONS] --in            # evaluate the script read from stdin, print the result, exit
    graphersal --graph <SOURCE> --server [--port N] [--host ADDR] [--ui-dir DIR]
               [--mcp [--mcp-read-only] [--mcp-token SECRET] [--mcp-query-tools] [--mcp-instructions FILE]
               [--mcp-max-chars N]]
               [--ai FILE]
                                         # DEV server: the playground UI over HTTP (unstable),
                                         # optionally an MCP endpoint for AI agents
    graphersal store <COMMAND> <DIR> ... # manage a Store (graphersal store help)
    graphersal embed-model <COMMAND> ... # download and list embedding models
                                         # (graphersal embed-model help)
    graphersal health [--ready] [--host ADDR] [--port N]
                                         # probe a running --server: exit 0 when /healthz
                                         # (/readyz) answers 200 (graphersal health --help)

OPTIONS:
    -g, --graph <SOURCE>   Graph to run against: "modern" (TinkerPop Modern, the default),
                           "empty", "large" (generated 110k benchmark graph), "tree"
                           (filesystem-like sample tree for glob_path), or a path to a
                           GraphSON (*.json, *.jsonl, *.graphson) or GraphML (*.xml,
                           *.graphml) file, or a packed snapshot (*.gsnap, written by
                           g.export_snapshot(path)), or a Store (graphersal store create
                           <dir>): a directory, or a single-file store (*.gstore): opened
                           read-write, every commit is written to its write-ahead log before
                           it returns. Data a GraphSON import skips (dates, unsupported types)
                           is listed in a note on stderr.
        --schema <FILE>    Schema file (the schema format, JSON) for the --graph: with a
                           GraphSON or GraphML file it is set on the new empty graph first and
                           the file is imported against it as one unit, so declared logical
                           types (uuid, array, object) come back and open/closed modes are
                           enforced; with modern, empty, large or tree it is applied to that
                           graph (validated). Data that violates it fails the start (exit 1).
                           Refused with a Store or a .gsnap snapshot: they carry their schema.
                           With --server, "Save to file" writes the schema back to it too.
        --schema-mode <M>  With --schema: overrides the file's "mode": none, open or closed.
        --create-store     With --graph <dir>: create the store there when it does not exist
                           yet (the directory must be missing or empty; a path ending in
                           .gstore becomes a single-file store).
        --read-only        Serve the --graph read-only: every write (queries, schema, saved
                           queries, indexes, "Save to file") is refused. A Store is loaded
                           without its lock and nothing in it is written (no recovery cut, no
                           checkpoint, no workload statistics), so a read-only file system, a
                           :ro container mount or a file without write permission works; its
                           Store panel and history are not offered. A Store that cannot be
                           opened for writing (no permission, read-only file system) falls back
                           to this mode with a note.
        --on-damage <P>    With --create-store: the damage policy of the new store, fixed for
                           its life: "maintenance" (the default: damage found on disk while it
                           is open makes it read-only) or "continue" (it keeps committing and
                           reports the damage).
    -e, --eval <SCRIPT>    Evaluate SCRIPT. May be repeated; scripts share one scope and are
                           evaluated in order, each result printed on its own.
    -i, --in, -            Read the script to evaluate from stdin. Used implicitly when stdin is
                           not a terminal.
    -f, --format <FORMAT>  Default visualizer for results rendered without a data terminal:
                           auto, table, markdown, json, jsonschema, mermaid, plantuml, tree.
    -t, --timeout <MS>     Default evaluationTimeout in milliseconds for every traversal, and
                           the budget of each script as a whole. A query overrides it with
                           g.with("evaluationTimeout", ms). 0 or no flag: no timeout.
        --max-rows <N>     Show at most N top-level results (rows, traversers, list items) of a
                           displayed result; default 100. N >= 1.
        --max-items <N>    Show at most N items of every nested list or map inside a displayed
                           result; default 100. N >= 1.
        --no-limit         Show displayed results completely. Cannot be combined with
                           --max-rows or --max-items.
        --spelling <S>     Spelling of step names in .profile() and error messages: "snake"
                           (has_label(), the default) or "camel" (Gremlin hasLabel()). A query
                           overrides it with g.with("render.spelling", "camel"). In the REPL:
                           /set spelling camel.
        --safe-limits      Run scripts under the SAFE resource limits of the library (10M
                           operations, 16 MiB strings, 100k array/map items) instead of none.
        --max-operations <N>   Rhai operations one script may run (a `loop {}` stops here).
        --max-string-size <N>  Longest string a script or a step (replace(), concat(), ...) may
                           build, in bytes (script.max_string_size and the session default of
                           traversal.max_string_bytes).
        --max-array-size <N>   Largest array a script may build, in items.
        --max-map-size <N>     Largest map a script may build, in entries.
        --max-traversers <N>   Default traversal.max_traversers: traversers one step may leave
                           alive (the library default is 10M; this tool defaults to unlimited).
                           For the four --max-* script limits and this one, 0 means unlimited.
        --max-value-depth <N>  Deepest nesting of arrays and maps in one value, for script
                           values (script.max_value_depth) and engine-built values
                           (traversal.max_value_depth); default 128, 0 means the default.
                           Never unlimited: deeper values would overflow the native stack.
        --memory-limit <BYTES|auto>
                           Memory budget of every traversal (memory.limit): a traversal that
                           would take the process's live heap over BYTES fails with a
                           "Resource limit 'memory.limit'" diagnostic and rolls back, instead of
                           the process being killed for lack of memory. "auto" is the cgroup
                           memory limit (Linux containers) minus the headroom, or no budget
                           without one. No flag or 0: no budget (the default).
        --memory-headroom <BYTES>
                           What --memory-limit auto keeps free below the cgroup limit
                           (memory.headroom); default a tenth of the limit, at least 64 MiB.
        --server           DEV MODE, UNSTABLE, LOCAL ONLY: serve the graph of --graph and the
                           playground UI on http://127.0.0.1:<port>/ (the same UI as the
                           browser playground, with this process as its engine instead of
                           WebAssembly). Binds the loopback (127.0.0.1, and [::1] when
                           available; inside a container 0.0.0.0, see --host), no
                           authentication. With a Store
                           directory (--graph <dir>) every commit is persisted and the UI
                           shows the store (checkpoints, marks, snapshot downloads); otherwise
                           the graph lives in memory and "Save to file" writes it back to a
                           --graph file on request. The limit options
                           above (--timeout, --memory-limit, --memory-headroom, --safe-limits,
                           --max-operations, --max-traversers, ...) apply to every query it
                           runs; the display options (--format, --max-rows, --max-items,
                           --no-limit, --spelling) are refused (the UI sets them per query).
                           Store warnings (a failed automatic checkpoint, damage found) go
                           to stderr; GRAPHERSAL_LOG=off|error|warn|info|debug|trace sets
                           the level (default warn). Stop it with Ctrl+C.
        --port <N>         Port of --server; default 8080. 0 picks a free port (the address
                           is printed on stdout).
        --host <ADDR>      Address --server listens on: an IP address or a host name, e.g.
                           0.0.0.0 (every IPv4 interface), :: (every IPv6 interface),
                           192.168.1.5, localhost. Default 127.0.0.1 (this machine only), but
                           0.0.0.0 inside a container (/.dockerenv or /run/.containerenv
                           exists, or KUBERNETES_SERVICE_HOST is set), so docker run -p
                           8080:8080 ... works without flags. A
                           non-loopback address makes the server reachable from the network
                           WITHOUT authentication (a warning on stderr says so); requests
                           must then name the server by an IP address, localhost, a name
                           without a dot, or the --host name.
        --ui-dir <DIR>     A directory whose files --server serves instead of the playground
                           UI embedded in the binary (a file missing there comes from the
                           embedded copy); default ./playground when it exists.
        --mcp              With --server: also serve an MCP endpoint (Streamable HTTP) on
                           http://127.0.0.1:<port>/mcp, on the same graph and session as the
                           browser, for an AI agent (Claude Code: claude mcp add --transport
                           http graphersal http://127.0.0.1:<port>/mcp).
        --mcp-read-only    With --mcp: the agent may only read (no writing tools; a query that
                           would write is refused).
        --mcp-token <SECRET>
                           With --mcp: the agent must send "Authorization: Bearer <SECRET>".
        --mcp-query-tools  With --mcp: also offer every saved query as a tool of its own (its
                           name, description and parameters); for small curated catalogs.
                           list_saved_queries and run_saved_query are always offered.
        --mcp-instructions <FILE>
                           With --mcp: a UTF-8 text file (at most 20000 characters) appended
                           to the instructions the MCP server answers to initialize, under
                           "Notes from the operator of this server about this graph:" (what
                           the graph holds, which queries to prefer). Operator-trusted text.
                           The AI Chat does not use it: its own "system_prompt" does that.
        --mcp-max-chars <N>
                           With --mcp: the largest tool answer in characters (default 60000,
                           at least 1000). A longer answer keeps whole results only (valid
                           compact JSON) and ends with a note naming the next page's offset.
                           The client may limit tool output too (Claude Code: about 25000
                           tokens, MAX_MCP_OUTPUT_TOKENS), so raising this alone may not help.
        --ai <FILE>        With --server: EXPERIMENTAL AI Chat, an LLM agent working on the
                           served graph. FILE is JSON: {"provider": "openai"|"anthropic",
                           "model": "<id>", "api_key_env": "<VARIABLE>", "base_url": null,
                           "read_only": false, "max_tool_calls_per_turn": 20,
                           "max_tokens": 4096}. "openai" is Chat Completions (OpenAI and
                           compatible servers: Azure, OpenRouter, Groq, Ollama, LM Studio,
                           vLLM through base_url). The key is read from the environment
                           variable api_key_env names and stays in this process.
        --embedder <NAME=PATH|REPO>
                           Register a Model2Vec embedding model under NAME for the scripts
                           (embed(), the embed step) of every mode, the --server and its MCP
                           tools: PATH is a model directory (config.json, tokenizer.json,
                           model.safetensors), REPO a model fetched with `graphersal
                           embed-model download REPO` (looked up in $GRAPHERSAL_EMBED_MODELS or
                           ~/.cache/graphersal/embed-models). May be repeated. Loaded once at
                           start, locally (no network); a model that does not load exits 2.
                           The built-in hash-ngram/1 is always registered.
        --default-embedder <NAME>
                           The embedder embed() uses without #{embedder: ..}: a --embedder NAME
                           or hash-ngram/1 (the default).
        --repl             Start the REPL even when stdin is not a terminal: lines are read
                           without editing, commands (/help, /set ...) work, no history file
                           (for scripted sessions of the shell).
        --args-file <FILE> Read arguments from FILE (also written @FILE), once: a JSON object
                           whose keys are the long flag names without dashes, e.g.
                           {"graph": "data.gstore", "server": true, "port": 8080, "mcp": true,
                           "embedder": ["a=/models/a"], "_comment": "ignored"}. A switch is
                           true (false = absent), a repeatable flag (eval, embedder) takes a
                           string or an array, numbers are written as numbers. Relative paths
                           resolve against the file's directory. The command line wins: a flag
                           given there replaces the file's, a repeatable one adds to it. An
                           unknown key is an error (exit 2); a key starting with "_" is a
                           comment. The file may not name another args file.
    -h, --help             Print this help and exit.
    -V, --version          Print the version and exit.

RESOURCE LIMITS:
    graphersal is a trusted local tool, so scripts run without resource limits by default. Use
    --safe-limits, or any --max-* flag, to bound a script; a limit hit stops the script with a
    "Resource limit '...' exceeded" diagnostic and exit code 1. In the REPL, /set max-operations
    <n> (and max-string-size, max-array-size, max-map-size, max-value-depth) changes the limit,
    /set safe-limits applies the SAFE defaults. Scripts run on a thread with a 64 MiB stack, so
    the scripting engine's own recursion over deeply nested values has room.

DISPLAY LIMITS:
    Only display is bounded, never data: to_list(), next() and variables hold every result.
    A cut result prints a notice ("… showing the first 100 rows; more exist. ...") on stderr,
    so stdout stays clean for pipes and --format json stays valid JSON; the exit code stays 0.
    A single call overrides the limits with g.v().to_table(#{max_rows: 20}) or
    g.v().to_json(#{no_limit: true}).

EXIT CODES:
    0  every script evaluated successfully
    1  a script failed to evaluate (diagnostic on stderr)
    2  invalid invocation: unknown option, or a graph or schema file that could not be loaded
       (exit 1 when the --graph data violates the --schema, or a --graph store cannot be
       opened: in use by another writer, damaged, an older format)
    141 stdout was closed before every result was written (`graphersal -e .. | head -1`):
       the run stops quietly at the next script; a failing script keeps its own code.
       Output is never a panic: a closed stderr only loses the diagnostics.

NOTE:
    Built with exact per-step memory tracking (the `alloc-tracking` feature): `.profile()`'s
    `Mem` column reports real allocator bytes, not estimates. `g.memory_usage()` reports a
    graph's own footprint directly and is unaffected by this feature either way.

EXAMPLES:
    echo 'g.v().values("age").max().next()' | graphersal --in --graph modern
    graphersal --graph modern -e 'g.v().has_label("person").count().next()'
    graphersal --graph ./mygraph.graphml -e 'g.infer_schema()' --format mermaid
    graphersal --graph ./data.graphml --schema ./schema.json --schema-mode closed -e 'g.v()'
    graphersal --graph ./tinkerpop-modern.json -e 'g.v().has_label("person").count().next()'
    graphersal --graph large --timeout 50 -e 'g.v().out().out().out().count().next()'
    graphersal -e 'g.memory_usage()'
    graphersal --max-operations 1000000 -e 'let n = 0; loop { n += 1; }'
    graphersal --graph large --no-limit -e 'g.v()' > all_vertices.txt
    graphersal --graph large --memory-limit auto -e 'g.v().out().out().fold()'
    graphersal --graph ./mygraph.json --server --port 8080      # then open http://localhost:8080/
    graphersal --graph ./mygraph.json --server --host 0.0.0.0  # reachable from the network (no auth!)
    graphersal store create data/ --from modern && graphersal --graph data/ --server
    graphersal --args-file args-dev-server.json --port 9000     # the file, with the port replaced
    graphersal embed-model download minishlab/potion-base-8M
    graphersal --embedder potion=minishlab/potion-base-8M --default-embedder potion \
        -e 'g.v().values("name").embed().to_list()'
"#;

/// What the process should do once the command line has been parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Start the interactive REPL.
    Repl,
    /// Evaluate the collected scripts and exit.
    Eval,
    /// Print usage and exit.
    Help,
    /// Print the version and exit.
    Version,
    /// Run the DEV server (`--server`): the playground UI over HTTP on 127.0.0.1.
    Server,
}

/// The graph a session is started against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GraphSpec {
    /// The TinkerPop Modern sample graph (6 vertices, 6 edges).
    #[default]
    Modern,
    /// An empty graph, to be populated by the script itself.
    Empty,
    /// The generated ~110k element benchmark graph.
    Large,
    /// The filesystem-like `file_tree` sample graph (37 vertices, 38 edges).
    FileTree,
    /// A graph imported from a GraphSON or GraphML file (chosen by the extension).
    File(String),
    /// A Store directory (`graphersal store create <dir>`): opened read-write with its lock,
    /// every commit goes to its write-ahead log.
    Store(String),
}

/// Why a graph could not be prepared for a session: the diagnostic and the exit code
/// ([`EXIT_USAGE_ERROR`] for an unreadable or invalid file, [`EXIT_QUERY_ERROR`] for data that
/// violates the `--schema`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartError {
    /// The process exit code.
    pub code: i32,
    /// The diagnostic, for stderr.
    pub message: String,
}

impl StartError {
    /// An invalid invocation or file ([`EXIT_USAGE_ERROR`]).
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_USAGE_ERROR,
            message: message.into(),
        }
    }

    /// An operation that failed on a valid invocation, such as a store that cannot be opened
    /// (in use, damaged, ...) ([`EXIT_QUERY_ERROR`]).
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_QUERY_ERROR,
            message: message.into(),
        }
    }

    /// Data that violates the `--schema` ([`EXIT_QUERY_ERROR`]).
    pub fn violation(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_QUERY_ERROR,
            message: message.into(),
        }
    }
}

impl From<String> for StartError {
    fn from(message: String) -> Self {
        Self::usage(message)
    }
}

/// `--schema FILE [--schema-mode MODE]`: the schema file for the `--graph`, and the mode that
/// overrides the file's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaSource {
    /// The schema file (the schema format, JSON).
    pub path: String,
    /// `--schema-mode`: replaces the file's `mode`.
    pub mode: Option<SchemaMode>,
}

impl SchemaSource {
    /// Reads and parses the schema file, with `--schema-mode` applied. A failure names the
    /// schema file (exit code [`EXIT_USAGE_ERROR`]).
    pub fn read(&self) -> Result<GraphSchema, StartError> {
        let text = std::fs::read_to_string(&self.path).map_err(|err| {
            StartError::usage(format!(
                "Error: cannot read the schema file '{}': {err}",
                self.path
            ))
        })?;
        let schema = GraphSchema::from_json(&text).map_err(|err| {
            StartError::usage(format!(
                "Schema file '{}': {}",
                self.path,
                graphersal::error::format_diagnostic(&err)
            ))
        })?;
        Ok(match self.mode {
            Some(mode) => schema.with_mode(mode),
            None => schema,
        })
    }
}

/// The refusal of `--schema` with a graph that carries its own schema (`what`: "a Store", "a
/// packed snapshot").
fn schema_refused(path: &str, what: &str) -> String {
    format!(
        "--schema does not apply to '{path}': {what} carries its own schema\nHelp: drop \
         --schema and change the stored schema with g.set_schema(...) or g.patch_schema(...)"
    )
}

/// A graph ready for a session: shared, and the Store that keeps it when it came from one.
pub struct Opened {
    /// The graph.
    pub graph: Arc<Graph>,
    /// The open Store of a `--graph <dir>`; close it (or drop it) when the session ends.
    pub store: Option<Arc<graphersal::persist::Store>>,
}

impl Opened {
    /// Closes the Store (a clean close: the WAL synced, the store marked closed).
    pub fn close(self) {
        if let Some(store) = self.store
            && let Err(err) = store.shutdown()
        {
            errln!("{}", graphersal::error::format_diagnostic(&err));
        }
    }

    /// Closes the Store cleanly on SIGINT/SIGTERM (exits with `exit_code` afterwards). Without
    /// a Store a signal ends the process by its default action, except as PID 1 (a container's
    /// first process ignores SIGTERM without a handler): there it exits with
    /// [`crate::signals::EXIT_INTERRUPTED`].
    pub fn close_on_signal(&self, cancel: Option<graphersal::exec::CancelToken>, exit_code: i32) {
        match &self.store {
            Some(store) => crate::signals::close_store_on_signal(store.clone(), cancel, exit_code),
            None => crate::signals::exit_on_signal_as_pid1(),
        }
    }
}

/// Why [`GraphSpec::open`] could not open a Store read-write.
enum StoreOpenError {
    /// The store may not be written by this process (no permission, a read-only file
    /// system): the I/O error's text. `--graph` falls back to a read-only open.
    NotWritable(String),
    /// Any other failure.
    Other(StartError),
}

impl From<StartError> for StoreOpenError {
    fn from(err: StartError) -> Self {
        StoreOpenError::Other(err)
    }
}

impl From<String> for StoreOpenError {
    fn from(message: String) -> Self {
        StoreOpenError::Other(message.into())
    }
}

/// Loads the Store at `path` read-only (`--read-only`, or a store this process may not write):
/// the latest state, without its lock and without writing anything into it
/// ([`graphersal::persist::Store::open_read_only`]; an interrupted last write is ignored, not
/// cut), with its embedding models registered and every write refused ([`refuse_writes`]).
fn open_store_read_only(path: &str) -> Result<Opened, StartError> {
    use graphersal::persist::{RecoveryTarget, Store};
    let format = |err: graphersal::persist::PersistError| {
        StartError::failed(graphersal::error::format_diagnostic(&err))
    };
    if !Store::is_store(path).map_err(format)? {
        return Err(StartError::failed(format!(
            "Error: '{path}' is not a Graphersal store (no GRAPH file)\nHelp: --read-only opens \
             an existing store; create one with `graphersal store create {path}` first."
        )));
    }
    let recovered = Store::open_read_only(path, RecoveryTarget::Latest).map_err(format)?;
    let graph = Graph::new(recovered.graph);
    register_stored_models_read_only(path, &graph).map_err(StartError::failed)?;
    refuse_writes(&graph)?;
    Ok(Opened {
        graph: Arc::new(graph),
        store: None,
    })
}

/// The commit hook of `--read-only`: vetoes every unit that changes something (data, schema,
/// catalog), so every front end (queries, the dev server's API, MCP, the REPL) refuses writes
/// with one diagnostic.
struct ReadOnlyGraph;

impl graphersal::changes::CommitHook for ReadOnlyGraph {
    fn name(&self) -> &str {
        "read-only"
    }

    fn before_commit(
        &mut self,
        _: &graphersal::changes::ChangeSet,
    ) -> Result<(), graphersal::changes::HookError> {
        Err(Box::new(graphersal::changes::HookRefusal::new(
            "the graph is served read-only: writes are refused",
            "Run reading queries only; to change the data, start graphersal without \
             --read-only on a copy the process may write.",
        )))
    }
}

/// Makes `graph` refuse every write ([`ReadOnlyGraph`]).
fn refuse_writes(graph: &Graph) -> Result<(), StartError> {
    use graphersal::prelude::GraphStorage;
    graph
        .write_checked()
        .and_then(|mut graph| graph.register_commit_hook(Box::new(ReadOnlyGraph)))
        .map_err(|err| StartError::failed(graphersal::error::format_diagnostic(&err)))
}

impl GraphSpec {
    /// Resolves a `--graph` argument: a known sample name, otherwise a file path.
    pub fn parse(spec: &str) -> Self {
        match spec.to_ascii_lowercase().as_str() {
            "modern" | "tinkerpop_modern" | "tinkerpop-modern" => GraphSpec::Modern,
            "empty" | "new" => GraphSpec::Empty,
            "large" | "large_110k" => GraphSpec::Large,
            "tree" | "file_tree" | "file-tree" => GraphSpec::FileTree,
            // A directory, or a missing path without a file extension: a Store directory; a
            // single-file store (an existing one, or any path ending in .gstore).
            _ if std::path::Path::new(spec).is_dir()
                || spec.ends_with('/')
                || (!std::path::Path::new(spec).exists()
                    && std::path::Path::new(spec).extension().is_none())
                || std::path::Path::new(spec).extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case(graphersal::persist::SINGLE_FILE_EXTENSION)
                })
                // A file that cannot be read is opened as a store: the open names the read
                // error (a plain graph file would only be refused as unreadable later).
                || !matches!(graphersal::persist::SingleFileDir::is_single_file(spec), Ok(false)) =>
            {
                GraphSpec::Store(spec.to_string())
            }
            _ => GraphSpec::File(spec.to_string()),
        }
    }

    /// Materializes the graph, reporting an import failure as a formatted diagnostic.
    pub fn load(&self) -> Result<Graph, String> {
        match self {
            GraphSpec::Modern => Ok(GraphSource::tinkerpop_modern()),
            GraphSpec::Empty => Ok(GraphSource::empty()),
            GraphSpec::Large => Ok(GraphSource::large_110k()),
            GraphSpec::FileTree => Ok(GraphSource::file_tree()),
            GraphSpec::File(path) if GraphSource::is_graphson_path(path) => {
                let (graph, report) = graphersal::io::GraphSONImport::new()
                    .load_file(path)
                    .map_err(|err| graphersal::error::format_diagnostic(&err))?;
                // Skipped data is not a failure, but the user must see it: a note on stderr.
                if report.has_skips() {
                    errln!("note: GraphSON import of '{path}': {report}");
                }
                Ok(graph)
            }
            GraphSpec::File(path) => GraphSource::from_file(path)
                .map_err(|err| graphersal::error::format_diagnostic(&err)),
            GraphSpec::Store(path) => Err(format!(
                "Error: '{path}' is a Store directory: it is opened with its lock, not loaded"
            )),
        }
    }

    /// Materializes the graph with the `--schema` applied (without one: [`Self::load`]). A
    /// GraphSON or GraphML file is imported into a new empty graph that stores the schema
    /// first, as one unit, so the declared logical types come back and the mode is enforced; a
    /// sample graph gets the schema set afterwards (validated against its data). A Store and a
    /// packed snapshot carry their own schema and are refused ([`Cli::parse`] refuses them
    /// already). Data that violates the schema is a [`StartError::violation`].
    pub fn load_with_schema(&self, schema: Option<&SchemaSource>) -> Result<Graph, StartError> {
        let Some(source) = schema else {
            return Ok(self.load()?);
        };
        let schema = source.read()?;
        let violation = |message: String| {
            StartError::violation(format!(
                "{message}\nnote: the --graph data does not fit the schema file '{}'",
                source.path
            ))
        };
        match self {
            GraphSpec::File(path) if GraphSource::is_snapshot_path(path) => {
                Err(StartError::usage(schema_refused(path, "a packed snapshot")))
            }
            GraphSpec::Store(path) => Err(StartError::usage(schema_refused(path, "a Store"))),
            GraphSpec::File(path) => {
                use graphersal::io::{GraphImport, IOError};
                let mut graph = GraphSource::empty();
                graph
                    .write()
                    .traversal_mut()
                    .set_schema(schema)
                    .map_err(|err| {
                        StartError::usage(graphersal::error::format_diagnostic(&*err))
                    })?;
                // Data that does not fit the schema is a violation; anything else (an
                // unreadable or malformed file) an invalid invocation.
                let failed = |err: IOError| {
                    let message = graphersal::error::format_diagnostic(&err);
                    match err {
                        IOError::Graph(_) | IOError::InvalidJsonValue { .. } => violation(message),
                        _ => StartError::usage(message),
                    }
                };
                if GraphSource::is_graphson_path(path) {
                    let report = graph.import_graphson(path).map_err(failed)?;
                    if report.has_skips() {
                        errln!("note: GraphSON import of '{path}': {report}");
                    }
                } else if GraphSource::is_graphml_path(path) {
                    graph.import_graphml(path).map_err(failed)?;
                } else {
                    // Not a graph file format: the loader's own diagnostic.
                    return Err(StartError::usage(self.load().err().unwrap_or_else(|| {
                        format!("Error: unsupported graph file format: '{path}'")
                    })));
                }
                Ok(graph)
            }
            GraphSpec::Modern | GraphSpec::Empty | GraphSpec::Large | GraphSpec::FileTree => {
                let graph = self.load()?;
                graph
                    .write()
                    .traversal_mut()
                    .set_schema(schema)
                    .map_err(|err| violation(graphersal::error::format_diagnostic(&*err)))?;
                Ok(graph)
            }
        }
    }

    /// Opens the graph for a session: a Store directory is opened read-write (created first
    /// with `create`, with the damage policy `on_damage`), anything else is loaded with the
    /// `--schema` `schema` applied ([`Self::load_with_schema`]; a Store refuses it). What the
    /// Store's open did goes to stderr. An existing store with another damage policy than
    /// `on_damage` is refused: the policy is fixed when a store is created.
    pub fn open(
        &self,
        create: bool,
        on_damage: Option<graphersal::persist::DamagePolicy>,
        schema: Option<&SchemaSource>,
        read_only: bool,
    ) -> Result<Opened, StartError> {
        let GraphSpec::Store(path) = self else {
            let graph = self.load_with_schema(schema)?;
            if read_only {
                refuse_writes(&graph)?;
            }
            return Ok(Opened {
                graph: Arc::new(graph),
                store: None,
            });
        };
        if schema.is_some() {
            return Err(StartError::usage(schema_refused(path, "a Store")));
        }
        if read_only {
            return open_store_read_only(path);
        }
        match self.open_store(path, create, on_damage) {
            // A store this process may not write (no permission, a read-only file system such
            // as a `:ro` container mount) is served read-only instead of not at all.
            Err(StoreOpenError::NotWritable(message)) if !create => {
                errln!(
                    "note: the store {path} cannot be opened for writing ({message}): serving \
                     it READ-ONLY, writes are refused. Help: pass --read-only to say so, or \
                     make the store writable for this user."
                );
                open_store_read_only(path)
            }
            Err(StoreOpenError::NotWritable(message)) => Err(StartError::failed(message)),
            Err(StoreOpenError::Other(err)) => Err(err),
            Ok(opened) => Ok(opened),
        }
    }

    /// [`Self::open`] of a Store read-write.
    fn open_store(
        &self,
        path: &str,
        create: bool,
        on_damage: Option<graphersal::persist::DamagePolicy>,
    ) -> Result<Opened, StoreOpenError> {
        use graphersal::persist::{Store, StoreOptions};
        // A store that cannot be opened (in use, damaged, an older format, ...) is a failed
        // operation (exit 1), not an invalid invocation.
        let format = |err: graphersal::persist::PersistError| {
            StartError::failed(graphersal::error::format_diagnostic(&err))
        };
        let store = if Store::is_store(path).map_err(format)? {
            // A backup opens read-only (it is restored with `store restore`).
            match Store::backup_marker_dir(path) {
                Ok(Some(marker)) => {
                    let store = Store::open_backup(path, StoreOptions::new()).map_err(format)?;
                    errln!(
                        "note: {path} is a BACKUP (up to commit {}, taken {}): opened READ-ONLY, \
                         writes are refused. Help: `graphersal store restore {path}` makes it the \
                         live store; `graphersal store fork {path} <new_dir>` makes a writable \
                         copy.",
                        marker.commit_seq,
                        crate::store_cmd::format_time(marker.taken_at)
                    );
                    store
                }
                _ => {
                    let store =
                        Store::open(path, StoreOptions::new()).map_err(|err| match &err {
                            graphersal::persist::PersistError::Io { source, .. }
                                if matches!(
                                    source.kind(),
                                    io::ErrorKind::PermissionDenied
                                        | io::ErrorKind::ReadOnlyFilesystem
                                ) =>
                            {
                                StoreOpenError::NotWritable(source.to_string())
                            }
                            _ => StoreOpenError::Other(format(err)),
                        })?;
                    if let Some(wanted) = on_damage
                        && store.damage_policy().map_err(format)? != wanted
                    {
                        let have = store.damage_policy().map_err(format)?;
                        drop(store);
                        return Err(format!(
                            "Error: the store {path} exists with the damage policy \"{have}\"; \
                             --on-damage {wanted} applies only to a store being created\nHelp: \
                             the damage policy is fixed when a store is created and never \
                             changes (`graphersal store info {path}` shows it); drop \
                             --on-damage, or create a new store with it."
                        )
                        .into());
                    }
                    store
                }
            }
        } else if create {
            if let Some(refusal) = crate::store_cmd::create_refusal(path) {
                return Err(StartError::failed(refusal).into());
            }
            let policy = on_damage.unwrap_or_default();
            let store = Store::create(path, StoreOptions::new().with_damage_policy(policy))
                .map_err(format)?;
            errln!("note: created the store {path} (damage policy {policy})");
            store
        } else {
            return Err(format!(
                "Error: '{path}' is not a Graphersal store (no GRAPH file)\nHelp: create one with \
                 `graphersal store create {path}`, or add --create-store to create it now."
            )
            .into());
        };
        let report = store.open_report();
        if let Some(damage) = store.maintenance() {
            errln!(
                "warning: the store {path} has DAMAGE and opened READ-ONLY in maintenance mode: \
                 queries read the intact data, writes are refused.\n{damage}Help: repair it into \
                 a new directory with `graphersal store repair {path} --to <new_dir>` (the \
                 damaged files stay untouched), or restore a backup."
            );
        } else if !report.clean_close {
            errln!(
                "note: the store {path} was not closed cleanly last time; recovered to commit {} \
                 ({} commits replayed{})",
                report.commit_seq,
                report.commits_replayed,
                report
                    .torn_tail
                    .map(|t| format!(", an interrupted last write of {} bytes cut", t.bytes))
                    .unwrap_or_default()
            );
        }
        if let Some(note) = crate::store_cmd::graph_file_repaired_note(path, &store) {
            errln!("{note}");
        }
        for warning in &report.warnings {
            errln!("warning: {warning}");
        }
        register_stored_models(path, &store).map_err(format)?;
        Ok(Opened {
            graph: store.graph().clone(),
            store: Some(Arc::new(store)),
        })
    }
}

/// A parsed command line.
#[derive(Debug, Clone)]
pub struct Cli {
    /// What to do: run the REPL, evaluate scripts, or print help / version.
    pub mode: Mode,
    /// The graph the session runs against.
    pub graph: GraphSpec,
    /// `--schema` (with `--schema-mode`): the schema file applied to the `--graph`.
    pub schema: Option<SchemaSource>,
    /// Default visualizer for results that are not produced by a data terminal.
    pub format: VisualizeFormat,
    /// Scripts collected from `--eval` and from positional arguments, in order.
    pub scripts: Vec<String>,
    /// Whether a script is to be read from stdin.
    pub read_stdin: bool,
    /// `--repl`: run the REPL even when stdin is not a terminal.
    pub repl: bool,
    /// Default `evaluationTimeout` in milliseconds from `--timeout`; `None` means no timeout.
    pub timeout_ms: Option<u64>,
    /// `--max-rows`; `None` keeps [`DEFAULT_MAX_ROWS`].
    pub max_rows: Option<usize>,
    /// `--max-items`; `None` keeps [`DEFAULT_MAX_ITEMS`].
    pub max_items: Option<usize>,
    /// `--no-limit`: show displayed results completely.
    pub no_limit: bool,
    /// `--safe-limits`: run scripts under the SAFE [`ScriptLimits::default`] instead of none.
    pub safe_limits: bool,
    /// `--max-operations`; `0` means unlimited.
    pub max_operations: Option<u64>,
    /// `--max-string-size`; `0` means unlimited.
    pub max_string_size: Option<usize>,
    /// `--max-array-size`; `0` means unlimited.
    pub max_array_size: Option<usize>,
    /// `--max-map-size`; `0` means unlimited.
    pub max_map_size: Option<usize>,
    /// `--max-traversers`: the `traversal.max_traversers` default of the session.
    pub max_traversers: Option<u64>,
    /// `--max-value-depth`: `ScriptLimits::max_value_depth` and the `traversal.max_value_depth`
    /// default of the session; `0` means the default.
    pub max_value_depth: Option<usize>,
    /// `--spelling`: the spelling of step names in diagnostics (`render.spelling`).
    pub spelling: Spelling,
    /// `--memory-limit`: the `memory.limit` preset of the session; `None` (or `0`) is no budget.
    pub memory_limit: Option<MemoryLimit>,
    /// `--memory-headroom`: the `memory.headroom` preset of the session.
    pub memory_headroom: Option<u64>,
    /// `--port`: the port of the dev server ([`Mode::Server`]); `0` picks a free one.
    pub port: u16,
    /// `--host`: the address the dev server binds (an IP address or a host name); `None` is
    /// the loopback, or `0.0.0.0` inside a container.
    pub host: Option<String>,
    /// `--ui-dir`: a directory whose files the dev server serves over the embedded playground
    /// UI; `None` is [`DEFAULT_UI_DIR`] when it exists, else the embedded UI alone.
    pub ui_dir: Option<String>,
    /// `--create-store`: a `--graph <dir>` that is not a store yet is created.
    pub create_store: bool,
    /// `--on-damage`: the damage policy of a store `--create-store` creates.
    pub on_damage: Option<graphersal::persist::DamagePolicy>,
    /// `--read-only`: the graph refuses every write; a Store is loaded without its lock and
    /// nothing in it is written (`GraphSpec::open`).
    pub read_only: bool,
    /// `--mcp`:the dev server also serves the MCP endpoint `POST /mcp`.
    pub mcp: bool,
    /// `--mcp-read-only`: the MCP endpoint offers no writing tool and refuses writing queries.
    pub mcp_read_only: bool,
    /// `--mcp-token`: the bearer token every MCP request must carry.
    pub mcp_token: Option<String>,
    /// `--mcp-query-tools`: the MCP endpoint also lists every saved query as a tool.
    pub mcp_query_tools: bool,
    /// `--mcp-max-chars`: the largest MCP tool answer in characters (`None`: the default).
    pub mcp_max_chars: Option<usize>,
    /// `--ai`: the AI Chat configuration file of the dev server.
    pub ai: Option<String>,
    /// `--mcp-instructions`: the TEXT of the operator's file, appended to the MCP server
    /// instructions (read and checked while parsing).
    pub mcp_instructions: Option<String>,
    /// `--embedder NAME=PATH|REPO` (repeatable, in order): Model2Vec models to register.
    pub embedders: Vec<(String, String)>,
    /// `--default-embedder NAME`: the default of the registry.
    pub default_embedder: Option<String>,
}

/// Default port of `--server`.
pub const DEFAULT_PORT: u16 = 8080;
/// Default `--ui-dir`: the playground directory of a repository checkout, relative to the
/// current directory; used only when it exists (the embedded UI serves everything else).
#[cfg_attr(not(feature = "dev-server"), allow(dead_code))]
pub const DEFAULT_UI_DIR: &str = "playground";

/// A `--memory-limit` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLimit {
    /// An explicit budget in bytes.
    Bytes(u64),
    /// The cgroup memory limit minus the headroom (`"auto"`).
    Auto,
}

impl MemoryLimit {
    /// Parses a `--memory-limit` value: `auto`, or a byte count (`0` is no budget: `None`).
    pub fn parse(raw: &str) -> Result<Option<Self>, String> {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("auto") {
            return Ok(Some(Self::Auto));
        }
        match trimmed.parse::<u64>() {
            Ok(0) => Ok(None),
            Ok(bytes) => Ok(Some(Self::Bytes(bytes))),
            Err(_) => Err(format!(
                "Invalid --memory-limit value: {raw}. Expected a number of bytes or \"auto\" \
                 (0 means no budget)"
            )),
        }
    }

    /// The `memory.limit` option value.
    pub fn to_property(self) -> ElementProperty {
        match self {
            Self::Bytes(bytes) => ElementProperty::from(i64::try_from(bytes).unwrap_or(i64::MAX)),
            Self::Auto => ElementProperty::from("auto"),
        }
    }
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            mode: Mode::Repl,
            graph: GraphSpec::default(),
            schema: None,
            format: VisualizeFormat::Auto,
            scripts: vec![],
            read_stdin: false,
            repl: false,
            timeout_ms: None,
            max_rows: None,
            max_items: None,
            no_limit: false,
            safe_limits: false,
            max_operations: None,
            max_string_size: None,
            max_array_size: None,
            max_map_size: None,
            max_traversers: None,
            max_value_depth: None,
            spelling: Spelling::default(),
            memory_limit: None,
            memory_headroom: None,
            port: DEFAULT_PORT,
            host: None,
            ui_dir: None,
            create_store: false,
            read_only: false,
            on_damage: None,
            mcp: false,
            mcp_read_only: false,
            mcp_token: None,
            mcp_query_tools: false,
            mcp_max_chars: None,
            ai: None,
            mcp_instructions: None,
            embedders: Vec::new(),
            default_embedder: None,
        }
    }
}

impl Cli {
    /// Parses command-line arguments, excluding the program name.
    ///
    /// Long options accept both `--graph modern` and `--graph=modern`. A bare argument is taken
    /// as a script to evaluate, so `graphersal 'g.v().count().next()'` needs no flag at all.
    pub fn parse<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        // `--args-file FILE` / `@FILE` become ordinary arguments in front of the real ones.
        let args = crate::args_file::expand(args.into_iter().map(Into::into).collect())?;
        let mut cli = Cli::default();
        let mut server = false;
        let mut schema_path: Option<String> = None;
        let mut schema_mode: Option<SchemaMode> = None;
        // The first display option given: the playground UI sets those per run, so --server
        // rejects them instead of ignoring them.
        let mut display_flag: Option<String> = None;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => {
                    (flag.to_string(), Some(value.to_string()))
                }
                _ => (arg.clone(), None),
            };

            // Reads the value of an option either from its `--flag=value` form or from the
            // following argument.
            let value = |args: &mut std::vec::IntoIter<String>| match inline.clone() {
                Some(value) => Ok(value),
                None => args
                    .next()
                    .ok_or_else(|| format!("Option {flag} requires a value")),
            };

            if matches!(
                flag.as_str(),
                "-f" | "--format" | "--max-rows" | "--max-items" | "--no-limit" | "--spelling"
            ) && display_flag.is_none()
            {
                display_flag = Some(flag.clone());
            }

            match flag.as_str() {
                "-h" | "--help" => {
                    return Ok(Cli {
                        mode: Mode::Help,
                        ..cli
                    });
                }
                "-V" | "--version" => {
                    return Ok(Cli {
                        mode: Mode::Version,
                        ..cli
                    });
                }
                "-i" | "--in" | "-" => {
                    cli.read_stdin = true;
                    cli.mode = Mode::Eval;
                }
                "--repl" => cli.repl = true,
                "-e" | "--eval" => {
                    cli.scripts.push(value(&mut args)?);
                    cli.mode = Mode::Eval;
                }
                "-g" | "--graph" => cli.graph = GraphSpec::parse(&value(&mut args)?),
                "--server" => server = true,
                "--schema" => schema_path = Some(value(&mut args)?),
                "--schema-mode" => {
                    let raw = value(&mut args)?;
                    schema_mode = Some(SchemaMode::from_name(&raw).ok_or_else(|| {
                        format!(
                            "Invalid --schema-mode value: {raw}. Expected none, open or closed \
                             (lowercase)"
                        )
                    })?);
                }
                "--create-store" => cli.create_store = true,
                "--read-only" => cli.read_only = true,
                "--on-damage" => {
                    let raw = value(&mut args)?;
                    cli.on_damage = Some(
                        graphersal::persist::DamagePolicy::parse(&raw)
                            .map_err(|why| format!("Invalid --on-damage value: {why}"))?,
                    );
                }
                "--port" => {
                    let raw = value(&mut args)?;
                    cli.port = raw.trim().parse::<u16>().map_err(|_| {
                        format!(
                            "Invalid --port value: {raw}. Expected a port number from 0 to 65535 \
                             (0 picks a free port)"
                        )
                    })?;
                }
                "--host" => cli.host = Some(value(&mut args)?),
                "--ui-dir" => cli.ui_dir = Some(value(&mut args)?),
                "--mcp" => cli.mcp = true,
                "--mcp-read-only" => cli.mcp_read_only = true,
                "--mcp-query-tools" => cli.mcp_query_tools = true,
                "--mcp-max-chars" => {
                    let raw = value(&mut args)?;
                    let n = raw
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n >= MCP_MIN_CHARS)
                        .ok_or_else(|| {
                            format!(
                                "Invalid --mcp-max-chars value: {raw}. Expected a number of \
                                 characters, at least {MCP_MIN_CHARS} (default 60000)"
                            )
                        })?;
                    cli.mcp_max_chars = Some(n);
                }
                "--ai" => cli.ai = Some(value(&mut args)?),
                "--mcp-instructions" => {
                    cli.mcp_instructions = Some(read_mcp_instructions(&value(&mut args)?)?)
                }
                "--embedder" => {
                    let raw = value(&mut args)?;
                    match raw.split_once('=') {
                        Some((name, spec)) if !name.is_empty() && !spec.is_empty() => {
                            if cli.embedders.iter().any(|(n, _)| n == name) {
                                return Err(format!("--embedder {name} is given twice"));
                            }
                            cli.embedders.push((name.to_string(), spec.to_string()));
                        }
                        _ => {
                            return Err(format!(
                                "Invalid --embedder value: {raw}. Expected NAME=PATH or \
                                 NAME=REPO (for example potion=minishlab/potion-base-8M)"
                            ));
                        }
                    }
                }
                "--default-embedder" => cli.default_embedder = Some(value(&mut args)?),
                "--mcp-token" => {
                    let token = value(&mut args)?;
                    if token.trim().is_empty() || !token.chars().all(|c| c.is_ascii_graphic()) {
                        return Err(
                            "Invalid --mcp-token: use visible ASCII characters without spaces"
                                .to_string(),
                        );
                    }
                    cli.mcp_token = Some(token);
                }
                "-f" | "--format" => {
                    let name = value(&mut args)?;
                    cli.format = VisualizeFormat::from_name(&name).ok_or_else(|| {
                        format!(
                            "Unknown format: {name}. Available formats: {}",
                            VisualizeFormat::names().join(", ")
                        )
                    })?;
                }
                "-t" | "--timeout" => {
                    let raw = value(&mut args)?;
                    let ms = raw.trim().parse::<u64>().map_err(|_| {
                        format!(
                            "Invalid --timeout value: {raw}. Expected a non-negative integer \
                             number of milliseconds (0 disables the timeout)"
                        )
                    })?;
                    cli.timeout_ms = (ms > 0).then_some(ms);
                }
                "--max-rows" => cli.max_rows = Some(parse_limit("--max-rows", &value(&mut args)?)?),
                "--max-items" => {
                    cli.max_items = Some(parse_limit("--max-items", &value(&mut args)?)?)
                }
                "--no-limit" => cli.no_limit = true,
                "--spelling" => cli.spelling = parse_spelling(&value(&mut args)?)?,
                "--safe-limits" => cli.safe_limits = true,
                "--max-operations" => {
                    cli.max_operations = Some(parse_count("--max-operations", &value(&mut args)?)?)
                }
                "--max-string-size" => {
                    cli.max_string_size =
                        Some(parse_count("--max-string-size", &value(&mut args)?)?)
                }
                "--max-array-size" => {
                    cli.max_array_size = Some(parse_count("--max-array-size", &value(&mut args)?)?)
                }
                "--max-map-size" => {
                    cli.max_map_size = Some(parse_count("--max-map-size", &value(&mut args)?)?)
                }
                "--max-traversers" => {
                    cli.max_traversers = Some(parse_count("--max-traversers", &value(&mut args)?)?)
                }
                "--max-value-depth" => {
                    cli.max_value_depth =
                        Some(parse_count("--max-value-depth", &value(&mut args)?)?)
                }
                "--memory-limit" => cli.memory_limit = MemoryLimit::parse(&value(&mut args)?)?,
                "--memory-headroom" => {
                    let raw = value(&mut args)?;
                    cli.memory_headroom = Some(raw.trim().parse::<u64>().map_err(|_| {
                        format!(
                            "Invalid --memory-headroom value: {raw}. Expected a non-negative \
                             integer number of bytes"
                        )
                    })?)
                }
                other if other.starts_with('-') && other.len() > 1 => {
                    return Err(format!("Unknown option: {other}"));
                }
                _ => {
                    cli.scripts.push(arg);
                    cli.mode = Mode::Eval;
                }
            }
        }

        if cli.repl && (cli.mode != Mode::Repl || server) {
            return Err(
                "--repl runs the interactive shell: drop --eval, --in, the script argument and \
                 --server"
                    .to_string(),
            );
        }
        if cli.on_damage.is_some() && !cli.create_store {
            return Err(
                "--on-damage is a creation parameter: use it with --create-store (or `graphersal \
                 store create <dir> --on-damage <policy>`)"
                    .to_string(),
            );
        }
        if cli.read_only && cli.create_store {
            return Err(
                "--read-only and --create-store exclude each other: a read-only open writes \
                 nothing, so it cannot create the store"
                    .to_string(),
            );
        }
        if cli.create_store {
            match &cli.graph {
                GraphSpec::File(path) | GraphSpec::Store(path) => {
                    cli.graph = GraphSpec::Store(path.clone())
                }
                _ => {
                    return Err(
                        "--create-store needs --graph <dir>: the directory of the store to create"
                            .to_string(),
                    );
                }
            }
        }

        match schema_path {
            Some(path) => {
                match &cli.graph {
                    GraphSpec::Store(graph) => return Err(schema_refused(graph, "a Store")),
                    GraphSpec::File(graph) if GraphSource::is_snapshot_path(graph) => {
                        return Err(schema_refused(graph, "a packed snapshot"));
                    }
                    _ => {}
                }
                cli.schema = Some(SchemaSource {
                    path,
                    mode: schema_mode,
                });
            }
            None if schema_mode.is_some() => {
                return Err(
                    "--schema-mode needs --schema: it overrides the mode of the schema \
                            file"
                        .to_string(),
                );
            }
            None => {}
        }

        if server {
            if !cli.scripts.is_empty() || cli.read_stdin {
                return Err(
                    "--server serves the playground UI and evaluates no script: drop --eval, \
                     --in and the script argument (run the scripts in the UI instead)"
                        .to_string(),
                );
            }
            if let Some(flag) = display_flag {
                return Err(format!(
                    "{flag} does not apply to --server: the playground UI sets the display \
                     (format, row and item limits, spelling) per query. The limit options \
                     (--timeout, --memory-limit, --memory-headroom, --safe-limits, --max-operations, \
                     --max-traversers, ...) apply to every query of the server"
                ));
            }
            cli.mode = Mode::Server;
        } else if cli.host.is_some() {
            return Err(
                "--host needs --server: it is the address the dev server listens on \
                 (graphersal --graph <src> --server --host 0.0.0.0)"
                    .to_string(),
            );
        } else if cli.mcp {
            return Err(
                "--mcp needs --server: the MCP endpoint is part of the dev server \
                 (graphersal --graph <src> --server --mcp)"
                    .to_string(),
            );
        }
        if cli.ai.is_some() {
            if !server {
                return Err(
                    "--ai needs --server: the AI Chat is part of the dev server \
                     (graphersal --graph <src> --server --ai ai.json)"
                        .to_string(),
                );
            }
            if !cfg!(feature = "ai") {
                return Err(
                    "--ai is not available: this graphersal was built without the `ai` feature"
                        .to_string(),
                );
            }
        }
        if (cli.mcp_read_only
            || cli.mcp_token.is_some()
            || cli.mcp_query_tools
            || cli.mcp_instructions.is_some())
            && !cli.mcp
        {
            return Err(
                "--mcp-read-only, --mcp-token, --mcp-query-tools and --mcp-instructions need \
                 --mcp"
                    .to_string(),
            );
        }

        if cli.mcp_max_chars.is_some() && !cli.mcp {
            return Err("--mcp-max-chars needs --mcp".to_string());
        }

        if cli.no_limit && (cli.max_rows.is_some() || cli.max_items.is_some()) {
            return Err(
                "--no-limit removes every display limit and cannot be combined with \
                        --max-rows or --max-items"
                    .to_string(),
            );
        }

        Ok(cli)
    }

    /// The display limits of this session: 100 rows and 100 nested items unless the command
    /// line says otherwise.
    pub fn render_options(&self) -> RenderOptions {
        if self.no_limit {
            return RenderOptions::unlimited().with_spelling(self.spelling);
        }
        RenderOptions::new()
            .with_max_rows(self.max_rows.unwrap_or(DEFAULT_MAX_ROWS))
            .with_max_items(self.max_items.unwrap_or(DEFAULT_MAX_ITEMS))
            .with_spelling(self.spelling)
    }
}

/// The longest `--mcp-instructions` text, in characters.
pub const MAX_MCP_INSTRUCTIONS_CHARS: usize = 20_000;

/// The smallest `--mcp-max-chars`: room for a status line, some results and the notes.
pub const MCP_MIN_CHARS: usize = 1000;

/// Reads the `--mcp-instructions` file: UTF-8, not empty, at most
/// [`MAX_MCP_INSTRUCTIONS_CHARS`] characters; returns the trimmed text.
fn read_mcp_instructions(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|err| format!("--mcp-instructions {path}: cannot read the file ({err})"))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("--mcp-instructions {path}: the file is not UTF-8 text"))?;
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("--mcp-instructions {path}: the file is empty"));
    }
    let chars = text.chars().count();
    if chars > MAX_MCP_INSTRUCTIONS_CHARS {
        return Err(format!(
            "--mcp-instructions {path}: the text has {chars} characters, the limit is \
             {MAX_MCP_INSTRUCTIONS_CHARS}\nHelp: keep the operator notes short; the agent also \
             reads the schema and the saved queries of the graph itself"
        ));
    }
    Ok(text.to_string())
}

/// Parses a `--spelling` / `/set spelling` value: `snake` or `camel`.
pub fn parse_spelling(raw: &str) -> Result<Spelling, String> {
    Spelling::parse(raw.trim()).ok_or_else(|| {
        format!(
            "Invalid spelling: {raw}. Expected \"snake\" (has_label()) or \"camel\" (hasLabel())"
        )
    })
}

/// Parses a resource-limit flag value: a non-negative integer, `0` meaning unlimited.
fn parse_count<T: std::str::FromStr>(flag: &str, raw: &str) -> Result<T, String> {
    raw.trim().parse::<T>().map_err(|_| {
        format!("Invalid {flag} value: {raw}. Expected a non-negative integer (0 means unlimited)")
    })
}

/// Parses a display limit flag value: an integer of at least 1.
fn parse_limit(flag: &str, raw: &str) -> Result<usize, String> {
    match raw.trim().parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "Invalid {flag} value: {raw}. Expected a positive integer; use --no-limit to show \
             results completely"
        )),
    }
}

impl Cli {
    /// The script limits of this session. This is a trusted local front end, so the default is
    /// [`ScriptLimits::unlimited`]; `--safe-limits` switches to the SAFE defaults and every
    /// `--max-*` flag overrides its own field on top of either.
    pub fn script_limits(&self) -> ScriptLimits {
        let mut limits = if self.safe_limits {
            ScriptLimits::default()
        } else {
            ScriptLimits::unlimited()
        };
        if let Some(n) = self.max_operations {
            limits.max_operations = n;
        }
        if let Some(n) = self.max_string_size {
            limits.max_string_size = n;
        }
        if let Some(n) = self.max_array_size {
            limits.max_array_size = n;
        }
        if let Some(n) = self.max_map_size {
            limits.max_map_size = n;
        }
        if let Some(n) = self.max_value_depth {
            limits.max_value_depth = n;
        }
        limits
    }

    /// The traversal budget of the session: `--timeout`, `--max-traversers` and
    /// `--max-value-depth`.
    pub fn budget(&self) -> ScriptBudget {
        ScriptBudget::new(self.timeout_ms)
            .with_max_traversers(self.traversal_max_traversers())
            .with_max_value_depth(self.max_value_depth)
            .with_max_string_bytes(self.traversal_max_string_bytes())
            .with_memory_limit(self.memory_limit, self.memory_headroom)
    }

    /// The `traversal.max_string_bytes` default of the session, following the script's string
    /// limit: unlimited unless `--safe-limits` (then the engine default, 16 MiB, applies) or
    /// `--max-string-size` says otherwise (`0` is unlimited).
    pub fn traversal_max_string_bytes(&self) -> Option<i64> {
        match self.max_string_size {
            Some(0) => Some(i64::MAX),
            Some(n) => Some(i64::try_from(n).unwrap_or(i64::MAX)),
            None if self.safe_limits => None,
            None => Some(i64::MAX),
        }
    }

    /// The `traversal.max_traversers` default of the session: unlimited unless `--safe-limits`
    /// (then the engine default applies) or `--max-traversers` says otherwise (`0` is unlimited).
    pub fn traversal_max_traversers(&self) -> Option<i64> {
        match self.max_traversers {
            Some(0) => Some(i64::MAX),
            Some(n) => Some(i64::try_from(n).unwrap_or(i64::MAX)),
            None if self.safe_limits => None,
            None => Some(i64::MAX),
        }
    }
}

/// Renders a script evaluation failure as the text to show the user.
///
/// A traversal failure already carries a formatted diagnostic report, so it is returned verbatim
/// rather than nested in another error prefix; anything else is printed with its error chain.
pub fn format_eval_error(err: &rhai::EvalAltResult, limits: &ScriptLimits) -> String {
    if let Some(limit) = graphersal::script::resource_limit_from_rhai(err, limits) {
        return graphersal::error::format_diagnostic(&limit);
    }
    match err {
        rhai::EvalAltResult::ErrorRuntime(diagnostic, ..) if diagnostic.is_string() => {
            diagnostic.to_string()
        }
        // A script stopped by its time budget carries its diagnostic as the termination token.
        rhai::EvalAltResult::ErrorTerminated(diagnostic, ..) if diagnostic.is_string() => {
            diagnostic.to_string()
        }
        _ => {
            let mut buf = format!("Error: {err}");
            let mut current = std::error::Error::source(err);
            while let Some(source) = current {
                buf.push_str(&format!("\nCaused by: {source}"));
                current = source.source();
            }
            buf
        }
    }
}

/// Time budget of one script evaluation on a shared engine.
///
/// The traversals of a script enforce `evaluationTimeout` themselves. Rhai code between them,
/// such as a `loop {}`, is not a traversal, so the engine is also stopped from
/// [`Engine::on_progress`] once the script as a whole runs past the same budget.
#[derive(Clone, Debug, Default)]
pub struct ScriptBudget {
    limit: Option<Duration>,
    started: Arc<Mutex<Option<Instant>>>,
    max_traversers: Option<i64>,
    max_value_depth: Option<i64>,
    max_string_bytes: Option<i64>,
    memory_limit: Option<MemoryLimit>,
    memory_headroom: Option<u64>,
}

impl ScriptBudget {
    /// A budget of `limit_ms` milliseconds per script; `None` never stops a script.
    pub fn new(limit_ms: Option<u64>) -> Self {
        Self {
            limit: limit_ms.map(Duration::from_millis),
            started: Arc::new(Mutex::new(None)),
            max_traversers: None,
            max_value_depth: None,
            max_string_bytes: None,
            memory_limit: None,
            memory_headroom: None,
        }
    }

    /// Sets the session presets of `memory.limit` and `memory.headroom` (`None`: not set, so no
    /// budget and the default headroom).
    pub fn with_memory_limit(mut self, limit: Option<MemoryLimit>, headroom: Option<u64>) -> Self {
        self.memory_limit = limit;
        self.memory_headroom = headroom;
        self
    }

    /// Sets the session default of `traversal.max_traversers` (`None` keeps the engine default).
    pub fn with_max_traversers(mut self, max: Option<i64>) -> Self {
        self.max_traversers = max;
        self
    }

    /// Sets the session default of `traversal.max_string_bytes` (`None` keeps the engine
    /// default).
    pub fn with_max_string_bytes(mut self, max: Option<i64>) -> Self {
        self.max_string_bytes = max;
        self
    }

    /// Sets the session default of `traversal.max_value_depth` from `--max-value-depth` (`None`
    /// or `0` keeps the engine default).
    pub fn with_max_value_depth(mut self, max: Option<usize>) -> Self {
        self.max_value_depth = max
            .filter(|n| *n > 0)
            .map(|n| i64::try_from(n).unwrap_or(i64::MAX));
        self
    }

    /// Default traversal options that carry the budget into every traversal source of a session:
    /// the scope's `g`, `_g` and `__` and the sources a script creates (`GraphSource::x()`).
    pub fn traversal_options(&self) -> Vec<(&'static str, ElementProperty)> {
        let mut options = Vec::new();
        if let Some(limit) = self.limit {
            options.push((
                "evaluationTimeout",
                ElementProperty::from(i64::try_from(limit.as_millis()).unwrap_or(i64::MAX)),
            ));
        }
        if let Some(max) = self.max_traversers {
            options.push(("traversal.max_traversers", ElementProperty::from(max)));
        }
        if let Some(max) = self.max_value_depth {
            options.push(("traversal.max_value_depth", ElementProperty::from(max)));
        }
        if let Some(max) = self.max_string_bytes {
            options.push(("traversal.max_string_bytes", ElementProperty::from(max)));
        }
        if let Some(limit) = self.memory_limit {
            options.push(("memory.limit", limit.to_property()));
        }
        if let Some(headroom) = self.memory_headroom {
            options.push((
                "memory.headroom",
                ElementProperty::from(i64::try_from(headroom).unwrap_or(i64::MAX)),
            ));
        }
        options
    }

    /// Builds the engine of a session under `limits`, with the budget's options preset on the
    /// traversal sources a script creates itself and the budget installed.
    pub fn engine(&self, limits: &ScriptLimits) -> Engine {
        let mut engine = graphersal::script::engine_with_options(
            limits,
            authorizer(),
            &self.traversal_options(),
        );
        graphersal::script::set_engine_embedders(&mut engine, embedders());
        self.install(&mut engine);
        engine
    }

    /// Builds the script scope over `graph` (with [`authorizer`]) with the budget as the default
    /// traversal timeout and
    /// `render` as the session display limits.
    pub fn scope(&self, graph: Arc<Graph>, render: &RenderOptions) -> Scope<'static> {
        let mut options = self.traversal_options();
        options.extend(graphersal::script::render_scope_options(render));
        let mut scope = graphersal::script::graph_scope_with_options(graph, &options, authorizer());
        graphersal::script::set_scope_embedders(&mut scope, embedders());
        scope
    }

    /// Replaces the traversal sources `g`, `_g` and `__` of a session scope by fresh ones carrying
    /// the budget's current options (after a REPL `/set` changed one); every other variable of
    /// the scope stays.
    pub fn refresh_scope(
        &self,
        scope: &mut Scope<'static>,
        graph: Arc<Graph>,
        render: &RenderOptions,
    ) {
        let fresh = self.scope(graph, render);
        for name in ["g", "_g", "__"] {
            if let Some(source) = fresh.get_value::<Dynamic>(name) {
                scope.set_or_push(name, source);
            }
        }
    }

    /// Stops `engine` once a script runs past the budget, checked every 1024 operations.
    pub fn install(&self, engine: &mut Engine) {
        let Some(limit) = self.limit else {
            return;
        };
        let started = self.started.clone();
        engine.on_progress(move |ops| {
            if ops & 1023 != 0 {
                return None;
            }
            let start = (*started.lock().ok()?)?;
            let elapsed = start.elapsed();
            (elapsed >= limit).then(|| Dynamic::from(script_timeout_diagnostic(limit, elapsed)))
        });
    }

    /// Starts the clock of the next script evaluation.
    pub fn start(&self) {
        if let Ok(mut started) = self.started.lock() {
            *started = Some(Instant::now());
        }
    }
}

/// Diagnostic of a script stopped by its budget outside of any traversal.
fn script_timeout_diagnostic(limit: Duration, elapsed: Duration) -> String {
    format!(
        "Error: Script exceeded evaluationTimeout of {} ms (ran {} ms)\n\
         Help: The script itself ran past the budget between traversals, for example in a loop.\n      \
         Bound the loop, or raise the budget with --timeout <ms>.",
        limit.as_millis(),
        elapsed.as_millis()
    )
}

/// Evaluates the scripts of a parsed command line and returns the process exit code.
///
/// Results go to `stdout`, one per script and only when the script produced a value; diagnostics
/// go to `stderr`. Evaluation stops at the first failing script.
pub fn run_eval(cli: &Cli) -> i32 {
    let mut scripts = cli.scripts.clone();

    if cli.read_stdin {
        let mut buf = String::new();
        if let Err(err) = io::stdin().read_to_string(&mut buf) {
            errln!("Error: cannot read script from stdin: {err}");
            return EXIT_USAGE_ERROR;
        }
        if !buf.trim().is_empty() {
            scripts.push(buf);
        }
    }

    if scripts.is_empty() {
        return EXIT_OK;
    }

    let opened = match cli.graph.open(
        cli.create_store,
        cli.on_damage,
        cli.schema.as_ref(),
        cli.read_only,
    ) {
        Ok(opened) => opened,
        Err(err) => {
            errln!("{}", err.message);
            return err.code;
        }
    };
    opened.close_on_signal(None, crate::signals::EXIT_INTERRUPTED);
    let code = eval_scripts(cli, scripts, opened.graph.clone());
    opened.close();
    code
}

/// Evaluates `scripts` against `graph`; returns the exit code.
fn eval_scripts(cli: &Cli, scripts: Vec<String>, graph: Arc<Graph>) -> i32 {
    let limits = cli.script_limits();
    let budget = cli.budget();
    let engine = budget.engine(&limits);
    let render = cli.render_options();
    let mut scope = budget.scope(graph, &render);

    for script in scripts {
        // Nobody reads the results any more (`graphersal -e .. | head -1`): stop quietly; the
        // caller's exit code says so (`out::EXIT_CLOSED_PIPE`).
        if !crate::out::stdout_open() {
            break;
        }
        budget.start();
        match engine.eval_with_scope::<Dynamic>(&mut scope, &script) {
            // A traversal left without a terminal executes while it is rendered; a failure there
            // is a failed script too (stderr, exit 1), not a result.
            Ok(value) => match graphersal::script::try_render_value(value, cli.format, &render) {
                Ok(rendered) => {
                    if !rendered.text.is_empty() {
                        outln!("{}", rendered.text);
                    }
                    // The notice is not part of the result: stdout stays clean for pipes.
                    if let Some(notice) = rendered.notice() {
                        errln!("{notice}");
                    }
                }
                Err(diagnostic) => {
                    errln!("{diagnostic}");
                    return EXIT_QUERY_ERROR;
                }
            },
            Err(err) => {
                let err = graphersal::script::map_eval_error(err, &limits);
                errln!("{}", format_eval_error(&err, &limits));
                return EXIT_QUERY_ERROR;
            }
        }
    }

    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_dev_server_options() {
        let cli = Cli::parse([
            "--graph", "large", "--server", "--port", "0", "--ui-dir", "ui",
        ])
        .unwrap();
        assert_eq!(cli.mode, Mode::Server);
        assert_eq!(cli.port, 0);
        assert_eq!(cli.ui_dir.as_deref(), Some("ui"));
        assert_eq!(Cli::parse(["--server"]).unwrap().port, DEFAULT_PORT);
        assert!(Cli::parse(["--port", "70000"]).is_err());
        let err = Cli::parse(["--server", "-e", "1"]).unwrap_err();
        assert!(err.contains("--server"), "{err}");

        let cli = Cli::parse([
            "--server",
            "--mcp",
            "--mcp-read-only",
            "--mcp-token",
            "s3cret",
        ])
        .unwrap();
        assert!(cli.mcp && cli.mcp_read_only);
        assert_eq!(cli.mcp_token.as_deref(), Some("s3cret"));
        assert!(Cli::parse(["--mcp"]).unwrap_err().contains("--server"));
        assert!(
            Cli::parse(["--server", "--mcp-read-only"])
                .unwrap_err()
                .contains("--mcp")
        );
        assert!(Cli::parse(["--server", "--mcp", "--mcp-token", "a b"]).is_err());
        assert!(
            Cli::parse(["--ai", "ai.json"])
                .unwrap_err()
                .contains("--server")
        );
        assert!(Cli::parse(["--server", "--ai"]).is_err());
        if cfg!(feature = "ai") {
            let cli = Cli::parse(["--server", "--ai=ai.json"]).unwrap();
            assert_eq!(cli.ai.as_deref(), Some("ai.json"));
            assert!(!cli.mcp, "--ai does not need --mcp");
        }
        assert!(
            Cli::parse(["--server", "--mcp", "--mcp-query-tools"])
                .unwrap()
                .mcp_query_tools
        );
        assert!(
            Cli::parse(["--server", "--mcp-query-tools"])
                .unwrap_err()
                .contains("--mcp")
        );
    }

    #[test]
    fn parses_defaults() {
        let cli = Cli::parse(Vec::<String>::new()).unwrap();
        assert_eq!(cli.mode, Mode::Repl);
        assert_eq!(cli.graph, GraphSpec::Modern);
        assert!(!cli.read_stdin);
    }

    #[test]
    fn parses_the_schema_flags() {
        let cli = Cli::parse([
            "--graph",
            "g.graphml",
            "--schema=s.json",
            "--schema-mode",
            "closed",
        ])
        .unwrap();
        assert_eq!(
            cli.schema,
            Some(SchemaSource {
                path: "s.json".to_string(),
                mode: Some(SchemaMode::Closed),
            })
        );
        assert_eq!(
            Cli::parse(["--schema", "s.json"])
                .unwrap()
                .schema
                .unwrap()
                .mode,
            None
        );
        assert!(Cli::parse(["--schema-mode", "open"]).is_err());
        assert!(Cli::parse(["--schema", "s.json", "--schema-mode", "Open"]).is_err());
        assert!(Cli::parse(["--graph", "x.gsnap", "--schema", "s.json"]).is_err());
        assert!(Cli::parse(["--graph", "dir/", "--schema", "s.json"]).is_err());
    }

    #[test]
    fn parses_stdin_and_graph() {
        let cli = Cli::parse(["--in", "--graph", "modern"]).unwrap();
        assert_eq!(cli.mode, Mode::Eval);
        assert!(cli.read_stdin);
        assert_eq!(cli.graph, GraphSpec::Modern);
    }

    #[test]
    fn parses_inline_option_values() {
        let cli = Cli::parse(["--graph=./g.graphml", "--format=json"]).unwrap();
        assert_eq!(cli.graph, GraphSpec::File("./g.graphml".to_string()));
        assert_eq!(cli.format, VisualizeFormat::Json);
    }

    #[test]
    fn collects_scripts_in_order() {
        let cli = Cli::parse(["-e", "first", "second"]).unwrap();
        assert_eq!(cli.mode, Mode::Eval);
        assert_eq!(cli.scripts, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn rejects_unknown_options_and_missing_values() {
        assert!(Cli::parse(["--nope"]).is_err());
        assert!(Cli::parse(["--graph"]).is_err());
    }

    #[test]
    fn parses_timeout() {
        assert_eq!(
            Cli::parse(["--timeout", "50"]).unwrap().timeout_ms,
            Some(50)
        );
        assert_eq!(Cli::parse(["--timeout=50"]).unwrap().timeout_ms, Some(50));
        assert_eq!(Cli::parse(["-t", "0"]).unwrap().timeout_ms, None);
        assert_eq!(Cli::parse(Vec::<String>::new()).unwrap().timeout_ms, None);
        assert!(Cli::parse(["--timeout", "-1"]).is_err());
        assert!(Cli::parse(["--timeout", "abc"]).is_err());
        assert!(Cli::parse(["--timeout"]).is_err());
    }

    #[test]
    fn parses_display_limits() {
        let cli = Cli::parse(Vec::<String>::new()).unwrap();
        assert_eq!(cli.render_options().max_rows, Some(DEFAULT_MAX_ROWS));
        assert_eq!(cli.render_options().max_items, Some(DEFAULT_MAX_ITEMS));
        let cli = Cli::parse(["--max-rows", "5", "--max-items=7"]).unwrap();
        assert_eq!(cli.render_options().max_rows, Some(5));
        assert_eq!(cli.render_options().max_items, Some(7));
        assert!(
            !Cli::parse(["--no-limit"])
                .unwrap()
                .render_options()
                .is_bounded()
        );
        assert!(Cli::parse(["--no-limit", "--max-rows", "5"]).is_err());
        assert!(Cli::parse(["--max-items", "3", "--no-limit"]).is_err());
        assert!(Cli::parse(["--max-rows", "0"]).is_err());
        assert!(Cli::parse(["--max-rows", "x"]).is_err());
    }

    #[test]
    fn rejects_unknown_formats() {
        assert!(Cli::parse(["--format", "yaml"]).is_err());
    }

    #[test]
    fn unknown_graph_names_are_file_paths() {
        assert_eq!(
            GraphSpec::parse("some_graph_from_disk.graphml"),
            GraphSpec::File("some_graph_from_disk.graphml".to_string())
        );
    }

    #[test]
    fn parses_the_embedder_flags() {
        let cli = Cli::parse([
            "--embedder",
            "a=minishlab/potion-base-8M",
            "--embedder=b=./models/x=y",
            "--default-embedder",
            "a",
            "--server",
        ])
        .unwrap();
        assert_eq!(
            cli.embedders,
            vec![
                ("a".to_string(), "minishlab/potion-base-8M".to_string()),
                ("b".to_string(), "./models/x=y".to_string()),
            ]
        );
        assert_eq!(cli.default_embedder.as_deref(), Some("a"));
        assert_eq!(cli.mode, Mode::Server);
        assert!(Cli::parse(["--embedder", "a"]).is_err());
        assert!(Cli::parse(["--embedder", "a="]).is_err());
        assert!(Cli::parse(["--embedder", "a=x", "--embedder", "a=y"]).is_err());
        assert!(Cli::parse(["--embedder"]).is_err());
        let plain = Cli::parse(["-e", "1"]).unwrap();
        assert!(plain.embedders.is_empty() && plain.default_embedder.is_none());
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("graphersal-cli-args-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_args_file_is_merged_before_the_command_line() {
        let dir = scratch("merge");
        let file = dir.join("a.json");
        std::fs::write(
            &file,
            r#"{"_c": "comment", "graph": "empty", "server": true, "port": 8080,
                "mcp": true, "embedder": ["a=./m"], "schema-mode": "open"}"#,
        )
        .unwrap();
        let path = file.to_str().unwrap();
        let cli = Cli::parse(["--args-file", path]).unwrap_err();
        assert!(cli.contains("--schema-mode needs --schema"), "{cli}");

        std::fs::write(
            &file,
            r#"{"graph": "empty", "server": true, "port": 8080, "mcp": true,
                "embedder": ["a=./m"], "ui-dir": "ui"}"#,
        )
        .unwrap();
        let cli = Cli::parse([
            format!("@{path}"),
            "--port".into(),
            "0".into(),
            "--embedder".into(),
            "b=repo/x".into(),
        ])
        .unwrap();
        assert_eq!(cli.mode, Mode::Server);
        assert_eq!(cli.graph, GraphSpec::Empty);
        assert_eq!(cli.port, 0, "the command line wins");
        assert!(cli.mcp);
        assert_eq!(
            cli.ui_dir.as_deref(),
            Some(dir.join("ui").to_str().unwrap())
        );
        assert_eq!(cli.embedders.len(), 2);
        assert_eq!(cli.embedders[0].0, "a");
        assert_eq!(cli.embedders[1], ("b".to_string(), "repo/x".to_string()));

        // the existing validation runs on the merged list
        let err = Cli::parse([format!("--args-file={path}"), "-e".into(), "1".into()]).unwrap_err();
        assert!(err.contains("--server serves the playground UI"), "{err}");
        let err = Cli::parse(["--args-file", path, "--args-file", path]).unwrap_err();
        assert!(err.contains("only once"), "{err}");
        let err = Cli::parse(["--args-file", "/nonexistent/x.json"]).unwrap_err();
        assert!(err.contains("cannot read"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_example_args_file_parses() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let path = dir.join("args-dev-server.json");
        let cli = Cli::parse([
            "--args-file".to_string(),
            path.to_str().unwrap().to_string(),
        ])
        .unwrap();
        assert_eq!(cli.mode, Mode::Server);
        assert!(cli.mcp && cli.mcp_read_only);
        assert_eq!(cli.timeout_ms, Some(30000));
        assert!(
            cli.mcp_instructions
                .as_deref()
                .is_some_and(|text| text.contains("TinkerPop"))
        );
    }

    #[test]
    fn mcp_instructions_are_checked() {
        let dir = scratch("notes");
        let notes = dir.join("n.md");
        let path = notes.to_str().unwrap();
        std::fs::write(&notes, "  Be kind.\n").unwrap();
        let cli = Cli::parse(["--server", "--mcp", "--mcp-instructions", path]).unwrap();
        assert_eq!(cli.mcp_instructions.as_deref(), Some("Be kind."));
        let err = Cli::parse(["--server", "--mcp-instructions", path]).unwrap_err();
        assert!(err.contains("need --mcp"), "{err}");
        std::fs::write(&notes, "  \n").unwrap();
        assert!(
            Cli::parse(["--server", "--mcp", "--mcp-instructions", path])
                .unwrap_err()
                .contains("empty")
        );
        std::fs::write(&notes, [0xff, 0xfe]).unwrap();
        assert!(
            Cli::parse(["--server", "--mcp", "--mcp-instructions", path])
                .unwrap_err()
                .contains("UTF-8")
        );
        std::fs::write(&notes, "x".repeat(MAX_MCP_INSTRUCTIONS_CHARS)).unwrap();
        assert!(Cli::parse(["--server", "--mcp", "--mcp-instructions", path]).is_ok());
        std::fs::write(&notes, "x".repeat(MAX_MCP_INSTRUCTIONS_CHARS + 1)).unwrap();
        let err = Cli::parse(["--server", "--mcp", "--mcp-instructions", path]).unwrap_err();
        assert!(
            err.contains("--mcp-instructions") && err.contains("limit"),
            "{err}"
        );
        let missing =
            Cli::parse(["--server", "--mcp", "--mcp-instructions", "/nonexistent/n"]).unwrap_err();
        assert!(missing.contains("cannot read"), "{missing}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
