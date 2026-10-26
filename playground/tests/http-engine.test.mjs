// Node tests of the HTTP engine (js/engine/http-engine.js, the dev server mode) against a fake
// fetch, and of the readiness gate (js/ready.js) that makes early engine calls wait.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { HttpEngine, SERVER_ID } from '../js/engine/http-engine.js';
import { createReadyGate } from '../js/ready.js';

const BASE = 'http://127.0.0.1:8080/';

/** A fake fetch: `routes` maps "METHOD path" to [status, body]; every call is recorded. */
function fakeFetch(routes) {
  const calls = [];
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    const method = init.method || 'GET';
    calls.push({ method, path: u.pathname + u.search, init });
    const key = `${method} ${u.pathname}`;
    const route = routes[key];
    if (!route) return { ok: false, status: 404, text: async () => '{"error":"not found"}' };
    const [status, body] = typeof route === 'function' ? route(init, u) : route;
    return { ok: status >= 200 && status < 300, status, text: async () => (typeof body === 'string' ? body : JSON.stringify(body)) };
  };
  return { fetch, calls };
}

const INFO = { server: SERVER_ID, version: '0.1.0', memoryTracked: true, saveable: true, source: '/tmp/g.json', dev: true };

test('detect: a dev server answers /api/info; anything else is the wasm playground', async () => {
  const { fetch } = fakeFetch({ 'GET /api/info': [200, INFO] });
  const engine = await HttpEngine.detect({ base: BASE, fetch });
  assert.ok(engine instanceof HttpEngine);
  const info = await engine.info();
  assert.equal(info.name, 'server');
  assert.equal(info.server, true);
  assert.equal(info.saveable, true);

  // A static server: 404, HTML, foreign JSON, a network error.
  assert.equal(await HttpEngine.detect({ base: BASE, fetch: fakeFetch({}).fetch }), null);
  const html = fakeFetch({ 'GET /api/info': [200, '<!doctype html>'] });
  assert.equal(await HttpEngine.detect({ base: BASE, fetch: html.fetch }), null);
  const other = fakeFetch({ 'GET /api/info': [200, { server: 'nginx' }] });
  assert.equal(await HttpEngine.detect({ base: BASE, fetch: other.fetch }), null);
  const down = async () => { throw new TypeError('Failed to fetch'); };
  assert.equal(await HttpEngine.detect({ base: BASE, fetch: down }), null);
});

test('execute posts the script and options as JSON and returns the server response', async () => {
  const response = { ok: true, terminal: 'execute', execution: { results: [1], error: null, profile: null }, graph: { vertexCount: 6 } };
  const { fetch, calls } = fakeFetch({ 'POST /api/execute': [200, response] });
  const engine = new HttpEngine(INFO, { base: BASE, fetch });
  const res = await engine.execute('g.v().count()', { profile: true });
  assert.deepEqual(res, response);
  assert.equal(calls[0].init.headers['Content-Type'], 'application/json');
  assert.deepEqual(JSON.parse(calls[0].init.body), { script: 'g.v().count()', options: { profile: true } });
  assert.deepEqual(await engine.graphInfo().catch(() => 'no route'), 'no route');
});

test('statistics is one GET; setSchema passes the mode as a query parameter', async () => {
  const stats = { statistics: { vertex_count: 6 }, memory: null, limits: { presets: { options: {} }, defaults: {} } };
  const info = { name: 'g', schemaMode: 'closed' };
  const { fetch, calls } = fakeFetch({ 'GET /api/statistics': [200, stats], 'POST /api/schema/set': [200, info] });
  const engine = new HttpEngine(INFO, { base: BASE, fetch });
  assert.deepEqual(await engine.statistics(), stats);
  assert.deepEqual(await engine.setSchema({ mode: 'none' }, 'closed'), info);
  assert.equal(calls[1].path, '/api/schema/set?mode=closed');
  assert.equal(calls[1].init.body, '{"mode":"none"}');
  await engine.setSchema('{}');
  assert.equal(calls[2].path, '/api/schema/set');
});

test('stop cancels on the server and flags the cancelled run as stopped', async () => {
  let release;
  const pending = new Promise((r) => { release = r; });
  const { fetch, calls } = fakeFetch({
    'POST /api/execute': [200, { ok: false, terminal: 'execute', execution: { results: [], error: { message: 'cancelled' }, profile: null } }],
    'POST /api/cancel': [200, { cancelled: true }],
  });
  // The execute answer arrives only after the cancel.
  const slowFetch = async (url, init) => {
    if (String(url).endsWith('/api/execute')) await pending;
    return fetch(url, init);
  };
  const engine = new HttpEngine(INFO, { base: BASE, fetch: slowFetch });
  const running = engine.execute('loop {}', {});
  const stopped = await engine.stop();
  assert.deepEqual(stopped, { restored: null, cancelled: true });
  release();
  const res = await running;
  assert.equal(res.stopped, true);
  assert.deepEqual(calls.map((c) => `${c.method} ${c.path}`), ['POST /api/cancel', 'POST /api/execute']);
});

