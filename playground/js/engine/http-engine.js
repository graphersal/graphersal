// The engine over HTTP: the DEV server of the CLI (`graphersal --graph <src> --server`, see the
// book page "Dev Server and MCP"). DEV mode, unstable, local only. It implements the interface of
// engine.js with one request per method; the server answers every method with the structure the
// WebAssembly engine returns (both run the same Rust session code, crate graphersal-session).
//
// Differences to the WebAssembly engine, by design:
// - the server serves ONE graph, the one it was started with: `listSampleGraphs()` is empty and
//   `loadGraph()` rejects (the UI hides the graph choice in this mode);
// - `stop(runId)` cancels the query on the server (`POST /api/cancel`): the run `runId` given to
//   `execute` (or, without one, every run of this page); it stops at its next check, or at once
//   when it still waits for the server, and rolls back; nothing restarts and no other page's
//   or AI Chat's query is touched;
// - `saveToSource()` writes the graph back to its `--graph` file (`info().saveable`);
// - on a Store directory (`--graph <dir>`, `info().store`): every commit is persisted, and
//   `storeInfo()`, `checkpoint(name)`, `mark(name)` and `snapshotUrl(commit)` (a `.gsnap`
//   download of a stored snapshot) drive the Store menu, and so do its time-travel actions on a
//   target (`{commit}`, `{mark}`, `{time}`): `openView`/`closeView` (a read-only view of a past
//   state), `fork`, `rollback`, and the attic (`atticChanges`, `atticRestore`, `atticFork`,
//   `atticRemove`);
// - every request names this page (`X-Graphersal-Client`, one id per page load), so the server's
//   change feed (`changes()`, `GET /api/changes`) tells this page's own changes from those of
//   another tab or of an AI agent over MCP (`info().mcp`: the server was started with `--mcp`);
// - the AI Chat (`info().ai`: the server was started with `--ai FILE`, EXPERIMENTAL): the
//   `ai*` methods drive `/api/ai/conversations*` (js/ai-chat.js); the agent runs in the server;
// - a host page may add request headers (EXPERIMENTAL), e.g. `Authorization: Bearer <token>`
//   for a server that requires a token: a classic script that runs before the playground's
//   modules sets `window.GRAPHERSAL_ENGINE = {headers: {...}}`; it is read ONCE, when the engine
//   is detected (`hostHeaders`), and every request of this engine carries the headers (the
//   engine's own headers win on a clash). The values are never logged nor put into a URL; with
//   host headers `snapshotUrl` answers null (a plain link cannot carry a header, so the Store
//   menu hides those download links). Unused, nothing changes.

import { Engine } from './engine.js';
import { newClientId } from '../change-feed.js';
import { packGraphView } from '../graph-data.js';

/** The `server` value of `GET /api/info` that identifies a Graphersal dev server. */
export const SERVER_ID = 'graphersal-dev';

/** The playground root (the directory of index.html): the dev server serves it at `/`. */
const DEFAULT_BASE = new URL('../../', import.meta.url);

/** The global a host page sets before the playground's modules run (see the header). */
export const HOST_CONFIG_GLOBAL = 'GRAPHERSAL_ENGINE';

/** A header name: an HTTP token (RFC 9110). */
const HEADER_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;

/** Headers the engine sets itself; a host cannot replace them. */
const OWN_HEADERS = Object.freeze(['accept', 'content-type', 'x-graphersal-client']);

/**
 * The request headers a host asks for (`config.headers`, `config` being
 * `window.GRAPHERSAL_ENGINE` unless given): a frozen copy of the valid entries and the problems of
 * the others. An entry is valid when its name is an HTTP token not set by the engine itself
 * (`Accept`, `Content-Type`, `X-Graphersal-Client`) and its value a string without line breaks.
 * A problem names the header, never its value (a value can be a secret).
 * @param {*} [config]
 * @returns {{headers: Readonly<Object<string,string>>, problems: string[]}}
 */
