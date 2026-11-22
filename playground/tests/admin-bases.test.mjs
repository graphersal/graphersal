// Node tests of bases with data in the admin panel (js/admin-api.js, P4): "Promote to base…"
// (the request, the one-way confirmation, the reasons it is disabled, the server's 409 help shown
// verbatim) and the import with role base (upload query and JSON path form, the refusals of a
// layer source and of a source missing its model), against a stateful mock (node:http on port 0)
// shaped after graphersal-server's docs/admin-api.md (Promote, Import).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  adminRequest, createAdmin, AdminError, promoteAction, confirmText, outcomeText, importRequest,
  uploadImport, BASE_IS_FROZEN,
} from '../js/admin-api.js';
import { databaseRows } from '../js/server-mode.js';

const MODEL_HELP = 'import it as an ordinary database, add its model (POST /api/admin/models), then promote it';

/**
 * The promote and import routes. Uploads starting with `L` are layer snapshots, with `N` name a
 * model they do not carry; the JSON form takes the same from the path (`/layer...`, `/nomodel...`).
 * Unknown parameters or fields are refused, as the server does.
 */
async function mockServer() {
  const dbs = new Map();
  const log = [];
  const entry = (name) => ({ name, ...dbs.get(name) });
  const add = (name, fields) => dbs.set(name, { state: 'unloaded', kind: 'plain', role: 'normal', readOnly: false, autoLoad: false, description: '', ...fields });
  add('acme', {});
  add('live', { state: 'loaded' });
  add('shared', { role: 'base', readOnly: true });
  add('draft', { kind: 'layer', base: 'shared' });
  add('vectors', { state: 'needs-model', reason: 'the model is missing', model: 'owner/m' });

  const importAs = (send, { name, role, base, layer, noModel, format }) => {
    if (role !== undefined && !['normal', 'base'].includes(role)) return send(400, { error: `unknown role "${role}"` });
    if (role === 'base') {
      if (layer) return send(400, { error: 'a layer snapshot cannot become a base' });
      if (base) return send(400, { error: 'base= is only for a layer source' });
      if (noModel) return send(409, { error: 'the source names the model owner/m but does not carry it', help: MODEL_HELP });
    }
    if (dbs.has(name)) return send(409, { error: `the name "${name}" is taken` });
    add(name, role === 'base' ? { role: 'base', readOnly: true } : {});
    return send(201, { ...entry(name), import: { format: format || 'graphson', modelsMissing: [], skipped: 0, notes: [] } });
  };

  const server = http.createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const raw = Buffer.concat(chunks);
    const url = new URL(req.url, 'http://x');
    log.push({ method: req.method, path: url.pathname, query: url.search, type: req.headers['content-type'] || '', bytes: raw.length, body: raw.toString() });
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    const path = decodeURIComponent(url.pathname).replace(/^\/api\/admin\//, '');
    const parts = path.split('/');

    if (path === 'databases' && req.method === 'GET') {
      return send(200, { databases: [...dbs.keys()].sort().map(entry), aliases: {}, problems: [] });
    }
    if (path === 'import' && req.method === 'POST') {
      if (req.headers['content-type'] === 'application/json') {
        const body = JSON.parse(raw.toString());
        const unknown = Object.keys(body).filter((k) => !['path', 'name', 'base', 'format', 'description', 'role'].includes(k));
        if (unknown.length) return send(400, { error: `unknown field ${unknown[0]}` });
        return importAs(send, { ...body, layer: body.path.startsWith('/layer'), noModel: body.path.startsWith('/nomodel') });
      }
      const q = Object.fromEntries(url.searchParams);
      const unknown = Object.keys(q).filter((k) => !['name', 'base', 'format', 'description', 'role'].includes(k));
      if (unknown.length) return send(400, { error: `unknown parameter ${unknown[0]}` });
      return importAs(send, { ...q, layer: raw[0] === 0x4c, noModel: raw[0] === 0x4e });
    }
    if (parts[0] === 'databases' && parts.length === 3 && parts[2] === 'promote') {
      const name = parts[1];
      if (req.method !== 'POST') return send(405, { error: 'method not allowed' });
      if (raw.length) return send(400, { error: 'promote takes no body' });
      if (!dbs.has(name)) return send(404, { error: `no database named "${name}"` });
      const d = dbs.get(name);
      if (d.state === 'loaded' || d.state === 'loading') {
        return send(409, { error: `"${name}" is ${d.state}`, help: `unload it first: POST /api/admin/databases/${name}/unload, then promote it again` });
      }
      if (d.kind === 'layer') return send(409, { error: `"${name}" is a layer`, help: `export it with materialize=true and import the file with role=base` });
      if (d.role === 'base') return send(409, { error: `"${name}" is already a base` });
      if (d.state === 'needs-model') return send(409, { error: `"${name}" needs a model`, help: 'add the model first: POST /api/admin/models' });
      Object.assign(d, { role: 'base', readOnly: true });
      return send(200, entry(name));
    }
    return send(404, { error: 'no such endpoint' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { server, base, dbs, log, options: { base, fetch: globalThis.fetch } };
}

/** A minimal XMLHttpRequest over node:http (one write; enough for the import's answer). */
class NodeXHR {
  constructor() { this.upload = {}; this.headers = {}; this.status = 0; this.responseText = ''; }
  open(method, url) { this.method = method; this.url = url; }
  setRequestHeader(k, v) { this.headers[k] = v; }
  send(body) {
    const buf = Buffer.from(body);
    const req = http.request(this.url, { method: this.method, headers: { ...this.headers, 'Content-Length': buf.length } }, (res) => {
      let text = '';
      res.setEncoding('utf8');
      res.on('data', (d) => { text += d; });
      res.on('end', () => { this.status = res.statusCode; this.responseText = text; this.onload && this.onload(); });
    });
    req.on('error', () => this.onerror && this.onerror());
    req.end(buf, () => this.upload.onload && this.upload.onload());
  }
  abort() {}
}

const rowOf = async (admin, name) => databaseRows(await admin.list()).rows.find((r) => r.name === name);

// ------------------------------------------------------------------ promote

test('promote: the request has no body; the confirmation says it is one way', () => {
  assert.deepEqual(adminRequest.promote('a b'), { method: 'POST', path: 'api/admin/databases/a%20b/promote' });
  assert.equal(BASE_IS_FROZEN, 'A base is frozen forever: it cannot be written or turned back; layers are built over it.');
  assert.equal(confirmText('promote', 'acme'), `Promote "acme" to a base? ${BASE_IS_FROZEN}`);
  assert.equal(outcomeText('promote', 'acme', { name: 'acme', role: 'base' }), '"acme" is now a base (read-only, frozen); create layers over it with "New database…".');
});

test('promoteAction: only an unloaded plain normal database; every other row says why not', () => {
  const plain = { name: 'a', state: 'unloaded', kind: 'plain', role: 'normal' };
  const ok = promoteAction(plain);
  assert.equal(ok.enabled, true);
  assert.ok(ok.title.endsWith(BASE_IS_FROZEN), 'the title warns before the click too');
  const why = (row, opts) => {
    const a = promoteAction({ ...plain, ...row }, opts);
    assert.equal(a.enabled, false, JSON.stringify(row));
    return a.title;
  };
  assert.match(why({ state: 'loaded' }), /^Unload it first/);
  assert.match(why({ state: 'loading' }), /^Loading…/);
  assert.match(why({ kind: 'layer', base: 'b' }), /^A layer cannot become a base: export it with "materialize" and import the file as a base$/);
  assert.equal(why({ role: 'base' }), 'Already a base');
  assert.match(why({ state: 'needs-model' }), /^Add its model first: a base cannot get a model later$/);
  assert.equal(why({ state: 'failed' }), 'A failed database cannot be promoted');
  assert.equal(why({ state: 'failed', kind: '' }), 'A failed database cannot be promoted', 'config not readable');
  assert.match(why({ aliasOf: 'x' }), /^An alias of x/);
  assert.equal(why({}, { busy: true }), 'Wait until the running action ends');
  // an unknown future state is not offered
  assert.match(why({ state: 'repairing' }), /it is repairing/);
});

test('promote against the mock: an unloaded plain database becomes a base; refusals show the help verbatim', async () => {
  const { server, options, dbs, log } = await mockServer();
  try {
    const admin = createAdmin(options);
    const acme = await rowOf(admin, 'acme');
    assert.equal(promoteAction(acme).enabled, true);
    const answer = await admin.promote('acme');
    assert.equal(answer.role, 'base');
    assert.equal(answer.readOnly, true);
    const sent = log.find((l) => l.path.endsWith('/promote'));
    assert.deepEqual([sent.method, sent.path, sent.bytes, sent.type], ['POST', '/api/admin/databases/acme/promote', 0, ''], 'no body, no content type');
    assert.equal(dbs.get('acme').role, 'base');
    // the row now is a base: the action is disabled
    assert.equal(promoteAction(await rowOf(admin, 'acme')).title, 'Already a base');

    // the UI disables these; the server's words when reached anyway (another panel, a stale list)
    const refused = async (name, status, help) => {
      await assert.rejects(admin.promote(name), (e) => {
        assert.ok(e instanceof AdminError);
        assert.equal(e.status, status);
        assert.equal(e.help, help);
        return true;
      });
      assert.equal(promoteAction(await rowOf(admin, name) || { name, state: 'unloaded', kind: 'plain', role: 'normal' }).enabled, status === 404);
    };
    await refused('live', 409, 'unload it first: POST /api/admin/databases/live/unload, then promote it again');
    await refused('draft', 409, 'export it with materialize=true and import the file with role=base');
    await refused('shared', 409, '');
    await refused('vectors', 409, 'add the model first: POST /api/admin/models');
    await refused('nobody', 404, '');
  } finally {
    server.close();
  }
});

// ------------------------------------------------------------------ import with role base

test('importRequest with role base: the query and the JSON field; no base parameter; normal sends no role', () => {
  const file = { size: 10 };
  assert.equal(importRequest({ source: 'upload', file, name: 'Lib', role: 'base' }).upload.path, 'api/admin/import?name=lib&role=base');
  assert.equal(importRequest({ source: 'upload', file, name: 'lib', role: 'normal' }).upload.path, 'api/admin/import?name=lib', 'normal is the default: not sent');
  assert.equal(importRequest({ source: 'upload', file, name: 'lib', base: 'shared', role: 'base' }).upload.path, 'api/admin/import?name=lib&role=base', 'a base is made from a plain source: no base=');
  assert.equal(importRequest({ source: 'upload', file, name: 'lib', base: 'shared' }).upload.path, 'api/admin/import?name=lib&base=shared');
  assert.deepEqual(importRequest({ source: 'path', path: '/data/lib.json', name: 'lib', format: 'graphson', role: 'base' }).request,
    { method: 'POST', path: 'api/admin/import', body: { path: '/data/lib.json', name: 'lib', format: 'graphson', role: 'base' } });
  assert.deepEqual(importRequest({ source: 'path', path: '/data/lib.json', name: 'lib' }).request.body, { path: '/data/lib.json', name: 'lib' });
  assert.equal(outcomeText('import', 'lib', { name: 'lib', kind: 'plain', role: 'base', state: 'unloaded', import: { format: 'graphson' } }), 'Imported "lib" (graphson, plain, base): unloaded.');
});

test('import with role base against the mock: upload and path form; a layer source and a missing model are refused verbatim', async () => {
  const { server, options, base, dbs, log } = await mockServer();
  try {
    const admin = createAdmin(options);
    const up = (name, bytes) => uploadImport({ base, XHR: NodeXHR }, importRequest({ source: 'upload', file: { size: bytes.length }, name, role: 'base' }).upload, bytes).promise;

    const lib = await up('lib', Buffer.from('{"graphson":1}'));
    assert.deepEqual([lib.name, lib.role, lib.readOnly, lib.kind], ['lib', 'base', true, 'plain']);
    assert.equal(log.at(-1).query, '?name=lib&role=base');
    assert.equal(dbs.get('lib').role, 'base');

    await assert.rejects(up('lay', Buffer.from('L-layer')), (e) => e instanceof AdminError && e.status === 400 && e.message === 'a layer snapshot cannot become a base' && e.help === '');
    await assert.rejects(up('vec', Buffer.from('N-model')), (e) => e.status === 409 && e.help === MODEL_HELP);
    assert.ok(!dbs.has('lay') && !dbs.has('vec'), 'nothing was created');

    const path = (p, name) => admin.importPath(importRequest({ source: 'path', path: p, name, role: 'base' }).request.body);
    const fromPath = await path('/data/lib2.json', 'lib2');
    assert.equal(fromPath.role, 'base');
    assert.deepEqual(JSON.parse(log.at(-1).body), { path: '/data/lib2.json', name: 'lib2', role: 'base' });
    await assert.rejects(path('/layer.gsnap', 'lay2'), (e) => e.status === 400);
    await assert.rejects(path('/nomodel.gsnap', 'vec2'), (e) => e.status === 409 && e.help === MODEL_HELP);
  } finally {
    server.close();
  }
});
