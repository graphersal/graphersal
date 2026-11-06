//! `graphersal embed-model download|import|list` (feature `model2vec`; book pages
//! "Embeddings", "Embedding Models in a Store" and "Command line"). The download cache is the
//! library's [`ModelCache`]; a model serves a database only once it is imported into it.
//!
//! The only network access of the binary besides the AI Chat: `download` fetches the three
//! files of a Model2Vec model (`config.json`, `tokenizer.json`, `model.safetensors`) from
//! `<endpoint>/<repo>/resolve/<revision>/<file>` (endpoint `https://huggingface.co`, or
//! `GRAPHERSAL_HF_ENDPOINT` for a mirror or a test; `HF_TOKEN` as a bearer token, sent to that
//! endpoint's host only, never logged). Each file streams into a `.part` file of a hidden
//! staging directory while its SHA-256 is computed; the hash Hugging Face states for an LFS
//! file (`X-Linked-Etag`) and the size are checked, the file renamed; the manifest
//! [`ModelCache::MANIFEST_FILE`] is written, the model loaded once as a check, and only then the
//! staging directory renamed to `<models-dir>/<repo>`. A failure (or a kill) leaves no model
//! behind: at most a hidden staging directory, which the next download removes.
//!
//! The model's licence travels with it (format spec 4.5): a download also fetches the model card
//! (`README.md`), the repository's `LICENSE*`/`NOTICE*` files and its metadata
//! (`<endpoint>/api/models/<repo>/revision/<revision>`: `cardData.license`,
//! `cardData.base_model`, `gated`), all recorded in the manifest. With `--into <store>` the
//! model goes into that Store (one blob and one catalog definition) instead of the models
//! directory; `import` puts a downloaded model or a local model directory into a Store.

use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use graphersal::catalog::BaseModel;
use graphersal::persist::{
    ImportModelOptions, ModelCache, ModelInfo, ModelRole, ModelSource, NewModel, Store,
};
use serde_json::{Value, json};

use crate::cli::{EXIT_OK, EXIT_QUERY_ERROR, EXIT_USAGE_ERROR};

/// The files a Model2Vec model in the Model2Vec layout consists of, in download order.
pub const MODEL_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// The default Hugging Face endpoint.
const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// The largest file a download accepts when the server states no size.
const MAX_FILE_BYTES: u64 = 8 << 30;

/// The largest metadata answer read.
const MAX_METADATA_BYTES: u64 = 8 << 20;

/// The model card's file name.
const CARD_FILE: &str = "README.md";

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

/// Whether `repo` is a Hugging Face repository name: `owner/name` (or a bare `name`), each part
/// of ASCII letters, digits, `.`, `_`, `-`, not starting with `.`.
pub fn is_repo_name(repo: &str) -> bool {
    let part_ok = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && !part.starts_with('.')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(name), None, None) => part_ok(name),
        (Some(owner), Some(name), None) => part_ok(owner) && part_ok(name),
        _ => false,
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

/// One downloaded file.
struct Fetched {
    name: String,
    size: u64,
    sha256: String,
    /// The commit the server resolved the revision to (`X-Repo-Commit`).
    commit: Option<String>,
}

