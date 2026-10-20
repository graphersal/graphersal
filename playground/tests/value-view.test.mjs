// Node tests of the collapsed long values in the element views (js/value-view.js): vectors (lists
// of numbers, how the inspector receives them), long and multi-line texts, the setting
// `valueMaxChars` (default, range, 0 = off).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  VALUE_VIEW_TUNING, VALUE_MAX_CHARS, maxCharsError, normalizeMaxChars, isNumericList, shortNumber, vectorText,
  fullText, cutText, valueView,
} from '../js/value-view.js';

const vec = (n) => Array.from({ length: n }, (_, i) => (i % 2 ? -0.16903085 : 0.123456) * (i + 1));

test('the setting valueMaxChars: default 200, 0 is off, a range otherwise', () => {
  assert.equal(VALUE_MAX_CHARS.default, 200);
  for (const ok of [0, 20, 200, '500', 100000]) assert.equal(maxCharsError(ok), null, String(ok));
  for (const bad of [-1, 1, 19, 100001, 2.5, '', 'x', null, undefined, NaN]) assert.ok(maxCharsError(bad), String(bad));
  assert.equal(normalizeMaxChars('300'), 300);
  assert.equal(normalizeMaxChars(0), 0);
  assert.equal(normalizeMaxChars(undefined), 200);
  assert.equal(normalizeMaxChars(5), 200);
});

test('a long list of numbers is a vector; short lists and mixed lists are not', () => {
  assert.ok(isNumericList(vec(VALUE_VIEW_TUNING.vectorMin)));
  assert.ok(!isNumericList(vec(VALUE_VIEW_TUNING.vectorMin - 1)));
  assert.ok(!isNumericList([...vec(20), 'x']));
  assert.ok(!isNumericList([...vec(20), null]));
  assert.ok(!isNumericList('[1, 2, 3]'));
  assert.ok(!isNumericList({ length: 20 }));
});

test('a vector collapses to its dimension and first components', () => {
  assert.equal(shortNumber(-0.16903085), '-0.169');
  assert.equal(shortNumber(0), '0');
  assert.equal(shortNumber(12), '12');
  assert.equal(shortNumber(1234.5678), '1230');
  const v = [0.12, -0.03, 0.5, 1, ...new Array(380).fill(0)];
  assert.equal(vectorText(v), 'vector[384] [0.12, -0.03, 0.5, 1, …]');
  const view = valueView(v);
  assert.equal(view.kind, 'vector');
  assert.equal(view.short, 'vector[384] [0.12, -0.03, 0.5, 1, …]');
  // The full value keeps every component unrounded.
  assert.equal(view.full, `[${v.join(', ')}]`);
  assert.deepEqual(JSON.parse(view.full), v);
  // Even a vector whose text is short is collapsed (its length is what matters).
  assert.ok(valueView(new Array(16).fill(0)).short);
});

test('a vector in a one-item list (a value_map() row) collapses too', () => {
  const v = [0.5, ...new Array(19).fill(0)];
  const view = valueView([v]);
  assert.equal(view.kind, 'vector');
  assert.equal(view.short, '[vector[20] [0.5, 0, 0, 0, …]]');
  assert.deepEqual(JSON.parse(view.full), [v]);
  assert.equal(valueView([v], { maxChars: 0 }).short, null);
  assert.equal(valueView([v, v]).kind, 'text');
});

test('a long text collapses to maxChars characters and "…"', () => {
  const text = 'x'.repeat(250);
  const view = valueView(text);
  assert.equal(view.kind, 'text');
  assert.equal(view.full, text);
  assert.equal(view.short, `${'x'.repeat(200)}…`);
  assert.equal(valueView('x'.repeat(200)).short, null, 'exactly the limit is not cut');
  assert.equal(valueView(text, { maxChars: 50 }).short, `${'x'.repeat(50)}…`);
  // Trailing spaces before the cut are dropped.
  assert.equal(cutText(`${'a'.repeat(9)} ${'b'.repeat(20)}`, 10), `${'a'.repeat(9)}…`);
  // A surrogate pair is never split.
  assert.equal(cutText(`${'a'.repeat(9)}😀abc`, 10), `${'a'.repeat(9)}…`);
});

test('a text with more than three lines collapses to three', () => {
  const view = valueView('one\ntwo\nthree\nfour\nfive');
  assert.equal(view.short, 'one\ntwo\nthree…');
  assert.equal(view.full, 'one\ntwo\nthree\nfour\nfive');
  assert.equal(valueView('one\ntwo\nthree').short, null);
});

test('maps and lists collapse by the length of their JSON; short values stay as they are', () => {
  const big = { text: 'y'.repeat(300) };
  const view = valueView(big);
  assert.equal(view.full, JSON.stringify(big));
  assert.ok(view.short.endsWith('…'));
  assert.equal(valueView(29).full, '29');
  assert.equal(valueView(29).short, null);
  assert.equal(valueView(null).full, 'null');
  assert.equal(valueView(true).full, 'true');
  assert.equal(valueView('marko').short, null);
  assert.equal(fullText([1, 2]), '[1, 2]');
  assert.equal(fullText(['a', 1]), '["a",1]');
});

test('valueMaxChars 0 shows every value in full, vectors too', () => {
  assert.equal(valueView('x'.repeat(5000), { maxChars: 0 }).short, null);
  assert.equal(valueView('a\nb\nc\nd\ne', { maxChars: 0 }).short, null);
  const v = valueView(vec(384), { maxChars: 0 });
  assert.equal(v.short, null);
  assert.equal(v.kind, 'vector');
  // An invalid setting falls back to the default.
  assert.equal(valueView('x'.repeat(250), { maxChars: 'bad' }).short, `${'x'.repeat(200)}…`);
});
