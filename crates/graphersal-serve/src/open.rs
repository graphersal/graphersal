//! Opening the databases a server serves (EXPERIMENTAL): a Store read-write
//! ([`open_store`]), the base of layers read from a Store ([`layer_base_of_store`]) and a
//! layer store over a given base ([`open_layer_store`]).
//!
//! What an open found that a person should know (a backup opened read-only, damage, an unclean
//! last close, a store just created, embedding models not served) comes back as data
//! ([`OpenNote`]), never printed: the host shows it its own way (a banner, a log through
//! `tracing`, an admin API). The embedding models are answered per database
//! ([`OpenedStore::models`], [`OpenedBase::models`]), so a host serving several databases keeps
//! them apart. The CLI's `--graph` keeps its own opening (its texts are pinned by its tests);
//! these functions are for hosts serving many databases.
//!
//! Every function takes any [`IntoStoreDir`] (a path, an `FsDir`, a `MemDir`); the diagnostics
//! name the directory by its location.

use std::fmt;
use std::sync::Arc;

use graphersal::persist::{
    DamagePolicy, IntoStoreDir, ModelInfo, ModelState, OpenReport, PersistError, RecoveryTarget,
    Store, StoreDir, StoreOptions,
};
use graphersal::storage::{LayerBase, LayeredGraph};

use crate::context::ServeError;

/// How [`open_store`] / [`open_layer_store`] open a store: create it when it does not exist
/// ([`Self::create`]), and the damage policy a new store gets ([`Self::on_damage`]).
///
/// `#[non_exhaustive]`: further options may be added; built through [`Self::new`] and the
/// setters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenStore {
    create: bool,
    on_damage: Option<DamagePolicy>,
}

impl OpenStore {
    /// Open an existing store only, with whatever damage policy it was created with.
    pub fn new() -> Self {
        Self::default()
    }

    /// With `true`, a directory that holds no store gets a new, empty one (the directory must
    /// not exist or be empty); default `false`: such a directory is refused
    /// ([`OpenErrorKind::NotAStore`]).
    pub fn create(mut self, create: bool) -> Self {
        self.create = create;
        self
    }

    /// The damage policy of a store being created. An existing store with another policy is
    /// refused ([`OpenErrorKind::DamagePolicyMismatch`]): the policy is fixed when a store is
    /// created. Default: none asked (a new store gets [`DamagePolicy::default`], an existing
    /// one keeps its own).
    pub fn on_damage(mut self, policy: DamagePolicy) -> Self {
        self.on_damage = Some(policy);
        self
    }

    /// Whether a missing store is created ([`Self::create`]).
    pub fn creates(&self) -> bool {
        self.create
    }

    /// The damage policy asked for ([`Self::on_damage`]).
    pub fn damage_policy(&self) -> Option<DamagePolicy> {
        self.on_damage
    }
}

/// How much an [`OpenNote`] matters.
///
/// `#[non_exhaustive]`: further levels may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NoteLevel {
    /// Worth knowing (a store created, an unclean close recovered, a backup opened read-only).
    Note,
    /// Something is wrong (damage, a warning of the recovery).
    Warning,
}

/// What an open found that a person should know, as data: a headline, the details as
/// `(name, value)` pairs, and the commands that help as `(why, command)` pairs (commands of the
/// `graphersal` CLI). Never printed by this crate.
///
/// `#[non_exhaustive]`: further fields may be added; built by this crate's open functions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenNote {
    /// How much it matters.
    pub level: NoteLevel,
    /// One line saying what happened.
    pub headline: String,
    /// The details, `(name, value)`.
    pub details: Vec<(String, String)>,
    /// The commands that help, `(why, command)`.
    pub commands: Vec<(String, String)>,
}

