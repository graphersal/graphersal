//! The store converter (format spec 15.1): upgrades a store, a backup directory or a
//! single-file store of an older RELEASED store format version to the current one, without the
//! Graphersal that wrote it.
//!
//! Every build reads the files of every released format version (15), so an upgrade rewrites
//! nothing but `GRAPH` and `GRAPH.copy`: the snapshots, WAL segments, `marks`, the attic and
//! everything else stay byte for byte (each file keeps its own file format version, which every
//! reader accepts), so the history, the marks, the lineage, the attic entries and every creation
//! parameter (the store id among them) are carried exactly. The journal never appends to a WAL
//! segment of an older file format version: the first read-write open starts a new segment
//! (`attach_journal`).
//!
//! The one exception (store format version 3 to 4, Graphersal 0.4.0): the embedding models kept
//! as files under `models/` become blobs with one catalog definition each, added by ONE commit
//! in a new WAL segment, and `models/` is removed (`migrate_models.rs`, under an `INTENT`). The
//! report lists the converted models, the models the schema needs that the store lacks and the
//! vector properties without a rule (`from`).
//!
//! The older store is checked first through an [`UpgradeView`]: the store as this build reads
//! it once upgraded (its `GRAPH` copies served with the current version word, nothing written),
//! scrubbed by the ordinary `verify`; a store that does not verify is refused (repair it into a
//! new current store instead, `repair_dir` reads older stores through the same view).

use std::io::{self, Read};
use std::sync::Arc;

use super::super::codec::{crc, put_u32};
use super::super::dir::{DirEntry, DirLockGuard, EntryKind, LockKind, StoreDir, StoreFile};
use super::super::error::PersistError;
use super::files::{
    self, DirLock, DirPath, GRAPH_COPY, GRAPH_FILE, GraphFile, Intent, STORE_VERSION,
};
use super::maintenance::{self, VerifyReport};
use super::migrate_models;
use super::{BackupMarker, Store};
use crate::graph::GraphStorage;
use crate::graph::system_journal::{Event, catalogue};
use crate::persist::StoreFailure;

/// How a store of one store format version becomes one of the current version (format spec
/// 15.1, the table [`UPGRADES`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeMethod {
    /// The store already has the version: nothing to do (in place: `already_current`; a
    /// copying upgrade is refused with [`StoreFailure::AlreadyCurrent`]).
    Nothing,
    /// Only the version word of `GRAPH` and `GRAPH.copy` (and their checksums) changes: every
    /// other file is read by this build as it is. Safe in place (two atomic replacements, each
    /// state readable) and into a copy. No row uses it since store format version 4 (every
    /// older row migrates; a store without models migrates by the version word alone).
    #[allow(dead_code)]
    InPlaceVersionWord,
    /// The version word plus the migration of the embedding models a Graphersal 0.4.0 store
    /// keeps under `models/` (`migrate_models.rs`): each becomes a blob, ONE commit defines
    /// them, `models/` is removed, all under an `INTENT` (format spec 11.1, 15.1). A store
    /// without `models/` changes only its version word. In place and into a copy.
    Migrate,
    /// The upgrade must write a NEW copy ([`Store::upgrade`]); [`Store::upgrade_in_place`]
    /// refuses it with [`StoreFailure::UpgradeNeedsCopy`] before anything is written. No row
    /// uses it yet: a row that does needs its conversion in `upgrade_copy` (today's copy
    /// rewrites only the version word).
    #[allow(dead_code)]
    CopyOnly,
    /// This build does not upgrade the version: refused with [`PersistError::OlderFormat`],
    /// whose help is the migration through a packed snapshot exported by the build that wrote
    /// the store. A version without a row is refused the same way.
    Unsupported,
}

#[cfg(test)]
impl UpgradeMethod {
    /// The name the book's table uses (`store_versions.md`, format spec 15.1).
    pub(crate) fn label(self) -> &'static str {
        match self {
            UpgradeMethod::Nothing => "nothing to do",
            UpgradeMethod::InPlaceVersionWord => "in place or into a copy (version word)",
            UpgradeMethod::Migrate => "in place or into a copy (migration)",
            UpgradeMethod::CopyOnly => "into a copy only",
            UpgradeMethod::Unsupported => "unsupported (packed snapshot)",
        }
    }
}

/// One row of the upgrade table: from store format version `from` to `to` by `method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UpgradeRow {
    pub(crate) from: u32,
    pub(crate) to: u32,
    pub(crate) method: UpgradeMethod,
    /// Which Graphersal wrote stores of `from` (for the book).
    pub(crate) written_by: &'static str,
}

/// THE upgrade compatibility table (format spec 15.1): one row per older store format version
/// to the current one. An upgrade is done only as its row says; a version without a row is
/// refused before anything is written. Bumping `STORE_VERSION` (or the file format version)
/// fails the test `the_upgrade_table_covers_every_version` until the table is reviewed: every
/// row's target moved, the previous version given its method, `UPGRADES_REVIEWED_FOR`
/// updated, and the book's copies of the table (`store_versions.md`, `format.md` 15.1)
/// changed with it (another test compares them).
pub(crate) const UPGRADES: &[UpgradeRow] = &[
    UpgradeRow {
        from: 1,
        to: 4,
        method: UpgradeMethod::Unsupported,
        written_by: "never released (no store id)",
    },
    UpgradeRow {
        from: 2,
        to: 4,
        method: UpgradeMethod::Migrate,
        written_by: "Graphersal 0.1.x (no models: the version word only)",
    },
    UpgradeRow {
        from: 3,
        to: 4,
        method: UpgradeMethod::Migrate,
        written_by: "Graphersal 0.2.x - 0.4.x",
    },
    UpgradeRow {
        from: 4,
        to: 4,
        method: UpgradeMethod::Nothing,
        written_by: "Graphersal 0.5.x and later",
    },
];

