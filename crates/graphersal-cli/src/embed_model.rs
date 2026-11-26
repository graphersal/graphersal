//! `graphersal embed-model download|import|list` (feature `model2vec`; book pages
//! "Embeddings", "Embedding Models in a Store" and "Command line"). The download cache is the
//! library's [`ModelCache`]; a model serves a database only once it is imported into it.
//!
//! The download itself is the library's ([`ModelCache::download_staged`], `graphersal` feature
//! `model-download`, so a host downloads without this crate): this module reads
//! `GRAPHERSAL_HF_ENDPOINT` and `HF_TOKEN`, prints the progress, the notes and the licence
//! warnings, and installs the model into the models directory or imports it into a Store.
//! graphersal-memory keeps an older COPY of the download (`src/download.rs`) from before the
//! move: it can switch to the library once a release carries it.
//!
//! The only network access of the binary besides the AI Chat: `download` fetches the three
//! files of a Model2Vec model (`config.json`, `tokenizer.json`, `model.safetensors`) from
//! `<endpoint>/<repo>/resolve/<revision>/<file>` (endpoint `https://huggingface.co`, or
//! `GRAPHERSAL_HF_ENDPOINT` for a mirror or a test; `HF_TOKEN` as a bearer token, sent to that
//! endpoint's host only, never logged), with the model card, the licence files and the
//! repository's metadata (format spec 4.5); a failure (or a kill) leaves no model behind. With
//! `--into <store>` the model goes into that Store (one blob and one catalog definition)
//! instead of the models directory; `import` puts a downloaded model or a local model directory
//! into a Store.

use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use graphersal::persist::{
    DownloadEvent, ImportModelOptions, ModelCache, ModelDownload, ModelDownloadError, ModelInfo,
    ModelSource, Store, model_licence_warnings,
};
use serde_json::Value;

use crate::cli::{EXIT_OK, EXIT_QUERY_ERROR, EXIT_USAGE_ERROR};

/// The files a Model2Vec model in the Model2Vec layout consists of, in download order.
pub const MODEL_FILES: [&str; 3] = ModelDownload::MODEL_FILES;

/// Usage of `graphersal embed-model`.
pub const EMBED_MODEL_HELP: &str = r#"graphersal embed-model - download and list Model2Vec embedding models

USAGE:
    graphersal embed-model download <REPO> [--revision <REV>] [--dir <MODELS-DIR>]
                                           [--into <STORE>]
    graphersal embed-model import <REPO|MODEL-DIR> --into <STORE> [--dir <MODELS-DIR>]
                                           [--repo <REPO>] [--license <SPDX>]
                                           [--replace] [--default]
    graphersal embed-model list [--dir <MODELS-DIR>]

COMMANDS:
    download   Fetch config.json, tokenizer.json and model.safetensors of the Hugging Face
               repository REPO (for example minishlab/potion-base-8M) into
               <MODELS-DIR>/<REPO>/, with its model card (README.md), its LICENSE*/NOTICE*
               files and a manifest graphersal-embed-model.json (repository, revision,
               commit, licence, base models, gated, files with sizes and SHA-256). The SHA-256
               Hugging Face states for a large file is checked; nothing is left behind when the
               download fails. --revision: a branch, tag or commit (default main).
               --into STORE: put the model into that Store (a closed store directory or
               .gstore file) instead of the models directory; the store's models serve its
               queries whenever the store is opened, and every copy of the store carries them.
    import     Put a Model2Vec model into the Store STORE, with its README.md and
               LICENSE*/NOTICE* files: REPO, a repository downloaded into the models directory
               (for example minishlab/potion-base-8M, no network), else MODEL-DIR, any
               directory with config.json, tokenizer.json and model.safetensors (write ./REPO
               for a directory that looks like a repository name). --repo names its Hugging Face
               repository (part of the model identity) when MODEL-DIR has no manifest;
               --license its SPDX licence id when the manifest does not say.
               --replace: a new revision of a repository the store holds replaces the current
               one (vector rules naming the old revision are re-embedded: the store is then
               opened, its graph loaded, and the import says so). --default: the model becomes
               the store's default (the one embed() uses without a name; also
               `graphersal store models default <STORE> <REPO>`).
    list       The downloaded models: repository, size, licence and model identity.

    The models directory is --dir, else $GRAPHERSAL_EMBED_MODELS, else the user cache:
    $XDG_CACHE_HOME/graphersal/embed-models or ~/.cache/graphersal/embed-models
    (%LOCALAPPDATA%\graphersal\embed-models on Windows).

    A model serves a database once it is imported into it (download once, import into
    any number of stores without the network); queries name it by its repository:
        graphersal embed-model import minishlab/potion-base-8M --into data/ --default
        graphersal --graph data/ -e 'g.v().values("name").embed().to_list()'

