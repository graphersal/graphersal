// Node tests of the UI profile model (js/ui-profiles.js): the built-in profiles, validation,
// delivery (names, objects, the link parameter), the routes of profile tabs and `ui.can`.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import {
  BUILTIN_PROFILES, BUILTIN_NAMES, CAN_KEYS, CAN_GROUPS, ProfileError, validateProfile,
  resolveProfile, createUi, profileFromHash, tabRouteFromHash, routeHash, RESERVED_PARAMS,
  checkApiMode, headerProblems,
} from '../js/ui-profiles.js';

const root = fileURLToPath(new URL('..', import.meta.url));

const minimal = (over = {}) => ({
  name: 'mine',
  title: 'Mine',
  startTab: 'one',
  tabs: [{ id: 'one', title: 'One', module: 'one.js' }],
  ...over,
});

test('full: every key reachable, as the WebAssembly playground and the dev server had it', () => {
  const full = BUILTIN_PROFILES.full;
  assert.deepEqual(validateProfile(full), full, 'the built-in profile passes its own validation');
  assert.deepEqual(Object.keys(full.can).sort(), [...CAN_KEYS].sort(), 'full names every key');
  const wasm = createUi(full, { server: false });
  const server = createUi(full, { server: true });
  for (const key of CAN_KEYS) {
    const expected = {
      devBadge: [false, true], mcp: [false, true], saveToFile: [false, true],
      share: [true, false], changeGraph: [true, false], examples: [true, false],
    }[key] || [true, true];
    assert.equal(wasm.can(key), expected[0], `wasm ${key}`);
    assert.equal(server.can(key), expected[1], `server ${key}`);
  }
  assert.equal(wasm.hasWorkspace, true);
  assert.equal(wasm.workspaceTab, 'play');
  assert.equal(full.startTab, 'play');
  assert.equal(full.title, 'Graphersal Playground', 'the page title stays');
});

test('the brief\'s keys exist: top-bar buttons, panels and Query tab kinds', () => {
  for (const key of ['view', 'catalog', 'optimize', 'store', 'share', 'mcp', 'examples', 'tab.ai']) {
    assert.ok(CAN_KEYS.includes(key), key);
  }
  assert.deepEqual(CAN_GROUPS.panels, ['panel.editor', 'panel.results', 'panel.graph', 'panel.schema', 'panel.profile']);
  assert.ok(['tab.query', 'tab.gql', 'tab.ai'].every((k) => CAN_GROUPS.query.includes(k)));
  assert.equal(new Set(CAN_KEYS).size, CAN_KEYS.length, 'no key twice');
});

test('demo: valid, a module tab first, its module file exists', () => {
  const demo = BUILTIN_PROFILES.demo;
  assert.deepEqual(validateProfile(demo), demo);
  const ui = createUi(demo, { server: false });
  assert.equal(ui.startTab, 'hello');
  assert.equal(ui.can('catalog'), false);
  assert.equal(ui.can('panel.editor'), true);
  assert.equal(ui.can('panel.schema'), false, 'a missing key is false');
  for (const tab of demo.tabs.filter((t) => t.module)) {
    assert.ok(existsSync(root + demo.files + tab.module), `${demo.files}${tab.module} exists`);
  }
  assert.equal(ui.moduleUrl(demo.tabs[0], 'http://127.0.0.1:8080/'), 'http://127.0.0.1:8080/js/profiles/demo/hello.js');
});

test('validation names every problem at once', () => {
  let err = null;
  try {
    validateProfile({ name: 'Bad Name', title: '', startTab: 'x', can: { nope: true, view: 'yes' }, tabs: [], extra: 1, files: 'x' });
  } catch (e) {
    err = e;
  }
  assert.ok(err instanceof ProfileError);
  const text = err.message;
  for (const part of ['unknown key "extra"', '"name"', '"title"', 'unknown key "nope"', '"can.view"', '"tabs"', '"files"']) {
    assert.ok(text.includes(part), `${part} in: ${text}`);
  }
  assert.ok(err.problems.length >= 7);
});