/// The (store format, file format) versions [`UPGRADES`] was last reviewed for. File format
/// version 3 (temporal v2) and 4 (blob references, the manifest's blob table, the system
/// journal's segments) need no row of their own: a store holds files of version 1 to 4, every
/// one of which this build reads as it is, and an older Graphersal refuses a newer file as
/// newer (`refuse_newer_version`, format spec 15), never as damage. Store format version 4
/// (Graphersal 0.5.0) migrates the 0.4.0 `models/` into blobs and catalog definitions.
#[cfg(test)]
pub(crate) const UPGRADES_REVIEWED_FOR: (u32, u32) = (4, 4);

/// The method [`UPGRADES`] gives for a store of store format version `from` (to the current
/// version); [`UpgradeMethod::Unsupported`] without a row.
pub(crate) fn upgrade_method(from: u32) -> UpgradeMethod {
    method_in(UPGRADES, from)
}

fn method_in(table: &[UpgradeRow], from: u32) -> UpgradeMethod {
    table
        .iter()
        .find(|row| row.from == from && row.to == STORE_VERSION)
        .map_or(UpgradeMethod::Unsupported, |row| row.method)
}

/// Whether this build upgrades a store of the older store format version `found` (in place
/// or into a copy): the `OlderFormat` help names the upgrade then, else the migration.
pub(crate) fn upgradable_from(found: u32) -> bool {
    found < STORE_VERSION
        && matches!(
            upgrade_method(found),
            UpgradeMethod::InPlaceVersionWord | UpgradeMethod::Migrate | UpgradeMethod::CopyOnly
        )
}

/// The name of the marker a copying upgrade writes into its target first and removes last: a
/// target that still holds it is an interrupted upgrade (no `GRAPH` yet, or written but the
/// marker not yet removed), which the next upgrade into it clears and repeats. A `.tmp` name:
/// a copy never carries it and an open removes it.
const UPGRADE_MARKER: &str = "upgrade-in-progress.tmp";

/// What an upgrade did ([`Store::upgrade`], [`Store::upgrade_in_place`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct UpgradeReport {
    /// The store format version the store had (the current one when it needed no upgrade).
    pub from_version: u32,
    /// The store format version it has now ([`STORE_FORMAT_VERSION`](crate::persist::STORE_FORMAT_VERSION)).
    pub to_version: u32,
    /// The store was already of the current version: nothing was written (in place only; a
    /// copying upgrade refuses such a store).
    pub already_current: bool,
    /// Whether the store was upgraded in place (`false`: into a new copy).
    pub in_place: bool,
    /// The backup marker, when the upgraded directory is a backup (it stays one).
    pub backup: Option<BackupMarker>,
    /// Files and bytes written into the copy (`0` in place: only `GRAPH` and `GRAPH.copy`
    /// are rewritten).
    pub files_copied: usize,
    /// See [`Self::files_copied`].
    pub bytes_copied: u64,
    /// The scrub of the older store before anything was written (every checksum of every
    /// snapshot, WAL record, `GRAPH` copy, attic entry): a store with problems is refused.
    pub verify: VerifyReport,
    /// The embedding models of a Graphersal 0.4.0 store (its `models/` directory) that became
    /// blobs with a catalog definition each (format spec 15.1), in the order of their former
    /// directory names; empty for a store without models.
    pub models_converted: Vec<ConvertedModel>,
    /// The commit that defined the converted models (`None`: nothing was committed).
    pub migration_commit: Option<u64>,
    /// The models the schema needs (a vector property's `embedder`) that the store does not
    /// keep: Graphersal 0.4.0 served them through `--embedder`; this version opens such a store
    /// only once they are imported ([`MissingModel::commands`]).
    pub models_missing: Vec<MissingModel>,
    /// The vector properties whose schema declaration names an `embedder` but no source
    /// property `from` (0.4.0's `embed_property` recorded only the model): nothing keeps their
    /// vectors current. The upgrade never guesses a source and never recomputes a vector;
    /// each entry says how to add one ([`VectorWithoutRule::note`]).
    pub vectors_without_rule: Vec<VectorWithoutRule>,
}

/// An embedding model a Graphersal 0.4.0 store kept under `models/`, converted by the upgrade
/// into a blob with a catalog definition ([`UpgradeReport::models_converted`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ConvertedModel {
    repo: String,
    identity: String,
    former_identity: String,
    bytes: u64,
}

impl ConvertedModel {
    /// Its repository (`owner/name`); `local/<12 hex>` for a model 0.4.0 kept without one, or
    /// for a second revision of a repository another model of the store took (the catalog
    /// keeps one model per repository).
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Its identity now (the definition's name, what the schema's `embedder` records).
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Its identity in the 0.4.0 store; differs from [`Self::identity`] when the repository
    /// was renamed (the schema's `embedder` naming it was rewritten in the migration commit).
    pub fn former_identity(&self) -> &str {
        &self.former_identity
    }

    /// Whether the identity changed (see [`Self::former_identity`]).
    pub fn renamed(&self) -> bool {
        self.identity != self.former_identity
    }

    /// The bytes of its blob (the model tar).
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// A model the schema needs that the upgraded store does not keep
/// ([`UpgradeReport::models_missing`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct MissingModel {
    identity: String,
    repo: Option<String>,
    needed_by: Vec<String>,
}

impl MissingModel {
    /// The identity the schema names.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Its repository, when the identity names one.
    pub fn repo(&self) -> Option<&str> {
        self.repo.as_deref()
    }

    /// The schema places (`label.key`) naming it.
    pub fn needed_by(&self) -> &[String] {
        &self.needed_by
    }

    /// The commands that put the model into the store at `store`: a download, then the import
    /// into the closed store (`<repo>` when the identity names no repository).
    pub fn commands(&self, store: &str) -> Vec<String> {
        let repo = self.repo.as_deref().unwrap_or("<repo>");
        vec![
            format!("graphersal embed-model download {repo}"),
            format!("graphersal embed-model import {repo} --into {store}"),
        ]
    }
}

/// A vector property with a model (`embedder`) but no vector rule (`from`) after an upgrade
/// ([`UpgradeReport::vectors_without_rule`]): its vectors are plain data that nothing keeps
/// current. Adding `"from": "<source property>"` to its declaration makes it a vector rule;
/// that first fill re-embeds EVERY element of the label while the whole database is locked
/// (book page "Vector Rules").
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VectorWithoutRule {
    kind: crate::graph::schema::ElementKind,
    label: String,
    property: String,
    embedder: String,
    model: String,
    elements: u64,
    not_counted: u64,
}

