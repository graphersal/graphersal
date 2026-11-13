// Node tests of the profile graph view's model (js/profile-graph.js): the profile's `graph` key,
// data validation with clear errors, style resolution (defaults, "*", label, classes, styleOf),
// the styled view the renderers draw, the legend, merging expanded neighbours without
// duplicates, carried positions, the selection highlight and the options of `api.graph`; plus
// the parts of the drawing code it shares (graph-data.js styled elements, the controller's
// tuning) and the wiring into the profile API and the profile validation.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  graphSpecProblems, styleProblems, checkGraphData, GraphDataError, mergeGraph, resolveNodeStyle,
  resolveEdgeStyle, styledView, legendEntries, carryPositions, highlightIds, selectionOf,
  checkGraphOptions, colorOf, BUILTIN_CLASSES, NODE_SIZE, EDGE_WIDTH, GRAPH_LIMITS, isStyleColor,
} from '../js/profile-graph.js';
import { toCyElements, edgeCaption, vertexCaption } from '../js/graph-data.js';
import { GraphController } from '../js/graph/controller.js';
import { DRAW_TUNING } from '../js/graph-data.js';
import { createProfileApi } from '../js/profile-api.js';
import { validateProfile, BUILTIN_PROFILES, ProfileError } from '../js/ui-profiles.js';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));

const ctx = (over = {}) => ({
  labelColor: (label) => ({ memory: '#111111', session: '#222222' }[label] || '#999999'),
  token: (name) => ({ accent: '#2f6fde', danger: '#c4372f' }[name] || ''),
  borderColor: '#000000',
  edgeColor: '#aaaaaa',
  neutralColor: '#888888',
  ...over,
});

const data = () => ({
  nodes: [
    { id: 'a', label: 'memory', caption: 'Alpha', data: { kind: 'user' } },
    { id: 'b', label: 'memory', data: { title: 'Beta', kind: 'feedback' }, classes: ['faded'] },
    { id: 3, label: 'session', data: {} },
  ],
  edges: [
    { id: 'e1', source: 'a', target: 'b', label: 'supersedes' },
    { id: 'e2', source: 'b', target: 3, label: 'about', data: { at: '2026-11-13' } },
  ],
});

test('graphSpecProblems: a valid spec has none, every mistake is named', () => {
  assert.deepEqual(graphSpecProblems({}), []);
  assert.deepEqual(graphSpecProblems({
    nodes: { '*': { size: 20 }, memory: { color: '#abc', shape: 'diamond', border: { width: 3, color: '--accent', style: 'dashed' }, caption: 'title', opacity: 0.5 } },
    edges: { supersedes: { color: '#aabbcc', style: 'dotted', arrow: false, width: 2, opacity: 1, caption: 'at' } },
    classes: { pinned: { border: { width: 5 } }, weak: { width: 0.5, opacity: 0.4 } },
    legend: [{ text: 'pinned', node: { border: { width: 5 } } }, { text: 'replaced', edge: { style: 'dashed' } }],
  }), []);
  assert.deepEqual(graphSpecProblems([]), ['"graph" must be an object']);
  const problems = graphSpecProblems({
    node: {},
    nodes: { memory: { colour: 'red', shape: 'blob', size: 500, border: { width: -1, style: 'wavy', glow: 1 } } },
    edges: { x: { style: 'zigzag', arrow: 'yes', width: 99, color: 'red' } },
    classes: { '1bad': { opacity: 3 } },
    legend: [{ text: '', node: {}, edge: {} }],
  });
  const text = problems.join('\n');
  for (const part of [
    '"graph" has the unknown key "node"',
    '"graph.nodes.memory" has the unknown key "colour"',
    '"graph.nodes.memory.shape" must be one of ellipse',
    `"graph.nodes.memory.size" must be a number from ${NODE_SIZE.min} to ${NODE_SIZE.max}`,
    '"graph.nodes.memory.border.width" must be a number from 0 to 12',
    '"graph.nodes.memory.border.style" must be one of solid, dashed, dotted',
    '"graph.nodes.memory.border" has the unknown key "glow"',
    '"graph.edges.x.style" must be one of solid, dashed, dotted',
    '"graph.edges.x.arrow" must be true or false',
    `"graph.edges.x.width" must be a number from ${EDGE_WIDTH.min} to ${EDGE_WIDTH.max}`,
    '"graph.edges.x.color" must be "#rgb", "#rrggbb" or a CSS token like "--accent"',
    '"graph.classes" has the invalid class name "1bad"',
    '"graph.classes.1bad.opacity" must be a number from 0.05 to 1',
    '"graph.legend[0].text" must be a text',
    '"graph.legend[0]" must have exactly one of "node" and "edge"',
  ]) assert.ok(text.includes(part), `missing: ${part}\n${text}`);
  // A node key on an edge and the other way round.
  assert.match(styleProblems({ shape: 'star' }, 'edge', 'x').join(), /unknown key "shape"/);
  assert.match(styleProblems({ arrow: true }, 'node', 'x').join(), /unknown key "arrow"/);
  assert.deepEqual(styleProblems({ shape: 'star', arrow: true }, 'class', 'x'), [], 'a class takes both');
  assert.ok(isStyleColor('#fff') && isStyleColor('#A1b2C3') && isStyleColor('--graph-edge'));
  assert.ok(!isStyleColor('red') && !isStyleColor('#ffff') && !isStyleColor('var(--x)') && !isStyleColor('--'));
});