impl OpenNote {
    fn new(level: NoteLevel, headline: impl Into<String>) -> Self {
        Self {
            level,
            headline: headline.into(),
            details: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn note(headline: impl Into<String>) -> Self {
        Self::new(NoteLevel::Note, headline)
    }

    fn warning(headline: impl Into<String>) -> Self {
        Self::new(NoteLevel::Warning, headline)
    }

    fn detail(mut self, name: &str, value: impl Into<String>) -> Self {
        self.details.push((name.to_string(), value.into()));
        self
    }

    fn command(mut self, why: &str, command: impl Into<String>) -> Self {
        self.commands.push((why.to_string(), command.into()));
        self
    }
}

/// Why an open failed ([`OpenError::kind`]).
///
/// `#[non_exhaustive]`: further kinds may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenErrorKind {
    /// The directory holds no store (no `GRAPH` file) and [`OpenStore::create`] was not asked.
    NotAStore,
    /// The store may not be written by this process (no permission, a read-only file system):
    /// a host may serve it read-only instead.
    NotWritable,
    /// The store exists with another damage policy than [`OpenStore::on_damage`] asked.
    DamagePolicyMismatch,
    /// Any other failure (in use, damaged beyond maintenance mode, an older format, a layer
    /// store over another base, ...): [`OpenError::persist_error`] has the engine's error.
    Failed,
}

/// An open that failed ([`ServeError::Open`]): its kind, the diagnostic, and the engine's
/// error behind it when there is one.
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OpenError {
    kind: OpenErrorKind,
    message: String,
    source: Option<Arc<PersistError>>,
}

impl OpenError {
    fn new(kind: OpenErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// The failure of an engine call (`kind` `Failed` unless the caller knows better).
    fn persist(kind: OpenErrorKind, err: PersistError) -> Self {
        Self {
            kind,
            message: graphersal::error::format_diagnostic(&err),
            source: Some(Arc::new(err)),
        }
    }

    /// Why the open failed.
    pub fn kind(&self) -> OpenErrorKind {
        self.kind
    }

    /// The diagnostic (the engine's, with its help where it gives one).
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The engine's error behind it, when an engine call failed.
    pub fn persist_error(&self) -> Option<&PersistError> {
        self.source.as_deref()
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|err| err as &(dyn std::error::Error + 'static))
    }
}

impl From<OpenError> for ServeError {
    fn from(err: OpenError) -> Self {
        ServeError::Open(err)
    }
}

/// An open Store ([`open_store`]).
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug)]
#[non_exhaustive]
pub struct OpenedStore {
    /// The store (its lock held until the last handle is dropped or it is shut down).
    pub store: Arc<Store>,
    /// The embedding models of the store's current position, each with its state.
    pub models: Vec<ModelInfo>,
    /// What the open found, in order.
    pub notes: Vec<OpenNote>,
}

/// The frozen base of layers read from a Store ([`layer_base_of_store`]).
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug)]
#[non_exhaustive]
pub struct OpenedBase {
    /// The base: share it (`Clone`) among every layer over the same commit.
    pub base: LayerBase,
    /// The embedding models of the store's latest position (its layers serve them).
    pub models: Vec<ModelInfo>,
    /// What the read found (models not served, ...).
    pub notes: Vec<OpenNote>,
}

/// An open layer store ([`open_layer_store`]).
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug)]
#[non_exhaustive]
pub struct OpenedLayerStore {
    /// The layer store (its lock held); its live graph is a layer over the base.
    pub store: Arc<Store<LayeredGraph>>,
    /// What the open found, in order.
    pub notes: Vec<OpenNote>,
}

