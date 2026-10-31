//! Catalog commits on a CLOSED store (format spec 12.12; book page "Embedding Models in a
//! Store"): a change of the catalog (today: the embedding models) appended to the WAL as ONE
//! ordinary commit WITHOUT loading the graph, in seconds on a store of any size. The next open
//! replays it like any other commit, so it follows the history (rollback, `changes_since`,
//! backups, replicas).
//!
//! The steps (the writer lock held throughout):
//! 1. the preconditions of every closed-store operation ([`closed_store_prelude`], shared
//!    with `Store::checkpoint_dir`): version guard, writer lock, container check, `GRAPH`,
//!    backup refused, interrupted `INTENT` completed, temporary files removed;
//! 2. the catalog view: the newest snapshot's catalog and schema (no element read) with the
//!    schema and catalog changes of the WAL after it replayed onto it, every before image
//!    checked, the data changes read (their frames checked) and skipped
//!    ([`Recovery::catalog_only`]); a damaged record refuses the commit, nothing written;
//! 3. the plan (the caller's: the model planner of `model_catalog.rs`) computes the changes
//!    from the view, writing a blob first when it needs one; no change = nothing written;
//! 4. the commit time from a clock raised to everything the files hold (the WAL tail, marks,
//!    checkpoint records, the system journal's tail);
//! 5. what an open does to the WAL before it writes: a recorded cut of a refused record
//!    applied, a torn tail cut, the `marks` file and the blob ledger made consistent;
//! 6. the commit appended through the Store's own journal writer (segment rotation, a segment
//!    of an older file format version never continued, a failed write cut back or its cut
//!    recorded in `GRAPH`, the ledger frames), always synced;
//! 7. `GRAPH`: a cleanly closed store records the new commit as `closed_at` (a crashed store
//!    stays as it was: the next open recovers it);
//! 8. the system journal records, then the lock is released. No snapshot is written.

use std::sync::{Arc, Mutex};

use super::files::{self, DirLock, DirPath, GraphFile, Intent, list_snapshots, list_wal};
use super::wal_files::{WalShared, open_segment_reader, wal_set_problems};
use super::{
    LoadedFiles, OpenReport, Store, StoreOptions, apply_wal_cut, complete_intent, cut_limit,
    drop_refused_segments, guard_version, new_journal, read_graph_file, remove_path, remove_tmp,
    sync_ledger, sync_marks, system_journal, wal_after,
};
use crate::graph::system_journal::{Event, catalogue};
use crate::graph::{
    ChangeSet, CommitMetadata, GraphStorage, Mutation, PersistentStorage, TraversalGraph,
};
use crate::persist::codec::Codec;
use crate::persist::dir::IntoStoreDir;
use crate::persist::journal::{Durability, DurableEnd};
use crate::persist::recover::{Recovery, RecoveryTarget};
use crate::persist::snapshot::ReadOptions;
use crate::persist::{PersistError, StoreFailure};

/// What the memory checks of a closed-store catalog commit name.
pub(crate) const CLOSED_COMMIT_OPERATION: &str = "closed catalog commit";

/// The options of a catalog commit on a CLOSED store ([`Store::set_default_model_dir`],
/// [`Store::remove_model_dir`]). The commit is always synced before the call returns
/// (durability is not configurable here: one small commit).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ClosedCommitOptions {
    principal: Option<String>,
    codec: Codec,
    wal_segment_bytes: Option<u64>,
    /// Attributes of the commit's metadata (crate-internal: the migration's `upgrade`).
    attributes: Vec<(String, String)>,
}

impl ClosedCommitOptions {
    /// The defaults: no principal, LZ4 record bodies, the default WAL segment size.
    pub fn new() -> Self {
        Self::default()
    }

    /// The principal recorded in the commit's metadata (the CLI records `cli`).
    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// The compression of the commit record's body (default LZ4).
    pub fn with_codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// The size past which the last WAL segment is closed and the commit goes into a new one
    /// (default [`DEFAULT_WAL_SEGMENT_BYTES`](super::DEFAULT_WAL_SEGMENT_BYTES)).
    #[doc(hidden)]
    pub fn with_wal_segment_bytes(mut self, bytes: u64) -> Self {
        self.wal_segment_bytes = Some(bytes);
        self
    }

    /// The same options with the metadata attribute `key` = `value` (crate-internal).
    pub(crate) fn with_attribute(mut self, key: &str, value: &str) -> Self {
        self.attributes.push((key.to_string(), value.to_string()));
        self
    }

