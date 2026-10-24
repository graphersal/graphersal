// Node tests of the filter popover's label swatches (js/graph/palette.js `labelSwatch`,
// `paletteLabels`): the swatch is the colour the drawing gives the label, it never assigns a slot,
// and a label not drawn yet has no colour.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  LabelPalette, labelSwatch, paletteLabels, slotColor, OTHER, PALETTE_LIGHT, PALETTE_DARK,
} from '../js/graph/palette.js';

const view = { labels: ['software', 'person', 'unused'], vertexLabel: Uint32Array.from([1, 0, 1, 1]) };

test('the swatch of a drawn label is its drawing colour in both themes', () => {
  const p = new LabelPalette();
  p.assign(paletteLabels(view, 'data'));
  assert.deepEqual(labelSwatch(p, 'software'), { color: PALETTE_LIGHT[0], assigned: true, other: false });
  assert.deepEqual(labelSwatch(p, 'person', true), { color: PALETTE_DARK[1], assigned: true, other: false });
  assert.equal(labelSwatch(p, 'person').color, p.color('person'));
});

test('a label not drawn yet has no colour and is not assigned by asking', () => {
  const p = new LabelPalette();
  p.assign(['person']);
  assert.deepEqual(labelSwatch(p, 'software'), { color: null, assigned: false, other: false });
  assert.equal(p.peek('software'), undefined, 'labelSwatch never assigns');
  p.assign(['software']);
  assert.equal(labelSwatch(p, 'software').color, slotColor(1));
});

test('a label past the palette shows the "other" colour', () => {
  const p = new LabelPalette(1);
  p.assign(['a', 'b']);
  assert.deepEqual(labelSwatch(p, 'b'), { color: OTHER.light, assigned: true, other: true });
});

test('paletteLabels: data views in label-table order (used labels only), schema in node order', () => {
  assert.deepEqual(paletteLabels(view, 'data'), ['software', 'person']);
  assert.deepEqual(paletteLabels({ nodes: [{ label: 'x' }, { label: 'y' }] }, 'schema'), ['x', 'y']);
  assert.deepEqual(paletteLabels(null, 'data'), []);
});
