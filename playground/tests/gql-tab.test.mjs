// Node tests of GQL query tabs: the language model and rewrites (js/gql-tab.js), the tab state
// (js/tabs.js `language` / `params`), the link format (js/link.js `lang=gql`) and "Save as
// query" from a GQL tab (js/saved-queries.js `draftFromGql`).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  parseParams, paramsErrorText, gqlCallText, rhaiLiteral, switchTabLanguage, withQueryParams, paramArgs,
  gqlErrorSpan, spanRange, tabLanguage, GQL_TAB_TEMPLATE,
} from '../js/gql-tab.js';
import * as tabs from '../js/tabs.js';
import { parseLink, linkHash } from '../js/link.js';
import { draftFromGql, draftToSpec } from '../js/saved-queries.js';

test('the Parameters field: blank is the empty map, a JSON map gives its names in order', () => {
  assert.deepEqual(parseParams('  '), { ok: true, entries: [], names: [] });
  const p = parseParams('{"min": 30, "names": ["a", "b"], "f": 1.0}');
  assert.equal(p.ok, true);
  assert.deepEqual(p.names, ['min', 'names', 'f']);
  assert.equal(p.entries[0][1].kind, 'int');
  assert.equal(p.entries[2][1].kind, 'float');
});

test('the Parameters field: faults name their line and column', () => {
  const bad = parseParams('{\n  "min": 30,\n  name: 1\n}');
  assert.equal(bad.ok, false);
  assert.equal(bad.line, 3);
  assert.equal(bad.column, 3);
  assert.match(paramsErrorText(bad), /^Parameters, line 3, column 3: a key is a string/);
  assert.match(parseParams("{'a': 1}").message, /double quotes/);
  assert.match(parseParams('[1, 2]').message, /not a list/);
  assert.match(parseParams('{"a": 1} x').message, /after the value/);
  assert.match(parseParams('{"a": tru}').message, /unexpected word "tru"/);
  assert.match(parseParams('{"a": ').message, /ends where a value/);
  assert.match(parseParams('{"a": [' + '['.repeat(200)).message, /nest deeper/);
});

test('the run script is the engine\'s gql_call_text: integers stay integers, floats floats', () => {
  assert.deepEqual(gqlCallText('MATCH (n) RETURN n'), { script: 'g.gql("MATCH (n) RETURN n")' });
  assert.deepEqual(gqlCallText('RETURN $a', '{}'), { script: 'g.gql("RETURN $a")' });
  assert.equal(
    gqlCallText("MATCH (n:person) WHERE n.age > $min\nRETURN n.name AS \"x\"", '{"min": 30, "f": 30.0, "e": 1e20, "s": 1.5e-7, "n": null, "b": true, "l": ["a\\"b", 2], "m": {"k k": 1}}').script,
    'g.gql("MATCH (n:person) WHERE n.age > $min\\nRETURN n.name AS \\"x\\"", #{min: 30, f: 30.0, e: 1e20, s: 1.5e-7, n: (), b: true, l: ["a\\"b", 2], m: #{"k k": 1}})',
  );
  assert.equal(rhaiLiteral({ kind: 'int', text: '99999999999999999999', value: 1e20 }), '1e20');
  assert.equal(rhaiLiteral({ kind: 'string', value: 'a\u0001' }), '"a\\u0001"');
  assert.match(gqlCallText('RETURN 1', '{"a":').error, /^Parameters, line 1/);
});

test('switching Rhai -> GQL takes the query and its literal parameters out of one g.gql call', () => {
  const r = switchTabLanguage({ script: 'g.gql(`\n  MATCH (p:person)\n  WHERE p.age > $min\n  RETURN p.name\n`, #{min: 30, who: "x"})' }, 'gql');
  assert.equal(r.script, 'MATCH (p:person)\nWHERE p.age > $min\nRETURN p.name');
  assert.equal(r.params, '{\n  "min": 30,\n  "who": "x"\n}');
  // Any other text is kept (to be rewritten), an empty tab gets the template.
  assert.deepEqual(switchTabLanguage({ script: 'g.v().count()' }, 'gql'), { script: 'g.v().count()', params: '' });
  assert.deepEqual(switchTabLanguage({ script: '  ' }, 'gql'), { script: GQL_TAB_TEMPLATE, params: '' });
  // A continued traversal is no plain call: kept.
  assert.equal(switchTabLanguage({ script: 'g.gql("MATCH (n) RETURN n").count()' }, 'gql').script, 'g.gql("MATCH (n) RETURN n").count()');
});

