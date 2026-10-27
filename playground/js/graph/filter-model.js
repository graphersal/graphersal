// The Graph panel's filter model (pure: no DOM, no Vue; tested under Node by
// tests/graph-filter.test.mjs). The drawer (components/graph-filter.js) edits a FILTER STATE, this
// module turns it into the engine's `GraphFilter` (engine.js) and back into what the drawer shows.
//
// Filter state (what is persisted per graph, js/util.js `storage`, key `filterStorageKey`):
//   { enabled, hideIsolated,
//     vertices: { off: [label, ...], conds: { label: [{key, op, value, value2, type}, ...] } },
//     edges:    { off: [...],        conds: { ... } } }
// `off` lists the HIDDEN labels, so a label that appears later (a new label in the data) is
// visible by default. Condition values are kept as typed (text); `buildFilter` converts them by
// the key's type, which comes ONLY from the schema (declared, else inferred), never from the
// value text (root rule 7): a number key sends a JSON number, every other key a string. Each
// condition also keeps the `type` its key had when chosen, so a stored filter applies after a
// reload before the schema is inferred again (inference runs only when the drawer opens).
//
// The panel sends the engine's "others visible" form (`buildFilter(.., {othersVisible: true})`):
// the hidden labels by name, conditions where there are any, every other label visible, so a label
// beyond the statistics' list (they name only the largest) is never hidden by accident. Without the
// option the allow-list form is built (the visible labels listed, the others hidden). A side
// without hidden labels and without conditions is left out, so the unfiltered case costs nothing.

/** Timing and size thresholds of the filter drawer. */
export const FILTER_TUNING = Object.freeze({
  /** Changes apply this long after the last edit (automatic mode). */
  debounceMs: 300,
  /**
   * Graphs with more vertices + edges than this apply only on an explicit Apply (the `large`
   * sample, about 110 000 elements, is above it): each application reads the whole graph.
   */
  manualApplyAbove: 100000,
});

/** The operators (the engine's `op` names) with their labels and value arity. */
export const OPS = Object.freeze([
  Object.freeze({ id: 'eq', label: '=', arity: 1 }),
  Object.freeze({ id: 'neq', label: '≠', arity: 1 }),
  Object.freeze({ id: 'lt', label: '<', arity: 1 }),
  Object.freeze({ id: 'lte', label: '≤', arity: 1 }),
  Object.freeze({ id: 'gt', label: '>', arity: 1 }),
  Object.freeze({ id: 'gte', label: '≥', arity: 1 }),
  Object.freeze({ id: 'between', label: 'between', arity: 2 }),
  Object.freeze({ id: 'contains', label: 'contains', arity: 1 }),
  Object.freeze({ id: 'starts_with', label: 'starts with', arity: 1 }),
  Object.freeze({ id: 'ends_with', label: 'ends with', arity: 1 }),
  Object.freeze({ id: 'regex', label: 'regex', arity: 1 }),
  Object.freeze({ id: 'exists', label: 'exists', arity: 0 }),
  Object.freeze({ id: 'missing', label: 'missing', arity: 0 }),
]);
const OP_BY_ID = new Map(OPS.map((o) => [o.id, o]));

/** The operator ids each key type offers (`unknown`: string operators, values sent as strings). */
const OPS_OF_TYPE = Object.freeze({
  number: Object.freeze(['eq', 'neq', 'lt', 'lte', 'gt', 'gte', 'between', 'exists', 'missing']),
  temporal: Object.freeze(['eq', 'neq', 'lt', 'lte', 'gt', 'gte', 'between', 'exists', 'missing']),
  string: Object.freeze(['eq', 'neq', 'contains', 'starts_with', 'ends_with', 'regex', 'exists', 'missing']),
  boolean: Object.freeze(['eq', 'neq', 'exists', 'missing']),
});

/** The key types `opsForType` knows. */
export const KEY_TYPES = Object.freeze(['number', 'temporal', 'string', 'boolean', 'unknown']);

/** The operators (`OPS` entries) a key of `type` offers. */
export function opsForType(type) {
  const ids = OPS_OF_TYPE[type] || OPS_OF_TYPE.string;
  return ids.map((id) => OP_BY_ID.get(id));
}

/** The operator entry of `id`, or undefined. */
export function opInfo(id) {
  return OP_BY_ID.get(id);
}

