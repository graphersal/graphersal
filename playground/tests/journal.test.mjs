// Node tests of the System journal panel's model (js/journal.js) and of the dev server engine's
// journal request (js/engine/http-engine.js `journal`).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  emptyJournal, journalSpec, newestSpec, startJournal, addJournalPage, addOlderPage, openJournal,
  readJournal, showMoreJournal, journalRows, journalFooter, journalTime, journalIsoTime, fieldsText,
  levelClass, sourceChoices, JOURNAL_LEVELS, JOURNAL_PAGE_ROWS,
} from '../js/journal.js';
import { HttpEngine, SERVER_ID } from '../js/engine/http-engine.js';

const T0 = Date.UTC(2026, 9, 31, 9, 15, 2, 345) * 1000 + 678; // µs

/** A record of the shape `Session::journal` answers. */
function record(seq, extra = {}) {
  return {
    seq, origin: { id: '01a19299-de18-7e16-9c2e-c54c014e9a01', name: 'host' }, origin_seq: seq,
    time: T0 + seq * 1_000_000, level: 'info', source: 'store', kind: 'store.opened',
    message: `record ${seq}`, fields: {}, ...extra,
  };
}

/**
 * A journal served like the engine: forward pages from `after` (oldest first), newest-first
 * pages below `before` (newest first, `next_before`); every request recorded.
 */
function engineOf(records) {
  const calls = [];
  const fetchPage = async (spec) => {
    calls.push(spec);
    const end = records.length ? records[records.length - 1].seq : 0;
    const matching = records.filter((r) => r.seq > (spec.after || 0)
      && (spec.before == null || r.seq < spec.before)
      && (!spec.source || r.source === spec.source || r.source.startsWith(`${spec.source}.`)));
    if (spec.newest_first) {
      const newest = matching.slice().reverse();
      const page = newest.slice(0, spec.limit);
      const more = newest.length > page.length;
      return {
        records: page, next_after: spec.before == null ? end : Math.min(end, spec.before - 1),
        next_before: more ? page[page.length - 1].seq : (spec.after || 0) + 1,
        truncated: false, more, notes: [],
      };
    }
    const page = matching.slice(0, spec.limit);
    const more = matching.length > page.length;
    return { records: page, next_after: more ? page[page.length - 1].seq : Math.max(end, spec.after || 0), truncated: false, more, notes: [] };
  };
  return { fetchPage, calls };
}

const seqs = (view) => view.records.map((r) => r.seq);

test('the requests leave out empty filters', () => {
  assert.deepEqual(journalSpec({}, 0), { after: 0, limit: 1000 });
  assert.deepEqual(journalSpec({ level: 'warn', source: ' store ' }, 7, 50), { after: 7, limit: 50, level: 'warn', source: 'store' });
  assert.deepEqual(journalSpec({ level: '', source: '' }, -3), { after: 0, limit: 1000 });
  assert.deepEqual(newestSpec({}), { newest_first: true, limit: JOURNAL_PAGE_ROWS });
  assert.deepEqual(newestSpec({ level: 'error' }, 40, 10), { newest_first: true, limit: 10, before: 40, level: 'error' });
  assert.equal(JOURNAL_LEVELS[0].name, '');
  assert.deepEqual(JOURNAL_LEVELS.slice(1).map((l) => l.name), ['info', 'notice', 'warn', 'error']);
});

test('opening reads only the newest page of a long journal', async () => {
  const engine = engineOf(Array.from({ length: 50_000 }, (_, i) => record(i + 1)));
  const view = await openJournal({}, engine.fetchPage);
  assert.equal(engine.calls.length, 1, 'one request');
  assert.deepEqual(engine.calls[0], { newest_first: true, limit: JOURNAL_PAGE_ROWS });
  assert.equal(view.records.length, JOURNAL_PAGE_ROWS);
  assert.equal(view.records[0].seq, 50_000, 'the newest first');
  assert.equal(view.after, 50_000, 'following goes on from the end');
  assert.equal(view.before, 50_000 - JOURNAL_PAGE_ROWS + 1);
  assert.equal(view.older, true);
});

