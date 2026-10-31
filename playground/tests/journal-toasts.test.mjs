// Node tests of the journal notifications' model and the shared journal follower
// (js/journal-toasts.js).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createToasts, createJournalFollower, passesMinLevel, normalizeMinLevel, levelRank, isSticky,
  toastClass, toastRole, moreText, cursorOf, matchesJournalFilters, batchAsPage,
  TOAST_MIN_LEVELS, DEFAULT_TOAST_MIN_LEVEL, TOAST_LIFE_MS,
} from '../js/journal-toasts.js';
import { startJournal, addJournalPage } from '../js/journal.js';

let seqN = 0;
function record(level, extra = {}) {
  seqN += 1;
  return {
    seq: seqN, level, source: 'store', kind: `store.${level}`, message: `a ${level} record ${seqN}`,
    time: 0, fields: {}, ...extra,
  };
}

/** A clock the test moves by hand. */
function clock(start = 1_000_000) {
  let t = start;
  return { now: () => t, advance: (ms) => { t += ms; } };
}

test('levels: ranks, the setting, sticky, colours and roles', () => {
  assert.deepEqual(['error', 'warn', 'notice', 'info', 'debug', 'what'].map(levelRank), [0, 1, 2, 3, 4, 5]);
  assert.equal(DEFAULT_TOAST_MIN_LEVEL, 'notice');
  assert.deepEqual(TOAST_MIN_LEVELS.map((l) => l.id), ['notice', 'warn', 'error', 'off']);
  assert.equal(normalizeMinLevel('bogus'), 'notice');
  assert.equal(normalizeMinLevel(undefined), 'notice');
  assert.equal(normalizeMinLevel('off'), 'off');
  assert.equal(passesMinLevel(record('notice'), 'notice'), true);
  assert.equal(passesMinLevel(record('info'), 'notice'), false);
  assert.equal(passesMinLevel(record('notice'), 'warn'), false);
  assert.equal(passesMinLevel(record('warn'), 'warn'), true);
  assert.equal(passesMinLevel(record('warn'), 'error'), false);
  assert.equal(passesMinLevel(record('error'), 'error'), true);
  assert.equal(passesMinLevel(record('error'), 'off'), false);
  assert.equal(isSticky('error') && isSticky('warn'), true);
  assert.equal(isSticky('notice') || isSticky('info'), false);
  assert.deepEqual(['error', 'warn', 'notice', 'info', 'debug', ''].map(toastClass),
    ['jt-error', 'jt-warn', 'jt-notice', 'jt-info', 'jt-debug', 'jt-debug']);
  assert.deepEqual(['error', 'warn', 'notice', 'info'].map(toastRole), ['alert', 'alert', 'status', 'status']);
  assert.equal(moreText(1), '+1 more journal record');
  assert.equal(moreText(4), '+4 more journal records');
});

test('records below the setting never pop up', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  assert.equal(m.push([record('info'), record('notice')], 'warn'), false);
  assert.equal(m.view().toasts.length, 0);
  assert.equal(m.push([record('notice')], 'off'), false);
  assert.equal(m.push([record('error')], 'error'), true);
  assert.equal(m.view().toasts.length, 1);
});

test('at most three toasts, newest first; the rest are one "+N more" toast', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  const rs = [record('error'), record('warn'), record('notice'), record('notice'), record('error')];
  m.push(rs, 'notice');
  const v = m.view();
  assert.deepEqual(v.toasts.map((t) => t.seq), [rs[2].seq, rs[1].seq, rs[0].seq]);
  assert.equal(v.more, 2);
  assert.equal(v.moreRole, 'alert', 'an error waits behind');
  assert.equal(v.toasts[0].role, 'status');
  assert.equal(v.toasts[2].role, 'alert');
  // Closing one promotes the oldest waiting record.
  m.close(v.toasts[1].id);
  assert.deepEqual(m.view().toasts.map((t) => t.seq), [rs[3].seq, rs[2].seq, rs[0].seq]);
  assert.equal(m.view().more, 1);
  // Closing the overflow drops the waiting ones.
  assert.equal(m.closeOverflow(), true);
  assert.equal(m.view().more, 0);
});

test('repeats of the same kind and message merge with a counter', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  const same = { kind: 'store.checkpoint', message: 'checkpoint written' };
  m.push([record('notice', same), record('notice', same), record('warn', same)], 'notice');
  const v = m.view();
  assert.equal(v.toasts.length, 1);
  assert.equal(v.toasts[0].count, 3);
  assert.equal(v.toasts[0].seq, seqN, 'the newest seq: a click opens there');
  assert.equal(v.toasts[0].level, 'warn', 'the most severe level of the merged records');
  assert.equal(v.toasts[0].sticky, true);
  // Another message of the same kind is another toast.
  m.push([record('notice', { kind: 'store.checkpoint', message: 'other' })], 'notice');
  assert.equal(m.view().toasts.length, 2);
});

