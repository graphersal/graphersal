// Node tests of the Store menu's history calendar model (js/store-calendar.js): local days in a
// pinned time zone, grouping once, the month grid, colour buckets, keyboard moves, the menu's
// recent lists, the filter, and a large history.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  localParts, clockText, dayKey, parseDayKey, historyEntries, recentHistory, filterEntries,
  historyIndex, bucket, BUCKETS, monthGrid, weekdayNames, daysInMonth, addDays, addMonths,
  sameDayIn, todayKey, yearRange, monthTotals, dayLabel, countText, RECENT_COUNT,
} from '../js/store-calendar.js';
import { actionDisabled } from '../js/store-model.js';

const UTC = () => 0;
// On a CI runner (`CI` set) time bounds are not checked: the runner's CPU is not ours.
const onCi = !!process.env.CI && process.env.CI !== 'false' && process.env.CI !== '0';
const BERLIN_SUMMER = () => 120; // +02:00
const NEW_YORK = () => -240; // -04:00
/** Microseconds of an ISO time. */
const us = (iso) => Date.parse(iso) * 1000;

const STORE = {
  commitSeq: 9,
  snapshots: [
    { commitSeq: 0, lastCommitAt: 0, createdAt: us('2026-10-01T10:00:00Z'), vertexCount: 6, edgeCount: 6 },
    { commitSeq: 4, name: 'nightly', lastCommitAt: us('2026-10-07T23:30:00Z'), createdAt: us('2026-10-07T23:31:00Z') },
    { commitSeq: 8, lastCommitAt: us('2026-10-08T09:00:00Z'), createdAt: us('2026-10-08T09:00:01Z') },
  ],
  marks: [
    { name: 'before-import', commitSeq: 2, time: us('2026-10-07T22:10:00Z') },
    { name: 'after-import', commitSeq: 8, time: us('2026-10-08T09:00:00Z') },
    { name: 'old', commitSeq: 0, time: 0 },
  ],
};

test('localParts gives the local day and clock of a time; none for 0', () => {
  const t = us('2026-10-07T23:30:05Z');
  assert.equal(localParts(t, UTC).key, '2026-10-07');
  assert.equal(localParts(t, BERLIN_SUMMER).key, '2026-10-08');
  assert.equal(clockText(localParts(t, BERLIN_SUMMER)), '01:30:05');
  assert.equal(localParts(us('2026-10-08T02:00:00Z'), NEW_YORK).key, '2026-10-07');
  assert.equal(localParts(0, UTC), null);
  assert.equal(localParts(-5, UTC), null);
  assert.equal(clockText(null), '-');
});

test('day keys round trip', () => {
  assert.equal(dayKey(2026, 0, 5), '2026-01-05');
  assert.deepEqual(parseDayKey('2026-12-31'), { year: 2026, month: 11, day: 31 });
  assert.equal(parseDayKey('2026-1-1'), null);
  assert.equal(parseDayKey(undefined), null);
});

test('historyEntries: newest first, snapshot time from its commit or its creation', () => {
  const e = historyEntries(STORE, UTC);
  assert.deepEqual(e.map((x) => x.id), ['s:8', 'm:after-import', 's:4', 'm:before-import', 's:0', 'm:old']);
  const s0 = e.find((x) => x.id === 's:0');
  assert.equal(s0.parts.key, '2026-10-01'); // no commit yet: the time it was written
  assert.equal(e.find((x) => x.id === 'm:old').parts, null);
  assert.equal(e.find((x) => x.id === 's:4').name, 'nightly');
  assert.deepEqual(historyEntries(null), []);
});

test('historyIndex groups by local day once; undated apart; newest/oldest and maxima', () => {
  const utc = historyIndex(historyEntries(STORE, UTC));
  assert.deepEqual([...utc.days.keys()].sort(), ['2026-10-01', '2026-10-07', '2026-10-08']);
  const d7 = utc.days.get('2026-10-07');
  assert.deepEqual(d7.marks.map((m) => m.name), ['before-import']);
  assert.deepEqual(d7.snapshots.map((s) => s.commitSeq), [4]);
  assert.equal(d7.total, 2);
  assert.equal(utc.undated.length, 1);
  assert.equal(utc.newest, '2026-10-08');
  assert.equal(utc.oldest, '2026-10-01');
  assert.deepEqual(utc.max, { mark: 1, snapshot: 1, total: 2 });

  // In Berlin the 23:30 UTC snapshot falls on the 8th, the 22:10 mark stays on the 8th too.
  const berlin = historyIndex(historyEntries(STORE, BERLIN_SUMMER));
  assert.equal(berlin.days.get('2026-10-08').snapshots.length, 2);
  assert.equal(berlin.days.get('2026-10-08').marks.length, 2);
  assert.equal(berlin.days.has('2026-10-07'), false);
  // Within a day the newest time first.
  assert.deepEqual(berlin.days.get('2026-10-08').marks.map((m) => m.name), ['after-import', 'before-import']);

  const empty = historyIndex([]);
  assert.equal(empty.newest, null);
  assert.equal(empty.days.size, 0);
});

