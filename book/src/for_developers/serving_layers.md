# Serving Layers

A server that hosts one product graph for many customers keeps the product once and gives each
customer a [layer](layered_graphs.md) over it. Clients configure a name such as `/mcp/acme`,
never a version. When the product is released again, the server publishes new layers over the
new product version, and the name moves to them. This page describes that serving model, which
pieces Graphersal provides for it, and what a host builds itself.

`graphersal-serve` hosts **one graph per process**, and it stays that way: it has no base pool,
no catalog of names and no multi-graph server. A host builds those on its extension points. A
complete reference host lives in the tests of `graphersal-serve`, in the module
`extension_tests::reference_resolver` (`crates/graphersal-serve/src/extension_tests.rs`). The
excerpts on this page come from it.

## The serving model

- **Base pool.** The server loads each product version once, as a frozen `LayerBase`, and
  keys it by its identity (`BaseRef`). `product@2026.3` and `product@2026.4` live side by side
  while layers over the old version still exist.
- **Catalog.** Each served version is named `<name>@<version>` and is a layer over a pooled
  base:

  ```text
  product@2026.3   -> base (served read-only as an empty layer over it)
  acme@2026.3      -> layer over product@2026.3
  globex@2026.3    -> another layer over the same base
  acme             -> alias, now acme@2026.3
  acme@2026.2      -> explicit pin to an old version, kept on purpose
  ```

- **Sessions are pinned.** An MCP session is bound to a graph when it starts (`initialize`)
  and stays on that graph for its whole life. A release flips the alias: new sessions get the
  new version, and open sessions finish on the old one.
- **Old versions are retired.** Once no alias and no pin names a version, and no session has
  used it for a while, the host drops it. When a base has no layers left, the host drops it
  from the pool.

## What Graphersal provides

| Piece | Where | What it does |
|---|---|---|
| `LayerBase` | `graphersal::storage` | A frozen base: the graph, its embedding models, a display name and its identity. A clone is one `Arc`. `new_layer()` makes a `Graph<LayeredGraph>` over it. |
| `LayerBase::from_store_read_only(dir, target)` | feature `persist` | Opens a versioned base store read-only at a commit or a mark (`RecoveryTarget::Mark("2026.3")`). It takes no lock and writes nothing, so the release process may keep the store open. |
| `LayerBase::from_snapshot_file(path, options)` | feature `persist`, native | Loads a base from a `.gsnap` file. |
| `BaseRef` | `graphersal::storage` | The base's identity: lineage id, commit sequence number and commit time. It is `Copy + Hash + Eq`, so it can key the pool. |
| Layer stores | `Store::create_layer` / `open_layer` | Keep a customer's layer durable. A layer store records the `BaseRef` of its base, and opens only over a base with that identity. |
| `Host::serves_layer` | `graphersal-mcp` | Makes the `initialize` instructions name `layer()`, `has_layer()` and `g.layer_diff()`. |
| Route tag | `graphersal-mcp` | `Host::session_route` puts the version into the session id: `<route>~<id>`. |
| `graphersal_mcp::session_route(id)` | `graphersal-mcp` | Reads the route out of an id, without checking the id. |
| `Mcp::verified_session_route(id)` | `graphersal-mcp` | Returns the route only when the endpoint knows the id. |
| `ContextBuilder::layered(true)` | `graphersal-serve` | Tells the context's `Host` that it serves a layer from the first `initialize` on. |
| `ContextBuilder::mcp_session_route(Some(..))` | `graphersal-serve`, feature `mcp` | Sets the route tag of the sessions this context starts. |
| `ContextBuilder::shared_mcp` / `mcp_profile` | `graphersal-serve`, feature `mcp` | One MCP endpoint for every context, with a profile per context. |
| `GraphResolver`, `Server::ready_with` | `graphersal-serve` | The extension point that picks the context of each request. |
| `GraphDirectory`, `Mcp::handle_directory` | `graphersal-mcp` | One MCP endpoint over all graphs: the agent lists them, selects one per session and may switch (see below). EXPERIMENTAL. |

### Base identity

