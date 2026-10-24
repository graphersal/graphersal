// Node tests of the date and time types in the playground's editors: the property editor's model
// (js/element-editor.js), the saved-query parameter model (js/saved-queries.js) and the schema
// editor's model (js/schema/draft.js). The engine's typed form of a date is `{type, value}` with
// the type `datetime`, `localdatetime`, `date`, `time` or `duration` and its ISO 8601 text.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  TYPES, TEMPORAL_TYPES, TEXT_TYPES, placeholderOf, nodeFromTyped, typedFromNode, scalarError,
  setType, emptyNode, nodeToJson, nodeFromJsonText, typesOf, defaultType, enumOptions, schemaHint,
} from '../js/element-editor.js';
import {
  PARAM_TYPES, ITEM_TYPES, FORMAT_OF, formatExample, paramFromEntry, paramDeclaration, parseDefault,
  newParam, formFields, formArgs, initialValues,
} from '../js/saved-queries.js';
import {
  BASE_TYPES, TEMPORAL_BASES, CONSTRAINTS, describe, typeText, setBaseType, constraintSummary,
  backfillScript, getAt,
} from '../js/schema/draft.js';

const SAMPLES = {
  datetime: '2023-08-02T09:30:00+02:00',
  localdatetime: '2023-08-02T09:30:00',
  date: '2023-08-02',
  time: '09:30:00.250',
  duration: 'P1M2DT3H',
};

// ------------------------------------------------------------------ property editor

test('the property editor offers the six date and time types as text types', () => {
  assert.deepEqual(TEMPORAL_TYPES, ['datetime', 'localdatetime', 'date', 'time', 'offsettime', 'duration']);
  for (const t of TEMPORAL_TYPES) {
    assert.ok(TYPES.includes(t), t);
    assert.ok(TEXT_TYPES.includes(t), t);
    assert.ok(placeholderOf(t), t);
    assert.equal(scalarError(t, placeholderOf(t)), null, `${t}: its placeholder is a valid value`);
  }
  // The menu keeps uuid before the dates and the containers after them.
  assert.ok(TYPES.indexOf('uuid') < TYPES.indexOf('datetime'));
  assert.ok(TYPES.indexOf('duration') < TYPES.indexOf('array'));
});

test('a typed date value round-trips through a form node', () => {
  for (const [type, value] of Object.entries(SAMPLES)) {
    const node = nodeFromTyped({ type, value });
    assert.equal(node.type, type);
    assert.equal(node.text, value);
    assert.deepEqual(typedFromNode(node), { typed: { type, value } });
    // Raw JSON keeps the type of the value it replaces (a date stays a date).
    const raw = nodeFromJsonText(nodeToJson(node), node);
    assert.equal(raw.node.type, type);
    assert.equal(raw.node.text, value);
  }
  // Without the hint a JSON string is a string: no type is guessed from content (rule 7).
  assert.equal(nodeFromJsonText('"2023-08-02"').node.type, 'string');
});

test('scalarError checks the shape of each date text; the engine checks the rest', () => {
  const ok = {
    date: ['2023-08-02', '-0044-03-15'],
    time: ['09:30', '09:30:15', '09:30:15.123456'],
    localdatetime: ['2023-08-02T09:30', '2023-08-02 09:30:00', '2023-08-02'],
    datetime: ['2023-08-02T09:30Z', '2023-08-02T09:30:00-07:00', '2023-08-02T09:30[Europe/Berlin]',
      '2023-08-02T09:30+02:00[Europe/Berlin]'],
    duration: ['P1D', 'PT90M', '-P1Y2M', 'P1M-2D', 'pt1h'],
  };
  for (const [type, texts] of Object.entries(ok)) {
    for (const text of texts) assert.equal(scalarError(type, text), null, `${type} ${text}`);
  }
  const bad = {
    date: ['02/08/2023', '2023-8-2', ''],
    time: ['9:30', '09:30 am'],
    localdatetime: ['2023-08-02T09:30Z'],
    // A stored datetime needs its offset or zone.
    datetime: ['2023-08-02T09:30', '2023-08-02', 'yesterday'],
    duration: ['1 day', 'P', ''],
  };
  for (const [type, texts] of Object.entries(bad)) {
    for (const text of texts) assert.match(scalarError(type, text), /e\.g\./, `${type} ${text}`);
  }
  const err = typedFromNode(Object.assign(emptyNode('datetime'), { text: '2023-08-02T09:30' }));
  assert.match(err.error, /offset or zone/);
});

