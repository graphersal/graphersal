// The Graph panel's EDIT MODE, its pure part (no DOM, no Vue; tested under Node:
// tests/edit-mode.test.mjs).
//
// In edit mode the mouse changes the graph (components/graph-canvas.js, components/graph-panel.js):
// a double click on empty space adds a vertex there, a drag from a vertex to another adds an
// edge (a rubber-band line follows the pointer), the Delete key or the inspector's delete button
// removes the selected vertex or edge after a confirmation, and a double click on an element
// edits its properties. Each change is ONE unit in the engine (crates/graphersal-session
// `create.rs`, `edit.rs`), validated there; the drawing then changes IN PLACE: the vertices keep
// their positions and a new vertex appears where it was asked for (`applyEdit`,
// `carryEditPositions`), no layout runs.
//
// What a tier can do comes from the renderer's `capabilities.addElements`: the Fast tier (WebGL)
// edits properties and deletes, but adds nothing with the mouse (`editModeState` says why).

import { vertexCount, edgeCount } from '../graph-data.js';

// A layer's drawing carries origin columns (graph-data.js `ORIGIN_COLUMNS`): an element created
// here is new in the layer ('added'); removals keep the columns aligned.
const appendOrigin = (column) => (Array.isArray(column) ? [...column, 'added'] : undefined);

/** Tuning of the gestures. */
export const EDIT_TUNING = Object.freeze({
  /** A press on a vertex that moves less than this many pixels is a click, not a drag. */
  dragThreshold: 6,
  /** Keep positions across redraws for this long after a change made here (ms). */
  carryMs: 10000,
  /** Keep positions only when at least this share of the new drawing's vertices was drawn. */
  carryOverlap: 0.5,
});

/**
 * The state of the toolbar's Edit mode switch and of what it allows.
 * @param {{caps?:Object|null, kind?:'data'|'schema', readOnly?:?string, running?:boolean,
 *          held?:boolean, on?:boolean}} input
 *   `caps` the renderer capabilities of the drawing's tier; `kind` 'schema' for the label
 *   overview; `readOnly` why the graph cannot be written (the engine's reason) or null;
 *   `running` a query runs; `held` the drawing waits for "Draw it"; `on` the user's switch.
 * @returns {{disabled:boolean, reason:?string, active:boolean, canAdd:boolean, hint:?string}}
 *   `active`: edit gestures are live; `canAdd`: adding with the mouse works in this tier (why
 *   not: js/graph/tiers.js `controlStates(caps).addElements`).
 */
export function editModeState({ caps = null, kind = 'data', readOnly = null, running = false, held = false, on = false } = {}) {
  let reason = null;
  if (kind === 'schema') reason = 'The label overview draws labels, not vertices and edges: open a sample or the whole graph to edit.';
  else if (readOnly) reason = `Read-only: ${readOnly}`;
  else if (held) reason = 'The drawing is not drawn yet (see the note above).';
  const disabled = reason !== null;
  const canAdd = !!(caps && caps.addElements);
  const active = on && !disabled;
  let hint = null;
  if (active) {
    hint = canAdd
      ? 'Edit mode: double-click empty space to add a vertex, drag from a vertex to another to add an edge, double-click an element to edit its properties, Delete removes the selected one.'
      : 'Edit mode (Fast tier): click an element, then edit its properties or delete it in its card; adding needs the Detailed tier.';
    if (running) hint += ' A query is running: changes wait until it has finished.';
  }
  return { disabled, reason, active, canAdd, hint };
}

// ------------------------------------------------------------------ the connect gesture

/**
 * The drag that adds an edge, as a state machine: `start` on a vertex, `move` with the pointer
 * (and the vertex under it, if any), `end` with the vertex under the pointer at release. A press
 * that never moved past the threshold is a click (`end` returns `{type: 'click'}`); a release on
 * another vertex connects; a release on the start vertex after leaving it makes a loop; anywhere
 * else nothing happens.
 */
export class ConnectGesture {
  constructor(threshold = EDIT_TUNING.dragThreshold) {
    this.threshold = threshold;
    this.reset();
  }

