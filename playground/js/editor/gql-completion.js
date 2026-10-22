// Autocompletion of GQL text (a query tab in GQL, the saved-query editor's GQL mode): keywords by
// position, the GQL functions with a short text (js/editor/gql-functions.js), labels, edge types
// and property keys of the live graph, the variables the query binds and the `$parameters`;
// after `CALL` the saved queries written in GQL (`getQueries()`, the catalog's list the
// playground already loads: `state.savedQueries`).
// The context comes from the pure js/editor/gql-complete.js; this file only builds the
// CodeMirror options.
//
// The graph's vocabulary (`getVocabulary()`): `{vertexLabels, edgeLabels, keys}` where `keys` is
// `keysBySchema(..)` of the stored schema or, without one, the inferred schema (store.js
// `gqlVocabulary`, loaded on first use and after every change of the data). It may also be a
// promise of it (the first load): a request that needs it then waits for it.

import { gqlCompletionContext, gqlParamChoices, gqlVariables, gqlProcedureChoices, paramToken, keysFor } from './gql-complete.js';
import { GQL_FUNCTIONS } from './gql-functions.js';

function docNode(fn, spelling) {
  const root = document.createElement('div');
  root.className = 'completion-doc';
  const h = document.createElement('h5');
  h.textContent = spelling + fn.call;
  root.appendChild(h);
  const others = [fn.name, ...fn.aliases].filter((n) => n !== spelling);
  if (others.length) {
    const a = document.createElement('div');
    a.className = 'aliases';
    a.textContent = 'also: ' + others.join(', ');
    root.appendChild(a);
  }
  const p = document.createElement('p');
  p.textContent = fn.doc;
  root.appendChild(p);
  const cat = document.createElement('div');
  cat.className = 'cat';
  cat.textContent = fn.kind === 'aggregate' ? 'GQL aggregate' : fn.kind === 'list' ? 'list predicate (Cypher)' : 'GQL function';
  root.appendChild(cat);
  return root;
}

/** Inserts `name(` and puts the cursor inside the parentheses (`(` already there: only the name). */
function applyCall(spelling) {
  return (view, completion, from, to) => {
    if (view.state.sliceDoc(to, to + 1) === '(') {
      view.dispatch({ changes: { from, to, insert: spelling }, selection: { anchor: from + spelling.length + 1 } });
      return;
    }
    view.dispatch({ changes: { from, to, insert: spelling + '()' }, selection: { anchor: from + spelling.length + 1 } });
  };
}

const functionOptions = () => GQL_FUNCTIONS.flatMap((fn) => [fn.name, ...fn.aliases].map((spelling) => ({
  label: spelling,
  type: 'function',
  detail: fn.kind === 'aggregate' ? 'aggregate' : 'function',
  info: () => docNode(fn, spelling),
  apply: applyCall(spelling),
  boost: spelling === fn.name ? 0 : -1,
})));

