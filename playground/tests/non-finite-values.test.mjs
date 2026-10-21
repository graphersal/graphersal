// Node tests of non-finite floats in the playground: the engine sends NaN, Infinity and -Infinity
// as GraphSON's typed double `{"@type": "g:Double", "@value": "NaN"}` (JSON has no number for
// them; crates/graphersal-session `display.rs` `non_finite_marker`), and the result table, the
// inspector and the Markdown copy show them bare (js/util.js).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatValue, isNonFiniteCell, jsonText, nonFiniteNumber, toMarkdownTable } from '../js/util.js';
import { valueView } from '../js/value-view.js';
import { inspectorRows } from '../js/graph-data.js';

const NAN = { '@type': 'g:Double', '@value': 'NaN' };
const INF = { '@type': 'g:Double', '@value': 'Infinity' };
const NINF = { '@type': 'g:Double', '@value': '-Infinity' };

test('a typed double is a non-finite float and shows bare', () => {
  for (const [cell, text] of [[NAN, 'NaN'], [INF, 'Infinity'], [NINF, '-Infinity']]) {
    assert.ok(isNonFiniteCell(cell));
    assert.equal(formatValue(cell), text);
    assert.equal(valueView(cell).full, text);
  }
  assert.ok(Number.isNaN(nonFiniteNumber(NAN)));
  assert.equal(nonFiniteNumber(INF), Infinity);
  assert.equal(nonFiniteNumber(NINF), -Infinity);
});

test('other values are not mistaken for one', () => {
  assert.ok(!isNonFiniteCell('NaN'));
  assert.ok(!isNonFiniteCell(NaN));
  assert.ok(!isNonFiniteCell(null));
  assert.ok(!isNonFiniteCell([NAN]));
  assert.ok(!isNonFiniteCell({ '@type': 'g:Double', '@value': 1.5 }));
  assert.ok(!isNonFiniteCell({ '@type': 'g:Double', '@value': 'NaN', extra: 1 }));
  assert.equal(formatValue('NaN'), 'NaN');
  assert.equal(formatValue({ '@type': 'g:Double', '@value': 'nan' }), '{"@type":"g:Double","@value":"nan"}');
});

test('nested non-finite floats show bare inside JSON text', () => {
  assert.equal(jsonText([1.5, NAN, { w: NINF, s: 'NaN' }]), '[1.5,NaN,{"w":-Infinity,"s":"NaN"}]');
  assert.equal(formatValue({ xs: [INF] }), '{"xs":[Infinity]}');
  assert.equal(jsonText({ a: 1 }), '{"a":1}');
  assert.equal(valueView([1, NAN]).full, '[1,NaN]');
});

test('the inspector and the Markdown copy show them bare', () => {
  const rows = inspectorRows({ kind: 'vertex', id: 'a', label: 'x', labels: ['x'], properties: { w: NAN, xs: [NINF, 2] } }, formatValue);
  assert.deepEqual(rows.slice(-2), [['w', 'NaN'], ['xs', '[-Infinity,2]']]);
  const md = toMarkdownTable({ columns: ['value'], rows: [[NAN], [INF], [[1, NINF]]] });
  assert.equal(md, '| value |\n| --- |\n| NaN |\n| Infinity |\n| [1,-Infinity] |\n');
});
