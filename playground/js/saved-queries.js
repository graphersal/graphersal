// Saved queries in the playground: the pure models behind the Catalog manager, the editor dialog and
// the run form (no Vue, no DOM, tested under Node: playground/tests/saved-queries.test.mjs).
//
// The engine's forms (crates/graphersal-session/src/queries.rs):
// - an ENTRY (listQueries/getQuery): {name, folder, description, params: [{name, schema,
//   default?, required, description?}], status: 'ok'|'error', error?, writes, effects: [effect],
//   denied_writes: [effect], language: 'rhai'|'gql', dialect} with an effect {action, element,
//   label, text}; getQuery adds `body` (the stored text) and `meta`;
// - a SPEC (defineQuery): {name, folder, description, params: [{name, schema, default?}],
//   language, body, replaces?}: a Rhai `body` is the text of ONE Rhai function named like the
//   query whose parameters are the declared ones, in that order; a GQL `body` is one plain GQL
//   query whose `$name`s are exactly the declared parameters; `replaces` names the query an
//   edit started from;
// - FIELD ERRORS (a refused defineQuery): [{field, message, help?}] with `field` one of name,
//   folder, description, meta, language, body, params, params.<parameter>, general.
//
// The editor shows a Rhai function's BODY only (what is inside its braces): the signature is
// written from the parameter list, so adding, removing, renaming or reordering a parameter
// never needs a second edit. A stored source this model cannot split (comments before `fn`, an
// unusual layout) is edited as the whole function text (`raw`). A GQL query is edited as it is
// stored. "Save as query" (`draftFromScript`) starts a draft from a query tab: a tab holding
// one `g.gql(`...`[, #{..}])` call becomes a GQL query with the `$name`s it reads as parameters.

import { gqlParamNames } from './editor/gql-tokens.js';
import { wrapGql } from './editor/gql-wrap.js';

/** The languages of a saved query's body. */
export const QUERY_LANGUAGES = Object.freeze(['rhai', 'gql']);

/** The body a new GQL draft starts with. */
export const GQL_DRAFT_BODY = 'MATCH (p:person)\nRETURN p.name AS name\nORDER BY name';

/**
 * The string `format`s the editor offers as types of their own: a uuid and the date and time
 * types (book "Dates and Times"); an argument is written as its text, the format reads it back.
 */
export const FORMAT_OF = Object.freeze({
  uuid: 'uuid', datetime: 'date-time', localdatetime: 'local-date-time', date: 'date', time: 'time',
  offsettime: 'offset-time', duration: 'duration',
});
const TYPE_OF_FORMAT = Object.fromEntries(Object.entries(FORMAT_OF).map(([t, f]) => [f, t]));
/** The parameter types written as text: plain strings and the formats. */
const TEXT_PARAM_TYPES = ['string', ...Object.keys(FORMAT_OF)];

/** The parameter types the editor offers; 'custom' is a schema edited as JSON. */
export const PARAM_TYPES = ['string', 'integer', 'number', 'boolean', ...Object.keys(FORMAT_OF), 'array', 'object', 'custom'];

/** Item types of an 'array' parameter. */
export const ITEM_TYPES = ['string', 'integer', 'number', 'boolean', ...Object.keys(FORMAT_OF)];

/** An example value of a format type (a field's placeholder), '' for the others. */
export function formatExample(type) {
  return {
    uuid: '01234567-89ab-cdef-0123-456789abcdef', datetime: '2023-08-02T09:30:00+02:00',
    localdatetime: '2023-08-02T09:30:00', date: '2023-08-02', time: '09:30:00', offsettime: '09:30:00+02:00',
    duration: 'P1DT2H',
  }[type] || '';
}

/** Schema keywords the editor's fields cover (anything else makes a parameter 'custom'). */
const FIELD_KEYWORDS = new Set([
  'type', 'format', 'items', 'minimum', 'maximum', 'minLength', 'maxLength', 'pattern', 'enum', 'description',
]);

const NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
const UUID_RE = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

// ------------------------------------------------------------------ the tree

/**
 * The saved queries grouped by folder, for the Catalog manager and the completion: `[{folder,
 * queries}]`, the root (`''`) first, then the folders by path; queries by name in each.
 * @param {Array<{name:string, folder:?string}>} list
 */
export function queryTree(list) {
  const groups = new Map();
  for (const entry of list || []) {
    const folder = entry.folder || '';
    if (!groups.has(folder)) groups.set(folder, []);
    groups.get(folder).push(entry);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => (a === '' ? -1 : b === '' ? 1 : a.localeCompare(b)))
    .map(([folder, queries]) => ({
      folder,
      depth: folder ? folder.split('/').length : 0,
      queries: [...queries].sort((a, b) => a.name.localeCompare(b.name)),
    }));
}

