// The GQL tokenizer of the playground: highlighting of GQL text (the saved-query editor's GQL
// mode, js/editor/gql-language.js) and the `$name` parameters a GQL query reads (Save as query,
// js/saved-queries.js). Pure: no CodeMirror, no DOM (tested in tests/gql-tokens.test.mjs).
//
// It follows the lexical rules of the engine's GQL lexer (crates/graphersal/src/gql/lexer.rs)
// as far as highlighting needs them: `//`, `/* */` and `--` comments (`--` is an edge when `>`
// or `(` follows it or a `)` precedes it on the same line), '...' and "..." strings (a doubled
// quote or a backslash escape inside), back-quoted names, `$name` / `$`quoted`` parameters,
// numbers, case-insensitive keywords, labels after `:` and property names after `.`, and the
// name of a called saved query after `CALL` (`CALL people.older(..)`: every segment of the
// dotted name a 'procedure'; `CALL (` / `CALL {` is a subquery). It never fails: text the engine
// would refuse is still given a token.

/** The keywords of the engine's lexer (ASCII case-insensitive), without the literals below. */
export const GQL_KEYWORDS = new Set([
  'MATCH', 'WHERE', 'RETURN', 'DISTINCT', 'AS', 'ORDER', 'BY', 'ASC', 'ASCENDING', 'DESC',
  'DESCENDING', 'OFFSET', 'SKIP', 'LIMIT', 'AND', 'OR', 'XOR', 'NOT', 'IS', 'IN', 'STARTS',
  'ENDS', 'CONTAINS', 'WITH', 'CASE', 'WHEN', 'THEN', 'ELSE', 'END', 'LABELED', 'OPTIONAL',
  'FILTER', 'LET', 'NULLS', 'FIRST', 'LAST', 'INSERT', 'CREATE', 'SET', 'REMOVE', 'DELETE',
  'DETACH', 'NODETACH', 'MERGE', 'CALL', 'YIELD', 'UNION', 'EXCEPT', 'INTERSECT', 'NEXT', 'FOR',
  'UNWIND', 'EXISTS', 'SHOW', 'USE', 'SHORTEST', 'CYPHER', 'ON',
]);

/** The literal keywords. */
export const GQL_ATOMS = new Set(['TRUE', 'FALSE', 'NULL', 'UNKNOWN']);

/** A fresh scanner state (the start of a text). */
export function gqlStartState() {
  return {
    inComment: false, inString: null, afterDot: false, afterColon: false, afterLabel: false, afterRParen: false,
    braces: 0, // open `{` (inside a map `:` separates a key from its value, it starts no label)
    // The name of a called saved query: 'start' after `CALL`, 'segment' after one of its
    // segments, 'dot' after the `.` between two; null elsewhere.
    procedure: null,
  };
}

/** Resets what only lasts to the end of a line (the `--` rule looks at `)` on the same line). */
export function gqlStartLine(state) {
  state.afterRParen = false;
}

const IDENT_START = /[\p{L}_]/u;
const IDENT_REST = /^[\p{L}\p{N}_]*/u;

/** Scans the rest of a string (or back-quoted name) closed by `quote`: the end, or -1 when it runs on. */
function stringEnd(line, pos, quote) {
  let i = pos;
  while (i < line.length) {
    const c = line[i];
    if (c === '\\' && quote !== '`') {
      i += 2;
      continue;
    }
    if (c === quote) {
      if (line[i + 1] === quote) {
        i += 2;
        continue;
      }
      return i + 1;
    }
    i += 1;
  }
  return -1;
}

/**
 * The token of `line` that starts at `pos` (after any whitespace, which is a token of its own):
 * `{end, type}` with `type` one of 'comment', 'string', 'number', 'keyword', 'atom', 'param',
 * 'label', 'property', 'name' (a back-quoted name), 'function', 'procedure' (a segment of the
 * saved query's name after `CALL`), 'variable', 'operator',
 * 'punctuation', or null (whitespace). `state` (from `gqlStartState`) carries comments and
 * strings across lines and is updated.
 */
