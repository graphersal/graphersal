// The engine boundary of the playground.
//
// The UI talks to the graph engine ONLY through the interface documented here. There are two
// implementations, and `index.js` picks one at start:
// - `wasm-engine.js`: the Graphersal library compiled to WebAssembly (crates/graphersal-wasm),
//   running in a module Web Worker (`worker.js`); the static playground;
// - `http-engine.js`: the CLI's DEV server (`graphersal --graph <src> --server`, unstable, local
//   only), chosen when `GET /api/info` on the serving origin answers as a Graphersal dev server.
// Both run the same Rust session code (crates/graphersal-session), so the answers have the same
// shapes. Every method is async.
//
// The shapes below are the engine's own (snake_case, the Rust map/JSON shape of `Execution` and
// `TraversalMetrics`); see `book/src/users_guide/profiling_and_execute.md` and book/src/users_guide/
// profiling_and_execute.md. When this file and the Rust code disagree, the Rust code wins.

/**
 * @typedef {Object} SampleGraph
 * @property {string} kind         'empty' | 'modern' | 'large' | 'tree' (the argument of `loadGraph`).
 * @property {string} title        Display name.
 * @property {string} description  One or two sentences for the start page.
 * @property {boolean} available   False when this engine cannot build it.
 * @property {string} [reason]     Why it is unavailable.
 */

/**
 * @typedef {Object} GraphInfo
 * @property {string} name          Display name of the loaded graph.
 * @property {string} kind          'empty' | 'modern' | 'large' | 'tree' | 'file'.
 * @property {number} vertexCount
 * @property {number} edgeCount
 * @property {string|null} schemaMode  'none' | 'open' | 'closed' when the graph stores a schema.
 * @property {number} [savedQueries]  How many saved queries the graph's catalog holds.
 * @property {string[]} [warnings]  Non-fatal notes from loading.
 */

/**
 * @typedef {Object} ExecuteOptions
 * Options of `execute(script, opts)`. Keys are snake_case like the engine's.
 * @property {boolean} [profile=false]        Collect TraversalMetrics for a traversal without a terminal.
 * @property {string[]} [profile_types=[]]    Extra metric sections, e.g. ['memory'] (ProfileType.Memory).
 * @property {number} [max_rows=100]          Display limit: top-level results shown (data is never cut).
 * @property {number} [max_items=100]         Display limit: items shown of every nested collection.
 * @property {boolean} [no_limit=false]       Display everything.
 * @property {string} [spelling='snake']      Step names in the profile and errors: 'snake' or 'camel'.
 * @property {number} [subgraph_max_elements=50000]  Most vertices + edges of the response's
 *   `subgraph` (the Graph panel's "Query result"; at most 200 000).
 */

/**
 * @typedef {Object} TableData
 * @property {string[]} columns
 * @property {Array<Array<*>>} rows  Cell values: scalars, or nested arrays/objects (rendered as JSON).
 */

/*
 * ---------------------------------------------------------------------------------------------
 * EXTENSIBILITY RULE. Every key is snake_case. A future ProfileType adds a NEW optional section
 * to each Metrics (as `memory` did); existing fields never change meaning or shape. Consumers
 * (this UI included) must ignore unknown keys and render an optional section only when it is
 * present: an optional section is ABSENT (no key), never null. A profile may be present together
 * with `error`: it is the partial profile up to the failure, and `error.step_id` (same numbering
 * as `Metrics.id`) marks the failing step.
 *
 * STEP IDS. Static positions in the optimized plan, not per-traverser occurrences. A top-level
 * step is its index ("3"); a step inside a child traversal alternates step index and child
 * traversal index: `<step>.<child>.<step>...` ("3.1.0" = top-level step 3, its child traversal 1,
 * step 0 in it). Children of a step are numbered in order: arguments first (union/coalesce/choose
 * branches, where/local/repeat bodies, ...), then modulator arguments (by(...), ...).
 * ---------------------------------------------------------------------------------------------
 */

/**
 * @typedef {Object} EngineError
 * The engine's error map.
 * @property {string} message   One line, plain text.
 * @property {string} [help]    The engine's `help()` text: how to fix the query.
 * @property {string} diagnostic The full rendered diagnostic (what the CLI prints).
 * @property {string} [step_id] Runtime errors: the failing step, numbered like `Metrics.id`.
 * @property {Array<{id:string, name:string}>} [step_path]  Runtime errors: from the outermost step
 *   to the failing one, e.g. [{id: "1", name: "union(...)"}, {id: "1.1.1", name: "property(\"x\")"}].
 * @property {{line:number, column:number}} [position]  Script errors (parse, unknown function):
 *   1-based position in the script, when Rhai knows it.
 */

