// Node tests of the admin panel's transfers (js/admin-api.js, P3): backups and restore, the
// export download, import by upload (progress, size pre-check, 413) and by server path, the
// models (list, add, remove, the databases waiting for one), the `needs-model` hint and the
// listing fields `sizeBytes`, `pooled`/`pooledBy`, `linkWarning`, all against a stateful mock
// (node:http on port 0) shaped after graphersal-server's docs/admin-api.md.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  adminRequest, createAdmin, AdminError, transferActions, backupRows, restoreOverAction, exportPath,
  uploadLimit, uploadSizeProblem, importRequest, isAbsolutePath, progressText, uploadImport,
  modelRows, missingRows, removeModelAction, addModelBody, needsModelHint, poolText, suggestName,
  confirmText, outcomeText,
} from '../js/admin-api.js';
import { databaseRows } from '../js/server-mode.js';

const MODEL = 'minishlab/potion-base-8M';
const BLOB = 'ab'.repeat(32);

// ------------------------------------------------------------------ the mock server

/**
 * A stateful mock of the transfer routes. `limit` is the upload limit (bytes); an upload over it
 * is read, discarded and answered `413` WITHOUT help (as the server does for a body declared at
 * most 64 MiB). Uploads whose first byte is `N` import as `needs-model`. Requests without the
 * host's `Authorization` header are refused.
 */