test('repeats merge into a waiting record too, counted in "+N more"', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  m.push([record('error'), record('error'), record('error')], 'notice');
  const same = { kind: 'k', message: 'm' };
  m.push([record('notice', same), record('notice', same)], 'notice');
  assert.equal(m.view().more, 2);
  m.close(m.view().toasts[0].id);
  const promoted = m.view().toasts[0];
  assert.equal(promoted.count, 2);
  assert.equal(m.view().more, 0);
});

test('notice and info close after 30 s; warn and error stay', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  m.push([record('error'), record('notice')], 'notice');
  assert.equal(m.nextDeadline(), c.now() + TOAST_LIFE_MS);
  c.advance(TOAST_LIFE_MS - 1);
  assert.equal(m.expire(), false);
  c.advance(1);
  assert.equal(m.expire(), true);
  assert.deepEqual(m.view().toasts.map((t) => t.level), ['error']);
  assert.equal(m.nextDeadline(), null, 'an error never closes by itself');
  c.advance(10 * TOAST_LIFE_MS);
  assert.equal(m.expire(), false);
  assert.equal(m.view().toasts.length, 1);
});

test('hovering pauses the timer; leaving resumes with the time left', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  m.push([record('notice')], 'notice');
  const id = m.view().toasts[0].id;
  c.advance(10_000);
  assert.equal(m.hover(id, true), true);
  assert.equal(m.nextDeadline(), null);
  c.advance(120_000);
  m.expire();
  assert.equal(m.view().toasts.length, 1, 'kept while hovered');
  m.hover(id, false);
  assert.equal(m.nextDeadline(), c.now() + 20_000);
  c.advance(20_000);
  m.expire();
  assert.equal(m.view().toasts.length, 0);
});

test('a merge restarts the timer', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  const same = { kind: 'k', message: 'm' };
  m.push([record('info', same)], 'notice');
  assert.equal(m.view().toasts.length, 0, 'info is below notice');
  m.push([record('notice', same)], 'notice');
  c.advance(25_000);
  m.push([record('notice', same)], 'notice');
  c.advance(25_000);
  m.expire();
  assert.equal(m.view().toasts.length, 1);
  assert.equal(m.view().toasts[0].count, 2);
});

test('waiting notices expire too; a promoted record gets its full time', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  m.push([record('error'), record('error'), record('error'), record('notice'), record('warn')], 'notice');
  assert.equal(m.view().more, 2);
  c.advance(TOAST_LIFE_MS);
  m.expire();
  assert.equal(m.view().more, 1, 'the waiting notice is gone, the warn waits');
  m.close(m.view().toasts[0].id);
  assert.equal(m.view().toasts.some((t) => t.level === 'warn'), true);
});

test('the queue is bounded: the rest are only counted', () => {
  const c = clock();
  const m = createToasts({ now: c.now, queueMax: 2 });
  m.push(Array.from({ length: 10 }, () => record('notice')), 'notice');
  assert.equal(m.view().toasts.length, 3);
  assert.equal(m.view().more, 7);
  c.advance(TOAST_LIFE_MS);
  m.expire();
  assert.equal(m.view().more, 0);
  assert.equal(m.view().toasts.length, 0);
});

test('cursorOf: next_after first, else the newest seq, else the fallback', () => {
  assert.equal(cursorOf({ next_after: 12, records: [{ seq: 3 }] }), 12);
  assert.equal(cursorOf({ records: [{ seq: 3 }, { seq: 9 }] }), 9);
  assert.equal(cursorOf({}, 4), 4);
  assert.equal(cursorOf(null), 0);
});

/** A journal like the engine's: forward pages from `after`, newest-first pages. */
function journalOf(records) {
  const calls = [];
  let fail = 0;
  const fetchPage = async (spec) => {
    calls.push(spec);
    if (fail > 0) {
      fail -= 1;
      throw new Error('offline');
    }
    const end = records.length ? records[records.length - 1].seq : 0;
    if (spec.newest_first) {
      return { records: records.slice().reverse().slice(0, spec.limit), next_after: end, more: false, notes: [] };
    }
    const matching = records.filter((r) => r.seq > spec.after);
    const page = matching.slice(0, spec.limit);
    const more = matching.length > page.length;
    return { records: page, next_after: more ? page[page.length - 1].seq : Math.max(end, spec.after), more, notes: [] };
  };
  return { fetchPage, calls, records, failNext: (n) => { fail = n; } };
}

/** Timers run by hand: `run()` fires the pending one. */
function timers() {
  let pending = null;
  let n = 0;
  return {
    setTimer: (fn, ms) => { n += 1; pending = { fn, ms, id: n }; return n; },
    clearTimer: (id) => { if (pending && pending.id === id) pending = null; },
    pending: () => pending,
    async run() {
      const p = pending;
      pending = null;
      if (p) await p.fn();
      return p;
    },
  };
}