impl VectorWithoutRule {
    /// Whether the label is a vertex or an edge label.
    pub fn element_kind(&self) -> crate::graph::schema::ElementKind {
        self.kind
    }

    /// The label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The vector property.
    pub fn property(&self) -> &str {
        &self.property
    }

    /// The model identity the declaration's `embedder` names.
    pub fn embedder(&self) -> &str {
        &self.embedder
    }

    /// The model: its repository when the store defines it, else the `embedder` text
    /// (`hash-ngram/1`, or the identity of a model the store lacks).
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The elements of the label holding a vector under the property, counted in the newest
    /// snapshot (see [`Self::exact`]).
    pub fn elements(&self) -> u64 {
        self.elements
    }

    /// Whether [`Self::elements`] is exact: `false` when commits after the newest snapshot may
    /// have added or removed vectors ([`Self::commits_not_counted`]; they are not read: the
    /// upgrade never loads the graph; a checkpoint before the upgrade makes the count exact).
    pub fn exact(&self) -> bool {
        self.not_counted == 0
    }

    /// The commits after the newest snapshot whose data the count did not read.
    pub fn commits_not_counted(&self) -> u64 {
        self.not_counted
    }

    /// The schema patch (the DSL's `g.patch_schema`) that adds the source property; replace
    /// `<source property>` with the property whose text the vectors were made from.
    pub fn schema_patch(&self) -> String {
        let group = match self.kind {
            crate::graph::schema::ElementKind::Vertex => "vertices",
            crate::graph::schema::ElementKind::Edge => "edges",
        };
        format!(
            "g.patch_schema(#{{{group}: #{{{label}: #{{schema: #{{properties: #{{{property}: \
             #{{from: \"<source property>\"}}}}}}}}}}}})",
            label = rhai_key(&self.label),
            property = rhai_key(&self.property),
        )
    }

    /// The text every front end shows for it (one line: the CLI prints it as a `note:`), with
    /// the ready command for the store at `store`.
    pub fn note(&self, store: &str) -> String {
        let count = if self.exact() {
            format!("{} element(s)", self.elements)
        } else {
            format!(
                "{} element(s) in the newest snapshot, {} later commit(s) not counted",
                self.elements, self.not_counted
            )
        };
        format!(
            "{}.{} has vectors of {} ({count}) but no source property; to keep it current add \
             \"from\": \"<source property>\" to its schema declaration (graphersal --graph \
             {store} -e '{}'); the first fill re-embeds every {} (the whole database is locked \
             while it runs)",
            self.label,
            self.property,
            self.model,
            self.schema_patch(),
            self.label
        )
    }
}

/// A map key of a Rhai object literal: bare when it is an identifier, else quoted.
fn rhai_key(key: &str) -> String {
    let bare = key
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if bare {
        key.to_string()
    } else {
        format!("{key:?}")
    }
}

/// What one `GRAPH` copy is, for the converter.
enum GraphCopy {
    /// A valid `GRAPH` of an older version (`older_version`).
    Older(u32),
    /// Current, newer, missing, damaged: left to the reader's own diagnosis.
    Other,
}

fn classify(dir: &DirPath, name: &str) -> (GraphCopy, Option<Vec<u8>>) {
    let Ok(bytes) = files::read_bounded(&dir.join(name), 64 << 20) else {
        return (GraphCopy::Other, None);
    };
    let kind = if let Some(version) = GraphFile::older_version(&bytes) {
        GraphCopy::Older(version)
    } else {
        GraphCopy::Other
    };
    (kind, Some(bytes))
}

/// The `GRAPH` bytes of an upgradable older version with the current version word (and its
/// checksum); everything else of the file is kept. `None`: not an upgradable older `GRAPH`.
fn patched(bytes: &[u8]) -> Option<Vec<u8>> {
    let version = GraphFile::older_version(bytes)?;
    if !upgradable_from(version) {
        return None;
    }
    // Every upgradable row converts by the version word (`UPGRADES`): the `GRAPH` layout of
    // versions 2 and 3 is the same (format spec 5.1, 15).
    let mut out = bytes.to_vec();
    let fixed = files::GRAPH_FIXED_BYTES;
    out[8..12].copy_from_slice(&STORE_VERSION.to_le_bytes());
    let mut checksum = Vec::with_capacity(4);
    put_u32(&mut checksum, crc(&out[..fixed - 4]));
    out[fixed - 4..fixed].copy_from_slice(&checksum);
    Some(out)
}

/// The store in `dir` as this build reads it once upgraded, when it is of an upgradable older
/// version (`None`: it is not; the caller keeps its own handling): a read-only view whose
/// `GRAPH` copies carry the current version.
pub(crate) fn older_view(dir: &DirPath) -> Option<DirPath> {
    let mut graph = Vec::new();
    for name in [GRAPH_FILE, GRAPH_COPY] {
        if let (GraphCopy::Older(_), Some(bytes)) = classify(dir, name)
            && let Some(bytes) = patched(&bytes)
        {
            graph.push((name.to_string(), bytes));
        }
    }
    (!graph.is_empty()).then(|| {
        DirPath::root(Arc::new(UpgradeView {
            inner: dir.store_dir().clone(),
            graph,
        }))
    })
}

/// The version found, or the refusal of a store this converter does not take.
fn source_version(dir: &DirPath) -> Result<u32, PersistError> {
    if !dir.join(GRAPH_FILE).exists() && !dir.join(GRAPH_COPY).exists() {
        return Err(PersistError::NotAStore {
            path: dir.display(),
            reason: "it has no GRAPH file".into(),
        });
    }
    let (main, _) = classify(dir, GRAPH_FILE);
    let (copy, _) = classify(dir, GRAPH_COPY);
    let older = |kind: &GraphCopy| match kind {
        GraphCopy::Older(version) => Some(*version),
        _ => None,
    };
    let upgradable = [older(&main), older(&copy)]
        .into_iter()
        .flatten()
        .filter(|version| upgradable_from(*version))
        .max();
    match upgradable {
        Some(version) => Ok(version),
        // The current version, a newer one, a pre-release one, damage: the reader says which.
        None => {
            let read = files::read_graph_file(dir)?;
            super::guard_version(dir, read.file.version)?;
            Ok(read.file.version)
        }
    }
}