const TEMPORAL_FORMATS = new Set(['date', 'date-time', 'local-date-time', 'time', 'offset-time', 'duration']);

/**
 * The filter type of one schema property node (`{"type": "integer"}`, `{"type": "string",
 * "format": "date"}`, `{"type": ["number", "null"]}`, `anyOf`): 'number' | 'temporal' | 'string' |
 * 'boolean' | 'unknown' (several kinds, arrays, objects, vectors, a missing node).
 */
export function typeOfSchemaNode(node) {
  if (!node || typeof node !== 'object') return 'unknown';
  if (Array.isArray(node.anyOf) || Array.isArray(node.oneOf)) {
    const kinds = new Set((node.anyOf || node.oneOf).map(typeOfSchemaNode).filter((t) => t !== 'null'));
    return kinds.size === 1 ? [...kinds][0] : 'unknown';
  }
  let t = node.type;
  if (Array.isArray(t)) {
    const rest = t.filter((x) => x !== 'null');
    if (rest.length === 1) t = rest[0];
    else if (rest.length === 2 && rest.includes('integer') && rest.includes('number')) t = 'number';
    else return 'unknown';
  }
  if (t === 'null') return 'null';
  if (t === 'integer' || t === 'number') return 'number';
  if (t === 'boolean') return 'boolean';
  if (t === 'string') return TEMPORAL_FORMATS.has(node.format) ? 'temporal' : 'string';
  return 'unknown';
}

/**
 * The property keys and their filter types per label from a schema in the schema format (the
 * stored one when it declares labels, else the inferred one): `{vertex: {label: {key: type}},
 * edge: {...}}`.
 */
export function keyTypesOf(schema) {
  const out = { vertex: {}, edge: {} };
  if (!schema || typeof schema !== 'object') return out;
  for (const [kind, section] of [['vertex', schema.vertices], ['edge', schema.edges]]) {
    for (const [label, entry] of Object.entries(section || {})) {
      const props = entry && entry.schema && entry.schema.properties;
      const types = {};
      if (props && typeof props === 'object') {
        for (const [key, node] of Object.entries(props)) {
          const t = typeOfSchemaNode(node);
          types[key] = t === 'null' ? 'unknown' : t;
        }
      }
      out[kind][label] = types;
    }
  }
  return out;
}

/** A filter state that filters nothing (enabled, so the first condition takes effect). */
export function emptyFilter() {
  return { enabled: true, hideIsolated: false, vertices: { off: [], conds: {} }, edges: { off: [], conds: {} } };
}

/** A new, empty condition (the key's type decides the operator list later). */
export function newCondition(key = '') {
  return { key, op: 'eq', value: '', value2: '' };
}

const text = (v) => (v === undefined || v === null ? '' : String(v));

function normalizeSide(raw) {
  const side = { off: [], conds: {} };
  if (!raw || typeof raw !== 'object') return side;
  if (Array.isArray(raw.off)) side.off = [...new Set(raw.off.filter((l) => typeof l === 'string'))];
  if (raw.conds && typeof raw.conds === 'object') {
    for (const [label, list] of Object.entries(raw.conds)) {
      if (!Array.isArray(list)) continue;
      const conds = list
        .filter((c) => c && typeof c === 'object')
        .map((c) => ({
          key: text(c.key),
          op: OP_BY_ID.has(c.op) ? c.op : 'eq',
          value: text(c.value),
          value2: text(c.value2),
          ...(KEY_TYPES.includes(c.type) ? { type: c.type } : {}),
        }));
      if (conds.length) side.conds[label] = conds;
    }
  }
  return side;
}

/** A filter state read back from storage (anything malformed becomes the empty filter's part). */
export function normalizeFilter(raw) {
  if (!raw || typeof raw !== 'object') return emptyFilter();
  return {
    enabled: raw.enabled !== false,
    hideIsolated: raw.hideIsolated === true,
    vertices: normalizeSide(raw.vertices),
    edges: normalizeSide(raw.edges),
  };
}

/** The localStorage key (without the page prefix) of the filter of graph `graph` (GraphInfo). */
export function filterStorageKey(graph) {
  if (!graph) return 'graph.filter';
  const kind = graph.kind || 'graph';
  const name = graph.name || '';
  return `graph.filter.${kind}:${name}`;
}