const rec = (seq, level = 'notice') => ({ seq, level, source: 'store', kind: 'k', message: `m${seq}`, time: seq });

test('the follower starts at the journal end: old records never pop up', async () => {
  const j = journalOf([rec(1), rec(2), rec(3)]);
  const t = timers();
  const f = createJournalFollower({ fetchPage: j.fetchPage, intervalMs: 2000, setTimer: t.setTimer, clearTimer: t.clearTimer });
  const batches = [];
  f.subscribe((b) => batches.push(b));
  assert.equal(t.pending().ms, 0, 'starts at once');
  await t.run();
  assert.deepEqual(j.calls[0], { newest_first: true, limit: 1 });
  assert.equal(f.status().cursor, 3);
  assert.equal(batches.length, 0);
  assert.equal(t.pending().ms, 2000);
  await t.run();
  assert.equal(batches.length, 0, 'nothing new');
  assert.deepEqual(j.calls[1], { after: 3, limit: 1000 });
  j.records.push(rec(4), rec(5, 'error'));
  await t.run();
  assert.equal(batches.length, 1);
  assert.deepEqual(batches[0].records.map((r) => r.seq), [4, 5]);
  assert.equal(batches[0].from, 3);
  assert.equal(batches[0].to, 5);
});

test('the follower reads several pages when more follow, bounded per poll', async () => {
  const j = journalOf([rec(1)]);
  const t = timers();
  const f = createJournalFollower({ fetchPage: j.fetchPage, setTimer: t.setTimer, clearTimer: t.clearTimer, limit: 2, maxRequests: 2 });
  const seen = [];
  f.subscribe((b) => seen.push(...b.records.map((r) => r.seq)));
  await t.run();
  for (let s = 2; s <= 8; s++) j.records.push(rec(s));
  await t.run();
  assert.deepEqual(seen, [2, 3, 4, 5]);
  await t.run();
  assert.deepEqual(seen, [2, 3, 4, 5, 6, 7, 8]);
});

test('the follower pauses while the page is hidden and resumes on poke', async () => {
  const j = journalOf([rec(1)]);
  const t = timers();
  let visible = false;
  const f = createJournalFollower({ fetchPage: j.fetchPage, isVisible: () => visible, setTimer: t.setTimer, clearTimer: t.clearTimer });
  f.subscribe(() => {});
  await t.run();
  assert.equal(j.calls.length, 0, 'no request while hidden');
  assert.equal(t.pending(), null, 'no timer either');
  visible = true;
  f.poke();
  await t.run();
  assert.equal(j.calls.length, 1);
});

test('the follower never has two requests in flight', async () => {
  let release;
  const calls = [];
  const fetchPage = (spec) => {
    calls.push(spec);
    return new Promise((resolve) => { release = () => resolve({ records: [], next_after: 0, more: false }); });
  };
  const t = timers();
  const f = createJournalFollower({ fetchPage, setTimer: t.setTimer, clearTimer: t.clearTimer });
  f.subscribe(() => {});
  const first = t.run();
  f.poke();
  f.subscribe(() => {});
  assert.equal(t.pending(), null, 'no poll scheduled while one runs');
  release();
  await first;
  assert.equal(calls.length, 1);
  assert.notEqual(t.pending(), null, 'the running poll schedules the next');
});

test('failed polls are silent and back off, then recover', async () => {
  const j = journalOf([rec(1)]);
  const t = timers();
  const f = createJournalFollower({ fetchPage: j.fetchPage, intervalMs: 1000, maxBackoffMs: 5000, setTimer: t.setTimer, clearTimer: t.clearTimer });
  const batches = [];
  f.subscribe((b) => batches.push(b));
  await t.run();
  j.failNext(3);
  await t.run();
  assert.equal(t.pending().ms, 2000);
  await t.run();
  assert.equal(t.pending().ms, 4000);
  await t.run();
  assert.equal(t.pending().ms, 5000, 'capped');
  assert.equal(f.status().failures, 3);
  j.records.push(rec(2));
  await t.run();
  assert.equal(t.pending().ms, 1000, 'back to the interval');
  assert.deepEqual(batches.map((b) => b.records.map((r) => r.seq)), [[2]]);
});

test('the follower stops without subscribers and starts at the end again', async () => {
  const j = journalOf([rec(1)]);
  const t = timers();
  const f = createJournalFollower({ fetchPage: j.fetchPage, setTimer: t.setTimer, clearTimer: t.clearTimer });
  const off = f.subscribe(() => {});
  await t.run();
  off();
  assert.equal(t.pending(), null);
  assert.equal(f.status().cursor, null);
  j.records.push(rec(2), rec(3));
  const batches = [];
  f.subscribe((b) => batches.push(b));
  await t.run();
  assert.equal(f.status().cursor, 3);
  assert.equal(batches.length, 0, 'records written while nobody followed do not pop up');
});