test('validation of tabs: ids, one builtin, module paths, startTab', () => {
  const bad = (over, part) => {
    assert.throws(() => validateProfile(minimal(over)), (e) => e instanceof ProfileError && e.message.includes(part), part);
  };
  bad({ tabs: [{ id: 'one', title: 'One', module: 'a.js' }, { id: 'one', title: 'Two', module: 'b.js' }] }, 'used twice');
  bad({ tabs: [{ id: 'one', title: 'One', builtin: 'workspace' }, { id: 'two', title: 'Two', builtin: 'workspace' }] }, 'only once');
  bad({ tabs: [{ id: 'one', title: 'One', builtin: 'graph' }] }, '.builtin must be one of workspace');
  bad({ tabs: [{ id: 'one', title: 'One' }] }, 'exactly one of "builtin" and "module"');
  bad({ tabs: [{ id: 'one', title: 'One', builtin: 'workspace', module: 'a.js' }] }, 'exactly one of');
  for (const module of ['../x.js', 'https://evil.example/x.js', '//evil.example/x.js', '/abs.js', 'a/./b.js', 'x.css', 'a\\b.js', 'x.js?v=1']) {
    bad({ tabs: [{ id: 'one', title: 'One', module }] }, '.module must be');
  }
  bad({ startTab: 'two' }, '"startTab" must be the id of one of the tabs (one)');
  bad({ tabs: [{ id: 'one', title: 'One', module: 'a.js', icon: 'x' }] }, 'unknown key "icon"');
  bad({ tabs: Array.from({ length: 13 }, (_, i) => ({ id: `t${i}`, title: 'T', module: 'a.js' })), startTab: 't0' }, '1 to 12 tabs');
});

test('validation of files and cards; normalized copy is frozen', () => {
  for (const files of ['', 'profile/', '/memory/', 'a/b/']) {
    assert.equal(validateProfile(minimal({ files })).files, files);
  }
  for (const files of ['profile', '../up/', 'https://cdn.example/', '//cdn.example/', 'a/../b/']) {
    assert.throws(() => validateProfile(minimal({ files })), /"files" must be/, files);
  }
  assert.throws(() => validateProfile(minimal({ cards: [] })), /"cards" must be an object/);
  const p = validateProfile(minimal({ cards: { person: { title: 'name' } } }));
  assert.deepEqual(p.cards, { person: { title: 'name' } });
  assert.deepEqual(p.can, {});
  assert.equal(p.files, '');
  assert.ok(Object.isFrozen(p) && Object.isFrozen(p.tabs) && Object.isFrozen(p.tabs[0]) && Object.isFrozen(p.cards.person));
  assert.throws(() => validateProfile(null), /must be an object/);
  assert.throws(() => validateProfile([]), /must be an object/);
});

test('resolveProfile: nothing is full, names are built-ins only, objects are validated', () => {
  assert.equal(resolveProfile(undefined), BUILTIN_PROFILES.full);
  assert.equal(resolveProfile(null), BUILTIN_PROFILES.full);
  assert.equal(resolveProfile('demo'), BUILTIN_PROFILES.demo);
  assert.deepEqual(BUILTIN_NAMES, ['full', 'demo']);
  assert.throws(() => resolveProfile('memory'), (e) => e instanceof ProfileError
    && e.message === 'The UI profile "memory" is unknown: the built-in profiles are full, demo.');
  assert.throws(() => resolveProfile('toString'), /unknown/, 'no prototype key is a profile');
  assert.throws(() => resolveProfile(42), /built-in name or a profile object/);
  assert.equal(resolveProfile(minimal()).name, 'mine');
  assert.throws(() => resolveProfile(minimal({ title: 5 })), /The UI profile "mine" is invalid: "title"/);
});

