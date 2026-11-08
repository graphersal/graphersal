// The Transform choice of the Property indexes form (js/indexes.js): the known transforms, the
// default name's `_to_lower` suffix (the engine's `PropertyIndexDefinition::default_name`), the
// draft and the spec it sends, the transform of a listed entry.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  INDEX_TRANSFORMS, defaultIndexName, indexDraftToSpec, indexTransformOf, newTransformIndexDraft,
  transformSpecOf,
} from '../js/indexes.js';

test('the transforms offered: none first, then to_lower with a query hint', () => {
  assert.deepEqual(INDEX_TRANSFORMS.map((t) => t.name), ['', 'to_lower', 'each', 'each to_lower']);
  assert.equal(INDEX_TRANSFORMS[0].label, 'none');
  assert.match(INDEX_TRANSFORMS[1].description, /has\(key, __\.to_lower\(\)\.is\(\.\.\)\)/u);
});

test('the default name gets the transform as a suffix, after _edge', () => {
  assert.equal(defaultIndexName('vertex', 'person', 'name', 'to_lower'), 'person_name_to_lower');
  assert.equal(defaultIndexName('edge', 'knows', 'note', 'to_lower'), 'knows_note_edge_to_lower');
  assert.equal(defaultIndexName('vertex', 'person', 'name', ''), 'person_name');
  assert.equal(defaultIndexName('vertex', 'person', 'name', undefined), 'person_name');
  assert.equal(defaultIndexName('vertex', '2nd', 'e-mail', 'to_lower'), 'index_2nd_e_mail_to_lower');
});

test('the draft starts without a transform and sends one only when chosen', () => {
  const draft = newTransformIndexDraft();
  assert.deepEqual(draft, {
    name: '', element: 'vertex', label: '', key: '', type: 'memory-ordered', transform: '',
  });
  assert.deepEqual(indexDraftToSpec({ ...draft, label: 'person', key: 'name' }).spec, {
    element: 'vertex', label: 'person', key: 'name', type: 'memory-ordered',
  });
  assert.deepEqual(
    indexDraftToSpec({ ...draft, label: 'person', key: 'name', transform: 'to_lower' }).spec,
    { element: 'vertex', label: 'person', key: 'name', type: 'memory-ordered', transform: 'to_lower' },
  );
  // A typed name wins over the suggestion (the engine's default is used only without one).
  assert.equal(
    indexDraftToSpec({ ...draft, label: 'person', key: 'name', transform: 'to_lower', name: 'ci' }).spec.name,
    'ci',
  );
});

test('the transform of a listed entry', () => {
  assert.equal(indexTransformOf({ name: 'a', transform: 'to_lower' }), 'to_lower');
  assert.equal(indexTransformOf({ name: 'a', transform: null }), '');
  assert.equal(indexTransformOf({ name: 'a' }), '');
  assert.equal(indexTransformOf(null), '');
});

test('each and the chain each, to_lower: offered with a query hint, sent as a name or an array', () => {
  const each = INDEX_TRANSFORMS.find((t) => t.name === 'each');
  assert.match(each.description, /has\(key, __\.unfold\(\)\.is\(\.\.\)\)/u);
  const chain = INDEX_TRANSFORMS.find((t) => t.name === 'each to_lower');
  assert.match(chain.description, /__\.unfold\(\)\.to_lower\(\)\.is/u);
  assert.equal(transformSpecOf('each'), 'each');
  assert.deepEqual(transformSpecOf('each to_lower'), ['each', 'to_lower']);
  assert.equal(transformSpecOf(''), undefined);
  const draft = { ...newTransformIndexDraft(), key: '_owners', anyLabel: true, transform: 'each to_lower' };
  assert.deepEqual(indexDraftToSpec(draft).spec, {
    element: 'vertex', label: null, key: '_owners', type: 'memory-ordered', transform: ['each', 'to_lower'],
  });
  // The default name joins a chain's names with `_`, like the engine.
  assert.equal(defaultIndexName('vertex', null, '_owners', 'each'), 'index__owners_each');
  assert.equal(defaultIndexName('vertex', 'doc', 'tags', 'each to_lower'), 'doc_tags_each_to_lower');
  // A listed chain (an array) shows space-separated.
  assert.equal(indexTransformOf({ transform: ['each', 'to_lower'] }), 'each to_lower');
  assert.equal(indexTransformOf({ transform: 'each' }), 'each');
});
