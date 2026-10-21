// GQL query tabs: the pure logic behind a Query tab whose language is GQL (tabs.js keeps
// `language` and `params` per tab, store.js runs it, components/query-editor.js shows it). No Vue,
// no CodeMirror, no DOM (tests/gql-tab.test.mjs).
//
// A Query tab is Rhai (the default) or GQL, chosen explicitly (the "+" menu's "GQL query", the
// editor's Rhai / GQL switch); nothing is detected from the text. A GQL tab holds the plain GQL
// query and a Parameters field: the JSON text of a map `{"name": value}` that `$name` reads.
//
// Running a GQL tab runs the Rhai script `g.gql("<query>", #{..})` (`gqlCallText`), built exactly
// like the engine's own `graphersal_session::queries::gql_call_text` (the MCP `gql` tool): the
// JSON values become Rhai literals, integers stay integers (`30`) and floats floats (`30.0`), as
// serde_json reads them. Errors keep their GQLSTATUS; `gqlErrorSpan` reads the span the engine
// reports for the query text (`GQL syntax error at line L, column C`, the caret excerpt), so
// the editor can underline it.

import { parseGqlCall, storedGqlText } from './saved-queries.js';
import { wrapGql } from './editor/gql-wrap.js';
import { gqlParamNames } from './editor/gql-tokens.js';

/** The languages of a Query tab. */
export const TAB_LANGUAGES = Object.freeze(['rhai', 'gql']);

/** The query a new, empty GQL tab starts with. */
export const GQL_TAB_TEMPLATE = 'MATCH (p:person)\nRETURN p.name AS name\nORDER BY name';

/** The deepest nesting of a parameter value (the engine's `MAX_LITERAL_DEPTH`). */
export const MAX_PARAM_DEPTH = 128;

/** A tab language from anything stored: 'gql' or the default 'rhai'. */
export function tabLanguage(value) {
  return value === 'gql' ? 'gql' : 'rhai';
}

// ------------------------------------------------------------------ the Parameters field

/** The 1-based `{line, column}` of `index` in `text`. */
function lineColumn(text, index) {
  let line = 1;
  let start = 0;
  for (let i = 0; i < index && i < text.length; i++) {
    if (text[i] === '\n') {
      line += 1;
      start = i + 1;
    }
  }
  return { line, column: index - start + 1 };
}

class ParamsError extends Error {
  constructor(message, at) {
    super(message);
    this.at = at;
  }
}

/**
 * A strict JSON reader that keeps what JSON.parse loses: whether a number was written as an
 * integer (`30`) or a float (`30.0`, `3e1`), and its text. Nodes: `{kind: 'null'|'bool'|'int'|
 * 'float'|'string'|'array'|'object', value, text?, items?, entries?}`.
 */