/**
 * The value of condition `cond` for a key of `type`: `{value}` (absent for `exists`/`missing`),
 * `{incomplete: true}` (nothing typed yet: the condition is not sent), or `{error}` (the text is
 * not a number for a number key: shown at the condition, not sent).
 */
export function conditionValue(cond, type) {
  const op = OP_BY_ID.get(cond.op);
  if (!op) return { error: `unknown operator "${cond.op}"` };
  if (op.arity === 0) return {};
  const one = (raw) => {
    const s = text(raw);
    if (type === 'number') {
      if (!s.trim()) return { incomplete: true };
      const n = Number(s.trim());
      return Number.isFinite(n) ? { value: n } : { error: 'enter a number' };
    }
    if (type === 'boolean') {
      if (s === 'true') return { value: true };
      if (s === 'false') return { value: false };
      return { incomplete: true };
    }
    if (!s) return { incomplete: true };
    return { value: s };
  };
  if (op.arity === 2) {
    const a = one(cond.value);
    const b = one(cond.value2);
    if (a.error || b.error) return { error: a.error || b.error };
    if (a.incomplete || b.incomplete) return { incomplete: true };
    return { value: [a.value, b.value] };
  }
  return one(cond.value);
}

/**
 * The type of condition `cond`'s key: the schema's (`types`, when loaded), else the type stored
 * with the condition when its key was chosen (so a stored filter applies after a reload without
 * inferring the schema first), else 'unknown'.
 */
export function keyType(types, kind, label, cond) {
  const t = types && types[kind] && types[kind][label] && types[kind][label][cond.key];
  return t || (KEY_TYPES.includes(cond.type) ? cond.type : 'unknown');
}

const FIELDS = Object.freeze([['vertices', 'vertex'], ['edges', 'edge']]);

/** The "others visible" form's field names per side. */
const OTHER_FIELDS = Object.freeze({
  vertices: Object.freeze({ other: 'otherVertices', hidden: 'hiddenVertices' }),
  edges: Object.freeze({ other: 'otherEdges', hidden: 'hiddenEdges' }),
});

/**
 * The engine conditions of `label` on `side` (complete ones, converted by key type); wrong values
 * go to `problems`, the UI rows of the sent ones to `sent["<field>.<label>"]`.
 */
function sendConditions(side, field, kind, label, types, problems, sent) {
  const list = [];
  const indices = [];
  ((side.conds || {})[label] || []).forEach((cond, index) => {
    if (!cond.key) return;
    const type = keyType(types, kind, label, cond);
    const v = conditionValue(cond, type);
    if (v.error) {
      problems.push({ field, label, index, message: v.error });
      return;
    }
    if (v.incomplete) return;
    const c = { key: cond.key, op: cond.op };
    if ('value' in v) c.value = v.value;
    list.push(c);
    indices.push(index);
  });
  if (list.length) sent[`${field}.${label}`] = indices;
  return list;
}

/**
 * The engine filter of filter state `model` over the known labels.
 * @param {Object} model  The filter state.
 * @param {{vertexLabels?: string[], edgeLabels?: string[], types?: {vertex: Object, edge: Object}, othersVisible?: boolean}} known
 *   The labels of the graph (statistics) and the key types (`keyTypesOf`). `othersVisible`: build
 *   the engine's "others visible" form (`otherVertices: 'visible'`, `hiddenVertices: [..]`, the
 *   conditioned labels in `vertices`; the same for edges), which does not depend on the label
 *   list (the badge still counts only hidden labels of the list); else the allow-list form.
 * @returns {{spec: Object|null, count: number, problems: Array<{field, label, index, message}>,
 *   sent: Object<string, number[]>}} `spec`: the GraphFilter (null: nothing is filtered; it is
 *   built whether or not the state is enabled); `count`: the active restrictions (hidden labels +
 *   sent conditions + hide isolated) for the badge; `problems`: conditions not sent because their
 *   value is wrong; `sent`: `"vertices.person"` -> the UI index of each condition sent, so an
 *   engine error about `vertices.person[i]` finds its row (`errorTarget`).
 */
