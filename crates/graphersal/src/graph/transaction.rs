//! Explicit transactions and dry runs over every [`GraphStorage`] (`book/src/users_guide/transactions.md`,
//! `book/src/for_developers/custom_storage.md`): the [`Transactional`] extension
//! trait. A closure is one unit; every traversal inside it is a nested unit (a savepoint).
//!
//! The closure form is deliberate: there is no guard object that could be forgotten. A panic
//! inside the closure rolls the unit back before it continues to the caller
//! ([`close_on_unwind`]), as a panic inside a traversal does; the engine itself never panics on
//! user input.

use crate::error::GraphError;
use crate::graph::change::{ChangeSet, CommitMetadata};
use crate::graph::hooks::RollbackReason;
use crate::graph::shared::{close_failed, note_failed_rollback};
use crate::graph::storage::{
    DRY_RUN_OPERATION, GraphStorage, close_on_unwind, rollback_failed_unit, rollback_unwound,
};

/// The rollback reason the hooks receive when a transaction closure returns `Err`.
const CLOSURE_FAILED: &str = "the transaction closure returned an error";

/// Explicit transactions and dry runs, for every [`GraphStorage`] (a blanket implementation:
/// nothing to implement, import the trait to call them). Built only on the trait's unit
/// methods: [`begin_unit`](GraphStorage::begin_unit),
/// [`set_unit_metadata`](GraphStorage::set_unit_metadata),
/// [`commit_unit`](GraphStorage::commit_unit), [`rollback_unit`](GraphStorage::rollback_unit)
/// and [`discard_unit`](GraphStorage::discard_unit).
///
/// What a unit guarantees is the storage's: on an atomic storage
/// ([`StorageCapabilities::atomic`](crate::graph::StorageCapabilities::atomic), for example
/// [`TraversalGraph`](crate::TraversalGraph)) `Err` rolls everything back; on a non-atomic one
/// the changes made before the error stay. [`Self::dry_run`] needs
/// [`StorageCapabilities::change_capture`](crate::graph::StorageCapabilities::change_capture)
/// and fails with [`GraphError::Unsupported`] (converted into `E`) without it. A host storage
/// gets these closures without implementing anything (the book page `for_developers/custom_storage.md`).
///
/// **Deviation from TinkerPop:** TinkerPop opens explicit transactions with `g.tx()` and
/// finishes them with `commit()`/`rollback()`; Graphersal has no `tx()` (neither in Rust nor in
/// the DSL) and uses these closures instead. See
/// `book/src/users_guide/transactions.md` and `book/src/for_developers/tinkerpop_deviations.md`.
pub trait Transactional: GraphStorage {
    /// Runs `body` as ONE unit of atomicity (an explicit transaction): everything it changes
    /// becomes final together when it returns `Ok`, or is rolled back together when it returns
    /// `Err`.
    ///
    /// - Every traversal run inside `body` is a nested unit, a savepoint: when it fails, only
    ///   its own changes are rolled back and its error is returned to `body`, which decides
    ///   whether to give up (`?`) or to continue (TinkerPop / SQL behaviour).
    /// - `Err` from `body` rolls back everything the transaction changed; the commit hooks get
    ///   `after_rollback` ([`RollbackReason::Failed`]) when it had changed something.
    /// - When `body` returns `Ok`, the transaction commits as one unit: one commit sequence
    ///   number, one [`ChangeSet`] for the [commit hooks](crate::graph::CommitHook). A
    ///   `before_commit` veto rolls back everything and comes back as `E::from(`
    ///   [`GraphError::CommitRejected`]`)`; the hook's own error is its `source()`. That is why
    ///   `E: From<GraphError>`: the closure's usual error types all qualify
    ///   ([`BoxedTraverserError`](crate::error::BoxedTraverserError), which `?` on a terminal
    ///   produces, [`TraverserError`](crate::TraverserError), [`GraphError`], or a boxed
    ///   `dyn Error`).
    /// - A `transaction` inside another one is a savepoint of the enclosing transaction: its
    ///   `Err` rolls back only its own changes, its `Ok` keeps them in the enclosing unit,
    ///   which alone commits (and can still roll them back).
    ///
    /// Auto-generated ids are never handed out twice: a rolled-back transaction leaves gaps in
    /// the id sequences. No isolation is needed: `&mut self` excludes every other reader and
    /// writer for the duration (a shared [`Graph`](crate::graph::Graph) is locked by its
    /// `write()` guard). A panic inside `body` rolls everything back (the hooks get
    /// `after_rollback`) and then continues to the caller.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::new();
    /// let result: TraverserResult<()> = graph.transaction(|g| {
    ///     g.traversal_mut().add_v("person").property("name", "ann").to_list()?;
    ///     // A failing traversal rolls back only itself; the closure decides what follows.
    ///     let failed = g.traversal_mut().add_v("ghost").constant("x").as_number(GType::LONG).to_list();
    ///     assert!(failed.is_err());
    ///     g.traversal_mut().add_v("person").property("name", "bob").to_list()?;
    ///     Ok(())
    /// });
    /// assert!(result.is_ok());
    /// assert_eq!(graph.vertex_count(), 2);
    ///
    /// // `Err` from the closure rolls back everything it did.
    /// let failed: Result<(), Box<dyn std::error::Error>> = graph.transaction(|g| {
    ///     g.traversal_mut().add_v("person").to_list()?;
    ///     Err("over budget".into())
    /// });
    /// assert!(failed.is_err());
    /// assert_eq!(graph.vertex_count(), 2);
    /// ```
    fn transaction<T, E>(&mut self, body: impl FnOnce(&mut Self) -> Result<T, E>) -> Result<T, E>
    where
        E: From<GraphError>,
    {
        self.transaction_with(CommitMetadata::default(), body)
    }

