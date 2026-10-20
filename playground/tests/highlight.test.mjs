// Node tests of the Graph panel's result highlight (js/graph/highlight.js): when it is shown,
// dismissing it ("Clear highlight" / Esc) for one result, a later result highlighting again, tab
// switches, and the clear key.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resultHighlight, createDismissals, isClearKey } from '../js/graph/highlight.js';

const idsOf = (s) => ({ nodes: s.ids, edges: [] });
const input = (over = {}) => {
  const dismissals = over.dismissals || createDismissals();
  return {
    source: 'graph', overview: false, drill: false, enabled: true, subgraph: { ids: ['1', '2'] },
    dismissed: (s) => dismissals.isDismissed(s), idsOf, ...over,
  };
};

test('the result is highlighted in the whole-graph drawing only', () => {
  assert.deepEqual(resultHighlight(input()), { nodes: ['1', '2'], edges: [] });
  assert.equal(resultHighlight(input({ source: 'result' })), null);
  assert.equal(resultHighlight(input({ source: 'neighbourhood' })), null);
  assert.equal(resultHighlight(input({ overview: true })), null);
  assert.equal(resultHighlight(input({ drill: true })), null);
  assert.equal(resultHighlight(input({ enabled: false })), null);
  assert.equal(resultHighlight(input({ subgraph: null })), null);
});

test('a dismissal clears the highlight of that result only; a later result highlights again', () => {
  const dismissals = createDismissals();
  const first = { ids: ['1'] };
  assert.ok(resultHighlight(input({ dismissals, subgraph: first })));
  assert.equal(dismissals.dismiss(first), true);
  assert.equal(dismissals.version, 1);
  assert.equal(dismissals.dismiss(first), false, 'twice is one dismissal');
  assert.equal(dismissals.version, 1);
  assert.equal(resultHighlight(input({ dismissals, subgraph: first })), null);
  // The next query result (a new subgraph object) highlights again.
  const second = { ids: ['3'] };
  assert.deepEqual(resultHighlight(input({ dismissals, subgraph: second })), { nodes: ['3'], edges: [] });
});

test('tab switches: another tab highlights its result, the cleared tab stays cleared', () => {
  const dismissals = createDismissals();
  const tabA = { ids: ['1'] };
  const tabB = { ids: ['2'] };
  dismissals.dismiss(tabA);
  assert.ok(resultHighlight(input({ dismissals, subgraph: tabB })));
  assert.equal(resultHighlight(input({ dismissals, subgraph: tabA })), null);
});

test('nothing to dismiss without a result', () => {
  const dismissals = createDismissals();
  assert.equal(dismissals.dismiss(null), false);
  assert.equal(dismissals.isDismissed(null), false);
  assert.equal(dismissals.version, 0);
});

test('Esc without modifiers clears, not in a field', () => {
  assert.ok(isClearKey({ key: 'Escape' }));
  assert.ok(!isClearKey({ key: 'Escape', inField: true }));
  assert.ok(!isClearKey({ key: 'Escape', shiftKey: true }));
  assert.ok(!isClearKey({ key: 'Escape', ctrlKey: true }));
  assert.ok(!isClearKey({ key: 'Enter' }));
  assert.ok(!isClearKey(null));
});
