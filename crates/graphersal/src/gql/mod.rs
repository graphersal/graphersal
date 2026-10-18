//! The GQL query language (ISO/IEC 39075), feature `gql`.
//!
//! A GQL read query (`MATCH ... WHERE ... RETURN ...`) is turned into an ordinary traversal plan:
//! it runs through the same optimizer, profile, limits, permissions and storages as a Gremlin
//! traversal. ISO GQL is the normative spelling; common Cypher spellings are accepted as aliases.
//! Version 1 is read-only.
//!
//! Planned stages, one unit each, testable alone (implemented so far: the lexer, the AST and the
//! parser):
//!
//! 1. **lexer** (`lexer.rs`): source text to tokens, each with a span (byte range, line, column);
//!    its module docs fix the lexical rules (contextual keywords, the `--` comment rule, escapes);
//! 2. **parser** (`parser.rs`, AST in `ast.rs`): hand-written recursive descent to an AST that
//!    keeps every span and the source text, with bounded nesting and errors that name the
//!    expected tokens; its module docs are the grammar of the supported subset;
//! 3. **binder**: variables, scopes, parameter references and column names, with GQLSTATUS codes
//!    for semantic errors;
//! 4. **plan**: the logical plan, a pattern graph plus a linear pipeline of operators;
//! 5. **planner**: the start node and expansion order, read only through `GraphStorage`
//!    (statistics and the index catalog), deterministic;
//! 6. **lowering**: the planned query to `GraphStep`s, existing steps plus internal `gql:*`
//!    steps, handed to the optimizer like any other plan.

pub(crate) mod ast;
pub(crate) mod lexer;
pub(crate) mod parser;
