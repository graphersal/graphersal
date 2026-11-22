// Server mode (pure, tested under Node by tests/server-mode.test.mjs): the playground served by a
// multi-database server (repo graphersal-server) under `/db/<name>/` for each database. The dev
// server (`graphersal --server`) and the WebAssembly playground announce none of it, so nothing
// here changes them. What such a server shows at its own root (a database list, an admin UI) is
// a UI profile of its own (js/ui-profiles.js), not code of this playground.
//
// What the page reads (the contract a multi-database server implements):
// - `GET api/info` of a database page (`/db/<name>/api/info`): the dev server's answer plus
//   `mountPath` (`/db/<name>`, set by graphersal-serve's `ContextBuilder::mount_path`) and,
//   optionally, `database` (the name to show; text). Without `database` the name is derived
//   from `mountPath` (`databaseName`).
// - optionally `home` (EXPERIMENTAL; any host, with or without a mount): the address of the
//   host's home page as a path from the SERVER ROOT (`/` = the root itself, `/admin/` = a page
//   below it). The header's database badge links there (`homeHref`). The link is relative to
//   the page (`mountPath` says how far up the root is), so a proxy prefix in front of the
//   server keeps working.

import { normalizeMountPath } from './util.js';

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
 * The relative link from a page under `mountPath` to the server root:
 * `/db/acme` -> `../../`; '' at the root. Relative, so a proxy prefix in front of the server
 * keeps working.
 */
export function rootHref(mountPath) {
  const mount = normalizeMountPath(mountPath);
  if (!mount) return '';
  return '../'.repeat(mount.split('/').length - 1);
}

/**
 * The link of the header's database badge to the host's home page, relative to this page, or
 * null when the info names none. `info.home` is a path from the server root: a text that starts
 * with `/` (so never a scheme), not with `//`, without `\\`, `.`/`..` segments, a query or a
 * fragment (anything else is ignored: the badge shows no link). The answer climbs from the page to the server root
 * (`rootHref(mountPath)`, the page being `<mountPath>/`) and goes down to `home`:
 * `{mountPath: '/db/acme', home: '/'}` -> `../../`; `{home: '/admin/'}` at the root -> `admin/`;
 * `{home: '/'}` at the root -> `./`.
 */
export function homeHref(info) {
  const home = info && info.home;
  if (typeof home !== 'string' || !home.startsWith('/') || home.startsWith('//')) return null;
  if (/[\\?#\0\s]/.test(home)) return null;
  const rest = home.slice(1);
  if (rest.split('/').some((segment) => segment === '.' || segment === '..')) return null;
  const href = rootHref(mountPathOf(info)) + rest;
  return href || './';
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
