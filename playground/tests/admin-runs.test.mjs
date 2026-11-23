// Node tests of the run administration (issue #32): the dialog's pure model (js/admin-runs.js)
// and the HTTP engine's calls with the admin token (js/engine/http-engine.js); the panel shows
// only when /api/info names `adminRuns`.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { adminRunsOf, adminRunsModel, clientText, durationText } from '../js/admin-runs.js';
import { HttpEngine, ADMIN_TOKEN_HEADER } from '../js/engine/http-engine.js';

const BASE = 'http://127.0.0.1:8080/';
const INFO = { protocol: 'graphersal/1', server: 'graphersal-dev', version: '0.7.0', dev: true };

/** A fake fetch: `routes` maps "METHOD path" to a function of (init) -> [status, body]. */
function fakeFetch(routes) {
  const calls = [];
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    const method = init.method || 'GET';
    calls.push({ method, path: u.pathname, init });
    const route = routes[`${method} ${u.pathname}`];
    if (!route) return { ok: false, status: 404, text: async () => '{"error":"unknown endpoint"}' };
    const [status, body] = route(init);
    return { ok: status >= 200 && status < 300, status, text: async () => JSON.stringify(body) };
  };
  return { fetch, calls };
}

test('the panel shows only when /api/info names adminRuns', async () => {
  assert.equal(adminRunsOf(null), null);
  assert.equal(adminRunsOf(INFO), null, 'off by default');
  assert.equal(adminRunsOf({ ...INFO, adminRuns: true }), null, 'only an object counts');
  assert.equal(adminRunsOf({ ...INFO, adminRuns: [] }), null);
  assert.deepEqual(adminRunsOf({ ...INFO, adminRuns: {} }), { tokenRequired: false });
  assert.deepEqual(adminRunsOf({ ...INFO, adminRuns: { tokenRequired: true } }), { tokenRequired: true });

  const off = new HttpEngine(INFO, { base: BASE, fetch: fakeFetch({}).fetch });
  assert.equal((await off.info()).adminRuns, null);
  const on = new HttpEngine({ ...INFO, adminRuns: { tokenRequired: true } }, { base: BASE, fetch: fakeFetch({}).fetch });
  assert.deepEqual((await on.info()).adminRuns, { tokenRequired: true });
});

test('the engine lists and kills with the admin token header, never in the URL', async () => {
  const runs = { runs: [{ id: 7, client: 'mcp:s1', kind: 'tool', state: 'running' }], textHeadChars: 200 };
  const { fetch, calls } = fakeFetch({
    'GET /api/admin/runs': (init) => (init.headers[ADMIN_TOKEN_HEADER] === 's3cret'
      ? [200, runs]
      : [403, { error: 'the run administration needs its token: send the header X-Graphersal-Admin-Token' }]),
    'POST /api/admin/runs/cancel': (init) => [200, { cancelled: JSON.parse(init.body).id === 7 }],
  });
  const engine = new HttpEngine({ ...INFO, adminRuns: { tokenRequired: true } }, { base: BASE, fetch, headers: { Authorization: 'Bearer host' } });
  assert.deepEqual(await engine.adminRuns(' s3cret '), runs);
  const listed = calls.at(-1);
  assert.equal(listed.path, '/api/admin/runs');
  assert.equal(listed.init.headers.Authorization, 'Bearer host', 'the host headers stay');
  assert.equal(listed.init.headers['X-Graphersal-Client'], engine.clientId);
  await assert.rejects(engine.adminRuns(''), (err) => err.status === 403 && /needs its token/.test(err.message));
  assert.equal(ADMIN_TOKEN_HEADER in calls.at(-1).init.headers, false, 'no token: no header');

  assert.deepEqual(await engine.adminKillRun(7, 's3cret'), { cancelled: true });
  const killed = calls.at(-1);
  assert.equal(killed.method, 'POST');
  assert.equal(killed.path, '/api/admin/runs/cancel');
  assert.equal(killed.init.headers['Content-Type'], 'application/json');
  assert.equal(killed.init.headers[ADMIN_TOKEN_HEADER], 's3cret');
  assert.deepEqual(JSON.parse(killed.init.body), { id: 7 });
  for (const call of calls) assert.ok(!call.path.includes('s3cret'));
});

test('the model names clients, durations and the cut query text', () => {
  assert.equal(adminRunsModel(null), null);
  assert.equal(adminRunsModel({}), null);
  assert.equal(clientText('page:abc'), 'page abc');
  assert.equal(clientText('mcp:s-1'), 'MCP s-1');
  assert.equal(clientText('mcp'), 'MCP');
  assert.equal(clientText('ai:c:2'), 'AI Chat c:2');
  assert.equal(clientText('host:x'), 'host:x');
  assert.equal(durationText(850), '850 ms');
  assert.equal(durationText(12400), '12.4 s');
  assert.equal(durationText(185000), '3 min 5 s');
  assert.equal(durationText(7260000), '2 h 1 min');
  assert.equal(durationText(-5), '0 ms');

  const model = adminRunsModel({
    textHeadChars: 5,
    runs: [
      { id: 1, client: 'page:me', alias: 'r1', kind: 'query', state: 'running', cancelled: false, startedAtMs: 1700000000000, durationMs: 1500, text: 'g.v()', identity: null, mount: null },
      { id: 2, client: 'ai:c', kind: 'tool', state: 'queued', cancelled: true, durationMs: 3, text: 'g.e', identity: 'alice', mount: '/db/a' },
      { id: 3, client: 'page:x', kind: 'layout', state: 'running', cancelled: false, durationMs: 0, text: null },
    ],
  }, 'me');
  assert.equal(model.textHeadChars, 5);
  const [own, cancelled, layout] = model.rows;
  assert.equal(own.client, 'page me');
  assert.equal(own.own, true);
  assert.equal(own.state, 'running');
  assert.equal(own.duration, '1.5 s');
  assert.equal(own.truncated, true, 'as long as the head: the server may have cut it');
  assert.ok(own.started);
  assert.equal(cancelled.own, false);
  assert.equal(cancelled.state, 'queued, cancelled');
  assert.equal(cancelled.cancelled, true);
  assert.equal(cancelled.truncated, false);
  assert.equal(cancelled.identity, 'alice');
  assert.equal(cancelled.mount, '/db/a');
  assert.equal(cancelled.started, '');
  assert.equal(layout.text, '');
  assert.equal(layout.kind, 'layout');
  assert.equal(adminRunsModel({ runs: [] }).rows.length, 0);
});
