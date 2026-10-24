// The model of the Graph panel's property editor (components/element-editor.js): the form of
// one element's properties as a tree of typed values, the schema of its label(s) mapped to form
// fields, the paths a compression rule covers, a raw-JSON form of any value, and the diff of the
// form against the stored properties as the engine's write request. Pure functions, no Vue, no
// DOM (tested under Node: tests/element-editor.test.mjs).
//
// Values come from and go to the engine in its TYPED form (crates/graphersal-session `edit.rs`):
// `{type, value}` with `int64` as decimal text, `float64` a number or "NaN"/"Infinity"/
// "-Infinity", `uuid` its canonical text, the date and time types (`datetime`,
// `localdatetime`, `date`, `time`, `offsettime`, `duration`) their ISO 8601 text, `array` a list of typed
// values, `object` a list of `[key, typed]` pairs, `null` without a value. A type is never guessed from a string's content
// (CLAUDE.md rule 7): a string stays a string until the user picks another type.
//
// The engine is the authority on the schema: the dialog checks a draft with a dry run of the
// write (`updateElement({.., validateOnly: true})`) and shows its errors per key. What is
// checked here is only what the engine cannot be asked (a value that does not parse as its
// type, an empty or repeated key, a required key left out).

/** The date and time types (book "Dates and Times"), each written as its ISO 8601 text. */
export const TEMPORAL_TYPES = Object.freeze(['datetime', 'localdatetime', 'date', 'time', 'offsettime', 'duration']);
/** The element kinds of a vector (book "Vectors"); a new vector is f32. */
export const VECTOR_ELEMENTS = Object.freeze(['f32', 'f16', 'i8', 'bit']);
/** The most components of a vector. */
export const MAX_VECTOR_DIMENSION = 65536;
/** The logical types of a value, in the order the type menus list them. */
export const TYPES = Object.freeze(['string', 'int64', 'float64', 'boolean', 'uuid', ...TEMPORAL_TYPES, 'vector', 'array', 'object', 'null']);
/** The scalar types edited as one line of text (a vector as its JSON list of numbers). */
export const TEXT_TYPES = Object.freeze(['string', 'int64', 'float64', 'uuid', ...TEMPORAL_TYPES, 'vector']);
/** The JSON Schema `format` of a string that declares a uuid or a date and time type. */
export const FORMAT_TYPES = Object.freeze({
  uuid: 'uuid', 'date-time': 'datetime', 'local-date-time': 'localdatetime', date: 'date', time: 'time',
  'offset-time': 'offsettime', duration: 'duration',
});

/** An example value of a text type, for a field's placeholder. */
export function placeholderOf(type) {
  return {
    string: 'value', int64: '42', float64: '1.5', uuid: '550e8400-e29b-41d4-a716-446655440000',
    datetime: '2023-08-02T09:30:00+02:00', localdatetime: '2023-08-02T09:30:00', date: '2023-08-02',
    time: '09:30:00', offsettime: '09:30:00+02:00', duration: 'P1DT2H', vector: '[0.1, 0.2, 0.3]',
  }[type] ?? '';
}
/** Containers: edited as a tree (or as raw JSON). */
export const CONTAINERS = Object.freeze(['array', 'object']);
/** A string at least this long, or holding a line break, is edited in a textarea. */
export const LONG_TEXT = 80;
/** Deepest value a raw JSON text may hold (the engine's `traversal.max_value_depth`). */
export const MAX_DEPTH = 128;

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
// The shape of the ISO 8601 texts the engine reads (crates/graphersal value/temporal/parse.rs);
// the engine checks the rest (calendar, ranges, zone names) in its dry run.
const DATE_PART = '[+-]?\\d{4}-\\d{2}-\\d{2}';
const TIME_PART = '\\d{2}:\\d{2}(?::\\d{2}(?:\\.\\d{1,9})?)?';
const OFFSET_PART = '(?:[Zz]|[+-]\\d{2}(?::?\\d{2}(?::\\d{2})?)?)';
const TEMPORAL_RE = {
  date: new RegExp(`^${DATE_PART}$`),
  time: new RegExp(`^${TIME_PART}$`),
  offsettime: new RegExp(`^${TIME_PART}${OFFSET_PART}$`),
  localdatetime: new RegExp(`^${DATE_PART}(?:[Tt ]${TIME_PART})?$`),
  // A stored date-time needs its offset or zone: without one the instant would be a guess.
  datetime: new RegExp(`^${DATE_PART}(?:[Tt ]${TIME_PART})?(?:${OFFSET_PART}(?:\\[[^\\]]+\\])?|\\[[^\\]]+\\])$`),
  duration: /^[+-]?[Pp](?=.)[-+0-9.,WwYyMmDdTtHhSs]*$/,
};
const TEMPORAL_HELP = {
  date: 'a date is YYYY-MM-DD, e.g. 2023-08-02',
  time: 'a time is HH:MM[:SS[.f]] (up to nine fraction digits), e.g. 09:30:00',
  offsettime: 'an offsettime is HH:MM[:SS[.f]] with its UTC offset, e.g. 09:30:00+02:00',
  localdatetime: 'a localdatetime is YYYY-MM-DDTHH:MM[:SS[.f]] without an offset, e.g. 2023-08-02T09:30:00',
  datetime: 'a datetime needs its UTC offset or zone, e.g. 2023-08-02T09:30:00+02:00 or 2023-08-02T09:30[Europe/Berlin]',
  duration: 'a duration is ISO 8601, e.g. P1Y2M3DT4H5M6S or PT90M',
};
const INT_RE = /^-?\d+$/;
const I64_MIN = -(2n ** 63n);
const I64_MAX = 2n ** 63n - 1n;