NETWORK:
    download is the only command that connects anywhere: to https://huggingface.co (or
    $GRAPHERSAL_HF_ENDPOINT); $HF_TOKEN, when set, is sent as a bearer token for gated or
    private repositories. Loading and running a model never uses the network.

LICENCES:
    A model's licence is the model's own. The licence (from the repository's metadata, else
    unknown), its text (the repository's licence file; else, for MIT and Apache-2.0, the
    unchanged standard text of the licence the model card names) and the model card stay
    with the model, also inside a Store. A store handed on (a backup, a .gsnap, a copied
    store) redistributes its models: `graphersal store info <STORE>` shows them and their
    licences; `store export --without-models` and `store backup --zip --without-models` leave
    them out. download and import warn when the licence is unknown or non-commercial, or the
    repository is gated.

EXIT CODES:
    0  done
    1  the download or a model failed
    2  invalid invocation
"#;

/// Runs `graphersal embed-model <args>`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let Some(command) = args.first() else {
        out!("{EMBED_MODEL_HELP}");
        return EXIT_USAGE_ERROR;
    };
    let rest = &args[1..];
    if matches!(command.as_str(), "help" | "--help" | "-h")
        || rest.iter().any(|a| a == "--help" || a == "-h")
    {
        out!("{EMBED_MODEL_HELP}");
        return EXIT_OK;
    }
    let parsed = match parse_flags(
        rest,
        &["revision", "dir", "into", "repo", "license"],
        &["replace", "default"],
    ) {
        Ok(parsed) => parsed,
        Err(message) => return usage(&message),
    };
    match command.as_str() {
        "download" => {
            let [repo] = parsed.positional.as_slice() else {
                return usage("embed-model download takes one repository (owner/name)");
            };
            let dir = match models_dir(parsed.flag("dir")) {
                Ok(dir) => dir,
                Err(message) => return usage(&message),
            };
            if parsed.flag("repo").is_some()
                || parsed.flag("license").is_some()
                || parsed.switch("replace")
                || parsed.switch("default")
            {
                return usage(
                    "--repo, --license, --replace and --default belong to `embed-model import`",
                );
            }
            let revision = parsed.flag("revision").unwrap_or("main");
            match download(repo, revision, &dir, parsed.flag("into")) {
                Ok(()) => EXIT_OK,
                Err(DownloadError::Usage(message)) => usage(&message),
                Err(DownloadError::Failed(message)) => {
                    errln!("Error: {message}");
                    EXIT_QUERY_ERROR
                }
                Err(DownloadError::Diagnostic(text)) => {
                    errln!("{text}");
                    EXIT_QUERY_ERROR
                }
            }
        }
        "import" => {
            let [model] = parsed.positional.as_slice() else {
                return usage(
                    "embed-model import takes one model: a downloaded repository or a directory",
                );
            };
            let Some(store) = parsed.flag("into") else {
                return usage(
                    "embed-model import needs --into <STORE>: the Store the model goes into",
                );
            };
            if parsed.flag("revision").is_some() {
                return usage("--revision belongs to `embed-model download`");
            }
            let dir = match import_source(model, parsed.flag("dir")) {
                Ok(dir) => dir,
                Err(message) => return usage(&message),
            };
            let options = ImportFlags {
                repo: parsed.flag("repo"),
                license: parsed.flag("license"),
                replace: parsed.switch("replace"),
                default: parsed.switch("default"),
            };
            match import(&dir, store, &options) {
                Ok(()) => EXIT_OK,
                Err(DownloadError::Usage(message)) => usage(&message),
                Err(DownloadError::Failed(message)) => {
                    errln!("Error: {message}");
                    EXIT_QUERY_ERROR
                }
                Err(DownloadError::Diagnostic(text)) => {
                    errln!("{text}");
                    EXIT_QUERY_ERROR
                }
            }
        }
        "list" => {
            if parsed.flag("revision").is_some()
                || parsed.flag("into").is_some()
                || parsed.flag("repo").is_some()
                || parsed.flag("license").is_some()
                || parsed.switch("replace")
                || parsed.switch("default")
                || !parsed.positional.is_empty()
            {
                return usage("embed-model list takes only --dir");
            }
            match models_dir(parsed.flag("dir")) {
                Ok(dir) => list(&dir),
                Err(message) => usage(&message),
            }
        }
        other => usage(&format!("Unknown embed-model command: {other}")),
    }
}

