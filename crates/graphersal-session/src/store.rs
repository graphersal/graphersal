//! The Store operations of a session (the playground's Store menu): position, snapshots, marks
//! and attic, Checkpoint and Mark, snapshot downloads, the time travel on any target (a
//! snapshot, a mark, a commit, a time: a read-only view, a fork, an in-place rollback), the
//! attic (changes page by page, restore, fork, remove), verify, the compaction advice and
//! Compact, and a REGISTRY of the stores of the session: the active one plus the forks made in
//! memory, with switch and close.
//!
//! The dev server of `graphersal-cli` (`--graph <dir>`, a store on disk) and the WebAssembly
//! playground (every loaded graph becomes a store in memory, [`Session::set_memory_stores`])
//! answer with these structures; the server adds only HTTP and the default directory of a fork
//! on disk ([`ForkInto::Dir`]).
//!
//! A target is a JSON object: `{"commit": N}`, `{"mark": "name"}` or `{"time": T}` with `T` an
//! RFC 3339 date-time (`"2026-10-08T02:43:00Z"`) or microseconds.

use crate::error::SessionError;
use std::sync::Arc;

use graphersal::changes::{ChangeSet, CommitHook, HookError, HookRefusal, Mutation};
use graphersal::persist::{
    BackupMarker, BackupMode, CompactionAdvice, DamageReport, ForkReport, FoundDamage, MemDir,
    ModelCopy, PersistError, RecoveryTarget, RollbackMode, SnapshotInfo, StoreDir, StoreInfo,
    StoreOptions, StoredModel,
};
use serde_json::{Value, json};

use crate::session::Session;
use crate::storage::SessionStore;

/// The default number of attic commits one page of [`Session::store_attic_changes`] describes.
pub const DEFAULT_CHANGES_PAGE: usize = 100;
/// The largest page of [`Session::store_attic_changes`].
pub const MAX_CHANGES_PAGE: usize = 500;
/// The sample changes listed per commit.
const MAX_SAMPLES: usize = 5;

/// A failed store operation: an HTTP-like status (400 a malformed request, 404 nothing there,
/// 409 not possible in this state, 422 the store refused it, 500 an internal failure) and the
/// diagnostic. `#[non_exhaustive]`: built through its constructors ([`Self::new`],
/// [`Self::bad_request`], [`Self::conflict`], [`Self::refused`], [`Self::internal`], `From`
/// the session's and the engine's errors), so fields can be added later.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoreError {
    /// The status.
    pub status: u16,
    /// The message (with its help).
    pub message: String,
    /// Structured fields a front end acts on, beside the message (an object; the dev server
    /// merges its keys into the error answer): for a Compact that would make marks
    /// unreachable, `{"marksUnreachable": [...]}`. `None` for most errors.
    pub data: Option<Value>,
}

impl StoreError {
    /// An error of `status` with `message` and no structured fields.
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            data: None,
        }
    }

    /// The error with the structured fields `data` (an object, see [`Self::data`]).
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// A malformed request (400).
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
            data: None,
        }
    }

    /// Not possible in this state (409).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: 409,
            message: message.into(),
            data: None,
        }
    }

    /// The store refused the operation (422), with the error's diagnostic.
    pub fn refused(err: PersistError) -> Self {
        Self {
            status: 422,
            message: graphersal::error::format_diagnostic(&err),
            data: None,
        }
    }

    /// An internal failure (500).
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: 500,
            message: message.into(),
            data: None,
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StoreError {}

/// A store refusal (422).
impl From<PersistError> for StoreError {
    fn from(err: PersistError) -> Self {
        StoreError::refused(err)
    }
}

/// A graph that refused the read or write (a view the thread may not have): 409.
impl From<graphersal::error::GraphError> for StoreError {
    fn from(err: graphersal::error::GraphError) -> Self {
        StoreError::conflict(err.to_string())
    }
}

/// A session error with its status: a graph refusal 409, a malformed request 400, a store or
/// engine refusal 422.
impl From<crate::error::SessionError> for StoreError {
    fn from(err: crate::error::SessionError) -> Self {
        use crate::error::SessionError;
        match err {
            SessionError::Graph(err) => StoreError::from(err),
            SessionError::Persist(err) => StoreError::refused(err),
            SessionError::Invalid(text) => StoreError::bad_request(text),
            other => StoreError::new(422, other.to_string()),
        }
    }
}

/// Where a fork goes.
pub enum ForkInto {
    /// A new store in memory, opened and added to the session's databases (the browser).
    Memory,
    /// A new store in this directory (the dev server: a directory next to the store); it is not
    /// opened, the answer says how to open it.
    Dir(Arc<dyn StoreDir>),
}

/// One store of the session.
struct Database {
    id: String,
    label: String,
    store: Arc<dyn SessionStore>,
}

/// The stores of a session: the active one serves the session's graph (unless a read-only
/// view of a past state is open).
pub(crate) struct Stores {
    databases: Vec<Database>,
    active: usize,
    /// The read-only view the session serves instead of the active store's live graph.
    view: Option<Value>,
    next_id: usize,
}

impl Stores {
    fn active(&self) -> Option<&Database> {
        self.databases.get(self.active)
    }
}

/// Refuses every commit of a read-only view, with the view's own message and help.
struct ReadOnlyView {
    refusal: HookRefusal,
}

impl CommitHook for ReadOnlyView {
    fn name(&self) -> &str {
        "read-only view"
    }

    fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
        Err(Box::new(self.refusal.clone()))
    }
}

impl Session {
    /// Every graph this session loads from now on becomes a store in memory (a [`MemDir`]):
    /// commit 0 is the loaded state, every commit after it is journaled, and the Store
    /// operations work on it (the WebAssembly playground). The current graph becomes one now.
    pub fn set_memory_stores(&mut self, yes: bool) {
        self.memory_stores = yes;
        if yes && self.stores.is_none() {
            self.start_memory_store();
        }
    }

    /// Serves `store` as the session's database "main" (the dev server's `--graph <dir>`; a
    /// `Store<S>` of any storage): its graph is the session's graph. Replaces the stores the
    /// session had. Returns the graph info; an error reading it is returned (the store stays
    /// attached).
    pub fn attach_store(
        &mut self,
        store: Arc<dyn SessionStore>,
        label: &str,
    ) -> Result<Value, SessionError> {
        let graph = store.session_graph();
        self.stores = Some(Stores {
            databases: vec![Database {
                id: "main".to_string(),
                label: label.to_string(),
                store,
            }],
            active: 0,
            view: None,
            next_id: 1,
        });
        self.load_shared(graph, label, "store")
    }

    /// Whether the session serves a store.
    pub fn has_store(&self) -> bool {
        self.stores.is_some()
    }

    /// The active store.
    pub fn active_store(&self) -> Option<Arc<dyn SessionStore>> {
        self.stores
            .as_ref()
            .and_then(Stores::active)
            .map(|db| db.store.clone())
    }

    /// The read-only view the session serves (`null`: the live graph).
    pub fn store_view(&self) -> Value {
        self.stores
            .as_ref()
            .and_then(|s| s.view.clone())
            .unwrap_or(Value::Null)
    }

    /// Makes the just loaded graph a store in memory (memory-store mode); drops the old stores.
    /// A failure keeps the plain graph and says why in the graph's warnings. A session whose
    /// storage keeps no stores ([`crate::StorageKind::new`]) serves the plain graph and says so.
    pub(crate) fn start_memory_store(&mut self) {
        // The old stores (and their memory) go with the old graph.
        self.stores = None;
        if !self.factory.supports_stores() {
            self.warnings.push(format!(
                "the graph is not kept as a store in memory: the session's storage ({}) keeps no \
                 stores; the Store menu is off",
                self.factory.storage_name()
            ));
            return;
        }
        // A graph another owner still holds cannot move into a store.
        if Arc::strong_count(&self.graph) != 1 {
            return;
        }
        let graph = std::mem::replace(&mut self.graph, self.factory.empty());
        let dir: Arc<dyn StoreDir> = Arc::new(MemDir::named("memory:main"));
        let created = self.factory.create_store(dir, graph, StoreOptions::new());
        match created {
            Ok(store) => {
                self.graph = store.session_graph();
                self.stores = Some(Stores {
                    databases: vec![Database {
                        id: "main".to_string(),
                        label: self.name.clone(),
                        store,
                    }],
                    active: 0,
                    view: None,
                    next_id: 1,
                });
            }
            Err(err) => {
                self.graph = self.factory.empty();
                self.warnings.push(format!(
                    "the graph could not be kept as a store in memory ({err}); the Store menu \
                     is off"
                ));
            }
        }
    }

    /// The automatic checkpoint of the active store when it is due and no thread runs it
    /// (wasm32), after a run.
    pub(crate) fn run_due_checkpoint(&self) {
        if let Some(store) = self.active_store()
            && let Err(err) = store.run_due_checkpoint()
        {
            // Reported by the Store menu (`lastAutoCheckpointError`); the commits are safe.
            let _ = err;
        }
    }