test('checkGraphData: normalizes ids, labels, data and classes', () => {
  const g = checkGraphData(data());
  assert.deepEqual(g.nodes.map((n) => n.id), ['a', 'b', '3'], 'ids as text');
  assert.equal(g.nodes[0].caption, 'Alpha');
  assert.equal(g.nodes[1].caption, null);
  assert.deepEqual(g.nodes[2].classes, []);
  assert.deepEqual(g.edges[1], { id: 'e2', label: 'about', caption: null, data: { at: '2026-11-13' }, classes: [], source: 'b', target: '3' });
  assert.deepEqual(checkGraphData({}), { nodes: [], edges: [] }, 'both lists may be left out');
  assert.equal(checkGraphData({ nodes: [{ id: 1 }] }).nodes[0].label, '', 'label defaults to ""');
});

test('checkGraphData: every problem is named in one error', () => {
  const err = (input) => {
    try {
      checkGraphData(input);
    } catch (e) {
      assert.ok(e instanceof GraphDataError);
      return e;
    }
    assert.fail('no error');
  };
  assert.match(err(null).message, /^api\.graph: the graph data is invalid: it must be an object \{nodes, edges\}\.$/);
  assert.match(err({ nodes: {} }).message, /"nodes" must be a list/);
  assert.match(err({ vertices: [] }).message, /unknown key "vertices" \(the keys are nodes, edges\)/);
  const e = err({
    nodes: [{ id: 'a', colour: 1 }, { id: 'a' }, { id: '' }, 'x', { id: 'c', label: 3, data: [], classes: ['ok', '2bad'] }],
    edges: [{ id: 'e', source: 'a', target: 'zz' }, { id: 'e', source: 'a', target: 'a' }, { source: 'a' }],
  });
  for (const part of [
    'nodes[0] has the unknown key "colour"',
    'nodes[1].id "a" is used twice',
    'nodes[2].id must be a non-empty text or a number',
    'nodes[3] must be an object',
    'nodes[4].label must be a text',
    'nodes[4].data must be an object',
    'nodes[4].classes must be a list of class names',
    'edges[0].target "zz" is not a node of the data',
    'edges[1].id "e" is used twice',
    'edges[2].id must be a non-empty text or a number',
    'edges[2].target must be the id of a node',
  ]) assert.ok(e.problems.some((p) => p.startsWith(part)), `missing: ${part}\n${e.problems.join('\n')}`);
  const many = err({ nodes: Array.from({ length: 20 }, () => ({ id: '' })) });
  assert.equal(many.problems.length, 20);
  assert.match(many.message, /; and 8 more\.$/, 'a long list is cut in the message, kept in `problems`');
  assert.equal(GRAPH_LIMITS.problems, 12);
});

