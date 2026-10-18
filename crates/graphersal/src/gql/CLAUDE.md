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
| `ast.rs`, `parser.rs` | hand-written recursive descent to an AST that keeps the source text; `parser.rs` module docs are the grammar and its decisions; `FUNCTIONS` there is the one function table (names, Cypher aliases, arity, scalar / aggregate) for every stage |
| `bind.rs` | variables, scopes, `$param` references, column names, GQLSTATUS for semantic errors |
| `eval.rs` | three-valued expression evaluator: `compile` once (names, constants, functions resolved), then `eval`/`truth` per row through the `Env` trait; its module docs fix the NULL, comparison, arithmetic and function semantics; left spines are loops (`Node::Chain`), so recursion stays within `MAX_DEPTH` |
| `plan.rs` | logical plan: pattern graph plus a linear operator pipeline |
| `planner.rs` | start node, expansion order, joins of repeated variables, edge-uniqueness checks |
| `runtime.rs` | what the internal steps share at run time: `GqlElement` handles, `Binding` (slot -> current object / path label / `gql:project` column), the traverser-backed `Env` (`Row`), value conversions (an element column stays a live `TraverserValue::Vertex`/`Edge`), `eval_error` (evaluator error -> `TraverserError::Gql`, kind `Data` for class `22`, GQLSTATUS kept), the total `ORDER BY` order (`compare_order`; tests of all steps in `runtime/tests.rs`) |
| `lower.rs` | the plan to `GraphStep`s (existing builders plus internal `gql:*` builders; every fallible part first, an error becomes the `gql:error` marker) and the Rust entry points `GraphTraversalSource::gql` / `gql_with` (start step only); its module docs are the table of forms per construct; `as()` only for a binding read while it is not the current element (or a join / trail / branch reason of the plan); `$param` pushdown decided on the value; edge hops choose their form at emission, once the labels are known; its own `lazy_barrier()` merge points between hops; tests `tests/all/gql_tests.rs` (differential vs Rhai twins, profile, errors) |

Files appear as their stage lands; this table is the target layout.

## Rules

- **Hand-written parser.** No parser generator, no new dependency (root rule 10).
- **No panics on any input** (root rule 1): every failure is a `TraverserError`.
- **Bounded recursion.** Deeply nested input yields an error, never a stack overflow: the parser
  bounds its recursion (`MAX_DEPTH`) and every expression tree's height (`MAX_HEIGHT`), so later
  recursive stages recurse at most `MAX_HEIGHT` levels over the AST. Keep the recursive parser functions thin
  (rare arms in functions of their own): unoptimized builds give every temporary a stack slot.
- **Bounded pattern size.** The binder refuses a `MATCH` clause with more than
  `bind::MAX_MATCH_EDGES` (64) edge patterns (kind `Limit`, `54001`): the trail checks grow with
  the square of a clause's edges, and every later stage with them. Keep every stage linear in
  the pattern otherwise (no `contains` scans per edge or per step); measure a long pattern after
  changing the planner, the lowering or the path analysis.
- **Spans everywhere.** Every token and AST node carries byte range, line and column, so every
  later stage can point at the offending text (caret under the span, multi-line aware). Errors
  name the expected tokens, suggest "did you mean ..." and carry the GQLSTATUS code as a field.
- **ISO GQL is normative**; common Cypher spellings are accepted as aliases. A refused or not yet
  supported construct gets a targeted message naming the GQL form or the Gremlin alternative.
- **Diagnostics** (`error.rs`): a failure of the `gql:error` marker leads with the GQL error
  (`step_failed_message`, no `Caused by:` line); the message and excerpt of a GQL root cause are
  never respelled under `render.spelling = camel`, and its `help()` respells the Gremlin steps
  but keeps the GQL function names (`respell_keeping`, `parser::lookup_function`). Advice is
  built from the failing query where it can be (`lower::start_with_gql`, the `$param` fixes).
  Tests: `error_diagnostics_tests.rs` (`mod gql`), robustness `random_gql_strings_never_panic`.
- **Read-only in version 1.** Write clauses are refused by the parser.
- **Lower into the ordinary pipeline.** No separate executor: the lowering builds plans with the
  ordinary builders and lets the optimizer do pushdown. Never push a `<>` comparison down
  (three-valued logic differs from `has()`).
- **The planner reads only through `GraphStorage`** (statistics, index catalog), never
  `TraversalGraph` internals, so it works on a host's own storage; its plan is deterministic.
- **Internal steps are `gql:*` and `pub(crate)`**: one file per step in its category, with arms in
  `for_each_access`, `path_requirement()` where they read the path, and `gql:` in the profile.
  Built: `gql:filter` (Filter, `filter_steps/step_gql_filter.rs`), `gql:project` (Map,
  `map_steps/step_gql_project.rs`, in `MAP_PRODUCING_KINDS`), `gql:order` (FlatBarrier,
  `flatbarrier_steps/step_gql_order.rs`), `gql:not_self_loop` (Filter, the incoming branch of an
  either-direction edge, `filter_steps/step_gql_not_self_loop.rs`), the `gql:error` marker
  (Filter, `plan_check`, `filter_steps/step_gql_error.rs`). They ask no permission of their own
  (value-only, like `has()`); their rendering puts GQL text in string literals so camelCase
  respelling leaves it alone. Trail checks use `where(P.neq("e1"))`, joins `where(P.eq("x"))`,
  no internal step.
- **Edge forms** (table in `lower.rs`): `out()`/`in()` when nothing runs on the edge, else
  `out_e()..in_v()`; either direction is `union(out.., in_e().gql:not_self_loop()..)`, never
  `both_e().other_v()` (full paths) or plain `both()` (a self-loop twice).
- **Merge points:** the lowering places its own `lazy_barrier()` between hops; never change
  `LazyBarrierRule` or another global rule for GQL (it would move existing Gremlin plans).
