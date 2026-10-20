// Node tests of the GQL tokenizer (js/editor/gql-tokens.js): the token types the GQL
// highlighting of the saved-query editor shows, the engine's `--` rule, comments and strings
// across lines, and the `$name` parameters "Save as query" declares.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { gqlTokens, gqlParamNames } from '../js/editor/gql-tokens.js';

const types = (text) => gqlTokens(text).map((t) => [t.text, t.type]);

test('keywords, labels, properties, parameters, literals and functions', () => {
  assert.deepEqual(types('match (p:person|Admin) WHERE p.age > $min RETURN count(p) AS n, true'), [
    ['match', 'keyword'], ['(', 'punctuation'], ['p', 'variable'], [':', 'punctuation'], ['person', 'label'],
    ['|', 'operator'], ['Admin', 'label'], [')', 'punctuation'], ['WHERE', 'keyword'], ['p', 'variable'],
    ['.', 'punctuation'], ['age', 'property'], ['>', 'operator'], ['$min', 'param'], ['RETURN', 'keyword'],
    ['count', 'function'], ['(', 'punctuation'], ['p', 'variable'], [')', 'punctuation'], ['AS', 'keyword'],
    ['n', 'variable'], [',', 'punctuation'], ['true', 'atom'],
  ]);
  assert.deepEqual(types("{name: 'it''s', n: 1.5e3, h: 0x1F, b: `odd name`}").filter(([, t]) => t !== 'punctuation'), [
    ['name', 'variable'], ["'it''s'", 'string'], ['n', 'variable'], ['1.5e3', 'number'], ['h', 'variable'],
    ['0x1F', 'number'], ['b', 'variable'], ['`odd name`', 'name'],
  ]);
  // A keyword written where a name stands after `.` or `:` is the name.
  assert.deepEqual(types('n.order')[2], ['order', 'property']);
  assert.deepEqual(types('(n:Match)')[3], ['Match', 'label']);
  assert.deepEqual(types('(n:L {k: v})').map(([, t]) => t).filter((t) => t !== 'punctuation'), ['variable', 'label', 'variable', 'variable']);
});

test('comments: //, /* */ across lines, and -- unless it is an edge', () => {
  assert.deepEqual(types('RETURN 1 // note'), [['RETURN', 'keyword'], ['1', 'number'], ['// note', 'comment']]);
  assert.deepEqual(types('RETURN /* a\nb */ 1'), [
    ['RETURN', 'keyword'], ['/* a', 'comment'], ['b */', 'comment'], ['1', 'number'],
  ]);
  assert.deepEqual(types('-- line\nRETURN x -- the name'), [
    ['-- line', 'comment'], ['RETURN', 'keyword'], ['x', 'variable'], ['-- the name', 'comment'],
  ]);
  for (const edge of ['(a)--(b)', '(a) -- (b)', '(a)-->(b)', 'MATCH --(b)']) {
    assert.ok(!gqlTokens(edge).some((t) => t.type === 'comment'), edge);
  }
  // The `)` rule is per line: a `--` at the start of the next line is a comment.
  assert.equal(gqlTokens('(a)\n-- note').pop().type, 'comment');
});

test('strings run across lines and keep $ text out of the parameters', () => {
  assert.deepEqual(types("RETURN 'a\nb' AS s"), [
    ['RETURN', 'keyword'], ["'a", 'string'], ["b'", 'string'], ['AS', 'keyword'], ['s', 'variable'],
  ]);
  assert.deepEqual(
    gqlParamNames("MATCH (p) WHERE p.age > $min AND p.name <> '$not' // $neither\nRETURN p LIMIT $n OFFSET $min"),
    ['min', 'n'],
  );
  assert.deepEqual(gqlParamNames('RETURN $`odd name` AS x, $0 AS y, "a \\" $no" AS z'), ['odd name', '0']);
  assert.deepEqual(gqlParamNames('/* $a */ RETURN 1'), []);
});

test('never fails: unterminated text and stray characters still give tokens', () => {
  for (const text of ["RETURN 'open", 'RETURN `open', 'RETURN /* open', '$', '@#~', '']) {
    assert.ok(Array.isArray(gqlTokens(text)), text);
  }
});
