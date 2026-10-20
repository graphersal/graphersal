// Node tests of the Examples panel model (js/examples-panel.js): the group tree with GQL
// subgroups, the plain case-insensitive search over titles and scripts with highlight pieces and
// one-line snippets, the shown tree while browsing and searching, the keyboard order and the
// remembered state; plus the shape of js/examples.json itself.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  exampleId, sectionKey, groupExamples, groupSize, findMatch, splitAt, snippet, searchExamples,
  visibleTree, flatVisible, stepSelection, loadPanelState, savePanelState,
} from '../js/examples-panel.js';

const EXAMPLES = JSON.parse(readFileSync(new URL('../js/examples.json', import.meta.url), 'utf8'));

const SAMPLE = [
  { group: 'Modern graph', title: 'Count', script: 'g.v().count()' },
  { group: 'Modern graph', title: 'Ordered', script: 'g.v()\n  .order().by("age")' },
  { group: 'GQL', subgroup: 'Basics', title: 'Who knows', script: 'g.gql("MATCH (p)-[:knows]->(f) RETURN f ORDER BY f")' },
  { group: 'GQL', subgroup: 'Paths', title: 'Hops', script: 'g.gql("MATCH (a)->{1,2}(b) RETURN b")' },
  { group: 'GQL', subgroup: 'Basics', title: 'Paging', script: 'g.gql("MATCH (p) RETURN p OFFSET 1")' },
  { group: 'Errors', title: 'A typo (.order( missing)', script: 'g.v().ordr()' },
];

test('examples.json: unique titles per group, GQL fully split into contiguous subgroups', () => {
  const ids = EXAMPLES.map(exampleId);
  assert.equal(new Set(ids).size, ids.length);
  for (const ex of EXAMPLES) {
    assert.equal(typeof ex.group, 'string');
    assert.ok(!ex.group.includes('::'), ex.group);
    assert.equal(typeof ex.title, 'string');
    assert.equal(typeof ex.script, 'string');
    assert.equal(typeof ex.graph, 'string');
    if (ex.group === 'GQL') assert.equal(typeof ex.subgroup, 'string', ex.title);
    else assert.equal(ex.subgroup, undefined, ex.title);
  }
  // Groups and subgroups are contiguous in the file, so the file order is the menu order.
  const runs = [];
  for (const ex of EXAMPLES) {
    const key = sectionKey(ex.group, ex.subgroup);
    if (runs[runs.length - 1] !== key) runs.push(key);
  }
  assert.equal(new Set(runs).size, runs.length, runs.join(' | '));
  const gql = groupExamples(EXAMPLES).find((g) => g.key === 'GQL');
  assert.ok(gql.subgroups.length >= 6);
  assert.equal(gql.items.length, 0);
});

test('the tree keeps file order and groups subgroups by first appearance', () => {
  const tree = groupExamples(SAMPLE);
  assert.deepEqual(tree.map((g) => g.key), ['Modern graph', 'GQL', 'Errors']);
  const gql = tree[1];
  assert.deepEqual(gql.subgroups.map((s) => s.key), ['GQL / Basics', 'GQL / Paths']);
  assert.deepEqual(gql.subgroups[0].items.map((e) => e.title), ['Who knows', 'Paging']);
  assert.equal(groupSize(gql), 3);
  assert.equal(groupSize(tree[0]), 2);
});

test('findMatch is a case-insensitive plain substring, never a pattern', () => {
  assert.deepEqual(findMatch('g.v().ORDER().by("x")', '.order('), { index: 5, length: 7 });
  assert.equal(findMatch('g.v().count()', '.order('), null);
  assert.equal(findMatch('abc', ''), null);
  // Pattern characters are literal.
  assert.equal(findMatch('g.v().out()', 'v.*out'), null);
  assert.deepEqual(findMatch('a [x*1..2] b', '[x*1..2]'), { index: 2, length: 8 });
  assert.deepEqual(findMatch('->{1,2}', '{1,2}'), { index: 2, length: 5 });
});

test('splitAt and snippet give highlight pieces; the snippet is one line with ellipses', () => {
  const m = findMatch('Who Knows', 'kno');
  assert.deepEqual(splitAt('Who Knows', m), { before: 'Who ', match: 'Kno', after: 'ws' });
  const script = '// a comment\ng.v()\n  .has_label("person")\n  .order().by("age")\n  .values("name")';
  const s = snippet(script, findMatch(script, '.order('), 12);
  assert.equal(s.match, '.order(');
  assert.ok(!/\n/.test(s.before + s.match + s.after));
  assert.ok(s.before.startsWith('…'));
  assert.ok(s.after.endsWith('…'));
  const whole = snippet('g.v().count()', findMatch('g.v().count()', 'count'));
  assert.deepEqual(whole, { before: 'g.v().', match: 'count', after: '()' });
});