let nextId = 1;
/** A fresh id for a node or a row (Vue keys). */
const newId = () => nextId++;

// ------------------------------------------------------------------ nodes

/**
 * A form node: `{id, type, text, bool, items, entries, expanded}`; `text` holds a scalar's input
 * (string, int64, float64, uuid), `bool` a boolean's, `items` an array's nodes, `entries` an
 * object's `[{id, key, node}]`.
 */
export function emptyNode(type = 'string') {
  return { id: newId(), type, text: '', bool: false, items: [], entries: [], expanded: true };
}

/** The input text of a float64 (`1` stays recognizable as a float: `1.0`). */
function floatText(value) {
  if (typeof value === 'string') return value;
  const text = String(value);
  return /[.eE]/.test(text) || !Number.isFinite(value) ? text : `${text}.0`;
}

/** A form node from a typed value. */
export function nodeFromTyped(typed) {
  const type = typed && TYPES.includes(typed.type) ? typed.type : 'null';
  const node = emptyNode(type);
  const v = typed ? typed.value : null;
  if (type === 'boolean') node.bool = v === true;
  else if (type === 'float64') node.text = floatText(v);
  else if (type === 'string' || type === 'int64' || type === 'uuid' || TEMPORAL_TYPES.includes(type)) node.text = v == null ? '' : String(v);
  else if (type === 'vector') {
    // The values as a JSON list; the element kind is kept (a new vector is f32).
    node.text = Array.isArray(v) ? `[${v.map((x) => floatText(x)).join(', ')}]` : '';
    node.element = VECTOR_ELEMENTS.includes(typed.element) ? typed.element : 'f32';
  }
  else if (type === 'array') node.items = (Array.isArray(v) ? v : []).map(nodeFromTyped);
  else if (type === 'object') {
    node.entries = (Array.isArray(v) ? v : []).map(([key, item]) => ({ id: newId(), key, node: nodeFromTyped(item) }));
  }
  // Large containers start collapsed.
  node.expanded = !(CONTAINERS.includes(type) && (node.items.length + node.entries.length) > 20);
  return node;
}

/** Why a scalar's text is not a value of its type, or null. */
export function scalarError(type, text) {
  const t = String(text ?? '').trim();
  if (type === 'int64') {
    if (!INT_RE.test(t)) return 'an int64 is a whole number, e.g. 42';
    const n = BigInt(t);
    if (n < I64_MIN || n > I64_MAX) return 'out of the int64 range';
    return null;
  }
  if (type === 'float64') {
    if (t === 'NaN' || t === 'Infinity' || t === '-Infinity') return null;
    if (!t || !Number.isFinite(Number(t))) return 'a float64 is a number, e.g. 1.5 (or NaN, Infinity, -Infinity)';
    return null;
  }
  if (type === 'uuid') return UUID_RE.test(t) ? null : 'a uuid is lowercase 8-4-4-4-12 hex digits, e.g. 550e8400-e29b-41d4-a716-446655440000';
  if (TEMPORAL_TYPES.includes(type)) return TEMPORAL_RE[type].test(t) ? null : TEMPORAL_HELP[type];
  if (type === 'vector') return vectorValues(t).error ?? null;
  return null;
}

/**
 * The numbers of a vector's JSON list text: `{values}` or `{error}` (a non-empty list of finite
 * numbers, at most 65536). The engine checks the element range (f16) in its dry run.
 */
export function vectorValues(text) {
  const help = 'a vector is a JSON list of numbers, e.g. [0.1, 0.2, 0.3]';
  let parsed;
  try {
    parsed = JSON.parse(String(text ?? '').trim());
  } catch {
    return { error: help };
  }
  if (!Array.isArray(parsed) || parsed.length === 0) return { error: help };
  if (parsed.length > MAX_VECTOR_DIMENSION) return { error: `a vector has at most ${MAX_VECTOR_DIMENSION} components` };
  if (!parsed.every((x) => typeof x === 'number' && Number.isFinite(x))) return { error: help };
  return { values: parsed };
}

