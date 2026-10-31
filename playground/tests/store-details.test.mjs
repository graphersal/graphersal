// Node tests of the Store menu's two calls (js/store-model.js `withStoreDetails`, `pendingText`):
// the summary shows at once with its sizes and compaction advice pending, the details fill them in.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { STORE_DETAIL_KEYS, withStoreDetails, pendingText, compactionModel } from '../js/store-model.js';

/** A `GET /api/store/summary` answer: everything but the figures of the second call. */
const SUMMARY = {
  dir: '/home/me/data',
  commitSeq: 7,
  memory: false,
  attic: [{ id: 'a', restorable: true, restoreBlocked: null }],
  compaction: null,
  compactionError: null,
  storeBytes: null,
  storeBytesText: null,
  graphBytes: null,
  graphBytesText: null,
  detailsPending: true,
};

/** Its `GET /api/store/details` answer. */
const DETAILS = {
  commitSeq: 7,
  compaction: { recommended: false, reason: 'the store is smaller than the minimum of 64 MiB', compactable: false },
  compactionError: null,
  storeBytes: 2048,
  storeBytesText: '2.0 KiB',
  graphBytes: 512,
  graphBytesText: '512 B',
};

test('the details fill in exactly the pending figures', () => {
  const info = withStoreDetails(SUMMARY, DETAILS);
  assert.equal(info.detailsPending, false);
  for (const key of STORE_DETAIL_KEYS) assert.deepEqual(info[key], DETAILS[key], key);
  // Everything else is the summary's.
  assert.equal(info.dir, SUMMARY.dir);
  assert.deepEqual(info.attic, SUMMARY.attic);
  // The summary itself is not changed.
  assert.equal(SUMMARY.detailsPending, true);
  assert.equal(SUMMARY.storeBytes, null);
  assert.equal(compactionModel(info).headline, 'No compaction needed');
});

test('details of another commit, a missing answer and a complete info leave the info as it is', () => {
  assert.equal(withStoreDetails(SUMMARY, { ...DETAILS, commitSeq: 8 }), SUMMARY);
  assert.equal(withStoreDetails(SUMMARY, null), SUMMARY);
  const complete = { ...SUMMARY, detailsPending: undefined, storeBytes: 1 };
  assert.equal(withStoreDetails(complete, DETAILS), complete);
  assert.equal(withStoreDetails(null, DETAILS), null);
});

test('a pending size reads as an ellipsis, a known one as its text, an unknown one as ?', () => {
  assert.equal(pendingText(SUMMARY, SUMMARY.storeBytesText), '…');
  assert.equal(pendingText(withStoreDetails(SUMMARY, DETAILS), '2.0 KiB'), '2.0 KiB');
  assert.equal(pendingText({ detailsPending: false }, null), '?');
  assert.equal(pendingText(null, null), '?');
});
