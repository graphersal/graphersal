# Graphersal Playground (static web app)

A single-page playground for Graphersal: write Gremlin-style traversals in the browser, see the
results as a table, JSON or raw text, draw the graph, inspect the schema, and read the profile of
the optimized plan.

Queries run on the **real engine**: the Graphersal library compiled to WebAssembly
(`crates/graphersal-wasm`), in a module Web Worker. There is no backend: the page is static
(Vue 3, plain JavaScript ES modules and CSS, libraries from CDNs at pinned versions; no npm, no
bundler) plus the WebAssembly build in `pkg/`, which is built locally and not committed. It runs
from any static file server, GitHub Pages, or a `play.<domain>` host.

## Two modes, one UI

| Mode | Served by | Engine | Detected by |
|---|---|---|---|
| **WebAssembly** (default) | any static file server | `js/engine/wasm-engine.js`: the wasm build in a Web Worker | `GET api/info` does not answer as a dev server |
| **Dev server** (DEV, unstable, local only) | `graphersal --graph <src> --server [--port N] [--ui-dir DIR]` | `js/engine/http-engine.js`: HTTP calls to that process | `GET api/info` answers `{"server": "graphersal-dev", ...}` |

`js/engine/index.js` asks `api/info` (relative to the page) once at start and picks the engine;
the rest of the UI is the same code. Both engines run the same Rust session logic
(`crates/graphersal-session`), so their answers have the same shapes. In dev server mode there is
no start page and no graph choice (the server serves the graph it was started with), the top bar
shows a **DEV SERVER · unstable** badge, Stop cancels the running query on the server (it rolls
back; nothing restarts), and **Save to file** writes the graph back to the server's `--graph` file
when it has one. No `pkg/` build is needed in that mode. Every request names the page
(`X-Graphersal-Client`), and the page polls the server's change feed (`GET api/changes`, every
2 s while visible, `js/change-feed.js`): a change it did not make (another tab, an AI agent over
the server's MCP endpoint, `--mcp`) reloads the graph, schema, statistics and Store menu, marks
the tabs' results as possibly stale and shows "changed: commit N". When such changes arrive in
three polls within 10 s (a bulk load), the page reloads at most every 30 s instead
(`js/change-burst.js`) and a banner says so, with **Refresh now** and **Pause auto refresh**
(manual mode until **Resume**); after 10 s without a change it reloads once more and returns to
normal. With `--mcp` the top bar shows
an **MCP** badge naming the connected agent. See the book page "Dev Server and MCP"
(`book/src/persistence/dev_server.md`).

Engine-dependent actions wait for the engine (`engineReady()` in `store.js`, the gate of
`js/ready.js`): a hash navigation to `#/play` while the engine still starts waits instead of
calling a missing engine.

## Build and run it locally

Prerequisites (one-time):

```bash
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.129   # must equal the wasm-bindgen crate version
# optional: wasm-opt (binaryen) shrinks the module further, e.g. `brew install binaryen`
```

Build the engine and serve the directory:

```bash
playground/build.sh                        # writes playground/pkg/ (git-ignored); --dev for a debug build
python3 playground/serve.py 8000                                  # static server with Cache-Control: no-store (python3 -m http.server lets browsers keep stale modules)
# open http://localhost:8000/
```

`build.sh` runs `cargo build -p graphersal-wasm --target wasm32-unknown-unknown --profile
wasm-release` (opt-level `z`, LTO), `wasm-bindgen --target web` into `pkg/`, then `wasm-opt -Oz`
when it is installed, and prints the module size (about 4.4 MB, 1.2 MB gzipped). Without `pkg/`
the page shows how to build it instead of a broken UI.

Any static server works. Opening `index.html` from the file system (`file://`) does not: browsers
block ES modules, workers and `fetch` there. The page needs network access to the CDNs listed below.

Routes: `#/` is the start page (choose a graph, save the current one), `#/play` the workspace.
`reset/` is a separate static page (see [Reset](#reset-if-the-playground-freezes)).

Tests of the WebAssembly build (Node.js runs them; `.cargo/config.toml` sets the runner):

```bash
cargo test -p graphersal-session -p graphersal-wasm              # session logic, natively
cargo test -p graphersal-wasm --target wasm32-unknown-unknown   # wasm-bindgen tests under Node.js
node --test playground/tests/*.test.mjs                         # tab store, /reset script, schema editor model; no browser
```

## Layout of this directory

```
playground/
  index.html               page shell, CDN scripts with SRI, the import map
  build.sh                 builds the WebAssembly engine into pkg/
  pkg/                     build output (graphersal_wasm.js + graphersal_wasm_bg.wasm), git-ignored
  css/app.css              all styles; light/dark colour tokens on :root
  assets/favicon.svg
  reset/index.html         the /reset page: plain HTML + inline script, loads nothing of the SPA
  tests/tabs.test.mjs      Node tests (node:test) of js/tabs.js and the reset script
  tests/http-engine.test.mjs  Node tests of the HTTP engine (fake fetch) and the readiness gate
  tests/save-load.test.mjs Node tests of the Save menu files and the Stop restore order
  tests/schema-draft.test.mjs  Node tests of the schema editor's model and flow (js/schema/)
  tests/statistics.test.mjs    Node tests of the Statistics tab's data model (js/statistics.js)
  tests/graph-data.test.mjs    Node tests of the Graph panel's data model (js/graph-data.js)
  tests/saved-queries.test.mjs Node tests of the saved-query models (js/saved-queries.js)
  tests/element-editor.test.mjs Node tests of the property editor's model (js/element-editor.js)
  tests/edit-mode.test.mjs     Node tests of the Graph panel's edit mode (js/graph/edit-mode.js, the creation form)
  tests/renderer.test.mjs      Node tests of the renderer interface, the graph controller and the layout client
  tests/palette.test.mjs       Node tests of the label palette
  tests/filter-swatch.test.mjs Node tests of the filter popover's vertex label swatches (labelSwatch)
  tests/tiers.test.mjs         Node tests of the tier choice: thresholds, hysteresis, switch, no-WebGL, controls
  tests/fast-tier.test.mjs     Node tests of the tier switch (positions kept), drill-down helpers, Stop registry
  tests/graph-toolbar.test.mjs Node tests of the Graph toolbar's fit (what collapses into the Display menu), Filter badge
  tests/ai-chat.test.mjs       Node tests of the AI Chat tab model and controller (fake server), the wasm stub, the HTTP calls
  tests/link.test.mjs          Node tests of playground links: parse/build, graph data encoding, damage, share size
  tests/markdown.test.mjs      Node tests of the chat's Markdown renderer and the table -> Markdown copy
  tests/value-view.test.mjs    Node tests of collapsed long values (vectors, long texts, the setting valueMaxChars)
  tests/highlight.test.mjs     Node tests of the Graph panel's result highlight and its dismissal
  tests/examples-panel.test.mjs  Node tests of the Examples panel model and the shape of examples.json
  js/
    main.js                Vue app root, global shortcuts, dialogs, toasts
    router.js              hash routing (#/ and #/play), applies playground links
    link.js                playground links #/play?graph=..&q=..&run=1 and links that carry a small graph
                           (data=, schema=: GraphSON + schema, deflate-raw, base64url): parse, build, encode (pure)
    store.js               application state and actions (the only caller of the engine for load/run/stop/save)
    store-model.js         the Store menu's model: targets, actions, dialog texts, attic (pure, Node-tested)
    store-calendar.js      the Store menu's history calendar: marks/snapshots grouped by local day once,
                           month grid, colour buckets, keyboard moves, the menu's newest five (pure, Node-tested)
    tabs.js                query tabs: pure state logic (no Vue/DOM), wrapped by store.js; tab kinds 'query' | 'ai',
                           a query tab's language 'rhai' | 'gql' with its Parameters text
    gql-tab.js             GQL query tabs: switch rewrite, Parameters JSON reader, run text, error span (pure, Node-tested)
    ai-chat.js             the AI Chat tab: chat state, event merging, /commands, one controller per tab (pure, Node-tested)
    markdown.js            the chat's safe Markdown subset as a plain tree, GFM tables, table -> Markdown (pure, Node-tested)
    config.js              external links; re-exports examples.json
    examples.json          the Examples panel (`group`, optional `subgroup` (GQL), per sample graph; every
                           entry is run by graphersal-wasm's test playground_examples_run_against_the_engine)
    examples-panel.js      the Examples panel's model: group tree, plain search + snippets, keyboard order,
                           remembered state (pure, Node-tested)
    util.js                storage (try/catch-wrapped localStorage), formatting
    value-view.js          long values in element views: collapsed summary and full text (pure, Node-tested)
    export.js              file names and downloads (drawings, Save menu)
    profile-text.js        TraversalMetrics as the CLI-style text table (Profile panel, Text tab)
    statistics.js          the Statistics tab's rows: counts, memory, limits (pure, Node-tested)
    graph-data.js          the Graph panel's data model: the compact graph view, Cytoscape elements,
                           the size-dependent draw plan and layout choice (pure, Node-tested)
    graph/                 the graph drawings, independent of the drawing engine
      renderer.js          THE RENDERER INTERFACE (contract, tier capabilities, lazy `rendererFactory(tier)`; pure, Node-tested)
      cytoscape-renderer.js  the Detailed renderer: Cytoscape.js behind the interface
      sigma-renderer.js    the Fast renderer: sigma.js + graphology (WebGL) behind the interface, loaded on first use
      tiers.js             which tier draws (size, hysteresis, the switch, no WebGL), banner, disabled controls (pure, Node-tested)
      controller.js        layout choice, engine layout with progress/Stop, tier switch keeping positions (pure, Node-tested)
      layout-jobs.js       the running engine layouts the toolbar's Stop cancels (pure, Node-tested)
      layout-client.js     drives the engine's force layout slice by slice (pure, Node-tested)
      palette.js           the label colours: collision-free slots, light/dark (pure, Node-tested)
      highlight.js         the result highlight: when it shows, its Clear/Esc dismissal per result (pure, Node-tested)
      edit-mode.js         the edit mode: switch state, connect drag, in-place patch, positions (pure, Node-tested)
      toolbar.js           the Graph toolbar: which items fit in the row, the rest in the Display menu (pure, Node-tested)
    saved-queries.js       saved queries: folder tree, editor draft <-> spec, run form (pure, Node-tested)
    element-editor.js      the property editor: typed values <-> form tree, raw JSON, schema fields, diff (pure, Node-tested)
    schema/                the schema editor's logic (no Vue, tested under Node)
      api.js               THE EDITOR'S ONLY WAY TO THE ENGINE: the seven schema methods
      draft.js             the draft: node reading, in-place edits, merge patch, JSON sync, queries
      flow.js              draft -> What changes -> Check against data -> Apply; Freeze as open/closed
    engine/                THE ENGINE BOUNDARY
      engine.js            documented async interface (JSDoc) + default display limits
      index.js             creates the engine, or reports that pkg/ is missing
      wasm-engine.js       the WebAssembly implementation: talks to the worker, Stop, restore point
      worker.js            module Web Worker: loads pkg/ and calls the wasm Playground class
      http-engine.js       the dev server implementation (graphersal --server): one HTTP call per method
    ready.js               the engine readiness gate (pure, Node-tested)
    change-feed.js         the dev server's change feed: own vs. other changes, the poller (Node-tested)
    change-burst.js        throttled reloads while the graph changes fast elsewhere (Node-tested)
    editor/
      dsl-language.js      CodeMirror highlighting for the Rhai/Gremlin DSL
      completion.js        step/predicate/token completion with signatures and docs
      completion-doc.js    splits a function doc into description and example (pure, Node-tested)
      completion-data.js   loads the catalogue and token values, the choices after `Class.` (pure, Node-tested)
      token-classes.js     the token classes the highlighter colours: fallback + the engine's (pure, Node-tested)
      tooltips.js          completion tooltips fixed-position in the body (or the enclosing modal dialog)
      tooltip-parent.js    picks that parent (pure, Node-tested)
      editor-status.js     the Query panel's status bar texts: run status, cursor position (pure, Node-tested)
      gql-tokens.js  gql-language.js  gql-wrap.js   GQL tokenizer, highlighting, "Wrap as GQL" (pure parts Node-tested)
      gql-complete.js      what to complete at a position of GQL text, variables, schema keys (pure, Node-tested)
      gql-completion.js    the GQL completion source (keywords, functions, labels, keys, variables, $params)
      gql-functions.js     the GQL functions with a short text; checked against the engine's table (Node-tested)
      param-completion.js  a saved query's declared parameters in its body's completion (pure, Node-tested)
    components/            Vue components (template strings, no SFCs)
      start-page.js  workspace.js  panel-frame.js  query-editor.js  query-tabs.js
      results-panel.js  statistics-view.js  data-table.js  error-view.js  graph-panel.js  graph-canvas.js
      schema-panel.js  schema-editor.js  profile-panel.js  help-dialog.js  settings-dialog.js  drop-menu.js
      build-missing.js  saved-queries.js (the saved-query dialog: edit and run)
      value-text.js (one property value: collapsed long text or vector with expand/collapse and copy)
      store-menu.js (the Store menu: newest 5 marks, then snapshots)  store-dialog.js  store-calendar.js (the history calendar dialog)
      store-task-label.js (a store action's button text: idle, or spinner + running text + elapsed time)
      element-editor.js (the Graph panel's property editor dialog)
      share-dialog.js (Share link, the top bar's green button: the tab's query with the graph, small graphs only)
      ai-chat.js (the AI Chat tab)  markdown-view.js (assistant answers: the markdown.js tree as Vue nodes)
```

## Query tabs

The Query panel has a tab strip (`components/query-tabs.js`); the logic is `js/tabs.js` (pure
functions on `{tabs, activeId}`, tested under Node), and `store.js` wraps it in the reactive state
(`state.tabs`, `state.activeId`, the getter `state.tab` for the active tab).

- **Per tab:** script, name, run options (the Profile/Memory toggles), last result, last profile,
  the scripts the Results/Profile refresh buttons re-run, and a `stale` flag. Results and Profile
  show the active tab's values; switching never re-runs anything. The editor is one CodeMirror
  view; each tab keeps its own `EditorState` (text, selection, undo history), swapped on switch.
- **Shared:** the engine and the graph (one session), the query history (each entry carries the
  tab name), the settings. One query runs at a time; its result goes to the tab that started it,
  also when another tab is active by then.
- **Stale:** a run with `mutated` marks every other tab that shows a result or profile as possibly
  stale (an amber dot on the tab, a notice in Results, a badge in Profile), and so does Stop
  (the graph went back to the restore point). Running the tab again clears it; loading a graph
  clears every tab's result.
- **Examples (and history entries)** go into the active tab when it is empty or still holds an
  unmodified example (the tab records the example it came from; any edit makes it user text);
  otherwise into a new tab. A tab the user did not rename takes the example's title as its name.
  The Profile panel's "Run an example" follows the same rule.
- **Closing** asks only when the tab's text would be lost (not empty, not an unmodified example,
  not in the history); the last tab is emptied instead of removed. At most 30 tabs.
- **Extras:** duplicate tab, close other tabs, run all tabs (left to right, each into its own tab;
  a Stop ends the series), rename by double-click (or F2 on a focused tab), reorder by drag,
  middle-click closes.

### GQL query tabs

A query tab has a language, Rhai (the default) or GQL (`tab.language`, with `tab.params`, the JSON
text of its Parameters field; both persist with the tab, a Rhai tab writes neither). It is chosen
explicitly, never detected: the "+" menu's **GQL query** (`newGqlTab`) or the Rhai / GQL switch in
the Query panel's header (`setTabLanguage`). The pure logic is `js/gql-tab.js`:

- **Switch** (`switchTabLanguage`, like the saved-query editor's `switchDraftLanguage`): a Rhai
  tab that is exactly one `g.gql(<literal>[, #{literals}])` call (`parseGqlCall`) becomes its query
  and Parameters; GQL -> Rhai writes `wrapGql(query)` plus the map; other text is kept. A
  Parameters field that is no JSON map refuses GQL -> Rhai with its fault.
- **Run** (`store.js` `run`): the tab runs `gqlCallText(query, params)`, i.e. `g.gql("<query>",
  #{..})` built exactly like `graphersal_session::queries::gql_call_text` (a strict JSON reader that
  keeps integers and floats apart, `()` for null, quoted map keys where needed). The error's
  `position` is replaced by the GQL span (`gqlErrorSpan` reads `GQL ... at line L, column C` and the
  caret width from the diagnostic), moved into the editor by the run's offset; the editor underlines
  it (`cm-gql-error`, `spanRange`) until the text changes. History entries carry `language` and
  `params`; Profile, Stop, Run selection, the result refresh and "Run all tabs" need nothing else.
- **Parameters field** (`parseParams`, `paramsErrorText`, `withQueryParams` for "+ Add
  $parameters": every `$name` the query reads that the map lacks, as `null`).
- **Completion** (`js/editor/gql-complete.js` pure, `js/editor/gql-completion.js` the CodeMirror
  source): the context at the cursor (label after `:` with vertex / edge from the bracket, property
  after `var.` with the variable's labels, a pattern's property-map key, `$param`, or a word with the
  keywords of its clause, `GQL_CLAUSE_KEYWORDS`), the variables the text binds (`gqlVariables`),
  the functions of `js/editor/gql-functions.js` (one line of text each; the set is the engine's
  `FUNCTIONS` / `list_function` table in `crates/graphersal/src/gql/parser.rs`, and
  `tests/gql-complete.test.mjs` reads that Rust table and fails on a missing or extra name: add a
  line there when the engine gains a function), and the graph's vocabulary (`store.js`
  `gqlVocabulary`: labels from `statistics()`, keys by label from the stored schema or, without
  one, `inferSchema()`; loaded on first use and again after a data change). The Parameters field's
  keys come first after `$`. After `CALL` (and inside its dotted name) the saved queries written in
  GQL are offered as `folder.name` (`gqlProcedureChoices` over `state.savedQueries`, the list the
  Catalog manager loads; the tokenizer gives the name's segments the type `procedure`), and a
  `YIELD` item binds a variable. The saved-query editor uses the same source for a GQL body (its
  declared parameters after `$`) and `js/editor/param-completion.js` for a Rhai body (the declared
  names with their type, above the DSL's globals); both read the parameter table on every request.
- **Examples**: an example may carry `lang: "gql"` and `params` (a JSON map); it opens as a GQL tab.
  The Rust tests that run every example (`graphersal-wasm`'s `playground_*examples*`) and the MCP
  examples resource turn such an example into its `g.gql(..)` call with `gql_call_text`.
- **Links**: `lang=gql` (and `params=<JSON>`) open a GQL tab (`js/link.js`); a link without `lang`
  is a Rhai script as before. **Save as query…** of a GQL tab hands `{language: 'gql', query,
  params}` to the saved-query editor (`draftFromGql`).

### AI Chat tabs (dev server with `--ai`, EXPERIMENTAL)

"+" in the tab strip offers **Query** (`Alt+N`) or **AI Chat**: a tab of kind `ai` (its own accent
colour, `--ai-accent`/`--ai-soft` in both themes) that shows the chat (`components/ai-chat.js`) in
place of the editor. The agent runs in the dev server (`graphersal --server --ai FILE`, book page
"Dev Server and MCP", section "AI Chat", linked from the tab when `LINKS.book` is set); the page only drives `/api/ai/conversations*` through `http-engine.js`'s
`ai*` methods. The model and the per-tab controller are `js/ai-chat.js`, wired in `store.js`.

- **Modes** (`aiChatMode()`): `on` when `/api/info` reports `ai`; `off` on a dev server without
  `--ai` (the tab says how to start it); `wasm` in the WebAssembly playground (read-only: "AI Chat
  is available when you run Graphersal locally: graphersal --server --ai ai.json", the worker
  engine's `ai*` methods reject with that text). The header shows the model, `read-only`, and
  "Data you query is sent to <host> (<API>)", the API being "OpenAI-compatible API" or
  "Anthropic API" (the `provider` key names the API format, not who runs it), or "The model runs
  on this machine (<API>)" for a local `base_url`.
- **Transcript:** grouped per turn by `groupTurns` (pure, derived from the stored events, so a
  restored transcript groups the same way): the person's message, ONE collapsed group with the
  turn's internal steps (the interim assistant texts that came with tool calls, each tool call
  collapsible with the query, the result summary and text, "Open in a query tab" and Run), then
  the FINAL answer (the last assistant text no tool call follows) expanded as Markdown, then
  errors with their help. The group's toggle is a button with `aria-expanded`; its summary
  (`turnSummary`) says how far the turn got ("Worked: 2 tool calls (…)", "Working: running query",
  "Stopped after 1 tool call: …"); it starts collapsed, also while the turn runs, and the
  person's choice to open it is kept for that turn while the page lives (`chat.openSteps`, not
  persisted). A turn without tool calls has no group. Send = Enter, Shift+Enter = new line.
- **Export ▾** (header, `DropMenu`): the tab's transcript as Markdown or JSON (pure
  `chatToMarkdown`/`chatToJson` over `chatExportModel`, which follows `groupTurns`; JSON
  `{format: "graphersal-ai-chat", version: 1, ...}`, shape in the book's "AI Chat" section),
  downloaded through `export.js` `downloadBlob` as `chatExportFileName(tab name)`; both items are
  disabled for an empty transcript. Only what the page holds: a restored transcript has the capped
  items and shortened tool result texts (marked `resultTruncated`).
- **Rendering** (`js/markdown.js` + `components/markdown-view.js`, no library): paragraphs,
  emphasis, inline code, lists, headings, quotes, links (http/https only, new tab,
  `rel="noopener noreferrer"`), fenced code, GFM tables. Model text is never HTML: the tree holds
  plain strings that Vue renders as text nodes; raw HTML stays visible text. Tables scroll inside
  the message and copy as normalised GFM; a fenced block tagged `graphersal`, `gremlin`, `groovy`
  or `rhai` (or untagged and starting with `g.`) is a query with Copy and Run (a NEW query tab
  that runs it), other blocks get Copy. The buttons show on hover and on keyboard focus.
- **Isolation:** every chat tab has its own conversation, `after` cursor, poll loop (700 ms while
  a turn runs, paused while the page is hidden, ended when the turn is done or the tab closes),
  Stop and errors. The conversation id and the transcript persist with the tab (capped); the
  server conversation belongs to the page that created it (one client id per page load), so a
  restored one answers 403/404: the transcript stays as history and the next Send starts a new
  conversation and says so. Two browser tabs that restored the same chat tab therefore never post
  into one conversation. Closing a chat tab deletes the conversation this page created.
- **Commands** (the whole message, never sent to the model): `/clear` stops a running turn,
  deletes the server conversation (`DELETE`) and empties this tab only; `/help` lists the
  commands; any other `/word` is a local error.

Shortcuts (also in the Help dialog). `Ctrl`/`⌘`+`T`, `+W`, `+N` and `+1..9` are reserved by the
browsers (Chrome and Firefox do not pass them to the page), so the tab shortcuts use `Alt`, like the
existing panel shortcuts (`Alt+1..5` stay the panels):

| Keys | Action |
|---|---|
| `Ctrl`/`⌘`+`Enter` | run the active tab, or only its selection when one exists |
| `Ctrl`/`⌘`+`Shift`+`Enter` | run all tabs |
| `Alt`+`N` / `Alt`+`W` | new tab / close tab |
| `Alt`+`[` / `Alt`+`]` | previous / next tab |
| `Alt`+`Shift`+`1`…`9` | tab 1…8; `9` is the last tab |

## Schema editor

The Schema panel's **Tables** and **JSON** tabs are an editor (user-facing description: the book
page "WEB Playground", section "The schema editor"; the format: the book page "Schemas"). The
**Graph** and **Mermaid** tabs draw the stored (or inferred) schema from the engine's `schema()`
view.

- **Only the seven schema methods.** The editor reaches the engine through `js/schema/api.js`
  (`get`, `set`, `patch`, `validate`, `validatePatch`, `infer`, `diff`, a thin adapter over the
  engine's `getSchema`, ...), so the same UI runs against the dev server's API
  (`js/engine/http-engine.js`, which has these methods too). Nothing else of the engine is called by the editor; queries it
  opens (violations, backfill) run as ordinary query tabs through the store.
- **Draft.** `js/schema/draft.js` (`SchemaDraft`) holds the stored schema, the inferred one (shown
  when nothing is stored), and the draft. Every edit first copies the working schema into the draft;
  `version` counts draft changes, so a diff or check of an older version is recognisably stale (it
  is hidden), and a reload of the engine state (`graphVersion`) clears them too. The JSON tab's text
  is `jsonText`: valid text becomes the draft, invalid text sets `jsonError` and the tables keep the
  last valid draft; a form edit rewrites the text. The text of the working schema is made on its
  first read only and kept until the working schema changes (a reload formats nothing).
- **Large schemas** (measured B14, below): the stored and the inferred schema are `markRaw` (the
  panel's api wrapper; replaced whole, never edited in place), the Tables view takes the label
  names once per change (`computed`) and renders at most `LABELS_SHOWN` per kind, the JSON tab
  holds a schema with more than 2000 labels until "Show the JSON anyway", and the worker hands
  `getSchema`/`inferSchema` to the page as JSON text (one string crosses faster than the object
  tree's structured clone; `wasm-engine.js` parses it).
- **Patch.** Apply sends `createMergePatch(stored ?? {}, draft)` (RFC 7396) to `patch_schema`. A
  merge patch cannot set a member TO `null` (`"default": null`); `exact` is then false and the flow
  validates and applies the whole draft with `validate_schema`/`set_schema` instead.
- **Flow** (`js/schema/flow.js`, `SchemaFlow`): What changes = `diff(draft)`; Check against data =
  `validatePatch(patch)`; Apply = the check (unless the current version was checked), stop on
  violations, `patch(patch)`, reload. Errors (an invalid draft with its located message, the
  aggregated violation report) are shown in the review, never thrown.
- **Freeze as open / closed** (`SchemaFlow.freeze(mode)`): `set(infer(), mode)`, the inferred
  schema stored with that mode in one step; offered (`canFreeze`) while no enforcing schema is stored
  and no draft is edited. A failure (unlabeled elements in `closed`) is `freezeError`, shown in the
  review; nothing changed.
- **Violations** open a new query tab and run `violationQuery(group)`: an exact filter for
  `unknown_label` and top-level `missing_required` / `undeclared_property`, else the group's sample
  ids (at most 5; the comment line says when the report kept fewer than the count).
  `backfillScript(draft, groups)` writes the backfill recipe as one run (draft with mode `none`,
  `property(key, default or placeholder)` on the elements that lack the key, the draft); it is
  opened, not run, because a placeholder may need editing.
- **Schemas cross the boundary as canonical text** (`getSchema`, `inferSchema`, `patchSchema`
  return `GraphSchema::to_json()`), so the editor shows the canonical key order and keeps the
  property order (a `serde_json::Value` would sort the keys).
- **Layout.** The tables scroll inside the schema area only (both directions); the toolbar wraps.
  Every control is a native button, select, checkbox or input with an `aria-label`, so the editor
  works from the keyboard.

### Compact and marks

Compact never loses a mark without a question. The Disk space block's model
(`compactionModel` in `js/store-model.js`) asks before Compact runs whenever the advice names
marks that would become unreachable (`pruneMarksUnreachable`), recommended or not: the question
lists them by name and says they cannot be opened, forked or rolled back to afterwards (only from
a backup that holds older snapshots). Without affected marks it asks only when compaction is not
recommended. After the confirmation the request carries `{"dropMarks": true}`
(`engine.compactStore({dropMarks})`); without it the session refuses such a Compact (409 with
`marksUnreachable` on the dev server, an error in the WebAssembly playground), so when the
advice named no mark, a mark that became affected meanwhile is not dropped either: the toast
names it and the next Compact asks.

### A running store action (P15)

Every Store action (Checkpoint, Mark, Verify, Compact, Back up, Back up from memory, Open
read-only, Back to the live graph, Fork, Roll back, Restore, Remove, Switch to, Close) runs through
`runStoreTask(id, fn, key)` in `js/store.js`: one at a time (refused while another one, a query or
`storeAction` runs), held in `state.storeTask` (`{id, key, startedAt, now}`, `now` ticking every
250 ms). Its button (Store menu or dialog) renders `components/store-task-label.js`: an inline
spinner, the running text of `STORE_TASKS` in `js/store-model.js` ("Verifying the store…") and the
elapsed time ("0:42"), bold in the accent colour (`.store-running`, light and dark tokens), with
`aria-busy`; `key` tells the rows of one action apart (the Restore of one attic entry). Every
other store button is disabled with the tooltip `Wait for "Verify the store" to finish (0:42).`
(`waitTitle`). The Store button in the top bar shows `Store · verifying… 0:42` with a spinner and
`aria-busy` (`storeButtonText`), so the state is visible with the menu closed; Verify keeps the menu
open (`data-keep-open` on a `button.menu-item`, see `drop-menu.js`). The end is a toast with the
result; a failure that found damage, and Verify's problems, get a **Details** button (a toast may
carry `{label, run}`) that opens the menu, where the last Verify's result is listed under its item.
A polite live region (`state.storeAnnounce`) announces "Verifying the store started." and "Verify
the store finished after 0:03.". The engine reports no progress for these actions and cannot
cancel them (the session's cancel token stops queries only), so there is neither a progress bar
nor a Cancel button. The model is Node-tested (`tests/store-model.test.mjs`); headless check: Verify
on a `large` store with its request held 3 s.

### Store actions and reloads on a large graph (B14)

A Store action that does not change the data (Checkpoint, Mark, Compact, Verify, Back up)
reloads only the Store menu (`storeInfo`). One that changes the served state (Open read-only, Back
to the live graph, Roll back, Restore, Switch to, a run that changed the graph, a change made
elsewhere through the change feed) bumps `graphVersion`/`dataVersion`/`storeVersion`, and every
panel reloads what it shows within its limits (the Graph panel's limits and tiers, the Schema
panel above, the Statistics tab only while open). On the `large` sample (111 110 vertices, one
label per vertex, no stored schema: the inferred schema is 18 MB of JSON, inferred in about
0.7 s in the worker) the hot path was the Schema panel: the inferred schema went through Vue's
deep reactivity, was formatted as JSON text on every reload and its label list was walked several
times per render. Measured 2026-10-10 on an Apple M3, headless Chrome, release builds, the longest
main-thread task after each action (Long Tasks API; "-" = none above 50 ms):

| Action | WebAssembly before | WebAssembly after | Dev server before | Dev server after |
|---|---|---|---|---|
| Checkpoint, Mark, Compact, Verify | - (Compact once 262 ms) | - | - | - |
| run that adds a vertex | 1319-1608 ms | 103-119 ms | 1650 ms | 120 ms |
| Open read-only (a mark) | 1189-1299 ms | 86-116 ms | 1183 ms | 106-115 ms |
| Back to the live graph | 1282-1363 ms | 85-119 ms | 1169 ms | 106-128 ms |
| Roll back (a mark) | 1274-2890 ms | 84-110 ms | 1264 ms | 110-112 ms |

With the Schema panel's JSON tab open the same reloads took 820-1004 ms before the JSON tab held
large schemas, 84-103 ms after; with its Graph tab open 77-82 ms. The page is quiet again 0.5-1.2 s
after a swap (the worker's inference), 2-4 s before.

**The Schema panel infers on demand (B18).** The remaining cost of a change on such a graph was
the inference itself (~0.7 s of engine time, ~18 MB over HTTP in server mode, per change), so the
panel no longer infers after a change (`js/schema/panel-sync.js` `SchemaPanelSync`, over
`SchemaFlow.load({infer})` and the `outdated` flag; tests `tests/schema-panel-sync.test.mjs`):
- it infers on its FIRST SHOW (visible and expanded: the workspace passes `active`; a collapsed or
  hidden panel reads nothing, and the panel starts collapsed) and on its refresh button;
- a change (`graphVersion` only: a run, the property editor, an apply, the change feed) reads only
  the stored schema (`getSchema`, cheap, authoritative: a new or changed stored schema shows at
  once and clears the badge); without one the panel keeps the schema inferred last and shows the
  badge **may be outdated - refresh** (`badge warn`, its title explains it). The drawings' view
  (`schema()`, which infers without a stored schema) follows a change only while a schema is
  stored;
- a REPLACED graph (`graphEpoch` bumps with `graphVersion`: a load, Stop's restart, the server's
  graph on start, a read-only view, back to live, a rollback, an attic restore, a database
  switch, a view opened or closed elsewhere) counts as a first show: the old drawings are
  dropped and the panel infers once, now when it is shown, else when it is shown next.

## Reset: if the playground freezes

`reset/index.html` (served as `/reset/`; servers that redirect `/reset` to `/reset/` work, and the
script derives the playground root from the address, so `/reset` served directly works too) does
not load Vue, CodeMirror, the CDN scripts or the WebAssembly. Its inline script removes every
`graphersal.playground.*` key from `localStorage` (and `sessionStorage`) **except**
`graphersal.playground.history`, deletes Cache Storage entries and IndexedDB databases named
`graphersal*` (the playground uses none today), writes `lastGraph = "modern"` and redirects to
`<root>#/play`, which rebuilds the Modern sample graph. It shows what it removed and a fallback link
in case the redirect is blocked. The start page, the Settings and the Help dialog point to it.

To make the freeze less likely in the first place, the Graph panel draws large views in the Fast
(WebGL) tier and holds back a drawing that would still be risky (Detailed forced above 5000
vertices + edges, or without WebGL; Fast above 200 000: `chooseTier` in `js/graph/tiers.js`) until
the user clicks "Draw it"; for the whole graph it does not even fetch the elements before that. The
consent is not persisted, so a reload never starts a large drawing by itself.

The reset script is kept between `// reset-script:begin` and `// reset-script:end`; the Node test
extracts it and runs it against stubbed storage. A quick syntax check:

```bash
sed -n '/reset-script:begin/,/reset-script:end/p' playground/reset/index.html > /tmp/reset.js && node --check /tmp/reset.js
```

## Graph drawing

The Graph panel (`components/graph-panel.js`) and the Schema panel's drawing go through
`components/graph-canvas.js`, which talks to a RENDERER (`js/graph/renderer.js`: `setData`,
`setPositions`, `getPositions`, `setLayout`, `setOptions`, `fit`, `resize`, `select`, `onClick`,
`onDoubleClick`, `onUserMove`, `exportPng`, `setTheme`, `destroy`, and `capabilities`: tier, edge
labels, curved edges, arrows, built-in layouts, PNG/SVG, schema) and never to the drawing engine.
There are two renderers, the TIERS: **Detailed**, Cytoscape.js on Canvas 2D
(`js/graph/cytoscape-renderer.js`), and **Fast**, sigma.js + graphology on WebGL
(`js/graph/sigma-renderer.js`); `js/graph/tiers.js` decides which one draws. Elements are
addressed by their index in the data's columns, so a renderer needs no ids or properties. The
controller (`js/graph/controller.js`) decides the layout and hands positions to the renderer; the
data model is `js/graph-data.js`. All of it except the two adapters is pure and Node-tested
(`tests/renderer.test.mjs` and `tests/fast-tier.test.mjs` run the panel's behaviour against fake
renderers and a fake engine, `tests/tiers.test.mjs` the tier choice).

- **Toolbar** (`js/graph/toolbar.js`, `tests/graph-toolbar.test.mjs`). The row holds the frequent
  actions: `[Filter] [Layout] [✎ Edit] ... [✕ Clear highlight] [Fit] [Export ▾] [⚙ ▾]`. The ⚙
  **Display** menu holds the drawing tier (Auto/Detailed/Fast with the automatic pick), Vertex IDs,
  Edge labels, Edge IDs, Highlight result, Vertex size and the mouse help. The panel measures the row
  (ResizeObserver) and `toolbarFit` moves Layout, then Edit, then Filter into the Display menu, and
  finally shortens Clear highlight, Fit, Export and Display to their icons, so the row never wraps or
  scrolls (down to a 320 px panel). Filter is a split button (badge `filterBadge`: `· N` active
  restrictions, `· off`) whose switch turns the filter off and on without losing it.

- **Vertex size** (`js/graph/size-by.js`, `tests/graph-size.test.mjs`; Display menu, stored as
  `graph.sizeBy`: `off` | `degree` | `in` | `out`). Only while it is not Off does the panel ask for
  the degree columns (`degrees: true` on `graphView` limits, neighbourhood and sample requests;
  `subgraph_degrees` in `store.js` `run`). `nodeScaleFor` gives the canvas a scale per vertex,
  `t = log1p(d) / log1p(dMax)` over the drawn vertices (whole-graph degrees from
  `vertexInDegree`/`vertexOutDegree`; a drawing without them, e.g. an older result or one patched by
  edit mode, counts its drawn edges), passed to the renderer as `nodeScale` (`setData` /
  `setOptions`, sizes change in place). Each renderer maps it into `SIZE_RANGE`: Cytoscape 12 to
  40 px (fixed size 24; the `node[size]` style, so cose lays out with the real sizes), sigma 0.5x to
  1.6x of its size for the vertex count (`nodeSizeFor`). Switching it on reads the drawing again
  with the degrees and keeps the vertices where they are (`graph-canvas.js` `keepPositions`).

- **Filter** (model `js/graph/filter-model.js`, `tests/graph-filter.test.mjs`; popover
  `components/graph-filter.js`, an overlay over the drawing anchored under the Filter button at the
  panel's left edge below the toolbar, also when Filter sits in the Display menu; it narrows from
  340 px to 280 px to keep clear of the inspector on the right and covers it only in a panel
  narrower than about 600 px; Esc closes, never docked). Each vertex label has a fixed-size colour
  swatch (`.fd-swatch`, aria-hidden) from `labelSwatch` in `js/graph/palette.js`
  (`tests/filter-swatch.test.mjs`): the slot the drawings use (the current drawing's labels are
  assigned first through the shared `paletteLabels` order); a label never drawn in this page has
  no slot yet (slots follow the order labels are first drawn), so it shows a dashed neutral
  outline instead of a guessed colour; a hidden label shows its colour as a ring. Edges are drawn
  in one neutral colour: no swatch. The popover edits a filter STATE `{enabled, hideIsolated,
  vertices/edges: {off: [hidden labels], conds: {label: [{key, op, value, value2, type}]}}}`, kept per graph in localStorage
  (`graph.filter.<kind>:<name>`, `filterStorageKey`); `buildFilter` turns it into the engine's
  `GraphFilter` in its "others visible" form (`othersVisible: true`: `hiddenVertices`/`hiddenEdges`
  by name, `otherVertices`/`otherEdges: "visible"`, only the conditioned labels in the maps), so a
  label beyond the largest ones `statistics()` lists stays visible (a side with no hidden label and
  no condition is left out, so no filter costs nothing). Key types come from the stored schema, else the inferred
  one (inferred only when the popover opens; each condition keeps its key's type so a stored filter
  applies after a reload without inferring); a number key sends a JSON number, every other value
  a string (never sniffed). Applying: 300 ms after the last edit, on a graph above
  `FILTER_TUNING.manualApplyAbove` (100 000 elements) only on Apply; the on/off switch, Clear and
  Reset apply at once. The whole graph and the neighbourhood go through the engine
  (`graphView(limits, filter)`, `neighbourhood(request, filter)`, one whole-graph request at a
  time via `createApplyQueue`, a newer selection supersedes an older one); a query result and a
  label sample are filtered by label on the client (`labelFilterView`); the label overview draws
  hidden labels faded (`faded` on the schema drawing's elements, class `filtered-out`). An engine
  error names `vertices.<label>[i]`: `errorTarget` maps it back to the drawer row through the sent
  indices, and the drawing keeps the last good result. "Open as query" writes
  `graphFilterQuery(filter)` into a new tab. A filtered neighbourhood answers `unfiltered: {nodes,
  edges, vertexIds}`: the strip says "X of Y", and expansions and filter changes keep
  `unfiltered.vertexIds` (`neighbourhoodIds`, `expandRequest`, `redrawRequest` with `hops: 0` in
  `js/graph/selection.js`), so every vertex the user expanded into is filtered anew.

- **Tiers.** `chooseTier` (`js/graph/tiers.js`) picks by the number of elements drawn (vertices +
  edges): **Detailed below 2000, Fast from 2000**, and once Fast, Detailed again only below 1600
  (hysteresis, so a drawing that grows and shrinks around the threshold does not flip). The panel's
  switch **Auto / Detailed / Fast** (also in Settings, kept in `localStorage` with the other
  settings) overrides the size; "Auto · Fast" shows what Auto chose. The schema drawing is always
  Detailed. A switch keeps the drawing's positions: the controller takes the old renderer's
  positions, destroys it (sigma empties the shared container when killed) and gives them to the new
  one as a preset (`replaceRenderer`), so neither the engine nor the browser lays out again.
  - What Fast lacks is a banner above the drawing, built from the renderer's `capabilities`
    (`missingFeatures` against Detailed's): no edge labels (hovering a vertex shows the labels of
    its edges), no curved edges and loops, no Concentric or Tree layout. Controls the tier cannot
    honour are disabled with the reason as tooltip (`controlStates`): "edge labels", "Edge IDs",
    and the Concentric and Tree entries of the layout menu. Force (cose) stays: in Fast the force
    layout is always the engine's (`layoutIn` in the controller); Circle and Grid are computed by
    the Fast renderer itself. PNG export works in both tiers (Fast: at the canvas size and the
    screen's pixel ratio, the whole drawing by resetting the camera for the capture). There is no
    SVG export in either tier yet (`capabilities.svg` false).
  - **Confirmations** ("Draw it") remain only where they protect: Detailed forced above 5000
    elements (a warning already from 2000), Fast above its range (200 000), and Detailed without
    WebGL above 5000. A whole-graph view is not even fetched before the confirmation.
  - **No WebGL** (or sigma.js failed to load or start: offline, blocked): the Fast choice is
    disabled with the reason, the drawing is Detailed with a notice and a hard cap of 10 000
    elements (the longest vertex prefix that fits, `capToElements`).
  - **Fast renderer.** sigma.js 3.0.3 and graphology 0.26.0 are injected from cdnjs as their UMD
    builds with SRI the first time a Fast drawing is made (`loadFastLibraries`, about 140 ms; small
    graphs never load them). Their ES module builds import the CommonJS `events` package by its
    bare name, which a browser cannot resolve without a bundler; jsDelivr's converted `+esm` files
    load, but jsDelivr says not to pin generated files with SRI, so the pinned UMD files win. The
    graphology graph is built once AT the engine's positions and handed to sigma in one piece
    (`setGraph`). Vertices are coloured from the same palette as Detailed, labels appear by zoom
    level (sigma's label grid; outlined in the surface colour), hover rings the vertex's
    neighbours and colours its edges with their labels (a partial repaint of those elements only),
    a selection fades the rest (reducers), straight edges with arrow heads, edges clickable up to
    20 000 edges (sigma's edge picking is an extra render pass), sigma's y axis mirrored so both
    tiers show the same orientation.
  - **Wheel zoom in Detailed** is our own (`js/graph/wheel-zoom.js`, a capturing listener that
    stops Cytoscape's): Cytoscape takes a mouse wheel for an "inaccurate scroll device" and moved
    about 1 % per notch (measured: 3 notches = +3.5 % with `wheelSensitivity: 0.25`). Now one step
    of x1.3 per wheel event, at most one per direction every 50 ms (sigma's rule, so the Fast and
    Detailed tiers feel alike; measured: 3 notches = x2.2), and a trackpad pinch (`ctrlKey`)
    follows its delta. The renderer's `wheelZooming` flag is set as Cytoscape's own handler does,
    so large drawings keep the cheap texture while zooming.
- **View selector** (the panel's tabs; model `js/graph/selection.js`, engine side
  `crates/graphersal-session/src/selection.rs`, Node tests `tests/selection.test.mjs`). **Whole
  graph** draws the graph when it fits the vertex and edge limits; a larger graph shows the label
  overview instead (`wholeGraphPlan`), the first-N slice only behind "Draw the first N vertices
  anyway". **Labels** draws the label meta-graph (`engine.metaGraph()`: labels by primary label
  from the statistics, the 40 largest plus one "other labels" node; label pairs from one pass over
  at most 5 000 000 edges, the 200 largest) as a schema-kind drawing in Detailed, nodes scaled by
  the log of their vertex count (`metaGraphDrawing`); a double click (`activate` from the canvas)
  asks `engine.labelSample()` for the first 1000 vertices of that label (or the label filter of
  "other labels") or the first edges of that label pair and shows them in the Neighbourhood tab.
  **Neighbourhood** asks `engine.neighbourhood({ids, hops, direction, keep, maxNodes, maxEdges})`:
  the centre comes from the id field, the inspector's "Neighbourhood" link or a Results row's ⌖
  button (`showNeighbourhood` in the store); **Depth** 1-5 and **direction** both/out/in redraw from
  the same centre; a double click (`expand` from the canvas, `expandFromEngine`) sends the drawn
  vertex ids as `keep` and the clicked one with depth 1, and the new view is drawn at
  `carryPositions` (old vertices where they were, new ones on rings around the clicked vertex; the
  controller's `show(.., {positions})` skips the layout). Notices say what is drawn, how many edges
  lead out (`hiddenEdges`), and what the limits left out (`leftOut`: the engine keeps counting the
  neighbourhood up to 1 000 000 vertices after the limit). The last neighbourhood or sample
  (`graph.selection` in `localStorage`) is fetched again after a reload; a graph change re-reads
  the drawn vertices (`keep`, dropped ones are named). **Query result** draws the execute
  response's `subgraph`.
- **Editing properties.** The inspector's pencil opens the property editor
  (`js/components/element-editor.js`, model `js/element-editor.js`): a modal with one line per
  property (name | type | value | actions, columns shared by every row through a CSS subgrid,
  `.ee-grid`; the value editor's root is `display: contents` there; a multi-line text, raw JSON
  and the children of a container go below the line; two lines per property below 560 px; the
  add row has the same columns; add, remove with undo, rename), nested arrays and objects as a tree
  (expand/collapse, add/remove items and keys), a raw-JSON mode per value and for the whole
  element. Values travel in the engine's typed form, so an int64 stays an int64 and a string is
  never retyped by its content; raw JSON keeps the types of the values it keeps (a new number is
  int64 without a fraction, else float64). The stored schema of the element's label(s) drives the
  form: declared properties are listed (also unset ones), required ones marked, type, bounds and
  description shown as hints, an `enum` as a menu. Strings at a compression rule's path (and long
  or multi-line strings) get a textarea. Every change is checked by the ENGINE with a dry run of
  the write (`updateElement({.., validateOnly: true})`, debounced); its errors appear at their
  keys and Save stays disabled until it accepts. Save writes the changed and removed top-level
  keys as one commit; a refusal keeps the dialog open with the engine's error. Afterwards the
  panels reload like after a mutating query and the inspector is read again. The pencil is
  disabled, with the reason as its tooltip, while a query runs and when the graph cannot be
  written (`elementForEdit().readOnly`: a read-only view of a past state, a backup, a store in
  maintenance mode or frozen by damage). Ids and labels are not edited here.
- **Edit mode.** The toolbar's Edit mode switch (`js/graph/edit-mode.js`, pure and Node-tested:
  the switch state `editModeState`, the drag state machine `ConnectGesture`, the in-place patch of
  the drawing `applyEdit`, the positions `carryEditPositions`, label options and the delete
  confirmation). Double click on empty space = a new vertex there; drag from a vertex to another =
  a new edge (the Detailed renderer draws a rubber band as an SVG line over the canvas and reports
  the pair through `onConnect`; vertices are not grabbable in edit mode); double click on an
  element = the property editor; Delete or the inspector's bin = delete after a confirmation with
  the engine's edge count (a dry run). The creation dialog is the property editor in create mode
  (`openCreate`): label from `labelChoices` (declared labels, for an edge filtered by the
  schema's connections in the ENGINE, plus the graph's labels; free text unless closed), optional
  id, the form from the label's schema, dry-run checks of `createElement`. Each change is one
  commit; the canvas emits `edited`, the panel patches the drawing it shows (the whole graph and a
  neighbourhood are read again, the neighbourhood keeping the new vertex; the result drawing and a
  drill-down stay patched), and the next redraws keep the positions and the view (no layout).
  Adding needs `capabilities.addElements` (Detailed); the Fast tier edits and deletes only
  (`controlStates().addElements` gives the reason). The switch is disabled, with the engine's
  reason, for the label overview and read-only graphs (`labelChoices().readOnly`).
- **Gestures (both tiers).** Click = inspector. Double click on a vertex selects it and its
  neighbours (again: one hop more, at most 1000 vertices; `neighbourhood` in `graph-data.js`);
  double click on empty space fits; `Esc` on the focused drawing (a click focuses it) clears the
  double-click selection and the result highlight. **Drill down:** in Fast a picked vertex (click) or the double-click
  selection offers "Open ... in Detailed": its neighbourhood within the drawing (`subView`) is drawn
  in the Detailed tier, with "Back to the whole drawing". Sigma has no lasso, so there is none.
- **Result highlight** (`js/graph/highlight.js`, Node tests `tests/highlight.test.mjs`). The
  Whole graph tab highlights what the active tab's result refers to (`renderer.select(ids)`, the
  rest faded, both tiers). **✕ Clear highlight** (toolbar end, only while a highlight is shown)
  and `Esc` dismiss it for that result: a dismissal is keyed by the result's subgraph object in a
  module-level `WeakSet` (the panel is re-mounted on a tab switch), so a new result, or another
  tab's, highlights again and a cleared tab stays cleared; the Display menu's "Highlight result" checkbox
  is the lasting switch. The canvas reports `Esc` as `clear-highlight`; it focuses itself on press and on
  release (Cytoscape blurs the active element on mousedown).
- **Long values** (`js/value-view.js`, Node tests `tests/value-view.test.mjs`; view
  `components/value-text.js`). The inspector and the Results table collapse a text above the
  setting `valueMaxChars` (default 200, 0 = off, else 20..100 000) or above 3 lines, and a list of
  at least 16 numbers (how the inspector receives a vector: plain JSON numbers) to
  `vector[N] [first 4, …]`; expand/collapse buttons carry `aria-expanded`, the inspector's copy
  button copies the full value. The table's vector marker (`graphersal:Vector`) keeps its
  `<details>`.
- **Stop.** A running engine layout is registered with the store (`js/graph/layout-jobs.js`), so the
  toolbar's Stop is enabled while only a layout runs and then cancels it (`layoutCancel`): the
  drawing keeps the positions reached so far, nothing restarts. With a query running, Stop works as
  before (WebAssembly: the worker restarts; a layout ended that way says so).

- **Compact data.** `graphView` (the whole graph, the first N vertices and the edges between them),
  `neighbourhood`/`labelSample`, and an execute response's `subgraph` (the elements a result refers
  to, at most `subgraph_max_elements`: the setting "Result graph", default 50 000, at most 200 000,
  plus a 16 MB bound on the estimated column size; `stoppedBy` says which stopped it) are COLUMNS
  (`CompactGraph` in `crates/graphersal-session/src/display.rs`): vertex ids, a label index per
  vertex into one label table, the `name` property as caption, edge ids, edge source/target as
  indices into the vertex columns, edge label indices. The index columns become `Uint32Array`s
  (`packGraphView`); the worker transfers their buffers instead of copying them, the HTTP engine
  converts the JSON the same way, so the page sees one shape. No properties are shipped: the
  inspector calls `element(kind, id)` when an element is clicked (a newer click wins; a dropped
  element says so). For 1000 vertices and 2000 edges with three properties each the view went
  from 342 KB to 56 KB of JSON.
- **Layout in the engine.** Cytoscape's force layout `cose` runs on the main thread and is
  quadratic per iteration, so it runs for at most 200 vertices (1000 iterations, 400 above 100
  vertices; `layoutFor` in `graph-data.js`). Above that, and for the layout choice "Force
  (engine, Barnes-Hut)" at any size, the ENGINE lays the drawing out
  (`crates/graphersal-session/src/layout.rs`): a ForceAtlas2-style force layout (repulsion through
  a Barnes-Hut quadtree, linear attraction along edges, strong gravity, adaptive speed),
  deterministic from a seed, `O(n log n)` per iteration (500 iterations up to 1000 vertices, 300
  up to 10 000, 200 up to 50 000, 120 above). It runs in the Web Worker (WebAssembly) or on the
  dev server's session thread in SLICES of 50 ms (`js/graph/layout-client.js`: `layoutStart`, then
  `layoutStep` until done), so the page shows progress, the worker answers other requests (an
  inspector click) between slices and the main thread never blocks; **Stop** next to the progress
  ends it and draws the positions reached so far (so does the toolbar's Stop, see **Stop** below).
  Positions come back as a `Float32Array` (`x, y` per vertex; the worker
  transfers its buffer; the dev server answers JSON that the HTTP engine converts), centred, the
  median edge 70 px long, and the renderer gets them as a preset: the elements are added AT their
  positions, one paint. The engine caches finished layouts by the drawn topology (and seed), so
  drawing the same view again, a re-layout or switching back from another layout answers at once;
  a graph change that changes the view misses the cache. Circle, Concentric, Tree and Grid stay
  the renderer's own layouts at any size. Below 2000 elements
  the drawing is as before (curved edges, arrows, outlined labels); from 2000 (`reduced`) edges are
  straight, labels lose the outline and are drawn only when at least 7 px on screen
  (`min-zoomed-font-size`), and edges are hidden behind a texture while the view moves
  (`hideEdgesOnViewport`/`textureOnViewport`, set on the renderer since Cytoscape reads them only
  at init); from 5000 (`minimal`, only after "Draw it") edges are haystack lines without arrows.
  The schema drawing uses the same layout rule. Since GR3 the `reduced` and `minimal` plans are
  reached only when Detailed is forced (or without WebGL, or a large drill-down): Auto draws 2000
  elements and more in Fast.
- **Defaults** (Settings): 10 000 vertices, 20 000 edges (the whole graph, a neighbourhood, a
  sample), "Result graph" 50 000 vertices + edges, tier Auto. Chosen by measurement (GR4 table
  below): the engine's view and its transfer stay below 100 ms up to 100 000 elements, the engine
  layout dominates (2.2 s for 20 000 elements, 4.1 s for 50 000, 8.4 s for 100 000 in the
  worker), so the whole-graph default stops where the drawing appears within about 2-3 s; larger
  graphs are reached through the Labels and Neighbourhood tabs. A query result is drawn without
  waiting (it is already in the response): 100 000 elements arrive in about 0.2 s, so its default
  is 50 000 (Fast draws that in 90 ms; its layout takes about 4 s with progress).
- **Colours** (`js/graph/palette.js`): a label gets the next free slot of a 12-colour palette the
  first time a drawing shows it (a view's labels in the order of its label table) and keeps it for
  the page, so two labels of one view never share a colour (up to 12; more labels share a neutral
  grey, which the legend says) and a label has the same colour in the whole graph, a result, the
  schema drawing and every tier. The palette is validated per theme (light against #ffffff, dark
  against #181d25: lightness band, chroma floor, colour-blind and normal-vision separation of
  neighbouring slots); every vertex also shows its caption and the legend names the labels, so
  colour is never the only cue. Nodes carry their slot as a class and the style colours the slots,
  so a theme switch recolours without touching the elements.
- **WebGL.** Cytoscape 3.34's experimental WebGL renderer (`renderer: {name: 'canvas', webgl:
  true}`) draws our styles correctly in a check, but measured no gain in the Detailed range (3000
  elements: first frame 219 ms vs 208 ms on canvas, zoom p90 17 ms vs 10 ms); at 10 000 it removed
  the long frames after a gesture but doubled the p90 frame time. It is not enabled; the Fast tier
  (sigma.js) takes the sizes where WebGL matters.

Measured once (GR1, 2026-10-08) in headless Chrome on an Apple M3 (GPU through ANGLE/Metal), dev
server mode, a `Fake(7)` graph with 4 vertex labels, 3 edge labels, a `name` per vertex; layout
setting Force (cose). "First frame" is from adding the elements to the second animation frame after
the layout (includes the layout); "longest task" the longest main-thread block while loading;
zoom/pan frame times during 60 wheel steps and a 30-step drag (p90 / max).

| Elements (vertices + edges) | Before: first frame | Before: longest task | Before: zoom p90 / max | After: first frame | After: longest task | After: zoom p90 / max |
|---|---|---|---|---|---|---|
| 300 (100 + 200) | 175 ms (cose 108 ms) | 114 ms | 10 / 21 ms | 165 ms (cose 108 ms) | 114 ms | 10 / 18 ms |
| 1000 (333 + 667) | 1303 ms (cose 1161 ms) | 1177 ms | 14 / 18 ms | 134 ms (grid) | 81 ms | 10 / 27 ms |
| 3000 (1000 + 2000) | 7107 ms (cose 6740 ms) | 6800 ms | 20 / 42 ms | 250 ms (grid) | 148 ms | 10 / 69 ms |

Each option measured alone at 10 000 elements (grid layout, everything else at full detail; first
frame / zoom p90 / max): full detail 947 ms / 16.5 / 141 ms; `min-zoomed-font-size` 840 ms / 52 /
110 ms; no text outline 874 ms / 10.5 / 269 ms; haystack edges 681 ms / 11.1 / 248 ms; no arrows
833 ms / 11.7 / 168 ms; texture while moving 857 ms / 8.8 / 133 ms; all (the `minimal` plan) 540
ms / 8.8 / 403 ms. Cose alone: 200 vertices (400 iterations) 259 ms, 101 vertices 77 ms. At 5000
elements the `minimal` plan draws its first frame in 311 ms.

Engine layout, measured once (GR2, 2026-10-08) on an Apple M3, a generated clustered graph (n
vertices in clusters of 50, 2n edges, 95 % inside a cluster; vertices + edges of the drawing),
cold (no cache), the default iterations; "wasm" is the release build in the playground's worker
in headless Chrome (`runEngineLayout` from the page: wall time, slices of 50 ms), "dev server"
the release CLI over HTTP, "native" the session's layout alone. The longest gap between animation
frames on the page while a layout ran was 9-11 ms in every case (the main thread stays free).

| Vertices (edges) | Iterations | wasm (worker) | dev server (HTTP) | native |
|---|---|---|---|---|
| 1000 (2000) | 500 | 342 ms | 266 ms (1861 edges: the first 1000 vertices of the 10 000 graph) | 188 ms |
| 10 000 (20 000) | 300 | 2.43 s | 1.55 s | 1.51 s |
| 50 000 (100 000) | 200 | 8.83 s | - | 5.52 s |

Tiers, measured once (GR3, 2026-10-08) on an Apple M3 in headless Chrome (ANGLE/Metal, 120 Hz
animation frames, so one frame is 8.3 ms), WebAssembly mode, release build, the same generated
clustered graphs (n vertices, 2n edges), a 1200 x 800 drawing, the engine's force layout.
"First frame": from `setData` to the second animation frame after `setPositions` (the layout
computed before; Fast's one-time library load, 136-143 ms, not included). "Pan" / "zoom": frame
times during a 40-step drag and 40 wheel steps (p90 / longest).

| Elements (vertices) | Detailed: first frame | Detailed: pan p90 / max | Detailed: zoom max | Fast: first frame | Fast: pan p90 / max | Fast: zoom max | Engine layout (wasm) |
|---|---|---|---|---|---|---|---|
| 1 632 (560) | 155 ms | 9.2 / 16.8 ms | 33.4 ms | - | - | - | 180 ms |
| 1 953 (667) | 177 ms | 9.1 / 24.9 ms | 9.4 ms | 30 ms | 9.0 / 9.4 ms | 9.4 ms | 213 ms |
| 2 342 (800) | 166 ms | 8.6 / **108 ms** | 9.4 ms | 41 ms | 9.1 / 9.4 ms | 9.4 ms | 257 ms |
| 3 000 (1000) | 192 ms | 8.8 / **142 ms** | 16.9 ms | 39 ms | 8.6 / 9.3 ms | 9.4 ms | 310 ms |
| 5 001 (1667) | 224 ms | 9.3 / **217 ms** | 9.4 ms | 41 ms | 8.7 / 9.3 ms | 9.4 ms | 338 ms |
| 10 002 (3334) | 429 ms | 9.2 / **441 ms** | 24.9 ms | 46 ms | 9.2 / 16.6 ms | 9.4 ms | 725 ms |
| 50 001 (16 667) | - | - | - | 90 ms | 8.4 / 9.3 ms | 9.4 ms | 2.73 s |
| 100 002 (33 334) | - | - | - | 215 ms | 9.1 / 16.7 ms | 17.5 ms | 5.73 s |

Detailed pans without a hitch below 2000 elements; from 2000 its `reduced` plan (edges replaced by
a texture while the view moves) costs one long frame when a drag starts (108-441 ms), growing with
the size. Fast stays within one or two frames at every size up to 100 000 elements and draws its
first frame 4-9x sooner. Hence Fast from 2000 elements (where Detailed's `reduced` plan begins),
back to Detailed below 1600. In the panel, a 50 001-element graph (the whole graph with raised
limits) was drawn in Fast 3.1 s after the load, nearly all of it the engine layout; clicking a
vertex showed its properties, "Open in Detailed" drew its neighbourhood with Cytoscape, and the
toolbar's Stop during the layout of 100 002 elements kept the positions after 31 of 200
iterations without restarting the engine.

View selector and defaults, measured once (GR4, 2026-10-08) on an Apple M3, headless Chrome, the
release WebAssembly build in the worker, the `large` sample graph (111 110 vertices, 111 100
edges, a tree with one label per vertex). "View" is `engine.graphView` from the page (session +
JSON + transfer to the page), "layout" the engine's force layout of that view (cold), "result"
`execute('g.v().limit(n).out_e()')` with the subgraph limit raised to 200 000 (whole response:
run, display, subgraph, transfer).

| What | Vertices + edges | Columns (JSON) | View / response | Engine layout |
|---|---|---|---|---|
| whole graph, limits 1000 / 2000 | 1000 + 990 | 45 KB | 7 ms | 0.30 s |
| whole graph, limits 5000 / 10 000 | 5000 + 4990 | 251 KB | 11 ms | 1.09 s |
| whole graph, limits 10 000 / 20 000 (default) | 10 000 + 9990 | 519 KB | 19 ms | 2.23 s |
| whole graph, limits 25 000 / 50 000 | 25 000 + 24 990 | 1.4 MB | 34 ms | 4.11 s |
| whole graph, limits 50 000 / 100 000 | 50 000 + 49 990 | 2.8 MB | 60 ms | 8.41 s |
| result subgraph | 50 010 + 50 000 | 2.8 MB | 210 ms (engine 164 ms) | - |
| result subgraph (the 200 000 maximum) | 100 005 + 99 995 | 5.9 MB | 407-429 ms (engine 305-318 ms) | - |
| label meta-graph (`metaGraph`) | 41 nodes, 69 pairs | - | 39 ms | Detailed, cose |
| neighbourhood, depth 5, limit 10 000 | 10 000 (+1111 left out counted) | - | 19 ms | - |
| label sample (1 of 111 110 labels) | 1 | - | 3-6 ms (to the notice: 5 ms) | - |

The browser check on the same graph (wasm and dev server): Whole graph shows the overview with
the size note; Labels draws 41 label nodes and 69 pairs; a double click on a label draws its
sample (1 vertex, "10 more edges lead out"); a double click on that vertex expands it from the
engine (11 vertices, the vertex kept its position); the centre `V_1_0` at depth 1 / 2 / 2 out /
2 in gives 12 / 121 / 111 / 2 vertices; depth 5 with the vertex limit lowered to 500 says "left out
10,611 vertices within this depth"; a 40 010-element query result is drawn in Fast.

## Architecture: the engine boundary

The UI talks to the engine **only** through the interface in `js/engine/engine.js`. All methods are
async; the engine answers from its Web Worker, so a long query never blocks the page:

| Method | Returns |
|---|---|
| `info()` | `{name, version, memoryTracked}` |
| `listSampleGraphs()` | `[{kind, title, description, available}]` (`empty`, `modern`, `large`, `tree`) |
| `loadGraph({kind, file?, schemaFile?})` | `{name, kind, vertexCount, edgeCount, schemaMode, warnings}`: a graph file starts WITHOUT the previous graph's schema; the optional `schemaFile` (JSON, not with a `.gsnap`) is set on the new empty graph first and the data imported against it as one unit (wasm `loadGraphsonBytes`/`loadGraphmlBytes(bytes, name, schema?)`) |
| `graphInfo()` | the same, for the loaded graph |
| `execute(script, {profile, profile_types, max_rows, max_items, no_limit, spelling, subgraph_max_elements})` | `{ok, terminal, implicit, mutated, changes:{data, schema}, execution, display:{table, json, raw, notice, total_rows}, subgraph, preferred_view, elapsed_ms, graph}` |
| `schema()` | `{mode, source, table, json, mermaid, graph:{nodes, edges}, truncated, total_labels}`: the stored schema (`source: "stored"`), else the inferred one (`mode`/`source: "inferred"`); `table` has one row per property path. Only the Schema panel's drawings (Graph, Mermaid) use it; the editor uses the seven methods below |
| `graphView({maxNodes, maxEdges})` | the drawing as columns, without properties (see [Graph drawing](#graph-drawing)): `{vertexIds, vertexLabel, vertexName, edgeIds, edgeSource, edgeTarget, edgeLabel, labels, truncated, totalNodes, totalEdges}` |
| `neighbourhood(request)` | `{ids, hops?, direction?, keep?, maxNodes?, maxEdges?}` (`hops` the depth, at most 5; `direction` `both`/`out`/`in`): the drawing's columns plus `seeds`, `missing`, `hiddenEdges`, and `leftOut: {vertices, edges, complete}` when the limits cut it (wasm `neighbourhood`, dev server `POST /api/neighbourhood`) |
| `labelSample(request)` | `{vertices: {labels, exclude?}}` or `{edge: {label, from?, to?}}`, with `maxNodes`/`maxEdges`: the first matching elements as columns plus `sample: {kind, shown, total, complete}` (wasm `labelSample`, `POST /api/sample`) |
| `metaGraph()` | `{labels: [{label, count}], other, edges: [{label, from, to, count}], otherPairs, otherEdges, labelTotal, vertexCount, edgeCount, scannedEdges, complete}` (wasm `metaGraph`, `GET /api/meta-graph`) |
| `element(kind, id)` | `{kind, id, label, labels?, source?, target?, properties}` of one vertex or edge (`kind` `'vertex'`/`'edge'`), or `null` when it no longer exists: the inspector's data, fetched on click |
| `elementForEdit(kind, id)` | the property editor's view: `{kind, id, label, labels, source?, target?, properties: [[key, typed]], readOnly}` with every value in the TYPED form `{type, value}` (`int64` as decimal text, `uuid` canonical text, `object` as `[key, typed]` pairs; graphersal-session `edit.rs`) and `readOnly` the reason the graph cannot be written now, or `null` (wasm `elementForEdit`, `GET /api/element/edit`) |
| `updateElement(request)` | `{kind, id, set: [[key, typed]], remove: [key], validateOnly?}`: the changed and removed top-level properties written as ONE unit (a dry run with `validateOnly`); `{ok: true, changed, element}` or `{ok: false, errors: [{field, message, help?}]}` (`field` the key or `general`; any failure refuses the whole update) (wasm `updateElement`, `POST /api/element/update`) |
| `labelChoices(request)` | `{kind: 'vertex'}` or `{kind: 'edge', from, to}`: the labels the edit mode offers, `{kind, mode, free, labels: [{label, declared, count, connects}], readOnly}` (graphersal-session `create.rs`; wasm `labelChoices`, `POST /api/element/labels`) |
| `createElement(request)` | `{kind: 'vertex', labels, id?, properties: [[key, typed]], validateOnly?}` or `{kind: 'edge', label, from, to, id?, properties, validateOnly?}`: ONE unit (a dry run with `validateOnly`, no automatic id used up); `{ok: true, created, element, drawn: {kind, id, label, caption, source?, target?}}` or `{ok: false, errors: [{field, message, help?}]}` (`field` a key, `label`, `id`, `from`/`to`, `general`) (wasm `createElement`, `POST /api/element/create`) |
| `deleteElement(request)` | `{kind, id, validateOnly?}`: a vertex with its edges, or an edge, as ONE unit; `{ok: true, deleted, edges}` or `{ok: false, errors}` (wasm `deleteElement`, `POST /api/element/delete`) |
| `layoutStart(input, options)`, `layoutStep(job, budgetMs)`, `layoutCancel(job)` | the engine's force layout of a drawing, slice by slice (see [Graph drawing](#graph-drawing); driven by `js/graph/layout-client.js`): `input` `{vertexCount, edgeSource, edgeTarget}` (a view's index columns), `options` `{budgetMs?, seed?, iterations?}`; each answers `{job, state: 'running'\|'done'\|'cancelled', iterations, totalIterations, progress, elapsedMs, vertexCount, cached}` plus `positions` (a `Float32Array`, `x, y` per vertex) once the job is no longer running (wasm `layoutStart`/`layoutStep`/`layoutCancel`/`layoutPositions`, dev server `POST /api/layout`, `/api/layout/step`, `/api/layout/cancel`) |
| `completions()` | `[{name, aliases, receiver, arities, signatures, doc}]` |
| `tokenValues()` | `{class: [member, ...]}`: the values of the DSL's token classes (`Order` -> `asc`, `desc`, `shuffle`; `GType` -> `LONG`, ...), from the getters the engine registers, only the classes a script's scope defines (wasm `tokenValues`, `GET /api/token-values`) |
| `memoryUsage()` | `{structure_bytes, index_bytes, property_bytes, total_bytes, total_mb, compressed_values, compressed_plain_bytes, compressed_stored_bytes, compression_dictionary_bytes}` |
| `statistics()` | `{statistics, memory, limits: {presets, defaults}}`: the Statistics tab of the Profile panel (loaded when the tab is shown; reloads while shown after a run with `changes.data`, and on the panel refresh). `statistics` is `g.statistics()` (labels by count, at most 50 per kind), `memory` the footprint, `limits.presets` the host's presets (`{options, scriptLimits, scriptTimeoutMs}`, as the dev server's `/api/info`), `limits.defaults` the engine's default of every traversal limit no preset sets. Limits are not part of `GraphStatistics` |
| `saveGraph(format)` | `{format, text, schema, warnings}`: the graph as GraphSON (`format` `'graphson'`, the default; wasm `saveGraphson`) or GraphML (`'graphml'`; wasm `saveGraphml`) text and the stored schema as JSON text (`null` without one) |
| `loadSchema(file)` | graph info: sets the CURRENT graph's schema from a JSON file in the schema format (wasm `setSchema(json)`; an old-format file fails with the located error); a graph file loaded afterwards does not keep it |
| `getSchema()`, `setSchema(schema, mode?)`, `patchSchema(patch)`, `validateSchema(schema)`, `validateSchemaPatch(patch)`, `inferSchema()`, `diffSchema(schema)` | the seven schema methods (wasm `getSchema`, ...): schemas and patches as objects or JSON text; `getSchema` is `null` without a stored schema |
| `listQueries(folder?)`, `getQuery(name)` | the saved queries `[{name, folder, description, params, status, error?}]`; one with its `body` (or null) |
| `defineQuery(spec)` | `{ok, query}` or `{ok: false, errors: [{field, message, help?}]}`: the editor's Save (`replaces` renames) |
| `dropQuery(name)`, `moveQuery(name, folder)` | `{dropped}`; the moved entry |
| `queryCallText(name, args)` | the call `g.query("name", #{..})` the run form writes into a tab |
| `saveCatalog()`, `loadCatalog(file)` | the catalog file (JSON text of every kind: saved queries, compression rules); `{loaded, graph}` |
| `listCompressions()` | the compression rules `[{name, element, label, path, codec, minBytes, dictionary, stats: {compressedValues, plainBytes, storedBytes, dictionaryBytes}}]` |
| `defineCompression(spec)` | `{ok, rule}` or `{ok: false, errors: [{field, message, help?}]}`: the Compression form's Save (`replaces` renames) |
| `dropCompression(name)`, `recompress(name)` | `{dropped}`; the rule's entry after a new dictionary and re-encode |
| `stop()` | `{restored}`: restarts the engine, restores the graph |

The worker calls the `Playground` class of `crates/graphersal-wasm` (its JS API is internal to the
playground; a designed npm package is planned). Structured results cross the
boundary as JSON text and are parsed in the worker.

`execute` runs a traversal left without a terminal through the engine's `execute(opts)` terminal,
so one run returns both results and metrics; explicit terminals (`to_list()`, `next()`,
`profile()`, `to_table()`, `to_json()`, `execute(...)`) are honoured and reported in `terminal`.
`execution` is the engine's own object in its Rust map/JSON shape (snake_case; keys in the
engine's order):

```
Execution        { results, error: null | {message, help?, step_id?, step_path?: [{id, name}], diagnostic, position?},
                   profile: null | TraversalMetrics }
TraversalMetrics { duration_ns, metrics: [Metrics], optimizer_rules_applied: [string] }
Metrics          { id, name, duration_ns, percent_duration, counts: {traverser_count, element_count},
                   calls, count_in, nested: [Metrics],
                   timing?: {min_duration_ns, max_duration_ns}, loops?: {total_loops, max_depth, avg_loops_per_traverser},
                   path_recording?: "full" | "labels(a,b)",
                   memory?: {bytes_materialized, bytes_retained, alloc_count, source: "estimated"|"tracked"} }
```

**Extensibility rule:** a future ProfileType adds a new optional section to each Metrics (as
`memory` did); existing fields never change. An optional section is absent (no key), never null;
the UI ignores unknown keys and renders a section only when present: `timing` as the Time cell's
min/avg/max tooltip, `loops` as a line under a `repeat()` step, `path_recording` as the
` [path: ..]` suffix of the step name (as in the CLI table). Step ids are static positions in the
optimized plan: `"3"` for a top-level step, `<step>.<child>.<step>` inside child traversals. A
runtime error carries `step_id` and `step_path`: the Results error view shows the path as a
breadcrumb, and with a (partial) profile the Profile panel marks the failing row and its
ancestors. A script error (parse error, unknown function) carries a 1-based `position` ("Go to
line …"). `diagnostic` is the full text the CLI prints.

`spelling` (`"snake"`, the default, or `"camel"`; the Settings dialog's "Step names") is the
session's `render.spelling`: how the profile and error messages spell the steps.

`execute` never rejects for a failing query; the failure is `execution.error`. It rejects only
when the engine itself fails or the run is stopped. `execution.results` is the full data (never
cut); `display` is its rendering, bounded like the CLI (100 rows, 100 nested items by default;
`g.with("render.max_rows", n)` and `to_table(#{max_rows: n})` override one query); `display.raw`
is the engine's own visualizer output, the same text the CLI prints. The Query toolbar's
**Profile** (default on) and **Memory** (default off) toggles set `profile` and
`profile_types: ["memory"]` for every run. Memory figures are exact (`"tracked"`): the module
installs the library's counting allocator as its global allocator.

**Mutations.** A run is one unit of atomicity: every statement and the implicit execution of a
trailing traversal are savepoints of it, and it commits only when the response is `ok`. A run that
reports an error (also an error a statement before it did not cause, or one returned as an
`execute()` result) leaves nothing behind. `mutated` is exact: the run committed a change to the
graph (the graph's commit sequence number, `last_commit_seq()`, advanced while it ran); a failed,
rolled-back run does not count, and a run that changed nothing (a `drop()` over an empty set) does
not either. The Results panel's refresh asks for confirmation when it is
set, and the Graph and Schema panels reload.

**Permissions.** The library's script entry allows everything by default; the playground allows
everything except file access (`AccessPolicy::allow_all().deny_resource(ResourceKind::File)`,
`playground_authorizer()` in `crates/graphersal-session/src/session.rs`; the dev server uses it too). The browser has no file system, so `export_graphml`,
`import_graphml`, `export_graphson`, `import_graphson`, `GraphSource::file(..)` and Rhai's `import` fail with a
`PermissionDenied` diagnostic; graphs are loaded and saved through the page. See the book page
"Permissions".

**Stop.** The Stop button next to Run terminates the worker (the only way to interrupt a running
WebAssembly call), starts a new one and restores the graph from the *restore point*: the sample
graph or graph file (GraphSON or GraphML) loaded last, or the text of the last **Save graph**,
whichever came later, each with the schema it had (its schema file, set later with **Load
schema**, or saved with the graph; for a graph file the schema is the load's schema argument, set
before the import). Changes made by queries after that point are lost; script variables never survive a run anyway
(every run gets a fresh scope). An engine trap (out of memory, a bug) restarts the worker the
same way.

**Save ▾** (top bar; the same entries as buttons on the start page when a graph is loaded)
downloads ONE file per entry: the data as GraphSON 3.0 (`graphersal-graph-YYYYMMDD-HHMMSS.json`)
or GraphML (`.graphml`), the schema (`graphersal-schema-YYYYMMDD-HHMMSS.json`; neither data
format carries it), or a packed snapshot (`.gsnap`, data, schema and catalog). The catalog
(saved queries, compression rules, property indexes, every kind) is ONE file of its own, in the **Catalog ▾**
dropdown: **Save catalog (.json)** (`graphersal-catalog-YYYYMMDD-HHMMSS.json`, the catalog JSON) and
**Load catalog…** (also on the start page), which adds the file's definitions to the current graph
(a compression rule compresses its values at once); a graph file loaded afterwards keeps them. One
click, one download:
browsers block or drop the second of two downloads started by one click. **Load from file** reads GraphSON (`.json`, `.jsonl`,
`.graphson`; wasm `loadGraphsonBytes`) and GraphML (`.graphml`, `.xml`; wasm `loadGraphmlBytes`)
through a dialog (`components/start-page.js`, model `js/load-dialog.js`, tested in
`tests/load-dialog.test.mjs`) with the graph file and an OPTIONAL **Schema file (JSON)**; the loaded
graph always starts WITHOUT the previous graph's schema (the catalog is kept). With a schema file the
schema is set on the new empty graph first and the data imported against it as one unit (the wasm
load's third argument): that restores vertices, edges, ids, labels, properties, the schema and its
mode, and an `open`/`closed` violation fails the load (nothing changes). A drop of ONE file on the
card loads it without a schema; a graph file + a `.json` schema dropped together pair up when the
graph file is not `.json` itself (`droppedFiles`). What a GraphSON import skips (dates, unsupported
types, dangling edges) comes back in the graph info's `warnings` and is shown as a notice. **Load
schema** (start page, next to "Load from file") applies a schema file to the current graph only.
GraphSON keeps every value type by itself; GraphML gets the declared types (`uuid`, `array`,
`object`) back from the schema. Save checks the files by loading them back the way Load from file
does (schema first, then the data) and warns about what does not survive (for GraphML: properties
whose type comes back as a string without a declaration, an undeclared `uuid`, `array`/`object`
values, a key with different types on different labels).

**The Catalog** (the top bar's **Catalog ▾**, `components/catalog.js`; models `js/compression.js`
and `js/indexes.js`, tested in `tests/compression.test.mjs` and `tests/indexes.test.mjs`): one
item per catalog kind with its count from the graph info's `catalog` (`CATALOG_KINDS`, plus the
property indexes appended by `catalogMenuItems` of `js/indexes.js`),
each opening its manager through `openCatalog(id)` (`state.catalogDialog`, the workspace hosts
`CatalogDialogs` once), plus **Save catalog (.json)** and **Load catalog…**. The **Saved queries**
manager lists the queries by folder with Run, Edit, Delete and New; the **Compression** manager
lists the rules with their stats (values, plain → stored, ratio, saving, dictionary) with Edit,
Recompress and Drop, and the rule form (name, element, label with the graph's labels from
`statistics()` as a datalist, path, minimum size, dictionary) shows the engine's field errors in
place; the **Property indexes** manager lists the indexes with their type and stats (entries,
distinct values, elements per value, memory, "not served" with the engine's `reason`,
`indexNotServedReason`) with Drop (confirmed), and the create form (element, label with the same
datalist, key, type from `INDEX_TYPES` with its description as the tooltip, optional name with
`defaultIndexName` as its placeholder) shows the engine's field errors in place (an unknown type
under Type, a key the schema declares as a vector, a list or a map under Key; engine
`listIndexes`/`createIndex`/
`dropIndex`). The Store menu shows the catalog per kind (`definitionsByKind`). The Statistics tab
adds the compressed strings and dictionaries, and the property indexes' memory, to its memory
part when there are any.

**Saved queries** (the **Catalog ▾ ▸ Saved queries** manager, `components/catalog.js`; the dialog
`components/saved-queries.js`; models in `js/saved-queries.js`): the tree by folder (descriptions
and signatures, `error` when a body no longer compiles) with Run, Edit, Delete and New; the editor
dialog (name, folder, description, parameters with type,
constraints, default and description, add/remove/reorder; the function's body in a CodeMirror
editor with the DSL completion, the `fn name(params) {` line written from the parameter list; a
stored text with comments before `fn` is edited raw); Save shows the engine's field errors in place;
Delete asks first. The run form is built from the parameter schemas and WRITES the call into a tab
(`openScript`, then `run`), so it is visible and in the history; the last values per query are kept
in `localStorage` (`savedQueryValues`). `g.query("` completes the names, grouped by folder. The list
reloads with `graphVersion` (a load, a run that changed the graph, a change feed notice). In the
WebAssembly engine the restore point of Stop keeps the catalog the page loaded or edited
(`catalogJson`, replayed by `loadCatalog(.., replace)` after the graph and the schema).

**Completions** come from the Rhai engine's own function metadata (`engine.completions()`): every
registered function per receiver (`traversal`, `P`, `TextP`, `By`, ..., `global` for free functions
such as `jpath(..)`), all spellings merged, the overloads as parameter lists with types, and the
function's doc: one sentence and a runnable example with its result, from the engine's doc table
(`crates/graphersal/src/script/register/docs.rs`; a test fails when a registered function has none).
The values after a token class (`T.`, `Order.`, `GType.`, `Pop.`, ...) come from the engine too
(`engine.tokenValues()`, the getters it registers; the CLI REPL completes from the same source,
`graphersal_session::views::token_values`), so there is no list in the playground: they are offered as
constants, with the doc of a same-named function when the class has one (`V.Table`), followed by the
class's other functions (`By.Property(..)` after `By.Id`, `By.Label`, ...). `js/editor/completion-data.js`
loads both (`loadCompletionData`, a failing call leaves its part empty) and builds these choices without
CodeMirror (node tests `tests/completion-data.test.mjs`). The highlighter colours the token classes from
`js/editor/token-classes.js`: a fixed fallback list (used until the engine's data is loaded) to which
`loadCompletionData` adds every class `tokenValues()` reports (`learnTokenClasses`, node tests
`tests/token-classes.test.mjs`).
The completion list and its doc panel are never clipped by a panel: every editor with completion
(the Query panel, the saved-query dialog) uses `editor/tooltips.js`, which renders CodeMirror's
tooltips with `position: fixed` into the document body, or into the modal `<dialog>` the editor sits
in (a modal dialog is in the top layer, so a body-parented tooltip would be behind it); the z-index
(`.cm-tooltip` in `css/app.css`) is above the panels and menus.

The **status bar** under the Query editor is always one line: the run status (`Run #3 · 6 results ·
1.3 ms`, truncated with an ellipsis), the key hints (shrink first, hidden when the Query panel is
narrower than 640 px) and the cursor position (`Ln 1, Col 30`, compact `1:30` below 520 px, the
full text as tooltip); the widths are container queries on the panel. All shortcuts, Alt+N
included, are listed in the help dialog.

**Run the selection.** Run (button and `Ctrl`/`⌘`+`Enter`) executes the editor's main selection when
it holds more than whitespace, else the whole tab (`js/editor/run-selection.js`, pure, tested in
`tests/run-selection.test.mjs`). The button then reads "Run selection" (its tooltip says that with
several selections the main one runs), the result carries `selection: {lines, offset}`, shown as
`Run #3 · selection (2 lines) · …` in the status bar and "ran the selection (N lines)" in the Results
header. The engine sees the text trimmed, so `run()` (`js/store.js`) moves the error's `position`
and the `(line L, position C)` in its message and diagnostic by the offset of the first non-blank
character of what ran (also the leading blank lines of a whole-tab run); the Results and Profile
refresh rerun the same selection with the same offset. Run all tabs runs whole tabs.

## Libraries (CDN, pinned)

| Library | Version | Source | Loaded as | Why |
|---|---|---|---|---|
| Vue | 3.5.43 | cdnjs `vue.esm-browser.prod.min.js` | ES module (import map) | Components with template strings compiled in the browser, no build step |
| CodeMirror 6 (`@codemirror/state` 6.7.6, `view` 6.43.13, `language` 6.12.4, `autocomplete` 6.20.3, `commands` 6.11.1, `search` 6.5.11; `@lezer/highlight` 1.2.5, `@lezer/common` 1.5.3, `style-mod` 4.1.4, `w3c-keyname` 2.2.8, `crelt` 1.0.7, `@marijn/find-cluster-break` 1.0.4) | see left | jsDelivr, original `dist` ES modules | ES modules (import map) | Editor |
| Split.js | 1.6.5 | cdnjs | classic script (`Split`) | Resizable panels |
| Cytoscape.js | 3.34.3 | cdnjs | classic script (`cytoscape`) | Graph and schema drawing (Detailed tier) |
| sigma.js | 3.0.3 | cdnjs `sigma.min.js` (UMD) | classic script (`Sigma`), injected on first use with SRI (`js/graph/sigma-renderer.js`) | Fast tier (WebGL) |
| graphology | 0.26.0 | cdnjs `graphology.umd.min.js` | classic script (`graphology`), injected on first use with SRI | The Fast tier's graph model |
| Mermaid | 11.17.2 | jsDelivr `mermaid.esm.min.mjs` | dynamic `import()` on first use | Schema diagram tab |

Integrity: the classic scripts carry cdnjs's SRI hashes (also the two the Fast tier injects, pinned
in `FAST_LIBRARIES`); the import map has an `integrity` section
with the sha384 of every mapped module (browsers that do not support import-map integrity ignore
it). Mermaid is imported lazily and split into chunks, so it has no SRI.

**Why CodeMirror 6 instead of Monaco.** CodeMirror is far smaller than Monaco (no web workers, no AMD loader), themes through
CSS custom properties (light/dark without reconfiguration), works on touch devices, and its
completion API takes a plain function, which is all the step catalogue needs. It is loaded as its
original ES modules with every bare import mapped to one pinned URL, because CodeMirror breaks when
two copies of `@codemirror/state` are loaded (which jsDelivr's `+esm` bundles would cause, since each
package pins different dependency versions). To upgrade, change the versions in the import map and
recompute the hashes, for example:

```bash
curl -s https://cdn.jsdelivr.net/npm/@codemirror/view@X.Y.Z/dist/index.js | openssl dgst -sha384 -binary | openssl base64 -A
```

**Why Split.js instead of dockview.** The workspace has a fixed arrangement (query and results on
the left; graph, schema and profile on the right) where each panel can be resized, collapsed to its
header, or hidden. Split.js does exactly that in about 2 KB (gzipped) without a framework adapter or build step;
dockview's free docking and tab dragging would need its Vue adapter (an SFC build) or a large
vanilla integration for features this page does not need.

## Persistence

Theme, display limits, the query tabs (`tabs`: names, scripts, run options, example provenance, the
active tab; never results), the query history (`history`, 100 entries), the last sample graph
(`lastGraph`), panel visibility, collapse state and sizes (`layout`), and a few view choices
(`graph.*`, `schema.tab`, `export.transparent`) are stored in `localStorage` under
`graphersal.playground.*`. The single-editor keys `script` and `runOptions` of earlier versions
seed the first tab once and are then removed. `reset/` clears all of them except `history`.
Every access is wrapped in try/catch, so the page works the same with storage disabled (settings
then last for the page view only). Graphs are not stored in the browser: a reload on `#/play`
rebuilds the last sample graph (without the changes queries made) or returns to the start page
after a file; use **Save graph** to keep a graph and "Load from file" to bring it back.
