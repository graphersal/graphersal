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
 * readOnly(value), autoLoad(value), aliases, setAlias(alias, target), removeAlias(alias),
 * backup(name), backups(name), restore(name, backup, newName?) (no `name` field when restoring
 * over the database itself), importPath({path, name, base?, format?, description?, role?}), models,
 * addModel({path?, repo?, license?}), removeModel(repo or blob hash), promote(name) (no body;
 * one way: a base is frozen forever). The two transfers that are not JSON requests are
 * `exportPath` (a download) and `uploadImport` (the file as the body).
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
  backup: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/backup` }),
  backups: (name) => ({ method: 'GET', path: `api/admin/databases/${seg(name)}/backups` }),
  restore: (name, backup, newName) => ({
    method: 'POST',
    path: `api/admin/databases/${seg(name)}/restore`,
    body: newName && newName !== name ? { backup, name: newName } : { backup },
  }),
  importPath: (body) => ({ method: 'POST', path: 'api/admin/import', body }),
  models: () => ({ method: 'GET', path: 'api/admin/models' }),
  addModel: (body) => ({ method: 'POST', path: 'api/admin/models', body }),
  removeModel: (id) => ({ method: 'DELETE', path: `api/admin/models/${modelSegs(id)}` }),
  promote: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/promote` }),
});

/** A model's repository (`owner/name`: its slash stays part of the path) or blob hash as path segments. */
function modelSegs(id) {
  return String(id).split('/').map(seg).join('/');
}

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
    case 'restoreOver':
      return `Restore "${subject}" from the backup ${extra}? Its current directory moves to the server's trash/ directory and the backup takes its place.`;
    case 'removeModel':
      return `Remove the model "${subject}" from the server's models/ directory? Its blob files are deleted.`;
    case 'promote':
      return `Promote "${subject}" to a base? ${BASE_IS_FROZEN}`;
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
    case 'backup': return `Backed up "${subject}" as ${answer.backup || 'a new backup'}${typeof answer.bytes === 'number' ? ` (${bytesText(answer.bytes)})` : ''}.`;
    case 'restore': {
      const from = answer.restoredFrom && answer.restoredFrom.backup ? ` from ${answer.restoredFrom.database || ''}/${answer.restoredFrom.backup}` : '';
      const was = answer.replaced ? `; the replaced directory moved to ${answer.trash || "the server's trash/ directory"}` : '';
      return `Restored "${answer.name || subject}"${from} (${answer.state || 'unloaded'})${was}.`;
    }
    case 'import': {
      const imp = answer.import && typeof answer.import === 'object' ? answer.import : {};
      const what = [imp.format, answer.kind, answer.role === 'base' ? 'base' : ''].filter(Boolean).join(', ');
      const state = answer.state === 'needs-model' ? `needs a model${answer.model ? ` (${answer.model})` : ''}` : answer.state || 'unloaded';
      return `Imported "${answer.name || subject}"${what ? ` (${what})` : ''}: ${state}.`;
    }
    case 'addModel': {
      const added = Array.isArray(answer.added) ? answer.added.length : 0;
      const resolved = Array.isArray(answer.resolved) ? answer.resolved : [];
      return `Added ${added} model blob${added === 1 ? '' : 's'}${resolved.length ? `; ready to load: ${resolved.join(', ')}` : ''}.`;
    }
    case 'removeModel': return `Removed the model "${answer.model || subject}"${typeof answer.bytes === 'number' ? ` (${bytesText(answer.bytes)} freed)` : ''}.`;
    case 'promote': return `"${answer.name || subject}" is now a base (read-only, frozen); create layers over it with "New database…".`;
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


// ------------------------------------------------------------------ backups, export, import, models (P3)

/**
 * The extra row actions of the transfers, each `{enabled, title}`: backup, backups (the list,
 * with restore), export. Kept apart from `rowActions` (its set is the P2 contract). A failed
 * database whose config cannot be read offers only its backups; a failed one cannot be backed up
 * or exported.
 */
