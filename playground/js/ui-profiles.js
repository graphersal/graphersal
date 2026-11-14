// UI profiles (EXPERIMENTAL): what one playground code base shows. A profile names the page's
// title, its tabs (the built-in workspace and/or tabs of its own, ES modules of the profile),
// the tab it starts on and what the user can reach (`can`: the top-bar buttons, the panels, the
// Query tab kinds). The built-in `full` profile is the playground as it always was; `demo` is a
// small profile for the tests and the smoke check. graphersal-memory's `memory` profile is a
// profile object its host delivers.
//
// Delivery (store.js `init`, router.js): the dev server's `GET /api/info` `uiProfile` (a
// built-in name or a profile object; `graphersal --server --ui-profile NAME|FILE`, a host's
// `ContextBuilder::ui_profile`), the WebAssembly playground's link parameter
// `#/play?profile=<name>` (built-in names only). Without either: `full`. An unknown name or an
// invalid object is an error the page shows, never another profile.
//
// The profile object (validated by `validateProfile`; unknown keys are errors, so a typo is
// never ignored):
//   name      [a-z][a-z0-9-]*, at most 40 characters
//   title     the page title and the top bar's brand (1..80 characters)
//   version   optional: the version of the product the profile belongs to (1..40 characters,
//             e.g. "0.1.0"); the top bar then shows it (`v0.1.0`) beside the brand instead of
//             the engine's version, which moves into its tooltip. Absent: the engine's version
//             as always.
//   startTab  the id of the tab the page opens on
//   can       {key: value}: what the user can reach. Keys: CAN_KEYS. A value is true, false,
//             'wasm' (only with the WebAssembly engine) or 'server' (only with the dev server);
//             a missing key is false. An engine capability still applies on top (the MCP badge
//             needs an MCP endpoint, the Store menu a store, ...).
//   tabs      [{id, title, builtin: 'workspace'} | {id, title, module: '<path>.js'}], 1..12
//             tabs, ids [a-z][a-z0-9-]* and unique; at most one builtin tab. A module tab's
//             module is loaded with `import(files + module)`: it default-exports a Vue
//             component that gets the props `api` (js/profile-api.js) and `params`.
//   header    optional: {module: '<path>.js'}, a module of the profile rendered in the top bar
//             (right of the spacer, left of the top bar's buttons): loaded like a module tab's
//             module, it default-exports a Vue component that gets the prop `api` (no
//             `params`); one per page, shown on every tab (a choice that affects every tab,
//             such as a project filter, belongs here rather than inside each tab).
//   cards     {<label>: card template}: the cards of `api.card(label, row, options)`; the
//             template format and its checks are js/profile-cards.js (an object, may be empty)
//   graph     the styles of `api.graph` views per label, classes and legend entries: the
//             format and its checks are js/profile-graph.js (an object, may be empty)
//   files     the base path of the module files, '' (the page's own directory) or a path
//             ending in '/' (relative to the page, or absolute on its origin); never another
//             origin, never '..'.
// Pure: no DOM, no Vue; tested in tests/ui-profiles.test.mjs.

import { cardsProblems } from './profile-cards.js';
import { graphSpecProblems } from './profile-graph.js';

/** The keys of `can`, grouped as the UI shows them. */
export const CAN_GROUPS = Object.freeze({
  // The top bar, left to right.
  topbar: Object.freeze([
    'devBadge', // the "DEV SERVER · unstable" badge
    'mcp', // the MCP badge and its usage dialog (needs an MCP endpoint)
    'share', // Share link (top bar and the tab menu's "Share as link…")
    'view', // View ▾ (show, hide and collapse panels)
    'catalog', // Catalog ▾
    'optimize', // Optimize
    'store', // the Store menu (needs a store)
    'save', // Save ▾
    'saveToFile', // Save to file (needs a graph file the server can write back)
    'changeGraph', // Change graph (the WebAssembly playground's graph choice)
    'settings', // ⚙ Settings
    'theme', // the theme switch
    'help', // ? Help and keyboard shortcuts
  ]),
  // The workspace's panels (a profile without the workspace tab shows none of them).
  panels: Object.freeze(['panel.editor', 'panel.results', 'panel.graph', 'panel.schema', 'panel.profile']),
  // The Query panel's own parts.
  query: Object.freeze([
    'examples', // the Examples drawer (written for the sample graphs)
    'tab.query', // Query tabs (Rhai)
    'tab.gql', // GQL query tabs
    'tab.ai', // AI Chat tabs (work with the dev server's --ai)
  ]),
});