fn usage(message: &str) -> i32 {
    errln!(
        "{}",
        crate::out::usage_text(message, "graphersal embed-model help")
    );
    EXIT_USAGE_ERROR
}

/// Positional arguments and `--name value` / `--name=value` flags.
struct Parsed {
    positional: Vec<String>,
    flags: Vec<(String, String)>,
    switches: Vec<String>,
}

impl Parsed {
    fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

fn parse_flags(args: &[String], known: &[&str], switches: &[&str]) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        positional: Vec::new(),
        flags: Vec::new(),
        switches: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(flag) = arg.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (flag, None),
            };
            if switches.contains(&name) && inline.is_none() {
                parsed.switches.push(name.to_string());
                continue;
            }
            if !known.contains(&name) {
                return Err(format!("Unknown option: --{name}"));
            }
            let value = match inline {
                Some(value) => value,
                None => args
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("Option --{name} requires a value"))?,
            };
            parsed.flags.push((name.to_string(), value));
        } else {
            parsed.positional.push(arg.clone());
        }
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------------------------
// The models directory (the library's download cache)
// ---------------------------------------------------------------------------------------------

/// A non-empty environment variable.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The models directory: `explicit` (`--dir`), else the user's download cache
/// ([`ModelCache::new`]: `$GRAPHERSAL_EMBED_MODELS`, else the user cache).
pub fn models_dir(explicit: Option<&str>) -> Result<PathBuf, String> {
    match explicit {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => ModelCache::new()
            .map(|cache| cache.dir().to_path_buf())
            .map_err(|_| {
                "No models directory: set GRAPHERSAL_EMBED_MODELS (or pass --dir); neither HOME \
                 nor XDG_CACHE_HOME is set"
                    .to_string()
            }),
    }
}

// ---------------------------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------------------------

/// A downloaded model found under the models directory.
struct Listed {
    repo: String,
    bytes: u64,
    license: String,
    identity: Result<String, String>,
}

/// The model directories under `dir` (at most two levels: `name` and `owner/name`) that hold a
/// manifest; hidden directories (staging) are skipped.
fn find_models(dir: &Path) -> Vec<Listed> {
    let mut found = Vec::new();
    let visible_dirs = |dir: &Path| -> Vec<(String, PathBuf)> {
        let mut dirs: Vec<(String, PathBuf)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                (!name.starts_with('.')).then(|| (name, entry.path()))
            })
            .collect();
        dirs.sort();
        dirs
    };
    let mut visit = |repo: String, path: PathBuf| {
        let manifest = path.join(ModelCache::MANIFEST_FILE);
        let Ok(text) = fs::read_to_string(&manifest) else {
            return false;
        };
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let repo = value
            .get("repo")
            .and_then(Value::as_str)
            .map_or(repo, str::to_string);
        let bytes = MODEL_FILES
            .iter()
            .filter_map(|file| fs::metadata(path.join(file)).ok())
            .map(|meta| meta.len())
            .sum();
        let license = value
            .get("license")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let identity = ModelCache::identity(&path).map_err(|err| reason_of(&err));
        found.push(Listed {
            repo,
            bytes,
            license,
            identity,
        });
        true
    };
    for (first, path) in visible_dirs(dir) {
        if visit(first.clone(), path.clone()) {
            continue;
        }
        for (second, inner) in visible_dirs(&path) {
            visit(format!("{first}/{second}"), inner);
        }
    }
    found
}

