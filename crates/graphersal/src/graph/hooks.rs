// Commit hooks; their public documentation is the module docs of `crate::changes`.

use crate::error::GraphError;
use crate::graph::change::ChangeSet;
use crate::graph::clock::CommitClock;

/// The error a [`CommitHook::before_commit`] returns to veto a commit: any error type, boxed
/// (`Err("disk full".into())` works too). It reaches the caller unchanged as the `source` of
/// [`GraphError::CommitRejected`](crate::error::GraphError::CommitRejected).
pub type HookError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A veto that brings its own advice: a [`CommitHook::before_commit`] returns it (boxed, as a
/// [`HookError`]) when the generic help of
/// [`GraphError::CommitRejected`](crate::error::GraphError::CommitRejected) ("fix the cause,
/// run the query again") would be wrong, for example for a state that refuses every write.
/// Its message is the reason in the error, its help replaces the generic help.
///
/// ```rust
/// use graphersal::prelude::*;
/// use graphersal::error::DiagnosticError;
/// use graphersal::changes::{ChangeSet, CommitHook, HookError, HookRefusal};
///
/// struct Frozen;
/// impl CommitHook for Frozen {
///     fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
///         Err(Box::new(HookRefusal::new(
///             "the graph is frozen for the audit: writes are refused",
///             "Read it as it is; write to the live copy instead.",
///         )))
///     }
/// }
///
/// let mut graph = TraversalGraph::tinkerpop_modern();
/// graph.register_commit_hook(Box::new(Frozen)).unwrap();
/// let err = graph.traversal_mut().add_v("x").to_list().unwrap_err();
/// assert!(err.to_string().contains("frozen for the audit"));
/// assert!(err.help().unwrap().contains("write to the live copy"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRefusal {
    message: String,
    help: String,
}

impl HookRefusal {
    /// A refusal with its `message` (why the commit is refused) and its `help` (what to do).
    pub fn new(message: impl Into<String>, help: impl Into<String>) -> Self {
        HookRefusal {
            message: message.into(),
            help: help.into(),
        }
    }

    /// Why the commit is refused.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// What to do instead.
    pub fn help(&self) -> &str {
        &self.help
    }
}

impl std::fmt::Display for HookRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HookRefusal {}

/// Receives the commit events of a storage; see the [module docs](crate::changes) for the order and the
/// veto rules. Every method has a default, so a hook implements only what it needs.
///
/// # Example
///
/// ```rust
/// use graphersal::prelude::*;
/// use graphersal::changes::{ChangeSet, CommitHook, HookError};
/// use std::sync::{Arc, Mutex};
///
/// /// Collects every committed change set (a stand-in for a write-ahead log).
/// struct Journal(Arc<Mutex<Vec<ChangeSet>>>);
///
/// impl CommitHook for Journal {
///     fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
///         self.0.lock().map_err(|_| "journal poisoned")?.push(changes.clone());
///         Ok(())
///     }
/// }
///
/// let journal = Arc::new(Mutex::new(Vec::new()));
/// let mut graph = TraversalGraph::tinkerpop_modern();
/// graph.register_commit_hook(Box::new(Journal(journal.clone()))).unwrap();
/// graph.traversal_mut().add_v("person").property("name", "ann").to_list().unwrap();
/// let journal = journal.lock().unwrap();
/// assert_eq!(journal.len(), 1);
/// assert_eq!(journal[0].commit_seq(), graph.last_commit_seq());
/// ```
pub trait CommitHook: Send + Sync {
    /// A short name for diagnostics: it names the hook in
    /// [`GraphError::CommitRejected`](crate::error::GraphError::CommitRejected) and in
    /// [`RollbackReason::Vetoed`]. Default: the Rust type name.
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// Called once per committed unit that changed something (or only raised an auto-id
    /// sequence: a sequence-only commit, see the [module docs](crate::changes)), after validation and
    /// before the changes are final. `Err` vetoes the commit: the unit is rolled back and the caller gets
    /// the error. A panic here rolls the unit back too (every hook gets `after_rollback`, the
    /// commit sequence number does not move) and then continues to the caller. The journal of a
    /// graph (feature `persist`) runs after every user hook, so it never writes a unit whose
    /// hook vetoed or panicked; a panic inside the journal itself cuts its record off again,
    /// poisons the journal and rolls the unit back the same way (the table in the book page
    /// `persistence/journal_and_recovery.md`, "Failures while committing"). Default: accept.
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        let _ = changes;
        Ok(())
    }

    /// Called after the unit is final, with the same change set `before_commit` saw. A panic
    /// here continues to the caller, but the unit stays committed (it is final, and durable
    /// when a journal is attached). Default: nothing.
    fn after_commit(&mut self, changes: &ChangeSet) {
        let _ = changes;
    }

    /// Called after a unit that changed something was rolled back: vetoed by a hook, or failed
    /// (a traversal error, a schema violation, a resource limit, a failing import or apply).
    /// Default: nothing.
    fn after_rollback(&mut self, reason: &RollbackReason) {
        let _ = reason;
    }
}

