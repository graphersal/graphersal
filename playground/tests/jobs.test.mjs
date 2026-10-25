// Node tests of the Scheduled jobs manager's model (js/jobs.js): rows from the engine's
// jobs() answer, the form's draft and spec, schedule texts, the Catalog item.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  jobRows, jobsMenuItem, scheduleText, relativeTime, actionText, newJobDraft, draftFromJob,
  jobDraftToSpec, queryChoices, argsSkeleton, formErrors,
} from '../js/jobs.js';

const NOW = 1_800_000_000_000_000;
const MIN = 60e6;

/** The shape `Session::jobs_view()` answers for a store with one job of a user. */
const VIEW = {
  timeZone: 'Europe/Berlin',
  store: true,
  jobs: [
    { name: 'stats_flush', kind: 'system', schedule: '* * * * *', enabled: true, nextAt: NOW + MIN, last: { startedAt: NOW - MIN, micros: 1500, error: null }, scheduled: true, reason: null, runs: { last_two_days: { count: 90, failed: 0 } }, setting: null },
    { name: 'compact', kind: 'system', schedule: '30 4 * * *', enabled: false, nextAt: null, last: null, scheduled: true, reason: null, runs: null, setting: { schedule: '30 4 * * *', enabled: null } },
    { name: 'purge', kind: 'user', schedule: '0 3 * * *', enabled: true, nextAt: NOW + 3 * 3600e6, last: { startedAt: NOW - 2 * 86400e6 - 1, micros: 20000, error: 'the job \'purge\': define the job again' }, scheduled: true, reason: null, runs: { last_two_days: { count: 1, failed: 1 } }, action: 'query', query: 'purge', args: { label: 'event' }, path: null, timeoutMs: 30000, description: 'old events', effects: [{ text: 'Execute definition query purge' }, { text: 'Delete data vertex' }], served: true },
  ],
};

test('rows: system jobs, next and last run, failures, the changed schedule', () => {
  const rows = jobRows(VIEW, NOW);
  assert.deepEqual(rows.map((r) => r.name), ['stats_flush', 'compact', 'purge']);
  assert.equal(rows[0].system, true);
  assert.equal(rows[0].scheduleText, 'Every minute');
  assert.equal(rows[0].next, 'in 1 min');
  assert.equal(rows[0].last, '1 min ago');
  assert.equal(rows[0].lastMs, 2);
  assert.equal(rows[0].action, 'writes the workload statistics');
  assert.equal(rows[1].next, '—');
  assert.equal(rows[1].last, 'never');
  assert.equal(rows[1].changed, true);
  assert.equal(rows[2].action, 'query purge(label: "event")');
  assert.equal(rows[2].scheduleText, 'Daily at 03:00');
  assert.equal(rows[2].next, 'in 3 h');
  assert.equal(rows[2].last, '2 days ago');
  assert.match(rows[2].lastError, /define the job again/);
  assert.deepEqual([rows[2].runs, rows[2].failed], [1, 1]);
  assert.deepEqual(rows[2].effects, ['Execute definition query purge', 'Delete data vertex']);
  assert.deepEqual(jobRows(null), []);
});

test('texts: schedules, relative times, actions, the Catalog item', () => {
  assert.equal(scheduleText('15 2 * * 1'), '15 2 * * 1');
  assert.equal(relativeTime(null, NOW), '—');
  assert.equal(relativeTime(NOW - 30 * MIN, NOW), '30 min ago');
  assert.equal(actionText({ action: 'backup', path: '/b' }), 'backup into /b');
  assert.equal(actionText({ action: 'query', query: 'q', args: {} }), 'query q()');
  assert.deepEqual(jobsMenuItem({ catalog: { job: 2 } }).count, 2);
  assert.equal(jobsMenuItem(null).count, 0);
  // With the jobs loaded: active of all, system jobs included.
  assert.equal(jobsMenuItem({ catalog: { job: 1 } }, VIEW).count, '2/3');
});

test('the form: a draft round trip, the spec and its local errors', () => {
  const draft = draftFromJob(VIEW.jobs[2]);
  assert.equal(draft.replaces, 'purge');
  const { spec } = jobDraftToSpec(draft);
  assert.deepEqual(spec, {
    name: 'purge', schedule: '0 3 * * *', action: 'query', enabled: true, query: 'purge',
    args: { label: 'event' }, timeout_ms: 30000, description: 'old events',
  });
  const bad = { ...newJobDraft(), name: 'x', argsText: '[1]', timeoutText: '1.5' };
  assert.deepEqual(jobDraftToSpec(bad).errors.map((e) => e.field), ['args', 'timeout_ms']);
  const backup = { ...newJobDraft(), name: 'b', action: 'backup', path: ' /b ' };
  assert.deepEqual(jobDraftToSpec(backup).spec, { name: 'b', schedule: '0 3 * * *', action: 'backup', enabled: true, path: '/b' });
  assert.deepEqual(formErrors([{ field: 'run_on', message: 'bad' }, { field: 'path', message: 'p' }]).map((e) => e.field), ['general', 'path']);
});

test('query choices and an arguments skeleton', () => {
  const choices = queryChoices([{ name: 'top', params: [{ name: 'n' }] }, { name: 'all', params: [] }]);
  assert.deepEqual(choices, [{ name: 'all', params: [] }, { name: 'top', params: ['n'] }]);
  assert.equal(argsSkeleton(['n']), '{\n  "n": null\n}');
});