test('mergeGraph: the union without duplicates; fresher content, old order; added ids', () => {
  const base = { nodes: [{ id: 'a', label: 'memory', data: { v: 1 } }, { id: 'b', label: 'memory' }], edges: [{ id: 'e1', source: 'a', target: 'b', label: 'relates_to' }] };
  const more = {
    nodes: [{ id: 'a', label: 'memory', data: { v: 2 } }, { id: 's1', label: 'session' }, { id: 's2', label: 'session' }],
    edges: [{ id: 'e2', source: 'a', target: 's1', label: 'about' }, { id: 'e3', source: 'a', target: 's2', label: 'about' }],
  };
  const { graph, added } = mergeGraph(base, more);
  assert.deepEqual(graph.nodes.map((n) => n.id), ['a', 'b', 's1', 's2']);
  assert.equal(graph.nodes[0].data.v, 2, 'an element in both takes the addition\'s content');
  assert.deepEqual(graph.edges.map((e) => e.id), ['e1', 'e2', 'e3']);
  assert.deepEqual(added, { nodes: ['s1', 's2'], edges: ['e2', 'e3'] });
  // Merging the same again adds nothing.
  const again = mergeGraph(graph, more);
  assert.deepEqual(again.added, { nodes: [], edges: [] });
  assert.equal(again.graph.nodes.length, 4);
  assert.equal(again.graph.edges.length, 3);
  // Number and text ids are the same element.
  assert.deepEqual(mergeGraph({ nodes: [{ id: 7 }] }, { nodes: [{ id: '7' }] }).added.nodes, []);
  // The merged data is valid data again; an addition with a dangling edge is refused.
  assert.doesNotThrow(() => checkGraphData(graph));
  assert.throws(() => mergeGraph(base, { edges: [{ id: 'x', source: 'a', target: 'nope' }] }), GraphDataError);
});

test('resolveNodeStyle: defaults, then "*", the label, the classes, then styleOf', () => {
  const [a, b, s] = checkGraphData(data()).nodes;
  const plain = resolveNodeStyle(s, {}, ctx());
  assert.deepEqual(plain, { color: '#222222', shape: 'ellipse', size: 24, opacity: 1, borderWidth: 0, borderColor: '#000000', borderStyle: 'solid', caption: 'session' });
  const spec = {
    nodes: { '*': { size: 20, shape: 'diamond' }, memory: { color: '--accent', caption: 'title', border: { width: 2 } } },
    classes: { faded: { opacity: 0.15 }, pinned: { border: { width: 5, color: '#ff0000' } } },
  };
  const ra = resolveNodeStyle(a, spec, ctx());
  assert.equal(ra.color, '#2f6fde', 'a token is resolved');
  assert.equal(ra.shape, 'diamond', '"*" applies');
  assert.equal(ra.size, 20);
  assert.equal(ra.borderWidth, 2);
  assert.equal(ra.caption, 'Alpha', 'the element\'s caption wins over the caption field');
  const rb = resolveNodeStyle(b, spec, ctx());
  assert.equal(rb.caption, 'Beta', 'the caption field of data');
  assert.equal(rb.opacity, 0.15, 'the profile\'s class replaces the built-in one');
  // styleOf: its classes after the element's, its keys last; border merged by key.
  const styleOf = (el, kind) => (kind === 'node' && el.id === 'b' ? { classes: ['pinned'], border: { style: 'dashed' }, color: '#00ff00' } : null);
  const rb2 = resolveNodeStyle(b, spec, ctx({ styleOf }));
  assert.deepEqual([rb2.borderWidth, rb2.borderColor, rb2.borderStyle, rb2.color], [5, '#ff0000', 'dashed', '#00ff00']);
  // Built-in classes without a profile entry; unknown classes change nothing.
  const rbuilt = resolveNodeStyle({ ...s, classes: ['bold', 'dashed', 'nope'] }, {}, ctx());
  assert.equal(rbuilt.borderWidth, BUILTIN_CLASSES.dashed.border.width, 'later class wins');
  assert.equal(rbuilt.borderStyle, 'dashed');
  assert.equal(resolveNodeStyle({ ...s, classes: ['faded'] }, {}, ctx()).opacity, 0.3);
  // An unknown token falls back to the label colour.
  assert.equal(resolveNodeStyle(s, { nodes: { session: { color: '--nope' } } }, ctx()).color, '#222222');
  // An invalid styleOf answer is a TypeError naming the problem.
  assert.throws(() => resolveNodeStyle(a, {}, ctx({ styleOf: () => ({ shape: 'blob' }) })), /"styleOf\(node a\)\.shape" must be one of/);
  assert.throws(() => resolveNodeStyle(a, {}, ctx({ styleOf: () => 'red' })), /styleOf must return an object or null/);
  assert.throws(() => resolveNodeStyle(a, {}, ctx({ styleOf: () => ({ classes: 'x' }) })), /classes must be a list/);
  // A long caption is cut.
  assert.equal(resolveNodeStyle({ ...s, caption: 'x'.repeat(500) }, {}, ctx()).caption.length, GRAPH_LIMITS.text);
});

