// Node tests of the admin panel's model (js/admin-api.js and the admin fields of
// js/server-mode.js `databaseRows`): request builders, every action against a stateful mock of
// a multi-database server's admin API (node:http on port 0, shaped after graphersal-server's
// docs/admin-api.md), error parsing with `help`, the form checks, the row rules and the poll of
// a load.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  adminRequest, sendAdmin, createAdmin, parseAdminError, AdminError, createBody, nameProblem,
  aliasRows, baseNames, realRows, rowActions, confirmText, outcomeText, loadAndWatch, bytesText,
} from '../js/admin-api.js';
import { databaseRows, fetchDatabases } from '../js/server-mode.js';

// ------------------------------------------------------------------ the mock server

const ALLOWED = {
  create: ['name', 'kind', 'role', 'base', 'schema', 'description', 'autoLoad', 'readOnly'],
  clone: ['name'], readOnly: ['readOnly'], autoLoad: ['autoLoad'], alias: ['target'],
};

/**
 * A stateful mock of the admin API: databases in a Map, aliases in an object, loads that stay
 * `loading` for `loadPolls` GETs. Every request is recorded (`log`), and refused without the
 * host's `Authorization` header (the panel must send the host headers on every request).
 */
async function mockAdmin({ loadPolls = 2, loadEnds = 'loaded' } = {}) {
  const dbs = new Map();
  const aliases = {};
  const pending = new Map(); // name -> remaining GETs while loading
  const log = [];
  const entry = (name) => ({ name, ...dbs.get(name) });
  const add = (name, fields) => dbs.set(name, {
    state: 'unloaded', kind: 'plain', role: 'normal', readOnly: false, autoLoad: false, description: '', ...fields,
  });
  add('acme', { state: 'loaded', description: 'CRM' });
  add('shared', { role: 'base', readOnly: true });
  add('broken', { state: 'failed', reason: 'database.toml: bad file' });

  const server = http.createServer(async (req, res) => {
    let text = '';
    for await (const chunk of req) text += chunk;
    log.push({ method: req.method, url: req.url, body: text, type: req.headers['content-type'] || '' });
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    if (req.headers.authorization !== 'Bearer t') return send(401, { error: 'a token is required' });
    let body = {};
    if (text) {
      if (req.headers['content-type'] !== 'application/json') return send(415, { error: 'send JSON' });
      body = JSON.parse(text);
    }
    const refuseUnknown = (allowed) => {
      const unknown = Object.keys(body).filter((k) => !allowed.includes(k));
      if (unknown.length) {
        send(400, { error: `unknown field ${unknown[0]}` });
        return true;
      }
      return false;
    };
    const path = decodeURIComponent(new URL(req.url, 'http://x').pathname).replace(/^\/api\/admin\//, '');
    const parts = path.split('/');
    const notFound = (name) => send(404, { error: `no database named "${name}"`, help: 'list them: GET /api/admin/databases' });

    if (path === 'databases' && req.method === 'GET') {
      const rows = [...dbs.keys()].map(entry);
      for (const [a, t] of Object.entries(aliases)) rows.push({ ...entry(t), name: a, aliasOf: t });
      rows.sort((x, y) => (x.name < y.name ? -1 : 1));
      return send(200, { databases: rows, aliases: { ...aliases }, problems: ['tmp/old: removed debris'] });
    }
    if (path === 'databases' && req.method === 'POST') {
      if (refuseUnknown(ALLOWED.create)) return;
      if (dbs.has(body.name) || aliases[body.name]) return send(409, { error: `the name "${body.name}" is taken` });
      const kind = body.kind || (body.base ? 'layer' : 'plain');
      if (kind === 'layer' && (!dbs.has(body.base) || dbs.get(body.base).role !== 'base')) {
        return send(409, { error: `"${body.base}" is not a base database`, help: 'create the base with role = "base"' });
      }
      const role = body.role || 'normal';
      add(body.name, {
        kind, role, readOnly: role === 'base' || !!body.readOnly, autoLoad: !!body.autoLoad,
        description: body.description || '', ...(body.base ? { base: body.base } : {}),
      });
      return send(201, entry(body.name));
    }
    if (parts[0] === 'databases' && parts.length >= 2) {
      const name = parts[1];
      const action = parts[2] || '';
      if (req.method === 'GET' && !action) {
        if (aliases[name]) return send(200, { ...entry(aliases[name]), name, aliasOf: aliases[name] });
        if (!dbs.has(name)) return notFound(name);
        if (pending.has(name)) {
          const left = pending.get(name) - 1;
          if (left <= 0) {
            pending.delete(name);
            const d = dbs.get(name);
            d.state = loadEnds;
            if (loadEnds === 'needs-model') { d.reason = 'the model is missing'; d.model = 'minishlab/potion-base-8M'; }
          } else pending.set(name, left);
        }
        return send(200, entry(name));
      }
      if (!dbs.has(name)) return notFound(name);
      const d = dbs.get(name);
      if (req.method === 'DELETE' && !action) {
        if (d.state === 'loaded') return send(409, { error: `"${name}" is loaded`, help: `unload it first: POST /api/admin/databases/${name}/unload` });
        const alias = Object.keys(aliases).find((a) => aliases[a] === name);
        if (alias) return send(409, { error: `the alias "${alias}" names "${name}"`, help: `remove or flip the alias: DELETE /api/admin/aliases/${alias}` });
        dbs.delete(name);
        return send(200, { name, trash: `trash/${name}-2026-11-22T10-00-00Z` });
      }
      if (req.method === 'POST' && action === 'clone') {
        if (refuseUnknown(ALLOWED.clone)) return;
        if (dbs.has(body.name)) return send(409, { error: `the name "${body.name}" is taken` });
        add(body.name, { kind: d.kind, role: d.role, readOnly: d.readOnly, description: d.description });
        return send(201, entry(body.name));
      }
      if (req.method === 'POST' && action === 'load') {
        if (d.state === 'loaded') return send(200, { ...entry(name), load: 'already-loaded' });
        if (d.state === 'loading') return send(202, { ...entry(name), load: 'already-loading' });
        d.state = 'loading';
        pending.set(name, loadPolls);
        return send(202, { ...entry(name), load: 'started' });
      }
      if (req.method === 'POST' && action === 'unload') {
        const was = d.state === 'loaded';
        d.state = 'unloaded';
        return send(200, { ...entry(name), unloaded: was });
      }
      if (req.method === 'PUT' && action === 'read-only') {
        if (refuseUnknown(ALLOWED.readOnly)) return;
        if (d.role === 'base' && body.readOnly === false) {
          return send(409, { error: `"${name}" is a base database and always read-only`, help: 'layers depend on an unchanging base' });
        }
        d.readOnly = body.readOnly;
        return send(200, entry(name));
      }
      if (req.method === 'PUT' && action === 'auto-load') {
        if (refuseUnknown(ALLOWED.autoLoad)) return;
        d.autoLoad = body.autoLoad;
        return send(200, entry(name));
      }
      return send(405, { error: 'method not allowed' });
    }
    if (parts[0] === 'aliases' && parts.length === 2) {
      const alias = parts[1];
      if (req.method === 'PUT') {
        if (refuseUnknown(ALLOWED.alias)) return;
        if (dbs.has(alias)) return send(409, { error: `"${alias}" is a database name` });
        if (!dbs.has(body.target)) return notFound(body.target);
        aliases[alias] = body.target;
        return send(200, { alias, target: body.target });
      }
      if (req.method === 'DELETE') {
        if (!aliases[alias]) return send(404, { error: `no alias named "${alias}"` });
        delete aliases[alias];
        return send(200, { alias, removed: true });
      }
    }
    if (path === 'aliases' && req.method === 'GET') return send(200, { aliases: { ...aliases } });
    return send(404, { error: 'no such endpoint' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { server, base, dbs, aliases, log, options: { base, fetch: globalThis.fetch, headers: { Authorization: 'Bearer t' } } };
}

// ------------------------------------------------------------------ request builders

test('adminRequest: the documented method, path and body of every action', () => {
  assert.deepEqual(adminRequest.list(), { method: 'GET', path: 'api/admin/databases' });
  assert.deepEqual(adminRequest.get('a b'), { method: 'GET', path: 'api/admin/databases/a%20b' });
  assert.deepEqual(adminRequest.create({ name: 'x' }), { method: 'POST', path: 'api/admin/databases', body: { name: 'x' } });
  assert.deepEqual(adminRequest.delete('x'), { method: 'DELETE', path: 'api/admin/databases/x' });
  assert.deepEqual(adminRequest.clone('x', 'y'), { method: 'POST', path: 'api/admin/databases/x/clone', body: { name: 'y' } });
  assert.deepEqual(adminRequest.load('x'), { method: 'POST', path: 'api/admin/databases/x/load' });
  assert.deepEqual(adminRequest.unload('x'), { method: 'POST', path: 'api/admin/databases/x/unload' });
  assert.deepEqual(adminRequest.readOnly('x', 1), { method: 'PUT', path: 'api/admin/databases/x/read-only', body: { readOnly: true } });
  assert.deepEqual(adminRequest.autoLoad('x', 0), { method: 'PUT', path: 'api/admin/databases/x/auto-load', body: { autoLoad: false } });
  assert.deepEqual(adminRequest.aliases(), { method: 'GET', path: 'api/admin/aliases' });
  assert.deepEqual(adminRequest.setAlias('live', 'x'), { method: 'PUT', path: 'api/admin/aliases/live', body: { target: 'x' } });
  assert.deepEqual(adminRequest.removeAlias('live'), { method: 'DELETE', path: 'api/admin/aliases/live' });
  // Paths stay relative: the page may sit behind a proxy prefix.
  for (const build of Object.values(adminRequest)) assert.doesNotMatch(build('x', 'y').path, /^\//);
});

test('parseAdminError: error and help verbatim; a body without error names the status', () => {
  const e = parseAdminError(409, JSON.stringify({ error: '"acme" is loaded', help: 'unload it first', state: 'loaded' }));
  assert.ok(e instanceof AdminError);
  assert.equal(e.message, '"acme" is loaded');
  assert.equal(e.help, 'unload it first');
  assert.equal(e.status, 409);
  assert.equal(e.state, 'loaded');
  assert.equal(parseAdminError(502, '<html>bad gateway</html>').message, 'HTTP 502');
  assert.equal(parseAdminError(500, '').help, '');
});

test('sendAdmin: JSON body with its content type, host headers, network failure as AdminError', async () => {
  const seen = [];
  const fetch = async (url, init) => {
    seen.push({ url: String(url), init });
    return { ok: true, status: 200, text: async () => '{"alias":"live","target":"acme"}' };
  };
  const answer = await sendAdmin({ base: 'http://h/root/', fetch, headers: { 'X-Host': '1' } }, adminRequest.setAlias('live', 'acme'));
  assert.deepEqual(answer, { alias: 'live', target: 'acme' });
  assert.equal(seen[0].url, 'http://h/root/api/admin/aliases/live');
  assert.equal(seen[0].init.method, 'PUT');
  assert.equal(seen[0].init.headers['Content-Type'], 'application/json');
  assert.equal(seen[0].init.headers['X-Host'], '1');
  assert.equal(seen[0].init.body, '{"target":"acme"}');
  await sendAdmin({ base: 'http://h/', fetch }, adminRequest.load('x'));
  assert.equal(seen[1].init.body, undefined, 'no body, no content type');
  assert.equal(seen[1].init.headers['Content-Type'], undefined);
  const down = async () => { throw new TypeError('connection refused'); };
  await assert.rejects(sendAdmin({ base: 'http://h/', fetch: down }, adminRequest.list()), (e) => e instanceof AdminError && /could not be reached: connection refused/.test(e.message));
  const notJson = async () => ({ ok: true, status: 200, text: async () => 'hello' });
  await assert.rejects(sendAdmin({ base: 'http://h/', fetch: notJson }, adminRequest.list()), /no JSON/);
});

// ------------------------------------------------------------------ every action on the mock

test('every admin action against the mock server, with the host headers', async () => {
  const { server, options, log, dbs, aliases } = await mockAdmin();
  try {
    const admin = createAdmin(options);

    // list: rows with the admin fields, aliases and problems
    const list = await fetchDatabases(options);
    assert.deepEqual(list.rows.map((r) => r.name), ['acme', 'broken', 'shared']);
    assert.deepEqual(list.problems, ['tmp/old: removed debris']);
    assert.deepEqual(list.aliases, []);
    assert.equal(list.rows[2].role, 'base');

    // create: plain, base, layer over the base (only documented fields are sent)
    const plain = createBody({ name: 'Notes', description: ' my notes ', autoLoad: true });
    assert.equal((await admin.create(plain.body)).name, 'notes');
    const layer = createBody({ name: 'draft', kind: 'layer', base: 'shared', schema: '{"mode":"open"}' });
    const made = await admin.create(layer.body);
    assert.equal(made.kind, 'layer');
    assert.equal(made.base, 'shared');
    await assert.rejects(admin.create(createBody({ name: 'bad', kind: 'layer', base: 'acme' }).body),
      (e) => e.status === 409 && /not a base/.test(e.message) && /role = "base"/.test(e.help));
    await assert.rejects(admin.create({ name: 'x', colour: 'red' }), /unknown field colour/);

    // load + poll until the state is no longer loading
    const states = [];
    const end = await loadAndWatch(admin, 'notes', { onState: (e) => states.push(e.state), sleep: async () => {} });
    assert.equal(end.state, 'loaded');
    assert.deepEqual(states, ['loading', 'loading', 'loaded']);
    assert.equal((await admin.load('notes')).load, 'already-loaded');

    // read-only and auto-load
    assert.equal((await admin.readOnly('notes', true)).readOnly, true);
    assert.equal((await admin.autoLoad('notes', false)).autoLoad, false);
    await assert.rejects(admin.readOnly('shared', false), (e) => e.status === 409 && /always read-only/.test(e.message) && e.help === 'layers depend on an unchanging base');

    // clone (works on a loaded database)
    const clone = await admin.clone('notes', 'notes-copy');
    assert.equal(clone.state, 'unloaded');
    assert.equal(clone.readOnly, true);

    // aliases: set, flip, remove
    assert.deepEqual(await admin.setAlias('live', 'notes'), { alias: 'live', target: 'notes' });
    assert.deepEqual(await admin.setAlias('live', 'notes-copy'), { alias: 'live', target: 'notes-copy' });
    assert.deepEqual((await admin.aliases()).aliases, { live: 'notes-copy' });
    const withAlias = await fetchDatabases(options);
    assert.deepEqual(withAlias.aliases, [{ alias: 'live', target: 'notes-copy' }]);
    assert.equal(withAlias.rows.find((r) => r.name === 'live').aliasOf, 'notes-copy');
    assert.equal((await admin.get('live')).aliasOf, 'notes-copy', 'GET follows an alias');

    // delete: refused while loaded (help names unload), while an alias names it; then works
    await assert.rejects(admin.delete('notes'), (e) => e.status === 409 && /unload/.test(e.help));
    await assert.rejects(admin.delete('notes-copy'), (e) => /alias "live"/.test(e.message));
    assert.deepEqual(await admin.removeAlias('live'), { alias: 'live', removed: true });
    assert.equal((await admin.unload('notes')).unloaded, true);
    assert.equal((await admin.unload('notes')).unloaded, false);
    const gone = await admin.delete('notes');
    assert.match(gone.trash, /^trash\/notes-/);
    assert.equal(dbs.has('notes'), false);
    assert.deepEqual(aliases, {});
    await assert.rejects(admin.get('notes'), (e) => e.status === 404 && /GET \/api\/admin\/databases/.test(e.help));

    // every request carried the JSON content type with its body; none was refused for headers
    for (const r of log) {
      if (r.body) assert.equal(r.type, 'application/json', `${r.method} ${r.url}`);
    }
    const noToken = createAdmin({ ...options, headers: {} });
    await assert.rejects(noToken.list(), (e) => e.status === 401 && e.message === 'a token is required');
  } finally {
    server.close();
  }
});

test('a load that ends failed or needs a model, and the poll limit', async () => {
  const { server, options } = await mockAdmin({ loadPolls: 1, loadEnds: 'needs-model' });
  try {
    const admin = createAdmin(options);
    const end = await loadAndWatch(admin, 'shared', { sleep: async () => {} });
    assert.equal(end.state, 'needs-model');
    assert.equal(end.model, 'minishlab/potion-base-8M');
    assert.match(outcomeText('load', 'shared', end), /needs a model \(minishlab\/potion-base-8M\): the model is missing/);
  } finally {
    server.close();
  }
  const { server: s2, options: o2 } = await mockAdmin({ loadPolls: 1000 });
  try {
    let clock = 0;
    const end = await loadAndWatch(createAdmin(o2), 'shared', {
      intervalMs: 1000, limitMs: 3000, sleep: async (ms) => { clock += ms; }, now: () => clock,
    });
    assert.equal(end.state, 'loading', 'the panel stops watching; the load goes on');
    assert.equal(clock, 3000);
    let stop = false;
    const polls = [];
    await loadAndWatch(createAdmin(o2), 'acme', { onState: (e) => polls.push(e.state) });
    assert.deepEqual(polls, ['loaded'], 'already loaded: no poll');
    const left = await loadAndWatch(createAdmin(o2), 'broken', { alive: () => !stop, sleep: async () => { stop = true; } });
    assert.equal(left.state, 'loading', 'an unmounted panel stops polling');
  } finally {
    s2.close();
  }
});

// ------------------------------------------------------------------ forms and rules

test('createBody: names, kinds, schema and defaults', () => {
  assert.deepEqual(createBody({ name: 'acme' }), { body: { name: 'acme' } });
  assert.deepEqual(createBody({ name: ' ACME ', kind: 'plain', role: 'base', readOnly: true, autoLoad: true }),
    { body: { name: 'acme', role: 'base', autoLoad: true } }, 'a base is read-only anyway');
  assert.deepEqual(createBody({ name: 'l', kind: 'layer', base: 'shared', role: 'base', readOnly: true }),
    { body: { name: 'l', kind: 'layer', base: 'shared', readOnly: true } }, 'a layer sends no role');
  assert.deepEqual(createBody({ name: 'x', description: 'd', schema: '{"mode":"closed"}' }),
    { body: { name: 'x', description: 'd', schema: { mode: 'closed' } } });
  assert.deepEqual(createBody({ name: '', kind: 'layer' }).problems, ['Enter a name.', 'Choose the base database of the layer.']);
  assert.match(createBody({ name: 'x', schema: '{nope' }).problems[0], /not JSON/);
  assert.deepEqual(createBody({ name: 'x', schema: '[1]' }).problems, ['The schema must be a JSON object.']);
  assert.match(createBody({ name: '-x' }).problems[0], /starts with a letter or digit/);
  assert.equal(nameProblem('a'.repeat(63)), null);
  assert.ok(nameProblem('a'.repeat(64)));
  assert.ok(nameProblem('a/b'));
  assert.equal(nameProblem('my.db_1-2'), null);
});

test('databaseRows carries the admin fields; aliasRows, realRows and baseNames', () => {
  const answer = {
    databases: [
      { name: 'acme', state: 'loaded', kind: 'plain', role: 'normal', readOnly: false, autoLoad: true, description: '', created: '2026-11-22T10:00:00Z', sizeBytes: 2048, memoryBytes: 1048576 },
      { name: 'shared', state: 'unloaded', kind: 'plain', role: 'base', readOnly: true, autoLoad: false },
      { name: 'draft', state: 'needs-model', kind: 'layer', role: 'normal', base: 'shared', reason: 'missing', model: 'm/x' },
      { name: 'live', state: 'loaded', kind: 'plain', role: 'normal', aliasOf: 'acme' },
      { name: 'old', state: 'failed', reason: 'unreadable' },
    ],
    aliases: { live: 'acme', bad: 7 },
    problems: ['p', 3],
  };
  const { rows, aliases, problems } = databaseRows(answer);
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by.acme.autoLoad, true);
  assert.equal(by.acme.sizeBytes, 2048);
  assert.equal(by.acme.memoryBytes, 1048576);
  assert.equal(by.acme.created, '2026-11-22T10:00:00Z');
  assert.equal(by.shared.sizeBytes, null);
  assert.equal(by.draft.base, 'shared');
  assert.equal(by.draft.model, 'm/x');
  assert.equal(by.old.kind, '');
  assert.deepEqual(aliases, [{ alias: 'live', target: 'acme' }]);
  assert.deepEqual(problems, ['p']);
  assert.deepEqual(realRows(rows).map((r) => r.name), ['acme', 'draft', 'old', 'shared']);
  assert.deepEqual(baseNames(rows), ['shared']);
  assert.deepEqual(aliasRows({}), []);
  assert.equal(bytesText(2048), '2.0 KB');
  assert.equal(bytesText(1048576), '1.0 MB');
  assert.equal(bytesText(512), '512 B');
  assert.equal(bytesText(null), '');
});

test('rowActions: what each state offers, and why not', () => {
  const rows = databaseRows({
    databases: [
      { name: 'on', state: 'loaded', kind: 'plain', role: 'normal' },
      { name: 'off', state: 'unloaded', kind: 'plain', role: 'normal' },
      { name: 'busy', state: 'loading', kind: 'layer', role: 'normal' },
      { name: 'base', state: 'unloaded', kind: 'plain', role: 'base', readOnly: true },
      { name: 'bad', state: 'failed', reason: 'unreadable' },
      { name: 'model', state: 'needs-model', kind: 'plain', role: 'normal' },
      { name: 'alias', state: 'loaded', kind: 'plain', role: 'normal', aliasOf: 'on' },
    ],
  }).rows;
  const a = Object.fromEntries(rows.map((r) => [r.name, rowActions(r, { aliasTargets: ['named'] })]));
  const enabled = (acts) => Object.keys(acts).filter((k) => acts[k].enabled).sort();
  assert.deepEqual(enabled(a.on), ['autoLoad', 'clone', 'readOnly', 'unload']);
  assert.equal(a.on.delete.title, 'Unload it first');
  assert.deepEqual(enabled(a.off), ['autoLoad', 'clone', 'delete', 'load', 'readOnly']);
  assert.deepEqual(enabled(a.busy), ['autoLoad', 'clone', 'readOnly']);
  assert.deepEqual(enabled(a.base), ['autoLoad', 'clone', 'delete', 'load']);
  assert.match(a.base.readOnly.title, /always read-only/);
  assert.deepEqual(enabled(a.bad), ['delete'], 'a failed database without a readable config can only be deleted');
  assert.deepEqual(enabled(a.model), ['autoLoad', 'clone', 'delete', 'load', 'readOnly']);
  assert.deepEqual(enabled(a.alias), []);
  assert.match(a.alias.load.title, /alias of on/);
  const named = rowActions({ ...rows[1], name: 'named' }, { aliasTargets: ['named'] });
  assert.match(named.delete.title, /alias names it/);
  assert.deepEqual(enabled(rowActions(rows[1], { busy: true })), []);
});

test('confirmations and outcomes name the subject', () => {
  assert.match(confirmText('delete', 'acme'), /Delete the database "acme"\?.*trash\//);
  assert.match(confirmText('unload', 'acme'), /30 s/);
  assert.match(confirmText('removeAlias', 'live'), /db\/live\//);
  assert.match(confirmText('flipAlias', 'live', 'next'), /"live" to "next"/);
  assert.equal(confirmText('load', 'acme'), null, 'not destructive');
  assert.equal(outcomeText('delete', 'acme', { trash: 'trash/acme-1' }), 'Deleted "acme"; it was moved to trash/acme-1.');
  assert.equal(outcomeText('unload', 'acme', { unloaded: false }), '"acme" was not loaded.');
  assert.equal(outcomeText('load', 'acme', { state: 'failed', reason: 'bad' }), '"acme" failed to load: bad.');
  assert.equal(outcomeText('readOnly', 'acme', { readOnly: true }), '"acme" is read-only.');
  assert.equal(outcomeText('setAlias', 'live', { alias: 'live', target: 'acme' }), 'The alias "live" names "acme".');
});