/**
 * The access badge of a saved query in the Catalog manager and the run form: `{text, kind,
 * title}` (`kind` is a badge class).
 * - `unavailable`: it cannot run (its body does not compile, its dialect is unknown, ...);
 * - `R/W`: a call may write; the title lists the inferred effects;
 * - `R/O ⚠`: read-only, but its body has writing steps, which a call denies (saved before
 *   queries could write, or stored with the read-only dialect): saving it again allows them;
 * - `R/O`: it only reads.
 * @param {{status?:string, error?:string, writes?:boolean, effects?:Array<{text:string}>, denied_writes?:Array<{text:string}>}} entry
 */
export function accessBadge(entry) {
  const texts = (list) => (list || []).map((effect) => effect.text).join(', ');
  if (entry.status === 'error') {
    return { text: 'unavailable', kind: 'danger', title: `It cannot run: ${entry.error || 'unknown reason'}` };
  }
  if (entry.writes) {
    return { text: 'R/W', kind: 'warn', title: `Changes data when it runs: ${texts(entry.effects)}` };
  }
  if ((entry.denied_writes || []).length) {
    return {
      text: 'R/O ⚠',
      kind: 'accent',
      title: `Read-only: its writes (${texts(entry.denied_writes)}) are denied; save the query again to allow them`,
    };
  }
  return { text: 'R/O', kind: 'ok', title: 'Read-only: never changes the graph' };
}

/**
 * The language badge of a saved query (Catalog manager, run form): `{text, title}`; `text` is
 * 'GQL' or 'Rhai' (an unknown language: its name), `title` names the dialect.
 * @param {{language?:string, dialect?:string}} entry
 */
export function languageBadge(entry) {
  const language = (entry && entry.language) || 'rhai';
  const text = language === 'gql' ? 'GQL' : language === 'rhai' ? 'Rhai' : language;
  const what = language === 'gql' ? 'one GQL query' : language === 'rhai' ? 'a Rhai function' : `the language ${language}`;
  return { text, title: `Written as ${what}${entry && entry.dialect ? ` (dialect ${entry.dialect})` : ''}` };
}

