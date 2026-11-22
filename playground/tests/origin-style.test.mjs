// Node tests of a LAYER's origin styling (js/graph/origin-style.js) in both renderer tiers: the
// Detailed tier's Cytoscape classes and rules (graph-data.js `toCyElements`), the Fast tier's
// sigma attributes, the legend, and that the origin columns survive every re-indexing of a
// drawing (slice, sub-view, label filter, edit patches). A plain graph's drawing is unchanged.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  ORIGIN_COLORS, ORIGIN_TEXT, FAST_RING_MAX, MODIFIED_DASH, hasOrigins, originAt, originColors, cyOriginClass, cyOriginRules,
  sigmaNodeOrigin, sigmaEdgeOrigin, fastRings, originCaption, originLegend,
} from '../js/graph/origin-style.js';
import { toCyElements, sliceGraphView, subView, inspectorRows, originColumns } from '../js/graph-data.js';
import { labelFilterView } from '../js/graph/filter-model.js';
import { applyEdit } from '../js/graph/edit-mode.js';

/** Modern-like view: 3 vertices, 2 edges; with `origins` the layer's columns. */
function view(origins = true) {
  const v = {
    vertexIds: ['1', '2', '3'],
    vertexLabel: Uint32Array.from([0, 0, 1]),
    vertexName: ['marko', 'vadas', 'lop'],
    edgeIds: ['7', '9'],
    edgeSource: Uint32Array.from([0, 0]),
    edgeTarget: Uint32Array.from([1, 2]),
    edgeLabel: Uint32Array.from([2, 3]),
    labels: ['person', 'software', 'knows', 'created'],
    truncated: false,
    totalNodes: 3,
    totalEdges: 2,
  };
  if (origins) {
    v.vertexOrigin = ['modified', 'base', 'added'];
    v.edgeOrigin = ['base', 'added'];
  }
  return v;
}

const slotOf = () => 0;
const colors = originColors(false);

test('origins are read per element; a plain drawing has none', () => {
  assert.equal(hasOrigins(view()), true);
  assert.equal(hasOrigins(view(false)), false);
  assert.equal(originAt(view(), 'node', 0), 'modified');
  assert.equal(originAt(view(), 'node', 2), 'added');
  assert.equal(originAt(view(), 'edge', 1), 'added');
  assert.equal(originAt(view(false), 'node', 0), null);
  assert.equal(originAt({ vertexOrigin: [null] }, 'node', 0), null, 'a stale handle');
  assert.equal(originAt(view(), 'node', 9), null, 'out of range');
});

test('Detailed tier: added and modified elements get their classes, base and plain drawings none', () => {
  const els = toCyElements(view(), { slotOf });
  assert.equal(els[0].classes, 's0 origin-modified');
  assert.equal(els[1].classes, 's0', 'base: drawn as before');
  assert.equal(els[2].classes, 's0 origin-added');
  assert.equal(els[3].classes, undefined, 'base edge: no class');
  assert.equal(els[4].classes, 'origin-added');
  const plain = toCyElements(view(false), { slotOf });
  assert.deepEqual(plain.map((e) => e.classes), ['s0', 's0', 's0', undefined, undefined], 'a plain graph is unchanged');
  assert.equal(cyOriginClass('base'), '');
  assert.equal(cyOriginClass(null), '');
});

test('Detailed tier: outline solid for added, dashed for modified (colour plus a second cue)', () => {
  const rules = cyOriginRules(colors);
  const by = Object.fromEntries(rules.map((r) => [r.selector, r.style]));
  assert.equal(by['node.origin-added']['border-style'], 'solid');
  assert.equal(by['node.origin-modified']['border-style'], 'dashed');
  assert.equal(by['node.origin-added']['border-color'], ORIGIN_COLORS.light.added);
  assert.equal(by['node.origin-modified']['border-color'], ORIGIN_COLORS.light.modified);
  assert.ok(by['node.origin-added']['border-width'] >= 2);
  assert.equal(by['edge.origin-added']['line-style'], 'solid');
  assert.equal(by['edge.origin-modified']['line-style'], 'dashed');
  assert.ok(by['edge.origin-added'].width > by['edge.origin-modified'].width, 'added edges are wider too');
  assert.equal(by['edge.origin-added']['target-arrow-shape'], undefined, 'arrows follow the size plan');
  assert.equal(cyOriginRules(colors, { arrows: false })[2].style['target-arrow-shape'], 'none');
});

test('Fast tier: a ring per changed vertex (solid / dashed), edges coloured and sized, the hover label names the origin', () => {
  const added = sigmaNodeOrigin('added', colors);
  assert.deepEqual(added, { origin: 'added', ringColor: colors.added, ringDash: [], highlighted: true });
  const modified = sigmaNodeOrigin('modified', colors);
  assert.deepEqual(modified.ringDash, [...MODIFIED_DASH], 'dashed ring');
  assert.equal(modified.ringColor, colors.modified);
  assert.equal(sigmaNodeOrigin('base', colors), null);
  assert.equal(sigmaNodeOrigin(null, colors), null);
  assert.equal(sigmaNodeOrigin('added', colors, false), null, 'above the ring cap: no ring');
  const e = sigmaEdgeOrigin('added', colors);
  assert.equal(e.color, colors.added);
  assert.ok(e.size > sigmaEdgeOrigin('modified', colors).size);
  assert.equal(sigmaEdgeOrigin('base', colors), null);
  assert.equal(originCaption('knows', 'added'), 'knows · added');
  assert.equal(originCaption('knows', 'base'), 'knows');
});

