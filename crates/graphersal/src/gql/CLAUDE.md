# GQL (`src/gql/`, feature `gql`)

The ISO GQL read query language on top of the traversal pipeline. A GQL query is parsed, bound,
planned and lowered into an ordinary `Vec<GraphStep>`, which then goes through the optimizer, path
analysis, profile, limits, permissions and every storage exactly like a Gremlin traversal. The
feature is off by default, strictly additive, has no dependencies, does not need `script`, and
builds for wasm32. The module doc of `mod.rs` lists the stages and what is implemented.

## Stage layout (one file per stage, each testable alone)

| File | Stage |
|---|---|
| `lexer.rs` | tokens with spans; comments `//`, `/* */`, `--` (comment unless `-->`/`--(` or after `)` on the same line); back-quoted identifiers; contextual, case-insensitive keywords; its module docs are the lexical spec |
| `ast.rs`, `parser.rs` | hand-written recursive descent to an AST that keeps the source text |
| `bind.rs` | variables, scopes, `$param` references, column names, GQLSTATUS for semantic errors |
| `plan.rs` | logical plan: pattern graph plus a linear operator pipeline |
| `planner.rs` | start node, expansion order, joins of repeated variables, edge-uniqueness checks |
| `lower.rs` | the plan to `GraphStep`s: existing steps plus internal `gql:*` steps |

Files appear as their stage lands; this table is the target layout.

## Rules

- **Hand-written parser.** No parser generator, no new dependency (root rule 10).
- **No panics on any input** (root rule 1): every failure is a `TraverserError`.
- **Bounded recursion.** Deeply nested input yields an error, never a stack overflow.
- **Spans everywhere.** Every token and AST node carries byte range, line and column, so every
  later stage can point at the offending text (caret under the span, multi-line aware). Errors
  name the expected tokens, suggest "did you mean ..." and carry the GQLSTATUS code as a field.
- **ISO GQL is normative**; common Cypher spellings are accepted as aliases. A refused or not yet
  supported construct gets a targeted message naming the GQL form or the Gremlin alternative.
- **Read-only in version 1.** Write clauses are refused by the parser.
- **Lower into the ordinary pipeline.** No separate executor: the lowering builds plans with the
  ordinary builders and lets the optimizer do pushdown. Never push a `<>` comparison down
  (three-valued logic differs from `has()`).
- **The planner reads only through `GraphStorage`** (statistics, index catalog), never
  `TraversalGraph` internals, so it works on a host's own storage; its plan is deterministic.
- **Internal steps are `gql:*` and `pub(crate)`**: one file per step in its category, with arms in
  `for_each_access`, `path_requirement()` where they read the path, and `gql:` in the profile.
