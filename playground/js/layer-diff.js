// The model of a LAYER's views (pure: no DOM, no Vue; tests/layer-diff.test.mjs): the top-bar
// badge, the "Layer changes" dialog (components/layer-diff.js) and whether the WebAssembly
// playground offers Start / Drop layer.
//
// The engine answers (crates/graphersal-session `layer.rs`, report S35.1):
// - `info.layer` (only on a layer): {base, baseRef: {graphId, commitSeq, committedAt}|null,
//   added: {vertices, edges}, modified: {...}, removed: {...}, ownBytes, sharedBytes};
// - `layerDiff(spec)` / `POST /api/layer/diff` with spec `{kinds?, labels?, limit?}`:
//   {ok: true, diff: {added: [entry], removed: [entry], modified: [change], truncated}} or
//   {ok: false, error}; entry = {kind, id, labels, properties, out?, in?}, change = {kind, id,
//   base_labels, labels, out?, in?, properties: {name: {base?, layer?}}} (a missing side is
//   absent, not null). Lists come vertices before edges.
// - `startLayer()` / `dropLayer()` (WebAssembly only): {ok: true, info} or {ok: false, error}.
// - `resetLayer()` / `POST /api/layer/reset` (any layer, both engines): {ok: true, reset: {added,
//   modified, removed: {vertices, edges}, schema, definitions}, info} or {ok: false, error}: every
//   change of the layer discarded in one commit, the layer kept (now equal to its base).

/** The limits the dialog offers (the engine's `limit`), the first is the default. */
export const DIFF_LIMITS = Object.freeze([100, 500, 2000]);

/** The kind filter's choices. */
export const DIFF_KINDS = Object.freeze([
  Object.freeze({ id: 'all', text: 'Vertices and edges' }),
  Object.freeze({ id: 'vertex', text: 'Vertices' }),
  Object.freeze({ id: 'edge', text: 'Edges' }),
]);

/** The groups of the dialog, in order, with their headings and what a click does. */
export const DIFF_GROUPS = Object.freeze([
  Object.freeze({ id: 'added', title: 'Added', note: 'new in this layer', focusable: true }),
  Object.freeze({ id: 'modified', title: 'Modified', note: 'base elements this layer changed: base → layer', focusable: true }),
  Object.freeze({ id: 'removed', title: 'Removed', note: 'base elements this layer removed: not in the graph any more, so they cannot be drawn', focusable: false }),
]);

/** The dialog's starting filter state. */
export function initialDiffState() {
  return { kind: 'all', labels: '', limit: DIFF_LIMITS[0] };
}

/** The labels of the filter's text: comma or space separated, trimmed, duplicates dropped. */
export function parseLabels(text) {
  const out = [];
  for (const part of String(text || '').split(/[,\s]+/)) {
    const label = part.trim();
    if (label && !out.includes(label)) out.push(label);
  }
  return out;
}

/**
 * The engine spec of a filter state: `{limit, kinds?, labels?}` (an unknown kind is both kinds;
 * a limit that is not a positive integer is the default).
 * @param {{kind?:string, labels?:string, limit?:number}} filter
 */
export function diffSpec(filter = {}) {
  const limit = Number.isInteger(filter.limit) && filter.limit > 0 ? filter.limit : DIFF_LIMITS[0];
  const spec = { limit };
  if (filter.kind === 'vertex' || filter.kind === 'edge') spec.kinds = filter.kind;
  const labels = parseLabels(filter.labels);
  if (labels.length) spec.labels = labels;
  return spec;
}

const isAbsent = (side) => side === undefined;

/** `[a, b]` -> `a, b`; empty -> `(no label)`. */
function labelText(labels) {
  return Array.isArray(labels) && labels.length ? labels.join(', ') : '(no label)';
}

/** The title of an entry: `vertex 7 · person` / `edge 9 · knows (1 → 2)`. */
function entryTitle(e) {
  const ends = e.kind === 'edge' && e.out !== undefined && e.in !== undefined ? ` (${e.out} → ${e.in})` : '';
  return `${e.kind} ${e.id} · ${labelText(e.labels)}${ends}`;
}

/** Kind order: vertices before edges (the engine sends them so; kept when a list is merged). */
const kindRank = (e) => (e && e.kind === 'edge' ? 1 : 0);

function sortByKind(list) {
  return list
    .map((e, k) => [e, k])
    .sort((a, b) => kindRank(a[0]) - kindRank(b[0]) || a[1] - b[1])
    .map(([e]) => e);
}

/**
 * The rows of a modified element's change: one per changed property `{name, base, layer,
 * change}` (`change`: 'set' when the layer added the property, 'unset' when it removed it,
 * 'changed'), values as text (`format`), an absent side as `(absent)`; a label change first as
 * the row `labels`.
 */