test('searchExamples matches titles and scripts and reports what matched', () => {
  assert.equal(searchExamples(SAMPLE, ''), null);
  assert.equal(searchExamples(SAMPLE, '   '), null);
  const hits = searchExamples(SAMPLE, ' .ORDER( ');
  assert.deepEqual([...hits.keys()], [exampleId(SAMPLE[1]), exampleId(SAMPLE[5])]);
  const ordered = hits.get(exampleId(SAMPLE[1]));
  assert.equal(ordered.title, null);
  assert.equal(ordered.snippet.match, '.order(');
  const typo = hits.get(exampleId(SAMPLE[5]));
  assert.deepEqual(typo.title, { before: 'A typo (', match: '.order(', after: ' missing)' });
  assert.equal(typo.snippet, null);
});

test('the real examples: .order( finds every example using it', () => {
  const hits = searchExamples(EXAMPLES, '.order(');
  const expected = EXAMPLES.filter((e) => e.script.toLowerCase().includes('.order(') || e.title.toLowerCase().includes('.order('));
  assert.ok(expected.length > 0);
  assert.equal(hits.size, expected.length);
});

test('visibleTree: collapsed while browsing, only hits and all expanded while searching', () => {
  const tree = groupExamples(SAMPLE);
  const collapsed = new Set(['Modern graph', 'GQL / Paths']);
  const browse = visibleTree(tree, collapsed, null);
  assert.equal(browse.length, 3);
  assert.equal(browse[0].collapsed, true);
  assert.equal(browse[1].collapsed, false);
  assert.equal(browse[1].subgroups[1].collapsed, true);
  assert.equal(browse[1].count, 3);
  assert.deepEqual(flatVisible(browse).map((e) => e.title), ['Who knows', 'Paging', 'A typo (.order( missing)']);

  const hits = searchExamples(SAMPLE, 'MATCH (a)');
  const found = visibleTree(tree, collapsed, hits);
  assert.deepEqual(found.map((g) => g.key), ['GQL']);
  assert.deepEqual(found[0].subgroups.map((s) => s.key), ['GQL / Paths']);
  assert.equal(found[0].subgroups[0].collapsed, false);
  assert.deepEqual(flatVisible(found).map((e) => e.title), ['Hops']);
  assert.deepEqual(visibleTree(tree, collapsed, searchExamples(SAMPLE, 'nothing like this')), []);
});

test('stepSelection walks the list; Down from the search starts after the last opened one', () => {
  const ids = ['a', 'b', 'c'];
  assert.equal(stepSelection(ids, null, 1), 'a');
  assert.equal(stepSelection(ids, null, 1, 'a'), 'b');
  assert.equal(stepSelection(ids, null, 1, 'c'), 'c');
  assert.equal(stepSelection(ids, null, 1, 'gone'), 'a');
  assert.equal(stepSelection(ids, null, -1), null);
  assert.equal(stepSelection(ids, 'a', 1), 'b');
  assert.equal(stepSelection(ids, 'c', 1), 'c');
  assert.equal(stepSelection(ids, 'b', -1), 'a');
  assert.equal(stepSelection(ids, 'a', -1), null);
  assert.equal(stepSelection([], null, 1), null);
});

test('the panel state round-trips and survives broken or unavailable storage', () => {
  const mem = new Map();
  const store = { get: (k, f) => (mem.has(k) ? mem.get(k) : f), set: (k, v) => mem.set(k, JSON.parse(JSON.stringify(v))) };
  assert.deepEqual(loadPanelState(store), { collapsed: new Set(), lastId: null, scrollTop: 0 });
  savePanelState(store, { collapsed: new Set(['GQL / Basics']), lastId: 'GQL::Hops', scrollTop: 123.6 });
  assert.deepEqual(loadPanelState(store), { collapsed: new Set(['GQL / Basics']), lastId: 'GQL::Hops', scrollTop: 124 });

  mem.set('examples.panel', { collapsed: 'x', lastId: 7, scrollTop: -5 });
  assert.deepEqual(loadPanelState(store), { collapsed: new Set(), lastId: null, scrollTop: 0 });
  mem.set('examples.panel', 'garbage');
  assert.deepEqual(loadPanelState(store), { collapsed: new Set(), lastId: null, scrollTop: 0 });

  const broken = { get() { throw new Error('denied'); }, set() { throw new Error('denied'); } };
  assert.deepEqual(loadPanelState(broken), { collapsed: new Set(), lastId: null, scrollTop: 0 });
  assert.doesNotThrow(() => savePanelState(broken, { collapsed: new Set(), lastId: null, scrollTop: 0 }));
});