/**
 * The typed value of a node: `{typed}` or `{error, path}` (`path` the keys and indices from the
 * node to the first value that does not parse).
 */
export function typedFromNode(node, path = []) {
  switch (node.type) {
    case 'null': return { typed: { type: 'null' } };
    case 'boolean': return { typed: { type: 'boolean', value: !!node.bool } };
    case 'string': return { typed: { type: 'string', value: String(node.text ?? '') } };
    case 'int64':
    case 'float64':
    case 'uuid':
    case 'datetime':
    case 'localdatetime':
    case 'date':
    case 'time':
    case 'offsettime':
    case 'duration': {
      const error = scalarError(node.type, node.text);
      if (error) return { error, path };
      const t = String(node.text).trim();
      if (node.type === 'float64') {
        const special = t === 'NaN' || t === 'Infinity' || t === '-Infinity';
        return { typed: { type: 'float64', value: special ? t : Number(t) } };
      }
      return { typed: { type: node.type, value: t } };
    }
    case 'vector': {
      const r = vectorValues(node.text);
      if (r.error) return { error: r.error, path };
      const element = VECTOR_ELEMENTS.includes(node.element) ? node.element : 'f32';
      return { typed: { type: 'vector', element, value: r.values } };
    }
    case 'array': {
      const value = [];
      for (let i = 0; i < node.items.length; i++) {
        const r = typedFromNode(node.items[i], [...path, i]);
        if (r.error) return r;
        value.push(r.typed);
      }
      return { typed: { type: 'array', value } };
    }
    case 'object': {
      const value = [];
      const seen = new Set();
      for (const e of node.entries) {
        if (!e.key) return { error: 'a key cannot be empty', path };
        if (seen.has(e.key)) return { error: `the key "${e.key}" appears twice`, path: [...path, e.key] };
        seen.add(e.key);
        const r = typedFromNode(e.node, [...path, e.key]);
        if (r.error) return r;
        value.push([e.key, r.typed]);
      }
      return { typed: { type: 'object', value } };
    }
    default: return { error: `unknown type ${node.type}`, path };
  }
}

/**
 * Changes a node's type in place. Scalar text is kept between the text types (the user chose
 * the new type; nothing is converted by guessing), a container starts empty, a boolean false.
 */
export function setType(node, type) {
  if (!TYPES.includes(type) || node.type === type) return node;
  const wasText = TEXT_TYPES.includes(node.type);
  if (!wasText) node.text = '';
  if (type !== 'array') node.items = [];
  if (type !== 'object') node.entries = [];
  node.type = type;
  node.expanded = true;
  return node;
}

/** Appends an item (of `type`) to an array node; returns it. */
export function addItem(node, type = 'string') {
  const item = emptyNode(type);
  node.items.push(item);
  node.expanded = true;
  return item;
}

/** Appends an entry `key` (of `type`) to an object node; returns it. */
export function addEntry(node, key = '', type = 'string') {
  const entry = { id: newId(), key, node: emptyNode(type) };
  node.entries.push(entry);
  node.expanded = true;
  return entry;
}

/** A one-line summary of a container: `[3 items]`, `{2 keys}`. */
export function summary(node) {
  if (node.type === 'array') return `[${node.items.length} item${node.items.length === 1 ? '' : 's'}]`;
  if (node.type === 'object') return `{${node.entries.length} key${node.entries.length === 1 ? '' : 's'}}`;
  return '';
}

/** Whether a string is edited in a textarea: long, multi-line, or covered by a compression rule. */
export function wantsTextarea(node, compressed = false) {
  if (node.type !== 'string') return false;
  return compressed || node.text.length >= LONG_TEXT || node.text.includes('\n');
}

// ------------------------------------------------------------------ raw JSON

/** A node as JSON text (int64 exact, a float keeps its point, a uuid or a date as its string). */
export function nodeToJson(node, indent = 2) {
  const out = [];
  const pad = (d) => (indent ? `\n${' '.repeat(indent * d)}` : '');
  const write = (n, d) => {
    switch (n.type) {
      case 'null': out.push('null'); break;
      case 'boolean': out.push(n.bool ? 'true' : 'false'); break;
      case 'int64': out.push(INT_RE.test(String(n.text).trim()) ? String(n.text).trim() : JSON.stringify(n.text)); break;
      case 'float64': {
        const t = String(n.text).trim();
        out.push(t && Number.isFinite(Number(t)) ? floatText(Number(t)) : JSON.stringify(t));
        break;
      }
      case 'vector': {
        // Its list of numbers (read back as a vector where one was: `nodeFromJson`).
        const r = vectorValues(n.text);
        out.push(r.error ? JSON.stringify(String(n.text ?? '')) : `[${r.values.map((x) => floatText(x)).join(', ')}]`);
        break;
      }
      case 'array':
        if (!n.items.length) { out.push('[]'); break; }
        out.push('[');
        n.items.forEach((item, i) => { out.push(pad(d + 1)); write(item, d + 1); if (i < n.items.length - 1) out.push(','); });
        out.push(pad(d), ']');
        break;
      case 'object':
        if (!n.entries.length) { out.push('{}'); break; }
        out.push('{');
        n.entries.forEach((e, i) => {
          out.push(pad(d + 1), JSON.stringify(e.key), indent ? ': ' : ':');
          write(e.node, d + 1);
          if (i < n.entries.length - 1) out.push(',');
        });
        out.push(pad(d), '}');
        break;
      default: out.push(JSON.stringify(String(n.text ?? '')));
    }
  };
  write(node, 0);
  return out.join('');
}

