// Node tests of a LAYER's memory figures: the process bar's "shared base" track beside the
// layer's own segments (js/memory-bar.js `sharedSegment`, `memoryBarModel().shared`) and the
// Optimize -> Memory "Delta only" toggle (js/optimize.js `memoryReportSpec(delta)`,
// `memoryScopeNote`). A plain graph shows neither.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { memoryBarModel, sharedSegment, SHARED_HINT } from '../js/memory-bar.js';
import { memoryReportSpec, memoryScopeNote, MEMORY_LIST_LIMIT } from '../js/optimize.js';

const MIB = 1024 * 1024;

function stats(shared, extra = {}) {
  return {
    memory: { structure_bytes: 2 * MIB, shared_bytes: shared },
    process: {
      models: [],
      memory: {
        usedBytes: 100 * MIB, usedTracked: true, limitBytes: 400 * MIB, limitSource: 'wasm', usedPercent: 25,
        segments: [{ name: 'structure', bytes: 2 * MIB }, { name: 'properties', bytes: 1 * MIB }, { name: 'other', bytes: 97 * MIB }],
        ...extra,
      },
    },
  };
}

test('a layer: the shared base is its own track, never one of the segments', () => {
  const m = memoryBarModel(stats(40 * MIB));
  assert.deepEqual(m.segments.map((s) => s.name), ['structure', 'properties', 'other'], 'the segments are the engine\'s, unchanged');
  assert.equal(m.shared.bytes, 40 * MIB);
  assert.equal(m.shared.text, '40 MiB');
  assert.equal(m.shared.share, 10, 'a share of the same whole as the segments (the ceiling)');
  assert.equal(m.shared.hint, SHARED_HINT);
  assert.match(m.label, /; shared base 40 MiB, shared with other layers$/);
});

test('a plain graph: no shared track, the label as before', () => {
  for (const shared of [0, undefined]) {
    const m = memoryBarModel(stats(shared));
    assert.equal(m.shared, null);
    assert.ok(!/shared/.test(m.label));
  }
  assert.equal(sharedSegment(null, 1), null);
  assert.equal(sharedSegment({}, 1), null);
});

test('without a ceiling the shared share is capped at the bar\'s width', () => {
  const m = memoryBarModel(stats(500 * MIB, { limitBytes: undefined, usedPercent: undefined, limitSource: 'none' }));
  assert.equal(m.noCeiling, true);
  assert.equal(m.shared.share, 100);
});

test('the memory report: Delta only asks for scope delta; the default spec is unchanged', () => {
  assert.deepEqual(memoryReportSpec(), { limit: MEMORY_LIST_LIMIT });
  assert.deepEqual(memoryReportSpec(false), { limit: MEMORY_LIST_LIMIT });
  assert.deepEqual(memoryReportSpec(true), { limit: MEMORY_LIST_LIMIT, scope: 'delta' });
});

test('the report\'s scope note: the shared base named for a layer, nothing for a plain graph', () => {
  const fmt = (n) => `${n / MIB} MiB`;
  const all = memoryScopeNote({ scope: 'all', shared_bytes: 4 * MIB }, fmt);
  assert.equal(all.scope, 'all');
  assert.match(all.text, /whole visible graph, base included; .*shared base is 4 MiB/);
  const delta = memoryScopeNote({ scope: 'delta', shared_bytes: 4 * MIB }, fmt);
  assert.equal(delta.scope, 'delta');
  assert.match(delta.text, /^Delta only: .*\(4 MiB\) is not walked/);
  assert.equal(memoryScopeNote({ scope: 'all', shared_bytes: 0 }), null);
  assert.equal(memoryScopeNote(null), null);
});