/**
 * @typedef {Object} MemoryMetrics
 * Optional section of Metrics, present with profile_types ['memory'].
 * @property {number} bytes_materialized   Heap bytes the step newly owns.
 * @property {number|null} bytes_retained  Net retained bytes; null unless `source` is "tracked".
 * @property {number|null} alloc_count     Allocator calls; null unless `source` is "tracked".
 * @property {'estimated'|'tracked'} source  "tracked" in the browser build (the module installs
 *   the counting allocator), "estimated" (size_of based) where no allocator hook is installed.
 */

/**
 * @typedef {Object} Metrics
 * One step of the optimized plan.
 * @property {string} id                   Static plan position ("3", "3.1.0"; see STEP IDS).
 * @property {string} name                 Plain step text as in the optimized plan, e.g. `v(labels: ["person"]).count()`.
 * @property {number} duration_ns
 * @property {number} percent_duration     Share of the total duration.
 * @property {{traverser_count:number, element_count:number}} counts
 *   traverser objects out, and the logical traversers they stand for (sum of bulks).
 * @property {number} calls
 * @property {number} count_in
 * @property {Metrics[]} nested            Steps of child traversals (`where`, `by`, `repeat` bodies, ...).
 * @property {{min_duration_ns:number, max_duration_ns:number}} [timing]  Optional: once the step completed a call.
 * @property {{total_loops:number, max_depth:number, avg_loops_per_traverser:number}} [loops]  Optional: `repeat()` only.
 * @property {string} [path_recording]     Optional: the step's path recording mode ("full", "labels(a,b)").
 * @property {MemoryMetrics} [memory]      Optional: with ProfileType Memory.
 */

/**
 * @typedef {Object} TraversalMetrics
 * @property {number} duration_ns
 * @property {Metrics[]} metrics
 * @property {string[]} optimizer_rules_applied  Rules that rewrote the plan (may be empty).
 */

/**
 * @typedef {Object} Execution
 * What the `execute(opts)` terminal returns, in the Rust map shape.
 * @property {Array<*>} results              What `to_list()` would return (data, never cut).
 * @property {EngineError|null} error        A runtime error is part of the object, not a throw.
 * @property {TraversalMetrics|null} profile When requested; partial when `error` is set.
 * Whether the run changed the graph is `ExecuteResponse.mutated`.
 */

/**
 * @typedef {Object} ExecuteResponse
 * The boundary's answer to `execute(script, opts)`: the engine's Execution plus its display.
 * @property {boolean} ok                 `!execution.error`.
 * @property {string} terminal            How the script ended: 'execute' (explicit, or a traversal
 *   without a terminal), 'to_list', 'next', 'iterate', 'profile', 'to_table', 'to_json',
 *   'to_markdown', 'visualize', 'value' (any other script value), 'unknown' (failed before it ran).
 * @property {boolean} implicit           The traversal had no terminal and ran as execute().
 * @property {boolean} mutated            The script committed a change to the graph (exact: the
 *   graph's commit sequence number advanced; a failed, rolled-back statement does not count).
 * @property {{data:boolean, schema:boolean, definitions:boolean}} changes  What the committed run
 *   changed: the data, the stored schema (`set_schema`, `patch_schema`) and/or the catalog of
 *   saved queries (`define_query`, `drop_query`, ...); all false when nothing was kept.
 * @property {{vertices:number, edges:number, text:string}} [change_summary]  Only when the
 *   committed run changed the data: the net vertex and edge count changes (after minus before)
 *   and both as one line (`+3 vertices, +2 edges`; empty when neither count moved).
 * @property {Execution} execution
 * @property {{table:TableData, json:string, raw:string, notice:?string, total_rows:?number}|null} display
 *   `execution.results` rendered within the display limits (null when the script failed). `raw`
 *   is the engine's own visualizer output (what the CLI prints).
 * @property {GraphView|null} subgraph  Elements the results refer to (at most
 *   `subgraph_max_elements`, default 50 000, and a byte bound; `truncated` and `stoppedBy`
 *   'elements' | 'bytes' when there are more), as columns.
 * @property {string} [preferred_view]    'raw' | 'json' when the script asked (to_table/to_json).
 * @property {number} elapsed_ms          Time spent in the engine.
 * @property {GraphInfo} graph            The graph after the run (counts may have changed).
 */