test('recentHistory: the newest few of each kind and whether there are more', () => {
  const r = recentHistory(STORE, 2);
  assert.deepEqual(r.snapshots.map((s) => s.commitSeq), [8, 4]);
  assert.deepEqual(r.marks.map((m) => m.name), ['after-import', 'before-import']);
  assert.equal(r.snapshotCount, 3);
  assert.equal(r.markCount, 3);
  assert.equal(r.more, true);
  const all = recentHistory(STORE);
  assert.equal(RECENT_COUNT, 5);
  assert.equal(all.more, false);
  assert.equal(all.snapshots.length, 3);
  // Unsorted input is sorted (newest commit first).
  const shuffled = recentHistory({ snapshots: [{ commitSeq: 3 }, { commitSeq: 9 }, { commitSeq: 1 }], marks: [] }, 2);
  assert.deepEqual(shuffled.snapshots.map((s) => s.commitSeq), [9, 3]);
  assert.deepEqual(recentHistory(null).marks, []);
});

test('filterEntries: kinds, name text (case-insensitive) and #commit', () => {
  const e = historyEntries(STORE, UTC);
  assert.deepEqual(filterEntries(e, { text: 'IMPORT' }).map((x) => x.id), ['m:after-import', 'm:before-import']);
  assert.deepEqual(filterEntries(e, { text: 'night' }).map((x) => x.id), ['s:4']);
  assert.deepEqual(filterEntries(e, { text: '#8' }).map((x) => x.id), ['s:8', 'm:after-import']);
  assert.deepEqual(filterEntries(e, { kinds: { mark: true, snapshot: false } }).map((x) => x.kind),
    ['mark', 'mark', 'mark']);
  assert.equal(filterEntries(e).length, e.length);
});

test('bucket: 0 for none, a log scale up to BUCKETS', () => {
  assert.equal(BUCKETS, 4);
  assert.equal(bucket(0, 10), 0);
  assert.equal(bucket(1, 1), 1);
  assert.equal(bucket(1, 1000), 1);
  assert.equal(bucket(1000, 1000), 4);
  assert.equal(bucket(10, 1000), 2);
  assert.equal(bucket(100, 1000), 3);
  assert.equal(bucket(5, 0), 1);
  for (let n = 1; n <= 50; n++) {
    assert.ok(bucket(n, 50) >= bucket(n - 1, 50), 'monotonic');
  }
});

