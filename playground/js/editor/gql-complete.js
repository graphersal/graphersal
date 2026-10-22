// What to complete at a position of GQL text: the pure part of the GQL completion
// (js/editor/gql-completion.js turns it into CodeMirror options). No CodeMirror, no DOM
// (tests/gql-complete.test.mjs). It reads the text through the highlighter's tokenizer
// (js/editor/gql-tokens.js), so it agrees with the colours.
//
// Contexts:
//   param     after `$`: the parameter names (the tab's Parameters field, the declared
//             parameters of a saved query, the `$name`s the text already reads)
//   label     after `:` (and `|`, `&`, `!` in a label expression): vertex labels inside `( )`,
//             edge types inside `[ ]`, the variable's kind elsewhere (`WHERE n:person`)
//   property  after `var.`: the property keys of the variable's labels
//   key       a key of a property map in a pattern, `(n:person {na`: the keys of its labels
//   procedure after `CALL` (and inside its dotted name, `CALL people.ol`): the saved queries
//             written in GQL, as `folder.name` (gqlProcedureChoices)
//   word      anything else: keywords by position (the clause the cursor is in), functions
//             where an expression may stand, and the variables the query binds
// Inside a string, a comment or a back-quoted name there is nothing to complete (null).

import { gqlTokens, gqlParamNames } from './gql-tokens.js';

const WORDLIKE = new Set(['keyword', 'atom', 'variable', 'function', 'label', 'property', 'procedure']);
const QUIET = new Set(['string', 'comment', 'name']);

/** The keywords that start (or continue) a clause, for the clause detection. */
const CLAUSES = new Set([
  'MATCH', 'OPTIONAL', 'WHERE', 'RETURN', 'WITH', 'LET', 'FILTER', 'FOR', 'UNWIND', 'ORDER', 'BY',
  'SKIP', 'OFFSET', 'LIMIT', 'INSERT', 'CREATE', 'SET', 'REMOVE', 'DELETE', 'DETACH', 'NODETACH',
  'CALL', 'UNION', 'NEXT', 'MERGE', 'ON', 'FINISH', 'YIELD',
]);

const EXPRESSION = ['AND', 'OR', 'XOR', 'NOT', 'IS NULL', 'IS NOT NULL', 'IN', 'STARTS WITH', 'ENDS WITH', 'CONTAINS', 'EXISTS', 'CASE', 'TRUE', 'FALSE', 'NULL'];
const READ_NEXT = ['MATCH', 'OPTIONAL MATCH', 'WHERE', 'RETURN', 'WITH', 'LET', 'FILTER', 'FOR', 'UNWIND', 'CALL'];
const WRITE_NEXT = ['INSERT', 'CREATE', 'SET', 'REMOVE', 'DELETE', 'DETACH DELETE', 'FINISH'];
const RESULT_TAIL = ['ORDER BY', 'SKIP', 'OFFSET', 'LIMIT', 'UNION', 'UNION ALL', 'NEXT'];

/**
 * The keywords offered in each clause, the likely next ones first. `start` is the beginning of a
 * query (or of a subquery body).
 */