export function changeRows(change, format = defaultFormat) {
  const rows = [];
  const base = change.base_labels || [];
  const layer = change.labels || [];
  if (base.join('\u0000') !== layer.join('\u0000')) {
    rows.push({ name: 'labels', base: labelText(base), layer: labelText(layer), change: 'changed' });
  }
  for (const [name, sides] of Object.entries(change.properties || {})) {
    const s = sides || {};
    const kind = isAbsent(s.base) ? 'set' : (isAbsent(s.layer) ? 'unset' : 'changed');
    rows.push({
      name,
      base: isAbsent(s.base) ? '(absent)' : format(s.base),
      layer: isAbsent(s.layer) ? '(absent)' : format(s.layer),
      change: kind,
    });
  }
  return rows;
}

/** The property rows of an added or removed entry: `{name, value}` with the value as text. */
export function entryRows(entry, format = defaultFormat) {
  return Object.entries(entry.properties || {}).map(([name, value]) => ({ name, value: format(value) }));
}

function defaultFormat(v) {
  if (v === null || v === undefined) return 'null';
  if (typeof v === 'object') return JSON.stringify(v);
  return String(v);
}

/**
 * Where a click on an entry goes in the Graph panel: `{vertex, element: {kind, id}}`, `vertex`
 * being the vertex whose neighbourhood is drawn (the vertex itself, an edge's `out` end), or null
 * for a removed element (it is not in the graph any more).
 */
export function focusTarget(entry, group) {
  if (!entry || group === 'removed') return null;
  if (entry.kind === 'vertex') return { vertex: String(entry.id), element: { kind: 'vertex', id: String(entry.id) } };
  if (entry.kind === 'edge' && entry.out !== undefined && entry.out !== null) {
    return { vertex: String(entry.out), element: { kind: 'edge', id: String(entry.id) } };
  }
  return null;
}

/**
 * The dialog's model of a `layerDiff` answer: `{error}` for a refusal, else `{groups: [{id,
 * title, note, focusable, count, entries: [{key, kind, id, title, rows, focus}]}], truncated,
 * total, empty, truncatedText}`. Entries are vertices before edges in each group; added and
 * removed entries carry `rows` `{name, value}`, modified ones `{name, base, layer, change}`.
 * @param {Object|null} answer
 * @param {{format?:(v:any)=>string, limit?:number}} [options]
 */
export function diffModel(answer, { format = defaultFormat, limit = null } = {}) {
  if (!answer) return null;
  if (answer.ok === false) return { error: answer.error || 'The layer diff failed.' };
  const diff = answer.diff || {};
  const groups = DIFF_GROUPS.map((g) => {
    const list = sortByKind(Array.isArray(diff[g.id]) ? diff[g.id] : []);
    const entries = list.map((e) => ({
      key: `${g.id}:${e.kind}:${e.id}`,
      kind: e.kind,
      id: e.id,
      title: entryTitle(e),
      rows: g.id === 'modified' ? changeRows(e, format) : entryRows(e, format),
      focus: g.focusable ? focusTarget(e, g.id) : null,
    }));
    return { ...g, count: entries.length, entries };
  });
  const total = groups.reduce((sum, g) => sum + g.count, 0);
  const truncated = diff.truncated === true;
  return {
    groups,
    total,
    empty: total === 0,
    truncated,
    truncatedText: truncated
      ? `Truncated: more changes match than the limit${limit ? ` of ${limit}` : ''} shows. Raise the limit or narrow the filter.`
      : null,
  };
}

// ---------------------------------------------------------------- the badge

const count = (side) => (side ? (Number(side.vertices) || 0) + (Number(side.edges) || 0) : 0);

/** `+3 ~1 −2` and the per-kind detail of a layer's counts. */
export function changeCounts(layer) {
  if (!layer) return null;
  const added = count(layer.added);
  const modified = count(layer.modified);
  const removed = count(layer.removed);
  const side = (s) => `${(s && s.vertices) || 0} vertices, ${(s && s.edges) || 0} edges`;
  return {
    added,
    modified,
    removed,
    total: added + modified + removed,
    text: `+${added} ~${modified} −${removed}`,
    detail: `added ${side(layer.added)}; modified ${side(layer.modified)}; removed ${side(layer.removed)}`,
  };
}

/**
 * The top-bar badge of a layer (`info.layer`), or null on a plain graph: `{text, counts, title,
 * label}`; `format` turns bytes into text.
 */