/// Scrubs the older store through its upgraded view; problems refuse the upgrade.
fn verify_source(view: &DirPath, source: &DirPath) -> Result<VerifyReport, PersistError> {
    let report = maintenance::verify(view, None)?;
    if !report.is_ok() {
        let first: Vec<String> = report
            .problems
            .iter()
            .take(5)
            .map(|p| format!("{}: {}", p.path, p.reason))
            .collect();
        return Err(PersistError::store(
            "upgrade the store",
            StoreFailure::DamagedSource,
            format!(
                "{} does not verify ({} problem(s): {}{})",
                source.display(),
                report.problems.len(),
                first.join("; "),
                if report.problems.len() > first.len() {
                    "; ..."
                } else {
                    ""
                }
            ),
        ));
    }
    Ok(report)
}

/// [`Store::upgrade_in_place`].
pub(super) fn upgrade_in_place(dir: &DirPath) -> Result<UpgradeReport, PersistError> {
    upgrade_in_place_by(dir, UPGRADES)
}

/// The refusal of an older store whose row (`UPGRADES`) says it is not upgraded in place.
fn needs_copy(dir: &DirPath, from_version: u32) -> PersistError {
    PersistError::store(
        "upgrade the store in place",
        StoreFailure::UpgradeNeedsCopy,
        format!(
            "{} has store format version {from_version}, which this build upgrades to version \
             {STORE_VERSION} only into a new copy",
            dir.display()
        ),
    )
}

/// [`upgrade_in_place`] by the rows of `table` (a test table for the methods no row uses).
fn upgrade_in_place_by(dir: &DirPath, table: &[UpgradeRow]) -> Result<UpgradeReport, PersistError> {
    // A store with files of a NEWER version is never worked on (format spec 15): refused
    // before the lock and before anything is written.
    files::refuse_newer_version(dir)?;
    let _lock = DirLock::exclusive(dir)?;
    let from_version = source_version(dir)?;
    if from_version != STORE_VERSION {
        match method_in(table, from_version) {
            UpgradeMethod::InPlaceVersionWord | UpgradeMethod::Migrate => {}
            UpgradeMethod::CopyOnly => return Err(needs_copy(dir, from_version)),
            UpgradeMethod::Nothing | UpgradeMethod::Unsupported => {
                return Err(not_upgradable(dir, from_version));
            }
        }
    }
    if from_version == STORE_VERSION {
        // An interrupted model migration (its `GRAPH` written already): completed now.
        if let Some(intent) = Intent::read(dir)?
            && intent.op == migrate_models::UPGRADE_MODELS_OP
        {
            let from = migrate_models::intent_from(&intent);
            let file = files::read_graph_file(dir)?.file;
            let migrated = migrate_models::complete(dir, file.clone(), from)?;
            // A crash between the two `GRAPH` writes of the migration commit: both rewritten.
            let read = files::read_graph_file(dir)?;
            if read.repaired.is_some() {
                files::write_graph_file(dir, &read.file)?;
            }
            let report = UpgradeReport {
                from_version: from,
                to_version: STORE_VERSION,
                already_current: false,
                in_place: true,
                backup: file.backup,
                ..UpgradeReport::default()
            };
            return finish(dir, report, migrated);
        }
        return Ok(UpgradeReport {
            from_version,
            to_version: STORE_VERSION,
            already_current: true,
            in_place: true,
            backup: files::read_graph_file(dir)?.file.backup,
            ..UpgradeReport::default()
        });
    }
    // The 0.4.0 models first: a damaged one refuses before anything is written.
    let legacy = migrate_models::check_legacy(dir)?;
    let view = older_view(dir).ok_or_else(|| {
        PersistError::corrupt(dir.display(), 0, "the GRAPH file changed while it was read")
    })?;
    let verify = verify_source(&view, dir)?;
    // The migration's INTENT before GRAPH changes: from here on a 0.4.0 refuses the store (an
    // unknown operation) and this version completes it (`complete_intent`).
    let migrating = has_models_dir(dir)?;
    if migrating {
        migrate_models::begin(dir, from_version, &legacy)?;
    }
    // The version word: GRAPH.copy, then GRAPH, each replaced atomically (format spec 5.3).
    // Every state a crash can leave is readable: both old (the store is unchanged), the copy
    // new and GRAPH old (a reader of this version uses the copy and rewrites GRAPH at its next
    // write, a repeated upgrade does it at once), both new (done).
    let file = files::read_graph_file(&view)?.file;
    files::write_graph_file(dir, &file)?;
    let migrated = if migrating {
        migrate_models::complete(dir, file.clone(), from_version)?
    } else {
        migrate_models::Migrated::default()
    };
    let report = UpgradeReport {
        from_version,
        to_version: STORE_VERSION,
        already_current: false,
        in_place: true,
        backup: file.backup,
        files_copied: 0,
        bytes_copied: 0,
        verify,
        ..UpgradeReport::default()
    };
    finish(dir, report, migrated)
}

/// Whether the store in `dir` has a `models/` directory (Graphersal 0.4.0).
fn has_models_dir(dir: &DirPath) -> Result<bool, PersistError> {
    Ok(super::models::models_dir(dir)
        .try_kind()
        .map_err(|err| PersistError::io("reading the store's models", err))?
        == Some(files::EntryKind::Dir))
}