/// Why a unit was rolled back; passed to [`CommitHook::after_rollback`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RollbackReason {
    /// A [`CommitHook::before_commit`] returned `Err`.
    #[non_exhaustive]
    Vetoed {
        /// The [`CommitHook::name`] of the hook that vetoed.
        hook: String,
        /// The display text of the hook's error.
        message: String,
    },
    /// The unit failed before it could commit (a traversal error, a schema violation, a
    /// resource limit or timeout, a failing GraphML import or change-set apply).
    #[non_exhaustive]
    Failed {
        /// The display text of the error that failed the unit.
        message: String,
    },
}

impl RollbackReason {
    /// [`RollbackReason::Failed`] with the display text of `error`.
    pub fn failed(error: &dyn std::fmt::Display) -> Self {
        RollbackReason::Failed {
            message: error.to_string(),
        }
    }

    /// The reason for a unit rolled back because of `error`: [`RollbackReason::Vetoed`] for a
    /// [`GraphError::CommitRejected`] (the veto of [`CommitHooks::before_commit`]),
    /// [`RollbackReason::Failed`] for every other error.
    pub fn from_error(error: &GraphError) -> Self {
        match error {
            GraphError::CommitRejected { hook, source } => RollbackReason::Vetoed {
                hook: hook.clone(),
                message: source.to_string(),
            },
            other => RollbackReason::failed(other),
        }
    }

    /// The display text of the error behind the rollback.
    pub fn message(&self) -> &str {
        match self {
            RollbackReason::Vetoed { message, .. } | RollbackReason::Failed { message } => message,
        }
    }
}

/// A named point in a graph's journal: "the state right after commit `commit_seq`". Created by
/// [`GraphStorage::mark`](crate::graph::GraphStorage::mark) and written by the journal as a
/// Mark record; a recovery can stop at it (`persist::RecoveryTarget::Mark`, feature `persist`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Mark {
    name: String,
    commit_seq: u64,
    time: i64,
}

impl Mark {
    /// A mark (built by the graph and by the journal reader).
    pub(crate) fn new(name: impl Into<String>, commit_seq: u64, time: i64) -> Self {
        Self {
            name: name.into(),
            commit_seq,
            time,
        }
    }

    /// The mark's name, unique within its journal.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The commit the mark points at: the last commit before it (`0`: before any commit).
    pub fn commit_seq(&self) -> u64 {
        self.commit_seq
    }

    /// When the mark was set, in microseconds since the Unix epoch (UTC); never earlier than
    /// the commit it points at.
    pub fn time(&self) -> i64 {
        self.time
    }
}

/// The hook of the graph's journal (`persist::Journal`, feature `persist`): a commit hook that
/// also records marks. It has its own slot in [`CommitHooks`], so its `before_commit` runs
/// after every user hook whatever the registration order: once its record is written nothing
/// can veto the commit any more, and the journal never holds a unit that did not happen.
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) trait JournalHook: CommitHook {
    /// Writes `mark`; `Err` rejects it (a duplicate name, a failed write).
    fn record_mark(&mut self, mark: &Mark) -> Result<(), HookError>;

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    /// Writes an informational Checkpoint record: a snapshot at `commit_seq` exists.
    fn record_checkpoint(&mut self, commit_seq: u64, time: i64) -> Result<(), HookError>;

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    /// Makes everything written durable now (a Store closing).
    fn sync_now(&mut self) -> Result<(), HookError>;
}

/// The journal of a graph, ready to be put into a storage's journal slot
/// ([`CommitHooks::set_journal`], `PersistentStorage::attach_journal` with feature `persist`).
///
/// Opaque: it wraps the crate-internal journal hook (a commit hook that also records marks,
/// checkpoints and syncs) and there is no public way to build one; the library's journal and
/// Store hand it out when they attach to a storage. A storage only keeps it in its
/// [`CommitHooks`].
pub struct JournalSlot(pub(crate) Box<dyn JournalHook>);

impl JournalSlot {
    /// Wraps a journal hook (built by `persist::Journal` when it attaches).
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn new(journal: Box<dyn JournalHook>) -> Self {
        Self(journal)
    }

    /// The journal's [`CommitHook::name`].
    pub fn name(&self) -> &str {
        self.0.name()
    }
}