export function transferActions(row, { busy = false } = {}) {
  const off = (title) => ({ enabled: false, title });
  const on = (title) => ({ enabled: true, title });
  if (row.aliasOf) {
    const why = `An alias of ${row.aliasOf}: use that database's row`;
    return { backup: off(why), backups: off(why), export: off(why) };
  }
  if (busy) {
    const why = 'Wait until the running action ends';
    return { backup: off(why), backups: off(why), export: off(why) };
  }
  const failed = row.state === 'failed' || !row.kind;
  return {
    backup: failed ? off('A failed database cannot be backed up') : on("Write a verified ZIP backup into the server's backups/ directory"),
    backups: on('List its backups and restore one'),
    export: failed ? off('A failed database cannot be exported') : on('Download a packed snapshot (.gsnap)'),
  };
}

/** The backups of a `GET .../backups` answer as rows, oldest first (as sent); malformed ones skipped. */
export function backupRows(answer) {
  const list = answer && Array.isArray(answer.backups) ? answer.backups : [];
  return list
    .filter((b) => b && typeof b === 'object' && typeof b.backup === 'string' && b.backup)
    .map((b) => ({
      backup: b.backup,
      database: typeof b.database === 'string' ? b.database : '',
      file: typeof b.file === 'string' ? b.file : '',
      bytes: typeof b.bytes === 'number' && Number.isFinite(b.bytes) && b.bytes >= 0 ? b.bytes : null,
      created: typeof b.created === 'string' ? b.created : '',
      kind: typeof b.kind === 'string' ? b.kind : '',
      role: typeof b.role === 'string' ? b.role : '',
      base: typeof b.base === 'string' ? b.base : '',
      layerBase: typeof b.layerBase === 'string' ? b.layerBase : '',
    }));
}

/**
 * Whether a backup of `name` may be restored over the database itself: `{enabled, title}`. The
 * database must be unloaded (or no longer exist: the backup becomes a new database of that
 * name). `row` is the database's listing row, or null when it does not exist.
 */
export function restoreOverAction(row) {
  if (!row) return { enabled: true, title: 'The database no longer exists: the backup becomes a new database of this name' };
  if (row.state === 'loaded' || row.state === 'loading') return { enabled: false, title: 'Unload it first (or restore into a new name)' };
  return { enabled: true, title: "Replace it; its current directory moves to the server's trash/ directory" };
}

/**
 * The export download: `{path, fileName}` with `path` relative to the server root
 * (`api/admin/databases/<name>/export?models=true|false[&materialize=true]`) and the file name the
 * server sends in its Content-Disposition (`<name>.gsnap`, `<name>.layer.gsnap` for a layer
 * snapshot). `materialize` is sent only for a layer (a standalone full snapshot instead of the
 * layer's changes); a layer snapshot never carries models.
 */
export function exportPath(name, { models = true, materialize = false, layer = false } = {}) {
  const query = [`models=${models ? 'true' : 'false'}`];
  if (layer && materialize) query.push('materialize=true');
  const layerFile = layer && !materialize;
  return {
    path: `api/admin/databases/${seg(name)}/export?${query.join('&')}`,
    fileName: `${name}${layerFile ? '.layer' : ''}.gsnap`,
  };
}

/** The upload size limit (`uploadBytes` of the root `/api/info`), or null when not announced. */
export function uploadLimit(info) {
  const n = info && info.uploadBytes;
  return typeof n === 'number' && Number.isFinite(n) && n > 0 ? n : null;
}

/** The problem with uploading `size` bytes under `limit` (null: none announced), or null. */
export function uploadSizeProblem(size, limit) {
  if (typeof size !== 'number' || !Number.isFinite(size)) return 'Choose a file.';
  if (size === 0) return 'The file is empty.';
  if (limit !== null && limit !== undefined && size > limit) {
    return `The file has ${bytesText(size)}; the server takes uploads up to ${bytesText(limit)} ([limits] upload_bytes in server.toml). Import it from a path on the server instead.`;
  }
  return null;
}

/** The formats an import names (`format`); '' = detected from the content. */
export const IMPORT_FORMATS = Object.freeze(['', 'snapshot', 'store', 'graphson', 'graphml']);