test('changing to a date type keeps the text the user typed', () => {
  const node = Object.assign(emptyNode('string'), { text: '2023-08-02' });
  setType(node, 'date');
  assert.deepEqual(typedFromNode(node), { typed: { type: 'date', value: '2023-08-02' } });
  setType(node, 'duration');
  assert.equal(node.text, '2023-08-02');
  assert.ok(typedFromNode(node).error);
});

test('a schema format declares the date type of a field', () => {
  assert.deepEqual(typesOf({ type: 'string', format: 'date-time' }), ['datetime']);
  assert.deepEqual(typesOf({ type: 'string', format: 'local-date-time' }), ['localdatetime']);
  assert.deepEqual(typesOf({ type: ['string', 'null'], format: 'date' }), ['date', 'null']);
  assert.deepEqual(typesOf({ type: 'string', format: 'time' }), ['time']);
  assert.deepEqual(typesOf({ type: 'string', format: 'duration' }), ['duration']);
  assert.deepEqual(typesOf({ anyOf: [{ type: 'string', format: 'date' }, { type: 'null' }] }), ['date', 'null']);
  assert.equal(defaultType({ type: ['null', 'string'], format: 'date-time' }), 'datetime');
  const options = enumOptions({ type: 'string', format: 'date', enum: ['2023-01-01', '2024-01-01'] });
  assert.deepEqual(options.map((o) => o.typed), [
    { type: 'date', value: '2023-01-01' }, { type: 'date', value: '2024-01-01' },
  ]);
  assert.equal(
    schemaHint({ type: 'string', format: 'date', formatMinimum: '2000-01-01', formatExclusiveMaximum: '2030-01-01' }),
    'date · ≥ 2000-01-01, < 2030-01-01',
  );
  assert.equal(schemaHint({ type: 'string', format: 'time', formatMaximum: '18:00:00' }), 'time · ≤ 18:00:00');
});

// ------------------------------------------------------------------ saved query parameters

test('saved query parameters offer the date formats as types', () => {
  for (const t of ['datetime', 'localdatetime', 'date', 'time', 'duration']) {
    assert.ok(PARAM_TYPES.includes(t), t);
    assert.ok(ITEM_TYPES.includes(t), t);
    assert.ok(formatExample(t), t);
  }
  assert.deepEqual(FORMAT_OF, {
    uuid: 'uuid', datetime: 'date-time', localdatetime: 'local-date-time', date: 'date', time: 'time', duration: 'duration',
  });
  const entry = { name: 'since', schema: { type: 'string', format: 'date-time' }, default: '2023-01-01T00:00:00Z' };
  const param = paramFromEntry(entry);
  assert.equal(param.type, 'datetime');
  assert.equal(param.defaultText, '2023-01-01T00:00:00Z');
  assert.deepEqual(paramDeclaration(param), { declaration: entry });
  const list = paramFromEntry({ name: 'days', schema: { type: 'array', items: { type: 'string', format: 'date' } } });
  assert.equal(list.type, 'array');
  assert.equal(list.itemsType, 'date');
  assert.deepEqual(paramDeclaration(list).declaration.schema, { type: 'array', items: { type: 'string', format: 'date' } });
  const fresh = Object.assign(newParam('took'), { type: 'duration', defaultText: ' PT1H ' });
  assert.deepEqual(paramDeclaration(fresh), {
    declaration: { name: 'took', schema: { type: 'string', format: 'duration' }, default: 'PT1H' },
  });
  assert.deepEqual(parseDefault('date', ''), { value: undefined });
});

