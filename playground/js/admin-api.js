// The admin API of a multi-database server (pure, tested under Node by tests/admin-api.test.mjs):
// request builders, the client, error parsing, form checks, the actions a row offers and the
// poll of a load. The view is components/landing-page.js (the admin panel on the landing view,
// shown only when the root `/api/info` announces `admin: true`, js/server-mode.js `isLanding`).
//
// The contract is repo graphersal-server's `docs/admin-api.md` (EXPERIMENTAL): every route below
// `api/admin/`, JSON bodies with `Content-Type: application/json` (unknown fields are refused, so
// the builders send only the documented ones), errors `{"error": <sentence>, "help"?: <text>}`
// shown verbatim (`AdminError`). Actions take REAL database names; an alias is changed only
// through the alias routes.

/** The database name rule of the server (lower case; names are answered in lower case). */
export const NAME_RULE = /^[a-z0-9][a-z0-9._-]{0,62}$/;

/** How often a started load is polled (milliseconds). */
export const LOAD_POLL_MS = 1000;

/** How long a load is polled before the panel stops watching it (it keeps running). */
export const LOAD_POLL_LIMIT_MS = 10 * 60 * 1000;

/** An admin API refusal or failure: `message` = the server's `error`, `help` its `help`. */
export class AdminError extends Error {
  constructor(message, { status = 0, help = '', state = '' } = {}) {
    super(message);
    this.name = 'AdminError';
    this.status = status;
    this.help = help;
    this.state = state;
  }
}

/**
 * The AdminError of a non-2xx answer: `{error, help?}` verbatim; a body without `error` names
 * the HTTP status. `text` is the raw body (maybe not JSON: a proxy's page).
 */
export function parseAdminError(status, text) {
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch (e) {
    data = null;
  }
  const error = data && typeof data.error === 'string' && data.error ? data.error : `HTTP ${status}`;
  const help = data && typeof data.help === 'string' ? data.help : '';
  const state = data && typeof data.state === 'string' ? data.state : '';
  return new AdminError(error, { status, help, state });
}

const seg = (name) => encodeURIComponent(String(name));

/**
 * The request of one admin action: `{method, path, body?}` with `path` relative to the server
 * root. Actions: list, get, create(body), delete, clone(newName), load, unload,
 * readOnly(value), autoLoad(value), aliases, setAlias(alias, target), removeAlias(alias).
 */
