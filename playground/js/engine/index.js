// The ONE place that creates the engine. Everything else calls `getEngine()` and talks to the
// interface documented in engine.js.
//
// Mode detection: when the page is served by the CLI's dev server (`graphersal --server`), its
// `GET /api/info` answers as a Graphersal dev server and the HTTP engine is used (no WebAssembly
// download, no graph choice). Otherwise (GitHub Pages, `python3 -m http.server`) it is the
// WebAssembly playground as always. At the root of a multi-database server (`/api/info` answers
// `admin: true` without `mountPath`, js/server-mode.js `isLanding`) there is no engine: the page
// shows the landing view (the database list) instead.

import { WasmEngine } from './wasm-engine.js';
import { HttpEngine } from './http-engine.js';
import { isLanding } from '../server-mode.js';

let instance = null;

/**
 * Returns the engine singleton, or `{engine: null, missing: true}` when the WebAssembly build is
 * not there (playground/build.sh has not been run): the page then explains how to build it, or
 * `{engine: null, missing: false, landing: {info, headers}}` at the root of a multi-database
 * server (the landing view; `headers` are the host's request headers).
 * @returns {Promise<{engine: import('./engine.js').Engine|null, missing: boolean, landing?: Object}>}
 */
export async function getEngine() {
  if (instance) return instance;
  const probe = await HttpEngine.probe();
  if (probe && isLanding(probe.info)) {
    instance = { engine: null, missing: false, landing: { info: probe.info, headers: probe.headers } };
    return instance;
  }
  const http = HttpEngine.fromProbe(probe);
  if (http) {
    instance = { engine: http, missing: false };
    return instance;
  }
  if (!(await WasmEngine.isAvailable())) return { engine: null, missing: true };
  const engine = new WasmEngine();
  await engine.info(); // starts the worker and instantiates the module
  instance = { engine, missing: false };
  return instance;
}