    /// The principal (crate-internal reader).
    pub(crate) fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    /// The store options the commit's journal writer takes.
    fn store_options(&self) -> StoreOptions {
        let mut options = StoreOptions::new()
            .with_durability(Durability::EveryCommit)
            .with_codec(self.codec);
        if let Some(bytes) = self.wal_segment_bytes {
            options = options.with_wal_segment_bytes(bytes);
        }
        options
    }
}

/// The preconditions of an operation on a CLOSED store (`Store::checkpoint_dir`, closed-store
/// catalog commits, blob restores): the version guard, a store, the writer lock (held
/// elsewhere: [`PersistError::Locked`]), the container check (damage: refused), `GRAPH` and
/// its version, a backup refused ([`PersistError::IsBackup`]), an interrupted operation
/// (`INTENT`) completed and temporary files removed, as an open does.
pub(crate) fn closed_store_prelude(dir: &DirPath) -> Result<(DirLock, GraphFile), PersistError> {
    files::refuse_other_version(dir)?;
    if !Store::is_store(dir.store_dir())? {
        return Err(PersistError::NotAStore {
            path: dir.display(),
            reason: "it has no GRAPH file".into(),
        });
    }
    let lock = DirLock::exclusive(dir)?;
    let backend = crate::persist::dir::backend_check(&**dir.store_dir());
    if !backend.damage.is_empty() {
        return Err(PersistError::corrupt(
            dir.display(),
            0,
            format!("the container is damaged: {}", backend.damage.join("; ")),
        ));
    }
    let mut graph_file = read_graph_file(dir)?.file;
    guard_version(dir, graph_file.version)?;
    if let Some(marker) = graph_file.backup {
        return Err(PersistError::IsBackup {
            path: dir.display(),
            commit_seq: marker.commit_seq,
        });
    }
    if let Some(intent) = Intent::read(dir)? {
        complete_intent(dir, &intent)?;
        graph_file = read_graph_file(dir)?.file;
    }
    remove_tmp(dir)?;
    Ok((lock, graph_file))
}

/// The catalog state of a closed store's latest position, read without its elements.
pub(crate) struct CatalogView {
    /// An element-less graph with the store's identity, position, time, id sequences, schema
    /// and catalog.
    pub(crate) state: TraversalGraph,
    /// The commits of the WAL after the newest snapshot that were read.
    pub(crate) wal_tail_commits: u64,
}