  reset() {
    this.source = null; // the vertex index the drag started on
    this.origin = null; // pointer position at the start (rendered pixels)
    this.dragging = false; // moved past the threshold
    this.left = false; // the pointer left the start vertex
    this.over = null; // the vertex under the pointer
  }

  /** Whether a drag is in progress (started, not ended). */
  get active() {
    return this.source !== null;
  }

  /** A press on vertex `index` at `point` ({x, y}, rendered pixels). */
  start(index, point) {
    this.reset();
    this.source = index;
    this.origin = { x: point.x, y: point.y };
    this.over = index;
  }

  /**
   * The pointer moved to `point`, over vertex `over` (null: none). Returns the rubber band to
   * draw (`{from: index, to: point, target: index|null}`) once dragging, else null.
   */
  move(point, over = null) {
    if (!this.active) return null;
    if (!this.dragging && Math.hypot(point.x - this.origin.x, point.y - this.origin.y) >= this.threshold) this.dragging = true;
    if (over !== this.source) this.left = true;
    this.over = over;
    if (!this.dragging) return null;
    const target = over !== null && (over !== this.source || this.left) ? over : null;
    return { from: this.source, to: { x: point.x, y: point.y }, target };
  }

  /**
   * The release, over vertex `over` (null: none): `{type: 'connect', from, to}`, `{type:
   * 'click', index}` or `{type: 'cancel'}`.
   */
  end(over = null) {
    if (!this.active) return { type: 'cancel' };
    const { source, dragging, left } = this;
    this.reset();
    if (!dragging) return { type: 'click', index: source };
    if (over === null) return { type: 'cancel' };
    if (over === source && !left) return { type: 'cancel' };
    return { type: 'connect', from: source, to: over };
  }
}

// ------------------------------------------------------------------ the drawing, patched

/**
 * The compact graph view (js/graph-data.js) with one change applied, as a new view (the input
 * stays as it is). Changes:
 * - `{type: 'add-vertex', drawn: {id, label, caption}}`;
 * - `{type: 'add-edge', drawn: {id, label, source, target}}` (vertex ids; an edge whose endpoint
 *   is not drawn is left out);
 * - `{type: 'remove', kind: 'vertex'|'edge', id}` (a vertex takes its edges with it).
 * An element already drawn is not added twice; removing one not drawn changes nothing.
 * @returns {Object} the patched view (`view` itself when nothing changed)
 */