test('ui.can: values true/false/wasm/server, unknown keys are false', () => {
  const profile = validateProfile(minimal({ can: { view: true, store: false, share: 'wasm', mcp: 'server' } }));
  const wasm = createUi(profile, { server: false });
  const server = createUi(profile, { server: true });
  assert.deepEqual(['view', 'store', 'share', 'mcp'].map((k) => wasm.can(k)), [true, false, true, false]);
  assert.deepEqual(['view', 'store', 'share', 'mcp'].map((k) => server.can(k)), [true, false, false, true]);
  const warn = console.warn;
  const warnings = [];
  console.warn = (m) => warnings.push(m);
  try {
    assert.equal(wasm.can('nope'), false);
    assert.equal(wasm.can('nope'), false);
  } finally {
    console.warn = warn;
  }
  assert.equal(warnings.length, 1, 'warned once');
  assert.equal(wasm.hasWorkspace, false);
  assert.equal(wasm.workspaceTab, null);
  assert.equal(wasm.tab('one').module, 'one.js');
  assert.equal(wasm.tab('two'), null);
});

test('the link parameter and the routes of profile tabs', () => {
  assert.equal(profileFromHash('#/play?profile=demo'), 'demo');
  assert.equal(profileFromHash('#/?profile=demo'), 'demo');
  assert.equal(profileFromHash('#/play?graph=modern&q=g.v()&profile=demo'), 'demo');
  assert.equal(profileFromHash('#/play'), null);
  assert.equal(profileFromHash(''), null);
  assert.deepEqual(tabRouteFromHash('#/play?profile=demo&tab=hello&slug=a%20b&n=2'), { tab: 'hello', params: { slug: 'a b', n: '2' } });
  assert.deepEqual(tabRouteFromHash('#/play'), { tab: null, params: {} });
  assert.deepEqual(tabRouteFromHash('#/?tab=x'), { tab: null, params: {} }, 'only on the workspace route');
  assert.equal(routeHash({}), '#/play');
  assert.equal(routeHash({ route: 'start' }), '#/');
  assert.equal(routeHash({ route: 'start', profile: 'demo' }), '#/?profile=demo');
  assert.equal(routeHash({ profile: 'full', tab: 'play' }), '#/play?tab=play', 'full is never written');
  assert.equal(routeHash({ profile: 'demo', tab: 'hello', params: { slug: 'a b', n: 2, skip: null } }), '#/play?profile=demo&tab=hello&slug=a%20b&n=2');
  assert.deepEqual(tabRouteFromHash(routeHash({ profile: 'demo', tab: 'hello', params: { x: '1' } })), { tab: 'hello', params: { x: '1' } });
  for (const name of RESERVED_PARAMS) {
    assert.throws(() => routeHash({ tab: 'hello', params: { [name]: '1' } }), /reserved/, name);
  }
});

test('index.html: every CDN script and module carries SRI', () => {
  const html = readFileSync(root + 'index.html', 'utf8');
  const scripts = [...html.matchAll(/<script[^>]*\ssrc="(https:[^"]+)"[^>]*>/g)];
  assert.ok(scripts.length >= 2);
  for (const [tag, src] of scripts) {
    assert.match(tag, /integrity="sha(384|512)-[A-Za-z0-9+/=]+"/, src);
    assert.match(tag, /crossorigin="anonymous"/, src);
  }
  const map = JSON.parse(/<script type="importmap">([\s\S]*?)<\/script>/.exec(html)[1]);
  for (const url of Object.values(map.imports)) {
    assert.match(map.integrity[url] || '', /^sha(384|512)-/, `importmap integrity for ${url}`);
  }
});

test('requestedProfile: full and none ask for the same', async () => {
  const { requestedProfile } = await import('../js/ui-profiles.js');
  assert.equal(requestedProfile('#/play?profile=full'), null);
  assert.equal(requestedProfile('#/play'), null);
  assert.equal(requestedProfile('#/play?profile=demo'), 'demo');
  assert.equal(requestedProfile('#/play?profile='), '', 'an empty name is asked for (and refused as unknown)');
  assert.throws(() => resolveProfile(''), /The UI profile "" is unknown/);
});