/**
 * Parses JSON text into a syntax tree that keeps each number's literal (`{kind: 'number', text}`),
 * so `1` and `1.0` stay apart and large integers stay exact. Throws a SyntaxError with the offset.
 */
export function parseJson(text) {
  let i = 0;
  const s = String(text);
  const fail = (what) => { throw new SyntaxError(`${what} at position ${i}`); };
  const ws = () => { while (i < s.length && ' \t\n\r'.includes(s[i])) i++; };
  const str = () => {
    const start = i;
    i++;
    while (i < s.length && s[i] !== '"') i += s[i] === '\\' ? 2 : 1;
    if (i >= s.length) fail('unterminated string');
    i++;
    try {
      return JSON.parse(s.slice(start, i));
    } catch (err) {
      i = start;
      return fail('invalid string');
    }
  };
  const value = (depth) => {
    if (depth > MAX_DEPTH) fail(`nested deeper than ${MAX_DEPTH} levels`);
    ws();
    const c = s[i];
    if (c === '{') {
      i++;
      const entries = [];
      ws();
      if (s[i] === '}') { i++; return { kind: 'object', entries }; }
      for (;;) {
        ws();
        if (s[i] !== '"') fail('expected a key');
        const key = str();
        ws();
        if (s[i] !== ':') fail('expected ":"');
        i++;
        entries.push([key, value(depth + 1)]);
        ws();
        if (s[i] === ',') { i++; continue; }
        if (s[i] === '}') { i++; return { kind: 'object', entries }; }
        fail('expected "," or "}"');
      }
    }
    if (c === '[') {
      i++;
      const items = [];
      ws();
      if (s[i] === ']') { i++; return { kind: 'array', items }; }
      for (;;) {
        items.push(value(depth + 1));
        ws();
        if (s[i] === ',') { i++; continue; }
        if (s[i] === ']') { i++; return { kind: 'array', items }; }
        fail('expected "," or "]"');
      }
    }
    if (c === '"') return { kind: 'string', value: str() };
    const m = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(s.slice(i));
    if (m) { i += m[0].length; return { kind: 'number', text: m[0] }; }
    for (const [word, v] of [['true', true], ['false', false], ['null', null]]) {
      if (s.startsWith(word, i)) { i += word.length; return v === null ? { kind: 'null' } : { kind: 'boolean', value: v }; }
    }
    return fail('unexpected character');
  };
  const out = value(0);
  ws();
  if (i < s.length) fail('unexpected text after the value');
  return out;
}

/**
 * A node from parsed JSON (`parseJson`). `hint` is the node that was at the same place before (a
 * raw edit of an existing value): its type is kept where the JSON value fits it (a uuid or a date
 * stays one, a float64 stays a float64 even when written `2`, NaN/Infinity written as strings stay
 * float64). Otherwise a number is an int64 when written without a fraction or exponent, else a
 * float64; a string is a string.
 */
export function nodeFromJson(ast, hint = null) {
  const h = hint && hint.type;
  switch (ast.kind) {
    case 'null': return emptyNode('null');
    case 'boolean': { const n = emptyNode('boolean'); n.bool = ast.value; return n; }
    case 'string': {
      const special = ['NaN', 'Infinity', '-Infinity'].includes(ast.value);
      const keep = h === 'uuid' || h === 'int64' || TEMPORAL_TYPES.includes(h) || (h === 'float64' && special);
      const n = emptyNode(keep ? h : 'string');
      n.text = ast.value;
      return n;
    }
    case 'number': {
      const integral = INT_RE.test(ast.text);
      const n = emptyNode(h === 'float64' || !integral ? 'float64' : 'int64');
      n.text = ast.text;
      return n;
    }
    case 'array': {
      // A list of numbers where a vector was stays a vector (its element kind kept).
      if (h === 'vector' && ast.items.length && ast.items.every((item) => item.kind === 'number')) {
        const n = emptyNode('vector');
        n.text = `[${ast.items.map((item) => item.text).join(', ')}]`;
        n.element = hint.element;
        return n;
      }
      const n = emptyNode('array');
      const old = h === 'array' ? hint.items : [];
      n.items = ast.items.map((item, i) => nodeFromJson(item, old[i] || null));
      return n;
    }
    case 'object': {
      const n = emptyNode('object');
      const old = h === 'object' ? new Map(hint.entries.map((e) => [e.key, e.node])) : new Map();
      n.entries = ast.entries.map(([key, item]) => ({ id: newId(), key, node: nodeFromJson(item, old.get(key) || null) }));
      return n;
    }
    default: return emptyNode('null');
  }
}

