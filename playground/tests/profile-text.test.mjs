// Node tests of the Profile panel's Text tab (js/profile-text.js): the run's peak memory line
// appears only when the metrics carry `query_memory`.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { profileText } from '../js/profile-text.js';

const STEP = {
  id: '0', name: 'v().count()', duration_ns: 1000, percent_duration: 100,
  counts: { traverser_count: 1, element_count: 1 }, calls: 1, count_in: 0, nested: [],
};

test('the peak line follows TOTAL only with query_memory', () => {
  const plain = { duration_ns: 1000, metrics: [STEP], optimizer_rules_applied: [] };
  assert.ok(!profileText(plain).includes('Peak memory'));
  const withPeak = {
    ...plain,
    query_memory: { peak_bytes: 2048, limit_bytes: 1024 * 1024 * 1024, limit: 'traversal.max_materialized_bytes', source: 'estimated' },
  };
  const lines = profileText(withPeak).split('\n');
  const total = lines.findIndex((l) => l.includes('TOTAL: duration:'));
  assert.equal(lines[total + 1], 'Peak memory 2 KiB of 1 GiB per query (traversal.max_materialized_bytes, estimated)');
  assert.match(lines[total + 2], /^=+$/);
});
