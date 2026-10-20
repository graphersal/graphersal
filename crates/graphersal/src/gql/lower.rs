//! The lowering: a planned GQL query ([`LogicalPlan`] of a [`BoundQuery`]) to ordinary
//! traversal steps, plus the Rust entry points [`GraphTraversalSource::gql`] and
//! [`GraphTraversalSource::gql_with`].
//!
//! The steps are built with the ordinary builders (`v()`, `has_label()`, `has()`, `as()`,
//! `optional()`, `skip()`, `limit()`) and the internal `gql:*` builders, so the plan goes through
//! the optimizer, the path analysis, the profile, the limits and the permission check like a
//! hand-written traversal (design D5, architecture e). Every fallible part (parsing, binding,
//! compiling expressions, checking parameter values) runs first; when one fails, the source gets
//! the `gql:error` marker instead, which raises the error before the traversal runs (D13).
//!
//! # Forms per construct (Phase 0 "Lowering forms fixed")
//!
//! | Construct | Steps |
//! |---|---|
//! | node scan `(p:A {k: v})` | `v()`, then `has_label(A)` (the scan labels first, so `SourceFilterPushdown` folds them into `v(labels: ..)`), the other label factors, the pushed conjuncts as `has(k, P..)`, the residual ones as `gql:filter`, then `as("p")` only when a label is needed |
//! | label factor `A`, `A\|B` | `has_label("A")`, `has_label("A", "B")`; `:A&B` is one `has_label` per factor |
//! | label factor with `!`, `%`, or `&` under `\|` | `gql:filter("A\|!B")` (`IS LABELED` on the current element) |
//! | pushed conjunct (`plan::classify`) | `has(k, P.eq(v))` (never the bare value), `P.lt` / `P.lte` / `P.gt` / `P.gte`, `P.within([..])` (`NULL`s dropped), `TextP.starting_with` / `ending_with` / `containing`, `IS NOT NULL` as `has(k, P.neq(()))` |
//! | `$param` operand | pushed only when its value fits the form (a non-null string, number or boolean; for `IN` a list of them, `NULL`s dropped; a string for the text forms), else `gql:filter` |
//! | other conjuncts | `gql:filter(expr)`, after the element it reads is bound |
//! | `-[e:T]->`, `<-[e:T]-` with nothing on the edge | `out("T")` / `in("T")` (also `-->` / `<--`, no labels), so `BarrierPushdown` and `lazy_barrier` apply |
//! | `-[e:T {k: v} WHERE ..]->`, an edge read later, joined or trail-checked | `out_e("T")` (`in_e` for `<-`), then on the edge: the other label factors (`has_label`), the pushed conjuncts (`has(k, P..)`), the join / trail checks, `gql:filter` for label factors with `!` / `%` and residual conjuncts, `as("e")` only when a label is needed; then `in_v()` (`out_v()`) |
//! | edge label `T`, `A\|B` | the labels of the adjacency step (the first `&` factor that is a label disjunction); other factors as for nodes, on the edge traverser |
//! | either direction `-[e]-`, `--`, `-[:T]-` | `union(__.out_e(..)<edge steps>.in_v(), __.in_e(..).gql:not_self_loop()<edge steps>.out_v())`, `as("e")` in both branches; without edge steps `union(__.out(..), __.in_e(..).gql:not_self_loop().out_v())`. Never `both_e().other_v()` (it forces full paths) nor plain `both()` (a self-loop would match twice; GQL binds it once) |
//! | target node `(b:L {k: v})` | after the hop: `has_label(L)`, the pushed `has()`, `gql:filter`, then `as("b")` only when a label is needed |
//! | join: a node bound earlier (a cycle, a variable repeated across patterns) | `where(P.eq("x"))` right after the hop reaching it (Phase 0 Q3) |
//! | an edge variable repeated in a later `MATCH` | `where(P.eq("e"))` on the edge traverser, no second `as("e")` |
//! | branch: the next hop starts at an earlier binding | `select("x")`, then the hop |
//! | trail (per `MATCH` clause) | `where(P.neq("e1"))` on the edge traverser for every earlier edge of the clause whose label set may overlap (anonymous edges get `#<id>` labels); skipped for disjoint label sets (Phase 0 Q8) |
//! | merge point | `lazy_barrier()` right after a hop that yields vertices when the next hop continues from them (before the target's filters and `as()`); merges only traversers with equal path labels, so it is invisible (see below) |
//! | several patterns / components | another `v()` per component (a Cartesian product, not optimized yet) |
//! | `RETURN` / `RETURN *` | `gql:project(columns)` (written order, `NULL` columns kept) |
//! | `DISTINCT` | `gql:distinct()` after `gql:project` (GQL equality, decision C3: `1` and `1.0` are one value, `NULL`s are equal) |
//! | `ORDER BY` | `gql:order(keys)` after `gql:project` (and `gql:distinct()`): a returned column is read from the row, any other variable through its path label; an alias is read only where the key names it (a substituted `LET` / `WITH` variable of the same name stays the variable). After `gql:aggregate` (a fresh path, no labels before it) a variable a column holds (`WITH p, count(*) AS c ORDER BY p.name`) is read from that column (`Site::ordered`), and a subquery body in a key reaches it through the row label (`as("#w<k>")`, `as("#r")` for `RETURN`, set before `gql:order` only when read) |
//! | `OFFSET n` / `LIMIT m` | `skip(n)` / `limit(m)`; a `$param` value must be a non-negative integer (checked before the run) |
//! | `LET x = expr`, a `WITH` that only projects | no step: the binder substitutes the definition, so the query lowers like the same query written without it (pushdown included) |
//! | `FILTER cond`, the `WHERE` of a projecting `WITH` | conjuncts like `WHERE`: `has()` / `gql:filter` where their bindings are bound |
//! | materializing `WITH` (aggregate, `DISTINCT`, `ORDER BY`, `OFFSET`, `LIMIT`; decision C2) | its result pipeline as for `RETURN` (`gql:project`, `gql:distinct()`, `gql:order`, `skip`, `limit`), then its `WHERE` as `gql:filter` on the row map (after `LIMIT`), then `as("#w<k>")` only when a later step reads the row after leaving it |
//! | a later `MATCH` continuing from a `WITH` column | `gql:bind("a")` (moves to the element the column holds; `NULL` or not an element: no row); a join, edge join or trail check against a column is `gql:filter` comparing the current element with the column |
//! | `OPTIONAL MATCH` | `optional(__.<part>)`, the part's own `WHERE` inside; a row it does not match continues unchanged and its variables read `NULL` (absent labels); after it every read goes through a label (no `Binding::Current`) |
//! | a leading `OPTIONAL MATCH` | `inject(0).optional(__.v()...)`: one unit row (decision C13) |
//! | a query that starts with `FOR`, `LET`, `WITH`, `FILTER` or `RETURN` (design M1, E4) | `inject(0)` first: one unit row that holds no binding (`RETURN 1 AS x` is `inject(0).gql:project("x")`); a leading `LET` / projecting `WITH` followed by `MATCH` needs none (substituted, the `MATCH` starts the query) |
//! | `FOR x IN list` (Cypher `UNWIND list AS x`; design mechanism 2) | `gql:for("x IN list")`: one derived traverser per item, the item itself the current object (`Binding::Current` on its row, `as("x")` only when read after a later step left it, read as that label); `NULL` list no row, a non-list one row (E1) |
//! | `FOR x IN list WITH ORDINALITY i` / `WITH OFFSET i` | `gql:for(".. WITH ORDINALITY i")`: the record `{x, i}`, read like a materializing `WITH` row (`Binding::Column`, `Binding::RowColumn` through `as("#f<k>")` only when read after leaving it) |
//! | a later `MATCH` continuing from a `FOR` item (design E3) | `gql:bind("x")` (an item that is not an element of the pattern's kind, `NULL` included: no row; never a bare `out()` on the item); a join or trail check against it is `gql:filter` as for a `WITH` column |
//! | a component that is not the root scan, with an equality an id or index serves (design M3, E5: `FOR id IN $ids MATCH (n) WHERE element_id(n) = id`, `MATCH (n:person {name: x})` with an index on `person.name`, a second component `(b:person) WHERE b.name = a.boss`) | `gql:lookup("(n:person)", "name: x") [index: vertex person.name]` (`[id]` for `element_id`) in place of the mid-traversal `v()`: per row the other side of the equality is evaluated where the row stands (no label for it), then the vertex of that id, or the index slice of every scan label, each kept when GQL `=` holds (`1 = 1.0`); a value no index serves, or a storage answering no index, scans the scan labels with the same check. Then `has_label(..)` of the scan labels, the node's other conjuncts and `as()` as after a scan; the equality is not repeated as `gql:filter` |
//! | quantified edge `-[e:T]->{m,n}` (also `{m,}`, `{,n}`, `{n}`, `*`, `+`, Cypher `[e*m..n]`; plan mechanism 5) | `repeat(B)` with the bounds as modulators: `{m,n}` `repeat(B).emit(__.loops().is(P.gte(m))).times(n)` (`emit()` for `m = 1`), `{n}` `repeat(B).times(n)`, `{0,n}` `emit().repeat(B).times(n)` (the zero-length match first), no upper bound: no `times()` (trail ends the loop; `repeat.max_loops` and `traversal.max_traversers` are errors, never a cut result, design D14); an upper bound beyond `u32::MAX` is no bound; `{0}` emits no step (the far node is the near one). The body `B` is the edge hop above (`out_e(T)`, the pushed `has()`, the inline `WHERE` as `gql:filter`, either direction as the `union`) with `gql:trail([e, ..])` before `as("e")`: the edge differs from every earlier iteration and every earlier edge of the clause |
//! | top-level `[NOT] EXISTS { .. }` / `EXISTS ( .. )` conjunct, a Cypher pattern predicate `(a)-[:T]->()` (design M2, the 2b fast path) | `filter(__.<body>)` / `not(__.<body>)` where the planner placed the conjunct, after the element's pushed `has()`, `gql:filter` residuals and `as()` (E11); the body is the subquery's plan for the host's current element lowered onto the child from the host's site: a start at the current element needs no step (`MATCH (p:person) WHERE NOT (p)-[:created]->()` is `v(labels: ["person"]).not(__.out("created"))`, no label), a start at another import `select("a")` / `gql:bind`, a join to an import `where(P.eq("a"))`, an unanchored first component a child-leading `v()`, or `gql:lookup` when the id or an index serves an equality of it reading an import (stage 2d.T5c, as in a `CALL` body); no merge point of the lowering inside a child; an absent (`NULL`) import matches nothing (E9). At most `MAX_LOWERED_SUBQUERIES` bodies per query (a `LET` holding one is copied into every reader) |
//! | `EXISTS` / a pattern predicate anywhere else in an expression: under `OR` / `XOR` / `CASE`, in a `RETURN` / `WITH` column, an `ORDER BY` key, an aggregate's argument, a `FOR` list (design M2, the 2b hook) | no step of its own: the evaluator node `Subquery(i)` reads `Env::subquery(i)`, answered by the `gql:*` step evaluating the expression (`gql:filter`, `gql:project`, `gql:order`, `gql:aggregate`, `gql:for`), which holds the body as its child `i` (`runtime::SubqueryHost`; the profile lists it under the step as `\>` lines). A `gql:filter` hosting one on an element runs after the element's `as()` and its plain residuals (E11), so a body reading the element through its label finds it. The body is lowered from the step's site as for the fast path (an `ORDER BY` key from the row map: imports through `select()`; after an aggregation an import a column holds through `gql:bind` from the row, `Site::ordered`). A row runs a child only when its evaluation reaches it (`a OR EXISTS {..}` runs nothing when `a` is TRUE), at most once per row, on one unit of the row (short-circuiting at the first match); `EXISTS` is TRUE or FALSE, never `NULL`, and composes three-valued like any boolean |
//! | pattern comprehension `[q = (a)-->(b) WHERE cond \| expr]` (stage 2c, design M2, E13) | a hosted subquery like `EXISTS` anywhere in an expression (same hosts, same site rules, `MAX_LOWERED_SUBQUERIES`), whose body ends with `gql:value(expr)` (the `|` part evaluated where the body ends: `Instr::Value`, hosting its own nested subqueries); the host runs the child to its end on one unit of the row and collects the values in arrival order, bulk expanded, checked against `bulk.max_expansion`, `traversal.max_materialized_bytes` and the memory budget before the list is built (`runtime::SubqueryHost`); an absent (`NULL`) import gives `[]`. `MATCH (p:person) RETURN [(p)-[:knows]->(f) \| f.name] AS l` is `v(labels: ["person"]).gql:project("l")` with the child `out("knows").gql:value("f.name")` |
//! | `COUNT { body }` (stage 2d, user decision D23, openCypher) anywhere an expression stands | a hosted subquery like a pattern comprehension (same hosts, site rules and bounds) whose child ends with `count()`: the host runs it on one unit of the row and answers its one result, the number of the body's matches (rows of a whole-query body), bulk counted, `0` for none or an absent import, never `NULL`; the optimizer's count fusions apply (`MATCH (p:person) RETURN COUNT { (p)-[:created]->() } AS c` is `v(labels: ["person"]).gql:project("c")` with the child `out("created").count()`; an uncorrelated `COUNT { MATCH (s:software) }` the child `v(labels: ["software"]).count()`) |
//! | `COLLECT { body RETURN x }` (stage 2d, D23) | a hosted subquery whose whole-query body ends with `gql:value("x")` reading its one returned column (`Binding::Column(0)` of the `RETURN` record): collected like a pattern comprehension (order of the body, its `ORDER BY` kept; `NULL` values kept; the list's limits); `[]` for an absent import |
//! | whole-query subquery body: `EXISTS { .. RETURN .. }`, `COUNT { }` / `COLLECT { }` with clauses other than `MATCH` (stage 2d) | the body (stages, `UNION`, `NEXT`, `CALL`) lowered onto the subquery's child from the host's site like a `CALL` body (`Lowering::composite`, `Start::At(site)`: no unit row, an import read where it is), without the merge points of its match pipeline; a top-level `[NOT] EXISTS` conjunct keeps the fast path (`filter(__.<body>)` / `not(__.<body>)`), everywhere else the 2b hook. Its aggregation, `DISTINCT`, `ORDER BY` / `LIMIT` run per row (an aggregating body gives a row even for no match: `EXISTS` of it is TRUE, `COUNT` 1) |
//! | trail of a single edge against a group variable | `gql:trail(["e"])` on the edge traverser (every iteration); single edges among themselves keep `where(P.neq(..))` |
//! | path variable `p = (a)-[e]->(b)` (plan mechanism 6, decision C11) | no step of its own: every node and edge of the pattern of a read path variable gets `as()` (a `WITH` column node after its `gql:bind` or join), a quantified edge also `as("#gv<id>")` on the vertex each iteration reaches (after `in_v()` / `out_v()` in the `repeat()` body); the slot reads `Binding::Path`, which assembles the path value in text order from those labels (a group reversed when walked from its far end), whichever node the planner starts at. Never `path()` (it needs full paths on every step). In an `OPTIONAL MATCH` part the path is anchored like a group variable, so a part not matched reads `NULL` |
//! | `q1 UNION [DISTINCT] q2`, `q1 UNION ALL q2` (stage 2d, design E15) | `union(__.<q1>, __.<q2>)`, each operand lowered onto its child like a query of its own (its own root scan, so its leading `v()` keeps the source pushdown and the property index; a unit operand `__.inject(0)`; its own `ORDER BY` / `OFFSET` / `LIMIT`; columns of a later operand projected in the first operand's order by the binder), the start step `g.union(..)` at the start of the query; then `gql:distinct()` for `UNION` / `UNION DISTINCT` (GQL equality, decision C3), nothing for `UNION ALL`. Each operand's `RETURN` row is a result pipeline of its own (`segments.len() + p`, row label `#r` for the first linear query, `#r<p>` for the others) |
//! | `q1 NEXT q2` (stage 2d, design E19) | after a linear query: the `WITH` it desugars to (the binder makes one pipeline). After a `UNION` block, or a `UNION` block after `NEXT`: each operand starts from the records ([`Stage::Rows`], read like the row of a materializing `WITH`: `Binding::Column` on the row, `RowColumn` through `as("#n<k>")` only when read after leaving it, `gql:bind` to continue a pattern from an element column); several operands are a mid-traversal `union()`, whose children each see every record (an aggregation in an operand reads the whole table) |
//! | `CALL (a) { body }`, `CALL (*) { .. }`, `CALL { .. }`, Cypher `CALL { WITH a .. }` (stage 2d, design mechanism 6, E16) | `flat_map(__.<body>)`: the body (a whole query: stages, `UNION`, `NEXT`) lowered onto the child from the site of the `CALL`, so it starts on the incoming row (no unit row): an import that is the current element needs no step (`Op::Resume` from it), another import is read where it is (`select("a")`, `gql:bind` from a `WITH` column, `Binding::Label` / `RowColumn` in expressions), an unanchored first component is a child-leading `v()` with its pushdown and index, or `gql:lookup` when its equality reads an import; the body's aggregation, `DISTINCT`, `ORDER BY`, `OFFSET` / `LIMIT` and `UNION` run per incoming row (`flat_map` runs the child once per traverser and multiplies its records by the bulk); an uncorrelated body runs per row too (E16). Its records continue the incoming path (outer labels stay readable); its `RETURN` row is the current object after it (`Binding::Column`), labelled `as("#c<k>")` only when a later step reads a column after leaving it; no `Binding::Current` of an outer binding after it. `MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f ORDER BY f.age DESC LIMIT 2 } RETURN p.name, f.name` is `v(labels: ["person"]).as("p").flat_map(__.out().as("f").gql:project("f").gql:order("f.age DESC").limit(2)).gql:project(..)` |
//! | `OPTIONAL CALL (a) { body }` | `optional(__.flat_map(__.<body>).as("#c<k>"))`: the row label inside, so a row the body gives no record continues unchanged and reads the returned names `NULL` (absent label); every later read of a returned name goes through `#c<k>` |
//! | parenthesized path pattern `(a) ((x)-[e:T]->(y)-[f]->(z) WHERE cond){m,n} (b)` (stage 2e, design item 9 and M5; also `*`, `+`, `?`, no quantifier) | `repeat(B)` with the modulators of a quantified edge (`?` is `{0,1}`: `emit().repeat(B).times(1)`; `{0}` no step, `b` is `a`; without a quantifier `B` inline, once). `B` is one iteration in walk order from the current element, which is the node the iteration before ended on (the merge rule: `a` for the first): `x`'s label factors, filters and `as("x")` (when read), then per body edge its 1a hop with `gql:trail([e, f, ..])` before `as("e")` (its own earlier iterations, every other body edge, the clause's other edges; without a repeating body a single edge is compared by `where(P.neq(..))`, a `?` edge always by `gql:trail`, which an absent edge passes), the node reached with its filters and `as()`, and the body conditions over several body elements as `gql:filter` right after the last of them, reading this iteration's elements (the latest occurrence of each label). Walked from `b` the body runs from its last node, hop by hop reversed. After the loop `b`'s filters and `as("b")`, or a join when `b` was bound earlier (`(a) ((x)-->(y))+ (a)`). No merge point of the lowering (every iteration labels its edges). A body variable read outside the body is a group list (`Binding::Group`, reversed when walked from `b`); with `?` a singleton, `NULL` after zero iterations; a path variable assembles the iterations from the body labels (`PathPart::Body`) |
//! | `INSERT pattern, ..` / Cypher `CREATE` (stage 3a, design W-d, W13) | per new node in text order `gql:insert_node(labels, "{k: expr, ..}")`, then per new edge in text order `gql:insert_edge(label, from, to, "{..}")`, each followed by its element's `as()` only when read later (D11): the first step reads the row before the clause, each later one runs on the element created just before (an edge's end inserted right before it is `Binding::Current`, another end a label, a `WITH` column or a `FOR` item as for any read); per row and per unit of bulk one element, emitted with bulk 1; the values' subqueries hosted by the step (`runtime::SubqueryHost`). `MATCH (p:person) INSERT (p)-[:R]->(:Q)` is `v(labels: ["person"]).as("p").gql:insert_node(["Q"]).gql:insert_edge("R", "p", "(#2)").discard()`. A leading `INSERT` starts from the unit row `inject(0)` (M1). After the clause the last element created is the current element; later clauses (`MATCH`, `RETURN`, ...) see the writes (the step-at-a-time executor: each step consumes its whole input before the next runs, so a clause never reads its own writes, W13) |
//! | `FINISH`, a query ending in a write clause (design W2) | `discard()` in place of the result pipeline: no row |
//! | `CALL { .. }` whose body ends in `FINISH` or a write (a unit subquery, design W3), also `OPTIONAL CALL` | `optional(__.<body>.discard())`: the body runs per row, the row passes on unchanged with its current element |
//! | a query that writes (design W15) | no merge point of the lowering (the executor never merges a mutating plan); the optimizer's general `lazy_barrier` rule still places its pass-through `lazy_barrier()` between adjacent vertex steps, as in a Gremlin write |
//! | group variable `e` read outside its element | `Binding::Group`: every occurrence of `as("e")` as a list in text order (reversed when walked from the far end), `[]` for a zero-length match; inside an `OPTIONAL MATCH` part anchored on the part's witness node, or on `as("#o<k>")` at the end of a part without a new node, so a part not matched reads `NULL` |
//!
//! # Path labels (design D11)
//!
//! A binding gets `as()` only when something reads it while it is not the current element: a
//! filter over several bindings, a `RETURN` column after a later scan moved on, an `ORDER BY`
//! key (which runs on the row map), or a reason of the plan (join, trail, branch). An
//! expression reading the current element reads it directly (`Binding::Current`), with no path
//! label at all, so `MATCH (p:person) RETURN p.name` records no path and keeps bulk merging.
//!
//! # Merge points (Phase 0 outcome 1)
//!
//! `LazyBarrierRule` never places a merge point across an `as()` or a filter, so the lowering
//! places its own `lazy_barrier()` between two hops (the global rule stays unchanged, so Gremlin
//! plans do not move). Merging is sound there: the merge key keeps traversers with different path
//! labels apart, the step passes everything through under full paths, a sack merge operator or
//! `bulk.one`, and it merges only where the next hop's fan-out pays for it. It pays off where the
//! middle node is not read and no edge before it is labelled; a trail check against an earlier
//! edge of the same `MATCH` (edges with overlapping label sets) labels that edge and keeps the
//! traversers apart.
//!
//! A materializing `WITH` column is read from its row: `Binding::Column` while the row is the
//! current object, `Binding::RowColumn` through the row label after a later step left it.
//!
//! # Not yet
//!
//! The phase 2 anchor ([`Op::Anchor`], a `gql()` continuing a traversal) fails with
//! [`GqlErrorKind::NotYet`]; stage 1a never plans one. The constructs still out of scope are
//! refused by the parser (shortest paths, `EXCEPT`, the E17 forms of a parenthesized path
//! pattern, ...) and the binder. Every construct the binder accepts lowers (stage 2d.T5c
//! lowered `COUNT { }`, `COLLECT { }` and whole-query subquery bodies, stage 2e.T4
//! parenthesized path patterns). A component that is not the
//! root scan and has no equality an id or index serves stays a mid-traversal `v()` plus filters
//! per row (design E5).
//!
//! # Aggregation
//!
//! An aggregating `RETURN` / `WITH` ([`ResultOp::Aggregate`]) is `gql:aggregate` in place of
//! `gql:project`: its columns are compiled by `aggregate::compile_aggregating` (a column without
//! an aggregate call is a grouping key), and `DISTINCT`, `ORDER BY`, `OFFSET` and `LIMIT` follow
//! it as after `gql:project`. Its output rows are generated traversers (fresh paths), so an
//! aggregating `WITH` row read later is labelled `#w<k>` like any materializing `WITH` row.