    fn stores_or_409(&self) -> Result<&Stores, StoreError> {
        self.stores.as_ref().ok_or_else(|| {
            if self.memory_stores && !self.factory.supports_stores() {
                return StoreError::conflict(crate::storage::unsupported_text(
                    "stores",
                    self.factory.storage_name(),
                ));
            }
            StoreError::conflict(
                "there is no store: the dev server runs on one with --graph <dir>; the browser \
                 keeps every loaded graph as one in memory",
            )
        })
    }

    fn store_or_409(&self) -> Result<Arc<dyn SessionStore>, StoreError> {
        self.stores_or_409()?
            .active()
            .map(|db| db.store.clone())
            .ok_or_else(|| StoreError::conflict("no store is active"))
    }

    fn refuse_in_view(&self, what: &str) -> Result<(), StoreError> {
        if self.store_view().is_null() {
            Ok(())
        } else {
            Err(StoreError::conflict(format!(
                "a read-only view of a past state is open: go back to the live graph first to {what}"
            )))
        }
    }

    fn set_view(&mut self, view: Option<Value>) {
        if let Some(stores) = &mut self.stores {
            stores.view = view;
        }
    }

    /// Serves the active store's live graph again; returns the graph info.
    fn serve_live(&mut self) -> Result<Value, StoreError> {
        let stores = self.stores_or_409()?;
        let db = stores
            .active()
            .ok_or_else(|| StoreError::conflict("no store is active"))?;
        let (graph, label) = (db.store.session_graph(), db.label.clone());
        self.set_view(None);
        Ok(self.load_shared(graph, &label, "store")?)
    }

    /// `GET /api/store`: identity, position, snapshots, marks, attic (with whether each entry
    /// can be restored now), the maintenance report, the open view, the sizes, and the
    /// databases of the session.
    pub fn store_info(&self) -> Result<Value, StoreError> {
        let stores = self.stores_or_409()?;
        let store = self.store_or_409()?;
        let info = store
            .info()
            .map_err(|err| StoreError::internal(err.to_string()))?;
        let mut answer = store_value(&info);
        if let Some(entries) = answer["attic"].as_array_mut() {
            for entry in entries {
                let id = entry["id"].as_str().unwrap_or_default().to_string();
                let check = store.attic_can_restore(&id);
                entry["restorable"] = json!(check.is_ok());
                entry["restoreBlocked"] = match check {
                    Ok(()) => Value::Null,
                    Err(PersistError::Maintenance { .. }) => {
                        json!("the store is in maintenance mode (read-only)")
                    }
                    Err(err) => json!(err.to_string()),
                };
            }
        }
        // The store's ONE size: every byte it occupies on its backend (the container file of a
        // single-file store; every file of a directory or a store in memory: snapshots, WAL,
        // attic AND the small files GRAPH, GRAPH.copy, marks, ...), the figure the compaction
        // advice works with. Snapshots + WAL + attic alone (without the small files) would be
        // a second, smaller "store size"; it is only the fallback without an advice.
        // An advice that cannot be computed (an unreadable file of a damaged store) does not
        // hide the rest of the info: it is `compactionError`, beside `compaction: null`.
        let (advice, advice_error) = match store.compaction_advice() {
            Ok(advice) => (Some(advice), None),
            Err(err) => (None, Some(graphersal::error::format_diagnostic(&err))),
        };
        let store_bytes = advice.as_ref().map_or_else(
            || {
                info.snapshot_bytes
                    + info.wal_bytes
                    + info.attic.iter().map(|a| a.bytes).sum::<u64>()
            },
            |advice| advice.total_bytes,
        );
        let graph_bytes = store
            .session_graph()
            .memory_usage()?
            .map(|m| m.total_bytes());
        answer["memory"] = json!(store.dir().is_volatile());
        answer["kind"] = json!(if store.dir().is_volatile() {
            "memory"
        } else if info.space.is_some() {
            "file"
        } else {
            "directory"
        });
        answer["space"] = info.space.map_or(Value::Null, |space| {
            json!({
                "totalBytes": space.total_bytes,
                "totalBytesText": bytes_text(space.total_bytes),
                "liveBytes": space.live_bytes,
                "garbageBytes": space.garbage_bytes(),
                "garbageBytesText": bytes_text(space.garbage_bytes()),
            })
        });
        // Cheap (no data is read): the menu shows it on every refresh.
        answer["compaction"] = advice.as_ref().map_or(Value::Null, advice_value);
        answer["compactionError"] = json!(advice_error);
        answer["storeBytes"] = json!(store_bytes);
        answer["storeBytesText"] = json!(bytes_text(store_bytes));
        answer["graphBytes"] = json!(graph_bytes);
        answer["graphBytesText"] = json!(graph_bytes.map(bytes_text));
        answer["walSinceCheckpoint"] = json!(store.wal_bytes_since_checkpoint());
        answer["lastAutoCheckpointError"] = json!(
            store
                .last_auto_checkpoint_error()?
                .map(|err| err.to_string())
        );
        answer["maintenance"] = store.maintenance().map(damage_value).unwrap_or(Value::Null);
        answer["backupFromMemory"] =
            json!(!info.damage_found.is_empty() && store.maintenance().is_none());
        answer["view"] = stores.view.clone().unwrap_or(Value::Null);
        answer["active"] = json!(stores.active().map(|db| db.id.clone()));
        answer["databases"] = Value::Array(
            stores
                .databases
                .iter()
                .enumerate()
                .map(|(i, db)| {
                    Ok(json!({
                        "id": db.id,
                        "label": db.label,
                        "active": i == stores.active,
                        "commitSeq": db.store.session_graph().last_commit_seq()?,
                        "location": db.store.location(),
                    }))
                })
                .collect::<Result<_, StoreError>>()?,
        );
        Ok(answer)
    }

