# GQL (`src/gql/`, feature `gql`)

The ISO GQL query language (reads, and since stage 3a `INSERT` / `CREATE` writes) on top of the
traversal pipeline. A GQL query is parsed, bound,
planned and lowered into an ordinary `Vec<GraphStep>`, which then goes through the optimizer, path
analysis, profile, limits, permissions and every storage exactly like a Gremlin traversal. The
feature is off by default, strictly additive, has no dependencies, does not need `script`, and
builds for wasm32. The module doc of `mod.rs` lists the stages and what is implemented.

## Stage layout (one file per stage, each testable alone)

| File | Stage |
|---|---|
| `lexer.rs` | tokens with spans; comments `//`, `/* */`, `--` (comment unless `-->`/`--(` or after `)` on the same line); back-quoted identifiers; contextual, case-insensitive keywords; its module docs are the lexical spec |
| `ast.rs`, `parser.rs` | hand-written recursive descent to an AST that keeps the source text (stage 2d: a `Query` is a `CompositeQuery` of `LinearQuery` operands, `UNION` blocks and `NEXT`, and dereferences to its first linear query; `Stage::Call` bodies and whole-query subquery bodies are `CompositeQuery`s; the functions from `query` down to `return_clause` stay thin and pass boxes, every frame there lies under each nested expression; stage 2e: parenthesized path patterns `( chain [WHERE] )` with a quantifier, `?` or none as `PathPattern::groups` (`GroupStep`: the `ParenthesizedPath`, the juxtaposed node, the plain steps after it; an implicit empty node, `NodePattern::is_implicit`, where the text has none next to one), E17 refusals by name; the functions from `path_pattern` down to `node_pattern` are thin out-of-line frames filling nodes in place, measured with the deep-nesting tests; bound since 2e.T2, planned since 2e.T3, lowered since 2e.T4; since 2e.T5 the pattern-predicate and pattern-comprehension lookaheads find them too: a node followed by `((`, a leading `((` whose body is a pattern and whose `)` is followed by `{..}`, `?`, `(`, an edge or a `*` / `+` without an operand, a non-recursive token scan); stage 3a: `Stage::Insert` (`INSERT` / Cypher `CREATE`, each pattern parsed as a path pattern then checked for the insert form, `check_insert_node` / `check_insert_edge`), `FINISH` and a linear query ending in a write clause (empty `ret`, `LinearQuery::end`), `Parser::writes` (write keyword spans: a writing `UNION` operand, a `CALL` body ending in a write), `Parser::write_ban` (writes inside `EXISTS` / `COUNT` / `COLLECT` / comprehensions), catalog `CREATE INDEX` / `CONSTRAINT` / `GRAPH` refused (`catalog_create`); bound since 3a.T2, planned and lowered since 3a.T4; `parser.rs` module docs are the grammar and its decisions; `FUNCTIONS` there is the one function table (names, Cypher aliases, arity, scalar / aggregate) for every stage; stage 3f: a chained comparison `a < b < c` is parsed as `a < b AND b < c` (`chained_comparison`, out of line so the recursive frame stays small; the middle operand cloned), Cypher's empty interval `*2..1` / `*..0` is `Quantifier::empty` (`{0}`, lowered as `discard()` after it), the GQL `{2,1}` stays 42001 |
| `bind.rs` | variables, scopes along the stages, comprehension locals (E14: never shadowing a name, unreadable inside a subquery yet: the error names the comprehension, list predicate or `reduce` that binds it, `LocalOwner`), segments (plain `MATCH` runs, `OPTIONAL MATCH`, materializing `WITH`), `LET` / projecting `WITH` as substitutions (bounded: `MAX_SUBSTITUTED_NODES`), group and path variables, subqueries (`EXISTS`, pattern predicates, pattern comprehensions as `SubqueryKind::Collect` with their `|` part in `BoundSubquery::map`: nested scopes, imports, locals, `BoundSubquery`, `MAX_SUBQUERIES`; a body condition or `|` part marks only the body's own locals read), implicit grouping checks (and the keys of an ISO `GROUP BY` on `RETURN`), `$param` references, column names, GQLSTATUS for semantic errors; stage 2d: every linear query a pipeline (`BoundLinear`, its segments in the one `segments` table, `BoundQuery` dereferences to the first), `UNION` blocks (E15: same names, reordered to the first operand), `NEXT` desugared to the 1b `WITH` (E19; after a `UNION` block a `Segment::Rows`), `Segment::Call` (imports, new output columns, `OPTIONAL`), whole-query subquery bodies (`BoundSubquery::query`), `MAX_BODY_DEPTH`; stage 2e: parenthesized path patterns as `BoundPath::groups` (`BoundGroupStep`, `BoundGroup`: body bindings new and consecutive, group variables when quantified, singletons without a quantifier, ISO conditional singletons with `?` (`BoundGroup::conditional`: an outer condition on one is a cross conjunct, `local_binding`; `{0,1}` written out makes group variables); merge rule `BoundPath::node_before`; body conditions see only the body, `Binder::in_body`, 0A000 otherwise, single-element ones on the element, the rest `BoundGroup::conjuncts`, never cross, marking nothing read; `ConjunctOrigin::BodyWhere`; each body edge counts toward `MAX_MATCH_EDGES`); stage 3a: `INSERT` / `CREATE` as `Segment::Write` (`BoundWrite::Insert`, `BoundInsert`: new nodes, then edges with source / target, node references without labels or properties, property maps without aggregates or reads of the clause's own elements, `reads`), W10 `materialize_for_write` (a `WITH *` before a write when a `LET` / projecting-`WITH` value reading the graph is in scope, also before a `CALL` whose body writes, `composite_writes`), `BoundLinear::end` (`RETURN`, `FINISH`, write ending: no columns), `BoundCall::unit` (a `CALL` body ending in `FINISH` or a write); the recursive functions (`resolve`, `substitute`, the composite path) are thin dispatches with rare forms in functions of their own (debug stack frames) |
| `eval.rs` | three-valued expression evaluator: `compile` once (names, constants, functions resolved; a subquery through `Scope::subquery` to `Node::Subquery(i)`; stage 2c map projections and list comprehensions, a comprehension variable a local through `Scope::local` to `Node::Local(i)`, bound per item by `LocalEnv` on the call stack over a `dyn Env` parent so nesting neither allocates nor instantiates the evaluator again; stage 2d: Cypher's `all` / `any` / `none` / `single` and `reduce` are the same node with its `Form` (parsed by name ahead of `FUNCTIONS`, `ast::ComprehensionForm`; `reduce` two locals, its `init` outside them, visited by every walk through `ListComprehension::init`), three-valued per the openCypher TCK; unit tests `eval/local_tests.rs`), then `eval`/`truth` per row through the `Env` trait (`Env::subquery(i)`: `None` stops the evaluation, the hosting step runs the child and evaluates again); its module docs fix the NULL, comparison, arithmetic and function semantics; left spines are loops (`Node::Chain`), so recursion stays within `MAX_DEPTH`; stage 3f: Cypher indexing / slicing `x[i]`, `x[a..b]`, `m[k]`, `n[k]` (`ast::ExprKind::Index`, parsed in the postfix loop, `x['key']` with a string literal parsed as `x.key` so plans and pushdown stay; chain links `LinkOp::Index` / `LinkOp::Slice`, errors 22G03 with `GqlHelpTopic::Subscript`; tests `eval/tests.rs`, `tests/all/gql_index_tests.rs`) |
| `functions/` | the stage 1c scalar functions, pure: `mod.rs` (dispatch by `family()`, the contract of a family file, shared argument helpers: NULL rule F2, 22G03 `wrong_kind`, integer / string / number arguments, `reserve` for results larger than their inputs), `convert.rs` (`CAST` strict, Cypher `toX` lenient, `toXOrNull`; one core with `Mode`), `strings.rs` (incl. the SQL `TRIM` forms), `numbers.rs`, `lists.rs` (lists, `keys` / `properties` through `Env::properties`, `startNode` / `endNode` and `IS [NOT] SOURCE OF` / `DESTINATION OF` through `Env::endpoints`, `nodes` / `relationships`); `test_env.rs` the test row of their unit tests. User documentation (keep in sync with a family file): book `gql/subset.md` "Functions" (tables, runnable examples), `gql/semantics.md` "Functions" (the F-rules, strict `CAST` vs lenient `toX`, rounding, the differences from Neo4j), `gql/aliases.md` "Compatibility functions", `gql/errors.md` (GQLSTATUS table), `gql/conformance.md`. F8: they ask no permission of their own (every edge pattern's lowering reaches both endpoints); a future planner that scans `e()` directly needs an access arm for `startNode` / `endNode` |
| `plan.rs` | logical plan: a tree of pipelines (stage 2d: `CompositePlan` of `UnionPlan` blocks chained by `NEXT`, each operand a `PipelinePlan`; the plan dereferences to its first pipeline, the whole query when linear); a pipeline is the first segment's operator pipeline, the later stages (`Stage::Match` / `Optional` / `Materialize` / `For`; 2d: `Stage::Call` holding its body's plan, `Stage::Rows` after `NEXT`; 3a: `Stage::Write`), the result pipeline (`Project` or `Aggregate`, ...; empty after `FINISH` or a write ending); stage 2e: `Op::Group` (`GroupExpand`: a parenthesized path pattern walked from an outer node over its `BodyHop`s, reversed hop by hop from the far end, body conditions per hop, trail per body edge, `GroupRef` into the bound query); the labelling reasons per binding (2e: `Labelling::body`), the subquery bodies (`SubqueryPlan`, one per subquery and start, `query` for a whole-query body; `SubqueryUse`: fast-path `EXISTS` conjuncts in `ElementFilters::exists` / last in `Op::Filter`, E11) |
| `planner.rs` | per segment: start node (a binding of an earlier segment: `Op::Resume`; a component that is not the root scan with an equality an id or index serves: `Op::Lookup`, design M3, E5), expansion order (plain edges before quantified ones), joins of repeated variables, edge-uniqueness checks (group variables included); then one pass in execution order for the position-dependent labels (reads after an `OPTIONAL MATCH`, `WITH` row labels), which also finds every subquery host and plans each body with its imports bound before it (design M2, E7: `Op::Resume` from the host's current element or an import, joins to imports, a child-leading scan; `Labelling::subquery` for an import read away from the current element inside a body; a worklist over nested bodies); stage 2d: every pipeline planned on its own (`Planner::composite` / `pipeline`, `SegmentKind`), a `CALL` body planned at its `CALL` (recursion, `MAX_BODY_DEPTH`) from the current element there, its first component may be a correlated lookup (since 2d.T5c also the first component of a body of `MATCH` clauses: `SegmentKind::Body` for every body); row labels of a `CALL` (`#c<k>`) and of `Stage::Rows` (`#n<k>`) by the position pass; stage 2e: a parenthesized path pattern is one pattern edge between its outer nodes (merge rule without joins of its own), walked as `Op::Group` (`expand_group`, ranked like a quantified edge), body elements in the component but never a start nor reached by another edge first, never an `OPTIONAL MATCH` witness; stage 3a: a write clause is a `Stage::Write` (its new elements bound after it, the last one the current element; the position pass reads each element's values where its insert step runs and hosts their subqueries there), a unit `CALL` keeps the outer row and its current element; a stage 1a query plans exactly as in stage 1a |
| `runtime.rs` | what the internal steps share at run time: `GqlElement` handles, `Binding` (slot -> current object / path label / `gql:project` column / labelled row column / group list / assembled path; stage 2e: `PathPart::Body`, a parenthesized path pattern's iterations from its body's labels, one occurrence each per iteration, reversed when walked from the far end), the traverser-backed `Env` (`Row`), `SubqueryHost` (the 2b hook: the subquery children a `gql:*` step holds, `child_traversals_mut` / `analyze_nested_paths` for every walker, lazy evaluation at most once per row, `SubqueryAnswers` bits plus the collected values; a child's mode read from its last step (`ChildMode`): `gql:value` collects (a pattern comprehension or `COLLECT { }`), `count()` counts (`COUNT { }`, `counted_run`, one unit of the row, bulk counted; the mode is read before the optimizer may fuse the `count()`), anything else is `EXISTS`; a collecting child runs to its end, `collect_list` checks `bulk.max_expansion`, `traversal.max_materialized_bytes` and the memory budget before building; `MAX_HOSTED_SUBQUERIES`; step tests `runtime/value_step_tests.rs`), value conversions (an element column stays a live `TraverserValue::Vertex`/`Edge`), `eval_error` (evaluator error -> `TraverserError::Gql`, kind `Data` for class `22`, GQLSTATUS kept), the total `ORDER BY` order (`compare_order`; tests of all steps in `runtime/tests.rs`) |
| `lower.rs` | the plan to `GraphStep`s (existing builders plus internal `gql:*` builders; every fallible part first, an error becomes the `gql:error` marker) and the Rust entry points `GraphTraversalSource::gql` / `gql_with` (start step only); its module docs are the table of forms per construct; stage 2d: walks `plan.body` and `bound.body` in parallel (`Lowering::composite` / `linear`), a `UNION` block as `union(__.<operand>, ..)` (start step at the start, mid-traversal after `NEXT`) + `gql:distinct()`, `Stage::Rows` as a row read like a `WITH` row (`#n<k>`), one result pipeline per linear query (`Lowering::linears`, `result_of`: `segments.len() + p`, `#r` / `#r<p>`), `[OPTIONAL] CALL { }` as `flat_map(__.<body>)` (`Instr::Call`; the body a composite lowered from the site of the `CALL`, `Start::At`, no unit row; `as("#c<k>")` only when read after leaving the row; `OPTIONAL CALL` as `optional(__.flat_map(..).as("#c<k>"))`), stage 2d.T5c: whole-query subquery bodies lowered by `Lowering::body` through `composite(.., Start::At(site))` onto the subquery's child (fast path or hook as before), `COUNT { }` closed by `Instr::Count` (`count()`), `COLLECT { }` by `Instr::Collect` (`gql:value` of `Binding::Column(0)`), tests `gql_composite_tests.rs`; `as()` only for a binding read while it is not the current element (or a join / trail / branch reason of the plan); `$param` pushdown decided on the value; edge hops choose their form at emission, once the labels are known; its own `lazy_barrier()` merge points between hops; stage 1b pipeline: one emission onto `g` or a child (`Sink`), `OPTIONAL MATCH` as `optional(__..)` (leading: `inject(0)`), a materializing `WITH` read from its row (`Binding::Column` on it, `RowColumn` through `as("#w<k>")` only when read after leaving it, `gql:bind` to continue a pattern, joins / trail against a column as `gql:filter`), `FOR` / `UNWIND` as `gql:for` (the item the current object, `as("x")` only when read after leaving it; with an ordinal the record `{x, i}` read like a `WITH` row, `as("#f<k>")`; a later `MATCH` from the item through `gql:bind`, E3), a query not starting with `MATCH` from the unit row `inject(0)` (M1), a top-level `[NOT] EXISTS` conjunct / pattern predicate as `filter(__.<body>)` / `not(__.<body>)` after the element's `as()` (the body plan for the host's current element lowered from the host site; no merge point of the lowering in a child; `MAX_LOWERED_SUBQUERIES`), `EXISTS` anywhere else in an expression, and every pattern comprehension (its body ending in `gql:value(expr)`, `Instr::Value`), as a child of the `gql:*` step evaluating it (a residual on an element holding one also after the element's `as()`; `Bodies` on `Instr::Filter` / `Project` / `Aggregate` / `Order` / `For`, collected per step by `LowerScope::subquery`; `mark_reads` and `in_part` recurse into them), `DISTINCT` as `gql:distinct()`, an aggregating `RETURN` / `WITH` as `gql:aggregate` in place of `gql:project` (`aggregate::compile_aggregating`, key = column without an aggregate; its fresh path drops every label, so its `ORDER BY` reads a variable a column holds from that column, `Site::ordered`: `Binding::Column` on the row, `RowColumn` through `as("#w<k>")` / `as("#r")` set before `gql:order` inside a subquery body, whose `select()` becomes `gql:bind` and label joins row checks, `Lowering::in_order`), a quantified edge as `repeat()` of the single hop with `gql:trail` before its `as()` (bounds as `times()` / `emit()` modulators, `{0}` no step, no `times()` when unbounded: `repeat.max_loops` is the D14 error), group variables as `Binding::Group` (text order, anchored in an `OPTIONAL MATCH` part on its witness or `as("#o<k>")`); stage 2e: a parenthesized path pattern (`Op::Group`) as `repeat(__.<body>)` with the quantified edge's modulators (`Instr::Repeat`, `Lowered::looped`; `?` `emit().repeat(B).times(1)`, no quantifier the body inline, `{0}` no step), the body one iteration in walk order (`Lowering::group_body`: `first`'s filters and `as()`, per hop the 1a edge form with `gql:trail` of its own label and the other edges before its `as()` when the body repeats, the node reached, then the body conditions as `gql:filter` at a `Site::body` site, where a body variable reads `Binding::Label` of this iteration, not its list), group lists of body nodes and edges reversed by the walk (`GroupTables::reversed`), a trail check against a `?` singleton (`Lowering::conditional`) always `gql:trail` (absent: passes), no merge point of the lowering after or inside the loop; the D14 advice names the pattern read back from the loop body (`error.rs` `gql_group_pattern`); tests `gql_qpp_tests.rs` (brute-force matches on the cyclic fixture, twins, paths, `?`, subqueries, limits); path variables as `Binding::Path` (every element of a read path's pattern labelled, a quantified edge's inner vertices `as("#gv<id>")` in the `repeat()` body, assembled in text order whatever the walk order, anchored like a group in an `OPTIONAL MATCH` part; never `path()`); stage 3a: `INSERT` as one `gql:insert_node` per new node, then one `gql:insert_edge` per new edge (text order, `as()` only when read later, the first on the row before the clause, each later one on the element created just before), `FINISH` / a write ending as `discard()`, a unit `CALL` as `optional(__.<body>.discard())`, no merge point of the lowering in a writing query (W15; the optimizer's general `lazy_barrier` rule may still place its pass-through `lazy_barrier()` between adjacent vertex steps, as in a Gremlin write); tests `tests/all/gql_write_tests.rs` (twins vs `add_v` / `add_e` comparing the final graph, Halloween guards, `FOR`, `CALL`, `RETURN` after writes, the read-plan guard, the Rhai write lock and create time); tests `tests/all/gql_tests.rs` (differential vs Rhai twins, profile, errors), `gql_pipeline_tests.rs` (1b pipeline), `gql_aggregate_tests.rs` (aggregation), `gql_quantifier_tests.rs` (quantified edges, brute-force trails on a cyclic fixture), `gql_path_tests.rs` (path variables), `gql_function_tests.rs` (stage 1c functions: twins, hand-computed Neo4j rows, limits), `gql_for_tests.rs` (stage 2a `FOR` / `UNWIND`, the unit row and `gql:lookup`), `gql_exists_tests.rs` (stage 2b `EXISTS` / pattern predicates: twins vs `where(__..)` / `not(__..)`, plans, `NULL` imports, trail; the hook: every host, three-valued logic, laziness and limits, profile children and walkers), `gql_expression_tests.rs` (stage 2c map projections, list and pattern comprehensions: twins vs `project()` / `value_map()`, `unfold().is(..).fold()` and `map(__.out(..)..fold())`, `NULL` targets, lists and imports, key order and E12, locals, bulk, limits) |

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
  A resource limit a `gql:*` step reaches gets GQL advice too (`gql_traverser_limit_help`,
  `gql_materialized_limit_help`; inside a subquery child the advice names the construct,
  `GqlSubqueryPlace`: an EXISTS body / pattern predicate, a pattern comprehension (failing
  level ends in `gql:value`), a `COLLECT { }` (ends in `gql:value` after its `RETURN` step), a
  `COUNT { }` (ends in `count()`, also fused as `...count()`), a `CALL { }` body (the child of
  `flat_map()`); the innermost such level decides, `union()` operands, `optional()` and
  `repeat()` bodies are transparent); every `gql:*` step calls `row.finish()` before it maps an
  evaluator error, so a refused `Env::reserve` surfaces as the real limit error, not 54001.
  Data errors carry targeted advice by GQLSTATUS (`gql_data_status_advice`) or by
  `GqlHelpTopic` (stage 2d: `Composite` for `UNION` / `NEXT`, `Call` for `CALL { }`, also on
  the refused body without `RETURN` and `IN TRANSACTIONS`, and on an outer name read by a first
  `WITH` of a scope-less `CALL` body that is no importing `WITH`, `bind::not_importing_with`;
  `CountCollect` for a name bound inside `COUNT { }` / `COLLECT { }` read outside it). The expected tokens name `CALL`
  among the clauses and `UNION` / `NEXT` after a result statement. Tests:
  `error_diagnostics_tests.rs` (`mod gql`), robustness
  `random_gql_strings_never_panic`, `random_gql_1b_queries_never_panic`,
  `random_gql_function_calls_never_panic`, the `random_gql_2a/2b/2c_*` families (2c: three
  rewrite pairs per query: `size()` of a comprehension vs `OPTIONAL MATCH` + `count()`, `FOR`
  over it vs a later `MATCH`, a map projection vs the record constructor; 2d list functions vs
  their comprehension rewrites; 2d composite: `CALL` vs a later `MATCH`, counting `CALL` vs
  `COUNT { }` vs `size()`, `COLLECT { }` vs the comprehension, `UNION` vs `OR` + `DISTINCT`,
  `UNION ALL` vs `FOR`, `NEXT` vs `FILTER`, a `UNION` body vs a `DISTINCT` body, whole-query vs
  `MATCH`-only `EXISTS`; 2e quantified path patterns: no quantifier vs the plain edge, a
  one-edge body vs the quantified edge, `?` vs `{0,1}`, the body written from the other end,
  `{2}` vs two written edges, the nested forms (`EXISTS`, pattern predicate, `COUNT { }` /
  comprehension, `CALL`, `OPTIONAL MATCH`) vs the quantified edge, each with and without
  property indexes).
- **Write clauses.** `INSERT` / `CREATE` since stage 3a; `SET` / `REMOVE` parsed and bound
  since 3c.T1 (`Stage::Set` / `Stage::Remove`, `BoundWrite::Set` / `Remove`, targets with their
  static kind `bind::TargetKind`), lowered since 3c.T3 to one `gql:set` / `gql:remove` per clause
  (`Instr::Set` / `Remove`; the row and the current element stay, targets bound at emission);
  `[DETACH | NODETACH] DELETE` parsed and bound since 3d.T1 (`Stage::Delete`,
  `BoundWrite::Delete`, expression targets with their static kind `bind::DeleteKind`; E1 label
  predicates refused by the parser, E2 known non-elements by the binder), lowered since 3d.T3
  to one `gql:delete` per clause (`Instr::Delete`, after the W10 materialization; the rows pass
  on); the `RETURN` ending a query that holds a `DELETE` uses the returning builders
  (`gql_project_returning` / `gql_aggregate_returning`, W28), never a `WITH` or a body's;
  `MERGE` parsed since 3e.T1 (`Stage::Merge(Box<MergeClause>)`: one path pattern in the merge
  form, `check_insert_node` / `check_insert_edge` with `word` `MERGE`: path variable and
  undirected edges allowed; `MergeAction` per `ON CREATE SET` / `ON MATCH SET`, items parsed by
  `set_item`; `GqlHelpTopic::Merge`), bound since 3e.T2 (`BoundWrite::Merge(Box<BoundMerge>)`:
  create template `create: BoundInsert` bound first, then the match child as a synthesized
  `CALL (*)` body `MATCH <pattern> RETURN <outputs>` (`merge_body`, anonymous elements `#m<k>`),
  outputs = record column bindings (`ColumnRef` on the MERGE segment), `on_create` / `on_match`
  via `set_items`); planned and lowered since 3e.T3 (`plan::WriteStage::merge`: the match
  child planned like a `CALL` body by `Planner::merge`, position pass `merge_positions`, row
  label `#m<k>`, every pattern element labelled under a path variable; `lower::Instr::Upsert`
  built by `Lowering::merge`: the match child lowered like a `CALL` body ending in its
  `gql:project`, the create child the 3a insert chain (`insert_chain`) at `Site::creating`
  (record columns read through their own labels, the path from `Lowered::created_paths`)
  ending in a `gql:project` of the same columns, the checks, the `ON` items via
  `Lowering::set_items` on the record; tests `lower/merge_tests.rs`, planner tests, end to end
  in `gql_write_tests.rs`).
- **Writes (stage 3a).** `INSERT` / `CREATE` lower to the internal write steps; a writing query
  is mutating for `is_mutating()` (`script/mutating.rs` lowers a leading `gql()` on an empty
  scratch graph), so the Rhai terminals take the write lock, and `gql_start` pins the
  evaluation's `now()` on every creating step. Visibility rests on the step-at-a-time executor
  (each step consumes its whole input first, design W13): a streaming executor must insert an
  eager boundary before every GQL write step (guards in `gql_write_tests.rs`). Errors of an
  INSERT pattern (W5-W7, a write inside a subquery expression, a NULL endpoint) carry
  `GqlHelpTopic::Insert`; a schema violation raised by a `gql:insert_*` step gets GQL advice
  (`error.rs` `gql_schema_help`, by the innermost failed step). Fuzz:
  `robustness_input_tests.rs` `random_gql_3a_*` (twins vs `add_v` / `add_e`, a failing last row
  leaves nothing); storage differential: `GQL_CURATED_3A`.
- **Lower into the ordinary pipeline.** No separate executor: the lowering builds plans with the
  ordinary builders and lets the optimizer do pushdown. Never push a `<>` comparison down
  (three-valued logic differs from `has()`).
- **The planner reads only through `GraphStorage`** (statistics, index catalog), never
  `TraversalGraph` internals, so it works on a host's own storage; its plan is deterministic.
- **Internal steps are `gql:*` and `pub(crate)`**: one file per step in its category, with arms in
  `for_each_access`, `path_requirement()` where they read the path, and `gql:` in the profile.
  Built: `gql:filter` (Filter, `filter_steps/step_gql_filter.rs`), `gql:project` (Map,
  `map_steps/step_gql_project.rs`, in `MAP_PRODUCING_KINDS`), `gql:value` (Map,
  `map_steps/step_gql_value.rs`, in `MAP_PRODUCING_KINDS`: the `|` part of a pattern
  comprehension, the last step of its child, stage 2c), `gql:order` (FlatBarrier,
  `flatbarrier_steps/step_gql_order.rs`), `gql:not_self_loop` (Filter, the incoming branch of an
  either-direction edge, `filter_steps/step_gql_not_self_loop.rs`), the `gql:error` marker
  (Filter, `plan_check`, `filter_steps/step_gql_error.rs`); stage 1b: `gql:aggregate`
  (FlatBarrier, `flatbarrier_steps/step_gql_aggregate.rs`, in `MAP_PRODUCING_KINDS`, generated
  traversers, `resets_path`), `gql:distinct` (FlatBarrier, GQL equality via
  `aggregate::GroupKey`), `gql:bind` (FlatMap, continues a pattern from a `Label` / `RowColumn`
  source; stage 3f: with a `GqlBindCheck` for a `LET` / `WITH` value written as a node or edge,
  `bind::Binding::checked` set by `Binder::materialize_values`, a value of another kind is 22G03), `gql:trail` (Filter, the trail check of a quantified edge); stage 2a: `gql:for`
  (FlatMap, `flatmap_steps/step_gql_for.rs`, in `MAP_PRODUCING_KINDS`: one derived traverser
  per list item, the item itself or the record `{x, i}` with an ordinal; `NULL` no row, a
  non-list one row, E1), `gql:lookup` (FlatMap, `flatmap_steps/step_gql_lookup.rs`: a component that
  is not the root scan started per row at the vertices one equality finds, `element_id(n) = e`
  by id or `n.k = e` through the property index of every scan label, each checked with GQL `=`,
  an unservable value or a storage serving no index scans the scan labels; planned only where an
  id or index serves it, E5; the one internal step that asks a permission: `Read` on vertices,
  like `v()`); stage 3a: `gql:insert_node` / `gql:insert_edge` (SideEffectMut,
  `sideeffect_steps/step_gql_insert_node.rs` / `step_gql_insert_edge.rs`: one element per row
  and per unit of bulk, emitted with bulk 1; values evaluated per row, `NULL` skipped, a graph
  element refused (22G03); one `add_vertex_with` / `add_edge_with` call per element, so schema
  defaults and `required` see every property; `set_create_time`; endpoints through `Binding`,
  `NULL` 22004, removed `ElementRemoved`; `Create` exactly like `add_v(labels)` / `add_e(label)`
  and `access_flow_after` states the created element; tests `runtime/insert_step_tests.rs`);
  stage 3c: `gql:set` / `gql:remove` (SideEffectMut, `sideeffect_steps/step_gql_set.rs` /
  `step_gql_remove.rs`, the shared target and write code in `step_gql_set.rs`: items left to
  right per row and per unit of bulk, rows emitted unchanged with bulk 1; a `NULL` value or
  entry removes through `drop_*_property`, never a stored null; `=` evaluates and checks the
  whole map before its first write; a `NULL` target is a no-op, a removed one
  `ElementRemoved`, a non-element or an edge label item 22G03, a `NULL` map 22004; access from
  the step's own `GqlWriteTarget` facts through `gql_updates()` / `gql_relabels()`, never the
  flow; tests `runtime/set_step_tests.rs`);
  stage 3d: `gql:delete` (FlatBarrier, `flatbarrier_steps/step_gql_delete.rs`, `GqlDeleteTarget`:
  collects the targets of every row first (elements, path elements, list items through an
  explicit stack; `NULL` and removed ones skipped, each element once, a row of bulk n once),
  the NODETACH check per clause before any removal (G1001, `status::EDGES_STILL_EXIST`), edges
  then vertices through `drop_edge` / `drop_vertex`, rows emitted unchanged; a non-element
  22G03; `Delete` from the step's own facts through `gql_deletes()`, flow passed on; every
  removed edge's label in `TraverserContext::gql_deleted` (E3: `Env::edge_type`, `type(r)` of
  a deleted edge); tests `runtime/delete_step_tests.rs`). Reads of a removed element through
  `runtime::Row` (`element_id`, properties, labels, `%`, `keys` / `properties`, endpoints, sort
  keys) keep the storage's `ElementRemoved` for `row.finish()` (W27, checked only when a read
  came back empty); a `RETURN` built with `gql_project_returning` / `gql_aggregate_returning`
  checks its columns for removed elements once a `gql:delete` removed something
  (`runtime::check_returned`, W28); the GQL help of `ElementRemoved` is chosen by the innermost
  failed `gql:` step (`error.rs` `gql_element_removed_help`).
  Stage 3e: `gql:merge` (SideEffectMut, `sideeffect_steps/step_gql_merge.rs`, one argument
  `GqlMerge`: `matcher` / `creator` children both ending in the record's `gql:project`, E1
  `checks` on the incoming row (`check_bindings`), `on_create` / `on_match` `GqlSetItem`s on the
  record (`item_bindings`, one subquery index space `item_children`), `creates`): per row and
  per unit of bulk, `tick()`, E1 (a `NULL` property value 22004), the matcher on the row
  (W34: after the earlier rows' writes), every match a record with `ON MATCH`, else the creator
  once with `ON CREATE` (the `gql:set` code, `StepGqlSet::apply_items`); records derived from
  the row (a new path position, children's positions do not continue it), bulk 1;
  `set_create_time` stamps the creator; `Create` per `creates` + the items' `Update`s (explicit
  arm, no `Update` without `ON` items), the children's requests from the walk, flow after it
  `AccessFlow::default()`; in `MAP_PRODUCING_KINDS`; profile `gql:merge("<pattern>",
  __.<matcher>, __.<creator>[, "ON CREATE SET .."][, "ON MATCH SET .."])`; tests
  `runtime/merge_step_tests.rs`.
  The others ask no permission of
  their own (value-only, like `has()`); `gql:filter`, `gql:project`, `gql:value`, `gql:order`,
  `gql:aggregate` and `gql:for` may hold subquery children (`runtime::SubqueryHost`), which every
  walker reaches through `child_traversals_mut` (the authorization walk included: their reads
  are asked like a Gremlin child's); their rendering puts GQL text in string literals so
  camelCase respelling leaves it alone. Trail checks between single edges use
  `where(P.neq("e1"))`, joins `where(P.eq("x"))`. A step reading EVERY occurrence of a label
  (`gql:trail`, a `Binding::Group` / `Binding::Path` group part) declares it in
  `history_labels()`: the path analysis then never retracts it at the `as()` that sets it again,
  so earlier occurrences survive in labels-only mode (a plain `select()` reads only the latest).
- **Edge forms** (table in `lower.rs`): `out()`/`in()` when nothing runs on the edge, else
  `out_e()..in_v()`; either direction is `union(out.., in_e().gql:not_self_loop()..)`, never
  `both_e().other_v()` (full paths) or plain `both()` (a self-loop twice).
- **Merge points:** the lowering places its own `lazy_barrier()` between hops; never change
  `LazyBarrierRule` or another global rule for GQL (it would move existing Gremlin plans).
