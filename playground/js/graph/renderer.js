// The renderer interface of the graph drawings (pure: no DOM; tested under Node with a fake).
//
// The Graph and Schema panels never talk to a drawing engine directly: they go through a
// renderer, an object with the methods below, created by `createRenderer(tier, container)`.
// Cytoscape.js is the "detailed" renderer (js/graph/cytoscape-renderer.js), sigma.js + graphology
// on WebGL the "fast" one (js/graph/sigma-renderer.js); js/graph/tiers.js decides which one draws.
// Layout is not the
// renderer's business: the engine computes force layouts (js/graph/layout-client.js) and hands
// the positions over with `setPositions`; a renderer may offer cheap built-in layouts of its own
// (`capabilities.layouts`, run by `setLayout`).
//
// Data: `setData(data, options)` takes the engine's compact graph view (`kind: 'data'`, see
// js/graph-data.js) or the schema view's `{nodes, edges}` (`kind: 'schema'`). Vertex `i` / edge
// `j` are addressed by their index in those columns (`{group: 'node'|'edge', index}`) in every
// callback, so a renderer never needs element ids or properties.

/**
 * @typedef {Object} RendererCapabilities
 * @property {string} tier            'detailed' (Cytoscape) | 'fast' (sigma.js, WebGL).
 * @property {boolean} edgeLabels     Draws edge labels.
 * @property {boolean} curvedEdges    Draws curved (bezier) edges and loops.
 * @property {boolean} arrows         Draws arrow heads.
 * @property {string[]} layouts       Built-in layouts `setLayout` runs ('circle', 'grid', ...).
 * @property {boolean} png            `exportPng` works.
 * @property {boolean} svg            An SVG export exists (none yet in any tier).
 * @property {boolean} schema         Draws the schema view (`kind: 'schema'`).
 * @property {boolean} addElements    In edit mode, adds with the mouse: a double click on empty
 *                                    space reports its position, a drag from a vertex to another
 *                                    draws a rubber band and reports the pair (`onConnect`).
 */

/**
 * @typedef {Object} RenderTarget
 * @property {'node'|'edge'} group
 * @property {number} index   Index of the vertex / edge in the data's columns (schema: in
 *                            `nodes` / `edges`).
 */

/**
 * @typedef {Object} Renderer
 * @property {RendererCapabilities} capabilities
 * @property {(data:Object|null, options:{kind:'data'|'schema', showIds?:boolean, edgeIds?:boolean,
 *            edgeLabels?:boolean, nodeScale?:?Float32Array, palette:{slot:(label:string)=>number}}) => void} setData
 *   Replaces what is drawn (clears the selection). Nothing is drawn before the first
 *   `setPositions` or `setLayout` after it, so a large drawing is painted once, in place.
 *   `nodeScale`: vertex size by degree (js/graph/size-by.js), a scale in [0, 1] per vertex that
 *   the renderer maps into its own `SIZE_RANGE`; null (or another length) = the fixed size. A
 *   schema drawing's nodes and edges may carry `faded: true` (a label the filter hides).
 * @property {(xy:Float32Array|number[]) => void} setPositions
 *   Places vertex `i` at `(xy[2i], xy[2i+1])` (a preset layout).
 * @property {() => Float32Array|null} getPositions   The current positions (`null`: nothing drawn).
 * @property {(name:string, options?:{iterations?:number}) => Promise<void>} setLayout
 *   Runs one of `capabilities.layouts`; resolves when it has finished.
 * @property {(options:{showIds?:boolean, edgeIds?:boolean, edgeLabels?:boolean, nodeScale?:?Float32Array}) => void} setOptions
 *   Caption and size options of the current data (sizes change in place, no new layout).
 * @property {() => void} fit          Fits the drawing into the view.
 * @property {() => void} resize       The container changed size.
 * @property {(ids:{nodes:string[], edges:string[]}|null) => void} select
 *   Highlights the elements with these ids and fades the rest (`null` clears).
 * @property {(handler:(target:RenderTarget|null) => void) => void} onClick
 *   A click on an element (`null`: on empty space).
 * @property {(handler:(target:RenderTarget|null, point:?{x:number, y:number}) => void) => void} onDoubleClick
 *   `point`: where, in drawing coordinates (those of `setPositions`), when the tier can add
 *   elements (`capabilities.addElements`), else null.
 * @property {(on:boolean) => void} setEditMode
 *   Edit mode (js/graph/edit-mode.js): with `capabilities.addElements`, dragging from a vertex
 *   draws a rubber band to the pointer instead of moving the vertex; otherwise nothing changes.
 * @property {(handler:(from:RenderTarget, to:RenderTarget) => void) => void} onConnect
 *   A drag in edit mode ended on a vertex (`to` may be `from`: a loop).
 * @property {(handler:() => void) => void} onUserMove   The user zoomed or panned.
 * @property {(options?:{full?:boolean, transparent?:boolean}) => Promise<Blob>} exportPng
 * @property {(dark:boolean) => void} setTheme   Re-reads the theme's colours.
 * @property {() => void} destroy
 */

