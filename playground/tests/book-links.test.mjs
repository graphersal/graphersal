import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { parseLink } from '../js/link.js';

// book/playground-links.js is a plain browser script; under Node it only exports `gqlOf`.
const { gqlOf } = createRequire(import.meta.url)('../../book/playground-links.js');

test('a single g.gql call with a string literal gives its GQL text', () => {
  assert.equal(gqlOf('g.gql("MATCH (n) RETURN n")'), 'MATCH (n) RETURN n');
  assert.equal(gqlOf('g.gql("MATCH (n) RETURN n").to_list()   // all nodes'), 'MATCH (n) RETURN n');
  assert.equal(gqlOf('g.gql("RETURN \\"a\\" AS x");'), 'RETURN "a" AS x');
  assert.equal(gqlOf('// note\ng.gql(`MATCH (n)\n  RETURN n`)'.replace('// note\n', '')), 'MATCH (n)\n  RETURN n');
});

test('anything else stays a Rhai script', () => {
  assert.equal(gqlOf('g.v().count()'), null);
  assert.equal(gqlOf('g.gql("RETURN 1").count()'), null);
  assert.equal(gqlOf('g.gql("RETURN $a", #{a: 1})'), null);
  assert.equal(gqlOf('g.gql(q)'), null);
  assert.equal(gqlOf('g.gql(`RETURN ${x}`)'), null);
  assert.equal(gqlOf('g.gql("RETURN 1"); g.v()'), null);
  assert.equal(gqlOf('g.gql("")'), null);
});

test('the playground reads the link the book writes', () => {
  const q = gqlOf('g.gql("MATCH (n) WHERE n.a = 1 RETURN n")');
  const hash = `#/play?graph=modern&q=${encodeURIComponent(q)}&lang=gql&run=1`;
  const link = parseLink(hash);
  assert.equal(link.language, 'gql');
  assert.equal(link.script, q);
  assert.equal(link.run, true);
});
