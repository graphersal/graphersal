// The profile API (EXPERIMENTAL: it may change in any release, like graphersal-serve): what a
// UI profile's own tab modules get (js/ui-profiles.js `tabs: [{id, title, module}]`).
//
// A module tab is an ES module whose default export is a Vue component (an options object with
// `template` or `render`, or a `setup` returning a render function; Vue is the page's own, import
// it with `import { ref, h } from 'vue'`). The page loads it with
// `defineAsyncComponent(() => import(files + module))` and renders it with two props:
//   api     this object (one per tab);
//   params  the tab's parameters from the address (`#/play?tab=<id>&k=v`: `{k: 'v'}`, strings),
//           reactive: a `navigate` to the same tab with other parameters updates them.
// A module that fails to load, has no default export or throws while rendering shows an error
// card in its tab; the rest of the page keeps working.
//
// The API (`api.version` 1):
//   query(name, params = {}) -> Promise<Array>
//       Calls the saved query `name` with the named arguments `params` (a JSON object: every
//       parameter the query declares that has no default, `project` included) through the
//       current engine, under the engine's rules (the dev server's read-only mode, its access
//       policy). Resolves to the query's results (what `to_list()` returns, as the engine's
//       JSON values); rejects with a ProfileQueryError ({message, help}) when the engine
//       refuses or the query fails. On the dev server the call is `POST /api/query` {name,
//       params} (the only query endpoint of a server in allow-list mode, which runs only its
//       selected saved queries under its access policy); on WebAssembly the worker's
//       `runSavedQuery`; the server binds no parameter.
//   markdown(text) -> VNode
//       The text rendered by the playground's safe Markdown renderer (never HTML: links are
//       checked, raw HTML is shown as text). Use it in a render function or as
//       `<component :is="api.markdown(text)" />`.
//   card(label, row, options = {}) -> VNode
//       The card of `row` (an element, a map of fields or a value, as `query` returns them) by
//       the profile's card template for `label` (`cards` in the profile; the format is
//       js/profile-cards.js); without a template, the label and a plain list of the row's
//       fields. Options (all optional; an unknown one throws a TypeError):
//         variant   'full' (default: the opened card with its Markdown body and fields) or
//                   'compact' (a list row: title, subtitle, badges, a one-line body excerpt);
//         selected  true marks the card as the current one (accent border, aria-current);
//         onOpen    (row) => void: the title becomes a button and a click on the card opens it
//                   (typically: show the full card of a compact row);
//         actions   [{id, text, kind?, when?, unless?, title?, disabled?}]: the buttons, in
//                   place of the template's `actions` (kind 'default' | 'primary' | 'danger';
//                   `when`/`unless` are field refs of the row);
//         onAction  (id, row) => void | Promise: called on a button's click. Without it the
//                   card shows no buttons. While a returned promise is pending the card's
//                   buttons are disabled; a rejection is shown as an error toast.
//       Missing fields render nothing; nothing of the row is ever rendered as HTML.
//   graph(data, options = {}) -> VNode
//       The graph view of `data` = {nodes: [{id, label, caption?, data?, classes?}], edges: [{id,
//       source, target, label, caption?, data?, classes?}]} (typically built from saved queries;
//       the format and its checks are js/profile-graph.js), drawn with the Graph panel's drawing
//       code (Cytoscape; sigma.js on WebGL for large views) and styled by the profile's `graph`
//       key per label plus the module's per-element styles. Render it like a card (in a render
//       function, or `<component :is="api.graph(data, options)" />`); keep the data in a ref and
//       replace it to change the drawing (vertices it had keep their positions). Options (all
//       optional; an unknown one throws a TypeError; js/profile-graph.js `checkGraphOptions`):
//         styleOf   (element, kind) => {classes?, ...style} | null: per-element style on top of
//                   the profile's (`classes` names: the profile's `graph.classes` or the
//                   built-in `faded`, `bold`, `dashed`);
//         onSelect  (selection) => void: a click chose {kind, id, label, data, element} or
//                   empty space (null);
//         onExpand  (node) => void | Promise: a double click on a node (or the side card's
//                   Expand button): merge its neighbours in with `api.mergeGraph`;
//         selected  {kind: 'node'|'edge', id} | null: the selection set from outside;
//         card      false (default) | true | {actions, onAction, expand}: the side card of the
//                   selected element through `api.card(label, element.data, ...)` (`actions` a
//                   list or (selection) => list; `onAction(id, selection)`; `expand` the Expand
//                   button's text, false for none, or (selection) => text | false per node);
//         legend    true (default: the labels drawn and the profile's `graph.legend`) | false |
//                   extra entries [{text, node: style} | {text, edge: style}];
//         layout    'cose' (default) | 'circle' | 'concentric' | 'breadthfirst' | 'grid';
//         height    the drawing's CSS height (default '520px');
//         highlight 'neighbours' (default) | 'element' | 'none': what a selection highlights;
//         emptyText the text of a view without nodes.
//       Invalid data shows its problems in the view (GraphDataError); the graph itself never
//       writes: a module writes through `query` from its card actions.
//   mergeGraph(base, addition) -> {graph, added: {nodes, edges}}
//       The union of two graph datas without duplicates (an element in both keeps its place and
//       takes `addition`'s content); `added` lists the new ids. Throws a GraphDataError for
//       invalid data.
//   toast(kind, text)
//       A notification: kind 'info' | 'success' | 'warn' | 'error'.
//   navigate(tab, params = {})
//       Shows the profile tab `tab` with `params` (strings; numbers and booleans are written as
//       text) and puts both into the address (a reload or the Back button returns there).
//       Throws for an unknown tab or a reserved parameter name.
//   theme
//       {mode, dark, token(name)}: the theme setting ('system' | 'light' | 'dark'), whether the
//       page is dark now, and the value of the CSS colour token `--<name>` (e.g. `token('accent')`,
//       `token('muted')`; '' when unknown). Prefer the tokens in your CSS (`var(--accent)`): they
//       follow the theme by themselves.
//   info
//       {profile: {name, title}, server: boolean, version: string|null, readOnly: boolean,
//       engine: Object}: the profile, whether the engine is the dev server (else the
//       WebAssembly playground), the engine's version and read-only flag, and the engine's whole
//       `info()` answer (a frozen copy; fields may be added).
// Pure: every dependency is injected (`createProfileApi`), so tests/profile-api.test.mjs runs
// it under Node; components/profile-tab.js wires it to the store and Vue.

