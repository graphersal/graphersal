// The schema editor's model: pure functions and the `SchemaDraft` class, no Vue, no DOM, no
// engine (tested under Node: playground/tests/schema-draft.test.mjs).
//
// A schema is plain data in the schema format (book: "Schemas"): an envelope (`$schema`, `meta`,
// `mode`, `vertices`, `edges`) around JSON Schema 2020-12 nodes. The editor never changes the
// stored schema directly: every edit goes into a DRAFT, a copy of the working schema (the stored
// one, else the inferred one). The engine sees the draft only through the seven schema methods
// (`js/schema/api.js`): `diff_schema` for "What changes", `validate_schema_patch` for "Check
// against data", `patch_schema` for "Apply", with a JSON Merge Patch (RFC 7396) computed here.
//
// Paths are arrays of keys into the schema document, e.g.
// ['vertices', 'person', 'schema', 'properties', 'address', 'properties', 'zip'].

/** The date and time types (book "Dates and Times"): a string node with their `format`. */
export const TEMPORAL_BASES = Object.freeze(['datetime', 'localdatetime', 'date', 'time', 'offsettime', 'duration']);

/** The base types of the type dropdown, in menu order. */
export const BASE_TYPES = Object.freeze(['string', 'integer', 'number', 'boolean', 'uuid', ...TEMPORAL_BASES, 'vector', 'array', 'object', 'null', 'any']);

/** The element kinds of a vector node (`{"type": "array", "format": "vector", "element": ..}`). */
export const VECTOR_ELEMENTS = Object.freeze(['f32', 'f16', 'i8', 'bit']);

/** The metrics of a vector node's `metric` (the default metric of `similarity`/`nearest`). */
export const VECTOR_METRICS = Object.freeze(['cosine', 'dot', 'l2', 'hamming']);

/** The largest `dimension` of a vector node (the engine's `MAX_VECTOR_DIMENSION`). */
export const MAX_VECTOR_DIMENSION = 65536;

/** The `format` of a base type written as `{"type": "string", "format": ..}`. */
export const FORMAT_OF_BASE = Object.freeze({
  uuid: 'uuid', datetime: 'date-time', localdatetime: 'local-date-time', date: 'date', time: 'time',
  offsettime: 'offset-time', duration: 'duration',
});
const BASE_OF_FORMAT = Object.freeze(Object.fromEntries(Object.entries(FORMAT_OF_BASE).map(([b, f]) => [f, b])));

/** The base type of a `string` node: its format's type (`uuid`, `date`, ...) or `string`. */
function stringBase(node) {
  return (node && BASE_OF_FORMAT[node.format]) || 'string';
}

/** The bounds of a date and time type: compared as values of that type, written as its text. */
const FORMAT_BOUNDS = ['formatMinimum', 'formatMaximum', 'formatExclusiveMinimum', 'formatExclusiveMaximum'];

/** The schema modes. */
export const MODES = Object.freeze(['none', 'open', 'closed']);

/** Constraint keywords per base type (JSON Schema spelling), in form order. */
export const CONSTRAINTS = Object.freeze({
  integer: ['minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum'],
  number: ['minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum'],
  string: ['minLength', 'maxLength', 'pattern'],
  uuid: [],
  ...Object.fromEntries(TEMPORAL_BASES.map((b) => [b, FORMAT_BOUNDS])),
  array: ['minItems', 'maxItems'],
  // `element`, `dimension`, `metric` and `embedder` are edited in the form's Vector block (not constraints).
  vector: [],
  object: [],
  boolean: [],
  null: [],
  any: [],
  union: [],
});

/**
 * The `defaultFunction`s a base type admits (the engine's `check_defaults`): each produces a value
 * of exactly that type, for a top-level property only.
 */
export const DEFAULT_FUNCTIONS = Object.freeze({
  uuid: Object.freeze(['uuidv4', 'uuidv7']),
  datetime: Object.freeze(['now']),
  date: Object.freeze(['today']),
});

/**
 * Whether `path` is a label's top-level property (`[vertices|edges, label, 'schema', 'properties',
 * key]`): the only place where `default`/`defaultFunction` are applied on create.
 */
export function isTopLevelProperty(path) {
  return path.length === 5 && path[2] === 'schema' && path[3] === 'properties';
}

