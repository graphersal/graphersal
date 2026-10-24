// The Graph panel's data model (pure: no Vue, no DOM; tested under Node): the engine's compact
// graph view and what the Cytoscape drawing makes of it.
//
// The engine sends what is drawn as COLUMNS, never as one object per element with all its
// properties (`CompactGraph` in crates/graphersal-session/src/display.rs):
//
//   vertexIds[i]   vertexLabel[i] (index into labels)   vertexName[i] (the `name` caption or null)
//   edgeIds[j]     edgeSource[j], edgeTarget[j] (indices into the vertex columns)   edgeLabel[j]
//   labels         the label table (vertex and edge labels)
//   truncated, totalNodes, totalEdges
//
// The index columns become Uint32Arrays (`packGraphView`; the worker transfers their buffers).
// Properties are fetched for the clicked element only (`engine.element(kind, id)`).

/** The index columns of a graph view (numbers: Uint32Array after `packGraphView`). */
export const INDEX_COLUMNS = Object.freeze(['vertexLabel', 'edgeSource', 'edgeTarget', 'edgeLabel']);

/**
 * Number columns a view carries only when asked (`degrees: true`): per vertex its incoming and
 * outgoing edges in the whole graph. Packed like the index columns when present.
 */
export const DEGREE_COLUMNS = Object.freeze(['vertexInDegree', 'vertexOutDegree']);

/**
 * Turns the index columns of `view` into Uint32Arrays, in place, and returns their buffers (the
 * worker's transfer list). Columns that already are typed arrays stay; `null` returns `[]`.
 * @param {Object|null} view
 * @returns {ArrayBuffer[]}
 */
export function packGraphView(view) {
  if (!view) return [];
  const transfer = [];
  for (const key of [...INDEX_COLUMNS, ...DEGREE_COLUMNS]) {
    const column = view[key];
    if (Array.isArray(column)) {
      view[key] = Uint32Array.from(column);
      transfer.push(view[key].buffer);
    }
  }
  return transfer;
}

/** Vertices in a view (0 for none). */
export function vertexCount(view) {
  return view && view.vertexIds ? view.vertexIds.length : 0;
}

/** Edges in a view (0 for none). */
export function edgeCount(view) {
  return view && view.edgeIds ? view.edgeIds.length : 0;
}

/** Vertices + edges in a view. */
export function elementCount(view) {
  return vertexCount(view) + edgeCount(view);
}

/**
 * The first `maxNodes` vertices of `view` and the edges between them, re-indexed. `view` itself
 * when it fits; otherwise a new view with `cut` = the vertex count of `view`.
 */
export function sliceGraphView(view, maxNodes) {
  if (!view || vertexCount(view) <= maxNodes) return view;
  const n = Math.max(0, maxNodes);
  const keep = [];
  for (let j = 0; j < edgeCount(view); j++) {
    if (view.edgeSource[j] < n && view.edgeTarget[j] < n) keep.push(j);
  }
  const pick = (column, Type) => {
    const out = Type ? new Type(keep.length) : new Array(keep.length);
    keep.forEach((j, k) => { out[k] = column[j]; });
    return out;
  };
  const degrees = {};
  for (const key of DEGREE_COLUMNS) if (view[key]) degrees[key] = view[key].slice(0, n);
  return {
    ...view,
    ...degrees,
    vertexIds: view.vertexIds.slice(0, n),
    vertexLabel: view.vertexLabel.slice(0, n),
    vertexName: view.vertexName.slice(0, n),
    edgeIds: pick(view.edgeIds),
    edgeSource: pick(view.edgeSource, Uint32Array),
    edgeTarget: pick(view.edgeTarget, Uint32Array),
    edgeLabel: pick(view.edgeLabel, Uint32Array),
    truncated: true,
    cut: vertexCount(view),
  };
}

/** The ids a view draws: `{nodes, edges}` (the Graph panel's highlight of a result). */
export function idsOf(view) {
  if (!view) return { nodes: [], edges: [] };
  return { nodes: Array.from(view.vertexIds), edges: Array.from(view.edgeIds) };
}

/** Primary label of vertex `i` (`''` when unlabeled). */
export function vertexLabelOf(view, i) {
  return view.labels[view.vertexLabel[i]] ?? '';
}

/** Label of edge `j`. */
export function edgeLabelOf(view, j) {
  return view.labels[view.edgeLabel[j]] ?? '';
}

/**
 * The caption of vertex `i`: its `name` (or `label:id` without one); with `showIds`
 * `id · name` (or `id · label`).
 */
export function vertexCaption(view, i, showIds = false) {
  const id = view.vertexIds[i];
  const name = view.vertexName[i];
  const label = vertexLabelOf(view, i);
  if (showIds) return `${id} · ${name ?? (label || 'v')}`;
  return name ?? `${label || 'v'}:${id}`;
}