test('resolveEdgeStyle: defaults, label, classes, styleOf, caption field', () => {
  const [e1, e2] = checkGraphData(data()).edges;
  assert.deepEqual(resolveEdgeStyle(e1, {}, ctx()), { color: '#aaaaaa', lineStyle: 'solid', arrow: true, width: 1.6, opacity: 1, caption: 'supersedes' });
  const spec = { edges: { about: { color: '--danger', style: 'dashed', arrow: false, width: 0.8, caption: 'at' } } };
  assert.deepEqual(resolveEdgeStyle(e2, spec, ctx()), { color: '#c4372f', lineStyle: 'dashed', arrow: false, width: 0.8, opacity: 1, caption: '2026-11-13' });
  const faded = resolveEdgeStyle({ ...e1, classes: ['faded', 'bold'] }, {}, ctx());
  assert.deepEqual([faded.opacity, faded.width], [0.3, 3.2]);
  assert.equal(resolveEdgeStyle(e1, {}, ctx({ styleOf: (el, kind) => (kind === 'edge' ? { width: 5 } : null) })).width, 5);
  assert.throws(() => resolveEdgeStyle(e1, {}, ctx({ styleOf: () => ({ shape: 'star' }) })), /unknown key "shape"/);
});

test('styledView: a compact graph view with a style per element, drawn by toCyElements', () => {
  const g = checkGraphData(data());
  const v = styledView(g, { edges: { about: { caption: 'at' } } }, ctx());
  assert.deepEqual(v.vertexIds, ['a', 'b', '3']);
  assert.deepEqual(v.labels, ['memory', 'session', 'supersedes', 'about']);
  assert.deepEqual(Array.from(v.vertexLabel), [0, 0, 1]);
  assert.deepEqual(Array.from(v.edgeSource), [0, 1]);
  assert.deepEqual(Array.from(v.edgeTarget), [1, 2]);
  assert.deepEqual(v.vertexName, ['Alpha', 'memory', 'session']);
  assert.equal(vertexCaption(v, 0), 'Alpha');
  assert.equal(edgeCaption(v, 1), '2026-11-13', 'the edge caption of the style');
  assert.equal(edgeCaption(v, 0), 'supersedes');
  assert.equal(edgeCaption(v, 0, true), 'e1 · supersedes');
  assert.equal(v.nodeStyle[1].opacity, 0.3, 'the element\'s class');
  const els = toCyElements(v, { slotOf: () => 0 });
  assert.equal(els[1].classes, 's0 styled');
  assert.deepEqual(
    { color: els[1].data.color, shape: els[1].data.shape, nsize: els[1].data.nsize, opacity: els[1].data.opacity, borderWidth: els[1].data.borderWidth, borderStyle: els[1].data.borderStyle },
    { color: '#111111', shape: 'ellipse', nsize: 24, opacity: 0.3, borderWidth: 0, borderStyle: 'solid' },
  );
  assert.equal(els[4].classes, 'styled');
  assert.deepEqual([els[4].data.source, els[4].data.target, els[4].data.arrow, els[4].data.lineStyle], ['nb', 'n3', 'triangle', 'solid']);
  // A Graph panel view (no styles) is drawn exactly as before.
  const panel = { vertexIds: [1], vertexLabel: [0], vertexName: [null], edgeIds: [], edgeSource: [], edgeTarget: [], edgeLabel: [], labels: ['person'] };
  assert.deepEqual(toCyElements(panel, { slotOf: () => 2 }), [{ group: 'nodes', classes: 's2', data: { id: 'n1', kind: 'vertex', elementId: 1, index: 0, label: 'person', caption: 'person:1' } }]);
});

