// The Unique box of the Property indexes form (js/indexes.js): the spec sends `unique: true`
// only when checked, the box is not allowed with `each` (the engine refuses it), and a listed
// entry says whether it is unique.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  indexDraftToSpec, isUniqueIndex, newTransformIndexDraft, uniqueAllowed,
} from '../js/indexes.js';

test('unique is sent only when checked', () => {
  const draft = { ...newTransformIndexDraft(), label: 'User', key: 'email' };
  assert.equal(indexDraftToSpec(draft).spec.unique, undefined);
  assert.equal(indexDraftToSpec({ ...draft, unique: false }).spec.unique, undefined);
  assert.deepEqual(indexDraftToSpec({ ...draft, unique: true }).spec, {
    element: 'vertex', label: 'User', key: 'email', type: 'memory-ordered', unique: true,
  });
  assert.deepEqual(
    indexDraftToSpec({ ...draft, unique: true, transform: 'to_lower' }).spec,
    { element: 'vertex', label: 'User', key: 'email', type: 'memory-ordered', transform: 'to_lower', unique: true },
  );
});

test('unique is not allowed with each', () => {
  assert.equal(uniqueAllowed(''), true);
  assert.equal(uniqueAllowed('to_lower'), true);
  assert.equal(uniqueAllowed('each'), false);
  assert.equal(uniqueAllowed('each to_lower'), false);
  const draft = { ...newTransformIndexDraft(), label: 'User', key: 'tags', transform: 'each', unique: true };
  const { errors } = indexDraftToSpec(draft);
  assert.equal(errors.length, 1);
  assert.equal(errors[0].field, 'unique');
});

test('a listed entry says whether it is unique', () => {
  assert.equal(isUniqueIndex({ name: 'a', unique: true }), true);
  assert.equal(isUniqueIndex({ name: 'a', unique: false }), false);
  assert.equal(isUniqueIndex({ name: 'a' }), false);
  assert.equal(isUniqueIndex(null), false);
});