test('the schema methods, views and saving map to their endpoints', async () => {
  const schema = { mode: 'open', vertices: {} };
  const { fetch, calls } = fakeFetch({
    'GET /api/schema/stored': [200, 'null'],
    'GET /api/schema/infer': [200, schema],
    'POST /api/schema/set': [200, { vertexCount: 6, schemaMode: 'open' }],
    'POST /api/schema/patch': [200, schema],
    'POST /api/schema/validate': [200, { violations: {}, diff: [], compatible: true }],
    'POST /api/schema/validate-patch': [200, { violations: {}, diff: [], compatible: true }],
    'POST /api/schema/diff': [200, { changes: [], compatible: true }],
    'GET /api/schema': [200, { mode: 'open', source: 'stored' }],
    'GET /api/graph': [200, { vertexCount: 6, schemaMode: 'open' }],
    'GET /api/graph-view': [200, { vertexIds: ['1'], vertexLabel: [0], vertexName: [null], edgeIds: ['0'], edgeSource: [0], edgeTarget: [0], edgeLabel: [1], labels: ['a', 'b'] }],
    'GET /api/element': (init, u) => [200, { kind: u.searchParams.get('kind'), id: u.searchParams.get('id'), properties: { name: 'marko' } }],
    'GET /api/completions': [200, []],
    'GET /api/token-values': [200, { Order: ['asc', 'desc', 'shuffle'] }],
    'GET /api/memory': [200, { total_bytes: 1 }],
    'GET /api/export': (init, u) => [200, { text: u.searchParams.get('format'), schema: null, warnings: [] }],
    'POST /api/save': [200, { path: '/tmp/g.json', format: 'graphson', bytes: 10, warnings: [] }],
  });
  const engine = new HttpEngine(INFO, { base: BASE, fetch });
  assert.equal(await engine.getSchema(), null);
  assert.deepEqual(await engine.inferSchema(), schema);
  assert.equal((await engine.setSchema(schema)).schemaMode, 'open');
  assert.deepEqual(await engine.patchSchema({ mode: 'open' }), schema);
  assert.equal((await engine.validateSchema(schema)).compatible, true);
  assert.equal((await engine.validateSchemaPatch('{"mode":"closed"}')).compatible, true);
  assert.equal((await engine.diffSchema(schema)).compatible, true);
  assert.equal((await engine.schema()).source, 'stored');
  const view = await engine.graphView({ maxNodes: 3, maxEdges: 4 });
  assert.ok(view.edgeSource instanceof Uint32Array, 'typed index columns');
  assert.deepEqual(Array.from(view.edgeSource), [0]);
  assert.deepEqual((await engine.element('vertex', '1')).properties, { name: 'marko' });
  assert.deepEqual(await engine.completions(), []);
  assert.deepEqual(await engine.tokenValues(), { Order: ['asc', 'desc', 'shuffle'] });
  assert.equal((await engine.memoryUsage()).total_bytes, 1);
  assert.deepEqual(await engine.saveGraph('graphml'), { text: 'graphml', schema: null, warnings: [], format: 'graphml' });
  assert.equal((await engine.saveGraph()).format, 'graphson');
  assert.equal((await engine.saveToSource()).path, '/tmp/g.json');
  const sent = calls.find((c) => c.path === '/api/schema/set');
  assert.deepEqual(JSON.parse(sent.init.body), schema);
  assert.ok(calls.some((c) => c.path === '/api/graph-view?maxNodes=3&maxEdges=4'));
  assert.ok(calls.some((c) => c.path === '/api/element?kind=vertex&id=1'));
  // patchSchema refreshes the graph info (schema mode).
  assert.ok(calls.some((c) => c.path === '/api/graph'));
});

test('server errors reject with the server message; the graph cannot be replaced', async () => {
  const { fetch } = fakeFetch({ 'POST /api/schema/set': [422, { error: 'Error: Invalid schema at /mode' }] });
  const engine = new HttpEngine(INFO, { base: BASE, fetch });
  await assert.rejects(engine.setSchema({ mode: 'bogus' }), /Invalid schema at \/mode/);
  await assert.rejects(engine.loadGraph({ kind: 'modern' }), /--graph/);
  assert.deepEqual(await engine.listSampleGraphs(), []);
  const down = new HttpEngine(INFO, { base: BASE, fetch: async () => { throw new TypeError('Failed to fetch'); } });
  await assert.rejects(down.graphInfo(), /not reachable/);
});

