// The ONE place that creates the engine. Everything else calls `getEngine()` and talks to the
// interface documented in engine.js.
//
// Mode detection: when the page is served by a Graphersal host (the CLI's dev server
// `graphersal --server`, or any other host built on graphersal-serve), its `GET /api/info` names
// the protocol (`protocol: 'graphersal/1'`, js/engine/http-engine.js `isEngineInfo`) and the
// HTTP engine is used (no WebAssembly download, no graph choice). Otherwise (GitHub Pages,
// `python3 -m http.server`, an older host without `protocol`) it is the WebAssembly playground
// as always. A host that requires a login (its `/api/info` answers 401 naming a `login` path)
// sends the page to its login page instead (js/engine/http-engine.js `loginRedirect`).

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
  const probe = await HttpEngine.probe();
  // A host that requires a login answered 401 naming its login page: the page is on its way
  // there (`loginRedirect`), so neither engine starts; the answer never comes.
  if (probe && probe.redirect) return new Promise(() => {});
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