export function buildFilter(model, known = {}) {
  const m = model || emptyFilter();
  const types = known.types || { vertex: {}, edge: {} };
  const labelsOf = { vertices: known.vertexLabels || [], edges: known.edgeLabels || [] };
  const spec = {};
  const problems = [];
  const sent = {};
  let count = 0;
  for (const [field, kind] of FIELDS) {
    const side = m[field] || { off: [], conds: {} };
    const off = new Set(side.off || []);
    const labels = labelsOf[field];
    const hidden = labels.filter((l) => off.has(l));
    let restricted = hidden.length > 0;
    count += hidden.length;
    const map = {};
    if (known.othersVisible) {
      // The "others visible" form: hidden labels by name, conditions only where there are any,
      // every other label visible (also those the statistics do not list).
      const conditioned = [...new Set([...labels, ...Object.keys(side.conds || {})])];
      for (const label of conditioned) {
        if (off.has(label)) continue;
        const list = sendConditions(side, field, kind, label, types, problems, sent);
        if (list.length) {
          count += list.length;
          map[label] = list;
        }
      }
      const names = OTHER_FIELDS[field];
      if (off.size || Object.keys(map).length) {
        spec[names.other] = 'visible';
        if (off.size) spec[names.hidden] = [...off];
        if (Object.keys(map).length) spec[field] = map;
      }
      continue;
    }
    for (const label of labels) {
      if (off.has(label)) continue;
      const list = sendConditions(side, field, kind, label, types, problems, sent);
      if (list.length) {
        restricted = true;
        count += list.length;
      }
      map[label] = list;
    }
    if (restricted) spec[field] = map;
  }
  if (m.hideIsolated) {
    spec.hideIsolated = true;
    count += 1;
  }
  return { spec: Object.keys(spec).length ? spec : null, count, problems, sent };
}

/** The filter to send: `built.spec` while the state is enabled, else null (the whole graph). */
export function activeSpec(model, built) {
  return model && model.enabled && built ? built.spec : null;
}

/**
 * The condition an engine error message names (`filter: vertices.person[0] (key "age", op "gt"):
 * ...`): `{field, label, index}` with `index` the condition's row in the drawer (through
 * `built.sent`), or null when the message names none of the sent conditions. Labels are matched
 * against those sent (the longest that fits), so a label with dots or brackets is found too.
 */
export function errorTarget(message, built) {
  const msg = text(message);
  if (!built || !built.sent) return null;
  let best = null;
  for (const [path, indices] of Object.entries(built.sent)) {
    const at = msg.indexOf(`${path}[`);
    if (at < 0) continue;
    const m = /^\[(\d+)\]/.exec(msg.slice(at + path.length));
    if (!m) continue;
    const i = Number(m[1]);
    if (i >= indices.length) continue;
    if (!best || path.length > best.path.length) best = { path, index: indices[i] };
  }
  if (!best) return null;
  const dot = best.path.indexOf('.');
  return { field: best.path.slice(0, dot), label: best.path.slice(dot + 1), index: best.index };
}

/** 'auto' (apply after `debounceMs`) or 'manual' (an Apply button) for a graph of `size` elements. */
export function applyMode(size, tuning = FILTER_TUNING) {
  return Number.isFinite(size) && size > tuning.manualApplyAbove ? 'manual' : 'auto';
}

/**
 * One filter request at a time, the newest wins: `push(value)` runs `run(value)` at once when
 * idle; while a run is in flight it only remembers the latest value and runs that afterwards
 * (older pending values are dropped). `run`'s result reaches `onResult(result, value)` only when
 * no newer value is waiting (a superseded answer is never shown); a rejection reaches
 * `onError(err, value)` under the same rule.
 * @param {(value:*) => Promise<*>} run
 * @param {{onResult?: Function, onError?: Function}} [handlers]
 */
export function createApplyQueue(run, { onResult = () => {}, onError = () => {} } = {}) {
  let busy = false;
  let pending = null; // {value} while a newer value waits
  async function loop(value) {
    busy = true;
    let current = value;
    for (;;) {
      try {
        const result = await run(current);
        if (!pending) onResult(result, current);
      } catch (err) {
        if (!pending) onError(err, current);
      }
      if (!pending) break;
      current = pending.value;
      pending = null;
    }
    busy = false;
  }
  return {
    push(value) {
      if (busy) {
        pending = { value };
        return;
      }
      loop(value);
    },
    get busy() { return busy; },
    get waiting() { return !!pending; },
  };
}

/**
 * Whether `label` of `field` ('vertices' | 'edges') is visible under GraphFilter `spec` (either
 * form: hidden by name, listed, or visible as one of the others).
 */