Every read-only open of the same commit has the same `BaseRef`, and so does a `.gsnap` export
of that commit's snapshot. A second load of a pooled commit can be dropped in favour of the
pooled base. A commit to the base store makes a new identity, so each product version is its own
base.

A `.gsnap` export copies the store's newest **snapshot**. Commits that are still only in the
write-ahead log are not in it, so **checkpoint before you export** a release. Otherwise the
export holds the commit of the newest snapshot, with that commit's identity.

Samples and GraphSON or GraphML files get a new lineage id at every load. They never count as
the same base twice, so a server should load its bases from a Store or a `.gsnap`.

This example pools two product versions loaded from one versioned store and lays two customers
over the same base. It needs only the `graphersal` crate:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreOptions};
use graphersal::prelude::*;
use graphersal::storage::{BaseRef, LayerBase};

// The release process: one base store, a mark per product version.
let dir = MemDir::named("product");
let store =
    Store::create_with(&dir, TraversalGraph::tinkerpop_modern(), StoreOptions::new()).unwrap();
store.mark("2026.3").unwrap();
GraphTraversalSource::new(&mut *store.graph().write()).add_v("feature").iterate().unwrap();
store.mark("2026.4").unwrap();
store.close().unwrap();

// The server's pool: one LayerBase per base commit.
fn pooled(pool: &mut HashMap<BaseRef, LayerBase>, dir: &MemDir, mark: &str) -> LayerBase {
    let loaded = LayerBase::from_store_read_only(dir, RecoveryTarget::Mark(mark.into()))
        .unwrap()
        .with_name(format!("product@{mark}"));
    let key = *loaded.base_ref().unwrap();
    pool.entry(key).or_insert(loaded).clone()
}
let mut pool = HashMap::new();
let acme = pooled(&mut pool, &dir, "2026.3").new_layer().unwrap();
let globex = pooled(&mut pool, &dir, "2026.3").new_layer().unwrap(); // the pooled base
assert_eq!(pool.len(), 1);
assert!(Arc::ptr_eq(acme.read().base(), globex.read().base()));
let next = pooled(&mut pool, &dir, "2026.4").new_layer().unwrap();
assert_eq!(pool.len(), 2);
assert_eq!(acme.read().vertex_count().unwrap(), 6);
assert_eq!(next.read().vertex_count().unwrap(), 7);
```

### The route tag

At an HTTP `initialize`, the endpoint asks the context for its route. If the context has one,
the session id becomes `<route>~<id>`, for example `acme@2026.3~9f2c…`:

- With signed ids (`Mcp::with_session_key`), the route is part of the signed data. A changed,
  removed or added route breaks the signature, and the endpoint answers `404`. Replicas that
  share the key verify each other's ids, so a load balancer may send a session's requests to any
  replica.
- In table mode (the default), the table stores the whole routed id. An edited route is an
  unknown id, which is also a `404`.
- A route is 1 to 128 bytes of `[A-Za-z0-9._-@/:+=]` (`is_valid_session_route`). An invalid
  route makes `initialize` fail with a JSON-RPC error, so a session is never left unpinned.

A stream connection (`Context::mcp_connection`) is its own session and has no route: the host
picks the context when it opens the stream.

## What a host builds

The host owns four pieces: the pool, the catalog, the path rewrite and the resolver. It also
decides when an old version goes.

### Pool and catalog

```rust,ignore
struct Served {
    context: Arc<Context>,
    base: BaseRef,
    last_used: Instant,
}

#[derive(Default)]
struct Catalog {
    versions: HashMap<String, Served>, // "acme@2026.3" -> its context
    aliases: HashMap<String, String>,  // "acme" -> "acme@2026.3"
    kept: HashSet<String>,             // explicit pins, never retired
}

