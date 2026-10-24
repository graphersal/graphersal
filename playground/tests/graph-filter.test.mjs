// Node tests of the Graph panel's filter model (js/graph/filter-model.js): the engine filter built
// from the drawer's state, operator lists by key type, values by type (never sniffed), the badge
// count, engine error placement, the apply mode and queue, the client-side label filter.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  FILTER_TUNING, OPS, opsForType, typeOfSchemaNode, keyTypesOf, emptyFilter, newCondition, normalizeFilter,
  filterStorageKey, conditionValue, buildFilter, activeSpec, errorTarget, applyMode, createApplyQueue,
  labelVisible, labelFilterView, filteredText, labelRows, setAllLabels, setLabel, setConditions, withKey, keyType,
  conditionErrorText,
} from '../js/graph/filter-model.js';

const known = {
  vertexLabels: ['person', 'software'],
  edgeLabels: ['knows', 'created'],
  types: {
    vertex: { person: { name: 'string', age: 'number', born: 'temporal', active: 'boolean' }, software: { name: 'string', lang: 'string' } },
    edge: { knows: { weight: 'number' }, created: { weight: 'number' } },
  },
};
const ids = (list) => list.map((o) => o.id);

test('operator lists follow the key type; an unknown type offers the string operators', () => {
  assert.deepEqual(ids(opsForType('number')), ['eq', 'neq', 'lt', 'lte', 'gt', 'gte', 'between', 'exists', 'missing']);
  assert.deepEqual(ids(opsForType('temporal')), ids(opsForType('number')));
  assert.deepEqual(ids(opsForType('string')), ['eq', 'neq', 'contains', 'starts_with', 'ends_with', 'regex', 'exists', 'missing']);
  assert.deepEqual(ids(opsForType('unknown')), ids(opsForType('string')));
  assert.deepEqual(ids(opsForType('boolean')), ['eq', 'neq', 'exists', 'missing']);
  assert.equal(OPS.length, 13, 'every engine operator');
});

test('schema nodes map to filter types', () => {
  assert.equal(typeOfSchemaNode({ type: 'integer' }), 'number');
  assert.equal(typeOfSchemaNode({ type: 'number' }), 'number');
  assert.equal(typeOfSchemaNode({ type: ['integer', 'null'] }), 'number');
  assert.equal(typeOfSchemaNode({ type: ['integer', 'number'] }), 'number');
  assert.equal(typeOfSchemaNode({ type: 'string' }), 'string');
  assert.equal(typeOfSchemaNode({ type: 'string', format: 'uuid' }), 'string');
  assert.equal(typeOfSchemaNode({ type: 'string', format: 'date' }), 'temporal');
  assert.equal(typeOfSchemaNode({ type: 'string', format: 'date-time' }), 'temporal');
  assert.equal(typeOfSchemaNode({ type: 'boolean' }), 'boolean');
  assert.equal(typeOfSchemaNode({ anyOf: [{ type: 'integer' }, { type: 'null' }] }), 'number');
  assert.equal(typeOfSchemaNode({ anyOf: [{ type: 'integer' }, { type: 'string' }] }), 'unknown');
  assert.equal(typeOfSchemaNode({ type: ['string', 'integer'] }), 'unknown');
  assert.equal(typeOfSchemaNode({ type: 'array' }), 'unknown');
  assert.equal(typeOfSchemaNode(null), 'unknown');
  const types = keyTypesOf({
    vertices: { person: { schema: { properties: { age: { type: 'integer' }, name: { type: 'string' } } } } },
    edges: { knows: { schema: { properties: { weight: { type: 'number' } } } }, plain: {} },
  });
  assert.deepEqual(types, { vertex: { person: { age: 'number', name: 'string' } }, edge: { knows: { weight: 'number' }, plain: {} } });
  assert.deepEqual(keyTypesOf(null), { vertex: {}, edge: {} });
});

test('an empty filter state filters nothing: no spec, count 0', () => {
  const b = buildFilter(emptyFilter(), known);
  assert.equal(b.spec, null);
  assert.equal(b.count, 0);
  assert.deepEqual(b.problems, []);
});