/**
 * @typedef {Object} GraphView
 * What is drawn, as columns (`CompactGraph` of graphersal-session; js/graph-data.js): no
 * properties, the inspector fetches them per element with `element(kind, id)`.
 * @property {string[]} vertexIds
 * @property {Uint32Array} vertexLabel   Per vertex: index of its primary label in `labels`.
 * @property {Array<string|null>} vertexName  Per vertex: its `name` property as text (the caption).
 * @property {string[]} edgeIds
 * @property {Uint32Array} edgeSource    Per edge: index of its out-vertex in the vertex columns.
 * @property {Uint32Array} edgeTarget    Per edge: index of its in-vertex.
 * @property {Uint32Array} edgeLabel     Per edge: index of its label in `labels`.
 * @property {string[]} labels           The label table (vertex and edge labels).
 * @property {boolean} truncated         More exists than is drawn.
 * @property {number} totalNodes
 * @property {number} totalEdges
 * @property {'elements'|'bytes'} [stoppedBy]  A result's subgraph: which limit stopped it.
 */

/**
 * @typedef {GraphView} SelectionView
 * A drawing selected from the graph (`neighbourhood`, `labelSample`): a GraphView whose totals
 * count the whole graph, plus:
 * @property {number[]} seeds        Indices of the vertices the request started from.
 * @property {string[]} missing      Requested ids that name no vertex (any more).
 * @property {number} hiddenEdges    Edges from drawn vertices to vertices not drawn.
 * @property {{vertices:number, edges:number, complete:boolean}} [leftOut]  A neighbourhood the
 *   limits cut: vertices within the depth and edges between drawn vertices that are not drawn.
 * @property {{kind:'vertices'|'edges', shown:number, total:number, complete:boolean}} [sample]
 *   A label sample: how many matching elements are drawn of how many.
 */

/**
 * @typedef {Object} MetaGraph
 * The label meta-graph (`metaGraph()`): vertex labels with their vertex counts (by primary
 * label), label pairs with their edge counts.
 * @property {Array<{label:string, count:number}>} labels  The largest labels (at most 40); '' is
 *   the unlabeled vertices.
 * @property {{labels:number, count:number}|null} other     The remaining labels as one node.
 * @property {Array<{label:string, from:number, to:number, count:number}>} edges  Label pairs (at
 *   most 200, largest first); `from`/`to` index `labels`, `labels.length` is `other`.
 * @property {number} otherPairs @property {number} otherEdges  Pairs (and their edges) not listed.
 * @property {number} labelTotal @property {number} vertexCount @property {number} edgeCount
 * @property {number} scannedEdges @property {boolean} complete  Every edge was counted.
 */

/**
 * @typedef {Object} LayoutStatus
 * A force layout job of the engine (`layoutStart`, `layoutStep`, `layoutCancel`).
 * @property {number} job
 * @property {'running'|'done'|'cancelled'} state
 * @property {number} iterations        Done so far.
 * @property {number} totalIterations   Planned.
 * @property {number} progress          0..1.
 * @property {number} elapsedMs         Time spent in iterations.
 * @property {number} vertexCount
 * @property {boolean} cached           Answered from the engine's cache of finished layouts.
 * @property {Float32Array} [positions] `x, y` per vertex, when the job is no longer running
 *   (centred; the median edge is 70 units long).
 */

/**
 * @typedef {Object} GraphElement
 * One element as the inspector shows it (`element(kind, id)`).
 * @property {'vertex'|'edge'} kind
 * @property {string} id
 * @property {string} label            Primary label ('' when unlabeled).
 * @property {string[]} [labels]       A vertex's full label set.
 * @property {string} [source]         An edge's out-vertex id.
 * @property {string} [target]         An edge's in-vertex id.
 * @property {Object<string,*>} properties
 */

/**
 * @typedef {Object} SchemaView
 * @property {TableData} table   One row per (element kind, label, property path): columns
 *   element, label, property (`address.zip`, `roles[].name`), type, required, nullable,
 *   constraints, notes.
 * @property {string} json       The schema in its JSON format (indented text).
 * @property {string} mermaid    Mermaid source of the schema diagram (nested objects as entities).
 * @property {{nodes:Array<{id:string,label:string,count?:number,properties:Object<string,string>}>,
 *             edges:Array<{id:string,label:string,source:string,target:string,properties:Object<string,string>}>}} graph
 *   The schema as a graph: vertex labels as nodes (with their element counts), edge labels (per
 *   connection) as edges.
 * @property {string} mode       'none' | 'open' | 'closed' (a stored schema), or 'inferred'.
 * @property {'stored'|'inferred'} source Whether the graph stores the schema or it is inferred.
 * @property {boolean} truncated More labels than the views show (the first 300 are shown).
 * @property {number} total_labels
 */

