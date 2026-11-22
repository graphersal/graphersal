// The ONE place that creates the engine. Everything else calls `getEngine()` and talks to the
// interface documented in engine.js.
//
// Mode detection: when the page is served by the CLI's dev server (`graphersal --server`), its
// `GET /api/info` answers as a Graphersal dev server and the HTTP engine is used (no WebAssembly
// download, no graph choice); so it is for another host whose `/api/info` names itself and
// delivers a UI profile (js/engine/http-engine.js `isEngineInfo`). Otherwise (GitHub Pages,
// `python3 -m http.server`) it is the WebAssembly playground as always.

import { WasmEngine } from './wasm-engine.js';
import { HttpEngine } from './http-engine.js';

let instance = null;

/**
 * Returns the engine singleton, or `{engine: null, missing: true}` when the WebAssembly build is
 * not there (playground/build.sh has not been run): the page then explains how to build it.
 * @returns {Promise<{engine: import('./engine.js').Engine|null, missing: boolean}>}
 */
export async function getEngine() {
  if (instance) return instance;
  const http = await HttpEngine.detect();
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