test('every lazily loaded CDN file carries SRI', () => {
  // A CDN URL in the scripts sits in an object with its integrity (js/cdn-script.js loads it).
  const files = ['js/graph/sigma-renderer.js', 'js/components/schema-panel.js'];
  for (const file of files) {
    const text = readFileSync(root + file, 'utf8');
    const urls = [...text.matchAll(/'(https:\/\/[^']+)'/g)];
    assert.ok(urls.length > 0, file);
    for (const m of urls) {
      const after = text.slice(m.index, m.index + 400);
      assert.match(after, /integrity: 'sha(384|512)-[A-Za-z0-9+/=]+'/, `${file}: ${m[1]}`);
    }
    assert.ok(!/\bimport\(\s*'https:/.test(text), `${file}: no dynamic import of a CDN module`);
  }
});

test('checkApiMode: a server in allow-list mode refuses a profile with the workspace', () => {
  const modules = validateProfile(minimal());
  for (const mode of [undefined, 'full', 'allow-list']) checkApiMode(modules, mode);
  for (const profile of [BUILTIN_PROFILES.full, BUILTIN_PROFILES.demo]) {
    checkApiMode(profile);
    checkApiMode(profile, 'full');
    assert.throws(() => checkApiMode(profile, 'allow-list'), (e) => e instanceof ProfileError
      && /cannot be shown by this server/.test(e.message)
      && /allow-list mode/.test(e.message)
      && e.problems.length === 2);
  }
});

test('version: optional product version, kept only when given', () => {
  assert.equal('version' in validateProfile(minimal()), false);
  assert.equal(validateProfile(minimal({ version: '0.1.0' })).version, '0.1.0');
  for (const version of ['', ' ', '0.1 beta', 'x'.repeat(41), 1, null]) {
    assert.throws(() => validateProfile(minimal({ version })), /"version" must be a text of 1 to 40 characters/, String(version));
  }
});

test('header: a top-bar module, validated like a module tab, kept only when given', () => {
  const ok = validateProfile(minimal({ header: { module: 'top/header.js' } }));
  assert.deepEqual(ok.header, { module: 'top/header.js' });
  assert.ok(Object.isFrozen(ok.header));
  assert.equal('header' in validateProfile(minimal()), false, 'absent stays absent');
  const ui = createUi(validateProfile(minimal({ header: { module: 'h.mjs' }, files: 'p/' })));
  assert.equal(ui.moduleUrl(ui.header, 'http://127.0.0.1:8080/'), 'http://127.0.0.1:8080/p/h.mjs');
  assert.equal(createUi(validateProfile(minimal())).header, null);
  const bad = (header, part) => {
    assert.throws(() => validateProfile(minimal({ header })), (e) => e instanceof ProfileError && e.message.includes(part), part);
  };
  for (const module of ['../x.js', 'https://evil.example/x.js', '//evil.example/x.js', '/abs.js', 'a/./b.js', 'x.css', 'a\\b.js', 'x.js?v=1', 3, undefined]) {
    bad({ module }, '"header.module" must be');
  }
  bad({ module: 'h.js', params: {} }, '"header" has the unknown key "params"');
  bad('h.js', '"header" must be an object');
  bad([], '"header" must be an object');
  assert.deepEqual(headerProblems({ module: 'h.js' }), []);
  assert.equal(headerProblems({ title: 'x' }).length, 2, 'the unknown key and the missing module');
});

test('demo: its header module file exists; the workspace renders the header left of the buttons', () => {
  const demo = BUILTIN_PROFILES.demo;
  assert.ok(existsSync(root + demo.files + demo.header.module));
  const ws = readFileSync(root + 'js/components/workspace.js', 'utf8');
  const spacer = ws.indexOf('<span class="spacer"></span>', ws.indexOf('<header class="topbar">'));
  const header = ws.indexOf('<profile-header');
  const theme = ws.indexOf("can('theme')");
  const share = ws.indexOf("can('share')", spacer);
  assert.ok(spacer > 0 && spacer < header && header < share && header < theme, 'spacer < header module < Share < theme');
});