/**
 * @typedef {Object} SchemaValidation
 * What `validateSchema` / `validateSchemaPatch` return; nothing is applied.
 * @property {{mode:string, elements_checked:number, violating_elements:number,
 *             total_violations:number, omitted_violations:number,
 *             groups:Array<{kind:string, element:string, label:string, field:string|null,
 *                           example_field:string|null, detail:string|null, count:number, sample_ids:string[],
 *                           omitted_ids:number}>}} violations
 * @property {SchemaChange[]} diff  The changes from the stored schema.
 * @property {boolean} compatible   Every change is backward compatible.
 */

/**
 * @typedef {Object} SchemaChange
 * @property {string} kind  'mode_changed' | 'label_added' | 'property_added' | 'type_changed' | ...
 * @property {string|null} element  'vertex' | 'edge' (null for mode/meta).
 * @property {string|null} label
 * @property {string|null} path  `$`, `$.address.zip`, `$.tags[*]`, or a connection.
 * @property {string|null} before
 * @property {string|null} after
 * @property {boolean} compatible
 */

/**
 * @typedef {Object} Completion
 * @property {string} name        Preferred spelling (snake_case).
 * @property {string[]} aliases   Other spellings (camelCase, `in_` for `in`, ...).
 * @property {string} receiver    What it is called on: 'traversal', 'P', 'TextP', 'By', 'Map', ...
 * @property {number[]} arities   Registered argument counts.
 * @property {string[]} [signatures] Parameter lists per overload, e.g. `(arg1: array)`.
 * @property {string} doc         The doc comment registered with the function ('' when none).
 */

/**
 * @typedef {Object} MemoryUsage
 * The DSL's `g.memory_usage()` map: an estimate of the graph's own footprint.
 * @property {number} structure_bytes
 * @property {number} index_bytes
 * @property {number} property_bytes
 * @property {number} total_bytes
 * @property {number} total_mb
 */

/**
 * @typedef {Object} StatisticsView
 * What `statistics()` returns (the Statistics tab of the Profile panel). Two sources side by side:
 * the graph's data counts and footprint, and the limits a query runs under.
 * @property {{vertex_count:number, edge_count:number,
 *             vertex_labels:Array<{label:string, count:number, primary:number}>,
 *             edge_labels:Array<{label:string, count:number}>,
 *             vertex_label_total:number, edge_label_total:number, truncated:boolean}|null} statistics
 *   `g.statistics()`: `count` = vertices carrying the label anywhere in their label set, `primary`
 *   = those whose first label it is; ordered by count, at most 50 labels per kind (`truncated`).
 * @property {MemoryUsage|null} memory  The graph's own footprint (`g.memory_usage()`).
 * @property {{presets:{options:Object<string,*>, scriptLimits:Object<string,number>, scriptTimeoutMs:number|null},
 *             defaults:Object<string,*>}} limits
 *   `presets`: what the host applies to every query (the dev server's `--timeout`, `--max-*`, ...;
 *   empty in the WebAssembly playground); `defaults`: the engine's default of every traversal limit
 *   option no preset sets (a query may still set one with `g.with(..)`).
 */

/**
 * @typedef {Object} EngineInfo
 * @property {string} name     'wasm' or 'server' (the dev server).
 * @property {string} version  The engine version.
 * @property {boolean} memoryTracked  Memory profiles report exact ("tracked") figures.
 * @property {boolean} [server]  Dev server mode: one graph (no graph choice), Stop cancels on the
 *   server, `saveToSource()` exists.
 * @property {boolean} [saveable]  Dev server: the graph came from a `--graph` file that
 *   `saveToSource()` writes back.
 * @property {string|null} [source]  Dev server: that file's path.
 * @property {Object|null} [ai]  Dev server started with `--ai FILE`: the AI Chat's `{provider,
 *   model, readOnly, sendsDataTo, maxToolCallsPerTurn}` (never the key); null or absent otherwise.
 */

/**
 * The interface every engine implements. This class is documentation and a guard: its methods
 * reject, so a missing override fails loudly instead of silently returning `undefined`.
 * @abstract
 */
export class Engine {
  /** @returns {Promise<EngineInfo>} */
  async info() { throw notImplemented('info'); }

  /** @returns {Promise<SampleGraph[]>} */
  async listSampleGraphs() { throw notImplemented('listSampleGraphs'); }

