// Node tests of a LAYER's views (js/layer-diff.js): the Layer changes dialog's model (spec,
// groups, base → layer rows, truncation, refusals, focus targets), the top-bar badge, and when
// the WebAssembly playground offers Start / Drop layer; plus the UI profile key `layerDiff`.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DIFF_LIMITS, initialDiffState, parseLabels, diffSpec, diffModel, changeRows, entryRows, focusTarget, changeCounts, layerBadge,
  layerActions, dropConfirmText, resetAction, resetConfirmText, resetDoneText,
} from '../js/layer-diff.js';
import { BUILTIN_PROFILES, CAN_KEYS, CAN_GROUPS, createUi } from '../js/ui-profiles.js';

/** A diff as the session answers it (S35.1): lists vertices before edges, keys unordered. */
function answer(extra = {}) {
  return {
    ok: true,
    diff: {
      added: [
        { kind: 'edge', id: 13, labels: ['knows'], properties: { weight: 0.5 }, out: 1, in: 100 },
        { kind: 'vertex', id: 100, labels: ['person'], properties: { name: 'zoe', age: 31 } },
      ],
      removed: [{ kind: 'vertex', id: 2, labels: ['person'], properties: { name: 'vadas' } }],
      modified: [
        {
          kind: 'vertex', id: 1, base_labels: ['person'], labels: ['person', 'admin'],
          properties: { age: { base: 29, layer: 30 }, city: { layer: 'Berlin' }, nick: { base: 'mk' } },
        },
      ],
      truncated: false,
      ...extra,
    },
  };
}

test('the spec: kind, labels and limit as the engine takes them', () => {
  assert.deepEqual(diffSpec(initialDiffState()), { limit: DIFF_LIMITS[0] });
  assert.deepEqual(diffSpec({ kind: 'edge', labels: ' knows, created knows', limit: 500 }), { limit: 500, kinds: 'edge', labels: ['knows', 'created'] });
  assert.deepEqual(diffSpec({ kind: 'vertex', limit: 0 }), { limit: DIFF_LIMITS[0], kinds: 'vertex' }, 'a bad limit is the default');
  assert.deepEqual(diffSpec({ kind: 'nonsense' }), { limit: DIFF_LIMITS[0] });
  assert.deepEqual(parseLabels(''), []);
});

test('the model: added, modified, removed in that order, vertices before edges in each', () => {
  const m = diffModel(answer());
  assert.deepEqual(m.groups.map((g) => g.id), ['added', 'modified', 'removed']);
  assert.deepEqual(m.groups.map((g) => g.count), [2, 1, 1]);
  assert.equal(m.total, 4);
  assert.equal(m.empty, false);
  assert.equal(m.truncated, false);
  assert.equal(m.truncatedText, null);
  const added = m.groups[0].entries;
  assert.deepEqual(added.map((e) => e.kind), ['vertex', 'edge'], 'vertices first');
  assert.equal(added[0].title, 'vertex 100 · person');
  assert.equal(added[1].title, 'edge 13 · knows (1 → 100)');
  assert.deepEqual(added[0].rows, [{ name: 'name', value: 'zoe' }, { name: 'age', value: '31' }]);
});

test('modified: each change as base → layer, an absent side named, a label change first', () => {
  const rows = diffModel(answer()).groups[1].entries[0].rows;
  assert.deepEqual(rows, [
    { name: 'labels', base: 'person', layer: 'person, admin', change: 'changed' },
    { name: 'age', base: '29', layer: '30', change: 'changed' },
    { name: 'city', base: '(absent)', layer: 'Berlin', change: 'set' },
    { name: 'nick', base: 'mk', layer: '(absent)', change: 'unset' },
  ]);
  // Only properties changed: no labels row; null is a value, not an absent side.
  assert.deepEqual(changeRows({ base_labels: ['a'], labels: ['a'], properties: { x: { base: null, layer: 1 } } }), [
    { name: 'x', base: 'null', layer: '1', change: 'changed' },
  ]);
  assert.deepEqual(entryRows({ properties: { tags: ['a', 'b'] } }, (v) => `<${v}>`), [{ name: 'tags', value: '<a,b>' }], 'the formatter is the caller\'s');
});

test('removed elements are listed but cannot be focused; added and modified ones focus their vertex', () => {
  const m = diffModel(answer());
  assert.equal(m.groups[2].focusable, false);
  assert.equal(m.groups[2].entries[0].focus, null);
  assert.deepEqual(m.groups[0].entries[0].focus, { vertex: '100', element: { kind: 'vertex', id: '100' } });
  assert.deepEqual(m.groups[0].entries[1].focus, { vertex: '1', element: { kind: 'edge', id: '13' } }, 'an edge: its out vertex');
  assert.deepEqual(m.groups[1].entries[0].focus, { vertex: '1', element: { kind: 'vertex', id: '1' } });
  assert.equal(focusTarget({ kind: 'edge', id: 5 }, 'added'), null, 'an edge without its ends');
  assert.equal(focusTarget({ kind: 'vertex', id: 5 }, 'removed'), null);
});

test('truncated, empty, refused', () => {
  const t = diffModel(answer({ truncated: true }), { limit: 100 });
  assert.equal(t.truncated, true);
  assert.match(t.truncatedText, /limit of 100/);
  const empty = diffModel({ ok: true, diff: { added: [], removed: [], modified: [], truncated: false } });
  assert.equal(empty.empty, true);
  assert.deepEqual(empty.groups.map((g) => g.count), [0, 0, 0]);
  assert.deepEqual(diffModel({ ok: false, error: 'limit must be a positive integer, not 0' }), { error: 'limit must be a positive integer, not 0' });
  assert.equal(diffModel(null), null);
});