    /// Checkpoint: `{snapshot}`. Refused in a view.
    pub fn store_checkpoint(&mut self, name: &str) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        self.refuse_in_view("write a snapshot")?;
        let name = name.trim();
        let info = store
            .checkpoint((!name.is_empty()).then_some(name))
            .map_err(StoreError::refused)?;
        Ok(json!({ "snapshot": snapshot_value(&info) }))
    }

    /// Mark: `{mark}`. Refused in a view.
    pub fn store_mark(&mut self, name: &str) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        self.refuse_in_view("set a mark")?;
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::bad_request("expected {\"name\": \"...\"}"));
        }
        let mark = store.mark(name).map_err(StoreError::refused)?;
        Ok(json!({ "mark": {
            "name": mark.name(),
            "commitSeq": mark.commit_seq(),
            "time": mark.time(),
            "timeText": format_time(mark.time()),
        }}))
    }

    /// The packed snapshot (`.gsnap`) of the stored snapshot at `commit` (`None`: the latest):
    /// `(bytes, its commit)`; a stored snapshot is copied as it is (404 when there is none).
    pub fn store_snapshot(&self, commit: Option<u64>) -> Result<(Vec<u8>, u64), StoreError> {
        self.store_snapshot_with(commit, ModelCopy::Include)
    }

    /// [`Self::store_snapshot`] with the store's embedding models ([`ModelCopy::Include`], what
    /// `store_snapshot` does) or without them ([`ModelCopy::Exclude`]): the Store menu's two
    /// downloads (format spec 4.5). A damaged model refuses the download with the models (422).
    pub fn store_snapshot_with(
        &self,
        commit: Option<u64>,
        models: ModelCopy,
    ) -> Result<(Vec<u8>, u64), StoreError> {
        let store = self.store_or_409()?;
        let mut bytes = Vec::new();
        let info = store
            .export_packed_with(commit, &mut bytes as &mut dyn std::io::Write, models)
            .map_err(|err| match err {
                PersistError::Corrupt { .. } => StoreError::refused(err),
                err => StoreError {
                    status: 404,
                    message: graphersal::error::format_diagnostic(&err),
                    data: None,
                },
            })?;
        Ok((bytes, info.commit_seq))
    }

    /// The embedding models of the active store (format spec 4.5), as the Store menu lists
    /// them: `{models: [model_value...], missing: [identity...]}` (`missing`: the identities
    /// the schema names that the store does not keep).
    pub fn store_models(&self) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let info = store
            .info()
            .map_err(|err| StoreError::internal(err.to_string()))?;
        Ok(json!({
            "models": models_value(&info),
            "missing": missing_models(&info),
        }))
    }

    /// Opens a read-only view of the state at `target` (the `target` of `body`): the session
    /// serves it instead of the live graph, a commit hook refuses every write. `{view, graph}`.
    pub fn store_open_view(&mut self, body: &Value) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let target = target_of(body)?;
        let recovered = store
            .read_only_view(target.clone())
            .map_err(StoreError::refused)?;
        let graph = recovered.graph;
        let commit = recovered.commit_seq;
        let committed_at = recovered.committed_at;
        let named = match &target {
            RecoveryTarget::Mark(name) => format!(" (mark \"{name}\")"),
            RecoveryTarget::Time(micros) => format!(" (at {})", format_time(*micros)),
            _ => String::new(),
        };
        let label = format!("Read-only view of commit {commit}{named}");
        let fork = if self.memory_stores {
            format!(
                "fork this state into a new database to change it (Store menu: Fork into a new \
                 database… on commit {commit})"
            )
        } else {
            format!(
                "fork this state into a new directory to change it there (Store menu: Fork to a \
                 new directory… on commit {commit}; CLI: graphersal store fork <dir> <new_dir> \
                 --at-commit {commit})"
            )
        };
        graph
            .register_hook(Box::new(ReadOnlyView {
                refusal: HookRefusal::new(
                    format!(
                        "the served state is a read-only view at commit {commit}{named} of the \
                         store {}: writes are refused",
                        store.location()
                    ),
                    format!(
                        "The view shows a past state and never changes. To write, go back to \
                         the live graph (Store menu: Back to the live graph) and run the write \
                         there, or {fork}."
                    ),
                ),
            }))
            .map_err(|err| StoreError::internal(err.to_string()))?;
        let graph = self.load_shared(graph, &label, "store-view")?;
        let view = json!({
            "commitSeq": commit,
            "target": target.to_string(),
            "label": label,
            "time": committed_at,
            "timeText": format_time(committed_at),
        });
        self.set_view(Some(view.clone()));
        Ok(json!({ "view": view, "graph": graph }))
    }

    /// Back to the live graph from a view: `{view: null, graph}`.
    pub fn store_close_view(&mut self) -> Result<Value, StoreError> {
        let graph = self.serve_live()?;
        Ok(json!({ "view": null, "graph": graph }))
    }

    /// Forks the state at the `target` of `body` into a new store (`into`); the active store is
    /// not changed. In memory the fork is opened and added to the databases (not switched to).
    /// `{fork: {...}, store}`.
    pub fn store_fork(&mut self, body: &Value, into: ForkInto) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let target = target_of(body)?;
        let what = target_text(&target);
        match into {
            ForkInto::Dir(dir) => {
                let location = dir.location();
                let info = store.fork(target, dir).map_err(StoreError::refused)?;
                Ok(json!({ "fork": dir_fork_value(&location, &info, &store.location()) }))
            }
            ForkInto::Memory => {
                let dir = MemDir::named("memory:fork");
                let info = store
                    .fork(target, Arc::new(dir.clone()))
                    .map_err(StoreError::refused)?;
                let parent = self.active_label();
                self.register_memory_fork(dir, format!("fork of {parent} @ {what}"), &info)
            }
        }
    }

    fn active_label(&self) -> String {
        self.stores
            .as_ref()
            .and_then(Stores::active)
            .map_or_else(|| "main".to_string(), |db| db.label.clone())
    }

    fn register_memory_fork(
        &mut self,
        dir: MemDir,
        label: String,
        report: &ForkReport,
    ) -> Result<Value, StoreError> {
        let info = &report.snapshot;
        let opened = self
            .factory
            .open_store(Arc::new(dir), StoreOptions::new())
            .map_err(|message| StoreError::new(422, message.to_string()))?;
        let stores = self
            .stores
            .as_mut()
            .ok_or_else(|| StoreError::conflict("there is no store"))?;
        let id = format!("fork-{}", stores.next_id);
        stores.next_id += 1;
        stores.databases.push(Database {
            id: id.clone(),
            label: label.clone(),
            store: opened,
        });
        Ok(json!({
            "fork": {
                "database": id,
                "label": label,
                "commitSeq": info.commit_seq,
                "graphId": uuid_text(info.graph_id),
                "parentGraphId": uuid_text(info.parent_graph_id),
                "marksNotCarried": marks_value(&report.marks_not_carried),
                "open": "Store menu > Databases > Switch to",
                "undo": "Store menu > Databases > Close (the store you forked from is unchanged)",
            },
            "store": self.store_info()?,
        }))
    }

    /// Rolls the active store back in place to the `target` of `body` (`delete: true` deletes
    /// the history instead of moving it to the attic); the session serves the live graph
    /// afterwards. `{rollback, graph, store}`.
    pub fn store_rollback(&mut self, body: &Value) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let target = target_of(body)?;
        let mode = if body.get("delete").and_then(Value::as_bool) == Some(true) {
            RollbackMode::Delete
        } else {
            RollbackMode::Move
        };
        self.serve_live()?;
        let report = store.rollback_to(target, mode);
        let graph = self.info();
        let report = report.map_err(StoreError::refused)?;
        let graph = graph?;
        let location = store.location();
        let undo = report.attic.as_ref().map(|id| {
            if self.memory_stores {
                "Store menu > Attic > Restore (only while nothing new is committed or marked)"
                    .to_string()
            } else {
                format!(
                    "Store menu > Attic > Restore (only while nothing new is committed or \
                     marked), or: graphersal store attic {location} restore {id}"
                )
            }
        });
        Ok(json!({
            "rollback": {
                "target": report.target,
                "previousCommitSeq": report.previous_commit_seq,
                "graphId": uuid_text(report.graph_id),
                "snapshotsMoved": report.snapshots_moved,
                "segmentsMoved": report.segments_moved,
                "attic": report.attic,
                "undo": undo,
            },
            "graph": graph,
            "store": self.store_info()?,
        }))
    }

    /// One page of what attic entry `id` holds: commits `offset..offset + limit` (`limit`
    /// default [`DEFAULT_CHANGES_PAGE`], at most [`MAX_CHANGES_PAGE`]) each with time, author,
    /// counts by kind and a few sample changes, and the totals over ALL its commits (streamed,
    /// never held at once).
    pub fn store_attic_changes(
        &self,
        id: &str,
        offset: usize,
        limit: Option<usize>,
    ) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let changes = store.attic_changes(id).map_err(StoreError::refused)?;
        changes_page(id, changes, offset, limit)
    }

    /// Undoes the rollback of attic entry `id`; the session serves the live graph.
    /// `{restored: {commitSeq}, graph, store}`.
    pub fn store_attic_restore(&mut self, id: &str) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        self.serve_live()?;
        let restored = store.attic_restore(id);
        let graph = self.info();
        let commit = restored.map_err(StoreError::refused)?;
        let graph = graph?;
        Ok(json!({
            "restored": { "commitSeq": commit },
            "graph": graph,
            "store": self.store_info()?,
        }))
    }

    /// The rolled-back history of attic entry `id` as a new store (`into`). `{fork: {...}}`.
    pub fn store_attic_fork(&mut self, id: &str, into: ForkInto) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        match into {
            ForkInto::Dir(dir) => {
                let location = dir.location();
                let info = store.attic_fork(id, dir).map_err(StoreError::refused)?;
                Ok(json!({ "fork": dir_fork_value(&location, &info, &store.location()) }))
            }
            ForkInto::Memory => {
                let dir = MemDir::named("memory:fork");
                let info = store
                    .attic_fork(id, Arc::new(dir.clone()))
                    .map_err(StoreError::refused)?;
                let parent = self.active_label();
                self.register_memory_fork(dir, format!("attic of {parent} ({id})"), &info)
            }
        }
    }

    /// Deletes attic entry `id` for good. `{removed}`.
    pub fn store_attic_remove(&mut self, id: &str) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        store.attic_remove(id).map_err(StoreError::refused)?;
        Ok(json!({ "removed": id }))
    }

    /// The compaction advice of the active store (cheap, no data is read): `{compaction}`.
    pub fn store_compaction_advice(&self) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let advice = store.compaction_advice().map_err(StoreError::refused)?;
        Ok(json!({ "compaction": advice_value(&advice) }))
    }

    /// Compact: a prune up to the current commit, then the backend's compaction (a single
    /// file is rewritten; a directory or a store in memory is only pruned). Refused in a view.
    ///
    /// A mark is never lost without a question: when the prune would make marks unreachable
    /// (the advice's `pruneMarksUnreachable`) and `drop_marks` is false, nothing is changed and
    /// the answer is a 409 whose `data` is `{"marksUnreachable": [...]}` (the dev server's
    /// `POST /api/store/compact` answers with that list; the playground asks the user and then
    /// sends `{"dropMarks": true}`). The library's `Store::compact` itself never asks.
    ///
    /// `{compact: {snapshotsRemoved, segmentsRemoved, marksUnreachable, bytesBefore, bytesAfter, bytesFreed},
    /// compaction}` (the advice afterwards).
    pub fn store_compact(&mut self, drop_marks: bool) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        self.refuse_in_view("compact the store")?;
        if !drop_marks {
            let advice = store.compaction_advice().map_err(StoreError::refused)?;
            let lost = advice.prune.marks_unreachable;
            if !lost.is_empty() {
                return Err(StoreError {
                    status: 409,
                    message: marks_unreachable_refusal(&lost),
                    data: Some(json!({ "marksUnreachable": lost })),
                });
            }
        }
        let report = store.compact().map_err(StoreError::refused)?;
        let advice = store.compaction_advice().map_err(StoreError::refused)?;
        Ok(json!({
            "compact": {
                "keptFrom": report.prune.kept_from,
                "snapshotsRemoved": report.prune.snapshots_removed,
                "segmentsRemoved": report.prune.segments_removed,
                "marksUnreachable": report.prune.marks_unreachable,
                "compacted": report.compaction.is_some(),
                "bytesBefore": report.compaction.map(|c| c.bytes_before),
                "bytesAfter": report.compaction.map(|c| c.bytes_after),
                "bytesFreed": report.bytes_freed(),
                "bytesFreedText": bytes_text(report.bytes_freed()),
            },
            "compaction": advice_value(&advice),
        }))
    }

    /// A backup of the active store into `into` (format spec 12.6-12.8, 12.11): full into an
    /// empty or missing directory, an increment into a backup of this store (`full`: always
    /// full). Every checksum is read in the store and in the copy; damage fails it (422) and is
    /// damage found while the store is open (its damage policy applies; the store info shows
    /// it). `from_memory`: the graph in memory is written instead, only when damage was found
    /// (nothing is written to the store). Allowed in a view (the files are copied, not the
    /// view). `{backup: {...}, store}` with the store info afterwards.
    pub fn store_backup(
        &mut self,
        into: Arc<dyn StoreDir>,
        full: bool,
        from_memory: bool,
    ) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let location = into.location();
        let mode = if full {
            BackupMode::Full
        } else {
            BackupMode::Auto
        };
        let result = if from_memory {
            store.backup_from_memory(into, mode)
        } else {
            store.backup_with(into, mode)
        };
        let report = result.map_err(StoreError::refused)?;
        Ok(json!({
            "backup": {
                "dir": location,
                "incremental": report.incremental,
                "fromMemory": report.from_memory,
                "commitSeq": report.commit_seq,
                "previousCommitSeq": report.previous_commit_seq,
                "snapshotCommitSeq": report.snapshot_commit_seq,
                "snapshotsCopied": report.snapshots_copied,
                "segmentsCopied": report.segments_copied,
                "bytes": report.bytes,
                "bytesText": bytes_text(report.bytes),
                "files": report.files.len(),
                "warnings": report.warnings,
                "takenAt": report.taken_at,
                "takenText": format_time(report.taken_at),
                "restore": format!("graphersal store restore {location}"),
            },
            "store": self.store_info()?,
        }))
    }

    /// Scrubs the active store (every checksum, commit continuity, lineage): `{verify}`.
    pub fn store_verify(&self) -> Result<Value, StoreError> {
        let store = self.store_or_409()?;
        let report = store.verify().map_err(StoreError::refused)?;
        Ok(json!({ "verify": {
            "ok": report.is_ok(),
            "snapshotsChecked": report.snapshots_checked,
            "segmentsChecked": report.segments_checked,
            "recordsChecked": report.records_checked,
            "firstCommit": report.first_commit,
            "lastCommit": report.last_commit,
            "problems": report.problems.iter().map(|p| json!({
                "path": p.path, "offset": p.offset, "reason": p.reason,
            })).collect::<Vec<_>>(),
            "notes": report.notes,
        }}))
    }

    /// Switches the session to database `id` (its live graph). `{graph, store}`.
    pub fn store_switch(&mut self, id: &str) -> Result<Value, StoreError> {
        let stores = self
            .stores
            .as_mut()
            .ok_or_else(|| StoreError::conflict("there is no store"))?;
        let index = stores
            .databases
            .iter()
            .position(|db| db.id == id)
            .ok_or_else(|| StoreError::new(404, format!("there is no database {id:?}")))?;
        stores.active = index;
        let graph = self.serve_live()?;
        Ok(json!({ "graph": graph, "store": self.store_info()? }))
    }

    /// Closes database `id` (not the active one): the store is closed and its memory freed.
    /// `{closed, store}`.
    pub fn store_close(&mut self, id: &str) -> Result<Value, StoreError> {
        let stores = self
            .stores
            .as_mut()
            .ok_or_else(|| StoreError::conflict("there is no store"))?;
        let index = stores
            .databases
            .iter()
            .position(|db| db.id == id)
            .ok_or_else(|| StoreError::new(404, format!("there is no database {id:?}")))?;
        if index == stores.active {
            return Err(StoreError::conflict(
                "this is the database in use: switch to another one first",
            ));
        }
        let db = stores.databases.remove(index);
        if index < stores.active {
            stores.active -= 1;
        }
        // Removed from the session either way; a failing close is the answer (its files stay
        // as the last durable commit left them).
        db.store.shutdown().map_err(StoreError::refused)?;
        Ok(json!({ "closed": id, "store": self.store_info()? }))
    }
}