  /**
   * Replaces the current graph. A graph file starts WITHOUT the previous graph's schema; the
   * optional `schemaFile` (JSON in the schema format, not with a snapshot) is set on the new
   * empty graph first and the data imported against it as one unit (a violation rejects, the
   * current graph stays).
   * @param {{kind:'empty'|'modern'|'large'|'tree'|'file', file?:File, schemaFile?:File}} request
   * @returns {Promise<GraphInfo>}
   */
  async loadGraph(request) { throw notImplemented('loadGraph'); }

  /** @returns {Promise<GraphInfo|null>} The loaded graph, or null before `loadGraph`. */
  async graphInfo() { throw notImplemented('graphInfo'); }

  /**
   * Runs a DSL script against the current graph. A traversal left without a terminal runs as
   * `execute(opts)`: results and, with `opts.profile`, metrics in one run. Explicit terminals
   * (`to_list()`, `next()`, `profile()`, `to_table()`, `execute(...)`, ...) are honoured. Never
   * rejects for a failing query: the failure is `execution.error`. Rejects only for engine faults
   * and when the run is stopped (`StoppedError`).
   * @param {string} script
   * @param {ExecuteOptions} [opts]
   * @returns {Promise<ExecuteResponse>}
   */
  async execute(script, opts) { throw notImplemented('execute'); }

  /** @returns {Promise<SchemaView>} */
  async schema() { throw notImplemented('schema'); }
  /** The schema views of `schema` (an object or JSON text: the editor's draft), with the graph's label counts. */
  async schemaViewOf(schema) { throw notImplemented('schemaViewOf'); }

  /**
   * Elements to draw for the whole graph, at most `limits.maxNodes` vertices (the first ones)
   * and the edges between them (at most `limits.maxEdges`), as columns without properties.
   * @param {{maxNodes?:number, maxEdges?:number}} [limits]
   * @returns {Promise<GraphView>}
   */
  async graphView(limits) { throw notImplemented('graphView'); }

  /**
   * The vertices within `hops` steps (the depth, 1 to 5) along `direction` ('both', 'out',
   * 'in') of the centre `ids`, plus the vertices `keep` (a drawing being expanded), and the edges
   * between them; bounded by `maxNodes` / `maxEdges` (`leftOut: {vertices, edges, complete}`
   * says what the limits cut). Rejects for an unknown id or a depth above 5.
   * @param {{ids:string[], hops?:number, direction?:string, keep?:string[], maxNodes?:number, maxEdges?:number}} request
   * @returns {Promise<SelectionView>}
   */
  async neighbourhood(request) { throw notImplemented('neighbourhood'); }

  /**
   * A sample (the first matches in storage order) of the vertices of some primary labels
   * (`{vertices: {labels, exclude?}}`, `exclude`: every label not listed) or of the edges of one
   * label pair (`{edge: {label, from?, to?}}`, `from`/`to` label filters).
   * @param {Object} request  Also `maxNodes`, `maxEdges`.
   * @returns {Promise<SelectionView>}
   */
  async labelSample(request) { throw notImplemented('labelSample'); }

  /** @returns {Promise<MetaGraph>} The label meta-graph. */
  async metaGraph() { throw notImplemented('metaGraph'); }

  /**
   * One element with its properties, for the inspector (drawings carry none).
   * @param {'vertex'|'edge'} kind @param {string} id
   * @returns {Promise<GraphElement|null>} null when it does not exist (any more).
   */
  async element(kind, id) { throw notImplemented('element'); }

  /**
   * One element for the property editor (js/element-editor.js), its values in the TYPED form
   * `{type, value}` (crates/graphersal-session `edit.rs`: `int64` as decimal text, `float64` a
   * number or "NaN"/"Infinity"/"-Infinity", `uuid` its canonical text, `array` a list of typed
   * values, `object` a list of `[key, typed]` pairs, `null` without `value`).
   * @param {'vertex'|'edge'} kind @param {string} id
   * @returns {Promise<{kind:string, id:string, label:string, labels:string[], source?:string,
   *   target?:string, properties:Array<[string, object]>, readOnly:string|null}|null>}
   *   `readOnly`: why the graph cannot be written now (a read-only view, a backup, maintenance
   *   mode), else null; null when the element does not exist (any more).
   */
  async elementForEdit(kind, id) { throw notImplemented('elementForEdit'); }

