// The engine's Web Worker (a module worker): loads the Graphersal WebAssembly build from
// playground/pkg/ (built by playground/build.sh) and answers the requests of wasm-engine.js.
//
// Protocol: request {id, method, args: [...]}, answer {id, ok: true, result} or
// {id, ok: false, error}. Results that the module returns as JSON text are parsed here, so
// the page receives plain objects (structured clone). Long queries block only this worker; the
// page stays responsive and can terminate it (the Stop button).

import init, { Playground } from '../../pkg/graphersal_wasm.js';
import { packGraphView } from '../graph-data.js';

let playground = null;

const JSON_METHODS = {
  loadSample: (kind) => playground.loadSample(kind),
  // `schema`: the optional schema file's JSON text, set on the new graph before the import.
  loadGraphml: (bytes, name, schema) => playground.loadGraphmlBytes(bytes, name, schema ?? undefined),
  loadGraphson: (bytes, name, schema) => playground.loadGraphsonBytes(bytes, name, schema ?? undefined),
  loadSnapshot: (bytes, name) => playground.loadSnapshotBytes(bytes, name),
  info: () => playground.info(),
  execute: (script, options) => playground.execute(script, JSON.stringify(options || {})),
  schema: () => playground.schema(),
  schemaViewOf: (json) => playground.schemaViewOf(json),
  // `request` (`{maxNodes, maxEdges, degrees?, filter?}`) only with a filter or degrees.
  graphView: (maxNodes, maxEdges, request) => (request
    ? playground.graphViewWith(JSON.stringify(request))
    : playground.graphView(maxNodes, maxEdges)),
  graphFilterQuery: (filter) => playground.graphFilterQuery(JSON.stringify(filter || {})),
  element: (kind, id) => playground.element(kind, id),
  elementForEdit: (kind, id) => playground.elementForEdit(kind, id),
  updateElement: (request) => playground.updateElement(JSON.stringify(request || {})),
  labelChoices: (request) => playground.labelChoices(JSON.stringify(request || {})),
  createElement: (request) => playground.createElement(JSON.stringify(request || {})),
  deleteElement: (request) => playground.deleteElement(JSON.stringify(request || {})),
  neighbourhood: (request) => playground.neighbourhood(JSON.stringify(request || {})),
  labelSample: (request) => playground.labelSample(JSON.stringify(request || {})),
  metaGraph: () => playground.metaGraph(),
  memoryUsage: () => playground.memoryUsage(),
  statistics: () => playground.statistics(),
  completions: () => playground.completions(),
  tokenValues: () => playground.tokenValues(),
  saveGraphml: () => playground.saveGraphml(),
  saveGraphson: () => playground.saveGraphson(),
  setSchema: (json, mode) => playground.setSchema(json, mode || undefined),
  patchSchema: (patch) => playground.patchSchema(patch),
  validateSchema: (json) => playground.validateSchema(json),
  validateSchemaPatch: (patch) => playground.validateSchemaPatch(patch),
  diffSchema: (json) => playground.diffSchema(json),
  // The store in memory (the Store menu).
  storeInfo: () => playground.storeInfo(),
  checkpoint: (name) => playground.checkpoint(name || ''),
  mark: (name) => playground.mark(name),
  openView: (target) => playground.openView(JSON.stringify({ target })),
  closeView: () => playground.closeView(),
  fork: (target) => playground.fork(JSON.stringify({ target })),
  rollback: (target, deleteHistory) => playground.rollback(JSON.stringify({ target, delete: !!deleteHistory })),
  atticChanges: (id, offset, limit) => playground.atticChanges(id, offset || 0, limit || 0),
  atticRestore: (id) => playground.atticRestore(id),
  atticFork: (id) => playground.atticFork(id),
  atticRemove: (id) => playground.atticRemove(id),
  verifyStore: () => playground.verifyStore(),
  compactionAdvice: () => playground.compactionAdvice(),
  compactStore: (dropMarks) => playground.compactStore(!!dropMarks),
  switchDatabase: (id) => playground.switchDatabase(id),
  closeDatabase: (id) => playground.closeDatabase(id),
  // Saved queries (the catalog of the graph).
  listQueries: (folder) => playground.listQueries(folder || undefined),
  getQuery: (name) => playground.getQuery(name),
  defineQuery: (spec) => playground.defineQuery(JSON.stringify(spec)),
  dropQuery: (name) => playground.dropQuery(name),
  moveQuery: (name, folder) => playground.moveQuery(name, folder || ''),
  queryCallText: (name, args) => playground.queryCallText(name, JSON.stringify(args || {})),
  previewQuery: (name, args) => playground.previewQuery(name, JSON.stringify(args || {})),
  loadCatalog: (text, replace) => playground.loadCatalog(text, !!replace),
  // Compression rules (the catalog's kind `compression`).
  listCompressions: () => playground.listCompressions(),
  defineCompression: (spec) => playground.defineCompression(JSON.stringify(spec)),
  dropCompression: (name) => playground.dropCompression(name),
  recompress: (name) => playground.recompress(name),
  estimateCompression: (spec) => playground.estimateCompression(JSON.stringify(spec)),
  // Property indexes (the catalog's kind `property_index`).
  listIndexes: () => playground.listIndexes(),
  createIndex: (spec) => playground.createIndex(JSON.stringify(spec)),
  dropIndex: (name) => playground.dropIndex(name),
  // Scheduled jobs (graphersal_session::jobs).
  jobs: () => playground.jobs(),
  defineJob: (spec) => playground.defineJob(JSON.stringify(spec || {})),
  dropJob: (name) => playground.dropJob(name),
  setJobEnabled: (name, enabled) => playground.setJobEnabled(name, !!enabled),
  setJobSchedule: (name, schedule) => playground.setJobSchedule(name, schedule || undefined),
  runJobNow: (name) => playground.runJobNow(name),
  jobHistory: (name, tier) => playground.jobHistory(name, tier || 'minutes'),
  // The Optimize panel: the memory report, the workload statistics, the storage advisor.
  memoryReport: (spec) => playground.memoryReport(JSON.stringify(spec || {})),
  workloadReport: (history) => playground.workloadReport(history || undefined),
  advise: (spec) => playground.advise(JSON.stringify(spec || {})),
  applyRecommendation: (id, spec) => playground.applyRecommendation(id, JSON.stringify(spec || {})),
  dismissRecommendation: (id, days) => playground.dismissRecommendation(id, days == null ? undefined : days),
  restoreRecommendation: (id) => playground.restoreRecommendation(id),
};

