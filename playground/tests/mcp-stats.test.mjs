// Node tests of the MCP usage panel's model (js/mcp-stats.js): the rows of GET /api/mcp/stats.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mcpStatsModel, msText, uptimeText, histogramText } from '../js/mcp-stats.js';

const BUCKETS = [1, 10, 100, 1000, 10000];

test('times are short texts', () => {
  assert.equal(msText(0.4219), '0.42 ms');
  assert.equal(msText(12.34), '12.3 ms');
  assert.equal(msText(1250), '1.25 s');
  assert.equal(uptimeText(9), '9 s');
  assert.equal(uptimeText(242), '4 min 2 s');
  assert.equal(uptimeText(3 * 3600 + 12 * 60 + 5), '3 h 12 min');
});

test('the histogram leaves empty buckets out', () => {
  assert.equal(histogramText([3, 0, 5, 0, 0, 1], BUCKETS), '<1 ms: 3 · <100 ms: 5 · ≥10 s: 1');
  assert.equal(histogramText([0, 0, 0, 0, 0, 0], BUCKETS), '');
});

test('the model lists tools, saved queries, clients and the refusals that happened', () => {
  const model = mcpStatsModel({
    startedAtMs: 1_700_000_000_000,
    uptimeS: 61,
    requests: { total: 12, withoutSession: 2, busy503: 1 },
    methods: { initialize: 2, 'tools/call': 8, ping: 0 },
    sessions: { initialized: 2, activeLast5Min: 2, activeIsEstimate: true },
    refusals: { queueFull: 1, unknownSession: 0 },
    latencyBucketsMs: BUCKETS,
    tools: [{ name: 'gql', calls: 5, errors: 1, totalMs: 50, meanMs: 10, maxMs: 30.5, histogram: [0, 2, 3, 0, 0, 0], answerChars: 900 }],
    savedQueries: [{ name: 'kb.symbol', calls: 3, errors: 0, totalMs: 3, meanMs: 1, maxMs: 2, histogram: [1, 2, 0, 0, 0, 0], answerChars: 30 }],
    clients: [{ name: 'claude-code', version: '2.1', initialized: 2, requests: 10, toolCalls: 8 }],
    clientsKept: 1,
    clientsMax: 4096,
  });
  assert.equal(model.summary.length, 3);
  assert.match(model.summary[0].text, /\(1 min 1 s\)$/);
  assert.equal(model.summary[1].text, '12 (1 answered 503 busy)');
  assert.equal(model.summary[2].text, '2 initialized, about 2 active in the last 5 min');
  assert.deepEqual(model.refusals, [{ name: 'queueFull', count: 1 }]);
  assert.deepEqual(model.methods, [{ name: 'initialize', count: 2 }, { name: 'tools/call', count: 8 }]);
  assert.deepEqual(model.tools[0], { name: 'gql', calls: 5, errors: 1, mean: '10.0 ms', max: '30.5 ms', chars: 900, histogram: '<10 ms: 2 · <100 ms: 3' });
  assert.equal(model.savedQueries[0].name, 'kb.symbol');
  assert.deepEqual(model.clients[0], { name: 'claude-code 2.1', initialized: 2, requests: 10, toolCalls: 8 });
});

test('no answer, no model; a full client book is said', () => {
  assert.equal(mcpStatsModel(null), null);
  const model = mcpStatsModel({ clientsKept: 4096, clientsMax: 4096 });
  assert.equal(model.summary[3].text, 'the 4096 most recent client names are kept');
  assert.deepEqual(model.tools, []);
});