test('switching GQL -> Rhai wraps the query with its parameters; a broken field refuses', () => {
  assert.deepEqual(
    switchTabLanguage({ script: 'MATCH (p:person)\nWHERE p.age > $min\nRETURN p.name', params: '{"min": 30}' }, 'rhai'),
    { script: 'g.gql(`\n  MATCH (p:person)\n  WHERE p.age > $min\n  RETURN p.name\n`, #{min: 30})', params: '' },
  );
  assert.deepEqual(switchTabLanguage({ script: 'RETURN 1', params: '' }, 'rhai'), { script: 'g.gql(`RETURN 1`)', params: '' });
  assert.match(switchTabLanguage({ script: 'RETURN $a', params: '{a: 1}' }, 'rhai').error, /^Parameters, line 1, column 2/);
  // There and back gives the same tab.
  const gql = { script: "MATCH (n {name: 'x'}) RETURN n.name AS `the name`", params: '{\n  "k": [1, 2.5]\n}' };
  const back = switchTabLanguage(switchTabLanguage(gql, 'rhai'), 'gql');
  assert.equal(back.script, gql.script);
  // A list is no literal parseGqlCall reads: the field comes back empty, the map stays in the Rhai text.
  assert.equal(switchTabLanguage({ script: 'RETURN $a', params: '{"a": 1}' }, 'rhai').script, 'g.gql(`RETURN $a`, #{a: 1})');
});

test('"+ Add $parameters" adds the missing $names as null, keeping the values written', () => {
  assert.deepEqual(withQueryParams('', 'MATCH (n) WHERE n.a = $a AND n.b IN $b RETURN n'), {
    text: '{\n  "a": null,\n  "b": null\n}', added: ['a', 'b'],
  });
  assert.deepEqual(withQueryParams('{"a": 1.50}', 'RETURN $a, $b'), { text: '{\n  "a": 1.50,\n  "b": null\n}', added: ['b'] });
  assert.deepEqual(withQueryParams('{"a": 1}', 'RETURN $a'), { text: '{"a": 1}', added: [] });
  assert.match(withQueryParams('{', 'RETURN $a').error, /Parameters/);
});

test('Save as query from a GQL tab: a GQL draft with its $parameters typed by the field', () => {
  const draft = draftFromGql('MATCH (n:person) WHERE n.age > $min AND n.name <> $not RETURN n.name AS name', paramArgs('{"min": 30, "not": "x", "l": [1]}'));
  assert.equal(draft.language, 'gql');
  assert.deepEqual(draft.params.map((p) => [p.name, p.type, p.defaultText]), [['min', 'integer', '30'], ['not', 'string', 'x']]);
  const { spec } = draftToSpec({ ...draft, name: 'older' });
  assert.equal(spec.language, 'gql');
  assert.equal(spec.body, 'MATCH (n:person) WHERE n.age > $min AND n.name <> $not RETURN n.name AS name');
});

test('the span of a GQL error: line, column, width and GQLSTATUS from the diagnostic', () => {
  const error = {
    message: "GQL syntax error at line 2, column 21: unknown function foo() (step #1 'gql:error(\"42001\")' refused the query before the traversal ran)",
    diagnostic: 'Error: GQL syntax error at line 2, column 21: unknown function foo()\n  at #1: v().gql:error("42001")\n             ^^^^^^^^^^^^^^^^^^\n  line 2:       RETURN n.name AS x, foo(n) AS y\n                                    ^^^\n  did you mean: floor()\n  GQLSTATUS:    42001',
    position: { line: 1, column: 1 },
  };
  assert.deepEqual(gqlErrorSpan(error), { line: 2, column: 21, width: 3, gqlstatus: '42001' });
  assert.equal(gqlErrorSpan({ message: 'Function not found: foo' }), null);
  assert.equal(gqlErrorSpan(null), null);
});

test('a span maps into the editor text, moved by where the text that ran starts', () => {
  const doc = 'MATCH (n:person)\nRETURN n.name AS x, foo(n) AS y';
  assert.deepEqual(spanRange(doc, { line: 2, column: 21, width: 3 }), { from: 37, to: 40 });
  assert.equal(doc.slice(37, 40), 'foo');
  // A selection starting on line 3, column 5: line 1 of the span moves by the column too.
  const text = '\n\n    RETURN fo(1)';
  const r = spanRange(text, { line: 1, column: 8, width: 2 }, { line: 3, column: 5 });
  assert.equal(text.slice(r.from, r.to), 'fo');
  // Characters, not UTF-16 units: an emoji before the span.
  const emoji = "RETURN '😀', bar(1)";
  const e = spanRange(emoji, { line: 1, column: 13, width: 3 });
  assert.equal(emoji.slice(e.from, e.to), 'bar');
  assert.equal(spanRange('x', { line: 5, column: 1, width: 1 }), null);
});