export const GQL_CLAUSE_KEYWORDS = Object.freeze({
  start: ['MATCH', 'OPTIONAL MATCH', 'INSERT', 'CREATE', 'RETURN', 'FOR', 'UNWIND', 'LET', 'WITH', 'FILTER', 'CALL'],
  MATCH: ['WHERE', 'RETURN', 'MATCH', 'OPTIONAL MATCH', 'WITH', 'LET', 'FILTER', 'FOR', 'UNWIND', 'CALL', ...WRITE_NEXT],
  WHERE: [...EXPRESSION, 'RETURN', 'WITH', 'MATCH', 'OPTIONAL MATCH', 'LET', 'FILTER', 'FOR', 'CALL', ...WRITE_NEXT],
  FILTER: [...EXPRESSION, 'RETURN', 'MATCH', 'OPTIONAL MATCH', 'WITH', 'LET', 'FILTER', ...WRITE_NEXT],
  RETURN: ['AS', 'DISTINCT', ...RESULT_TAIL, 'GROUP BY', ...EXPRESSION],
  WITH: ['AS', 'DISTINCT', 'WHERE', 'MATCH', 'OPTIONAL MATCH', 'RETURN', 'ORDER BY', 'SKIP', 'LIMIT', 'WITH', 'LET', 'FOR', 'UNWIND', 'CALL', ...WRITE_NEXT, ...EXPRESSION],
  LET: ['MATCH', 'RETURN', 'FILTER', 'LET', 'WITH', 'OPTIONAL MATCH', ...EXPRESSION],
  FOR: ['IN', 'AS', 'WITH ORDINALITY', 'MATCH', 'RETURN', 'WITH', 'FILTER', 'LET', ...WRITE_NEXT],
  ORDER: ['ASC', 'DESC', 'NULLS FIRST', 'NULLS LAST', 'SKIP', 'OFFSET', 'LIMIT', 'UNION', 'NEXT'],
  LIMIT: ['UNION', 'UNION ALL', 'NEXT'],
  INSERT: ['RETURN', 'INSERT', 'SET', 'MATCH', 'WITH', 'FINISH'],
  SET: ['SET', 'REMOVE', 'RETURN', 'DELETE', 'DETACH DELETE', 'WITH', 'FINISH', 'MATCH'],
  DELETE: ['RETURN', 'FINISH', 'WITH', 'MATCH'],
  MERGE: ['ON CREATE SET', 'ON MATCH SET', 'RETURN', 'SET', 'WITH', 'MERGE', 'FINISH'],
  CALL: ['YIELD', 'RETURN', 'MATCH', 'WITH', 'FILTER', 'LET', 'AS', 'WHERE', ...WRITE_NEXT],
  UNION: ['ALL', 'MATCH', 'RETURN', 'FOR', 'LET'],
});

/** The clause name a clause keyword belongs to (the key of GQL_CLAUSE_KEYWORDS). */
function clauseOf(word) {
  switch (word) {
    case 'OPTIONAL': return 'MATCH';
    case 'UNWIND': return 'FOR';
    case 'BY': case 'ORDER': return 'ORDER';
    case 'SKIP': case 'OFFSET': case 'LIMIT': return 'LIMIT';
    case 'CREATE': return 'INSERT';
    case 'REMOVE': return 'SET';
    case 'DETACH': case 'NODETACH': return 'DELETE';
    case 'NEXT': case 'FINISH': return 'start';
    case 'ON': return 'MERGE';
    case 'YIELD': return 'CALL';
    default: return GQL_CLAUSE_KEYWORDS[word] ? word : null;
  }
}

/** Clauses whose items are expressions: functions are offered there. */
const EXPRESSION_CLAUSES = new Set(['WHERE', 'FILTER', 'RETURN', 'WITH', 'LET', 'FOR', 'ORDER', 'SET', 'DELETE', 'MERGE', 'LIMIT']);

const upper = (t) => t.text.toUpperCase();
const isPunct = (t, c) => !!t && (t.type === 'punctuation' || t.type === 'operator') && t.text === c;

/**
 * The variables the query binds, in the order they first appear: `[{name, kind, labels}]` with
 * `kind` 'node' (`(n:person)`), 'edge' (`[e:knows]`), 'path' (`p = (..)`) or 'value' (`AS x`,
 * `FOR x IN`, `LET x =`), and the labels written on the pattern element.
 */