/// Opens the Store in `dir` read-write (its lock held, recovery run, every commit durable in
/// its WAL); a backup opens read-only, a damaged store in maintenance mode (read-only), both
/// with a note. With [`OpenStore::create`] a directory without a store gets a new one.
///
/// # Errors
///
/// [`ServeError::Open`]: [`OpenErrorKind::NotAStore`], [`OpenErrorKind::NotWritable`],
/// [`OpenErrorKind::DamagePolicyMismatch`], or [`OpenErrorKind::Failed`] with the engine's
/// error (in use, damaged, a non-empty directory to create in, ...).
pub fn open_store(dir: impl IntoStoreDir, options: OpenStore) -> Result<OpenedStore, ServeError> {
    let dir = dir.into_store_dir();
    let location = dir.location();
    let failed = |err| OpenError::persist(OpenErrorKind::Failed, err);
    let mut notes = Vec::new();
    let store = if Store::is_store(&dir).map_err(failed)? {
        match Store::backup_marker_dir(&dir) {
            // A backup opens read-only (it is restored with `store restore`).
            Ok(Some(marker)) => {
                let store = Store::open_backup(&dir, StoreOptions::new()).map_err(failed)?;
                notes.push(backup_note(&location, marker, false));
                store
            }
            _ => {
                let store = Store::open(&dir, StoreOptions::new()).map_err(|err| {
                    let kind = if is_not_writable(&err) {
                        OpenErrorKind::NotWritable
                    } else {
                        OpenErrorKind::Failed
                    };
                    OpenError::persist(kind, err)
                })?;
                check_damage_policy(
                    &location,
                    "store",
                    store.damage_policy().map_err(failed)?,
                    options,
                )?;
                store
            }
        }
    } else if options.create {
        let policy = options.on_damage.unwrap_or_default();
        let store =
            Store::create(&dir, StoreOptions::new().with_damage_policy(policy)).map_err(failed)?;
        notes.push(
            OpenNote::note(format!("created the store {location}"))
                .detail("damage policy", policy.to_string()),
        );
        store
    } else {
        return Err(not_a_store(&location).into());
    };
    let report = store.open_report();
    if let Some(damage) = store.maintenance() {
        notes.push(
            OpenNote::warning(format!(
                "the store {location} has DAMAGE and opened READ-ONLY in maintenance mode: \
                 queries read the intact data, writes are refused"
            ))
            .detail("damage", damage.to_string())
            .command(
                "build a repaired copy in a new directory",
                format!("graphersal store repair {location} --to <new_dir>"),
            ),
        );
    } else {
        notes.extend(unclean_close_note(&location, report));
    }
    if let Some(what) = &report.graph_file_repaired {
        notes.push(graph_file_repaired_note(&location, what, &store));
    }
    notes.extend(warning_notes(report));
    let models = store.models().map_err(failed)?;
    notes.extend(model_notes(&location, &models));
    Ok(OpenedStore {
        store: Arc::new(store),
        models,
        notes,
    })
}

/// The frozen base of layers read from the Store in `dir`: its latest state loaded read-only
/// ([`LayerBase::from_store_read_only`]: no lock, nothing in it written, so a writer may keep
/// the store open), with the embedding models of that position, named after the store's
/// location. Every read of the same commit has the same `BaseRef`: a host pools its bases by
/// it.
///
/// # Errors
///
/// [`ServeError::Open`]: [`OpenErrorKind::NotAStore`], or [`OpenErrorKind::Failed`] with the
/// engine's error (a layer store is no base, a graph a layer cannot carry, ...).
pub fn layer_base_of_store(dir: impl IntoStoreDir) -> Result<OpenedBase, ServeError> {
    let dir = dir.into_store_dir();
    let location = dir.location();
    let failed = |err| OpenError::persist(OpenErrorKind::Failed, err);
    if !Store::is_store(&dir).map_err(failed)? {
        return Err(not_a_store(&location).into());
    }
    let base = LayerBase::from_store_read_only(&dir, RecoveryTarget::Latest).map_err(failed)?;
    let models = Store::models_dir(&dir).map_err(failed)?;
    let notes = model_notes(&location, &models);
    Ok(OpenedBase {
        base,
        models,
        notes,
    })
}