export const adminRequest = Object.freeze({
  list: () => ({ method: 'GET', path: 'api/admin/databases' }),
  get: (name) => ({ method: 'GET', path: `api/admin/databases/${seg(name)}` }),
  create: (body) => ({ method: 'POST', path: 'api/admin/databases', body }),
  delete: (name) => ({ method: 'DELETE', path: `api/admin/databases/${seg(name)}` }),
  clone: (name, newName) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/clone`, body: { name: newName } }),
  load: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/load` }),
  unload: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/unload` }),
  readOnly: (name, value) => ({ method: 'PUT', path: `api/admin/databases/${seg(name)}/read-only`, body: { readOnly: !!value } }),
  autoLoad: (name, value) => ({ method: 'PUT', path: `api/admin/databases/${seg(name)}/auto-load`, body: { autoLoad: !!value } }),
  aliases: () => ({ method: 'GET', path: 'api/admin/aliases' }),
  setAlias: (alias, target) => ({ method: 'PUT', path: `api/admin/aliases/${seg(alias)}`, body: { target } }),
  removeAlias: (alias) => ({ method: 'DELETE', path: `api/admin/aliases/${seg(alias)}` }),
});

/**
 * Sends one request (`adminRequest.*`) and resolves to the parsed answer; rejects with an
 * AdminError (the server's `error`/`help`, or the network failure). A body is sent as JSON.
 * @param {{base: URL|string, fetch: Function, headers?: Object<string,string>}} options
 */
export async function sendAdmin({ base, fetch, headers = {} }, request) {
  const init = {
    method: request.method,
    cache: 'no-store',
    headers: { ...headers, Accept: 'application/json' },
  };
  if (request.body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(request.body);
  }
  let r;
  try {
    r = await fetch(new URL(request.path, base), init);
  } catch (err) {
    throw new AdminError(`The server could not be reached: ${err && err.message ? err.message : err}`);
  }
  const text = await r.text();
  if (!r.ok) throw parseAdminError(r.status, text);
  try {
    return text ? JSON.parse(text) : {};
  } catch (e) {
    throw new AdminError(`The server sent no JSON (HTTP ${r.status})`, { status: r.status });
  }
}

/** An admin client: one method per action of `adminRequest`, each answering `sendAdmin`'s promise. */
export function createAdmin(options) {
  const client = {};
  for (const [action, build] of Object.entries(adminRequest)) {
    client[action] = (...args) => sendAdmin(options, build(...args));
  }
  return client;
}

/** The problem with a database or alias name, or null. */
export function nameProblem(name, what = 'name') {
  const n = typeof name === 'string' ? name.trim() : '';
  if (!n) return `Enter a ${what}.`;
  if (!NAME_RULE.test(n.toLowerCase())) {
    return `A ${what} starts with a letter or digit and has at most 63 of a-z, 0-9, '.', '_', '-'.`;
  }
  return null;
}

/**
 * The create body from the create form, or the problems: `{body}` | `{problems: [text]}`.
 * Form: `{name, kind: 'plain'|'layer', role: 'normal'|'base', base, description, schema (JSON
 * text, optional), readOnly, autoLoad}`. Only the documented fields are sent, and only the ones
 * that differ from the server's defaults (a layer sends `kind` + `base`, no `role`).
 */
export function createBody(form) {
  const problems = [];
  const f = form || {};
  const name = typeof f.name === 'string' ? f.name.trim().toLowerCase() : '';
  const np = nameProblem(name);
  if (np) problems.push(np);
  const body = { name };
  if (f.kind === 'layer') {
    body.kind = 'layer';
    const base = typeof f.base === 'string' ? f.base.trim() : '';
    if (!base) problems.push('Choose the base database of the layer.');
    else body.base = base;
  } else if (f.role === 'base') {
    body.role = 'base';
  }
  const description = typeof f.description === 'string' ? f.description.trim() : '';
  if (description) body.description = description;
  const schemaText = typeof f.schema === 'string' ? f.schema.trim() : '';
  if (schemaText) {
    let schema;
    try {
      schema = JSON.parse(schemaText);
    } catch (e) {
      problems.push(`The schema is not JSON: ${e.message}`);
    }
    if (schema !== undefined) {
      if (!schema || typeof schema !== 'object' || Array.isArray(schema)) problems.push('The schema must be a JSON object.');
      else body.schema = schema;
    }
  }
  if (f.autoLoad === true) body.autoLoad = true;
  // A base is always read-only on the server; the flag is sent only for a normal database.
  if (f.readOnly === true && body.role !== 'base') body.readOnly = true;
  return problems.length ? { problems } : { body };
}

/** The listing's aliases as sorted rows `[{alias, target}]` (`aliases` of `GET api/admin/databases`). */
export function aliasRows(answer) {
  const map = answer && answer.aliases && typeof answer.aliases === 'object' ? answer.aliases : {};
  return Object.entries(map)
    .filter(([a, t]) => typeof a === 'string' && a && typeof t === 'string' && t)
    .map(([alias, target]) => ({ alias, target }))
    .sort((a, b) => (a.alias < b.alias ? -1 : a.alias > b.alias ? 1 : 0));
}

/** The real databases (no alias rows) of `databaseRows`' rows. */
export function realRows(rows) {
  return rows.filter((r) => !r.aliasOf);
}

/** The names of the databases a layer may be created over (`role` `base`, real rows). */
export function baseNames(rows) {
  return realRows(rows).filter((r) => r.role === 'base').map((r) => r.name);
}

/**
 * What a row offers, each `{enabled, title}` (title = why not, when disabled): load, unload,
 * clone, delete, readOnly (the toggle), autoLoad (the toggle). Alias rows offer nothing here
 * (they are changed in the aliases section). `busy` = an action of this row is running. A rule
 * the server checks anyway (delete while an alias names it, unload of a base in use) is left to
 * the server, whose error and help the panel shows.
 */
export function rowActions(row, { busy = false, aliasTargets = [] } = {}) {
  const off = (title) => ({ enabled: false, title });
  const on = (title) => ({ enabled: true, title });
  if (row.aliasOf) {
    const why = `An alias of ${row.aliasOf}: change it under Aliases`;
    return { load: off(why), unload: off(why), clone: off(why), delete: off(why), readOnly: off(why), autoLoad: off(why) };
  }
  if (busy) {
    const why = 'Wait until the running action ends';
    return { load: off(why), unload: off(why), clone: off(why), delete: off(why), readOnly: off(why), autoLoad: off(why) };
  }
  const configless = !row.kind; // a failed database whose database.toml cannot be read
  const s = row.state;
  return {
    load: s === 'loaded' || s === 'loading'
      ? off(s === 'loaded' ? 'Already loaded' : 'Loading…')
      : configless ? off('Its configuration cannot be read') : on('Load it into memory and serve it'),
    unload: s === 'loaded' ? on('Close it; running requests get up to 30 s') : off('Not loaded'),
    clone: configless || s === 'failed'
      ? off('A failed database cannot be cloned')
      : on('Copy its latest state into a new database'),
    delete: s === 'loaded' || s === 'loading'
      ? off('Unload it first')
      : aliasTargets.includes(row.name)
        ? off('An alias names it: remove or flip the alias first')
        : on("Move it to the server's trash/ directory"),
    readOnly: configless
      ? off('Its configuration cannot be read')
      : row.role === 'base'
        ? off('A base database is always read-only')
        : on(row.readOnly ? 'Allow writes again' : 'Refuse every write'),
    autoLoad: configless ? off('Its configuration cannot be read') : on('Load it when the server starts'),
  };
}

/** The confirmation text of a destructive action, or null (no confirmation needed). */
export function confirmText(action, subject, extra = '') {
  switch (action) {
    case 'delete':
      return `Delete the database "${subject}"? Its directory moves to the server's trash/ directory (recoverable by hand only).`;
    case 'unload':
      return `Unload "${subject}"? Running requests and jobs get up to 30 s; its pages stop answering until it is loaded again.`;
    case 'removeAlias':
      return `Remove the alias "${subject}"? Links to db/${subject}/ stop working.`;
    case 'flipAlias':
      return `Point the alias "${subject}" to "${extra}"? Its pages serve the other database from now on.`;
    default:
      return null;
  }
}

