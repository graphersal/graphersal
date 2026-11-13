import { test } from 'node:test';
import assert from 'node:assert/strict';
import { versionLabel } from '../js/build-info.js';

test('a clean build of the release tag shows the version alone', () => {
  const label = versionLabel('0.2.0', { commit: 'abc123d', tag: 'v0.2.0', dirty: false });
  assert.equal(label.text, 'v0.2.0');
  assert.equal(label.title, 'Engine 0.2.0, the release build (commit abc123d)');
});

test('a development build adds its commit', () => {
  const label = versionLabel('0.2.0', { commit: 'abc123d', tag: '', dirty: false });
  assert.equal(label.text, 'v0.2.0 · abc123d');
  assert.match(label.title, /development build of commit abc123d, not a release$/);
});

test('uncommitted changes are marked, also on the release tag', () => {
  const label = versionLabel('0.2.0', { commit: 'abc123d', tag: 'v0.2.0', dirty: true });
  assert.equal(label.text, 'v0.2.0 · abc123d + changes');
  assert.match(label.title, /with uncommitted changes/);
});

test('without a build file only the engine version is shown', () => {
  assert.deepEqual(versionLabel('0.2.0', null), { text: 'v0.2.0', title: 'Engine 0.2.0' });
  assert.equal(versionLabel(null, null), null);
});

test('a profile with a version shows the product version; the engine moves to the tooltip', () => {
  const product = { title: 'graphersal-memory', version: '0.1.0' };
  assert.deepEqual(versionLabel('0.6.1', null, product), {
    text: 'v0.1.0',
    title: 'graphersal-memory 0.1.0 · Graphersal engine 0.6.1',
  });
  // A build file does not matter then, and a profile without a version changes nothing.
  assert.equal(versionLabel('0.6.1', { commit: 'abc123d', tag: '', dirty: true }, product).text, 'v0.1.0');
  assert.deepEqual(versionLabel('0.6.1', null, { title: 'Mine' }), versionLabel('0.6.1', null));
});