/// Opens the layer store in `dir` read-write over `base` (recovery, its lock held, every
/// commit durable in its WAL; the base is never written); a backup of a layer store opens
/// read-only with a note. With [`OpenStore::create`] a directory without a store gets a new
/// layer store over `base`.
///
/// # Errors
///
/// [`ServeError::Open`]: [`OpenErrorKind::NotAStore`], [`OpenErrorKind::NotWritable`],
/// [`OpenErrorKind::DamagePolicyMismatch`], or [`OpenErrorKind::Failed`] with the engine's
/// error (a store over another base: `LayerBaseMismatch`, a plain store, in use, ...).
pub fn open_layer_store(
    dir: impl IntoStoreDir,
    base: &LayerBase,
    options: OpenStore,
) -> Result<OpenedLayerStore, ServeError> {
    let dir: Arc<dyn StoreDir> = dir.into_store_dir();
    let location = dir.location();
    let failed = |err| OpenError::persist(OpenErrorKind::Failed, err);
    let mut notes = Vec::new();
    let store = if Store::is_store(&dir).map_err(failed)? {
        match Store::backup_marker_dir(&dir) {
            Ok(Some(marker)) => {
                let store =
                    Store::<LayeredGraph>::open_layer_backup(&dir, base, StoreOptions::new())
                        .map_err(failed)?;
                notes.push(backup_note(&location, marker, true));
                store
            }
            _ => {
                let store = Store::<LayeredGraph>::open_layer(&dir, base, StoreOptions::new())
                    .map_err(|err| {
                        let kind = if is_not_writable(&err) {
                            OpenErrorKind::NotWritable
                        } else {
                            OpenErrorKind::Failed
                        };
                        OpenError::persist(kind, err)
                    })?;
                check_damage_policy(
                    &location,
                    "layer store",
                    store.damage_policy().map_err(failed)?,
                    options,
                )?;
                notes.extend(unclean_close_note(&location, store.open_report()));
                store
            }
        }
    } else if options.create {
        let policy = options.on_damage.unwrap_or_default();
        let store = Store::<LayeredGraph>::create_layer(
            &dir,
            base,
            StoreOptions::new().with_damage_policy(policy),
        )
        .map_err(failed)?;
        let mut note = OpenNote::note(format!("created the layer store {location}"))
            .detail("base", base.name());
        if let Some(identity) = base.base_ref() {
            note = note.detail("base identity", identity.to_string());
        }
        notes.push(note.detail("damage policy", policy.to_string()));
        store
    } else {
        return Err(not_a_store(&location).into());
    };
    notes.extend(warning_notes(store.open_report()));
    Ok(OpenedLayerStore {
        store: Arc::new(store),
        notes,
    })
}

/// Whether `err` says this process may not write the store.
fn is_not_writable(err: &PersistError) -> bool {
    matches!(
        err,
        PersistError::Io { source, .. }
            if matches!(
                source.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            )
    )
}

/// The refusal of a directory without a store.
fn not_a_store(location: &str) -> OpenError {
    OpenError::new(
        OpenErrorKind::NotAStore,
        format!(
            "Error: '{location}' is not a Graphersal store (no GRAPH file)\nHelp: create it \
             first (graphersal store create {location})"
        ),
    )
}

/// Refuses an existing store whose damage policy is not the one `options` asked for.
fn check_damage_policy(
    location: &str,
    what: &str,
    have: DamagePolicy,
    options: OpenStore,
) -> Result<(), OpenError> {
    match options.on_damage {
        Some(wanted) if wanted != have => Err(OpenError::new(
            OpenErrorKind::DamagePolicyMismatch,
            format!(
                "Error: the {what} {location} exists with the damage policy \"{have}\"; the \
                 damage policy \"{wanted}\" applies only to a store being created\nHelp: the \
                 damage policy is fixed when a store is created and never changes (see it with \
                 graphersal store info {location})"
            ),
        )),
        _ => Ok(()),
    }
}