    /// [`Self::transaction`] with [`CommitMetadata`] (a principal, free attributes) that the
    /// commit's [`ChangeSet`] carries to the commit hooks, for example who ran a migration
    /// (handed to the storage by [`GraphStorage::set_unit_metadata`] right before the commit).
    ///
    /// The metadata belongs to the commit, and only the outermost unit commits: inside an
    /// enclosing transaction, the metadata of a nested `transaction_with` that returns `Ok` is
    /// added to the enclosing one's (a principal only when none is set yet, attributes only
    /// for keys not set yet; the enclosing transaction wins), and a nested one that returns
    /// `Err` contributes nothing.
    ///
    /// A storage that cannot open the unit fails the call with its error before `body` runs. A
    /// storage that cannot attach the metadata fails it after rolling the unit back. A storage
    /// whose rollback fails (after `body` returned `Err`) closes the unit anyway, and the call
    /// fails with [`GraphError::RollbackFailed`] instead of `body`'s error (`E` has no text to
    /// keep; the error names the closure's failure as its cause). Run on the guard of
    /// [`Graph::write`](crate::graph::Graph::write), such a failure poisons that graph
    /// ([`Graph::is_poisoned`](crate::graph::Graph::is_poisoned)); on a bare storage the error
    /// is all there is.
    fn transaction_with<T, E>(
        &mut self,
        metadata: CommitMetadata,
        body: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<GraphError>,
    {
        let mark = self.begin_unit()?;
        let outcome = close_on_unwind(self, body, |this| rollback_unwound(this, mark));
        match outcome {
            Ok(value) => {
                if let Err(err) = self.set_unit_metadata(metadata) {
                    return Err(E::from(rollback_failed_unit(self, mark, err)));
                }
                self.commit_unit(mark)?;
                Ok(value)
            }
            Err(err) => match self.rollback_unit(mark, &RollbackReason::failed(&CLOSURE_FAILED)) {
                Ok(()) => Err(err),
                // `E` carries no text to keep: the rollback failure, which names the closure's
                // failure as its cause, is the error to report.
                Err(rollback) => {
                    let failed = GraphError::rollback_failed(&CLOSURE_FAILED, rollback);
                    note_failed_rollback(&failed);
                    Err(E::from(failed))
                }
            },
        }
    }

    /// Runs `body` like [`Self::transaction`] and then ALWAYS rolls everything back: a preview
    /// of what `body` would change (a migration, a playground run). Returns `body`'s result
    /// together with the [`ChangeSet`] of the changes it made and that were undone, in execution
    /// order, exactly as a commit hook would have received them (call
    /// [`ChangeSet::compact`] for the net effect).
    ///
    /// Built on [`GraphStorage::discard_unit`], so it needs a storage with
    /// [`change_capture`](crate::graph::StorageCapabilities::change_capture); on any other
    /// storage it fails with `E::from(`[`GraphError::Unsupported`]`)` after closing the unit
    /// with [`GraphStorage::rollback_unit`] (which undoes nothing on a non-atomic storage).
    ///
    /// - The graph is left as it was, including its schema and catalog; only the auto-id
    ///   sequences keep their progress (an id once handed out is never reused, also not after a
    ///   dry run).
    /// - No commit hook is called: nothing is committed or attempted, so there is nothing to
    ///   veto, journal or report. The change set is derived whether or not hooks are
    ///   registered.
    /// - The change set's [`ChangeSet::commit_seq`] and [`ChangeSet::committed_at`] are `0`
    ///   (never committed) and its metadata is empty; [`GraphStorage::last_commit_seq`] does
    ///   not move. Its [`ChangeSet::node_sequence`] and [`ChangeSet::edge_sequence`] are the
    ///   sequences after the dry run (they keep their progress, see above).
    /// - Traversals inside `body` are savepoints as in a transaction: a failing one rolls back
    ///   only itself and does not appear in the change set. `Err` from `body` comes back
    ///   unchanged (everything is rolled back, no change set).
    /// - Inside an enclosing transaction a dry run is a savepoint that is always rolled back:
    ///   its change set holds only its own changes, the enclosing unit's stay.
    /// - A panic inside `body` rolls everything back too, then continues to the caller.
    /// - A storage that cannot discard the unit (an error other than the `Unsupported` of a
    ///   storage without change capture) fails the dry run with that error; run on the guard of
    ///   [`Graph::write`](crate::graph::Graph::write), it poisons that graph
    ///   ([`Graph::is_poisoned`](crate::graph::Graph::is_poisoned)).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let (dropped, changes) = graph
    ///     .dry_run(|g| g.traversal_mut().v(None).has_label("software").drop().to_list())
    ///     .unwrap();
    /// assert!(dropped.is_empty());
    /// // Two vertices and their four incident edges would have been dropped ...
    /// assert_eq!(changes.len(), 6);
    /// // ... but the graph is untouched.
    /// assert_eq!(graph.vertex_count(), 6);
    /// assert_eq!(graph.edge_count(), 6);
    /// ```
    fn dry_run<T, E>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<(T, ChangeSet), E>
    where
        E: From<GraphError>,
    {
        let mark = self.begin_unit()?;
        let outcome = close_on_unwind(self, body, |this| {
            // Unwinding: only the unit is closed; its change set is not wanted, and a failure
            // to close it cannot be reported (the panic continues).
            match this.discard_unit(mark) {
                Ok(_) => {}
                Err(err) if !close_failed(&err) => {}
                Err(err) => {
                    tracing::error!("closing a dry run a panic unwound through failed: {err}");
                    note_failed_rollback(&err);
                }
            }
        });
        match outcome {
            Ok(value) => {
                let changes = self.discard_unit(mark).inspect_err(|err| {
                    if close_failed(err) {
                        note_failed_rollback(err);
                    }
                })?;
                debug_assert!(
                    self.capabilities().is_ok_and(|c| c.change_capture),
                    "a storage that overrides discard_unit must claim change_capture"
                );
                Ok((value, changes))
            }
            Err(err) => match self.discard_unit(mark) {
                // The unit is closed the same way and its change set dropped; on a storage
                // without change capture its `Unsupported` is dropped too: `body`'s own error
                // is the one to report.
                Ok(_) => Err(err),
                Err(GraphError::Unsupported { operation }) if operation == DRY_RUN_OPERATION => {
                    Err(err)
                }
                // The storage could not undo the dry run: that is the error to report (it names
                // the closure's failure as its cause).
                Err(failed) => {
                    let failed = GraphError::rollback_failed(&CLOSURE_FAILED, failed);
                    note_failed_rollback(&failed);
                    Err(E::from(failed))
                }
            },
        }
    }
}

impl<G: GraphStorage + ?Sized> Transactional for G {}
