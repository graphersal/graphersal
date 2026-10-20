// Node tests of the query editor's "Wrap as GQL" action (js/editor/gql-wrap.js): a GQL query
// becomes the Rhai script g.gql(`...`) that runs it.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { wrapGql, rhaiStringOf, isGqlCall, GQL_TEMPLATE } from '../js/editor/gql-wrap.js';

test('a one-line query becomes a one-line call', () => {
  assert.equal(wrapGql('  MATCH (p:person) RETURN p.name  '), 'g.gql(`MATCH (p:person) RETURN p.name`)');
});

test('a multi-line query keeps its lines, indented, without the common indentation', () => {
  const text = '\n    MATCH (p:person)\n      WHERE p.age > 30\n    RETURN p.name AS name\n\n';
  assert.equal(wrapGql(text), 'g.gql(`\n  MATCH (p:person)\n    WHERE p.age > 30\n  RETURN p.name AS name\n`)');
});

test('back-quoted GQL identifiers are written doubled', () => {
  assert.equal(wrapGql('MATCH (p) RETURN p.name AS `the name`'), 'g.gql(`MATCH (p) RETURN p.name AS ``the name```)');
  assert.equal(
    wrapGql('MATCH (p)\nRETURN p.name AS `n`'),
    'g.gql(`\n  MATCH (p)\n  RETURN p.name AS ``n``\n`)',
  );
});

test('a query holding ${ becomes a double-quoted string', () => {
  assert.equal(rhaiStringOf("WHERE p.x = '${a}'"), "\"WHERE p.x = '${a}'\"");
  assert.equal(
    wrapGql('MATCH (p)\nWHERE p.x = "${a}\\"\nRETURN p'),
    'g.gql("MATCH (p)\\nWHERE p.x = \\"${a}\\\\\\"\\nRETURN p")',
  );
});

test('an existing g.gql call is left alone; empty text gives the template', () => {
  const call = '  g.gql("MATCH (p) RETURN p").count()';
  assert.equal(isGqlCall(call), true);
  assert.equal(wrapGql(call), call);
  assert.equal(isGqlCall('g.v().count()'), false);
  assert.equal(wrapGql('  \n '), wrapGql(GQL_TEMPLATE));
  assert.ok(wrapGql('').startsWith('g.gql(`\n  MATCH (p:person)'));
});