/**
 * The import request from the import form, or the problems: `{upload: {path, size}}` (the file
 * as the body of `POST api/admin/import?<query>`), `{request}` (the JSON path form) or
 * `{problems: [text]}`. Form: `{source: 'upload'|'path', file ({size}), path, name, base,
 * format, description, role: 'normal'|'base'}`; `limit` = `uploadLimit(info)`. Only documented
 * parameters are sent, and only when set: `role` only as `base` (the server's default is
 * `normal`), and then without `base` (a base is made only from a plain source, for which the
 * server refuses `base`; a layer source or a missing model is the server's refusal, shown as sent).
 */
export function importRequest(form, limit = null) {
  const f = form || {};
  const problems = [];
  const name = typeof f.name === 'string' ? f.name.trim().toLowerCase() : '';
  const np = nameProblem(name, 'name for the new database');
  if (np) problems.push(np);
  const fields = { name };
  const asBase = f.role === 'base';
  const base = typeof f.base === 'string' ? f.base.trim() : '';
  if (base && !asBase) fields.base = base;
  const format = typeof f.format === 'string' ? f.format : '';
  if (format) {
    if (!IMPORT_FORMATS.includes(format)) problems.push(`Unknown format "${format}".`);
    else fields.format = format;
  }
  const description = typeof f.description === 'string' ? f.description.trim() : '';
  if (description) fields.description = description;
  if (asBase) fields.role = 'base';
  if (f.source === 'path') {
    const path = typeof f.path === 'string' ? f.path.trim() : '';
    if (!path) problems.push("Enter the source's path on the server.");
    else if (!isAbsolutePath(path)) problems.push('The path must be absolute (it is read on the server).');
    if (problems.length) return { problems };
    return { request: adminRequest.importPath({ path, ...fields }) };
  }
  const size = f.file && typeof f.file.size === 'number' ? f.file.size : NaN;
  const sp = uploadSizeProblem(size, limit);
  if (sp) problems.push(sp);
  if (problems.length) return { problems };
  const query = Object.entries(fields).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&');
  return { upload: { path: `api/admin/import?${query}`, size } };
}

/** Whether a server path is absolute (unix `/...`, Windows `C:\...`, `C:/...` or `\\server\...`). */
export function isAbsolutePath(path) {
  return /^\//.test(path) || /^[A-Za-z]:[\\/]/.test(path) || /^\\\\/.test(path);
}

/** The text of an upload's progress: `12 MB of 40 MB (30 %)`, or the bytes sent when the total is unknown. */
export function progressText(loaded, total) {
  if (typeof total === 'number' && total > 0) {
    const pct = Math.min(100, Math.floor((loaded / total) * 100));
    return `${bytesText(loaded)} of ${bytesText(total)} (${pct} %)`;
  }
  return `${bytesText(loaded) || '0 B'} sent`;
}

/**
 * Uploads `file` (a Blob/File, or anything `XMLHttpRequest.send` takes) to `upload.path`
 * (`importRequest`'s `upload`) as the raw body with `Content-Type: application/octet-stream`
 * and the host headers, through `XMLHttpRequest` (the only browser API with upload progress).
 * Answers `{promise, abort}`: `promise` resolves to the parsed answer (the import entry) and
 * rejects with an AdminError: the server's `{error, help}`; a `413` without `help` gets the
 * limit's advice; a connection the server closed (it answers an upload declared far above its
 * limit without reading it) names the limit; `abort()` rejects with `aborted: true`.
 * `onProgress(loaded, total)` follows `xhr.upload.onprogress`; `onSent()` fires when the body is
 * sent (the import then runs within the request). `XHR` is injectable (tests).
 * @param {{base: URL|string, headers?: Object<string,string>, XHR?: Function, limit?: number|null}} options
 */