/** The caption of edge `j`: its label, with `edgeIds` `id · label`. */
export function edgeCaption(view, j, edgeIds = false) {
  const label = edgeLabelOf(view, j);
  return edgeIds ? `${view.edgeIds[j]} · ${label}` : label;
}

/**
 * How the Detailed (Cytoscape) drawing adapts to its size, from the GR1 measurements (the numbers
 * are in playground/README.md, "Graph drawing"). The force layout `cose` is quadratic per
 * iteration and runs on the main thread (200 vertices: about 260 ms, 333: 1.2 s, 1000: 6.7 s), so
 * it is used only up to `coseMaxNodes`; above, the engine's force layout runs (`layoutFor`). Drawing tiers by elements (vertices + edges):
 * `full` below `reducedFrom` (curved edges, arrows, outlined labels: the drawing as before);
 * `reduced` (straight edges with arrows, labels without outline and only when readable, a texture
 * instead of the edges while the view moves); `minimal` from `minimalFrom` (haystack edges: the
 * cheapest, no arrows). Only drawings above the confirmation size reach `minimal`.
 */
export const DRAW_TUNING = Object.freeze({
  /** Most vertices the force layout `cose` lays out; above, the engine's force layout runs. */
  coseMaxNodes: 200,
  /** Fewer `cose` iterations (400 instead of 1000) above this many vertices. */
  coseFewerIterationsFrom: 100,
  /** From this many elements: the `reduced` tier. */
  reducedFrom: 2000,
  /** From this many elements: the `minimal` tier. */
  minimalFrom: 5000,
  /** `reduced` and `minimal`: labels smaller than this on screen (px) are not drawn. */
  minZoomedFontSize: 7,
});

/** The built-in (renderer) layouts that are cheap at any size the Detailed tier draws. */
const FAST_LAYOUTS = new Set(['grid', 'circle', 'concentric', 'breadthfirst']);

/**
 * The layout actually run for a requested one. `cose` (the renderer's force layout, on the
 * main thread) runs up to `coseMaxNodes` vertices; above, the ENGINE's force layout
 * (`name: 'engine'`, Barnes-Hut in the worker / on the dev server, js/graph/layout-client.js)
 * takes over and `note` says so. `engine` asks for the engine's layout at any size. Every other
 * built-in layout runs as asked; an unknown name is `grid`.
 * @param {string} requested
 * @param {number} nodes
 * @returns {{name:string, note:?string, iterations:number}}
 */
export function layoutFor(requested, nodes, tuning = DRAW_TUNING) {
  if (requested === 'cose') {
    if (nodes > tuning.coseMaxNodes) {
      return {
        name: 'engine',
        note: `The browser's force layout runs for at most ${tuning.coseMaxNodes} vertices (it would block the page); ` +
          `${nodes} vertices are laid out by the engine's force layout instead.`,
        iterations: 0,
      };
    }
    return { name: 'cose', note: null, iterations: nodes > tuning.coseFewerIterationsFrom ? 400 : 1000 };
  }
  if (requested === 'engine') return { name: 'engine', note: null, iterations: 0 };
  return { name: FAST_LAYOUTS.has(requested) ? requested : 'grid', note: null, iterations: 0 };
}

/**
 * The style choices for a drawing of `nodes` vertices and `edges` edges (see `DRAW_TUNING`).
 * @returns {{tier:'full'|'reduced'|'minimal', arrows:boolean, outline:boolean, curve:string,
 *            edgeWidth:number, hideEdgesOnViewport:boolean, textureOnViewport:boolean,
 *            minZoomedFontSize:number}}
 */
export function drawPlan(nodes, edges, tuning = DRAW_TUNING) {
  const size = nodes + edges;
  if (size < tuning.reducedFrom) {
    return {
      tier: 'full', arrows: true, outline: true, curve: 'bezier', edgeWidth: 1.6,
      hideEdgesOnViewport: false, textureOnViewport: false, minZoomedFontSize: 0,
    };
  }
  const minimal = size >= tuning.minimalFrom;
  return {
    tier: minimal ? 'minimal' : 'reduced',
    arrows: !minimal,
    outline: false,
    curve: minimal ? 'haystack' : 'straight',
    edgeWidth: 1.2,
    hideEdgesOnViewport: true,
    textureOnViewport: true,
    minZoomedFontSize: tuning.minZoomedFontSize,
  };
}

/**
 * Cytoscape element definitions of a view: nodes `n<id>`, edges `e<id>` with `data` {kind,
 * elementId, index, label, caption} (no properties: the inspector fetches them). Each node has
 * the class `s<slot>` of its label's palette slot (the style colours the slots, so a theme switch
 * recolours without touching the elements) and, with `positions` (`x, y` per vertex), its
 * position.
 * @param {Object} view
 * @param {{showIds?:boolean, edgeIds?:boolean, slotOf:(label:string)=>number,
 *          positions?:ArrayLike<number>|null}} options
 */