export function applyEdit(view, change) {
  const base = view || emptyView();
  const n = vertexCount(base);
  const m = edgeCount(base);
  const labels = Array.from(base.labels || []);
  const labelIndex = (label) => {
    const i = labels.indexOf(label);
    if (i >= 0) return i;
    labels.push(label);
    return labels.length - 1;
  };
  const indexOfVertex = (id) => Array.prototype.findIndex.call(base.vertexIds, (x) => String(x) === String(id));
  const indexOfEdge = (id) => Array.prototype.findIndex.call(base.edgeIds, (x) => String(x) === String(id));
  if (!change) return view;
  if (change.type === 'add-vertex') {
    const d = change.drawn;
    if (!d || indexOfVertex(d.id) >= 0) return view;
    return {
      ...base,
      labels,
      vertexIds: [...Array.from(base.vertexIds), String(d.id)],
      vertexLabel: appendU32(base.vertexLabel, labelIndex(d.label || '')),
      vertexName: [...Array.from(base.vertexName || []), d.caption ?? null],
      ...(Array.isArray(base.vertexOrigin) ? { vertexOrigin: appendOrigin(base.vertexOrigin) } : {}),
      totalNodes: (base.totalNodes ?? n) + 1,
    };
  }
  if (change.type === 'add-edge') {
    const d = change.drawn;
    if (!d || indexOfEdge(d.id) >= 0) return view;
    const s = indexOfVertex(d.source);
    const t = indexOfVertex(d.target);
    if (s < 0 || t < 0) return view;
    return {
      ...base,
      labels,
      edgeIds: [...Array.from(base.edgeIds), String(d.id)],
      edgeSource: appendU32(base.edgeSource, s),
      edgeTarget: appendU32(base.edgeTarget, t),
      edgeLabel: appendU32(base.edgeLabel, labelIndex(d.label || '')),
      ...(Array.isArray(base.edgeOrigin) ? { edgeOrigin: appendOrigin(base.edgeOrigin) } : {}),
      totalEdges: (base.totalEdges ?? m) + 1,
    };
  }
  if (change.type === 'remove' && change.kind === 'edge') {
    const j = indexOfEdge(change.id);
    if (j < 0) return view;
    const keep = [];
    for (let k = 0; k < m; k++) if (k !== j) keep.push(k);
    return { ...withEdges(base, keep), totalEdges: Math.max(0, (base.totalEdges ?? m) - 1) };
  }
  if (change.type === 'remove' && change.kind === 'vertex') {
    const i = indexOfVertex(change.id);
    if (i < 0) return view;
    const map = new Int32Array(n);
    for (let k = 0, next = 0; k < n; k++) map[k] = k === i ? -1 : next++;
    const keep = [];
    for (let k = 0; k < m; k++) if (base.edgeSource[k] !== i && base.edgeTarget[k] !== i) keep.push(k);
    const edges = withEdges(base, keep);
    const without = (column) => Array.from(column).filter((_, k) => k !== i);
    const out = {
      ...edges,
      vertexIds: without(base.vertexIds),
      vertexLabel: Uint32Array.from(without(base.vertexLabel)),
      vertexName: without(base.vertexName || []),
      ...(Array.isArray(base.vertexOrigin) ? { vertexOrigin: without(base.vertexOrigin) } : {}),
      edgeSource: Uint32Array.from(edges.edgeSource, (s) => map[s]),
      edgeTarget: Uint32Array.from(edges.edgeTarget, (t) => map[t]),
      totalNodes: Math.max(0, (base.totalNodes ?? n) - 1),
      totalEdges: Math.max(0, (base.totalEdges ?? m) - (m - keep.length)),
    };
    delete out.of; // a drill-down's map to its parent drawing no longer holds
    return out;
  }
  return view;
}

function emptyView() {
  return {
    vertexIds: [], vertexLabel: new Uint32Array(0), vertexName: [],
    edgeIds: [], edgeSource: new Uint32Array(0), edgeTarget: new Uint32Array(0), edgeLabel: new Uint32Array(0),
    labels: [], truncated: false, totalNodes: 0, totalEdges: 0,
  };
}

function appendU32(column, value) {
  const out = new Uint32Array((column ? column.length : 0) + 1);
  if (column) out.set(column);
  out[out.length - 1] = value;
  return out;
}

/** The view with only the edges `keep` (indices, in order). */
function withEdges(view, keep) {
  return {
    ...view,
    edgeIds: keep.map((j) => view.edgeIds[j]),
    edgeSource: Uint32Array.from(keep, (j) => view.edgeSource[j]),
    edgeTarget: Uint32Array.from(keep, (j) => view.edgeTarget[j]),
    edgeLabel: Uint32Array.from(keep, (j) => view.edgeLabel[j]),
    ...(Array.isArray(view.edgeOrigin) ? { edgeOrigin: keep.map((j) => view.edgeOrigin[j] ?? null) } : {}),
  };
}

/**
 * The positions of `next` from the drawing `prev` at `positions` (`x, y` per vertex of `prev`):
 * every vertex drawn before keeps its place; a vertex in `place` (a Map id -> [x, y]: where the
 * user added it) goes there; any other new vertex goes next to its first drawn neighbour, else
 * near the drawing's centre. Null when too few of `next`'s vertices were drawn before
 * (`EDIT_TUNING.carryOverlap`): that is another drawing, which gets a layout of its own.
 * @returns {Float32Array|null}
 */