/** `{node}` from raw JSON text (types kept from `hint`, see `nodeFromJson`), or `{error}`. */
export function nodeFromJsonText(text, hint = null) {
  try {
    return { node: nodeFromJson(parseJson(text), hint) };
  } catch (err) {
    return { error: `Not valid JSON: ${err.message}` };
  }
}

// ------------------------------------------------------------------ schema

const SCHEMA_TYPES = { integer: 'int64', number: 'float64', string: 'string', boolean: 'boolean', array: 'array', object: 'object', null: 'null' };

/** The logical types a schema node admits (all of them when it names none). */
export function typesOf(schema) {
  if (!schema || typeof schema !== 'object') return TYPES.slice();
  if (Array.isArray(schema.anyOf)) {
    const out = [];
    for (const s of schema.anyOf) for (const t of typesOf(s)) if (!out.includes(t)) out.push(t);
    return out;
  }
  const raw = schema.type === undefined ? null : [].concat(schema.type);
  if (!raw) {
    if (schema.const !== undefined) return [typeOfLiteral(schema.const)];
    if (Array.isArray(schema.enum)) return [...new Set(schema.enum.map(typeOfLiteral))];
    return TYPES.slice();
  }
  return raw.map((t) => {
    if (t === 'string' && FORMAT_TYPES[schema.format]) return FORMAT_TYPES[schema.format];
    // `{"type": "array", "format": "vector", ..}` declares a vector (a list is coerced to it).
    if (t === 'array' && schema.format === 'vector') return 'vector';
    return SCHEMA_TYPES[t];
  }).filter(Boolean);
}

function typeOfLiteral(v) {
  if (v === null) return 'null';
  if (typeof v === 'boolean') return 'boolean';
  if (typeof v === 'number') return Number.isInteger(v) ? 'int64' : 'float64';
  if (Array.isArray(v)) return 'array';
  if (typeof v === 'object') return 'object';
  return 'string';
}

/** The type a new value of a schema node starts as: its first non-null type, else string. */
export function defaultType(schema) {
  const ts = typesOf(schema);
  if (ts.length === TYPES.length) return 'string';
  return ts.find((t) => t !== 'null') || ts[0] || 'string';
}

/**
 * The fields the schema declares for an element: `{mode, declared, fields: Map(key -> field),
 * additional}` where a field is `{key, schema, required, label}` (the first label in label order
 * that declares a key gives its schema, as the engine's coercion does; `required` is the union),
 * `declared` says whether any of the labels has a schema, and `additional` whether keys the
 * schema does not declare are admitted (any label admitting them is enough).
 * @param {object|null} schema the stored schema (`getSchema()`)
 * @param {'vertex'|'edge'} kind
 * @param {string[]} labels the element's labels (an edge has one)
 */
export function schemaFields(schema, kind, labels) {
  const mode = (schema && schema.mode) || 'none';
  const section = schema ? (kind === 'edge' ? schema.edges : schema.vertices) || {} : {};
  const fields = new Map();
  let declared = false;
  let additional = false;
  for (const label of labels || []) {
    const entry = section[label];
    const node = entry && entry.schema;
    if (!node) continue;
    declared = true;
    const admits = node.additionalProperties !== undefined ? node.additionalProperties !== false : mode !== 'closed';
    if (admits) additional = true;
    const required = Array.isArray(node.required) ? node.required : [];
    for (const [key, sub] of Object.entries(node.properties || {})) {
      if (!fields.has(key)) fields.set(key, { key, schema: sub, required: false, label });
    }
    for (const key of required) {
      const f = fields.get(key);
      if (f) f.required = true;
      else fields.set(key, { key, schema: null, required: true, label });
    }
  }
  if (!declared) additional = mode !== 'closed';
  return { mode, declared, fields, additional };
}

/** The schema of a child of a container's schema: an object's property, an array's items. */
export function childSchema(schema, key) {
  if (!schema || typeof schema !== 'object') return null;
  if (typeof key === 'number') return schema.items || null;
  return (schema.properties && schema.properties[key]) || null;
}