// ------------------------------------------------------------------------------- JSON shapes

/// A target from the request body's `target`: `{"commit": N}`, `{"mark": "name"}` or
/// `{"time": T}` (RFC 3339 text or microseconds).
pub fn target_of(body: &Value) -> Result<RecoveryTarget, StoreError> {
    let hint = "expected {\"target\": {\"commit\": N} | {\"mark\": \"name\"} | {\"time\": \
                \"2026-10-08T02:43:00Z\"}}";
    let target = body
        .get("target")
        .filter(|t| t.is_object())
        .ok_or_else(|| StoreError::bad_request(hint))?;
    if let Some(commit) = target.get("commit") {
        return commit
            .as_u64()
            .map(RecoveryTarget::CommitSeq)
            .ok_or_else(|| StoreError::bad_request("commit must be a commit sequence number"));
    }
    if let Some(mark) = target.get("mark") {
        return match mark.as_str().map(str::trim) {
            Some(name) if !name.is_empty() => Ok(RecoveryTarget::Mark(name.to_string())),
            _ => Err(StoreError::bad_request("mark must be a mark name")),
        };
    }
    if let Some(time) = target.get("time") {
        let micros = match time {
            Value::String(text) => parse_time(text).map_err(StoreError::bad_request)?,
            other => other.as_i64().ok_or_else(|| {
                StoreError::bad_request("time must be an RFC 3339 date-time or microseconds")
            })?,
        };
        return Ok(RecoveryTarget::Time(micros));
    }
    Err(StoreError::bad_request(hint))
}

/// How a target reads in a database label: `commit 5`, `mark "m1"`, `2026-10-08T02:43:00Z`.
fn target_text(target: &RecoveryTarget) -> String {
    match target {
        RecoveryTarget::CommitSeq(seq) => format!("commit {seq}"),
        RecoveryTarget::Mark(name) => format!("mark \"{name}\""),
        RecoveryTarget::Time(micros) => format_time(*micros),
        other => other.to_string(),
    }
}

/// A short, file-name-safe label of a target (`commit-5`, `mark-before-import`,
/// `time-20261008T024300Z`), for the default fork directory.
pub fn target_label(target: &RecoveryTarget) -> String {
    match target {
        RecoveryTarget::CommitSeq(seq) => format!("commit-{seq}"),
        RecoveryTarget::Mark(name) => format!("mark-{name}"),
        RecoveryTarget::Time(micros) => {
            format!("time-{}", format_time(*micros).replace(['-', ':'], ""))
        }
        other => other.to_string(),
    }
}

fn dir_fork_value(dir: &str, report: &ForkReport, from: &str) -> Value {
    let info = &report.snapshot;
    json!({
        "dir": dir,
        "commitSeq": info.commit_seq,
        "graphId": uuid_text(info.graph_id),
        "parentGraphId": uuid_text(info.parent_graph_id),
        "marksNotCarried": marks_value(&report.marks_not_carried),
        "open": format!("graphersal --graph {dir} --server --port 0"),
        "undo": format!("delete the directory {dir} ({from} is unchanged)"),
    })
}

/// Marks a fork or repair did not carry: `[{name, commitSeq}]` in order.
fn marks_value(marks: &[graphersal::changes::Mark]) -> Value {
    marks
        .iter()
        .map(|m| json!({ "name": m.name(), "commitSeq": m.commit_seq() }))
        .collect()
}

/// The damage report of a store in maintenance mode, as the UI shows it.
pub fn damage_value(report: &DamageReport) -> Value {
    json!({
        "recoverable": report.is_recoverable(),
        "commitSeq": report.commit_seq,
        "baseSnapshot": report.base_snapshot,
        "lineageApproximateBefore": report.lineage_approximate_before,
        "lineageNote": report.lineage_note(),
        "damage": report.damage.iter().map(|d| json!({
            "kind": d.kind.name(),
            "path": d.path,
            "offset": d.offset,
            "reason": d.reason,
            "donor": d.donor.as_ref().map(ToString::to_string),
            "lost": !d.is_recoverable(),
            "text": d.to_string(),
        })).collect::<Vec<_>>(),
        "notes": report.notes,
        "text": report.to_string(),
    })
}