/** Every key of `can`. */
export const CAN_KEYS = Object.freeze([...CAN_GROUPS.topbar, ...CAN_GROUPS.panels, ...CAN_GROUPS.query]);

/** The values a `can` key may take. */
export const CAN_VALUES = Object.freeze([true, false, 'wasm', 'server']);

/** The built-in tabs a profile may name (`{builtin: ...}`). */
export const BUILTIN_TABS = Object.freeze(['workspace']);

/** The most tabs a profile may have. */
export const MAX_TABS = 12;

const NAME = /^[a-z][a-z0-9-]{0,39}$/;
const PROFILE_KEYS = Object.freeze(['name', 'title', 'version', 'startTab', 'can', 'tabs', 'header', 'cards', 'graph', 'files']);
const TAB_KEYS = Object.freeze(['id', 'title', 'builtin', 'module']);
const HEADER_KEYS = Object.freeze(['module']);

const allCan = (value) => Object.freeze(Object.fromEntries(CAN_KEYS.map((key) => [key, value])));

/**
 * The built-in profiles. `full` is today's playground: the whole workspace, with the parts the
 * WebAssembly playground and the dev server each had (Share, Examples and Change graph only
 * with WebAssembly; the DEV SERVER badge, the MCP badge and Save to file only with the dev
 * server).
 */
export const BUILTIN_PROFILES = deepFreeze({
  full: {
    name: 'full',
    title: 'Graphersal Playground',
    startTab: 'play',
    can: {
      ...allCan(true),
      devBadge: 'server',
      mcp: 'server',
      share: 'wasm',
      saveToFile: 'server',
      changeGraph: 'wasm',
      examples: 'wasm',
    },
    tabs: [{ id: 'play', title: 'Playground', builtin: 'workspace' }],
    cards: {},
    graph: {},
    files: '',
  },
  // A small profile for the tests and the smoke check: the workspace without the catalog,
  // optimizer, store, share, examples and AI Chat, plus two tabs of its own that call the
  // profile API (js/profiles/demo/: cards in Hello, the graph view in Graph) and a top-bar module.
  demo: {
    name: 'demo',
    title: 'Graphersal Demo',
    startTab: 'hello',
    can: {
      view: true,
      save: true,
      changeGraph: 'wasm',
      devBadge: 'server',
      settings: true,
      theme: true,
      help: true,
      'panel.editor': true,
      'panel.results': true,
      'panel.graph': true,
      'tab.query': true,
      'tab.gql': true,
    },
    tabs: [
      { id: 'hello', title: 'Hello', module: 'hello.js' },
      { id: 'graph', title: 'Graph', module: 'graph.js' },
      { id: 'play', title: 'Playground', builtin: 'workspace' },
    ],
    // The top-bar module (js/profiles/demo/header.js): the engine's kind and a Graph button.
    header: { module: 'header.js' },
    // The person card of the Hello tab (js/profile-cards.js): `senior` and `about` are fields
    // the tab adds to each row of its saved query; `city` is never there (renders nothing).
    cards: {
      person: {
        title: 'name',
        subtitle: [{ field: 'age', prefix: 'age ' }, 'city'],
        badges: [{ field: 'senior', text: '30+', tone: 'accent' }, { field: 'city', tone: 'ok' }],
        body: 'about',
        fields: [{ field: 'name', title: 'Name' }, { field: 'age', title: 'Age' }, { field: 'city', title: 'City' }],
        actions: [{ id: 'hello', text: 'Say hello', kind: 'primary' }],
      },
    },
    // The Graph tab's styles (js/profile-graph.js): people and software per label, the two edge
    // labels, a legend entry for the faded class the tab sets while a person is expanded.
    graph: {
      nodes: {
        person: { shape: 'ellipse', caption: 'name' },
        software: { shape: 'round-rectangle', color: '--warn', size: 28 },
      },
      edges: {
        knows: { color: '--accent', width: 2 },
        created: { style: 'dashed' },
      },
      legend: [{ text: 'not next to the expanded person', node: { opacity: 0.3 } }],
    },
    files: 'js/profiles/demo/',
  },
});

/** The names of the built-in profiles. */
export const BUILTIN_NAMES = Object.freeze(Object.keys(BUILTIN_PROFILES));

function deepFreeze(value) {
  if (value && typeof value === 'object') {
    Object.values(value).forEach(deepFreeze);
    Object.freeze(value);
  }
  return value;
}