export function uploadImport({ base, headers = {}, XHR = globalThis.XMLHttpRequest, limit = null }, upload, file, { onProgress = () => {}, onSent = () => {} } = {}) {
  let xhr = null;
  const promise = new Promise((resolve, reject) => {
    if (typeof XHR !== 'function') {
      reject(new AdminError('This browser cannot upload files (no XMLHttpRequest).'));
      return;
    }
    xhr = new XHR();
    xhr.open('POST', String(new URL(upload.path, base)));
    for (const [k, v] of Object.entries(headers)) xhr.setRequestHeader(k, v);
    xhr.setRequestHeader('Accept', 'application/json');
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    if (xhr.upload) {
      xhr.upload.onprogress = (e) => onProgress(e.loaded, e.lengthComputable ? e.total : upload.size);
      xhr.upload.onload = () => onSent();
    }
    xhr.onload = () => {
      const text = typeof xhr.responseText === 'string' ? xhr.responseText : '';
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(text ? JSON.parse(text) : {});
        } catch (e) {
          reject(new AdminError(`The server sent no JSON (HTTP ${xhr.status})`, { status: xhr.status }));
        }
        return;
      }
      const err = parseAdminError(xhr.status, text);
      if (xhr.status === 413 && !err.help) {
        err.help = limit ? `The server takes uploads up to ${bytesText(limit)}; import a larger file from a path on the server.` : 'Import a larger file from a path on the server.';
      }
      reject(err);
    };
    xhr.onerror = () => {
      const tooBig = limit && upload.size > limit;
      reject(new AdminError(
        tooBig
          ? `The server closed the connection: the file is larger than its upload limit (${bytesText(limit)}).`
          : 'The upload failed: the connection was closed before the server answered.',
        { help: tooBig ? 'Import it from a path on the server instead.' : '' },
      ));
    };
    xhr.onabort = () => {
      const err = new AdminError('The upload was cancelled.');
      err.aborted = true;
      reject(err);
    };
    xhr.send(file);
  });
  return {
    promise,
    abort: () => {
      if (xhr) xhr.abort();
    },
  };
}

/** The model rows of `GET api/admin/models` (`models`), malformed ones skipped. */
export function modelRows(answer) {
  const list = answer && Array.isArray(answer.models) ? answer.models : [];
  const strings = (v) => (Array.isArray(v) ? v.filter((x) => typeof x === 'string' && x) : []);
  return list
    .filter((m) => m && typeof m === 'object' && typeof m.blob === 'string' && m.blob)
    .map((m) => ({
      repo: typeof m.repo === 'string' && m.repo ? m.repo : null,
      identity: typeof m.identity === 'string' ? m.identity : '',
      license: typeof m.license === 'string' ? m.license : '',
      blob: m.blob,
      bytes: typeof m.bytes === 'number' && Number.isFinite(m.bytes) && m.bytes >= 0 ? m.bytes : null,
      users: strings(m.users),
      waiting: strings(m.waiting),
      copied: m.copied === true,
      copiedIn: strings(m.copiedIn),
      linkWarning: typeof m.linkWarning === 'string' ? m.linkWarning : '',
      links: typeof m.links === 'number' ? m.links : null,
      // What `DELETE models/<id>` takes: the repository, else the blob hash.
      id: typeof m.repo === 'string' && m.repo ? m.repo : m.blob,
    }));
}

/** The `needs-model` databases of `GET api/admin/models` (`missing`) as `[{database, model}]`. */
export function missingRows(answer) {
  const list = answer && Array.isArray(answer.missing) ? answer.missing : [];
  return list
    .filter((m) => m && typeof m.database === 'string' && m.database && typeof m.model === 'string' && m.model)
    .map((m) => ({ database: m.database, model: m.model }));
}

/** Whether a model row may be removed: `{enabled, title}` (the server checks the catalog too). */
export function removeModelAction(model) {
  if (model.users.length) return { enabled: false, title: `Used by ${model.users.join(', ')}: delete those databases first` };
  if (model.waiting.length) return { enabled: false, title: `Waited for by ${model.waiting.join(', ')}` };
  return { enabled: true, title: "Delete its blob from the server's models/ directory" };
}