export function hostHeaders(config = globalThis[HOST_CONFIG_GLOBAL]) {
  const headers = {};
  const problems = [];
  if (config === undefined || config === null) return { headers: Object.freeze(headers), problems };
  if (typeof config !== 'object' || Array.isArray(config)) {
    return { headers: Object.freeze(headers), problems: [`window.${HOST_CONFIG_GLOBAL} must be an object`] };
  }
  const given = config.headers;
  if (given === undefined || given === null) return { headers: Object.freeze(headers), problems };
  if (typeof given !== 'object' || Array.isArray(given)) {
    return { headers: Object.freeze(headers), problems: [`window.${HOST_CONFIG_GLOBAL}.headers must be an object of header names and texts`] };
  }
  for (const [name, value] of Object.entries(given)) {
    if (!HEADER_NAME.test(name)) {
      problems.push(`the header name ${JSON.stringify(name)} is not valid`);
    } else if (OWN_HEADERS.includes(name.toLowerCase())) {
      problems.push(`the header ${name} is set by the engine itself`);
    } else if (typeof value !== 'string' || /[\r\n\0]/.test(value)) {
      problems.push(`the header ${name} must be a text without line breaks`);
    } else {
      headers[name] = value;
    }
  }
  return { headers: Object.freeze(headers), problems };
}

/** The host headers of the page, read once; problems warned by header name only. */
function readHostHeaders() {
  const { headers, problems } = hostHeaders();
  if (problems.length && typeof console !== 'undefined') {
    console.warn(`window.${HOST_CONFIG_GLOBAL}: ${problems.join('; ')} (ignored)`);
  }
  return headers;
}