use std::sync::Arc;

use super::aggregate::{AggregateCall, Aggregating, compile_aggregating};
use super::ast::{
    BinaryOp, CallArgs, EdgeDirection, Expr, ExprKind, Identifier, LabelExpr, Literal, MapEntry,
    ProjectionItem, Quantifier, QueryEnd, SortDirection, SubqueryKind,
};
use super::bind::{
    BindingId, BindingKind, BoundComposite, BoundInsert, BoundLinear, BoundQuery, BoundStep,
    BoundWrite, Column, ColumnRef, Conjunct, Count, InsertProperty, Predicate, Segment, SortKey,
    bind_with_parameters,
};
use super::eval::{Scope, Value, compile};
use super::lexer::Span;
use super::parser::parse;
use super::plan::{
    CallStage, CompositePlan, ElementFilters, Expand, ForStage, GroupExpand, LogicalPlan, Lookup,
    LookupBy, Materialize, Op, PipelinePlan, PushOp, PushOperand, Pushdown, ResultOp, Stage,
    SubqueryUse, UnionPlan, WriteStage, and_factors, exists_conjunct, label_disjuncts,
};
use super::planner::{StorageFacts, plan};
use super::runtime::{Binding, GqlElement, MAX_HOSTED_SUBQUERIES, PathPart, RowExpr, eval_error};
use crate::error::{GqlErrorBuilder, GqlErrorKind, GqlHelpTopic, TraverserError};
use crate::graph::GraphStorage;
use crate::traversal::step::GraphStep;
use crate::traversal::step::flatbarrier_steps::step_barrier::lazy_barrier_step;
use crate::traversal::step::flatbarrier_steps::step_gql_aggregate::GqlAggregateColumn;
use crate::traversal::step::flatbarrier_steps::step_gql_order::GqlSortKey;
use crate::traversal::step::flatmap_steps::step_gql_for::GqlOrdinal;
use crate::traversal::step::flatmap_steps::step_gql_lookup::{GqlLookup, GqlLookupKey};
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::traversal::step::sideeffect_steps::step_gql_insert_edge::GqlEndpoint;
use crate::traversal::step::sideeffect_steps::step_gql_insert_node::GqlInsertProperty;
use crate::value::{Number, P, PValue};
use crate::{
    AnonymousTraversal, ArgStrings, CompactString, ElementKind, ElementProperty,
    GraphTraversalSource,
};

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Runs a GQL read query (ISO/IEC 39075, feature `gql`): `MATCH` and `OPTIONAL MATCH`
    /// patterns, `WHERE`, the linear pipeline `LET`, `FILTER`, `WITH` (Cypher) and `FOR x IN
    /// list [WITH ORDINALITY i]` (Cypher `UNWIND list AS x`), a query that starts without
    /// `MATCH` (`RETURN 1 + 2 AS x`, `FOR id IN $ids MATCH ..`), `RETURN`
    /// (mandatory; `RETURN *` returns every named variable), `ORDER BY`, `OFFSET` (Cypher `SKIP`)
    /// and `LIMIT`, and the aggregates `count`, `sum`, `avg`, `min`, `max` and `collect_list`
    /// (Cypher `collect`) in `RETURN` and `WITH`, grouped by the items without an aggregate
    /// (NULL skipped except by `count(*)`), and quantified edges (`-[e:knows]->{1,3}`, `*`, `+`,
    /// Cypher `[e:knows*1..3]`) with trail semantics (no edge twice in one match; the edge
    /// variable is the list of the edges), and path variables (`MATCH p = (a)-->(b) RETURN p`,
    /// `path_length(p)`, Cypher `length(p)`; a path is the Gremlin path value). Use
    /// [`Self::gql_with`] for a query with `$parameters`.
    ///
    /// Every result is one map per row: the keys are the column names (the `AS` alias, else the
    /// expression as written) in written order, a missing property is a `null` value (never a
    /// missing key), and a returned node or edge stays the live element, so Gremlin steps may
    /// follow (`select("p").out("knows")`). `WHERE` uses three-valued logic: a row passes only
    /// when the condition is TRUE (a missing property, a stored `null` and a comparison across
    /// types are NULL). Without `ORDER BY` the row order is unspecified. `ORDER BY` sorts `NULL`
    /// last ascending and first descending.
    ///
    /// The query is compiled into ordinary steps (`.profile()` shows them, with internal
    /// `gql:*` steps where Gremlin has no equivalent), so property indexes, the optimizer,
    /// limits and permissions apply as to a Gremlin traversal. A query that does not parse or
    /// bind fails before the traversal runs, with a [`TraverserError::Gql`] carrying the
    /// GQLSTATUS code and a caret under the offending text; building the traversal itself never
    /// fails. `gql()` starts a traversal: called after other steps, the traversal fails the
    /// same way.
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let rows = graph
    ///     .traversal()
    ///     .gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name ORDER BY name")
    ///     .select("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(rows, ["josh".into(), "peter".into()]);
    ///
    /// // An error fails the traversal before it runs, even on an empty graph.
    /// let empty = TraversalGraph::new();
    /// let error = empty.traversal().gql("MATCH (p) RETRUN p").to_list().unwrap_err();
    /// assert!(matches!(error.root_cause(), TraverserError::Gql { .. }));
    /// ```
    pub fn gql(self, query: &str) -> Self {
        self.gql_with(
            query,
            std::iter::empty::<(CompactString, ElementProperty)>(),
        )
    }

    /// Runs a GQL read query with parameter values: `$name` in the query reads the value given
    /// for `name` (a value, never query text). A parameter the query reads but `params` lacks
    /// fails the traversal before it runs; unused parameters are ignored. Otherwise as
    /// [`Self::gql`].
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let ages = graph
    ///     .traversal()
    ///     .gql_with(
    ///         "MATCH (p:person {name: $name}) RETURN p.age AS age",
    ///         [("name", ElementProperty::from("marko"))],
    ///     )
    ///     .select("age")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(ages, [ElementProperty::from(29)]);
    /// ```
    pub fn gql_with<I, K, V>(self, query: &str, params: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<CompactString>,
        V: Into<ElementProperty>,
    {
        let params: Vec<(CompactString, ElementProperty)> = params
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        if !self.steps.is_empty() {
            return self.gql_error(not_at_start(query));
        }
        let lowered = {
            let facts = StorageFacts::from_storage(&self.storage);
            prepare::<S>(query, &params, &facts)
        };
        match lowered {
            Ok(lowered) => lowered.emit(self),
            Err(error) => self.v(None).gql_error(error),
        }
    }
}

/// The error of `gql()` called after other steps (a start step in this version, design D4).
fn not_at_start(query: &str) -> TraverserError {
    GqlErrorBuilder::new(
        GqlErrorKind::NotYet,
        0..query.len(),
        "gql() starts a traversal in this version; it cannot follow other steps",
    )
    .alternative(start_with_gql(query))
    .into_error(query)
}

/// The advice of a `gql()` that does not start the traversal, built from its `query`: start
/// with it, express what the steps before it selected in the pattern, and continue with Gremlin
/// steps after it.
pub(crate) fn start_with_gql(query: &str) -> String {
    format!(
        "start the traversal with it, as in g.gql({}), and write what the steps before it \
         selected as the MATCH pattern and its WHERE condition; Gremlin steps may follow it, as \
         in g.gql(\"MATCH (p:person) RETURN p\").select(\"p\").out(\"created\")",
        crate::value::literal::rhai_string(query)
    )
}

/// `name` as the key of a Rhai object map literal: bare when it is an identifier, quoted
/// otherwise (a back-quoted GQL parameter name may hold any character).
pub(crate) fn rhai_map_key(name: &str) -> String {
    let identifier = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if identifier {
        name.to_string()
    } else {
        crate::value::literal::rhai_string(name)
    }
}

/// Where a step runs: the binding the traverser stands on (read as `Binding::Current`) and the
/// materializing `WITH` or `FOR` whose row is the current object (its columns read as
/// `Binding::Column`; the item of a `FOR` without an ordinal, which is the row itself, as
/// `Binding::Current`). Every other binding is read through its path label, a column of a row
/// left behind through the row's label (`Binding::RowColumn`; the item of a `FOR` without an
/// ordinal as `Binding::Label` of its row label).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Site {
    /// The binding the traverser stands on.
    current: Option<BindingId>,
    /// The segment of the materializing `WITH` or `FOR` whose row is the current object (the
    /// `RETURN` row: `segments.len()`, see [`Site::ordered`]).
    row: Option<usize>,
    /// The `OPTIONAL MATCH` segment whose part the step runs in: a group variable of that part
    /// reads its list there without the part's anchor (the part matched so far).
    part: Option<usize>,
    /// The aggregating result pipeline (a materializing `WITH` segment, or `segments.len()`
    /// for `RETURN`) whose `ORDER BY` the step serves, a key or a subquery body the key holds.
    /// `gql:aggregate` starts every output on a fresh path, so the labels of the bindings
    /// before it are gone: a binding one of its columns holds (`WITH p, count(*) AS c ORDER
    /// BY p.name`) is read from that column, `Binding::Column` on the row
    /// (`row == ordered`), `Binding::RowColumn` through the row label after leaving it.
    ordered: Option<usize>,
    /// Inside the body of a parenthesized path pattern (stage 2e): a body variable is one
    /// element there (this iteration's, `Binding::Label` of its latest occurrence), not the
    /// list it is outside the body.
    body: bool,
}

impl Site {
    /// On the element of binding `id`.
    fn on(id: BindingId) -> Self {
        Site {
            current: Some(id),
            row: None,
            part: None,
            ordered: None,
            body: false,
        }
    }

    /// On the element of body binding `id` inside the body of a parenthesized path pattern
    /// (see [`Site::body`]).
    fn in_body(id: BindingId) -> Self {
        Site {
            body: true,
            ..Site::on(id)
        }
    }

    /// On the row of the materializing `WITH` or `FOR` of segment `segment`.
    fn row(segment: usize) -> Self {
        Site {
            current: None,
            row: Some(segment),
            part: None,
            ordered: None,
            body: false,
        }
    }

    /// On the row of the aggregating result pipeline `result` (see [`Site::ordered`]): its
    /// `ORDER BY`.
    fn ordered(result: usize) -> Self {
        Site {
            current: None,
            row: Some(result),
            part: None,
            ordered: Some(result),
            body: false,
        }
    }

    /// Away from the row of `ORDER BY` of `site` (a step of a subquery body it holds, a row
    /// check): keeps only [`Site::ordered`].
    fn away(site: Site) -> Self {
        Site {
            ordered: site.ordered,
            ..Site::default()
        }
    }
}

/// One step of the lowered plan, every fallible part already done.
enum Instr<S: GraphStorage> {
    /// `v()`.
    Scan,
    /// `gql:lookup(..)` with its value read at `site` (an [`Op::Lookup`], design M3).
    Lookup { lookup: GqlLookup<S>, site: Site },
    /// `inject(0)`: the one unit row a query that does not start with `MATCH` starts from
    /// (decision C13, design M1).
    Unit,
    /// `has_label(labels)`.
    HasLabel(Vec<String>),
    /// `has(key, predicate)`.
    Has(String, P<PValue>),
    /// `gql:filter(condition)` at `site`, hosting the bodies of the subqueries the condition
    /// holds (the 2b hook: `EXISTS` under `OR`, `CASE`, ...).
    Filter {
        condition: RowExpr<S>,
        site: Site,
        subqueries: Bodies<S>,
    },
    /// `gql:filter(condition)` comparing the current element (the extra slot after the
    /// bindings) with a materializing `WITH` column binding: a join, an edge join or a trail
    /// check against an element the row holds, which has no path label of its own (or, at a
    /// site with [`Site::ordered`], a binding an aggregating projection's column holds).
    RowCheck(RowExpr<S>, Site),
    /// `as(label)` of the binding, emitted only when a label is needed.
    Label(BindingId),
    /// `as("#w<k>")` of the row of the materializing `WITH` of segment `k` (of a `FOR`:
    /// `as("#f<k>")` on its record, `as("x")` on the item itself without an ordinal), emitted
    /// only when a later step reads the row after leaving it (`Binding::RowColumn`).
    RowLabel(usize),
    /// `gql:for(variable IN list)` at `site` (`FOR` / `UNWIND`, design mechanism 2).
    For {
        list: RowExpr<S>,
        site: Site,
        variable: CompactString,
        ordinal: Option<GqlOrdinal>,
        subqueries: Bodies<S>,
    },
    /// `gql:project(columns)` at `site`.
    Project {
        columns: Vec<GqlColumn<S>>,
        site: Site,
        subqueries: Bodies<S>,
    },
    /// `gql:aggregate(keys, aggregates, columns)` at `site`: an aggregating `RETURN` / `WITH`.
    Aggregate {
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        site: Site,
        subqueries: Bodies<S>,
    },
    /// `gql:distinct()` (GQL equality, decision C3).
    Distinct,
    /// `gql:order(keys)`, on the row map of the `gql:project` / `gql:aggregate` before it
    /// (`columns` columns), at `site` (`Site::default()`, or [`Site::ordered`] after an
    /// aggregation).
    Order {
        keys: Vec<GqlSortKey<S>>,
        columns: usize,
        site: Site,
        subqueries: Bodies<S>,
    },
    /// `skip(n)`.
    Skip(usize),
    /// `limit(n)`.
    Limit(usize),
    /// One pattern edge walked from the current vertex ([`Op::Expand`]); see [`Hop`].
    Hop(Hop<S>),
    /// `repeat(__.<body>)` with the modulators of `quantifier` (as for a quantified edge,
    /// [`Lowered::looped`]): the body of a quantified parenthesized path pattern (stage 2e,
    /// [`Op::Group`]), one iteration from the node the iteration before ended on.
    Repeat {
        body: Vec<Instr<S>>,
        quantifier: Quantifier,
    },
    /// `select(label)`: back to a binding labelled earlier (a branch of the pattern, a later
    /// segment starting at a node bound before it).
    Select(BindingId),
    /// `gql:bind(name)`: moves to the element a materializing `WITH` column binding holds, read
    /// at `site` (a later segment starting at it, a branch back to it).
    Bind {
        id: BindingId,
        site: Site,
        name: CompactString,
        kind: ElementKind,
    },
    /// `where(P.eq(label))`: the current element is the one bound earlier to the binding (a
    /// join: a cycle closing, a node or edge variable repeated).
    Same(BindingId),
    /// `where(P.neq(label))`: the current edge differs from an earlier edge (trail).
    Differs(BindingId),
    /// `gql:trail(labels)`: the current edge differs from every occurrence of these bindings'
    /// labels (trail against a group variable, or the trail check in the body of a quantified
    /// edge, its own label included; plan mechanism 5).
    Trail(Vec<BindingId>),
    /// `as("#o<k>")` at the end of the part of `OPTIONAL MATCH` segment `k` that binds no new
    /// node: the anchor that tells a part not matched (its group variables `NULL`) from a
    /// zero-length match (`[]`); emitted only when a group variable of the part is read.
    PartLabel(usize),
    /// A merge point (`lazy_barrier()`) right after a hop whose next hop walks `direction`
    /// over `labels` (the step the merge point judges its fan-out with).
    Merge {
        direction: EdgeDirection,
        labels: Option<Vec<String>>,
    },
    /// `optional(__.<steps>)`: the part of an `OPTIONAL MATCH` (plan mechanism 4).
    Optional(Vec<Instr<S>>),
    /// `union(__.<operand>, ..)`: the operands of a `UNION` block (stage 2d, design E15), each
    /// lowered onto its child like a query of its own; the start step `g.union(..)` when the
    /// block starts the query.
    Union(Vec<Vec<Instr<S>>>),
    /// `flat_map(__.<body>)`: the body of a `CALL { }` (stage 2d, design mechanism 6), lowered
    /// onto the child from the site of the `CALL`; per incoming row its records, which continue
    /// the incoming path. An `OPTIONAL CALL` is this inside an [`Instr::Optional`].
    Call(Vec<Instr<S>>),
    /// `filter(__.<body>)` (`not(__.<body>)` when `negated`): a top-level `[NOT] EXISTS`
    /// conjunct or pattern predicate, the subquery body lowered onto the child from the host's
    /// site (design M2, the 2b fast path); the child short-circuits at its first match.
    Exists { negated: bool, body: Vec<Instr<S>> },
    /// `gql:value(expr)` at `site`: the last step of a pattern comprehension's body, its `|`
    /// part (stage 2c); the host collects the values into the list.
    Value {
        expr: RowExpr<S>,
        site: Site,
        subqueries: Bodies<S>,
    },
    /// `count()`: the last step of a `COUNT { }` body (stage 2d): the host answers the count of
    /// the body's matches or rows, bulk counted.
    Count,
    /// `gql:value(column)`: the last step of a `COLLECT { }` body (stage 2d), the one column of
    /// the body's `RETURN` record (the current object, `Binding::Column(0)`, `expr` its slot
    /// 0); the host collects the values into the list.
    Collect(RowExpr<S>),
    /// `gql:insert_node(labels, properties)` at `site` (stage 3a): one new node of an `INSERT`
    /// per row, its values read at `site`, hosting the subqueries they hold.
    InsertNode {
        labels: Vec<CompactString>,
        properties: Vec<GqlInsertProperty<S>>,
        site: Site,
        subqueries: Bodies<S>,
    },
    /// `gql:insert_edge(label, from, to, properties)` at `site` (stage 3a): one new edge of an
    /// `INSERT` per row, its ends and values read at `site`.
    InsertEdge {
        label: CompactString,
        from: InsertEnd,
        to: InsertEnd,
        properties: Vec<GqlInsertProperty<S>>,
        site: Site,
        subqueries: Bodies<S>,
    },
    /// `discard()`: the end of a pipeline ending in `FINISH` or a write clause (stage 3a, design
    /// W2): every row has done its work, none is returned.
    Discard,
}