/**
 * The add-model body from the form, or the problems: `{body}` | `{problems}`. Form `{source:
 * 'path'|'repo', path, repo, license}`: a path (absolute, on the server: a Model2Vec model
 * directory or a `.gsnap` carrying model blobs; `repo`/`license` name a directory's model when it
 * does not), or a repository alone (a model `models/` already holds).
 */
export function addModelBody(form) {
  const f = form || {};
  const repo = typeof f.repo === 'string' ? f.repo.trim() : '';
  const license = typeof f.license === 'string' ? f.license.trim() : '';
  if (f.source === 'repo') {
    if (!repo) return { problems: ['Enter the repository (owner/name).'] };
    return { body: { repo } };
  }
  const path = typeof f.path === 'string' ? f.path.trim() : '';
  if (!path) return { problems: ['Enter the path of a model directory or .gsnap on the server.'] };
  if (!isAbsolutePath(path)) return { problems: ['The path must be absolute (it is read on the server).'] };
  const body = { path };
  if (repo) body.repo = repo;
  if (license) body.license = license;
  return { body };
}

/** The hint of a `needs-model` row: how to get it loadable (empty for other states). */
export function needsModelHint(row) {
  if (!row || row.state !== 'needs-model') return '';
  const model = row.model ? `"${row.model}"` : 'its model';
  return `Add ${model} under Models (from a model directory or .gsnap on the server; when models/ already holds it: "Give to waiting"); the database then becomes unloaded and can be loaded.`;
}

/**
 * The state text of a listing row with the pool: an unloaded base held in the shared pool by
 * its loaded layers reads `in pool (layers: a, b)`; otherwise the row's `stateText`.
 */
export function poolText(row) {
  if (!row || !row.pooled) return row ? row.stateText : '';
  if (row.state === 'loaded') return row.stateText;
  return row.pooledBy && row.pooledBy.length ? `in pool (layers: ${row.pooledBy.join(', ')})` : 'in pool';
}

/**
 * A database name suggested from an upload's file name: the extensions of the import sources
 * dropped (`acme.layer.gsnap` -> `acme`), lower case, every other character `-`; '' when nothing
 * usable is left. Only a suggestion: the form checks it with `nameProblem`.
 */
export function suggestName(fileName) {
  let n = typeof fileName === 'string' ? fileName : '';
  n = n.replace(/^.*[\\/]/, '').toLowerCase();
  n = n.replace(/(\.layer)?\.(gsnap|gstore|graphson|jsonl|json|graphml|xml)$/, '');
  n = n.replace(/[^a-z0-9._-]+/g, '-').replace(/^[^a-z0-9]+/, '').slice(0, 63).replace(/[._-]+$/, '');
  return NAME_RULE.test(n) ? n : '';
}


// ------------------------------------------------------------------ bases with data (P4)

/** What promotion means, said in every confirmation of it (one way: no route turns a base back). */
export const BASE_IS_FROZEN = 'A base is frozen forever: it cannot be written or turned back; layers are built over it.';

/**
 * Whether a row may be promoted to a base (`POST .../promote`): `{enabled, title}`. Only an
 * UNLOADED plain normal database whose config is readable; every other row is disabled with the
 * reason (the server refuses the same cases with `409` and a help, shown verbatim when reached).
 */
export function promoteAction(row, { busy = false } = {}) {
  const off = (title) => ({ enabled: false, title });
  if (row.aliasOf) return off(`An alias of ${row.aliasOf}: use that database's row`);
  if (busy) return off('Wait until the running action ends');
  if (row.state === 'failed' || !row.kind) return off('A failed database cannot be promoted');
  if (row.kind === 'layer') return off('A layer cannot become a base: export it with "materialize" and import the file as a base');
  if (row.role === 'base') return off('Already a base');
  if (row.state === 'loaded') return off('Unload it first: only an unloaded database can be promoted');
  if (row.state === 'loading') return off('Loading…: unload it once loaded, then promote it');
  if (row.state === 'needs-model') return off('Add its model first: a base cannot get a model later');
  if (row.state !== 'unloaded') return off(`Only an unloaded database can be promoted (it is ${row.state})`);
  return { enabled: true, title: `Turn it into a base. ${BASE_IS_FROZEN}` };
}