/** The changes of a call preview (`previewQuery`'s `changes`) as one sentence. */
export function previewText(changes) {
  const parts = [
    ['vertices_added', 'adds', 'vertex', 'vertices'],
    ['vertices_dropped', 'deletes', 'vertex', 'vertices'],
    ['edges_added', 'adds', 'edge', 'edges'],
    ['edges_dropped', 'deletes', 'edge', 'edges'],
    ['properties_set', 'sets', 'property', 'properties'],
    ['properties_removed', 'removes', 'property', 'properties'],
    ['labels_changed', 'changes', 'label', 'labels'],
    ['other', 'makes', 'other change', 'other changes'],
  ]
    .filter(([key]) => (changes || {})[key] > 0)
    .map(([key, verb, one, many]) => `${verb} ${changes[key]} ${changes[key] === 1 ? one : many}`);
  if (!parts.length) return 'With these arguments it changes nothing.';
  const sentence = parts.length === 1 ? parts[0] : `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
  return `With these arguments it ${sentence}.`;
}

/** A one-line signature for a menu row's tooltip: `top(country, limit = 20)`. */
export function signatureText(entry) {
  const params = (entry.params || []).map((p) =>
    ('default' in p ? `${p.name} = ${JSON.stringify(p.default)}` : p.name));
  return `${entry.name}(${params.join(', ')})`;
}

// ------------------------------------------------------------------ source <-> body

/**
 * Splits a stored function text `fn name(a, b) { ... }` into its parameter names and its body
 * (inside the braces, without the newline after `{` and before `}`). `null` when the text has
 * anything before `fn` (a doc comment) or another shape: it is then edited raw.
 */
export function splitSource(source, name) {
  const escaped = name.replace(/[$]/g, '\\$&');
  const m = new RegExp(`^\\s*fn\\s+${escaped}\\s*\\(([^)]*)\\)\\s*\\{([\\s\\S]*)\\}\\s*$`).exec(source || '');
  if (!m) return null;
  const params = m[1].split(',').map((p) => p.trim()).filter(Boolean);
  let body = m[2];
  if (!body.includes('\n')) body = body.trim();
  else {
    if (body.startsWith('\n')) body = body.slice(1);
    else if (body.startsWith('\r\n')) body = body.slice(2);
    body = body.replace(/\r?\n[ \t]*$/, '');
  }
  return { params, body };
}

/**
 * The stored text of a draft: a Rhai draft's function `fn name(p1, p2) {\n<body>\n}` (a raw
 * draft: its text), a GQL draft's query as written.
 */
export function composeSource(draft) {
  if (draft.raw || draft.language === 'gql') return draft.body;
  const params = draft.params.map((p) => p.name.trim()).filter(Boolean).join(', ');
  return `fn ${draft.name.trim()}(${params}) {\n${draft.body}\n}`;
}

// ------------------------------------------------------------------ parameters

/** An empty editor parameter. */
export function newParam(name = '') {
  return {
    name, type: 'string', itemsType: 'string', minimum: '', maximum: '', minLength: '', maxLength: '',
    pattern: '', enumText: '', description: '', defaultText: '', schemaText: '',
  };
}

/** The editor's simple type of a schema node, or null when the fields cannot show it. */
function simpleType(schema) {
  if (!schema || typeof schema !== 'object' || Array.isArray(schema)) return null;
  if (Object.keys(schema).some((k) => !FIELD_KEYWORDS.has(k))) return null;
  if (schema.type === 'string' && TYPE_OF_FORMAT[schema.format]) return TYPE_OF_FORMAT[schema.format];
  if (schema.format !== undefined) return null;
  if (['string', 'integer', 'number', 'boolean', 'object'].includes(schema.type)) {
    return schema.items === undefined ? schema.type : null;
  }
  if (schema.type === 'array') {
    const items = schema.items;
    if (!items) return null;
    const t = simpleType(items);
    const plain = Object.keys(items).every((k) => k === 'type' || k === 'format');
    return plain && ITEM_TYPES.includes(t) ? 'array' : null;
  }
  return null;
}

/** The text of a default value in its field ('' without one). */
function defaultTextOf(type, entry) {
  if (!('default' in entry)) return '';
  const value = entry.default;
  if (TEXT_PARAM_TYPES.includes(type) && typeof value === 'string') return value;
  if (['integer', 'number', 'boolean'].includes(type)) return String(value);
  return JSON.stringify(value);
}

/** An editor parameter from an entry's parameter (`{name, schema, default?}`). */
export function paramFromEntry(entry) {
  const schema = entry.schema || {};
  const param = newParam(entry.name);
  const type = simpleType(schema);
  if (!type) {
    param.type = 'custom';
    param.schemaText = JSON.stringify(schema, null, 2);
    param.defaultText = 'default' in entry ? JSON.stringify(entry.default) : '';
    return param;
  }
  param.type = type;
  if (type === 'array') param.itemsType = simpleType(schema.items);
  for (const key of ['minimum', 'maximum', 'minLength', 'maxLength', 'pattern']) {
    if (schema[key] !== undefined) param[key] = String(schema[key]);
  }
  if (Array.isArray(schema.enum)) {
    param.enumText = schema.enum.every((v) => typeof v === 'string' && !v.includes(','))
      ? schema.enum.join(', ')
      : JSON.stringify(schema.enum);
  }
  param.description = schema.description || '';
  param.defaultText = defaultTextOf(type, entry);
  return param;
}

function numberField(text, label) {
  const t = String(text ?? '').trim();
  if (t === '') return { value: undefined };
  const n = Number(t);
  return Number.isFinite(n) ? { value: n } : { error: `${label} is not a number: ${t}` };
}

/** The enum list of a field: a JSON array, or comma-separated values of the type. */
function enumOf(text, type) {
  const t = String(text ?? '').trim();
  if (!t) return { value: undefined };
  if (t.startsWith('[')) {
    try {
      const list = JSON.parse(t);
      return Array.isArray(list) ? { value: list } : { error: 'allowed values: a JSON array' };
    } catch (err) {
      return { error: `allowed values are not JSON: ${err.message}` };
    }
  }
  const items = t.split(',').map((s) => s.trim()).filter((s) => s !== '');
  if (type === 'integer' || type === 'number') {
    const numbers = items.map(Number);
    if (numbers.some((n) => !Number.isFinite(n))) return { error: 'allowed values: numbers separated by commas' };
    return { value: numbers };
  }
  return { value: items };
}

/** A default's field text as a value of the type: `{value}` (undefined: none) or `{error}`. */
export function parseDefault(type, text) {
  const t = String(text ?? '');
  if (t.trim() === '') return { value: undefined };
  switch (type) {
    case 'string':
      return { value: t };
    case 'uuid':
      return UUID_RE.test(t.trim()) ? { value: t.trim() } : { error: `default is not a UUID: ${t}` };
    case 'datetime':
    case 'localdatetime':
    case 'date':
    case 'time':
    case 'offsettime':
    case 'duration':
      // The text; the engine reads it as the format when the query is stored.
      return { value: t.trim() };
    case 'integer':
      return /^[-+]?\d+$/.test(t.trim()) ? { value: Number(t.trim()) } : { error: `default is not an integer: ${t}` };
    case 'number': {
      const n = Number(t.trim());
      return Number.isFinite(n) ? { value: n } : { error: `default is not a number: ${t}` };
    }
    case 'boolean':
      if (t.trim() === 'true') return { value: true };
      if (t.trim() === 'false') return { value: false };
      return { error: 'default is true or false' };
    default:
      try {
        return { value: JSON.parse(t) };
      } catch (err) {
        return { error: `default is not JSON: ${err.message}` };
      }
  }
}

/**
 * The declaration of an editor parameter: `{declaration: {name, schema, default?}}` or
 * `{error}` (a field that does not parse; the engine checks everything else).
 */
export function paramDeclaration(param) {
  const name = param.name.trim();
  let schema;
  if (param.type === 'custom') {
    try {
      schema = JSON.parse(param.schemaText || '{}');
    } catch (err) {
      return { error: `schema is not JSON: ${err.message}` };
    }
    if (!schema || typeof schema !== 'object' || Array.isArray(schema)) return { error: 'schema is a JSON object' };
  } else {
    schema = FORMAT_OF[param.type] ? { type: 'string', format: FORMAT_OF[param.type] } : { type: param.type };
    if (param.type === 'array') {
      const it = param.itemsType || 'string';
      schema.items = FORMAT_OF[it] ? { type: 'string', format: FORMAT_OF[it] } : { type: it };
    }
    const numeric = param.type === 'integer' || param.type === 'number';
    const keys = numeric ? ['minimum', 'maximum'] : param.type === 'string' ? ['minLength', 'maxLength'] : [];
    for (const key of keys) {
      const n = numberField(param[key], key);
      if (n.error) return { error: n.error };
      if (n.value !== undefined) schema[key] = n.value;
    }
    if (param.type === 'string' && String(param.pattern || '').trim()) schema.pattern = param.pattern.trim();
    if (['string', 'integer', 'number'].includes(param.type)) {
      const e = enumOf(param.enumText, param.type);
      if (e.error) return { error: e.error };
      if (e.value !== undefined && e.value.length) schema.enum = e.value;
    }
    if (String(param.description || '').trim()) schema.description = param.description.trim();
  }
  const declaration = { name, schema };
  const parsed = parseDefault(param.type === 'custom' ? 'json' : param.type, param.defaultText);
  if (parsed.error) return { error: parsed.error };
  if (parsed.value !== undefined) declaration.default = parsed.value;
  return { declaration };
}

// ------------------------------------------------------------------ the editor draft

/** A draft for "New saved query" (in `folder`, optional; `language` 'rhai' or 'gql'). */
export function newDraft(folder = '', language = 'rhai') {
  const body = language === 'gql' ? GQL_DRAFT_BODY : 'g.v()';
  return { name: '', folder, description: '', params: [], language, body, raw: false, replaces: null };
}

/** A draft for editing a stored query (`getQuery` entry). */
export function draftFromQuery(entry) {
  const language = entry.language || 'rhai';
  if (language !== 'rhai') {
    // A GQL query (or a language this playground does not know: kept and sent back as it is).
    return {
      name: entry.name,
      folder: entry.folder || '',
      description: entry.description || '',
      params: (entry.params || []).map(paramFromEntry),
      language,
      body: entry.body || '',
      raw: false,
      replaces: entry.name,
    };
  }
  const split = splitSource(entry.body || '', entry.name);
  const params = (entry.params || []).map(paramFromEntry);
  const sameSignature = split && split.params.join(',') === params.map((p) => p.name).join(',');
  return {
    name: entry.name,
    folder: entry.folder || '',
    description: entry.description || '',
    params,
    language,
    body: split && sameSignature ? split.body : entry.body || '',
    raw: !(split && sameSignature),
    replaces: entry.name,
  };
}

/**
 * The draft's body after its language switches to `language` (the editor's Rhai / GQL switch):
 * Rhai -> GQL takes the query out of a body that is one `g.gql(..)` call, gives the GQL
 * template for the untouched new-query body, and keeps any other text (to be rewritten); GQL ->
 * Rhai wraps the query into `g.gql(`...`, #{p: p, ..})` passing every declared parameter.
 * A raw draft stops being raw (a GQL query has no signature to keep).
 */
export function switchDraftLanguage(draft, language) {
  if (draft.language === language) return draft;
  const params = draft.params.map((p) => p.name.trim()).filter(Boolean);
  let body = draft.body;
  if (language === 'gql') {
    const call = parseGqlCall(draft.raw ? (splitSource(body, draft.name.trim())?.body ?? body) : body);
    if (call) body = storedGqlText(call.query);
    else if (!body.trim() || body.trim() === 'g.v()') body = GQL_DRAFT_BODY;
  } else if (body.trim()) {
    body = wrapGql(body);
    if (params.length) body = `${body.slice(0, -1)}, #{${params.map((p) => `${p}: ${p}`).join(', ')}})`;
  } else {
    body = 'g.v()';
  }
  return { ...draft, language, body, raw: false };
}

/**
 * The parameters a GQL draft's query reads that the list does not declare yet, added as
 * string parameters (the "Add $parameters" button): the new params list.
 */
export function withDetectedParams(draft) {
  const declared = new Set(draft.params.map((p) => p.name.trim()));
  const added = gqlParamNames(draft.body).filter((name) => !declared.has(name)).map((name) => newParam(name));
  return [...draft.params, ...added];
}

// ------------------------------------------------------------------ Save as query

/** Reads a Rhai string literal at `pos` of `text` (backtick or double-quoted): `{value, end}` or null. */
function readRhaiString(text, pos) {
  const quote = text[pos];
  if (quote === '`') {
    let value = '';
    let i = pos + 1;
    while (i < text.length) {
      if (text[i] === '`') {
        if (text[i + 1] === '`') {
          value += '`';
          i += 2;
          continue;
        }
        return { value, end: i + 1 };
      }
      if (text.startsWith('${', i)) return null; // an interpolation: not a plain text
      value += text[i];
      i += 1;
    }
    return null;
  }
  if (quote !== '"') return null;
  const SIMPLE = { n: '\n', r: '\r', t: '\t', '\\': '\\', '"': '"', "'": "'", 0: '\0' };
  let value = '';
  let i = pos + 1;
  while (i < text.length) {
    const c = text[i];
    if (c === '"') return { value, end: i + 1 };
    if (c === '\\') {
      const e = text[i + 1];
      if (e in SIMPLE) {
        value += SIMPLE[e];
        i += 2;
        continue;
      }
      const hex = { x: 2, u: 4, U: 8 }[e];
      const digits = hex ? text.slice(i + 2, i + 2 + hex) : '';
      if (!hex || !/^[0-9a-fA-F]+$/.test(digits) || digits.length !== hex) return null;
      const code = parseInt(digits, 16);
      if (code > 0x10ffff) return null;
      value += String.fromCodePoint(code);
      i += 2 + hex;
      continue;
    }
    if (c === '\n') return null; // a double-quoted Rhai string ends at the line
    value += c;
    i += 1;
  }
  return null;
}

/** Skips whitespace and Rhai comments from `pos`. */
function skipSpace(text, pos) {
  let i = pos;
  for (;;) {
    while (i < text.length && /\s/.test(text[i])) i += 1;
    if (text.startsWith('//', i)) {
      const nl = text.indexOf('\n', i);
      i = nl < 0 ? text.length : nl + 1;
    } else if (text.startsWith('/*', i)) {
      const close = text.indexOf('*/', i + 2);
      if (close < 0) return text.length;
      i = close + 2;
    } else {
      return i;
    }
  }
}

/** Reads one literal value of a `#{..}` map: `{value, end}` (undefined value: not a literal) or null. */
function readLiteral(text, pos) {
  const string = readRhaiString(text, pos);
  if (string) return string;
  const m = /^(-?\d[\d_]*(\.\d[\d_]*)?([eE][-+]?\d+)?|true|false|\(\s*\))/.exec(text.slice(pos));
  if (!m) return null;
  const word = m[0];
  let value;
  if (word === 'true' || word === 'false') value = word === 'true';
  else if (word.startsWith('(')) value = null;
  else {
    const n = Number(word.replace(/_/g, ''));
    value = Number.isFinite(n) ? { number: n, integer: !/[.eE]/.test(word) } : undefined;
  }
  return { value, end: pos + word.length };
}

/** Reads a `#{key: literal, ..}` map at `pos`: `{map, end}` (each value a literal or undefined) or null. */
function readMap(text, pos) {
  if (!text.startsWith('#{', pos)) return null;
  const map = {};
  let i = skipSpace(text, pos + 2);
  if (text[i] === '}') return { map, end: i + 1 };
  for (;;) {
    let key;
    const quoted = readRhaiString(text, i);
    if (quoted) {
      key = quoted.value;
      i = quoted.end;
    } else {
      const m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(text.slice(i));
      if (!m) return null;
      key = m[0];
      i += key.length;
    }
    i = skipSpace(text, i);
    if (text[i] !== ':') return null;
    i = skipSpace(text, i + 1);
    const literal = readLiteral(text, i);
    if (!literal) return null;
    map[key] = literal.value;
    i = skipSpace(text, literal.end);
    if (text[i] === ',') {
      i = skipSpace(text, i + 1);
      if (text[i] === '}') return { map, end: i + 1 };
      continue;
    }
    if (text[i] === '}') return { map, end: i + 1 };
    return null;
  }
}

/**
 * The GQL query of a script that is exactly one call `g.gql(<string>[, #{..}])` (optionally
 * followed by `;`, comments around it): `{query, args}` (`args`: the map's literal values by
 * name, `{}` without a map or when its values are not plain literals), or null for any other
 * script (a continued traversal, a computed text, an interpolating backtick string).
 */
export function parseGqlCall(script) {
  const text = String(script ?? '');
  let i = skipSpace(text, 0);
  const head = /^g\s*\.\s*gql\s*\(/.exec(text.slice(i));
  if (!head) return null;
  i = skipSpace(text, i + head[0].length);
  const literal = readRhaiString(text, i);
  if (!literal) return null;
  i = skipSpace(text, literal.end);
  let args = {};
  if (text[i] === ',') {
    i = skipSpace(text, i + 1);
    if (text.startsWith('#{', i)) {
      const map = readMap(text, i);
      if (map) {
        args = map.map;
        i = skipSpace(text, map.end);
      } else {
        // Not a literal map: skip to its closing brace (string-blind, best effort).
        let depth = 0;
        let j = i + 1;
        for (; j < text.length; j++) {
          if (text[j] === '{') depth += 1;
          else if (text[j] === '}' && --depth === 0) break;
        }
        if (j >= text.length) return null;
        i = skipSpace(text, j + 1);
      }
    } else {
      return null;
    }
  }
  if (text[i] !== ')') return null;
  i = skipSpace(text, i + 1);
  if (text[i] === ';') i = skipSpace(text, i + 1);
  if (i !== text.length) return null;
  return { query: literal.value, args };
}

/**
 * The query text of a `g.gql(..)` literal as it is stored: without the blank lines around it and
 * the indentation `wrapGql` writes (the common indentation of its lines), trailing spaces cut.
 */
export function storedGqlText(query) {
  const lines = String(query ?? '').replace(/\r\n?/g, '\n').split('\n');
  while (lines.length && !lines[0].trim()) lines.shift();
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  const used = lines.filter((l) => l.trim());
  const indent = used.length ? Math.min(...used.map((l) => l.length - l.trimStart().length)) : 0;
  return lines.map((l) => l.slice(indent).trimEnd()).join('\n');
}

/** An editor parameter for the `$name` of a tab's query, typed by the tab's value when it has one. */
function paramFromArg(name, value) {
  const param = newParam(name);
  if (value === undefined || value === null) return param;
  if (typeof value === 'boolean') {
    param.type = 'boolean';
    param.defaultText = String(value);
  } else if (typeof value === 'string') {
    param.defaultText = value;
  } else if (typeof value === 'object' && 'number' in value) {
    param.type = value.integer ? 'integer' : 'number';
    param.defaultText = String(value.number);
  }
  return param;
}

/**
 * The draft "Save as query" starts from the text of a query tab: a tab holding exactly one
 * `g.gql(`...`[, #{..}])` call gives a GQL draft (its query text, its `$name`s as parameters,
 * typed and defaulted by the tab's literal values); any other text a Rhai draft whose body is
 * the tab's text, without parameters.
 */
export function draftFromScript(script, folder = '') {
  const call = parseGqlCall(script);
  if (call) {
    return {
      ...newDraft(folder, 'gql'),
      body: storedGqlText(call.query),
      params: gqlParamNames(call.query).map((name) => paramFromArg(name, call.args[name])),
    };
  }
  const body = String(script ?? '').trim();
  return { ...newDraft(folder, 'rhai'), body: body || 'g.v()' };
}

/**
 * The draft "Save as query" starts from a GQL query tab: its query text and its `$name`s as
 * parameters, typed and defaulted by the tab's Parameters values (`args` in the form
 * `parseGqlCall` gives them, js/gql-tab.js `paramArgs`).
 */
export function draftFromGql(query, args = {}, folder = '') {
  return {
    ...newDraft(folder, 'gql'),
    body: storedGqlText(query),
    params: gqlParamNames(query).map((name) => paramFromArg(name, args[name])),
  };
}

/**
 * The spec `defineQuery` takes, from a draft: `{spec}`, or `{errors}` (field errors found
 * before the engine is asked: a parameter field that does not parse, a missing name).
 */
export function draftToSpec(draft) {
  const errors = [];
  const name = draft.name.trim();
  if (!name) errors.push({ field: 'name', message: 'A saved query needs a name (letters, digits and _, e.g. top_customers).' });
  else if (!NAME_RE.test(name)) errors.push({ field: 'name', message: `"${name}" is not a valid name: letters, digits and _, not starting with a digit.` });
  const params = [];
  const seen = new Set();
  draft.params.forEach((param, index) => {
    const pname = param.name.trim();
    const field = `params.${pname || index}`;
    if (!pname) {
      errors.push({ field, message: `Parameter ${index + 1} needs a name.` });
      return;
    }
    if (seen.has(pname)) {
      errors.push({ field, message: `The parameter "${pname}" is declared twice.` });
      return;
    }
    seen.add(pname);
    const result = paramDeclaration(param);
    if (result.error) errors.push({ field, message: result.error });
    else params.push(result.declaration);
  });
  if (errors.length) return { errors };
  const spec = {
    name,
    folder: draft.folder.trim() || null,
    description: draft.description.trim() || null,
    params,
    body: composeSource({ ...draft, name }),
  };
  // Rhai is the engine's default: a Rhai spec stays as it always was.
  if (draft.language && draft.language !== 'rhai') spec.language = draft.language;
  if (draft.replaces) spec.replaces = draft.replaces;
  return { spec };
}

/** Field errors by field: `{name: [..], 'params.limit': [..], ...}`. */
export function errorsByField(errors) {
  const out = {};
  for (const e of errors || []) {
    const text = e.help ? `${e.message} ${e.help}` : e.message;
    (out[e.field] = out[e.field] || []).push(text);
  }
  return out;
}

// ------------------------------------------------------------------ the run form

/** The base type of a schema node without `null` (`['integer', 'null']` -> 'integer'). */
function baseType(schema) {
  const t = schema && schema.type;
  if (Array.isArray(t)) {
    const rest = t.filter((x) => x !== 'null');
    return rest.length === 1 ? rest[0] : null;
  }
  return t || null;
}

/**
 * The fields of the run form, one per parameter: `{name, kind, required, hint, schema, min,
 * max, options, itemsKind, defaultValue, hasDefault}`. `kind`: 'select' (enum), 'integer',
 * 'number', 'checkbox' (boolean), 'uuid', a date and time type ('datetime', 'localdatetime',
 * 'date', 'time', 'offsettime', 'duration': its ISO 8601 text), 'list' (array of simple items, one per line),
 * 'json' (objects and anything else), 'text' (string).
 */
export function formFields(entry) {
  return (entry.params || []).map((p) => {
    const schema = p.schema || {};
    const type = baseType(schema);
    let kind = 'json';
    let itemsKind = null;
    if (Array.isArray(schema.enum)) kind = 'select';
    else if (type === 'string' && TYPE_OF_FORMAT[schema.format]) kind = TYPE_OF_FORMAT[schema.format];
    else if (type === 'string') kind = 'text';
    else if (type === 'integer' || type === 'number') kind = type;
    else if (type === 'boolean') kind = 'checkbox';
    else if (type === 'array' && schema.items) {
      const it = baseType(schema.items);
      itemsKind = it === 'string' && TYPE_OF_FORMAT[schema.items.format] ? TYPE_OF_FORMAT[schema.items.format] : it;
      kind = ITEM_TYPES.includes(itemsKind) ? 'list' : 'json';
    }
    return {
      name: p.name,
      kind,
      required: !!p.required,
      hint: p.description || schema.description || '',
      schema,
      min: schema.minimum,
      max: schema.maximum,
      options: Array.isArray(schema.enum) ? schema.enum : null,
      itemsKind,
      hasDefault: 'default' in p,
      defaultValue: p.default,
    };
  });
}

/** A value as the text of its field (a select option is the value's JSON). */
function fieldText(field, value) {
  if (value === undefined) return '';
  switch (field.kind) {
    case 'select':
      return JSON.stringify(value);
    case 'checkbox':
      return value === true;
    case 'text':
    case 'uuid':
    case 'datetime':
    case 'localdatetime':
    case 'date':
    case 'time':
    case 'offsettime':
    case 'duration':
      return typeof value === 'string' ? value : JSON.stringify(value);
    case 'integer':
    case 'number':
      return String(value);
    case 'list':
      return Array.isArray(value) ? value.map((v) => (typeof v === 'string' ? v : JSON.stringify(v))).join('\n') : JSON.stringify(value);
    default:
      return JSON.stringify(value, null, 2);
  }
}

/**
 * The form's first values: the last values used for this query (`saved`, from localStorage)
 * where they still fit the fields, else the defaults (prefilled), else empty.
 */
export function initialValues(fields, saved = {}) {
  const values = {};
  for (const f of fields) {
    const last = saved && Object.prototype.hasOwnProperty.call(saved, f.name) ? saved[f.name] : undefined;
    if (last !== undefined && (f.kind === 'checkbox' ? typeof last === 'boolean' : typeof last === 'string')) {
      values[f.name] = last;
    } else if (f.kind === 'checkbox') {
      values[f.name] = f.hasDefault ? f.defaultValue === true : false;
    } else {
      values[f.name] = f.hasDefault ? fieldText(f, f.defaultValue) : '';
    }
  }
  return values;
}

function parseItem(kind, text) {
  switch (kind) {
    case 'integer':
      return /^[-+]?\d+$/.test(text) ? { value: Number(text) } : { error: `"${text}" is not an integer` };
    case 'number': {
      const n = Number(text);
      return text !== '' && Number.isFinite(n) ? { value: n } : { error: `"${text}" is not a number` };
    }
    case 'boolean':
      return text === 'true' || text === 'false' ? { value: text === 'true' } : { error: `"${text}" is not true or false` };
    case 'uuid':
      return UUID_RE.test(text) ? { value: text } : { error: `"${text}" is not a UUID (8-4-4-4-12 hex digits)` };
    default:
      return { value: text };
  }
}

/**
 * The named arguments of a filled form: `{args, errors}` (`errors`: field name -> message).
 * An empty field is left out, so the parameter's default applies; an empty required field is
 * an error. Values are converted to the declared type here (the engine checks them again).
 */
export function formArgs(fields, values) {
  const args = {};
  const errors = {};
  for (const f of fields) {
    const raw = values[f.name];
    if (f.kind === 'checkbox') {
      args[f.name] = raw === true;
      continue;
    }
    const text = String(raw ?? '').trim();
    if (text === '') {
      if (f.required) errors[f.name] = 'required';
      continue;
    }
    let result;
    switch (f.kind) {
      case 'select':
        try {
          result = { value: JSON.parse(text) };
        } catch (err) {
          result = { value: text };
        }
        break;
      case 'integer':
      case 'number':
        result = parseItem(f.kind, text);
        if (result.value !== undefined && f.kind === 'integer' && !Number.isSafeInteger(result.value)) {
          result = { error: `${text} is outside the exact integer range of the form; write the call by hand` };
        }
        if (result.value !== undefined && f.min !== undefined && result.value < f.min) result = { error: `at least ${f.min}` };
        if (result.value !== undefined && f.max !== undefined && result.value > f.max) result = { error: `at most ${f.max}` };
        break;
      case 'uuid':
        result = parseItem('uuid', text);
        break;
      case 'list':
        if (text.startsWith('[')) {
          try {
            result = { value: JSON.parse(text) };
          } catch (err) {
            result = { error: `not a JSON array: ${err.message}` };
          }
        } else {
          const items = [];
          for (const line of text.split('\n').map((l) => l.trim()).filter((l) => l !== '')) {
            const item = parseItem(f.itemsKind, line);
            if (item.error) {
              result = item;
              break;
            }
            items.push(item.value);
          }
          if (!result) result = { value: items };
        }
        break;
      case 'json':
        try {
          result = { value: JSON.parse(text) };
        } catch (err) {
          result = { error: `not JSON: ${err.message}` };
        }
        break;
      default:
        result = { value: String(raw) };
    }
    if (result.error) errors[f.name] = result.error;
    else args[f.name] = result.value;
  }
  return { args, errors };
}

/** The values to remember for a query (localStorage, per query name). */
export function rememberValues(all, name, values, max = 100) {
  const next = { ...(all && typeof all === 'object' ? all : {}) };
  delete next[name];
  next[name] = { ...values };
  const names = Object.keys(next);
  for (const old of names.slice(0, Math.max(0, names.length - max))) delete next[old];
  return next;
}

// ------------------------------------------------------------------ completion

/**
 * The name being typed as the first argument of `g.query(`, or null: `before` is the text of
 * the line up to the cursor. `{prefix, quote}`: the typed part of the name and its quote.
 */
export function queryNameContext(before) {
  const m = /\bquery\s*\(\s*(["'`])([A-Za-z0-9_]*)$/.exec(before || '');
  return m ? { quote: m[1], prefix: m[2] } : null;
}
