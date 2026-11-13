// What a graph drawing does, independent of the engine that draws it (pure: no DOM, no Vue;
// tested under Node with a fake renderer and a fake engine).
//
// The controller owns one renderer (js/graph/renderer.js) and decides how a drawing is laid out
// (`layoutFor` of js/graph-data.js): a built-in layout of the renderer for small drawings, or
// the ENGINE's force layout (js/graph/layout-client.js) whose positions it hands to the renderer
// with `setPositions`. It reports what is going on through `onState`: `{layout: {running,
// progress, iterations, totalIterations, vertexCount} | null, note}`; `cancelLayout()` stops a
// running engine layout and draws the positions reached so far. A newer `show()` or
// `applyLayout()` supersedes an older one (its late answer is dropped).
//
// Tiers: `replaceRenderer(renderer)` swaps the drawing engine (Detailed <-> Fast) under the same
// data and KEEPS THE POSITIONS (the new renderer gets them as a preset); a layout the renderer
// does not have (`capabilities.layouts`) runs as the engine's force layout, with a note.

import { layoutFor, DRAW_TUNING } from '../graph-data.js';
import { layoutInputOf, runEngineLayout, DEFAULT_SLICE_MS } from './layout-client.js';

/** The number of vertices of the data (`kind` 'data': a compact view; 'schema': `{nodes}`). */
export function nodeCountOf(data, kind) {
  if (!data) return 0;
  return kind === 'schema' ? data.nodes.length : data.vertexIds.length;
}

/** Display names of the built-in layouts, for the notes. */
const LAYOUT_LABELS = Object.freeze({ cose: 'Force (cose)', circle: 'Circle', concentric: 'Concentric', breadthfirst: 'Tree', grid: 'Grid' });

/**
 * The layout `chosen` (from `layoutFor`) as `caps` can run it: a built-in layout the renderer does
 * not have becomes the engine's force layout; `cose` silently (in the Fast tier the engine IS the
 * force layout), any other with a note naming it.
 * @param {import('./renderer.js').RendererCapabilities} caps
 * @param {{name:string, note:?string, iterations:number}} chosen
 * @param {string} requested
 */
export function layoutIn(caps, chosen, requested) {
  if (!caps) return chosen;
  if (requested === 'cose' && !caps.layouts.includes('cose')) return { name: 'engine', note: null, iterations: 0 };
  if (chosen.name === 'engine' || caps.layouts.includes(chosen.name)) return chosen;
  const name = LAYOUT_LABELS[requested] || requested;
  const where = caps.tier === 'fast' ? 'the Fast tier' : 'this drawing';
  return { name: 'engine', note: `${name} is not available in ${where}: the engine's force layout is used instead.`, iterations: 0 };
}

export class GraphController {
  /**
   * @param {{renderer: import('./renderer.js').Renderer, engine?: Object,
   *          isHidden?: () => boolean, onState?: (state:Object) => void, budgetMs?: number,
   *          tuning?: Object}} options
   *   `tuning`: the `layoutFor` tuning (js/graph-data.js `DRAW_TUNING`); a drawing without an
   *   engine layout (the profile graph view on a server in allow-list mode) lets `cose` run
   *   for more vertices.
   */
  constructor({ renderer, engine = null, isHidden = () => false, onState = () => {}, budgetMs = DEFAULT_SLICE_MS, tuning = DRAW_TUNING }) {
    this.renderer = renderer;
    this.tuning = tuning;
    this.engine = engine;
    this.isHidden = isHidden;
    this.onState = onState;
    this.budgetMs = budgetMs;
    this.data = null;
    this.kind = 'data';
    this.layout = 'cose';
    this.seq = 0;
    this.pending = null; // a built-in layout skipped while the canvas had no size
    this.abort = null; // AbortController of the running engine layout
    this.state = { layout: null, note: null };
    this.options = {}; // the show() options, for a replacement renderer
  }

  setState(patch) {
    this.state = { ...this.state, ...patch };
    this.onState(this.state);
  }

  /**
   * Draws `data` and lays it out with `options.layout`. With `options.positions` (`x, y` per
   * vertex, e.g. a drawing that grew by an expansion and keeps its old vertices in place) the
   * data is drawn at those positions and not laid out; with `keepView` too, the view (zoom and
   * pan) stays as it is instead of fitting the drawing (an edit that changed it in place).
   * @param {Object|null} data
   * @param {{kind?:'data'|'schema', layout?:string, showIds?:boolean, edgeIds?:boolean,
   *          edgeLabels?:boolean, palette:Object, positions?:?Float32Array, keepView?:boolean}} options
   * @returns {Promise<void>} when it is laid out (or superseded)
   */
  show(data, options) {
    this.data = data;
    this.kind = options.kind || 'data';
    if (options.layout) this.layout = options.layout;
    const { positions = null, keepView = false, ...rest } = options;
    this.options = { ...rest, kind: this.kind };
    this.stopEngineLayout();
    this.renderer.setData(data, this.options);
    const n = nodeCountOf(data, this.kind);
    if (positions && n > 0 && positions.length === 2 * n) {
      this.seq++;
      this.pending = null;
      this.renderer.setPositions(positions);
      if (!keepView && !this.isHidden()) this.renderer.fit();
      this.setState({ layout: null, note: null });
      return Promise.resolve();
    }
    return this.applyLayout(this.layout);
  }