/** A short text of a schema node's constraints: `int64 · 0 … 150 · one of 3`. */
export function schemaHint(schema) {
  if (!schema) return '';
  const parts = [];
  const ts = typesOf(schema);
  if (ts.length < TYPES.length) parts.push(ts.join(' | '));
  const lo = schema.minimum ?? schema.exclusiveMinimum;
  const hi = schema.maximum ?? schema.exclusiveMaximum;
  if (lo !== undefined || hi !== undefined) {
    parts.push(`${lo !== undefined ? `${schema.exclusiveMinimum !== undefined ? '>' : '≥'} ${lo}` : ''}${lo !== undefined && hi !== undefined ? ', ' : ''}${hi !== undefined ? `${schema.exclusiveMaximum !== undefined ? '<' : '≤'} ${hi}` : ''}`);
  }
  if (schema.minLength !== undefined || schema.maxLength !== undefined) parts.push(`length ${schema.minLength ?? 0} … ${schema.maxLength ?? '∞'}`);
  if (schema.minItems !== undefined || schema.maxItems !== undefined) parts.push(`${schema.minItems ?? 0} … ${schema.maxItems ?? '∞'} items`);
  const flo = schema.formatMinimum ?? schema.formatExclusiveMinimum;
  const fhi = schema.formatMaximum ?? schema.formatExclusiveMaximum;
  if (flo !== undefined || fhi !== undefined) {
    parts.push([
      flo !== undefined ? `${schema.formatExclusiveMinimum !== undefined ? '>' : '≥'} ${flo}` : '',
      fhi !== undefined ? `${schema.formatExclusiveMaximum !== undefined ? '<' : '≤'} ${fhi}` : '',
    ].filter(Boolean).join(', '));
  }
  if (schema.pattern) parts.push(`pattern ${schema.pattern}`);
  if (schema.const !== undefined) parts.push(`always ${JSON.stringify(schema.const)}`);
  if (schema.deprecated) parts.push('deprecated');
  return parts.join(' · ');
}

/** The description (or title) of a schema node, for the field's hint. */
export function schemaDescription(schema) {
  if (!schema) return '';
  return schema.description || schema.title || '';
}

/**
 * The options of an `enum` (or `const`) as `[{label, typed}]`, typed by the declared type (an
 * integer of a `number` field is a float64; a string of a uuid or date field a uuid or a date);
 * null without one.
 */
export function enumOptions(schema) {
  if (!schema) return null;
  const values = Array.isArray(schema.enum) ? schema.enum : schema.const !== undefined ? [schema.const] : null;
  if (!values) return null;
  const ts = typesOf(schema);
  const options = [];
  for (const v of values) {
    let typed;
    if (v === null) typed = { type: 'null' };
    else if (typeof v === 'boolean') typed = { type: 'boolean', value: v };
    else if (typeof v === 'number') {
      typed = Number.isInteger(v) && ts.includes('int64') ? { type: 'int64', value: String(v) } : { type: 'float64', value: v };
    } else if (typeof v === 'string') {
      const special = ts.find((t) => t === 'uuid' || TEMPORAL_TYPES.includes(t));
      typed = { type: special && !ts.includes('string') ? special : 'string', value: v };
    }
    else continue; // a container in an enum: edited as a value, not offered in the menu
    options.push({ label: v === null ? 'null' : typeof v === 'string' ? v : JSON.stringify(v), typed });
  }
  return options;
}

/** Whether a node holds the typed value of an option (the enum menu's selection). */
export function sameScalar(node, typed) {
  const r = typedFromNode(node);
  return !!r.typed && JSON.stringify(r.typed) === JSON.stringify(typed);
}

/** Sets a node to an option's typed value. */
export function setScalar(node, typed) {
  const fresh = nodeFromTyped(typed);
  node.type = fresh.type;
  node.text = fresh.text;
  node.bool = fresh.bool;
  node.items = [];
  node.entries = [];
}

// ------------------------------------------------------------------ compression rules

/** The names of a canonical rule path (`$.a.b`, `$['x y'].c`); null for a path with an index. */
export function pathNames(text) {
  const s = String(text || '').trim();
  let i = s.startsWith('$') ? 1 : 0;
  const names = [];
  while (i < s.length) {
    if (s[i] === '.') {
      const m = /^[^.[\]]+/.exec(s.slice(i + 1));
      if (!m) return null;
      names.push(m[0]);
      i += 1 + m[0].length;
    } else if (s[i] === '[') {
      const q = s[i + 1];
      if (q !== "'" && q !== '"') return null; // an index: rules name keys only
      let j = i + 2;
      let name = '';
      while (j < s.length && s[j] !== q) {
        if (s[j] === '\\' && j + 1 < s.length) {
          const e = s[j + 1];
          name += { n: '\n', t: '\t', r: '\r', b: '\b', f: '\f' }[e] ?? e;
          j += 2;
        } else name += s[j++];
      }
      if (s[j + 1] !== ']') return null;
      names.push(name);
      i = j + 2;
    } else if (names.length === 0 && i === 0) {
      // A bare first name (`name`, `a.b`).
      const m = /^[^.[\]]+/.exec(s);
      names.push(m[0]);
      i = m[0].length;
    } else return null;
  }
  return names.length ? names : null;
}

