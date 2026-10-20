//! The GQL query language (ISO/IEC 39075), feature `gql`.
//!
//! A GQL read query (`MATCH ... WHERE ... RETURN ...`) is turned into an ordinary traversal plan:
//! it runs through the same optimizer, profile, limits, permissions and storages as a Gremlin
//! traversal. ISO GQL is the normative spelling; common Cypher spellings are accepted as aliases.
//! Version 1 is read-only.
//!
//! The entry points are [`GraphTraversalSource::gql`](crate::GraphTraversalSource::gql) and
//! [`GraphTraversalSource::gql_with`](crate::GraphTraversalSource::gql_with) (in `lower.rs`).
//!
//! Stages, one unit each, testable alone:
//!
//! 1. **lexer** (`lexer.rs`): source text to tokens, each with a span (byte range, line, column);
//!    its module docs fix the lexical rules (contextual keywords, the `--` comment rule, escapes);
//! 2. **parser** (`parser.rs`, AST in `ast.rs`): hand-written recursive descent to an AST that
//!    keeps every span and the source text, with bounded nesting and errors that name the
//!    expected tokens (stage 2d: composite queries of `UNION` / `NEXT` and `CALL { }` bodies;
//!    stage 2e: parenthesized path patterns, bound since 2e.T2, planned since 2e.T3
//!    (`plan::Op::Group`), refused "not yet" before planning, `bind::refuse_unplanned_groups`,
//!    until the lowering takes them);
//!    its module docs are the grammar of the supported subset; its expressions
//!    compile into the three-valued evaluator (`eval.rs`), which runs them per row, with the
//!    stage 1c scalar functions in `functions/` (one file per family);
//! 3. **binder** (`bind.rs`): variables, scopes, parameter references and column names, with GQLSTATUS codes
//!    for semantic errors;
//! 4. **plan** (`plan.rs`): the logical plan, per connected component of the pattern graph a scan
//!    and expansions with join, trail and filter checks, then the result pipeline; it also
//!    classifies which conjuncts may be pushed into `has()`;
//! 5. **planner** (`planner.rs`): the start node and expansion order, the correlated lookup of
//!    a later component (only where an id or a property index serves it), read only through
//!    `GraphStorage` (statistics and the property indexes), deterministic;
//! 6. **lowering** (`lower.rs`, its module docs list the form of every construct): the planned
//!    query to `GraphStep`s, existing steps plus internal `gql:*`
//!    steps (`gql:filter`, `gql:project`, `gql:order`, `gql:not_self_loop`, the `gql:error`
//!    marker; for stage 1b `gql:aggregate`, `gql:distinct`, `gql:bind`, `gql:trail`; for stage 2a
//!    `gql:for` (`FOR` / `UNWIND`), `gql:lookup` (a component that is not the root scan found
//!    per row by id or property index) and the unit row `inject(0)` of a query that does not
//!    start with `MATCH`; their shared
//!    run-time part is `runtime.rs`, the aggregation semantics `aggregate.rs`), handed to the
//!    optimizer like any other plan.

pub(crate) mod aggregate;
pub(crate) mod ast;
pub(crate) mod bind;
pub(crate) mod eval;
pub(crate) mod functions;
pub(crate) mod lexer;
pub(crate) mod lower;
pub(crate) mod parser;
pub(crate) mod plan;
pub(crate) mod planner;
pub(crate) mod runtime;