impl std::fmt::Debug for JournalSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("JournalSlot").field(&self.name()).finish()
    }
}

/// The commit hooks of one storage: the user hooks in registration order, then the journal
/// slot. A storage with `change_capture` embeds one and calls it at its commit point, so the
/// ordering contract of the [`changes`](crate::changes) module (and of the journal) is
/// implemented once, here. [`TraversalGraph`](crate::TraversalGraph) uses it itself. There is
/// no reusable undo log or change-capture component: a host storage derives what a unit changed
/// from its own bookkeeping (the book page `for_developers/custom_storage.md`).
///
/// # A guide for storage implementers
///
/// - [`Self::register`] in
///   [`GraphStorage::register_commit_hook`](crate::storage::GraphStorage::register_commit_hook);
///   [`Self::set_journal`] in `PersistentStorage::attach_journal` (feature `persist`).
/// - [`Self::is_active`]: capture changes only while it is `true` (nothing is paid without
///   hooks).
/// - At the outermost commit of a unit that changed something: first validate everything and
///   **assign the commit sequence number** (and the commit time, from [`Self::commit_time`]:
///   the instance clock, shared with the marks and the system journal), build the [`ChangeSet`] with
///   [`ChangeSet::builder`], then call [`Self::before_commit`]. It runs `before_commit` of every
///   user hook in registration order and THEN the journal's, which writes the record: nothing
///   fallible may follow it, or the journal would hold a unit that did not happen. On `Ok`
///   make the unit final and call [`Self::after_commit`] with the same change set; on `Err`
///   (a veto, [`GraphError::CommitRejected`]) roll the unit back, call [`Self::after_rollback`]
///   with [`RollbackReason::from_error`] and return the error unchanged. When
///   [`Self::before_commit`] panics (a user hook's panic, which comes before the journal, or the
///   journal's own, after it has cut its partial record off again and poisoned itself), catch
///   it (`std::panic::catch_unwind`), roll the unit back, call [`Self::after_rollback`] and resume
///   the panic, so the unit is never left open with its changes in effect.
/// - A rolled-back outermost unit that had changed something (a failed traversal, an import)
///   calls [`Self::after_rollback`] too.
/// - [`GraphStorage::mark`](crate::storage::GraphStorage::mark) forwards to [`Self::mark`]
///   (after refusing a mark while a unit is open).
///
/// ```rust
/// use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation, RollbackReason};
/// use graphersal::storage::CommitHooks;
///
/// struct Veto;
/// impl CommitHook for Veto {
///     fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
///         Err("not today".into())
///     }
/// }
///
/// let mut hooks = CommitHooks::new();
/// hooks.register(Box::new(Veto));
/// let mut builder = ChangeSet::builder(1, 1_700_000_000_000_000);
/// builder.push(Mutation::add_vertex("v1", ["person"], Default::default()));
/// let changes = builder.build().unwrap();
/// let err = hooks.before_commit(&changes).unwrap_err();
/// // The storage rolls its unit back, then tells every hook why.
/// hooks.after_rollback(&RollbackReason::from_error(&err));
/// assert!(err.to_string().contains("not today"));
/// ```
#[derive(Default)]
pub struct CommitHooks {
    hooks: Vec<Box<dyn CommitHook>>,
    journal: Option<Box<dyn JournalHook>>,
    /// The instance clock commit and mark times come from ([`Self::commit_time`]); it stays
    /// with the hooks, so a reload in place (which keeps them) keeps its floor.
    clock: CommitClock,
}

impl std::fmt::Debug for CommitHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.hooks.iter().map(|hook| hook.name()))
            .entries(self.journal.iter().map(|hook| hook.name()))
            .finish()
    }
}

impl CommitHooks {
    /// No hooks, an empty journal slot, a new [`CommitClock`].
    pub fn new() -> Self {
        Self::default()
    }

    /// The time of a commit (or mark) at this point: the clock's floor first raised to
    /// `last_committed_at` (the storage's last commit time, microseconds since the Unix epoch),
    /// then [`CommitClock::now`], so it is never earlier than the previous commit nor than
    /// anything else the instance timed with the same clock. A storage that captures changes
    /// times every commit with it (also when no hook is registered).
    pub fn commit_time(&self, last_committed_at: i64) -> i64 {
        self.clock.raise(last_committed_at);
        self.clock.now()
    }

    /// The instance clock (a clone sharing its state); a storage hands it out through
    /// [`GraphStorage::commit_clock`](crate::storage::GraphStorage::commit_clock).
    pub fn clock(&self) -> CommitClock {
        self.clock.clone()
    }

