// Node tests of the Graph panel's resize guard (js/graph/resize-guard.js).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { wholeSize, createResizeGuard } from '../js/graph/resize-guard.js';

test('the whole-pixel size floors, so a canvas never gets larger than its container', () => {
  assert.deepEqual(wholeSize({ width: 909.828125, height: 758.5 }), { width: 909, height: 758 });
  assert.deepEqual(wholeSize({ width: 640, height: 480 }), { width: 640, height: 480 });
  assert.deepEqual(wholeSize({ width: -3, height: NaN }), { width: 0, height: 0 });
  assert.deepEqual(wholeSize(null), { width: 0, height: 0 });
});

test('the first measurement counts, sub-pixel jitter does not', () => {
  const g = createResizeGuard();
  assert.equal(g.changed({ width: 909.83, height: 758.5 }), true);
  assert.equal(g.changed({ width: 909.83, height: 758.5 }), false);
  assert.equal(g.changed({ width: 909.2, height: 758.9 }), false);
});

test('a real change counts once, also a change back', () => {
  const g = createResizeGuard();
  g.changed({ width: 910, height: 758.5 });
  assert.equal(g.changed({ width: 893, height: 741.5 }), true); // a 17 px scrollbar on both axes
  assert.equal(g.changed({ width: 893, height: 741.5 }), false);
  assert.equal(g.changed({ width: 910, height: 758.5 }), true);
});

test('reset makes the next measurement count again', () => {
  const g = createResizeGuard();
  g.changed({ width: 500, height: 300 });
  g.reset();
  assert.equal(g.changed({ width: 500, height: 300 }), true);
});