test('conditions become the engine filter: listed labels visible, numbers stay numbers', () => {
  let m = emptyFilter();
  m = setConditions(m, 'vertices', 'person', [
    { ...newCondition('age'), op: 'gt', value: '30' },
    { ...newCondition('name'), op: 'contains', value: 'an' },
  ]);
  m = setConditions(m, 'edges', 'knows', [{ ...newCondition('weight'), op: 'gte', value: '0.5' }]);
  m = setLabel(m, 'edges', 'created', false);
  const b = buildFilter(m, known);
  assert.deepEqual(b.spec, {
    vertices: {
      person: [{ key: 'age', op: 'gt', value: 30 }, { key: 'name', op: 'contains', value: 'an' }],
      software: [],
    },
    edges: { knows: [{ key: 'weight', op: 'gte', value: 0.5 }] },
  });
  assert.equal(b.count, 4, 'three conditions and one hidden label');
  assert.deepEqual(b.sent, { 'vertices.person': [0, 1], 'edges.knows': [0] });
});

test('values are converted by the schema type only, never by their text', () => {
  // A string key keeps "30" a string; a number key makes it 30; an unknown key sends a string.
  assert.deepEqual(conditionValue({ op: 'eq', value: '30' }, 'string'), { value: '30' });
  assert.deepEqual(conditionValue({ op: 'eq', value: '30' }, 'number'), { value: 30 });
  assert.deepEqual(conditionValue({ op: 'eq', value: '30' }, 'unknown'), { value: '30' });
  assert.deepEqual(conditionValue({ op: 'eq', value: '2024-01-01' }, 'temporal'), { value: '2024-01-01' });
  assert.deepEqual(conditionValue({ op: 'eq', value: 'true' }, 'boolean'), { value: true });
  assert.deepEqual(conditionValue({ op: 'eq', value: 'true' }, 'string'), { value: 'true' });
  assert.deepEqual(conditionValue({ op: 'between', value: '1', value2: '5' }, 'number'), { value: [1, 5] });
  assert.deepEqual(conditionValue({ op: 'between', value: 'a', value2: 'm' }, 'string'), { value: ['a', 'm'] });
  assert.deepEqual(conditionValue({ op: 'exists' }, 'number'), {});
  assert.deepEqual(conditionValue({ op: 'missing', value: 'x' }, 'string'), {});
  assert.deepEqual(conditionValue({ op: 'gt', value: '' }, 'number'), { incomplete: true });
  assert.deepEqual(conditionValue({ op: 'between', value: '1', value2: '' }, 'number'), { incomplete: true });
  assert.deepEqual(conditionValue({ op: 'gt', value: 'abc' }, 'number'), { error: 'enter a number' });
  assert.deepEqual(conditionValue({ op: 'eq', value: '' }, 'string'), { incomplete: true });
});

test('incomplete conditions are not sent; wrong values are problems at their row', () => {
  let m = setConditions(emptyFilter(), 'vertices', 'person', [
    { ...newCondition('age'), op: 'gt', value: '' }, // incomplete
    { ...newCondition('age'), op: 'lt', value: 'x' }, // problem
    { ...newCondition(''), op: 'eq', value: 'y' }, // no key yet
    { ...newCondition('name'), op: 'eq', value: 'marko' },
  ]);
  const b = buildFilter(m, known);
  assert.deepEqual(b.spec.vertices.person, [{ key: 'name', op: 'eq', value: 'marko' }]);
  assert.deepEqual(b.sent['vertices.person'], [3], 'the sent condition is UI row 3');
  assert.deepEqual(b.problems, [{ field: 'vertices', label: 'person', index: 1, message: 'enter a number' }]);
  assert.equal(b.count, 1);
  m = setConditions(emptyFilter(), 'vertices', 'person', [{ ...newCondition('age'), op: 'gt', value: '' }]);
  assert.equal(buildFilter(m, known).spec, null, 'only an incomplete condition: nothing filtered');
});

