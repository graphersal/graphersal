// Node tests of the process memory bar's model (js/memory-bar.js): it renders the segments the
// engine computed (`statistics().process.memory`), never recomputes them.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { memoryBarModel, memoryLevel, percentText, queryPeakModel } from '../js/memory-bar.js';

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;

/** A tracked dev-server answer with every segment and compression. */
function tracked(extra = {}) {
  return {
    process: {
      models: [],
      memory: {
        source: 'ps', liveHeapBytes: 1600 * MIB,
        usedBytes: 1600 * MIB, usedTracked: true, limitBytes: 4 * GIB, limitSource: 'cgroup', usedPercent: 39.1,
        segments: [
          { name: 'structure', bytes: 180 * MIB }, { name: 'properties', bytes: 420 * MIB },
          { name: 'compressed', bytes: 95 * MIB }, { name: 'indexes', bytes: 60 * MIB },
          { name: 'models', bytes: 506 * MIB }, { name: 'other', bytes: 339 * MIB },
        ],
        compression: { plainBytes: 380 * MIB, storedBytes: 95 * MIB },
        ...extra,
      },
    },
  };
}

test('a tracked answer renders every segment against the ceiling', () => {
  const m = memoryBarModel(tracked());
  assert.equal(m.usedText, '1.6 GiB');
  assert.equal(m.limitText, '4 GiB');
  assert.equal(m.sourceText, 'cgroup limit');
  assert.equal(m.percentText, '39 %');
  assert.equal(m.level, 'normal');
  assert.equal(m.notTracked, false);
  assert.equal(m.noCeiling, false);
  assert.deepEqual(m.segments.map((s) => s.name), ['structure', 'properties', 'compressed', 'indexes', 'models', 'other']);
  // Widths are shares of the ceiling: the room left stays empty.
  const total = m.segments.reduce((sum, s) => sum + s.share, 0);
  assert.ok(Math.abs(total - (1600 / 4096) * 100) < 0.01, String(total));
  assert.equal(m.segments[0].colour, 'var(--mem-structure, var(--text-faint))');
  assert.equal(m.segments[1].text, '420 MiB');
  assert.equal(m.savingText, 'compression saves 285 MiB (plain 380 MiB → 95 MiB, −75 %)');
  assert.match(m.label, /^Process memory 1\.6 GiB of 4 GiB, 39 %; structure 180 MiB; /);
});

test('untracked: only the known segments, marked as the known part', () => {
  const m = memoryBarModel(tracked({
    usedTracked: false, usedBytes: 1261 * MIB, usedPercent: 30.8, limitSource: 'physical',
    segments: tracked().process.memory.segments.slice(0, 5),
  }));
  assert.equal(m.notTracked, true);
  assert.equal(m.usedText, '1.2 GiB known');
  assert.equal(m.sourceText, '80 % of RAM');
  assert.ok(!m.segments.some((s) => s.name === 'other'));
});

test('no ceiling: no percentage, no level, the bar is the used part', () => {
  const memory = tracked().process.memory;
  delete memory.limitBytes;
  delete memory.usedPercent;
  memory.limitSource = 'none';
  const m = memoryBarModel({ process: { memory } });
  assert.equal(m.noCeiling, true);
  assert.equal(m.percent, null);
  assert.equal(m.level, null);
  assert.equal(m.percentText, '');
  assert.equal(m.limitText, null);
  assert.equal(m.sourceText, 'memory.limit none');
  const total = m.segments.reduce((sum, s) => sum + s.share, 0);
  assert.ok(Math.abs(total - 100) < 0.01, String(total));
});

test('70 and 90 % are the colour thresholds', () => {
  assert.equal(memoryLevel(69.9), 'normal');
  assert.equal(memoryLevel(70), 'amber');
  assert.equal(memoryLevel(90), 'amber');
  assert.equal(memoryLevel(90.1), 'red');
  assert.equal(memoryLevel(null), null);
  assert.equal(memoryBarModel(tracked({ usedPercent: 75 })).level, 'amber');
  assert.equal(memoryBarModel(tracked({ usedPercent: 95 })).level, 'red');
  assert.equal(percentText(0.42), '0.4 %');
  assert.equal(percentText(47.06), '47 %');
});

test('a tiny segment keeps its legend entry and size', () => {
  const m = memoryBarModel(tracked({
    segments: [{ name: 'structure', bytes: 3 * 1024 }, { name: 'properties', bytes: 1200 }, { name: 'other', bytes: 2 * GIB }],
  }));
  const tiny = m.segments.find((s) => s.name === 'properties');
  assert.ok(tiny.share < 0.001);
  assert.equal(tiny.text, '1.2 KiB');
  assert.match(m.label, /properties 1\.2 KiB/);
});

test('no bar without segments; nothing saved is said so', () => {
  assert.equal(memoryBarModel(null), null);
  assert.equal(memoryBarModel({ process: { memory: { source: 'wasm' } } }), null);
  const m = memoryBarModel(tracked({ compression: { plainBytes: 10, storedBytes: 40 } }));
  assert.equal(m.savingText, 'compression saves nothing (plain 10 B → 40 B)');
  assert.equal(memoryBarModel(tracked({ compression: undefined })).savingText, null);
  assert.equal(memoryBarModel(tracked({ estimateAboveHeap: true })).estimateAboveHeap, true);
});

test('the query peak is against the per-query limit', () => {
  const p = { query_memory: { peak_bytes: 1.2 * MIB, limit_bytes: GIB, limit: 'traversal.max_materialized_bytes', source: 'tracked' } };
  const peak = queryPeakModel(p);
  assert.equal(peak.text, 'Peak memory 1.2 MiB of 1 GiB per query (traversal.max_materialized_bytes, tracked)');
  assert.equal(peak.level, 'normal');
  assert.equal(queryPeakModel({ query_memory: { ...p.query_memory, peak_bytes: 0.8 * GIB } }).level, 'amber');
  assert.equal(queryPeakModel({ duration_ns: 1 }), null);
  assert.equal(queryPeakModel(null), null);
});
