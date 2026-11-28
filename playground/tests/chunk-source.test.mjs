// The chunk source the worker hands the engine module for a File or Blob (js/engine/chunk-source.js),
// and the restore point that keeps the File itself instead of its bytes (wasm-engine.js).
import test from 'node:test';
import assert from 'node:assert/strict';

import { chunkSource, isBlob } from '../js/engine/chunk-source.js';
import { restoreCalls } from '../js/engine/wasm-engine.js';

/** A Blob-like file over `bytes`, read by the fake reader below (Node has no FileReaderSync). */
function fakeFile(bytes, name = 'g.json') {
  return { name, size: bytes.length, slice: (from, to) => bytes.slice(from, to) };
}

class FakeReader {
  readAsArrayBuffer(slice) {
    if (slice === 'gone') throw new Error('NotReadableError');
    return slice.buffer.slice(slice.byteOffset, slice.byteOffset + slice.byteLength);
  }
}

test('a chunk source reads the slices the module asks for, fewer at the end', () => {
  const bytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7]);
  const source = chunkSource(fakeFile(bytes), FakeReader);
  assert.equal(source.size, 7);
  assert.deepEqual([...source.read(0, 3)], [1, 2, 3]);
  assert.deepEqual([...source.read(5, 3)], [6, 7]);
  assert.deepEqual([...source.read(7, 3)], []);
});

test('a file that cannot be read any more fails with a message naming it', () => {
  const file = { name: 'big.json', size: 10, slice: () => 'gone' };
  const source = chunkSource(file, FakeReader);
  assert.throws(
    () => source.read(0, 4),
    (err) => typeof err === 'string' && err.includes('"big.json"') && err.includes('changed or been removed'),
  );
});

test('a restore point keeps the File and hands it to the load', () => {
  const file = new Blob(['{"id":"a"}']);
  assert.ok(isBlob(file));
  assert.ok(!isBlob(new Uint8Array([1])));
  assert.deepEqual(
    restoreCalls({ type: 'graphson', source: file, name: 'g.json', kind: 'file', schema: null }),
    [['loadGraphson', file, 'g.json', null]],
  );
  assert.deepEqual(
    restoreCalls({ type: 'snapshot', source: file, name: 'g.gsnap', kind: 'file', schema: null }),
    [['loadSnapshot', file, 'g.gsnap']],
  );
});