  /**
   * The property editor's Save: writes the changed (`set`) and removed (`remove`) top-level
   * properties of one element as ONE unit, validated by the engine; any failing key refuses the
   * whole update. `validateOnly: true` runs it as a dry run (nothing committed).
   * @param {{kind:string, id:string, set:Array<[string, object]>, remove:string[], validateOnly?:boolean}} request
   * @returns {Promise<{ok:true, changed:boolean, element:object} | {ok:false, errors:Array<{field:string, message:string, help?:string}>}>}
   *   `field`: the top-level key a failure belongs to, or 'general'.
   */
  async updateElement(request) { throw notImplemented('updateElement'); }

  /**
   * The labels the Graph panel's edit mode offers for a new element (crates/graphersal-session
   * `create.rs`): the schema's declared labels first, then the graph's own; for an edge only the
   * declared labels whose `connections` allow the pair (`*` matches all; any label of an endpoint
   * matches) and, outside mode `closed`, the graph's other edge labels and the declared labels that
   * connect other labels (`connects: false`).
   * @param {{kind:'vertex'} | {kind:'edge', from:string, to:string}} request
   * @returns {Promise<{kind:string, mode:string, free:boolean, readOnly:string|null,
   *   labels:Array<{label:string, declared:boolean, count:number, connects:boolean|null}>}>}
   *   `free`: a label may be typed by hand (false in mode `closed`).
   */
  async labelChoices(request) { throw notImplemented('labelChoices'); }

  /**
   * Creates a vertex or an edge as ONE unit, validated by the engine; values in the typed form
   * (see `elementForEdit`). `validateOnly: true` is a dry run (nothing committed, no id used up).
   * @param {{kind:'vertex', labels:string[], id?:string, properties:Array<[string, object]>, validateOnly?:boolean}
   *   | {kind:'edge', label:string, from:string, to:string, id?:string, properties:Array<[string, object]>, validateOnly?:boolean}} request
   * @returns {Promise<{ok:true, created:boolean, element:object|null,
   *   drawn:{kind:string, id:string, label:string, caption:string|null, source?:string, target?:string}|null}
   *   | {ok:false, errors:Array<{field:string, message:string, help?:string}>}>}
   *   `field`: a property key, 'label', 'id', 'from'/'to' or 'general'.
   */
  async createElement(request) { throw notImplemented('createElement'); }

  /**
   * Deletes a vertex (with its edges) or an edge as ONE unit; `validateOnly: true` only counts
   * the edges a vertex would take with it.
   * @param {{kind:'vertex'|'edge', id:string, validateOnly?:boolean}} request
   * @returns {Promise<{ok:true, deleted:boolean, edges:number} | {ok:false, errors:Array<{field:string, message:string}>}>}
   */
  async deleteElement(request) { throw notImplemented('deleteElement'); }

  /**
   * Starts the engine's force layout of a drawing (Barnes-Hut, ForceAtlas2-style; see
   * js/graph/layout-client.js, which drives it) and runs its first slice.
   * @param {{vertexCount:number, edgeSource:Uint32Array, edgeTarget:Uint32Array}} input
   *   The drawing's topology (a view's index columns).
   * @param {{budgetMs?:number, seed?:number, iterations?:number}} [options]
   * @returns {Promise<LayoutStatus>}
   */
  async layoutStart(input, options) { throw notImplemented('layoutStart'); }

  /**
   * Runs the next slice of layout `job` (at most `budgetMs`, at least one iteration).
   * @returns {Promise<LayoutStatus>}
   */
  async layoutStep(job, budgetMs) { throw notImplemented('layoutStep'); }

  /**
   * Cancels layout `job`; the answer carries the positions reached so far.
   * @returns {Promise<LayoutStatus>}
   */
  async layoutCancel(job) { throw notImplemented('layoutCancel'); }

  /** @returns {Promise<Completion[]>} Every DSL function the engine registers. */
  async completions() { throw notImplemented('completions'); }

  /**
   * @returns {Promise<Object<string, string[]>>} The values of the DSL's token classes
   *   (`{Order: ['asc', 'desc', 'shuffle'], T: [...], ...}`), from the engine's registered getters.
   */
  async tokenValues() { throw notImplemented('tokenValues'); }

  /** @returns {Promise<MemoryUsage|null>} */
  async memoryUsage() { throw notImplemented('memoryUsage'); }

  /** @returns {Promise<StatisticsView>} Counts, memory footprint and limits (the Statistics tab). */
  async statistics() { throw notImplemented('statistics'); }

  /**
   * The schema methods of the engine (the same seven as in the DSL and the Rust API; schemas are
   * plain objects in the schema format). `getSchema` returns `null` without a stored schema and
   * never infers; `inferSchema` stores nothing.
   * @returns {Promise<Object|null>}
   */
  async getSchema() { throw notImplemented('getSchema'); }