/**
 * The create-time default of a property node for the table: `default "x"`, `default now()`, or
 * null without one. Read from the property node itself (annotations are not on the value node).
 */
export function defaultSummary(node) {
  if (!isPlainObject(node)) return null;
  if (typeof node.defaultFunction === 'string') return `default ${node.defaultFunction}()`;
  if ('default' in node) return `default ${short(node.default)}`;
  return null;
}

/** Keywords that constrain any kind of value. */
export const VALUE_CONSTRAINTS = Object.freeze(['enum', 'const']);

/** Annotation keywords: stored and round-tripped, no effect on validation. */
export const ANNOTATIONS = Object.freeze(['title', 'description', 'default', 'defaultFunction', 'examples', 'deprecated', '$comment']);

/** Keywords that belong to one kind of value; removed when the type changes to another kind. */
const KIND_KEYWORDS = {
  minimum: 'numeric', maximum: 'numeric', exclusiveMinimum: 'numeric', exclusiveMaximum: 'numeric',
  minLength: 'string', maxLength: 'string', pattern: 'string', format: 'string',
  minItems: 'array', maxItems: 'array', items: 'array',
  properties: 'object', required: 'object', additionalProperties: 'object',
  formatMinimum: 'temporal', formatMaximum: 'temporal', formatExclusiveMinimum: 'temporal', formatExclusiveMaximum: 'temporal',
  element: 'vector', dimension: 'vector', metric: 'vector', embedder: 'vector',
};

const KIND_OF_BASE = {
  integer: 'numeric', number: 'numeric', string: 'string', uuid: 'string', array: 'array', object: 'object', vector: 'vector',
  ...Object.fromEntries(TEMPORAL_BASES.map((b) => [b, 'temporal'])),
};

// ------------------------------------------------------------------ plain data helpers

export function isPlainObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

export function clone(v) {
  return v === undefined ? undefined : JSON.parse(JSON.stringify(v));
}

/** Deep equality of JSON values; object key order is ignored, array order is not. */
export function deepEqual(a, b) {
  if (a === b) return true;
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    return a.every((x, i) => deepEqual(x, b[i]));
  }
  if (isPlainObject(a)) {
    if (!isPlainObject(b)) return false;
    const ka = Object.keys(a);
    if (ka.length !== Object.keys(b).length) return false;
    return ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && deepEqual(a[k], b[k]));
  }
  return false;
}

/** The value at `path`, or undefined. */
export function getAt(doc, path) {
  let v = doc;
  for (const key of path) {
    if (v === null || typeof v !== 'object') return undefined;
    v = v[key];
  }
  return v;
}

/** The object at `path`, created (as `{}`) along the way when missing. */
function ensureAt(doc, path) {
  let v = doc;
  for (const key of path) {
    if (!isPlainObject(v[key])) v[key] = {};
    v = v[key];
  }
  return v;
}

/** A copy of `obj` with key `from` renamed to `to`, keeping the key order. */
function renameKey(obj, from, to) {
  const out = {};
  for (const [k, v] of Object.entries(obj)) out[k === from ? to : k] = v;
  return out;
}

// ------------------------------------------------------------------ JSON Merge Patch (RFC 7396)

/** RFC 7396 MergePatch(target, patch): `null` removes a member, an object merges, anything else replaces. */
export function applyMergePatch(target, patch) {
  if (!isPlainObject(patch)) return clone(patch);
  const result = isPlainObject(target) ? clone(target) : {};
  for (const [k, v] of Object.entries(patch)) {
    if (v === null) delete result[k];
    else result[k] = applyMergePatch(result[k], v);
  }
  return result;
}

/**
 * The merge patch that turns `base` into `target`. `exact` is false when no merge patch can
 * express the change: RFC 7396 cannot set a member TO `null` (a `"default": null`, a
 * `"const": null`); the editor then applies the whole draft with `set_schema` instead.
 * @returns {{patch: Object, exact: boolean}}
 */
export function createMergePatch(base, target) {
  const patch = mergePatchOf(base, target);
  return { patch, exact: deepEqual(applyMergePatch(base, patch), target) };
}

function mergePatchOf(base, target) {
  if (!isPlainObject(base) || !isPlainObject(target)) return clone(target);
  const patch = {};
  for (const k of Object.keys(base)) if (!(k in target)) patch[k] = null;
  for (const [k, v] of Object.entries(target)) {
    if (!(k in base)) patch[k] = clone(v);
    else if (!deepEqual(base[k], v)) patch[k] = mergePatchOf(base[k], v);
  }
  return patch;
}