  /** Caption options changed (kept for a replacement renderer). */
  setOptions(options) {
    Object.assign(this.options, options);
    this.renderer.setOptions(options);
  }

  /**
   * Draws the current data with another renderer (a tier switch). The old renderer's positions
   * are handed over as a preset, so the drawing keeps its shape; without positions (nothing drawn
   * yet) the current layout runs. A running engine layout continues and lands in the new one.
   * The old renderer is destroyed BEFORE `create()` makes the new one (both draw into the same
   * container, and sigma empties it when killed). With `redraw: false` the renderer is only
   * swapped (new data follows with `show()`), so the old one never draws data it is not meant for.
   * @param {() => import('./renderer.js').Renderer} create
   * @param {{redraw?:boolean}} [options]
   * @returns {Promise<void>}
   */
  async replaceRenderer(create, { redraw = true } = {}) {
    const old = this.renderer;
    const positions = redraw ? old.getPositions() : null;
    old.destroy();
    const renderer = create();
    this.renderer = renderer;
    this.pending = null;
    if (!redraw) {
      this.seq++;
      this.stopEngineLayout();
      return;
    }
    renderer.setData(this.data, this.options);
    if (this.abort) return; // the running engine layout calls setPositions on the new renderer
    const n = nodeCountOf(this.data, this.kind);
    if (positions && n > 0 && positions.length === 2 * n) {
      this.seq++; // a built-in layout still running in the old renderer is superseded
      renderer.setPositions(positions);
      if (!this.isHidden()) renderer.fit();
      return;
    }
    await this.applyLayout(this.layout);
  }

  /**
   * Lays out the current data with `name` (a menu choice, or the same again: re-layout).
   * @returns {Promise<void>}
   */
  async applyLayout(name = this.layout) {
    this.layout = name;
    const seq = ++this.seq;
    this.stopEngineLayout();
    const n = nodeCountOf(this.data, this.kind);
    if (!this.data || n === 0) {
      this.pending = null;
      this.renderer.setPositions(new Float32Array(0));
      this.setState({ layout: null, note: null });
      return;
    }
    const chosen = layoutIn(this.renderer.capabilities, layoutFor(name, n, this.tuning), name);
    if (chosen.name === 'engine') {
      this.pending = null;
      await this.runEngine(seq, chosen.note);
      return;
    }
    if (this.isHidden()) {
      this.pending = name; // `resume()` runs it once the canvas has a size
      return;
    }
    this.pending = null;
    this.setState({ layout: null, note: chosen.note });
    await this.renderer.setLayout(chosen.name, { iterations: chosen.iterations });
    if (seq === this.seq) this.renderer.fit();
  }

  /** The canvas got a size: runs a built-in layout that was skipped while it had none. */
  resume() {
    if (this.pending === null) return null;
    return this.applyLayout(this.pending);
  }

  async runEngine(seq, note) {
    const abort = new AbortController();
    this.abort = abort;
    const vertexCount = nodeCountOf(this.data, this.kind);
    this.setState({ layout: { running: true, progress: 0, iterations: 0, totalIterations: 0, vertexCount }, note });
    let result;
    try {
      result = await runEngineLayout(this.engine, layoutInputOf(this.data, this.kind), {
        budgetMs: this.budgetMs,
        signal: abort.signal,
        onProgress: (status) => {
          if (seq !== this.seq || status.state !== 'running') return;
          this.setState({
            layout: {
              running: true,
              progress: status.progress,
              iterations: status.iterations,
              totalIterations: status.totalIterations,
              vertexCount,
            },
          });
        },
      });
    } catch (err) {
      if (this.abort === abort) this.abort = null;
      if (seq !== this.seq) return;
      // No engine layout (an older worker or server, a failure, or the engine was restarted by
      // Stop during a query): a grid instead, with why.
      const why = err && err.message ? err.message : String(err);
      const restarted = err && err.name === 'StoppedError';
      this.setState({
        layout: null,
        note: restarted
          ? 'The layout ended with the engine restart (Stop); the drawing is a grid.'
          : `The engine's force layout failed (${why}); the drawing is a grid.`,
      });
      await this.renderer.setLayout('grid', {});
      if (seq === this.seq) this.renderer.fit();
      return;
    }
    if (this.abort === abort) this.abort = null;
    if (seq !== this.seq) return; // superseded
    this.renderer.setPositions(result.positions);
    this.renderer.fit();
    const stopped = result.state === 'cancelled'
      ? `Layout stopped after ${result.iterations} of ${result.totalIterations} iterations: the drawing shows the positions reached so far.`
      : null;
    this.setState({ layout: null, note: stopped || note });
  }

  /** Stops the running engine layout; its positions so far are drawn. */
  cancelLayout() {
    if (this.abort) this.abort.abort();
  }

  /** Abandons a running engine layout (a newer drawing replaces it). */
  stopEngineLayout() {
    if (this.abort) {
      this.abort.abort();
      this.abort = null;
    }
  }

  destroy() {
    this.seq++;
    this.stopEngineLayout();
    this.renderer.destroy();
  }
}