test('the stored key type applies while the schema is not loaded; the schema wins once it is', () => {
  const cond = withKey(newCondition(), 'age', 'number');
  assert.equal(cond.type, 'number');
  const m = setConditions(emptyFilter(), 'vertices', 'person', [{ ...cond, op: 'gt', value: '30' }]);
  const noTypes = { vertexLabels: ['person'], edgeLabels: [] };
  assert.deepEqual(buildFilter(m, noTypes).spec.vertices.person, [{ key: 'age', op: 'gt', value: 30 }]);
  assert.equal(keyType({ vertex: { person: { age: 'string' } } }, 'vertex', 'person', cond), 'string');
  assert.equal(keyType(null, 'vertex', 'person', { key: 'x' }), 'unknown');
});

test('withKey keeps an operator the new type offers, else takes the first', () => {
  assert.equal(withKey({ ...newCondition('age'), op: 'gt' }, 'born', 'temporal').op, 'gt');
  assert.equal(withKey({ ...newCondition('age'), op: 'gt' }, 'name', 'string').op, 'eq');
  assert.equal(withKey({ ...newCondition('name'), op: 'contains' }, 'x', 'unknown').op, 'contains');
});

test('hidden labels, all / none, hide isolated', () => {
  let m = setAllLabels(emptyFilter(), 'vertices', known.vertexLabels, false);
  assert.deepEqual(buildFilter(m, known).spec, { vertices: {} }, 'none: no vertex label listed');
  assert.equal(buildFilter(m, known).count, 2);
  m = setAllLabels(m, 'vertices', known.vertexLabels, true);
  assert.equal(buildFilter(m, known).spec, null);
  m = setLabel(m, 'vertices', 'software', false);
  assert.deepEqual(buildFilter(m, known).spec, { vertices: { person: [] } });
  m = { ...m, hideIsolated: true };
  assert.deepEqual(buildFilter(m, known).spec, { vertices: { person: [] }, hideIsolated: true });
  assert.equal(buildFilter(m, known).count, 2);
  // A label hidden in the state but no longer in the graph is ignored (nothing hidden is left: the
  // field is left out); a new label is visible.
  const later = { ...known, vertexLabels: ['person', 'city'] };
  assert.deepEqual(buildFilter(m, later).spec, { hideIsolated: true });
  const more = { ...known, vertexLabels: ['person', 'software', 'city'] };
  assert.deepEqual(buildFilter(m, more).spec, { vertices: { person: [], city: [] }, hideIsolated: true });
  // The conditions of a hidden label are kept in the state but not sent.
  m = setConditions(m, 'vertices', 'software', [{ ...newCondition('lang'), value: 'java' }]);
  assert.equal(buildFilter(m, known).spec.vertices.software, undefined);
  assert.equal(m.vertices.conds.software.length, 1);
});

test('disabled keeps the filter but sends none', () => {
  const m = { ...setLabel(emptyFilter(), 'vertices', 'software', false), enabled: false };
  const b = buildFilter(m, known);
  assert.ok(b.spec, 'the build still exists');
  assert.equal(activeSpec(m, b), null);
  assert.deepEqual(activeSpec({ ...m, enabled: true }, b), b.spec);
});

test('stored states are normalized; storage keys per graph', () => {
  assert.deepEqual(normalizeFilter(null), emptyFilter());
  assert.deepEqual(normalizeFilter('junk'), emptyFilter());
  const n = normalizeFilter({
    enabled: false,
    hideIsolated: 1,
    vertices: { off: ['a', 'a', 3], conds: { person: [{ key: 'age', op: 'near', value: 5, type: 'number' }, null], x: 'no' } },
  });
  assert.equal(n.enabled, false);
  assert.equal(n.hideIsolated, false);
  assert.deepEqual(n.vertices.off, ['a']);
  assert.deepEqual(n.vertices.conds, { person: [{ key: 'age', op: 'eq', value: '5', value2: '', type: 'number' }] });
  assert.deepEqual(n.edges, { off: [], conds: {} });
  assert.equal(filterStorageKey({ kind: 'modern', name: 'Modern' }), 'graph.filter.modern:Modern');
  assert.equal(filterStorageKey({ kind: 'file', name: 'air-routes.json' }), 'graph.filter.file:air-routes.json');
  assert.equal(filterStorageKey(null), 'graph.filter');
});