export function gqlVariables(text) {
  const tokens = gqlTokens(text);
  const out = new Map();
  // Inside `YIELD a [AS x], ..`: a yielded name is a new variable, unless `AS` renames it.
  let yielding = false;
  const add = (name, kind, labels = []) => {
    const v = out.get(name);
    if (!v) out.set(name, { name, kind, labels: [...new Set(labels)] });
    else {
      if (v.kind === 'value' && kind !== 'value') v.kind = kind;
      for (const l of labels) if (!v.labels.includes(l)) v.labels.push(l);
    }
  };
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (t.type === 'keyword') {
      const word = upper(t);
      if (word === 'YIELD') yielding = true;
      else if (word !== 'AS') yielding = false;
    }
    if (t.type !== 'variable') continue;
    const prev = tokens[i - 1];
    const next = tokens[i + 1];
    if (yielding && !(next && next.type === 'keyword' && upper(next) === 'AS')) {
      add(t.text, 'value');
      continue;
    }
    if (isPunct(prev, '(') || isPunct(prev, '[')) {
      // A pattern element: its labels follow `:` (and `|`, `&`, `!`) up to the closing bracket.
      const labels = [];
      for (let j = i + 1; j < tokens.length; j++) {
        const u = tokens[j];
        if (u.type === 'label') labels.push(u.text.replace(/^`|`$/g, ''));
        else if (!(u.type === 'punctuation' || u.type === 'operator') || ![':', '|', '&', '!'].includes(u.text)) break;
      }
      // `(x IN list ...)`: a comprehension local, `f(n)`: an argument, not a pattern element.
      if (next && next.type === 'keyword' && upper(next) === 'IN') add(t.text, 'value');
      else if (prev.text === '(' && tokens[i - 2] && tokens[i - 2].type === 'function') continue;
      else add(t.text, prev.text === '[' ? 'edge' : 'node', labels);
      continue;
    }
    if (prev && prev.type === 'keyword' && ['AS', 'FOR', 'LET'].includes(upper(prev))) {
      add(t.text, 'value');
      continue;
    }
    if (isPunct(next, '=') && prev && (prev.type === 'keyword' ? ['MATCH', 'OPTIONAL'].includes(upper(prev)) : isPunct(prev, ','))) {
      const after = tokens[i + 2];
      add(t.text, isPunct(after, '(') ? 'path' : 'value');
    }
  }
  return [...out.values()];
}

/**
 * What to complete at `pos` of `text`: null (nothing: inside a string or comment), or
 * `{kind, from, prefix, ...}`:
 *   {kind: 'param', from, prefix}                      from is the `$`
 *   {kind: 'label', from, prefix, element}             element 'vertex' | 'edge'
 *   {kind: 'property', from, prefix, variable}         after `variable.`
 *   {kind: 'key', from, prefix, element, labels}       a pattern's property map key
 *   {kind: 'procedure', from, prefix}                  a saved query's name after `CALL`; from
 *                                                      and prefix cover the whole dotted name
 *   {kind: 'word', from, prefix, clause, keywords, functions, variables}
 * `variables` (word) and the variable lookups come from `gqlVariables(text)`.
 */