/// The report of a finished upgrade of the store in `dir` (now of the current version): the
/// migrated models, the models the schema needs but the store lacks, the vector properties
/// without a rule; the journal record `store.upgraded`.
fn finish(
    dir: &DirPath,
    mut report: UpgradeReport,
    migrated: migrate_models::Migrated,
) -> Result<UpgradeReport, PersistError> {
    report.models_converted = migrated
        .converted
        .iter()
        .map(|c| ConvertedModel {
            repo: c.repo.clone(),
            identity: c.identity.clone(),
            former_identity: c.former_identity.clone(),
            bytes: c.bytes,
        })
        .collect();
    report.migration_commit = migrated.commit_seq;
    let state = match migrated.state {
        Some(state) => state,
        None => {
            let file = files::read_graph_file(dir)?.file;
            super::closed_commit::catalog_view(dir, &file)?.state
        }
    };
    let schema = state.get_schema().map_err(PersistError::Graph)?;
    let definitions =
        super::model_catalog::model_definitions(&state).map_err(PersistError::Graph)?;
    let defined = |identity: &str| {
        let base = super::model_catalog::base_identity(identity);
        definitions.iter().find(|d| d.name() == base)
    };
    for (identity, places) in super::model_catalog::needed_models(schema.as_ref()) {
        if defined(&identity).is_none() {
            report.models_missing.push(MissingModel {
                repo: crate::value::vector::embed::identity::parse_identity(&identity)
                    .and_then(|parsed| parsed.repo.map(str::to_string)),
                identity,
                needed_by: places,
            });
        }
    }
    // The vector properties with a model but no rule, counted in the newest snapshot.
    let mut wanted: Vec<(bool, String, String)> = Vec::new();
    let mut found: Vec<VectorWithoutRule> = Vec::new();
    if let Some(schema) = &schema {
        let roots = schema
            .vertices
            .iter()
            .map(|(label, v)| (false, label, &v.schema))
            .chain(schema.edges.iter().map(|(label, e)| (true, label, &e.schema)));
        for (edge, label, root) in roots {
            for (key, node) in &root.properties {
                let Some(vector) = node.vector.as_ref() else {
                    continue;
                };
                let (Some(embedder), None) = (vector.embedder.as_deref(), vector.from.as_deref())
                else {
                    continue;
                };
                let model = defined(embedder)
                    .and_then(|d| d.as_model().map(|m| m.repo().to_string()))
                    .unwrap_or_else(|| embedder.to_string());
                wanted.push((edge, label.to_string(), key.to_string()));
                found.push(VectorWithoutRule {
                    kind: if edge {
                        crate::graph::schema::ElementKind::Edge
                    } else {
                        crate::graph::schema::ElementKind::Vertex
                    },
                    label: label.to_string(),
                    property: key.to_string(),
                    embedder: embedder.to_string(),
                    model,
                    elements: 0,
                    not_counted: 0,
                });
            }
        }
    }
    if !wanted.is_empty()
        && let Some((seq, path)) = files::list_snapshots(dir)?.pop()
    {
        let depth = crate::persist::snapshot::ReadOptions::default().max_value_depth();
        let counts = super::merge::count_vectors(seq, path, depth, &wanted)?;
        // The data's last commit: the migration commit holds no data.
        let last = match migrated.commit_seq {
            Some(commit) => commit.saturating_sub(1),
            None => state.last_commit_seq(),
        };
        for (entry, count) in found.iter_mut().zip(counts) {
            entry.elements = count;
            entry.not_counted = last.saturating_sub(seq);
        }
    }
    report.vectors_without_rule = found;
    // The journal record only when the models were migrated (the migration commit wrote the
    // journal already): an upgrade by the version word writes nothing but `GRAPH`.
    if report.models_converted.is_empty() {
        return Ok(report);
    }
    let mut event = Event::new(format!(
        "the store was upgraded from store format version {} to {}",
        report.from_version, report.to_version
    ))
    .count("from_version", u64::from(report.from_version))
    .count("to_version", u64::from(report.to_version))
    .field("in_place", report.in_place)
    .count("models", report.models_converted.len() as u64);
    if let Some(commit) = report.migration_commit {
        event = event.commit(commit);
    }
    super::system_journal::append_closed(dir, None, &catalogue::UPGRADED, event);
    Ok(report)
}

/// [`Store::upgrade`].
pub(super) fn upgrade_copy(from: &DirPath, to: &DirPath) -> Result<UpgradeReport, PersistError> {
    upgrade_copy_by(from, to, UPGRADES)
}

/// The refusal of an older store version without a supported row: [`PersistError::OlderFormat`],
/// whose help is the migration through a packed snapshot.
fn not_upgradable(dir: &DirPath, found: u32) -> PersistError {
    PersistError::OlderFormat {
        path: dir.display(),
        found,
        supported: STORE_VERSION,
    }
}