test('a second subscriber shares the poll', async () => {
  const j = journalOf([rec(1)]);
  const t = timers();
  const f = createJournalFollower({ fetchPage: j.fetchPage, setTimer: t.setTimer, clearTimer: t.clearTimer });
  const a = [];
  const b = [];
  f.subscribe((x) => a.push(x));
  await t.run();
  f.subscribe((x) => b.push(x));
  j.records.push(rec(2));
  await t.run();
  assert.equal(j.calls.length, 2, 'one request per poll for both');
  assert.equal(a.length, 1);
  assert.equal(b.length, 1);
});

test('rebase takes the end again and announces only recent records', async () => {
  const j = journalOf([rec(1)]);
  const t = timers();
  const nowMs = 100_000;
  const f = createJournalFollower({ fetchPage: j.fetchPage, setTimer: t.setTimer, clearTimer: t.clearTimer, nowMs: () => nowMs, rebaseWindowMs: 30_000 });
  const batches = [];
  f.subscribe((b) => batches.push(b));
  await t.run();
  // The graph is replaced: a new journal with an old record and one just written (µs times).
  j.records.length = 0;
  j.records.push({ ...rec(1), time: 10_000 * 1000 }, { ...rec(2, 'warn'), time: 95_000 * 1000 });
  f.rebase();
  await t.run();
  assert.deepEqual(j.calls[j.calls.length - 1], { newest_first: true, limit: 50 });
  assert.equal(batches.length, 1);
  assert.deepEqual(batches[0].records.map((r) => r.seq), [2]);
  assert.equal(batches[0].from, null, 'does not continue the previous batch');
  assert.equal(f.status().cursor, 2);
});

test('the panel filters follow the engine: level and above, source prefix', () => {
  const r = { level: 'notice', source: 'store.backup' };
  assert.equal(matchesJournalFilters(r, {}), true);
  assert.equal(matchesJournalFilters(r, { level: 'notice' }), true);
  assert.equal(matchesJournalFilters(r, { level: 'warn' }), false);
  assert.equal(matchesJournalFilters(r, { source: 'store' }), true);
  assert.equal(matchesJournalFilters(r, { source: 'store.backup' }), true);
  assert.equal(matchesJournalFilters(r, { source: 'stor' }), false);
  assert.equal(matchesJournalFilters(r, { source: 'auth' }), false);
});

test('a follower batch continues the panel view in memory, or asks for a read', () => {
  const view = startJournal({ records: [rec(5), rec(4)], next_after: 5, more: false });
  const page = batchAsPage(view, { records: [rec(6, 'info'), rec(7, 'warn')], from: 5, to: 7 }, { level: 'notice' });
  assert.deepEqual(page.records.map((r) => r.seq), [7]);
  const next = addJournalPage(view, page);
  assert.deepEqual(next.records.map((r) => r.seq), [7, 5, 4]);
  assert.equal(next.after, 7);
  // An empty batch still moves the cursor.
  assert.equal(addJournalPage(next, batchAsPage(next, { records: [], from: 7, to: 9 }, {})).after, 9);
  // A gap (the follower ahead of the view) or a rebase: the panel reads by itself.
  assert.equal(batchAsPage(view, { records: [rec(9)], from: 8, to: 9 }, {}), null);
  assert.equal(batchAsPage(view, { records: [rec(9)], from: null, to: 9 }, {}), null);
  // Records the view holds already are not added twice.
  const overlap = batchAsPage(next, { records: [rec(6), rec(7), rec(8)], from: 5, to: 8 }, {});
  assert.deepEqual(overlap.records.map((r) => r.seq), [8]);
});

test('rebase on the same journal does not announce delivered records again', async () => {
  const nowMs = 100_000;
  const recent = (seq) => ({ ...rec(seq), time: 99_000 * 1000 });
  const j = journalOf([recent(1)]);
  const t = timers();
  const f = createJournalFollower({ fetchPage: j.fetchPage, setTimer: t.setTimer, clearTimer: t.clearTimer, nowMs: () => nowMs });
  const seen = [];
  f.subscribe((b) => seen.push(...b.records.map((r) => r.seq)));
  await t.run();
  j.records.push(recent(2), recent(3));
  await t.run();
  assert.deepEqual(seen, [2, 3]);
  // The graph is replaced but the journal stays (a rollback on the dev server): one more record.
  j.records.push(recent(4));
  f.rebase();
  f.rebase(); // twice before the poll: still the old cursor
  await t.run();
  assert.deepEqual(seen, [2, 3, 4], 'records 2 and 3 are not announced twice');
  assert.equal(f.status().cursor, 4);
});