function readJson(text) {
  let i = 0;
  const space = () => {
    while (i < text.length && /[ \t\r\n]/.test(text[i])) i += 1;
  };
  const fail = (message) => {
    throw new ParamsError(message, i);
  };
  function value(depth) {
    if (depth > MAX_PARAM_DEPTH) fail(`the values nest deeper than ${MAX_PARAM_DEPTH} levels`);
    space();
    const c = text[i];
    if (c === '{') return object(depth);
    if (c === '[') return array(depth);
    if (c === '"') return { kind: 'string', value: string() };
    const word = /^(true|false|null)/.exec(text.slice(i));
    if (word) {
      i += word[0].length;
      if (word[0] === 'null') return { kind: 'null', value: null };
      return { kind: 'bool', value: word[0] === 'true' };
    }
    const num = /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?/.exec(text.slice(i));
    if (num && num[0] !== '-') {
      i += num[0].length;
      const integer = !num[2] && !num[3];
      return { kind: integer ? 'int' : 'float', value: Number(num[0]), text: num[0] };
    }
    if (i >= text.length) fail('the text ends where a value should follow');
    if (c === "'") fail('strings take double quotes in JSON ("text"), not single quotes');
    if (/[A-Za-z_]/.test(c)) fail(`unexpected word "${/^[A-Za-z_][A-Za-z0-9_]*/.exec(text.slice(i))[0]}": strings take double quotes ("text"), and the literals are true, false and null`);
    return fail(`unexpected "${c}"`);
  }
  function string() {
    const start = i;
    i += 1;
    while (i < text.length) {
      const c = text[i];
      if (c === '"') {
        i += 1;
        try {
          return JSON.parse(text.slice(start, i));
        } catch (e) {
          i = start;
          return fail('the string holds an invalid escape');
        }
      }
      if (c === '\\') i += 2;
      else if (c === '\n') fail('a string ends at the end of its line (write \\n for a line break)');
      else i += 1;
    }
    i = start;
    return fail('the string is not closed (")');
  }
  function array(depth) {
    i += 1;
    const items = [];
    space();
    if (text[i] === ']') {
      i += 1;
      return { kind: 'array', items };
    }
    for (;;) {
      items.push(value(depth + 1));
      space();
      if (text[i] === ',') {
        i += 1;
        continue;
      }
      if (text[i] === ']') {
        i += 1;
        return { kind: 'array', items };
      }
      fail('a list continues with "," or ends with "]"');
    }
  }
  function object(depth) {
    i += 1;
    const entries = [];
    space();
    if (text[i] === '}') {
      i += 1;
      return { kind: 'object', entries };
    }
    for (;;) {
      space();
      if (text[i] !== '"') fail('a key is a string in double quotes ("name": value)');
      const key = string();
      space();
      if (text[i] !== ':') fail(`":" must follow the key "${key}"`);
      i += 1;
      const item = value(depth + 1);
      const at = entries.findIndex(([k]) => k === key);
      if (at >= 0) entries.splice(at, 1);
      entries.push([key, item]);
      space();
      if (text[i] === ',') {
        i += 1;
        continue;
      }
      if (text[i] === '}') {
        i += 1;
        return { kind: 'object', entries };
      }
      fail('a map continues with "," or ends with "}"');
    }
  }
  const root = value(0);
  space();
  if (i < text.length) fail('unexpected text after the value');
  return root;
}

/**
 * The Parameters field of a GQL tab: `{ok: true, entries: [[name, node]], names}` (blank text is
 * the empty map), or `{ok: false, message, line, column}` with the 1-based position of the fault.
 */
export function parseParams(text) {
  const source = String(text ?? '');
  if (!source.trim()) return { ok: true, entries: [], names: [] };
  let root;
  try {
    root = readJson(source);
  } catch (err) {
    if (!(err instanceof ParamsError)) throw err;
    const { line, column } = lineColumn(source, err.at);
    return { ok: false, message: err.message, line, column };
  }
  if (root.kind !== 'object') {
    const what = { array: 'a list', string: 'a string', int: 'a number', float: 'a number', bool: 'a boolean', null: 'null' }[root.kind];
    return { ok: false, message: `the parameters are a JSON map {"name": value}, not ${what}`, line: 1, column: 1 };
  }
  return { ok: true, entries: root.entries, names: root.entries.map(([k]) => k) };
}

/** The text of a field's error: `line 1, column 9: unexpected "x"`. */
export function paramsErrorText(parsed) {
  if (!parsed || parsed.ok) return '';
  return `Parameters, line ${parsed.line}, column ${parsed.column}: ${parsed.message}`;
}

// ------------------------------------------------------------------ Rhai literals

/** A double-quoted Rhai string (the engine's `rhai_string`). */
export function rhaiString(text) {
  let out = '"';
  for (const c of String(text)) {
    if (c === '"') out += '\\"';
    else if (c === '\\') out += '\\\\';
    else if (c === '\n') out += '\\n';
    else if (c === '\r') out += '\\r';
    else if (c === '\t') out += '\\t';
    else if (/[\u0000-\u001f\u007f-\u009f]/.test(c)) out += '\\u' + c.codePointAt(0).toString(16).padStart(4, '0');
    else out += c;
  }
  return out + '"';
}

const I64_MIN = -(2n ** 63n);
const I64_MAX = 2n ** 63n - 1n;

/** A float as the engine writes it: `2.5`, `30.0`, `1e20`, `1.5e-7`. */
function floatText(x) {
  const abs = Math.abs(x);
  let text = abs !== 0 && !(abs >= 1e-6 && abs < 1e16) ? x.toExponential().replace('e+', 'e') : String(x);
  if (!/[.e]/.test(text)) text += '.0';
  return text;
}