/** A profile that cannot be used: `problems` lists every reason, `message` says them in one text. */
export class ProfileError extends Error {
  constructor(subject, problems) {
    super(`${subject}: ${problems.join('; ')}.`);
    this.name = 'ProfileError';
    this.problems = problems;
  }
}

const isPlainObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const quote = (v) => JSON.stringify(String(v).slice(0, 60));

/**
 * Whether `path` is a safe relative or same-origin absolute path: no scheme, no `//` start, no
 * backslash, no `.`/`..` segment, no query or fragment.
 */
function isSafePath(path) {
  if (typeof path !== 'string') return false;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(path) || path.startsWith('//')) return false;
  if (/[\\?#\0]/.test(path) || /\s/.test(path)) return false;
  return !path.split('/').some((segment) => segment === '..' || segment === '.');
}

/** Whether `module` is a module file path below `files`: relative, a .js/.mjs file, safe. */
const isModulePath = (module) => typeof module === 'string' && /\.m?js$/.test(module) && !module.startsWith('/') && isSafePath(module);

/**
 * The problems of a profile's `header` key (`{module: '<path>.js'}`): an empty list when it is
 * valid. Unknown keys are problems.
 * @param {*} header
 * @returns {string[]}
 */
export function headerProblems(header) {
  if (!isPlainObject(header)) return ['"header" must be an object {"module": "<path>.js"}'];
  const problems = [];
  for (const key of Object.keys(header)) {
    if (!HEADER_KEYS.includes(key)) problems.push(`"header" has the unknown key ${quote(key)} (the keys are ${HEADER_KEYS.join(', ')})`);
  }
  if (!isModulePath(header.module)) {
    problems.push('"header.module" must be a relative path of a .js file below "files" (no "..", no scheme)');
  }
  return problems;
}

/**
 * Checks a profile object and returns its normalized, frozen copy (`cards` and `graph` default
 * to `{}`, `files` to ''; `version` and `header` only when given). Throws a ProfileError naming every problem.
 * @param {*} input
 * @returns {Readonly<{name:string, title:string, version?:string, startTab:string, can:Object, tabs:Array, header?:{module:string}, cards:Object, graph:Object, files:string}>}
 */
export function validateProfile(input) {
  const problems = [];
  if (!isPlainObject(input)) throw new ProfileError('The UI profile is invalid', ['it must be an object']);
  const subject = typeof input.name === 'string' && NAME.test(input.name)
    ? `The UI profile "${input.name}" is invalid`
    : 'The UI profile is invalid';
  for (const key of Object.keys(input)) {
    if (!PROFILE_KEYS.includes(key)) problems.push(`unknown key ${quote(key)} (the keys are ${PROFILE_KEYS.join(', ')})`);
  }
  if (typeof input.name !== 'string' || !NAME.test(input.name)) {
    problems.push('"name" must be lowercase letters, digits and "-", starting with a letter (at most 40 characters)');
  }
  if (typeof input.title !== 'string' || !input.title.trim() || input.title.length > 80) {
    problems.push('"title" must be a text of 1 to 80 characters');
  }
  if (input.version !== undefined
    && (typeof input.version !== 'string' || !input.version.trim() || input.version.length > 40 || /\s/.test(input.version))) {
    problems.push('"version" must be a text of 1 to 40 characters without spaces (e.g. "0.1.0")');
  }

  const can = {};
  if (input.can !== undefined && !isPlainObject(input.can)) {
    problems.push('"can" must be an object');
  } else {
    for (const [key, value] of Object.entries(input.can || {})) {
      if (!CAN_KEYS.includes(key)) {
        problems.push(`"can" has the unknown key ${quote(key)}`);
      } else if (!CAN_VALUES.includes(value)) {
        problems.push(`"can.${key}" must be true, false, "wasm" or "server"`);
      } else {
        can[key] = value;
      }
    }
  }

  const tabs = [];
  if (!Array.isArray(input.tabs) || input.tabs.length === 0 || input.tabs.length > MAX_TABS) {
    problems.push(`"tabs" must be a list of 1 to ${MAX_TABS} tabs`);
  } else {
    const ids = new Set();
    let builtins = 0;
    input.tabs.forEach((tab, i) => {
      const at = `"tabs[${i}]"`;
      if (!isPlainObject(tab)) {
        problems.push(`${at} must be an object`);
        return;
      }
      for (const key of Object.keys(tab)) {
        if (!TAB_KEYS.includes(key)) problems.push(`${at} has the unknown key ${quote(key)}`);
      }
      if (typeof tab.id !== 'string' || !NAME.test(tab.id)) {
        problems.push(`${at}.id must be lowercase letters, digits and "-", starting with a letter`);
      } else if (ids.has(tab.id)) {
        problems.push(`${at}.id ${quote(tab.id)} is used twice`);
      } else {
        ids.add(tab.id);
      }
      if (typeof tab.title !== 'string' || !tab.title.trim() || tab.title.length > 40) {
        problems.push(`${at}.title must be a text of 1 to 40 characters`);
      }
      const hasBuiltin = tab.builtin !== undefined;
      const hasModule = tab.module !== undefined;
      if (hasBuiltin === hasModule) {
        problems.push(`${at} must have exactly one of "builtin" and "module"`);
      } else if (hasBuiltin) {
        builtins += 1;
        if (!BUILTIN_TABS.includes(tab.builtin)) {
          problems.push(`${at}.builtin must be one of ${BUILTIN_TABS.join(', ')}`);
        }
      } else if (!isModulePath(tab.module)) {
        problems.push(`${at}.module must be a relative path of a .js file below "files" (no "..", no scheme)`);
      }
      tabs.push(Object.freeze({ ...tab }));
    });
    if (builtins > 1) problems.push('"tabs" may hold the built-in workspace only once');
    if (typeof input.startTab !== 'string' || !ids.has(input.startTab)) {
      problems.push(`"startTab" must be the id of one of the tabs (${[...ids].join(', ') || 'none'})`);
    }
  }

  if (input.header !== undefined) problems.push(...headerProblems(input.header));
  if (input.cards !== undefined) problems.push(...cardsProblems(input.cards));
  if (input.graph !== undefined) problems.push(...graphSpecProblems(input.graph));
  const files = input.files === undefined ? '' : input.files;
  if (typeof files !== 'string' || (files !== '' && (!files.endsWith('/') || !isSafePath(files)))) {
    problems.push('"files" must be "" or a path ending in "/" on this origin (no "..", no scheme)');
  }

  if (problems.length) throw new ProfileError(subject, problems);
  return deepFreeze({
    name: input.name,
    title: input.title,
    ...(input.version === undefined ? {} : { version: input.version }),
    startTab: input.startTab,
    can,
    tabs,
    ...(input.header === undefined ? {} : { header: { module: input.header.module } }),
    cards: JSON.parse(JSON.stringify(input.cards || {})),
    graph: JSON.parse(JSON.stringify(input.graph || {})),
    files,
  });
}

/**
 * The profile a delivery names: `undefined`/`null` is `full`, a string a built-in profile's
 * name, an object a profile of its own. Throws a ProfileError for an unknown name or an invalid
 * object.
 * @param {*} spec
 */
export function resolveProfile(spec) {
  if (spec === undefined || spec === null) return BUILTIN_PROFILES.full;
  if (typeof spec === 'string') {
    if (Object.prototype.hasOwnProperty.call(BUILTIN_PROFILES, spec)) return BUILTIN_PROFILES[spec];
    throw new ProfileError(`The UI profile ${quote(spec)} is unknown`, [`the built-in profiles are ${BUILTIN_NAMES.join(', ')}`]);
  }
  if (isPlainObject(spec)) return validateProfile(spec);
  throw new ProfileError('The UI profile is invalid', ['it must be a built-in name or a profile object']);
}

/**
 * The `profile` parameter of a location hash (`#/play?profile=demo`, `#/?profile=demo`), or
 * null when it has none. The WebAssembly playground reads it; built-in names only.
 * @param {string} hash
 */
export function profileFromHash(hash) {
  const h = String(hash || '').replace(/^#/, '');
  const at = h.indexOf('?');
  if (at < 0) return null;
  let params;
  try {
    params = new URLSearchParams(h.slice(at + 1));
  } catch (e) {
    return null;
  }
  return params.has('profile') ? params.get('profile') : null;
}

/**
 * The profile a WebAssembly playground's address asks for: its `profile` parameter, with `full`
 * written as null (none), so `#/play?profile=full` and `#/play` ask for the same.
 * @param {string} hash
 */
export function requestedProfile(hash) {
  const name = profileFromHash(hash);
  return name === 'full' ? null : name;
}

/**
 * The profile tab and its parameters in a location hash (`#/play?tab=x&a=1`): `{tab, params}`;
 * `tab` is null without one. `profile`, `tab` and the link parameters of js/link.js are not
 * tab parameters.
 * @param {string} hash
 */
export function tabRouteFromHash(hash) {
  const h = String(hash || '').replace(/^#/, '');
  const at = h.indexOf('?');
  const result = { tab: null, params: {} };
  if (!h.startsWith('/play') || at < 0) return result;
  let params;
  try {
    params = new URLSearchParams(h.slice(at + 1));
  } catch (e) {
    return result;
  }
  if (!params.has('tab')) return result;
  result.tab = params.get('tab');
  for (const [key, value] of params) {
    if (!RESERVED_PARAMS.includes(key)) result.params[key] = value;
  }
  return result;
}

/** The hash parameters that are never a tab's: the profile, the tab and those of a playground link. */
export const RESERVED_PARAMS = Object.freeze(['profile', 'tab', 'graph', 'data', 'schema', 'v', 'q', 'lang', 'params', 'title', 'run']);

/**
 * The location hash of a route: `play` (with an optional profile tab and its parameters) or
 * `start`; `profile` is written when the WebAssembly playground shows a profile other than
 * `full` (so a reload keeps it). Tab parameters must be strings (numbers and booleans are
 * written as text); a reserved name is refused.
 * @param {{route?: 'play'|'start', profile?: string|null, tab?: string|null, params?: Object}} route
 */
export function routeHash({ route = 'play', profile = null, tab = null, params = {} } = {}) {
  const parts = [];
  if (profile && profile !== 'full') parts.push(`profile=${encodeURIComponent(profile)}`);
  if (route === 'play' && tab) {
    parts.push(`tab=${encodeURIComponent(tab)}`);
    for (const [key, value] of Object.entries(params || {})) {
      if (RESERVED_PARAMS.includes(key)) throw new Error(`"${key}" is not a tab parameter name (reserved).`);
      if (value === undefined || value === null) continue;
      parts.push(`${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`);
    }
  }
  const base = route === 'start' ? '#/' : '#/play';
  return parts.length ? `${base}?${parts.join('&')}` : base;
}

/**
 * Checks that `profile` can be shown by a server in the API mode `apiMode` (`info().apiMode`):
 * a server in allow-list mode answers only its selected saved queries (`POST /api/query`), the
 * change feed and its info, so a profile with the built-in workspace (which reads the whole
 * served graph) cannot be shown there. Throws a ProfileError; any other mode passes.
 * @param {Object} profile  A profile from `resolveProfile`.
 * @param {string} [apiMode]  'full' (default) or 'allow-list'.
 */
export function checkApiMode(profile, apiMode = 'full') {
  if (apiMode !== 'allow-list') return;
  if (profile.tabs.some((t) => t.builtin === 'workspace')) {
    throw new ProfileError(`The UI profile ${quote(profile.name)} cannot be shown by this server`, [
      'the server answers only its saved queries (allow-list mode), and the profile has the built-in workspace, which reads the whole graph',
      'serve a profile whose tabs are modules of its own',
    ]);
  }
}

/**
 * The profile as the components use it.
 * @param {Object} profile  A profile from `resolveProfile`.
 * @param {{server: boolean}} engine  Whether the engine is the dev server (else WebAssembly).
 */
export function createUi(profile, { server = false } = {}) {
  const warned = new Set();
  const workspaceTab = profile.tabs.find((t) => t.builtin === 'workspace') || null;
  const ui = {
    profile,
    name: profile.name,
    title: profile.title,
    tabs: profile.tabs,
    startTab: profile.startTab,
    /** The id of the built-in workspace tab, or null when the profile has none. */
    workspaceTab: workspaceTab ? workspaceTab.id : null,
    /** Whether the profile shows the built-in workspace (and so needs the served graph). */
    hasWorkspace: !!workspaceTab,
    /**
     * Whether the user can reach `key` (CAN_KEYS) with this engine. An unknown key is false
     * (and warns once): a typo hides a part instead of breaking the page.
     */
    can(key) {
      if (!CAN_KEYS.includes(key)) {
        if (!warned.has(key) && typeof console !== 'undefined') {
          warned.add(key);
          console.warn(`ui.can: unknown key "${key}"`);
        }
        return false;
      }
      const value = profile.can[key];
      if (value === 'wasm') return !server;
      if (value === 'server') return server;
      return value === true;
    },
    /** The profile's top-bar module (`{module}`), or null when it has none. */
    header: profile.header || null,
    /** The tab `id`, or null. */
    tab(id) {
      return profile.tabs.find((t) => t.id === id) || null;
    },
    /** The URL of a module tab's (or the header's) file, resolved against `base` (the page's address). */
    moduleUrl(tab, base) {
      return new URL(profile.files + tab.module, base).href;
    },
  };
  return Object.freeze(ui);
}