test('legendEntries: labels drawn, then the profile\'s entries, then the extra ones', () => {
  const g = checkGraphData(data());
  const spec = { nodes: { memory: { shape: 'diamond' } }, legend: [{ text: 'loaded at every start', node: { border: { width: 4 } } }] };
  const entries = legendEntries(g, spec, [{ text: 'replaced', edge: { style: 'dashed' } }], ctx());
  assert.deepEqual(entries.map((e) => [e.kind, e.text]), [
    ['node', 'memory'], ['node', 'session'], ['edge', 'supersedes'], ['edge', 'about'],
    ['node', 'loaded at every start'], ['edge', 'replaced'],
  ]);
  assert.equal(entries[0].style.shape, 'diamond');
  assert.equal(entries[4].style.borderWidth, 4);
  assert.equal(entries[4].style.color, '#888888', 'an entry without a colour is neutral (no palette slot taken)');
  assert.equal(entries[5].style.lineStyle, 'dashed');
  assert.ok(entries[4].extra && !entries[0].extra);
});

test('carryPositions: kept vertices stay, new ones go next to their neighbours', () => {
  const before = checkGraphData({ nodes: [{ id: 'a' }, { id: 'b' }], edges: [] });
  const xy = Float32Array.from([0, 0, 100, 0]);
  // Nothing new: the same positions in the new order.
  const same = carryPositions(['a', 'b'], xy, checkGraphData({ nodes: [{ id: 'b' }, { id: 'a' }] }));
  assert.deepEqual(Array.from(same), [100, 0, 0, 0]);
  // New vertices around the vertex they are connected to (a ring of radius >= spacing).
  const grown = checkGraphData({
    nodes: [{ id: 'a' }, { id: 'b' }, { id: 's1' }, { id: 's2' }, { id: 'lone' }],
    edges: [{ id: 'x', source: 'b', target: 's1' }, { id: 'y', source: 's2', target: 'b' }],
  });
  const out = carryPositions(['a', 'b'], xy, grown, { anchor: 'a', spacing: 50 });
  assert.deepEqual(Array.from(out.slice(0, 4)), [0, 0, 100, 0]);
  const dist = (i, cx, cy) => Math.hypot(out[2 * i] - cx, out[2 * i + 1] - cy);
  assert.ok(Math.abs(dist(2, 100, 0) - 50) < 1e-3 && Math.abs(dist(3, 100, 0) - 50) < 1e-3, 'around b');
  assert.ok(Math.hypot(out[4] - out[6], out[5] - out[7]) > 10, 'two new vertices do not overlap');
  assert.ok(Math.abs(dist(4, 0, 0) - 50) < 1e-3, 'a vertex without drawn neighbours goes around the anchor');
  // Deterministic.
  assert.deepEqual(Array.from(carryPositions(['a', 'b'], xy, grown, { anchor: 'a', spacing: 50 })), Array.from(out));
  // Nothing drawn before (or no positions): null = lay it out.
  assert.equal(carryPositions(['zz'], Float32Array.from([1, 1]), before), null);
  assert.equal(carryPositions(null, null, before), null);
  assert.equal(carryPositions(['a', 'b'], Float32Array.from([0]), before), null);
});