fn list(dir: &Path) -> i32 {
    let models = find_models(dir);
    if models.is_empty() {
        crate::out::print_note(
            "note",
            &crate::out::Note::new(format!("No downloaded models in {}", dir.display()))
                .command(
                    "download one",
                    "graphersal embed-model download <owner/name>",
                )
                .text(),
        );
        return EXIT_OK;
    }
    // One section per model, one field per line (as `graphersal store models`).
    let sections: Vec<crate::sections::Section> = models
        .iter()
        .map(|model| {
            let mut section = crate::sections::Section::new("Downloaded model");
            section.row("model", model.repo.clone());
            section.row(
                "full name",
                match &model.identity {
                    Ok(identity) => identity.clone(),
                    Err(err) => format!("(unreadable: {err})"),
                },
            );
            section.row("size", human_bytes(model.bytes));
            section.row("licence", model.license.clone());
            section
        })
        .collect();
    outln!("{}", crate::sections::render(&sections));
    EXIT_OK
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------------------------
// download
// ---------------------------------------------------------------------------------------------

enum DownloadError {
    /// Invalid arguments (exit 2).
    Usage(String),
    /// The download failed (exit 1).
    Failed(String),
    /// The operation failed with a library error, already laid out by
    /// `graphersal::error::format_diagnostic` (its `Error: ` headline included; exit 1).
    Diagnostic(String),
}

fn failed(message: impl Into<String>) -> DownloadError {
    DownloadError::Failed(message.into())
}

/// A refused download request is a usage error (exit 2), every other failure exit 1.
fn download_failed(err: ModelDownloadError) -> DownloadError {
    match err {
        ModelDownloadError::InvalidRequest { reason, .. } => DownloadError::Usage(reason),
        other => failed(other.to_string()),
    }
}

/// `graphersal embed-model download`: the library's download ([`ModelCache::download_staged`],
/// endpoint `$GRAPHERSAL_HF_ENDPOINT`, token `$HF_TOKEN`) with its progress on stderr, then
/// installed into the models directory or imported into the store `into`.
fn download(
    repo: &str,
    revision: &str,
    models: &Path,
    into: Option<&str>,
) -> Result<(), DownloadError> {
    // The library's own check of the repository and the revision, before anything else.
    let mut request = ModelDownload::new(repo).with_revision(revision);
    request.validate().map_err(download_failed)?;
    if let Some(store) = into
        && !Store::is_store(store)
            .map_err(|err| DownloadError::Usage(format!("--into {store}: {err}")))?
    {
        return Err(DownloadError::Usage(not_a_store(store)));
    }
    if let Some(endpoint) = env("GRAPHERSAL_HF_ENDPOINT") {
        request = request.with_endpoint(endpoint);
    }
    if let Some(token) = env("HF_TOKEN") {
        request = request.with_token(token);
    }
    let mut progress = Progress::new();
    let staged = ModelCache::at(models)
        .download_staged(&request, |event| progress.on(event))
        .map_err(download_failed)?;
    print_licence_warnings(&staged.report().warnings);

    if let Some(store) = into {
        // Into the store; the staging directory is removed when `staged` drops (nothing in
        // the models directory).
        let new = staged.new_model().map_err(download_failed)?;
        let stored = add_to_store(
            store,
            ModelSource::Files(new),
            ImportModelOptions::new().with_principal("cli"),
        )?;
        crate::out::print_block(&imported_note(repo, store, &stored).text());
        return Ok(());
    }

    let report = staged.install().map_err(download_failed)?;
    let mut note =
        crate::out::Note::new(format!("Downloaded {repo} into {}", report.dir.display()))
            .detail("full name", &report.identity);
    if let Some(dimension) = report.dimension {
        note = note.detail("dimension", dimension.to_string());
    }
    let note = note.command(
        "import it into a store",
        format!("graphersal embed-model import {repo} --into <STORE>"),
    );
    crate::out::print_block(&note.text());
    Ok(())
}

/// The usage error of an `--into` that names no store, with the command that creates one.
fn not_a_store(store: &str) -> String {
    crate::out::Note::new(format!("--into {store}: not a Graphersal store"))
        .command("create one", format!("graphersal store create {store}"))
        .text()
}

/// The block an import into the store `store` prints: what went in (`source`, a repository or
/// a directory) and the model the store keeps now.
fn imported_note(source: &str, store: &str, stored: &ModelInfo) -> crate::out::Note {
    crate::out::Note::new(format!("Imported {source} into the store {store}"))
        .detail("full name", &stored.identity)
        .detail("dimension", stored.dimension.to_string())
        .detail("licence", &stored.license)
        .detail("default", if stored.default { "yes" } else { "no" })
}

/// Prints the warnings about passing a model on ([`model_licence_warnings`]).
fn print_licence_warnings(warnings: &[String]) {
    for warning in warnings {
        errln!("warning: {warning}");
    }
}

/// Imports `source` into the closed store at `store`: one commit appended to its WAL without
/// loading the graph (`Store::import_model_dir`); a replace whose old revision vector rules
/// name opens the store and re-embeds them (said in a note).
fn add_to_store(
    store: &str,
    source: ModelSource,
    options: ImportModelOptions,
) -> Result<ModelInfo, DownloadError> {
    match Store::import_model_dir(store, source, options) {
        Ok(report) => {
            if report.already_present {
                errln!("note: the store {store} holds this model already");
            }
            if let Some(old) = &report.replaced {
                let note = crate::out::Note::new(format!(
                    "the import replaced the revision of {} the store kept",
                    old.repo
                ))
                .detail("new full name", &report.model.identity)
                .detail("replaced", &old.identity);
                crate::out::print_note("note", &note.text());
            }
            if !report.reembedded.is_empty() {
                let note = crate::out::Note::new(
                    "the store was opened (its graph loaded) to re-embed with the new revision",
                )
                .detail("re-embedded", report.reembedded.join(", "))
                .detail("when", "in the replacing commit");
                crate::out::print_note("note", &note.text());
            }
            if report.model.license_text == "template" {
                let mut note = crate::out::Note::new(format!(
                    "the repository of {} has no licence file",
                    report.model.repo
                ));
                for (key, value) in crate::store_cmd::licence_rows(&report.model) {
                    note = note.detail(&key, value);
                }
                let note = note.detail(
                    "kept as",
                    "LICENSE (its first lines say where the text comes from)",
                );
                crate::out::print_note("note", &note.text());
            }
            Ok(report.model)
        }
        Err(err) => Err(DownloadError::Diagnostic(
            graphersal::error::format_diagnostic(&err),
        )),
    }
}

/// The model directory `embed-model import MODEL` reads: a repository downloaded into the
/// models directory (`--dir`, else the user's cache) first, else MODEL as a directory
/// ([`ModelCache::resolve`]).
fn import_source(model: &str, models: Option<&str>) -> Result<PathBuf, String> {
    let cache = ModelCache::at(models_dir(models)?);
    cache.resolve(model).map_err(|err| {
        format!(
            "{}\n{}",
            reason_of(&err),
            crate::out::Note::new("download it first, or pass the directory of a Model2Vec model")
                .command(
                    "download the model",
                    "graphersal embed-model download <owner/name>"
                )
                .help()
        )
    })
}

/// The reason of a refusal, without the operation it names.
fn reason_of(err: &graphersal::persist::PersistError) -> String {
    match err {
        graphersal::persist::PersistError::Store { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

/// The flags of `embed-model import`.
struct ImportFlags<'a> {
    repo: Option<&'a str>,
    license: Option<&'a str>,
    replace: bool,
    default: bool,
}

/// `graphersal embed-model import <dir> --into <store> [--repo] [--license] [--replace]
/// [--default]`: the library's directory import ([`ModelSource::Dir`]: the Model2Vec layouts,
/// the card, the licence files, the download manifest) as one closed-store commit.
fn import(dir: &Path, store: &str, flags: &ImportFlags<'_>) -> Result<(), DownloadError> {
    if !Store::is_store(store)
        .map_err(|err| DownloadError::Usage(format!("--into {store}: {err}")))?
    {
        return Err(DownloadError::Usage(not_a_store(store)));
    }
    if let Some(repo) = flags.repo
        && !ModelDownload::is_repo_name(repo)
    {
        return Err(DownloadError::Usage(format!(
            "--repo {repo}: not a Hugging Face repository name (owner/name)"
        )));
    }
    let manifest: Value = fs::read(dir.join(ModelCache::MANIFEST_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let text = |key: &str| {
        manifest
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let repo = match (flags.repo, text("repo")) {
        (Some(given), Some(stated)) if given != stated => {
            return Err(DownloadError::Usage(format!(
                "--repo {given}: the model's manifest names {stated}"
            )));
        }
        (Some(given), _) => Some(given.to_string()),
        (None, stated) => stated,
    };
    // Load it once as a check: a directory that is not a Model2Vec model fails here.
    ModelCache::check(dir).map_err(|err| {
        failed(format!(
            "{} is not a Model2Vec model: {}",
            dir.display(),
            reason_of(&err)
        ))
    })?;
    let license = text("license")
        .or_else(|| flags.license.map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let gated = manifest
        .get("gated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    print_licence_warnings(&model_licence_warnings(
        repo.as_deref().unwrap_or(&dir.display().to_string()),
        &license,
        gated,
    ));
    let mut options = ImportModelOptions::new()
        .with_principal("cli")
        .with_replace(flags.replace)
        .with_default(flags.default);
    if let Some(repo) = flags.repo {
        options = options.with_repo(repo);
    }
    if let Some(license) = flags.license {
        options = options.with_license(license);
    }
    let stored = add_to_store(store, ModelSource::Dir(dir.to_path_buf()), options)?;
    crate::out::print_block(&imported_note(&dir.display().to_string(), store, &stored).text());
    Ok(())
}

/// The progress of a download on stderr ([`DownloadEvent`]): the headline, one line per file
/// (rewritten in place on a terminal, else one line at the end of the file) and the notes.
struct Progress {
    terminal: bool,
    /// When the line of the current file was last written (`None`: no file in progress).
    last: Option<Instant>,
}

impl Progress {
    fn new() -> Self {
        Progress {
            terminal: io::stderr().is_terminal(),
            last: None,
        }
    }

    fn text(name: &str, done: u64, total: Option<u64>) -> String {
        match total {
            Some(total) => format!("  {name}: {} / {}", human_bytes(done), human_bytes(total)),
            None => format!("  {name}: {}", human_bytes(done)),
        }
    }

    fn on(&mut self, event: &DownloadEvent<'_>) {
        match *event {
            DownloadEvent::Started {
                repo,
                revision,
                endpoint,
                ..
            } => errln!("Downloading {repo} (revision {revision}) from {endpoint}"),
            DownloadEvent::Progress {
                name, bytes, total, ..
            } => {
                let last = self.last.get_or_insert_with(Instant::now);
                if self.terminal && last.elapsed() >= Duration::from_millis(200) {
                    *last = Instant::now();
                    crate::out::write_stderr(format_args!(
                        "\r{}\x1b[K",
                        Self::text(name, bytes, total)
                    ));
                }
            }
            DownloadEvent::FileDone {
                name, bytes, total, ..
            } => {
                self.last = None;
                if self.terminal {
                    crate::out::write_stderr(format_args!(
                        "\r{}\x1b[K\n",
                        Self::text(name, bytes, total)
                    ));
                } else {
                    errln!("{}", Self::text(name, bytes, total));
                }
            }
            DownloadEvent::Note { text, .. } => errln!("note: {text}"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names() {
        for ok in ["minishlab/potion-base-8M", "bert-base-uncased", "a.b/c_d-e"] {
            assert!(ModelDownload::is_repo_name(ok), "{ok}");
        }
        for bad in [
            "",
            "/",
            "a/",
            "/a",
            "a/b/c",
            "../x",
            "a/..",
            "a b/c",
            "a/.hidden",
        ] {
            assert!(!ModelDownload::is_repo_name(bad), "{bad}");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(human_bytes(10), "10 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(30 * 1024 * 1024), "30.0 MiB");
    }
}