struct LayerHost {
    mcp: Arc<Mcp>, // one endpoint for every context
    pool: Mutex<HashMap<BaseRef, LayerBase>>,
    catalog: Mutex<Catalog>,
}
```

### One context per version

Each version gets a session thread that serves its layer, and a context built with
`.layered(true)`, its route, the shared endpoint and a profile of its own. Here a read-only
customer gets `Profile::new(true).with_access_policy(AccessPolicy::read_only())`.

```rust,ignore
fn publish(&self, version: &str, base: &LayerBase, profile: Profile) {
    let layer = Arc::new(base.new_layer()?);
    let name = version.to_string();
    let (jobs, ()) = spawn_session(
        SessionPresets::new(),
        move |session| session.load_shared(layer, &name, "layer").map(|_| ()),
        None,
    )?;
    let context = Context::builder(Runner::Session(jobs), host_check)
        .layered(true)
        .shared_mcp(self.mcp.clone())
        .mcp_profile(profile)
        .mcp_session_route(Some(version.to_string())) // "acme@2026.3"
        .build();
    self.catalog.lock()?.versions.insert(version.into(), Served { context, .. });
}
```

A customer layer that must survive restarts comes from a layer store instead
(`Store::<LayeredGraph>::open_layer(dir, &base, options)`), and the session serves the store's
graph. The layer store refuses a base whose `BaseRef` differs from the one it was created over.

### Path rewrite

A context answers MCP at `/mcp` only. The host's listener turns `POST /mcp/acme` into
`POST /mcp` and puts the name into a header. It drops any copy of that header the client sent,
so only the path names the graph:

```rust,ignore
const GRAPH_HEADER: &str = "X-Graphersal-Graph";

