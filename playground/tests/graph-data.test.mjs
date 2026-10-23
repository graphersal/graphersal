// Node tests of the Graph panel's data model (js/graph-data.js): the compact graph view, its
// conversion to Cytoscape elements, the slice of a large result, and the size-dependent plan.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  packGraphView, sliceGraphView, toCyElements, vertexCaption, edgeCaption, elementCount, idsOf,
  legendLabels, inspectorRows, layoutFor, drawPlan, DRAW_TUNING, INDEX_COLUMNS,
} from '../js/graph-data.js';

/** The modern graph's people: marko(1) knows vadas(2) and josh(4); josh created lop(3). */
function view() {
  return {
    vertexIds: ['1', '2', '4', '3'],
    vertexLabel: [0, 0, 0, 1],
    vertexName: ['marko', 'vadas', null, 'lop'],
    edgeIds: ['7', '8', '11'],
    edgeSource: [0, 0, 2],
    edgeTarget: [1, 2, 3],
    edgeLabel: [2, 2, 3],
    labels: ['person', 'software', 'knows', 'created'],
    truncated: false,
    totalNodes: 4,
    totalEdges: 3,
  };
}

test('packGraphView turns the index columns into transferable Uint32Arrays', () => {
  const v = view();
  const transfer = packGraphView(v);
  assert.equal(transfer.length, INDEX_COLUMNS.length);
  for (const key of INDEX_COLUMNS) assert.ok(v[key] instanceof Uint32Array, key);
  assert.deepEqual(Array.from(v.edgeTarget), [1, 2, 3]);
  assert.deepEqual(v.vertexIds, ['1', '2', '4', '3'], 'ids stay strings');
  assert.deepEqual(packGraphView(v), [], 'packing twice changes nothing');
  assert.deepEqual(packGraphView(null), []);
  // A structured clone with the transfer list moves the buffers (what the worker does).
  const moved = structuredClone(v, { transfer });
  assert.deepEqual(Array.from(moved.edgeSource), [0, 0, 2]);
  assert.equal(v.edgeSource.length, 0, 'the sender\'s buffer is detached');
});

test('toCyElements: one node/edge per row, ids prefixed, captions, palette slot classes, positions', () => {
  const v = view();
  packGraphView(v);
  const looked = [];
  const slotOf = (l) => { looked.push(l); return v.labels.indexOf(l); };
  const els = toCyElements(v, { slotOf });
  assert.equal(els.length, elementCount(v));
  assert.deepEqual(els[0], {
    group: 'nodes',
    classes: 's0',
    data: { id: 'n1', kind: 'vertex', elementId: '1', index: 0, label: 'person', caption: 'marko' },
  });
  assert.equal(els[3].classes, 's1', 'software: its own slot');
  assert.equal(els[2].data.caption, 'person:4', 'no name: label:id');
  const josh = els.find((e) => e.data.id === 'e11');
  assert.deepEqual(
    { source: josh.data.source, target: josh.data.target, label: josh.data.label, caption: josh.data.caption },
    { source: 'n4', target: 'n3', label: 'created', caption: 'created' },
  );
  assert.equal(josh.data.properties, undefined, 'no properties in the drawing');
  assert.equal(looked.length, v.labels.length, 'one slot lookup per label, not per element');
  const withIds = toCyElements(v, { showIds: true, edgeIds: true, slotOf: () => 0 });
  assert.equal(withIds[0].data.caption, '1 · marko');
  assert.equal(withIds[2].data.caption, '4 · person');
  assert.equal(withIds[4].data.caption, '7 · knows');
  // Positions (x, y per vertex) are placed on the nodes; edges have none.
  const placed = toCyElements(v, { slotOf: () => 0, positions: Float32Array.from([1, 2, 3, 4, 5, 6, 7, 8]) });
  assert.deepEqual(placed[2].position, { x: 5, y: 6 });
  assert.equal(placed[4].position, undefined);
});

test('captions of unlabeled and unnamed vertices', () => {
  const v = { ...view(), vertexLabel: [4, 0, 0, 1], labels: [...view().labels, ''], vertexName: [null, 'vadas', null, 'lop'] };
  assert.equal(vertexCaption(v, 0), 'v:1');
  assert.equal(vertexCaption(v, 0, true), '1 · v');
  assert.equal(edgeCaption(v, 2), 'created');
  assert.deepEqual(legendLabels(v), ['', 'person', 'software']);
  assert.deepEqual(legendLabels(v, 1), ['']);
  assert.deepEqual(legendLabels(null), []);
});