/// One end of an edge an `INSERT` creates ([`Instr::InsertEdge`]): the node binding, its
/// variable as written (`None`: anonymous, shown by its path label) and the node pattern at
/// that end.
#[derive(Debug, Clone)]
struct InsertEnd {
    id: BindingId,
    name: Option<CompactString>,
    span: Span,
}

/// Where a linear query starts ([`Lowering::linear`]).
#[derive(Debug, Clone, Copy)]
enum Start {
    /// It starts the query: one unit row first unless it starts with `MATCH`.
    Query,
    /// It starts at this site: on the incoming row of a `CALL` body, or (`Site::default()`)
    /// with the [`Stage::Rows`] of the block before `NEXT`.
    At(Site),
}

/// The subquery bodies a `gql:*` step hosts (the 2b hook, `runtime::SubqueryHost`): body `i`
/// answers the step's subquery `i`, each lowered from the step's site.
type Bodies<S> = Vec<Vec<Instr<S>>>;

/// One walked pattern edge. The form is chosen at emission, once the labels are known: the
/// vertex step (`out(labels)`, `in(labels)`) when nothing runs on the edge traverser, else the
/// edge step, `steps` on the edge, then the vertex at the other end (`out_e(labels)..in_v()`,
/// `in_e(labels)..out_v()`). Either direction is a `union` of both, the incoming branch without
/// self-loops, so a self-loop matches once.
struct Hop<S: GraphStorage> {
    /// The direction as seen from the current vertex.
    direction: EdgeDirection,
    /// The labels of the adjacency step (`None`: any edge).
    labels: Option<Vec<String>>,
    /// The steps on the edge traverser: label factors, pushed `has()`, join and trail checks,
    /// `gql:filter`s, the edge's `as()` placeholder.
    steps: Vec<Instr<S>>,
    /// The quantifier of a quantified edge: the hop is the body of a `repeat()` (see
    /// [`Lowered::quantified`]).
    quantifier: Option<Quantifier>,
    /// The group variable of a quantified edge: the vertex each iteration reaches gets its
    /// inner label (`as("#gv<id>")`) when a path variable reads the group.
    inner: Option<BindingId>,
}

/// The lowered query, ready to be emitted onto a source.
struct Lowered<S: GraphStorage> {
    query: Arc<str>,
    instrs: Vec<Instr<S>>,
    /// The path label of each binding.
    labels: Vec<CompactString>,
    /// Whether each binding gets `as()`.
    labelled: Vec<bool>,
    /// The materializing `WITH` column each binding stands for (`None`: a pattern binding).
    columns: Vec<Option<ColumnRef>>,
    /// The row label of each segment (`#w<k>`; used only for a materializing `WITH`), then
    /// the one of the `RETURN` row (`#r`, read only by an aggregating `RETURN`'s `ORDER BY`).
    row_labels: Vec<CompactString>,
    /// Whether the row of each segment (then the `RETURN` row) gets its row label.
    rows_labelled: Vec<bool>,
    /// Per result pipeline (each segment, then `RETURN`): the binding each column holds, when
    /// the column is a bare variable (see [`Site::ordered`]); empty for the other segments.
    ordered: Vec<Vec<Option<BindingId>>>,
    /// Per segment: a `FOR` without an ordinal, whose row is the item itself (its item reads
    /// `Binding::Current` on the row, `Binding::Label` of the row label after it).
    items: Vec<bool>,
    /// Per binding: the `OPTIONAL MATCH` part and the anchor of a group variable (`None`: not a
    /// group variable).
    groups: Vec<Option<GroupRead>>,
    /// The part label of each `OPTIONAL MATCH` segment that needs one (`#o<k>`, see
    /// [`Instr::PartLabel`]); empty for the others.
    part_labels: Vec<CompactString>,
    /// Per binding: the inner label of a group variable a path variable reads (`#gv<id>`, set
    /// on the vertex each iteration reaches); empty for the others.
    inner_labels: Vec<CompactString>,
    /// Per binding: how a path variable is assembled (`None`: not a path variable).
    paths: Vec<Option<PathRead>>,
    /// The query writes (stage 3a): no merge point of the lowering (design W15; the executor
    /// never merges a mutating plan, so `lazy_barrier()` would be a no-op in the profile).
    writes: bool,
}

/// How a path variable is read (`Binding::Path`, plan mechanism 6).
#[derive(Debug, Clone)]
struct PathRead {
    /// The segment that binds it.
    segment: usize,
    /// The parts in text order: the labels of the pattern's nodes and edges, a quantified
    /// edge as its edges' label and its inner vertices' label.
    parts: Vec<PathPart>,
    /// The label the path holds whenever its `OPTIONAL MATCH` part matched (as for a group
    /// variable); `None` outside an `OPTIONAL MATCH`.
    anchor: Option<CompactString>,
}

/// What [`Lowering::groups`] finds: how each group variable is read, the part label of each
/// `OPTIONAL MATCH` segment that needs one, the anchor of each segment, and per binding
/// whether it was walked against the text direction (a quantified edge's edge, every variable
/// of a parenthesized path pattern's body).
struct GroupTables {
    groups: Vec<Option<GroupRead>>,
    part_labels: Vec<CompactString>,
    anchors: Vec<Option<CompactString>>,
    reversed: Vec<bool>,
}

/// How a group variable is read outside its element pattern (`Binding::Group`).
#[derive(Debug, Clone)]
struct GroupRead {
    /// The segment that binds it.
    segment: usize,
    /// The label the path holds whenever its `OPTIONAL MATCH` part matched (the part's witness
    /// node or its part label); `None` outside an `OPTIONAL MATCH`.
    anchor: Option<CompactString>,
    /// Walked from the node written after the edge ([`Expand::reversed`]): the list is the
    /// reverse of path order.
    reversed: bool,
}

impl<S: GraphStorage> Lowered<S> {
    /// Where variable `slot` reads from at `site` (see [`Site`]).
    fn binding(&self, slot: usize, site: Site) -> Binding {
        if let Some(column) = ordered_column(&self.ordered, site, slot) {
            return match site.ordered {
                Some(result) if site.row != Some(result) => Binding::RowColumn {
                    label: self.row_label(result),
                    column,
                },
                _ => Binding::Column(column),
            };
        }
        if let Some(Some(path)) = self.paths.get(slot) {
            // Inside its own part the part matched so far: no anchor to check.
            let anchor = path
                .anchor
                .as_ref()
                .filter(|_| site.part != Some(path.segment));
            let parts = anchor
                .map(|label| PathPart::Anchor(label.clone()))
                .into_iter()
                .chain(path.parts.iter().cloned())
                .collect();
            return Binding::Path(parts);
        }
        if site.current == Some(BindingId(slot)) {
            return Binding::Current;
        }
        // Inside the body of a parenthesized path pattern a body variable is this iteration's
        // element (a body condition reads only the body's variables).
        if site.body && matches!(self.groups.get(slot), Some(Some(_))) {
            return Binding::Label(self.labels.get(slot).cloned().unwrap_or_default());
        }
        if let Some(Some(group)) = self.groups.get(slot) {
            return Binding::Group {
                label: self.labels.get(slot).cloned().unwrap_or_default(),
                // Inside its own part the part matched so far: no anchor to check.
                anchor: if site.part == Some(group.segment) {
                    None
                } else {
                    group.anchor.clone()
                },
                reversed: group.reversed,
            };
        }
        match self.columns.get(slot).copied().flatten() {
            Some(column) if self.items.get(column.segment).copied().unwrap_or(false) => {
                if site.row == Some(column.segment) {
                    Binding::Current
                } else {
                    Binding::Label(self.row_label(column.segment))
                }
            }
            Some(column) if site.row == Some(column.segment) => Binding::Column(column.column),
            Some(column) => Binding::RowColumn {
                label: self.row_label(column.segment),
                column: column.column,
            },
            None => Binding::Label(self.labels.get(slot).cloned().unwrap_or_default()),
        }
    }

    /// Where each variable slot reads from at `site`.
    fn bindings(&self, site: Site) -> Arc<[Binding]> {
        (0..self.labels.len())
            .map(|slot| self.binding(slot, site))
            .collect()
    }

    /// The slots of `ORDER BY` at `site` on a new row map of `columns` columns: every binding
    /// read away from its row (a binding a column of an aggregating projection holds from that
    /// column, [`Site::ordered`]), then the row's columns.
    fn order_bindings(&self, columns: usize, site: Site) -> Arc<[Binding]> {
        (0..self.labels.len())
            .map(|slot| self.binding(slot, site))
            .chain((0..columns).map(Binding::Column))
            .collect()
    }

    /// The slots of a [`Instr::RowCheck`] at `site`: every binding read away from its row,
    /// then the current element.
    fn check_bindings(&self, site: Site) -> Arc<[Binding]> {
        (0..self.labels.len())
            .map(|slot| self.binding(slot, Site::away(site)))
            .chain(std::iter::once(Binding::Current))
            .collect()
    }

    /// The row label of segment `segment`.
    fn row_label(&self, segment: usize) -> CompactString {
        self.row_labels.get(segment).cloned().unwrap_or_default()
    }

    fn emit<'g>(mut self, source: GraphTraversalSource<'g, S>) -> GraphTraversalSource<'g, S>
    where
        S: 'g,
    {
        let instrs = std::mem::take(&mut self.instrs);
        self.emit_into(source, instrs)
    }

    /// Emits `instrs` onto `g` (the source, or the child of an `optional()`).
    fn emit_into<'g, T: Sink<'g, S>>(&self, mut g: T, instrs: Vec<Instr<S>>) -> T
    where
        S: 'g,
    {
        for instr in instrs {
            g = match instr {
                Instr::Scan => g.v(ArgStrings::None),
                Instr::Lookup { lookup, site } => {
                    g.gql_lookup(Arc::clone(&self.query), lookup, self.bindings(site))
                }
                Instr::Unit => g.inject(vec![ElementProperty::from(0_i64)]),
                Instr::HasLabel(labels) => g.has_label(labels),
                Instr::Has(key, predicate) => g.has_p(key, predicate),
                Instr::Filter {
                    condition,
                    site,
                    subqueries,
                } => g.gql_filter(
                    Arc::clone(&self.query),
                    condition,
                    self.bindings(site),
                    self.children(subqueries),
                ),
                Instr::RowCheck(condition, site) => g.gql_filter(
                    Arc::clone(&self.query),
                    condition,
                    self.check_bindings(site),
                    Vec::new(),
                ),
                Instr::Label(id) => {
                    if self.labelled.get(id.0).copied().unwrap_or(false)
                        && let Some(label) = self.labels.get(id.0)
                    {
                        g.as_(label.clone())
                    } else {
                        g
                    }
                }
                Instr::RowLabel(segment) => {
                    if self.rows_labelled.get(segment).copied().unwrap_or(false) {
                        g.as_(self.row_label(segment))
                    } else {
                        g
                    }
                }
                Instr::Project {
                    columns,
                    site,
                    subqueries,
                } => g.gql_project(
                    Arc::clone(&self.query),
                    columns,
                    self.bindings(site),
                    self.children(subqueries),
                ),
                Instr::For {
                    list,
                    site,
                    variable,
                    ordinal,
                    subqueries,
                } => g.gql_for(
                    Arc::clone(&self.query),
                    variable,
                    list,
                    self.bindings(site),
                    ordinal,
                    self.children(subqueries),
                ),
                Instr::Aggregate {
                    columns,
                    calls,
                    site,
                    subqueries,
                } => g.gql_aggregate(
                    Arc::clone(&self.query),
                    columns,
                    calls,
                    self.bindings(site),
                    self.children(subqueries),
                ),
                Instr::Distinct => g.gql_distinct(),
                Instr::Order {
                    keys,
                    columns,
                    site,
                    subqueries,
                } => g.gql_order(
                    Arc::clone(&self.query),
                    keys,
                    self.order_bindings(columns, site),
                    self.children(subqueries),
                ),
                Instr::Skip(n) => g.skip(n),
                Instr::Limit(n) => g.limit(n),
                Instr::Hop(hop) => match hop.quantifier {
                    Some(quantifier) => self.quantified(g, hop, quantifier),
                    None => self.hop(g, hop),
                },
                Instr::Repeat { body, quantifier } => {
                    let body = self.emit_into(AnonymousTraversal::<'g, S>::new(), body);
                    self.looped(g, body, quantifier)
                }
                Instr::Select(id) => g.select(self.label(id)),
                Instr::Bind {
                    id,
                    site,
                    name,
                    kind,
                } => g.gql_bind(name, self.binding(id.0, site), kind),
                Instr::Same(id) => g.where_p(P::Eq(PValue::String(self.label(id)))),
                Instr::Differs(id) => g.where_p(P::Neq(PValue::String(self.label(id)))),
                Instr::Trail(ids) => g.gql_trail(ids.iter().map(|&id| self.label(id)).collect()),
                Instr::PartLabel(segment) => match self.part_labels.get(segment) {
                    Some(label) if !label.is_empty() => g.as_(label.clone()),
                    _ => g,
                },
                Instr::Merge { .. } if self.writes => g,
                Instr::Merge { direction, labels } => {
                    let walk = AnonymousTraversal::<'g, S>::new();
                    let mut walk = match direction {
                        EdgeDirection::Right => walk.out(adjacency(&labels)),
                        EdgeDirection::Left => walk.in_(adjacency(&labels)),
                        EdgeDirection::Any => walk.both(adjacency(&labels)),
                    };
                    match walk.steps.pop() {
                        Some(GraphStep::FlatMap(_, next)) => g.push_step(lazy_barrier_step(next)),
                        _ => g,
                    }
                }
                Instr::Optional(steps) => {
                    let part = self.emit_into(AnonymousTraversal::new(), steps);
                    g.optional(part)
                }
                Instr::Union(operands) => g.union(self.children(operands)),
                Instr::Call(body) => {
                    let child = self.emit_into(AnonymousTraversal::new(), body);
                    g.flat_map(child)
                }
                Instr::Exists { negated, body } => {
                    let child = self.emit_into(AnonymousTraversal::new(), body);
                    if negated {
                        g.not(child)
                    } else {
                        g.filter_t(child)
                    }
                }
                Instr::Value {
                    expr,
                    site,
                    subqueries,
                } => g.gql_value(
                    Arc::clone(&self.query),
                    expr,
                    self.bindings(site),
                    self.children(subqueries),
                ),
                Instr::Count => g.count(),
                Instr::Collect(expr) => g.gql_value(
                    Arc::clone(&self.query),
                    expr,
                    Arc::from([Binding::Column(0)]),
                    Vec::new(),
                ),
                Instr::InsertNode {
                    labels,
                    properties,
                    site,
                    subqueries,
                } => g.gql_insert_node(
                    Arc::clone(&self.query),
                    labels,
                    properties,
                    self.bindings(site),
                    self.children(subqueries),
                ),
                Instr::InsertEdge {
                    label,
                    from,
                    to,
                    properties,
                    site,
                    subqueries,
                } => g.gql_insert_edge(
                    Arc::clone(&self.query),
                    label,
                    self.endpoint(from, site),
                    self.endpoint(to, site),
                    properties,
                    self.bindings(site),
                    self.children(subqueries),
                ),
                Instr::Discard => g.discard(),
            };
        }
        g
    }

    /// The child traversals of hosted subquery bodies (see [`Bodies`]).
    fn children<'g>(&self, bodies: Bodies<S>) -> Vec<AnonymousTraversal<'g, S>>
    where
        S: 'g,
    {
        bodies
            .into_iter()
            .map(|body| self.emit_into(AnonymousTraversal::new(), body))
            .collect()
    }

    /// The endpoint of an inserted edge read at `site` (the node inserted just before is the
    /// current object there).
    fn endpoint(&self, end: InsertEnd, site: Site) -> GqlEndpoint {
        GqlEndpoint {
            name: end
                .name
                .unwrap_or_else(|| CompactString::from(format!("({})", self.label(end.id)))),
            binding: self.binding(end.id.0, site),
            span: end.span,
        }
    }

    /// The path label of binding `id`.
    fn label(&self, id: BindingId) -> CompactString {
        self.labels.get(id.0).cloned().unwrap_or_default()
    }

    /// Whether binding `id` gets `as()`.
    fn is_labelled(&self, id: BindingId) -> bool {
        self.labelled.get(id.0).copied().unwrap_or(false)
    }

    /// Emits a hop (see [`Hop`]).
    fn hop<'g, T: Sink<'g, S>>(&self, g: T, hop: Hop<S>) -> T
    where
        S: 'g,
    {
        let labels = || adjacency(&hop.labels);
        let on_edge = hop
            .steps
            .iter()
            .any(|instr| !matches!(instr, Instr::Label(id) if !self.is_labelled(*id)));
        if !on_edge {
            return match hop.direction {
                EdgeDirection::Right => g.out(labels()),
                EdgeDirection::Left => g.in_(labels()),
                EdgeDirection::Any => g.union(vec![
                    AnonymousTraversal::new().out(labels()),
                    AnonymousTraversal::new()
                        .in_e(labels())
                        .gql_not_self_loop()
                        .out_v(),
                ]),
            };
        }
        let mut edge = AnonymousTraversal::<'g, S>::new();
        for instr in hop.steps {
            edge = self.edge_step(edge, instr);
        }
        let append = |mut t: AnonymousTraversal<'g, S>, steps: &[GraphStep<'g, S>]| {
            for step in steps {
                t = t.push_step(step.clone());
            }
            t
        };
        match hop.direction {
            EdgeDirection::Right => {
                let mut g = g.out_e(labels());
                for step in edge.steps {
                    g = g.push_step(step);
                }
                g.in_v()
            }
            EdgeDirection::Left => {
                let mut g = g.in_e(labels());
                for step in edge.steps {
                    g = g.push_step(step);
                }
                g.out_v()
            }
            EdgeDirection::Any => {
                let outgoing =
                    append(AnonymousTraversal::new().out_e(labels()), &edge.steps).in_v();
                let incoming = append(
                    AnonymousTraversal::new().in_e(labels()).gql_not_self_loop(),
                    &edge.steps,
                )
                .out_v();
                g.union(vec![outgoing, incoming])
            }
        }
    }

    /// Emits a quantified edge (plan mechanism 5): the single hop is the body of a `repeat()`
    /// whose modulators give the bounds. `{m,n}`: `repeat(B).emit(__.loops().is(P.gte(m)))
    /// .times(n)` (`emit()` for `m = 1`); `{n}`: `repeat(B).times(n)`; `{0,n}`:
    /// `emit().repeat(B).times(n)` (the zero-length match first); no upper bound: no `times()`,
    /// the loop ends when trail leaves no unused edge, or fails with `repeat.max_loops` (D14:
    /// an error, never a cut result). An upper bound beyond `u32::MAX` is no bound (the loop
    /// counter is a `u32`); `{0}` emits no step at all (see [`Lowering::ops`]).
    fn quantified<'g, T: Sink<'g, S>>(&self, g: T, hop: Hop<S>, quantifier: Quantifier) -> T
    where
        S: 'g,
    {
        let inner = hop
            .inner
            .and_then(|id| self.inner_labels.get(id.0))
            .filter(|label| !label.is_empty())
            .cloned();
        let body = self.hop(AnonymousTraversal::<'g, S>::new(), hop);
        let body = match inner {
            Some(label) => body.as_(label),
            None => body,
        };
        self.looped(g, body, quantifier)
    }

    /// Emits `repeat(body)` with the modulators of `quantifier` (see [`Self::quantified`]): the
    /// loop of a quantified edge, and of a quantified parenthesized path pattern (stage 2e).
    fn looped<'g, T: Sink<'g, S>>(
        &self,
        g: T,
        body: AnonymousTraversal<'g, S>,
        quantifier: Quantifier,
    ) -> T
    where
        S: 'g,
    {
        let max = quantifier.max.and_then(|n| u32::try_from(n).ok());
        let at_least = || {
            let min = i64::try_from(quantifier.min).unwrap_or(i64::MAX);
            AnonymousTraversal::<'g, S>::new()
                .loops()
                .is(P::Gte(PValue::Number(Number::from_i64(min))))
        };
        let walk = AnonymousTraversal::<'g, S>::new();
        let walk = match (quantifier.min, max) {
            (0, Some(0)) => return g,
            (0, Some(n)) => walk.emit().repeat(body).times(n),
            (0, None) => walk.emit().repeat(body),
            (m, Some(n)) if u64::from(n) == m => walk.repeat(body).times(n),
            (1, Some(n)) => walk.repeat(body).emit().times(n),
            (1, None) => walk.repeat(body).emit(),
            (_, Some(n)) => walk.repeat(body).emit_with(at_least()).times(n),
            (_, None) => walk.repeat(body).emit_with(at_least()),
        };
        let mut g = g;
        for step in walk.steps {
            g = g.push_step(step);
        }
        g
    }

    /// Emits one step on the edge traverser of a hop.
    fn edge_step<'g>(
        &self,
        t: AnonymousTraversal<'g, S>,
        instr: Instr<S>,
    ) -> AnonymousTraversal<'g, S>
    where
        S: 'g,
    {
        match instr {
            Instr::HasLabel(labels) => t.has_label(labels),
            Instr::Has(key, predicate) => t.has_p(key, predicate),
            Instr::Filter {
                condition,
                site,
                subqueries,
            } => t.gql_filter_hosted(
                Arc::clone(&self.query),
                condition,
                self.bindings(site),
                self.children(subqueries),
            ),
            Instr::RowCheck(condition, site) => t.gql_filter(
                Arc::clone(&self.query),
                condition,
                self.check_bindings(site),
            ),
            Instr::Label(id) if self.is_labelled(id) => t.as_(self.label(id)),
            Instr::Same(id) => t.where_p(P::Eq(PValue::String(self.label(id)))),
            Instr::Differs(id) => t.where_p(P::Neq(PValue::String(self.label(id)))),
            Instr::Trail(ids) => t.gql_trail(ids.iter().map(|&id| self.label(id)).collect()),
            Instr::Exists { negated, body } => {
                let child = self.emit_into(AnonymousTraversal::new(), body);
                if negated {
                    t.not(child)
                } else {
                    t.filter_t(child)
                }
            }
            Instr::Label(_)
            | Instr::PartLabel(_)
            | Instr::Scan
            | Instr::Lookup { .. }
            | Instr::Unit
            | Instr::RowLabel(_)
            | Instr::Project { .. }
            | Instr::For { .. }
            | Instr::Aggregate { .. }
            | Instr::Distinct
            | Instr::Order { .. }
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Hop(_)
            | Instr::Repeat { .. }
            | Instr::Select(_)
            | Instr::Bind { .. }
            | Instr::Merge { .. }
            | Instr::Value { .. }
            | Instr::Count
            | Instr::Collect(_)
            | Instr::Optional(_)
            | Instr::Union(_)
            | Instr::Call(_)
            | Instr::InsertNode { .. }
            | Instr::InsertEdge { .. }
            | Instr::Discard => t,
        }
    }
}