/** The Rhai literal of a parameter node: `()`, `true`, `5`, `2.5`, `"x"`, `[..]`, `#{k: v}`. */
export function rhaiLiteral(node) {
  switch (node.kind) {
    case 'null': return '()';
    case 'bool': return node.value ? 'true' : 'false';
    case 'int': {
      const big = BigInt(node.text);
      if (big >= I64_MIN && big <= I64_MAX) return big.toString();
      return floatText(node.value);
    }
    case 'float':
      if (!Number.isFinite(node.value)) throw new Error(`the number ${node.text} has no Rhai literal`);
      return floatText(node.value);
    case 'string': return rhaiString(node.value);
    case 'array': return `[${node.items.map(rhaiLiteral).join(', ')}]`;
    case 'object': return `#{${node.entries.map(([k, v]) => `${rhaiKey(k)}: ${rhaiLiteral(v)}`).join(', ')}}`;
    default: throw new Error(`unknown value ${node.kind}`);
  }
}

/** A Rhai object-map key: bare when it is an identifier, a string literal otherwise. */
function rhaiKey(key) {
  return /^[A-Za-z_][A-Za-z0-9_]*$/.test(key) ? key : rhaiString(key);
}

/**
 * The Rhai script that runs a GQL tab: `g.gql("<query>")`, or `g.gql("<query>", #{..})` with the
 * parameters. `{script}`, or `{error}` when the Parameters field is not a JSON map.
 */
export function gqlCallText(query, paramsText = '') {
  const parsed = parseParams(paramsText);
  if (!parsed.ok) return { error: paramsErrorText(parsed) };
  try {
    const args = parsed.entries.length ? `, ${rhaiLiteral({ kind: 'object', entries: parsed.entries })}` : '';
    return { script: `g.gql(${rhaiString(query)}${args})` };
  } catch (err) {
    return { error: `Parameters: ${err.message}` };
  }
}

// ------------------------------------------------------------------ switching the language

/** A parameter value read by `parseGqlCall` (a literal, or undefined) as JSON text. */
function argJson(value) {
  if (value === null) return 'null';
  if (typeof value === 'boolean' || typeof value === 'string') return JSON.stringify(value);
  if (value && typeof value === 'object' && 'number' in value) {
    const n = value.number;
    if (value.integer) return String(n);
    const text = String(n);
    return /[.e]/.test(text) ? text : `${text}.0`;
  }
  return 'null';
}

/** The Parameters text of a map of `parseGqlCall` arguments (`{}` gives ''). */
export function paramsTextOf(args) {
  const keys = Object.keys(args || {});
  if (!keys.length) return '';
  return `{\n${keys.map((k) => `  ${JSON.stringify(k)}: ${argJson(args[k])}`).join(',\n')}\n}`;
}

/**
 * The tab's text and parameters after its language switches to `language`, like the saved-query
 * editor's switch (`switchDraftLanguage`): Rhai -> GQL takes the query (and the literal values of
 * its `#{..}` map) out of a script that is exactly one `g.gql(..)` call, keeps any other text;
 * GQL -> Rhai writes the query and its parameters as `g.gql(`...`, #{..})`. An empty tab gets
 * the other language's starting text. Returns `{script, params}` or `{error}` (GQL -> Rhai with
 * a Parameters field that is not a JSON map: nothing is switched).
 */
export function switchTabLanguage({ script, params = '' }, language) {
  const text = String(script ?? '');
  if (tabLanguage(language) === 'gql') {
    const call = parseGqlCall(text);
    if (call) {
      const own = paramsTextOf(call.args);
      return { script: storedGqlText(call.query), params: own || params };
    }
    if (!text.trim()) return { script: GQL_TAB_TEMPLATE, params };
    return { script: text, params };
  }
  if (!text.trim()) return { script: '', params };
  const parsed = parseParams(params);
  if (!parsed.ok) return { error: paramsErrorText(parsed) };
  let rhai = wrapGql(text);
  if (parsed.entries.length) {
    let map;
    try {
      map = rhaiLiteral({ kind: 'object', entries: parsed.entries });
    } catch (err) {
      return { error: `Parameters: ${err.message}` };
    }
    rhai = `${rhai.slice(0, -1)}, ${map})`;
  }
  return { script: rhai, params: '' };
}

/**
 * The Parameters text with an entry for every `$name` the query reads that it lacks (the
 * "+ Add $parameters" button), each `null` to be filled in: `{text, added}`, or `{error}` when the
 * field is not a JSON map. The existing text is kept as written when nothing is missing.
 */