/// The note of a backup opened read-only (`layer`: a backup of a layer store).
fn backup_note(location: &str, marker: graphersal::persist::BackupMarker, layer: bool) -> OpenNote {
    let note = OpenNote::note(format!(
        "{location} is a BACKUP{}: opened READ-ONLY, writes are refused",
        if layer { " of a layer store" } else { "" }
    ))
    .detail("backup", format!("up to commit {}", marker.commit_seq))
    .detail(
        "taken",
        graphersal_session::store::format_time(marker.taken_at),
    )
    .command(
        if layer {
            "make it the live layer store"
        } else {
            "make it the live store"
        },
        format!("graphersal store restore {location}"),
    );
    if layer {
        note
    } else {
        note.command(
            "or make a writable copy",
            format!("graphersal store fork {location} <new_dir>"),
        )
    }
}

/// The note of a store whose last close was not clean (what the open recovered).
fn unclean_close_note(location: &str, report: &OpenReport) -> Option<OpenNote> {
    if report.clean_close {
        return None;
    }
    let mut note = OpenNote::note(format!(
        "the store {location} was not closed cleanly last time"
    ))
    .detail("recovered to", format!("commit {}", report.commit_seq))
    .detail("commits replayed", report.commits_replayed.to_string());
    if let Some(torn) = &report.torn_tail {
        note = note.detail(
            "interrupted last write",
            format!("{} bytes cut", torn.bytes),
        );
    }
    Some(note)
}

/// The note of a `GRAPH` file the open repaired from its other copy.
fn graph_file_repaired_note(location: &str, what: &str, store: &Store) -> OpenNote {
    let read_only = store.backup_marker().is_some()
        || store.maintenance().is_some()
        || !matches!(store.read_only_reason(), Ok(None));
    if read_only {
        OpenNote::note(format!(
            "the store {location}: {what}; the copy is left as it is (a read-only open writes \
             nothing)"
        ))
    } else {
        OpenNote::note(format!(
            "the store {location}: {what}; the other copy was rewritten from it"
        ))
        .command(
            "check the rest of the store",
            format!("graphersal store verify {location}"),
        )
    }
}

/// The warnings of the recovery.
fn warning_notes(report: &OpenReport) -> impl Iterator<Item = OpenNote> + '_ {
    report.warnings.iter().map(OpenNote::warning)
}

/// The notes about the embedding models of the store at `location`: none the default, models
/// not served (damaged, unsupported, missing).
fn model_notes(location: &str, models: &[ModelInfo]) -> Vec<OpenNote> {
    let mut notes = Vec::new();
    if !models.is_empty() && !models.iter().any(|m| m.default) {
        let repo = match models {
            [only] => only.repo.as_str(),
            _ => "<repo>",
        };
        notes.push(
            OpenNote::note(format!(
                "the store {location} holds embedding models but none is the default"
            ))
            .detail("effect", "embed() without a model name uses hash-ngram/1")
            .detail("name one", format!("#{{embedder: \"{}\"}}", models[0].repo))
            .command(
                "or make one the default, so embed() without a model name uses it",
                format!("graphersal store models default {location} {repo}"),
            ),
        );
    }
    for model in models {
        let headline = format!(
            "the embedding model {} of the store {location} is not served",
            model.repo
        );
        match &model.state {
            ModelState::Damaged { reason } | ModelState::Unsupported { reason } => {
                notes.push(OpenNote::warning(headline).detail("why", reason.as_str()));
            }
            ModelState::Missing => notes.push(
                OpenNote::warning(headline)
                    .detail("why", "its blob is missing")
                    .command(
                        "download the model into the local models directory",
                        format!("graphersal embed-model download {}", model.repo),
                    )
                    .command(
                        "import it into the store in place of the missing copy",
                        format!(
                            "graphersal embed-model import {} --into {location} --replace",
                            model.repo
                        ),
                    ),
            ),
            _ => {}
        }
    }
    notes
}