/**
 * The key paths a compression rule covers for this element: rules of its kind and one of its
 * labels (`listCompressions()` entries), as arrays of names.
 */
export function compressionPaths(rules, kind, labels) {
  const out = [];
  for (const r of rules || []) {
    if ((r.element || 'vertex') !== kind || !(labels || []).includes(r.label)) continue;
    const names = pathNames(r.path);
    if (names) out.push({ names, rule: r.name });
  }
  return out;
}

/** The rule covering the key path `path` (names; an index never matches), or null. */
export function compressedAt(paths, path) {
  if (path.some((p) => typeof p === 'number')) return null;
  const hit = paths.find((p) => p.names.length === path.length && p.names.every((n, i) => n === path[i]));
  return hit ? hit.rule : null;
}

// ------------------------------------------------------------------ the form

/**
 * The form of an element (`elementForEdit()`): `{rows, original}`. A row is `{id, key,
 * originalKey, node, removed, declared}`: the element's properties in its order, then the
 * properties the schema declares that it has not (`declared: true`, not set, `node` null until
 * the user sets one). `original` maps each stored key to its typed value.
 */
export function formFromElement(element, fields = null) {
  const original = new Map();
  const rows = [];
  for (const [key, typed] of element.properties || []) {
    original.set(key, typed);
    rows.push({ id: newId(), key, originalKey: key, node: nodeFromTyped(typed), removed: false, declared: false });
  }
  if (fields) {
    for (const f of fields.fields.values()) {
      if (original.has(f.key)) continue;
      rows.push({ id: newId(), key: f.key, originalKey: null, node: null, removed: false, declared: true });
    }
  }
  return { rows, original };
}

/**
 * A new row (Add property). `init` is the value typed in the add row: `{text}` for the text
 * types, `{bool}` for a boolean; containers and null start empty.
 */
export function newRow(key = '', type = 'string', init = null) {
  return { id: newId(), key, originalKey: null, node: nodeWithValue(type, init), removed: false, declared: false };
}

/** An empty node of `type` holding the add row's value (`{text}` or `{bool}`, see `newRow`). */
export function nodeWithValue(type = 'string', init = null) {
  const node = emptyNode(TYPES.includes(type) ? type : 'string');
  if (!init) return node;
  if (node.type === 'boolean') node.bool = init.bool === true;
  else if (!CONTAINERS.includes(node.type) && node.type !== 'null') node.text = String(init.text ?? '');
  return node;
}

/**
 * Whether a row differs from what is stored (the dialog marks it): new, renamed, removed, or a
 * value the write sets (`diff` = `diffForm`'s answer).
 */
export function rowChanged(row, diff) {
  if (row.removed) return true;
  if (!row.node) return false;
  if (row.originalKey === null || row.key !== row.originalKey) return true;
  return !!(diff && diff.set.some(([k]) => k === row.key));
}

/**
 * The write request of a form and its local errors: `{set: [[key, typed]], remove: [key],
 * errors: Map(rowId -> message), changed}`. A stored key whose row was removed or renamed is
 * removed; a row whose value differs from the stored one (or is new) is set; unchanged rows
 * are not written. Errors: an empty or repeated key, a value that does not parse (with the
 * nested path), a key the schema requires that is not set (only where the schema is enforced).
 */
export function diffForm(form, fields = null) {
  const set = [];
  const remove = [];
  const errors = new Map();
  const kept = new Map(); // key -> rowId of the live rows
  for (const row of form.rows) {
    if (row.removed || !row.node) continue;
    const key = row.key;
    if (!key) {
      errors.set(row.id, 'a property needs a name');
      continue;
    }
    if (kept.has(key)) {
      errors.set(row.id, `"${key}" is set twice`);
      continue;
    }
    kept.set(key, row.id);
  }
  for (const key of form.original.keys()) {
    if (!kept.has(key)) remove.push(key);
  }
  for (const row of form.rows) {
    if (row.removed || !row.node || errors.has(row.id)) continue;
    const r = typedFromNode(row.node);
    if (r.error) {
      const where = r.path.length ? ` (at ${pathText(r.path)})` : '';
      errors.set(row.id, `${r.error}${where}`);
      continue;
    }
    const before = form.original.get(row.key);
    if (!before || JSON.stringify(before) !== JSON.stringify(r.typed)) set.push([row.key, r.typed]);
  }
  if (fields && fields.mode !== 'none') {
    for (const f of fields.fields.values()) {
      if (!f.required || kept.has(f.key)) continue;
      const row = form.rows.find((r) => r.key === f.key);
      if (row) errors.set(row.id, `"${f.key}" is required by the schema (label ${f.label})`);
    }
  }
  return { set, remove, errors, changed: set.length > 0 || remove.length > 0 };
}