async function mockServer({ limit = 1 << 20 } = {}) {
  const dbs = new Map();
  const backups = new Map(); // name -> [backup]
  const models = [{ repo: MODEL, identity: `model2vec/${MODEL}@0123456789ab`, license: 'MIT', blob: BLOB, bytes: 30000000, users: ['acme'], waiting: [], copied: false, copiedIn: [], links: 2 }];
  const log = [];
  const entry = (name) => ({ name, ...dbs.get(name) });
  const add = (name, fields) => dbs.set(name, { state: 'unloaded', kind: 'plain', role: 'normal', readOnly: false, autoLoad: false, description: '', sizeBytes: 4096, ...fields });
  add('acme', { state: 'loaded' });
  add('shared', { role: 'base', readOnly: true, pooled: true, pooledBy: ['draft'] });
  add('draft', { kind: 'layer', base: 'shared', state: 'loaded' });
  add('vectors', { state: 'needs-model', reason: 'the model is missing', model: 'owner/other' });
  let stamp = 0;

  const server = http.createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const raw = Buffer.concat(chunks);
    const url = new URL(req.url, 'http://x');
    log.push({ method: req.method, path: url.pathname, query: url.search, type: req.headers['content-type'] || '', bytes: raw.length });
    const send = (status, body, extra = {}) => {
      res.writeHead(status, { 'Content-Type': 'application/json', ...extra });
      res.end(JSON.stringify(body));
    };
    if (req.headers.authorization !== 'Bearer t') return send(401, { error: 'a token is required' });
    const path = decodeURIComponent(url.pathname).replace(/^\/api\/admin\//, '');
    const parts = path.split('/');
    const body = raw.length && req.headers['content-type'] === 'application/json' ? JSON.parse(raw.toString()) : {};
    const unload = (n) => `unload it first: POST /api/admin/databases/${n}/unload`;

    if (path === 'databases' && req.method === 'GET') {
      return send(200, { databases: [...dbs.keys()].sort().map(entry), aliases: {}, problems: [] });
    }
    if (path === 'import' && req.method === 'POST') {
      if (req.headers['content-type'] === 'application/json') {
        const allowed = ['path', 'name', 'base', 'format', 'description'];
        const unknown = Object.keys(body).filter((k) => !allowed.includes(k));
        if (unknown.length) return send(400, { error: `unknown field ${unknown[0]}` });
        if (!body.path.startsWith('/')) return send(400, { error: 'the path must be absolute' });
        if (body.path === '/missing') return send(404, { error: 'no file at /missing' });
        add(body.name, {});
        return send(201, { ...entry(body.name), import: { format: 'graphson', modelsMissing: [], skipped: 2, notes: ['2 values skipped'] } });
      }
      for (const k of url.searchParams.keys()) {
        if (!['name', 'base', 'format', 'description'].includes(k)) return send(400, { error: `unknown parameter ${k}` });
      }
      if (raw.length > limit) return send(413, { error: `the body is larger than ${limit} bytes` });
      const name = url.searchParams.get('name');
      if (dbs.has(name)) return send(409, { error: `the name "${name}" is taken`, help: 'list the databases: GET /api/admin/databases' });
      if (raw[0] === 0x4e) {
        add(name, { state: 'needs-model', reason: 'the snapshot names a model it does not carry', model: MODEL, description: url.searchParams.get('description') || '' });
        return send(201, { ...entry(name), import: { format: 'snapshot', modelsMissing: [MODEL], skipped: 0, notes: [`model ${MODEL} is missing`] } });
      }
      add(name, { description: url.searchParams.get('description') || '' });
      return send(201, { ...entry(name), import: { format: url.searchParams.get('format') || 'snapshot', modelsMissing: [], skipped: 0, notes: [] } });
    }
    if (path === 'models' && req.method === 'GET') {
      const missing = [...dbs.keys()].filter((n) => dbs.get(n).state === 'needs-model').map((n) => ({ database: n, model: dbs.get(n).model }));
      return send(200, { models, missing });
    }
    if (path === 'models' && req.method === 'POST') {
      if (body.repo && !body.path) {
        if (!models.some((m) => m.repo === body.repo)) return send(404, { error: `models/ holds no model ${body.repo}`, help: 'add it from a path first' });
        const resolved = [...dbs.keys()].filter((n) => dbs.get(n).state === 'needs-model' && dbs.get(n).model === body.repo);
        for (const n of resolved) Object.assign(dbs.get(n), { state: 'unloaded', reason: undefined, model: undefined });
        const stillMissing = [...dbs.keys()].filter((n) => dbs.get(n).state === 'needs-model').map((n) => ({ database: n, model: dbs.get(n).model }));
        return send(200, { added: [], resolved, stillMissing, notes: [] });
      }
      if (!body.path || !body.path.startsWith('/')) return send(400, { error: 'give an absolute path or a repository' });
      const m = { repo: body.repo || 'owner/other', identity: '', license: body.license || null, blob: 'cd'.repeat(32), bytes: 1000, users: [], waiting: [], copied: false, copiedIn: [] };
      models.push(m);
      return send(201, { added: [m], resolved: [], stillMissing: [], notes: [] });
    }
    if (parts[0] === 'models' && req.method === 'DELETE') {
      const id = parts.slice(1).join('/');
      const i = models.findIndex((m) => m.repo === id || m.blob === id);
      if (i < 0) return send(404, { error: `no model ${id}` });
      if (models[i].users.length) {
        return send(409, { error: `${id} is used by ${models[i].users.join(', ')}`, help: `delete those databases first: DELETE /api/admin/databases/${models[i].users[0]}` });
      }
      const [m] = models.splice(i, 1);
      return send(200, { model: id, removed: [m.blob], bytes: m.bytes });
    }
    if (parts[0] === 'databases' && parts.length === 3) {
      const [, name, action] = parts;
      if (!dbs.has(name) && !(action === 'backups' || action === 'restore')) return send(404, { error: `no database named "${name}"` });
      if (action === 'backup' && req.method === 'POST') {
        stamp++;
        const b = { database: name, backup: `2026-11-22T10-00-0${stamp}Z`, file: `backups/${name}/2026-11-22T10-00-0${stamp}Z.zip`, bytes: 1234, created: '2026-11-22T10:00:00Z', kind: dbs.get(name).kind, role: dbs.get(name).role };
        backups.set(name, [...(backups.get(name) || []), b]);
        return send(201, b);
      }
      if (action === 'backups' && req.method === 'GET') return send(200, { database: name, backups: backups.get(name) || [] });
      if (action === 'restore' && req.method === 'POST') {
        const unknown = Object.keys(body).filter((k) => !['backup', 'name'].includes(k));
        if (unknown.length) return send(400, { error: `unknown field ${unknown[0]}` });
        const b = (backups.get(name) || []).find((x) => x.backup === body.backup.replace(/\.zip$/, ''));
        if (!b) return send(404, { error: `no backup ${body.backup} of "${name}"`, help: `list them: GET /api/admin/databases/${name}/backups` });
        const target = body.name || name;
        if (target === name) {
          if (dbs.has(name) && dbs.get(name).state === 'loaded') return send(409, { error: `"${name}" is loaded`, help: unload(name) });
          const replaced = dbs.has(name);
          add(name, {});
          return send(201, { ...entry(name), restoredFrom: { database: name, backup: b.backup }, replaced, ...(replaced ? { trash: `trash/${name}-2026-11-22T11-00-00Z` } : {}) });
        }
        if (dbs.has(target)) return send(409, { error: `the name "${target}" is taken`, help: 'list the databases: GET /api/admin/databases' });
        add(target, {});
        return send(201, { ...entry(target), restoredFrom: { database: name, backup: b.backup }, replaced: false });
      }
      if (action === 'export' && req.method === 'GET') {
        for (const [k, v] of url.searchParams) {
          if (!['models', 'materialize'].includes(k) || !['true', 'false'].includes(v)) return send(400, { error: `bad parameter ${k}=${v}` });
        }
        const layer = dbs.get(name).kind === 'layer' && url.searchParams.get('materialize') !== 'true';
        const file = Buffer.from('GSNAP-bytes');
        res.writeHead(200, {
          'Content-Type': 'application/octet-stream',
          'Content-Length': file.length,
          'Content-Disposition': `attachment; filename="${name}${layer ? '.layer' : ''}.gsnap"`,
          'X-Graphersal-Snapshot': layer ? 'layer' : 'full',
          'X-Graphersal-Models': !layer && url.searchParams.get('models') !== 'false' ? 'included' : 'excluded',
        });
        return res.end(file);
      }
    }
    return send(404, { error: 'no such endpoint' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { server, base, dbs, backups, models, log, options: { base, fetch: globalThis.fetch, headers: { Authorization: 'Bearer t' } } };
}

/**
 * A minimal XMLHttpRequest over node:http for `uploadImport`: sends the body in 16 KiB chunks and
 * fires `upload.onprogress` after each, `upload.onload` at the end, then `onload` with the
 * answer; `abort()` destroys the request and fires `onabort`. Records the request headers.
 */
class NodeXHR {
  constructor() {
    this.upload = {};
    this.headers = {};
    this.status = 0;
    this.responseText = '';
    NodeXHR.last = this;
  }
  open(method, url) { this.method = method; this.url = url; }
  setRequestHeader(k, v) { this.headers[k] = v; }
  send(body) {
    const buf = Buffer.from(body);
    this.req = http.request(this.url, { method: this.method, headers: { ...this.headers, 'Content-Length': buf.length } }, (res) => {
      let text = '';
      res.setEncoding('utf8');
      res.on('data', (d) => { text += d; });
      res.on('end', () => { this.status = res.statusCode; this.responseText = text; this.onload && this.onload(); });
    });
    this.req.on('error', () => { if (!this.aborted) this.onerror && this.onerror(); });
    const step = 16 * 1024;
    const write = (off) => {
      if (this.aborted) return;
      if (off >= buf.length) {
        this.req.end();
        this.upload.onload && this.upload.onload();
        return;
      }
      const end = Math.min(buf.length, off + step);
      this.req.write(buf.subarray(off, end), () => {
        this.upload.onprogress && this.upload.onprogress({ loaded: end, total: buf.length, lengthComputable: true });
        if (NodeXHR.abortAt && end >= NodeXHR.abortAt) { NodeXHR.abortAt = 0; this.pendingAbort && this.pendingAbort(); }
        write(end);
      });
    };
    write(0);
  }
  abort() {
    this.aborted = true;
    if (this.req) this.req.destroy();
    this.onabort && this.onabort();
  }
}

// ------------------------------------------------------------------ request builders

test('the transfer request builders: documented paths and bodies, relative', () => {
  assert.deepEqual(adminRequest.backup('a b'), { method: 'POST', path: 'api/admin/databases/a%20b/backup' });
  assert.deepEqual(adminRequest.backups('x'), { method: 'GET', path: 'api/admin/databases/x/backups' });
  assert.deepEqual(adminRequest.restore('x', 'B'), { method: 'POST', path: 'api/admin/databases/x/restore', body: { backup: 'B' } });
  assert.deepEqual(adminRequest.restore('x', 'B', 'x').body, { backup: 'B' }, 'the own name sends no name field');
  assert.deepEqual(adminRequest.restore('x', 'B', 'y').body, { backup: 'B', name: 'y' });
  assert.deepEqual(adminRequest.importPath({ path: '/a', name: 'n' }), { method: 'POST', path: 'api/admin/import', body: { path: '/a', name: 'n' } });
  assert.deepEqual(adminRequest.models(), { method: 'GET', path: 'api/admin/models' });
  assert.deepEqual(adminRequest.addModel({ repo: 'o/n' }), { method: 'POST', path: 'api/admin/models', body: { repo: 'o/n' } });
  assert.deepEqual(adminRequest.removeModel('o w/n'), { method: 'DELETE', path: 'api/admin/models/o%20w/n' }, "the repository's slash stays a path separator");
  assert.deepEqual(exportPath('acme'), { path: 'api/admin/databases/acme/export?models=true', fileName: 'acme.gsnap' });
  assert.deepEqual(exportPath('acme', { models: false, materialize: true }), { path: 'api/admin/databases/acme/export?models=false', fileName: 'acme.gsnap' }, 'materialize only for a layer');
  assert.deepEqual(exportPath('d', { layer: true }), { path: 'api/admin/databases/d/export?models=true', fileName: 'd.layer.gsnap' });
  assert.deepEqual(exportPath('d', { layer: true, materialize: true, models: false }), { path: 'api/admin/databases/d/export?models=false&materialize=true', fileName: 'd.gsnap' });
});

// ------------------------------------------------------------------ backups and restore

test('backup, list, restore into a new name and over an unloaded database; refusals carry help', async () => {
  const { server, options, dbs } = await mockServer();
  try {
    const admin = createAdmin(options);
    const b = await admin.backup('acme');
    assert.match(outcomeText('backup', 'acme', b), /^Backed up "acme" as 2026-11-22T10-00-01Z \(1\.2 KB\)\.$/);
    await admin.backup('acme');
    const rows = backupRows(await admin.backups('acme'));
    assert.deepEqual(rows.map((r) => r.backup), ['2026-11-22T10-00-01Z', '2026-11-22T10-00-02Z'], 'oldest first, as sent');
    assert.equal(rows[0].bytes, 1234);
    assert.equal(rows[0].file, 'backups/acme/2026-11-22T10-00-01Z.zip');
    assert.deepEqual(backupRows({ backups: [null, { bytes: 1 }, { backup: 'x', bytes: -1 }] }).map((r) => [r.backup, r.bytes]), [['x', null]]);

    // over a loaded database: the UI disables it, the server refuses with help naming the unload
    const acme = databaseRows(await admin.list()).rows.find((r) => r.name === 'acme');
    const over = restoreOverAction(acme);
    assert.equal(over.enabled, false);
    assert.match(over.title, /Unload it first/);
    await assert.rejects(admin.restore('acme', rows[0].backup), (e) => e instanceof AdminError && e.status === 409 && /POST \/api\/admin\/databases\/acme\/unload/.test(e.help));

    // into a new name
    const copy = await admin.restore('acme', rows[0].backup, 'acme-restored');
    assert.equal(copy.name, 'acme-restored');
    assert.equal(dbs.get('acme-restored').state, 'unloaded');
    assert.equal(outcomeText('restore', 'acme-restored', copy), 'Restored "acme-restored" from acme/2026-11-22T10-00-01Z (unloaded).');
    await assert.rejects(admin.restore('acme', rows[0].backup, 'acme-restored'), (e) => e.status === 409 && /GET \/api\/admin\/databases/.test(e.help));

    // over the database itself once unloaded: replaced, the old directory in trash
    dbs.get('acme').state = 'unloaded';
    assert.equal(restoreOverAction({ ...acme, state: 'unloaded' }).enabled, true);
    const back = await admin.restore('acme', `${rows[1].backup}.zip`);
    assert.equal(back.replaced, true);
    assert.match(outcomeText('restore', 'acme', back), /replaced directory moved to trash\/acme-/);
    assert.match(confirmText('restoreOver', 'acme', rows[1].backup), /Restore "acme" from the backup .*trash\//);

    // a deleted database: its backups are still listed and restore recreates it
    dbs.delete('acme');
    assert.equal(backupRows(await admin.backups('acme')).length, 2);
    assert.equal(restoreOverAction(null).enabled, true);
    assert.equal((await admin.restore('acme', rows[0].backup)).replaced, false);
    await assert.rejects(admin.restore('acme', 'nope'), (e) => e.status === 404 && /backups/.test(e.help));
  } finally {
    server.close();
  }
});

// ------------------------------------------------------------------ export

test('export: the download carries the chosen options and the server file name', async () => {
  const { server, options } = await mockServer();
  try {
    const get = (p) => fetch(new URL(p, options.base), { headers: options.headers });
    let r = await get(exportPath('acme', { models: false }).path);
    assert.equal(r.status, 200);
    assert.equal(r.headers.get('x-graphersal-models'), 'excluded');
    assert.equal(r.headers.get('content-disposition'), 'attachment; filename="acme.gsnap"');
    assert.equal(await r.text(), 'GSNAP-bytes');
    const layer = exportPath('draft', { layer: true });
    r = await get(layer.path);
    assert.equal(r.headers.get('x-graphersal-snapshot'), 'layer');
    assert.equal(r.headers.get('content-disposition'), `attachment; filename="${layer.fileName}"`);
    const mat = exportPath('draft', { layer: true, materialize: true });
    r = await get(mat.path);
    assert.equal(r.headers.get('x-graphersal-snapshot'), 'full');
    assert.equal(r.headers.get('content-disposition'), `attachment; filename="${mat.fileName}"`);
  } finally {
    server.close();
  }
});

// ------------------------------------------------------------------ import

test('import by upload: progress events, the answer, needs-model with its hint, a taken name', async () => {
  const { server, options, log, dbs } = await mockServer();
  try {
    const file = Buffer.alloc(100 * 1024, 0x47);
    const built = importRequest({ source: 'upload', file: { size: file.length }, name: ' New DB ', description: 'from a file', format: 'snapshot' }, 1 << 20);
    assert.ok(built.problems, 'a space is not allowed in a name');
    const ok = importRequest({ source: 'upload', file: { size: file.length }, name: 'Uploaded', description: 'a b&c', format: 'snapshot' }, 1 << 20);
    assert.deepEqual(ok.upload, { path: 'api/admin/import?name=uploaded&format=snapshot&description=a%20b%26c', size: file.length });
    const progress = [];
    let sent = false;
    const up = uploadImport({ ...options, XHR: NodeXHR, limit: 1 << 20 }, ok.upload, file, {
      onProgress: (loaded, total) => progress.push([loaded, total]),
      onSent: () => { sent = true; },
    });
    const answer = await up.promise;
    assert.equal(answer.name, 'uploaded');
    assert.equal(answer.import.format, 'snapshot');
    assert.equal(dbs.get('uploaded').description, 'a b&c');
    assert.ok(progress.length >= 6, `progress events: ${progress.length}`);
    assert.deepEqual(progress.at(-1), [file.length, file.length]);
    assert.ok(progress.every(([l], i) => i === 0 || l > progress[i - 1][0]), 'monotonic');
    assert.equal(sent, true);
    assert.equal(NodeXHR.last.headers['Content-Type'], 'application/octet-stream');
    assert.equal(NodeXHR.last.headers.Authorization, 'Bearer t', 'the host headers ride along');
    const req = log.find((r) => r.path === '/api/admin/import');
    assert.equal(req.bytes, file.length);
    assert.equal(outcomeText('import', 'uploaded', answer), 'Imported "uploaded" (snapshot, plain): unloaded.');
    assert.equal(progressText(50 * 1024, 100 * 1024), '50 KB of 100 KB (50 %)');
    assert.equal(progressText(2048, 0), '2.0 KB sent');

    // a snapshot naming a model it does not carry: needs-model, with the hint to add it
    const nm = importRequest({ source: 'upload', file: { size: 4 }, name: 'vec2' }, null);
    const vec = await uploadImport({ ...options, XHR: NodeXHR }, nm.upload, Buffer.from('NNNN')).promise;
    assert.equal(vec.state, 'needs-model');
    assert.match(outcomeText('import', 'vec2', vec), /: needs a model \(minishlab\/potion-base-8M\)\.$/);
    const row = databaseRows({ databases: [vec] }).rows[0];
    assert.match(needsModelHint(row), /^Add "minishlab\/potion-base-8M" under Models .* can be loaded\.$/);
    assert.equal(needsModelHint({ state: 'unloaded' }), '');

    // a taken name: the server's error and help
    await assert.rejects(uploadImport({ ...options, XHR: NodeXHR }, ok.upload, file).promise,
      (e) => e instanceof AdminError && e.status === 409 && /taken/.test(e.message) && /GET \/api\/admin\/databases/.test(e.help));
  } finally {
    server.close();
  }
});

test('import: the size pre-check refuses before sending; a 413 and a closed connection are explained; cancel', async () => {
  const { server, options, log } = await mockServer({ limit: 64 * 1024 });
  try {
    const limit = uploadLimit({ server: 'graphersal-server', admin: true, uploadBytes: 64 * 1024 });
    assert.equal(limit, 65536);
    assert.equal(uploadLimit({ admin: true }), null);
    assert.equal(uploadLimit({ uploadBytes: -1 }), null);
    const built = importRequest({ source: 'upload', file: { size: 100 * 1024 }, name: 'big' }, limit);
    assert.deepEqual(built.problems, ['The file has 100 KB; the server takes uploads up to 64 KB ([limits] upload_bytes in server.toml). Import it from a path on the server instead.']);
    assert.equal(log.length, 0, 'nothing was sent');
    assert.equal(uploadSizeProblem(10, null), null);
    assert.equal(uploadSizeProblem(0, null), 'The file is empty.');
    assert.equal(uploadSizeProblem(undefined, null), 'Choose a file.');

    // the server's limit is lower than the page knew: its 413 gets the advice
    const sneaky = importRequest({ source: 'upload', file: { size: 100 * 1024 }, name: 'big' }, null);
    await assert.rejects(uploadImport({ ...options, XHR: NodeXHR, limit }, sneaky.upload, Buffer.alloc(100 * 1024)).promise,
      (e) => e.status === 413 && /larger than 65536 bytes/.test(e.message) && /up to 64 KB; import a larger file from a path/.test(e.help));

    // a connection the server closed (an upload declared far above the limit): named as such
    class Dropped { constructor() { this.upload = {}; } open() {} setRequestHeader() {} send() { setTimeout(() => this.onerror(), 0); } abort() {} }
    await assert.rejects(uploadImport({ ...options, XHR: Dropped, limit }, { path: 'api/admin/import?name=x', size: 1e9 }, null).promise,
      (e) => /closed the connection: the file is larger than its upload limit \(64 KB\)/.test(e.message) && /path on the server/.test(e.help));
    await assert.rejects(uploadImport({ ...options, XHR: Dropped, limit }, { path: 'api/admin/import?name=x', size: 10 }, null).promise,
      (e) => /connection was closed before the server answered/.test(e.message));
    await assert.rejects(uploadImport({ ...options, XHR: undefined }, { path: 'x', size: 1 }, null).promise, /cannot upload/);

    // cancel during the upload
    const small = importRequest({ source: 'upload', file: { size: 60 * 1024 }, name: 'cancelled' }, limit);
    let up;
    NodeXHR.abortAt = 16 * 1024;
    up = uploadImport({ ...options, XHR: NodeXHR, limit }, small.upload, Buffer.alloc(60 * 1024));
    NodeXHR.last.pendingAbort = () => up.abort();
    await assert.rejects(up.promise, (e) => e.aborted === true && /cancelled/.test(e.message));
  } finally {
    server.close();
  }
});

test('import from a server path: the JSON form, absolute paths only, notes in the answer', async () => {
  const { server, options } = await mockServer();
  try {
    const admin = createAdmin(options);
    assert.deepEqual(importRequest({ source: 'path', path: 'rel/x.json', name: 'p' }).problems, ['The path must be absolute (it is read on the server).']);
    assert.deepEqual(importRequest({ source: 'path', path: '', name: '' }).problems.length, 2);
    assert.deepEqual(importRequest({ source: 'path', path: '/x', name: 'p', format: 'csv' }).problems, ['Unknown format "csv".']);
    const built = importRequest({ source: 'path', path: '/data/g.json', name: 'Fromjson', format: 'graphson', base: '' });
    assert.deepEqual(built.request, { method: 'POST', path: 'api/admin/import', body: { path: '/data/g.json', name: 'fromjson', format: 'graphson' } });
    const answer = await admin.importPath(built.request.body);
    assert.deepEqual(answer.import.notes, ['2 values skipped']);
    await assert.rejects(admin.importPath({ path: '/missing', name: 'm' }), (e) => e.status === 404);
    assert.ok(isAbsolutePath('/a') && isAbsolutePath('C:\\data\\x') && isAbsolutePath('d:/x') && isAbsolutePath('\\\\srv\\share'));
    assert.ok(!isAbsolutePath('a/b') && !isAbsolutePath('./a') && !isAbsolutePath('C:x'));
    assert.equal(suggestName('ACME Prod.layer.gsnap'), 'acme-prod');
    assert.equal(suggestName('/home/u/graph.json'), 'graph');
    assert.equal(suggestName('C:\\x\\My_Data.graphml'), 'my_data');
    assert.equal(suggestName('...'), '');
    assert.equal(suggestName('a'.repeat(80) + '.gstore').length, 63);
  } finally {
    server.close();
  }
});

// ------------------------------------------------------------------ models

test('models: list, the waiting databases, add by path and by repository, remove refused with help', async () => {
  const { server, options, dbs } = await mockServer();
  try {
    const admin = createAdmin(options);
    let answer = await admin.models();
    const rows = modelRows(answer);
    assert.equal(rows.length, 1);
    assert.equal(rows[0].repo, MODEL);
    assert.equal(rows[0].id, MODEL);
    assert.deepEqual(rows[0].users, ['acme']);
    assert.deepEqual(missingRows(answer), [{ database: 'vectors', model: 'owner/other' }]);

    // remove: the UI disables a used model; the server refuses with help anyway
    assert.match(removeModelAction(rows[0]).title, /Used by acme/);
    await assert.rejects(admin.removeModel(MODEL), (e) => e.status === 409 && /used by acme/.test(e.message) && /DELETE \/api\/admin\/databases\/acme/.test(e.help));

    // by repository: unknown to models/ -> 404 with help
    await assert.rejects(admin.addModel(addModelBody({ source: 'repo', repo: 'owner/other' }).body), (e) => e.status === 404 && e.help === 'add it from a path first');
    // from a path: added; then by repository resolves the waiting database
    const added = await admin.addModel(addModelBody({ source: 'path', path: '/models/other', repo: 'owner/other', license: 'MIT' }).body);
    assert.equal(outcomeText('addModel', '', added), 'Added 1 model blob.');
    const resolved = await admin.addModel({ repo: 'owner/other' });
    assert.deepEqual(resolved.resolved, ['vectors']);
    assert.equal(outcomeText('addModel', '', resolved), 'Added 0 model blobs; ready to load: vectors.');
    assert.equal(dbs.get('vectors').state, 'unloaded');
    answer = await admin.models();
    assert.deepEqual(missingRows(answer), []);
    const other = modelRows(answer).find((m) => m.repo === 'owner/other');
    assert.equal(removeModelAction(other).enabled, true);
    const gone = await admin.removeModel(other.id);
    assert.equal(outcomeText('removeModel', other.id, gone), 'Removed the model "owner/other" (1000 B freed).');
    assert.match(confirmText('removeModel', 'owner/other'), /models\/ directory/);

    // the form
    assert.deepEqual(addModelBody({ source: 'path', path: 'models/x' }).problems, ['The path must be absolute (it is read on the server).']);
    assert.deepEqual(addModelBody({ source: 'path', path: '' }).problems.length, 1);
    assert.deepEqual(addModelBody({ source: 'repo', repo: ' ' }).problems, ['Enter the repository (owner/name).']);
    assert.deepEqual(addModelBody({ source: 'path', path: '/m', repo: '', license: '' }).body, { path: '/m' });

    // rows: a blob that is no model archive is removed by its hash; copies and waiting
    const odd = modelRows({ models: [{ repo: null, blob: BLOB, bytes: 5, users: [], waiting: ['w'], copied: true, copiedIn: ['a'], linkWarning: 'no hard links here' }, { repo: 'x' }] });
    assert.equal(odd.length, 1);
    assert.equal(odd[0].id, BLOB);
    assert.equal(odd[0].copied, true);
    assert.equal(odd[0].linkWarning, 'no hard links here');
    assert.match(removeModelAction(odd[0]).title, /Waited for by w/);
    assert.deepEqual(missingRows({ missing: [{ database: 'a' }, { database: 'b', model: 'm' }] }), [{ database: 'b', model: 'm' }]);
  } finally {
    server.close();
  }
});

// ------------------------------------------------------------------ listing fields and row actions

test('listing fields: sizeBytes, pooled/pooledBy, linkWarning; transfer actions per state', async () => {
  const { server, options } = await mockServer();
  try {
    const { rows } = databaseRows(await createAdmin(options).list());
    const by = Object.fromEntries(rows.map((r) => [r.name, r]));
    assert.equal(by.shared.sizeBytes, 4096);
    assert.equal(by.shared.pooled, true);
    assert.deepEqual(by.shared.pooledBy, ['draft']);
    assert.equal(poolText(by.shared), 'in pool (layers: draft)');
    assert.equal(poolText(by.acme), 'loaded');
    assert.equal(poolText({ ...by.shared, pooledBy: [] }), 'in pool');
    assert.equal(poolText({ ...by.shared, state: 'loaded', stateText: 'loaded' }), 'loaded');
    assert.equal(by.acme.pooled, false);
    assert.deepEqual(by.acme.pooledBy, []);
    assert.equal(by.acme.linkWarning, '');
    assert.equal(databaseRows({ databases: [{ name: 'c', state: 'unloaded', linkWarning: 'copied: no hard links' }] }).rows[0].linkWarning, 'copied: no hard links');
    assert.match(needsModelHint(by.vectors), /^Add "owner\/other" under Models/);
  } finally {
    server.close();
  }
  const enabled = (acts) => Object.keys(acts).filter((k) => acts[k].enabled).sort();
  assert.deepEqual(enabled(transferActions({ name: 'a', state: 'loaded', kind: 'plain' })), ['backup', 'backups', 'export']);
  assert.deepEqual(enabled(transferActions({ name: 'a', state: 'needs-model', kind: 'plain' })), ['backup', 'backups', 'export']);
  assert.deepEqual(enabled(transferActions({ name: 'a', state: 'failed', kind: '' })), ['backups']);
  assert.deepEqual(enabled(transferActions({ name: 'a', state: 'loaded', kind: 'plain', aliasOf: 'b' })), []);
  assert.deepEqual(enabled(transferActions({ name: 'a', state: 'loaded', kind: 'plain' }, { busy: true })), []);
});
