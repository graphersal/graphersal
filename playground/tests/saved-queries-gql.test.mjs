// Node tests of saved queries written in GQL in the playground models (js/saved-queries.js):
// the language badge, GQL drafts (new, edited, to the spec), the Rhai / GQL switch, the detected
// `$parameters`, and "Save as query" from a query tab (`g.gql(..)` call parsed, its text dedented,
// its parameters typed by the tab's values).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  languageBadge, newDraft, draftFromQuery, draftToSpec, composeSource, switchDraftLanguage,
  withDetectedParams, parseGqlCall, draftFromScript, storedGqlText, newParam, GQL_DRAFT_BODY,
} from '../js/saved-queries.js';
import { wrapGql } from '../js/editor/gql-wrap.js';

const OLDER = {
  name: 'older', folder: 'people', description: 'Older than', language: 'gql', dialect: 'graphersal-gql/1',
  params: [{ name: 'min', schema: { type: 'integer' }, default: 30, required: false }],
  body: 'MATCH (p:person) WHERE p.age > $min\nRETURN p.name AS name', status: 'ok', writes: false,
};

test('the language badge names the language and its dialect', () => {
  assert.deepEqual(languageBadge(OLDER), { text: 'GQL', title: 'Written as one GQL query (dialect graphersal-gql/1)' });
  assert.equal(languageBadge({ dialect: 'graphersal-rhai/2' }).text, 'Rhai');
  assert.equal(languageBadge({ language: 'rhai', dialect: 'graphersal-rhai/1' }).title,
    'Written as a Rhai function (dialect graphersal-rhai/1)');
  assert.equal(languageBadge({ language: 'sparql', dialect: 'graphersal-sparql/1' }).text, 'sparql');
});

test('a GQL draft is edited and stored as written, with its language in the spec', () => {
  const draft = draftFromQuery(OLDER);
  assert.equal(draft.language, 'gql');
  assert.equal(draft.raw, false);
  assert.equal(draft.body, OLDER.body);
  assert.equal(composeSource(draft), OLDER.body);
  const { spec } = draftToSpec(draft);
  assert.deepEqual(spec, {
    name: 'older', folder: 'people', description: 'Older than', language: 'gql',
    params: [{ name: 'min', schema: { type: 'integer' }, default: 30 }],
    body: OLDER.body, replaces: 'older',
  });
  const fresh = newDraft('', 'gql');
  assert.equal(fresh.body, GQL_DRAFT_BODY);
  assert.equal(newDraft().language, 'rhai');
  // A Rhai spec carries no language (the engine's default).
  const rhai = newDraft();
  rhai.name = 'q';
  assert.equal('language' in draftToSpec(rhai).spec, false);
});

test('the Rhai / GQL switch converts the body both ways', () => {
  const rhai = { ...newDraft(), name: 'q' };
  const gql = switchDraftLanguage(rhai, 'gql');
  assert.equal(gql.language, 'gql');
  assert.equal(gql.body, GQL_DRAFT_BODY);
  // A body that is one g.gql(..) call gives its query.
  const wrapped = { ...newDraft(), name: 'q', body: wrapGql('MATCH (n)\nRETURN n') };
  assert.equal(switchDraftLanguage(wrapped, 'gql').body, 'MATCH (n)\nRETURN n');
  // Any other Rhai text is kept, to be rewritten.
  assert.equal(switchDraftLanguage({ ...rhai, body: 'g.v().count()' }, 'gql').body, 'g.v().count()');
  // GQL -> Rhai passes every declared parameter.
  const back = switchDraftLanguage({ ...draftFromQuery(OLDER) }, 'rhai');
  assert.equal(back.language, 'rhai');
  assert.equal(back.body, 'g.gql(`\n  MATCH (p:person) WHERE p.age > $min\n  RETURN p.name AS name\n`, #{min: min})');
  assert.equal(switchDraftLanguage(gql, 'gql'), gql);
});