/// What the plan of a closed-store catalog commit returns.
pub(crate) struct CatalogPlan {
    /// The schema and catalog changes of the commit (empty: nothing to do, nothing written).
    pub(crate) mutations: Vec<Mutation>,
    /// The journal events of the change, written after the commit with its sequence number
    /// and the field `closed`.
    pub(crate) events: Vec<(&'static catalogue::EventKind, Event)>,
}

/// What a closed-store catalog commit did.
pub(crate) struct ClosedCommit {
    /// The commit appended; `None` when the plan changed nothing.
    pub(crate) commit_seq: Option<u64>,
    /// Its time.
    pub(crate) committed_at: Option<i64>,
    /// The catalog state after it (before it when nothing changed).
    pub(crate) state: TraversalGraph,
}

/// The WAL after the newest snapshot as the view read it, and what an open would fix before
/// writing.
struct Tail {
    /// Every WAL segment the commit may continue (after a recorded cut), in order.
    segments: Vec<(u64, DirPath)>,
    /// The segments after the newest snapshot.
    replay: Vec<(u64, DirPath)>,
    /// A last segment newer than the recorded head and shorter than its header (a crash while
    /// it was being started): removed before writing.
    unstarted: Option<DirPath>,
    /// The torn tail of the last segment: `(segment, offset)` where its valid records end.
    torn: Option<(DirPath, u64)>,
    marks: Vec<crate::graph::hooks::Mark>,
    clock_floor: i64,
}

/// Reads the catalog view of the closed store in `dir` (format spec 12.12): nothing written.
fn read_view(dir: &DirPath, graph_file: &GraphFile) -> Result<(CatalogView, Tail), PersistError> {
    let depth = ReadOptions::default().max_value_depth();
    // The WAL as an open would see it: nothing after a recorded cut of a refused record, the
    // set of segments checked against GRAPH (format spec 9.8).
    let mut segments = list_wal(dir)?;
    drop_refused_segments(&mut segments, graph_file.wal_cut);
    let mut checked = graph_file.clone();
    if let Some((cut, _)) = graph_file.wal_cut
        && checked.wal_head.is_some_and(|head| head > cut)
    {
        checked.wal_head = Some(cut);
    }
    if let Some((name, reason)) = wal_set_problems(&segments, &checked).into_iter().next() {
        return Err(PersistError::corrupt(dir.join(name).display(), 0, reason));
    }
    let mut unstarted = None;
    if let Some((first, path)) = segments.last().cloned()
        && checked.wal_head.is_none_or(|head| first > head)
        && path
            .len()
            .map_err(|err| PersistError::io("reading the last WAL segment", err))?
            < crate::persist::wal::WAL_HEADER_BYTES as u64
    {
        segments.pop();
        unstarted = Some(path);
    }
    let Some((snapshot_seq, snapshot_path)) = list_snapshots(dir)?.pop() else {
        return Err(PersistError::NotAStore {
            path: dir.display(),
            reason: "it has no snapshot".into(),
        });
    };
    let (state, manifest) = super::merge::base_catalog(snapshot_seq, snapshot_path, depth)?;
    // A state this version must not write (format spec 16 rule 3): the store would open
    // read-only here.
    if let Some(reason) = manifest.critical_segment_reason() {
        return Err(PersistError::ReadOnly {
            path: dir.display(),
            reason,
        });
    }
    let replay = wal_after(&segments, snapshot_seq).to_vec();
    let mut recovery = Recovery::new()
        .start_graph(state)
        .verify_before_images()
        .catalog_only()
        .options({
            let mut read = ReadOptions::default();
            read.operation = Some(CLOSED_COMMIT_OPERATION);
            read
        })
        .chain(graph_file.chain())
        .target(RecoveryTarget::Latest);
    for (first, path) in &replay {
        recovery = recovery.journal(open_segment_reader(
            path,
            cut_limit(graph_file.wal_cut, *first),
        )?);
    }
    let recovered = recovery.run()?;
    let state = recovered.graph;
    let report = recovered.report;
    if state.graph_id() != graph_file.graph_id {
        return Err(PersistError::corrupt(
            dir.join(files::WAL_DIR).display(),
            0,
            "the snapshot and the WAL end in another lineage than the store's GRAPH file",
        ));
    }
    if graph_file.clean
        && let Some(closed_at) = graph_file.closed_at
        && report.commit_seq < closed_at
    {
        return Err(PersistError::corrupt(
            dir.join(files::WAL_DIR).display(),
            0,
            format!(
                "the store was closed cleanly at commit {closed_at}, but its files end at commit \
                 {}: the WAL after it is lost",
                report.commit_seq
            ),
        ));
    }
    if let Some(reason) = crate::persist::definitions::critical_unknown(state.definitions.iter()) {
        return Err(PersistError::ReadOnly {
            path: dir.display(),
            reason,
        });
    }
    let torn = report.torn_tail.as_ref().and_then(|torn| {
        replay
            .get(torn.journal)
            .map(|(_, path)| (path.clone(), torn.offset))
    });
    let clock_floor = report.clock_floor.max(manifest.fixed.created_at);
    Ok((
        CatalogView {
            state,
            wal_tail_commits: report.commits_replayed,
        },
        Tail {
            segments,
            replay,
            unstarted,
            torn,
            marks: report.marks,
            clock_floor,
        },
    ))
}

/// The journal of a commit hook's refusal as a [`PersistError`].
fn hook_error(err: crate::graph::hooks::HookError) -> PersistError {
    match err.downcast::<PersistError>() {
        Ok(err) => *err,
        Err(other) => PersistError::io(
            "writing a closed-store catalog commit",
            std::io::Error::other(other.to_string()),
        ),
    }
}

/// Appends ONE commit holding the schema and catalog changes `plan` computes from the catalog
/// view of the CLOSED store in `dir` (format spec 12.12; the module documentation lists the
/// steps). `plan` runs under the writer lock and may write blobs (the commit references them;
/// nothing collects them meanwhile). No change planned: nothing is written.
pub(crate) fn commit_catalog_closed(
    dir: &DirPath,
    operation: &'static str,
    options: &ClosedCommitOptions,
    plan: &mut dyn FnMut(&CatalogView, &DirPath) -> Result<CatalogPlan, PersistError>,
) -> Result<ClosedCommit, PersistError> {
    let (lock, graph_file) = closed_store_prelude(dir)?;
    let committed = commit_catalog_locked(dir, graph_file, operation, options, plan);
    drop(lock);
    committed
}

/// The catalog view of the store in `dir` whose `GRAPH` is `graph_file` (read-only; the
/// caller holds the writer lock or works on a store nothing else opens).
pub(crate) fn catalog_view(
    dir: &DirPath,
    graph_file: &GraphFile,
) -> Result<CatalogView, PersistError> {
    read_view(dir, graph_file).map(|(view, _)| view)
}

/// [`commit_catalog_closed`] after its preconditions: the caller holds the writer lock (or the
/// store is a new copy nothing else opens), `graph_file` is the store's current `GRAPH` and no
/// `INTENT` stands in the way (the store migration of `migrate_models.rs` runs its commit
/// under its own `INTENT`). A backup directory is not refused here.
pub(crate) fn commit_catalog_locked(
    dir: &DirPath,
    graph_file: GraphFile,
    operation: &'static str,
    options: &ClosedCommitOptions,
    plan: &mut dyn FnMut(&CatalogView, &DirPath) -> Result<CatalogPlan, PersistError>,
) -> Result<ClosedCommit, PersistError> {
    let started = web_time::Instant::now();
    let (mut view, tail) = read_view(dir, &graph_file)?;
    let planned = plan(&view, dir)?;
    if planned.mutations.is_empty() {
        return Ok(ClosedCommit {
            commit_seq: None,
            committed_at: None,
            state: view.state,
        });
    }
    // The commit time: never earlier than anything the files hold (format spec 2, "Time").
    let clock = crate::graph::CommitClock::new();
    clock.raise(tail.clock_floor);
    clock.raise(PersistentStorage::last_committed_at(&view.state));
    let _ = system_journal::open_journal(system_journal::OpenJournal {
        dir,
        clock: &clock,
        writable: false,
        instance_name: None,
        segment_bytes: system_journal::DEFAULT_JOURNAL_SEGMENT_BYTES,
    });
    let committed_at = clock.now();
    let last = view.state.last_commit_seq();
    let commit_seq = last
        .checked_add(1)
        .ok_or(crate::error::GraphError::SequenceOverflow {
            sequence: crate::error::COMMIT_SEQUENCE,
        })?;
    let (node_sequence, edge_sequence) = view.state.id_sequences();
    let mut builder = ChangeSet::builder(commit_seq, committed_at)
        .with_sequences(node_sequence, edge_sequence)
        .with_metadata({
            let mut metadata = match options.principal() {
                Some(principal) => CommitMetadata::new().with_principal(principal.to_string()),
                None => CommitMetadata::new(),
            };
            for (key, value) in &options.attributes {
                metadata = metadata.with_attribute(key, value.as_str());
            }
            metadata
        });
    let ops = planned.mutations.len() as u64;
    let mut kinds: Vec<String> = Vec::new();
    for mutation in planned.mutations {
        if let Mutation::SetDefinition { kind, .. } = &mutation {
            let kind = kind.to_string();
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        } else if matches!(mutation, Mutation::SetSchema { .. })
            && !kinds.iter().any(|k| k == "schema")
        {
            kinds.push("schema".to_string());
        }
        builder.push(mutation);
    }
    let changes = builder.build().map_err(PersistError::Graph)?;
    // The commit continues the view: its before images are checked as a replay will check
    // them (nothing is written when they do not match).
    PersistentStorage::replay(&mut view.state, &changes, true).map_err(|reason| {
        PersistError::store(
            operation,
            StoreFailure::ChangedConcurrently,
            format!("the planned commit does not continue the store's catalog: {reason}"),
        )
    })?;

    // What an open does to the WAL before it writes (format spec 9.6, 9.7, 9.8).
    let mut graph_file = graph_file;
    if let Some(cut) = graph_file.wal_cut.take() {
        apply_wal_cut(dir, cut)?;
        if graph_file.wal_head.is_some_and(|head| head > cut.0) {
            graph_file.wal_head = Some(cut.0);
        }
        // Recorded before anything is appended: a recorded cut must never cut the new commit.
        super::write_graph_file(dir, &graph_file)?;
    }
    if let Some(path) = &tail.unstarted {
        remove_path(path).map_err(|err| PersistError::io("removing a torn WAL segment", err))?;
        files::sync_dir(&dir.join(files::WAL_DIR))?;
    }
    if let Some((path, offset)) = &tail.torn {
        path.truncate(*offset)
            .map_err(|err| PersistError::io("cutting the torn WAL tail", err))?;
    }
    let store_options = options.store_options();
    let loaded = LoadedFiles {
        segments: tail.segments.clone(),
        replay: tail.replay.clone(),
        marks: tail.marks.clone(),
        read_only: None,
        clock_floor: tail.clock_floor,
    };
    let mut report = OpenReport::default();
    let marks = sync_marks(dir, &loaded, &store_options, &mut report)?;
    sync_ledger(
        dir,
        &loaded,
        (graph_file.graph_id, last),
        graph_file.clean,
        &mut report,
    )?;

    // The commit, through the Store's journal writer.
    let clean = graph_file.clean;
    let shared_file = Arc::new(Mutex::new(graph_file));
    let wal = Arc::new(WalShared::default());
    let (mut journal, _durable) = new_journal(
        dir,
        (view.state.graph_id(), last),
        &store_options,
        &wal,
        &shared_file,
        &tail.segments,
        &marks,
        DurableEnd::default(),
    )?;
    crate::graph::hooks::CommitHook::before_commit(&mut journal, &changes).map_err(hook_error)?;
    drop(journal);
    // A cleanly closed store ends at the new commit (format spec 9.8: closed_at moves forward;
    // a crash before this write leaves a WAL ending later than closed_at, which is valid).
    {
        let mut file = shared_file.lock().map_err(|_| {
            PersistError::store(
                operation,
                StoreFailure::LockPoisoned,
                "a store lock is poisoned",
            )
        })?;
        if clean {
            file.closed_at = Some(commit_seq);
        }
        super::write_graph_file(dir, &file)?;
    }

    // The system journal (format spec 4.7): the change's records and the commit's.
    let journal = system_journal::open_journal(system_journal::OpenJournal {
        dir,
        clock: &clock,
        writable: true,
        instance_name: None,
        segment_bytes: system_journal::DEFAULT_JOURNAL_SEGMENT_BYTES,
    });
    for (kind, event) in planned.events {
        journal.emit(kind, event.commit(commit_seq).field("closed", true));
    }
    journal.emit(
        &catalogue::CATALOG_CLOSED_COMMIT,
        Event::new(format!(
            "a catalog commit was appended to the closed store ({ops} change(s) of {})",
            kinds.join(", ")
        ))
        .commit(commit_seq)
        .count("ops", ops)
        .field(
            "kinds",
            crate::value::ElementProperty::Array(
                kinds
                    .iter()
                    .map(|k| crate::value::ElementProperty::from(k.as_str()))
                    .collect(),
            ),
        )
        .count("wal_tail_commits", view.wal_tail_commits)
        .count("ms", started.elapsed().as_millis() as u64),
    );
    journal.detach_writer();
    Ok(ClosedCommit {
        commit_seq: Some(commit_seq),
        committed_at: Some(committed_at),
        state: view.state,
    })
}

/// The models of a catalog state with their blob states in the store at `dir`.
fn model_infos(dir: &DirPath, state: &TraversalGraph) -> Vec<super::ModelInfo> {
    let schema = state.get_schema().ok().flatten();
    let definitions: Vec<_> = state
        .definitions
        .iter()
        .filter(|d| d.kind() == crate::graph::DefinitionKind::MODEL)
        .cloned()
        .collect();
    super::model_catalog::infos_of(&definitions, schema.as_ref(), &|model| {
        super::model_catalog::blob_state(dir, model.blob())
    })
}

impl Store {
    /// Imports an embedding model into the CLOSED store in `dir` without loading its graph
    /// (CLI: `graphersal embed-model import <repo> --into <dir>`; format spec 12.12): the
    /// writer lock is taken (a store open elsewhere is refused with [`PersistError::Locked`]:
    /// import into the open store with [`Store::import_model`] instead), the catalog of the
    /// latest position is read from the newest snapshot and the WAL after it, the model's files
    /// are written as one blob and ONE commit defining it is appended to the WAL (synced). The
    /// same semantics, refusals and report as [`Store::import_model`]; a store whose schema
    /// needs a missing model takes the import (it is how the model comes back). Refused on a
    /// backup, on damage (open the store, it opens in maintenance mode, and repair it), on a
    /// store this version may only read (a critical definition or a schema of a newer
    /// version). A replace whose old revision's vectors have vector rules re-embeds them in
    /// the same commit, which needs the data: then (only then) the store is OPENED, its graph
    /// loaded and [`Store::import_model`] run; [`ImportModelReport::reembedded`](super::ImportModelReport::reembedded)
    /// names the properties and the first warning says so. Vectors of the old revision without
    /// a rule refuse the replace ([`StoreFailure::ModelInUse`]) on both paths.
    pub fn import_model_dir(
        dir: impl IntoStoreDir,
        source: super::ModelSource,
        options: super::ImportModelOptions,
    ) -> Result<super::ImportModelReport, PersistError> {
        use super::model_catalog::{
            ImportPlan, Prepared, import_changes, import_event, model_info, plan_import, prepare,
            unknown, warnings_of, write_model_blob,
        };
        const OP: &str = "import a model";
        let dir = DirPath::root(dir.into_store_dir());
        let prepared: Prepared = prepare(source, &options)?;
        let mut warnings = warnings_of(&prepared);
        let identity = prepared.identity.clone();
        let repo = prepared.manifest.repo.clone();
        let mut prepared = Some(prepared);
        let mut outcome: Option<(bool, Option<crate::graph::definition::Definition>)> = None;
        let mut check_warnings = Vec::new();
        // K1: a replace whose old revision's vector rules re-embed data needs the graph.
        let mut needs_open = false;
        let closed = ClosedCommitOptions {
            principal: options.principal().map(str::to_string),
            ..ClosedCommitOptions::default()
        };
        let committed = commit_catalog_closed(&dir, OP, &closed, &mut |view, dir| {
            let state = &view.state;
            let definitions = super::model_catalog::model_definitions(state)?;
            let schema = state.get_schema()?;
            match plan_import(
                &definitions,
                schema.as_ref(),
                &identity,
                &repo,
                options.replace(),
            )? {
                ImportPlan::Present => {
                    outcome = Some((true, None));
                    if !options.default_model() {
                        return Ok(CatalogPlan {
                            mutations: Vec::new(),
                            events: Vec::new(),
                        });
                    }
                    let from = super::model_catalog::default_repo(&definitions);
                    let mutations =
                        super::model_catalog::default_changes(&definitions, Some(&identity));
                    let events = if mutations.is_empty() {
                        Vec::new()
                    } else {
                        vec![super::model_catalog::default_event(
                            from.as_deref(),
                            Some(&repo),
                        )]
                    };
                    Ok(CatalogPlan { mutations, events })
                }
                ImportPlan::New {
                    replaces_rules: true,
                    ..
                } => {
                    needs_open = true;
                    Ok(CatalogPlan {
                        mutations: Vec::new(),
                        events: Vec::new(),
                    })
                }
                ImportPlan::New {
                    old,
                    replaces_rules: false,
                } => {
                    let default = options.default_model()
                        || old.as_ref().is_some_and(|o| {
                            o.as_model().is_some_and(
                                crate::graph::model_definition::ModelDefinition::is_default,
                            )
                        });
                    let Some(prepared) = prepared.take() else {
                        return Err(unknown(OP, &repo));
                    };
                    let (definition, checked) = write_model_blob(
                        prepared,
                        default,
                        &mut |reader| {
                            super::blobs::write_blob(dir, reader, &mut |_| {})
                                .map(|written| written.blob)
                        },
                        dir,
                        None,
                    )?;
                    check_warnings = checked;
                    let mutations = import_changes(&definitions, old.as_ref(), definition.clone());
                    let event = import_event(&definition, old.as_ref().map(|o| o.name()));
                    outcome = Some((false, old));
                    Ok(CatalogPlan {
                        mutations,
                        events: vec![event],
                    })
                }
            }
        })?;
        if needs_open {
            // K1 (format spec 12.12): the replace re-embeds the properties its vector rules
            // name in the replacing unit, which needs the elements: the store is opened (its
            // graph loaded) and the open store's import runs that unit.
            let Some(prepared) = prepared.take() else {
                return Err(unknown(OP, &repo));
            };
            let store = Store::open_at(dir, StoreOptions::new().with_checkpoint_after_bytes(0))?;
            let report = store.import_prepared(prepared, &options);
            store.close()?;
            let mut report = report?;
            report.warnings.insert(
                0,
                format!(
                    "the store was opened (its graph loaded) to re-embed {} with the new \
                     revision in the replacing commit (their vector rules named the old one)",
                    report.reembedded.join(", ")
                ),
            );
            return Ok(report);
        }
        warnings.extend(check_warnings);
        let (already_present, old) = outcome.unwrap_or((true, None));
        let state = &committed.state;
        let definitions =
            super::model_catalog::model_definitions(state).map_err(PersistError::Graph)?;
        let schema = state.get_schema().map_err(PersistError::Graph)?;
        let model = definitions
            .iter()
            .find(|d| d.name() == identity)
            .and_then(|d| {
                d.as_model().map(|m| {
                    model_info(
                        d,
                        m,
                        schema.as_ref(),
                        super::model_catalog::blob_state(&dir, m.blob()),
                    )
                })
            })
            .ok_or_else(|| unknown(OP, &repo))?;
        let replaced = old.as_ref().and_then(|o| {
            o.as_model().map(|m| {
                model_info(
                    o,
                    m,
                    schema.as_ref(),
                    super::model_catalog::blob_state(&dir, m.blob()),
                )
            })
        });
        Ok(super::ImportModelReport {
            model,
            already_present,
            commit_seq: committed.commit_seq,
            replaced,
            reembedded: Vec::new(),
            warnings,
        })
    }

