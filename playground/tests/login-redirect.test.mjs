// Node tests of the generic 401 handling of the HTTP engine (js/engine/http-engine.js
// `loginTarget`/`loginRedirect`): a host that requires a login answers 401 with
// `{"error", "login": "/"}`, and the page goes there with a return path. Against a fake fetch
// (a mock host) and a fake location.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { HttpEngine, loginRedirect, loginTarget } from '../js/engine/http-engine.js';

const BASE = 'http://127.0.0.1:8080/db/acme/';
const INFO = { protocol: 'graphersal/1', server: 'graphersal-server', version: '0.1.0', mountPath: '/db/acme' };

/** A fake page location at `path` (pathname + search) recording `assign`. */
function fakeLocation(path = '/db/acme/?profile=full') {
  const url = new URL(path, 'http://127.0.0.1:8080');
  const assigned = [];
  return {
    href: url.href,
    origin: url.origin,
    pathname: url.pathname,
    search: url.search,
    assign: (to) => assigned.push(to),
    assigned,
  };
}

/** A mock host: `routes` maps "METHOD path" to [status, body]; anything else is 401 `refusal`. */
function mockHost(routes, refusal) {
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    const route = routes[`${init.method || 'GET'} ${u.pathname}`];
    const [status, body] = route || [401, refusal];
    const text = body === undefined ? '' : (typeof body === 'string' ? body : JSON.stringify(body));
    return {
      ok: status >= 200 && status < 300,
      status,
      text: async () => text,
      arrayBuffer: async () => new TextEncoder().encode(text).buffer,
    };
  };
  return fetch;
}

const LOGIN = { error: 'log in to open this database', login: '/' };

test('loginTarget: a 401 naming a same-origin path, with the return path', () => {
  const at = fakeLocation('/db/acme/?profile=full');
  assert.equal(loginTarget({ status: 401 }, LOGIN, at), '/?next=%2Fdb%2Facme%2F%3Fprofile%3Dfull');
  assert.equal(loginTarget({ status: 401 }, { login: '/login?lang=en' }, at),
    '/login?lang=en&next=%2Fdb%2Facme%2F%3Fprofile%3Dfull', 'a query of its own: &next=');
  assert.equal(loginTarget({ status: 401 }, { login: '/login#form' }, at),
    '/login?next=%2Fdb%2Facme%2F%3Fprofile%3Dfull#form', 'the fragment stays last');
  assert.equal(loginTarget({ status: 401 }, LOGIN, fakeLocation('/db/acme/')), '/?next=%2Fdb%2Facme%2F');
});

test('loginTarget: nothing for other statuses, bodies without login, foreign or odd paths', () => {
  const at = fakeLocation();
  assert.equal(loginTarget({ status: 403 }, LOGIN, at), null, 'only 401');
  assert.equal(loginTarget({ status: 404 }, LOGIN, at), null);
  assert.equal(loginTarget({ status: 401 }, { error: 'a token is needed' }, at), null, 'no login');
  assert.equal(loginTarget({ status: 401 }, null, at), null, 'no JSON');
  assert.equal(loginTarget({ status: 401 }, [LOGIN], at), null);
  for (const login of ['//evil.example/', 'https://evil.example/', 'http:/x', 'login', '', ' /',
    '/\\evil.example', '/\\/evil.example', '/a b', '/x\n', '/x\t', 42, null, { path: '/' }]) {
    assert.equal(loginTarget({ status: 401 }, { login }, at), null, JSON.stringify(login));
  }
  assert.equal(loginTarget({ status: 401 }, LOGIN, null), null, 'no location (Node)');
  assert.equal(loginRedirect({ status: 401 }, LOGIN, undefined), null, 'no location: nothing happens');
});

test('loginTarget: never a loop to the page it is on', () => {
  assert.equal(loginTarget({ status: 401 }, { login: '/' }, fakeLocation('/')), null);
  assert.equal(loginTarget({ status: 401 }, { login: '/?next=%2Fx' }, fakeLocation('/?next=%2Fx')), null);
  assert.equal(loginTarget({ status: 401 }, { login: '/login' }, fakeLocation('/login?next=%2Fdb%2Fa%2F')), null);
});