test('an engine error is placed at the condition it names (through the sent indices)', () => {
  const m = setConditions(emptyFilter(), 'vertices', 'person', [
    { ...newCondition('age'), op: 'gt', value: '' },
    { ...newCondition('name'), op: 'regex', value: '(' },
  ]);
  const b = buildFilter(m, known);
  const msg = 'filter: vertices.person[0] (key "name", op "regex"): regex parse error';
  assert.deepEqual(errorTarget(msg, b), { field: 'vertices', label: 'person', index: 1 });
  assert.equal(errorTarget('filter: vertices.software[0] (key "x")', b), null, 'a label that sent nothing');
  assert.equal(errorTarget('something else', b), null);
  assert.equal(errorTarget(msg, null), null);
  // Labels with dots or brackets: the longest sent label that matches.
  const odd = { vertexLabels: ['a', 'a.b[1]'], edgeLabels: [], types: { vertex: {}, edge: {} } };
  let m2 = setConditions(emptyFilter(), 'vertices', 'a', [{ ...newCondition('k'), value: 'x' }]);
  m2 = setConditions(m2, 'vertices', 'a.b[1]', [{ ...newCondition('k'), value: 'x' }, { ...newCondition('k'), value: 'y' }]);
  const b2 = buildFilter(m2, odd);
  assert.deepEqual(errorTarget('filter: vertices.a.b[1][1] (key "k", op "eq"): bad', b2), { field: 'vertices', label: 'a.b[1]', index: 1 });
  assert.deepEqual(errorTarget('filter: vertices.a[0] (key "k", op "eq"): bad', b2), { field: 'vertices', label: 'a', index: 0 });
});

test('an engine error at a condition is shown without its location and Help paragraph', () => {
  const msg = 'filter: vertices.person[1] (key "name", op "regex"): Error: Invalid regular expression \'(\': unclosed group\nHelp: TextP.regex() takes ...';
  assert.deepEqual(conditionErrorText(msg), { text: 'Error: Invalid regular expression \'(\': unclosed group', help: 'Help: TextP.regex() takes ...' });
  assert.deepEqual(conditionErrorText('filter: edges.knows[0] (key "w"): "op" is one of eq'), { text: '"op" is one of eq', help: '' });
  assert.deepEqual(conditionErrorText('something else'), { text: 'something else', help: '' });
});

test('apply mode: automatic up to the threshold, manual above (the large sample)', () => {
  assert.equal(applyMode(6 + 6), 'auto');
  assert.equal(applyMode(FILTER_TUNING.manualApplyAbove), 'auto');
  assert.equal(applyMode(FILTER_TUNING.manualApplyAbove + 1), 'manual');
  assert.equal(applyMode(110000), 'manual');
  assert.equal(applyMode(NaN), 'auto');
  assert.equal(FILTER_TUNING.debounceMs, 300);
});

test('the apply queue runs one request at a time and only the newest answer is shown', async () => {
  const started = [];
  const resolvers = [];
  const shown = [];
  const errors = [];
  const q = createApplyQueue((v) => {
    started.push(v);
    return new Promise((resolve, reject) => resolvers.push({ resolve, reject }));
  }, { onResult: (r, v) => shown.push([r, v]), onError: (e, v) => errors.push([e.message, v]) });
  q.push('a');
  assert.equal(q.busy, true);
  q.push('b');
  q.push('c'); // supersedes b
  assert.deepEqual(started, ['a']);
  resolvers[0].resolve('A');
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(shown, [], 'the answer for a is superseded');
  assert.deepEqual(started, ['a', 'c'], 'b was never sent');
  resolvers[1].resolve('C');
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(shown, [['C', 'c']]);
  assert.equal(q.busy, false);
  q.push('d');
  resolvers[2].reject(new Error('bad'));
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(errors, [['bad', 'd']]);
});