// Methods that answer a drawing (compact columns).
const DRAWINGS = new Set(['graphView', 'neighbourhood', 'labelSample']);

// The force layout (js/graph/layout-client.js drives it slice by slice): the status JSON, and for
// a job that is no longer running its positions as a Float32Array (moved, not copied).
const LAYOUT_METHODS = {
  layoutStart: (input, options) => {
    const { budgetMs = 50, ...rest } = options || {};
    return playground.layoutStart(input.vertexCount, input.edgeSource, input.edgeTarget, JSON.stringify(rest), budgetMs);
  },
  layoutStep: (job, budgetMs) => playground.layoutStep(job, budgetMs ?? 50),
  layoutCancel: (job) => playground.layoutCancel(job),
};

// Methods whose result is not JSON text (passed on as is: a Uint8Array), or JSON text the page
// parses itself: a schema can be megabytes (inferred from a graph with one label per vertex), and
// one string crosses to the page several times faster than the object tree's structured clone.
const RAW_METHODS = {
  getSchema: () => playground.getSchema(),
  inferSchema: () => playground.inferSchema(),
  saveSnapshot: () => playground.saveSnapshot(),
  storeSnapshot: (commit) => playground.storeSnapshot(commit ?? undefined),
  storeSnapshotWith: (commit, withModels) => playground.storeSnapshotWith(commit ?? undefined, !!withModels),
  // The catalog file's text exactly as the engine writes it.
  saveCatalog: () => playground.saveCatalog(),
};

// Every few seconds the worker ticks the scheduled jobs of the active store (book page
// "Scheduled Jobs": the host drives the clock), in the browser's time zone. A tick runs between
// two requests (the worker is single-threaded), never inside one.
const JOB_TICK_MS = 5000;
let jobTicker = null;

function startJobTicker() {
  try {
    const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    if (zone) playground.setTimeZone(zone);
  } catch {
    // An unknown zone: the jobs run in UTC.
  }
  if (jobTicker) clearInterval(jobTicker);
  jobTicker = setInterval(() => {
    if (!playground) return;
    try {
      playground.tickJobs(Date.now());
    } catch {
      // A failed tick is recorded by the job itself; a trap shows at the next request.
    }
  }, JOB_TICK_MS);
}

async function start() {
  await init({ module_or_path: new URL('../../pkg/graphersal_wasm_bg.wasm', import.meta.url) });
  playground = new Playground();
  startJobTicker();
  return {
    version: Playground.version(),
    memoryTracked: Playground.memoryTracked(),
    samples: JSON.parse(Playground.sampleGraphs()),
    // What this worker answers: the page checks it (a cached, older worker.js lacks newer
    // methods, e.g. the Store menu's).
    methods: [...Object.keys(JSON_METHODS), ...Object.keys(LAYOUT_METHODS), ...Object.keys(RAW_METHODS)],
  };
}

self.onmessage = async (event) => {
  const { id, method, args = [] } = event.data || {};
  try {
    let result;
    let transfer = [];
    if (method === 'start') {
      result = await start();
    } else if (!playground) {
      throw new Error('The engine is not started');
    } else if (JSON_METHODS[method]) {
      result = JSON.parse(JSON_METHODS[method](...args));
      // Drawings: the index columns as typed arrays, their buffers moved instead of copied.
      if (DRAWINGS.has(method)) transfer = packGraphView(result);
      if (method === 'execute' && result) transfer = packGraphView(result.subgraph);
    } else if (LAYOUT_METHODS[method]) {
      result = JSON.parse(LAYOUT_METHODS[method](...args));
      if (result.state !== 'running') {
        result.positions = playground.layoutPositions(result.job);
        transfer = [result.positions.buffer];
      }
    } else if (RAW_METHODS[method]) {
      result = RAW_METHODS[method](...args);
    } else {
      throw new Error(`Unknown engine method: ${method} (the engine worker is older than the page: reload it bypassing the cache, Shift+Reload)`);
    }
    self.postMessage({ id, ok: true, result }, transfer);
  } catch (err) {
    // A JsError from the module (a failed import, unknown sample) or a trap of the module
    // (out of memory, a bug): both become a rejected request on the page.
    // After a trap the module's memory cannot be trusted: `fatal` asks the page to restart the
    // worker.
    const fatal = typeof WebAssembly !== 'undefined' && err instanceof WebAssembly.RuntimeError;
    if (fatal) playground = null;
    self.postMessage({ id, ok: false, fatal, error: err && err.message ? err.message : String(err) });
  }
};