test('following reads only what is new, in front of the held records', async () => {
  const records = Array.from({ length: 10 }, (_, i) => record(i + 1));
  const engine = engineOf(records);
  let view = await openJournal({}, engine.fetchPage, 4);
  assert.deepEqual(seqs(view), [10, 9, 8, 7]);
  records.push(record(11), record(12));
  engine.calls.length = 0;
  view = await readJournal(view, {}, engine.fetchPage);
  assert.deepEqual(engine.calls, [{ after: 10, limit: 1000 }]);
  assert.deepEqual(seqs(view), [12, 11, 10, 9, 8, 7]);
  assert.equal(view.after, 12);
  // Nothing new: one request, nothing added.
  engine.calls.length = 0;
  view = await readJournal(view, {}, engine.fetchPage);
  assert.equal(engine.calls.length, 1);
  assert.equal(view.records.length, 6);
  // A bounded number of requests per refresh; the next refresh goes on.
  const many = engineOf(Array.from({ length: 50 }, (_, i) => record(i + 1)));
  const partial = await readJournal(emptyJournal(), {}, async (spec) => many.fetchPage({ ...spec, limit: 10 }), 2);
  assert.equal(partial.after, 20);
  assert.equal(partial.more, true);
});

test('following keeps at most `keep` records; the dropped ones become older records again', () => {
  let view = startJournal({ records: [record(3), record(2), record(1)], next_after: 3, next_before: 1, more: false });
  assert.equal(view.older, false);
  view = addJournalPage(view, { records: [record(4), record(5)], next_after: 5, more: false }, 3);
  assert.deepEqual(seqs(view), [5, 4, 3]);
  assert.equal(view.older, true);
  assert.equal(view.before, 3);
  // A record already held is not added twice; the cursor never goes back.
  view = addJournalPage(view, { records: [record(5)], next_after: 2, notes: ['segment x: skipped'] }, 3);
  assert.deepEqual(seqs(view), [5, 4, 3]);
  assert.equal(view.after, 5);
  assert.deepEqual(view.notes, ['segment x: skipped']);
});

test('show more renders the held rows first, then pages backwards with `before`', async () => {
  const engine = engineOf(Array.from({ length: 1000 }, (_, i) => record(i + 1)));
  let view = await openJournal({}, engine.fetchPage, 400);
  let shown = JOURNAL_PAGE_ROWS;
  engine.calls.length = 0;
  ({ view, shown } = await showMoreJournal(view, shown, {}, engine.fetchPage));
  assert.equal(shown, 2 * JOURNAL_PAGE_ROWS);
  assert.deepEqual(engine.calls, [{ newest_first: true, limit: 200, before: 601 }]);
  assert.equal(view.records.length, 600);
  assert.equal(view.records[599].seq, 401);
  assert.ok(view.records.every((r, i) => i === 0 || r.seq < view.records[i - 1].seq), 'newest first');
  engine.calls.length = 0;
  ({ view, shown } = await showMoreJournal(view, shown, {}, engine.fetchPage));
  ({ view, shown } = await showMoreJournal(view, shown, {}, engine.fetchPage));
  ({ view, shown } = await showMoreJournal(view, shown, {}, engine.fetchPage));
  assert.equal(view.records.length, 1000);
  assert.equal(view.older, false);
  assert.equal(journalFooter(view, shown).more, false);
  // Nothing more to read: no request.
  engine.calls.length = 0;
  ({ view } = await showMoreJournal(view, shown, {}, engine.fetchPage));
  assert.equal(engine.calls.length, 0);
  // Held rows not yet shown need no request either.
  const held = startJournal({ records: Array.from({ length: 500 }, (_, i) => record(500 - i)), next_after: 500, next_before: 1, more: false });
  const local = await showMoreJournal(held, JOURNAL_PAGE_ROWS, {}, engine.fetchPage);
  assert.equal(engine.calls.length, 0);
  assert.equal(local.shown, 2 * JOURNAL_PAGE_ROWS);
  // An older page never adds a record twice.
  const again = addOlderPage(held, { records: [record(3), record(1)], next_before: 1, more: false });
  assert.equal(again.records.length, 500);
});