export function labelVisible(spec, field, label) {
  if (!spec) return true;
  const names = OTHER_FIELDS[field];
  if (names && Array.isArray(spec[names.hidden]) && spec[names.hidden].includes(label)) return false;
  const listed = !!spec[field] && Object.prototype.hasOwnProperty.call(spec[field], label);
  if (listed) return true;
  const other = names && spec[names.other];
  if (other === 'visible') return true;
  if (other === 'hidden') return false;
  return !spec[field];
}

/**
 * The label part of `spec` applied to a compact graph view (a query result's subgraph, a label
 * sample: they carry no properties, so conditions are not checked here): vertices whose primary
 * label is hidden and edges whose label is hidden or whose end is hidden are dropped;
 * `hideIsolated` drops vertices left without an edge. Returns `view` itself when nothing is
 * dropped, else a new view (degree columns kept) with `matchedNodes`/`matchedEdges` and `of`
 * (old vertex indices).
 */
export function labelFilterView(view, spec) {
  if (!view || !spec) return view;
  const n = view.vertexIds.length;
  const m = view.edgeIds.length;
  const keepV = new Uint8Array(n);
  for (let i = 0; i < n; i++) keepV[i] = labelVisible(spec, 'vertices', view.labels[view.vertexLabel[i]]) ? 1 : 0;
  const keepE = new Uint8Array(m);
  for (let j = 0; j < m; j++) {
    keepE[j] = keepV[view.edgeSource[j]] && keepV[view.edgeTarget[j]] && labelVisible(spec, 'edges', view.labels[view.edgeLabel[j]]) ? 1 : 0;
  }
  if (spec.hideIsolated) {
    const linked = new Uint8Array(n);
    for (let j = 0; j < m; j++) {
      if (keepE[j]) {
        linked[view.edgeSource[j]] = 1;
        linked[view.edgeTarget[j]] = 1;
      }
    }
    for (let i = 0; i < n; i++) if (!linked[i]) keepV[i] = 0;
  }
  const vi = [];
  for (let i = 0; i < n; i++) if (keepV[i]) vi.push(i);
  const ej = [];
  for (let j = 0; j < m; j++) if (keepE[j]) ej.push(j);
  if (vi.length === n && ej.length === m) return view;
  const map = new Int32Array(n).fill(-1);
  vi.forEach((old, k) => { map[old] = k; });
  const out = {
    ...view,
    vertexIds: vi.map((i) => view.vertexIds[i]),
    vertexLabel: Uint32Array.from(vi, (i) => view.vertexLabel[i]),
    vertexName: vi.map((i) => view.vertexName[i]),
    edgeIds: ej.map((j) => view.edgeIds[j]),
    edgeSource: Uint32Array.from(ej, (j) => map[view.edgeSource[j]]),
    edgeTarget: Uint32Array.from(ej, (j) => map[view.edgeTarget[j]]),
    edgeLabel: Uint32Array.from(ej, (j) => view.edgeLabel[j]),
    matchedNodes: vi.length,
    matchedEdges: ej.length,
    of: Uint32Array.from(vi),
  };
  for (const key of ['vertexInDegree', 'vertexOutDegree']) {
    if (view[key] && view[key].length === n) out[key] = Uint32Array.from(vi, (i) => view[key][i]);
    else delete out[key];
  }
  return out;
}

const fmt = (n) => Number(n).toLocaleString('en-US');
const SCOPE_NAMES = Object.freeze({ neighbourhood: 'the neighbourhood', result: 'the query result', sample: 'the sample' });

/**
 * The "Filtered: ..." strip above the drawing for `view` (with `matchedNodes`/`matchedEdges`), or
 * null when the view is not filtered. `scope`: 'graph' (X of the graph's totals), 'neighbourhood',
 * 'result' or 'sample' (what is left of that drawing; `before` = `{vertices, edges}` it had; a
 * neighbourhood's own `unfiltered` counts serve when `before` is not given).
 */