    /// [`Store::set_default_model`] of the CLOSED store in `dir` without loading its graph
    /// (format spec 12.12): one commit appended to its WAL, nothing when the model is the
    /// default already. Locking and refusals as [`Store::import_model_dir`].
    pub fn set_default_model_dir(
        dir: impl IntoStoreDir,
        repo: Option<&str>,
        options: ClosedCommitOptions,
    ) -> Result<super::ModelChangeReport, PersistError> {
        use super::model_catalog::{
            default_changes, default_event, default_repo, model_definitions, of_repo, unknown,
        };
        const OP: &str = "set the default model";
        let dir = DirPath::root(dir.into_store_dir());
        let committed = commit_catalog_closed(&dir, OP, &options, &mut |view, _| {
            let definitions = model_definitions(&view.state)?;
            let target = match repo {
                Some(repo) => Some(of_repo(&definitions, repo).ok_or_else(|| unknown(OP, repo))?),
                None => None,
            };
            let mutations = default_changes(&definitions, target.map(|t| t.name()));
            let events = if mutations.is_empty() {
                Vec::new()
            } else {
                vec![default_event(default_repo(&definitions).as_deref(), repo)]
            };
            Ok(CatalogPlan { mutations, events })
        })?;
        let model = match (committed.commit_seq, repo) {
            (Some(_), Some(repo)) => model_infos(&dir, &committed.state)
                .into_iter()
                .find(|m| m.repo == repo),
            _ => None,
        };
        Ok(super::ModelChangeReport {
            commit_seq: committed.commit_seq,
            committed_at: committed.committed_at,
            model,
        })
    }

