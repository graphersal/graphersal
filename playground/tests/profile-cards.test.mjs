// Node tests of the profile cards model (js/profile-cards.js): template validation, field paths
// on elements and maps, missing fields, formats, badges, compact vs full, actions and options.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  cardTemplateProblems, cardsProblems, actionProblems, fieldValue, resolveRef, isMissing,
  formatCardValue, cardModel, checkCardOptions, markdownPlainText, CARD_LIMITS,
} from '../js/profile-cards.js';
import { validateProfile, BUILTIN_PROFILES } from '../js/ui-profiles.js';

const vertex = { id: '1', label: 'person', properties: { name: 'marko', age: 29, tags: ['a', 'b'], nested: { city: 'Rome' } } };
const valueMapRow = { name: ['marko'], age: [29] };

// The memory card graphersal-memory will show (design: memory-ui-design.md section 6).
const MEMORY = {
  title: ['title', 'slug'],
  subtitle: ['kind', 'scope', 'source', { field: 'updated', format: 'date' }],
  badges: [
    { field: 'pinned', text: 'pinned', tone: 'accent' },
    { field: 'superseded_by', text: 'replaced', tone: 'warn' },
    { field: 'conflicts', text: 'conflict', tone: 'danger' },
  ],
  body: 'body',
  fields: [{ field: 'why', title: 'Why', format: 'markdown' }, { field: 'links', title: 'Links' }, { field: 'created', title: 'Created', format: 'datetime' }],
  actions: [
    { id: 'pin', text: 'Pin', unless: 'pinned' },
    { id: 'unpin', text: 'Unpin', when: 'pinned' },
    { id: 'forget', text: 'Forget', kind: 'danger', unless: 'imported_at' },
  ],
};

test('a valid template has no problems; the memory, session and fact cards validate', () => {
  assert.deepEqual(cardTemplateProblems(MEMORY, 'cards.memory'), []);
  assert.deepEqual(cardTemplateProblems({}, 'cards.x'), []);
  const session = { title: 'headline', subtitle: [{ field: 'started', format: 'date' }, 'harness'], badges: [{ field: 'decisions', format: 'count', prefix: 'decisions ' }], fields: [] };
  const fact = { title: 'text', badges: [{ field: 'kind', tone: { decision: 'accent', correction: 'warn', outcome: 'ok', open_thread: 'danger' } }], subtitle: [{ field: 'date', format: 'date' }, 'session'], body: 'why' };
  assert.deepEqual(cardsProblems({ memory: MEMORY, session, fact }), []);
  const p = validateProfile({ name: 'm', title: 'M', startTab: 'a', tabs: [{ id: 'a', title: 'A', module: 'a.js' }], cards: { memory: MEMORY } });
  assert.deepEqual(p.cards.memory, MEMORY);
});

test('template problems name every key that is wrong', () => {
  const problems = cardTemplateProblems({
    titel: 'x',
    title: '',
    subtitle: [{ field: 'a', colour: 'red' }, 5],
    badges: [{ field: 'a', tone: 'pink' }, { text: 'fixed' }, { field: 'b', format: 'markdown' }],
    body: ['ok', 'bad..path'],
    excerpt: -1,
    fields: 'name',
    actions: [{ id: '1x', text: '' }, { id: 'a', text: 'A', kind: 'huge' }, { id: 'a', text: 'B' }],
  }, 'cards.p');
  const text = problems.join('\n');
  for (const needle of [
    'cards.p has the unknown key "titel"',
    'cards.p.title must be a field path',
    'cards.p.subtitle[0] has the unknown key "colour"',
    'cards.p.subtitle[1] must be a field path or an object',
    'cards.p.badges[0].tone must be one of neutral, accent, ok, warn, danger',
    'cards.p.badges[1].field is required',
    'cards.p.badges[2].format "markdown" is only for "fields"',
    'cards.p.body must be a list of field paths',
    'cards.p.excerpt must be a whole number',
    'cards.p.fields must be a list',
    'cards.p.actions[0].id must be letters',
    'cards.p.actions[0].text must be a text of 1 to 40 characters',
    'cards.p.actions[1].kind must be one of default, primary, danger',
    'cards.p.actions[2].id "a" is used twice',
  ]) assert.ok(text.includes(needle), `missing: ${needle}\n${text}`);
  assert.deepEqual(cardTemplateProblems([], 'cards.q'), ['cards.q must be an object (a card template)']);
  assert.match(cardTemplateProblems({ subtitle: [{ field: 'a', tone: 'ok' }] }, 'c').join(), /tone is only for "badges"/);
  assert.deepEqual(cardTemplateProblems({ badges: [{ text: 'new', when: 'fresh' }] }, 'c'), [], 'a fixed badge with when');
});

