// The query editor's "Wrap as GQL" action: a GQL query typed into the editor becomes the Rhai
// script that runs it, `g.gql(`...`)`. Pure: no CodeMirror, no DOM.
//
// A Rhai backtick string spans lines and keeps the text as it is, except that a backtick is
// written doubled (GQL back-quoted identifiers stay intact) and `${` starts an interpolation;
// a query holding `${` is written as a double-quoted string with escapes instead.

/** The query the action inserts into an empty editor. */
export const GQL_TEMPLATE = 'MATCH (p:person)-[:knows]->(f:person)\nRETURN p.name AS who, f.name AS friend\nORDER BY who, friend';

/** True when `text` (trimmed) already is a `g.gql(...)` call: the action leaves it alone. */
export function isGqlCall(text) {
  return /^g\.gql\s*\(/.test(String(text).trim());
}

/** A Rhai string literal holding `query` exactly: a backtick string, or `"..."` when it holds `${`. */
export function rhaiStringOf(query) {
  if (!query.includes('${')) return '`' + query.replace(/`/g, '``') + '`';
  let out = '"';
  for (const c of query) {
    if (c === '"') out += '\\"';
    else if (c === '\\') out += '\\\\';
    else if (c === '\n') out += '\\n';
    else if (c === '\r') out += '\\r';
    else if (c === '\t') out += '\\t';
    else out += c;
  }
  return out + '"';
}

/**
 * The script that runs the GQL query `text`: `g.gql(`query`)`, a multi-line query indented on
 * its own lines. Empty text gives the template; a `g.gql(...)` call comes back unchanged.
 */
export function wrapGql(text) {
  const raw = String(text ?? '');
  if (isGqlCall(raw)) return raw;
  const lines = raw.replace(/\r\n?/g, '\n').split('\n');
  // Drop leading and trailing blank lines and the common indentation.
  while (lines.length && !lines[0].trim()) lines.shift();
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  if (!lines.length) return wrapGql(GQL_TEMPLATE);
  const indent = Math.min(...lines.filter((l) => l.trim()).map((l) => l.length - l.trimStart().length));
  const query = lines.map((l) => l.slice(indent).trimEnd()).join('\n');
  const literal = rhaiStringOf(query);
  if (lines.length === 1 || literal.startsWith('"')) return `g.gql(${literal})`;
  return 'g.gql(`\n' + query.replace(/`/g, '``').split('\n').map((l) => (l ? '  ' + l : l)).join('\n') + '\n`)';
}