test('the run form takes a date as its text', () => {
  const fields = formFields({ params: [
    { name: 'since', schema: { type: 'string', format: 'date-time' }, required: true },
    { name: 'on', schema: { type: 'string', format: 'date' }, default: '2024-02-29' },
    { name: 'days', schema: { type: 'array', items: { type: 'string', format: 'date' } } },
  ] });
  assert.deepEqual(fields.map((f) => f.kind), ['datetime', 'date', 'list']);
  assert.equal(fields[2].itemsKind, 'date');
  const values = initialValues(fields);
  assert.equal(values.on, '2024-02-29');
  const { args, errors } = formArgs(fields, { ...values, since: '2023-08-02T09:30Z', days: '2024-01-01\n2024-01-02' });
  assert.deepEqual(errors, {});
  assert.deepEqual(args, { since: '2023-08-02T09:30Z', on: '2024-02-29', days: ['2024-01-01', '2024-01-02'] });
});

// ------------------------------------------------------------------ schema editor

function schemaWith(node) {
  return { mode: 'open', vertices: { event: { schema: { type: 'object', properties: { at: node } } } } };
}
const AT = ['vertices', 'event', 'schema', 'properties', 'at'];

test('the schema editor knows the date types and their bounds', () => {
  for (const b of TEMPORAL_BASES) {
    assert.ok(BASE_TYPES.includes(b), b);
    assert.deepEqual(CONSTRAINTS[b], ['formatMinimum', 'formatMaximum', 'formatExclusiveMinimum', 'formatExclusiveMaximum']);
  }
  assert.equal(describe({ type: 'string', format: 'date-time' }).base, 'datetime');
  assert.equal(describe({ type: ['string', 'null'], format: 'local-date-time' }).base, 'localdatetime');
  assert.equal(typeText({ type: 'array', items: { type: 'string', format: 'duration' } }), 'array of duration');
  assert.equal(typeText({ type: ['string', 'integer'], format: 'date' }), 'date | integer');
  assert.deepEqual(
    constraintSummary({ type: 'string', format: 'date', formatMinimum: '2000-01-01', formatExclusiveMaximum: '2030-01-01' }),
    ['>= 2000-01-01, < 2030-01-01'],
  );
  assert.deepEqual(constraintSummary({ type: 'string', format: 'time', formatMaximum: '18:00:00' }), ['<= 18:00:00']);
});

test('setBaseType writes the format and drops what does not fit the new type', () => {
  const doc = schemaWith({ type: ['string', 'null'], minLength: 1, description: 'when' });
  setBaseType(doc, AT, 'datetime');
  assert.deepEqual(getAt(doc, AT), { type: ['string', 'null'], format: 'date-time', description: 'when' });
  getAt(doc, AT).formatMinimum = '2000-01-01T00:00:00Z';
  setBaseType(doc, AT, 'date');
  // A bound of the old type is not one of the new type's: it would not parse.
  assert.deepEqual(getAt(doc, AT), { type: ['string', 'null'], format: 'date', description: 'when' });
  getAt(doc, AT).formatMaximum = '2030-01-01';
  setBaseType(doc, AT, 'string');
  assert.deepEqual(getAt(doc, AT), { type: ['string', 'null'], description: 'when' });
  setBaseType(doc, AT, 'uuid');
  assert.deepEqual(getAt(doc, AT), { type: ['string', 'null'], format: 'uuid', description: 'when' });
});

test('backfill writes a date placeholder through its constructor', () => {
  const draft = schemaWith({ type: 'string', format: 'date' });
  draft.mode = 'closed';
  draft.vertices.event.schema.required = ['at'];
  const fill = backfillScript(draft, [{ kind: 'missing_required', element: 'vertex', label: 'event', field: 'at', count: 2 }]);
  assert.match(fill.script, /\.property\("at", date\("1970-01-01"\)\)/);
  draft.vertices.event.schema.properties.at.default = '2024-02-29';
  const withDefault = backfillScript(draft, [{ kind: 'missing_required', element: 'vertex', label: 'event', field: 'at', count: 2 }]);
  assert.match(withDefault.script, /\.property\("at", date\("2024-02-29"\)\)/);
});
