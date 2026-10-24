// What the Graph panel draws: the VIEW SELECTOR (pure: no DOM, no Vue; tested under Node).
//
// Drawing "the first N vertices" of a large graph shows an arbitrary slice. The selector offers
// meaningful selections instead (`playground/README.md`):
// - WHOLE GRAPH, when it fits the limits (Settings); a larger graph shows the label overview
//   instead, with the first-N slice only on request;
// - LABELS: the label meta-graph (vertex labels as nodes sized by their vertex count, label
//   pairs as edges with their edge counts), drawn like the schema; double-clicking a label or a
//   pair draws a SAMPLE of it (the first matches, said so);
// - NEIGHBOURHOOD: the vertices within N hops of one vertex (by id, picked in a drawing, the
//   inspector or a result table), fetched from the engine; double-clicking a vertex expands it
//   from the engine, keeping the drawing's positions;
// - QUERY RESULT: the elements the last result refers to (at most the "Result graph" limit).
//
// The engine side is crates/graphersal-session/src/selection.rs (bounded, cancellable).

import { TIER_TUNING } from './tiers.js';
import { vertexCount, edgeCount, vertexCaption } from '../graph-data.js';

/** The panel's sources (its tabs). */
export const VIEW_SOURCES = Object.freeze([
  { id: 'graph', label: 'Whole graph', title: 'The whole graph when it fits the limits (Settings), else its labels' },
  { id: 'labels', label: 'Labels', title: 'Vertex labels sized by their count and the edges between them; double-click one for a sample' },
  { id: 'neighbourhood', label: 'Neighbourhood', title: 'A vertex and its neighbours, fetched from the engine; double-click a vertex to expand it' },
  { id: 'result', label: 'Query result', title: 'The vertices, edges and paths the last query returned' },
]);

/** A known source (`graph` for anything else, e.g. an old setting). */
export function normalizeSource(id) {
  return VIEW_SOURCES.some((s) => s.id === id) ? id : 'graph';
}

/** Selection sizes. */
export const SELECTION_TUNING = Object.freeze({
  /** Vertices of a label sample (at most the vertex limit). */
  sampleNodes: 1000,
  /** Hops of a new neighbourhood (each double click adds one more around a vertex). */
  defaultHops: 1,
  /** The deepest neighbourhood the depth control offers (the engine's `MAX_HOPS`). */
  maxHops: 5,
  /** Most label nodes the engine's meta-graph lists (`META_MAX_LABELS`). */
  metaLabels: 40,
});

const fmt = (n) => Number(n).toLocaleString('en-US');
const plural = (n, one, many = `${one}s`) => `${fmt(n)} ${n === 1 ? one : many}`;

/**
 * What the "Whole graph" source shows for a graph of `vertexCount` + `edgeCount` under the limits
 * `maxNodes` / `maxEdges`: `whole` (it fits), `overview` (too large: the labels instead), or
 * `slice` (too large and the user asked for the first `maxNodes` vertices anyway).
 * @returns {{mode:'whole'|'overview'|'slice', notice:?string}}
 */
export function wholeGraphPlan({ vertexCount: v, edgeCount: e, maxNodes, maxEdges, sliceRequested = false }) {
  if (v <= maxNodes && e <= maxEdges) return { mode: 'whole', notice: null };
  const size = `${plural(v, 'vertex', 'vertices')} and ${plural(e, 'edge')}`;
  const limits = `the limits of ${fmt(maxNodes)} vertices and ${fmt(maxEdges)} edges`;
  if (sliceRequested) {
    return {
      mode: 'slice',
      notice: `The graph has ${size}, more than ${limits}: this is the first ${fmt(maxNodes)} vertices in storage order ` +
        'and the edges between them, an arbitrary slice. The Labels and Neighbourhood views show meaningful parts.',
    };
  }
  return {
    mode: 'overview',
    notice: `The graph has ${size}, more than ${limits}, so its labels are shown instead: ` +
      'double-click a label or an edge to draw a sample of it.',
  };
}

/** The display name of a label ('' is the unlabeled vertices). */
export function labelName(label) {
  return label === '' ? '(no label)' : label;
}

