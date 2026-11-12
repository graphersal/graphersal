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
//       refuses or the query fails. Today the call runs as the script `g.query(name, #{..})`;
//       on a server with the saved-query endpoint it becomes `POST /api/query` {name, params}
//       with the same answer.
//   markdown(text) -> VNode
//       The text rendered by the playground's safe Markdown renderer (never HTML: links are
//       checked, raw HTML is shown as text). Use it in a render function or as
//       `<component :is="api.markdown(text)" />`.
//   card(label, row) -> VNode
//       The card of `row` (an element or a map of fields) by the profile's card template for
//       `label` (`cards` in the profile); without a template, a plain list of the row's fields.
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
 * @param {(label: string, row: *) => *} deps.renderCard  A VNode of the card of `row`.
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
    card(label, row) {
      return deps.renderCard(String(label === undefined || label === null ? '' : label), row);
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