/** `a.b[2]` of a path of keys and indices. */
export function pathText(path) {
  return path.map((p, i) => (typeof p === 'number' ? `[${p}]` : i ? `.${p}` : p)).join('');
}

/**
 * The engine's errors (`updateElement` answer) by row: `{byRow: Map(rowId -> [message]), general:
 * [message]}`; an error of a key no row has (a removed key the schema requires) is general.
 */
export function engineErrors(form, errors) {
  const byRow = new Map();
  const general = [];
  for (const e of errors || []) {
    const help = e.help ? String(e.help).split('\n')[0] : '';
    const text = help ? `${e.message} ${help}` : e.message;
    const row = e.field !== 'general' && (form.rows.find((r) => r.key === e.field && !r.removed && r.node)
      || form.rows.find((r) => r.originalKey === e.field));
    if (row) byRow.set(row.id, [...(byRow.get(row.id) || []), text]);
    else general.push(e.field !== 'general' ? `${e.field}: ${text}` : text);
  }
  return { byRow, general };
}

/**
 * The form of a NEW element (the edit mode's creation dialog): the properties the schema declares
 * for its label in the schema's order, the required ones already set to an empty value of their
 * declared type, the optional ones offered unset; rows of `previous` (the same dialog before the
 * label changed) that hold a value keep it. Nothing is stored yet, so every live row is written.
 * @param {ReturnType<typeof schemaFields>|null} fields
 * @param {{rows:Array, original:Map}|null} previous
 */
export function formForNew(fields = null, previous = null) {
  const keep = previous ? previous.rows.filter((r) => r.node && !r.removed) : [];
  const used = new Set();
  const rows = [];
  if (fields && fields.declared) {
    for (const f of fields.fields.values()) {
      const old = keep.find((r) => r.key === f.key && !used.has(r));
      if (old) {
        used.add(old);
        rows.push({ ...old, declared: true });
        continue;
      }
      rows.push({
        id: newId(), key: f.key, originalKey: null, removed: false, declared: true,
        node: f.required ? emptyNode(defaultType(f.schema)) : null,
      });
    }
  }
  for (const r of keep) if (!used.has(r)) rows.push({ ...r, declared: false });
  return { rows, original: new Map() };
}

/** The fields of a creation's engine errors that are not properties. */
export const HEAD_FIELDS = Object.freeze(['label', 'id', 'from', 'to']);

/**
 * Splits engine errors (`createElement` answer) into those of the dialog's head fields (`label`,
 * `id`, `from`, `to`: `{field: [message]}`) and the rest (property keys and `general`).
 */
export function splitHeadErrors(errors) {
  const head = {};
  const rest = [];
  for (const e of errors || []) {
    if (HEAD_FIELDS.includes(e.field)) {
      const help = e.help ? String(e.help).split('\n')[0] : '';
      (head[e.field] = head[e.field] || []).push(help ? `${e.message} ${help}` : e.message);
    } else rest.push(e);
  }
  return { head, rest };
}

/** The whole form as one object node (the element's raw JSON mode). */
export function formAsNode(form) {
  const node = emptyNode('object');
  node.entries = form.rows.filter((r) => !r.removed && r.node).map((r) => ({ id: newId(), key: r.key, node: r.node }));
  return node;
}

/**
 * Replaces the rows of a form by the entries of an object node (applying the element's raw
 * JSON): rows of keys that stay keep their identity (and original key), keys that go are
 * removed, new keys are added; declared but unset rows stay.
 */
export function applyObjectToForm(form, node) {
  const byKey = new Map(form.rows.filter((r) => !r.removed && r.node).map((r) => [r.key, r]));
  const rows = [];
  for (const e of node.entries) {
    const row = byKey.get(e.key);
    if (row) {
      row.node = e.node;
      rows.push(row);
      byKey.delete(e.key);
    } else {
      const declared = form.rows.find((r) => r.declared && !r.node && r.key === e.key);
      rows.push(declared ? { ...declared, node: e.node } : { ...newRow(e.key), node: e.node });
    }
  }
  for (const r of form.rows) {
    if (rows.includes(r)) continue;
    if (r.removed) rows.push(r);
    else if (!r.node) {
      if (!rows.some((x) => x.key === r.key)) rows.push(r);
    } else if (r.originalKey !== null) rows.push({ ...r, removed: true }); // a stored key that is gone
  }
  form.rows = rows;
  return form;
}
