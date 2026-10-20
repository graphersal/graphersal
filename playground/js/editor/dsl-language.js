// Syntax highlighting for the Graphersal DSL (Rhai with Gremlin-style method chains), as a
// CodeMirror 6 StreamLanguage: comments, strings, numbers, Rhai keywords, token classes
// (P, T, Order, ...; the engine's own classes once loaded), and step names after '.' or '::'.

import { StreamLanguage, HighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { tags as t } from '@lezer/highlight';

const KEYWORDS = new Set([
  'let', 'const', 'fn', 'if', 'else', 'for', 'in', 'while', 'loop', 'do', 'until', 'break',
  'continue', 'return', 'throw', 'try', 'catch', 'switch', 'import', 'export', 'as', 'private',
]);

// Global names of the DSL scope: the sources and the token classes (a fixed fallback plus the
// classes the engine reports, see token-classes.js).
import { TOKEN_CLASSES } from './token-classes.js';

export { TOKEN_CLASSES };

const ATOMS = new Set(['true', 'false', 'null']);

function token(stream, state) {
  if (state.inComment) {
    if (stream.skipTo('*/')) {
      stream.match('*/');
      state.inComment = false;
    } else {
      stream.skipToEnd();
    }
    return 'comment';
  }
  if (stream.eatSpace()) return null;
  if (stream.match('//')) {
    stream.skipToEnd();
    state.afterDot = false;
    return 'comment';
  }
  if (stream.match('/*')) {
    state.inComment = true;
    return 'comment';
  }
  const ch = stream.peek();
  if (ch === '"' || ch === '`' || ch === "'") {
    stream.next();
    let escaped = false;
    let c;
    while ((c = stream.next()) != null) {
      if (c === ch && !escaped) break;
      escaped = !escaped && c === '\\';
    }
    state.afterDot = false;
    return 'string';
  }
  if (stream.match(/^\d[\d_]*(\.\d[\d_]*)?([eE][-+]?\d+)?/)) {
    state.afterDot = false;
    return 'number';
  }
  const word = stream.match(/^[A-Za-z_][A-Za-z0-9_]*/);
  if (word) {
    const w = word[0];
    if (state.afterDot) {
      state.afterDot = false;
      return 'propertyName';
    }
    if (KEYWORDS.has(w)) return 'keyword';
    if (ATOMS.has(w)) return 'atom';
    if (TOKEN_CLASSES.has(w)) return 'typeName';
    return 'variableName';
  }
  if (stream.match('::') || stream.eat('.')) {
    state.afterDot = true;
    return 'punctuation';
  }
  stream.next();
  state.afterDot = false;
  return /[-+*/%=<>!&|^?]/.test(stream.current()) ? 'operator' : 'punctuation';
}

export const dslLanguage = StreamLanguage.define({
  name: 'graphersal-dsl',
  startState: () => ({ afterDot: false, inComment: false }),
  token,
  languageData: {
    commentTokens: { line: '//', block: { open: '/*', close: '*/' } },
    closeBrackets: { brackets: ['(', '[', '{', '"'] },
  },
});

/** Colours come from CSS custom properties, so light/dark switching needs no reconfiguration. */
export const dslHighlight = syntaxHighlighting(HighlightStyle.define([
  { tag: t.keyword, color: 'var(--syn-keyword)', fontWeight: '600' },
  { tag: t.propertyName, color: 'var(--syn-step)' },
  { tag: t.typeName, color: 'var(--syn-token)', fontWeight: '600' },
  { tag: t.string, color: 'var(--syn-string)' },
  { tag: t.number, color: 'var(--syn-number)' },
  { tag: t.atom, color: 'var(--syn-keyword)' },
  { tag: t.comment, color: 'var(--syn-comment)', fontStyle: 'italic' },
  { tag: t.variableName, color: 'var(--syn-var)' },
  // GQL `$name` parameters (js/editor/gql-language.js).
  { tag: t.special(t.variableName), color: 'var(--syn-number)', fontWeight: '600' },
  { tag: [t.operator, t.punctuation], color: 'var(--text-muted)' },
]));