export function toCyElements(view, { showIds = false, edgeIds = false, slotOf, positions = null }) {
  const out = new Array(elementCount(view));
  const classes = view.labels.map((l) => 's' + slotOf(l));
  const n = vertexCount(view);
  for (let i = 0; i < n; i++) {
    const id = view.vertexIds[i];
    out[i] = {
      group: 'nodes',
      classes: classes[view.vertexLabel[i]],
      data: { id: 'n' + id, kind: 'vertex', elementId: id, index: i, label: vertexLabelOf(view, i), caption: vertexCaption(view, i, showIds) },
    };
    if (positions) out[i].position = { x: positions[2 * i], y: positions[2 * i + 1] };
  }
  for (let j = 0; j < edgeCount(view); j++) {
    const id = view.edgeIds[j];
    out[n + j] = {
      group: 'edges',
      data: {
        id: 'e' + id,
        kind: 'edge',
        elementId: id,
        index: j,
        label: edgeLabelOf(view, j),
        source: 'n' + view.vertexIds[view.edgeSource[j]],
        target: 'n' + view.vertexIds[view.edgeTarget[j]],
        caption: edgeCaption(view, j, edgeIds),
      },
    };
  }
  return out;
}

/** The legend: the vertex labels of a view in first-use order, at most `max`. */
export function legendLabels(view, max = 12) {
  if (!view) return [];
  const seen = new Set();
  const out = [];
  for (let i = 0; i < vertexCount(view) && out.length < max; i++) {
    const label = vertexLabelOf(view, i);
    if (!seen.has(label)) {
      seen.add(label);
      out.push(label);
    }
  }
  return out;
}

/**
 * The inspector's rows of an element as `engine.element()` returns it (`{kind, id, label,
 * labels, source?, target?, properties}`): `[name, value]` pairs, values as text.
 */
export function inspectorRows(element, formatValue) {
  if (!element) return [];
  const rows = [['id', element.id], ['label', element.label || '(none)']];
  if (element.labels && element.labels.length > 1) rows.push(['labels', element.labels.join(', ')]);
  if (element.kind === 'vertex' && Number.isFinite(element.inDegree) && Number.isFinite(element.outDegree)) {
    rows.push(['edges', `in ${element.inDegree} · out ${element.outDegree}`]);
  }
  if (element.kind === 'edge') rows.push(['out → in', `${element.source} → ${element.target}`]);
  for (const [k, v] of Object.entries(element.properties || {})) rows.push([k, formatValue(v)]);
  return rows;
}

// --- Neighbourhoods, sub-views and caps (the Fast tier's drill down, the no-WebGL limit) ----------

const adjacencyCache = new WeakMap();

/**
 * The undirected adjacency of a view as CSR (`offsets[i]..offsets[i+1]` index `neighbours` and
 * `edges`), built once per view and cached.
 * @returns {{offsets:Uint32Array, neighbours:Uint32Array, edges:Uint32Array}}
 */
export function adjacencyOf(view) {
  const cached = adjacencyCache.get(view);
  if (cached) return cached;
  const n = vertexCount(view);
  const m = edgeCount(view);
  const offsets = new Uint32Array(n + 1);
  for (let j = 0; j < m; j++) {
    offsets[view.edgeSource[j] + 1]++;
    offsets[view.edgeTarget[j] + 1]++;
  }
  for (let i = 0; i < n; i++) offsets[i + 1] += offsets[i];
  const fill = offsets.slice(0, n);
  const neighbours = new Uint32Array(2 * m);
  const edges = new Uint32Array(2 * m);
  for (let j = 0; j < m; j++) {
    const s = view.edgeSource[j];
    const t = view.edgeTarget[j];
    neighbours[fill[s]] = t;
    edges[fill[s]++] = j;
    neighbours[fill[t]] = s;
    edges[fill[t]++] = j;
  }
  const out = { offsets, neighbours, edges };
  adjacencyCache.set(view, out);
  return out;
}

/**
 * The vertices within `hops` steps of `seeds` (either direction), breadth first, at most
 * `maxNodes` (the seeds first, then nearer before farther). Indices in visiting order.
 * @param {Object} view
 * @param {number[]} seeds   Vertex indices.
 * @param {number} [hops]
 * @param {number} [maxNodes]
 * @returns {number[]}
 */