/// [`upgrade_copy`] by the rows of `table`.
fn upgrade_copy_by(
    from: &DirPath,
    to: &DirPath,
    table: &[UpgradeRow],
) -> Result<UpgradeReport, PersistError> {
    // A store with files of a NEWER version is never worked on (format spec 15).
    files::refuse_newer_version(from)?;
    // The writer lock keeps a writer of the older version (or this one) out while the files
    // are copied; readers take no lock. Nothing of `from` is written.
    let _lock = DirLock::exclusive(from)?;
    let from_version = source_version(from)?;
    if from_version != STORE_VERSION {
        match method_in(table, from_version) {
            // Every supported row converts by the version word (and the model migration,
            // run on the copy); a `CopyOnly` row needs its conversion here when one is added.
            UpgradeMethod::InPlaceVersionWord | UpgradeMethod::Migrate | UpgradeMethod::CopyOnly => {
            }
            UpgradeMethod::Nothing | UpgradeMethod::Unsupported => {
                return Err(not_upgradable(from, from_version));
            }
        }
    }
    if from_version == STORE_VERSION {
        return Err(PersistError::store(
            "upgrade the store",
            StoreFailure::AlreadyCurrent,
            format!(
                "{} already has the current store format version {STORE_VERSION}",
                from.display()
            ),
        ));
    }
    // The 0.4.0 models first: a damaged one refuses before anything is written.
    let legacy = migrate_models::check_legacy(from)?;
    let view = older_view(from).ok_or_else(|| {
        PersistError::corrupt(
            from.display(),
            0,
            "the GRAPH file changed while it was read",
        )
    })?;
    let verify = verify_source(&view, from)?;
    let file = files::read_graph_file(&view)?.file;
    let mut migrated = migrate_models::Migrated::default();

    // An interrupted upgrade into `to` is cleared and repeated; anything else in it refuses.
    if to.join(UPGRADE_MARKER).exists() {
        files::undo_new_dir(to, true);
    }
    if files::has_entries(to) {
        return Err(PersistError::DirectoryNotEmpty { path: to.display() });
    }
    let existed = to.exists();
    let result = (|| {
        to.create_dir_all()
            .map_err(|err| PersistError::io("creating the upgraded store", err))?;
        files::write_atomic(to, UPGRADE_MARKER, from.display().as_bytes())?;
        let mut copied = (0usize, 0u64);
        let mut graph_files = Vec::new();
        super::copy_tree(from, to, from, &mut copied, &mut graph_files)?;
        // The copy is checked before it becomes a store: a view of it with the new GRAPH
        // copies in memory. Until they are written it has no GRAPH, so an interrupted upgrade
        // never leaves something that opens as a store.
        let bytes = file.encode();
        let staged = DirPath::root(Arc::new(UpgradeView {
            inner: to.store_dir().clone(),
            graph: vec![
                (GRAPH_FILE.to_string(), bytes.clone()),
                (GRAPH_COPY.to_string(), bytes.clone()),
            ],
        }));
        let checked = maintenance::verify(&staged, None)?;
        if !checked.is_ok() {
            return Err(PersistError::store(
                "upgrade the store",
                StoreFailure::CopyDoesNotVerify,
                format!(
                    "the copy in {} does not verify: {:?}",
                    to.display(),
                    checked.problems
                ),
            ));
        }
        files::write_graph_file(to, &file)?;
        copied.0 += 2;
        copied.1 += 2 * bytes.len() as u64;
        // The model migration on the copy (the marker stays until it is done).
        if has_models_dir(to)? {
            migrate_models::begin(to, from_version, &legacy)?;
            migrated = migrate_models::complete(to, file.clone(), from_version)?;
        }
        to.join(UPGRADE_MARKER)
            .remove_file()
            .map_err(|err| PersistError::io("finishing the upgraded store", err))?;
        files::sync_dir(to)?;
        Ok(copied)
    })();
    match result {
        Ok((files_copied, bytes_copied)) => finish(
            to,
            UpgradeReport {
                from_version,
                to_version: STORE_VERSION,
                already_current: false,
                in_place: false,
                backup: file.backup,
                files_copied,
                bytes_copied,
                verify,
                ..UpgradeReport::default()
            },
            migrated,
        ),
        Err(err) => {
            files::undo_new_dir(to, existed);
            Err(err)
        }
    }
}

impl Store {
    /// Upgrades the store (or backup directory, or single-file store) in `from`, written by an
    /// older RELEASED Graphersal (store format version 2, Graphersal 0.1.x; 3, Graphersal 0.2.x
    /// to 0.4.x), into a NEW store
    /// in `to` (a new or empty directory, or a new single file such as `x.gstore`) of the
    /// current format, without the older Graphersal (CLI: `graphersal store upgrade <from>
    /// <to>`). Nothing of `from` changes; its writer lock is held while it is copied (a store
    /// still open by a writer is refused with [`PersistError::Locked`]).
    ///
    /// Everything is carried exactly: the graph, its schema and catalog, the whole history
    /// (snapshots and WAL), marks, lineage, attic entries (they stay restorable) and every
    /// creation parameter, the store id included: the copy is the SAME store in the new format
    /// (keep using it, not the older one; an upgraded backup of it continues to take
    /// increments). Only `GRAPH` and `GRAPH.copy` are rewritten; every other file is copied
    /// byte for byte and stays readable as it is (format spec 15.1). A backup directory stays
    /// a backup.
    ///
    /// The embedding models of a Graphersal 0.4.0 store (files under `models/`) are migrated in
    /// the copy: each becomes a blob, ONE commit (principal `upgrade`) defines them and
    /// `models/` is removed; a model without a repository name gets `local/<12 hex>` (and the
    /// schema's `embedder` naming it is renamed in that commit). No default model is set and
    /// no vector is recomputed. A damaged model refuses the upgrade before anything is written
    /// ([`StoreFailure::UpgradeModelDamaged`]). The report lists the converted models, the
    /// models the schema needs that the store lacks ([`UpgradeReport::models_missing`]: the
    /// store opens once they are imported) and the vector properties without a rule
    /// ([`UpgradeReport::vectors_without_rule`]). Take a full backup afterwards: an older
    /// backup does not continue the migrated store.
    ///
    /// The older store is scrubbed first (every checksum); damage is refused with
    /// [`StoreFailure::DamagedSource`] (repair it into a new current store instead:
    /// [`Store::repair_dir`] reads older stores). The copy is verified before its `GRAPH` is
    /// written, so an interrupted upgrade never leaves a store in `to` (the next upgrade into
    /// `to` clears what it left). A store of the current format is refused with
    /// [`StoreFailure::AlreadyCurrent`].
    pub fn upgrade(
        from: impl super::IntoStoreDir,
        to: impl super::IntoStoreDir,
    ) -> Result<UpgradeReport, PersistError> {
        upgrade_copy(
            &DirPath::root(from.into_store_dir()),
            &DirPath::root(to.into_store_dir()),
        )
    }

    /// Upgrades the store (or backup directory, or single-file store) in `dir`, written by an
    /// older RELEASED Graphersal, IN PLACE to the current format (CLI: `graphersal store
    /// upgrade <dir> --in-place`). Carries everything like [`Store::upgrade`]; takes the
    /// writer lock; the store is scrubbed first and refused when damaged, before anything is
    /// written. Then only `GRAPH.copy` and `GRAPH` are replaced (each atomically), so a crash
    /// at any point leaves the store readable, older or upgraded, never losing anything; run
    /// it again to complete it. A Graphersal 0.4.0 store's models are migrated as
    /// [`Store::upgrade`] describes, under an `INTENT` written before `GRAPH` changes: an
    /// interrupted migration is completed by the next read-write open or upgrade. Afterwards a Graphersal of the older version no longer opens
    /// the store (it refuses it as newer): back it up first if you might go back. A store of
    /// the current format is left as it is ([`UpgradeReport::already_current`]).
    pub fn upgrade_in_place(dir: impl super::IntoStoreDir) -> Result<UpgradeReport, PersistError> {
        upgrade_in_place(&DirPath::root(dir.into_store_dir()))
    }
}

