// Node tests of the Statistics tab's data model (js/statistics.js): the rows built from the
// engine's `statistics()` answer, the number formats, and when a run makes it reload.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { statisticsModel, formatBytes, formatCount, formatLimit, changedData, counted } from '../js/statistics.js';

/** The shape `Session::statistics()` returns for the modern graph in the wasm playground. */
const WASM = {
  statistics: {
    vertex_count: 6,
    edge_count: 6,
    vertex_labels: [{ label: 'person', count: 4, primary: 4 }, { label: 'software', count: 2, primary: 2 }],
    edge_labels: [{ label: 'created', count: 4 }, { label: 'knows', count: 2 }],
    vertex_label_total: 2,
    edge_label_total: 2,
    truncated: false,
  },
  memory: { structure_bytes: 2048, index_bytes: 512, property_bytes: 1536, total_bytes: 4096, total_mb: 0.004 },
  limits: {
    presets: { options: {}, scriptLimits: {}, scriptTimeoutMs: null },
    defaults: {
      'bulk.max_expansion': 10000000,
      'repeat.max_loops': 10000,
      'traversal.max_materialized_bytes': 268435456,
      'traversal.max_string_bytes': 16777216,
      'traversal.max_traversers': 10000000,
      'traversal.max_value_depth': 128,
    },
  },
};

test('counts and labels come from g.statistics()', () => {
  const m = statisticsModel(WASM);
  assert.equal(m.counts.vertices, 6);
  assert.equal(m.counts.edges, 6);
  assert.deepEqual(m.counts.vertexLabels[0], { label: 'person', count: 4, primary: 4 });
  assert.deepEqual(m.counts.edgeLabels[1], { label: 'knows', count: 2 });
  assert.equal(m.counts.vertexLabelTotal, 2);
  assert.equal(m.counts.truncated, false);
});

test('memory is shown in binary units', () => {
  const m = statisticsModel(WASM);
  assert.deepEqual(m.memory.map((r) => r.name), ['Structure', 'Indexes', 'Properties', 'Total']);
  assert.equal(m.memory[0].text, '2 KiB');
  assert.equal(m.memory[2].text, '1.5 KiB');
  assert.equal(m.memory[3].text, '4 KiB');
});

test('compressed strings and dictionaries are shown when there are any', () => {
  const m = statisticsModel({
    ...WASM,
    memory: {
      ...WASM.memory,
      compressed_values: 3,
      compressed_plain_bytes: 30720,
      compressed_stored_bytes: 10240,
      compression_dictionary_bytes: 2048,
    },
  });
  assert.deepEqual(m.memory.map((r) => r.name), [
    'Structure', 'Indexes', 'Properties', 'Compressed strings', 'Compression dictionaries', 'Total',
  ]);
  assert.equal(m.memory[3].text, '3 (30 KiB → 10 KiB)');
  assert.equal(m.memory[4].text, '2 KiB');
});

test('wasm: no presets, every engine default is listed', () => {
  const m = statisticsModel(WASM);
  assert.equal(m.noPresets, true);
  assert.equal(m.limits.length, 6);
  assert.ok(m.limits.every((l) => l.source === 'default'));
  const byKey = Object.fromEntries(m.limits.map((l) => [l.key, l.text]));
  assert.equal(byKey['traversal.max_materialized_bytes'], '256 MiB');
  assert.equal(byKey['traversal.max_string_bytes'], '16 MiB');
  assert.equal(byKey['traversal.max_traversers'], '10,000,000');
  assert.equal(byKey['traversal.max_value_depth'], '128');
});

test('dev server: presets first, then script limits, then the remaining defaults', () => {
  const raw = {
    ...WASM,
    limits: {
      presets: {
        options: { evaluationTimeout: 200, 'memory.limit': 64000000, 'traversal.max_traversers': 3 },
        scriptLimits: { max_operations: 100000000, max_string_size: 1048576 },
        scriptTimeoutMs: 200,
      },
      defaults: { 'repeat.max_loops': 10000 },
    },
  };
  const m = statisticsModel(raw);
  assert.equal(m.noPresets, false);
  assert.deepEqual(m.limits.map((l) => [l.key, l.text, l.source]), [
    ['evaluationTimeout', '200 ms', 'preset'],
    ['memory.limit', '61 MiB', 'preset'],
    ['traversal.max_traversers', '3', 'preset'],
    ['script timeout', '200 ms', 'preset'],
    ['script.max_operations', '100,000,000', 'script'],
    ['script.max_string_size', '1 MiB', 'script'],
    ['repeat.max_loops', '10,000', 'default'],
  ]);
});

test('a storage without statistics or memory, and an empty answer', () => {
  const m = statisticsModel({ statistics: null, memory: null, limits: WASM.limits });
  assert.equal(m.counts, null);
  assert.equal(m.memory, null);
  assert.equal(m.limits.length, 6);
  const empty = statisticsModel(null);
  assert.equal(empty.counts, null);
  assert.deepEqual(empty.limits, []);
  assert.equal(empty.noPresets, true);
});

test('a truncated label list keeps the totals', () => {
  const raw = { ...WASM, statistics: { ...WASM.statistics, vertex_label_total: 111110, truncated: true } };
  const m = statisticsModel(raw);
  assert.equal(m.counts.truncated, true);
  assert.equal(m.counts.vertexLabelTotal, 111110);
});

test('formats', () => {
  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(1023), '1023 B');
  assert.equal(formatBytes(1024 * 1024 * 1024), '1 GiB');
  assert.equal(formatBytes(150 * 1024 + 300), '150 KiB');
  assert.equal(formatCount(1234567), '1,234,567');
  assert.equal(counted(1, 'vertex label', 'vertex labels'), '1 vertex label');
  assert.equal(counted(1200, 'vertex', 'vertices'), '1,200 vertices');
  assert.equal(formatLimit('memory.limit', 'auto'), 'auto');
  assert.equal(formatLimit('x', null), 'none');
});

test('only a run that changed the data makes the block reload', () => {
  assert.equal(changedData({ ok: true, changes: { data: true, schema: false } }), true);
  assert.equal(changedData({ ok: true, changes: { data: false, schema: true } }), false);
  assert.equal(changedData({ ok: false, changes: { data: false, schema: false } }), false);
  assert.equal(changedData(null), false);
});

test('the process memory section lists models and the AI Chat history, and is hidden when empty', () => {
  assert.equal(statisticsModel({ ...WASM, process: { models: [] } }).process, null);
  assert.equal(statisticsModel(WASM).process, null);
  const m = statisticsModel({
    ...WASM,
    process: {
      models: [{ name: 'tiny', model: 'model2vec@abc', dimension: 256, bytes: 2097152, default: true }],
      aiChat: { conversations: 2, historyBytes: 5120 },
    },
  });
  assert.deepEqual(m.process, [
    { name: 'Model tiny', text: `${formatBytes(2097152)} (256 dimensions, default)` },
    { name: 'AI Chat history', text: `${formatBytes(5120)} (2 conversations)` },
  ]);
});
