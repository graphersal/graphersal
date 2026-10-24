// Node tests of vertex size by degree (js/graph/size-by.js): the degrees from the engine's columns
// or the drawing, the log scale, the size range, the choices.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  SIZE_BY_CHOICES, SIZE_RANGE, normalizeSizeBy, degreesOf, degreeScale, sizeOf, nodeScaleFor,
} from '../js/graph/size-by.js';

/** 3 vertices, edges 0->1, 0->2, 1->2 (drawn), with whole-graph degree columns. */
function view(withColumns = true) {
  const v = {
    vertexIds: ['a', 'b', 'c'],
    vertexLabel: Uint32Array.from([0, 0, 0]),
    vertexName: [null, null, null],
    edgeIds: ['e1', 'e2', 'e3'],
    edgeSource: Uint32Array.from([0, 0, 1]),
    edgeTarget: Uint32Array.from([1, 2, 2]),
    edgeLabel: Uint32Array.from([1, 1, 1]),
    labels: ['v', 'e'],
  };
  if (withColumns) {
    v.vertexInDegree = Uint32Array.from([0, 10, 99]);
    v.vertexOutDegree = Uint32Array.from([5, 1, 0]);
  }
  return v;
}

test('the choices and their normalization', () => {
  assert.deepEqual(SIZE_BY_CHOICES.map((c) => c.id), ['off', 'degree', 'in', 'out']);
  assert.equal(normalizeSizeBy('in'), 'in');
  assert.equal(normalizeSizeBy('bogus'), 'off');
  assert.equal(normalizeSizeBy(undefined), 'off');
});

test('degrees come from the whole-graph columns: total = in + out', () => {
  assert.deepEqual([...degreesOf(view(), 'degree')], [5, 11, 99]);
  assert.deepEqual([...degreesOf(view(), 'in')], [0, 10, 99]);
  assert.deepEqual([...degreesOf(view(), 'out')], [5, 1, 0]);
});

test('without the columns (or with columns of another length) the drawn edges are counted', () => {
  assert.deepEqual([...degreesOf(view(false), 'degree')], [2, 2, 2]);
  assert.deepEqual([...degreesOf(view(false), 'in')], [0, 1, 2]);
  assert.deepEqual([...degreesOf(view(false), 'out')], [2, 1, 0]);
  const patched = { ...view(), vertexIds: ['a', 'b', 'c', 'd'], vertexLabel: Uint32Array.from([0, 0, 0, 0]) };
  assert.deepEqual([...degreesOf(patched, 'degree')], [2, 2, 2, 0], 'a vertex added by edit mode');
  assert.deepEqual([...degreesOf(null, 'degree')], []);
});

test('the scale is log1p(d) / log1p(dMax) over the drawn vertices', () => {
  const t = degreeScale([0, 1, 9, 99]);
  assert.equal(t[0], 0);
  assert.ok(Math.abs(t[1] - Math.log(2) / Math.log(100)) < 1e-6);
  assert.ok(Math.abs(t[2] - Math.log(10) / Math.log(100)) < 1e-6);
  assert.equal(t[3], 1);
  assert.deepEqual([...degreeScale([0, 0])], [0, 0], 'all zero: no division by zero');
  assert.deepEqual([...degreeScale([])], []);
  // log scale: a hub of 1000 does not make degree-10 vertices invisible
  const hub = degreeScale([10, 1000]);
  assert.ok(hub[0] > 0.3 && hub[0] < 0.4, String(hub[0]));
});

test('sizes stay within the renderer range', () => {
  const { min, max } = SIZE_RANGE.detailed;
  assert.equal(sizeOf(0, min, max), min);
  assert.equal(sizeOf(1, min, max), max);
  assert.equal(sizeOf(0.5, 10, 20), 15);
  assert.equal(sizeOf(7, 10, 20), 20, 'clamped above');
  assert.equal(sizeOf(-1, 10, 20), 10, 'clamped below');
  assert.equal(sizeOf(NaN, 10, 20), 10);
  assert.ok(min >= 12 && max <= 80 && min < 24 && max > 24, 'around the fixed size 24 of the Detailed tier');
  assert.ok(SIZE_RANGE.fast.minFactor < 1 && SIZE_RANGE.fast.maxFactor > 1);
});

test('the canvas scale: null when Off or not a data drawing', () => {
  assert.equal(nodeScaleFor(view(), 'off'), null);
  assert.equal(nodeScaleFor({ nodes: [], edges: [] }, 'degree'), null);
  assert.equal(nodeScaleFor(null, 'degree'), null);
  const t = nodeScaleFor(view(), 'degree');
  assert.equal(t.length, 3);
  assert.equal(t[2], 1);
  assert.ok(t[0] < t[1] && t[1] < t[2]);
});