/// A read-only view of a store directory whose `GRAPH` copies are served from memory (the
/// upgraded bytes of an older store, or the not yet written ones of an upgraded copy); every
/// other name reads through. Writes are refused: the view only reads (`verify`, a salvage).
#[derive(Debug)]
pub(crate) struct UpgradeView {
    inner: Arc<dyn StoreDir>,
    graph: Vec<(String, Vec<u8>)>,
}

impl UpgradeView {
    fn served(&self, name: &str) -> Option<&[u8]> {
        self.graph
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, bytes)| bytes.as_slice())
    }

    fn refuse() -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a store being upgraded is only read",
        )
    }
}

impl StoreDir for UpgradeView {
    fn location(&self) -> String {
        self.inner.location()
    }

    fn is_volatile(&self) -> bool {
        self.inner.is_volatile()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn local_path(&self) -> Option<&std::path::Path> {
        self.inner.local_path()
    }

    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
        if self.served(name).is_some() {
            return Ok(Some(EntryKind::File));
        }
        self.inner.entry_kind(name)
    }

    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
        let mut entries = self.inner.list(dir)?;
        if dir.is_empty() {
            for (name, _) in &self.graph {
                if !entries.iter().any(|e| &e.name == name) {
                    entries.push(DirEntry::new(name.clone(), EntryKind::File));
                }
            }
        }
        Ok(entries)
    }

    fn file_len(&self, name: &str) -> io::Result<u64> {
        match self.served(name) {
            Some(bytes) => Ok(bytes.len() as u64),
            None => self.inner.file_len(name),
        }
    }

    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        match self.served(name) {
            Some(bytes) if bytes.len() as u64 > max => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{name} is larger than {max} bytes"),
            )),
            Some(bytes) => Ok(bytes.to_vec()),
            None => self.inner.read(name, max),
        }
    }

    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        match self.served(name) {
            Some(bytes) => usize::try_from(offset)
                .ok()
                .and_then(|start| bytes.get(start..start.checked_add(len)?))
                .map(<[u8]>::to_vec)
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof)),
            None => self.inner.read_range(name, offset, len),
        }
    }

    fn open_read(&self, name: &str) -> io::Result<Box<dyn Read + Send>> {
        match self.served(name) {
            Some(bytes) => Ok(Box::new(io::Cursor::new(bytes.to_vec()))),
            None => self.inner.open_read(name),
        }
    }

    fn create_dir_all(&self, _: &str) -> io::Result<()> {
        Err(Self::refuse())
    }

    fn create(&self, _: &str, _: bool) -> io::Result<Box<dyn StoreFile>> {
        Err(Self::refuse())
    }

    fn open_write(&self, _: &str, _: u64) -> io::Result<Box<dyn StoreFile>> {
        Err(Self::refuse())
    }

    fn append(&self, _: &str, _: &[u8]) -> io::Result<()> {
        Err(Self::refuse())
    }

    fn write_atomic(&self, _: &str, _: &[u8]) -> io::Result<()> {
        Err(Self::refuse())
    }

    fn rename(&self, _: &str, _: &str) -> io::Result<()> {
        Err(Self::refuse())
    }

    fn truncate(&self, _: &str, _: u64) -> io::Result<()> {
        Err(Self::refuse())
    }

    fn remove_file(&self, _: &str) -> io::Result<()> {
        Err(Self::refuse())
    }

    fn remove_tree(&self, _: &str) -> io::Result<()> {
        Err(Self::refuse())
    }

    fn sync_dir(&self, _: &str) -> io::Result<()> {
        Ok(())
    }

    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        self.inner.lock(kind)
    }

    fn check(&self) -> io::Result<super::super::dir::DirCheck> {
        self.inner.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::{MemDir, StoreOptions};

    /// The book pages that hold the generated copy of [`UPGRADES`], between
    /// `<!-- upgrades:begin -->` and `<!-- upgrades:end -->`.
    const BOOK_PAGES: [&str; 3] = [
        "persistence/store_versions.md",
        "persistence/format.md",
        "persistence/store_cli.md",
    ];

    /// The table as the book shows it.
    fn book_table() -> String {
        let mut out = String::from(
            "| From store format version | Written by | To | Upgrade |\n|---|---|---|---|\n",
        );
        for row in UPGRADES {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                row.from,
                row.written_by,
                row.to,
                row.method.label()
            ));
        }
        out
    }

    #[test]
    fn the_upgrade_table_covers_every_version() {
        use crate::persist::codec::FORMAT_VERSION;
        assert_eq!(
            UPGRADES_REVIEWED_FOR,
            (STORE_VERSION, FORMAT_VERSION),
            "the store or file format version changed: review the upgrade table UPGRADES \
             (upgrade.rs): give the previous released version its row to the new version (is \
             the version word enough, InPlaceVersionWord, or does it need a copy, CopyOnly?), \
             move every row's target, then update UPGRADES_REVIEWED_FOR and regenerate the \
             book's copies (GRAPHERSAL_UPDATE_BOOK=1)"
        );
        for version in 1..=STORE_VERSION {
            let rows: Vec<_> = UPGRADES.iter().filter(|row| row.from == version).collect();
            assert_eq!(rows.len(), 1, "store format version {version}: {rows:?}");
            assert_eq!(rows[0].to, STORE_VERSION, "{:?}", rows[0]);
            assert_eq!(
                rows[0].method == UpgradeMethod::Nothing,
                version == STORE_VERSION,
                "{:?}",
                rows[0]
            );
        }
        assert!(UPGRADES.iter().all(|row| row.from <= STORE_VERSION));
        // A newer version has no row: it is never upgraded (format spec 15).
        assert_eq!(
            upgrade_method(STORE_VERSION + 1),
            UpgradeMethod::Unsupported
        );
        assert!(!upgradable_from(STORE_VERSION + 1));
    }

    #[test]
    fn the_book_shows_the_upgrade_table() {
        let book = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../book/src");
        let table = book_table();
        let update = std::env::var_os("GRAPHERSAL_UPDATE_BOOK").is_some();
        for page in BOOK_PAGES {
            let path = book.join(page);
            let text = std::fs::read_to_string(&path).unwrap();
            let (begin, end) = ("<!-- upgrades:begin -->\n", "<!-- upgrades:end -->");
            let start = text
                .find(begin)
                .unwrap_or_else(|| panic!("{page}: no {begin}"));
            let stop = text.find(end).unwrap_or_else(|| panic!("{page}: no {end}"));
            let found = &text[start + begin.len()..stop];
            if found == table {
                continue;
            }
            if update {
                let new = format!("{}{table}{}", &text[..start + begin.len()], &text[stop..]);
                std::fs::write(&path, new).unwrap();
            } else {
                panic!(
                    "{page}: the upgrade table differs from UPGRADES (upgrade.rs); regenerate \
                     with GRAPHERSAL_UPDATE_BOOK=1:\n{table}\nfound:\n{found}"
                );
            }
        }
    }

    /// A closed store of the current version in memory, its `GRAPH` copies then set to store
    /// format `version` (checksum recomputed).
    fn store_of_version(version: u32) -> MemDir {
        let dir = MemDir::new();
        Store::create(&dir, StoreOptions::new())
            .unwrap()
            .close()
            .unwrap();
        for name in [GRAPH_FILE, GRAPH_COPY] {
            let mut bytes = dir.read(name, 1 << 20).unwrap();
            bytes[8..12].copy_from_slice(&version.to_le_bytes());
            let fixed = files::GRAPH_FIXED_BYTES;
            let sum = crc(&bytes[..fixed - 4]);
            bytes[fixed - 4..fixed].copy_from_slice(&sum.to_le_bytes());
            dir.write_atomic(name, &bytes).unwrap();
        }
        dir
    }

    fn graph_copies(dir: &MemDir) -> Vec<Vec<u8>> {
        [GRAPH_FILE, GRAPH_COPY]
            .iter()
            .map(|name| dir.read(name, 1 << 20).unwrap())
            .collect()
    }

    fn root(dir: &MemDir) -> DirPath {
        DirPath::root(Arc::new(dir.clone()))
    }

    fn version_of(dir: &MemDir) -> u32 {
        let bytes = dir.read(GRAPH_FILE, 1 << 20).unwrap();
        u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]])
    }

    #[test]
    fn row_1_to_4_is_refused_with_the_packed_snapshot_migration() {
        assert_eq!(upgrade_method(1), UpgradeMethod::Unsupported);
        let dir = store_of_version(1);
        let before = graph_copies(&dir);
        for result in [
            upgrade_in_place(&root(&dir)),
            upgrade_copy(&root(&dir), &root(&MemDir::new())),
        ] {
            match result {
                Err(err @ PersistError::OlderFormat { found: 1, .. }) => {
                    let text = crate::error::format_diagnostic(&err);
                    assert!(text.contains("packed snapshot"), "{text}");
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(graph_copies(&dir), before);
    }

    #[test]
    fn row_2_to_4_upgrades_in_place_and_into_a_copy() {
        assert_eq!(upgrade_method(2), UpgradeMethod::Migrate);
        assert!(upgradable_from(2));
        let dir = store_of_version(2);
        let target = MemDir::new();
        let report = upgrade_copy(&root(&dir), &root(&target)).unwrap();
        assert_eq!((report.from_version, report.to_version), (2, 4));
        assert_eq!((version_of(&dir), version_of(&target)), (2, 4));
        // No models/: the version word only, no migration commit.
        assert_eq!(report.migration_commit, None);
        assert!(report.models_converted.is_empty());
        let report = upgrade_in_place(&root(&dir)).unwrap();
        assert!(report.in_place && !report.already_current);
        assert_eq!(version_of(&dir), 4);
        assert_eq!(report.migration_commit, None);
        Store::open(&dir, StoreOptions::new())
            .unwrap()
            .close()
            .unwrap();
    }

    #[test]
    fn row_4_to_4_has_nothing_to_do() {
        assert_eq!(upgrade_method(STORE_VERSION), UpgradeMethod::Nothing);
        let dir = store_of_version(STORE_VERSION);
        let before = graph_copies(&dir);
        assert!(upgrade_in_place(&root(&dir)).unwrap().already_current);
        match upgrade_copy(&root(&dir), &root(&MemDir::new())) {
            Err(PersistError::Store {
                cause: StoreFailure::AlreadyCurrent,
                ..
            }) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(graph_copies(&dir), before);
    }

    /// A `CopyOnly` row (none today): in place is refused before anything is written, a copy
    /// is made.
    #[test]
    fn a_copy_only_row_refuses_the_in_place_upgrade() {
        let table = [
            UpgradeRow {
                from: 2,
                to: STORE_VERSION,
                method: UpgradeMethod::CopyOnly,
                written_by: "test",
            },
            UpgradeRow {
                from: STORE_VERSION,
                to: STORE_VERSION,
                method: UpgradeMethod::Nothing,
                written_by: "test",
            },
        ];
        let dir = store_of_version(2);
        let before = graph_copies(&dir);
        match upgrade_in_place_by(&root(&dir), &table) {
            Err(
                err @ PersistError::Store {
                    cause: StoreFailure::UpgradeNeedsCopy,
                    ..
                },
            ) => {
                let text = crate::error::format_diagnostic(&err);
                assert!(
                    text.contains("graphersal store upgrade <dir> <new_dir>"),
                    "{text}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(graph_copies(&dir), before);
        let target = MemDir::new();
        upgrade_copy_by(&root(&dir), &root(&target), &table).unwrap();
        assert_eq!(version_of(&target), STORE_VERSION);
        // A version without a row is refused like an unsupported one.
        assert!(matches!(
            upgrade_in_place_by(&root(&dir), &table[1..]),
            Err(PersistError::OlderFormat { found: 2, .. })
        ));
        assert_eq!(graph_copies(&dir), before);
    }
}