/** Node scale of a meta-graph label with `count` vertices, the largest having `max`: 1 to 2.2. */
export function metaScale(count, max) {
  if (!(max > 1) || !(count > 0)) return 1;
  return 1 + 1.2 * (Math.log(count) / Math.log(max));
}

/**
 * The meta-graph (engine `metaGraph()`) as a drawing of kind `schema` (graph-canvas.js): nodes
 * `{id, label, count, scale, properties}` (the "other labels" node last), edges `{id, label,
 * source, target, count, properties}`. Every node and edge carries `pick`, what a double click
 * samples (`sampleRequestFor`).
 */
export function metaGraphDrawing(meta) {
  if (!meta) return null;
  const max = Math.max(1, ...meta.labels.map((l) => l.count), meta.other ? meta.other.count : 0);
  const nodes = meta.labels.map((l, i) => ({
    id: `L${i}`,
    label: labelName(l.label),
    count: l.count,
    scale: metaScale(l.count, max),
    properties: {},
    pick: { kind: 'label', index: i },
  }));
  if (meta.other) {
    nodes.push({
      id: `L${meta.labels.length}`,
      label: `other labels (${fmt(meta.other.labels)})`,
      count: meta.other.count,
      scale: metaScale(meta.other.count, max),
      properties: {},
      pick: { kind: 'other' },
      other: true,
    });
  }
  const edges = meta.edges.map((e, j) => ({
    id: `P${j}`,
    label: e.label === '' ? '(no label)' : e.label,
    source: `L${e.from}`,
    target: `L${e.to}`,
    count: e.count,
    properties: {},
    pick: { kind: 'pair', index: j },
  }));
  return { nodes, edges };
}

/** The label filter of meta-graph node `index` (`labels.length`: every other label). */
function filterOf(meta, index) {
  if (index < meta.labels.length) return { labels: [meta.labels[index].label] };
  return { labels: meta.labels.map((l) => l.label), exclude: true };
}

/**
 * What a double click on a meta-graph element samples: `{request, title, total}` for
 * `engine.labelSample` (null for an unknown pick). `limits` = `{maxNodes, maxEdges}`.
 */
export function sampleRequestFor(meta, pick, limits) {
  if (!meta || !pick) return null;
  const maxNodes = Math.min(SELECTION_TUNING.sampleNodes, limits.maxNodes);
  const bounds = { maxNodes, maxEdges: limits.maxEdges };
  if (pick.kind === 'label' && meta.labels[pick.index]) {
    const l = meta.labels[pick.index];
    return { request: { vertices: filterOf(meta, pick.index), ...bounds }, title: labelName(l.label), total: l.count };
  }
  if (pick.kind === 'other' && meta.other) {
    return {
      request: { vertices: filterOf(meta, meta.labels.length), ...bounds },
      title: `the ${fmt(meta.other.labels)} other labels`,
      total: meta.other.count,
    };
  }
  if (pick.kind === 'pair' && meta.edges[pick.index]) {
    const e = meta.edges[pick.index];
    const name = (i) => (i < meta.labels.length ? labelName(meta.labels[i].label) : 'other labels');
    return {
      request: { edge: { label: e.label, from: filterOf(meta, e.from), to: filterOf(meta, e.to) }, ...bounds },
      title: `${name(e.from)} –${e.label === '' ? '' : e.label}→ ${name(e.to)}`,
      total: e.count,
    };
  }
  return null;
}

/** The notice of the meta-graph: what is shown, what is grouped, how to drill down. */
export function metaNotice(meta) {
  if (!meta) return null;
  const parts = [];
  parts.push(`${plural(meta.labelTotal, 'vertex label')} of ${plural(meta.vertexCount, 'vertex', 'vertices')}`);
  if (meta.other) parts[0] += ` (the ${meta.labels.length} largest shown, the other ${fmt(meta.other.labels)} as one node)`;
  let text = `Labels of the graph: ${parts[0]}; node size follows the vertex count. `;
  text += `Edges: ${plural(meta.edges.length + meta.otherPairs, 'label pair')} of ${plural(meta.edgeCount, 'edge')}`;
  if (meta.otherPairs) text += ` (the ${meta.edges.length} largest drawn; ${fmt(meta.otherEdges)} edges in ${fmt(meta.otherPairs)} smaller pairs are not)`;
  text += '. ';
  if (!meta.complete) text += `Pair counts are from the first ${fmt(meta.scannedEdges)} edges. `;
  text += 'Double-click a label or an edge to draw a sample of it.';
  return text;
}