export function layerBadge(layer, format = (n) => `${n} B`) {
  if (!layer) return null;
  const counts = changeCounts(layer);
  const base = layer.base || 'base';
  const ref = layer.baseRef;
  const at = ref && Number.isFinite(ref.commitSeq) ? ` at commit ${ref.commitSeq}` : '';
  const bytes = Number.isFinite(layer.ownBytes) || Number.isFinite(layer.sharedBytes)
    ? ` Memory: the layer's own ${format(layer.ownBytes || 0)}, the shared base ${format(layer.sharedBytes || 0)}.`
    : '';
  return {
    text: `Layer over ${base}`,
    counts,
    title: `A layer over ${base}${at}: writes go to the layer, the base stays unchanged. Changes: ${counts.detail}.${bytes} Click: the layer's changes.`,
    label: `Layer over ${base}, ${counts.added} added, ${counts.modified} modified, ${counts.removed} removed. Show the layer's changes`,
  };
}

// ---------------------------------------------------------------- start / drop (WebAssembly)

/**
 * Whether Start layer / Drop layer are offered: only in the WebAssembly playground (on the dev
 * server a layer comes from the CLI flags), with an engine that has them (`engineInfo.layers`: an
 * older cached worker does not), a graph loaded, no read-only view of a past state, the profile
 * key `layerDiff`. Start needs no layer yet, Drop a layer. Each comes with the reason when not.
 * @param {{server?:boolean, engineLayers?:boolean, graph?:Object|null, storeView?:Object|null,
 *          allowed?:boolean, running?:boolean}} s
 * @returns {{start:boolean, drop:boolean, reason:string|null}}
 */
export function layerActions({ server = false, engineLayers = false, graph = null, storeView = null, allowed = true, running = false } = {}) {
  const none = (reason) => ({ start: false, drop: false, reason });
  if (!allowed) return none('not in this UI profile');
  if (server) return none('the dev server serves the layer its command line names (--layer)');
  if (!engineLayers) return none('this engine build has no layers (rebuild the playground)');
  if (!graph) return none('no graph is loaded');
  if (storeView) return none('a read-only view of a past state is shown');
  if (running) return { start: false, drop: false, reason: 'a query is running' };
  const layer = !!graph.layer;
  return { start: !layer, drop: layer, reason: null };
}

/** The confirmation text of Drop layer (it discards the layer's changes). */
export function dropConfirmText(layer) {
  const c = changeCounts(layer);
  const what = c && c.total ? `its ${c.total} change${c.total === 1 ? '' : 's'} (${c.text})` : 'the layer (it has no changes)';
  return `Drop the layer and discard ${what}? The base graph comes back unchanged.`;
}

// ---------------------------------------------------------------- discard all changes (both engines)

/**
 * Whether "Discard all changes" is offered in the Layer changes dialog: on any layer (the
 * WebAssembly playground's and the dev server's `--layer` / `--layer-store`), with an engine that
 * has `resetLayer` (`engineReset`: an older cached worker lacks it), no read-only view of a past
 * state, a writable graph (`readOnly`: the reason it is not, e.g. a read-only server), no query
 * running, the profile key `layerDiff`. With the reason when not.
 * @param {{engineReset?:boolean, graph?:Object|null, storeView?:Object|null, readOnly?:string|null,
 *          allowed?:boolean, running?:boolean}} s
 * @returns {{reset:boolean, reason:string|null}}
 */
export function resetAction({ engineReset = false, graph = null, storeView = null, readOnly = null, allowed = true, running = false } = {}) {
  const no = (reason) => ({ reset: false, reason });
  if (!allowed) return no('not in this UI profile');
  if (!graph || !graph.layer) return no('the graph is not a layer');
  if (!engineReset) return no('this engine build cannot discard a layer\'s changes (rebuild the playground)');
  if (storeView) return no('a read-only view of a past state is shown');
  if (readOnly) return no(readOnly);
  if (running) return no('a query is running');
  return { reset: true, reason: null };
}

/** The confirmation text of "Discard all changes" (the layer stays, its changes go). */
export function resetConfirmText(layer) {
  const c = changeCounts(layer);
  const base = (layer && layer.base) || 'the base';
  const what = c && c.total
    ? `all ${c.total} change${c.total === 1 ? '' : 's'} of this layer (${c.text})`
    : 'every change of this layer';
  return `Discard ${what}, its own schema and catalog changes included? The layer stays and shows ${base} again; ${base} itself is not changed. One commit.`;
}

/** The notice after a reset (`answer.reset`, the layer's base name). */
export function resetDoneText(reset, base = 'its base') {
  if (!reset) return `Layer changes discarded: the layer equals ${base} again.`;
  const n = count(reset.added) + count(reset.modified) + count(reset.removed);
  const extra = [reset.schema ? 'the schema' : null, reset.definitions ? `${reset.definitions} catalog definition${reset.definitions === 1 ? '' : 's'}` : null]
    .filter(Boolean);
  if (!n && !extra.length) return `The layer had no changes: it already equals ${base}.`;
  const parts = [`${n} element change${n === 1 ? '' : 's'}`, ...extra];
  return `Layer changes discarded (${parts.join(', ')}): the layer equals ${base} again.`;
}
