// Node tests of the Optimize panel's model (js/optimize.js): recommendation rows, the workload
// tables with their sparkline buckets, the chart geometry and the memory tables.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  recommendationRows, adviceSummary, effectText, channels, workloadRows, bucketSeries, sparkPath,
  chartGeometry, formatMicros, memoryRows, labelRows, memoryTotals, TIERS,
} from '../js/optimize.js';

const MIN = 60e6;
const NOW = 1_800_000_000_000_000;

const ADVICE = {
  workload_level: 'persisted',
  observed_since: NOW - 3 * TIERS.days.width,
  recommendations: [
    { id: 'create_index:vertex:person:age', kind: 'create_index', title: 'Create an index on person.age', evidence: 'filtered 12 times', cost: 'about 1 KiB', memory_bytes: -2048, measured: false, confidence: 'medium', reversible: true, command: 'g.create_index(#{label: "person", key: "age"})' },
    { id: 'add_compression:vertex:customer:cv', kind: 'add_compression', title: 'Compress customer.cv (lz4)', evidence: '1 MiB', cost: 'reads decompress', memory_bytes: 786432, measured: true, confidence: 'high', reversible: true, command: 'g.define_compression(#{..})' },
  ],
};

test('recommendations become rows with their memory effect', () => {
  const rows = recommendationRows(ADVICE);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].badge, 'Index');
  assert.equal(rows[0].effect, 'takes about 2 KiB');
  assert.equal(rows[0].frees, false);
  assert.equal(rows[1].effect, 'frees 768 KiB');
  assert.equal(rows[1].frees, true);
  assert.deepEqual(recommendationRows(null), []);
  assert.equal(effectText(0, true), 'no memory change');
});

test('the summary counts, adds up what can be freed and says what it rests on', () => {
  assert.equal(adviceSummary(ADVICE, NOW), '2 recommendations · up to 768 KiB of memory to free · workload observed for 3 days');
  assert.match(adviceSummary({ workload_level: 'off', recommendations: [] }, NOW), /^0 recommendations · no workload statistics/);
});

const REPORT = {
  level: 'persisted',
  queries: [
    { query: 'v().count()', channel: 'mcp', last_hour: { count: 2, failed: 0, items: 2, total_micros: 300, max_micros: 200 }, last_two_days: { count: 2, total_micros: 300 }, last_ninety_days: { count: 2, total_micros: 300 }, history: [{ start: NOW - MIN, count: 2, failed: 0, items: 2, total_micros: 300, max_micros: 200 }] },
    { query: 'v(labels: [?]).values(?)', channel: 'playground', last_hour: { count: 5, failed: 1, items: 20, total_micros: 5000, max_micros: 2000 }, last_two_days: { count: 5, total_micros: 5000 }, last_ninety_days: { count: 5, total_micros: 5000 }, history: [] },
    { query: 'old()', channel: 'cli', last_hour: { count: 0 }, last_two_days: { count: 1, total_micros: 10 }, last_ninety_days: { count: 1, total_micros: 10 } },
  ],
  properties: [
    { element: 'vertex', label: 'person', key: 'age', access: 'filter_scan', channel: 'mcp', last_hour: { count: 1, items: 4, total_micros: 0 }, last_two_days: { count: 1 }, last_ninety_days: { count: 1 } },
  ],
  locks: [],
  jobs: [],
};

test('workload rows: the window, the channel, the most total time first', () => {
  const rows = workloadRows(REPORT, 'queries', 'hour', 'all', NOW);
  assert.deepEqual(rows.map((r) => r.name), ['v(labels: [?]).values(?)', 'v().count()']);
  assert.equal(rows[0].mean, 1000);
  assert.equal(rows[0].failed, 1);
  assert.deepEqual(workloadRows(REPORT, 'queries', 'hour', 'mcp', NOW).map((r) => r.name), ['v().count()']);
  assert.equal(workloadRows(REPORT, 'queries', 'two_days', 'all', NOW).length, 3);
  assert.equal(workloadRows(REPORT, 'properties', 'hour', 'all', NOW)[0].name, 'person.age · filter_scan');
  assert.deepEqual(channels(REPORT), ['cli', 'mcp', 'playground']);
});