export function gqlCompletionContext(text, pos) {
  const doc = String(text ?? '');
  const before = doc.slice(0, pos);
  const tokens = gqlTokens(before);
  let last = tokens[tokens.length - 1];
  if (last && last.to === before.length && QUIET.has(last.type)) return null;
  // An unclosed string or block comment reaching the cursor: the tokenizer gives it to the end.
  if (last && QUIET.has(last.type) && last.to >= before.length) return null;
  let prefix = '';
  let from = before.length;
  let body = tokens;
  if (last && last.to === before.length) {
    if (last.type === 'param') {
      return { kind: 'param', from: last.from, prefix: last.text };
    }
    if (WORDLIKE.has(last.type)) {
      prefix = last.text;
      from = last.from;
      body = tokens.slice(0, -1);
    }
  }
  if (!prefix && before.endsWith('$')) return { kind: 'param', from: before.length - 1, prefix: '$' };
  const prev = body[body.length - 1];
  // The name of a called saved query: right after `CALL`, or inside its dotted name.
  {
    let i = body.length - 1;
    let start = prefix ? from : before.length;
    let named = !!prefix && last.type === 'procedure';
    if (!prefix || named) {
      // Walk back over `segment .` pairs to the `CALL`.
      while (i >= 0 && isPunct(body[i], '.') && body[i - 1] && body[i - 1].type === 'procedure') {
        start = body[i - 1].from;
        i -= 2;
        named = true;
      }
      const call = body[i];
      if (call && call.type === 'keyword' && upper(call) === 'CALL' && (named || !prefix || last.type === 'procedure')) {
        return { kind: 'procedure', from: start, prefix: doc.slice(start, before.length) };
      }
    }
  }
  // The open brackets at the cursor.
  const stack = [];
  let clause = 'start';
  for (let i = 0; i < body.length; i++) {
    const t = body[i];
    if (t.type === 'punctuation') {
      if (t.text === '(' || t.text === '[' || t.text === '{') {
        stack.push({ c: t.text, i });
        // A subquery body starts a query of its own.
        continue;
      }
      if (t.text === ')' || t.text === ']' || t.text === '}') stack.pop();
      continue;
    }
    if (t.type === 'keyword') {
      const word = upper(t);
      const top = stack[stack.length - 1];
      // Clauses count at the top level and directly inside a `{ }` subquery body.
      if (CLAUSES.has(word) && (!top || top.c === '{')) {
        const c = clauseOf(word);
        if (c) clause = c;
      }
    }
  }
  const top = stack[stack.length - 1];
  const variables = gqlVariables(doc);
  const variable = (name) => variables.find((v) => v.name === name) || null;

  // A label after `:` (or `|`, `&`, `!` inside a label expression).
  const labelOp = prev && (prev.type === 'punctuation' || prev.type === 'operator') && [':', '|', '&', '!'].includes(prev.text);
  if (labelOp && (!top || top.c !== '{')) {
    const isLabelChain = prev.text === ':' || body.slice(0, -1).reverse().find((t) => t.type !== 'operator' || t.text !== '!')?.type === 'label' || prev.text === '!';
    if (isLabelChain) {
      let element = 'vertex';
      if (top && top.c === '[') element = 'edge';
      else if (!top || top.c !== '(') {
        // `WHERE e:knows`: the kind of the variable before the colon.
        const owner = body.slice(0, -1).reverse().find((t) => t.type === 'variable');
        if (owner && variable(owner.text) && variable(owner.text).kind === 'edge') element = 'edge';
      }
      return { kind: 'label', from, prefix, element };
    }
  }
  // A property after `var.`.
  if (isPunct(prev, '.') && body[body.length - 2] && body[body.length - 2].type === 'variable') {
    return { kind: 'property', from, prefix, variable: body[body.length - 2].text };
  }
  // A key of a pattern element's property map: `(n:person {na` / `{name: 'x', ag`.
  if (top && top.c === '{' && (isPunct(prev, '{') || isPunct(prev, ','))) {
    const outer = stack[stack.length - 2];
    const opener = body[top.i - 1];
    if (outer && (outer.c === '(' || outer.c === '[') && opener && (opener.type === 'label' || opener.type === 'variable' || isPunct(opener, '('))) {
      const labels = body.slice(outer.i + 1, top.i).filter((t) => t.type === 'label').map((t) => t.text.replace(/^`|`$/g, ''));
      return { kind: 'key', from, prefix, element: outer.c === '[' ? 'edge' : 'vertex', labels };
    }
  }
  const inPattern = !!top && (top.c === '(' || top.c === '[') && (clause === 'MATCH' || clause === 'INSERT' || clause === 'MERGE');
  let keywords = GQL_CLAUSE_KEYWORDS[clause] || GQL_CLAUSE_KEYWORDS.start;
  if (top && top.c === '{' && body[top.i - 1] && body[top.i - 1].type === 'keyword') keywords = [...GQL_CLAUSE_KEYWORDS.start, ...keywords];
  if (inPattern) keywords = ['WHERE'];
  else if (top && top.c !== '{') keywords = [...EXPRESSION, 'AS', 'DISTINCT'];
  const functions = !inPattern && (EXPRESSION_CLAUSES.has(clause) || (!!top && top.c !== '{'));
  return {
    kind: 'word',
    from,
    prefix,
    clause,
    keywords: [...new Set(keywords)],
    functions,
    variables: clause === 'start' && !top ? [] : variables,
  };
}