/** The one-line outcome of a finished action (shown as a notice). */
export function outcomeText(action, subject, answer = {}) {
  switch (action) {
    case 'create': return `Created "${subject}" (unloaded).`;
    case 'clone': return `Cloned into "${subject}" (unloaded).`;
    case 'delete': return `Deleted "${subject}"${answer.trash ? `; it was moved to ${answer.trash}` : ''}.`;
    case 'load':
      if (answer.state === 'loaded') return `"${subject}" is loaded.`;
      if (answer.state === 'failed') return `"${subject}" failed to load${answer.reason ? `: ${answer.reason}` : ''}.`;
      if (answer.state === 'needs-model') return `"${subject}" needs a model${answer.model ? ` (${answer.model})` : ''}${answer.reason ? `: ${answer.reason}` : ''}.`;
      return `"${subject}" is ${answer.state || 'loading'}.`;
    case 'unload': return answer.unloaded === false ? `"${subject}" was not loaded.` : `"${subject}" is unloaded.`;
    case 'readOnly': return answer.readOnly ? `"${subject}" is read-only.` : `"${subject}" accepts writes.`;
    case 'autoLoad': return answer.autoLoad ? `"${subject}" loads when the server starts.` : `"${subject}" no longer loads at start.`;
    case 'setAlias': return `The alias "${answer.alias || subject}" names "${answer.target}".`;
    case 'removeAlias': return `Removed the alias "${subject}".`;
    default: return '';
  }
}

/**
 * Starts a load and watches it: `POST load`, then `GET databases/<name>` every `intervalMs`
 * while the state is `loading`; `onState(entry)` gets every answer. Resolves to the last entry
 * (`loaded`, `failed`, `needs-model`, or still `loading` after `limitMs`: the panel stops
 * watching, the load goes on); rejects with the AdminError of a refused request. `sleep` and
 * `now` are injectable (tests).
 */
export async function loadAndWatch(admin, name, {
  onState = () => {}, intervalMs = LOAD_POLL_MS, limitMs = LOAD_POLL_LIMIT_MS,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), now = () => Date.now(),
  alive = () => true,
} = {}) {
  let entry = await admin.load(name);
  onState(entry);
  const start = now();
  while (entry && entry.state === 'loading' && alive() && now() - start < limitMs) {
    await sleep(intervalMs);
    if (!alive()) break;
    entry = await admin.get(name);
    onState(entry);
  }
  return entry;
}

/** A byte count as text (`1.5 MB`), or '' for a missing value. */
export function bytesText(n) {
  if (typeof n !== 'number' || !Number.isFinite(n) || n < 0) return '';
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}