    /// [`Store::remove_model`] from the CLOSED store in `dir` without loading its graph (CLI:
    /// `graphersal store models remove <dir> <repo>`; format spec 12.12): one commit dropping
    /// the definition, appended to its WAL. Refused while the schema needs the model
    /// ([`StoreFailure::ModelInUse`]); locking and the other refusals as
    /// [`Store::import_model_dir`].
    pub fn remove_model_dir(
        dir: impl IntoStoreDir,
        repo: &str,
        options: ClosedCommitOptions,
    ) -> Result<super::ModelChangeReport, PersistError> {
        use super::model_catalog::{model_definitions, plan_remove, remove_changes, remove_event};
        const OP: &str = "remove a model";
        let dir = DirPath::root(dir.into_store_dir());
        let mut removed = None;
        let committed = commit_catalog_closed(&dir, OP, &options, &mut |view, dir| {
            let definitions = model_definitions(&view.state)?;
            let schema = view.state.get_schema()?;
            let definition = plan_remove(&definitions, schema.as_ref(), repo)?;
            removed = definition.as_model().map(|m| {
                super::model_catalog::model_info(
                    &definition,
                    m,
                    schema.as_ref(),
                    super::model_catalog::blob_state(dir, m.blob()),
                )
            });
            Ok(CatalogPlan {
                mutations: remove_changes(&definition),
                events: vec![remove_event(&definition)],
            })
        })?;
        Ok(super::ModelChangeReport {
            commit_seq: committed.commit_seq,
            committed_at: committed.committed_at,
            model: removed,
        })
    }
}
