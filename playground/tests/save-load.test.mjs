// Node tests of the Save menu / "Load schema" logic that runs without a browser: the ONE file each
// Save entry downloads (js/export.js) and the order in which Stop rebuilds a restore point
// (js/engine/wasm-engine.js). Graph files (GraphSON, GraphML) carry no schema, so the schema
// travels as its own file and is the load's schema argument (set BEFORE the import).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { savedGraphFile, schemaFile, isEmptySchema, catalogFile, isEmptyCatalog } from '../js/export.js';
import { restoreCalls, graphFileFormat } from '../js/engine/wasm-engine.js';

const at = new Date(2026, 9, 8, 7, 5, 3);

test('save: the data entry downloads the GraphML alone, also when the graph stores a schema', () => {
  const file = savedGraphFile({ format: 'graphml', text: '<graphml/>', schema: '{"mode":"open"}', warnings: [] }, at);
  assert.deepEqual(file, { name: 'graphersal-graph-20261008-070503.graphml', type: 'application/xml', text: '<graphml/>' });
});

test('save: the schema entry downloads the schema as its own pretty JSON file', () => {
  const file = schemaFile({ mode: 'open', vertices: { person: { schema: { type: 'object' } } } }, at);
  assert.equal(file.name, 'graphersal-schema-20261008-070503.json');
  assert.equal(file.type, 'application/json');
  assert.deepEqual(JSON.parse(file.text), { mode: 'open', vertices: { person: { schema: { type: 'object' } } } });
  assert.ok(file.text.includes('\n  '));
});

test('save: an empty schema is no schema', () => {
  assert.equal(isEmptySchema(null), true);
  assert.equal(isEmptySchema({}), true);
  assert.equal(isEmptySchema({ mode: 'none' }), true);
  assert.equal(isEmptySchema({ mode: 'open' }), false);
  assert.equal(isEmptySchema({ vertices: { a: { schema: { type: 'object' } } } }), false);
  assert.equal(isEmptySchema({ edges: { e: { connections: [] } } }), false);
});

test('restore: a GraphML point is loaded in one call with its schema as the load argument', () => {
  const bytes = new Uint8Array([60]);
  assert.deepEqual(
    restoreCalls({ type: 'graphml', bytes, name: 'g.graphml', kind: 'file', schema: '{"mode":"open"}' }),
    [['loadGraphml', bytes, 'g.graphml', '{"mode":"open"}']],
  );
  assert.deepEqual(
    restoreCalls({ type: 'graphml', bytes, name: 'g.graphml', kind: 'file', schema: null }),
    [['loadGraphml', bytes, 'g.graphml', null]],
  );
});

test('restore: a sample is loaded first, then gets the schema loaded onto it', () => {
  assert.deepEqual(restoreCalls({ type: 'sample', kind: 'modern', schema: '{}' }), [
    ['loadSample', 'modern'],
    ['setSchema', '{}'],
  ]);
  assert.deepEqual(restoreCalls({ type: 'sample', kind: 'modern' }), [['loadSample', 'modern']]);
});

test('save: GraphSON is one .json graph file', () => {
  const file = savedGraphFile({ format: 'graphson', text: '{"id":"1"}\n', schema: '{"mode":"open"}', warnings: [] }, at);
  assert.deepEqual(file, { name: 'graphersal-graph-20261008-070503.json', type: 'application/json', text: '{"id":"1"}\n' });
});

test('restore: a GraphSON point is loaded in one call with its schema as the load argument', () => {
  const bytes = new Uint8Array([123]);
  assert.deepEqual(
    restoreCalls({ type: 'graphson', bytes, name: 'g.json', kind: 'file', schema: '{"mode":"open"}' }),
    [['loadGraphson', bytes, 'g.json', '{"mode":"open"}']],
  );
  assert.deepEqual(
    restoreCalls({ type: 'graphson', bytes, name: 'g.json', kind: 'file', schema: null }),
    [['loadGraphson', bytes, 'g.json', null]],
  );
});

test('load: the file extension picks the graph format', () => {
  assert.equal(graphFileFormat('modern.json'), 'graphson');
  assert.equal(graphFileFormat('modern.JSONL'), 'graphson');
  assert.equal(graphFileFormat('modern.graphson'), 'graphson');
  assert.equal(graphFileFormat('modern.graphml'), 'graphml');
  assert.equal(graphFileFormat('modern.XML'), 'graphml');
  assert.equal(graphFileFormat('modern.gsnap'), 'snapshot');
  assert.equal(graphFileFormat('modern.GSNAP'), 'snapshot');
  assert.equal(graphFileFormat('modern.csv'), null);
});

test('save: a snapshot is one binary .gsnap file that carries its schema', () => {
  const bytes = new Uint8Array([71, 82, 83, 76]);
  const file = savedGraphFile({ format: 'snapshot', bytes, schema: null, warnings: [] }, at);
  assert.deepEqual(file, { name: 'graphersal-graph-20261008-070503.gsnap', type: 'application/octet-stream', bytes });
});

test('save: a snapshot without the models says so in its file name', () => {
  const bytes = new Uint8Array([71, 82, 83, 76]);
  const file = savedGraphFile({ format: 'snapshot', bytes, schema: null, warnings: [], withoutModels: true }, at);
  assert.equal(file.name, 'graphersal-graph-without-models-20261008-070503.gsnap');
});

test('restore: a snapshot point is loaded first, a later schema goes on top', () => {
  const bytes = new Uint8Array([1]);
  assert.deepEqual(
    restoreCalls({ type: 'snapshot', bytes, name: 'g.gsnap', kind: 'file', schema: null }),
    [['loadSnapshot', bytes, 'g.gsnap']],
  );
  assert.deepEqual(
    restoreCalls({ type: 'snapshot', bytes, name: 'g.gsnap', kind: 'file', schema: '{}' }),
    [['loadSnapshot', bytes, 'g.gsnap'], ['setSchema', '{}']],
  );
});

test('restore: the catalog comes last and replaces the catalog the graph brought', () => {
  const bytes = new Uint8Array([1]);
  const catalog = '{"definitions":[]}';
  assert.deepEqual(restoreCalls({ type: 'sample', kind: 'modern', schema: '{}', catalog }), [
    ['loadSample', 'modern'], ['setSchema', '{}'], ['loadCatalog', catalog, true],
  ]);
  assert.deepEqual(restoreCalls({ type: 'graphson', bytes, name: 'g.json', kind: 'file', schema: null, catalog }), [
    ['loadGraphson', bytes, 'g.json', null], ['loadCatalog', catalog, true],
  ]);
  assert.deepEqual(restoreCalls({ type: 'snapshot', bytes, name: 'g.gsnap', kind: 'file', schema: null, catalog }), [
    ['loadSnapshot', bytes, 'g.gsnap'], ['loadCatalog', catalog, true],
  ]);
});

test('save: the catalog file is the engine text under its own name', () => {
  const at = new Date(2026, 9, 8, 7, 5, 3);
  const text = '{"definitions":[{"kind":"query","name":"q"},{"kind":"compression","name":"c"}]}\n';
  assert.deepEqual(catalogFile(text, at), { name: 'graphersal-catalog-20261008-070503.json', type: 'application/json', text });
  assert.equal(isEmptyCatalog(text), false);
  assert.equal(isEmptyCatalog('{"definitions":[]}'), true);
  assert.equal(isEmptyCatalog('nope'), true);
});
