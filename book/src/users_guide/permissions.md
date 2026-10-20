# Permissions

Graphersal is an in-process, in-memory library: the program that holds the graph usually also
writes the queries. **Everything is therefore allowed by default**, in the Rust API and in the Rhai
script entry alike. A host that runs input it does not trust (a web form, a multi-tenant query
service, a pasted snippet, an LLM) plugs in an **authorizer**: a
`graphersal::auth::Authorizer` that is asked for every request a query makes, before the query
runs.

Resource use is a separate concern, bounded by [Resource Limits](resource_limits.md) and
[Query Limits](query_limits.md).

## Requests: an action on a resource

Every step and every file or schema operation of a query makes one or more
`AccessRequest`s, an `Action` on a `Resource`:

| Action | |
|---|---|
| `Read` | read data, the schema, a catalog definition or a file |
| `Create` | create elements, write a file |
| `Update` | change elements (properties, labels), the schema or an option |
| `Delete` | drop elements or properties |
| `Execute` | run code: Rhai's `import "module"`, calling a saved query, running an embedder |
| `Custom(name)` | an action of an embedding server, never produced by the library |

| Resource | |
|---|---|
| `Data { element, label }` | graph data; `element` (`Vertex`/`Edge`) and `label` are set when the step knows them statically from the plan, `None` otherwise |
| `Schema` | the graph's schema |
| `Definition { kind, name }` | a definition of the graph's catalog (a saved query, ...); `kind` and `name` are `None` for a request about all of them (listing) |
| `File { path }` | a file, with the path as the script wrote it |
| `Option { key }` | an administrative `g.with()` option ([below](#administrative-gwith-options)) |
| `Embedder { name }` | an embedder of the registry the host gave its traversals, by its registry name (`"hash-ngram/1"`; [Vectors](vectors.md#embeddings)) |
| `Custom { kind, name }` | a resource of an embedding server (a database, a tenant, ...), never produced by the library |

What each step requests is decided in one place in the engine (`GraphStep::for_each_access`):

| Query | Requests |
|---|---|
| `V()` / `out_v()`, `in_v()`, `both_v()`, `other_v()` | `Read` on `Data(vertex)` |
| `E()` / `out_e()`, `in_e()`, `both_e()` | `Read` on `Data(edge)` |
| `out()`, `in()`, `both()` | `Read` on `Data(edge)` and on `Data(vertex)` |
| `subgraph("sg")` | `Read` on `Data(vertex)` (it copies the endpoints of the edges it receives) |
| `inject(v)` of a vertex / an edge value | `Read` on `Data(vertex)` / `Data(edge)` (it emits the stored element of that id) |
| every other step (`has()`, `values()`, `limit()`, `count()`, `inject()` of plain values, `constant()`, `math()`, `as()`, `by()`, ...) | nothing |
| `addV("person")`, `addV(["a", "b"])` | `Create` on `Data(vertex "person")`, one request per label (`Data(vertex)` without a static label) |
| `addE("knows")` | `Create` on `Data(edge "knows")` |
| `g.gql("INSERT (:person {name: 'ann'})-[:knows]->(:person)")` | `Create` on `Data(vertex "person")` per new node and label, `Create` on `Data(edge "knows")` per new edge, and nothing for the properties (`addV("person").property(..)` also asks `Update`, see [Writing with GQL](../gql/writes.md#permissions)); the `MATCH` before an `INSERT` asks its reads like its Gremlin twin |
| `g.gql("MATCH (a)-[e:knows]->(b) SET e.seen = true, b:known")`, `REMOVE ..` | `Update` on `Data(vertex)` for a node target (any label, as for `addLabel()`), on `Data(edge "knows")` for an edge whose pattern names its labels, on `Data` when the kind is known only at run time (a `FOR` item); the same as the Gremlin twins `property()`, `removeProperty()`, `addLabel()`, `dropLabel()` |
| `mergeV(..)` / `mergeE(..)` | `Create` and `Update` on `Data(vertex)` / `Data(edge)` |
| `property(..)`, `removeProperty(..)` | `Update` on the `Data` the steps before it state (see below), `Data` when they state nothing |
| `addLabel(..)`, `dropLabel(..)` / `setLabel(..)` | `Update` on `Data(vertex)` / `Data(edge)`, with the edge label the steps before it state |
| `drop()` | `Delete` on the `Data` the steps before it state, `Data` when they state nothing |
| `infer_schema()`, `g.get_schema()`, `g.infer_schema()`, `g.diff_schema(..)`, `g.validate_schema(..)`, `g.validate_schema_patch(..)` | `Read` on `Schema` |
| `g.statistics()`, `g.memory_usage()` | `Read` on `Data` |
| `embed(..)` (the step) and `embed(text)` (the function) | `Execute` on `Embedder("name")`, the embedder that runs (the default resolved to its name), for every embedder including the built-in `hash-ngram/1` |
| `g.embedders()` | nothing (it lists what the host registered and runs none of them) |
| `g.embed_property(..)` | `Read` and `Update` on `Data(element "label")`, `Execute` on `Embedder("name")`, and `Update` on `Schema` when it declares the property (or its `embedder`) in the schema; never in a saved query (refused when the query is saved, denied in a read-only one) |
| `nearest(key, "text", k)`, `similarity(key, "text")` (a text query) | `Execute` on the embedder of the model the schema declares for the property (on each one when labels declare different models) |
| `g.set_schema(..)`, `g.patch_schema(..)` | `Update` on `Schema` (a schema is never read from a file) |
| `list_definitions(kind)` (Rust) | `Read` on `Definition(kind)` (`Definition` for every kind) |
| `get_definition(kind, name)` (Rust) | `Read` on `Definition(kind "name")` |
| `set_definition(..)`, `define_query(..)` (Rust) | `Create` on `Definition(kind "name")` for a new definition, `Update` when it replaces one |
| `move_query(..)`, `describe_query(..)` (Rust) | `Update` on `Definition(query "name")` |
| `remove_definition(kind, name)` (Rust) | `Delete` on `Definition(kind "name")` |
| `g.query("name", ..)` | `Execute` on `Definition(query "name")` before the body runs; for a query that writes, then each of its inferred effects (`Create Data(vertex "person")`, ...); the body's own requests then go through the host's authorizer under the saved query's restriction (a read-only query: every write denied; a query that writes: every write outside its effects denied; see [Saved Queries](saved_queries.md)) |
| `g.queries()`, `g.queries(folder)` | `Read` on `Definition(query)` |
| `g.get_query("name")` | `Read` on `Definition(query "name")` |
| `g.define_query(..)`, `g.define_queries(..)` | `Create` on `Definition(query "name")` per query, `Update` when it replaces one |
| `g.move_query(..)`, `g.describe_query(..)` | `Update` on `Definition(query "name")` |
| `g.drop_query("name")` | `Delete` on `Definition(query "name")` |
| `g.define_compression(..)` | `Create` on `Definition(compression "name")`, `Update` when it replaces one |
| `g.compressions()` | `Read` on `Definition(compression)` |
| `g.recompress("name")` / `g.drop_compression("name")` | `Update` / `Delete` on `Definition(compression "name")` |
| `g.mark("name")` | `Update` on `Data` |
| `g.export_snapshot(path)` | `Read` on `Data`, `Create` on `File(path)` |
| `g.export_graphml(path)` | `Read` on `Data`, `Create` on `File(path)` |
| `g.import_graphml(path)` | `Create` on `Data`, `Read` on `File(path)` |
| `g.export_graphson(path)` / `exportGraphson` | `Read` on `Data`, `Create` on `File(path)` |
| `g.import_graphson(path)` / `importGraphson` | `Create` on `Data`, `Read` on `File(path)` |
| `GraphSource::file(path)` | `Read` on `File(path)` |
| `import "module"` (Rhai) | `Execute` on `File("module.rhai")` |
| an administrative `g.with(key, ..)` | `Update` on `Option(key)` |

A write of the elements a step receives (`property`, `removeProperty`, `drop()`, the label steps)
names what the steps before it in the same traversal state, so a policy can tell
`g.E().hasLabel("knows").drop()` (`Delete` on `Data(edge "knows")`) from `g.E().drop()` (`Delete`
on `Data(edge)`): `V()`, `out()`, the endpoint steps and `mergeV()` give vertices, `E()`,
`outE("knows")` and `mergeE()` edges (with the labels given), `addV("person")` /
`addE("knows")` their new element, and `hasLabel(..)` the labels of edges; filters, side effects,
modulators and the ordering and windowing steps (`limit`, `order`, `dedup`, ...) keep what is
stated, every other step ends it, and a child traversal starts from nothing. A vertex has a label
SET, so a vertex label is stated only where the plan gives all of them (`addV(labels)`):
`g.V().hasLabel("person").drop()` asks `Delete` on `Data(vertex)`, because a vertex labelled
`{person, secret}` passes that filter.

Only steps that **reach graph data** ask for a read: the sources (`V()`, `E()`, also in the middle
of a traversal) and the adjacency steps. A value-only step works on what earlier steps produced,
so it asks nothing: `g.V().has("age", P.gt(30)).values("name").limit(2)` asks exactly once, for
`Read` on `Data(vertex)`, and `g.inject(1, 2).sum()` runs under `AccessPolicy::deny_all()`. No
live element handle survives a run: `let v = g.V().next()` is a detached copy of the vertex
(see [Vertices and Edges as Values](vertex_and_edge_values.md)), and reading the copy (`v.name`,
`v["age"]`) asks nothing. Passing it back (`g.inject(v)`, also an edge's endpoint `e.out_v`)
reaches the stored element of its id, so it asks `Read` on `Data(vertex)` (`Data(edge)` for an
edge) like `V()`: a copy cannot open a way to data the policy hides.

Labels of reads are not reported: a request reports what the plan states, and a plan cannot say
statically which labels a read reaches. Element- and label-level read security is a later storage
decorator ([Later](#later)). A policy that restricts one element kind or label must treat `None`
as "possibly that one".

## The ready-made policy: `AccessPolicy`

`AccessPolicy` allows or denies by action kind and resource kind, plus an optional file root. It
covers what the shipped front ends need without a trait implementation of their own:

```rust
use graphersal::prelude::*;
use graphersal::auth::{AccessPolicy, ActionKind, ResourceKind};
use graphersal::catalog::Definition;

let query_service = AccessPolicy::read_only();             // Read on Data, Schema, Definition;
                                                           // Execute on Definition (saved queries)
                                                           // and on Embedder
let editor = AccessPolicy::read_only()
    .allow(ActionKind::Create, ResourceKind::Data)
    .allow(ActionKind::Update, ResourceKind::Data)
    .allow(ActionKind::Delete, ResourceKind::Data);
let server = AccessPolicy::allow_all().deny_resource(ResourceKind::File);
let nothing = AccessPolicy::deny_all();                    // a script can only compute
# let _ = (query_service, editor, server, nothing);
```

## Restricting a script

`graphersal::script::engine()`, `graph_scope(g)` and the `eval_*` functions without limits allow
everything. The entries that take explicit limits take the authorizer too:

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::auth::{AccessPolicy, Authorizer};
use graphersal::script::ScriptLimits;

let graph = Arc::new(GraphSource::tinkerpop_modern());
let read_only: Arc<dyn Authorizer> = Arc::new(AccessPolicy::read_only());
let names = graphersal::script::eval_value_with_limits(
    graph.clone(),
    r#"g.V().hasLabel("person").values("name").toList()"#,
    Default::default(),
    &ScriptLimits::default(),
    read_only.clone(),
);
assert!(names.is_ok());
let denied = graphersal::script::eval_value_with_limits(
    graph,
    r#"g.addV("x").next()"#,
    Default::default(),
    &ScriptLimits::default(),
    read_only,
);
assert!(denied
    .unwrap_err()
    .to_string()
    .contains(r#"Permission denied for 'add_v' (Create Data(vertex "x"))"#));
```

A host that drives its own engine passes the same authorizer to both halves of the entry: the
engine (Rhai's `import`, and the graphs a script creates with `GraphSource::empty()` and friends)
and the scope (the session's `g`):

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::auth::{AccessPolicy, Authorizer};
use graphersal::script::{engine_with_limits, graph_scope_with_options, ScriptLimits};

let graph = Arc::new(GraphSource::tinkerpop_modern());
let authorizer: Arc<dyn Authorizer> = Arc::new(AccessPolicy::read_only());
let engine = engine_with_limits(&ScriptLimits::default(), authorizer.clone());
let mut scope = graph_scope_with_options(graph, &[], authorizer);
```

A Rust-built traversal is checked the same way with
`GraphTraversalSource::with_authorizer(authorizer)`.

The front ends this project ships:

| Front end | Policy |
|---|---|
| `graphersal` CLI | everything (`AllowAll`): a local tool over your own graph and files |
| Web playground and the dev server (`graphersal --server`) | everything but files (`AccessPolicy::allow_all().deny_resource(ResourceKind::File)`): the browser has no file system; graphs are loaded and saved through the page |
| MCP endpoint of the dev server | the same as the page; with `--mcp-read-only`, `read_only_authorizer()` of `graphersal-session`: no files, and no `Create`/`Update`/`Delete` on `Data`, `Schema` or `Definition` |
| Python binding (`Graph.execute`, `query`, `dry_run`) | everything when the call names no `policy=`: a Python program is a trusted local host. Pass `policy=graphersal.AccessPolicy.read_only()` (or another `AccessPolicy`) per call for scripts you did not write |

## Writing an authorizer: roles from a token

The subject (who asks) is not part of the request. It lives inside the authorizer instance, so a
server builds one authorizer per connection or session, for example from the roles in a JWT it
has already verified, and maps them to actions and resources. Roles are not a library concept:

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::auth::{AccessRequest, Action, Authorizer, Denied, Resource};

/// Built per connection from the verified token's claims.
struct Session {
    roles: Vec<String>,
}

impl Session {
    fn has(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

impl Authorizer for Session {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        let allowed = match (request.action, request.resource) {
            (_, _) if self.has("admin") => true,
            // Everybody reads data and the schema.
            (Action::Read, Resource::Data { .. } | Resource::Schema) => true,
            // Editors change data, except elements labelled `salary`.
            (Action::Create | Action::Update | Action::Delete, Resource::Data { label, .. }) => {
                self.has("editor") && label != Some("salary")
            }
            // Server-owned resources go through the same policy.
            (Action::Custom("open"), Resource::Custom { kind: "database", name }) => {
                name == Some("shared") || self.has("dba")
            }
            _ => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(Denied::new(format!("{request} needs another role")))
        }
    }
}

let session = Arc::new(Session { roles: vec!["editor".into()] });
// The server checks its own resources at its own boundary ...
let open = AccessRequest::new(
    Action::Custom("open"),
    Resource::Custom { kind: "database", name: Some("shared") },
);
assert!(session.authorize(&open).is_ok());
// ... and hands the same authorizer to the script entry.
let graph = Arc::new(GraphSource::tinkerpop_modern());
let denied = graphersal::script::eval_value_with_limits(
    graph,
    r#"g.addV("salary").property("amount", 1).next()"#,
    Default::default(),
    &graphersal::script::ScriptLimits::default(),
    session,
);
assert!(denied.unwrap_err().to_string().contains("needs another role"));
```

`Resource::Custom` and `Action::Custom` are never produced by the library: they exist so that an
embedding server can put its own resources (databases, tenants, endpoints) under the same policy
and call `authorize` itself.

## The check

The check runs **once per execution, before anything runs**, on the plan **as written**: before
the optimizer, so no rule can hide a step by fusing it (`add_property_fold` folds `property()` into
`addV`, `count_pushdown` folds `count()` into `V()`; the check still sees each of them). It
recurses into every child traversal (`union`, `where`, `by`, `repeat`, `sideEffect`, ...), and it
costs one pass over the steps. Each **distinct** request is asked once per execution:
`g.V().out().in()` asks `Read` on `Data(vertex)` and on `Data(edge)`, once each. There are no
per-element checks. A denial is
`TraverserError::PermissionDenied { step, request, reason }` naming the first step (in plan order)
whose request was denied, the request (`AccessRequestBuf`, `as_request()` gives the action and the
resource) and the authorizer's reason; nothing of the traversal has run. A denied step is located
like a failing one: the error is the `TraverserError::StepFailed` chain of that step
(`at #1: v().add_v("x")`, `step_location()`; step ids are positions of the plan as written) and
the denial is its `root_cause()`. A denied `with("key")` option or a source method (file, schema)
has no step and stays the bare denial. `execute()` returns the denial in its error instead of
throwing.

"Once per execution" means once per traversal that runs. A script, and a saved query's body, may
run several traversals: each one is checked when it runs, so a denial in a later one comes after
the earlier ones have run (in the playground the whole script is one unit and is rolled back; a
saved query that writes runs as one unit too). A **saved query** is checked in layers.
`g.query("name", ..)` asks `Execute` on the query before its body runs; a query that writes then
asks its whole effect set, so a denial comes before its first write. The body's traversals then
go through the same check with the host's authorizer **plus** the saved query's restriction: a
step the host denies is denied inside the body too (the error carries a line `in saved query:
"name"`); in a read-only query every write is denied whatever the host allows, in a query that
writes every write outside its effects. A traversal any query returns is read-only, so
`g.query("name").drop()` is denied as well. Nothing in a saved query can do more than its caller
may.

Functions are never hidden: a denied step is still registered, listed by `help()` and completed by
the playground. Only running it is denied.

## Administrative `g.with()` options

A query may set the semantic options freely; setting an administrative one is an `Update` on
`Option(key)`:

| Option | Request |
|---|---|
| `evaluationTimeout`, `repeat.max_loops`, `bulk.max_expansion`, `traversal.max_traversers`, `traversal.max_value_depth`, `traversal.max_materialized_bytes`, `traversal.max_string_bytes`, `memory.limit`, `memory.headroom` (resource ceilings) | `Update` on `Option(key)` |
| `optimizer.disabled`, `optimizer.enabled`, `path.analysis`, `bulk.merge` (engine switches) | `Update` on `Option(key)` |
| `repeat.order`, `bulk.one` (`withBulk(false)`), `random.seed`, `render.spelling`, the display options (`render.max_rows`, ...), unknown keys (ignored by the engine) | none |

An option the **host** preset (`graph_scope_with_options(graph, &[("evaluationTimeout", ..)], ..)`,
or set on a Rust source before `with_authorizer`) is not the query's setting and asks nothing; a
query that sets such a key to another value does. The presets apply to every source of the
session: `g`, `_g` and `__`, and, when the engine is built with
`engine_with_options(&limits, authorizer, &options)` and the same options, the sources a script
creates itself (`GraphSource::empty()`, ...), so a script cannot escape a preset by starting a
traversal elsewhere.

`ExecutionPolicy` is the complementary control on the storage: it fixes the *value* of an option
(locked keys, defaults) for every entry, Rust included. Both apply; the authorization check runs
first.

## File access and the file root

An authorizer decides each file request; `Authorizer::resolve_file(path)` then maps the path to
the file actually accessed (the default returns it unchanged). `AccessPolicy::with_file_root(dir)`
confines every file access to `dir`:

- a relative path is resolved under `dir` (`g.export_graphml("out.graphml")` writes `dir/out.graphml`);
- a path that leaves `dir` is denied: `..` past the root, an absolute path elsewhere, a symbolic
  link inside the root that points out of it;
- Rhai's `import "helpers"` loads `dir/helpers.rhai` the same way;
- the root must exist; on wasm32 (no file system) every rooted access is denied, never a panic.

The root does not allow file access by itself: the policy must also allow the action on
`ResourceKind::File`. A host that reads files on behalf of a script calls
`policy.resolve_file(path)` itself.

## Graphs the script builds itself

A graph the script built itself is **private** to it: `GraphSource::empty()`,
`GraphSource::tinkerpop_modern()`, `GraphSource::file(path)`, `schema.toGraph()`, the scratch
sources `__` and `_g`, and the results of `subgraph()`/`cap()` and `toGraph()`. Changing it is not
a change of the host graph, so a request that writes data or the schema of it
(`Create`/`Update`/`Delete` on `Data`, `Schema` or `Definition`, `AccessRequest::writes_graph`) is allowed
without asking the authorizer:

```rhai
let sg = g.E().hasLabel("knows").subgraph("sg").cap("sg").next();
sg.addV("note").property("text", "mine").next();   // allowed with AccessPolicy::read_only()
```

Reading a private graph still asks for `Read`, and its file and option requests are asked as
usual. Naming the host graph as the explicit target of a subgraph
(`GraphSource::tinkerpop_modern().withSideEffect("sg", g).E().subgraph("sg")`) writes into the host
graph: it asks for `Create` on `Data` (operation `with_side_effect`) and gets no relaxation. Such a
target must be a `TraversalGraph`: a host graph of another storage is refused with
`GraphError::Unsupported` ([Set Side Effects](set_side_effects.md#writing-into-your-own-graph)).

## A whole script as one unit

`eval_value_atomic(graph, script, params, &limits, authorizer)`
([Transactions](transactions.md#a-whole-script-as-one-unit-rhai)) checks every traversal the same
way. While it runs, the script works on a private copy of the graph's lock, so it must not return a
closure or function pointer (also inside an array or a map): a closure that captured `g` would
point at an empty graph once the script ends. Such a script fails with
`ScriptError::ReturnedClosure` and everything it changed is rolled back; return data or a
traversal instead.

## Later

Element- and label-level read security (a `Filtered<S, Policy>` storage decorator) and a
`ReadOnly<S>` decorator are additive follow-ups. [`g.gql()`](../gql/index.md) uses the same
authorizer: a GQL query is lowered into ordinary steps before the check, so it is allowed or
denied like its Gremlin equivalent. Its writes (`INSERT`) ask `Create` like `addV()` /
`addE()`, but nothing for the new element's properties, where `addV(..).property(..)` also asks
`Update` ([Writing with GQL](../gql/writes.md#permissions)); `SET` and `REMOVE` ask `Update`
like `property()` and `addLabel()`; a read-only policy refuses them before anything runs.