test('ready gate: an action started before the engine exists waits for it', async () => {
  const gate = createReadyGate();
  const order = [];
  let engine = null;
  const early = (async () => {
    await gate.ready;
    order.push(engine ? 'ran with engine' : 'ran without engine');
  })();
  await new Promise((r) => setTimeout(r, 5));
  assert.deepEqual(order, []);
  assert.equal(gate.isOpen(), false);
  engine = {};
  gate.open();
  gate.open(); // idempotent
  await early;
  assert.deepEqual(order, ['ran with engine']);
  assert.equal(gate.isOpen(), true);
});

test('a Store directory: store info, checkpoint, mark, snapshot downloads', async () => {
  const store = { dir: '/tmp/data', commitSeq: 4, snapshots: [{ commitSeq: 2, name: 'm' }], marks: [] };
  const gsnap = new Uint8Array([71, 82, 83, 76]);
  const routes = {
    'GET /api/store': [200, { store }],
    'POST /api/store/checkpoint': (init) => [200, { snapshot: { commitSeq: 4, name: JSON.parse(init.body).name } }],
    'POST /api/store/mark': (init) => [200, { mark: { name: JSON.parse(init.body).name, commitSeq: 4 } }],
  };
  const { fetch: jsonFetch, calls } = fakeFetch(routes);
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    if (u.pathname === '/api/export' && u.searchParams.get('format') === 'gsnap') {
      return { ok: true, status: 200, arrayBuffer: async () => gsnap.buffer };
    }
    return jsonFetch(url, init);
  };
  const engine = new HttpEngine({ ...INFO, store: '/tmp/data' }, { base: BASE, fetch });
  assert.equal((await engine.info()).store, '/tmp/data');
  assert.deepEqual(await engine.storeInfo(), store);
  assert.deepEqual(await engine.checkpoint('nightly'), { commitSeq: 4, name: 'nightly' });
  assert.deepEqual(await engine.mark('m2'), { name: 'm2', commitSeq: 4 });
  assert.equal(calls[1].init.headers['Content-Type'], 'application/json');
  assert.equal(engine.snapshotUrl(2), `${BASE}api/store/snapshot?commit=2`);
  const saved = await engine.saveGraph('snapshot');
  assert.equal(saved.format, 'snapshot');
  assert.deepEqual([...saved.bytes], [...gsnap]);
  // Without a store the server answers null.
  const plain = new HttpEngine(INFO, { base: BASE, fetch: fakeFetch({ 'GET /api/store': [200, { store: null }] }).fetch });
  assert.equal(await plain.storeInfo(), null);
  assert.equal((await plain.info()).maintenance, null);
});

test('a damaged store: info() carries the maintenance report for the banner', async () => {
  const maintenance = { recoverable: false, commitSeq: 8, damage: [{ kind: 'wal_record', lost: true, text: 'x' }] };
  const engine = new HttpEngine({ ...INFO, store: '/tmp/data', maintenance }, { base: BASE, fetch: fakeFetch({}).fetch });
  const info = await engine.info();
  assert.equal(info.store, '/tmp/data');
  assert.deepEqual(info.maintenance, maintenance);
});

test('a backup directory: info() carries its marker for the read-only banner', async () => {
  const backup = { commitSeq: 7, takenText: '2026-10-08T04:57:01Z', restore: 'graphersal store restore /tmp/bk' };
  const engine = new HttpEngine({ ...INFO, store: '/tmp/bk', backup }, { base: BASE, fetch: fakeFetch({}).fetch });
  const info = await engine.info();
  assert.deepEqual(info.backup, backup);
  const plain = new HttpEngine(INFO, { base: BASE, fetch: fakeFetch({}).fetch });
  assert.equal((await plain.info()).backup, null);
});

