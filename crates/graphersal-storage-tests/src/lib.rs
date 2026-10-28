//! Conformance tests for [`GraphStorage`](graphersal::storage::GraphStorage) implementations.
//!
//! **EXPERIMENTAL**, like the trait itself: the `GraphStorage` trait and this suite may change
//! in any minor release before 1.0 (also in a 0.4.x release). Pin the exact version
//! (`graphersal-storage-tests = "=0.2.0"`, the same version as `graphersal`).
//!
//! The contract is derived from the trait documentation and from the behaviour of
//! [`TraversalGraph`](graphersal::TraversalGraph), the reference implementation; where the
//! documentation is silent, `TraversalGraph` is the spec and the point is recorded in the
//! crate's `README.md`. The suite uses only `graphersal`'s public API.
//!
//! # Usage
//!
//! Add the crate as a dev-dependency of the crate that implements `GraphStorage` and invoke
//! [`conformance!`] in one of its tests:
//!
//! ```text
//! // tests/conformance.rs of the crate that implements `MyStorage`
//! graphersal_storage_tests::conformance!(my_storage, base = MyStorage::new() => base);
//! ```
//!
//! expands to a module `my_storage` with one `#[test]` function per contract item, each running
//! against a fresh, empty storage, so a failure names the item. The right-hand side after `=>` is
//! the expression that yields the storage under test, so a wrapper is exercised by naming it
//! there. On the reference implementation:
//!
//! ```
//! use graphersal::TraversalGraph;
//!
//! graphersal_storage_tests::conformance!(owned, base = TraversalGraph::new() => base);
//! graphersal_storage_tests::conformance!(by_mut_ref, base = TraversalGraph::new() => &mut base);
//! # fn main() {}
//! ```
//!
//! # Optional parts of the contract
//!
//! - **Schemas.** The schema items (`schema_application_failure_leaves_graph_and_schema_unchanged`,
//!   `schema_modes_enforce_declared_properties`, `add_applies_schema_defaults_insert_does_not`,
//!   the schema write inside
//!   `extension::rollback_restores_everything`) check schema enforcement only when the storage
//!   supports schemas: when `set_schema` returns `GraphError::Unsupported` (the trait default)
//!   they return early and pass. Schema enforcement is not reusable outside `graphersal` yet, so
//!   a storage in another crate usually keeps that default. [`conformance_core!`] leaves the
//!   schema items out entirely, for a storage that accepts schemas but deliberately does not
//!   enforce them.
//! - **Catalog.** The catalog items (`definitions_are_stored_validated_and_removed` and the
//!   extension items `rollback_restores_definitions`, `definition_changes_are_captured`) run in
//!   both macros: a storage without a catalog keeps the trait defaults and is checked for exactly
//!   those.
//! - **Capabilities.** Both macros run the capability-conditional items of [`extension`]
//!   (atomicity, commit hooks, change capture), each checking only what the storage's
//!   `capabilities()` claim.
//!
//! The contract functions themselves live in [`contract`] (one public function per item, plus
//! helpers and the invariant checker [`contract::assert_consistent`]) and can be called directly;
//! [`smoke`] compares traversals over the storage with the reference implementation.
//!
//! [`forwarding::Forwarding`] is a test helper, not part of the contract: a storage that forwards
//! every method to a `TraversalGraph` but has its own handle types, used to prove that the front
//! ends (the DSL, saved queries, the session) work over a storage other than `TraversalGraph`.

#![warn(missing_docs)]

pub mod contract;
pub mod extension;
pub mod failing;
pub mod forwarding;
pub mod smoke;

