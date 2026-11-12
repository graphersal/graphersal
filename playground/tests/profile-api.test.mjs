// Node tests of the profile API (js/profile-api.js) with injected dependencies: the saved-query
// call and its errors, Markdown and cards, toasts, navigation, theme tokens and the info.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createProfileApi, ProfileQueryError, PROFILE_API_VERSION, TOAST_KINDS } from '../js/profile-api.js';

function harness(over = {}) {
  const calls = { queries: [], toasts: [], navigations: [], markdown: [], cards: [] };
  const deps = {
    profile: { name: 'demo', title: 'Demo', can: {} },
    engineInfo: { version: '0.6.1', readOnly: true, nested: { a: 1 } },
    server: true,
    runQuery: async (name, params) => {
      calls.queries.push([name, params]);
      return { ok: true, results: [{ name: 'marko' }] };
    },
    renderMarkdown: (text) => {
      calls.markdown.push(text);
      return { vnode: 'md', text };
    },
    renderCard: (label, row) => {
      calls.cards.push([label, row]);
      return { vnode: 'card', label };
    },
    showToast: (text, kind) => calls.toasts.push([kind, text]),
    navigate: (tab, params) => {
      if (tab !== 'hello') throw new Error(`no tab ${tab}`);
      calls.navigations.push([tab, params]);
    },
    themeState: () => ({ mode: 'system', dark: true }),
    cssToken: (name) => (name === 'accent' ? ' #2563eb ' : ''),
    ...over,
  };
  return { api: createProfileApi(deps), calls };
}

test('the api object is frozen and versioned', () => {
  const { api } = harness();
  assert.equal(api.version, PROFILE_API_VERSION);
  assert.equal(PROFILE_API_VERSION, 1);
  assert.ok(Object.isFrozen(api));
  assert.deepEqual(Object.keys(api).sort(), ['card', 'info', 'markdown', 'navigate', 'query', 'theme', 'toast', 'version']);
});

test('query: name and params go to the engine, results come back', async () => {
  const { api, calls } = harness();
  assert.deepEqual(await api.query('people', { project: 'p' }), [{ name: 'marko' }]);
  assert.deepEqual(await api.query('people'), [{ name: 'marko' }]);
  assert.deepEqual(calls.queries, [['people', { project: 'p' }], ['people', {}]]);
});

test('query: refusals and failures reject with ProfileQueryError', async () => {
  const failing = harness({ runQuery: async () => ({ ok: false, error: { message: 'boom', help: 'fix it' } }) }).api;
  await assert.rejects(failing.query('q'), (e) => e instanceof ProfileQueryError && e.message === 'boom' && e.help === 'fix it' && e.query === 'q');
  const throwing = harness({ runQuery: async () => { throw new Error('403 forbidden'); } }).api;
  await assert.rejects(throwing.query('q'), (e) => e instanceof ProfileQueryError && e.message === '403 forbidden');
  const bare = harness({ runQuery: async () => ({ ok: false }) }).api;
  await assert.rejects(bare.query('q'), /The saved query "q" failed\./);
  const { api, calls } = harness();
  await assert.rejects(api.query(''), /non-empty text/);
  await assert.rejects(api.query('q', [1]), /object of named arguments/);
  await assert.rejects(api.query('q', null), /object of named arguments/);
  assert.equal(calls.queries.length, 0, 'nothing reached the engine');
  const noResults = harness({ runQuery: async () => ({ ok: true }) }).api;
  assert.deepEqual(await noResults.query('q'), []);
});

test('markdown and card hand over to the renderers', () => {
  const { api, calls } = harness();
  assert.deepEqual(api.markdown('# Hi'), { vnode: 'md', text: '# Hi' });
  api.markdown(null);
  assert.deepEqual(calls.markdown, ['# Hi', '']);
  assert.deepEqual(api.card('person', { name: 'marko' }), { vnode: 'card', label: 'person' });
  assert.deepEqual(calls.cards, [['person', { name: 'marko' }]]);
});

test('toast takes the four kinds only', () => {
  const { api, calls } = harness();
  for (const kind of TOAST_KINDS) api.toast(kind, `a ${kind}`);
  assert.deepEqual(calls.toasts.map((t) => t[0]), ['info', 'success', 'warn', 'error']);
  assert.throws(() => api.toast('loud', 'x'), /kind must be one of info, success, warn, error/);
  assert.throws(() => api.toast('x'), TypeError);
});

test('navigate passes on; unknown tabs and bad params throw', () => {
  const { api, calls } = harness();
  api.navigate('hello', { slug: 'a' });
  api.navigate('hello');
  assert.deepEqual(calls.navigations, [['hello', { slug: 'a' }], ['hello', {}]]);
  assert.throws(() => api.navigate('nope'), /no tab nope/);
  assert.throws(() => api.navigate('hello', 'x'), /must be an object/);
});

test('theme and info', () => {
  let dark = true;
  const { api } = harness({ themeState: () => ({ mode: 'dark', dark }) });
  assert.equal(api.theme.mode, 'dark');
  assert.equal(api.theme.dark, true);
  dark = false;
  assert.equal(api.theme.dark, false, 'read live');
  assert.equal(api.theme.token('accent'), '#2563eb');
  assert.equal(api.theme.token('nope'), '');
  assert.equal(api.theme.token('--accent'), '', 'a name, not a CSS variable');
  assert.deepEqual(api.info.profile, { name: 'demo', title: 'Demo' });
  assert.equal(api.info.server, true);
  assert.equal(api.info.version, '0.6.1');
  assert.equal(api.info.readOnly, true);
  assert.equal(api.info.engine.nested.a, 1);
  assert.ok(Object.isFrozen(api.info.engine.nested), 'a frozen copy');
  const wasm = harness({ server: false, engineInfo: undefined }).api;
  assert.deepEqual([wasm.info.server, wasm.info.version, wasm.info.readOnly], [false, null, false]);
});

test('card passes checked options to the renderer and refuses bad ones before rendering', () => {
  const seen = [];
  const { api } = harness({ renderCard: (label, row, options) => { seen.push([label, row, options]); return { vnode: 'card' }; } });
  const onOpen = () => {};
  api.card('memory', { slug: 's' }, { variant: 'compact', selected: true, onOpen });
  assert.deepEqual(seen[0], ['memory', { slug: 's' }, { variant: 'compact', selected: true, onOpen, actions: null, onAction: null }]);
  api.card(undefined, null);
  assert.deepEqual(seen[1], ['', null, { variant: 'full', selected: false, onOpen: null, actions: null, onAction: null }]);
  assert.throws(() => api.card('memory', {}, { variant: 'big' }), /api\.card: "variant" must be one of full, compact/);
  assert.throws(() => api.card('memory', {}, { actions: [{ id: 'x', text: 'X' }], onAction: 1 }), /"onAction" must be a function/);
  assert.equal(seen.length, 2, 'nothing rendered for bad options');
});