test('highlightIds and selectionOf', () => {
  const g = checkGraphData(data());
  assert.deepEqual(highlightIds(g, { kind: 'node', id: 'b' }), { nodes: ['b', 'a', '3'], edges: ['e1', 'e2'] });
  assert.deepEqual(highlightIds(g, { kind: 'node', id: 'b' }, 'element'), { nodes: ['b'], edges: [] });
  assert.deepEqual(highlightIds(g, { kind: 'edge', id: 'e2' }), { nodes: ['b', '3'], edges: ['e2'] });
  assert.deepEqual(highlightIds(g, { kind: 'edge', id: 'e2' }, 'element'), { nodes: [], edges: ['e2'] });
  assert.equal(highlightIds(g, { kind: 'node', id: 'b' }, 'none'), null);
  assert.equal(highlightIds(g, { kind: 'node', id: 'gone' }), null);
  assert.equal(highlightIds(g, null), null);
  const sel = selectionOf(g, { kind: 'node', id: '3' });
  assert.deepEqual([sel.kind, sel.id, sel.label], ['node', '3', 'session']);
  assert.equal(sel.element, g.nodes[2]);
  assert.equal(selectionOf(g, { kind: 'edge', id: 'e1' }).label, 'supersedes');
  assert.equal(selectionOf(g, { kind: 'edge', id: 'nope' }), null);
});

test('checkGraphOptions: defaults, and every mistake named', () => {
  const d = checkGraphOptions(undefined);
  assert.deepEqual({ ...d }, {
    styleOf: null, onSelect: null, onExpand: null, selected: null, card: false, legend: true, layout: 'cose', height: '520px', highlight: 'neighbours', emptyText: 'Nothing to show.',
  });
  assert.ok(Object.isFrozen(d));
  const fn = () => {};
  const o = checkGraphOptions({ styleOf: fn, onSelect: fn, onExpand: fn, selected: { kind: 'node', id: 7 }, card: { actions: fn, onAction: fn, expand: 'Show sessions' }, legend: [{ text: 't', node: {} }], layout: 'grid', height: '60vh', highlight: 'element', emptyText: 'No memories.' });
  assert.deepEqual(o.selected, { kind: 'node', id: '7' });
  assert.equal(o.layout, 'grid');
  assert.throws(() => checkGraphOptions('x'), /the options must be an object/);
  let err;
  try {
    checkGraphOptions({ onClick: fn, styleOf: 1, selected: { kind: 'vertex', id: 1 }, card: { actions: 'x', onAction: 1, expand: '', extra: 1 }, legend: 'yes', layout: 'force', height: '100', highlight: 'all', emptyText: 3 });
  } catch (e) {
    err = e;
  }
  assert.ok(err instanceof TypeError);
  for (const part of [
    'unknown option "onClick"', '"styleOf" must be a function', '"selected" must be null or {kind: "node"|"edge", id}',
    '"card" has the unknown key "extra"', '"card.actions" must be a list or a function', '"card.onAction" must be a function',
    '"card.expand" must be a button text or false', '"legend" must be a list', '"layout" must be one of cose, circle',
    '"height" must be a CSS length', '"highlight" must be one of neighbours, element, none', '"emptyText" must be a text',
  ]) assert.ok(err.message.includes(part), `missing: ${part}\n${err.message}`);
  assert.equal(checkGraphOptions({ card: true }).card, true);
  assert.equal(checkGraphOptions({ card: { expand: false } }).card.expand, false);
});

test('colorOf: hex as it is, tokens resolved, the fallback otherwise', () => {
  assert.equal(colorOf('#abc', null, '#000'), '#abc');
  assert.equal(colorOf('--accent', ctx().token, '#000'), '#2f6fde');
  assert.equal(colorOf('--none', ctx().token, '#000'), '#000');
  assert.equal(colorOf(undefined, ctx().token, '#000'), '#000');
});

