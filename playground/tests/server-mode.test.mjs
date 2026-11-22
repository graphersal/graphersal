// Node tests of server mode (js/server-mode.js, js/util.js storage namespace, the HTTP engine's
// probe, its host-neutral detection and the /reset page with mounts): the playground served by a
// multi-database server under `/db/<name>/`, whose own root page is a UI profile of that host.
// A mock server (node:http on port 0) and fake fetches stand in for the server.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import vm from 'node:vm';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { setStorageNamespace, storage, storageKey, normalizeMountPath } from '../js/util.js';
import {
  databaseName, rootHref, homeHref, infoAfterChanges, serverReadOnlyReason,
} from '../js/server-mode.js';
import { HttpEngine, SERVER_ID, isEngineInfo } from '../js/engine/http-engine.js';
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

// ------------------------------------------------------------------ /api/info: header, home, host

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
});

test('homeHref: home is a path from the server root, linked relative to the mount', () => {
  assert.equal(homeHref({ mountPath: '/db/acme', home: '/' }), '../../');
  assert.equal(homeHref({ mountPath: '/db/acme/', home: '/admin/' }), '../../admin/');
  assert.equal(homeHref({ mountPath: '/acme', home: '/ui/index.html' }), '../ui/index.html');
  assert.equal(homeHref({ home: '/' }), './', 'at the root: the page itself');
  assert.equal(homeHref({ home: '/admin/' }), 'admin/');
  // A proxy prefix in front of the server: the relative link keeps it.
  const page = 'https://example.test/proxy/db/acme/';
  assert.equal(new URL(homeHref({ mountPath: '/db/acme', home: '/' }), page).href, 'https://example.test/proxy/');
  assert.equal(new URL(homeHref({ mountPath: '/db/acme', home: '/admin/' }), page).href, 'https://example.test/proxy/admin/');
  for (const home of [undefined, null, '', 'admin/', '//evil.test/', 'https://evil.test/', '/a/../b', '/./', '/a?b', '/a#b', '/a b', '/a\\b', 42, true]) {
    assert.equal(homeHref({ mountPath: '/db/acme', home }), null, `ignored: ${JSON.stringify(home)}`);
  }
  assert.equal(homeHref(null), null);
  assert.equal(homeHref({ mountPath: '/db/acme', admin: true }), null, 'admin is no home link any more');
});

test('the HTTP engine passes mountPath, database, home and the raw info on', async () => {
  const fake = (info) => async (url) => {
    assert.equal(new URL(url).pathname, '/db/acme/api/info');
    return { ok: true, status: 200, text: async () => JSON.stringify(info) };
  };
  const base = 'http://127.0.0.1:8484/db/acme/';
  const raw = { server: SERVER_ID, version: '1', mountPath: '/db/acme', home: '/', uploadBytes: 4096, mcp: { endpoint: '/db/acme/mcp', readOnly: false, client: null } };
  const engine = await HttpEngine.detect({ base, fetch: fake(raw) });
  const info = await engine.info();
  assert.equal(info.mountPath, '/db/acme');
  assert.equal(info.database, null);
  assert.equal(info.home, '/');
  assert.equal(info.admin, undefined, 'the admin field is gone');
  assert.deepEqual(info.host, raw, 'the whole answer as the host sent it');
  assert.notEqual(info.host, engine.serverInfo, 'a copy');
  assert.equal(homeHref(info), '../../');
  assert.equal(databaseName(info), 'acme');
  assert.equal(mcpEndpointUrl(info.mcp, 'h:1'), 'http://h:1/db/acme/mcp');
  assert.match(mcpBadgeTitle(info.mcp, 'h:1', 'https:'), /https:\/\/h:1\/db\/acme\/mcp/);

  const plain = await (await HttpEngine.detect({ base, fetch: fake({ server: SERVER_ID, version: '1' }) })).info();
  assert.equal(plain.mountPath, null, 'the dev server announces no mount');
  assert.equal(plain.home, null);
  assert.equal(databaseName(plain), null);
  assert.deepEqual(plain.host, { server: SERVER_ID, version: '1' });
});

test('isEngineInfo: the dev server, or another named host that delivers a UI profile', () => {
  assert.equal(isEngineInfo({ server: SERVER_ID }), true, 'the dev server as always');
  assert.equal(isEngineInfo({ server: SERVER_ID, uiProfile: null }), true);
  assert.equal(isEngineInfo({ server: 'graphersal-server', uiProfile: { name: 'server' } }), true);
  assert.equal(isEngineInfo({ server: 'graphersal-server', uiProfile: 'demo' }), true);
  assert.equal(isEngineInfo({ server: 'graphersal-server' }), false, 'no profile: not a host for this page');
  assert.equal(isEngineInfo({ server: 'graphersal-server', uiProfile: null }), false);
  assert.equal(isEngineInfo({ server: '  ', uiProfile: 'demo' }), false, 'the host names itself');
  assert.equal(isEngineInfo({ uiProfile: 'demo' }), false);
  assert.equal(isEngineInfo({ server: 7, uiProfile: 'demo' }), false);
  assert.equal(isEngineInfo([]), false);
  assert.equal(isEngineInfo(null), false);
  assert.equal(isEngineInfo('graphersal-dev'), false);
});

/** A mock multi-database server: the root delivers its own UI profile, a mount answers a database. */
async function mockServer() {
  const server = http.createServer((req, res) => {
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    if (req.url === '/api/info') {
      if (req.headers.authorization !== 'Bearer t') return send(401, { error: 'a token is required' });
      return send(200, {
        server: 'graphersal-server', version: '0.9.0', uploadBytes: 1024,
        uiProfile: { name: 'server', title: 'Server', startTab: 'list', tabs: [{ id: 'list', title: 'List', module: 'list.js' }], files: 'server-ui/' },
      });
    }
    if (req.url === '/db/acme/api/info') return send(200, { server: SERVER_ID, version: '0.9.0', mountPath: '/db/acme', database: 'acme', home: '/' });
    return send(404, { error: 'not found' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { server, base: `http://127.0.0.1:${server.address().port}/` };
}

test('a host that is not the dev server delivers its profile; its mounts link home', async () => {
  const { server, base } = await mockServer();
  try {
    const root = await HttpEngine.detect({ base, fetch: globalThis.fetch, headers: { Authorization: 'Bearer t' } });
    assert.ok(root, 'graphersal-server with a uiProfile is an engine host');
    const info = await root.info();
    assert.equal(info.server, true);
    assert.equal(info.uiProfile.name, 'server');
    assert.equal(info.host.server, 'graphersal-server');
    assert.equal(info.host.uploadBytes, 1024, 'a field of the host\'s own reaches the profile');
    assert.equal(await HttpEngine.detect({ base, fetch: globalThis.fetch, headers: {} }), null, 'refused: no engine');

    const db = await HttpEngine.detect({ base: `${base}db/acme/`, fetch: globalThis.fetch, headers: {} });
    const dbInfo = await db.info();
    assert.equal(databaseName(dbInfo), 'acme');
    assert.equal(new URL(homeHref(dbInfo), `${base}db/acme/`).href, base);
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