test('Fast tier: rings only up to FAST_RING_MAX changed vertices', () => {
  assert.equal(fastRings(view()), true);
  assert.equal(fastRings(view(false)), false);
  const many = { vertexOrigin: Array.from({ length: FAST_RING_MAX + 1 }, () => 'added') };
  assert.equal(fastRings(many), false);
  assert.equal(fastRings({ vertexOrigin: [...many.vertexOrigin.slice(1), 'base', 'base'] }), true);
});

test('colours: a colour-blind-safe pair per theme, the CSS tokens win, defined for light and both dark blocks', () => {
  assert.notEqual(ORIGIN_COLORS.light.added, ORIGIN_COLORS.light.modified);
  assert.notEqual(ORIGIN_COLORS.dark.added, ORIGIN_COLORS.light.added, 'a dark variant');
  assert.deepEqual(originColors(true), { added: ORIGIN_COLORS.dark.added, modified: ORIGIN_COLORS.dark.modified });
  const read = (name) => ({ '--origin-added': '#111111' }[name] || '');
  assert.deepEqual(originColors(false, read), { added: '#111111', modified: ORIGIN_COLORS.light.modified });
  const css = readFileSync(new URL('../css/app.css', import.meta.url), 'utf8');
  assert.equal(css.match(/--origin-added:/g).length, 3, 'light, prefers-color-scheme dark, data-theme dark');
  assert.equal(css.match(/--origin-modified:/g).length, 3);
  assert.match(css, /\.legend-ring\.origin-modified \{[^}]*dashed/);
});

test('legend: added, modified and base only when the graph is a layer and the drawing has origins', () => {
  const layer = { base: 'modern' };
  const entries = originLegend(view(), layer);
  assert.deepEqual(entries.map((e) => e.origin), ['added', 'modified', 'base']);
  assert.deepEqual(entries.map((e) => e.cue), ['solid outline', 'dashed outline', 'no outline']);
  assert.deepEqual(originLegend(view(), null), [], 'a plain graph has no origin legend');
  assert.deepEqual(originLegend(view(false), layer), []);
});

test('the inspector names a layer element\'s origin; a plain element has no such row', () => {
  const rows = inspectorRows({ kind: 'vertex', id: 1, label: 'person', properties: {}, origin: 'modified' }, String);
  assert.deepEqual(rows.at(-1), ['origin', ORIGIN_TEXT.modified]);
  const plain = inspectorRows({ kind: 'vertex', id: 1, label: 'person', properties: {} }, String);
  assert.ok(!plain.some((r) => r[0] === 'origin'));
});

test('the origin columns follow every re-indexing of a drawing', () => {
  assert.deepEqual(originColumns(view(false), [0], [0]), {}, 'a plain view gets none');
  const sliced = sliceGraphView(view(), 2);
  assert.deepEqual(sliced.vertexOrigin, ['modified', 'base']);
  assert.deepEqual(sliced.edgeOrigin, ['base'], 'only the edge between the kept vertices');
  const sub = subView(view(), [2, 0]);
  assert.deepEqual(sub.vertexOrigin, ['added', 'modified']);
  assert.deepEqual(sub.edgeOrigin, ['added']);
  assert.equal(subView(view(false), [0]).vertexOrigin, undefined);
  const filtered = labelFilterView(view(), { vertices: { person: [] } });
  assert.deepEqual(filtered.vertexOrigin, ['modified', 'base']);
  assert.deepEqual(filtered.edgeOrigin, ['base']);
  // Edit patches: a new element is added in the layer; removals keep the columns aligned.
  const withVertex = applyEdit(view(), { type: 'add-vertex', drawn: { id: '4', label: 'person', caption: 'josh' } });
  assert.deepEqual(withVertex.vertexOrigin, ['modified', 'base', 'added', 'added']);
  const withEdge = applyEdit(view(), { type: 'add-edge', drawn: { id: '8', label: 'knows', source: '2', target: '3' } });
  assert.deepEqual(withEdge.edgeOrigin, ['base', 'added', 'added']);
  const noVertex = applyEdit(view(), { type: 'remove', kind: 'vertex', id: '1' });
  assert.deepEqual(noVertex.vertexOrigin, ['base', 'added']);
  assert.deepEqual(noVertex.edgeOrigin, [], 'its edges went with it');
  const noEdge = applyEdit(view(), { type: 'remove', kind: 'edge', id: '7' });
  assert.deepEqual(noEdge.edgeOrigin, ['added']);
  const plainAdd = applyEdit(view(false), { type: 'add-vertex', drawn: { id: '4', label: 'person' } });
  assert.equal(plainAdd.vertexOrigin, undefined, 'a plain drawing gets no origin column');
});