    /// The instance clock, borrowed (no clone).
    #[inline]
    pub(crate) fn clock_ref(&self) -> &CommitClock {
        &self.clock
    }

    /// `true` while at least one hook (or the journal) is registered: change capture is on.
    #[inline]
    pub fn is_active(&self) -> bool {
        !self.hooks.is_empty() || self.journal.is_some()
    }

    /// Registers a user hook behind the ones registered before (it still runs before the
    /// journal). Hooks cannot be removed.
    pub fn register(&mut self, hook: Box<dyn CommitHook>) {
        self.hooks.push(hook);
    }

    /// Puts `journal` into the journal slot; [`GraphError::Unsupported`] when the slot is taken
    /// (one journal per graph).
    pub fn set_journal(&mut self, journal: JournalSlot) -> Result<(), GraphError> {
        if self.journal.is_some() {
            return Err(GraphError::Unsupported {
                operation: "a second journal on one graph",
            });
        }
        self.journal = Some(journal.0);
        Ok(())
    }

    /// Whether a journal is in the journal slot.
    pub fn has_journal(&self) -> bool {
        self.journal.is_some()
    }

    /// Runs `before_commit` of the user hooks in registration order and then of the journal
    /// (always last); the first `Err` stops: the hooks after it are not called and it comes
    /// back as [`GraphError::CommitRejected`] naming the vetoing hook. The caller has assigned
    /// the commit sequence number already, and nothing fallible may follow an `Ok` (the journal
    /// has written its record).
    pub fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), GraphError> {
        for hook in &mut self.hooks {
            if let Err(source) = hook.before_commit(changes) {
                return Err(GraphError::CommitRejected {
                    hook: hook.name().to_string(),
                    source,
                });
            }
        }
        if let Some(journal) = &mut self.journal
            && let Err(source) = journal.before_commit(changes)
        {
            return Err(GraphError::CommitRejected {
                hook: journal.name().to_string(),
                source,
            });
        }
        Ok(())
    }

    /// Runs `after_commit` of every hook (user hooks, then the journal) with the change set
    /// [`Self::before_commit`] accepted.
    pub fn after_commit(&mut self, changes: &ChangeSet) {
        for hook in &mut self.hooks {
            hook.after_commit(changes);
        }
        if let Some(journal) = &mut self.journal {
            journal.after_commit(changes);
        }
    }

    /// Runs `after_rollback` of every hook (user hooks, then the journal).
    pub fn after_rollback(&mut self, reason: &RollbackReason) {
        for hook in &mut self.hooks {
            hook.after_rollback(reason);
        }
        if let Some(journal) = &mut self.journal {
            journal.after_rollback(reason);
        }
    }

    /// Has the journal record a mark named `name` pointing at `commit_seq` (the storage's last
    /// commit), timed by the instance clock ([`Self::commit_time`]: never earlier than
    /// `last_committed_at`). The storage refuses a mark while a unit is
    /// open before calling this. [`GraphError::MarkUnavailable`] for a name outside 1 to 255
    /// bytes or without a journal, [`GraphError::MarkRejected`] when the journal refuses it.
    pub fn mark(
        &mut self,
        name: &str,
        commit_seq: u64,
        last_committed_at: i64,
    ) -> Result<Mark, GraphError> {
        if name.is_empty() || name.len() > 255 {
            return Err(GraphError::MarkUnavailable {
                reason: "a mark name must have 1 to 255 bytes",
            });
        }
        let Some(journal) = self.journal.as_deref_mut() else {
            return Err(GraphError::MarkUnavailable {
                reason: "no journal is registered on this graph",
            });
        };
        // `Self::commit_time` on the field (the journal is borrowed).
        self.clock.raise(last_committed_at);
        let time = self.clock.now();
        let mark = Mark::new(name, commit_seq, time);
        journal
            .record_mark(&mark)
            .map_err(|source| GraphError::MarkRejected {
                hook: journal.name().to_string(),
                source,
            })?;
        Ok(mark)
    }
}

impl crate::graph::TraversalGraph {
    /// [`GraphStorage::mark`](crate::graph::GraphStorage::mark) of `TraversalGraph`: refuses a
    /// mark while a unit is open, then has the journal slot record it ([`CommitHooks::mark`]).
    pub(crate) fn journal_mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        if self.undo_depth() > 0 {
            return Err(GraphError::MarkUnavailable {
                reason: "a mark cannot be set while a unit (a traversal or a transaction) is open",
            });
        }
        let (commit_seq, last_committed_at) = (self.last_commit_seq(), self.last_committed_at());
        self.undo.hooks.mark(name, commit_seq, last_committed_at)
    }
}
