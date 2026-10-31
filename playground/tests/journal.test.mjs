// Node tests of the System journal panel's model (js/journal.js) and of the dev server engine's
// journal request (js/engine/http-engine.js `journal`).
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  emptyJournal, journalSpec, addJournalPage, readJournal, journalRows, journalFooter, journalTime,
  journalIsoTime, fieldsText, levelClass, sourceChoices, JOURNAL_LEVELS, JOURNAL_PAGE_ROWS,
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

/** A journal of `n` records served page by page like the engine (cursor `after`, `limit`). */
function engineOf(records) {
  const calls = [];
  const fetchPage = async (spec) => {
    calls.push(spec);
    const matching = records.filter((r) => r.seq > spec.after
      && (!spec.source || r.source === spec.source || r.source.startsWith(`${spec.source}.`)));
    const page = matching.slice(0, spec.limit);
    const last = page.length ? page[page.length - 1].seq : spec.after;
    return { records: page, next_after: last, truncated: false, more: matching.length > page.length, notes: [] };
  };
  return { fetchPage, calls };
}

test('the request leaves out empty filters and keeps the cursor', () => {
  assert.deepEqual(journalSpec({}, 0), { after: 0, limit: 1000 });
  assert.deepEqual(journalSpec({ level: 'warn', source: ' store ' }, 7, 50), { after: 7, limit: 50, level: 'warn', source: 'store' });
  assert.deepEqual(journalSpec({ level: '', source: '' }, -3), { after: 0, limit: 1000 });
  assert.equal(JOURNAL_LEVELS[0].name, '');
  assert.deepEqual(JOURNAL_LEVELS.slice(1).map((l) => l.name), ['info', 'notice', 'warn', 'error']);
});

test('pages are added newest first, without duplicates, bounded, with the cursor moved', () => {
  let view = addJournalPage(emptyJournal(), { records: [record(1), record(2)], next_after: 2, more: true, truncated: false, notes: [] });
  assert.deepEqual(view.records.map((r) => r.seq), [2, 1]);
  assert.equal(view.after, 2);
  assert.equal(view.more, true);
  // The same record again (an overlapping read) is not added twice.
  view = addJournalPage(view, { records: [record(2), record(3)], next_after: 3, more: false, notes: ['segment x: skipped'] });
  assert.deepEqual(view.records.map((r) => r.seq), [3, 2, 1]);
  assert.equal(view.more, false);
  assert.deepEqual(view.notes, ['segment x: skipped']);
  // At most `keep` records; the oldest are dropped and counted.
  view = addJournalPage(view, { records: [record(4), record(5)], next_after: 5, truncated: true }, 3);
  assert.deepEqual(view.records.map((r) => r.seq), [5, 4, 3]);
  assert.equal(view.dropped, 2);
  assert.equal(view.truncated, true);
  // A page with nothing new keeps the view and never moves the cursor back.
  const same = addJournalPage(view, { records: [], next_after: 1 });
  assert.equal(same.after, 5);
  assert.equal(same.records.length, 3);
});

test('reading goes on from the cursor and asks only for what is new', async () => {
  const records = Array.from({ length: 2500 }, (_, i) => record(i + 1));
  const engine = engineOf(records);
  let view = await readJournal(emptyJournal(), {}, engine.fetchPage);
  assert.deepEqual(engine.calls.map((c) => c.after), [0, 1000, 2000]);
  assert.equal(view.records.length, 2000, 'the newest 2000 are kept');
  assert.equal(view.records[0].seq, 2500);
  assert.equal(view.dropped, 500);
  records.push(record(2501));
  engine.calls.length = 0;
  view = await readJournal(view, {}, engine.fetchPage);
  assert.deepEqual(engine.calls, [{ after: 2500, limit: 1000 }]);
  assert.equal(view.records[0].seq, 2501);
  // A bounded number of requests per refresh; the next refresh goes on.
  const many = engineOf(Array.from({ length: 50 }, (_, i) => record(i + 1)));
  const partial = await readJournal(emptyJournal(), {}, async (spec) => many.fetchPage({ ...spec, limit: 10 }), 2);
  assert.equal(partial.after, 20);
  assert.equal(partial.more, true);
});

test('filters go to the engine', async () => {
  const engine = engineOf([record(1), record(2, { source: 'auth', kind: 'auth.denied', level: 'warn' }), record(3, { source: 'store.backup' })]);
  const view = await readJournal(emptyJournal(), { source: 'store' }, engine.fetchPage);
  assert.deepEqual(view.records.map((r) => r.seq), [3, 1]);
  assert.equal(engine.calls[0].source, 'store');
});

test('rows: readable times, level badges, fields, at most `shown` of them', () => {
  const view = addJournalPage(emptyJournal(), {
    records: [
      record(1),
      record(2, { level: 'error', source: 'store.backup', kind: 'store.backup_failed', fields: { path: '/b', bytes: 12, nested: { a: [1] } }, principal: 'cli', commit: 9 }),
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

test('the footer counts the rows not shown and what is gone', () => {
  const records = Array.from({ length: JOURNAL_PAGE_ROWS + 5 }, (_, i) => record(i + 1));
  const view = addJournalPage(emptyJournal(), { records, next_after: records.length, truncated: true });
  const footer = journalFooter(view, JOURNAL_PAGE_ROWS);
  assert.equal(footer.hidden, 5);
  assert.match(footer.text, /^5 more · the oldest records are gone/);
  assert.equal(journalFooter(view, JOURNAL_PAGE_ROWS + 5).hidden, 0);
  assert.equal(journalFooter(emptyJournal(), JOURNAL_PAGE_ROWS).text, '');
});

test('source suggestions are the sources and their dotted prefixes', () => {
  const view = addJournalPage(emptyJournal(), {
    records: [record(1, { source: 'store.backup' }), record(2, { source: 'auth' }), record(3)],
    next_after: 3,
  });
  assert.deepEqual(sourceChoices(view), ['auth', 'store', 'store.backup']);
});

test('the dev server engine asks GET /api/journal with the request keys as parameters', async () => {
  const calls = [];
  const page = { records: [record(1)], next_after: 1, truncated: false, more: false, notes: [] };
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    calls.push({ method: init.method || 'GET', path: u.pathname, search: u.searchParams });
    const body = u.pathname === '/api/info'
      ? { server: SERVER_ID, version: '0.5.0', dev: true }
      : page;
    return { ok: true, status: 200, text: async () => JSON.stringify(body) };
  };
  const engine = await HttpEngine.detect({ base: 'http://127.0.0.1:8080/', fetch });
  const answer = await engine.journal({ after: 4, level: 'warn', source: '', limit: 1000 });
  assert.deepEqual(answer, page);
  const call = calls.find((c) => c.path === '/api/journal');
  assert.equal(call.method, 'GET');
  assert.equal(call.search.get('after'), '4');
  assert.equal(call.search.get('level'), 'warn');
  assert.equal(call.search.get('limit'), '1000');
  assert.equal(call.search.has('source'), false, 'empty filters are left out');
});