/// The marker of a backup directory as the UI shows it (a read-only banner): its position,
/// when it was taken, and how to make it the live store.
pub fn backup_value(marker: &BackupMarker, location: &str) -> Value {
    json!({
        "commitSeq": marker.commit_seq,
        "takenAt": marker.taken_at,
        "takenText": format_time(marker.taken_at),
        "restore": format!("graphersal store restore {location}"),
        "fork": format!("graphersal store fork {location} <new_dir>"),
    })
}

/// A snapshot's listing fields as the UI reads them.
pub fn snapshot_value(s: &SnapshotInfo) -> Value {
    json!({
        "commitSeq": s.commit_seq,
        "name": s.name,
        "createdAt": s.created_at,
        "createdText": format_time(s.created_at),
        "lastCommitAt": s.last_commit_at,
        "lastCommitText": format_time(s.last_commit_at),
        "vertexCount": s.vertex_count,
        "edgeCount": s.edge_count,
        "graphId": uuid_text(s.graph_id),
    })
}

/// The refusal of a Compact that would make `marks` unreachable without the confirmation.
fn marks_unreachable_refusal(marks: &[String]) -> String {
    let names = marks
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Compact would make the mark(s) {names} unreachable: the prune removes the history they \
         need, so they cannot be opened, forked or rolled back to afterwards (only from a backup \
         that holds older snapshots). Nothing was changed. Help: confirm with \
         {{\"dropMarks\": true}} to compact anyway, or back the store up first."
    )
}

/// A compaction advice as JSON (`recommended`, `reason`, `decidedBy`, the numbers).
pub fn advice_value(advice: &CompactionAdvice) -> Value {
    json!({
        "recommended": advice.recommended,
        "reason": advice.reason,
        "decidedBy": advice.decided_by.name(),
        "compactable": advice.compactable,
        "totalBytes": advice.total_bytes,
        "totalBytesText": bytes_text(advice.total_bytes),
        "liveBytes": advice.live_bytes,
        "liveBytesText": bytes_text(advice.live_bytes),
        "garbageBytes": advice.garbage_bytes,
        "garbageBytesText": bytes_text(advice.garbage_bytes),
        "garbageRatio": advice.garbage_ratio,
        "pruneBytes": advice.prune_bytes,
        "pruneBytesText": bytes_text(advice.prune_bytes),
        "pruneFirst": advice.prune_first,
        "pruneSnapshots": advice.prune.snapshots_removed,
        "pruneSegments": advice.prune.segments_removed.len(),
        "pruneMarksUnreachable": advice.prune.marks_unreachable,
        "reclaimableBytes": advice.reclaimable_bytes,
        "reclaimableBytesText": bytes_text(advice.reclaimable_bytes),
        "reclaimableRatio": advice.reclaimable_ratio,
        "spaceNeeded": advice.space_needed,
        "spaceNeededText": bytes_text(advice.space_needed),
        "freeSpace": advice.free_space,
        "freeSpaceText": advice.free_space.map(bytes_text),
        "policy": {
            "minGarbageRatio": advice.policy.min_garbage_ratio,
            "minReclaimableBytes": advice.policy.min_reclaimable_bytes,
            "minTotalBytes": advice.policy.min_total_bytes,
        },
    })
}

/// Identity, position, snapshots, marks and attic.
pub fn store_value(info: &StoreInfo) -> Value {
    json!({
        "dir": info.location,
        "formatVersion": info.format_version,
        "graphId": uuid_text(info.graph_id),
        "lineage": info.lineage.iter().map(|e| json!({
            "graphId": uuid_text(e.graph_id),
            "branchedAt": e.branched_at,
        })).collect::<Vec<_>>(),
        "createdAt": info.created_at,
        "commitSeq": info.commit_seq,
        "lastCommitAt": info.last_commit_at,
        "lastCommitText": format_time(info.last_commit_at),
        "durable": info.durable.map(|d| json!({
            "segment": d.segment, "offset": d.offset, "commitSeq": d.commit_seq,
        })),
        "snapshots": info.snapshots.iter().map(snapshot_value).collect::<Vec<_>>(),
        "snapshotBytes": info.snapshot_bytes,
        "marks": info.marks.iter().map(|m| json!({
            "name": m.name(),
            "commitSeq": m.commit_seq(),
            "time": m.time(),
            "timeText": format_time(m.time()),
        })).collect::<Vec<_>>(),
        "backup": info.backup.as_ref().map(|m| backup_value(m, &info.location)),
        "walSegments": info.wal_segments,
        "walBytes": info.wal_bytes,
        "chunkBytes": info.chunk_bytes,
        // The catalog's definitions (saved queries and any other kind).
        "definitions": info.definitions,
        // Per kind (`query`, `compression`, `kind <n>`), for the Store menu.
        "definitionsByKind": info.definitions_by_kind,
        // Fixed when the store was created (book: Persistence, Creation parameters).
        "damagePolicy": info.damage_policy.name(),
        "storeId": uuid_text(info.store_id),
        "createdText": format_time(info.created_at),
        // Damage found on disk while the store is open (by a backup, a checkpoint, verify).
        "damageFound": info.damage_found.iter().map(|found| json!({
            "foundBy": found.found_by,
            "at": found.at,
            "atText": format_time(found.at),
            "problems": found.problems.iter().map(|p| json!({
                "path": p.path, "offset": p.offset, "reason": p.reason,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "frozenByDamage": info.frozen_by_damage,
        // Why the store stopped after a failed in-place rollback or restore (`null`: not).
        "stopped": info.stopped,
        "damageText": found_damage_text(&info.location, &info.damage_found, info.frozen_by_damage),
        // The embedding models the store keeps, with their licences (format spec 4.5), and the
        // identities the schema names that it does not keep.
        "models": models_value(info),
        "missingModels": missing_models(info),
        "attic": info.attic.iter().map(|a| json!({
            "id": a.id,
            "target": a.target,
            "firstCommitSeq": a.target + 1,
            "lastCommitSeq": a.last_commit_seq,
            "time": a.time,
            "timeText": format_time(a.time),
            "reason": a.reason,
            "snapshots": a.snapshots,
            "bytes": a.bytes,
            "bytesText": bytes_text(a.bytes),
        })).collect::<Vec<_>>(),
    })
}

/// One stored model as the Store menu shows it.
pub fn model_value(model: &StoredModel, used: bool) -> Value {
    json!({
        "id": model.id,
        "identity": model.identity,
        "family": model.family,
        "repo": model.repo,
        "revision": model.revision,
        "commit": model.commit,
        "license": model.license,
        "licenseText": model.license_text,
        "baseModels": model.base_models.iter().map(|b| json!({
            "repo": b.repo, "license": b.license,
        })).collect::<Vec<_>>(),
        "gated": model.gated,
        "addedAt": model.added_at,
        "bytes": model.bytes,
        "bytesText": bytes_text(model.bytes),
        "files": model.files.iter().map(|f| json!({
            "name": f.name, "role": f.role, "bytes": f.bytes,
        })).collect::<Vec<_>>(),
        "usedBySchema": used,
        "problem": model.problem,
    })
}

/// Whether the schema's model identity `named` (with or without a `?normalize=` suffix) is the
/// stored model `model` (the same files: the same 12 hex digits).
fn names_model(named: &str, model: &StoredModel) -> bool {
    named
        .split('?')
        .next()
        .and_then(|base| base.rsplit_once('@'))
        .is_some_and(|(_, hex)| model.id.strip_prefix("model2vec-") == Some(hex))
}

/// The models of `info`, each with whether the schema names it.
fn models_value(info: &StoreInfo) -> Vec<Value> {
    info.models
        .iter()
        .map(|model| {
            let used = info
                .schema_models
                .iter()
                .any(|named| names_model(named, model));
            model_value(model, used)
        })
        .collect()
}

/// The model identities the schema of `info` names that the store does not keep.
fn missing_models(info: &StoreInfo) -> Vec<String> {
    info.schema_models
        .iter()
        .filter(|named| !info.models.iter().any(|model| names_model(named, model)))
        .cloned()
        .collect()
}

/// The damage found on disk while `store` is open, as the UI's banner reads it (cheap: no file
/// is read): `{text, frozen, foundBy, policy, problems}`, or `null` while none was found.
/// Fails when the store's lock of that list is poisoned.
pub fn found_damage_value(store: &dyn SessionStore) -> Result<Value, PersistError> {
    let found = store.damage_found()?;
    let Some(first) = found.first() else {
        return Ok(Value::Null);
    };
    let frozen = store.is_frozen_by_damage();
    Ok(json!({
        "text": found_damage_text(&store.location(), &found, frozen),
        "frozen": frozen,
        "foundBy": first.found_by,
        "policy": store.damage_policy()?.name(),
        "problems": found.iter().map(|f| f.problems.len()).sum::<usize>(),
    }))
}

/// What the UI says about damage found while the store at `location` is open (empty when none
/// was): what found it, the first problem, what the damage policy did (`frozen`), what to do.
pub fn found_damage_text(location: &str, found: &[FoundDamage], frozen: bool) -> String {
    let Some(first) = found.first() else {
        return String::new();
    };
    let problem = first
        .problems
        .first()
        .map(|p| {
            format!(
                "{}{}: {}",
                p.path,
                p.offset
                    .map(|o| format!(" at byte {o}"))
                    .unwrap_or_default(),
                p.reason
            )
        })
        .unwrap_or_default();
    let count: usize = found.iter().map(|f| f.problems.len()).sum();
    let effect = if frozen {
        "the store is READ-ONLY now (damage policy maintenance)"
    } else {
        "the store keeps committing (damage policy continue)"
    };
    format!(
        "DAMAGE found on disk by {} at {} ({count} problem(s); first: {problem}): {effect}. The \
         graph in memory is intact: save it with a backup from memory into a new directory, \
         then repair the store: graphersal store repair {} --to <new_dir>.",
        first.found_by,
        format_time(first.at),
        location
    )
}

/// The kind (a counter name) and a one-line text of a change.
fn mutation_text(mutation: &Mutation) -> (&'static str, String) {
    use graphersal::storage::ElementKind;
    let element = |kind: &ElementKind, id: &str| match kind {
        ElementKind::Vertex => format!("vertex {id}"),
        ElementKind::Edge => format!("edge {id}"),
    };
    match mutation {
        Mutation::AddVertex { id, labels, .. } => (
            "addVertex",
            if labels.is_empty() {
                format!("add vertex {id}")
            } else {
                format!("add vertex {id} ({})", labels.join(", "))
            },
        ),
        Mutation::DropVertex { id, .. } => ("dropVertex", format!("drop vertex {id}")),
        Mutation::AddEdge {
            id,
            label,
            out_id,
            in_id,
            ..
        } => (
            "addEdge",
            format!(
                "add edge {id} {out_id} -{}-> {in_id}",
                label.as_deref().unwrap_or("")
            ),
        ),
        Mutation::DropEdge { id, .. } => ("dropEdge", format!("drop edge {id}")),
        Mutation::SetProperty {
            element: e, key, ..
        } => (
            "setProperty",
            format!("set {key:?} on {}", element(&e.kind, &e.id)),
        ),
        Mutation::RemoveProperty {
            element: e, key, ..
        } => (
            "removeProperty",
            format!("remove {key:?} from {}", element(&e.kind, &e.id)),
        ),
        Mutation::AddLabel { id, label, .. } => {
            ("addLabel", format!("add label {label:?} to vertex {id}"))
        }
        Mutation::RemoveLabel { id, label, .. } => (
            "removeLabel",
            format!("remove label {label:?} from vertex {id}"),
        ),
        Mutation::SetEdgeLabel { id, after, .. } => {
            ("setEdgeLabel", format!("set label {after:?} on edge {id}"))
        }
        Mutation::SetSchema { .. } => ("setSchema", "replace the schema".to_string()),
        _ => ("other", "another change".to_string()),
    }
}

/// One page of change sets: `commits` `offset..offset + limit`, `commitCount`,
/// `mutationCount` and `totals` over all of them (streamed).
pub fn changes_page(
    id: &str,
    changes: impl Iterator<Item = Result<ChangeSet, PersistError>>,
    offset: usize,
    limit: Option<usize>,
) -> Result<Value, StoreError> {
    let limit = limit
        .unwrap_or(DEFAULT_CHANGES_PAGE)
        .clamp(1, MAX_CHANGES_PAGE);
    let mut commits = Vec::new();
    let mut totals = serde_json::Map::new();
    let mut count = 0usize;
    let mut mutations = 0usize;
    for change in changes {
        let change = change.map_err(StoreError::refused)?;
        let index = count;
        count += 1;
        mutations += change.mutations().len();
        let listed = index >= offset && index < offset.saturating_add(limit);
        let mut counts = serde_json::Map::new();
        let mut samples = Vec::new();
        for mutation in change.mutations() {
            let (kind, text) = mutation_text(mutation);
            let n = totals.get(kind).and_then(Value::as_u64).unwrap_or(0);
            totals.insert(kind.to_string(), json!(n + 1));
            if listed {
                let n = counts.get(kind).and_then(Value::as_u64).unwrap_or(0);
                counts.insert(kind.to_string(), json!(n + 1));
                if samples.len() < MAX_SAMPLES {
                    samples.push(text);
                }
            }
        }
        if listed {
            commits.push(json!({
                "commitSeq": change.commit_seq(),
                "time": change.committed_at(),
                "timeText": format_time(change.committed_at()),
                "principal": change.metadata().principal(),
                "mutations": change.mutations().len(),
                "counts": counts,
                "samples": samples,
            }));
        }
    }
    Ok(json!({
        "id": id,
        "commitCount": count,
        "mutationCount": mutations,
        "totals": totals,
        "offset": offset,
        "limit": limit,
        "commits": commits,
        "hasMore": offset.saturating_add(commits.len()) < count,
    }))
}

// ------------------------------------------------------------------------------- texts

/// The refusal of a bare date (`2026-10-08`): a day is not a moment, and which moment of it
/// (start or end, in which zone) changes the target, so the help shows both explicit forms.
fn bare_date_message(date: &str) -> String {
    format!(
        "{date:?} is a date without a time and a zone, which is refused (it names no moment): \
         write {date}T23:59:59Z for the end of that day in UTC, or with an offset, e.g. \
         {date}T00:00:00+02:00 for its start at UTC+02:00 (RFC 3339)"
    )
}

/// Parses a time target: microseconds since the Unix epoch, or an RFC 3339 / ISO 8601
/// date-time with a zone (`Z` or an offset `+02:00`, `+0200`, `+02`): `2026-10-08T02:43:00Z`,
/// `2026-10-08T02:43Z` (no seconds), `2026-10-08 02:43:00.5+02:00`. A date-time names the whole
/// unit it is written to: without seconds the whole minute, without fraction the whole second,
/// so the result is the LAST microsecond of it (a time copied from a listing, which shows whole
/// seconds, includes the commit listed with it). A time without a zone is refused (it would
/// depend on the machine's time zone), and so is a bare date (`2026-10-08`): the error shows
/// the explicit forms (`2026-10-08T23:59:59Z`, `2026-10-08T00:00:00+02:00`).
pub fn parse_time(text: &str) -> Result<i64, SessionError> {
    let text = text.trim();
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return text
            .parse::<i64>()
            .map_err(|_| format!("{text:?} is too large for microseconds since the epoch"))
            .map_err(SessionError::from);
    }
    let bad = |why: &str| {
        format!(
            "{text:?} is not a date-time: {why} (expected RFC 3339 like 2026-10-08T02:43:00Z, \
             2026-10-08T02:43Z or 2026-10-08T04:43:00+02:00, or microseconds since the epoch)"
        )
    };
    let bytes = text.as_bytes();
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = bytes.get(from..from + len)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (Some(year), Some(month), Some(day)) = (digits(0, 4), digits(5, 2), digits(8, 2)) else {
        return Err(bad("the date must be YYYY-MM-DD").into());
    };
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return Err(bad("the date must be YYYY-MM-DD").into());
    }
    if bytes.len() == 10 {
        return Err(bare_date_message(text).into());
    }
    if !matches!(bytes.get(10), Some(b'T' | b't' | b' ')) {
        return Err(bad("the time follows the date after a 'T'").into());
    }
    let (Some(hour), Some(minute)) = (digits(11, 2), digits(14, 2)) else {
        return Err(bad("the time must be HH:MM or HH:MM:SS").into());
    };
    if bytes.get(13) != Some(&b':') {
        return Err(bad("the time must be HH:MM or HH:MM:SS").into());
    }
    let mut pos = 16;
    // The microseconds the written precision leaves open (the rest of the unit it names).
    let mut open = 59_999_999;
    let mut second = 0;
    let mut fraction = 0;
    if bytes.get(pos) == Some(&b':') {
        second = digits(pos + 1, 2).ok_or_else(|| bad("the seconds must be two digits"))?;
        pos += 3;
        open = 999_999;
        if matches!(bytes.get(pos), Some(b'.' | b',')) {
            pos += 1;
            let start = pos;
            while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                pos += 1;
            }
            let written = &text[start..pos];
            if written.is_empty() {
                return Err(bad("a fraction needs digits").into());
            }
            let kept: String = written.chars().take(6).collect();
            let scale = 10_i64.pow(6 - kept.len() as u32);
            fraction = kept.parse::<i64>().map_err(|_| bad("bad fraction"))? * scale;
            open = scale - 1;
        }
    }
    let offset_minutes = match bytes.get(pos) {
        Some(b'Z' | b'z') if pos + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-')) => {
            let zone = &text[pos + 1..];
            let (h, m) = match zone.len() {
                2 => (digits(pos + 1, 2), Some(0)),
                4 => (digits(pos + 1, 2), digits(pos + 3, 2)),
                5 if zone.as_bytes()[2] == b':' => (digits(pos + 1, 2), digits(pos + 4, 2)),
                _ => (None, None),
            };
            let (Some(h), Some(m)) = (h, m) else {
                return Err(bad("the offset must be +HH:MM, +HHMM or +HH").into());
            };
            if h > 23 || m > 59 {
                return Err(bad("the offset is out of range").into());
            }
            let minutes = h * 60 + m;
            if *sign == b'-' { -minutes } else { minutes }
        }
        None => {
            return Err(bad(
                "add a zone: Z for UTC or an offset like +02:00 (a local time would depend on \
                 this machine's time zone)",
            )
            .into());
        }
        _ => return Err(bad("unexpected text after the time").into()),
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(bad("the month must be 01..12").into()),
    };
    if day < 1 || day > month_days {
        return Err(bad("the day does not exist in that month").into());
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(bad("the time is out of range").into());
    }
    // Days since 1970-01-01 from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Ok(seconds * 1_000_000 + fraction + open)
}

/// A byte count for people (`512 B`, `1.5 KiB`, `3.2 MiB`).
pub fn bytes_text(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Microseconds since the Unix epoch as RFC 3339 in UTC, whole seconds
/// (`2026-10-08T02:43:00Z`, the form a time target takes back; `-` for none).
pub fn format_time(micros: i64) -> String {
    if micros <= 0 {
        return "-".to_string();
    }
    let secs = micros.div_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// The canonical text of a lineage id.
pub fn uuid_text(id: u128) -> String {
    let h = format!("{id:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RunOptions;

    fn run(session: &mut Session, script: &str) -> Value {
        let answer: Value =
            serde_json::from_str(&session.execute(script, &RunOptions::default())).unwrap();
        assert_eq!(answer["ok"], true, "{answer}");
        answer
    }

    fn memory_session() -> Session {
        let mut session = Session::new();
        session.set_memory_stores(true);
        session.load_sample("modern").unwrap();
        session
    }

    fn add(session: &mut Session, name: &str) {
        run(
            session,
            &format!("g.add_v(\"person\").property(\"name\", \"{name}\").next()"),
        );
    }

    /// A `.gsnap` carrying one embedding model (format spec 4.5), and that model.
    fn packed_with_a_model() -> (Vec<u8>, graphersal::persist::StoredModel) {
        use graphersal::persist::{ModelRole, NewModel, Store};
        use std::io::Cursor;
        let store = Store::create(MemDir::new(), StoreOptions::new()).unwrap();
        let model = store
            .add_model(
                NewModel::new()
                    .with_file(
                        "config.json",
                        ModelRole::Config,
                        Cursor::new(b"{}".to_vec()),
                    )
                    .with_file(
                        "tokenizer.json",
                        ModelRole::Tokenizer,
                        Cursor::new(vec![1; 50]),
                    )
                    .with_file(
                        "model.safetensors",
                        ModelRole::Weights,
                        Cursor::new(vec![2; 4000]),
                    )
                    .with_repo("acme/tiny")
                    .with_license("mit"),
            )
            .unwrap()
            .model;
        let mut bytes = Vec::new();
        store.export_packed(None, &mut bytes).unwrap();
        (bytes, model)
    }

    #[test]
    fn the_models_of_a_loaded_snapshot_stay_with_the_store_and_its_downloads() {
        let (packed, model) = packed_with_a_model();
        let mut s = memory_session();
        s.load_snapshot(&packed, "with-model.gsnap").unwrap();
        let info = s.store_info().unwrap();
        assert_eq!(
            info["models"][0]["identity"],
            model.identity.as_str(),
            "{info}"
        );
        assert_eq!(info["models"][0]["license"], "mit");
        assert_eq!(info["models"][0]["licenseText"], "template");
        assert_eq!(info["models"][0]["usedBySchema"], false);
        assert_eq!(info["missingModels"], json!([]));
        let models = s.store_models().unwrap();
        assert_eq!(models["models"][0]["id"], model.id.as_str());

        let commit = info["snapshots"][0]["commitSeq"].as_u64();
        let (with, _) = s.store_snapshot(commit).unwrap();
        let (without, _) = s.store_snapshot_with(commit, ModelCopy::Exclude).unwrap();
        assert!(without.len() + 4000 < with.len());
        // The download with models loads into a new store with the model again.
        s.load_snapshot(&with, "again.gsnap").unwrap();
        assert_eq!(
            s.store_info().unwrap()["models"][0]["id"],
            model.id.as_str()
        );
        s.load_snapshot(&without, "bare.gsnap").unwrap();
        assert_eq!(s.store_info().unwrap()["models"], json!([]));
    }

    #[test]
    fn a_loaded_graph_is_a_store_in_memory_with_every_operation() {
        let mut s = memory_session();
        let info = s.store_info().unwrap();
        assert_eq!(info["memory"], true);
        assert_eq!(info["commitSeq"], 0);
        assert_eq!(info["databases"][0]["label"], "Modern (TinkerPop)");
        assert!(info["storeBytes"].as_u64().unwrap() > 0);
        // One store size: the same figure the Disk space block's advice works with.
        assert_eq!(
            info["storeBytes"], info["compaction"]["totalBytes"],
            "{info}"
        );
        assert!(info["graphBytes"].as_u64().unwrap() > 0);

        add(&mut s, "ann");
        s.store_mark("after-ann").unwrap();
        let snapshot = s.store_checkpoint("one").unwrap();
        assert_eq!(snapshot["snapshot"]["commitSeq"], 1);
        add(&mut s, "bob");
        let (bytes, commit) = s.store_snapshot(Some(1)).unwrap();
        assert_eq!(commit, 1);
        assert!(!bytes.is_empty());

        // A view: queries read the past, writes are refused, Checkpoint/Mark are 409.
        let view = s
            .store_open_view(&json!({"target": {"mark": "after-ann"}}))
            .unwrap();
        assert_eq!(view["view"]["commitSeq"], 1);
        assert_eq!(s.info().unwrap()["vertexCount"], 7);
        let refused: Value =
            serde_json::from_str(&s.execute("g.add_v(\"x\").next()", &RunOptions::default()))
                .unwrap();
        assert_eq!(refused["ok"], false);
        // The view's own refusal: what is served, what to do; never the generic hook help.
        let text = refused.to_string();
        assert!(
            text.contains(
                "the served state is a read-only view at commit 1 (mark \\\"after-ann\\\")"
            ),
            "{text}"
        );
        assert!(text.contains("writes are refused"), "{text}");
        assert!(
            text.contains("go back to the live graph (Store menu: Back to the live graph)"),
            "{text}"
        );
        assert!(text.contains("Fork into a new database"), "{text}");
        assert!(!text.contains("write-ahead"), "{text}");
        assert!(!text.contains("run the query again"), "{text}");
        assert_eq!(s.store_mark("m").unwrap_err().status, 409);
        s.store_close_view().unwrap();
        assert_eq!(s.info().unwrap()["vertexCount"], 8);

        // Rollback, attic, restore; verify.
        let rolled = s.store_rollback(&json!({"target": {"commit": 1}})).unwrap();
        assert_eq!(rolled["graph"]["vertexCount"], 7);
        let id = rolled["rollback"]["attic"].as_str().unwrap().to_string();
        let page = s.store_attic_changes(&id, 0, None).unwrap();
        assert_eq!(page["commitCount"], 1);
        let restored = s.store_attic_restore(&id).unwrap();
        assert_eq!(restored["restored"]["commitSeq"], 2);
        assert_eq!(s.store_verify().unwrap()["verify"]["ok"], true);
        assert_eq!(s.store_rollback(&json!({})).unwrap_err().status, 400);
    }

    #[test]
    fn forks_are_databases_to_switch_to_and_close() {
        let mut s = memory_session();
        add(&mut s, "ann");
        let fork = s
            .store_fork(&json!({"target": {"commit": 0}}), ForkInto::Memory)
            .unwrap();
        let id = fork["fork"]["database"].as_str().unwrap().to_string();
        assert_eq!(
            fork["fork"]["label"],
            "fork of Modern (TinkerPop) @ commit 0"
        );
        assert_eq!(fork["store"]["databases"].as_array().unwrap().len(), 2);
        // Still on main; the fork is a separate graph.
        assert_eq!(s.info().unwrap()["vertexCount"], 7);
        let switched = s.store_switch(&id).unwrap();
        assert_eq!(switched["graph"]["vertexCount"], 6);
        add(&mut s, "bob");
        assert_eq!(s.store_info().unwrap()["commitSeq"], 1);
        assert_eq!(s.store_close(&id).unwrap_err().status, 409);
        s.store_switch("main").unwrap();
        assert_eq!(s.info().unwrap()["vertexCount"], 7);
        let closed = s.store_close(&id).unwrap();
        assert_eq!(closed["store"]["databases"].as_array().unwrap().len(), 1);
        assert_eq!(s.store_switch(&id).unwrap_err().status, 404);
        // Loading another graph starts over with a new store.
        s.load_sample("tree").unwrap();
        let info = s.store_info().unwrap();
        assert_eq!(info["commitSeq"], 0);
        assert_eq!(info["databases"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn attic_changes_come_page_by_page_with_totals_over_all() {
        let mut s = memory_session();
        for i in 0..7 {
            add(&mut s, &format!("p{i}"));
        }
        let rolled = s.store_rollback(&json!({"target": {"commit": 0}})).unwrap();
        let id = rolled["rollback"]["attic"].as_str().unwrap().to_string();
        let first = s.store_attic_changes(&id, 0, Some(3)).unwrap();
        assert_eq!(first["commitCount"], 7);
        assert_eq!(first["totals"]["addVertex"], 7);
        assert_eq!(first["commits"].as_array().unwrap().len(), 3);
        assert_eq!(first["commits"][0]["commitSeq"], 1);
        assert_eq!(first["hasMore"], true);
        let last = s.store_attic_changes(&id, 6, Some(3)).unwrap();
        assert_eq!(last["commits"].as_array().unwrap().len(), 1);
        assert_eq!(last["commits"][0]["commitSeq"], 7);
        assert_eq!(last["hasMore"], false);
        // The page size is capped; past the end is an empty page.
        let capped = s.store_attic_changes(&id, 0, Some(100_000)).unwrap();
        assert_eq!(capped["limit"], MAX_CHANGES_PAGE);
        let empty = s.store_attic_changes(&id, 50, None).unwrap();
        assert!(empty["commits"].as_array().unwrap().is_empty());
        assert_eq!(empty["limit"], DEFAULT_CHANGES_PAGE);
    }

    #[test]
    fn without_a_store_the_operations_are_409() {
        let mut s = Session::new();
        s.load_sample("modern").unwrap();
        assert!(!s.has_store());
        assert_eq!(s.store_info().unwrap_err().status, 409);
        assert_eq!(s.store_checkpoint("").unwrap_err().status, 409);
        assert_eq!(s.store_compaction_advice().unwrap_err().status, 409);
        assert_eq!(s.store_compact(true).unwrap_err().status, 409);
    }

    #[test]
    fn compact_asks_before_it_makes_marks_unreachable() {
        let mut s = memory_session();
        for (i, mark) in ["m1", "m2", "", ""].iter().enumerate() {
            add(&mut s, &format!("p{i}"));
            if !mark.is_empty() {
                s.store_mark(mark).unwrap();
            }
            s.store_checkpoint("").unwrap();
        }
        let marks = |s: &Session| s.store_info().unwrap()["marks"].as_array().unwrap().len();
        assert_eq!(marks(&s), 2);
        // Without the confirmation: 409 with the list, nothing changed.
        let refused = s.store_compact(false).unwrap_err();
        assert_eq!(refused.status, 409);
        assert_eq!(
            refused.data,
            Some(json!({"marksUnreachable": ["m1", "m2"]}))
        );
        assert!(
            refused
                .message
                .contains("the mark(s) \"m1\", \"m2\" unreachable"),
            "{}",
            refused.message
        );
        assert!(refused.message.contains("dropMarks"), "{}", refused.message);
        assert_eq!(marks(&s), 2);
        assert_eq!(
            s.store_info().unwrap()["snapshots"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        // Confirmed: the prune runs and names them.
        let done = s.store_compact(true).unwrap();
        assert_eq!(done["compact"]["marksUnreachable"], json!(["m1", "m2"]));
        assert_eq!(marks(&s), 0);
        // No mark affected: no confirmation needed.
        add(&mut s, "late");
        s.store_checkpoint("").unwrap();
        assert!(s.store_compact(false).is_ok());
    }

    /// A [`MemDir`] whose reads fail from the `n`-th one on after it is armed.
    #[derive(Debug)]
    struct FailingReads {
        inner: Arc<dyn StoreDir>,
        left: std::sync::atomic::AtomicUsize,
    }

    impl FailingReads {
        fn gate(&self) -> std::io::Result<()> {
            use std::sync::atomic::Ordering;
            match self.left.load(Ordering::Acquire) {
                usize::MAX => Ok(()),
                0 => Err(std::io::Error::other("injected read failure")),
                left => {
                    self.left.store(left - 1, Ordering::Release);
                    Ok(())
                }
            }
        }
    }

    impl StoreDir for FailingReads {
        fn location(&self) -> String {
            self.inner.location()
        }
        fn entry_kind(
            &self,
            name: &str,
        ) -> std::io::Result<Option<graphersal::persist::EntryKind>> {
            self.inner.entry_kind(name)
        }
        fn list(&self, dir: &str) -> std::io::Result<Vec<graphersal::persist::DirEntry>> {
            self.inner.list(dir)
        }
        fn file_len(&self, name: &str) -> std::io::Result<u64> {
            self.inner.file_len(name)
        }
        fn read(&self, name: &str, max: u64) -> std::io::Result<Vec<u8>> {
            self.gate()?;
            self.inner.read(name, max)
        }
        fn read_range(&self, name: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
            self.gate()?;
            self.inner.read_range(name, offset, len)
        }
        fn open_read(&self, name: &str) -> std::io::Result<Box<dyn std::io::Read + Send>> {
            self.gate()?;
            self.inner.open_read(name)
        }
        fn create_dir_all(&self, name: &str) -> std::io::Result<()> {
            self.inner.create_dir_all(name)
        }
        fn create(
            &self,
            name: &str,
            exclusive: bool,
        ) -> std::io::Result<Box<dyn graphersal::persist::StoreFile>> {
            self.inner.create(name, exclusive)
        }
        fn open_write(
            &self,
            name: &str,
            offset: u64,
        ) -> std::io::Result<Box<dyn graphersal::persist::StoreFile>> {
            self.inner.open_write(name, offset)
        }
        fn append(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.inner.append(name, bytes)
        }
        fn write_atomic(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.inner.write_atomic(name, bytes)
        }
        fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
            self.inner.rename(from, to)
        }
        fn truncate(&self, name: &str, len: u64) -> std::io::Result<()> {
            self.inner.truncate(name, len)
        }
        fn remove_file(&self, name: &str) -> std::io::Result<()> {
            self.inner.remove_file(name)
        }
        fn remove_tree(&self, name: &str) -> std::io::Result<()> {
            self.inner.remove_tree(name)
        }
        fn sync_dir(&self, name: &str) -> std::io::Result<()> {
            self.inner.sync_dir(name)
        }
        fn lock(
            &self,
            kind: graphersal::persist::LockKind,
        ) -> std::io::Result<Option<graphersal::persist::DirLockGuard>> {
            self.inner.lock(kind)
        }
    }

    /// A rollback whose reload fails: the session serves an EMPTY graph, the answer is the
    /// store's refusal with the reload error and its help, the Store menu names the stopped
    /// state, and writes are refused.
    #[test]
    fn a_failed_reload_serves_an_empty_graph_and_names_the_stopped_store() {
        use graphersal::persist::Store;
        use std::sync::atomic::{AtomicUsize, Ordering};
        for n in 0.. {
            let failing = Arc::new(FailingReads {
                inner: graphersal::persist::IntoStoreDir::into_store_dir(MemDir::new()),
                left: AtomicUsize::new(usize::MAX),
            });
            let store: Store =
                Store::create(failing.clone() as Arc<dyn StoreDir>, StoreOptions::new()).unwrap();
            let mut s = Session::new();
            s.attach_store(Arc::new(store), "main").unwrap();
            add(&mut s, "ann");
            add(&mut s, "bob");
            failing.left.store(n, Ordering::Release);
            let result = s.store_rollback(&json!({"target": {"commit": 1}}));
            failing.left.store(usize::MAX, Ordering::Release);
            let err = match result {
                Ok(_) => panic!("no read of the reload failed"),
                Err(err) => err,
            };
            if !err
                .message
                .contains("the reload of the graph after the rollback failed")
            {
                continue;
            }
            assert_eq!(err.status, 422);
            assert!(
                err.message.contains("the graph in memory was emptied"),
                "{}",
                err.message
            );
            assert!(
                err.message
                    .contains("Close the store and open it again (restart the server"),
                "{}",
                err.message
            );
            assert_eq!(s.info().unwrap()["vertexCount"], 0);
            let info = s.store_info().unwrap();
            let stopped = info["stopped"].as_str().unwrap_or_default();
            assert!(stopped.contains("was emptied"), "{info}");
            let refused: Value =
                serde_json::from_str(&s.execute("g.add_v(\"x\").next()", &RunOptions::default()))
                    .unwrap();
            assert_eq!(refused["ok"], false, "{refused}");
            let mark = s.store_mark("m").unwrap_err();
            assert!(
                mark.message.contains("close and reopen"),
                "{}",
                mark.message
            );
            assert_eq!(s.info().unwrap()["vertexCount"], 0);
            return;
        }
    }

    #[test]
    fn texts_round_trip() {
        assert_eq!(format_time(0), "-");
        assert_eq!(parse_time("1970-01-01T00:00:01Z").unwrap(), 1_999_999);
        assert_eq!(bytes_text(2048), "2.0 KiB");
        assert_eq!(uuid_text(1), "00000000-0000-0000-0000-000000000001");
    }
}
