// Node tests of server mode (js/server-mode.js, js/util.js storage namespace, the HTTP engine's
// probe and the /reset page with mounts): the playground served by a multi-database server under
// `/db/<name>/`, with a landing view at its root. A mock server (node:http on port 0) and fake
// fetches stand in for the server.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import vm from 'node:vm';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { setStorageNamespace, storage, storageKey, normalizeMountPath } from '../js/util.js';
import {
  isLanding, databaseName, rootHref, databaseHref, databaseRows, fetchDatabases, anyLoading,
  infoAfterChanges, serverReadOnlyReason, DATABASE_STATES,
} from '../js/server-mode.js';
import { HttpEngine, SERVER_ID } from '../js/engine/http-engine.js';
import { editModeState } from '../js/graph/edit-mode.js';
import { mcpBadgeTitle, mcpEndpointUrl } from '../js/change-feed.js';

/** A Storage stub (the Web Storage API surface used by util.js and the reset page). */
function storageStub(entries = {}) {
  const map = new Map(Object.entries(entries));
  return {
    get length() { return map.size; },
    key: (i) => [...map.keys()][i] ?? null,
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(k, String(v)),
    removeItem: (k) => map.delete(k),
    dump: () => Object.fromEntries(map),
  };
}

/** Runs `fn` with `globalThis.window.localStorage` = a fresh stub; restores the root namespace. */
function withLocalStorage(fn) {
  const had = Object.prototype.hasOwnProperty.call(globalThis, 'window');
  const before = globalThis.window;
  const local = storageStub();
  globalThis.window = { localStorage: local };
  try {
    return fn(local);
  } finally {
    setStorageNamespace(null);
    if (had) globalThis.window = before;
    else delete globalThis.window;
  }
}

// ------------------------------------------------------------------ storage namespace

test('storage keys are unchanged at the root (dev server, WebAssembly playground)', () => {
  withLocalStorage((local) => {
    for (const root of [undefined, null, '', '/', 42]) {
      setStorageNamespace(root);
      assert.equal(storageKey('tabs'), 'graphersal.playground.tabs');
    }
    storage.set('tabs', { tabs: [1] });
    storage.set('theme', 'dark');
    assert.deepEqual(local.dump(), {
      'graphersal.playground.tabs': '{"tabs":[1]}',
      'graphersal.playground.theme': '"dark"',
    });
    assert.deepEqual(storage.get('tabs', null), { tabs: [1] });
  });
});

test('under a mount every key is namespaced except the shared theme', () => {
  withLocalStorage((local) => {
    local.setItem('graphersal.playground.tabs', '"root tabs"');
    setStorageNamespace('/db/acme/');
    assert.equal(storageKey('tabs'), 'graphersal.playground.@/db/acme#tabs');
    assert.equal(storage.get('tabs', 'none'), 'none', 'the root state is not read under a mount');
    storage.set('tabs', 'acme tabs');
    storage.set('history', []);
    storage.set('theme', 'light');
    setStorageNamespace('db/other');
    storage.set('tabs', 'other tabs');
    assert.equal(storage.get('theme', null), 'light', 'the theme is shared by every mount');
    storage.remove('tabs');
    assert.deepEqual(local.dump(), {
      'graphersal.playground.tabs': '"root tabs"',
      'graphersal.playground.@/db/acme#tabs': '"acme tabs"',
      'graphersal.playground.@/db/acme#history': '[]',
      'graphersal.playground.theme': '"light"',
    });
    setStorageNamespace('/db/acme');
    assert.equal(storage.get('tabs', null), 'acme tabs');
  });
  assert.equal(normalizeMountPath('//db//x//'), '/db//x');
  assert.equal(normalizeMountPath('/'), '');
});

// ------------------------------------------------------------------ /reset with mounts

function runResetPage(pathname, entries) {
  const html = readFileSync(fileURLToPath(new URL('../reset/index.html', import.meta.url)), 'utf8');
  const m = /\/\/ reset-script:begin[^\n]*\n([\s\S]*?)\/\/ reset-script:end/.exec(html);
  assert.ok(m, 'the reset script is between its markers');
  const el = () => ({ textContent: '', hidden: true, href: '', appendChild() {} });
  const elements = { status: el(), details: el(), continue: el() };
  const local = storageStub(entries);
  const window = {
    localStorage: local,
    sessionStorage: storageStub(),
    location: { pathname, origin: 'http://localhost:8484', replace() {} },
  };
  vm.runInNewContext(m[1], {
    window,
    document: { getElementById: (id) => elements[id], createElement: () => ({ textContent: '' }) },
    setTimeout: () => 0,
    Promise,
  });
  return { local, elements };
}