test('monthGrid: whole weeks from Monday, the neighbours fill, 4 to 6 rows', () => {
  const oct = monthGrid(2026, 9); // 1 Oct 2026 is a Thursday
  assert.equal(oct.length % 7, 0);
  assert.equal(oct[0].key, '2026-09-28');
  assert.equal(oct[0].inMonth, false);
  assert.equal(oct[3].key, '2026-10-01');
  assert.equal(oct.filter((c) => c.inMonth).length, 31);
  assert.equal(oct[oct.length - 1].key, '2026-11-01');
  const feb = monthGrid(2027, 1); // 1 Feb 2027 is a Monday, 28 days: exactly 4 rows
  assert.equal(feb.length, 28);
  assert.equal(monthGrid(2026, 2).length, 42); // March 2026 starts on a Sunday: 6 rows
  assert.equal(monthGrid(2026, 9, 0)[0].key, '2026-09-27'); // Sunday first
  assert.deepEqual(weekdayNames(1), ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']);
  assert.equal(daysInMonth(2028, 1), 29);
  assert.equal(daysInMonth(2026, 1), 28);
});

test('keyboard moves: days across months and years, months keeping the day', () => {
  assert.equal(addDays('2026-10-31', 1), '2026-11-01');
  assert.equal(addDays('2026-01-01', -1), '2025-12-31');
  assert.equal(addDays('2026-10-08', 7), '2026-10-15');
  assert.deepEqual(addMonths({ year: 2026, month: 0 }, -1), { year: 2025, month: 11 });
  assert.deepEqual(addMonths({ year: 2026, month: 11 }, 1), { year: 2027, month: 0 });
  assert.deepEqual(addMonths({ year: 2026, month: 5 }, -30), { year: 2023, month: 11 });
  assert.equal(sameDayIn({ year: 2026, month: 1 }, 31), '2026-02-28');
  assert.equal(sameDayIn({ year: 2026, month: 2 }, 15), '2026-03-15');
});

test('today, years and month totals for the selectors', () => {
  assert.equal(todayKey(Date.parse('2026-10-10T23:30:00Z'), BERLIN_SUMMER), '2026-10-11');
  const idx = historyIndex(historyEntries({
    snapshots: [{ commitSeq: 1, lastCommitAt: us('2023-03-02T12:00:00Z') }],
    marks: [{ name: 'a', commitSeq: 2, time: us('2026-10-08T12:00:00Z') }, { name: 'b', commitSeq: 3, time: us('2026-10-09T12:00:00Z') }],
  }, UTC));
  assert.deepEqual(yearRange(idx, '2026-10-10'), [2026, 2025, 2024, 2023]);
  assert.deepEqual(yearRange(historyIndex([]), '2026-10-10'), [2026]);
  assert.deepEqual(yearRange(historyIndex([]), '2026-10-10', 2024), [2026, 2025, 2024]);
  const totals = monthTotals(idx);
  assert.deepEqual(totals.get('2026-10'), { marks: 2, snapshots: 0 });
  assert.deepEqual(totals.get('2023-03'), { marks: 0, snapshots: 1 });
});

test('labels of days and counts', () => {
  assert.equal(countText(0, 0), 'nothing');
  assert.equal(countText(1, 2), '1 mark, 2 snapshots');
  assert.equal(countText(0, 1), '1 snapshot');
  const idx = historyIndex(historyEntries(STORE, UTC));
  assert.equal(dayLabel('2026-10-07', idx.days.get('2026-10-07')), 'Wednesday, 7 October 2026: 1 mark, 1 snapshot');
  assert.equal(dayLabel('2026-10-09', null), 'Friday, 9 October 2026: nothing');
});

test('a large history: 20 000 snapshots and 5 000 marks are grouped once, quickly', () => {
  const start = us('2020-01-01T00:00:00Z');
  const hour = 3600 * 1e6;
  const store = {
    commitSeq: 30000,
    snapshots: Array.from({ length: 20000 }, (_, i) => ({ commitSeq: i, lastCommitAt: start + i * hour })),
    marks: Array.from({ length: 5000 }, (_, i) => ({ name: `m${i}`, commitSeq: i * 4, time: start + i * 4 * hour })),
  };
  const t0 = performance.now();
  const idx = historyIndex(historyEntries(store, UTC));
  const ms = performance.now() - t0;
  let n = 0;
  for (const d of idx.days.values()) n += d.total;
  assert.equal(n, 25000);
  assert.equal(idx.days.get('2020-01-01').snapshots.length, 24);
  assert.equal(idx.days.get('2020-01-01').marks.length, 6);
  assert.equal(idx.max.total, 30);
  // A time bound depends on the runner's CPU: not checked on CI (`CI` set), as in the Rust tests.
  if (!onCi) assert.ok(ms < 1000, `grouping took ${ms} ms`);
  assert.equal(recentHistory(store).snapshots[0].commitSeq, 19999);
});

test('actionDisabled: locked disables all; rollback not at or after the store commit, not in maintenance', () => {
  const store = { commitSeq: 5 };
  assert.equal(actionDisabled(store, 'view', 9, false), false);
  assert.equal(actionDisabled(store, 'view', 1, true), true);
  assert.equal(actionDisabled(store, 'rollback', 4, false), false);
  assert.equal(actionDisabled(store, 'rollback', 5, false), true);
  assert.equal(actionDisabled({ commitSeq: 5, maintenance: {} }, 'rollback', 1, false), true);
  assert.equal(actionDisabled(store, 'fork', 5, false), false);
});

test('actionDisabled: Compact is enabled whatever the advice, not while locked, viewing, damaged or on a backup', () => {
  const store = { commitSeq: 5, compaction: { recommended: false, reason: 'small' } };
  assert.equal(actionDisabled(store, 'compact', null, false), false);
  assert.equal(actionDisabled(store, 'compact', null, true), true);
  assert.equal(actionDisabled(store, 'compact', null, false, true), true);
  assert.equal(actionDisabled({ ...store, maintenance: {} }, 'compact', null, false), true);
  assert.equal(actionDisabled({ ...store, backup: { commitSeq: 5 } }, 'compact', null, false), true);
  assert.equal(actionDisabled({ ...store, frozenByDamage: true }, 'compact', null, false), true);
  assert.equal(actionDisabled(null, 'compact', null, false), true);
});