export function withQueryParams(paramsText, query) {
  const parsed = parseParams(paramsText);
  if (!parsed.ok) return { error: paramsErrorText(parsed) };
  const missing = gqlParamNames(query).filter((name) => !parsed.names.includes(name));
  if (!missing.length) return { text: String(paramsText ?? ''), added: [] };
  const lines = parsed.entries.map(([k, v]) => `  ${JSON.stringify(k)}: ${jsonText(v)}`);
  for (const name of missing) lines.push(`  ${JSON.stringify(name)}: null`);
  return { text: `{\n${lines.join(',\n')}\n}`, added: missing };
}

/** A parameter node back as compact JSON text (numbers as written). */
function jsonText(node) {
  switch (node.kind) {
    case 'int':
    case 'float': return node.text;
    case 'array': return `[${node.items.map(jsonText).join(', ')}]`;
    case 'object': return `{${node.entries.map(([k, v]) => `${JSON.stringify(k)}: ${jsonText(v)}`).join(', ')}}`;
    default: return JSON.stringify(node.value);
  }
}

/**
 * The parameter values for the saved-query editor ("Save as query" from a GQL tab): by name, in
 * the form `parseGqlCall` gives them (`{number, integer}`, strings, booleans, null; lists and maps
 * undefined: the editor then takes no default).
 */
export function paramArgs(paramsText) {
  const parsed = parseParams(paramsText);
  const args = {};
  if (!parsed.ok) return args;
  for (const [name, node] of parsed.entries) {
    if (node.kind === 'int' || node.kind === 'float') args[name] = { number: node.value, integer: node.kind === 'int' };
    else if (node.kind === 'array' || node.kind === 'object') args[name] = undefined;
    else args[name] = node.value;
  }
  return args;
}

// ------------------------------------------------------------------ errors

/**
 * The span a GQL error reports for the query text: `{line, column, width, gqlstatus}` (1-based,
 * columns in characters), or null when the error carries none (a Rhai error, a limit). Read from
 * the message (`GQL syntax error at line 2, column 21: ...`) and the diagnostic (the caret line
 * under the excerpt, `GQLSTATUS: 42001`).
 */
export function gqlErrorSpan(error) {
  if (!error) return null;
  const texts = [error.message, error.diagnostic].filter((t) => typeof t === 'string');
  let at = null;
  for (const t of texts) {
    at = /GQL [a-z ]+ at line (\d+), column (\d+)/.exec(t);
    if (at) break;
  }
  if (!at) return null;
  const diagnostic = typeof error.diagnostic === 'string' ? error.diagnostic : '';
  const status = /GQLSTATUS:\s*([0-9A-Z]{5})/.exec(diagnostic) || /\(([0-9A-Z]{5})\)/.exec(error.message || '');
  let width = 1;
  const lines = diagnostic.split('\n');
  for (let i = 0; i + 1 < lines.length; i++) {
    if (/^\s+(line \d+|query):/.test(lines[i])) {
      const carets = /\^+/.exec(lines[i + 1]);
      if (carets) width = carets[0].length;
      break;
    }
  }
  return { line: Number(at[1]), column: Number(at[2]), width, gqlstatus: status ? status[1] : null };
}

/**
 * The character range `[from, to)` of a span in `text` (the editor's text; `offset` the
 * `{line, column}` of the first character that ran, from js/editor/run-selection.js `runText`).
 * Columns count characters (code points), as the engine does. Null when the span lies outside.
 */
export function spanRange(text, span, offset = { line: 1, column: 1 }) {
  if (!span) return null;
  const lines = String(text).split('\n');
  const lineNo = span.line + offset.line - 1;
  if (lineNo < 1 || lineNo > lines.length) return null;
  let from = 0;
  for (let i = 0; i < lineNo - 1; i++) from += lines[i].length + 1;
  const line = lines[lineNo - 1];
  const chars = Array.from(line);
  const startCol = (span.line === 1 ? span.column + offset.column - 1 : span.column) - 1;
  if (startCol > chars.length) return null;
  const units = (n) => chars.slice(0, n).join('').length;
  const start = from + units(startCol);
  const end = from + units(Math.min(chars.length, startCol + Math.max(1, span.width || 1)));
  return { from: start, to: Math.max(end, start + (start < from + line.length ? 1 : 0)) };
}