/** Which edges lead to neighbours (the engine's `direction`). */
export const DIRECTIONS = Object.freeze([
  { id: 'both', label: 'both', title: 'Neighbours along edges in either direction' },
  { id: 'out', label: 'out →', title: 'The vertices this one points to (its outgoing edges)' },
  { id: 'in', label: '← in', title: 'The vertices pointing to this one (its incoming edges)' },
]);

/** A known direction (`both` for anything else). */
export function normalizeDirection(d) {
  return DIRECTIONS.some((x) => x.id === d) ? d : 'both';
}

/** A valid depth: a whole number from 1 to `maxHops` (1 for anything else). */
export function normalizeDepth(n) {
  const v = Number(n);
  return Number.isInteger(v) && v >= 1 && v <= SELECTION_TUNING.maxHops ? v : 1;
}

/** A neighbourhood request for the centre `ids` at depth `hops` along `direction` (the panel's limits). */
export function neighbourhoodRequest(ids, hops, limits, direction = 'both') {
  return { ids: ids.map(String), hops, direction: normalizeDirection(direction), maxNodes: limits.maxNodes, maxEdges: limits.maxEdges };
}

/**
 * The vertex ids of a neighbourhood drawing as the engine selected them: before a filter
 * (`view.unfiltered.vertexIds` of a filtered answer), else the drawn ones. Kept by expansions and
 * redraws, so vertices a filter hides come back when it changes.
 */
export function neighbourhoodIds(view) {
  if (!view) return [];
  const all = view.unfiltered && Array.isArray(view.unfiltered.vertexIds) ? view.unfiltered.vertexIds : view.vertexIds;
  return Array.from(all, String);
}

/** The request that expands vertex `index` of `view` by one hop along `direction`, keeping everything drawn (before a filter). */
export function expandRequest(view, index, limits, direction = 'both') {
  return {
    ids: [String(view.vertexIds[index])],
    hops: 1,
    direction: normalizeDirection(direction),
    keep: neighbourhoodIds(view),
    maxNodes: limits.maxNodes,
    maxEdges: limits.maxEdges,
  };
}

/**
 * The request that draws the same neighbourhood again (a filter change, a graph change): every
 * vertex it held before any filter (`neighbourhoodIds`, so all expansions stay) at depth 0, the
 * centre `ids` first named again so it stays the seed.
 */
export function redrawRequest(view, ids, limits) {
  return {
    ids: (ids || []).map(String),
    hops: 0,
    keep: neighbourhoodIds(view),
    maxNodes: limits.maxNodes,
    maxEdges: limits.maxEdges,
  };
}

/**
 * Positions for `next` when it grows from `prev` (drawn at `positions`, `x, y` per vertex):
 * every vertex of `prev` keeps its place; a new vertex is placed on a ring around `anchor` (a
 * vertex id of `prev`, the expanded one), or around the drawing's centre without one. Null when
 * there is nothing to keep (no positions, or none of `prev`'s vertices is in `next`).
 * @returns {Float32Array|null}
 */
export function carryPositions(prev, positions, next, anchor = null, spacing = 70) {
  if (!prev || !next || !positions || positions.length !== 2 * vertexCount(prev)) return null;
  const at = new Map();
  for (let i = 0; i < vertexCount(prev); i++) at.set(String(prev.vertexIds[i]), [positions[2 * i], positions[2 * i + 1]]);
  const n = vertexCount(next);
  const out = new Float32Array(2 * n);
  const fresh = [];
  let kept = 0;
  let cx = 0;
  let cy = 0;
  for (let i = 0; i < n; i++) {
    const p = at.get(String(next.vertexIds[i]));
    if (p) {
      out[2 * i] = p[0];
      out[2 * i + 1] = p[1];
      cx += p[0];
      cy += p[1];
      kept++;
    } else {
      fresh.push(i);
    }
  }
  if (!kept) return null;
  const centre = (anchor !== null && at.get(String(anchor))) || [cx / kept, cy / kept];
  // A ring per 12 vertices, each further out; start angle varies so rings do not align.
  fresh.forEach((i, k) => {
    const ring = Math.floor(k / 12);
    const onRing = Math.min(12, fresh.length - ring * 12);
    const a = (2 * Math.PI * (k % 12)) / onRing + ring * 0.5;
    const r = spacing * (1 + ring * 0.8);
    out[2 * i] = centre[0] + r * Math.cos(a);
    out[2 * i + 1] = centre[1] + r * Math.sin(a);
  });
  return out;
}