test('history buckets are laid on the tier slots, ending at now', () => {
  const series = bucketSeries(REPORT.queries[0].history, 'minutes', NOW);
  assert.equal(series.length, 60);
  assert.equal(series[58].count, 2);
  assert.equal(series[58].mean, 150);
  assert.equal(series[59].count, 0);
  // A bucket older than the tier keeps is left out.
  assert.equal(bucketSeries([{ start: NOW - 90 * MIN, count: 1 }], 'minutes', NOW).reduce((s, b) => s + b.count, 0), 0);
  assert.equal(workloadRows(REPORT, 'queries', 'hour', 'mcp', NOW)[0].series.length, 60);
});

test('a sparkline path and the chart geometry scale to their boxes', () => {
  assert.equal(sparkPath([0, 2, 1], 100, 10), 'M0.0 10.0 L50.0 0.0 L100.0 5.0');
  assert.equal(sparkPath([], 100, 10), '');
  const series = [
    { start: 0, count: 4, failed: 1, mean: 10, max: 20 },
    { start: MIN, count: 2, failed: 0, mean: 5, max: 40 },
  ];
  const g = chartGeometry(series, 200, 100, 'minutes');
  assert.equal(g.countMax, 4);
  assert.equal(g.timeMax, 40);
  assert.equal(g.bars[0].h, 100);
  assert.equal(g.bars[0].failedH, 25);
  assert.equal(g.bars[1].x, 100);
  assert.equal(g.max, 'M50.0 50.0 L150.0 0.0');
  assert.match(g.bars[0].title, /4 runs, 1 failed, mean 10 µs, max 20 µs/);
  assert.equal(formatMicros(1500), '1.5 ms');
  assert.equal(formatMicros(2_500_000), '2.50 s');
});

test('memory rows: the most bytes first, with their badges', () => {
  const report = {
    property_bytes: 300,
    properties: [
      { element: 'vertex', label: 'person', path: '$.name', elements: 4, bytes: 100, value_types: [{ type: 'string', count: 4 }], strings: { distinct_estimate: 4 }, rules: [], indexes: ['person_name'], vectors: [] },
      { element: 'vertex', label: 'customer', path: '$.cv', elements: 9, bytes: 200, value_types: [{ type: 'string', count: 9 }], strings: null, rules: ['cv'], indexes: [], vectors: [], max_value_bytes: 50 },
    ],
    labels: [
      { element: 'vertex', label: 'person', elements: 4, slot_bytes: 384, property_bytes: 100, values: 8, degree: { max: 3 } },
      { element: 'edge', label: 'knows', elements: 2, slot_bytes: 224, property_bytes: 64, values: 2, degree: null },
    ],
  };
  const rows = memoryRows(report);
  assert.deepEqual(rows.map((r) => r.target), ['customer.cv', 'person.name']);
  assert.deepEqual(rows[0].badges, ['compressed (cv)']);
  assert.deepEqual(rows[1].badges, ['indexed (person_name)']);
  assert.equal(rows[1].distinct, 4);
  assert.deepEqual(labelRows(report).map((r) => [r.label, r.maxDegree]), [['person', 3], ['knows', null]]);
  assert.deepEqual(memoryTotals(report), [{ label: 'Properties', bytes: 300 }]);
});

test('workload rows: only the slow ones, sorted by a column', () => {
  const slow = workloadRows(REPORT, 'queries', 'hour', 'all', NOW, { minMicros: 1000 });
  assert.deepEqual(slow.map((r) => r.name), ['v(labels: [?]).values(?)']);
  const byRuns = workloadRows(REPORT, 'queries', 'hour', 'all', NOW, { sort: 'count' });
  assert.deepEqual(byRuns.map((r) => r.count), [5, 2]);
  const byMax = workloadRows(REPORT, 'queries', 'hour', 'all', NOW, { sort: 'max' });
  assert.equal(byMax[0].max, 2000);
});
