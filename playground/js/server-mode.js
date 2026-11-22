// Server mode (pure, tested under Node by tests/server-mode.test.mjs): the playground served by a
// multi-database server (repo graphersal-server) under `/db/<name>/` for each database, with a
// landing view at `/` that lists the databases. The dev server (`graphersal --server`) and the
// WebAssembly playground announce none of it, so nothing here changes them.
//
// What the page reads (the contract a multi-database server implements):
// - `GET api/info` of a database page (`/db/<name>/api/info`): the dev server's answer plus
//   `mountPath` (`/db/<name>`, set by graphersal-serve's `ContextBuilder::mount_path`) and,
//   optionally, `database` (the name to show; text). Without `database` the name is derived
//   from `mountPath` (`databaseName`).
// - `GET api/info` at the root (`/api/info`): `{"server": <text>, "version": <text>, "admin":
//   true}` without `mountPath`: the page shows the landing view (`isLanding`) and creates no
//   engine. `admin` absent or not `true`: the page is the ordinary playground.
// - `GET api/admin/databases` (only read when `isLanding`):
//   `{"databases": [{"name", "state", "kind", "role", "readOnly", "autoLoad", "description",
//   "base"?, "created"?, "aliasOf"?, "reason"?, "model"?}], "aliases": {alias: database},
//   "problems": [text]}` with `state` one of `DATABASE_STATES`. Checked by `databaseRows`; a
//   malformed entry is skipped and counted, never shown half. The optional byte counts
//   `sizeBytes` (on disk) and `memoryBytes` (when loaded) are shown when a server sends them;
//   `pooled` + `pooledBy` (a base held in the shared pool by its loaded layers) and
//   `linkWarning` (model blobs copied, not hard-linked) are optional too.
//   The admin actions are js/admin-api.js.

import { normalizeMountPath } from './util.js';
import { aliasRows } from './admin-api.js';

/** The database states of a multi-database server, in the words the landing view shows. */
export const DATABASE_STATES = Object.freeze({
  unloaded: 'unloaded',
  loading: 'loading…',
  loaded: 'loaded',
  'needs-model': 'needs a model',
  failed: 'failed',
});

/** The badge tone (CSS class `badge <tone>`) of each state; unknown states get none. */
export const STATE_TONES = Object.freeze({
  unloaded: '',
  loading: 'accent',
  loaded: 'ok',
  'needs-model': 'warn',
  failed: 'danger',
});

/** How often the landing view reloads the list while a database is loading (milliseconds). */
export const LANDING_POLL_MS = 2000;

/** Whether the `/api/info` answer asks for the landing view (the root of a multi-database server). */
export function isLanding(info) {
  return !!info && typeof info === 'object' && info.admin === true && !mountPathOf(info);
}

/** The answer's `mountPath` normalised (one leading `/`, no trailing `/`), or ''. */
export function mountPathOf(info) {
  return normalizeMountPath(info && info.mountPath);
}

/**
 * The database name the header shows, or null (no mount: the dev server, the WebAssembly
 * playground). `info.database` (a non-empty text) wins; else the last segment of `mountPath`,
 * percent-decoded (`/db/my%20graph` -> `my graph`).
 */
export function databaseName(info) {
  const mount = mountPathOf(info);
  if (!mount) return null;
  if (typeof info.database === 'string' && info.database.trim()) return info.database.trim();
  const last = mount.slice(mount.lastIndexOf('/') + 1);
  try {
    return decodeURIComponent(last);
  } catch (e) {
    return last;
  }
}

/**
 * The relative link from a page under `mountPath` to the server root (its landing view):
 * `/db/acme` -> `../../`; '' at the root. Relative, so a proxy prefix in front of the server
 * keeps working.
 */
export function rootHref(mountPath) {
  const mount = normalizeMountPath(mountPath);
  if (!mount) return '';
  return '../'.repeat(mount.split('/').length - 1);
}

/** The page of database `name`, relative to the server root: `db/<name>/` (name percent-encoded). */
export function databaseHref(name) {
  return `db/${encodeURIComponent(name)}/`;
}