test('time travel on a Store: view, back to live, fork, rollback and the attic', async () => {
  const view = { commitSeq: 2, label: 'Read-only view of commit 2', timeText: '2026-10-08T02:43:00Z' };
  const body = (init) => JSON.parse(init.body);
  const { fetch, calls } = fakeFetch({
    'POST /api/store/view': (init) => [200, { view: { ...view, target: body(init).target }, graph: { name: view.label, vertexCount: 2 } }],
    'POST /api/store/live': [200, { view: null, graph: { name: '/tmp/data', vertexCount: 5 } }],
    'POST /api/store/fork': (init) => [200, { fork: { dir: body(init).dir || '/tmp/data-fork-commit-2', commitSeq: 2, open: 'graphersal --graph /tmp/data-fork-commit-2 --server --port 0' } }],
    'POST /api/store/rollback': (init) => [200, { rollback: { target: 2, previousCommitSeq: 5, attic: body(init).delete ? null : 'a1' }, graph: { vertexCount: 2 }, store: { commitSeq: 2 } }],
    'GET /api/store/attic/changes': (init, u) => [200, { id: u.searchParams.get('id'), commitCount: 3, commits: [] }],
    'POST /api/store/verify': [200, { verify: { ok: true, problems: [], notes: [] } }],
    'GET /api/store/compaction': [200, { compaction: { recommended: false, reason: 'small' } }],
    'POST /api/store/compact': [200, { compact: { bytesFreed: 10 }, compaction: { recommended: false } }],
    'POST /api/store/attic/restore': [200, { restored: { commitSeq: 5 }, graph: { vertexCount: 5 }, store: { commitSeq: 5 } }],
    'POST /api/store/attic/fork': (init) => [200, { fork: { dir: '/tmp/x', commitSeq: 5, id: body(init).id } }],
    'POST /api/store/attic/remove': (init) => [200, { removed: body(init).id }],
  });
  const engine = new HttpEngine({ ...INFO, store: '/tmp/data', view: null }, { base: BASE, fetch });
  assert.equal((await engine.info()).view, null);

  const opened = await engine.openView({ mark: 'm1' });
  assert.deepEqual(opened.view.target, { mark: 'm1' });
  assert.equal((await engine.info()).view.commitSeq, 2, 'info() follows the view');
  assert.equal(engine.graph.vertexCount, 2);
  await engine.closeView();
  assert.equal((await engine.info()).view, null);
  assert.equal(engine.graph.vertexCount, 5);

  const fork = await engine.fork({ commit: 2 });
  assert.equal(fork.fork.dir, '/tmp/data-fork-commit-2');
  assert.deepEqual(body(calls.at(-1).init), { target: { commit: 2 }, dir: '' });
  await engine.fork({ time: '2026-10-08T02:43Z' }, 'branch');
  assert.deepEqual(body(calls.at(-1).init), { target: { time: '2026-10-08T02:43Z' }, dir: 'branch' });

  await engine.openView({ commit: 2 });
  const rolled = await engine.rollback({ commit: 2 });
  assert.equal(rolled.rollback.attic, 'a1');
  assert.deepEqual(body(calls.at(-1).init), { target: { commit: 2 }, delete: false });
  assert.equal((await engine.info()).view, null, 'a rollback ends a view');
  const deleted = await engine.rollback({ commit: 1 }, true);
  assert.equal(deleted.rollback.attic, null);
  assert.equal(body(calls.at(-1).init).delete, true);

  const changes = await engine.atticChanges('2026 a/b');
  assert.equal(changes.id, '2026 a/b');
  assert.equal(calls.at(-1).path, '/api/store/attic/changes?id=2026%20a%2Fb&offset=0&limit=100');
  await engine.atticChanges('a1', 100, 50);
  assert.equal(calls.at(-1).path, '/api/store/attic/changes?id=a1&offset=100&limit=50');
  assert.equal((await engine.verifyStore()).verify.ok, true);
  assert.equal(calls.at(-1).path, '/api/store/verify');
  assert.equal((await engine.compactionAdvice()).compaction.recommended, false);
  assert.equal(calls.at(-1).path, '/api/store/compaction');
  assert.equal(calls.at(-1).method, 'GET');
  assert.equal((await engine.compactStore()).compact.bytesFreed, 10);
  assert.equal(calls.at(-1).path, '/api/store/compact');
  assert.equal(body(calls.at(-1).init).dropMarks, false);
  await engine.compactStore({ dropMarks: true });
  assert.equal(body(calls.at(-1).init).dropMarks, true);
  assert.equal((await engine.atticRestore('a1')).restored.commitSeq, 5);
  assert.equal(engine.graph.vertexCount, 5);
  assert.equal((await engine.atticFork('a1')).fork.id, 'a1');
  assert.equal((await engine.atticRemove('a1')).removed, 'a1');
  for (const c of calls.filter((c) => c.method === 'POST')) {
    assert.equal(c.init.headers['Content-Type'], 'application/json');
  }
});

