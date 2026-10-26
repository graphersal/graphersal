// The engine: the Graphersal library compiled to WebAssembly (crates/graphersal-wasm), running
// in a module Web Worker (worker.js). This class implements the interface of engine.js on top of
// a small request/answer protocol with the worker.
//
// Stop: `stop()` terminates the worker (the only way to end a running WebAssembly call), starts a
// new one and restores the graph from the RESTORE POINT: the sample graph or graph file loaded
// last, or the text of the last "Save graph", whichever came later, each with the schema it had
// (loaded with the graph file, set later with "Load schema", or saved with the graph). Changes made by queries after that point
// are lost; script variables never survive a run anyway (every run gets a fresh scope). The
// catalog the page loaded or edited (Catalog ▸ Load catalog, the saved-query editor, the
// Compression and Property indexes managers) is part of the restore point too, like the schema; definitions a script
// makes are not.
//
// The store in memory: every graph the worker loads is kept as a Store in memory (commit 0 =
// the loaded state; crates/graphersal-session/src/store.rs): the Store menu's operations
// (`storeInfo`, `checkpoint`, `mark`, `snapshotBytes`, `openView`/`closeView`, `fork`,
// `rollback`, the attic, `verifyStore`, the databases) run on it in the worker. All of it is
// lost on page reload, and on Stop (the new worker starts a new store from the restore point).

import { catalogTotal } from '../compression.js';
import { Engine, AI_CHAT_UNAVAILABLE } from './engine.js';

/**
 * The graph file format of a file name: `'graphson'` for `.json`, `.jsonl` and `.graphson`,
 * `'graphml'` for `.graphml` and `.xml`, `'snapshot'` for `.gsnap` (the packed snapshot),
 * `null` otherwise (the same extensions as the engine's `GraphSource::file`).
 * @param {string} name
 * @returns {'graphson'|'graphml'|'snapshot'|null}
 */
export function graphFileFormat(name) {
  if (/\.(json|jsonl|graphson)$/i.test(name)) return 'graphson';
  if (/\.(graphml|xml)$/i.test(name)) return 'graphml';
  if (/\.gsnap$/i.test(name)) return 'snapshot';
  return null;
}

/**
 * The engine calls that rebuild a restore point in a fresh worker, in order: a sample is loaded
 * and then gets its schema; a graph file (GraphSON or GraphML) is loaded in ONE call with its
 * schema as the load's schema argument (set on the new empty graph first, so the import is
 * checked against it and GraphML restores the declared types; neither format carries a schema),
 * or with `null` (no schema). A packed snapshot carries its own schema: it is loaded first, and a
 * schema set after it (`point.schema`) is applied on top. The catalog (`point.catalog`, its JSON text: saved
 * queries, compression rules, every kind) comes last and replaces the catalog the graph brought
 * (a snapshot carries one).
 * @param {{type:'sample', kind:string, schema:string|null, catalog?:string|null} | {type:'graphml'|'graphson'|'snapshot', bytes:Uint8Array, name:string, schema:string|null, catalog?:string|null}} point
 * @returns {Array<[string, ...any]>}
 */
export function restoreCalls(point) {
  const schema = point.schema ? [['setSchema', point.schema]] : [];
  const catalog = point.catalog ? [['loadCatalog', point.catalog, true]] : [];
  if (point.type === 'sample') return [['loadSample', point.kind], ...schema, ...catalog];
  if (point.type === 'snapshot') return [['loadSnapshot', point.bytes, point.name], ...schema, ...catalog];
  const load = point.type === 'graphson' ? 'loadGraphson' : 'loadGraphml';
  return [[load, point.bytes, point.name, point.schema || null], ...catalog];
}

const WASM_URL = new URL('../../pkg/graphersal_wasm_bg.wasm', import.meta.url);

