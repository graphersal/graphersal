//! The GQL query language (ISO/IEC 39075), feature `gql`.
//!
//! A GQL read query (`MATCH ... WHERE ... RETURN ...`) is turned into an ordinary traversal plan:
//! it runs through the same optimizer, profile, limits, permissions and storages as a Gremlin
//! traversal. ISO GQL is the normative spelling; common Cypher spellings are accepted as aliases.
//! Version 1 is read-only.
//!
//! Planned stages, one unit each, testable alone (none is implemented yet):
//!
//! 1. **lexer**: source text to tokens, each with a span (byte range, line, column);
//! 2. **parser**: hand-written recursive descent to an AST that keeps every span and the source
//!    text, with bounded recursion depth and errors that name the expected tokens;
//! 3. **binder**: variables, scopes, parameter references and column names, with GQLSTATUS codes
//!    for semantic errors;
//! 4. **plan**: the logical plan, a pattern graph plus a linear pipeline of operators;
//! 5. **planner**: the start node and expansion order, read only through `GraphStorage`
//!    (statistics and the index catalog), deterministic;
//! 6. **lowering**: the planned query to `GraphStep`s, existing steps plus internal `gql:*`
//!    steps, handed to the optimizer like any other plan.