test('tabs: a GQL tab keeps its language and parameters through save and restore', () => {
  const ts = tabs.restoreTabs(null);
  const tab = tabs.addTab(ts, { language: 'gql', script: GQL_TAB_TEMPLATE });
  assert.equal(tab.language, 'gql');
  tabs.setParams(ts, tab.id, '{"a": 1}');
  const saved = JSON.parse(JSON.stringify(tabs.serializeTabs(ts)));
  // A Rhai tab writes neither field (older playgrounds read the tabs as before).
  assert.equal('language' in saved.tabs[0], false);
  assert.equal(saved.tabs[1].language, 'gql');
  const back = tabs.restoreTabs(saved);
  assert.equal(back.tabs[1].language, 'gql');
  assert.equal(back.tabs[1].params, '{"a": 1}');
  assert.equal(back.tabs[0].language, 'rhai');
  // Duplicates keep the language; a Rhai tab has no parameters.
  const copy = tabs.duplicateTab(back, back.tabs[1].id);
  assert.equal(copy.language, 'gql');
  assert.equal(copy.params, '{"a": 1}');
  tabs.setParams(back, back.tabs[0].id, '{"x": 1}');
  assert.equal(back.tabs[0].params, '');
  assert.equal(tabLanguage('python'), 'rhai');
});

test('tabs: switching the language rewrites the tab; an example opens in its language', () => {
  const ts = tabs.restoreTabs(null);
  const id = ts.tabs[0].id;
  tabs.setLanguage(ts, id, 'gql', { script: 'RETURN 1', params: '{}' });
  assert.equal(ts.tabs[0].language, 'gql');
  assert.equal(ts.tabs[0].script, 'RETURN 1');
  tabs.setScript(ts, id, '');
  // The emptied GQL tab takes a Rhai example as Rhai.
  const { tab, reused } = tabs.openExample(ts, { title: 'Count', script: 'g.v().count()' });
  assert.equal(reused, true);
  assert.equal(tab.language, 'rhai');
  assert.equal(tab.params, '');
  // The unmodified example is replaced by a GQL example, in GQL with its parameters.
  const gql = tabs.openExample(ts, { title: 'Older', script: 'MATCH (n) WHERE n.age > $min RETURN n', language: 'gql', params: '{"min": 30}' });
  assert.equal(gql.reused, true);
  assert.equal(gql.tab.language, 'gql');
  assert.equal(gql.tab.params, '{"min": 30}');
  // A tab with user text: a new tab.
  tabs.setScript(ts, id, 'MATCH (x) RETURN x');
  const other = tabs.openExample(ts, { title: 'Count', script: 'g.v().count()' });
  assert.equal(other.reused, false);
  assert.equal(other.tab.language, 'rhai');
});

test('links: lang=gql opens a GQL tab with its parameters; old links read as before', () => {
  const hash = linkHash({ graph: 'modern', script: 'MATCH (n) WHERE n.age > $min RETURN n.name', run: true, language: 'gql', params: '{"min": 30}' });
  assert.equal(hash, '#/play?graph=modern&q=MATCH%20(n)%20WHERE%20n.age%20%3E%20%24min%20RETURN%20n.name&lang=gql&params=%7B%22min%22%3A%2030%7D&run=1');
  assert.deepEqual(parseLink(hash), {
    graph: 'modern', data: null, schema: null, script: 'MATCH (n) WHERE n.age > $min RETURN n.name', title: '', run: true, language: 'gql', params: '{"min": 30}',
  });
  // A Rhai link writes no lang and keeps its old shape.
  assert.equal(linkHash({ graph: 'modern', script: 'g.v()' }), '#/play?graph=modern&q=g.v()');
  assert.equal('language' in parseLink('#/play?graph=modern&q=g.v()'), false);
  assert.equal('language' in parseLink('#/play?graph=modern&q=g.v()&lang=rhai'), false);
  assert.match(parseLink('#/play?graph=modern&q=x&lang=sql').error, /language "sql" is unknown/);
  // Blank parameters are not written.
  assert.equal(linkHash({ script: 'RETURN 1', language: 'gql', params: ' ' }), '#/play?graph=modern&q=RETURN%201&lang=gql');
});