  /**
   * Replaces the stored schema. `mode` ('none' | 'open' | 'closed', optional) replaces the
   * schema's own mode: `setSchema(await inferSchema(), 'closed')` freezes the structure.
   * @param {Object|string} schema @param {string} [mode] @returns {Promise<GraphInfo>}
   */
  async setSchema(schema, mode) { throw notImplemented('setSchema'); }

  /** @param {Object|string} patch JSON Merge Patch. @returns {Promise<Object>} The new schema. */
  async patchSchema(patch) { throw notImplemented('patchSchema'); }

  /** @param {Object|string} schema @returns {Promise<SchemaValidation>} */
  async validateSchema(schema) { throw notImplemented('validateSchema'); }

  /** @param {Object|string} patch @returns {Promise<SchemaValidation>} */
  async validateSchemaPatch(patch) { throw notImplemented('validateSchemaPatch'); }

  /** @returns {Promise<Object>} The schema the data satisfies (mode `none`). */
  async inferSchema() { throw notImplemented('inferSchema'); }

  /** @param {Object|string} schema @returns {Promise<{changes:SchemaChange[], compatible:boolean}>} */
  async diffSchema(schema) { throw notImplemented('diffSchema'); }

  /**
   * "Save graph": the graph as GraphSON 3.0 (`format` 'graphson', the default; keeps every value
   * type) or GraphML text and, separately, its stored schema as JSON text in the schema format
   * (`null` without one). Neither format carries the schema: loading the graph file together
   * with the schema file (`loadGraph({kind: 'file', file, schemaFile})`) restores elements,
   * types and schema. `warnings` name
   * what does not survive (for GraphML: undeclared uuid/array/object values come back as
   * strings).
   * With `format` 'snapshot' (only when `supportsSnapshots`): the bytes of a packed snapshot
   * (`.gsnap`, lossless, schema included) instead of `text`.
   * @param {'graphson'|'graphml'|'snapshot'} [format]
   * @returns {Promise<{format:'graphson'|'graphml'|'snapshot', text?:string, bytes?:Uint8Array, schema:string|null, warnings:string[]}>}
   */
  async saveGraph(format) { throw notImplemented('saveGraph'); }

  /** Whether `saveGraph('snapshot')` and loading `.gsnap` files work with this engine. */
  get supportsSnapshots() { return false; }

  /**
   * "Load schema": sets the current graph's schema from a schema JSON file. A graph loaded
   * from a file afterwards does NOT keep it (give the schema file with the graph file instead).
   * Rejects (nothing changes) on invalid JSON or data that violates the schema.
   * @param {File} file
   * @returns {Promise<GraphInfo>}
   */
  async loadSchema(file) { throw notImplemented('loadSchema'); }

  // ---------------------------------------------------------------- saved queries
  // The catalog of the graph (crates/graphersal-session/src/queries.rs has the JSON forms; the
  // models are js/saved-queries.js). A change is a commit like any other.

  /**
   * The saved queries (in `folder` and below it): `[{name, folder, description, params: [{name,
   * schema, default?, required, description?}], status: 'ok'|'error', error?}]`.
   * @param {string} [folder] @returns {Promise<Array<Object>>}
   */
  async listQueries(folder) { throw notImplemented('listQueries'); }

  /** The saved query `name` with `body`, `dialect`, `meta`; null when there is none. */
  async getQuery(name) { throw notImplemented('getQuery'); }

  /**
   * Defines or replaces a saved query from the editor's spec `{name, folder, description,
   * params: [{name, schema, default?}], body, replaces?}`. Never rejects for a refused
   * definition: `{ok: true, query}` or `{ok: false, errors: [{field, message, help?}]}`.
   */
  async defineQuery(spec) { throw notImplemented('defineQuery'); }

  /** Drops the saved query `name`: `{dropped}`. */
  async dropQuery(name) { throw notImplemented('dropQuery'); }

  /** Moves the saved query `name` into `folder` ('' = the root); resolves to its entry. */
  async moveQuery(name, folder) { throw notImplemented('moveQuery'); }

  /** The call text `g.query("name", #{..})` of a saved query with named arguments `args`. */
  async queryCallText(name, args) { throw notImplemented('queryCallText'); }

  /**
   * The preview of a call of the saved query `name` with named arguments `args`, run as a dry
   * run and rolled back (the run form shows it before running a query that writes): `{ok: true,
   * changes: {vertices_added, vertices_dropped, edges_added, edges_dropped, properties_set,
   * properties_removed, labels_changed, other}}` or `{ok: false, error}`.
   */
  async previewQuery(name, args) { throw notImplemented('previewQuery'); }