test('the controller takes a layout tuning: cose above 200 vertices without an engine', async () => {
  const calls = [];
  const renderer = {
    capabilities: { tier: 'fake', layouts: ['cose', 'grid'] },
    setData() {}, setPositions() {}, getPositions() { return null; }, fit() { calls.push('fit'); },
    async setLayout(name, options) { calls.push([name, options.iterations]); },
    destroy() {},
  };
  const nodes = Array.from({ length: 300 }, (_, i) => ({ id: i }));
  const view = styledView(checkGraphData({ nodes }), {}, ctx());
  const plain = new GraphController({ renderer, engine: null });
  await plain.show(view, { kind: 'data', layout: 'cose' });
  assert.ok(!calls.some((c) => Array.isArray(c) && c[0] === 'cose'), 'the default tuning hands 300 vertices to the engine (here: none, a grid)');
  calls.length = 0;
  const tuned = new GraphController({ renderer, engine: null, tuning: { ...DRAW_TUNING, coseMaxNodes: 1500 } });
  await tuned.show(view, { kind: 'data', layout: 'cose' });
  assert.deepEqual(calls[0], ['cose', 400]);
});

test('api.graph checks its options and hands them to the page; api.mergeGraph merges', () => {
  const seen = [];
  const api = createProfileApi({
    profile: { name: 'demo', title: 'Demo' },
    engineInfo: {},
    server: false,
    runQuery: async () => ({ ok: true, results: [] }),
    renderMarkdown: () => null,
    renderCard: () => null,
    renderGraph: (d, options) => {
      seen.push([d, options]);
      return { vnode: 'graph' };
    },
    showToast: () => {},
    navigate: () => {},
    themeState: () => ({ mode: 'system', dark: false }),
    cssToken: () => '',
  });
  const d = data();
  assert.deepEqual(api.graph(d, { layout: 'grid' }), { vnode: 'graph' });
  assert.equal(seen[0][0], d, 'the data goes to the view as it is (the view checks it and shows the problems)');
  assert.equal(seen[0][1].layout, 'grid');
  assert.equal(seen[0][1].highlight, 'neighbours');
  assert.throws(() => api.graph(d, { colour: 'red' }), /unknown option "colour"/);
  assert.deepEqual(api.mergeGraph({ nodes: [{ id: 1 }] }, { nodes: [{ id: 2 }] }).added, { nodes: ['2'], edges: [] });
  // A page without a graph view says so.
  const bare = createProfileApi({
    profile: { name: 'x', title: 'X' }, engineInfo: {}, server: false, runQuery: async () => ({ ok: true }), renderMarkdown: () => null,
    renderCard: () => null, showToast: () => {}, navigate: () => {}, themeState: () => ({ mode: 'system', dark: false }), cssToken: () => '',
  });
  assert.throws(() => bare.graph(d), /this page has no graph view/);
});

test('the profile\'s graph key is validated and normalized; demo has a graph tab', () => {
  const minimal = (over = {}) => ({ name: 'mine', title: 'Mine', startTab: 'one', tabs: [{ id: 'one', title: 'One', module: 'one.js' }], ...over });
  assert.deepEqual(validateProfile(minimal()).graph, {}, 'graph defaults to {}');
  const p = validateProfile(minimal({ graph: { nodes: { memory: { color: '#123456' } } } }));
  assert.deepEqual(p.graph, { nodes: { memory: { color: '#123456' } } });
  assert.ok(Object.isFrozen(p.graph.nodes.memory));
  assert.throws(() => validateProfile(minimal({ graph: { nodes: { memory: { shape: 'blob' } } } })), (e) => e instanceof ProfileError
    && e.message.includes('"graph.nodes.memory.shape" must be one of'));
  assert.throws(() => validateProfile(minimal({ graph: 3 })), /"graph" must be an object/);
  const demo = BUILTIN_PROFILES.demo;
  assert.deepEqual(validateProfile(demo), demo);
  const tab = demo.tabs.find((t) => t.id === 'graph');
  assert.equal(tab.module, 'graph.js');
  assert.ok(existsSync(root + demo.files + tab.module));
  assert.deepEqual(graphSpecProblems(demo.graph), []);
  assert.deepEqual(BUILTIN_PROFILES.full.graph, {}, 'full has no graph styles (its Graph panel is unchanged)');
});