/**
 * The landing view's rows from a `GET /api/admin/databases` answer:
 * `{rows: [{name, href, state, stateText, tone, kind, role, readOnly, autoLoad, description,
 * base, created, aliasOf, reason, model, sizeBytes, memoryBytes, pooled, pooledBy, linkWarning}],
 * skipped, aliases: [{alias,
 * target}], problems: [text]}`, sorted by name; `skipped` counts the malformed entries. An unknown `state` is shown
 * as it is (a newer server). Throws an Error when the answer has no `databases` list.
 */
export function databaseRows(answer) {
  if (!answer || typeof answer !== 'object' || !Array.isArray(answer.databases)) {
    throw new Error('the server answered no database list');
  }
  const rows = [];
  let skipped = 0;
  for (const d of answer.databases) {
    if (!d || typeof d !== 'object' || typeof d.name !== 'string' || !d.name || typeof d.state !== 'string') {
      skipped++;
      continue;
    }
    rows.push({
      name: d.name,
      href: databaseHref(d.name),
      state: d.state,
      stateText: Object.prototype.hasOwnProperty.call(DATABASE_STATES, d.state) ? DATABASE_STATES[d.state] : d.state,
      tone: Object.prototype.hasOwnProperty.call(STATE_TONES, d.state) ? STATE_TONES[d.state] : '',
      kind: typeof d.kind === 'string' ? d.kind : '',
      role: typeof d.role === 'string' ? d.role : '',
      readOnly: d.readOnly === true,
      autoLoad: d.autoLoad === true,
      description: typeof d.description === 'string' ? d.description : '',
      base: typeof d.base === 'string' && d.base ? d.base : null,
      created: typeof d.created === 'string' ? d.created : '',
      aliasOf: typeof d.aliasOf === 'string' && d.aliasOf ? d.aliasOf : null,
      reason: typeof d.reason === 'string' ? d.reason : '',
      model: typeof d.model === 'string' ? d.model : '',
      sizeBytes: byteCount(d.sizeBytes),
      memoryBytes: byteCount(d.memoryBytes),
      pooled: d.pooled === true,
      pooledBy: Array.isArray(d.pooledBy) ? d.pooledBy.filter((x) => typeof x === 'string' && x) : [],
      linkWarning: typeof d.linkWarning === 'string' ? d.linkWarning : '',
    });
  }
  rows.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const problems = Array.isArray(answer.problems) ? answer.problems.filter((p) => typeof p === 'string' && p) : [];
  return { rows, skipped, aliases: aliasRows(answer), problems };
}

function byteCount(n) {
  return typeof n === 'number' && Number.isFinite(n) && n >= 0 ? n : null;
}

/** Whether the list should be reloaded soon (a database is loading). */
export function anyLoading(rows) {
  return rows.some((r) => r.state === 'loading');
}

/**
 * Fetches the database list: `GET <base>api/admin/databases` with the host's headers; resolves
 * to `databaseRows`' answer, rejects with the server's error text (an Error whose `help` is the
 * server's `help`, when it sent one).
 * @param {{base: URL|string, fetch: Function, headers?: Object<string,string>}} options
 */
export async function fetchDatabases({ base, fetch, headers = {} }) {
  const r = await fetch(new URL('api/admin/databases', base), {
    cache: 'no-store',
    headers: { ...headers, Accept: 'application/json' },
  });
  const text = await r.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch (e) {
    throw new Error(`The server sent no JSON (HTTP ${r.status})`);
  }
  if (!r.ok) {
    const err = new Error((data && data.error) || `HTTP ${r.status}`);
    err.help = data && typeof data.help === 'string' ? data.help : '';
    throw err;
  }
  return databaseRows(data);
}

/**
 * The engine info after a change-feed answer (`GET /api/changes`): a copy with `readOnly` from
 * the answer when it is a boolean that differs (a multi-database server toggles it at run time),
 * else null (nothing changed; an older server without the field changes nothing).
 */
export function infoAfterChanges(engineInfo, answer) {
  if (!engineInfo || !answer || typeof answer.readOnly !== 'boolean') return null;
  if (answer.readOnly === !!engineInfo.readOnly) return null;
  return { ...engineInfo, readOnly: answer.readOnly };
}

/** The reason the UI gives for a server that refuses writes now (`readOnly`), or null. */
export function serverReadOnlyReason(engineInfo) {
  if (!engineInfo || !engineInfo.server || !engineInfo.readOnly) return null;
  return 'the server serves this graph read-only';
}