/// The traversal the lowering emits onto: the source (`g`), or a child traversal (`__`: the
/// part inside `optional()`). The same builders on both, so one emission serves both.
trait Sink<'g, S: GraphStorage + 'g>: Sized {
    fn v(self, ids: ArgStrings) -> Self;
    fn inject(self, values: Vec<ElementProperty>) -> Self;
    fn has_label(self, labels: Vec<String>) -> Self;
    fn has_p(self, key: String, predicate: P<PValue>) -> Self;
    fn gql_filter(
        self,
        query: Arc<str>,
        condition: RowExpr<S>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    fn as_(self, label: CompactString) -> Self;
    fn gql_project(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    fn gql_aggregate(
        self,
        query: Arc<str>,
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    #[allow(clippy::too_many_arguments)]
    fn gql_for(
        self,
        query: Arc<str>,
        variable: CompactString,
        list: RowExpr<S>,
        bindings: Arc<[Binding]>,
        ordinal: Option<GqlOrdinal>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    fn gql_lookup(self, query: Arc<str>, lookup: GqlLookup<S>, bindings: Arc<[Binding]>) -> Self;
    fn gql_value(
        self,
        query: Arc<str>,
        expr: RowExpr<S>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    fn gql_distinct(self) -> Self;
    fn discard(self) -> Self;
    fn gql_insert_node(
        self,
        query: Arc<str>,
        labels: Vec<CompactString>,
        properties: Vec<GqlInsertProperty<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    #[allow(clippy::too_many_arguments)]
    fn gql_insert_edge(
        self,
        query: Arc<str>,
        label: CompactString,
        from: GqlEndpoint,
        to: GqlEndpoint,
        properties: Vec<GqlInsertProperty<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    fn gql_order(
        self,
        query: Arc<str>,
        keys: Vec<GqlSortKey<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self;
    fn skip(self, n: usize) -> Self;
    fn limit(self, n: usize) -> Self;
    fn select(self, label: CompactString) -> Self;
    fn gql_bind(self, name: CompactString, binding: Binding, kind: ElementKind) -> Self;
    fn where_p(self, predicate: P<PValue>) -> Self;
    fn gql_trail(self, labels: Vec<CompactString>) -> Self;
    fn out(self, labels: ArgStrings) -> Self;
    fn in_(self, labels: ArgStrings) -> Self;
    fn out_e(self, labels: ArgStrings) -> Self;
    fn in_e(self, labels: ArgStrings) -> Self;
    fn in_v(self) -> Self;
    fn out_v(self) -> Self;
    fn union(self, branches: Vec<AnonymousTraversal<'g, S>>) -> Self;
    fn optional(self, part: AnonymousTraversal<'g, S>) -> Self;
    fn flat_map(self, child: AnonymousTraversal<'g, S>) -> Self;
    fn filter_t(self, child: AnonymousTraversal<'g, S>) -> Self;
    fn not(self, child: AnonymousTraversal<'g, S>) -> Self;
    fn count(self) -> Self;
    fn push_step(self, step: GraphStep<'g, S>) -> Self;
}

/// [`Sink`] for a traversal type with the builders of the same names (inherent methods take
/// precedence over the trait's, so each body calls the builder).
macro_rules! sink {
    ($traversal:ident) => {
        impl<'g, S: GraphStorage + 'g> Sink<'g, S> for $traversal<'g, S> {
            fn v(self, ids: ArgStrings) -> Self {
                self.v(ids)
            }
            fn inject(self, values: Vec<ElementProperty>) -> Self {
                self.inject(values)
            }
            fn has_label(self, labels: Vec<String>) -> Self {
                self.has_label(labels)
            }
            fn has_p(self, key: String, predicate: P<PValue>) -> Self {
                self.has_p(key, predicate)
            }
            fn gql_filter(
                self,
                query: Arc<str>,
                condition: RowExpr<S>,
                bindings: Arc<[Binding]>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_filter_hosted(query, condition, bindings, children)
            }
            fn as_(self, label: CompactString) -> Self {
                self.as_(label)
            }
            fn gql_project(
                self,
                query: Arc<str>,
                columns: Vec<GqlColumn<S>>,
                bindings: Arc<[Binding]>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_project_hosted(query, columns, bindings, children)
            }
            fn gql_aggregate(
                self,
                query: Arc<str>,
                columns: Vec<GqlAggregateColumn<S>>,
                calls: Vec<AggregateCall<GqlElement<S>>>,
                bindings: Arc<[Binding]>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_aggregate_hosted(query, columns, calls, bindings, children)
            }
            fn gql_for(
                self,
                query: Arc<str>,
                variable: CompactString,
                list: RowExpr<S>,
                bindings: Arc<[Binding]>,
                ordinal: Option<GqlOrdinal>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_for_hosted(query, variable, list, bindings, ordinal, children)
            }
            fn gql_lookup(
                self,
                query: Arc<str>,
                lookup: GqlLookup<S>,
                bindings: Arc<[Binding]>,
            ) -> Self {
                self.gql_lookup(query, lookup, bindings)
            }
            fn gql_value(
                self,
                query: Arc<str>,
                expr: RowExpr<S>,
                bindings: Arc<[Binding]>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_value_hosted(query, expr, bindings, children)
            }
            fn gql_distinct(self) -> Self {
                self.gql_distinct()
            }
            fn discard(self) -> Self {
                self.discard()
            }
            fn gql_insert_node(
                self,
                query: Arc<str>,
                labels: Vec<CompactString>,
                properties: Vec<GqlInsertProperty<S>>,
                bindings: Arc<[Binding]>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_insert_node(query, labels, properties, bindings, children)
            }
            fn gql_insert_edge(
                self,
                query: Arc<str>,
                label: CompactString,
                from: GqlEndpoint,
                to: GqlEndpoint,
                properties: Vec<GqlInsertProperty<S>>,
                bindings: Arc<[Binding]>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_insert_edge(query, label, from, to, properties, bindings, children)
            }
            fn gql_order(
                self,
                query: Arc<str>,
                keys: Vec<GqlSortKey<S>>,
                bindings: Arc<[Binding]>,
                children: Vec<AnonymousTraversal<'g, S>>,
            ) -> Self {
                self.gql_order_hosted(query, keys, bindings, children)
            }
            fn skip(self, n: usize) -> Self {
                self.skip(n)
            }
            fn limit(self, n: usize) -> Self {
                self.limit(n)
            }
            fn select(self, label: CompactString) -> Self {
                self.select(label)
            }
            fn gql_bind(self, name: CompactString, binding: Binding, kind: ElementKind) -> Self {
                self.gql_bind(name, binding, kind)
            }
            fn where_p(self, predicate: P<PValue>) -> Self {
                self.where_p(predicate)
            }
            fn gql_trail(self, labels: Vec<CompactString>) -> Self {
                self.gql_trail(labels)
            }
            fn out(self, labels: ArgStrings) -> Self {
                self.out(labels)
            }
            fn in_(self, labels: ArgStrings) -> Self {
                self.in_(labels)
            }
            fn out_e(self, labels: ArgStrings) -> Self {
                self.out_e(labels)
            }
            fn in_e(self, labels: ArgStrings) -> Self {
                self.in_e(labels)
            }
            fn in_v(self) -> Self {
                self.in_v()
            }
            fn out_v(self) -> Self {
                self.out_v()
            }
            fn union(self, branches: Vec<AnonymousTraversal<'g, S>>) -> Self {
                self.union(branches)
            }
            fn optional(self, part: AnonymousTraversal<'g, S>) -> Self {
                self.optional(part)
            }
            fn flat_map(self, child: AnonymousTraversal<'g, S>) -> Self {
                self.flat_map(child)
            }
            fn filter_t(self, child: AnonymousTraversal<'g, S>) -> Self {
                self.filter_t(child)
            }
            fn not(self, child: AnonymousTraversal<'g, S>) -> Self {
                self.not(child)
            }
            fn count(self) -> Self {
                self.count()
            }
            fn push_step(self, step: GraphStep<'g, S>) -> Self {
                self.push_step(step)
            }
        }
    };
}

sink!(GraphTraversalSource);
sink!(AnonymousTraversal);

/// The labels argument of an adjacency step.
fn adjacency(labels: &Option<Vec<String>>) -> ArgStrings {
    match labels {
        Some(labels) => ArgStrings::List(labels.clone()),
        None => ArgStrings::None,
    }
}

/// Parses, binds, plans and lowers `query`; every error is the [`TraverserError::Gql`] the
/// marker raises.
fn prepare<S: GraphStorage>(
    query: &str,
    params: &[(CompactString, ElementProperty)],
    facts: &StorageFacts<'_>,
) -> Result<Lowered<S>, TraverserError> {
    let parsed = parse(query)?;
    let bound = bind_with_parameters(&parsed, params.iter().map(|(name, _)| name.as_str()))?;
    let plan = plan(&bound, facts, None);
    Lowering::new(query, &bound, &plan, params).run()
}

/// The data writes the plan of `query` asks the authorization check for (each once, in plan
/// order, child traversals depth first) and the [`AccessFlow`] the plan states after its last
/// step: what a saved query's body calling `g.gql(query)` may write (`script/effects.rs`).
/// `query` is lowered exactly as `gql()` lowers it, on an empty scratch graph (the write steps
/// do not depend on the planner's statistics), each parameter it reads standing for a
/// placeholder value (labels never come from parameters, so the requests do not depend on
/// them). A query that does not parse or bind writes nothing (it fails before it runs);
/// `None` when no placeholder lowers (a parameter whose value the plan itself needs, such as
/// `LIMIT $n`), which the caller treats as "may write anything".
#[cfg(feature = "script")]
pub(crate) fn planned_writes(
    query: &str,
) -> Option<(
    Vec<crate::graph::AccessRequestBuf>,
    crate::traversal::step::AccessFlow,
)> {
    use crate::graph::TraversalGraph;
    let nothing = || Some((Vec::new(), Default::default()));
    let Ok(parsed) = parse(query) else {
        return nothing();
    };
    let Ok(bound) = super::bind::bind(&parsed) else {
        return nothing();
    };
    let scratch = TraversalGraph::new();
    for placeholder in [ElementProperty::from(1), ElementProperty::Null] {
        let params: Vec<(CompactString, ElementProperty)> = bound
            .parameters
            .iter()
            .map(|p| (CompactString::new(&p.name), placeholder.clone()))
            .collect();
        let source = GraphTraversalSource::new(&scratch);
        let lowered = {
            let facts = StorageFacts::from_storage(&source.storage);
            prepare::<&TraversalGraph>(query, &params, &facts)
        };
        if let Ok(lowered) = lowered {
            let mut steps = lowered.emit(source).steps;
            let mut writes = Vec::new();
            let flow =
                crate::traversal::permission_check::plan_write_requests(&mut steps, &mut writes);
            return Some((writes, flow));
        }
    }
    None
}

/// The most subquery bodies one query lowers (module docs, "Subqueries"): each host of a
/// subquery gets its own child, and a `LET` value holding one puts it into every expression
/// that reads the name, also inside other bodies, so nested bodies multiply. Far beyond any
/// written query; it keeps a query fast to prepare.
pub(crate) const MAX_LOWERED_SUBQUERIES: usize = 1024;

/// The names a compiled expression resolves (slot `i` < bindings: binding `i`; then the
/// `RETURN` columns for `ORDER BY`).
struct LowerScope<'a> {
    /// The key of each binding ([`Binding::key`](super::bind::Binding), the name stored
    /// expressions use; `None`: anonymous).
    names: &'a [Option<String>],
    /// The slot the reserved empty name stands for: the element a synthesized expression
    /// (an inline property of an anonymous node, a label factor) is about, or an `ORDER BY`
    /// key that is a whole column. No query can write the empty name.
    this: Option<usize>,
    /// The aliased `RETURN` columns `ORDER BY` may name, with their slots (an alias shadows a
    /// variable of the same name).
    aliases: &'a [(String, usize)],
    params: &'a [(CompactString, ElementProperty)],
    bound: &'a BoundQuery,
    /// The subqueries the step being compiled hosts, in hosting order (the 2b hook); `None`:
    /// no subquery can run here.
    hooks: Option<&'a Hooks>,
}

/// The subqueries (indexes into `BoundQuery::subqueries`) one step hosts, in the order its
/// compiled expressions number them ([`Scope::subquery`]); each once.
type Hooks = std::cell::RefCell<Vec<usize>>;

impl Scope for LowerScope<'_> {
    fn subquery(&self, span: Span) -> Option<Result<usize, super::eval::EvalError>> {
        let hooks = self.hooks?;
        let k = self.bound.subquery_at(span)?;
        let mut hooks = hooks.borrow_mut();
        if let Some(index) = hooks.iter().position(|&h| h == k) {
            return Some(Ok(index));
        }
        if hooks.len() >= MAX_HOSTED_SUBQUERIES {
            return Some(Err(super::eval::EvalError::new(
                super::eval::status::LIMIT,
                format!("one expression step holds more than {MAX_HOSTED_SUBQUERIES} subqueries"),
                span,
            )));
        }
        hooks.push(k);
        Some(Ok(hooks.len() - 1))
    }

    fn variable(&self, name: &str) -> Option<usize> {
        if name.is_empty() {
            return self.this;
        }
        if let Some((_, slot)) = self.aliases.iter().find(|(alias, _)| alias == name) {
            return Some(*slot);
        }
        self.names.iter().position(|n| n.as_deref() == Some(name))
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        parameter(self.params, name).cloned()
    }
}

/// The value given for `$name` (the last one when given twice).
fn parameter<'p>(
    params: &'p [(CompactString, ElementProperty)],
    name: &str,
) -> Option<&'p ElementProperty> {
    params
        .iter()
        .rev()
        .find(|(n, _)| n.as_str() == name)
        .map(|(_, value)| value)
}

/// The labels for the step that reaches an element (`None`: no label restriction there), the
/// steps that run on the element before its `as()`, and the conjuncts holding a subquery (the
/// hooked `gql:filter`s, then the fast-path `EXISTS` children) that run after it (design E11:
/// last; after the `as()`, so a body reading the element through its label, a path variable for
/// one, finds it).
type ElementSteps<S> = (Option<Vec<String>>, Vec<Instr<S>>, Vec<Instr<S>>);

struct Lowering<'q> {
    query: &'q str,
    shared: Arc<str>,
    bound: &'q BoundQuery,
    plan: &'q LogicalPlan,
    params: &'q [(CompactString, ElementProperty)],
    names: Vec<Option<String>>,
    /// The subquery bodies lowered so far (bounded by [`MAX_LOWERED_SUBQUERIES`]).
    children: std::cell::Cell<usize>,
    /// Every linear query of the bound query (stage 2d): the top-level ones in text order
    /// first (the first one is the query when it is linear), then those of `CALL` bodies and
    /// whole-query subquery bodies. The `RETURN` row of linear query `p` is result pipeline
    /// `segments.len() + p` ([`Site::ordered`], [`Lowering::result_of`]).
    linears: Vec<&'q BoundLinear>,
    /// Per binding: a variable of the body of a parenthesized path pattern quantified by `?`
    /// (stage 2e, ISO conditional singletons): absent (`NULL`) after zero iterations, so a
    /// trail check against it reads every occurrence (`gql:trail`, none passes) instead of
    /// `where(P.neq(..))`, which would drop the row.
    conditional: Vec<bool>,
}

impl<'q> Lowering<'q> {
    fn new(
        query: &'q str,
        bound: &'q BoundQuery,
        plan: &'q LogicalPlan,
        params: &'q [(CompactString, ElementProperty)],
    ) -> Self {
        Lowering {
            query,
            shared: Arc::from(query),
            bound,
            plan,
            params,
            names: bound.bindings.iter().map(|b| b.key.clone()).collect(),
            children: std::cell::Cell::new(0),
            linears: linears(bound),
            conditional: conditional_bindings(bound),
        }
    }

    /// The result pipeline index of the `RETURN` row of `linear` (see [`Self::linears`]).
    fn result_of(&self, linear: &BoundLinear) -> usize {
        let p = self
            .linears
            .iter()
            .position(|l| std::ptr::eq(*l, linear))
            .unwrap_or(0);
        self.bound.segments.len() + p
    }

    /// The `RETURN` columns of result pipeline `result` when it is a `RETURN` row.
    fn return_columns(&self, result: usize) -> Option<&'q [Column]> {
        let p = result.checked_sub(self.bound.segments.len())?;
        self.linears.get(p).map(|l| l.columns.as_slice())
    }

    /// Every pipeline of the plan, those of `CALL` bodies and whole-query subquery bodies
    /// included (in no particular order).
    fn pipelines(&self) -> Vec<&'q PipelinePlan> {
        let plan: &'q LogicalPlan = self.plan;
        let mut work: Vec<&'q CompositePlan> = vec![&plan.body];
        work.extend(plan.subqueries.iter().filter_map(|s| s.query.as_deref()));
        let mut pipelines = Vec::new();
        while let Some(composite) = work.pop() {
            for pipeline in composite.blocks().flat_map(UnionPlan::operands) {
                pipelines.push(pipeline);
                for stage in &pipeline.stages {
                    if let Stage::Call(call) = stage {
                        work.push(&call.plan);
                    }
                }
            }
        }
        pipelines
    }

    fn scope<'s>(
        &'s self,
        this: Option<usize>,
        aliases: &'s [(String, usize)],
        hooks: Option<&'s Hooks>,
    ) -> LowerScope<'s> {
        LowerScope {
            names: &self.names,
            this,
            aliases,
            params: self.params,
            bound: self.bound,
            hooks,
        }
    }

    /// Compiles `expr` where no subquery can run (the binder or planner keeps them away).
    fn compile<S: GraphStorage>(
        &self,
        expr: &Expr,
        this: Option<usize>,
        aliases: &[(String, usize)],
    ) -> Result<RowExpr<S>, TraverserError> {
        self.compile_in(expr, this, aliases, None)
    }

    /// Compiles `expr` for a step hosting the subqueries it holds in `hooks` (the 2b hook).
    fn compile_in<S: GraphStorage>(
        &self,
        expr: &Expr,
        this: Option<usize>,
        aliases: &[(String, usize)],
        hooks: Option<&Hooks>,
    ) -> Result<RowExpr<S>, TraverserError> {
        compile(expr, &self.scope(this, aliases, hooks))
            .map_err(|error| eval_error(self.query, error))
    }

    /// The bodies of the subqueries a step at `site` hosts (`hooks`, in hosting order), each
    /// the body's plan for the site's current element lowered from the site (as for the fast
    /// path, [`Self::body`]).
    fn bodies<S: GraphStorage>(
        &self,
        hooks: Hooks,
        site: Site,
    ) -> Result<Bodies<S>, TraverserError> {
        hooks
            .into_inner()
            .into_iter()
            .map(|k| match self.bound.subqueries.get(k) {
                Some(sub) => self.body(k, sub.span, site),
                None => Err(self.unlowered()),
            })
            .collect()
    }

    fn run<S: GraphStorage>(self) -> Result<Lowered<S>, TraverserError> {
        let mut instrs = Vec::new();
        let bound = self.bound;
        self.composite(&self.plan.body, &bound.body, Start::Query, &mut instrs)?;
        let labels = self.labels();
        let row_labels = self.row_labels(&labels);
        let columns: Vec<Option<ColumnRef>> = bound.bindings.iter().map(|b| b.column).collect();
        let ordered = self.ordered_tables();
        let (mut labelled, rows_labelled) = self.labelled(&instrs, &columns, &ordered);
        let items = self
            .bound
            .segments
            .iter()
            .map(|segment| matches!(segment, Segment::For(f) if f.ordinal.is_none()))
            .collect();
        let GroupTables {
            groups,
            part_labels,
            anchors,
            reversed,
        } = self.groups(&labels, &mut labelled);
        let inner_labels = self.inner_labels(&labels);
        let paths = self.paths(&labels, &inner_labels, &reversed, &anchors);
        Ok(Lowered {
            query: self.shared,
            instrs,
            labels,
            labelled,
            columns,
            row_labels,
            rows_labelled,
            ordered,
            items,
            groups,
            part_labels,
            inner_labels,
            paths,
            writes: bound.writes(),
        })
    }

    /// Lowers a composite query (stage 2d, design item 8), the whole query or the body of a
    /// `CALL` (`start`: where its first block starts): its blocks in `NEXT` order, each one
    /// operand inline or several as `union(__.<op>, ..)` (the start step `g.union(..)` for the
    /// first block of the query, a mid-traversal `union()` after `NEXT` or in a `CALL` body,
    /// whose operands each read every record of the block before, or the incoming row: the
    /// children of `union()` see the whole input), then `gql:distinct()` for `UNION` /
    /// `UNION DISTINCT`.
    #[inline(never)]
    fn composite<S: GraphStorage>(
        &self,
        plan: &'q CompositePlan,
        bound: &'q BoundComposite,
        start: Start,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let blocks = std::iter::once(&bound.first).chain(&bound.next);
        if plan.blocks().count() != bound.next.len() + 1 {
            return Err(self.unlowered());
        }
        for (k, (block_plan, block)) in plan.blocks().zip(blocks).enumerate() {
            if block_plan.operands().count() != block.operands().count() {
                return Err(self.unlowered());
            }
            // A block after `NEXT` starts with the `Stage::Rows` of the block before.
            let start = if k == 0 {
                start
            } else {
                Start::At(Site::default())
            };
            if block_plan.unions.is_empty() {
                self.linear(&block_plan.first, &block.first, start, instrs)?;
            } else {
                let mut branches = Vec::with_capacity(block_plan.unions.len() + 1);
                for (pipeline, linear) in block_plan.operands().zip(block.operands()) {
                    let mut branch = Vec::new();
                    self.linear(pipeline, linear, start, &mut branch)?;
                    branches.push(branch);
                }
                instrs.push(Instr::Union(branches));
            }
            if block_plan.distinct {
                instrs.push(Instr::Distinct);
            }
        }
        Ok(())
    }

    /// Lowers one linear query: its match pipeline, its later stages, its result pipeline,
    /// from `start` (see [`Start`]).
    fn linear<S: GraphStorage>(
        &self,
        plan: &'q PipelinePlan,
        linear: &BoundLinear,
        start: Start,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let site = match start {
            Start::Query => {
                // A query that does not start with `MATCH` (`OPTIONAL MATCH`, `FOR`, `WITH`,
                // `RETURN`, `CALL`, or the conjuncts of a leading `LET` / `FILTER` pipeline)
                // starts from one unit row.
                if !matches!(plan.ops.first(), Some(Op::Scan(_) | Op::Anchor(_))) {
                    instrs.push(Instr::Unit);
                }
                Site::default()
            }
            Start::At(site) => site,
        };
        let mut site = self.ops(&plan.ops, site, instrs)?;
        for stage in &plan.stages {
            site = match stage {
                Stage::Match { ops, .. } => self.ops(ops, site, instrs)?,
                Stage::Optional { ops, segment, .. } => {
                    let mut part = Vec::new();
                    self.ops(ops, site, &mut part)?;
                    part.push(Instr::PartLabel(*segment));
                    in_part(&mut part, *segment);
                    instrs.push(Instr::Optional(part));
                    // The current object differs per row (matched or not): every later read
                    // goes through a label.
                    Site::default()
                }
                Stage::Materialize(m) => self.materialize(m, site, instrs)?,
                Stage::For(f) => self.for_stage(f, site, instrs)?,
                // The records of the block before are the rows (design E19): read like the row
                // of a materializing `WITH` (`#n<k>` only when read after leaving it).
                Stage::Rows(rows) => {
                    instrs.push(Instr::RowLabel(rows.segment));
                    Site::row(rows.segment)
                }
                Stage::Call(call) => self.call(call, site, instrs)?,
                Stage::Write(write) => self.write(write, site, instrs)?,
            };
        }
        if linear.end == QueryEnd::Return {
            self.pipeline(
                &plan.result,
                &linear.columns,
                &linear.order_by,
                (site, self.result_of(linear)),
                instrs,
            )?;
        } else {
            // `FINISH` or a write ending (design W2): the rows did their work, none returns.
            instrs.push(Instr::Discard);
        }
        Ok(())
    }

    /// Lowers a write clause at `site` (stage 3a, [`WriteStage`]): an `INSERT` as one insert
    /// step per new node in text order, then one per new edge in text order (design W-d),
    /// each followed by its element's `as()` (emitted only when read later, D11). The first
    /// step reads the row at `site`; each later one runs on the element created just before
    /// (the current object), so an edge's end inserted right before it is `Binding::Current`.
    /// Returns the site on the last element created.
    fn write<S: GraphStorage>(
        &self,
        stage: &WriteStage,
        mut site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        let Some(Segment::Write(write)) = self.bound.segments.get(stage.segment) else {
            return Err(self.unlowered());
        };
        match &**write {
            BoundWrite::Insert(insert) => {
                for node in &insert.nodes {
                    let hooks = Hooks::default();
                    let properties = self.insert_properties(&node.properties, &hooks)?;
                    instrs.push(Instr::InsertNode {
                        labels: node
                            .labels
                            .iter()
                            .map(|l| CompactString::from(l.name.as_str()))
                            .collect(),
                        properties,
                        site,
                        subqueries: self.bodies(hooks, site)?,
                    });
                    instrs.push(Instr::Label(node.binding));
                    site = Site::on(node.binding);
                }
                for edge in &insert.edges {
                    let hooks = Hooks::default();
                    let properties = self.insert_properties(&edge.properties, &hooks)?;
                    instrs.push(Instr::InsertEdge {
                        label: CompactString::from(edge.label.name.as_str()),
                        from: self.insert_end(insert, edge.source, edge.span),
                        to: self.insert_end(insert, edge.target, edge.span),
                        properties,
                        site,
                        subqueries: self.bodies(hooks, site)?,
                    });
                    instrs.push(Instr::Label(edge.binding));
                    site = Site::on(edge.binding);
                }
            }
        }
        Ok(site)
    }

    /// The values of an inserted element, compiled for a step hosting the subqueries they
    /// hold (`hooks`).
    fn insert_properties<S: GraphStorage>(
        &self,
        properties: &[InsertProperty],
        hooks: &Hooks,
    ) -> Result<Vec<GqlInsertProperty<S>>, TraverserError> {
        properties
            .iter()
            .map(|p| {
                Ok(GqlInsertProperty {
                    key: CompactString::from(p.key.name.as_str()),
                    value: self.compile_in(&p.value, None, &[], Some(hooks))?,
                })
            })
            .collect()
    }

    /// The end `id` of the edge of `insert` written at `edge`: its node pattern is the one of
    /// the clause nearest to the edge pattern (each occurrence of the node in the clause, the
    /// pattern of a new node included).
    fn insert_end(&self, insert: &BoundInsert, id: BindingId, edge: Span) -> InsertEnd {
        let binding = self.bound.binding(id);
        let created = insert
            .nodes
            .iter()
            .filter(|n| n.binding == id)
            .map(|n| n.span);
        let occurrences = binding
            .into_iter()
            .flat_map(|b| b.occurrences.iter().copied())
            .filter(|o| o.start >= insert.span.start && o.end <= insert.span.end);
        let distance = |o: &Span| {
            if o.end <= edge.start {
                edge.start - o.end
            } else {
                o.start.saturating_sub(edge.end)
            }
        };
        let span = created
            .chain(occurrences)
            .min_by_key(distance)
            .unwrap_or(edge);
        InsertEnd {
            id,
            name: binding
                .and_then(|b| b.name.as_ref())
                .map(|n| CompactString::from(n.name.as_str())),
            span,
        }
    }

    /// Lowers a `[OPTIONAL] CALL { }` at `site` (stage 2d, design mechanism 6): its body onto
    /// the child of `flat_map()`, from `site` (the body starts on the incoming row: at its
    /// current element, reading the imports where they are), then the row label `#c<k>`
    /// (emitted only when read later); `OPTIONAL CALL` is `optional(__.flat_map(__.<body>)
    /// .as("#c<k>"))`, the row label inside, so a row the body gives no record keeps going and
    /// reads its returned names `NULL` (absent label). Returns the site after it: the body's
    /// record is the current object (its columns read `Binding::Column`); after an
    /// `OPTIONAL CALL` nothing (every read through a label).
    #[inline(never)]
    fn call<S: GraphStorage>(
        &self,
        call: &'q CallStage,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        let Some(Segment::Call(bound_call)) = self.bound.segments.get(call.segment) else {
            return Err(self.unlowered());
        };
        let mut body = Vec::new();
        self.composite(&call.plan, &bound_call.body, Start::At(site), &mut body)?;
        if bound_call.unit() {
            // A unit subquery (stage 3a, design W3): the body (ending in `discard()`) runs per
            // row, and the row passes on unchanged: `optional(__.<body>.discard())`, also for
            // `OPTIONAL CALL`.
            instrs.push(Instr::Optional(body));
            return Ok(site);
        }
        if call.optional {
            instrs.push(Instr::Optional(vec![
                Instr::Call(body),
                Instr::RowLabel(call.segment),
            ]));
            Ok(Site::default())
        } else {
            instrs.push(Instr::Call(body));
            instrs.push(Instr::RowLabel(call.segment));
            Ok(Site::row(call.segment))
        }
    }

    /// Lowers the match pipeline `ops` of one segment, starting at `site`; returns the site
    /// after it.
    fn ops<S: GraphStorage>(
        &self,
        ops: &[Op],
        mut site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        for (at, op) in ops.iter().enumerate() {
            match op {
                Op::Scan(scan) => {
                    instrs.push(Instr::Scan);
                    let (labels, steps, exists) =
                        self.element(scan.binding, &scan.filters, Vec::new())?;
                    if let Some(labels) = labels {
                        instrs.push(Instr::HasLabel(labels));
                    }
                    instrs.extend(steps);
                    instrs.push(Instr::Label(scan.binding));
                    instrs.extend(exists);
                    site = Site::on(scan.binding);
                }
                Op::Filter(ids) => {
                    for &id in ids {
                        self.conjunct(id, site, instrs)?;
                    }
                }
                Op::Expand(expand) => {
                    if !expand.from_current {
                        self.move_to(expand.from, site, instrs)?;
                    }
                    let zero_only = expand.quantifier.is_some_and(|q| q.max == Some(0));
                    // `{0}` matches only the zero-length path: no step, the far node is the
                    // node the walk starts from (its filters and label run on it).
                    if !zero_only {
                        instrs.push(Instr::Hop(self.hop(expand)?));
                    }
                    // After a quantified edge every traverser carries its own edges' labels,
                    // so a merge point there would never merge.
                    if let Some(next) = next_hop(ops, at)
                        && expand.quantifier.is_none()
                    {
                        instrs.push(Instr::Merge {
                            direction: next.direction,
                            labels: self.edge_labels(next),
                        });
                    }
                    if !expand.to_bound {
                        let (labels, steps, exists) =
                            self.element(expand.to, &expand.to_filters, Vec::new())?;
                        if let Some(labels) = labels {
                            instrs.push(Instr::HasLabel(labels));
                        }
                        instrs.extend(steps);
                        instrs.push(Instr::Label(expand.to));
                        instrs.extend(exists);
                    }
                    site = Site::on(expand.to);
                }
                Op::Group(group) => site = self.group(group, site, instrs)?,
                Op::Join { binding } => {
                    instrs.push(self.same(*binding)?);
                    if self.is_column(*binding) {
                        // A column binding has no label of its own; a path variable reads it
                        // through one set here (emitted only then).
                        instrs.push(Instr::Label(*binding));
                    }
                    site = Site::on(*binding);
                }
                Op::Resume(resume) => {
                    if !resume.from_current && site.current != Some(resume.binding) {
                        self.move_to(resume.binding, site, instrs)?;
                    }
                    site = Site::on(resume.binding);
                }
                Op::Lookup(lookup) => {
                    let lookup_instr = self.lookup(lookup, site)?;
                    instrs.push(lookup_instr);
                    let scan = &lookup.scan;
                    let (labels, steps, exists) =
                        self.element(scan.binding, &scan.filters, Vec::new())?;
                    if let Some(labels) = labels {
                        instrs.push(Instr::HasLabel(labels));
                    }
                    instrs.extend(steps);
                    instrs.push(Instr::Label(scan.binding));
                    instrs.extend(exists);
                    site = Site::on(scan.binding);
                }
                Op::Anchor(scan) => return Err(self.anchor_not_yet(scan.binding)),
            }
        }
        Ok(site)
    }

    /// The `gql:lookup` of `lookup` (design M3), its value read at `site` (the row before it).
    fn lookup<S: GraphStorage>(
        &self,
        lookup: &Lookup,
        site: Site,
    ) -> Result<Instr<S>, TraverserError> {
        let binding = self.bound.binding(lookup.scan.binding);
        let name = binding
            .and_then(|b| b.name.as_ref())
            .map_or("", |n| n.name.as_str());
        let labels = lookup.scan.scan_labels.clone().unwrap_or_default();
        let pattern = if labels.is_empty() {
            format!("({name})")
        } else {
            format!("({name}:{})", labels.join("|"))
        };
        let equality = self
            .bound
            .conjuncts
            .get(lookup.conjunct)
            .and_then(|c| self.query.get(c.span.start..c.span.end))
            .unwrap_or_default()
            .to_string();
        let key = match &lookup.by {
            LookupBy::Id => GqlLookupKey::Id,
            LookupBy::Index(key) => GqlLookupKey::Property(CompactString::from(key.as_str())),
        };
        Ok(Instr::Lookup {
            lookup: GqlLookup {
                pattern,
                equality,
                value: self.compile(&lookup.value, None, &[])?,
                key,
                labels,
            },
            site,
        })
    }

    /// Moves from `site` to node `id` bound earlier: `select(label)` for a pattern binding,
    /// `gql:bind` for a materializing `WITH` column binding (read from its row).
    fn move_to<S: GraphStorage>(
        &self,
        id: BindingId,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let Some(binding) = self.bound.binding(id) else {
            return Err(self.unlowered());
        };
        if binding.column.is_none() {
            instrs.push(Instr::Select(id));
            return Ok(());
        }
        instrs.push(self.bind(id, site)?);
        // A path variable reads the element through a label set here (emitted only then).
        instrs.push(Instr::Label(id));
        Ok(())
    }

    /// `gql:bind`: moves to the element binding `id` holds, read at `site` (a materializing
    /// `WITH` column binding, or a binding an aggregating projection's column holds,
    /// [`Site::ordered`]).
    fn bind<S: GraphStorage>(&self, id: BindingId, site: Site) -> Result<Instr<S>, TraverserError> {
        let Some(binding) = self.bound.binding(id) else {
            return Err(self.unlowered());
        };
        let kind = match binding.kind {
            BindingKind::Node => ElementKind::Vertex,
            BindingKind::Edge => ElementKind::Edge,
            BindingKind::Path | BindingKind::Value => return Err(self.unlowered()),
        };
        let name = binding
            .name
            .as_ref()
            .map(|n| n.name.as_str())
            .or(binding.key.as_deref())
            .unwrap_or_default();
        Ok(Instr::Bind {
            id,
            site,
            name: CompactString::from(name),
            kind,
        })
    }

    /// The check that the current element is the one bound earlier to `id` (a join).
    fn same<S: GraphStorage>(&self, id: BindingId) -> Result<Instr<S>, TraverserError> {
        if self.is_column(id) {
            self.row_check(id, BinaryOp::Eq)
        } else {
            Ok(Instr::Same(id))
        }
    }

    /// The check that the current edge differs from the edge bound earlier to `id` (trail).
    fn differs<S: GraphStorage>(&self, id: BindingId) -> Result<Instr<S>, TraverserError> {
        if self.is_column(id) {
            self.row_check(id, BinaryOp::Ne)
        } else {
            Ok(Instr::Differs(id))
        }
    }

    /// Whether `id` is a materializing `WITH` column binding (no path label of its own).
    fn is_column(&self, id: BindingId) -> bool {
        self.bound.binding(id).is_some_and(|b| b.column.is_some())
    }

    /// `current <op> var` over the extra slot of [`Instr::RowCheck`], for column binding `id`.
    fn row_check<S: GraphStorage>(
        &self,
        id: BindingId,
        op: BinaryOp,
    ) -> Result<Instr<S>, TraverserError> {
        let Some(binding) = self.bound.binding(id) else {
            return Err(self.unlowered());
        };
        let (Some(key), Some(&span)) = (binding.key.clone(), binding.occurrences.last()) else {
            return Err(self.unlowered());
        };
        let variable = |name: String| Expr {
            kind: ExprKind::Variable(Identifier { name, span }),
            span,
        };
        let expr = Expr {
            kind: ExprKind::Binary {
                op,
                left: Box::new(variable(String::new())),
                right: Box::new(variable(key)),
            },
            span,
        };
        let extra = self.bound.bindings.len();
        Ok(Instr::RowCheck(
            self.compile(&expr, Some(extra), &[])?,
            Site::default(),
        ))
    }

    /// Lowers a materializing `WITH` (decision C2) at `site`: its result pipeline, its `WHERE`
    /// on the new row, the row label (emitted only when read later). Returns the row's site.
    fn materialize<S: GraphStorage>(
        &self,
        m: &Materialize,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        let Some(Segment::With(with)) = self.bound.segments.get(m.segment) else {
            return Err(self.unlowered());
        };
        let row_labelled = self.pipeline(
            &m.result,
            &with.columns,
            &with.order_by,
            (site, m.segment),
            instrs,
        )?;
        let row = Site::row(m.segment);
        for &c in &m.filters {
            self.conjunct(c, row, instrs)?;
        }
        if !row_labelled {
            instrs.push(Instr::RowLabel(m.segment));
        }
        Ok(row)
    }

    /// Lowers a `FOR` / `UNWIND` at `site`: `gql:for` over its list (read at `site`), then the
    /// row label (emitted only when read later). Returns the row's site: the item (or the
    /// record `{x, i}`) is the current object.
    fn for_stage<S: GraphStorage>(
        &self,
        f: &ForStage,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        let Some(Segment::For(bound_for)) = self.bound.segments.get(f.segment) else {
            return Err(self.unlowered());
        };
        let name = |id: BindingId| {
            self.bound
                .binding(id)
                .and_then(|b| b.name.as_ref())
                .map(|n| CompactString::from(n.name.as_str()))
                .unwrap_or_default()
        };
        let hooks = Hooks::default();
        let list = self.compile_in(&bound_for.list, None, &[], Some(&hooks))?;
        let subqueries = self.bodies(hooks, site)?;
        let ordinal = bound_for.ordinal.map(|(kind, id)| GqlOrdinal {
            name: name(id),
            kind,
        });
        instrs.push(Instr::For {
            list,
            site,
            variable: name(bound_for.variable),
            ordinal,
            subqueries,
        });
        instrs.push(Instr::RowLabel(f.segment));
        Ok(Site::row(f.segment))
    }

    /// The error of a plan part the lowering does not emit (the binder refuses those queries
    /// with their own messages first, so this is a guard only).
    fn unlowered(&self) -> TraverserError {
        GqlErrorBuilder::new(
            GqlErrorKind::NotYet,
            0..self.query.len(),
            "this query form is not yet supported in this version",
        )
        .alternative(
            "write the query as MATCH patterns with WHERE and RETURN, or chain Gremlin steps \
             after gql()",
        )
        .into_error(self.query)
    }

    /// The error of the phase 2 anchor (a mid-traversal `gql()`), which stage 1a never plans.
    fn anchor_not_yet(&self, id: BindingId) -> TraverserError {
        let span = self
            .bound
            .binding(id)
            .and_then(|b| b.occurrences.first().copied())
            .map_or(0..0, |s| s.start..s.end);
        GqlErrorBuilder::new(
            GqlErrorKind::NotYet,
            span,
            "gql() cannot continue a traversal yet",
        )
        .alternative(start_with_gql(self.query))
        .into_error(self.query)
    }

    /// The label factors and conjuncts of element `id` where it is bound: the first factor that
    /// is a label disjunction (the scan's `has_label`, folded into `v(labels: ..)` by the
    /// optimizer, or the labels of an edge's adjacency step), then the steps on the element: the
    /// other label disjunctions as `has_label`, the pushed conjuncts as `has(k, P..)`, `checks`
    /// (join and trail checks), the other label factors and the residual conjuncts as
    /// `gql:filter`; apart from them the residual conjuncts holding a subquery and the fast-path
    /// `EXISTS` children (the caller emits them after the element's `as()`).
    fn element<S: GraphStorage>(
        &self,
        id: BindingId,
        filters: &ElementFilters,
        checks: Vec<Instr<S>>,
    ) -> Result<ElementSteps<S>, TraverserError> {
        let mut instrs = Vec::new();
        let labels = self
            .bound
            .binding(id)
            .map(|b| b.labels.as_slice())
            .unwrap_or_default();
        let factors = and_factors(labels);
        let first_factor = factors.iter().position(|f| label_disjuncts(f).is_some());
        let first = first_factor
            .and_then(|at| factors.get(at))
            .and_then(|f| label_disjuncts(f));
        let mut label_filters = Vec::new();
        for (at, factor) in factors.iter().enumerate() {
            if Some(at) == first_factor {
                continue;
            }
            match label_disjuncts(factor) {
                Some(labels) => instrs.push(Instr::HasLabel(labels)),
                None => label_filters.push(*factor),
            }
        }
        let mut residual = filters.residual.clone();
        for &c in &filters.pushed {
            match self.pushed(c)? {
                Some((key, predicate)) => instrs.push(Instr::Has(key, predicate)),
                None => residual.push(c),
            }
        }
        residual.sort_unstable();
        // A residual holding a subquery (the hook) runs after the element's `as()` too: its
        // body may read the element through its label (a later body clause resuming at it, a
        // path variable for it), and it runs after the plain residuals (design E11).
        let (hooked, residual): (Vec<usize>, Vec<usize>) = residual.into_iter().partition(|&c| {
            self.bound
                .conjuncts
                .get(c)
                .is_some_and(|conjunct| SubqueryUse::of(conjunct) != SubqueryUse::None)
        });
        instrs.extend(checks);
        for factor in label_filters {
            let condition = self.compile(&is_labeled(factor), Some(id.0), &[])?;
            instrs.push(Instr::Filter {
                condition,
                site: Site::on(id),
                subqueries: Vec::new(),
            });
        }
        for c in residual {
            self.conjunct(c, Site::on(id), &mut instrs)?;
        }
        // The conjuncts holding a subquery last (design E11), after the element's `as()`: the
        // hooked ones, then the fast-path `EXISTS` children.
        let mut exists = Vec::new();
        for c in hooked {
            self.conjunct(c, Site::on(id), &mut exists)?;
        }
        for &c in &filters.exists {
            self.conjunct(c, Site::on(id), &mut exists)?;
        }
        Ok((first, instrs, exists))
    }

    /// The hop of an expansion: the edge's labels, filters, join and trail checks (Phase 0 Q3,
    /// Q8) and its `as()` placeholder (none for an edge bound earlier, which is checked against
    /// its label instead).
    fn hop<S: GraphStorage>(&self, expand: &Expand) -> Result<Hop<S>, TraverserError> {
        let id = expand.edge;
        let mut checks = Vec::new();
        if expand.edge_join {
            if expand.quantifier.is_some() {
                // The binder refuses a group variable bound again; a guard only.
                return Err(self.unlowered());
            }
            checks.push(self.same(id)?);
        }
        // Trail (plan mechanism 5): a quantified edge checks its own earlier iterations and
        // every earlier edge of the clause with `gql:trail` (before its own `as()`, so it
        // compares with the earlier occurrences only); a single edge checks an earlier group
        // variable with `gql:trail` and an earlier single edge with `where(P.neq(..))`.
        let mut every = Vec::new();
        if expand.quantifier.is_some() {
            every.push(id);
        }
        for &other in &expand.trail {
            if expand.quantifier.is_some() || self.is_group(other) || self.is_conditional(other) {
                every.push(other);
            } else {
                checks.push(self.differs(other)?);
            }
        }
        if !every.is_empty() {
            checks.push(Instr::Trail(every));
        }
        let (labels, mut steps, exists) = if expand.edge_join {
            (None, checks, Vec::new())
        } else {
            self.element(id, &expand.edge_filters, checks)?
        };
        // An edge joined to a column binding has no label of its own: a path variable reads it
        // through one set here (emitted only then).
        if !expand.edge_join || self.is_column(id) {
            steps.push(Instr::Label(id));
        }
        steps.extend(exists);
        Ok(Hop {
            direction: expand.direction,
            labels,
            steps,
            quantifier: expand.quantifier,
            inner: expand.quantifier.map(|_| id),
        })
    }

    /// Whether `id` is a group variable (the edge of a quantified edge, a variable of a
    /// quantified parenthesized path pattern's body).
    fn is_group(&self, id: BindingId) -> bool {
        self.bound.binding(id).is_some_and(|b| b.group)
    }

    /// Whether `id` is a conditional singleton (see [`Lowering::conditional`]).
    fn is_conditional(&self, id: BindingId) -> bool {
        self.conditional.get(id.0).copied().unwrap_or(false)
    }

    /// Lowers a parenthesized path pattern (stage 2e, [`Op::Group`]) at `site`: back to `from`
    /// when the walk does not continue from the current element, then the body
    /// ([`Self::group_body`]) once without a quantifier, as `repeat(__.<body>)` with the
    /// modulators of a quantified edge otherwise (`?` is `{0,1}`: `emit().repeat(B).times(1)`;
    /// `{0}` no step, the far node is the near one), then the outer node `to` with its filters
    /// and `as()` (a join follows when it was bound earlier). No merge point: every iteration
    /// labels its edges, so traversers never merge there. Returns the site after it.
    #[inline(never)]
    fn group<S: GraphStorage>(
        &self,
        group: &GroupExpand,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        if !group.from_current {
            self.move_to(group.from, site, instrs)?;
        }
        let zero_only = group.quantifier.is_some_and(|q| q.max == Some(0));
        if !zero_only {
            let body = self.group_body(group)?;
            match group.quantifier {
                None => instrs.extend(body),
                Some(quantifier) => instrs.push(Instr::Repeat { body, quantifier }),
            }
        }
        if !group.to_bound {
            let (labels, steps, exists) = self.element(group.to, &group.to_filters, Vec::new())?;
            if let Some(labels) = labels {
                instrs.push(Instr::HasLabel(labels));
            }
            instrs.extend(steps);
            instrs.push(Instr::Label(group.to));
            instrs.extend(exists);
        }
        Ok(Site::on(group.to))
    }

    /// One iteration of a parenthesized path pattern's body, in walk order from the current
    /// element (design item 9): the body conditions over no element, `first`'s label factors,
    /// filters and `as()`; per hop the edge in its 1a form with its trail check before its
    /// `as()` (`gql:trail` of its own label and of every other edge it must differ from when
    /// the body repeats, or against a group variable or a conditional singleton; else
    /// `where(P.neq(..))`), the node reached with its filters and `as()`, then the body
    /// conditions whose last element in walk order is bound there (`gql:filter`, reading this
    /// iteration's elements: [`Site::body`]).
    fn group_body<S: GraphStorage>(
        &self,
        group: &GroupExpand,
    ) -> Result<Vec<Instr<S>>, TraverserError> {
        let repeats = group.quantifier.is_some() && !group.conditional;
        let mut body = Vec::new();
        for &c in &group.first_conjuncts {
            self.conjunct(c, Site::in_body(group.first), &mut body)?;
        }
        let (labels, steps, exists) =
            self.element(group.first, &group.first_filters, Vec::new())?;
        if let Some(labels) = labels {
            body.push(Instr::HasLabel(labels));
        }
        body.extend(steps);
        body.push(Instr::Label(group.first));
        body.extend(exists);
        for hop in &group.hops {
            let mut checks = Vec::new();
            let mut every = Vec::new();
            if repeats {
                every.push(hop.edge);
            }
            for &other in &hop.trail {
                if self.is_column(other) {
                    checks.push(self.differs(other)?);
                } else if repeats || self.is_group(other) || self.is_conditional(other) {
                    every.push(other);
                } else {
                    checks.push(self.differs(other)?);
                }
            }
            if !every.is_empty() {
                checks.push(Instr::Trail(every));
            }
            let (labels, mut steps, exists) = self.element(hop.edge, &hop.edge_filters, checks)?;
            steps.push(Instr::Label(hop.edge));
            steps.extend(exists);
            body.push(Instr::Hop(Hop {
                direction: hop.direction,
                labels,
                steps,
                quantifier: None,
                inner: None,
            }));
            let (labels, steps, exists) = self.element(hop.to, &hop.to_filters, Vec::new())?;
            if let Some(labels) = labels {
                body.push(Instr::HasLabel(labels));
            }
            body.extend(steps);
            body.push(Instr::Label(hop.to));
            body.extend(exists);
            for &c in &hop.filters {
                self.conjunct(c, Site::in_body(hop.to), &mut body)?;
            }
        }
        Ok(body)
    }

    /// The adjacency labels of an expansion's edge (the first label disjunction factor), as
    /// [`Self::hop`] emits them.
    fn edge_labels(&self, expand: &Expand) -> Option<Vec<String>> {
        if expand.edge_join {
            return None;
        }
        let labels = self.bound.binding(expand.edge)?.labels.as_slice();
        and_factors(labels).into_iter().find_map(label_disjuncts)
    }

    /// `gql:filter` of conjunct `c` at `site` (skipped when it is constantly TRUE).
    fn conjunct<S: GraphStorage>(
        &self,
        c: usize,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let Some(conjunct) = self.bound.conjuncts.get(c) else {
            return Ok(());
        };
        if let Predicate::Expr(expr) = &conjunct.predicate
            && let Some((span, negated)) = exists_conjunct(expr)
        {
            return self.exists(span, negated, site, instrs);
        }
        let (expr, this) = self.condition(conjunct);
        let hooks = Hooks::default();
        let condition: RowExpr<S> = self.compile_in(&expr, this, &[], Some(&hooks))?;
        if matches!(
            condition.as_const(),
            Some(Value::Scalar(ElementProperty::Boolean(true)))
        ) {
            return Ok(());
        }
        let subqueries = self.bodies(hooks, site)?;
        instrs.push(Instr::Filter {
            condition,
            site,
            subqueries,
        });
        Ok(())
    }

    /// The child of a top-level `[NOT] EXISTS` conjunct whose subquery is written at `span`,
    /// run at `site` (design M2, the 2b fast path): the body's plan for the host's current
    /// element, lowered from `site` like a later segment (its start `Op::Resume` from the
    /// current element needs no step, a start at another import is `select()` / `gql:bind`, an
    /// unanchored component a child-leading `v()`), without merge points (a `lazy_barrier()`
    /// would defeat the short circuit).
    fn exists<S: GraphStorage>(
        &self,
        span: Span,
        negated: bool,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let Some(k) = self.bound.subquery_at(span) else {
            return Err(self.unlowered());
        };
        let body = self.body(k, span, site)?;
        instrs.push(Instr::Exists { negated, body });
        Ok(())
    }

    /// The body of subquery `k` (written at `span`) run at `site`: its plan for the site's
    /// current element lowered from the site, without merge points of its match pipeline (a
    /// `lazy_barrier()` would defeat the short circuit); counted against
    /// [`MAX_LOWERED_SUBQUERIES`]. A whole-query body (stage 2d) is lowered like a `CALL` body
    /// ([`Self::composite`] from `site`). What closes it follows the kind: nothing for
    /// `EXISTS` (the host tests for a result), `count()` for `COUNT { }`, the `|` part of a
    /// pattern comprehension (`gql:value(expr)` where the body ends), the one returned column
    /// of `COLLECT { }` (`gql:value(column)` on the body's `RETURN` record).
    #[inline(never)]
    fn body<S: GraphStorage>(
        &self,
        k: usize,
        span: Span,
        site: Site,
    ) -> Result<Vec<Instr<S>>, TraverserError> {
        let (Some(body_plan), Some(sub)) = (
            self.plan.subquery_plan(self.bound, k, site.current),
            self.bound.subqueries.get(k),
        ) else {
            return Err(self.unlowered());
        };
        let lowered = self.children.get().saturating_add(1);
        if lowered > MAX_LOWERED_SUBQUERIES {
            return Err(self.too_many_children(span));
        }
        self.children.set(lowered);
        let mut body = Vec::new();
        let end = match (&body_plan.query, sub.query.as_deref()) {
            (Some(plan), Some(query)) => {
                self.composite(plan, query, Start::At(site), &mut body)?;
                None
            }
            (None, None) => Some(self.ops(&body_plan.ops, site, &mut body)?),
            _ => return Err(self.unlowered()),
        };
        body.retain(|instr| !matches!(instr, Instr::Merge { .. }));
        match (sub.kind, end, &sub.map, sub.query.as_deref()) {
            (SubqueryKind::Exists, ..) => {}
            (SubqueryKind::Count, ..) => body.push(Instr::Count),
            // A pattern comprehension ends with its `|` part, evaluated where the body ends.
            (SubqueryKind::Collect, Some(end), Some(map), None) => {
                let hooks = Hooks::default();
                let expr: RowExpr<S> = self.compile_in(map, None, &[], Some(&hooks))?;
                let subqueries = self.bodies(hooks, end)?;
                body.push(Instr::Value {
                    expr,
                    site: end,
                    subqueries,
                });
            }
            // `COLLECT { .. RETURN x }`: the one column of the `RETURN` record (the binder
            // checks there is exactly one; every operand of a `UNION` returns the same name).
            (SubqueryKind::Collect, None, None, Some(query)) => {
                let Some(column) = query.last().first.columns.first() else {
                    return Err(self.unlowered());
                };
                let written = column.expr.span;
                let read = Expr {
                    kind: ExprKind::Variable(Identifier {
                        name: String::new(),
                        span: written,
                    }),
                    span: written,
                };
                body.push(Instr::Collect(self.compile(&read, Some(0), &[])?));
            }
            (SubqueryKind::Collect, ..) => return Err(self.unlowered()),
        }
        Ok(body)
    }

    /// The error of a query that lowers more than [`MAX_LOWERED_SUBQUERIES`] subquery bodies.
    fn too_many_children(&self, span: Span) -> TraverserError {
        GqlErrorBuilder::new(
            GqlErrorKind::Limit,
            span.start..span.end,
            format!(
                "the query runs more than {MAX_LOWERED_SUBQUERIES} subquery bodies once its LET \
                 variables are substituted (each place that reads a LET holding EXISTS or a \
                 pattern comprehension runs its own copy), COUNT {{ }} and COLLECT {{ }} \
                 included"
            ),
        )
        .alternative(
            "write the EXISTS or the pattern comprehension where it is used instead of in a LET \
             that is read many times (the same holds for COUNT { } and COLLECT { }), or join \
             the patterns into fewer subqueries; a CALL (a) { .. } body is not copied: it runs \
             once per row, and later clauses read its columns, as in MATCH (p) CALL (p) { \
             OPTIONAL MATCH (p)-->(f) RETURN count(f) AS n } FILTER n > 0 RETURN p, n",
        )
        .topic(GqlHelpTopic::Subquery)
        .into_error(self.query)
    }

    /// The condition of a conjunct as an expression; an inline property `{k: v}` becomes
    /// `var.k = v` (over the reserved name when the element is anonymous).
    fn condition(&self, conjunct: &Conjunct) -> (Expr, Option<usize>) {
        match &conjunct.predicate {
            Predicate::Expr(expr) => (expr.clone(), None),
            Predicate::Property {
                binding,
                key,
                value,
            } => {
                let name = self
                    .names
                    .get(binding.0)
                    .cloned()
                    .flatten()
                    .unwrap_or_default();
                let this = name.is_empty().then_some(binding.0);
                let span = conjunct.span;
                let target = Expr {
                    kind: ExprKind::Variable(Identifier { name, span }),
                    span,
                };
                let property = Expr {
                    kind: ExprKind::Property {
                        target: Box::new(target),
                        key: key.clone(),
                    },
                    span,
                };
                let expr = Expr {
                    kind: ExprKind::Binary {
                        op: BinaryOp::Eq,
                        left: Box::new(property),
                        right: Box::new(value.clone()),
                    },
                    span,
                };
                (expr, this)
            }
        }
    }

    /// The `has(key, P)` of pushed conjunct `c`; `None` when a parameter's value does not fit
    /// the form (the conjunct then stays in `gql:filter`).
    fn pushed(&self, c: usize) -> Result<Option<(String, P<PValue>)>, TraverserError> {
        let Some(Pushdown { key, op, operand }) =
            self.plan.pushdown.get(c).and_then(Option::as_ref)
        else {
            return Ok(None);
        };
        let predicate = match (op, operand) {
            (PushOp::NotNull, _) => Some(P::Neq(PValue::Null)),
            (PushOp::In, PushOperand::List(items)) => items
                .iter()
                .map(literal_pvalue)
                .collect::<Option<Vec<_>>>()
                .map(P::WithIn),
            (PushOp::In, PushOperand::Parameter { name, .. }) => parameter(self.params, name)
                .and_then(list_pvalues)
                .map(P::WithIn),
            (op, PushOperand::Value(literal)) => {
                literal_pvalue(literal).and_then(|v| single(*op, v))
            }
            (op, PushOperand::Parameter { name, .. }) => parameter(self.params, name)
                .and_then(scalar_pvalue)
                .and_then(|v| single(*op, v)),
            (_, PushOperand::None | PushOperand::List(_)) => None,
        };
        Ok(predicate.map(|p| (key.clone(), p)))
    }

    /// A result pipeline (`RETURN`, or a materializing `WITH`) of `columns` at `site`; `result`
    /// is its index (the `WITH` segment, `segments.len()` for `RETURN`). Returns whether it
    /// already set its row label: an aggregating pipeline sets it before its `ORDER BY`
    /// (read only when a key or a body reads a column after leaving the row,
    /// [`Site::ordered`]).
    fn pipeline<S: GraphStorage>(
        &self,
        ops: &[ResultOp],
        columns: &[Column],
        order_by: &[SortKey],
        (site, result): (Site, usize),
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<bool, TraverserError> {
        let n = self.bound.bindings.len();
        let aggregated = ops.contains(&ResultOp::Aggregate);
        let mut row_labelled = false;
        for op in ops {
            match op {
                ResultOp::Project => {
                    let hooks = Hooks::default();
                    let mut compiled = Vec::with_capacity(columns.len());
                    for column in columns {
                        compiled.push(GqlColumn {
                            name: CompactString::from(column.name.as_str()),
                            expr: self.compile_in(&column.expr, None, &[], Some(&hooks))?,
                        });
                    }
                    instrs.push(Instr::Project {
                        columns: compiled,
                        site,
                        subqueries: self.bodies(hooks, site)?,
                    });
                }
                ResultOp::Aggregate => {
                    let hooks = Hooks::default();
                    let Aggregating { outputs, calls } = compile_aggregating(
                        columns.iter().map(|c| &c.expr),
                        &self.scope(None, &[], Some(&hooks)),
                    )
                    .map_err(|error| eval_error(self.query, error))?;
                    let columns = columns
                        .iter()
                        .zip(outputs)
                        .map(|(column, expr)| GqlAggregateColumn {
                            name: CompactString::from(column.name.as_str()),
                            expr,
                            key: !column.aggregate,
                        })
                        .collect();
                    instrs.push(Instr::Aggregate {
                        columns,
                        calls,
                        site,
                        subqueries: self.bodies(hooks, site)?,
                    });
                }
                ResultOp::Distinct => instrs.push(Instr::Distinct),
                ResultOp::Order => {
                    let hooks = Hooks::default();
                    let mut keys = Vec::with_capacity(order_by.len());
                    for key in order_by {
                        let expr = match key.column {
                            Some(i) => {
                                let reference = Expr {
                                    kind: ExprKind::Variable(Identifier {
                                        name: String::new(),
                                        span: key.expr.span,
                                    }),
                                    span: key.expr.span,
                                };
                                self.compile(&reference, Some(n + i), &[])?
                            }
                            None => self.sort_key(key, columns, n, &hooks)?,
                        };
                        keys.push(GqlSortKey {
                            expr,
                            descending: key.direction == SortDirection::Desc,
                        });
                    }
                    // The keys run on the row map: no current element; after an aggregation the
                    // bindings its columns hold are read from them (`Site::ordered`).
                    let site = if aggregated {
                        Site::ordered(result)
                    } else {
                        Site::default()
                    };
                    let mut subqueries = self.bodies(hooks, site)?;
                    if aggregated {
                        for body in &mut subqueries {
                            self.in_order(body, result)?;
                        }
                        instrs.push(Instr::RowLabel(result));
                        row_labelled = true;
                    }
                    instrs.push(Instr::Order {
                        keys,
                        columns: columns.len(),
                        site,
                        subqueries,
                    });
                }
                ResultOp::Offset(count) => instrs.push(Instr::Skip(self.count(count, "OFFSET")?)),
                ResultOp::Limit(count) => instrs.push(Instr::Limit(self.count(count, "LIMIT")?)),
            }
        }
        Ok(row_labelled)
    }

    /// Marks every site of `body` (a subquery body an `ORDER BY` key of the aggregating result
    /// pipeline `result` holds) with [`Site::ordered`], and reads the bindings that pipeline's
    /// columns hold from their columns instead of their path labels, which the aggregation
    /// dropped: `select()` becomes `gql:bind`, a join or trail check by label a row check.
    fn in_order<S: GraphStorage>(
        &self,
        body: &mut [Instr<S>],
        result: usize,
    ) -> Result<(), TraverserError> {
        let away = Site {
            ordered: Some(result),
            ..Site::default()
        };
        let held = |id: BindingId| self.ordered_column(result, id).is_some();
        for instr in body.iter_mut() {
            match instr {
                Instr::Filter {
                    site, subqueries, ..
                }
                | Instr::Project {
                    site, subqueries, ..
                }
                | Instr::For {
                    site, subqueries, ..
                }
                | Instr::Aggregate {
                    site, subqueries, ..
                }
                | Instr::Order {
                    site, subqueries, ..
                }
                | Instr::Value {
                    site, subqueries, ..
                }
                | Instr::InsertNode {
                    site, subqueries, ..
                }
                | Instr::InsertEdge {
                    site, subqueries, ..
                } => {
                    site.ordered = Some(result);
                    for nested in subqueries.iter_mut() {
                        self.in_order(nested, result)?;
                    }
                }
                Instr::Lookup { site, .. }
                | Instr::Bind { site, .. }
                | Instr::RowCheck(_, site) => site.ordered = Some(result),
                Instr::Hop(hop) => self.in_order(&mut hop.steps, result)?,
                Instr::Optional(part)
                | Instr::Exists { body: part, .. }
                | Instr::Call(part)
                | Instr::Repeat { body: part, .. } => {
                    self.in_order(part, result)?;
                }
                Instr::Union(operands) => {
                    for operand in operands.iter_mut() {
                        self.in_order(operand, result)?;
                    }
                }
                Instr::Select(id) if held(*id) => *instr = self.bind(*id, away)?,
                Instr::Same(id) if held(*id) => {
                    *instr = self.row_check(*id, BinaryOp::Eq)?;
                    self.in_order(std::slice::from_mut(instr), result)?;
                }
                Instr::Differs(id) if held(*id) => {
                    *instr = self.row_check(*id, BinaryOp::Ne)?;
                    self.in_order(std::slice::from_mut(instr), result)?;
                }
                Instr::Scan
                | Instr::Unit
                | Instr::HasLabel(_)
                | Instr::Has(..)
                | Instr::Label(_)
                | Instr::RowLabel(_)
                | Instr::Distinct
                | Instr::Skip(_)
                | Instr::Limit(_)
                | Instr::Select(_)
                | Instr::Same(_)
                | Instr::Differs(_)
                | Instr::Trail(_)
                | Instr::PartLabel(_)
                | Instr::Merge { .. }
                | Instr::Count
                | Instr::Collect(_)
                | Instr::Discard => {}
            }
        }
        Ok(())
    }

    /// The column of the aggregating result pipeline `result` that holds binding `id` (see
    /// [`Site::ordered`]).
    fn ordered_column(&self, result: usize, id: BindingId) -> Option<usize> {
        let columns = match self.bound.segments.get(result) {
            Some(Segment::With(with)) => &with.columns,
            Some(_) => return None,
            None => self.return_columns(result)?,
        };
        columns.iter().position(|c| c.variable == Some(id))
    }

    /// The column variables of each result pipeline ([`Lowered::ordered`]).
    fn ordered_tables(&self) -> Vec<Vec<Option<BindingId>>> {
        self.bound
            .segments
            .iter()
            .map(|segment| match segment {
                Segment::With(with) => with.columns.iter().map(|c| c.variable).collect(),
                Segment::Match(_)
                | Segment::Optional(_)
                | Segment::For(_)
                | Segment::Call(_)
                | Segment::Rows(_)
                | Segment::Write(_) => Vec::new(),
            })
            .chain(
                self.linears
                    .iter()
                    .map(|l| l.columns.iter().map(|c| c.variable).collect()),
            )
            .collect()
    }

    /// An `ORDER BY` key that is not a whole column. An identifier the key names as a column
    /// alias reads the column (slot `n + column`); every other variable is a binding, also one
    /// whose key equals an alias (a substituted `LET` / `WITH` name standing for a variable,
    /// T2 note): the binder's alias references are the identifiers of
    /// [`SortKey::aliases`] written inside the key, which are renamed to names no binding has.
    fn sort_key<S: GraphStorage>(
        &self,
        key: &SortKey,
        columns: &[Column],
        n: usize,
        hooks: &Hooks,
    ) -> Result<RowExpr<S>, TraverserError> {
        if key.aliases.is_empty() {
            return self.compile_in(&key.expr, None, &[], Some(hooks));
        }
        let mut renames: Vec<(&str, String)> = Vec::with_capacity(key.aliases.len());
        let mut aliases: Vec<(String, usize)> = Vec::with_capacity(key.aliases.len());
        for &i in &key.aliases {
            let Some(column) = columns.get(i) else {
                continue;
            };
            let mut reserved = format!("\u{0}{i}");
            while self
                .names
                .iter()
                .any(|n| n.as_deref() == Some(reserved.as_str()))
            {
                reserved.insert(0, '\u{0}');
            }
            renames.push((column.name.as_str(), reserved.clone()));
            aliases.push((reserved, n + i));
        }
        let mut expr = key.expr.clone();
        rename_aliases(&mut expr, key.span, &renames);
        self.compile_in(&expr, None, &aliases, Some(hooks))
    }

    /// The value of `OFFSET` / `LIMIT`: a literal, or a parameter whose value must be a
    /// non-negative integer (GQLSTATUS `22G02` negative, `22G03` not an integer).
    fn count(&self, count: &Count, keyword: &str) -> Result<usize, TraverserError> {
        let (name, span) = match count {
            Count::Literal(n) => return Ok(usize::try_from(*n).unwrap_or(usize::MAX)),
            Count::Parameter { name, span } => (name, span),
        };
        let refused = |code: &'static str, what: &str| {
            GqlErrorBuilder::new(
                GqlErrorKind::Semantic,
                span.start..span.end,
                format!("{keyword} must be a non-negative integer, but ${name} is {what}"),
            )
            .gqlstatus(code)
            .alternative(format!(
                "give ${name} a value such as 10, as in g.gql({}, #{{{}: 10}})",
                crate::value::literal::rhai_string(self.query),
                rhai_map_key(name)
            ))
            .into_error(self.query)
        };
        match parameter(self.params, name).map(|v| v.clone().into_logical()) {
            Some(ElementProperty::Number(number)) => match number.as_i64() {
                Some(value) => match u64::try_from(value) {
                    Ok(value) => Ok(usize::try_from(value).unwrap_or(usize::MAX)),
                    Err(_) => Err(refused("22G02", "negative")),
                },
                None => Err(refused("22G03", "a float")),
            },
            Some(ElementProperty::Null) | None => Err(refused("22G03", "null")),
            Some(_) => Err(refused("22G03", "not a number")),
        }
    }

    /// Which bindings need `as()` (module docs): a reason of the plan other than `read`, or a
    /// read away from the current element; and which materializing `WITH` rows need their row
    /// label: the plan says so, or a step reads a column after the row was left.
    fn labelled<S: GraphStorage>(
        &self,
        instrs: &[Instr<S>],
        columns: &[Option<ColumnRef>],
        ordered: &[Vec<Option<BindingId>>],
    ) -> (Vec<bool>, Vec<bool>) {
        let n = self.bound.bindings.len();
        let mut labelled: Vec<bool> = self
            .plan
            .labelling
            .iter()
            .map(|l| {
                l.join
                    || l.trail
                    || l.branch
                    || l.resume
                    || l.optional
                    || l.group
                    || l.path
                    || l.body
            })
            .collect();
        labelled.resize(n, false);
        // One more per `RETURN` row (`Site::ordered`).
        let mut rows = vec![false; self.bound.segments.len() + self.linears.len()];
        for stage in self.pipelines().into_iter().flat_map(|p| &p.stages) {
            let (segment, labelled) = match stage {
                Stage::Materialize(m) => (m.segment, m.labelled),
                Stage::For(f) => (f.segment, f.labelled),
                Stage::Call(c) => (c.segment, c.labelled),
                Stage::Rows(r) => (r.segment, r.labelled),
                Stage::Match { .. } | Stage::Optional { .. } | Stage::Write(_) => continue,
            };
            if labelled && let Some(flag) = rows.get_mut(segment) {
                *flag = true;
            }
        }
        mark_reads(
            instrs,
            &mut Marks {
                columns,
                ordered,
                labelled: &mut labelled,
                rows: &mut rows,
            },
        );
        (labelled, rows)
    }

    /// How each group variable is read ([`GroupRead`]), the part label of each
    /// `OPTIONAL MATCH` segment that needs one, and the anchor of each segment (`None`: none
    /// needed). A group or path variable of an `OPTIONAL MATCH` part that is read anywhere is
    /// anchored: on the part's witness node (labelled for it), or, when the part binds no new
    /// node (`OPTIONAL MATCH (a)-[e]->{0,2}(a)`), on the part label `#o<k>` set at the end of
    /// the part. Without the anchor a part not matched (`NULL`) and a zero-length match (`[]`)
    /// would read alike, and a path over elements bound before the part would read as matched.
    fn groups(&self, labels: &[CompactString], labelled: &mut [bool]) -> GroupTables {
        let mut part_labels = vec![CompactString::default(); self.bound.segments.len()];
        let mut anchors: Vec<Option<CompactString>> = vec![None; self.bound.segments.len()];
        let pipelines = self.pipelines();
        for stage in pipelines.iter().flat_map(|p| &p.stages) {
            let Stage::Optional {
                segment, witness, ..
            } = stage
            else {
                continue;
            };
            let read = self.bound.bindings.iter().any(|b| {
                (b.group || b.kind == BindingKind::Path)
                    && b.segment == *segment
                    && self.plan.labelling.get(b.id.0).is_some_and(|l| l.read)
            });
            if !read {
                continue;
            }
            let anchor = match witness {
                Some(w) => {
                    if let Some(flag) = labelled.get_mut(w.0) {
                        *flag = true;
                    }
                    labels.get(w.0).cloned()
                }
                None => {
                    let mut label = format!("#o{segment}");
                    while labels.iter().any(|l| l.as_str() == label) {
                        label.insert(0, '#');
                    }
                    let label = CompactString::from(label);
                    if let Some(slot) = part_labels.get_mut(*segment) {
                        *slot = label.clone();
                    }
                    Some(label)
                }
            };
            if let Some(slot) = anchors.get_mut(*segment) {
                *slot = anchor;
            }
        }
        let mut reversed = vec![false; self.bound.bindings.len()];
        let stage_ops = pipelines.iter().flat_map(|p| {
            std::iter::once(p.ops.as_slice()).chain(p.stages.iter().filter_map(
                |stage| match stage {
                    Stage::Match { ops, .. } | Stage::Optional { ops, .. } => Some(ops.as_slice()),
                    Stage::Materialize(_)
                    | Stage::For(_)
                    | Stage::Call(_)
                    | Stage::Rows(_)
                    | Stage::Write(_) => None,
                },
            ))
        });
        for op in stage_ops.flatten() {
            if let Op::Expand(e) = op
                && e.quantifier.is_some()
                && let Some(flag) = reversed.get_mut(e.edge.0)
            {
                *flag = e.reversed;
            }
            // Every variable of a parenthesized path pattern's body: its iterations were walked
            // from the node after the pattern (stage 2e).
            if let Op::Group(g) = op {
                let body =
                    std::iter::once(g.first).chain(g.hops.iter().flat_map(|h| [h.edge, h.to]));
                for id in body {
                    if let Some(flag) = reversed.get_mut(id.0) {
                        *flag = g.reversed;
                    }
                }
            }
        }
        let groups = self
            .bound
            .bindings
            .iter()
            .map(|b| {
                b.group.then(|| GroupRead {
                    segment: b.segment,
                    anchor: anchors.get(b.segment).cloned().flatten(),
                    reversed: reversed.get(b.id.0).copied().unwrap_or(false),
                })
            })
            .collect();
        GroupTables {
            groups,
            part_labels,
            anchors,
            reversed,
        }
    }

    /// The inner label of each group variable a read path variable passes through
    /// (`#gv<id>`, more `#` in front while it collides with a binding's label); empty for the
    /// others. The planner marks every element of a read path's pattern ([`Labelling::path`]).
    ///
    /// [`Labelling::path`]: super::plan::Labelling
    fn inner_labels(&self, labels: &[CompactString]) -> Vec<CompactString> {
        self.bound
            .bindings
            .iter()
            .map(|b| {
                let path = self.plan.labelling.get(b.id.0).is_some_and(|l| l.path);
                if !(b.group && path) {
                    return CompactString::default();
                }
                let mut label = format!("#gv{}", b.id.0);
                while labels.iter().any(|l| l.as_str() == label) {
                    label.insert(0, '#');
                }
                CompactString::from(label)
            })
            .collect()
    }

    /// How each path variable is assembled ([`PathRead`]): its pattern's nodes and edges in
    /// text order, a quantified edge as a group part (reversed when walked from its far end),
    /// anchored like a group variable when it is bound in an `OPTIONAL MATCH` part.
    fn paths(
        &self,
        labels: &[CompactString],
        inner_labels: &[CompactString],
        reversed: &[bool],
        anchors: &[Option<CompactString>],
    ) -> Vec<Option<PathRead>> {
        let mut paths: Vec<Option<PathRead>> = vec![None; self.bound.bindings.len()];
        let label = |id: BindingId| labels.get(id.0).cloned().unwrap_or_default();
        let walked_back = |id: BindingId| reversed.get(id.0).copied().unwrap_or(false);
        let step_parts = |parts: &mut Vec<PathPart>, steps: &[BoundStep]| {
            for step in steps {
                if step.quantifier.is_some() {
                    parts.push(PathPart::Group {
                        edges: label(step.edge),
                        vertices: inner_labels.get(step.edge.0).cloned().unwrap_or_default(),
                        reversed: walked_back(step.edge),
                    });
                } else {
                    parts.push(PathPart::Element(label(step.edge)));
                }
                parts.push(PathPart::Element(label(step.node)));
            }
        };
        for path in self.bound.matches.iter().flat_map(|m| &m.paths) {
            let Some(variable) = path.variable else {
                continue;
            };
            let Some(binding) = self.bound.binding(variable) else {
                continue;
            };
            let mut parts = Vec::with_capacity(2 * path.steps.len() + 1 + 3 * path.groups.len());
            parts.push(PathPart::Element(label(path.start)));
            step_parts(&mut parts, &path.steps);
            // A parenthesized path pattern (stage 2e): its iterations from the labels of its
            // body's elements after the first node (which is the node before it, merge rule),
            // then the node after it (the last iteration's last node).
            for group in &path.groups {
                let body = &group.group;
                parts.push(PathPart::Body {
                    labels: body
                        .steps
                        .iter()
                        .flat_map(|s| [label(s.edge), label(s.node)])
                        .collect(),
                    reversed: walked_back(body.start),
                });
                parts.push(PathPart::Element(label(group.node)));
                step_parts(&mut parts, &group.steps);
            }
            if let Some(slot) = paths.get_mut(variable.0) {
                *slot = Some(PathRead {
                    segment: binding.segment,
                    parts,
                    anchor: anchors.get(binding.segment).cloned().flatten(),
                });
            }
        }
        paths
    }

    /// The path label of each binding: its key (the variable name, or `#<id>` for a name bound
    /// again after a `WITH` dropped it, decision C14); an anonymous binding `#<id>`
    /// (no plain identifier starts with `#`), with more `#` in front while it collides with a
    /// back-quoted variable name.
    fn labels(&self) -> Vec<CompactString> {
        self.names
            .iter()
            .enumerate()
            .map(|(id, name)| match name {
                Some(name) => CompactString::from(name.as_str()),
                None => {
                    let mut label = format!("#{id}");
                    while self
                        .names
                        .iter()
                        .any(|n| n.as_deref() == Some(label.as_str()))
                    {
                        label.insert(0, '#');
                    }
                    CompactString::from(label)
                }
            })
            .collect()
    }

    /// The row label of each materializing `WITH` segment, `#w<k>`, of each `FOR` with an
    /// ordinal, `#f<k>`, of each `CALL`, `#c<k>`, and of each [`Stage::Rows`], `#n<k>` (more
    /// `#` in front while it collides with a binding's label); of a `FOR` without an ordinal
    /// its item's own label (the row is the item); empty for the other segments.
    fn row_labels(&self, labels: &[CompactString]) -> Vec<CompactString> {
        self.bound
            .segments
            .iter()
            .enumerate()
            .map(|(k, segment)| {
                let kind = match segment {
                    Segment::For(f) if f.ordinal.is_none() => {
                        return labels.get(f.variable.0).cloned().unwrap_or_default();
                    }
                    Segment::With(_) => 'w',
                    Segment::For(_) => 'f',
                    Segment::Call(_) => 'c',
                    Segment::Rows(_) => 'n',
                    Segment::Match(_) | Segment::Optional(_) | Segment::Write(_) => {
                        return CompactString::default();
                    }
                };
                let mut label = format!("#{kind}{k}");
                while labels.iter().any(|l| l.as_str() == label) {
                    label.insert(0, '#');
                }
                CompactString::from(label)
            })
            .chain((0..self.linears.len()).map(|p| {
                // `#r` for the first linear query (the query when it is linear), `#r<p>` for the
                // others (stage 2d).
                let mut label = if p == 0 {
                    String::from("#r")
                } else {
                    format!("#r{p}")
                };
                while labels.iter().any(|l| l.as_str() == label) {
                    label.insert(0, '#');
                }
                CompactString::from(label)
            }))
            .collect()
    }
}

/// Every linear query of `bound` ([`Lowering::linears`]): the top-level ones in text order,
/// then those of each `CALL` body (every `Segment::Call`, nested ones included, since their
/// segments are in the one table), then those of whole-query subquery bodies.
fn linears(bound: &BoundQuery) -> Vec<&BoundLinear> {
    let calls = bound.segments.iter().filter_map(|segment| match segment {
        Segment::Call(call) => Some(&*call.body),
        Segment::Match(_)
        | Segment::Optional(_)
        | Segment::With(_)
        | Segment::For(_)
        | Segment::Rows(_)
        | Segment::Write(_) => None,
    });
    let bodies = bound.subqueries.iter().filter_map(|s| s.query.as_deref());
    std::iter::once(&bound.body)
        .chain(calls)
        .chain(bodies)
        .flat_map(BoundComposite::linears)
        .collect()
}

/// Per binding of `bound`: a variable of a parenthesized path pattern's body quantified by `?`
/// ([`Lowering::conditional`]).
fn conditional_bindings(bound: &BoundQuery) -> Vec<bool> {
    let mut conditional = vec![false; bound.bindings.len()];
    let groups = bound
        .matches
        .iter()
        .flat_map(|m| &m.paths)
        .flat_map(|path| &path.groups)
        .filter(|g| g.group.conditional);
    for group in groups {
        let body = std::iter::once(group.group.start)
            .chain(group.group.steps.iter().flat_map(|s| [s.edge, s.node]));
        for id in body {
            if let Some(flag) = conditional.get_mut(id.0) {
                *flag = true;
            }
        }
    }
    conditional
}

/// `this IS LABELED <factor>` over the reserved name.
fn is_labeled(factor: &LabelExpr) -> Expr {
    let span: Span = factor.span;
    Expr {
        kind: ExprKind::IsLabeled {
            operand: Box::new(Expr {
                kind: ExprKind::Variable(Identifier {
                    name: String::new(),
                    span,
                }),
                span,
            }),
            labels: factor.clone(),
            negated: false,
        },
        span,
    }
}

/// The predicate of a single-operand form.
fn single(op: PushOp, value: PValue) -> Option<P<PValue>> {
    let text = matches!(value, PValue::String(_));
    Some(match op {
        PushOp::Eq => P::Eq(value),
        PushOp::Lt => P::Lt(value),
        PushOp::Le => P::Lte(value),
        PushOp::Gt => P::Gt(value),
        PushOp::Ge => P::Gte(value),
        PushOp::StartsWith if text => P::StartingWith(value),
        PushOp::EndsWith if text => P::EndingWith(value),
        PushOp::Contains if text => P::Containing(value),
        PushOp::StartsWith | PushOp::EndsWith | PushOp::Contains | PushOp::In | PushOp::NotNull => {
            return None;
        }
    })
}

/// A pushable literal: a string, a number (never `NaN`) or a boolean.
fn literal_pvalue(literal: &Literal) -> Option<PValue> {
    match literal {
        Literal::Null => None,
        Literal::Bool(flag) => Some(PValue::Boolean(*flag)),
        Literal::Integer(value) => Some(PValue::Number(Number::from_i64(*value))),
        Literal::Float(value) if value.is_nan() => None,
        Literal::Float(value) => Some(PValue::Number(Number::from_f64(*value))),
        Literal::String(text) => Some(PValue::String(CompactString::from(text.as_str()))),
    }
}

/// A pushable parameter value: a string, a number (never `NaN`) or a boolean (Phase 0 Q5
/// verified exactly these kinds; others stay in `gql:filter`).
fn scalar_pvalue(value: &ElementProperty) -> Option<PValue> {
    match value.clone().into_logical() {
        ElementProperty::String(text) => Some(PValue::String(text)),
        ElementProperty::Number(number) if number.as_f64().is_some_and(f64::is_nan) => None,
        ElementProperty::Number(number) => Some(PValue::Number(number)),
        ElementProperty::Boolean(flag) => Some(PValue::Boolean(flag)),
        _ => None,
    }
}

/// The elements of a list parameter for `IN`, `NULL`s dropped; `None` when it is not a list
/// or holds a value that is not pushable.
fn list_pvalues(value: &ElementProperty) -> Option<Vec<PValue>> {
    let ElementProperty::Array(items) = value.clone().into_logical() else {
        return None;
    };
    items
        .iter()
        .filter(|item| !matches!(item, ElementProperty::Null))
        .map(scalar_pvalue)
        .collect()
}

/// What [`mark_reads`] records.
struct Marks<'a> {
    /// The materializing `WITH` column each binding stands for.
    columns: &'a [Option<ColumnRef>],
    /// The column variables of each result pipeline ([`Lowered::ordered`]).
    ordered: &'a [Vec<Option<BindingId>>],
    /// Per binding: read away from its element (needs `as()`).
    labelled: &'a mut [bool],
    /// Per segment: its row is read after it was left (needs `as("#w<k>")`).
    rows: &'a mut [bool],
}

impl Marks<'_> {
    /// Records the reads of `slots` at `site` and clears them. Slots past the bindings (the
    /// columns of `ORDER BY`, the current element of a row check) read no label.
    fn read(&mut self, slots: &mut Vec<usize>, site: Site) {
        for slot in slots.drain(..) {
            if ordered_column(self.ordered, site, slot).is_some() {
                // Read from the aggregating projection's row: through its label away from it.
                if let Some(result) = site.ordered
                    && site.row != Some(result)
                    && let Some(flag) = self.rows.get_mut(result)
                {
                    *flag = true;
                }
                continue;
            }
            if site.current == Some(BindingId(slot)) {
                continue;
            }
            match self.columns.get(slot) {
                Some(Some(column)) => {
                    if site.row != Some(column.segment)
                        && let Some(flag) = self.rows.get_mut(column.segment)
                    {
                        *flag = true;
                    }
                }
                Some(None) => {
                    if let Some(flag) = self.labelled.get_mut(slot) {
                        *flag = true;
                    }
                }
                None => {}
            }
        }
    }
}

/// Marks every binding an expression of `instrs` reads while it is not the current element, and
/// every materializing `WITH` row a step reads while it is not the current object.
fn mark_reads<S: GraphStorage>(instrs: &[Instr<S>], marks: &mut Marks<'_>) {
    let mut slots = Vec::new();
    for instr in instrs {
        match instr {
            Instr::Filter {
                condition,
                site,
                subqueries,
            } => {
                condition.variables(&mut slots);
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::RowCheck(condition, site) => {
                condition.variables(&mut slots);
                marks.read(&mut slots, Site::away(*site));
            }
            Instr::Project {
                columns,
                site,
                subqueries,
            } => {
                for column in columns {
                    column.expr.variables(&mut slots);
                }
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::For {
                list,
                site,
                subqueries,
                ..
            } => {
                list.variables(&mut slots);
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::Lookup { lookup, site } => {
                lookup.value.variables(&mut slots);
                marks.read(&mut slots, *site);
            }
            Instr::Aggregate {
                columns,
                calls,
                site,
                subqueries,
            } => {
                for column in columns {
                    column.expr.variables(&mut slots);
                }
                for argument in calls.iter().filter_map(|call| call.argument.as_ref()) {
                    argument.variables(&mut slots);
                }
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::Order {
                keys,
                site,
                subqueries,
                ..
            } => {
                for key in keys {
                    key.expr.variables(&mut slots);
                }
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::Bind { id, site, .. } => {
                slots.push(id.0);
                marks.read(&mut slots, *site);
            }
            Instr::Value {
                expr,
                site,
                subqueries,
            } => {
                expr.variables(&mut slots);
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::InsertNode {
                properties,
                site,
                subqueries,
                ..
            } => {
                for property in properties {
                    property.value.variables(&mut slots);
                }
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::InsertEdge {
                from,
                to,
                properties,
                site,
                subqueries,
                ..
            } => {
                for property in properties {
                    property.value.variables(&mut slots);
                }
                slots.extend([from.id.0, to.id.0]);
                marks.read(&mut slots, *site);
                subqueries.iter().for_each(|body| mark_reads(body, marks));
            }
            Instr::Hop(hop) => mark_reads(&hop.steps, marks),
            Instr::Optional(part)
            | Instr::Exists { body: part, .. }
            | Instr::Call(part)
            | Instr::Repeat { body: part, .. } => {
                mark_reads(part, marks);
            }
            Instr::Union(operands) => operands.iter().for_each(|o| mark_reads(o, marks)),
            Instr::Scan
            | Instr::Unit
            | Instr::HasLabel(_)
            | Instr::Has(..)
            | Instr::Label(_)
            | Instr::RowLabel(_)
            | Instr::Distinct
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Select(_)
            | Instr::Same(_)
            | Instr::Differs(_)
            | Instr::Trail(_)
            | Instr::PartLabel(_)
            | Instr::Merge { .. }
            | Instr::Count
            | Instr::Collect(_)
            | Instr::Discard => {}
        }
    }
}

/// Marks every site of `instrs` (the part of `OPTIONAL MATCH` segment `segment`) as inside
/// that part ([`Site::part`]).
fn in_part<S: GraphStorage>(instrs: &mut [Instr<S>], segment: usize) {
    for instr in instrs {
        match instr {
            Instr::Filter {
                site, subqueries, ..
            }
            | Instr::Project {
                site, subqueries, ..
            }
            | Instr::For {
                site, subqueries, ..
            }
            | Instr::Aggregate {
                site, subqueries, ..
            }
            | Instr::Value {
                site, subqueries, ..
            }
            | Instr::InsertNode {
                site, subqueries, ..
            }
            | Instr::InsertEdge {
                site, subqueries, ..
            } => {
                site.part = Some(segment);
                subqueries
                    .iter_mut()
                    .for_each(|body| in_part(body, segment));
            }
            Instr::Order { subqueries, .. } => subqueries
                .iter_mut()
                .for_each(|body| in_part(body, segment)),
            Instr::Lookup { site, .. } | Instr::Bind { site, .. } => site.part = Some(segment),
            Instr::Hop(hop) => in_part(&mut hop.steps, segment),
            Instr::Optional(part)
            | Instr::Exists { body: part, .. }
            | Instr::Call(part)
            | Instr::Repeat { body: part, .. } => {
                in_part(part, segment);
            }
            Instr::Union(operands) => operands.iter_mut().for_each(|o| in_part(o, segment)),
            Instr::Scan
            | Instr::Unit
            | Instr::HasLabel(_)
            | Instr::Has(..)
            | Instr::RowCheck(..)
            | Instr::Label(_)
            | Instr::RowLabel(_)
            | Instr::Distinct
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Select(_)
            | Instr::Same(_)
            | Instr::Differs(_)
            | Instr::Trail(_)
            | Instr::PartLabel(_)
            | Instr::Merge { .. }
            | Instr::Count
            | Instr::Collect(_)
            | Instr::Discard => {}
        }
    }
}

/// The column of the aggregating result pipeline `site` serves the `ORDER BY` of
/// ([`Site::ordered`]) that holds binding `slot`; `None` elsewhere.
fn ordered_column(ordered: &[Vec<Option<BindingId>>], site: Site, slot: usize) -> Option<usize> {
    ordered
        .get(site.ordered?)?
        .iter()
        .position(|v| *v == Some(BindingId(slot)))
}

/// The next expansion after `ops[at]` when it continues from the vertex `ops[at]` reached
/// (only join checks and filters between them): the place of a merge point (Phase 0 outcome 1).
fn next_hop(ops: &[Op], at: usize) -> Option<&Expand> {
    for op in ops.get(at + 1..)? {
        match op {
            Op::Join { .. } | Op::Filter(_) => {}
            Op::Expand(next) if next.from_current => return Some(next),
            Op::Expand(_)
            | Op::Group(_)
            | Op::Scan(_)
            | Op::Lookup(_)
            | Op::Anchor(_)
            | Op::Resume(_) => {
                return None;
            }
        }
    }
    None
}

/// Renames the alias references of an `ORDER BY` key: every variable written inside `within`
/// (the key's text) whose name is a renamed alias. A variable outside it comes from a
/// substituted `LET` / `WITH` definition and stays a binding. Recursion is bounded by the
/// expression height (`MAX_HEIGHT`, kept by substitution).
fn rename_aliases(expr: &mut Expr, within: Span, renames: &[(&str, String)]) {
    match &mut expr.kind {
        ExprKind::Variable(var) => {
            let inside = var.span.start >= within.start && var.span.end <= within.end;
            if inside && let Some((_, reserved)) = renames.iter().find(|(n, _)| *n == var.name) {
                var.name = reserved.clone();
            }
        }
        ExprKind::Literal(_) | ExprKind::Parameter(_) => {}
        ExprKind::List(items) => {
            for item in items {
                rename_aliases(item, within, renames);
            }
        }
        ExprKind::Map(map) => {
            for entry in &mut map.entries {
                rename_aliases(&mut entry.value, within, renames);
            }
        }
        ExprKind::Property { target, .. } => rename_aliases(target, within, renames),
        ExprKind::Unary { operand, .. }
        | ExprKind::IsNull { operand, .. }
        | ExprKind::IsLabeled { operand, .. } => rename_aliases(operand, within, renames),
        ExprKind::Binary { left, right, .. }
        | ExprKind::IsEndpoint {
            operand: left,
            edge: right,
            ..
        } => {
            rename_aliases(left, within, renames);
            rename_aliases(right, within, renames);
        }
        ExprKind::Case(case) => {
            if let Some(operand) = &mut case.operand {
                rename_aliases(operand, within, renames);
            }
            for arm in &mut case.whens {
                rename_aliases(&mut arm.when, within, renames);
                rename_aliases(&mut arm.then, within, renames);
            }
            if let Some(else_result) = &mut case.else_result {
                rename_aliases(else_result, within, renames);
            }
        }
        ExprKind::FunctionCall { args, .. } => match args {
            CallArgs::Star(_) => {}
            CallArgs::Exprs(args) => {
                for arg in args {
                    rename_aliases(arg, within, renames);
                }
            }
            CallArgs::Cast { operand, .. } => rename_aliases(operand, within, renames),
            CallArgs::Trim {
                character, source, ..
            } => {
                if let Some(character) = character {
                    rename_aliases(character, within, renames);
                }
                rename_aliases(source, within, renames);
            }
        },
        ExprKind::MapProjection(projection) => {
            rename_aliases(&mut projection.target, within, renames);
            for item in &mut projection.items {
                match item {
                    ProjectionItem::Entry(entry) => {
                        rename_aliases(&mut entry.value, within, renames);
                    }
                    // `x` of an alias: the key stays the written name, the value reads the
                    // renamed alias.
                    ProjectionItem::Variable(var) => {
                        let inside = var.span.start >= within.start && var.span.end <= within.end;
                        if inside
                            && let Some((_, reserved)) =
                                renames.iter().find(|(n, _)| *n == var.name)
                        {
                            let value = Expr {
                                kind: ExprKind::Variable(Identifier {
                                    name: reserved.clone(),
                                    span: var.span,
                                }),
                                span: var.span,
                            };
                            *item = ProjectionItem::Entry(MapEntry {
                                key: var.clone(),
                                value,
                            });
                        }
                    }
                    ProjectionItem::Property(_) | ProjectionItem::AllProperties(_) => {}
                }
            }
        }
        ExprKind::ListComprehension(comprehension) => {
            if let Some(init) = comprehension.init_mut() {
                rename_aliases(init, within, renames);
            }
            rename_aliases(&mut comprehension.list, within, renames);
            if let Some(filter) = &mut comprehension.filter {
                rename_aliases(filter, within, renames);
            }
            if let Some(map) = &mut comprehension.map {
                rename_aliases(map, within, renames);
            }
        }
        // A subquery body (a pattern comprehension too) is a scope of its own, bound and
        // substituted by the binder (`BoundQuery::subqueries`); an alias never resolves inside
        // it.
        ExprKind::Subquery(_) | ExprKind::PatternComprehension(_) => {}
    }
}
