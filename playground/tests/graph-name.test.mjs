// The header's graph name: a path is split so the directory part can be shortened from the left
// while the file or directory name stays visible (js/util.js `splitGraphName`).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { splitGraphName } from '../js/util.js';

test('a path splits into its directory and its last name', () => {
  assert.deepEqual(splitGraphName('/Users/aleks/Development/graphersal/ai/graph/kb.gstore'), {
    dir: '/Users/aleks/Development/graphersal/ai/graph/',
    base: 'kb.gstore',
  });
  assert.deepEqual(splitGraphName('C:\\data\\graphs\\kb'), { dir: 'C:\\data\\graphs\\', base: 'kb' });
  assert.deepEqual(splitGraphName('relative/dir/store'), { dir: 'relative/dir/', base: 'store' });
});

test('a directory path with a trailing separator keeps its last name visible', () => {
  assert.deepEqual(splitGraphName('/var/lib/graphs/kb/'), { dir: '/var/lib/graphs/', base: 'kb/' });
});

test('a name that is not a path is the base alone', () => {
  assert.deepEqual(splitGraphName('Modern (TinkerPop)'), { dir: '', base: 'Modern (TinkerPop)' });
  assert.deepEqual(splitGraphName(''), { dir: '', base: '' });
  assert.deepEqual(splitGraphName(undefined), { dir: '', base: '' });
});