/// Runs the whole contract, schema items included (they pass without checking anything when
/// `set_schema` is `GraphError::Unsupported`), against the storage built by the given
/// expressions. See the [crate documentation](crate).
///
/// Form: `conformance!(module_name, base_ident = init_expr => storage_expr)`. Every generated test
/// evaluates `let mut base_ident = init_expr; let mut storage = storage_expr;` and passes
/// `&mut storage` to one function of [`contract`].
#[macro_export]
macro_rules! conformance {
    ($m:ident, $base:ident = $init:expr_2021 => $storage:expr_2021) => {
        $crate::__conformance_tests! { $m, $base, $init, $storage;
            explicit_vertex_ids_are_preserved
            auto_vertex_ids_are_unique
            explicit_numeric_vertex_id_bumps_the_sequence
            duplicate_vertex_id_is_rejected
            edge_ids_are_preserved_unique_and_bump_the_sequence
            lookups_after_drop_find_nothing
            vertex_labels_are_an_ordered_set
            unlabeled_vertex_has_no_labels
            add_and_remove_vertex_label
            label_with_the_reserved_delimiter_is_rejected
            labels_index_matches_scans_after_every_mutation
            edge_endpoints_and_single_label
            set_edge_label_changes_label_index_and_counts
            adjacency_matches_endpoints
            edge_to_a_dropped_vertex_is_an_error_not_a_panic
            stale_handle_never_aliases
            every_property_kind_round_trips
            null_is_stored_and_absent_is_not
            property_overwrite_and_removal
            drop_on_an_invalid_handle
            jpath_writes_create_append_and_fault
            jpath_removals_shift_and_ignore_missing
            graph_elements_are_rejected_as_property_values
            drop_vertex_cascades_its_edges
            drop_edge_updates_adjacency_and_counts
            slot_reuse_does_not_resurrect_dropped_ids
            traversal_smoke_matches_the_reference
            definitions_are_stored_validated_and_removed
            mutation_outside_unit_records_nothing
            schema_application_failure_leaves_graph_and_schema_unchanged
            schema_modes_enforce_declared_properties
            add_applies_schema_defaults_insert_does_not
        ; extension:
            capabilities_are_consistent
            rollback_restores_everything
            savepoint_rollback_keeps_the_outer_unit
            unit_changes_never_claims_an_undone_change
            commit_hooks_follow_the_capability
            applied_change_set_reproduces_the_unit
            change_set_builder_round_trip
            applied_change_set_raises_the_id_sequences
            rollback_restores_definitions
            definition_changes_are_captured
            edge_label_change_rolls_back_and_is_captured
            discard_unit_restores_state_keeps_sequences
            transaction_with_metadata_reaches_hooks
            transactional_panic_rolls_back
        ; ignored:
        }
    };
}

/// [`conformance!`] without the schema items, for a storage that accepts `set_schema` but
/// deliberately does not enforce schemas. A storage whose `set_schema` keeps the trait default
/// (`GraphError::Unsupported`) can use [`conformance!`]: its schema items skip themselves. See
/// the [crate documentation](crate).
#[macro_export]
macro_rules! conformance_core {
    ($m:ident, $base:ident = $init:expr_2021 => $storage:expr_2021) => {
        $crate::__conformance_tests! { $m, $base, $init, $storage;
            explicit_vertex_ids_are_preserved
            auto_vertex_ids_are_unique
            explicit_numeric_vertex_id_bumps_the_sequence
            duplicate_vertex_id_is_rejected
            edge_ids_are_preserved_unique_and_bump_the_sequence
            lookups_after_drop_find_nothing
            vertex_labels_are_an_ordered_set
            unlabeled_vertex_has_no_labels
            add_and_remove_vertex_label
            label_with_the_reserved_delimiter_is_rejected
            labels_index_matches_scans_after_every_mutation
            edge_endpoints_and_single_label
            set_edge_label_changes_label_index_and_counts
            adjacency_matches_endpoints
            edge_to_a_dropped_vertex_is_an_error_not_a_panic
            stale_handle_never_aliases
            every_property_kind_round_trips
            null_is_stored_and_absent_is_not
            property_overwrite_and_removal
            drop_on_an_invalid_handle
            jpath_writes_create_append_and_fault
            jpath_removals_shift_and_ignore_missing
            graph_elements_are_rejected_as_property_values
            drop_vertex_cascades_its_edges
            drop_edge_updates_adjacency_and_counts
            slot_reuse_does_not_resurrect_dropped_ids
            traversal_smoke_matches_the_reference
            definitions_are_stored_validated_and_removed
            mutation_outside_unit_records_nothing
        ; extension:
            capabilities_are_consistent
            rollback_restores_everything
            savepoint_rollback_keeps_the_outer_unit
            unit_changes_never_claims_an_undone_change
            commit_hooks_follow_the_capability
            applied_change_set_reproduces_the_unit
            change_set_builder_round_trip
            applied_change_set_raises_the_id_sequences
            rollback_restores_definitions
            definition_changes_are_captured
            edge_label_change_rolls_back_and_is_captured
            discard_unit_restores_state_keeps_sequences
            transaction_with_metadata_reaches_hooks
            transactional_panic_rolls_back
        ; ignored:
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __conformance_tests {
    ($m:ident, $base:ident, $init:expr_2021, $storage:expr_2021; $($name:ident)* ; extension: $($ename:ident)* ; ignored: $($iname:ident $reason:literal)*) => {
        #[allow(unused_mut, unused_variables, clippy::redundant_closure_call)]
        mod $m {
            use super::*;
            $(
                #[test]
                fn $name() {
                    let mut $base = $init;
                    let mut storage = $storage;
                    $crate::contract::$name(&mut storage);
                }
            )*
            $(
                #[test]
                fn $ename() {
                    let mut $base = $init;
                    let mut storage = $storage;
                    $crate::extension::$ename(&mut storage);
                }
            )*
            $(
                #[test]
                #[ignore = $reason]
                fn $iname() {
                    let mut $base = $init;
                    let mut storage = $storage;
                    $crate::contract::$iname(&mut storage);
                }
            )*
        }
    };
}
