// Node tests of the engine filter's "others visible" form as the Graph panel sends it
// (js/graph/filter-model.js `buildFilter(.., {othersVisible: true})`, `labelVisible`,
// `filteredText` with a neighbourhood's `unfiltered` counts) and of the neighbourhood redraw
// requests that keep every expanded vertex (js/graph/selection.js).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  emptyFilter, newCondition, buildFilter, setConditions, setLabel, setAllLabels, labelVisible, labelFilterView, filteredText,
} from '../js/graph/filter-model.js';
import { neighbourhoodIds, expandRequest, redrawRequest } from '../js/graph/selection.js';

const known = {
  vertexLabels: ['person', 'software'],
  edgeLabels: ['knows', 'created'],
  types: {
    vertex: { person: { name: 'string', age: 'number' }, software: { lang: 'string' } },
    edge: { knows: { weight: 'number' }, created: { weight: 'number' } },
  },
  othersVisible: true,
};

test('others visible: hidden labels by name, conditions only where there are any', () => {
  let m = emptyFilter();
  assert.equal(buildFilter(m, known).spec, null, 'nothing restricted: no spec');
  m = setConditions(m, 'vertices', 'person', [{ ...newCondition('age'), op: 'gt', value: '30' }]);
  m = setLabel(m, 'edges', 'created', false);
  const b = buildFilter(m, known);
  assert.deepEqual(b.spec, {
    otherVertices: 'visible',
    vertices: { person: [{ key: 'age', op: 'gt', value: 30 }] },
    otherEdges: 'visible',
    hiddenEdges: ['created'],
  });
  assert.equal(b.count, 2);
  assert.deepEqual(b.sent, { 'vertices.person': [0] });
});

test('others visible does not depend on the label list', () => {
  let m = setLabel(emptyFilter(), 'vertices', 'city', false); // a label beyond the listed ones
  m = setConditions(m, 'vertices', 'tag', [{ ...newCondition('name'), op: 'contains', value: 'x', type: 'string' }]);
  m = { ...m, hideIsolated: true };
  const a = buildFilter(m, known).spec;
  const b = buildFilter(m, { othersVisible: true }).spec;
  assert.deepEqual(a, b, 'the same spec before and after the labels are read');
  assert.deepEqual(a, {
    otherVertices: 'visible',
    hiddenVertices: ['city'],
    vertices: { tag: [{ key: 'name', op: 'contains', value: 'x' }] },
    hideIsolated: true,
  });
  // The conditions of a hidden label are kept in the state but not sent.
  m = setLabel(m, 'vertices', 'tag', false);
  assert.deepEqual(buildFilter(m, known).spec.hiddenVertices.sort(), ['city', 'tag']);
  assert.equal(buildFilter(m, known).spec.vertices, undefined);
  // "none" hides the listed labels by name.
  const none = setAllLabels(emptyFilter(), 'vertices', known.vertexLabels, false);
  assert.deepEqual(buildFilter(none, known).spec, { otherVertices: 'visible', hiddenVertices: ['person', 'software'] });
});

test('labelVisible and the client label filter read the others-visible form', () => {
  const spec = { otherVertices: 'visible', hiddenVertices: ['software'], vertices: { person: [{ key: 'age', op: 'gt', value: 1 }] } };
  assert.equal(labelVisible(spec, 'vertices', 'software'), false);
  assert.equal(labelVisible(spec, 'vertices', 'person'), true, 'conditions are not checked on the client');
  assert.equal(labelVisible(spec, 'vertices', 'city'), true, 'an unlisted label is visible');
  assert.equal(labelVisible(spec, 'edges', 'knows'), true);
  assert.equal(labelVisible({ hiddenEdges: ['knows'] }, 'edges', 'knows'), false);
  assert.equal(labelVisible({ hiddenEdges: ['knows'] }, 'edges', 'created'), true);
  assert.equal(labelVisible({ otherEdges: 'hidden' }, 'edges', 'created'), false);
  assert.equal(labelVisible({ edges: { knows: [] }, hiddenEdges: ['knows'] }, 'edges', 'knows'), false, 'hidden wins');
  const view = {
    vertexIds: ['1', '2', '3'], vertexLabel: Uint32Array.from([0, 1, 2]), vertexName: ['a', 'b', 'c'],
    edgeIds: ['e'], edgeSource: Uint32Array.from([0]), edgeTarget: Uint32Array.from([2]), edgeLabel: Uint32Array.from([3]),
    labels: ['person', 'software', 'city', 'knows'],
  };
  const f = labelFilterView(view, spec);
  assert.deepEqual(f.vertexIds, ['1', '3']);
  assert.deepEqual(f.edgeIds, ['e']);
});

test('a filtered neighbourhood says "X of Y" from its unfiltered counts', () => {
  const view = { vertexIds: ['1', '2'], edgeIds: ['e'], matchedNodes: 2, matchedEdges: 1, unfiltered: { nodes: 4, edges: 3, vertexIds: ['1', '2', '3', '4'] } };
  assert.equal(filteredText(view, 'neighbourhood'), 'Filtered: 2 of 4 vertices, 1 of 3 edges of the neighbourhood');
  const { unfiltered, ...old } = view;
  assert.match(filteredText(old, 'neighbourhood'), /pass$/, 'without the counts: the old wording');
});

test('expansions and redraws keep every vertex the engine selected, hidden ones included', () => {
  const limits = { maxNodes: 100, maxEdges: 200 };
  const filtered = { vertexIds: ['1', '4'], unfiltered: { nodes: 5, edges: 4, vertexIds: ['1', '2', '3', '4', '5'] } };
  assert.deepEqual(neighbourhoodIds(filtered), ['1', '2', '3', '4', '5']);
  assert.deepEqual(neighbourhoodIds({ vertexIds: [1, 2] }), ['1', '2']);
  assert.deepEqual(neighbourhoodIds(null), []);
  const grow = expandRequest(filtered, 1, limits);
  assert.deepEqual(grow.ids, ['4']);
  assert.deepEqual(grow.keep, ['1', '2', '3', '4', '5']);
  assert.deepEqual(redrawRequest(filtered, ['1'], limits), {
    ids: ['1'], hops: 0, keep: ['1', '2', '3', '4', '5'], maxNodes: 100, maxEdges: 200,
  });
});