export function neighbourhood(view, seeds, hops = 1, maxNodes = Infinity) {
  const n = vertexCount(view);
  const seen = new Uint8Array(n);
  const out = [];
  for (const s of seeds) {
    if (s >= 0 && s < n && !seen[s] && out.length < maxNodes) {
      seen[s] = 1;
      out.push(s);
    }
  }
  if (!out.length) return out;
  const { offsets, neighbours } = adjacencyOf(view);
  let frontier = out.slice();
  for (let h = 0; h < hops && frontier.length && out.length < maxNodes; h++) {
    const next = [];
    for (const v of frontier) {
      for (let k = offsets[v]; k < offsets[v + 1]; k++) {
        const w = neighbours[k];
        if (seen[w]) continue;
        if (out.length >= maxNodes) return out;
        seen[w] = 1;
        out.push(w);
        next.push(w);
      }
    }
    frontier = next;
  }
  return out;
}

/**
 * The view of the vertices `indices` (in that order) and the edges between them, re-indexed;
 * `of[k]` is the old index of new vertex `k`. The degree columns (`DEGREE_COLUMNS`) are carried
 * when the view has them.
 * @param {Object} view
 * @param {number[]} indices
 * @returns {Object} a compact view with `truncated: false` and `of: Uint32Array`
 */
export function subView(view, indices) {
  const map = new Int32Array(vertexCount(view)).fill(-1);
  indices.forEach((old, k) => { map[old] = k; });
  const keep = [];
  for (let j = 0; j < edgeCount(view); j++) {
    if (map[view.edgeSource[j]] >= 0 && map[view.edgeTarget[j]] >= 0) keep.push(j);
  }
  const degrees = {};
  for (const key of DEGREE_COLUMNS) {
    if (view[key] && view[key].length === vertexCount(view)) degrees[key] = Uint32Array.from(indices, (i) => view[key][i]);
  }
  return {
    ...degrees,
    vertexIds: indices.map((i) => view.vertexIds[i]),
    vertexLabel: Uint32Array.from(indices, (i) => view.vertexLabel[i]),
    vertexName: indices.map((i) => view.vertexName[i]),
    edgeIds: keep.map((j) => view.edgeIds[j]),
    edgeSource: Uint32Array.from(keep, (j) => map[view.edgeSource[j]]),
    edgeTarget: Uint32Array.from(keep, (j) => map[view.edgeTarget[j]]),
    edgeLabel: Uint32Array.from(keep, (j) => view.edgeLabel[j]),
    labels: view.labels,
    truncated: false,
    totalNodes: indices.length,
    totalEdges: keep.length,
    of: Uint32Array.from(indices),
  };
}

/**
 * The ids of the vertices `indices` and of the edges between them: what `renderer.select` takes
 * to highlight a neighbourhood.
 * @returns {{nodes:string[], edges:string[]}}
 */
export function idsOfIndices(view, indices) {
  const inside = new Uint8Array(vertexCount(view));
  for (const i of indices) inside[i] = 1;
  const edges = [];
  for (let j = 0; j < edgeCount(view); j++) {
    if (inside[view.edgeSource[j]] && inside[view.edgeTarget[j]]) edges.push(view.edgeIds[j]);
  }
  return { nodes: indices.map((i) => view.vertexIds[i]), edges };
}

/**
 * The view cut to at most `maxElements` vertices + edges: the longest prefix of its vertices whose
 * vertices and the edges between them fit (`sliceGraphView`, found by bisection). `view` itself
 * when it fits.
 */
export function capToElements(view, maxElements) {
  if (!view || elementCount(view) <= maxElements) return view;
  // The edges among the first k vertices grow with k: bisect the largest k that fits.
  const within = (k) => {
    let e = 0;
    for (let j = 0; j < edgeCount(view); j++) if (view.edgeSource[j] < k && view.edgeTarget[j] < k) e++;
    return k + e;
  };
  let lo = 0;
  let hi = Math.min(vertexCount(view), maxElements);
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (within(mid) <= maxElements) lo = mid;
    else hi = mid - 1;
  }
  return sliceGraphView(view, lo);
}

/** Positions of `n` vertices on a circle (`x, y` per vertex), about `spacing` apart. */
export function circlePositions(n, spacing = 40) {
  const out = new Float32Array(2 * n);
  if (n <= 1) return out;
  const r = Math.max(spacing, (n * spacing) / (2 * Math.PI));
  for (let i = 0; i < n; i++) {
    const a = (2 * Math.PI * i) / n - Math.PI / 2;
    out[2 * i] = r * Math.cos(a);
    out[2 * i + 1] = r * Math.sin(a);
  }
  return out;
}

/** Positions of `n` vertices on a square grid (`x, y` per vertex, row by row), `spacing` apart. */
export function gridPositions(n, spacing = 60) {
  const out = new Float32Array(2 * n);
  const cols = Math.max(1, Math.ceil(Math.sqrt(n)));
  for (let i = 0; i < n; i++) {
    out[2 * i] = (i % cols) * spacing;
    out[2 * i + 1] = Math.floor(i / cols) * spacing;
  }
  return out;
}