/** A name as GQL writes it: back-quoted when it is no plain identifier. */
function nameText(name) {
  return /^[\p{L}_][\p{L}\p{N}_]*$/u.test(name) ? name : '`' + name.replace(/`/g, '``') + '`';
}

/**
 * The CodeMirror completion source for GQL.
 * @param {() => ({vertexLabels?: string[], edgeLabels?: string[], keys?: object}|Promise<object>|null)} getVocabulary
 * @param {() => Array<string|{name: string, detail?: string}>} [getParams]  the parameters to offer
 *   after `$` first (a tab's Parameters field, a saved query's declared parameters)
 * @param {() => Array<object>|null} [getQueries]  the saved queries (`engine.listQueries()`
 *   entries), offered after `CALL`
 */
export function gqlCompletionSource(getVocabulary, getParams = () => [], getQueries = () => []) {
  return (context) => {
    const text = context.state.doc.toString();
    const ctx = gqlCompletionContext(text, context.pos);
    if (!ctx) return null;
    if (ctx.kind === 'procedure') return procedureResult(ctx, getQueries());
    if (ctx.kind === 'param') {
      const given = (getParams() || []).map((p) => (typeof p === 'string' ? { name: p } : p));
      const details = new Map(given.map((p) => [p.name, p.detail]));
      // The `$name` being typed is no name the query reads yet.
      const names = gqlParamChoices(text.slice(0, ctx.from) + text.slice(context.pos), given.map((p) => p.name));
      if (!names.length) return null;
      return {
        from: ctx.from,
        options: names.map((name, i) => ({
          label: paramToken(name),
          type: 'variable',
          detail: details.get(name) || (i < given.length ? 'parameter' : 'read by the query'),
          boost: details.has(name) ? 2 : 0,
        })),
        validFor: /^\$[\p{L}\p{N}_]*$/u,
      };
    }
    // Without a typed prefix, open only after `:`, `.`, `$` and in a property map (or on Ctrl+Space).
    if (!ctx.prefix && !context.explicit && ctx.kind === 'word') return null;
    if (ctx.kind === 'word') return wordResult(ctx);
    const vocab = getVocabulary();
    if (vocab && typeof vocab.then === 'function') return vocab.then((v) => graphResult(ctx, text, v || {}));
    return graphResult(ctx, text, vocab || {});
  };
}

/** The saved queries in GQL after `CALL`: `folder.name(` with the parameters in the detail. */
function procedureResult(ctx, queries) {
  const choices = gqlProcedureChoices(queries);
  if (!choices.length) return null;
  return {
    from: ctx.from,
    options: choices.map((q) => ({
      label: q.text,
      type: 'function',
      detail: `saved query (${q.params.join(', ')})`,
      apply: applyCall(q.text),
    })),
    validFor: /^[\p{L}\p{N}_.`]*$/u,
  };
}

/** Labels, edge types and property keys from the graph's vocabulary. */
function graphResult(ctx, text, vocab) {
  if (ctx.kind === 'label') {
    const labels = ctx.element === 'edge' ? vocab.edgeLabels || [] : vocab.vertexLabels || [];
    // Nothing known (yet): no result, so the next keystroke asks again.
    if (!labels.length) return null;
    return {
      from: ctx.from,
      options: labels.map((l) => ({ label: nameText(l), type: 'type', detail: ctx.element === 'edge' ? 'edge type' : 'label' })),
      validFor: /^[\p{L}\p{N}_]*$/u,
    };
  }
  let element = ctx.element || 'any';
  let labels = ctx.labels || [];
  if (ctx.kind === 'property') {
    const v = ctx.variable && gqlVariablesCache(text).find((x) => x.name === ctx.variable);
    element = v && v.kind === 'edge' ? 'edge' : v && v.kind === 'node' ? 'vertex' : 'any';
    labels = v ? v.labels : [];
  }
  const keys = keysFor(vocab.keys, element, labels);
  if (!keys.length) return null;
  return {
    from: ctx.from,
    options: keys.map((k) => ({ label: nameText(k), type: 'property', detail: labels.length ? labels.join('|') : 'property' })),
    validFor: /^[\p{L}\p{N}_]*$/u,
  };
}

/** Keywords (the likely ones first), the query's variables and, in expressions, the functions. */
function wordResult(ctx) {
  const options = [
    ...ctx.keywords.map((k, i) => ({ label: k, type: 'keyword', boost: Math.max(-1, 2 - Math.floor(i / 4)) })),
    ...ctx.variables.map((v) => ({
      label: nameText(v.name),
      type: 'variable',
      detail: v.labels.length ? `${v.kind} :${v.labels.join('|')}` : v.kind,
      boost: 3,
    })),
    ...(ctx.functions ? functionOptions() : []),
  ];
  return { from: ctx.from, options, validFor: /^[\p{L}\p{N}_]*$/u };
}

// The variables of the last text asked for (a completion asks once per keystroke).
let cached = { text: null, vars: [] };
function gqlVariablesCache(text) {
  if (cached.text !== text) cached = { text, vars: gqlVariables(text) };
  return cached.vars;
}