test('sliceGraphView keeps the first vertices and the edges between them, re-indexed', () => {
  const v = view();
  packGraphView(v);
  assert.equal(sliceGraphView(v, 4), v, 'fits: unchanged');
  const s = sliceGraphView(v, 2);
  assert.deepEqual(s.vertexIds, ['1', '2']);
  assert.deepEqual(s.edgeIds, ['7']);
  assert.deepEqual(Array.from(s.edgeSource), [0]);
  assert.deepEqual(Array.from(s.edgeTarget), [1]);
  assert.ok(s.edgeLabel instanceof Uint32Array);
  assert.equal(s.cut, 4);
  assert.equal(s.truncated, true);
  assert.equal(v.vertexIds.length, 4, 'the input stays whole');
  assert.equal(sliceGraphView(null, 3), null);
  assert.deepEqual(idsOf(s), { nodes: ['1', '2'], edges: ['7'] });
  assert.deepEqual(idsOf(null), { nodes: [], edges: [] });
});

test('inspectorRows: id, label, labels, endpoints, then the properties in order', () => {
  const fmt = (x) => (typeof x === 'object' ? JSON.stringify(x) : String(x));
  assert.deepEqual(inspectorRows({ kind: 'vertex', id: '1', label: 'person', labels: ['person', 'admin'], properties: { name: 'marko', tags: ['a'] } }, fmt), [
    ['id', '1'], ['label', 'person'], ['labels', 'person, admin'], ['name', 'marko'], ['tags', '["a"]'],
  ]);
  assert.deepEqual(inspectorRows({ kind: 'edge', id: '7', label: 'knows', source: '1', target: '2', properties: { weight: 0.5 } }, fmt), [
    ['id', '7'], ['label', 'knows'], ['out → in', '1 → 2'], ['weight', '0.5'],
  ]);
  assert.deepEqual(inspectorRows({ kind: 'vertex', id: '9', label: '', properties: {} }, fmt), [['id', '9'], ['label', '(none)']]);
  assert.deepEqual(inspectorRows(null, fmt), []);
});

test('layoutFor: cose only up to coseMaxNodes, then the engine layout with a note; other layouts as asked', () => {
  const max = DRAW_TUNING.coseMaxNodes;
  const small = layoutFor('cose', 10);
  assert.deepEqual([small.name, small.note, small.iterations], ['cose', null, 1000]);
  assert.equal(layoutFor('cose', DRAW_TUNING.coseFewerIterationsFrom + 1).iterations, 400);
  assert.equal(layoutFor('cose', max).name, 'cose');
  const big = layoutFor('cose', max + 1);
  assert.equal(big.name, 'engine');
  assert.match(big.note, new RegExp(`at most ${max} vertices`));
  assert.match(big.note, /engine's force layout/);
  assert.deepEqual(layoutFor('engine', 3), { name: 'engine', note: null, iterations: 0 });
  assert.deepEqual(layoutFor('engine', 100000), { name: 'engine', note: null, iterations: 0 });
  for (const name of ['grid', 'circle', 'concentric', 'breadthfirst']) {
    assert.deepEqual(layoutFor(name, 100000), { name, note: null, iterations: 0 });
  }
  assert.equal(layoutFor('unknown', 3).name, 'grid');
});

test('drawPlan: full detail when small, then reduced, then minimal', () => {
  const full = drawPlan(10, DRAW_TUNING.reducedFrom - 11);
  assert.deepEqual(
    [full.tier, full.arrows, full.outline, full.curve, full.hideEdgesOnViewport, full.textureOnViewport, full.minZoomedFontSize],
    ['full', true, true, 'bezier', false, false, 0],
  );
  const reduced = drawPlan(DRAW_TUNING.reducedFrom, 0);
  assert.deepEqual(
    [reduced.tier, reduced.arrows, reduced.outline, reduced.curve, reduced.hideEdgesOnViewport, reduced.textureOnViewport],
    ['reduced', true, false, 'straight', true, true],
  );
  assert.equal(reduced.minZoomedFontSize, DRAW_TUNING.minZoomedFontSize);
  const minimal = drawPlan(DRAW_TUNING.minimalFrom - 1, 1);
  assert.deepEqual([minimal.tier, minimal.arrows, minimal.curve], ['minimal', false, 'haystack']);
  assert.ok(DRAW_TUNING.reducedFrom < DRAW_TUNING.minimalFrom);
  assert.ok(DRAW_TUNING.coseMaxNodes < DRAW_TUNING.reducedFrom);
});

test('inspectorRows: a vertex with degrees shows "in N · out M" right after id and label', () => {
  const rows = inspectorRows({ kind: 'vertex', id: '1', label: 'person', inDegree: 3, outDegree: 5, properties: { name: 'marko' } }, (v) => v);
  assert.deepEqual(rows, [['id', '1'], ['label', 'person'], ['edges', 'in 3 · out 5'], ['name', 'marko']]);
  const edge = inspectorRows({ kind: 'edge', id: '0', label: 'knows', source: '1', target: '2', inDegree: 1, outDegree: 1, properties: {} }, (v) => v);
  assert.equal(edge.some((r) => r[0] === 'edges'), false);
  assert.equal(inspectorRows({ kind: 'vertex', id: '1', label: 'p', properties: {} }, (v) => v).some((r) => r[0] === 'edges'), false);
});