import { checkCardOptions } from './profile-cards.js';
import { checkGraphOptions, mergeGraph } from './profile-graph.js';

/** The version of the profile API; a change that breaks a module raises it. */
export const PROFILE_API_VERSION = 1;

/** The kinds `api.toast` takes. */
export const TOAST_KINDS = Object.freeze(['info', 'success', 'warn', 'error']);

/** A refused or failed `api.query`: the engine's message and, when it has one, its help text. */
export class ProfileQueryError extends Error {
  constructor(message, help = '', name = '') {
    super(message);
    this.name = 'ProfileQueryError';
    this.help = help;
    this.query = name;
  }
}

const isPlainObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

function freezeCopy(value) {
  if (value === undefined) return null;
  const copy = JSON.parse(JSON.stringify(value));
  const freeze = (v) => {
    if (v && typeof v === 'object') {
      Object.values(v).forEach(freeze);
      Object.freeze(v);
    }
    return v;
  };
  return freeze(copy);
}

/**
 * The profile API of one tab.
 * @param {Object} deps
 * @param {{name: string, title: string}} deps.profile
 * @param {Object} deps.engineInfo        The engine's `info()` answer.
 * @param {boolean} deps.server           Whether the engine is the dev server.
 * @param {(name: string, params: Object) => Promise<{ok: boolean, results?: Array, error?: {message: string, help?: string}}>} deps.runQuery
 * @param {(text: string) => *} deps.renderMarkdown  A VNode of the safe Markdown renderer.
 * @param {(label: string, row: *, options: Object) => *} deps.renderCard  A VNode of the card of
 *   `row`; `options` checked by js/profile-cards.js `checkCardOptions`.
 * @param {(data: *, options: Object) => *} [deps.renderGraph]  A VNode of the graph view of
 *   `data`; `options` checked by js/profile-graph.js `checkGraphOptions`.
 * @param {(text: string, kind: string) => void} deps.showToast
 * @param {(tab: string, params: Object) => void} deps.navigate  Throws for an unknown tab.
 * @param {() => {mode: string, dark: boolean}} deps.themeState
 * @param {(name: string) => string} deps.cssToken  The value of the CSS custom property `--name`.
 */
export function createProfileApi(deps) {
  const info = Object.freeze({
    profile: Object.freeze({ name: deps.profile.name, title: deps.profile.title }),
    server: !!deps.server,
    version: (deps.engineInfo && deps.engineInfo.version) || null,
    readOnly: !!(deps.engineInfo && deps.engineInfo.readOnly),
    engine: freezeCopy(deps.engineInfo || {}),
  });
  const theme = Object.freeze({
    get mode() {
      return deps.themeState().mode;
    },
    get dark() {
      return deps.themeState().dark;
    },
    token(name) {
      if (typeof name !== 'string' || !/^[a-z][a-z0-9-]*$/.test(name)) return '';
      return String(deps.cssToken(name) || '').trim();
    },
  });
  return Object.freeze({
    version: PROFILE_API_VERSION,
    info,
    theme,
    async query(name, params = {}) {
      if (typeof name !== 'string' || !name.trim()) throw new ProfileQueryError('api.query: the saved query\'s name must be a non-empty text.');
      if (!isPlainObject(params)) throw new ProfileQueryError(`api.query("${name}"): the parameters must be an object of named arguments.`, '', name);
      let answer;
      try {
        answer = await deps.runQuery(name, params);
      } catch (err) {
        throw new ProfileQueryError(err && err.message ? err.message : String(err), (err && err.help) || '', name);
      }
      if (!answer || !answer.ok) {
        const error = (answer && answer.error) || {};
        throw new ProfileQueryError(error.message || `The saved query "${name}" failed.`, error.help || '', name);
      }
      return Array.isArray(answer.results) ? answer.results : [];
    },
    markdown(text) {
      return deps.renderMarkdown(text === undefined || text === null ? '' : String(text));
    },
    card(label, row, options) {
      const checked = checkCardOptions(options);
      return deps.renderCard(String(label === undefined || label === null ? '' : label), row, checked);
    },
    graph(data, options) {
      const checked = checkGraphOptions(options);
      if (typeof deps.renderGraph !== 'function') throw new Error('api.graph: this page has no graph view.');
      return deps.renderGraph(data, checked);
    },
    mergeGraph(base, addition) {
      return mergeGraph(base, addition);
    },
    toast(kind, text) {
      if (!TOAST_KINDS.includes(kind)) throw new TypeError(`api.toast: the kind must be one of ${TOAST_KINDS.join(', ')}.`);
      deps.showToast(String(text === undefined || text === null ? '' : text), kind);
    },
    navigate(tab, params = {}) {
      if (!isPlainObject(params)) throw new TypeError('api.navigate: the parameters must be an object.');
      deps.navigate(tab, params);
    },
  });
}
