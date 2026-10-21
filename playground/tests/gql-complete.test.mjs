// Node tests of the GQL completion: the context at a position (js/editor/gql-complete.js), the
// CodeMirror source over a stub context (js/editor/gql-completion.js), the function list against
// the engine's table (js/editor/gql-functions.js vs crates/graphersal/src/gql/parser.rs).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import {
  gqlCompletionContext, gqlVariables, gqlParamChoices, paramToken, keysBySchema, keysFor,
} from '../js/editor/gql-complete.js';
import { gqlCompletionSource } from '../js/editor/gql-completion.js';
import { GQL_FUNCTIONS, gqlFunctionSpellings } from '../js/editor/gql-functions.js';

/** The context at the `‸` of `marked`. */
function at(marked) {
  const pos = marked.indexOf('‸');
  return gqlCompletionContext(marked.replace('‸', ''), pos);
}

test('labels after ":" in a node pattern, edge types in an edge pattern', () => {
  assert.deepEqual(at('MATCH (n:‸'), { kind: 'label', from: 9, prefix: '', element: 'vertex' });
  assert.deepEqual(at('MATCH (n:pe‸'), { kind: 'label', from: 9, prefix: 'pe', element: 'vertex' });
  assert.equal(at('MATCH (n)-[e:‸').element, 'edge');
  assert.equal(at('MATCH (n)-[:kn‸').element, 'edge');
  assert.equal(at('MATCH (n:person|‸').kind, 'label');
  assert.equal(at('MATCH (n:person&!‸').kind, 'label');
  // Outside a pattern: the variable's kind.
  assert.equal(at('MATCH (n)-[e]->(m) WHERE e:‸').element, 'edge');
  assert.equal(at('MATCH (n)-[e]->(m) WHERE n:‸').element, 'vertex');
  // A `:` in a map separates a key from its value.
  assert.notEqual(at('MATCH (n {name:‸').kind, 'label');
});

test('properties after "var." and the keys of a pattern\'s property map', () => {
  assert.deepEqual(at('MATCH (n:person) WHERE n.‸'), { kind: 'property', from: 25, prefix: '', variable: 'n' });
  assert.equal(at('MATCH (n:person) RETURN n.na‸').prefix, 'na');
  assert.deepEqual(at('MATCH (n:person {‸'), { kind: 'key', from: 17, prefix: '', element: 'vertex', labels: ['person'] });
  assert.deepEqual(at("MATCH (n:person {name: 'x', a‸").labels, ['person']);
  assert.equal(at('MATCH ()-[e:knows {w‸').element, 'edge');
});

test('$parameters after "$"', () => {
  assert.deepEqual(at('MATCH (n) WHERE n.age > $‸'), { kind: 'param', from: 24, prefix: '$' });
  assert.deepEqual(at('MATCH (n) WHERE n.age > $mi‸'), { kind: 'param', from: 24, prefix: '$mi' });
});

test('nothing inside strings and comments', () => {
  assert.equal(at("MATCH (n) WHERE n.name = 'ab‸"), null);
  assert.equal(at('MATCH (n) // a comm‸'), null);
  assert.equal(at('MATCH (n) /* a ‸'), null);
});

test('keywords by position: the clause the cursor is in', () => {
  assert.equal(at('‸').clause, 'start');
  assert.equal(at('‸').keywords[0], 'MATCH');
  assert.deepEqual(at('‸').variables, []);
  const afterMatch = at('MATCH (n:person) ‸');
  assert.equal(afterMatch.clause, 'MATCH');
  assert.equal(afterMatch.keywords[0], 'WHERE');
  assert.ok(afterMatch.keywords.includes('DETACH DELETE'));
  assert.equal(afterMatch.functions, false);
  const where = at('MATCH (n:person) WHERE n.age > 3 ‸');
  assert.ok(where.keywords.includes('AND') && where.keywords.includes('RETURN'));
  assert.equal(where.functions, true);
  const ret = at('MATCH (n:person) RETURN n.name AS x ‸');
  assert.ok(ret.keywords.includes('ORDER BY') && ret.keywords.includes('LIMIT'));
  assert.ok(at('MATCH (n) RETURN n ORDER BY n.name ‸').keywords.includes('DESC'));
  assert.ok(at('MATCH (n) SET n.a = 1 ‸').keywords.includes('REMOVE'));
  assert.ok(at('MATCH (n) DETACH DELETE n ‸').keywords.includes('RETURN'));
  assert.ok(at('MERGE (n:person {name: "x"}) ‸').keywords.includes('ON CREATE SET'));
  // Inside a pattern only WHERE; inside a call's parentheses expression words.
  assert.deepEqual(at('MATCH (n ‸').keywords, ['WHERE']);
  assert.ok(at('MATCH (n) RETURN count(‸').keywords.includes('DISTINCT'));
  // A subquery body starts a query of its own.
  assert.equal(at('MATCH (p) WHERE EXISTS { ‸').keywords[0], 'MATCH');
  // A keyword typed in lower case still counts.
  assert.equal(at('match (n) return n.name as x ‸').clause, 'RETURN');
});

test('the variables the query binds, with their kind and labels', () => {
  const vars = gqlVariables("MATCH p = (a:person {name: 'marko'})-[e:knows|created]->(b) WITH b, count(*) AS n FOR x IN [1] LET y = 2 RETURN [z IN [1] | z] AS l");
  assert.deepEqual(vars.map((v) => [v.name, v.kind, v.labels]), [
    ['p', 'path', []], ['a', 'node', ['person']], ['e', 'edge', ['knows', 'created']], ['b', 'node', []],
    ['n', 'value', []], ['x', 'value', []], ['y', 'value', []], ['z', 'value', []], ['l', 'value', []],
  ]);
  // A function argument is no pattern element.
  assert.deepEqual(gqlVariables('MATCH (n) RETURN labels(n) AS l').map((v) => v.name), ['n', 'l']);
});