test('loginRedirect assigns the target once; null without one', () => {
  const at = fakeLocation();
  assert.equal(loginRedirect({ status: 401 }, LOGIN, at), '/?next=%2Fdb%2Facme%2F%3Fprofile%3Dfull');
  assert.deepEqual(at.assigned, ['/?next=%2Fdb%2Facme%2F%3Fprofile%3Dfull']);
  assert.equal(loginRedirect({ status: 401 }, { login: '//evil.example/' }, at), null);
  assert.equal(at.assigned.length, 1);
});

test('probe: a 401 with login redirects and starts no engine; without login it falls back as today', async () => {
  const at = fakeLocation();
  const fetch = mockHost({}, LOGIN);
  const probe = await HttpEngine.probe({ base: BASE, fetch, location: at, headers: {} });
  assert.deepEqual(probe, { redirect: '/?next=%2Fdb%2Facme%2F%3Fprofile%3Dfull' });
  assert.deepEqual(at.assigned, ['/?next=%2Fdb%2Facme%2F%3Fprofile%3Dfull']);
  assert.equal(HttpEngine.fromProbe(probe), null, 'no engine');

  for (const refusal of [{ error: 'a token is needed' }, { error: 'x', login: '//evil.example/' }, 'not json', undefined]) {
    const page = fakeLocation();
    const none = await HttpEngine.probe({ base: BASE, fetch: mockHost({}, refusal), location: page, headers: {} });
    assert.equal(none, null, `fallback to wasm for ${JSON.stringify(refusal)}`);
    assert.deepEqual(page.assigned, []);
  }
  const loop = fakeLocation('/');
  assert.equal(await HttpEngine.probe({ base: 'http://127.0.0.1:8080/', fetch, location: loop, headers: {} }), null,
    'already on the login path: no loop, the fallback of today');
  assert.deepEqual(loop.assigned, []);
});

test('request, requestBytes and saveCatalog: a 401 with login redirects and still rejects', async () => {
  const at = fakeLocation();
  const fetch = mockHost({ 'GET /db/acme/api/info': [200, INFO] }, LOGIN);
  const engine = new HttpEngine(INFO, { base: BASE, fetch, location: at, headers: {} });
  await assert.rejects(engine.changes(), (err) => err.status === 401 && err.message === LOGIN.error);
  await assert.rejects(engine.requestBytes('store/snapshot?commit=1'), (err) => err.status === 401 && err.message === LOGIN.error);
  await assert.rejects(engine.saveCatalog(), (err) => err.status === 401);
  const target = '/?next=%2Fdb%2Facme%2F%3Fprofile%3Dfull';
  assert.deepEqual(at.assigned, [target, target, target]);
});

test('request: a 401 without login, other errors and foreign logins keep today\'s errors', async () => {
  const at = fakeLocation();
  const plain = new HttpEngine(INFO, { base: BASE, fetch: mockHost({}, { error: 'a token is needed' }), location: at, headers: {} });
  await assert.rejects(plain.changes(), (err) => err.status === 401 && err.message === 'a token is needed');
  await assert.rejects(plain.requestBytes('store/snapshot?commit=1'), /a token is needed/);
  await assert.rejects(plain.saveCatalog(), /HTTP 401/);
  const foreign = new HttpEngine(INFO, { base: BASE, fetch: mockHost({}, { error: 'no', login: 'https://evil.example/' }), location: at, headers: {} });
  await assert.rejects(foreign.changes(), /no/);
  const forbidden = new HttpEngine(INFO, { base: BASE, fetch: mockHost({ 'GET /db/acme/api/changes': [403, LOGIN] }), location: at, headers: {} });
  await assert.rejects(forbidden.changes(), (err) => err.status === 403);
  assert.deepEqual(at.assigned, [], 'nothing navigated');
});