  /**
   * "Catalog ▸ Save catalog": the whole catalog (saved queries, compression rules, every other
   * kind) as JSON text (`{"definitions": [..]}`).
   */
  async saveCatalog() { throw notImplemented('saveCatalog'); }

  /**
   * "Catalog ▸ Load catalog": stores the definitions of a catalog file in one commit (replacing
   * those of the same kind and name; a compression rule re-encodes its values). A graph loaded
   * from a GraphSON/GraphML file afterwards keeps them.
   * @param {File} file @returns {Promise<{loaded:number, graph:GraphInfo}>}
   */
  async loadCatalog(file) { throw notImplemented('loadCatalog'); }

  /**
   * The compression rules with their current stats: `[{name, element, label, path, codec,
   * minBytes, dictionary, stats: {compressedValues, plainBytes, storedBytes, dictionaryBytes}}]`.
   */
  async listCompressions() { throw notImplemented('listCompressions'); }

  /**
   * Defines or replaces a compression rule from the form's spec `{name, element, label, path,
   * minBytes, dictionary, replaces?}`. Never rejects for a refused rule: `{ok: true, rule}` or
   * `{ok: false, errors: [{field, message, help?}]}`.
   */
  async defineCompression(spec) { throw notImplemented('defineCompression'); }

  /** Drops the compression rule `name` (its values are decompressed): `{dropped}`. */
  async dropCompression(name) { throw notImplemented('dropCompression'); }

  /** Rebuilds the rule's dictionary and re-encodes its values (no data change): its entry. */
  async recompress(name) { throw notImplemented('recompress'); }

  /**
   * Estimates compression rules for `{label, path, element?, codec?, minBytes?, dictionary?}`
   * without changing the graph: `{ok: true, estimate: {valuesFound, stringValues, histogram,
   * candidates: [{codec, minBytes, dictionary, compressedValues, plainBytes, storedBytes,
   * dictionaryBytes, savedBytes, ratio, current, decodeBytesPerSec, ...}], best, current,
   * warnings, graphChanged: false, ...}}` or `{ok: false, errors: [{field, message, help?}]}`.
   */
  async estimateCompression(spec) { throw notImplemented('estimateCompression'); }

  /**
   * The property indexes with their current stats: `[{name, element, label, key, served,
   * stats: {entries, distinctValues, bytes} | null}]` (`stats` null when the storage does not
   * serve indexes).
   */
  async listIndexes() { throw notImplemented('listIndexes'); }

  /**
   * Creates a property index from the form's spec `{label, key, element?, name?}` (built from
   * the data in the same commit). Never rejects for a refused index: `{ok: true, index}` or
   * `{ok: false, errors: [{field, message, help?}]}`.
   */
  async createIndex(spec) { throw notImplemented('createIndex'); }

  /** Drops the property index `name`: `{dropped}`. */
  async dropIndex(name) { throw notImplemented('dropIndex'); }

  /**
   * AI Chat (dev server with `--ai` only; js/ai-chat.js): `aiCreateConversation() -> {id}`,
   * `aiSend(id, text) -> {turn}`, `aiEvents(id, after) -> {events, running, last}`,
   * `aiStop(id) -> {stopped}`, `aiDelete(id)`. Errors carry the HTTP status as `status`. The
   * WebAssembly engine rejects them (the agent needs the local server: AI_CHAT_UNAVAILABLE).
   */
  async aiCreateConversation() { throw notImplemented('aiCreateConversation'); }
  async aiSend(id, text) { throw notImplemented('aiSend'); }
  async aiEvents(id, after) { throw notImplemented('aiEvents'); }
  async aiStop(id) { throw notImplemented('aiStop'); }
  async aiDelete(id) { throw notImplemented('aiDelete'); }

  /**
   * Stops a running query by restarting the engine; the graph is restored from the last loaded
   * or saved state. @returns {Promise<{restored:string|null}>}
   */
  async stop() { throw notImplemented('stop'); }
}

/** Why the AI Chat is not available in the WebAssembly playground (decision 6 of the AI Chat). */
export const AI_CHAT_UNAVAILABLE =
  'AI Chat is available when you run Graphersal locally: graphersal --server --ai ai.json';

/** Default display limits (camelCase in the UI's settings), identical to the CLI. */
export const DEFAULT_LIMITS = Object.freeze({ maxRows: 100, maxItems: 100, noLimit: false });

function notImplemented(method) {
  return new Error(`Engine.${method}() is not implemented by this engine`);
}
