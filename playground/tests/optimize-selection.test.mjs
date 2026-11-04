// Node tests of the Optimize panel's "several at once" model (js/optimize.js): the groups of the
// advice, the selection (nothing selected by default, toggles per recommendation and per group,
// pruned when the advice changes) and the summary with the confirmation text.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  recommendationGroups, pruneSelection, groupSelected, toggleGroup, toggleRecommendation, selectionSummary,
} from '../js/optimize.js';

const MIB = 1024 * 1024;

const dedup = (key, mib) => ({
  id: `add_compression:vertex:file:${key}`,
  kind: 'add_compression',
  title: `Store each distinct file.${key} once (dedup)`,
  memory_bytes: mib * MIB,
  measured: true,
});

const ADVICE = {
  recommendations: [
    dedup('path', 40),
    { id: 'create_index:vertex:person:age', kind: 'create_index', title: 'Create an index on person.age', memory_bytes: -2048, measured: false },
    dedup('kind', 10),
    dedup('owner', 2),
  ],
  groups: [
    { key: 'add_compression:dedup', kind: 'add_compression', codec: 'dedup', title: 'dedup: 3 keys, total -52.0 MiB', ids: ['add_compression:vertex:file:path', 'add_compression:vertex:file:kind', 'add_compression:vertex:file:owner'], memory_bytes: 52 * MIB },
    { key: 'create_index', kind: 'create_index', codec: null, title: 'create index: 1 key, total +0.0 MiB', ids: ['create_index:vertex:person:age'], memory_bytes: -2048 },
  ],
};

test('the groups come from the engine, with their ids and counts', () => {
  const groups = recommendationGroups(ADVICE);
  assert.equal(groups.length, 2);
  assert.equal(groups[0].title, 'dedup: 3 keys, total -52.0 MiB');
  assert.equal(groups[0].count, 3);
  assert.deepEqual(recommendationGroups(null), []);
});

test('an older engine without groups gets one group per kind', () => {
  const groups = recommendationGroups({ recommendations: ADVICE.recommendations });
  assert.deepEqual(groups.map((g) => g.key), ['add_compression', 'create_index']);
  assert.equal(groups[0].count, 3);
  assert.match(groups[0].title, /^add compression: 3 keys, total -52/);
  assert.match(groups[1].title, /^create index: 1 key, total \+2/);
});

test('nothing is selected by default; a group is selected and cleared in one click', () => {
  const none = pruneSelection(ADVICE, undefined);
  assert.deepEqual(none, []);
  const [dedups] = recommendationGroups(ADVICE);
  assert.equal(groupSelected(dedups, none), false);
  const all = toggleGroup(ADVICE, none, dedups);
  assert.deepEqual(all, dedups.ids);
  assert.equal(groupSelected(dedups, all), true);
  // A partly selected group is completed first, then cleared.
  const partly = toggleRecommendation(ADVICE, all, 'add_compression:vertex:file:kind');
  assert.equal(groupSelected(dedups, partly), false);
  assert.deepEqual(toggleGroup(ADVICE, partly, dedups), dedups.ids);
  assert.deepEqual(toggleGroup(ADVICE, all, dedups), []);
});

test('the selection keeps the advice order and drops ids no longer advised', () => {
  const picked = toggleRecommendation(ADVICE, ['add_compression:vertex:file:owner'], 'add_compression:vertex:file:path');
  assert.deepEqual(picked, ['add_compression:vertex:file:path', 'add_compression:vertex:file:owner']);
  const later = { recommendations: ADVICE.recommendations.filter((r) => !r.id.endsWith(':path')) };
  assert.deepEqual(pruneSelection(later, picked), ['add_compression:vertex:file:owner']);
});

test('the summary adds up the selection and asks before applying it in one commit', () => {
  assert.deepEqual(selectionSummary(ADVICE, []), { count: 0, bytes: 0, text: 'nothing selected', confirm: '' });
  const [dedups] = recommendationGroups(ADVICE);
  const summary = selectionSummary(ADVICE, dedups.ids);
  assert.equal(summary.count, 3);
  assert.equal(summary.bytes, 52 * MIB);
  assert.equal(summary.text, '3 selected · frees 52 MiB');
  assert.match(summary.confirm, /^Apply these 3 recommendations in one commit \(frees 52 MiB\)\?/);
  assert.match(summary.confirm, /All or nothing/);
  assert.match(summary.confirm, /Store each distinct file\.path once/);
  // An estimate in the selection: "about".
  const mixed = selectionSummary(ADVICE, ['create_index:vertex:person:age']);
  assert.equal(mixed.text, '1 selected · takes about 2 KiB');
  assert.match(mixed.confirm, /^Apply this recommendation in one commit/);
  // Long selections list five titles and count the rest.
  const many = { recommendations: Array.from({ length: 8 }, (_, i) => dedup(`k${i}`, 1)) };
  const all = many.recommendations.map((r) => r.id);
  assert.match(selectionSummary(many, all).confirm, /… and 3 more/);
});
