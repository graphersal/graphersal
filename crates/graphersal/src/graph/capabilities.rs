//! [`StorageCapabilities`]: what a [`GraphStorage`](crate::graph::GraphStorage) can do
//! (`book/src/users_guide/transactions.md`). Storage FACTS, not permissions: a host or a generic
//! server reads them to decide what it can rely on, and the conformance suite
//! (`graphersal-storage-tests`) runs the atomicity and change-capture items only for a storage
//! that claims them.

/// What a storage guarantees; returned by
/// [`GraphStorage::capabilities`](crate::graph::GraphStorage::capabilities).
///
/// [`Default`] (= [`Self::none`]) claims nothing, which is what the trait's default
/// implementation reports. A storage builds its value with the `with_*` methods:
///
/// ```rust
/// use graphersal::storage::StorageCapabilities;
///
/// let caps = StorageCapabilities::none().with_writable(true).with_atomic(true);
/// assert!(caps.atomic && caps.writable && !caps.change_capture);
/// assert!(caps.is_consistent());
/// ```
///
/// `#[non_exhaustive]`: more capabilities can be added later; each new one defaults to "not
/// supported".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct StorageCapabilities {
    /// A unit of work (one traversal, an import into a live graph, an applied change set) is
    /// all-or-nothing: [`GraphStorage::rollback_unit`](crate::graph::GraphStorage::rollback_unit)
    /// undoes everything since its mark, schema changes included, and
    /// [`GraphStorage::unit_changes`](crate::graph::GraphStorage::unit_changes) reports
    /// what an open unit holds (part of the same contract).
    pub atomic: bool,
    /// The storage builds a [`ChangeSet`](crate::graph::ChangeSet) per committed unit and
    /// supports [commit hooks](crate::graph::CommitHook). Requires [`Self::atomic`]: a storage
    /// that cannot roll back cannot honour a `before_commit` veto, so there is no
    /// change-capture semantics for a non-atomic storage.
    pub change_capture: bool,
    /// What concurrent readers see while a unit is open.
    pub isolation: IsolationLevel,
    /// Several writers may run units at the same time.
    pub concurrent_writers: bool,
    /// Committed units survive a process restart without help from a commit hook.
    pub durability: bool,
    /// The mutating methods are implemented (a read-only view rejects them).
    pub writable: bool,
    /// The content never changes for the lifetime of the storage (a frozen base, for example
    /// under a future layered overlay).
    pub immutable: bool,
    /// The storage maintains and serves property indexes (book page "Property Indexes":
    /// `GraphStorage::property_indexes` and the index lookups). Without it, an index
    /// definition can still be stored in the catalog, but `create_index` is refused with
    /// `GraphError::Unsupported`. A read-only view (`&G`) claims nothing but still serves the
    /// indexes of the storage it views.
    pub property_indexes: bool,
    /// The property index lookups answer an exact condition EXACTLY (book page "Property
    /// Indexes", section "Kept exact"): for an equality slice ([`PropertyIndexRange::equal`])
    /// of a string, boolean, uuid or temporal value, the lookups
    /// ([`GraphStorage::vertices_by_property_index`] and its siblings) of an index without a
    /// transform return exactly the elements whose value at the key equals it, and of an index
    /// with the transform `each` exactly the elements whose value is it or is an array holding
    /// it, and of an index with `to_lower` (`["each", "to_lower"]`) exactly the elements whose
    /// string value (string array element) lowers to it, each element once: never more. The optimizer then drops the re-check of such a
    /// condition in a plan that does not write. Without it (the default) every lookup counts as
    /// a superset and every condition is re-checked on each candidate. A read-only view (`&G`)
    /// or a shared `Arc` states it for the storage it reads through, since its lookups are the
    /// storage's.
    ///
    /// [`PropertyIndexRange::equal`]: crate::graph::PropertyIndexRange::equal
    /// [`GraphStorage::vertices_by_property_index`]: crate::graph::GraphStorage::vertices_by_property_index
    pub exact_property_index_lookups: bool,
}

impl StorageCapabilities {
    /// No capability at all (the same as [`Default`]).
    pub const fn none() -> Self {
        Self {
            atomic: false,
            change_capture: false,
            isolation: IsolationLevel::None,
            concurrent_writers: false,
            durability: false,
            writable: false,
            immutable: false,
            property_indexes: false,
            exact_property_index_lookups: false,
        }
    }

    /// Sets [`Self::atomic`].
    pub const fn with_atomic(mut self, atomic: bool) -> Self {
        self.atomic = atomic;
        self
    }

    /// Sets [`Self::change_capture`] (which requires [`Self::atomic`]).
    pub const fn with_change_capture(mut self, change_capture: bool) -> Self {
        self.change_capture = change_capture;
        self
    }

    /// Sets [`Self::isolation`].
    pub const fn with_isolation(mut self, isolation: IsolationLevel) -> Self {
        self.isolation = isolation;
        self
    }

    /// Sets [`Self::concurrent_writers`].
    pub const fn with_concurrent_writers(mut self, concurrent_writers: bool) -> Self {
        self.concurrent_writers = concurrent_writers;
        self
    }

    /// Sets [`Self::durability`].
    pub const fn with_durability(mut self, durability: bool) -> Self {
        self.durability = durability;
        self
    }

    /// Sets [`Self::writable`].
    pub const fn with_writable(mut self, writable: bool) -> Self {
        self.writable = writable;
        self
    }

    /// Sets [`Self::property_indexes`].
    pub const fn with_property_indexes(mut self, property_indexes: bool) -> Self {
        self.property_indexes = property_indexes;
        self
    }

    /// Sets [`Self::exact_property_index_lookups`].
    pub const fn with_exact_property_index_lookups(
        mut self,
        exact_property_index_lookups: bool,
    ) -> Self {
        self.exact_property_index_lookups = exact_property_index_lookups;
        self
    }

    /// Sets [`Self::immutable`].
    pub const fn with_immutable(mut self, immutable: bool) -> Self {
        self.immutable = immutable;
        self
    }

    /// Whether the claims fit together: `change_capture` requires `atomic`, and a storage
    /// cannot be both `writable` and `immutable`. Checked by a debug assertion when a traversal
    /// starts and by the conformance suite.
    pub const fn is_consistent(&self) -> bool {
        (!self.change_capture || self.atomic) && !(self.writable && self.immutable)
    }
}

/// What concurrent readers of a storage see while a writer's unit is open.
///
/// `#[non_exhaustive]`: stronger levels can be added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IsolationLevel {
    /// The storage itself isolates nothing: readers and the writer are serialized by the
    /// caller (for [`TraversalGraph`](crate::graph::TraversalGraph) by `&mut` or the lock of
    /// [`Graph`](crate::graph::Graph): readers wait for the writer).
    #[default]
    None,
    /// Readers keep seeing the last committed state while a writer's unit is open.
    Snapshot,
}