/** A compact view: 4 vertices (2 person, 1 software, 1 city), edges knows 0->1, created 0->2, lives 1->3. */
function view() {
  return {
    vertexIds: ['1', '2', '3', '4'],
    vertexLabel: Uint32Array.from([0, 0, 1, 2]),
    vertexName: ['marko', 'vadas', 'lop', 'berlin'],
    edgeIds: ['7', '8', '9'],
    edgeSource: Uint32Array.from([0, 0, 1]),
    edgeTarget: Uint32Array.from([1, 2, 3]),
    edgeLabel: Uint32Array.from([3, 4, 5]),
    labels: ['person', 'software', 'city', 'knows', 'created', 'lives'],
    truncated: false,
    totalNodes: 4,
    totalEdges: 3,
    vertexInDegree: Uint32Array.from([0, 1, 1, 1]),
    vertexOutDegree: Uint32Array.from([2, 1, 0, 0]),
  };
}

test('label filter of a result subgraph: hidden vertex labels hide their edges, edge labels too', () => {
  const v = view();
  assert.equal(labelFilterView(v, null), v);
  assert.equal(labelFilterView(v, { vertices: { person: [], software: [], city: [] } }), v, 'nothing dropped: the same view');
  const f = labelFilterView(v, { vertices: { person: [], city: [] } });
  assert.deepEqual(f.vertexIds, ['1', '2', '4']);
  assert.deepEqual(f.edgeIds, ['7', '9']);
  assert.deepEqual([...f.edgeSource], [0, 1]);
  assert.deepEqual([...f.edgeTarget], [1, 2]);
  assert.deepEqual([...f.vertexInDegree], [0, 1, 1], 'degree columns follow the vertices');
  assert.equal(f.matchedNodes, 3);
  assert.equal(f.matchedEdges, 2);
  const e = labelFilterView(v, { edges: { knows: [] } });
  assert.deepEqual(e.vertexIds, ['1', '2', '3', '4']);
  assert.deepEqual(e.edgeIds, ['7']);
  const iso = labelFilterView(v, { edges: { knows: [] }, hideIsolated: true });
  assert.deepEqual(iso.vertexIds, ['1', '2']);
  assert.equal(labelVisible({ vertices: { person: [] } }, 'vertices', 'city'), false);
  assert.equal(labelVisible({ vertices: { person: [] } }, 'edges', 'knows'), true);
  assert.equal(labelVisible(null, 'vertices', 'x'), true);
});

test('the Filtered strip', () => {
  const v = { ...view(), matchedNodes: 2, matchedEdges: 1, totalNodes: 1204, totalEdges: 5310 };
  assert.equal(filteredText(v), 'Filtered: 2 of 1,204 vertices, 1 of 5,310 edges');
  assert.equal(filteredText(view()), null, 'not filtered');
  assert.equal(filteredText(v, 'neighbourhood'), 'Filtered: 2 vertices and 1 edge of the neighbourhood pass');
  assert.equal(filteredText(v, 'result', { vertices: 4, edges: 3 }), 'Filtered: 2 of 4 vertices, 1 of 3 edges of the query result');
  assert.equal(filteredText(v, 'sample', { vertices: 4, edges: 3 }), 'Filtered: 2 of 4 vertices, 1 of 3 edges of the sample');
});

test('drawer rows: labels with counts, keys, conditions with their types, problems and engine errors', () => {
  let m = setConditions(emptyFilter(), 'vertices', 'person', [
    { ...newCondition('age'), op: 'gt', value: 'x' },
    { ...newCondition('name'), op: 'regex', value: '(' },
  ]);
  m = setLabel(m, 'vertices', 'software', false);
  const b = buildFilter(m, known);
  const rows = labelRows(m, 'vertices', [{ label: 'person', count: 1204 }, { label: 'software', count: 312 }], known.types, {
    problems: b.problems,
    error: { field: 'vertices', label: 'person', index: 1, message: 'regex parse error' },
  });
  assert.equal(rows.length, 2);
  assert.deepEqual(rows[0].keys, ['active', 'age', 'born', 'name']);
  assert.equal(rows[0].on, true);
  assert.equal(rows[1].on, false);
  assert.equal(rows[0].conds[0].type, 'number');
  assert.equal(rows[0].conds[0].problem, 'enter a number');
  assert.equal(rows[0].conds[1].problem, 'regex parse error');
  assert.equal(rows[0].conds[1].arity, 1);
  assert.deepEqual(ids(rows[0].conds[1].ops), ids(opsForType('string')));
});