test('validateProfile reports card problems with the label', () => {
  assert.throws(
    () => validateProfile({ name: 'm', title: 'M', startTab: 'a', tabs: [{ id: 'a', title: 'A', module: 'a.js' }], cards: { memory: { title: 5, budge: [] } } }),
    (e) => e.problems.some((p) => p.startsWith('"cards.memory".title must be'))
      && e.problems.some((p) => p.includes('"cards.memory" has the unknown key "budge"')),
  );
  assert.throws(() => validateProfile({ name: 'm', title: 'M', startTab: 'a', tabs: [{ id: 'a', title: 'A', module: 'a.js' }], cards: { memory: 'title' } }),
    /"cards.memory" must be an object \(a card template\)/);
});

test('field paths: element properties first, then the element; value_map lists unwrapped; nesting and indexes', () => {
  assert.equal(fieldValue(vertex, 'name'), 'marko');
  assert.equal(fieldValue(vertex, 'id'), '1');
  assert.equal(fieldValue(vertex, 'label'), 'person');
  assert.equal(fieldValue(vertex, 'nested.city'), 'Rome');
  assert.equal(fieldValue(vertex, 'tags.1'), 'b');
  assert.equal(fieldValue(vertex, 'tags.7'), undefined);
  assert.equal(fieldValue(vertex, 'properties.name'), 'marko');
  assert.equal(fieldValue(valueMapRow, 'name'), 'marko');
  assert.equal(fieldValue({ a: [{ b: 1 }] }, 'a.b'), 1, 'a one-item list on the way is unwrapped');
  assert.equal(fieldValue({ a: 1 }, 'a.b'), undefined);
  assert.equal(fieldValue(null, 'a'), undefined);
  assert.equal(fieldValue('text', 'a'), undefined);
  assert.equal(fieldValue({ a: 1 }, ''), undefined);
  assert.equal(fieldValue({ constructor: 1 }, 'toString'), undefined, 'no prototype keys');
  assert.deepEqual(resolveRef({ slug: 's', title: '' }, ['title', 'slug']), { value: 's', path: 'slug' });
  assert.equal(resolveRef({}, ['title', 'slug']), null);
  for (const v of [undefined, null, '', []]) assert.ok(isMissing(v));
  for (const v of [0, false, 'x', [1], {}]) assert.ok(!isMissing(v));
});

test('formats: text, lists, dates from ISO text and milliseconds, count; never "undefined"', () => {
  assert.equal(formatCardValue(undefined), '');
  assert.equal(formatCardValue(null), '');
  assert.equal(formatCardValue(['a', 'b']), 'a, b');
  assert.equal(formatCardValue({ a: 1 }), '{"a":1}');
  assert.equal(formatCardValue(false), 'false');
  assert.equal(formatCardValue('2026-10-28T10:15:00Z', 'date'), '2026-10-28');
  assert.equal(formatCardValue('2026-10-28T10:15:00Z', 'datetime'), '2026-10-28 10:15');
  assert.equal(formatCardValue('2026-10-28', 'datetime'), '2026-10-28');
  assert.equal(formatCardValue(Date.UTC(2026, 10, 13, 8, 5), 'datetime'), '2026-11-13 08:05');
  assert.equal(formatCardValue('yesterday', 'date'), 'yesterday', 'a text that is no date stays as it is');
  assert.equal(formatCardValue({ '@type': 'g:Date', '@value': '2026-01-02' }, 'date'), '2026-01-02');
  assert.equal(formatCardValue([1, 2, 3], 'count'), '3');
  assert.equal(formatCardValue(4, 'count'), '4');
});

