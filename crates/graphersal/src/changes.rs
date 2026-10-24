//! Change capture and commit hooks: what one committed unit of work changed.
//!
//! For hosts that observe or veto commits (an audit log, replication, a journal of their own):
//! register a [`CommitHook`] through
//! [`GraphStorage::register_commit_hook`](crate::storage::GraphStorage::register_commit_hook)
//! and receive a [`ChangeSet`] per committed unit. A storage implementer finds the same types in
//! [`crate::storage`]. Guide: the book page `users_guide/transactions.md`.
//!
//! # Change sets
//!
//! Change capture: what one committed unit changed, as an ordered list of [`Mutation`]s
//! (`book/src/users_guide/transactions.md`).
//!
//! A [`ChangeSet`] is what a commit hook (persistence, replication, CDC, audit) receives. It is
//! built from the undo log of [`TraversalGraph`](crate::graph::TraversalGraph) when the
//! outermost unit commits, and only while change capture is on (section 2.4.3: a graph without
//! hooks pays nothing for capture; the undo log keeps the before-values for rollback anyway).
//!
//! ## Content
//!
//! - The **position** of the commit: [`ChangeSet::commit_seq`], the monotonic commit time
//!   [`ChangeSet::committed_at`] (microseconds since the Unix epoch, UTC) and the auto-id
//!   sequences after the unit ([`ChangeSet::node_sequence`], [`ChangeSet::edge_sequence`]), so a
//!   journal replayed into an empty graph never hands out an id of a dropped or rolled-back
//!   element again.
//! - The mutations in **execution order**, each with its before AND after values: a consumer
//!   can replay them deterministically (a drop and a re-add of the same id stay two operations)
//!   and a CDC consumer gets the old and the new value.
//! - **Cascades are explicit.** Dropping a vertex removes its incident edges first: the
//!   `ChangeSet` lists one [`Mutation::DropEdge`] per removed edge (in removal order, a
//!   self-loop once) *before* the [`Mutation::DropVertex`]. Replaying the list in order never
//!   drops an edge twice and needs no knowledge of the cascade rule.
//! - A **jpath write or removal** (`property(jpath("a.b[1]"), v)`) is a
//!   [`Mutation::SetProperty`] of the top-level key with the whole new value, so the format does
//!   not depend on the path syntax.
//! - **Schema changes** are in the same stream ([`Mutation::SetSchema`]), so a replay applies
//!   schema and data in the order they happened; so are **catalog changes**
//!   ([`Mutation::SetDefinition`], one mutation kind for every definition kind).
//! - A write of an equal value is still a [`Mutation::SetProperty`] (`before == after`): it was a
//!   write. [`ChangeSet::compact`] drops it.
//!
//! ## Building change sets
//!
//! The graph builds the change sets of its own commits. Outside it (a storage that captures
//! changes itself, a replication feed, a test) a change set is built with
//! [`ChangeSet::builder`] (commit sequence number and time, then the auto-id sequences and the
//! metadata) and one constructor per mutation kind ([`Mutation::add_vertex`],
//! [`Mutation::drop_vertex`], [`Mutation::add_edge`], [`Mutation::drop_edge`],
//! [`Mutation::set_property`], [`Mutation::remove_property`], [`Mutation::add_label`],
//! [`Mutation::remove_label`], [`Mutation::set_edge_label`], [`Mutation::set_schema`],
//! [`Mutation::set_definition`]; the images [`VertexState::new`], [`EdgeState::new`]).
//! [`ChangeSetBuilder::build`] makes the cheap checks; a built change set applies
//! ([`GraphStorage::apply_change_set`](crate::graph::GraphStorage::apply_change_set)), replays
//! and serializes like a captured one.
//!
//! ## Serialization
//!
//! With the `serde` feature every type here serializes as JSON, and a serialized change set
//! replays into an IDENTICAL graph without any schema: property values (in element images,
//! property writes and commit attributes, at any nesting depth) carry an explicit per-value
//! type tag for every logical type plain JSON cannot hold (CLAUDE.md rule 7 allows explicit
//! tags; content is never sniffed): `uuid`, written in TinkerPop GraphSON style as
//! `{"@type": "g:UUID", "@value": "<canonical lowercase uuid>"}`, and the date and time types
//! with their canonical text (up to nine fraction digits, exact): `gx:OffsetDateTime`,
//! `gx:ZonedDateTime` (a `datetime` with a zone name, `..+02:00[Europe/Berlin]`),
//! `gx:OffsetTime`, `gx:LocalDate`, `gx:LocalDateTime`, `gx:LocalTime`, and for a `duration`
//! `gx:Duration` (a time part only), `gx:Period` (months and days only) or
//! `graphersal:Interval` (both), and a vector as
//! `{"@type": "graphersal:Vector", "@value": {"element": "f32", "data": [..]}}` (exact for every
//! element kind: an `i8` vector with `scale` and `codes`, a `bit` vector with `dimension` and
//! `bits` in hex). JSON-native types stay untagged.
//!
//! Escaping: a user object that has an `"@type"` key of its own is wrapped as
//! `{"@type": "graphersal:Object", "@value": {..the object..}}` (its keys are then taken
//! literally, its values are read as tagged values again), so no user map can be misread as a
//! tag. On reading, an object with an `"@type"` key must be exactly one of these forms
//! (`@type` and `@value` only, a canonical uuid string, the text of a value of the tag's type
//! whose parts fit the tag, or an object payload); anything else is a deserialization error,
//! never a guess. The tags are used for the serialized `ChangeSet`
//! only; GraphML and the rest of the physical codec are unchanged.
//!
//! # Commit hooks
//!
//! Commit hooks: how a host (a server, a persistence layer, a replica, an audit log) learns
//! what each unit of work changed, and how it can veto a commit
//! (`book/src/users_guide/transactions.md`).
//!
//! A [`CommitHook`] is registered on a storage with
//! [`GraphStorage::register_commit_hook`](crate::graph::GraphStorage::register_commit_hook).
//! Only a storage that is atomic and captures changes supports hooks
//! ([`StorageCapabilities::change_capture`](crate::graph::StorageCapabilities::change_capture),
//! which requires `atomic`); every other storage rejects the registration with
//! [`GraphError::Unsupported`](crate::error::GraphError::Unsupported). [`TraversalGraph`](crate::TraversalGraph)
//! supports them.
//!
//! ## Events
//!
//! | Event | When | Can veto? |
//! |---|---|---|
//! | [`CommitHook::before_commit`] | the unit validated, before it is final | yes: `Err` rolls the unit back |
//! | [`CommitHook::after_commit`] | after the unit is final | no |
//! | [`CommitHook::after_rollback`] | after the unit was rolled back | no |
//!
//! - The events belong to the OUTERMOST unit (one traversal, a GraphML import into a live graph,
//!   an [`apply_change_set`](crate::graph::GraphStorage::apply_change_set)); a nested unit
//!   (a savepoint) emits nothing of its own, its surviving changes are part of the outer
//!   unit's [`ChangeSet`].
//! - Several hooks run in registration order. The first `before_commit` that returns `Err`
//!   stops the commit: the remaining `before_commit` hooks are not called, the unit is rolled
//!   back, ALL hooks receive `after_rollback` with [`RollbackReason::Vetoed`], and the caller
//!   gets [`GraphError::CommitRejected`](crate::error::GraphError::CommitRejected) carrying the
//!   hook's error (inside a [`TraverserError`](crate::error::TraverserError) for a traversal).
//! - A unit that changed nothing (a read-only traversal, `drop()` over an empty set) emits no
//!   event and does not advance the commit sequence number. A unit that changed nothing but
//!   raised an auto-id sequence
//!   ([`apply_change_set`](crate::graph::GraphStorage::apply_change_set) of a change set whose
//!   mutations were all compacted away) is a *sequence-only commit*: it gets a commit sequence
//!   number and the hooks see a [`ChangeSet`] without mutations whose
//!   [`node_sequence`](ChangeSet::node_sequence)/[`edge_sequence`](ChangeSet::edge_sequence)
//!   carry the raise (a veto rolls it back like any commit; the raise stays in memory).
//! - A hook gets the [`ChangeSet`] only, never the graph: it runs while the writer holds the
//!   graph, so re-entering it is impossible by signature; the before and after values in the
//!   change set are enough to persist, replicate or audit the change.
//! - Hooks run synchronously, strictly in commit sequence order.
//! - While at least one hook is registered, the storage builds a [`ChangeSet`] for every
//!   committed unit (change capture). Without hooks nothing is captured and nothing is paid.
//! - Hooks cannot be removed (a server registers them once, before it publishes the
//!   graph); removal may be added later.
//!
//! **Deviation from TinkerPop:** TinkerPop's `EventStrategy` calls a `MutationListener` for
//! each mutation after it happened. Graphersal's hooks are called once per committed unit with
//! the whole [`ChangeSet`], before the commit is final (with a veto) and after it; there are no
//! per-mutation events. See `book/src/for_developers/tinkerpop_deviations.md`.
//!
//! ## Write-ahead logging
//!
//! A server persists in `before_commit`: when the write fails, the hook returns `Err` and the
//! in-memory graph rolls back, so memory and journal never diverge and the client never gets
//! "ok" for data that is lost on restart. The library's own journal (`persist::Journal`,
//! feature `persist`) has a slot of its own: its `before_commit` runs after every hook
//! registered here, whatever the order of registration, so no later hook can veto a unit that
//! is already in the journal. A hand-written write-ahead hook registered here gets no such
//! guarantee: register it last. At recovery the server loads its last snapshot into a
//! graph that nobody else sees yet and replays its journal with
//! [`TraversalGraph::replay`](crate::graph::TraversalGraph::replay), which runs outside units
//! and fires no hooks (it must not re-write the journal it reads).

pub use crate::graph::{
    ChangeSet, ChangeSetBuilder, CommitHook, CommitMetadata, EdgeState, ElementRef, HookError,
    HookRefusal, Mark, Mutation, RollbackReason, VertexState,
};