const layer = {
  base: 'modern',
  baseRef: { graphId: 'x', commitSeq: 4, committedAt: 0 },
  added: { vertices: 1, edges: 1 },
  modified: { vertices: 1, edges: 0 },
  removed: { vertices: 1, edges: 1 },
  ownBytes: 2048,
  sharedBytes: 4096,
};

test('the badge: the base, the counts and the memory split; none on a plain graph', () => {
  assert.equal(layerBadge(null), null);
  assert.equal(layerBadge(undefined), null);
  const b = layerBadge(layer, (n) => `${n / 1024} KiB`);
  assert.equal(b.text, 'Layer over modern');
  assert.deepEqual([b.counts.added, b.counts.modified, b.counts.removed], [2, 1, 2]);
  assert.equal(b.counts.text, '+2 ~1 −2');
  assert.match(b.title, /at commit 4/);
  assert.match(b.title, /own 2 KiB, the shared base 4 KiB/);
  assert.match(b.label, /2 added, 1 modified, 2 removed/);
  const noRef = layerBadge({ ...layer, baseRef: null });
  assert.ok(!/at commit/.test(noRef.title));
  assert.equal(changeCounts({ base: 'b' }).total, 0, 'missing counts are 0');
});

test('Start / Drop layer: WebAssembly only, start without a layer, drop with one', () => {
  const graph = { name: 'Modern' };
  assert.deepEqual(layerActions({ engineLayers: true, graph }), { start: true, drop: false, reason: null });
  assert.deepEqual(layerActions({ engineLayers: true, graph: { ...graph, layer } }), { start: false, drop: true, reason: null });
  const server = layerActions({ server: true, engineLayers: true, graph: { ...graph, layer } });
  assert.equal(server.start || server.drop, false, 'the dev server: the layer comes from its flags');
  assert.match(server.reason, /--layer/);
  assert.equal(layerActions({ engineLayers: false, graph }).start, false, 'an older worker without the methods');
  assert.equal(layerActions({ engineLayers: true, graph: null }).start, false);
  assert.equal(layerActions({ engineLayers: true, graph, storeView: { commitSeq: 2 } }).start, false, 'not on a read-only view');
  assert.equal(layerActions({ engineLayers: true, graph, running: true }).start, false);
  assert.equal(layerActions({ engineLayers: true, graph, allowed: false }).start, false, 'the profile hides it');
  assert.match(dropConfirmText(layer), /discard its 5 changes \(\+2 ~1 −2\)/);
  assert.match(dropConfirmText({ base: 'b' }), /no changes/);
});

test('Discard all changes: on every writable layer of an engine that has it, with its reason when not', () => {
  const graph = { name: 'Modern', layer };
  assert.deepEqual(resetAction({ engineReset: true, graph }), { reset: true, reason: null });
  const cases = [
    [{ engineReset: true, graph: { name: 'Modern' } }, /not a layer/],
    [{ engineReset: true, graph: null }, /not a layer/],
    [{ engineReset: false, graph }, /rebuild/],
    [{ engineReset: true, graph, storeView: { commitSeq: 2 } }, /past state/],
    [{ engineReset: true, graph, readOnly: 'the server serves this graph read-only' }, /read-only/],
    [{ engineReset: true, graph, running: true }, /running/],
    [{ engineReset: true, graph, allowed: false }, /profile/],
  ];
  for (const [s, reason] of cases) {
    const a = resetAction(s);
    assert.equal(a.reset, false, JSON.stringify(s));
    assert.match(a.reason, reason);
  }
  assert.match(resetConfirmText(layer), /Discard all 5 changes of this layer \(\+2 ~1 −2\)/);
  assert.match(resetConfirmText(layer), /schema and catalog/);
  assert.match(resetConfirmText(layer), /The layer stays and shows modern again; modern itself is not changed/);
  assert.match(resetConfirmText({ base: 'b' }), /every change of this layer/);
  const reset = { added: { vertices: 2, edges: 0 }, modified: { vertices: 1, edges: 0 }, removed: { vertices: 0, edges: 2 }, schema: true, definitions: 1 };
  assert.equal(resetDoneText(reset, 'modern'),
    'Layer changes discarded (5 element changes, the schema, 1 catalog definition): the layer equals modern again.');
  const none = { added: { vertices: 0, edges: 0 }, modified: { vertices: 0, edges: 0 }, removed: { vertices: 0, edges: 0 }, schema: false, definitions: 0 };
  assert.match(resetDoneText(none, 'modern'), /had no changes/);
  assert.match(resetDoneText(null), /equals its base again/);
});

test('UI profile key layerDiff: a top-bar key, on in `full` with both engines, off in `demo`', () => {
  assert.ok(CAN_KEYS.includes('layerDiff'));
  assert.ok(CAN_GROUPS.topbar.includes('layerDiff'));
  assert.equal(createUi(BUILTIN_PROFILES.full, { server: false }).can('layerDiff'), true);
  assert.equal(createUi(BUILTIN_PROFILES.full, { server: true }).can('layerDiff'), true);
  assert.equal(createUi(BUILTIN_PROFILES.demo, { server: false }).can('layerDiff'), false);
});