// ------------------------------------------------------------------ reading a node

/**
 * What a JSON Schema node means for the editor.
 * - `base`: one of BASE_TYPES, or 'union' (several kinds: edited as JSON).
 * - `nullable`: `null` is admitted (`["T", "null"]` or `anyOf: [T, {"type": "null"}]`).
 * - `value`: the path, relative to the node, of the node that carries the type, constraints and
 *   `properties`/`items`: `[]`, or `['anyOf', i]` for the nullable `anyOf` form (Pydantic's
 *   `Optional[X]`). Annotations always stay on the node itself.
 * @returns {{base: string, nullable: boolean, value: Array<string|number>}}
 */
export function describe(node) {
  if (!isPlainObject(node)) return { base: 'any', nullable: true, value: [] };
  if (Array.isArray(node.anyOf)) {
    const members = node.anyOf;
    const nulls = members.filter((m) => isPlainObject(m) && m.type === 'null' && Object.keys(m).length === 1);
    const others = members.filter((m) => !nulls.includes(m));
    if (nulls.length === 1 && others.length === 1 && !('type' in node)) {
      const inner = describe(others[0]);
      if (inner.base !== 'union' && inner.value.length === 0 && !inner.nullable) {
        return { base: inner.base, nullable: true, value: ['anyOf', members.indexOf(others[0])] };
      }
    }
    return { base: 'union', nullable: nulls.length > 0, value: [] };
  }
  if (!('type' in node)) return { base: 'any', nullable: true, value: [] };
  const types = Array.isArray(node.type) ? node.type : [node.type];
  const nullable = types.includes('null');
  const kinds = types.filter((t) => t !== 'null');
  if (kinds.length === 0) return { base: 'null', nullable: true, value: [] };
  if (kinds.length > 1) return { base: 'union', nullable, value: [] };
  let base = kinds[0];
  if (base === 'string') base = stringBase(node);
  if (base === 'array' && node.format === 'vector') base = 'vector';
  if (!BASE_TYPES.includes(base)) base = 'union';
  return { base, nullable, value: [] };
}

/** The node that carries a node's type, constraints and children (see `describe`). */
export function valueNode(node) {
  return getAt(node, describe(node).value) || {};
}

/** The type as the tables show it: `string`, `array of integer`, `object {3 fields}`. */
export function typeText(node, { collapsed = false } = {}) {
  const d = describe(node);
  const v = valueNode(node);
  if (d.base === 'union') return unionText(node);
  if (d.base === 'vector') {
    const element = VECTOR_ELEMENTS.includes(v.element) ? v.element : 'f32';
    return Number.isInteger(v.dimension) ? `vector<${element}, ${v.dimension}>` : `vector<${element}>`;
  }
  if (d.base === 'array') {
    if (!isPlainObject(v.items)) return 'array';
    return `array of ${typeText(v.items, { collapsed: true })}`;
  }
  if (d.base === 'object' && collapsed) {
    const n = isPlainObject(v.properties) ? Object.keys(v.properties).length : 0;
    return `object {${n} field${n === 1 ? '' : 's'}}`;
  }
  return d.base;
}

function unionText(node) {
  if (Array.isArray(node.anyOf)) {
    return `anyOf<${node.anyOf.map((m) => (describe(m).base === 'null' ? 'null' : typeText(m, { collapsed: true }))).join(' | ')}>`;
  }
  const types = Array.isArray(node.type) ? node.type : [node.type];
  return types.filter((t) => t !== 'null').map((t) => (t === 'string' ? stringBase(node) : t)).join(' | ');
}