test('/reset clears the keys of every mount and keeps every history', () => {
  const page = runResetPage('/db/acme/reset/', {
    'graphersal.playground.history': '["root"]',
    'graphersal.playground.tabs': '{}',
    'graphersal.playground.@/db/acme#history': '["acme"]',
    'graphersal.playground.@/db/acme#tabs': '{}',
    'graphersal.playground.@/db/acme#layout': '{}',
    'graphersal.playground.@/db/other#history': '["other"]',
    'graphersal.playground.@/db/other#settings': '{}',
    'graphersal.playground.@/db/x#historyx': '1',
    'other.app.key': 'kept',
  });
  assert.deepEqual(page.local.dump(), {
    'graphersal.playground.history': '["root"]',
    'graphersal.playground.@/db/acme#history': '["acme"]',
    'graphersal.playground.@/db/other#history': '["other"]',
    'graphersal.playground.lastGraph': '"modern"',
    'other.app.key': 'kept',
  });
  assert.equal(page.elements.continue.href, 'http://localhost:8484/db/acme/#/play', 'back to the mount it was opened from');
});

// ------------------------------------------------------------------ /api/info: landing, header

test('the landing view needs admin: true at the root; the dev server is never a landing', () => {
  assert.equal(isLanding({ server: 'graphersal-server', admin: true }), true);
  assert.equal(isLanding({ server: 'graphersal-server', admin: true, mountPath: '' }), true);
  assert.equal(isLanding({ server: SERVER_ID, admin: true, mountPath: '/db/acme' }), false, 'a database page');
  assert.equal(isLanding({ server: SERVER_ID }), false, 'admin absent: inert');
  assert.equal(isLanding({ server: SERVER_ID, admin: 'yes' }), false);
  assert.equal(isLanding(null), false);
});

test('the header name: the database field, else the mount path; none at the root', () => {
  assert.equal(databaseName({ mountPath: '/db/acme' }), 'acme');
  assert.equal(databaseName({ mountPath: '/db/acme', database: 'Acme CRM' }), 'Acme CRM');
  assert.equal(databaseName({ mountPath: '/db/acme', database: '  ' }), 'acme');
  assert.equal(databaseName({ mountPath: '/db/my%20graph/' }), 'my graph');
  assert.equal(databaseName({ mountPath: '/db/bad%zz' }), 'bad%zz');
  assert.equal(databaseName({ database: 'ignored without a mount' }), null);
  assert.equal(databaseName(null), null);
  assert.equal(rootHref('/db/acme'), '../../');
  assert.equal(rootHref('/acme'), '../');
  assert.equal(rootHref(null), '');
  assert.equal(databaseHref('a b/c'), 'db/a%20b%2Fc/');
});

test('the HTTP engine passes mountPath, database and admin on; probe sees a landing', async () => {
  const fake = (info) => async (url) => {
    assert.equal(new URL(url).pathname, '/db/acme/api/info');
    return { ok: true, status: 200, text: async () => JSON.stringify(info) };
  };
  const base = 'http://127.0.0.1:8484/db/acme/';
  const engine = await HttpEngine.detect({
    base, fetch: fake({ server: SERVER_ID, version: '1', mountPath: '/db/acme', mcp: { endpoint: '/db/acme/mcp', readOnly: false, client: null } }),
  });
  const info = await engine.info();
  assert.equal(info.mountPath, '/db/acme');
  assert.equal(info.database, null);
  assert.equal(info.admin, false);
  assert.equal(databaseName(info), 'acme');
  assert.equal(mcpEndpointUrl(info.mcp, 'h:1'), 'http://h:1/db/acme/mcp');
  assert.match(mcpBadgeTitle(info.mcp, 'h:1', 'https:'), /https:\/\/h:1\/db\/acme\/mcp/);

  const plain = await (await HttpEngine.detect({ base, fetch: fake({ server: SERVER_ID, version: '1' }) })).info();
  assert.equal(plain.mountPath, null, 'the dev server announces no mount');
  assert.equal(databaseName(plain), null);

  const probe = await HttpEngine.probe({ base, fetch: fake({ server: 'graphersal-server', admin: true }) });
  assert.equal(isLanding(probe.info), true);
  assert.equal(HttpEngine.fromProbe(probe), null, 'a landing is not a dev server');
});

// ------------------------------------------------------------------ the database list