test('parameter choices, tokens and the keys of a schema', () => {
  assert.deepEqual(gqlParamChoices('RETURN $b, $a, $b', ['a', 'c', '']), ['a', 'c', 'b']);
  assert.equal(paramToken('min'), '$min');
  assert.equal(paramToken('a b'), '$`a b`');
  const keys = keysBySchema({
    vertices: { person: { schema: { properties: { name: {}, age: {} } } }, software: { schema: { properties: { name: {}, lang: {} } } } },
    edges: { knows: { schema: { properties: { weight: {} } } } },
  });
  assert.deepEqual(keysFor(keys, 'vertex', ['person']), ['age', 'name']);
  assert.deepEqual(keysFor(keys, 'vertex', []), ['age', 'lang', 'name']);
  assert.deepEqual(keysFor(keys, 'vertex', ['nobody']), ['age', 'lang', 'name']);
  assert.deepEqual(keysFor(keys, 'any', []), ['age', 'lang', 'name', 'weight']);
  assert.deepEqual(keysBySchema(null), { vertex: {}, edge: {} });
});

/** A CodeMirror completion context over `marked` (‸ = cursor), as far as the sources read it. */
function cmContext(marked, explicit = false) {
  const pos = marked.indexOf('‸');
  const text = marked.replace('‸', '');
  return {
    pos,
    explicit,
    state: { doc: { toString: () => text }, sliceDoc: (a, b) => text.slice(a, b) },
  };
}

const VOCAB = {
  vertexLabels: ['person', 'software', 'two words'],
  edgeLabels: ['created', 'knows'],
  keys: keysBySchema({ vertices: { person: { schema: { properties: { name: {}, age: {} } } } }, edges: {} }),
};

test('the CodeMirror source: labels, keys, functions, variables and parameters', () => {
  const source = gqlCompletionSource(() => VOCAB, () => [{ name: 'min', detail: 'Parameters field' }]);
  const labels = source(cmContext('MATCH (n:p‸'));
  assert.equal(labels.from, 9);
  assert.deepEqual(labels.options.map((o) => o.label), ['person', 'software', '`two words`']);
  assert.deepEqual(source(cmContext('MATCH ()-[:‸')).options.map((o) => o.label), ['created', 'knows']);
  assert.deepEqual(source(cmContext('MATCH (n:person) RETURN n.‸')).options.map((o) => o.label), ['age', 'name']);
  const params = source(cmContext('MATCH (n) WHERE n.age > $‸ AND n.x = $other'));
  assert.deepEqual(params.options.map((o) => [o.label, o.detail]), [['$min', 'Parameters field'], ['$other', 'read by the query']]);
  const words = source(cmContext('MATCH (n:person) RETURN up‸'));
  const names = words.options.map((o) => o.label);
  assert.ok(names.includes('upper') && names.includes('toUpper') && names.includes('n') && names.includes('ORDER BY'));
  // No popup on a bare position unless asked (Ctrl+Space).
  assert.equal(source(cmContext('MATCH (n) ‸')), null);
  assert.ok(source(cmContext('MATCH (n) ‸', true)).options.length > 0);
  // Without a vocabulary yet: no result (the next keystroke asks again), no failure.
  assert.equal(gqlCompletionSource(() => null)(cmContext('MATCH (n:‸')), null);
});

test('the CodeMirror source waits for the first load of the vocabulary', async () => {
  const source = gqlCompletionSource(() => Promise.resolve(VOCAB));
  const result = await source(cmContext('MATCH ()-[:‸'));
  assert.deepEqual(result.options.map((o) => o.label), ['created', 'knows']);
  // Keywords never wait.
  assert.ok(Array.isArray(source(cmContext('MATCH (n) RE‸')).options));
});

test('every GQL function of the engine is listed, and nothing else (parser.rs FUNCTIONS)', () => {
  const rust = readFileSync(fileURLToPath(new URL('../../crates/graphersal/src/gql/parser.rs', import.meta.url)), 'utf8');
  const table = rust.slice(rust.indexOf('pub(crate) const FUNCTIONS: &[FunctionSpec] = &['), rust.indexOf('pub(crate) struct UnsupportedFunction'));
  const engine = new Set();
  for (const m of table.matchAll(/&\[([^\]]*)\]/g)) for (const n of m[1].matchAll(/"([^"]+)"/g)) engine.add(n[1]);
  const listFn = rust.slice(rust.indexOf('pub(crate) fn list_function'), rust.indexOf('pub(crate) fn lookup_unsupported_function'));
  for (const m of listFn.matchAll(/\("([a-z]+)", /g)) engine.add(m[1]);
  assert.ok(engine.size > 60, `the table was read (${engine.size} names)`);
  const ours = gqlFunctionSpellings();
  assert.deepEqual([...engine].filter((n) => !ours.includes(n)), [], 'missing in js/editor/gql-functions.js');
  assert.deepEqual(ours.filter((n) => !engine.has(n)), [], 'not a function of the engine');
  assert.equal(new Set(ours).size, ours.length, 'each spelling once');
  for (const f of GQL_FUNCTIONS) assert.ok(f.doc && f.call.startsWith('('), f.name);
});