/** Every method a renderer has (the contract the node test checks). */
export const RENDERER_METHODS = Object.freeze([
  'setData', 'setPositions', 'getPositions', 'setLayout', 'setOptions', 'fit', 'resize', 'select',
  'onClick', 'onDoubleClick', 'onUserMove', 'exportPng', 'setTheme', 'destroy', 'setEditMode', 'onConnect',
]);

/** The capability keys every renderer declares. */
export const CAPABILITY_KEYS = Object.freeze(['tier', 'edgeLabels', 'curvedEdges', 'arrows', 'layouts', 'png', 'svg', 'schema', 'addElements']);

/**
 * What `renderer` lacks to be a renderer: missing methods and capability keys (empty: it is one).
 * @returns {string[]}
 */
export function rendererProblems(renderer) {
  if (!renderer || typeof renderer !== 'object') return ['not an object'];
  const out = RENDERER_METHODS.filter((m) => typeof renderer[m] !== 'function').map((m) => `method ${m}`);
  const caps = renderer.capabilities;
  if (!caps || typeof caps !== 'object') out.push('capabilities');
  else {
    for (const key of CAPABILITY_KEYS) if (!(key in caps)) out.push(`capability ${key}`);
    if (!Array.isArray(caps.layouts)) out.push('capabilities.layouts must be an array');
  }
  return out;
}

/** The renderer tiers. */
export const TIERS = Object.freeze(['detailed', 'fast']);

/**
 * What each tier draws (the renderers declare exactly these; the panel reads them before a
 * renderer exists, e.g. for the banner of a tier that is still loading).
 * @type {Readonly<Record<string, RendererCapabilities>>}
 */
export const TIER_CAPABILITIES = Object.freeze({
  detailed: Object.freeze({
    tier: 'detailed',
    edgeLabels: true,
    curvedEdges: true,
    arrows: true,
    layouts: Object.freeze(['cose', 'circle', 'concentric', 'breadthfirst', 'grid']),
    png: true,
    svg: false,
    schema: true,
    addElements: true,
  }),
  fast: Object.freeze({
    tier: 'fast',
    edgeLabels: false,
    curvedEdges: false,
    arrows: true,
    layouts: Object.freeze(['circle', 'grid']),
    png: true,
    svg: false,
    schema: false,
    addElements: false,
  }),
});

/**
 * Loads what the renderer of `tier` needs (its module with `import()`, and for 'fast' the sigma.js
 * and graphology scripts) and returns a constructor `(container, options) => Renderer`. Nothing is
 * loaded before a tier is first used, so small graphs never pay for the Fast tier. Rejects when
 * the tier cannot run here (offline, no WebGL).
 * @param {string} tier
 * @returns {Promise<(container:HTMLElement, options?:Object) => Renderer>}
 */
export async function rendererFactory(tier) {
  if (tier === 'detailed') {
    const { CytoscapeRenderer } = await import('./cytoscape-renderer.js');
    return (container, options = {}) => new CytoscapeRenderer(container, options);
  }
  if (tier === 'fast') {
    const { SigmaRenderer, loadFastLibraries } = await import('./sigma-renderer.js');
    const libs = await loadFastLibraries();
    return (container, options = {}) => new SigmaRenderer(container, { ...options, libs });
  }
  throw new Error(`unknown renderer tier: ${tier}`);
}

/**
 * The renderer of `tier` drawing into `container` (see `rendererFactory`).
 * @param {string} tier
 * @param {HTMLElement} container
 * @param {Object} [options]
 * @returns {Promise<Renderer>}
 */
export async function createRenderer(tier, container, options = {}) {
  const make = await rendererFactory(tier);
  return make(container, options);
}