/** The caption of the first seed of a selection (or null). */
export function seedCaption(view) {
  if (!view || !view.seeds || !view.seeds.length) return null;
  return vertexCaption(view, view.seeds[0], false);
}

/**
 * The notice of a neighbourhood or sample drawing: what is drawn, how much is hidden, how to see
 * more. `state` = `{title?, hops?, sampleTitle?, expanded?: number}`.
 */
export function selectionNotice(view, state = {}) {
  if (!view) return null;
  const v = vertexCount(view);
  const e = edgeCount(view);
  const drawn = `${plural(v, 'vertex', 'vertices')} and ${plural(e, 'edge')}`;
  let text;
  const times = plural(state.expanded || 0, 'time');
  if (state.sampleTitle && (state.expanded || !view.sample)) {
    text = `A sample of ${state.sampleTitle}, expanded ${times}: ${drawn}.`;
  } else if (view.sample) {
    const s = view.sample;
    const what = s.kind === 'edges' ? 'edges' : 'vertices';
    text = s.shown < s.total
      ? `A sample of ${state.sampleTitle || 'the label'}: the first ${fmt(s.shown)} of ${fmt(s.total)} ${what} in storage order (not random), ${drawn} drawn.`
      : `All ${s.kind === 'edges' ? plural(s.total, 'edge') : plural(s.total, 'vertex', 'vertices')} of ${state.sampleTitle || 'the label'}: ${drawn}.`;
  } else {
    const hops = state.hops ?? 1;
    const along = { out: ' along outgoing edges', in: ' along incoming edges' }[state.direction] || '';
    text = `Neighbourhood of ${state.title || seedCaption(view) || 'the vertex'}: ${drawn}`;
    text += state.expanded ? ` (depth ${hops}${along}, expanded ${times}).` : ` within ${plural(hops, 'hop')}${along}.`;
  }
  const left = view.leftOut;
  if (left) {
    const parts = [];
    if (left.vertices) parts.push(`${left.complete ? '' : 'at least '}${plural(left.vertices, 'vertex', 'vertices')}`);
    if (left.edges) parts.push(plural(left.edges, 'edge'));
    text += ` The limits (${fmt(state.maxNodes ?? v)} vertices, ${fmt(state.maxEdges ?? e)} edges; Settings) left out ${parts.join(' and ')} within this depth.`;
  } else if (view.truncated) {
    text += ` Stopped at the limit (${fmt(v)} vertices; raise it in the settings).`;
  }
  if (view.hiddenEdges) text += ` ${plural(view.hiddenEdges, 'more edge')} lead${view.hiddenEdges === 1 ? 's' : ''} out of the drawing: double-click a vertex to expand it.`;
  else text += ' Nothing more is connected to it.';
  if (view.missing && view.missing.length) text += ` Not found: ${view.missing.slice(0, 3).join(', ')}${view.missing.length > 3 ? ', …' : ''}.`;
  return text;
}

/** The notice of a query result drawing (its subgraph), or null when it is complete. */
export function resultNotice(sg, limit) {
  if (!sg || !sg.truncated) return null;
  const drawn = `${plural(vertexCount(sg), 'vertex', 'vertices')} and ${plural(edgeCount(sg), 'edge')}`;
  if (sg.stoppedBy === 'bytes') {
    return `Showing the first ${drawn} the result refers to: the ids and names reached the size bound of the drawing (16 MB).`;
  }
  return `Showing the first ${drawn} the result refers to (the "Result graph" limit of ${fmt(limit)} vertices + edges in the settings; at most ${fmt(TIER_TUNING.fastMax)}).`;
}