/** A schema or patch as JSON text: objects are serialized, strings pass unchanged. */
function asJson(value) {
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/** A layout status of the server with its `positions` array as a Float32Array. */
export function withPositions(status) {
  if (status && Array.isArray(status.positions)) status.positions = Float32Array.from(status.positions);
  return status;
}

export class HttpEngine extends Engine {
  /**
   * @param {Object} info  The answer of `GET /api/info`.
   * @param {{base?: URL|string, fetch?: Function, headers?: Object<string,string>}} [options]
   *   `base`: the server root; `fetch`: the fetch implementation (tests pass a fake);
   *   `headers`: the host's request headers (default: `window.GRAPHERSAL_ENGINE.headers`, read
   *   now; checked by `hostHeaders`).
   */
  constructor(info, options = {}) {
    super();
    this.base = new URL('api/', options.base || DEFAULT_BASE);
    this.fetch = options.fetch || globalThis.fetch.bind(globalThis);
    /** The host's headers, sent with every request (never logged). */
    this.hostHeaders = options.headers !== undefined
      ? hostHeaders({ headers: options.headers }).headers
      : readHostHeaders();
    this.serverInfo = info;
    this.graph = null;
    this.stopRequested = false;
    /** This page's id in the server's change feed. */
    this.clientId = options.clientId || newClientId();
  }

  /**
   * Asks the serving origin whether it is a Graphersal dev server. Resolves to an HttpEngine,
   * or null when it is not (a static file server answers 404 or HTML; the WebAssembly playground
   * is used then).
   * @param {{base?: URL|string, fetch?: Function, headers?: Object<string,string>}} [options]
   * @returns {Promise<HttpEngine|null>}
   */
  static async detect(options = {}) {
    const fetchImpl = options.fetch || (typeof fetch === 'function' ? fetch.bind(globalThis) : null);
    if (!fetchImpl) return null;
    const headers = options.headers !== undefined
      ? hostHeaders({ headers: options.headers }).headers
      : readHostHeaders();
    try {
      const url = new URL('api/info', options.base || DEFAULT_BASE);
      const r = await fetchImpl(url, { cache: 'no-store', headers: { ...headers, Accept: 'application/json' } });
      if (!r.ok) return null;
      const info = JSON.parse(await r.text());
      if (!info || info.server !== SERVER_ID) return null;
      return new HttpEngine(info, { ...options, fetch: fetchImpl, headers });
    } catch (err) {
      return null;
    }
  }

  /** The headers of a request: the host's, then `own`, then this page's client id. */
  headers(own = {}) {
    return { ...this.hostHeaders, ...own, 'X-Graphersal-Client': this.clientId };
  }

  /** One request; resolves to the parsed JSON answer, rejects with the server's error text. */
  async request(method, path, body) {
    const init = { method, cache: 'no-store', headers: this.headers({ Accept: 'application/json' }) };
    if (method !== 'GET') {
      init.headers['Content-Type'] = 'application/json';
      init.body = body === undefined ? '' : body;
    }
    let r;
    try {
      r = await this.fetch(new URL(path, this.base), init);
    } catch (err) {
      throw new Error(`The dev server is not reachable (${err && err.message ? err.message : err}); is graphersal --server still running?`);
    }
    const text = await r.text();
    let data;
    try {
      data = text ? JSON.parse(text) : null;
    } catch (err) {
      throw new Error(`The dev server sent no JSON (HTTP ${r.status})`);
    }
    if (!r.ok) {
      const err = new Error((data && data.error) || `HTTP ${r.status}`);
      err.status = r.status;
      throw err;
    }
    return data;
  }

  async info() {
    const i = this.serverInfo;
    return {
      name: 'server',
      version: i.version,
      memoryTracked: !!i.memoryTracked,
      server: true,
      dev: true,
      saveable: !!i.saveable,
      source: i.source || null,
      store: i.store || null,
      // The store is on disk (the browser keeps its stores in memory).
      storeMemory: false,
      // A damaged store opened read-only (maintenance mode): its damage report.
      maintenance: i.maintenance || null,
      // Damage found on disk while the store is open: {text, frozen, foundBy, policy}, or null.
      damageFound: i.damageFound || null,
      // A backup directory served read-only: its marker (commit, time, the restore command).
      backup: i.backup || null,
      // A read-only view of a past state of the store (openView), or null: the live graph.
      view: i.view || null,
      // The MCP endpoint (`--mcp`): {endpoint, readOnly, client}, or null.
      mcp: i.mcp || null,
      // The AI Chat (`--ai FILE`): {provider, model, readOnly, sendsDataTo, maxToolCallsPerTurn},
      // or null (the server runs without --ai).
      ai: i.ai || null,
      // --read-only (or a Store this process may not write): every write is refused.
      readOnly: !!i.readOnly,
      // The UI profile the server names (`--ui-profile`, `ContextBuilder::ui_profile`): a
      // built-in name or a profile object (js/ui-profiles.js validates it), or null: `full`.
      uiProfile: i.uiProfile === undefined ? null : i.uiProfile,
      // 'allow-list' (`ContextBuilder::api_allow_list`): the server answers only `info`,
      // `changes` and `query` (runSavedQuery) of its selected saved queries; else 'full'.
      apiMode: i.apiMode === 'allow-list' ? 'allow-list' : 'full',
    };
  }

  /**
   * The change feed: `{version, commitSeq, recent: [{version, by, commitSeq}], view, mcp}`
   * (js/change-feed.js reads it); answered by the server without waiting for a running query.
   */
  async changes() {
    return this.request('GET', 'changes');
  }

  /** The MCP endpoint's usage statistics (`--mcp`; memory only, since the server started). */
  async mcpStats() {
    return this.request('GET', 'mcp/stats');
  }

  /** AI Chat: a new conversation owned by this page; resolves to `{id}`. */
  async aiCreateConversation() {
    return this.request('POST', 'ai/conversations', '{}');
  }

  /** AI Chat: starts a turn with the person's message; resolves to `{turn}` (409 while one runs). */
  async aiSend(id, text) {
    return this.request('POST', `ai/conversations/${encodeURIComponent(id)}/messages`, JSON.stringify({ text }));
  }

  /** AI Chat: the events after `after`: `{events, running, last}`. */
  async aiEvents(id, after = 0) {
    return this.request('GET', `ai/conversations/${encodeURIComponent(id)}/events?after=${Number(after) || 0}`);
  }

  /** AI Chat: stops this conversation's running turn only; resolves to `{stopped}`. */
  async aiStop(id) {
    return this.request('POST', `ai/conversations/${encodeURIComponent(id)}/stop`, '{}');
  }

  /** AI Chat: deletes the conversation on the server. */
  async aiDelete(id) {
    return this.request('DELETE', `ai/conversations/${encodeURIComponent(id)}`);
  }

  /** One GET of binary data (a `.gsnap` download); rejects with the server's error text. */
  async requestBytes(path) {
    let r;
    try {
      r = await this.fetch(new URL(path, this.base), { method: 'GET', cache: 'no-store', headers: { ...this.hostHeaders } });
    } catch (err) {
      throw new Error(`The dev server is not reachable (${err && err.message ? err.message : err}); is graphersal --server still running?`);
    }
    if (!r.ok) {
      let message = `HTTP ${r.status}`;
      try {
        message = JSON.parse(await r.text()).error || message;
      } catch (err) { /* not JSON */ }
      throw new Error(message);
    }
    return new Uint8Array(await r.arrayBuffer());
  }

  /**
   * The Store of the server (`--graph <dir>`): `{dir, graphId, commitSeq, lastCommitText,
   * snapshots: [{commitSeq, name, lastCommitText, vertexCount, edgeCount}], marks: [{name,
   * commitSeq, timeText}], walSegments, walBytes, ...}`, or null when the server has none.
   */
  async storeInfo() {
    const res = await this.request('GET', 'store');
    return res ? res.store : null;
  }

  /**
   * The Store menu's first call: `storeInfo()` without the figures that weigh the store or walk
   * the graph (`compaction`, `storeBytes`, `graphBytes` null, `detailsPending: true`), or null
   * without a store.
   */
  async storeSummary() {
    const res = await this.request('GET', 'store/summary');
    return res ? res.store : null;
  }

  /** Its second call: `{commitSeq, compaction, compactionError, storeBytes, graphBytes, ...}`. */
  async storeDetails() {
    const res = await this.request('GET', 'store/details');
    return res ? res.details : null;
  }

  /** Writes a snapshot of the current state (optionally named); resolves to its listing. */
  async checkpoint(name = '') {
    const res = await this.request('POST', 'store/checkpoint', JSON.stringify({ name }));
    return res.snapshot;
  }

  /** Sets a mark at the current position; resolves to `{name, commitSeq, timeText}`. */
  async mark(name) {
    const res = await this.request('POST', 'store/mark', JSON.stringify({ name }));
    return res.mark;
  }

  /**
   * The download URL of the stored snapshot at `commit` (packed `.gsnap`), with the store's
   * embedding models unless `withModels` is false. Null when the host sets request headers: a
   * plain download link cannot carry them (the Store menu then hides the link).
   */
  snapshotUrl(commit, withModels = true) {
    if (Object.keys(this.hostHeaders).length) return null;
    const without = withModels ? '' : '&models=without';
    return new URL(`store/snapshot?commit=${encodeURIComponent(String(commit))}${without}`, this.base).href;
  }

  /**
   * Serves a read-only view of the state at `target` (`{commit}`, `{mark}` or `{time}`, see
   * js/store-model.js) instead of the live graph: queries read it, writes are refused. Resolves
   * to `{view: {commitSeq, label, timeText, target}, graph}`.
   */
  async openView(target) {
    const res = await this.request('POST', 'store/view', JSON.stringify({ target }));
    this.serverInfo = { ...this.serverInfo, view: res.view };
    if (res.graph) this.graph = res.graph;
    return res;
  }

  /** Back to the live graph after `openView`; resolves to `{view: null, graph}`. */
  async closeView() {
    const res = await this.request('POST', 'store/live', '{}');
    this.serverInfo = { ...this.serverInfo, view: null };
    if (res.graph) this.graph = res.graph;
    return res;
  }

  /**
   * Forks the state at `target` into a NEW store directory: `dir` (a relative path is next to
   * the store; empty: `<store>-fork-<target>`). Resolves to `{fork: {dir, commitSeq, open, undo}}`.
   */
  async fork(target, dir = '') {
    return this.request('POST', 'store/fork', JSON.stringify({ target, dir }));
  }

  /**
   * Rolls the store back in place to `target` (the server cancels the running query first);
   * `deleteHistory`: delete what comes after instead of moving it to the attic. Resolves to
   * `{rollback: {target, previousCommitSeq, attic, undo}, graph, store}`.
   */
  async rollback(target, deleteHistory = false) {
    const res = await this.request('POST', 'store/rollback', JSON.stringify({ target, delete: !!deleteHistory }));
    this.serverInfo = { ...this.serverInfo, view: null };
    if (res.graph) this.graph = res.graph;
    return res;
  }

  /**
   * One page of what attic entry `id` holds: commits `offset..offset + limit` (default 100)
   * and the totals over all of them: `{commitCount, mutationCount, totals, offset, limit,
   * commits, hasMore}`.
   */
  async atticChanges(id, offset = 0, limit = 100) {
    return this.request('GET', `store/attic/changes?id=${encodeURIComponent(id)}&offset=${Number(offset) || 0}&limit=${Number(limit) || 100}`);
  }

  /** Scrubs the store: `{verify: {ok, problems, notes, snapshotsChecked, recordsChecked}}`. */
  async verifyStore() {
    return this.request('POST', 'store/verify', '{}');
  }

  /**
   * A backup of the store on the server: `{dir}` (default `<store>-backup` next to it; an
   * existing backup of the store takes an increment), `full`, `from_memory` (the graph in
   * memory, only when damage was found). Resolves to `{backup: {dir, incremental, fromMemory,
   * commitSeq, bytesText, warnings, restore}, store}`.
   */
  async backupStore(body) {
    return this.request('POST', 'store/backup', JSON.stringify(body || {}));
  }

  /**
   * Whether Compact is worth it now, cheaply (no data is read): `{compaction: {recommended,
   * reason, decidedBy, totalBytes, liveBytes, garbageBytes, pruneBytes, reclaimableBytes, ...}}`.
   */
  async compactionAdvice() {
    return this.request('GET', 'store/compaction');
  }

  /**
   * Compact: a prune up to the current commit, then (a single-file store) the file rewritten
   * without its garbage: `{compact: {snapshotsRemoved, segmentsRemoved, bytesFreed, ...},
   * compaction}`. `dropMarks`: the user confirmed that marks become unreachable (without it
   * the server refuses such a Compact with 409 and the list `marksUnreachable`).
   */
  async compactStore({ dropMarks = false } = {}) {
    return this.request('POST', 'store/compact', JSON.stringify({ dropMarks: !!dropMarks }));
  }

  /** Undoes the rollback of attic entry `id`; resolves to `{restored: {commitSeq}, graph, store}`. */
  async atticRestore(id) {
    const res = await this.request('POST', 'store/attic/restore', JSON.stringify({ id }));
    this.serverInfo = { ...this.serverInfo, view: null };
    if (res.graph) this.graph = res.graph;
    return res;
  }

  /** The rolled-back history of attic entry `id` as a new store (`dir` as in `fork`). */
  async atticFork(id, dir = '') {
    return this.request('POST', 'store/attic/fork', JSON.stringify({ id, dir }));
  }

  /** Deletes attic entry `id` for good. */
  async atticRemove(id) {
    return this.request('POST', 'store/attic/remove', JSON.stringify({ id }));
  }

  // Saved queries (`/api/queries*`): the catalog of the served graph; a change is a commit and
  // shows in the change feed like any other.

  async listQueries(folder) {
    const q = folder ? `?folder=${encodeURIComponent(folder)}` : '';
    return this.request('GET', `queries${q}`);
  }

  async getQuery(name) {
    return this.request('GET', `queries/get?name=${encodeURIComponent(name)}`);
  }

  async defineQuery(spec) {
    return this.request('POST', 'queries/define', JSON.stringify(spec));
  }

  async dropQuery(name) {
    return this.request('POST', 'queries/drop', JSON.stringify({ name }));
  }

  async moveQuery(name, folder) {
    return this.request('POST', 'queries/move', JSON.stringify({ name, folder: folder || '' }));
  }

  async queryCallText(name, args) {
    return (await this.request('POST', 'queries/call-text', JSON.stringify({ name, args: args || {} }))).text;
  }

  async previewQuery(name, args) {
    return this.request('POST', 'queries/preview', JSON.stringify({ name, args: args || {} }));
  }

  /**
   * `POST /api/query`: also the only query endpoint of a server in allow-list mode
   * (`info().apiMode`). A refusal of the server (403: a saved query it does not offer; 400: a
   * malformed call) rejects like every request, with the server's text and `status`.
   */
  async runSavedQuery(name, params) {
    return this.request('POST', 'query', JSON.stringify({ name, params: params || {} }));
  }

  /** The catalog file's text (every kind) exactly as the server writes it. */
  async saveCatalog() {
    let r;
    try {
      r = await this.fetch(new URL('catalog/export', this.base), { method: 'GET', cache: 'no-store', headers: this.headers() });
    } catch (err) {
      throw new Error(`The dev server is not reachable (${err && err.message ? err.message : err}); is graphersal --server still running?`);
    }
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return r.text();
  }

  async loadCatalog(file) {
    if (!file) throw new Error('No file given');
    if (!/\.json$/i.test(file.name)) {
      throw new Error(`"${file.name}" is not a catalog file: it is JSON (.json)`);
    }
    const res = await this.request('POST', 'catalog/load', await file.text());
    if (res && res.graph) this.graph = res.graph;
    return res;
  }

  // Compression rules (`/api/compression*`): a define or drop is a commit and shows in the
  // change feed like any other.

  async listCompressions() {
    return this.request('GET', 'compression');
  }

  async defineCompression(spec) {
    return this.request('POST', 'compression/define', JSON.stringify(spec));
  }

  async dropCompression(name) {
    return this.request('POST', 'compression/drop', JSON.stringify({ name }));
  }

  async dropCompressions(names) {
    return this.request('POST', 'compression/drop-many', JSON.stringify({ names }));
  }

  async recompress(name) {
    return this.request('POST', 'compression/recompress', JSON.stringify({ name }));
  }

  // Read-only: an estimate changes nothing and commits nothing.
  async estimateCompression(spec) {
    return this.request('POST', 'compression/estimate', JSON.stringify(spec));
  }

  // Property indexes (`/api/indexes*`): a create or drop is a commit and shows in the change
  // feed like any other.

  async listIndexes() {
    return this.request('GET', 'indexes');
  }

  async createIndex(spec) {
    return this.request('POST', 'indexes/create', JSON.stringify(spec));
  }

  async dropIndex(name) {
    return this.request('POST', 'indexes/drop', JSON.stringify({ name }));
  }

  // Scheduled jobs (`/api/jobs*`): a define, drop or switch of a user job is a commit.

  async jobs() {
    return this.request('GET', 'jobs');
  }

  async defineJob(spec) {
    return this.request('POST', 'jobs/define', JSON.stringify(spec));
  }

  async dropJob(name) {
    return this.request('POST', 'jobs/drop', JSON.stringify({ name }));
  }

  async setJobEnabled(name, enabled) {
    return this.request('POST', 'jobs/enable', JSON.stringify({ name, enabled: !!enabled }));
  }

  async setJobSchedule(name, schedule) {
    return this.request('POST', 'jobs/schedule', JSON.stringify({ name, schedule: schedule || null }));
  }

  async runJobNow(name) {
    return this.request('POST', 'jobs/run', JSON.stringify({ name }));
  }

  async jobHistory(name, tier) {
    return this.request('GET', `jobs/history?name=${encodeURIComponent(name)}&tier=${encodeURIComponent(tier || 'minutes')}`);
  }

  // The Optimize panel (`/api/memory/report`, `/api/workload`, `/api/advise*`): reading, but
  // applying a recommendation is a commit and shows in the change feed.

  async memoryReport(spec) {
    return this.request('POST', 'memory/report', JSON.stringify(spec || {}));
  }

  async workloadReport(history) {
    return this.request('GET', history ? `workload?history=${encodeURIComponent(history)}` : 'workload');
  }

  async advise(spec) {
    return this.request('POST', 'advise', JSON.stringify(spec || {}));
  }

  // The System journal panel (`GET /api/journal`, the request keys as query parameters).

  async journal(spec) {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(spec || {})) {
      if (value !== undefined && value !== null && value !== '') params.set(key, String(value));
    }
    const query = params.toString();
    return this.request('GET', query ? `journal?${query}` : 'journal');
  }

  async dismissRecommendation(id, days) {
    return this.request('POST', 'advise/dismiss', JSON.stringify(days == null ? { id } : { id, days }));
  }

  async restoreRecommendation(id) {
    return this.request('POST', 'advise/restore', JSON.stringify({ id }));
  }

  async applyRecommendation(id, spec) {
    return this.request('POST', 'advise/apply', JSON.stringify({ id, spec: spec || {} }));
  }

  async applyRecommendations(ids, spec) {
    return this.request('POST', 'advise/apply', JSON.stringify({ ids: ids || [], spec: spec || {} }));
  }

  async listSampleGraphs() {
    return [];
  }

  async loadGraph() {
    throw new Error('The dev server serves the graph it was started with (graphersal --graph <src> --server); restart it with another --graph to change the graph.');
  }

  async graphInfo() {
    this.graph = await this.request('GET', 'graph');
    return this.graph;
  }

  async execute(script, opts = {}, { runId } = {}) {
    this.stopRequested = false;
    const body = runId ? { script, options: opts, runId } : { script, options: opts };
    const res = await this.request('POST', 'execute', JSON.stringify(body));
    if (res && res.graph) this.graph = res.graph;
    if (res) packGraphView(res.subgraph);
    // A query cancelled by Stop comes back as a failed run (rolled back); flag it like the
    // WebAssembly engine's StoppedError, so "Run all tabs" ends the series.
    if (this.stopRequested && res && !res.ok) res.stopped = true;
    this.stopRequested = false;
    return res;
  }

  async schema() {
    return this.request('GET', 'schema');
  }

  async schemaViewOf(schema) {
    return this.request('POST', 'schema/view', asJson(schema));
  }

  async getSchema() {
    return this.request('GET', 'schema/stored');
  }

  async setSchema(schema, mode) {
    const path = mode ? `schema/set?mode=${encodeURIComponent(mode)}` : 'schema/set';
    this.graph = await this.request('POST', path, asJson(schema));
    return this.graph;
  }

  async patchSchema(patch) {
    const schema = await this.request('POST', 'schema/patch', asJson(patch));
    await this.graphInfo();
    return schema;
  }

  async validateSchema(schema) {
    return this.request('POST', 'schema/validate', asJson(schema));
  }

  async validateSchemaPatch(patch) {
    return this.request('POST', 'schema/validate-patch', asJson(patch));
  }

  async inferSchema() {
    return this.request('GET', 'schema/infer');
  }

  async diffSchema(schema) {
    return this.request('POST', 'schema/diff', asJson(schema));
  }

  async graphView(limits = {}, filter = null) {
    const maxNodes = limits.maxNodes ?? 500;
    const maxEdges = limits.maxEdges ?? 1000;
    let view;
    if (!filter && !limits.degrees) {
      const q = new URLSearchParams({ maxNodes: String(maxNodes), maxEdges: String(maxEdges) });
      view = await this.request('GET', `graph-view?${q}`);
    } else {
      const request = { maxNodes, maxEdges, degrees: !!limits.degrees };
      if (filter) request.filter = filter;
      view = await this.request('POST', 'graph-view', JSON.stringify(request));
    }
    packGraphView(view); // the same typed index columns as the WebAssembly engine
    return view;
  }

  async graphFilterQuery(filter) {
    const res = await this.request('POST', 'graph-filter-query', JSON.stringify(filter || {}));
    return res.query;
  }

  async neighbourhood(request, filter = null) {
    const body = filter ? { ...(request || {}), filter } : (request || {});
    const view = await this.request('POST', 'neighbourhood', JSON.stringify(body));
    packGraphView(view);
    return view;
  }

  async labelSample(request) {
    const view = await this.request('POST', 'sample', JSON.stringify(request || {}));
    packGraphView(view);
    return view;
  }

  async metaGraph() {
    return this.request('GET', 'meta-graph');
  }

  async element(kind, id) {
    const q = new URLSearchParams({ kind, id: String(id) });
    return this.request('GET', `element?${q}`);
  }

  async elementForEdit(kind, id) {
    const q = new URLSearchParams({ kind, id: String(id) });
    return this.request('GET', `element/edit?${q}`);
  }

  // A saved edit is a commit: the change feed records it as this page's.
  async updateElement(request) {
    return this.request('POST', 'element/update', JSON.stringify(request || {}));
  }

  async labelChoices(request) {
    return this.request('POST', 'element/labels', JSON.stringify(request || {}));
  }

  // Creating and deleting are commits: the change feed records them as this page's.
  async createElement(request) {
    return this.request('POST', 'element/create', JSON.stringify(request || {}));
  }

  async deleteElement(request) {
    return this.request('POST', 'element/delete', JSON.stringify(request || {}));
  }

  /**
   * The engine's force layout on the server (`POST /api/layout`, one slice per request; see
   * engine.js). The positions (a JSON array) become a Float32Array, as in the WebAssembly engine.
   */
  async layoutStart(input, options = {}) {
    const body = {
      ...options,
      vertexCount: input.vertexCount,
      edgeSource: Array.from(input.edgeSource),
      edgeTarget: Array.from(input.edgeTarget),
    };
    return withPositions(await this.request('POST', 'layout', JSON.stringify(body)));
  }

  async layoutStep(job, budgetMs) {
    const body = budgetMs === undefined ? { job } : { job, budgetMs };
    return withPositions(await this.request('POST', 'layout/step', JSON.stringify(body)));
  }

  async layoutCancel(job) {
    return withPositions(await this.request('POST', 'layout/cancel', JSON.stringify({ job })));
  }

  async completions() {
    return this.request('GET', 'completions');
  }

  async tokenValues() {
    return this.request('GET', 'token-values');
  }

  async memoryUsage() {
    return this.request('GET', 'memory');
  }

  async statistics() {
    return this.request('GET', 'statistics');
  }

  /**
   * "Save graph" (download): the same `{format, text, schema, warnings}` as the wasm engine. A
   * snapshot carries the store's embedding models unless `options.withModels` is false.
   */
  async saveGraph(format = 'graphson', { withModels = true } = {}) {
    if (format === 'snapshot') {
      const bytes = await this.requestBytes(`export?format=gsnap${withModels ? '' : '&models=without'}`);
      return { format: 'snapshot', bytes, schema: null, warnings: [], withoutModels: !withModels };
    }
    const f = format === 'graphml' ? 'graphml' : 'graphson';
    const saved = await this.request('GET', `export?format=${f}`);
    return { ...saved, format: f };
  }

  /**
   * "Save to file": writes the graph back to the server's `--graph` file (atomically), and the
   * schema to the `--schema` file when the server was started with one (`schemaPath`).
   * `warning` is set when a stored schema could not be saved (GraphML/GraphSON without
   * `--schema`).
   * @returns {Promise<{path:string, format:string, bytes:number, warnings:string[],
   *   schemaPath?:string, warning?:string}>}
   */
  async saveToSource() {
    return this.request('POST', 'save');
  }

  async loadSchema(file) {
    if (!file) throw new Error('No file given');
    if (!/\.json$/i.test(file.name)) {
      throw new Error(`"${file.name}" is not a schema file: a schema is JSON (.json)`);
    }
    return this.setSchema(await file.text());
  }

  /**
   * Stop: cancels the query `runId` of this page on the server (without one: every run of this
   * page); it rolls back, the graph stays as before it.
   * @param {string} [runId]
   * @returns {Promise<{restored: null, cancelled: boolean}>}
   */
  async stop(runId) {
    this.stopRequested = true;
    const res = await this.request('POST', 'cancel', runId ? JSON.stringify({ runId }) : undefined);
    return { restored: null, cancelled: !!(res && res.cancelled) };
  }
}