export function scanGql(line, pos, state) {
  if (state.inComment) {
    const close = line.indexOf('*/', pos);
    if (close < 0) return { end: line.length, type: 'comment' };
    state.inComment = false;
    return { end: close + 2, type: 'comment' };
  }
  if (state.inString) {
    const quote = state.inString;
    const end = stringEnd(line, pos, quote);
    if (end < 0) return { end: line.length, type: quote === '`' ? 'name' : 'string' };
    state.inString = null;
    return { end, type: quote === '`' ? 'name' : 'string' };
  }
  const ws = /^\s+/.exec(line.slice(pos));
  if (ws) return { end: pos + ws[0].length, type: null };
  const rest = line.slice(pos);
  const afterDot = state.afterDot;
  const afterColon = state.afterColon;
  const afterLabel = state.afterLabel;
  state.afterDot = false;
  state.afterColon = false;
  state.afterLabel = false;
  const rparen = state.afterRParen;
  state.afterRParen = false;
  const procedure = state.procedure;
  state.procedure = null;
  if (rest.startsWith('//')) return { end: line.length, type: 'comment' };
  if (rest.startsWith('/*')) {
    const close = line.indexOf('*/', pos + 2);
    if (close < 0) {
      state.inComment = true;
      return { end: line.length, type: 'comment' };
    }
    return { end: close + 2, type: 'comment' };
  }
  if (rest.startsWith('--') && !(rest[2] === '>' || rest[2] === '(' || rparen)) {
    return { end: line.length, type: 'comment' };
  }
  const c = rest[0];
  if (c === "'" || c === '"' || c === '`') {
    const end = stringEnd(line, pos + 1, c);
    if (end < 0) {
      state.inString = c;
      return { end: line.length, type: c === '`' ? 'name' : 'string' };
    }
    if (c === '`') {
      if (procedure === 'start' || procedure === 'dot') {
        state.procedure = 'segment';
        return { end, type: 'procedure' };
      }
      state.afterLabel = afterColon;
      return { end, type: afterColon ? 'label' : afterDot ? 'property' : 'name' };
    }
    return { end, type: 'string' };
  }
  if (c === '$') {
    if (rest[1] === '`') {
      const end = stringEnd(line, pos + 2, '`');
      return { end: end < 0 ? line.length : end, type: 'param' };
    }
    const m = IDENT_REST.exec(rest.slice(1));
    return { end: pos + 1 + m[0].length, type: 'param' };
  }
  const number = /^(0[xX][0-9a-fA-F_]+|0[oO][0-7_]+|0[bB][01_]+|(\d[\d_]*)?\.?\d[\d_]*([eE][-+]?\d+)?)/.exec(rest);
  if (number && number[0] && /\d/.test(c === '.' ? rest[1] || '' : c)) {
    return { end: pos + number[0].length, type: 'number' };
  }
  if (IDENT_START.test(c)) {
    const m = IDENT_REST.exec(rest.slice(1));
    const word = c + m[0];
    const end = pos + word.length;
    if (procedure === 'start' || procedure === 'dot') {
      state.procedure = 'segment';
      return { end, type: 'procedure' };
    }
    if (afterColon) {
      state.afterLabel = true;
      return { end, type: 'label' };
    }
    if (afterDot) return { end, type: 'property' };
    const upper = word.toUpperCase();
    if (GQL_ATOMS.has(upper)) return { end, type: 'atom' };
    if (upper === 'CALL') state.procedure = 'start';
    if (/^\s*\(/.test(line.slice(end))) return { end, type: GQL_KEYWORDS.has(upper) ? 'keyword' : 'function' };
    if (GQL_KEYWORDS.has(upper)) return { end, type: 'keyword' };
    return { end, type: 'variable' };
  }
  if (c === '.') {
    if (procedure === 'segment') {
      state.procedure = 'dot';
      return { end: pos + 1, type: 'punctuation' };
    }
    state.afterDot = true;
    return { end: pos + 1, type: 'punctuation' };
  }
  if (c === ':') {
    state.afterColon = state.braces === 0;
    return { end: pos + 1, type: 'punctuation' };
  }
  if (c === '{') state.braces += 1;
  if (c === '}') state.braces = Math.max(0, state.braces - 1);
  if ((c === '|' || c === '&') && afterLabel) {
    // Label expressions: `:a|b`, `:a&b` keep the label colour.
    state.afterColon = true;
    return { end: pos + 1, type: 'operator' };
  }
  if (c === '!' && afterColon) {
    state.afterColon = true;
    return { end: pos + 1, type: 'operator' };
  }
  if (c === ')') {
    state.afterRParen = true;
    return { end: pos + 1, type: 'punctuation' };
  }
  if (/[-+*/%=<>^~]/.test(c)) return { end: pos + 1, type: 'operator' };
  return { end: pos + 1, type: 'punctuation' };
}

/** Every token of `text`: `[{from, to, type, text}]` (offsets into `text`, whitespace left out). */
export function gqlTokens(text) {
  const out = [];
  const state = gqlStartState();
  let offset = 0;
  for (const line of String(text ?? '').split('\n')) {
    gqlStartLine(state);
    let pos = 0;
    while (pos < line.length) {
      const { end, type } = scanGql(line, pos, state);
      const next = Math.max(end, pos + 1);
      if (type) out.push({ from: offset + pos, to: offset + next, type, text: line.slice(pos, next) });
      pos = next;
    }
    offset += line.length + 1;
  }
  return out;
}

/**
 * The parameters a GQL query reads, each once, in the order of their first `$name` (strings and
 * comments left out; `` $`a b` `` gives `a b`).
 */
export function gqlParamNames(text) {
  const names = [];
  for (const token of gqlTokens(text)) {
    if (token.type !== 'param') continue;
    let name = token.text.slice(1);
    if (name.startsWith('`')) name = name.slice(1, name.endsWith('`') ? -1 : undefined).replace(/``/g, '`');
    if (name && !names.includes(name)) names.push(name);
  }
  return names;
}