test('the memory card, full: heading, subtitle, badges, Markdown body, fields; no action without onAction', () => {
  const row = {
    slug: 'result-for-shared-state', title: 'Result for shared state', kind: 'feedback', scope: 'project', source: 'agent',
    updated: '2026-10-28T09:00:00Z', created: '2026-10-27T08:30:00Z', pinned: true, superseded_by: null, conflicts: [],
    body: 'Public methods **touching shared state** return `Result`.\n\n<script>alert(1)</script>',
    why: 'So an error can be *added* later.', links: ['a', 'b'],
  };
  const m = cardModel('memory', row, MEMORY, checkCardOptions({}));
  assert.deepEqual(m.heading, { text: 'Result for shared state', fromLabel: false });
  assert.equal(m.subtitle, 'feedback · project · agent · 2026-10-28');
  assert.deepEqual(m.badges, [{ text: 'pinned', tone: 'accent' }], 'missing superseded_by and empty conflicts show no badge');
  assert.equal(m.body, row.body, 'the Markdown text goes to the safe renderer as it is');
  assert.equal(m.excerpt, null);
  assert.deepEqual(m.fields.map((f) => [f.title, f.value, f.format]), [
    ['Why', 'So an error can be *added* later.', 'markdown'],
    ['Links', ['a', 'b'], 'text'],
    ['Created', '2026-10-27 08:30', 'datetime'],
  ]);
  assert.deepEqual(m.actions, []);
  assert.equal(m.variant, 'full');
  assert.equal(m.openable, false);
});

test('the memory card, compact: excerpt of the body as plain text, no fields; actions by when/unless', () => {
  const row = { slug: 'kg', kind: 'project', superseded_by: 'memory-in-graphersal-memory', imported_at: '2026-11-11', body: `# Heading\n\nThe KB is **the source** of truth ${'x'.repeat(300)}` };
  const m = cardModel('memory', row, MEMORY, checkCardOptions({ variant: 'compact', selected: true, onOpen() {}, onAction() {} }));
  assert.deepEqual(m.heading, { text: 'kg', fromLabel: false }, 'the title falls back to the slug');
  assert.equal(m.subtitle, 'project');
  assert.deepEqual(m.badges, [{ text: 'replaced', tone: 'warn' }]);
  assert.equal(m.body, null);
  assert.ok(m.excerpt.startsWith('Heading The KB is the source of truth'), m.excerpt);
  assert.ok(m.excerpt.endsWith('…') && m.excerpt.length <= CARD_LIMITS.excerptDefault + 1);
  assert.deepEqual(m.fields, []);
  assert.deepEqual(m.actions.map((a) => a.id), ['pin'], 'imported: no forget; not pinned: pin, no unpin');
  assert.ok(m.selected && m.openable);
  const noExcerpt = cardModel('memory', row, { ...MEMORY, excerpt: 0 }, { variant: 'compact' });
  assert.equal(noExcerpt.excerpt, null);
});

test('missing everything: the heading is the label, nothing else renders', () => {
  const m = cardModel('memory', {}, MEMORY, checkCardOptions({ onAction() {} }));
  assert.deepEqual(m.heading, { text: 'memory', fromLabel: true });
  assert.equal(m.subtitle, '');
  assert.deepEqual(m.badges, []);
  assert.equal(m.body, null);
  assert.deepEqual(m.fields, []);
  assert.deepEqual(m.actions.map((a) => a.id), ['pin', 'forget']);
  assert.ok(!JSON.stringify(m).includes('undefined'));
});

test('the default card (no template): every field of the row; elements show their properties', () => {
  const m = cardModel('person', vertex, null);
  assert.deepEqual(m.heading, { text: 'person', fromLabel: true });
  assert.deepEqual(m.fields.map((f) => [f.key, f.value]), [['name', 'marko'], ['age', 29], ['tags', ['a', 'b']], ['nested', { city: 'Rome' }]]);
  assert.deepEqual(cardModel('person', valueMapRow, null).fields.map((f) => [f.key, f.value]), [['name', 'marko'], ['age', 29]]);
  assert.deepEqual(cardModel('n', 42, null).fields.map((f) => [f.key, f.value]), [['value', 42]]);
  assert.deepEqual(cardModel('n', null, null).fields, []);
  // Fields the title, subtitle, badges and body show are left out of the default list.
  const t = cardModel('person', { name: 'm', age: 1, city: 'x', note: 'n' }, { title: 'name', subtitle: ['age'], badges: [{ field: 'city' }] });
  assert.deepEqual(t.fields.map((f) => f.key), ['note']);
  assert.deepEqual(cardModel('person', { name: 'm', blank: '', none: null }, { title: 'name' }).fields, [], 'missing values left out');
  assert.deepEqual(cardModel('person', vertex, { fields: [] }).fields, []);
});