/// A staging directory removed on drop unless it was moved into place.
struct Staging {
    path: PathBuf,
    done: bool,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_dir_all(&self.path);
            // The owner directory a failed first download created (removed only when empty).
            if let Some(parent) = self.path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

fn download(
    repo: &str,
    revision: &str,
    models: &Path,
    into: Option<&str>,
) -> Result<(), DownloadError> {
    if let Some(store) = into
        && !Store::is_store(store)
            .map_err(|err| DownloadError::Usage(format!("--into {store}: {err}")))?
    {
        return Err(DownloadError::Usage(not_a_store(store)));
    }
    if !is_repo_name(repo) {
        return Err(DownloadError::Usage(format!(
            "'{repo}' is not a Hugging Face repository name: owner/name of letters, digits, \
             '.', '_' and '-' (for example minishlab/potion-base-8M)"
        )));
    }
    if revision.is_empty()
        || revision.starts_with('/')
        || revision.contains("..")
        || !revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return Err(DownloadError::Usage(format!(
            "'{revision}' is not a revision: a branch, tag or commit of letters, digits, '.', \
             '_', '-' and '/'"
        )));
    }
    let endpoint = env("GRAPHERSAL_HF_ENDPOINT")
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string())
        .trim_end_matches('/')
        .to_string();
    let token = env("HF_TOKEN");
    let target = models.join(repo);
    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| models.to_path_buf());
    fs::create_dir_all(&parent)
        .map_err(|err| failed(format!("cannot create {}: {err}", parent.display())))?;
    let leaf = repo.rsplit('/').next().unwrap_or(repo);
    remove_stale_staging(&parent, leaf);
    let mut staging = Staging {
        path: parent.join(format!(".{leaf}.download-{}", std::process::id())),
        done: false,
    };
    let _ = fs::remove_dir_all(&staging.path);
    fs::create_dir_all(&staging.path)
        .map_err(|err| failed(format!("cannot create {}: {err}", staging.path.display())))?;

    errln!("Downloading {repo} (revision {revision}) from {endpoint}");
    let client = Client::new(&endpoint, token);
    let encoded_revision = revision.replace('/', "%2F");
    let mut fetched = Vec::new();
    for name in MODEL_FILES {
        let url = format!("{endpoint}/{repo}/resolve/{encoded_revision}/{name}");
        let file = client.fetch(&url, repo, revision, name, &staging.path, false)?;
        fetched.push((
            file.ok_or_else(|| failed(format!("{repo} has no {name}")))?,
            None,
        ));
    }
    let commit = fetched
        .iter()
        .find_map(|(f, _): &(Fetched, Option<ModelRole>)| f.commit.clone());

    // What travels with the model: its metadata, card and licence files (format spec 4.5).
    let meta = client.metadata(&endpoint, repo, Some(&encoded_revision));
    if let Some(note) = &meta.note {
        errln!("note: {note}");
    }
    let card_url = format!("{endpoint}/{repo}/resolve/{encoded_revision}/{CARD_FILE}");
    if let Some(card) = client.fetch(&card_url, repo, revision, CARD_FILE, &staging.path, true)? {
        fetched.push((card, Some(ModelRole::Card)));
    }
    for (name, role) in licence_files(&meta.siblings) {
        let url = format!("{endpoint}/{repo}/resolve/{encoded_revision}/{name}");
        let file = client
            .fetch(&url, repo, revision, &name, &staging.path, false)?
            .ok_or_else(|| failed(format!("{repo} lists {name} but does not serve it")))?;
        fetched.push((file, Some(role)));
    }
    let base_models: Vec<(String, Option<String>)> = meta
        .base_models
        .iter()
        .take(4)
        .map(|base| (base.clone(), client.metadata(&endpoint, base, None).license))
        .collect();
    let license = meta
        .license
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let has_licence_file = fetched
        .iter()
        .any(|(_, role)| *role == Some(ModelRole::License));
    let manifest = json!({
        "format": 2,
        "family": "model2vec",
        "repo": repo,
        "revision": revision,
        "commit": commit,
        "license": license,
        "license_text": if has_licence_file { "file" } else { "none" },
        "base_models": base_models.iter().map(|(repo, license)| match license {
            Some(license) => json!({"repo": repo, "license": license}),
            None => json!({"repo": repo}),
        }).collect::<Vec<_>>(),
        "gated": meta.gated,
        "files": fetched.iter().map(|(f, role)| json!({
            "name": f.name,
            "role": role.map_or_else(|| role_of_model_file(&f.name), ModelRole::as_str),
            "size": f.size,
            "sha256": f.sha256,
        })).collect::<Vec<_>>(),
        "downloaded_at": now_text(),
    });
    let manifest_path = staging.path.join(ModelCache::MANIFEST_FILE);
    write_synced(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).unwrap_or_default()
        )
        .as_bytes(),
    )
    .map_err(|err| failed(format!("cannot write {}: {err}", manifest_path.display())))?;

    // Load it once: a repository that is not a Model2Vec model fails here, not later.
    let identity = ModelCache::check(&staging.path).map_err(|err| {
        failed(format!(
            "{repo} was downloaded but does not load as a Model2Vec model: {}",
            reason_of(&err)
        ))
    })?;
    let dimension = safetensors_dimension(&staging.path.join("model.safetensors"));
    licence_warnings(repo, &license, meta.gated);

    if let Some(store) = into {
        // Into the store; the staging directory is removed (nothing in the models directory).
        let mut new = NewModel::new()
            .with_repo(repo)
            .with_revision(revision)
            .with_license(license.as_str())
            .with_gated(meta.gated);
        if let Some(commit) = &commit {
            new = new.with_commit(commit.as_str());
        }
        for (base, license) in &base_models {
            new = new.with_base_model(BaseModel::new(base.as_str(), license.clone()));
        }
        for (file, role) in &fetched {
            let role = role.unwrap_or_else(|| model_role(&file.name));
            let reader = fs::File::open(staging.path.join(&file.name)).map_err(|err| {
                failed(format!("cannot read the downloaded {}: {err}", file.name))
            })?;
            new = new.with_file(file.name.as_str(), role, reader);
        }
        let stored = add_to_store(
            store,
            ModelSource::Files(new),
            ImportModelOptions::new().with_principal("cli"),
        )?;
        crate::out::print_block(&imported_note(repo, store, &stored).text());
        return Ok(());
    }

    install(&staging, &target)?;
    staging.done = true;
    let mut note = crate::out::Note::new(format!("Downloaded {repo} into {}", target.display()))
        .detail("full name", &identity);
    if let Some(dimension) = dimension {
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

/// The role of one of the three model files by its name.
fn model_role(name: &str) -> ModelRole {
    match name {
        "tokenizer.json" => ModelRole::Tokenizer,
        "model.safetensors" => ModelRole::Weights,
        _ => ModelRole::Config,
    }
}

fn role_of_model_file(name: &str) -> &'static str {
    model_role(name).as_str()
}

