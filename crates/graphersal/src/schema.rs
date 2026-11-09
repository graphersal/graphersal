//! Schemas: the format, enforcement modes, validation, inference, diff, patch and visualization.
//!
//! For everyone who declares, checks or evolves the shape of a graph's data, from a query writer
//! freezing a structure (`g.set_schema(g.infer_schema(), ..)`) to a server validating a schema
//! patch before applying it. Guide: the book page `users_guide/schema_enforcement.md`.
//!
//! # Loading and printing
//!
//! Loading the schema format (JSON text or an [`ElementProperty`](crate::ElementProperty)
//! value: a Rhai map, a Python dict) into a [`GraphSchema`], and writing it back in its
//! canonical form.
//!
//! Loading is strict: an unknown keyword is an error everywhere, the envelope included, and so
//! is every JSON Schema keyword outside the supported subset. Every error names its location as
//! a JSON Pointer into the input (`/vertices/person/schema/properties/age/minimum`). Local
//! `$ref`s (`#/$defs/Name`) are inlined, so a loaded schema has neither `$ref` nor `$defs`.
//!
//! # Diff and compatibility
//!
//! What changes between two schemas, and whether each change is backward compatible.
//!
//! ## Compatibility
//!
//! A change is **backward compatible** when data and clients written against the old schema keep
//! working: the old data stays valid and every write that followed the old schema still succeeds.
//! In short, a compatible change only *adds* (an optional key, a label, an allowed value) or
//! *relaxes* (a wider type, a looser bound, a less strict mode); a change that *tightens* a rule
//! or removes a declaration something could depend on (a label, a key of a closed object) is
//! incompatible. This is the usual schema-registry convention: adding an optional property is
//! compatible, also to an open object, and so is removing a key declaration from an open object
//! (the key becomes an undeclared one, which the object admits). A "compatible changes only"
//! policy (a `before_commit` hook) admits exactly the compatible changes.
//!
//! The classification is static: it compares the two schemas, never the data. Whether the
//! *existing* data conflicts with the new schema (an element that already holds a newly declared
//! key with another type, say) is what
//! [`validate_schema`](crate::GraphTraversalSource::validate_schema) finds; applying the schema
//! fails on such data anyway.
//!
//! | Change | Compatible when |
//! |---|---|
//! | `mode` | it gets less strict (`closed` → `open` → `none`); stricter is incompatible |
//! | `meta`, annotations (`title`, `description`, ...) | always |
//! | `default`, `defaultFunction` (added, changed, removed) | always: it changes only what a create stores for an absent top-level property, never existing data or a valid write; listed as `default_changed` |
//! | a vector's `embedder` (the model that produced its vectors) | when it is declared where there was none; changing or removing it is incompatible (stored vectors and a text query embedded by another model are not comparable; a text query needs the model); listed as `embedder_changed` |
//! | label added | the old mode was `closed` (no client could write it), or its schema requires no key and admits undeclared keys |
//! | label removed | never |
//! | connection added / removed | the edge label's connections still allow every old pair (the new list is empty, or contains every old pair), or the new mode is not `closed` (topology binds only there) |
//! | property added | it is optional (not in `required`), also in an open object; a key the old schema already required without declaring it is compatible only if the new declaration accepts everything |
//! | property removed | the new object admits undeclared keys (open: `mode` `open`/`none` or `additionalProperties: true`), so old data and old writers stay valid; from a closed object never |
//! | key made required | never |
//! | key made optional | always |
//! | `additionalProperties` | `false` → `true` |
//! | type | the new type admits every kind the old one did (`["integer"]` → `["integer", "null"]`) |
//! | a bound (`minimum`, `maxLength`, `minItems`, `formatMinimum`, ...) | it is removed or relaxed |
//! | `format` (`date` → `date-time`, added, removed) | never: it is another type |
//! | `pattern`, `const` | removed |
//! | `enum` | removed, or a superset |
//!
//! With a new mode `none` every change is compatible (nothing is enforced any more).

pub use crate::graph::schema::{
    Annotations, Constraints, DefaultFunction, EdgeConnection, EdgeSchema, GraphSchema, JsonType,
    META_SCHEMA_JSON, PropertySchema, SCHEMA_FORMAT_URL, SchemaChange, SchemaChangeKind,
    SchemaDiff, SchemaFormat, SchemaInferer, SchemaMode, SchemaPatch, SchemaPattern, SchemaRow,
    SchemaSummary, SchemaSummaryRequest, SchemaValidation, SchemaViolationReport, ValueType,
    VectorSchema, VertexSchema, ViolationGroup, ViolationKind, VisualizeFormat, type_of,
};
pub use crate::graph::schema::{
    MAX_GROUPS, PRINTED_GROUPS, SAMPLE_ID_CHARS, SAMPLE_IDS, UNLABELED,
};
/// The bounds of an aggregated `SchemaError::ExistingDataViolatesSchema` report (groups kept and
/// printed, sample ids per group and their length) and the group key of elements without a label.
/// Vector rules (book page "Vector Rules"): the fills a schema change starts and the one
/// warning text every front end shows before applying it.
pub use crate::graph::vector_rules::{FillReason, PlannedFill, fill_warning};