/**
 * The parameter names to offer after `$`: the given ones (a tab's Parameters field, a saved
 * query's declared parameters) first, then the `$name`s the text reads, each once.
 */
export function gqlParamChoices(text, given = []) {
  const names = [];
  for (const n of [...given, ...gqlParamNames(text)]) {
    const name = String(n || '').trim();
    if (name && !names.includes(name)) names.push(name);
  }
  return names;
}

/** `$name` as written in GQL: back-quoted when it is no plain identifier. */
export function paramToken(name) {
  return /^[\p{L}_][\p{L}\p{N}_]*$/u.test(name) ? `$${name}` : `$\`${name.replace(/`/g, '``')}\``;
}

/**
 * The property keys a schema declares, by element kind and label: `{vertex: {label: [keys]},
 * edge: {...}}` from the schema format (`{vertices: {label: {schema: {properties}}}, edges}`;
 * the stored or the inferred schema).
 */
export function keysBySchema(schema) {
  const out = { vertex: {}, edge: {} };
  if (!schema || typeof schema !== 'object') return out;
  for (const [kind, section] of [['vertex', schema.vertices], ['edge', schema.edges]]) {
    for (const [label, entry] of Object.entries(section || {})) {
      const props = entry && entry.schema && entry.schema.properties;
      out[kind][label] = props && typeof props === 'object' ? Object.keys(props) : [];
    }
  }
  return out;
}

/**
 * The keys to offer for an element: those of its labels, or of every label of its kind when it
 * has none (or none of them is known); `element` 'any' looks at both kinds. Sorted, each once.
 */
export function keysFor(keys, element, labels = []) {
  const kinds = element === 'any' ? ['vertex', 'edge'] : [element];
  const out = new Set();
  for (const kind of kinds) {
    const byLabel = (keys && keys[kind]) || {};
    const known = labels.filter((l) => byLabel[l]);
    for (const l of known.length ? known : Object.keys(byLabel)) for (const k of byLabel[l] || []) out.add(k);
  }
  return [...out].sort();
}

/** A saved query's folder segment or name as GQL writes it: back-quoted when no identifier. */
function procedureSegment(text) {
  return /^[\p{L}_][\p{L}\p{N}_]*$/u.test(text) ? text : '`' + text.replace(/`/g, '``') + '`';
}

/**
 * The saved queries a GQL `CALL` can call, from `listQueries()` entries: those written in GQL
 * whose status is not "error", as `{text, name, folder, params}` with `text` the dotted name
 * (`people.older`: the folder's segments, then the name; back-quoted where needed), sorted.
 * @param {Array<{name: string, folder?: ?string, language?: string, status?: string, params?: Array<{name: string}>}>} list
 */
export function gqlProcedureChoices(list) {
  const out = [];
  for (const entry of list || []) {
    if (!entry || typeof entry.name !== 'string' || entry.language !== 'gql' || entry.status === 'error') continue;
    const folder = entry.folder ? String(entry.folder).split('/').filter((x) => x !== '') : [];
    const text = [...folder, entry.name].map(procedureSegment).join('.');
    out.push({ text, name: entry.name, folder: entry.folder || '', params: (entry.params || []).map((p) => p && p.name).filter(Boolean) });
  }
  return out.sort((a, b) => (a.text < b.text ? -1 : a.text > b.text ? 1 : 0));
}