test('filters go to every request', async () => {
  const engine = engineOf([record(1), record(2, { source: 'auth', kind: 'auth.denied', level: 'warn' }), record(3, { source: 'store.backup' })]);
  const view = await openJournal({ source: 'store' }, engine.fetchPage);
  assert.deepEqual(seqs(view), [3, 1]);
  await readJournal(view, { source: 'store' }, engine.fetchPage);
  assert.ok(engine.calls.every((c) => c.source === 'store'));
});

test('rows: readable times, level badges, fields, at most `shown` of them', () => {
  const view = startJournal({
    records: [
      record(2, { level: 'error', source: 'store.backup', kind: 'store.backup_failed', fields: { path: '/b', bytes: 12, nested: { a: [1] } }, principal: 'cli', commit: 9 }),
      record(1),
    ],
    next_after: 2,
  });
  const rows = journalRows(view, 10, true);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].seq, 2);
  assert.equal(rows[0].time, '2026-10-31 09:15:04.345');
  assert.equal(rows[0].iso, '2026-10-31T09:15:04.345678Z');
  assert.equal(rows[0].levelClass, 'badge danger');
  assert.equal(rows[0].fields, 'path=/b, bytes=12, nested={"a":[1]}');
  assert.equal(rows[0].extra, 'by cli · commit 9');
  assert.equal(rows[0].origin, 'host #2');
  assert.equal(rows[1].fields, '');
  assert.equal(journalRows(view, 1).length, 1);
  assert.equal(levelClass('warn'), 'badge warn');
  assert.equal(levelClass('notice'), 'badge accent');
  assert.equal(levelClass('info'), 'badge');
  assert.equal(journalTime(undefined), '');
  assert.equal(journalIsoTime(0), '1970-01-01T00:00:00.000000Z');
  assert.equal(fieldsText(null), '');
});

test('the footer counts the rows not shown and says what is older or gone', () => {
  const records = Array.from({ length: JOURNAL_PAGE_ROWS + 5 }, (_, i) => record(JOURNAL_PAGE_ROWS + 5 - i));
  const view = startJournal({ records, next_after: records.length, next_before: 1, truncated: true, more: false });
  const footer = journalFooter(view, JOURNAL_PAGE_ROWS);
  assert.equal(footer.hidden, 5);
  assert.equal(footer.more, true);
  assert.match(footer.text, /^5 more loaded · the oldest records are gone/);
  assert.equal(journalFooter(view, JOURNAL_PAGE_ROWS + 5).more, false);
  const older = journalFooter({ ...view, older: true }, JOURNAL_PAGE_ROWS + 5);
  assert.equal(older.more, true);
  assert.match(older.text, /older records in the journal/);
  assert.equal(journalFooter(emptyJournal(), JOURNAL_PAGE_ROWS).text, '');
});

test('source suggestions are the sources and their dotted prefixes', () => {
  const view = startJournal({
    records: [record(3), record(2, { source: 'auth' }), record(1, { source: 'store.backup' })],
    next_after: 3,
  });
  assert.deepEqual(sourceChoices(view), ['auth', 'store', 'store.backup']);
});

test('the dev server engine asks GET /api/journal with the request keys as parameters', async () => {
  const calls = [];
  const page = { records: [record(1)], next_after: 1, next_before: 1, truncated: false, more: false, notes: [] };
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    calls.push({ method: init.method || 'GET', path: u.pathname, search: u.searchParams });
    const body = u.pathname === '/api/info'
      ? { server: SERVER_ID, version: '0.5.0', dev: true }
      : page;
    return { ok: true, status: 200, text: async () => JSON.stringify(body) };
  };
  const engine = await HttpEngine.detect({ base: 'http://127.0.0.1:8080/', fetch });
  const answer = await engine.journal({ newest_first: true, before: 40, level: 'warn', source: '', limit: 300 });
  assert.deepEqual(answer, page);
  const call = calls.find((c) => c.path === '/api/journal');
  assert.equal(call.method, 'GET');
  assert.equal(call.search.get('newest_first'), 'true');
  assert.equal(call.search.get('before'), '40');
  assert.equal(call.search.get('level'), 'warn');
  assert.equal(call.search.get('limit'), '300');
  assert.equal(call.search.has('source'), false, 'empty filters are left out');
});