test('time-travel errors reject with the server message', async () => {
  const { fetch } = fakeFetch({
    'POST /api/store/fork': [409, { error: "'/tmp/b' exists already: a fork needs a new directory" }],
    'POST /api/store/attic/restore': [422, { error: 'Error: cannot restore the attic entry: the store has new commits since the rollback' }],
  });
  const engine = new HttpEngine({ ...INFO, store: '/tmp/data' }, { base: BASE, fetch });
  await assert.rejects(engine.fork({ commit: 1 }, '/tmp/b'), /exists already/);
  await assert.rejects(engine.atticRestore('a1'), /new commits/);
});

test('saved queries map to /api/queries*', async () => {
  const entry = { name: 'q', folder: 'a/b', params: [], status: 'ok' };
  const { fetch, calls } = fakeFetch({
    'GET /api/queries': [200, [entry]],
    'GET /api/queries/get': [200, entry],
    'POST /api/queries/define': [200, { ok: false, errors: [{ field: 'name', message: 'bad' }] }],
    'POST /api/queries/drop': [200, { dropped: true }],
    'POST /api/queries/move': [200, entry],
    'POST /api/queries/call-text': [200, { text: 'g.query("q", #{n: 1})' }],
    'GET /api/catalog/export': [200, '{"definitions":[]}\n'],
    'POST /api/catalog/load': [200, { loaded: 1, graph: { savedQueries: 1 } }],
  });
  const engine = new HttpEngine(INFO, { base: BASE, fetch });
  assert.deepEqual(await engine.listQueries('a/b'), [entry]);
  assert.ok(calls.some((c) => c.path === '/api/queries?folder=a%2Fb'));
  assert.equal((await engine.getQuery('q')).name, 'q');
  assert.equal((await engine.defineQuery({ name: '1' })).errors[0].field, 'name');
  assert.equal((await engine.dropQuery('q')).dropped, true);
  assert.equal((await engine.moveQuery('q', 'a/b')).folder, 'a/b');
  assert.equal(await engine.queryCallText('q', { n: 1 }), 'g.query("q", #{n: 1})');
  assert.deepEqual(JSON.parse(calls.find((c) => c.path === '/api/queries/call-text').init.body), { name: 'q', args: { n: 1 } });
  assert.equal(await engine.saveCatalog(), '{"definitions":[]}\n');
  const file = { name: 'q.json', text: async () => '{"definitions":[]}' };
  assert.equal((await engine.loadCatalog(file)).loaded, 1);
  await assert.rejects(engine.loadCatalog({ name: 'q.txt', text: async () => '' }), /JSON/);
});

test('compression rules map to /api/compression*', async () => {
  const rule = { name: 'cv', element: 'vertex', label: 'customer', path: '$.cv', minBytes: 256, dictionary: true, stats: {} };
  const { fetch, calls } = fakeFetch({
    'GET /api/compression': [200, [rule]],
    'POST /api/compression/define': [200, { ok: true, rule }],
    'POST /api/compression/drop': [200, { dropped: true }],
    'POST /api/compression/recompress': [200, rule],
  });
  const engine = new HttpEngine(INFO, { base: BASE, fetch });
  assert.deepEqual(await engine.listCompressions(), [rule]);
  assert.equal((await engine.defineCompression({ name: 'cv', label: 'customer', path: 'cv' })).ok, true);
  assert.deepEqual(JSON.parse(calls.find((c) => c.path === '/api/compression/define').init.body).path, 'cv');
  assert.equal((await engine.dropCompression('cv')).dropped, true);
  assert.deepEqual(JSON.parse(calls.find((c) => c.path === '/api/compression/drop').init.body), { name: 'cv' });
  assert.equal((await engine.recompress('cv')).name, 'cv');
});

test('a compression estimate maps to POST /api/compression/estimate', async () => {
  const answer = { ok: true, estimate: { valuesFound: 4, candidates: [], best: null, graphChanged: false } };
  const { fetch, calls } = fakeFetch({ 'POST /api/compression/estimate': [200, answer] });
  const engine = new HttpEngine(INFO, { base: BASE, fetch });
  const spec = { element: 'vertex', label: 'person', path: 'name' };
  assert.deepEqual(await engine.estimateCompression(spec), answer);
  assert.deepEqual(JSON.parse(calls.find((c) => c.path === '/api/compression/estimate').init.body), spec);
});

test('the snapshot download URL leaves the models out on request', async () => {
  const { HttpEngine } = await import('../js/engine/http-engine.js');
  const engine = new HttpEngine({ store: '/tmp/data' }, { base: 'http://127.0.0.1:1/', fetch: async () => { throw new Error('no fetch'); } });
  assert.equal(engine.snapshotUrl(2), 'http://127.0.0.1:1/api/store/snapshot?commit=2');
  assert.equal(engine.snapshotUrl(2, false), 'http://127.0.0.1:1/api/store/snapshot?commit=2&models=without');
});