const DATABASES = {
  databases: [
    { name: 'zeta', state: 'failed', kind: 'plain', readOnly: false, description: '', reason: 'bad file' },
    { name: 'acme', state: 'loaded', kind: 'plain', readOnly: false, description: 'CRM' },
    { name: 'base', state: 'unloaded', kind: 'base', readOnly: true, description: 'shared base' },
    { name: 'live', state: 'loaded', kind: 'plain', readOnly: false, description: '', aliasOf: 'acme' },
    { name: 'docs', state: 'needs-model', kind: 'plain', readOnly: false, description: '' },
    { name: 'next', state: 'loading', kind: 'layer', readOnly: false, description: '' },
    { name: 'future', state: 'hibernating', kind: 'plain' },
    { state: 'loaded' },
    'nonsense',
  ],
};

test('databaseRows: sorted rows with state words, links and tones; malformed entries counted', () => {
  const { rows, skipped } = databaseRows(DATABASES);
  assert.equal(skipped, 2);
  assert.deepEqual(rows.map((r) => r.name), ['acme', 'base', 'docs', 'future', 'live', 'next', 'zeta']);
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by.acme.href, 'db/acme/');
  assert.equal(by.acme.stateText, DATABASE_STATES.loaded);
  assert.equal(by.acme.tone, 'ok');
  assert.equal(by.base.readOnly, true);
  assert.equal(by.docs.stateText, 'needs a model');
  assert.equal(by.zeta.tone, 'danger');
  assert.equal(by.zeta.reason, 'bad file');
  assert.equal(by.live.aliasOf, 'acme');
  assert.equal(by.acme.aliasOf, null);
  assert.equal(by.future.stateText, 'hibernating', 'a state of a newer server is shown as it is');
  assert.equal(anyLoading(rows), true);
  assert.equal(anyLoading(rows.filter((r) => r.state !== 'loading')), false);
  assert.throws(() => databaseRows({}), /no database list/);
});

/** A mock multi-database server: the root answers `admin`, a mount answers a database. */
async function mockServer() {
  const server = http.createServer((req, res) => {
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    if (req.url === '/api/info') return send(200, { server: 'graphersal-server', version: '0.9.0', admin: true });
    if (req.url === '/api/admin/databases') {
      if (req.headers.authorization !== 'Bearer t') return send(401, { error: 'a token is required' });
      return send(200, DATABASES);
    }
    if (req.url === '/db/acme/api/info') return send(200, { server: SERVER_ID, version: '0.9.0', mountPath: '/db/acme', database: 'acme' });
    return send(404, { error: 'not found' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { server, base: `http://127.0.0.1:${server.address().port}/` };
}

test('the landing view reads the list from a mock server with the host headers', async () => {
  const { server, base } = await mockServer();
  try {
    const probe = await HttpEngine.probe({ base, fetch: globalThis.fetch, headers: { Authorization: 'Bearer t' } });
    assert.equal(isLanding(probe.info), true);
    const { rows } = await fetchDatabases({ base, fetch: globalThis.fetch, headers: probe.headers });
    assert.equal(rows.length, 7);
    assert.equal(new URL(rows[0].href, base).href, `${base}db/acme/`);
    await assert.rejects(fetchDatabases({ base, fetch: globalThis.fetch }), /a token is required/);

    const db = await HttpEngine.detect({ base: `${base}db/acme/`, fetch: globalThis.fetch, headers: {} });
    const info = await db.info();
    assert.equal(isLanding(info), false);
    assert.equal(databaseName(info), 'acme');
  } finally {
    server.close();
  }
});

// ------------------------------------------------------------------ read-only from the feed

test('readOnly follows the change feed; the edit mode follows it', () => {
  const info = { server: true, readOnly: false, mountPath: '/db/acme' };
  assert.equal(infoAfterChanges(info, { version: 3 }), null, 'an older server without the field');
  assert.equal(infoAfterChanges(info, { readOnly: false }), null, 'unchanged');
  const on = infoAfterChanges(info, { readOnly: true });
  assert.equal(on.readOnly, true);
  assert.equal(on.mountPath, '/db/acme');
  assert.equal(info.readOnly, false, 'a copy');
  const reason = serverReadOnlyReason(on);
  assert.match(reason, /read-only/);
  assert.match(editModeState({ caps: { edit: true }, readOnly: reason }).reason || '', /Read-only/);
  const off = infoAfterChanges(on, { readOnly: false });
  assert.equal(serverReadOnlyReason(off), null);
  assert.equal(serverReadOnlyReason({ server: false, readOnly: true }), null, 'the WebAssembly engine has no server flag');
});
