// Node tests of the saved-query editor's parameter completion (js/editor/param-completion.js):
// the declared parameters in a Rhai body, and after `$` in a GQL body (js/editor/gql-completion.js).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { gqlCompletionSource } from '../js/editor/gql-completion.js';
import { keysBySchema } from '../js/editor/gql-complete.js';
import { declaredParams, rhaiNameContext, rhaiParamSource, paramTypeText } from '../js/editor/param-completion.js';
import { newParam } from '../js/saved-queries.js';

/** A CodeMirror completion context over `marked` (‸ = cursor), as far as the sources read it. */
function cmContext(marked, explicit = false) {
  const pos = marked.indexOf('‸');
  const text = marked.replace('‸', '');
  return { pos, explicit, state: { doc: { toString: () => text }, sliceDoc: (a, b) => text.slice(a, b) } };
}

const VOCAB = { vertexLabels: ['person'], edgeLabels: [], keys: keysBySchema(null) };

test('saved queries: the declared parameters complete in a Rhai body, typed', () => {
  const params = [{ ...newParam('min'), type: 'integer' }, { ...newParam('names'), type: 'array', itemsType: 'string' }, newParam(' '), newParam('min')];
  assert.deepEqual(declaredParams(params), [{ name: 'min', detail: 'integer' }, { name: 'names', detail: 'array of string' }]);
  assert.equal(paramTypeText(newParam('x')), 'string');
  assert.deepEqual(rhaiNameContext('g.v().has("age", P.gt(mi'), { from: 22, prefix: 'mi' });
  assert.equal(rhaiNameContext('g.v().va'), null);
  assert.equal(rhaiNameContext('g.v().has("mi'), null);
  assert.equal(rhaiNameContext('// mi'), null);
  // The table is read on every request: a renamed parameter shows at once.
  const table = [{ ...newParam('min'), type: 'integer' }];
  const source = rhaiParamSource(() => table);
  const first = source(cmContext('g.v().has("age", P.gt(m‸'));
  assert.deepEqual(first.options.map((o) => [o.label, o.detail, o.boost]), [['min', 'parameter · integer', 5]]);
  table[0].name = 'lowest';
  assert.deepEqual(source(cmContext('g.v().has("age", P.gt(l‸')).options.map((o) => o.label), ['lowest']);
  table.length = 0;
  assert.equal(source(cmContext('g.v().has("age", P.gt(l‸')), null);
  assert.equal(source(cmContext('g.v().‸', true)), null);
});

test('saved queries: a GQL body offers the declared parameters after "$"', () => {
  const table = [{ ...newParam('min'), type: 'integer' }];
  const source = gqlCompletionSource(() => VOCAB, () => declaredParams(table));
  assert.deepEqual(source(cmContext('MATCH (n) WHERE n.age > $‸')).options.map((o) => [o.label, o.detail]), [['$min', 'integer']]);
  table.push(newParam('who'));
  assert.deepEqual(source(cmContext('MATCH (n) WHERE n.name = $w‸')).options.map((o) => o.label), ['$min', '$who']);
});
