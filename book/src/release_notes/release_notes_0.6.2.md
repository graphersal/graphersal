# Release 0.6.2

A patch release of the 0.6 line for the playground and the dev server: **UI profiles**
(EXPERIMENTAL) let one playground code base serve several pages, each with its own tabs, cards,
graph views and top-bar module, and the dev server's hosting core gains an allow-list mode that
offers a page nothing but selected saved queries. The engine (`graphersal`) is unchanged: no
format change, no API change; stores, snapshots, journals and backups of 0.6.0 and 0.6.1 are read
and written exactly as before.

## Playground: UI profiles (EXPERIMENTAL)

A [UI profile](../users_guide/web_playground.md#ui-profiles) says what a playground page shows.
The playground as before is the built-in profile `full`, and it stays the default; `demo` is a
small built-in profile for tests and smoke checks. A profile names its title, its tabs (the
built-in workspace or ES modules of its own), its start tab and what the user can reach (`can`).

- **Profile API.** A profile's own tabs get an `api` object: `query` (a saved query),
  `markdown`, `card`, `graph`, `mergeGraph`, `toast`, `navigate`, `theme` and `info`. A tab that
  fails to load shows an error card; an invalid profile is shown as an error naming every
  problem, never replaced by another profile.
- **Delivery.** The dev server takes `--ui-profile <NAME|FILE>` (a built-in name or a JSON file;
  `GET /api/info` names it as `uiProfile`); the browser playground takes
  `#/play?profile=<built-in>`.
- **Cards.** [Card templates per label](../users_guide/web_playground.md#cards)
  (`cards: {<label>: template}`) with title, subtitle, badges, Markdown body, fields and action
  buttons, rendered by `api.card(label, row, options)`.
- **Graph views.** [`api.graph`](../users_guide/web_playground.md#graph-views) draws a module's
  `{nodes, edges}` with the Graph panel's renderers, styled per label by the profile key `graph`;
  `api.mergeGraph` merges expanded neighbours without duplicates and keeps the positions already
  drawn. The `full` profile's Graph panel draws exactly as before.
- **Top-bar module and version.** `header: {module}` puts a module of the profile into the top
  bar on every tab; the optional key `version` shows the product's version there, the engine's
  version moves into the tooltip.
- **Request headers of a host** (EXPERIMENTAL). A host page can set
  `window.GRAPHERSAL_ENGINE = {headers}` (for example `Authorization`) before the playground's
  modules run; the HTTP engine sends them with every request. Unused, every request is as before.
- **Subresource integrity everywhere.** Mermaid now loads from cdnjs's single-file build with its
  SRI hash, like every other CDN file (a test checks that each has one).

## Dev server

- **`POST /api/query` `{name, params?, runId?}`** runs one saved query with named arguments and
  answers every result (`{ok, results, mutated, changes}`), in every mode; the playground's
  engines (`runSavedQuery`) and the profile API's `query` use it. `openapi.json` and the
  [HTTP API table](../persistence/dev_server.md) describe it.
- **Allow-list mode** (EXPERIMENTAL, for hosts built on `graphersal-serve`; the `graphersal` CLI
  never turns it on): `ContextBuilder::api_allow_list(selection, policy)` makes the server answer
  only `GET /api/info` (`apiMode: "allow-list"`), `GET /api/changes` and `POST /api/query` for the
  selected saved queries, each run under the access policy; every other `/api/` route, the host's
  extra routes and `/mcp` answer `403`. The playground refuses to show a profile with the built-in
  workspace on such a server.
- The MCP usage dialog of the playground is wider and fits its tables without horizontal scrolling.

## Internal crates

`graphersal-session`, `graphersal-mcp` and `graphersal-serve` are internal (the last two
EXPERIMENTAL) and may change in any release; their changes in 0.6.2 are additive:

- `graphersal-session`: `Session::run_saved_query`, `queries::SavedQuerySelection` (folders with
  their subfolders, names, excluded names) and `authorizer_for(read_only, policy)`, moved here from
  `graphersal-mcp`, which now uses them (its saved-query tool selection is unchanged).
- `graphersal-serve`: `ContextBuilder::ui_profile`, `ContextBuilder::api_allow_list`,
  `Context::is_allow_list` and the route `POST /api/query`.
- `graphersal-wasm` (not published): `runSavedQuery`.

## Upgrading from 0.6.1

A drop-in replacement: no format change, no API change of `graphersal`, no upgrade step. Stores,
snapshots, journals and backups of 0.6.0, 0.6.1 and 0.6.2 are interchangeable. A crate that pins
`graphersal-mcp`, `graphersal-serve` or `graphersal-storage-tests` to the exact version moves the
pin to `=0.6.2`.