let name = path.strip_prefix("/mcp/").filter(|name| valid_name(name))?; // else 404
let mut request = Request::new(method, "/mcp", body);
for (field, value) in client_headers {
    if !field.eq_ignore_ascii_case(GRAPH_HEADER) {
        request = request.with_header(field, value);
    }
}
server.handle(request.with_header(GRAPH_HEADER, name))
```

### The resolver

The resolver runs for every request. A request with a session id goes to the version in the
id's route, after two checks. A request without one, such as `initialize`, goes to the alias's
current version, or to the version the path names (`/mcp/acme@2026.2`):

```rust,ignore
impl GraphResolver for LayerHost {
    fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
        let name = request.header(GRAPH_HEADER).ok_or_else(|| not_found())?;
        let routed = match request.header("Mcp-Session-Id") {
            Some(id) => {
                // 1. The endpoint knows the id (signature, or the table).
                let route = self.mcp.verified_session_route(id).ok_or_else(|| not_found())?;
                // 2. The route is a version of the name in the path.
                let belongs = if name.contains('@') {
                    route == name
                } else {
                    route.split_once('@').is_some_and(|(of, _)| of == name)
                };
                if !belongs {
                    return Err(not_found());
                }
                Some(route)
            }
            None => None,
        };
        let mut catalog = self.catalog.lock()?;
        let version = match routed {
            Some(route) => route.to_string(),
            None if name.contains('@') => name.to_string(), // an explicit pin
            None => catalog.aliases.get(name).cloned().ok_or_else(|| not_found())?,
        };
        let served = catalog.versions.get_mut(&version).ok_or_else(|| not_found())?;
        served.last_used = Instant::now();
        Ok(served.context.clone())
    }
}
```

A release publishes the new version and then flips the alias:
`catalog.aliases.insert("acme".into(), "acme@2026.4".into())`. Nothing else changes. Sessions
already open carry `acme@2026.3` in their ids and keep reaching that version's context.

### When to drop an old version

An open session keeps working only while the resolver still holds its version's context. The
endpoint cannot tell the host when a session has ended: a client may never send `DELETE`, and a
signed id stays valid for good. So the host decides. The reference host's policy:

- A version is **needed** while an alias names it or it is in `kept` (an explicit pin).
- A version that is not needed is **retired** after `grace` without a request (`retire_idle`).
  The resolver records `last_used` on every request it resolves. Retiring removes the context
  from the catalog. Its session thread then ends and its layer is dropped.
- A session still routed to a retired version gets `404` for an unknown session. The MCP
  client then starts a new session, which gets the current version. Pick the grace period so
  that this rarely happens during a working session (minutes to hours, not seconds).
- When no served version reads a pooled base any more, the base leaves the pool
  (`evict_unused_bases`), and its memory is freed with the last layer over it.

Other policies fit the same pieces: a deadline per release, a count of sessions per version kept
by the host, or retiring only at night. The test
`a_retired_version_drops_its_context_layer_and_finally_its_base` checks the reference policy:
the context, the layer and finally the base are released.

## One endpoint over all graphs (directory mode)

Instead of one MCP URL per graph (`/mcp/acme`), a host may offer a single endpoint that an
agent's harness configures once. The host implements `graphersal_mcp::GraphDirectory` over its
catalog:

- `list()`: the graphs and aliases an agent may name, each a `GraphEntry` with its kind
  (plain, layer, base), state (unloaded, loading, loaded, needs-model, failed with a reason),
  read-only flag and description.
- `resolve(name)`: the route of a name, an alias resolved now (`acme` -> `acme@2026.3`). The
  endpoint calls it on every call of a session, so keep it cheap.
- `host(route)`: the context serving that route (a `graphersal-serve` `Context` is a
  `graphersal_mcp::Host`) and its MCP profile, or an error whose text the agent reads, such as
  `database 'acme@2026.3' is not loaded; ask an operator to load it`.

The host's listener passes every request to its endpoint path to
`mcp.handle_directory(&catalog, request)`. Inside an MCP session the agent calls
`list_graphs`, selects a graph with `use_graph {name}` and may switch later; every data tool
takes an optional `graph` argument that names another graph for one call. Each call runs on
that graph's context under that context's profile, so its read-only flag and access policy
apply. Saved queries are not tools of their own here: the agent runs them with
`run_saved_query` (and `graph`).

- **The selection is a name.** `use_graph` stores the name the agent asked for (a graph or an
  alias, spelled as `list()` spells it when one entry matches ignoring ASCII case) and the
  endpoint resolves it again on every call, as an HTTP path `/db/<name>/` is. This is not the
  route tag above: a route tag pins a session to one version, a directory selection follows
  its name.
  - **Re-pointed alias:** the session follows it silently. Errors of the new target (not
    loaded, read-only) are ordinary tool errors.
  - **Removed alias:** a call fails with "pick another graph: list_graphs, then use_graph" only
    while no graph or alias of that name exists. The selection stays, so an alias created
    again under that name simply works on.
  - **Renamed alias:** the host renames it in its catalog, then calls
    `mcp.rename_selection(old, new)` (it answers the number of sessions moved). Each moved
    session's next `tools/call` answer starts with one line, `note: alias "<first old>" was
    renamed to "<current>"`; several renames before that call give one note from the first
    name to the current one.
  - **A real graph name** stays on that graph whatever the aliases do.
- `list_graphs` answers `selected` (the session's name, or `null`) and `selectedGraph` (the
  graph that name resolves to now, or `null`); `use_graph` answers `selected` (the name) and
  `graph` (its graph now).
- **A new session starts empty.** After a reconnect, or when the endpoint forgot the session
  (the table keeps `MAX_SESSIONS`, 64, unless `Mcp::with_max_sessions` says otherwise; the
  client then gets `404` and initializes again), calls that name no graph fail with a message
  that names `list_graphs` and `use_graph`, and the instructions tell the agent to select
  again.
- **Table sessions only.** The selection lives in the session table, so a directory endpoint
  refuses signed session ids (`Mcp::with_session_key`).
- `Mcp::with_endpoint(path)` sets the path `Mcp::status` shows.


- **The route is visible to the client.** It is part of the `Mcp-Session-Id` header, so never
  put a secret, a tenant key or a storage path into it. A versioned name is enough.
- **Check the name in the path.** A client that may use two graphs holds genuine session ids
  for both. Without the name check, an id that `/mcp/globex` handed out would reach globex's
  graph when sent to `/mcp/acme`, past any authorization that the host based on the path. The
  check is needed in both session modes: a signature proves the id is genuine, not that it
  belongs to this path.
- **Edited routes.** In table mode an edited id is an unknown id, and with signed ids it fails
  the signature. Ask the endpoint (`verified_session_route`) before trusting the route. The
  plain `session_route` decoder does not check anything.
- **Ignore the client's copy of the routing header.** The host sets the name header from the
  path and drops one the client sent.
- **Profiles are per context.** The read-only flag and the access policy of each customer are
  in that customer's contexts (`mcp_profile`). One shared endpoint does not mean one shared
  policy.
- Authentication stays the host's job, in a `RequestFilter` that knows the path's name.