test('badges: value text, true shows the field name, tone maps, when/unless, prefix, zero hides', () => {
  const t = {
    badges: [
      { field: 'kind', tone: { correction: 'warn' } },
      { field: 'pinned' },
      { field: 'count', prefix: 'n=' },
      { text: 'new', when: 'fresh' },
      { text: 'old', unless: 'fresh' },
      { field: 'zero' },
      { field: 'off' },
    ],
  };
  const m = cardModel('fact', { kind: 'correction', pinned: true, count: 3, fresh: true, zero: 0, off: false }, t);
  assert.deepEqual(m.badges, [
    { text: 'correction', tone: 'warn' },
    { text: 'pinned', tone: 'neutral' },
    { text: 'n=3', tone: 'neutral' },
    { text: 'new', tone: 'neutral' },
  ]);
  assert.deepEqual(cardModel('fact', { kind: 'decision' }, t).badges, [{ text: 'decision', tone: 'neutral' }, { text: 'old', tone: 'neutral' }]);
});

test('actions: the module may replace the template\'s; disabled and kinds pass through', () => {
  const options = checkCardOptions({ onAction() {}, actions: [{ id: 'open', text: 'Open', kind: 'primary', disabled: true, title: 'Open it' }] });
  assert.deepEqual(cardModel('memory', { pinned: true }, MEMORY, options).actions, [{ id: 'open', text: 'Open', kind: 'primary', title: 'Open it', disabled: true }]);
  assert.deepEqual(actionProblems([{ id: 'a', text: 'A', extra: 1 }], 'x'), ['x[0] has the unknown key "extra" (the keys are id, text, kind, when, unless, title, disabled)']);
  assert.match(actionProblems('no', 'x')[0], /must be a list/);
});

test('checkCardOptions: defaults, and a TypeError naming every bad option', () => {
  assert.deepEqual(checkCardOptions(undefined), { variant: 'full', selected: false, onOpen: null, actions: null, onAction: null });
  assert.equal(checkCardOptions({ variant: 'compact' }).variant, 'compact');
  assert.throws(() => checkCardOptions([]), /the options must be an object/);
  assert.throws(() => checkCardOptions({ variant: 'tiny', selected: 'yes', onAction: 'x', colour: 1, actions: [{}] }), (e) => e instanceof TypeError
    && /unknown option "colour"/.test(e.message) && /"variant" must be one of full, compact/.test(e.message)
    && /"selected" must be true or false/.test(e.message) && /"onAction" must be a function/.test(e.message)
    && /"actions"\[0\]\.id/.test(e.message));
});

test('markdownPlainText flattens blocks to one line of text (no markup)', () => {
  assert.equal(markdownPlainText('# Title\n\n- **one**\n- `two`\n\n> quoted [link](https://x.org)'), 'Title one two quoted link');
  assert.equal(markdownPlainText('<b>raw</b>'), '<b>raw</b>', 'raw HTML stays text');
});

test('the demo profile\'s person card renders the modern graph\'s rows', () => {
  const t = BUILTIN_PROFILES.demo.cards.person;
  assert.deepEqual(cardTemplateProblems(t, 'cards.person'), []);
  const row = { name: 'marko', age: 29, senior: false, about: '**marko** is 29.' };
  const compact = cardModel('person', row, t, checkCardOptions({ variant: 'compact', onOpen() {} }));
  assert.equal(compact.heading.text, 'marko');
  assert.equal(compact.subtitle, 'age 29', 'the missing city is left out');
  assert.deepEqual(compact.badges, []);
  assert.equal(compact.excerpt, 'marko is 29.');
  assert.deepEqual(compact.actions, []);
  const full = cardModel('person', { ...row, age: 32, senior: true }, t, checkCardOptions({ onAction() {} }));
  assert.deepEqual(full.badges, [{ text: '30+', tone: 'accent' }]);
  assert.deepEqual(full.fields.map((f) => [f.title, f.value]), [['Name', 'marko'], ['Age', 32]]);
  assert.deepEqual(full.actions.map((a) => a.id), ['hello']);
  assert.equal(BUILTIN_PROFILES.full.cards && Object.keys(BUILTIN_PROFILES.full.cards).length, 0, 'full defines no cards');
});