/** A JSON value in a compact form for the constraint column. */
function short(v, max = 24) {
  const text = typeof v === 'string' ? v : JSON.stringify(v);
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

function range(lo, hi, loEx, hiEx) {
  if (lo !== undefined && hi !== undefined && !loEx && !hiEx) return `${lo}..${hi}`;
  const parts = [];
  if (lo !== undefined) parts.push(`${loEx ? '>' : '>='} ${lo}`);
  if (hi !== undefined) parts.push(`${hiEx ? '<' : '<='} ${hi}`);
  return parts.join(', ');
}

/**
 * The compact constraint column: `0..150`, `<= 10`, `length 1..80`, `^\d{5}$`, `items >= 1`,
 * `one of a, b`, `= x`. One string per constraint group.
 * @returns {string[]}
 */
export function constraintSummary(node) {
  const v = valueNode(node);
  const out = [];
  const lo = v.exclusiveMinimum !== undefined ? v.exclusiveMinimum : v.minimum;
  const hi = v.exclusiveMaximum !== undefined ? v.exclusiveMaximum : v.maximum;
  if (lo !== undefined || hi !== undefined) out.push(range(lo, hi, v.exclusiveMinimum !== undefined, v.exclusiveMaximum !== undefined));
  if (v.minLength !== undefined || v.maxLength !== undefined) out.push(`length ${range(v.minLength, v.maxLength)}`);
  if (v.pattern !== undefined) out.push(String(v.pattern));
  if (v.minItems !== undefined || v.maxItems !== undefined) out.push(`items ${range(v.minItems, v.maxItems)}`);
  const flo = v.formatExclusiveMinimum !== undefined ? v.formatExclusiveMinimum : v.formatMinimum;
  const fhi = v.formatExclusiveMaximum !== undefined ? v.formatExclusiveMaximum : v.formatMaximum;
  if (flo !== undefined || fhi !== undefined) {
    out.push(range(flo, fhi, v.formatExclusiveMinimum !== undefined, v.formatExclusiveMaximum !== undefined));
  }
  if (Array.isArray(v.enum)) out.push(`one of ${v.enum.map((x) => short(x, 12)).join(', ')}`);
  if (v.const !== undefined) out.push(`= ${short(v.const)}`);
  return out;
}

/** The `properties` of an object node (through a nullable `anyOf`), `{}` when none. */
export function propertiesOf(node) {
  const v = valueNode(node);
  return isPlainObject(v.properties) ? v.properties : {};
}

/** The path of the object node whose `properties` the nested table of a property shows, or null. */
export function nestedObjectPath(doc, path) {
  const node = getAt(doc, path);
  const d = describe(node);
  const vp = [...path, ...d.value];
  if (d.base === 'object') return vp;
  if (d.base === 'array') {
    const items = getAt(doc, [...vp, 'items']);
    if (!isPlainObject(items)) return null;
    const di = describe(items);
    if (di.base === 'object') return [...vp, 'items', ...di.value];
  }
  return null;
}

// ------------------------------------------------------------------ editing a node (in place)

/** Sets the base type of the node at `path`, keeping its nullability and annotations. */
export function setBaseType(doc, path, base) {
  if (!BASE_TYPES.includes(base)) throw new Error(`Unknown type: ${base}`);
  const node = getAt(doc, path);
  if (!isPlainObject(node)) return;
  const d = describe(node);
  if (d.base === base) return;
  // A nullable `anyOf` collapses to the plain form: the member's keywords move up.
  if (d.value.length) {
    const member = getAt(node, d.value);
    delete node.anyOf;
    for (const [k, v] of Object.entries(member)) if (!(k in node)) node[k] = v;
  }
  if (d.base === 'union') delete node.anyOf;
  const nullable = d.nullable && base !== 'any' && base !== 'null';
  const kind = KIND_OF_BASE[base];
  for (const key of Object.keys(node)) {
    if (KIND_KEYWORDS[key] && KIND_KEYWORDS[key] !== kind) delete node[key];
  }
  delete node.enum;
  delete node.const;
  if (d.base !== base) for (const k of FORMAT_BOUNDS) delete node[k];
  delete node.format;
  delete node.type;
  if (base === 'any') return;
  const format = FORMAT_OF_BASE[base];
  const t = format ? 'string' : base === 'vector' ? 'array' : base;
  node.type = base === 'null' ? 'null' : nullable ? [t, 'null'] : t;
  if (format) node.format = format;
  if (base === 'vector') {
    node.format = 'vector';
    node.element = 'f32';
  }
  if (base === 'array' && !isPlainObject(node.items)) node.items = { type: 'string' };
  if (base === 'object' && !isPlainObject(node.properties)) node.properties = {};
  reorderType(node);
}

/** Puts `type` (and `format`) first, as the canonical form does, for a readable JSON tab. */
function reorderType(node) {
  const entries = Object.entries(node);
  const first = entries.filter(([k]) => k === 'type' || k === 'format');
  const rest = entries.filter(([k]) => k !== 'type' && k !== 'format');
  for (const k of Object.keys(node)) delete node[k];
  for (const [k, v] of [...first, ...rest]) node[k] = v;
}

/** Admits or forbids `null` for the node at `path` (a no-op for `any`, `null` and unions). */
export function setNullable(doc, path, nullable) {
  const node = getAt(doc, path);
  if (!isPlainObject(node)) return;
  const d = describe(node);
  if (d.base === 'any' || d.base === 'null' || d.nullable === nullable) return;
  if (d.value.length) {
    // `anyOf: [X, null]` without the null: X itself, with the wrapper's annotations.
    const member = getAt(node, d.value);
    delete node.anyOf;
    for (const [k, v] of Object.entries(member)) if (!(k in node)) node[k] = v;
    reorderType(node);
    return;
  }
  if (d.base === 'union' && Array.isArray(node.anyOf)) {
    if (nullable) node.anyOf.push({ type: 'null' });
    else node.anyOf = node.anyOf.filter((m) => !(isPlainObject(m) && m.type === 'null'));
    return;
  }
  const types = (Array.isArray(node.type) ? node.type : [node.type]).filter((t) => t !== 'null');
  if (nullable) types.push('null');
  node.type = types.length === 1 ? types[0] : types;
}

/** Sets (or, with `undefined`/'' , removes) a keyword of the node at `path`. */
export function setKeyword(doc, path, keyword, value) {
  const node = getAt(doc, path);
  if (!isPlainObject(node)) return;
  if (value === undefined || value === '') delete node[keyword];
  else node[keyword] = value;
}

/** Marks key `key` of the object node at `objPath` as required or optional. */
export function setRequired(doc, objPath, key, required) {
  const node = getAt(doc, objPath);
  if (!isPlainObject(node)) return;
  const list = Array.isArray(node.required) ? node.required.filter((k) => k !== key) : [];
  if (required) list.push(key);
  if (list.length) node.required = list;
  else delete node.required;
}

export function isRequired(doc, objPath, key) {
  const node = getAt(doc, objPath);
  return !!(node && Array.isArray(node.required) && node.required.includes(key));
}

function checkName(name, what) {
  const n = String(name ?? '').trim();
  if (!n) throw new Error(`The ${what} needs a name`);
  return n;
}

/** Adds property `name` (a string) to the object node at `objPath` (created when missing). */
export function addProperty(doc, objPath, name, node = { type: 'string' }) {
  const n = checkName(name, 'property');
  const obj = ensureAt(doc, objPath);
  if (!obj.type && !obj.anyOf) obj.type = 'object';
  if (!isPlainObject(obj.properties)) obj.properties = {};
  if (n in obj.properties) throw new Error(`Property "${n}" already exists`);
  obj.properties[n] = node;
  return n;
}

export function renameProperty(doc, objPath, from, to) {
  const n = checkName(to, 'property');
  const obj = getAt(doc, objPath);
  if (!obj || !isPlainObject(obj.properties) || !(from in obj.properties) || n === from) return from;
  if (n in obj.properties) throw new Error(`Property "${n}" already exists`);
  obj.properties = renameKey(obj.properties, from, n);
  if (Array.isArray(obj.required)) obj.required = obj.required.map((k) => (k === from ? n : k));
  return n;
}

export function removeProperty(doc, objPath, key) {
  const obj = getAt(doc, objPath);
  if (!obj || !isPlainObject(obj.properties)) return;
  delete obj.properties[key];
  if (Array.isArray(obj.required)) {
    obj.required = obj.required.filter((k) => k !== key);
    if (!obj.required.length) delete obj.required;
  }
}

// ------------------------------------------------------------------ labels and envelope

const SECTION = { vertex: 'vertices', edge: 'edges' };

export function addLabel(doc, kind, name) {
  const n = checkName(name, `${kind} label`);
  const section = ensureAt(doc, [SECTION[kind]]);
  if (n in section) throw new Error(`The ${kind} label "${n}" already exists`);
  section[n] = kind === 'edge'
    ? { connections: [], schema: { type: 'object', properties: {} } }
    : { schema: { type: 'object', properties: {} } };
  return n;
}

/** Renames a label; a renamed vertex label is renamed in every edge connection too. */
export function renameLabel(doc, kind, from, to) {
  const n = checkName(to, `${kind} label`);
  const key = SECTION[kind];
  if (!isPlainObject(doc[key]) || !(from in doc[key]) || n === from) return from;
  if (n in doc[key]) throw new Error(`The ${kind} label "${n}" already exists`);
  doc[key] = renameKey(doc[key], from, n);
  if (kind === 'vertex' && isPlainObject(doc.edges)) {
    for (const edge of Object.values(doc.edges)) {
      for (const c of Array.isArray(edge && edge.connections) ? edge.connections : []) {
        if (c.from === from) c.from = n;
        if (c.to === from) c.to = n;
      }
    }
  }
  return n;
}

export function removeLabel(doc, kind, name) {
  const key = SECTION[kind];
  if (isPlainObject(doc[key])) delete doc[key][name];
}

export function addConnection(doc, edgeLabel, from = '*', to = '*') {
  const edge = ensureAt(doc, ['edges', edgeLabel]);
  if (!Array.isArray(edge.connections)) edge.connections = [];
  edge.connections.push({ from, to });
}

export function setConnection(doc, edgeLabel, index, side, value) {
  const c = getAt(doc, ['edges', edgeLabel, 'connections', index]);
  if (c) c[side] = value;
}

export function removeConnection(doc, edgeLabel, index) {
  const list = getAt(doc, ['edges', edgeLabel, 'connections']);
  if (Array.isArray(list)) list.splice(index, 1);
}

// ------------------------------------------------------------------ text form

/**
 * Parses the JSON tab's text. A schema is a JSON object; anything else is an error the tab shows
 * inline (the forms keep the last valid state).
 * @returns {{ok: true, value: Object} | {ok: false, error: string}}
 */
export function parseSchemaText(text) {
  let value;
  try {
    value = JSON.parse(text);
  } catch (err) {
    return { ok: false, error: `Not valid JSON: ${err && err.message ? err.message : err}` };
  }
  if (!isPlainObject(value)) return { ok: false, error: 'A schema is a JSON object ({ "mode": ..., "vertices": ..., "edges": ... })' };
  return { ok: true, value };
}

export function schemaText(schema) {
  return JSON.stringify(schema ?? {}, null, 2);
}

// ------------------------------------------------------------------ queries for the Results panel

/** A Rhai string literal (JSON's escapes are Rhai's). */
export function rhaiString(s) {
  return JSON.stringify(String(s));
}

/** A JSON value as a Rhai literal: maps `#{..}`, `()` for null. */
export function rhaiLiteral(v) {
  if (v === null || v === undefined) return '()';
  if (Array.isArray(v)) return `[${v.map(rhaiLiteral).join(', ')}]`;
  if (isPlainObject(v)) return `#{${Object.entries(v).map(([k, x]) => `${rhaiString(k)}: ${rhaiLiteral(x)}`).join(', ')}}`;
  if (typeof v === 'number' && Number.isInteger(v) && !Number.isSafeInteger(v)) return String(v);
  return typeof v === 'string' ? rhaiString(v) : String(v);
}

const ELEMENT_WORD = { vertex: ['vertex', 'vertices'], edge: ['edge', 'edges'] };

/** A sample id the report cut (longer than 40 characters) cannot be looked up. */
const isCutId = (id) => typeof id === 'string' && id.endsWith('…');

/**
 * The query that shows the offending elements of one violation group of `validate_schema(_patch)`
 * (`{kind, element, label, field, example_field, detail, count, sample_ids, omitted_ids}`), with a
 * comment line that names the rule (and the first concrete path of a `[*]` field). Exact filters for the rules a traversal can express (`unknown_label`,
 * top-level `missing_required` and `undeclared_property`); the sample ids otherwise.
 * @returns {{title: string, script: string}}
 */
export function violationQuery(group) {
  const source = group.element === 'edge' ? 'g.E()' : 'g.V()';
  const label = group.label ?? '';
  const words = ELEMENT_WORD[group.element] || ELEMENT_WORD.vertex;
  const what = `${group.count} ${group.count === 1 ? words[0] : words[1]}`;
  const field = group.field ?? '';
  const topLevel = field && !field.startsWith('$') && !field.startsWith('(');
  const where = field ? `${label}${topLevel ? '.' : ' '}${field}` : label;
  const detail = group.detail ? ` (${group.detail})` : '';
  const example = group.example_field ? ` (first at ${group.example_field})` : '';
  const comment = `// ${where}: ${group.kind.replace(/_/g, ' ')}${detail}, ${what}${example}`;
  const byLabel = label ? `${source}.hasLabel(${rhaiString(label)})` : null;
  let body = null;
  if (group.kind === 'unknown_label' && byLabel) body = byLabel;
  else if (group.kind === 'missing_required' && byLabel && topLevel) body = `${byLabel}.not(__.has(${rhaiString(field)}))`;
  else if (group.kind === 'undeclared_property' && byLabel && topLevel) body = `${byLabel}.has(${rhaiString(field)})`;
  let script;
  if (body) {
    script = `${comment}\n${body}`;
  } else {
    const ids = (group.sample_ids || []).filter((id) => !isCutId(id));
    const shown = ids.length < group.count
      ? `\n// the report keeps ${ids.length} sample id${ids.length === 1 ? '' : 's'} of ${group.count}`
      : '';
    const idsText = ids.map(rhaiString).join(', ');
    const method = group.element === 'edge' ? 'g.E' : 'g.V';
    script = ids.length ? `${comment}${shown}\n${method}(${idsText})` : `${comment}\n${byLabel || source}`;
  }
  return { title: `Violations: ${where}`.slice(0, 60), script };
}

/** A placeholder value of a base type for a backfill script. */
function placeholder(base) {
  return {
    string: '', integer: 0, number: 0, boolean: false, array: [], object: {}, uuid: '00000000-0000-0000-0000-000000000000',
    datetime: '1970-01-01T00:00:00Z', localdatetime: '1970-01-01T00:00:00', date: '1970-01-01', time: '00:00:00',
    offsettime: '00:00:00Z', duration: 'PT0S',
  }[base] ?? null;
}

/**
 * A backfill value as a Rhai literal: a date and time value through its constructor
 * (`date("1970-01-01")`), since the draft that declares the key enforces nothing yet (mode
 * `none` stores a string as a string).
 */
function fillLiteral(base, value) {
  if (TEMPORAL_BASES.includes(base) && typeof value === 'string') return `${base}(${rhaiString(value)})`;
  return rhaiLiteral(value);
}

/**
 * A script that backfills the `missing_required` violations of top-level keys and then sets the
 * draft, as ONE playground run (a run is one unit: nothing is kept unless all of it succeeds):
 * 1. the draft with mode `none` (declares the keys, enforces nothing yet),
 * 2. `property(key, value)` on every element of the label that lacks the key; the value is the
 *    draft's `default` (a create stores it, existing data needs this backfill) or a placeholder,
 * 3. the draft itself, which validates all data again.
 * Returns null when no group can be backfilled this way.
 * @returns {{title: string, script: string} | null}
 */
export function backfillScript(draft, groups) {
  const fills = [];
  for (const g of groups || []) {
    if (g.kind !== 'missing_required' || !g.label || !g.field || g.field.startsWith('$')) continue;
    const section = g.element === 'edge' ? 'edges' : 'vertices';
    const node = getAt(draft, [section, g.label, 'schema', 'properties', g.field]);
    const hasDefault = isPlainObject(node) && 'default' in node;
    const base = describe(node).base;
    const value = hasDefault ? node.default : placeholder(base);
    const source = g.element === 'edge' ? 'g.E()' : 'g.V()';
    fills.push(`${source}.hasLabel(${rhaiString(g.label)}).not(__.has(${rhaiString(g.field)})).property(${rhaiString(g.field)}, ${fillLiteral(base, value)}).toList();` +
      (hasDefault ? '  // the draft\'s default' : '  // placeholder: edit it'));
  }
  if (!fills.length) return null;
  const relaxed = { ...draft, mode: 'none' };
  const script = [
    '// Backfill, then apply the schema draft. A playground run is one unit: if any line fails,',
    '// nothing of it is kept.',
    '// 1. The draft with mode "none": the keys are declared, nothing is enforced yet.',
    `g.set_schema(${rhaiString(JSON.stringify(relaxed))});`,
    '// 2. Backfill the missing required keys.',
    ...fills,
    '// 3. The draft itself: all data is validated against it now.',
    `g.set_schema(${rhaiString(JSON.stringify(draft))});`,
    'g.get_schema()',
  ].join('\n');
  return { title: 'Backfill and apply the schema', script };
}

// ------------------------------------------------------------------ the draft

/**
 * The formatted text of a draft's working schema, per draft (outside the draft, so reading the
 * text never writes reactive state): `{version, working, text}`.
 */
const TEXT_CACHE = new WeakMap();

/**
 * The editor's state: the stored schema (`stored`, null without one), the inferred one shown
 * when nothing is stored (`inferred`), and the DRAFT (`draft`, null when not editing). Every edit
 * method starts a draft from the working schema first. `version` counts draft changes, so review
 * results (diff, check) of an older version are recognisably stale. `jsonText`/`jsonError` are
 * the JSON tab: text edits update the draft when they parse, and form edits rewrite the text.
 * The text of the working schema is made on first read only (and kept until it changes): the
 * schema inferred from a graph with one label per vertex is megabytes of JSON, which no reload
 * should format while the JSON tab is not shown.
 *
 * Vue wraps an instance in `reactive()`; the class itself knows nothing of Vue.
 */
export class SchemaDraft {
  constructor() {
    this.stored = null;
    this.inferred = null;
    this.draft = null;
    this.draftOrigin = null; // 'edit' | 'inferred' | 'file' | 'json'
    this.version = 0;
    this.editedText = null; // the JSON tab's own text (an edit), else null: the working schema's
    this.textVersion = 0; // bumped when the working schema's text must be made again
    this.jsonError = null;
  }

  /** The JSON tab's text: the user's edit, else the working schema formatted (on first read). */
  get jsonText() {
    if (this.editedText !== null) return this.editedText;
    const version = this.textVersion;
    const working = this.working;
    const hit = TEXT_CACHE.get(this);
    if (hit && hit.version === version && hit.working === working) return hit.text;
    const text = schemaText(working);
    TEXT_CACHE.set(this, { version, working, text });
    return text;
  }

  /** The schema the views show: the draft, else the stored, else the inferred one. */
  get working() {
    return this.draft ?? this.stored ?? this.inferred ?? {};
  }

  /** 'draft' | 'stored' | 'inferred' | 'none'. */
  get source() {
    if (this.draft) return 'draft';
    if (this.stored) return 'stored';
    if (this.inferred) return 'inferred';
    return 'none';
  }

  /** What the draft changes against: the stored schema in its canonical form, `{}` without one. */
  get base() {
    return this.stored ?? {};
  }

  /** The merge patch from the stored schema to the draft: `{patch, exact}`, null without a draft. */
  get patch() {
    return this.draft ? createMergePatch(this.base, this.draft) : null;
  }

  /** The draft differs from the stored schema. */
  get dirty() {
    return !!this.draft && !deepEqual(this.draft, this.base);
  }

  /** New engine state (after a load, an Apply, a run that changed the schema). The draft stays. */
  setEngineState({ stored, inferred }) {
    this.stored = stored ?? null;
    this.inferred = inferred ?? null;
    if (!this.draft) this.syncText();
  }

  /** Starts a draft (a copy of `schema`, default: the working schema), replacing any draft. */
  start(schema = this.working, origin = 'edit') {
    this.draft = clone(schema ?? {});
    this.draftOrigin = origin;
    this.changed();
  }

  /** Runs `fn(draft)` on the draft (started from the working schema when there is none). */
  edit(fn) {
    if (!this.draft) {
      this.draft = clone(this.working);
      this.draftOrigin = 'edit';
    }
    const result = fn(this.draft);
    this.changed();
    return result;
  }

  discard() {
    this.draft = null;
    this.draftOrigin = null;
    this.version++;
    this.syncText();
  }

  /** The JSON tab's text changed: a valid schema object becomes the draft, else only the error shows. */
  setJsonText(text) {
    this.editedText = text;
    const parsed = parseSchemaText(text);
    if (!parsed.ok) {
      this.jsonError = parsed.error;
      return false;
    }
    this.jsonError = null;
    if (deepEqual(parsed.value, this.working)) return true;
    if (!this.draft) this.draftOrigin = 'json';
    this.draft = parsed.value;
    this.version++;
    return true;
  }

  changed() {
    this.version++;
    this.syncText();
  }

  /** The JSON text follows the working schema again (form edits, discard, new engine state); made on next read. */
  syncText() {
    this.editedText = null;
    this.textVersion++;
    this.jsonError = null;
  }
}
