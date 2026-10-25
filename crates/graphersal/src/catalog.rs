//! The catalog: named definitions stored beside the schema (saved queries, property indexes,
//! compression rules and host-defined kinds 128-255).
//!
//! For hosts that manage saved queries or keep their own definitions in a graph's catalog. A host
//! kind is stored opaque, byte for byte; `encode_payload`/`decode_payload` (feature `persist`)
//! are the payload codec the library uses for its own kinds. Guides: the book pages
//! `users_guide/saved_queries.md`, `users_guide/property_indexes.md` and
//! `users_guide/compressed_properties.md`.
//!
//! # The catalog
//!
//! The catalog of definitions: named, typed entries stored IN the database next to the schema
//! (`book/src/users_guide/saved_queries.md`). Three kinds are defined: the **saved query**
//! ([`QueryDefinition`], kind `1`), the **property index** ([`PropertyIndexDefinition`], kind
//! `2`) and the **compression rule** ([`CompressionDefinition`], kind `3`); kinds
//! `128..=255` belong to hosts, every other kind is unassigned; a kind this version does not
//! decode is kept as an opaque entry (see below).
//!
//! ## Model
//!
//! A [`Definition`] is `kind | flags | name | payload`:
//!
//! - [`DefinitionKind`] is a byte; a reader knows some kinds ([`DefinitionKind::is_known`]) and
//!   keeps every other one **opaque** ([`DefinitionBody::Opaque`]: the payload bytes exactly
//!   as read), so a definition written by a newer version survives an older one byte for byte.
//! - [`DefinitionFlags`] bit 0 is *critical*: a reader that does not know the kind must not
//!   write the store (it could not keep the definition consistent with the data). Bits 1-7 are
//!   reserved and preserved.
//! - The name is unique per kind and case-sensitive.
//! - The payload of a known kind is a property map (format spec: the codec's tagged value
//!   encoding); a saved query's keys are listed at [`QueryDefinition::to_payload`]. Keys this
//!   version does not know are kept ([`QueryDefinition::extra`]) and written back.
//!
//! ## Kind ranges
//!
//! | Kinds | Owner |
//! |---|---|
//! | `0` | invalid |
//! | `1..=127` | the library: `1` saved query, `2` property index, `3` compression rule, the rest unassigned |
//! | `128..=255` | hosts ([`DefinitionKind::HOST_FIRST`]..=[`DefinitionKind::HOST_LAST`]): never assigned by the library |
//!
//! A host (a server built on the library, say) adds its own catalog entities under a host kind
//! ([`DefinitionKind::host`]): it builds them with [`Definition::opaque`] and encodes the payload
//! as a property map with `encode_payload` / `decode_payload` (feature `persist`), the codec
//! of the library's own kinds. The library stores such a definition as [opaque](DefinitionBody::Opaque)
//! bytes: validated like every opaque definition, carried byte for byte through the WAL,
//! checkpoints, forks, backups and repairs, and never damage. The [critical](DefinitionFlags::is_critical)
//! flag means what it means for every kind this library does not decode: a store holding a
//! critical host definition opens read-only, and a journal refuses to store one.
//!
//! ## Where definitions live
//!
//! [`GraphStorage`](crate::graph::GraphStorage) has `definitions` / `definition` /
//! `set_definition` / `remove_definition` (defaults: none stored, writes unsupported);
//! [`TraversalGraph`](crate::graph::TraversalGraph) keeps the catalog beside the schema, at
//! most [`MAX_DEFINITIONS`] entries. A catalog change is a database change like a schema
//! change: recorded in the open unit (rolled back with it), captured as a
//! [`Mutation::SetDefinition`](crate::graph::Mutation::SetDefinition), seen (and possibly
//! vetoed) by the commit hooks, replayed by `replay` and `apply_change_set`; outside a unit it
//! is a unit of its own. [`GraphTraversalSource`](crate::GraphTraversalSource) offers the
//! authorized API (`list_definitions`, `get_definition`, `set_definition`, `define_query`,
//! `remove_definition`, `move_query`, `describe_query`).
//!
//! ## What is checked where
//!
//! [`Definition::validate`] (run by every `set_definition`) checks the name, the folder, the
//! sizes, the parameters (names, schemas, defaults) and nothing else: the body of a saved query
//! (`source`, Rhai) is compiled by the script layer when the query is defined through the DSL
//! and every time it is called, never here (no Rhai in the core).
//!
//! ## Text forms
//!
//! [`Definition::to_json`] / [`Definition::from_json`] (and [`catalog_to_json`] /
//! [`catalog_from_json`], `{"definitions": [..]}`) are the exchange form (`queries.json`, the
//! playground session, the serialized `ChangeSet`): the payload keys plus `kind`, `name` and
//! `flags`, with `params` as an object instead of a string. Reading is lenient where a newer
//! version may have added something (unknown keys are kept) and strict where only a mistake
//! can be the cause (a known key of the wrong type). A user-facing parameter declaration is
//! read strictly by [`QueryParam::from_value`].

pub use crate::error::DefinitionError;
pub use crate::graph::compression::{
    Codec, CompressionDefinition, CompressionStats, DEFAULT_DEDUP_MIN_BYTES, DEFAULT_MIN_BYTES,
    MAX_MIN_BYTES,
};
pub use crate::graph::compression_estimate::{
    CandidateEstimate, CompressionCandidate, CompressionEstimate, CompressionEstimateRequest,
    CurrentCompression, DEFAULT_DEDUP_ESTIMATE_THRESHOLDS, DEFAULT_ESTIMATE_MAX_BYTES,
    DEFAULT_ESTIMATE_MAX_VALUES, DEFAULT_ESTIMATE_THRESHOLDS, EstimateWarning, SizeBucket,
};
pub use crate::graph::definition::{
    Definition, DefinitionBody, DefinitionFlags, DefinitionKind, GQL_QUERY_DIALECT,
    GQL_WRITING_QUERY_DIALECT, MAX_DEFINITION_NAME_BYTES, MAX_DEFINITIONS, MAX_DESCRIPTION_BYTES,
    MAX_DIALECT_BYTES, MAX_FOLDER_BYTES, MAX_FOLDER_SEGMENTS, MAX_META_BYTES,
    MAX_OPAQUE_PAYLOAD_BYTES, MAX_PARAMETERS, MAX_QUERY_NAME_BYTES, MAX_SOURCE_BYTES,
    QUERY_DIALECT, QueryDefinition, QueryParam, RESERVED_PAYLOAD_KEYS, WRITING_QUERY_DIALECT,
    catalog_from_json, catalog_to_json,
};
#[cfg(feature = "persist")]
pub use crate::graph::definition::{decode_payload, encode_payload};
pub use crate::graph::jobs::{
    JobDefinition, JobTask, MAX_JOB_EFFECTS, MAX_JOB_PATH_BYTES, MAX_SCHEDULE_BYTES, RunOn,
};
pub use crate::graph::property_index::{
    MAX_INDEX_KEY_BYTES, MAX_INDEX_LABEL_BYTES, PropertyIndexDefinition, PropertyIndexTransform,
    PropertyIndexType,
};