export function filteredText(view, scope = 'graph', before = null) {
  if (!view || !Number.isFinite(view.matchedNodes)) return null;
  const x = view.matchedNodes;
  const a = Number.isFinite(view.matchedEdges) ? view.matchedEdges : view.edgeIds.length;
  const v = (k) => (k === 1 ? 'vertex' : 'vertices');
  const e = (k) => (k === 1 ? 'edge' : 'edges');
  if (scope === 'graph') {
    return `Filtered: ${fmt(x)} of ${fmt(view.totalNodes)} ${v(view.totalNodes)}, ${fmt(a)} of ${fmt(view.totalEdges)} ${e(view.totalEdges)}`;
  }
  if (!before && view.unfiltered && Number.isFinite(view.unfiltered.nodes)) {
    before = { vertices: view.unfiltered.nodes, edges: view.unfiltered.edges };
  }
  if (before) {
    return `Filtered: ${fmt(x)} of ${fmt(before.vertices)} ${v(before.vertices)}, ${fmt(a)} of ${fmt(before.edges)} ${e(before.edges)} of ${SCOPE_NAMES[scope] || SCOPE_NAMES.neighbourhood}`;
  }
  return `Filtered: ${fmt(x)} ${v(x)} and ${fmt(a)} ${e(a)} of ${SCOPE_NAMES[scope] || SCOPE_NAMES.neighbourhood} pass`;
}

/**
 * The drawer's label rows of one side: every known label in alphabetical order with its count,
 * whether it is shown, and its conditions with their key types, op lists and the problem or
 * engine error at each row.
 * @param {Object} model  The filter state.
 * @param {'vertices'|'edges'} field
 * @param {Array<{label:string, count:number}>} labels
 * @param {Object} types  `keyTypesOf(..)`.
 * @param {{problems?: Array, error?: {field,label,index,message}|null}} [built]
 */
export function labelRows(model, field, labels, types, { problems = [], error = null } = {}) {
  const kind = field === 'vertices' ? 'vertex' : 'edge';
  const side = (model && model[field]) || { off: [], conds: {} };
  const off = new Set(side.off || []);
  const sorted = [...labels].sort((a, b) => a.label.localeCompare(b.label));
  return sorted.map(({ label, count }) => {
    const keyTypes = types[kind][label] || {};
    const conds = ((side.conds || {})[label] || []).map((c, index) => {
      const type = c.key ? keyType(types, kind, label, c) : 'unknown';
      const p = problems.find((x) => x.field === field && x.label === label && x.index === index);
      const engine = error && error.field === field && error.label === label && error.index === index ? error.message : null;
      return { ...c, index, type, ops: opsForType(type), arity: (OP_BY_ID.get(c.op) || { arity: 1 }).arity, problem: p ? p.message : engine };
    });
    return { label, count, on: !off.has(label), keys: Object.keys(keyTypes).sort(), conds };
  });
}

/** The state with every label of `field` shown (`on`) or hidden; conditions are kept. */
export function setAllLabels(model, field, labels, on) {
  const side = model[field];
  return { ...model, [field]: { ...side, off: on ? [] : [...new Set([...(side.off || []), ...labels])] } };
}

/** The state with `label` of `field` shown or hidden. */
export function setLabel(model, field, label, on) {
  const side = model[field];
  const off = new Set(side.off || []);
  if (on) off.delete(label);
  else off.add(label);
  return { ...model, [field]: { ...side, off: [...off] } };
}

/** The state with the conditions of `label` replaced by `conds` (an empty list removes them). */
export function setConditions(model, field, label, conds) {
  const side = model[field];
  const next = { ...(side.conds || {}) };
  if (conds.length) next[label] = conds;
  else delete next[label];
  return { ...model, [field]: { ...side, conds: next } };
}

/**
 * A condition after its key changed: the operator is kept when the new key's type offers it,
 * else it falls back to the first of its list.
 */
export function withKey(cond, key, type) {
  const ops = opsForType(type).map((o) => o.id);
  return { ...cond, key, type, op: ops.includes(cond.op) ? cond.op : ops[0] };
}

/**
 * An engine error as the drawer shows it at its condition: without the location prefix (`filter:
 * vertices.person[0] (key "k", op "o"): `, the row already says it) and without the `Help:`
 * paragraph (kept for the tooltip). `{text, help}`.
 */
export function conditionErrorText(message) {
  const msg = text(message);
  const cut = msg.search(/\s*\bHelp:/);
  const head = (cut >= 0 ? msg.slice(0, cut) : msg).trim();
  const help = cut >= 0 ? msg.slice(cut).trim() : '';
  const located = /^filter: (?:vertices|edges)\..*?\[\d+\](?: \(key "[^"]*"(?:, op "[^"]*")?\))?: /s.exec(head);
  return { text: located ? head.slice(located[0].length) : head, help };
}