export function carryEditPositions(prev, positions, next, place = new Map(), tuning = EDIT_TUNING) {
  const n = vertexCount(next);
  const xy = positions || new Float32Array(0); // nothing drawn yet: an empty drawing
  if (!prev || !next || xy.length !== 2 * vertexCount(prev)) return null;
  const at = new Map();
  for (let i = 0; i < vertexCount(prev); i++) at.set(String(prev.vertexIds[i]), [xy[2 * i], xy[2 * i + 1]]);
  const out = new Float32Array(2 * n);
  const known = new Uint8Array(n);
  let placed = 0; // drawn before, or placed by the user
  let kept = 0;
  let cx = 0;
  let cy = 0;
  for (let i = 0; i < n; i++) {
    const id = String(next.vertexIds[i]);
    const p = at.get(id) || (place && place.get(id));
    if (!p) continue;
    out[2 * i] = p[0];
    out[2 * i + 1] = p[1];
    known[i] = 1;
    placed++;
    if (at.has(id)) {
      kept++;
      cx += p[0];
      cy += p[1];
    }
  }
  if (n > 0 && placed < Math.ceil(n * tuning.carryOverlap)) return null;
  if (n === 0) return out;
  const centre = kept ? [cx / kept, cy / kept] : [0, 0];
  // A new vertex without a place: next to a drawn neighbour, spread around it.
  let k = 0;
  for (let i = 0; i < n; i++) {
    if (known[i]) continue;
    let anchor = null;
    for (let j = 0; j < edgeCount(next) && !anchor; j++) {
      const s = next.edgeSource[j];
      const t = next.edgeTarget[j];
      if (s === i && known[t]) anchor = [out[2 * t], out[2 * t + 1]];
      else if (t === i && known[s]) anchor = [out[2 * s], out[2 * s + 1]];
    }
    const c = anchor || centre;
    const a = (2 * Math.PI * (k % 12)) / 12 + Math.floor(k / 12) * 0.5;
    const r = 60 * (1 + Math.floor(k / 12) * 0.8);
    out[2 * i] = c[0] + r * Math.cos(a);
    out[2 * i + 1] = c[1] + r * Math.sin(a);
    known[i] = 1;
    k++;
  }
  return out;
}

// ------------------------------------------------------------------ dialogs

/**
 * The label options of the creation dialog from the engine's `labelChoices` answer: `[{label,
 * note}]` in the engine's order, `note` saying where a label comes from (declared by the schema,
 * N in the graph, connects other labels).
 */
export function labelOptions(choices) {
  if (!choices || !Array.isArray(choices.labels)) return [];
  return choices.labels.map((c) => {
    const parts = [];
    if (c.declared) parts.push(c.connects === false ? 'schema: connects other labels' : 'declared');
    if (c.count) parts.push(`${c.count.toLocaleString('en-US')} in the graph`);
    return { label: c.label, note: parts.join(', ') };
  });
}

/**
 * Whether `label` may be used, from the engine's `labelChoices` answer: `null` when it may, else
 * why not (empty, or not among the choices of a closed schema). The engine checks again.
 */
export function labelProblem(choices, label) {
  const l = String(label ?? '').trim();
  if (!l) return 'Choose a label.';
  if (l.includes('::')) return 'A label cannot contain "::".';
  if (choices && !choices.free && !(choices.labels || []).some((c) => c.label === l)) {
    return choices.kind === 'edge'
      ? `The closed schema allows no "${l}" edge between these vertices.`
      : `The closed schema declares no vertex label "${l}".`;
  }
  return null;
}

/** The sentence when a closed schema leaves no edge label for a pair of vertices. */
export function noEdgeLabelText(choices, fromLabel, toLabel) {
  if (!choices || choices.free || (choices.labels || []).length) return null;
  return `The closed schema declares no edge from ${fromLabel || 'this vertex'} to ${toLabel || 'that vertex'}: add a connection to an edge label in the schema first.`;
}

/**
 * The confirmation text of a deletion: `{title, text}`; `edges` the edges a vertex takes with it
 * (the engine's dry-run count).
 */
export function deleteConfirmText(kind, id, label, edges = 0) {
  const what = `${kind} ${id}${label ? ` (${label})` : ''}`;
  if (kind !== 'vertex') return { title: `Delete ${what}?`, text: 'One commit.' };
  const withEdges = edges === 0 ? 'It has no edges.' : `Its ${edges} edge${edges === 1 ? '' : 's'} will be deleted with it.`;
  return { title: `Delete ${what}?`, text: `${withEdges} One commit.` };
}