/** Rejection reason of the requests that were pending when the worker was stopped. */
/** A schema or patch as JSON text: objects are serialized, strings pass unchanged. */
function asJson(value) {
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/** A plain copy of a JSON value (a reactive proxy cannot be posted to the worker). */
function plain(value) {
  return value == null ? value : JSON.parse(JSON.stringify(value));
}

function aiUnavailable() {
  return new Error(AI_CHAT_UNAVAILABLE);
}

export class StoppedError extends Error {
  constructor() {
    super('Stopped: the engine was restarted');
    this.name = 'StoppedError';
  }
}

export class WasmEngine extends Engine {
  constructor() {
    super();
    this.worker = null;
    this.pending = new Map();
    this.nextId = 1;
    this.meta = null; // {version, memoryTracked, samples}
    this.graph = null; // GraphInfo of the loaded graph
    this.restorePoint = null; // {type: 'sample', kind, schema} | {type: 'graphml'|'graphson', bytes, name, kind, schema}
    this.schemaJson = null; // the current graph's schema as last loaded or saved (JSON text), or null
    this.catalogJson = null; // the catalog as last loaded, edited or saved (catalog JSON text), or null
    this.startup = null;
  }

  /** Packed snapshots (`.gsnap`) are saved and loaded in the browser. */
  get supportsSnapshots() { return true; }

  /** Whether the WebAssembly build exists (playground/build.sh has been run). */
  static async isAvailable() {
    try {
      const r = await fetch(WASM_URL, { method: 'HEAD', cache: 'no-store' });
      return r.ok;
    } catch (err) {
      return false;
    }
  }

  spawn() {
    const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
    worker.onmessage = (event) => {
      const { id, ok, result, error, fatal } = event.data || {};
      const entry = this.pending.get(id);
      if (!entry) return;
      this.pending.delete(id);
      if (ok) entry.resolve(result);
      else {
        const err = new Error(error);
        err.fatal = !!fatal;
        entry.reject(err);
        if (fatal) this.restart().catch(() => {});
      }
    };
    worker.onerror = (event) => {
      // The module failed to load or the worker crashed: every pending request fails.
      const message = event && event.message ? event.message : 'the engine worker failed';
      for (const entry of this.pending.values()) entry.reject(new Error(message));
      this.pending.clear();
    };
    this.worker = worker;
    this.startup = this.request('start').then((meta) => {
      this.meta = meta;
      return meta;
    });
    return this.startup;
  }

  request(method, ...args) {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      this.pending.set(id, { resolve, reject });
      // Bytes are copied, not transferred: the restore point keeps its own copy.
      this.worker.postMessage({ id, method, args });
    });
  }

  async call(method, ...args) {
    if (!this.worker) this.spawn();
    await this.startup;
    return this.request(method, ...args);
  }

  async info() {
    if (!this.worker) this.spawn();
    const meta = await this.startup;
    // storeMemory: every loaded graph is a store in memory (the Store menu works on it).
    // A cached, older worker.js does not list the store methods: no Store menu then.
    const storeMemory = Array.isArray(meta.methods) && meta.methods.includes('storeInfo');
    // ai: null: the AI Chat needs the local dev server (`--ai`); the tab is read-only here.
    return { name: 'wasm', version: meta.version, memoryTracked: meta.memoryTracked, storeMemory, ai: null };
  }

  // The AI Chat stub: the agent runs in the dev server only (the API key never reaches a page).
  async aiCreateConversation() { throw aiUnavailable(); }
  async aiSend() { throw aiUnavailable(); }
  async aiEvents() { throw aiUnavailable(); }
  async aiStop() { throw aiUnavailable(); }
  async aiDelete() { throw aiUnavailable(); }

  /** The Store menu's information of the store in memory (see http-engine.js `storeInfo`). */
  async storeInfo() {
    return this.call('storeInfo');
  }

  /** Writes a snapshot of the current state (optionally named); resolves to its listing. */
  async checkpoint(name = '') {
    return (await this.call('checkpoint', name)).snapshot;
  }

  /** Sets a mark at the current position; resolves to `{name, commitSeq, timeText}`. */
  async mark(name) {
    return (await this.call('mark', name)).mark;
  }

  /**
   * The stored snapshot at `commit` as `.gsnap` bytes (a Uint8Array), with the store's embedding
   * models unless `withModels` is false.
   */
  async snapshotBytes(commit, withModels = true) {
    return withModels ? this.call('storeSnapshot', commit) : this.call('storeSnapshotWith', commit, false);
  }

  /** A read-only view of the state at `target`: `{view, graph}`. */
  async openView(target) {
    return this.swapped(await this.call('openView', plain(target)));
  }

  /** Back to the live graph: `{view: null, graph}`. */
  async closeView() {
    return this.swapped(await this.call('closeView'));
  }

  /** Forks the state at `target` into a new store in memory (a database): `{fork, store}`. */
  async fork(target) {
    return this.call('fork', plain(target));
  }

  /** Rolls the store back to `target`: `{rollback, graph, store}`. */
  async rollback(target, deleteHistory = false) {
    return this.swapped(await this.call('rollback', plain(target), !!deleteHistory));
  }

  /** One page of attic entry `id` (see http-engine.js `atticChanges`). */
  async atticChanges(id, offset = 0, limit = 100) {
    return this.call('atticChanges', id, offset, limit);
  }

  /** Undoes the rollback of attic entry `id`: `{restored, graph, store}`. */
  async atticRestore(id) {
    return this.swapped(await this.call('atticRestore', id));
  }

  /** Attic entry `id` as a new store in memory (a database): `{fork, store}`. */
  async atticFork(id) {
    return this.call('atticFork', id);
  }

  /** Deletes attic entry `id` for good. */
  async atticRemove(id) {
    return this.call('atticRemove', id);
  }

  /** Scrubs the store: `{verify}`. */
  async verifyStore() {
    return this.call('verifyStore');
  }

  /** Whether Compact is worth it now (in memory: a prune): `{compaction}`. */
  async compactionAdvice() {
    return this.call('compactionAdvice');
  }

  /**
   * Compact (in memory: a prune up to the current commit): `{compact, compaction}`.
   * `dropMarks`: the user confirmed that marks become unreachable (refused without it).
   */
  async compactStore({ dropMarks = false } = {}) {
    return this.call('compactStore', !!dropMarks);
  }

  /** Switches to database `id` (a fork, or main): `{graph, store}`. */
  async switchDatabase(id) {
    return this.swapped(await this.call('switchDatabase', id));
  }

  /** Closes database `id` (not the one in use; its memory is freed): `{closed, store}`. */
  async closeDatabase(id) {
    return this.call('closeDatabase', id);
  }

  /** An answer that swapped the session's graph: the graph info follows it. */
  swapped(res) {
    if (res && res.graph) this.graph = res.graph;
    return res;
  }

  async listSampleGraphs() {
    await this.info();
    return this.meta.samples;
  }

  /**
   * Loads a sample (`{kind}`) or a graph file (`{kind: 'file', file, schemaFile?}`). A graph file
   * starts WITHOUT the previous graph's schema; the optional `schemaFile` (JSON) is set on the new
   * empty graph first and the data imported against it as one unit (a violation fails the load,
   * the current graph stays). The catalog is kept (a graph file carries none). A snapshot brings
   * its own schema and takes no schema file.
   */
  async loadGraph(request) {
    const kind = request && request.kind;
    if (kind === 'file') {
      const file = request.file;
      if (!file) throw new Error('No file given');
      const format = graphFileFormat(file.name);
      if (!format) {
        throw new Error(`"${file.name}" is not a graph file: load a snapshot (.gsnap), GraphSON (.json, .jsonl, .graphson) or GraphML (.graphml, .xml)`);
      }
      const schemaFile = request.schemaFile || null;
      if (schemaFile && !/\.json$/i.test(schemaFile.name)) {
        throw new Error(`"${schemaFile.name}" is not a schema file: a schema is JSON (.json)`);
      }
      if (schemaFile && format === 'snapshot') {
        throw new Error('A snapshot (.gsnap) brings its own schema: load it without a schema file');
      }
      const bytes = new Uint8Array(await file.arrayBuffer());
      if (format === 'snapshot') {
        // A snapshot brings its own schema.
        this.graph = await this.call('loadSnapshot', bytes, file.name);
        this.schemaJson = null;
        this.catalogJson = null;
        this.restorePoint = { type: 'snapshot', bytes, name: file.name, kind: 'file', schema: null, catalog: null };
        return this.graph;
      }
      // The new graph gets the schema file's schema or none (never the previous graph's); the
      // engine carries the catalog over.
      const schema = schemaFile ? await schemaFile.text() : null;
      this.graph = await this.call(format === 'graphson' ? 'loadGraphson' : 'loadGraphml', bytes, file.name, schema);
      this.schemaJson = schema;
      this.restorePoint = { type: format, bytes, name: file.name, kind: 'file', schema, catalog: this.catalogJson };
    } else {
      this.graph = await this.call('loadSample', kind);
      this.schemaJson = null;
      this.catalogJson = null;
      this.restorePoint = { type: 'sample', kind, schema: null, catalog: null };
    }
    return this.graph;
  }

  async loadSchema(file) {
    if (!file) throw new Error('No file given');
    if (!/\.json$/i.test(file.name)) {
      throw new Error(`"${file.name}" is not a schema file: a schema is JSON (.json)`);
    }
    const json = await file.text();
    this.graph = await this.call('setSchema', json);
    this.schemaJson = json;
    if (this.restorePoint) this.restorePoint = { ...this.restorePoint, schema: json };
    return this.graph;
  }

  async graphInfo() {
    return this.graph;
  }

  async execute(script, opts = {}) {
    const res = await this.call('execute', script, opts);
    if (res && res.graph) this.graph = res.graph;
    return res;
  }

  async schema() {
    return this.call('schema');
  }

  async schemaViewOf(schema) {
    return this.call('schemaViewOf', asJson(schema));
  }

  async getSchema() {
    return JSON.parse(await this.call('getSchema')); // JSON text from the worker (worker.js RAW_METHODS)
  }

  async setSchema(schema, mode) {
    const json = asJson(schema);
    this.graph = await this.call('setSchema', json, mode || null);
    // With a mode the stored schema differs from `json`: remember what is stored.
    this.rememberSchema(mode ? await this.call('getSchema') : json); // the stored schema's JSON text
    return this.graph;
  }

  async patchSchema(patch) {
    const schema = await this.call('patchSchema', asJson(patch));
    this.rememberSchema(JSON.stringify(schema));
    this.graph = await this.call('info');
    return schema;
  }

  async validateSchema(schema) {
    return this.call('validateSchema', asJson(schema));
  }

  async validateSchemaPatch(patch) {
    return this.call('validateSchemaPatch', asJson(patch));
  }

  async inferSchema() {
    return JSON.parse(await this.call('inferSchema')); // JSON text from the worker (worker.js RAW_METHODS)
  }

  async diffSchema(schema) {
    return this.call('diffSchema', asJson(schema));
  }

  async listQueries(folder) {
    return this.call('listQueries', folder || '');
  }

  async getQuery(name) {
    return this.call('getQuery', name);
  }

  async defineQuery(spec) {
    const res = await this.call('defineQuery', plain(spec));
    if (res && res.ok) await this.catalogChanged();
    return res;
  }

  async dropQuery(name) {
    const res = await this.call('dropQuery', name);
    await this.catalogChanged();
    return res;
  }

  async moveQuery(name, folder) {
    const res = await this.call('moveQuery', name, folder || '');
    await this.catalogChanged();
    return res;
  }

  async queryCallText(name, args) {
    return this.call('queryCallText', name, plain(args || {}));
  }

  async previewQuery(name, args) {
    return this.call('previewQuery', name, plain(args || {}));
  }

  async saveCatalog() {
    return this.call('saveCatalog');
  }

  async loadCatalog(file) {
    if (!file) throw new Error('No file given');
    if (!/\.json$/i.test(file.name)) {
      throw new Error(`"${file.name}" is not a catalog file: it is JSON (.json)`);
    }
    const res = await this.call('loadCatalog', await file.text(), false);
    if (res && res.graph) this.graph = res.graph;
    await this.catalogChanged();
    return res;
  }

  // Compression rules (the catalog's kind `compression`).

  async listCompressions() {
    return this.call('listCompressions');
  }

  async defineCompression(spec) {
    const res = await this.call('defineCompression', plain(spec));
    if (res && res.ok) await this.catalogChanged();
    return res;
  }

  async dropCompression(name) {
    const res = await this.call('dropCompression', name);
    await this.catalogChanged();
    return res;
  }

  async recompress(name) {
    return this.call('recompress', name);
  }

  // Read-only: an estimate changes nothing.
  async estimateCompression(spec) {
    return this.call('estimateCompression', plain(spec));
  }

  // Property indexes (the catalog's kind `property_index`).

  async listIndexes() {
    return this.call('listIndexes');
  }

  async createIndex(spec) {
    const res = await this.call('createIndex', plain(spec));
    if (res && res.ok) await this.catalogChanged();
    return res;
  }

  async dropIndex(name) {
    const res = await this.call('dropIndex', name);
    await this.catalogChanged();
    return res;
  }

  // Scheduled jobs: a define, drop or switch of a user job is a catalog change.

  async jobs() {
    return this.call('jobs');
  }

  async defineJob(spec) {
    const res = await this.call('defineJob', plain(spec));
    if (res && res.ok) await this.catalogChanged();
    return res;
  }

  async dropJob(name) {
    const res = await this.call('dropJob', name);
    await this.catalogChanged();
    return res;
  }

  async setJobEnabled(name, enabled) {
    const res = await this.call('setJobEnabled', name, !!enabled);
    await this.catalogChanged();
    return res;
  }

  async setJobSchedule(name, schedule) {
    return this.call('setJobSchedule', name, schedule || null);
  }

  async runJobNow(name) {
    return this.call('runJobNow', name);
  }

  async jobHistory(name, tier) {
    return this.call('jobHistory', name, tier || 'minutes');
  }

  // The Optimize panel.

  async memoryReport(spec) {
    return this.call('memoryReport', plain(spec || {}));
  }

  async workloadReport(history) {
    return this.call('workloadReport', history || null);
  }

  async advise(spec) {
    return this.call('advise', plain(spec || {}));
  }

  async dismissRecommendation(id, days) {
    return this.call('dismissRecommendation', id, days == null ? null : days);
  }

  async restoreRecommendation(id) {
    return this.call('restoreRecommendation', id);
  }

  async applyRecommendation(id, spec) {
    const res = await this.call('applyRecommendation', id, plain(spec || {}));
    if (res && res.ok) await this.catalogChanged();
    return res;
  }

  /** The page changed the catalog: the restore point of a Stop keeps the new catalog. */
  async catalogChanged() {
    this.catalogJson = await this.call('saveCatalog');
    if (this.restorePoint) this.restorePoint = { ...this.restorePoint, catalog: this.catalogJson };
    this.graph = await this.call('info');
  }

  /** The schema the restore point brings back after a Stop (as `loadSchema` keeps it). */
  rememberSchema(json) {
    this.schemaJson = json;
    if (this.restorePoint) this.restorePoint = { ...this.restorePoint, schema: json };
  }

  async graphView(limits = {}, filter = null) {
    const maxNodes = limits.maxNodes ?? 500;
    const maxEdges = limits.maxEdges ?? 1000;
    if (!filter && !limits.degrees) return this.call('graphView', maxNodes, maxEdges);
    const request = { maxNodes, maxEdges, degrees: !!limits.degrees };
    if (filter) request.filter = plain(filter);
    return this.call('graphView', maxNodes, maxEdges, request);
  }

  async graphFilterQuery(filter) {
    return this.call('graphFilterQuery', plain(filter));
  }

  async element(kind, id) {
    return this.call('element', kind, String(id));
  }

  async elementForEdit(kind, id) {
    return this.call('elementForEdit', kind, String(id));
  }

  // Like a query's change, an edit is lost on Stop (the restore point is the last loaded or
  // saved graph).
  async updateElement(request) {
    return this.call('updateElement', plain(request));
  }

  async labelChoices(request) {
    return this.call('labelChoices', plain(request));
  }

  async createElement(request) {
    return this.call('createElement', plain(request));
  }

  async deleteElement(request) {
    return this.call('deleteElement', plain(request));
  }

  async neighbourhood(request, filter = null) {
    return this.call('neighbourhood', filter ? { ...plain(request), filter: plain(filter) } : request);
  }

  async labelSample(request) {
    return this.call('labelSample', request);
  }

  async metaGraph() {
    return this.call('metaGraph');
  }

  /** The engine's force layout, first slice (see engine.js; positions come as a Float32Array). */
  async layoutStart(input, options = {}) {
    return this.call('layoutStart', input, { ...options });
  }

  async layoutStep(job, budgetMs) {
    return this.call('layoutStep', job, budgetMs);
  }

  async layoutCancel(job) {
    return this.call('layoutCancel', job);
  }

  async completions() {
    return this.call('completions');
  }

  async tokenValues() {
    return this.call('tokenValues');
  }

  async memoryUsage() {
    return this.call('memoryUsage');
  }

  async statistics() {
    return this.call('statistics');
  }

  /**
   * "Save graph": GraphSON (default) or GraphML text and the stored schema as JSON text (or
   * null), or with `format` 'snapshot' the bytes of a packed snapshot (lossless, schema
   * included). The saved state becomes the restore point of `stop()`.
   * @param {'graphson'|'graphml'|'snapshot'} [format]
   * @returns {Promise<{format: 'graphson'|'graphml'|'snapshot', text?: string, bytes?: Uint8Array, schema: string|null, warnings: string[]}>}
   */
  async saveGraph(format = 'graphson') {
    if (format === 'snapshot') {
      const bytes = await this.call('saveSnapshot');
      this.schemaJson = null;
      this.catalogJson = null;
      this.restorePoint = {
        type: 'snapshot',
        bytes,
        name: this.graph ? this.graph.name : 'saved graph',
        kind: this.graph ? this.graph.kind : 'file',
        schema: null,
        catalog: null,
      };
      return { format: 'snapshot', bytes, schema: null, warnings: [] };
    }
    const saved = await this.call(format === 'graphml' ? 'saveGraphml' : 'saveGraphson');
    this.schemaJson = saved.schema;
    // The graph file carries no catalog: the restore point keeps the current one.
    const catalog = catalogTotal(this.graph) ? await this.call('saveCatalog') : null;
    this.catalogJson = catalog;
    this.restorePoint = {
      catalog,
      type: format === 'graphml' ? 'graphml' : 'graphson',
      bytes: new TextEncoder().encode(saved.text),
      name: this.graph ? this.graph.name : 'saved graph',
      kind: this.graph ? this.graph.kind : 'file',
      schema: saved.schema,
    };
    return { ...saved, format: format === 'graphml' ? 'graphml' : 'graphson' };
  }

  /**
   * Ends a running query: terminates the worker, starts a new one and restores the graph from
   * the restore point. Pending requests reject with `StoppedError`.
   * @returns {Promise<{restored: string|null}>} what was restored.
   */
  async stop() {
    return this.restart(true);
  }

  async restart(stopped = false) {
    if (this.worker) this.worker.terminate();
    this.worker = null;
    for (const entry of this.pending.values()) entry.reject(stopped ? new StoppedError() : new Error('The engine was restarted'));
    this.pending.clear();
    this.spawn();
    await this.startup;
    const point = this.restorePoint;
    if (!point) {
      this.graph = null;
      this.schemaJson = null;
      return { restored: null };
    }
    let info = null;
    for (const [method, ...args] of restoreCalls(point)) {
      const answer = await this.request(method, ...args);
      // `loadCatalog` answers {loaded, graph}; the others the graph info.
      info = answer && answer.graph ? answer.graph : answer;
    }
    this.schemaJson = point.schema || null;
    this.catalogJson = point.catalog || null;
    this.graph = point.type === 'sample' ? info : { ...info, name: point.name, kind: point.kind };
    return { restored: this.graph.name };
  }
}
