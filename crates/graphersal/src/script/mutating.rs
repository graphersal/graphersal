//! `ScriptGraphTraversalSource::is_mutating()`: whether a script plan contains a mutating step.

use super::*;

impl ScriptGraphTraversalSource {
    /// Whether the traversal contains a mutating step (`add_v`, `property`, `drop`, `merge_v`,
    /// ...), also inside a child traversal. Decided from the plan, before it runs: a `true` plan
    /// may still leave the graph unchanged (no traverser reached the mutating step). Script hosts
    /// use it to warn before re-running a query (the browser playground's refresh); the
    /// terminals use it to take the graph's write lock instead of its read lock.
    ///
    /// Derived from the core's single step -> [`AccessRequest`](crate::graph::AccessRequest)
    /// mapping: the plan is lowered to engine steps (without a graph) and is mutating when some
    /// step requests a write of data or the schema, the same mapping the authorization check
    /// uses.
    ///
    /// A traversal built from a saved query's result (or inside a query's body) is never
    /// mutating on the host graph: it carries the query's read-only restriction, so a writing
    /// step in it fails with `PermissionDenied` before anything runs
    /// (`book/src/users_guide/saved_queries.md`).
    pub fn is_mutating(&self) -> bool {
        if self.access.read_only && !self.private {
            return false;
        }
        self.plan_writes()
    }

    /// Whether the plan as written has a step that writes data or the schema (the mapping
    /// behind [`Self::is_mutating`], without the saved-query exception).
    pub(crate) fn plan_writes(&self) -> bool {
        // A leading `gql()` is lowered by the root hook, which needs a storage: an empty
        // scratch graph serves (the write steps do not depend on the planner's statistics);
        // its calls of saved queries (stage 3h) are looked up in the real graph's catalog.
        #[cfg(feature = "gql")]
        if self.leading_gql().is_some() {
            let scratch = TraversalGraph::new();
            let catalog = self.g.as_deref().map(super::saved_query_gql::GraphCatalog);
            let catalog = catalog
                .as_ref()
                .map(|c| c as &dyn crate::gql::procedure::Procedures);
            let (_, start) = self.gql_start(GraphTraversalSource::new(&scratch), catalog);
            let mut plan = self.lower_onto(true, start);
            return crate::traversal::permission_check::plan_mutates(&mut plan.steps);
        }
        let mut plan = self.lower::<&TraversalGraph>(true);
        crate::traversal::permission_check::plan_mutates(&mut plan.steps)
    }
}