test('the $parameters of the query are added once, in their order', () => {
  const draft = { ...newDraft('', 'gql'), body: 'MATCH (p) WHERE p.age > $min AND p.name = $name RETURN p LIMIT $min' };
  draft.params = [newParam('name')];
  assert.deepEqual(withDetectedParams(draft).map((p) => p.name), ['name', 'min']);
});

test('parseGqlCall reads exactly one g.gql(..) call', () => {
  assert.deepEqual(parseGqlCall('g.gql("MATCH (n) RETURN n")'), { query: 'MATCH (n) RETURN n', args: {} });
  assert.deepEqual(parseGqlCall(' // first\n g.gql(`RETURN ``x`` AS y`); '), { query: 'RETURN `x` AS y', args: {} });
  assert.deepEqual(parseGqlCall('g.gql("a\\"b\\n\\u0041", #{min: 30, ratio: 2.5, name: "x", on: true, none: ()})'), {
    query: 'a"b\nA',
    args: { min: { number: 30, integer: true }, ratio: { number: 2.5, integer: false }, name: 'x', on: true, none: null },
  });
  // A map of computed values: the query, without values.
  assert.deepEqual(parseGqlCall('g.gql("RETURN $a AS a", #{a: x + 1})'), { query: 'RETURN $a AS a', args: {} });
  for (const other of [
    'g.gql("RETURN 1 AS x").to_list()', 'g.v().count()', 'g.gql(text)', 'g.gql(`${x}`)', 'g.gql("a"',
    'let q = g.gql("RETURN 1 AS x"); q', 'g.gql("RETURN 1", 5)', '',
  ]) {
    assert.equal(parseGqlCall(other), null, other);
  }
});

test('Save as query: a g.gql tab gives a GQL draft with its typed $parameters', () => {
  const script = 'g.gql(`\n  MATCH (p:person)\n  WHERE p.age > $min AND p.name <> $name\n  RETURN p.name AS name\n`, #{min: 30, name: "vadas"})';
  const draft = draftFromScript(script);
  assert.equal(draft.language, 'gql');
  assert.equal(draft.body, 'MATCH (p:person)\nWHERE p.age > $min AND p.name <> $name\nRETURN p.name AS name');
  assert.deepEqual(draft.params.map((p) => [p.name, p.type, p.defaultText]), [['min', 'integer', '30'], ['name', 'string', 'vadas']]);
  assert.equal(draft.replaces, null);
  const { spec } = draftToSpec({ ...draft, name: 'older' });
  assert.equal(spec.language, 'gql');
  assert.deepEqual(spec.params, [
    { name: 'min', schema: { type: 'integer' }, default: 30 },
    { name: 'name', schema: { type: 'string' }, default: 'vadas' },
  ]);
  // A $name the tab passes no value for is a required string; a value it does not read is left out.
  const loose = draftFromScript('g.gql("RETURN $x AS x, $f AS f, $b AS b", #{f: 1.5, b: false, unused: 1})');
  assert.deepEqual(loose.params.map((p) => [p.name, p.type, p.defaultText]), [
    ['x', 'string', ''], ['f', 'number', '1.5'], ['b', 'boolean', 'false'],
  ]);
  // Any other tab is a Rhai draft of its text.
  const rhai = draftFromScript('  g.v().has_label("person").values("name")\n');
  assert.equal(rhai.language, 'rhai');
  assert.equal(rhai.body, 'g.v().has_label("person").values("name")');
  assert.deepEqual(rhai.params, []);
});

test('storedGqlText undoes the layout wrapGql writes', () => {
  for (const query of ['MATCH (n)\nRETURN n', 'RETURN 1 AS x', 'MATCH (a)\n  WHERE a.x > 1\nRETURN a']) {
    assert.equal(storedGqlText(parseGqlCall(wrapGql(query)).query), query);
  }
});