/// The licence and notice files of a repository's file list: `LICENSE*`, `LICENCE*`,
/// `COPYING*` (role licence) and `NOTICE*` (role notice) at its root, names a Store accepts.
fn licence_files(siblings: &[String]) -> Vec<(String, ModelRole)> {
    siblings
        .iter()
        .filter(|name| !name.contains('/') && is_plain_file_name(name))
        .filter_map(|name| {
            let upper = name.to_ascii_uppercase();
            if upper.starts_with("LICENSE")
                || upper.starts_with("LICENCE")
                || upper.starts_with("COPYING")
            {
                Some((name.clone(), ModelRole::License))
            } else if upper.starts_with("NOTICE") {
                Some((name.clone(), ModelRole::Notice))
            } else {
                None
            }
        })
        .take(8)
        .collect()
}

/// A file name a Store keeps in a model directory (format spec 4.5).
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && !name.starts_with('.')
        && !name.starts_with("tmp-")
        && name != ModelCache::MANIFEST_FILE
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The warnings about passing a model on: an unknown or non-commercial licence, custom terms,
/// a gated repository.
fn licence_warnings(repo: &str, license: &str, gated: bool) {
    let lower = license.to_ascii_lowercase();
    if lower == "unknown" {
        errln!(
            "warning: the licence of {repo} is unknown (its model card names none, or its \
             metadata could not be read): check the repository before you hand on a store or \
             file that carries the model"
        );
    } else if lower.contains("-nc")
        || lower.contains("noncommercial")
        || lower.contains("non-commercial")
    {
        errln!(
            "warning: {repo} is under {license}, a NON-COMMERCIAL licence: a store or file \
             carrying it may not be used or handed on commercially"
        );
    } else if lower == "other" {
        errln!(
            "warning: {repo} is under custom terms (licence \"other\"): read its model card \
             before you hand on a store or file that carries the model"
        );
    }
    if gated {
        errln!(
            "warning: {repo} is a GATED repository: the terms you accepted to download it may \
             restrict passing it on"
        );
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

/// The dimension of the embedding tensor of a safetensors file (its header: the 2-dimensional
/// tensor `embeddings`, `0` or `embedding.weight`); `None` when it does not say.
fn safetensors_dimension(path: &Path) -> Option<u64> {
    let mut file = fs::File::open(path).ok()?;
    let mut len = [0u8; 8];
    file.read_exact(&mut len).ok()?;
    let len = u64::from_le_bytes(len);
    if len > 100 << 20 {
        return None;
    }
    let mut header = vec![0u8; usize::try_from(len).ok()?];
    file.read_exact(&mut header).ok()?;
    let header: Value = serde_json::from_slice(&header).ok()?;
    ["embeddings", "0", "embedding.weight"]
        .iter()
        .find_map(|name| {
            let shape = header.get(name)?.get("shape")?.as_array()?;
            match shape.as_slice() {
                [_, cols] => cols.as_u64(),
                _ => None,
            }
        })
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
        && !is_repo_name(repo)
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
    licence_warnings(
        repo.as_deref().unwrap_or(&dir.display().to_string()),
        &license,
        gated,
    );
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

/// Moves the finished staging directory to `target`, replacing an older download.
fn install(staging: &Staging, target: &Path) -> Result<(), DownloadError> {
    let old = target.with_file_name(format!(
        ".{}.old-{}",
        target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        std::process::id()
    ));
    let replaced = target.exists();
    if replaced {
        fs::rename(target, &old).map_err(|err| {
            failed(format!(
                "cannot replace the earlier download {}: {err}",
                target.display()
            ))
        })?;
    }
    if let Err(err) = fs::rename(&staging.path, target) {
        if replaced {
            let _ = fs::rename(&old, target);
        }
        return Err(failed(format!(
            "cannot move the model to {}: {err}",
            target.display()
        )));
    }
    if replaced {
        let _ = fs::remove_dir_all(&old);
    }
    if let Some(parent) = target.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Removes the leftovers of interrupted downloads of `leaf` in `parent`.
fn remove_stale_staging(parent: &Path, leaf: &str) {
    let prefixes = [format!(".{leaf}.download-"), format!(".{leaf}.old-")];
    for entry in fs::read_dir(parent).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Now, as the canonical UTC date-time text.
fn now_text() -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    graphersal::temporal::DateTime::from_micros(micros - micros % 1_000_000, 0)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// What the Hugging Face API says about a repository (the parts a Store records).
#[derive(Debug, Default)]
struct HubMetadata {
    /// `cardData.license` (an SPDX id), when stated.
    license: Option<String>,
    /// `cardData.base_model` (a string or a list).
    base_models: Vec<String>,
    /// `gated`: `false`, or `"auto"`/`"manual"`.
    gated: bool,
    /// The repository's files (`siblings[].rfilename`).
    siblings: Vec<String>,
    /// Why the metadata is missing, when it is.
    note: Option<String>,
}

impl HubMetadata {
    fn from_json(value: &Value) -> Self {
        let card = value.get("cardData");
        let license = card
            .and_then(|c| c.get("license"))
            .and_then(|l| match l {
                Value::String(text) => Some(text.clone()),
                Value::Array(items) => items.first().and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .filter(|l| {
                !l.trim().is_empty() && l.len() <= 64 && l.chars().all(|c| c.is_ascii_graphic())
            });
        let base_models = match card.and_then(|c| c.get("base_model")) {
            Some(Value::String(base)) => vec![base.clone()],
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|base| is_repo_name(base))
        .collect();
        let gated = match value.get("gated") {
            Some(Value::Bool(gated)) => *gated,
            Some(Value::String(_)) => true,
            _ => false,
        };
        let siblings = value
            .get("siblings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| s.get("rfilename").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        HubMetadata {
            license,
            base_models,
            gated,
            siblings,
            note: None,
        }
    }
}

/// The HTTP client of a download.
struct Client {
    agent: ureq::Agent,
    /// The authority (`host[:port]`) of the endpoint: the only one `HF_TOKEN` is sent to.
    endpoint_authority: String,
    token: Option<String>,
}

/// The `scheme://authority` part and the authority of an absolute URL.
fn origin_of(url: &str) -> Option<(&str, &str)> {
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin_len = scheme_end + 3 + authority_end;
    Some((&url[..origin_len], &rest[..authority_end]))
}

/// `location` (a redirect target) resolved against `base`.
fn resolve_location(base: &str, location: &str) -> Option<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Some(location.to_string());
    }
    let (origin, _) = origin_of(base)?;
    if let Some(rest) = location.strip_prefix("//") {
        let scheme = &base[..base.find("://")?];
        return Some(format!("{scheme}://{rest}"));
    }
    if location.starts_with('/') {
        return Some(format!("{origin}{location}"));
    }
    let path = base[origin.len()..].split(['?', '#']).next().unwrap_or("/");
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    Some(format!("{origin}{dir}{location}"))
}

/// The header `name` of a response, as text.
fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// The hash a server states for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stated {
    /// The SHA-256 of the content (Hugging Face LFS files: `X-Linked-Etag`).
    Sha256(String),
    /// The git blob id: SHA-1 of `blob <size>\0` + the content (Hugging Face files kept in git).
    GitSha1(String),
}

/// The hash inside an ETag value (`"<hex>"`, `W/"<hex>"`): 64 hex digits are a SHA-256, 40 a
/// git blob id; anything else is not a content hash.
fn etag_hash(value: &str) -> Option<Stated> {
    let hex = value.trim().trim_start_matches("W/").trim_matches('"');
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        64 => Some(Stated::Sha256(hex.to_lowercase())),
        40 => Some(Stated::GitSha1(hex.to_lowercase())),
        _ => None,
    }
}

impl Client {
    fn new(endpoint: &str, token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .user_agent(concat!("graphersal/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Client {
            agent,
            endpoint_authority: origin_of(endpoint)
                .map(|(_, authority)| authority.to_string())
                .unwrap_or_default(),
            token,
        }
    }

    /// The Hugging Face metadata of `repo` (at `revision`, URL-encoded; `None`: the default
    /// branch): `cardData.license`, `cardData.base_model`, `gated` and the file list. Best
    /// effort: an unreachable or unreadable answer gives empty metadata and a note.
    fn metadata(&self, endpoint: &str, repo: &str, revision: Option<&str>) -> HubMetadata {
        let url = match revision {
            Some(revision) => format!("{endpoint}/api/models/{repo}/revision/{revision}"),
            None => format!("{endpoint}/api/models/{repo}"),
        };
        let mut request = self.agent.get(&url);
        if origin_of(&url).is_some_and(|(_, a)| a == self.endpoint_authority)
            && let Some(token) = &self.token
        {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let unreadable = |why: String| HubMetadata {
            note: Some(format!(
                "the metadata of {repo} could not be read ({why}): its licence is unknown"
            )),
            ..HubMetadata::default()
        };
        let response = match request.call() {
            Ok(response) => response,
            Err(err) => return unreadable(err.to_string()),
        };
        let status = response.status().as_u16();
        if status != 200 {
            return unreadable(format!("HTTP {status}"));
        }
        let mut body = Vec::new();
        if let Err(err) = response
            .into_body()
            .into_reader()
            .take(MAX_METADATA_BYTES)
            .read_to_end(&mut body)
        {
            return unreadable(err.to_string());
        }
        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
            return unreadable("not JSON".to_string());
        };
        HubMetadata::from_json(&value)
    }

    /// Downloads `url` (the file `name` of `repo`) into `dir/name`, following redirects
    /// itself so the stated hash of the first answer is seen and the token goes to the
    /// endpoint's host only.
    fn fetch(
        &self,
        url: &str,
        repo: &str,
        revision: &str,
        name: &str,
        dir: &Path,
        optional: bool,
    ) -> Result<Option<Fetched>, DownloadError> {
        let mut url = url.to_string();
        let mut stated: Option<Stated> = None;
        let mut expected_size: Option<u64> = None;
        let mut commit: Option<String> = None;
        for _ in 0..10 {
            let mut request = self.agent.get(&url);
            let same_host = origin_of(&url).is_some_and(|(_, a)| a == self.endpoint_authority);
            if same_host && let Some(token) = &self.token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            let response = request
                .call()
                .map_err(|err| failed(format!("cannot fetch {name} of {repo}: {err}")))?;
            let status = response.status().as_u16();
            if stated.is_none() {
                stated = header(&response, "x-linked-etag").and_then(|v| etag_hash(&v));
            }
            if expected_size.is_none() {
                expected_size =
                    header(&response, "x-linked-size").and_then(|v| v.trim().parse().ok());
            }
            if commit.is_none() {
                commit = header(&response, "x-repo-commit");
            }
            match status {
                200 => {
                    if stated.is_none() {
                        stated = header(&response, "etag").and_then(|v| etag_hash(&v));
                    }
                    if expected_size.is_none() {
                        expected_size =
                            header(&response, "content-length").and_then(|v| v.trim().parse().ok());
                    }
                    let (sha256, size) =
                        self.stream(response, name, dir, expected_size, stated.as_ref())?;
                    return Ok(Some(Fetched {
                        name: name.to_string(),
                        size,
                        sha256,
                        commit,
                    }));
                }
                301 | 302 | 303 | 307 | 308 => {
                    let location = header(&response, "location").ok_or_else(|| {
                        failed(format!("{name} of {repo}: a redirect without a Location"))
                    })?;
                    url = resolve_location(&url, &location).ok_or_else(|| {
                        failed(format!("{name} of {repo}: cannot follow the redirect"))
                    })?;
                }
                401 | 403 => {
                    return Err(failed(format!(
                        "{repo} refused the download of {name} (HTTP {status})\nHelp: the \
                         repository does not exist (Hugging Face answers 401 then), or it is \
                         gated or private and needs a Hugging Face token in HF_TOKEN (and the \
                         licence accepted on its page)"
                    )));
                }
                404 if optional => return Ok(None),
                404 => {
                    return Err(failed(format!(
                        "{repo} at revision {revision} has no {name} (HTTP 404)\nHelp: check \
                         the repository name and the revision; a Model2Vec model holds \
                         config.json, tokenizer.json and model.safetensors at its root (for \
                         example minishlab/potion-base-8M)"
                    )));
                }
                _ => {
                    return Err(failed(format!(
                        "the download of {name} of {repo} failed: HTTP {status}"
                    )));
                }
            }
        }
        Err(failed(format!("{name} of {repo}: too many redirects")))
    }

    /// Streams the body into `dir/name.part`, checks size and hash, renames it to `dir/name`.
    fn stream(
        &self,
        response: ureq::http::Response<ureq::Body>,
        name: &str,
        dir: &Path,
        expected_size: Option<u64>,
        stated: Option<&Stated>,
    ) -> Result<(String, u64), DownloadError> {
        let part = dir.join(format!("{name}.part"));
        let mut file = fs::File::create(&part)
            .map_err(|err| failed(format!("cannot create {}: {err}", part.display())))?;
        let mut reader = response.into_body().into_reader();
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        // A git blob id needs the size up front: checked only when the server states it.
        let mut git = match (stated, expected_size) {
            (Some(Stated::GitSha1(_)), Some(size)) => {
                let mut context =
                    ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
                context.update(format!("blob {size}\0").as_bytes());
                Some(context)
            }
            _ => None,
        };
        let mut buffer = vec![0u8; 256 * 1024];
        let mut total: u64 = 0;
        let limit = expected_size.unwrap_or(MAX_FILE_BYTES);
        let progress = Progress::new(name, expected_size);
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|err| failed(format!("the download of {name} broke off: {err}")))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > limit {
                return Err(failed(format!(
                    "{name} is larger than {}",
                    human_bytes(limit)
                )));
            }
            digest.update(&buffer[..read]);
            if let Some(git) = git.as_mut() {
                git.update(&buffer[..read]);
            }
            file.write_all(&buffer[..read])
                .map_err(|err| failed(format!("cannot write {}: {err}", part.display())))?;
            progress.update(total);
        }
        file.sync_all()
            .map_err(|err| failed(format!("cannot write {}: {err}", part.display())))?;
        drop(file);
        progress.finish(total);
        if let Some(expected) = expected_size
            && total != expected
        {
            return Err(failed(format!(
                "the download of {name} broke off: {total} of {expected} bytes"
            )));
        }
        let sha256 = hex(digest.finish().as_ref());
        let mismatch = match stated {
            Some(Stated::Sha256(expected)) if *expected != sha256 => {
                Some(format!("SHA-256 {sha256}, the server states {expected}"))
            }
            Some(Stated::GitSha1(expected)) => git
                .map(|git| hex(git.finish().as_ref()))
                .filter(|actual| actual != expected)
                .map(|actual| format!("git blob id {actual}, the server states {expected}")),
            _ => None,
        };
        if let Some(mismatch) = mismatch {
            return Err(failed(format!(
                "{name}: the download is not the file the server describes ({mismatch}); \
                 nothing was kept"
            )));
        }
        fs::rename(&part, dir.join(name))
            .map_err(|err| failed(format!("cannot rename {}: {err}", part.display())))?;
        Ok((sha256, total))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The progress line of one file on stderr: rewritten in place on a terminal, else one line at
/// the end.
struct Progress {
    name: String,
    expected: Option<u64>,
    terminal: bool,
    last: std::cell::Cell<Instant>,
}

impl Progress {
    fn new(name: &str, expected: Option<u64>) -> Self {
        Progress {
            name: name.to_string(),
            expected,
            terminal: io::stderr().is_terminal(),
            last: std::cell::Cell::new(Instant::now()),
        }
    }

    fn text(&self, done: u64) -> String {
        match self.expected {
            Some(total) => format!(
                "  {}: {} / {}",
                self.name,
                human_bytes(done),
                human_bytes(total)
            ),
            None => format!("  {}: {}", self.name, human_bytes(done)),
        }
    }

    fn update(&self, done: u64) {
        if self.terminal && self.last.get().elapsed() >= Duration::from_millis(200) {
            self.last.set(Instant::now());
            crate::out::write_stderr(format_args!("\r{}\x1b[K", self.text(done)));
        }
    }

    fn finish(&self, done: u64) {
        if self.terminal {
            crate::out::write_stderr(format_args!("\r{}\x1b[K\n", self.text(done)));
        } else {
            errln!("{}", self.text(done));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names() {
        for ok in ["minishlab/potion-base-8M", "bert-base-uncased", "a.b/c_d-e"] {
            assert!(is_repo_name(ok), "{ok}");
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
            assert!(!is_repo_name(bad), "{bad}");
        }
    }

    #[test]
    fn redirects_resolve() {
        let base = "https://huggingface.co/a/b/resolve/main/model.safetensors";
        assert_eq!(
            resolve_location(base, "https://cdn.example/x?y=1").as_deref(),
            Some("https://cdn.example/x?y=1")
        );
        assert_eq!(
            resolve_location(base, "/api/resolve-cache/models/a/b/c/config.json").as_deref(),
            Some("https://huggingface.co/api/resolve-cache/models/a/b/c/config.json")
        );
        assert_eq!(
            resolve_location(base, "other.json").as_deref(),
            Some("https://huggingface.co/a/b/resolve/main/other.json")
        );
        assert_eq!(
            resolve_location("http://127.0.0.1:9/x", "//cdn/y").as_deref(),
            Some("http://cdn/y")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:8080/a/b"),
            Some(("http://127.0.0.1:8080", "127.0.0.1:8080"))
        );
    }

    #[test]
    fn etags() {
        let hex = "a".repeat(64);
        assert_eq!(
            etag_hash(&format!("\"{hex}\"")),
            Some(Stated::Sha256(hex.clone()))
        );
        assert_eq!(
            etag_hash(&format!("W/\"{hex}\"")),
            Some(Stated::Sha256(hex))
        );
        let sha1 = "b".repeat(40);
        assert_eq!(
            etag_hash(&format!("\"{sha1}\"")),
            Some(Stated::GitSha1(sha1))
        );
        assert_eq!(etag_hash("\"0123abcd\""), None);
        assert_eq!(etag_hash(&format!("\"{}\"", "z".repeat(64))), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_bytes(10), "10 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(30 * 1024 * 1024), "30.0 MiB");
    }
}
