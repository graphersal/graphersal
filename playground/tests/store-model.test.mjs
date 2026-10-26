// Node tests of the Store menu's data model (js/store-model.js): targets, "Go to commit/time",
// the rollback confirmation, the default fork directory, the attic rows and the changes summary.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  EXPLAIN, ACTIONS, commitTarget, markTarget, targetText, targetLabel, fileSafe, baseName,
  defaultForkDir, parseCommit, parseTime, rollbackPlan, atticRows, countsText, changesModel,
  viewBanner, actionsFor, storeWhere, databaseRows, CHANGES_PAGE, backupBanner, compactionModel,
  STORE_TASKS, storeTask, elapsedText, runningTask, isRunning, waitTitle, taskButton, storeButtonText,
  taskStartText, taskEndText, actionDisabled,
} from '../js/store-model.js';

/** The `GET /api/store` answer of a store at commit 5 with a mark at 2 and one attic entry. */
const STORE = {
  dir: '/home/me/data',
  commitSeq: 5,
  marks: [{ name: 'before-import', commitSeq: 2, timeText: '2026-10-08T02:43:00Z' }],
  snapshots: [{ commitSeq: 0 }, { commitSeq: 3, name: 'nightly' }],
  attic: [
    { id: '20261008T010000Z-6', target: 5, firstCommitSeq: 6, lastCommitSeq: 9, timeText: '2026-10-08T01:00:00Z', reason: 'rollback to commit 5', bytes: 2048, bytesText: '2.0 KiB', restorable: false, restoreBlocked: 'the store has new commits since the rollback' },
    { id: '20261008T020000Z-4', target: 3, firstCommitSeq: 4, lastCommitSeq: 4, timeText: '2026-10-08T02:00:00Z', reason: 'rollback to mark "x"', bytes: 100, bytesText: '100 B', restorable: true, restoreBlocked: null },
  ],
};

test('the menu explains snapshots, marks and the attic, and offers three actions per target', () => {
  assert.match(EXPLAIN.snapshot, /full copy of the graph at a commit/);
  assert.match(EXPLAIN.mark, /any commit can be restored, not only snapshots/);
  assert.deepEqual(ACTIONS.map((a) => a.label), ['Open read-only here', 'Fork to a new directory…', 'Roll back here…']);
});

test('targets read as text and as file-safe labels', () => {
  assert.deepEqual(commitTarget('7'), { commit: 7 });
  assert.deepEqual(markTarget('m1'), { mark: 'm1' });
  assert.equal(targetText({ commit: 7 }), 'commit 7');
  assert.equal(targetText({ mark: 'm1' }), 'mark "m1"');
  assert.equal(targetText({ time: '2026-10-08T02:43Z' }), 'the last commit at or before 2026-10-08T02:43Z');
  assert.equal(targetLabel({ commit: 7 }), 'commit-7');
  assert.equal(targetLabel({ time: '2026-10-08T02:43:00Z' }), 'time-20261008T024300Z');
  assert.equal(fileSafe('mark-a b/c'), 'mark-a-b-c');
  assert.equal(baseName('/home/me/data/'), 'data');
  assert.equal(baseName('C:\\work\\data'), 'data');
  assert.equal(defaultForkDir('/home/me/data', targetLabel({ mark: 'before import' })), 'data-fork-mark-before-import');
});

test('Go to commit takes a number up to the current commit', () => {
  assert.deepEqual(parseCommit(' #3 ', 5), { target: { commit: 3 } });
  assert.deepEqual(parseCommit('0', 5), { target: { commit: 0 } });
  assert.match(parseCommit('6', 5).error, /at commit 5/);
  assert.match(parseCommit('three', 5).error, /commit number/);
  assert.match(parseCommit('', 5).error, /commit number/);
  assert.match(parseCommit('1e3', 5).error, /commit number/);
});

test('Go to time keeps the typed precision and adds the local offset or keeps a given zone', () => {
  // A datetime-local value (no zone) gets the machine's offset at that date.
  assert.deepEqual(parseTime('2026-10-08T04:43', 120), { target: { time: '2026-10-08T04:43+02:00' } });
  assert.deepEqual(parseTime('2026-10-08T04:43:10', -330), { target: { time: '2026-10-08T04:43:10-05:30' } });
  assert.deepEqual(parseTime('2026-10-08T02:43', 0), { target: { time: '2026-10-08T02:43+00:00' } });
  // RFC 3339 with a zone (pasted from a listing) passes unchanged.
  assert.deepEqual(parseTime('2026-10-08T02:43:00Z', 120), { target: { time: '2026-10-08T02:43:00Z' } });
  assert.deepEqual(parseTime('2026-10-08 02:43:00.5+01:00'), { target: { time: '2026-10-08T02:43:00.5+01:00' } });
  assert.deepEqual(parseTime('1760000000000000'), { target: { time: 1760000000000000 } });
  assert.match(parseTime('2026-10-08T02:43').error, /zone/);
  assert.match(parseTime('', 0).error, /Choose/);
  assert.match(parseTime('yesterday', 0).error, /not a date-time/);
});

test('the rollback dialog says what moves to the attic and what the live graph becomes', () => {
  const plan = rollbackPlan(STORE, { mark: 'before-import' });
  assert.equal(plan.title, 'Roll back the store to commit 2?');
  assert.equal(plan.commit, 2);
  assert.match(plan.lines[0], /^Everything after commit 2 \(commits 3\.\.5\) moves to the attic\. You can restore it as long as nothing new is committed or marked/);
  assert.match(plan.lines[1], /live graph switches to commit 2/);
  assert.match(plan.lines[1], /cancelled/);
  assert.equal(plan.nothingToDo, false);
  const del = rollbackPlan(STORE, { commit: 1 }, true);
  assert.match(del.lines[0], /DELETED for good\. This cannot be undone\./);
  assert.match(del.lines[0], /commits 2\.\.5/);
  // A time target: its commit is known only to the server.
  const time = rollbackPlan(STORE, { time: '2026-10-08T02:43Z' });
  assert.equal(time.commit, null);
  assert.match(time.title, /the last commit at or before 2026-10-08T02:43Z/);
  assert.equal(rollbackPlan(STORE, { commit: 5 }).nothingToDo, true);
});

test('attic rows: newest first, Restore enabled only while allowed, with the reason otherwise', () => {
  const rows = atticRows(STORE);
  assert.deepEqual(rows.map((r) => r.range), ['commit 4', 'commits 6..9']);
  assert.equal(rows[0].restorable, true);
  assert.equal(rows[0].blocked, null);
  assert.equal(rows[1].restorable, false);
  assert.match(rows[1].blocked, /new commits/);
  assert.equal(rows[1].size, '2.0 KiB');
  assert.deepEqual(atticRows(null), []);
});

test('the changes of an attic entry are summarized per commit and in total', () => {
  assert.equal(countsText({ addVertex: 2, setProperty: 1, dropEdge: 0 }), '2 vertices added, 1 property written');
  assert.equal(countsText({}), 'no changes');
  const model = changesModel({
    id: 'x', commitCount: 2, mutationCount: 3, truncated: false,
    totals: { addVertex: 2, setProperty: 1 },
    commits: [
      { commitSeq: 4, timeText: '2026-10-08T02:00:00Z', principal: 'ann', mutations: 2, counts: { addVertex: 1, setProperty: 1 }, samples: ['add vertex 7 (person)', 'set "name" on vertex 7'] },
      { commitSeq: 5, timeText: '2026-10-08T02:01:00Z', principal: null, mutations: 1, counts: { addVertex: 1 }, samples: ['add vertex 8'] },
    ],
  });
  assert.equal(model.headline, '2 commits, 3 changes: 2 vertices added, 1 property written');
  assert.deepEqual(model.rows[0], { commit: 4, when: '2026-10-08T02:00:00Z', by: 'ann', what: '1 vertex added, 1 property written', samples: ['add vertex 7 (person)', 'set "name" on vertex 7'] });
  assert.equal(model.rows[1].by, '');
  assert.equal(model.page, 'commits 4..5 of 2');
  assert.equal(model.prevOffset, null);
  assert.equal(model.nextOffset, null);
});

test('the changes come page by page: the range, Previous and Next', () => {
  const commits = (from, n) => Array.from({ length: n }, (_, i) => ({ commitSeq: from + i, timeText: '-', counts: { addVertex: 1 }, samples: [] }));
  const first = changesModel({ commitCount: 250, mutationCount: 250, totals: { addVertex: 250 }, offset: 0, limit: CHANGES_PAGE, commits: commits(19, 100), hasMore: true });
  assert.equal(first.page, 'commits 19..118 of 250');
  assert.equal(first.headline, '250 commits, 250 changes: 250 vertices added');
  assert.equal(first.prevOffset, null);
  assert.equal(first.nextOffset, 100);
  const last = changesModel({ commitCount: 250, mutationCount: 250, totals: {}, offset: 200, limit: 100, commits: commits(219, 50), hasMore: false });
  assert.equal(last.page, 'commits 219..268 of 250');
  assert.equal(last.prevOffset, 100);
  assert.equal(last.nextOffset, null);
  assert.equal(changesModel({ commitCount: 3, offset: 50, limit: 100, commits: [] }).page, 'no commits from #51 on (3 in all)');
});

test('a store in memory forks into databases and says it is lost on reload', () => {
  assert.equal(actionsFor(true)[1].label, 'Fork into a new database…');
  assert.equal(actionsFor(false)[1].label, 'Fork to a new directory…');
  // A backup offers nothing that writes: views and forks only.
  assert.deepEqual(actionsFor(false, true).map((a) => a.id), ['view', 'fork']);
  assert.deepEqual(actionsFor(false).map((a) => a.id), ['view', 'fork', 'rollback']);
  const banner = backupBanner({ commitSeq: 7, takenText: 'T', restore: 'graphersal store restore bk' });
  assert.match(banner, /up to commit 7/);
  assert.match(banner, /graphersal store restore bk/);
  assert.equal(backupBanner(null), '');
  assert.match(storeWhere({ backup: { commitSeq: 7 } }), /backup/);
  assert.match(storeWhere({ memory: true }), /lost on page reload/);
  assert.match(storeWhere({ memory: false }), /on disk/);
  assert.match(rollbackPlan({ memory: true, commitSeq: 5, marks: [] }, { commit: 2 }).lines[0], /new database/);
  const rows = databaseRows({
    databases: [
      { id: 'main', label: 'Modern (TinkerPop)', active: false, commitSeq: 4 },
      { id: 'fork-1', label: 'fork of Modern (TinkerPop) @ commit 2', active: true, commitSeq: 3 },
    ],
  });
  assert.deepEqual(rows.map((r) => [r.id, r.active, r.canClose, r.commit]), [['main', false, true, 4], ['fork-1', true, false, 3]]);
  assert.deepEqual(databaseRows(null), []);
});

test('the view banner names the commit and the time of its last commit', () => {
  assert.equal(viewBanner(null), '');
  assert.equal(
    viewBanner({ commitSeq: 2, label: 'Read-only view of commit 2 (mark "m1")', timeText: '2026-10-08T02:43:00Z' }),
    'Read-only view of commit 2 (mark "m1") (last commit 2026-10-08T02:43:00Z)',
  );
  assert.equal(viewBanner({ commitSeq: 0, timeText: '-' }), 'Read-only view of commit 0');
});

test('the disk space block follows the compaction advice; Compact asks first when not recommended', () => {
  assert.equal(compactionModel(null), null);
  assert.equal(compactionModel({ memory: true }), null);
  assert.match(storeWhere({ kind: 'file' }), /One file on disk/);
  const file = compactionModel({
    kind: 'file',
    compaction: {
      recommended: true, reason: 'a compaction would give back 80.0 MiB (62 % of the store)',
      compactable: true, totalBytesText: '128.0 MiB', liveBytesText: '48.0 MiB',
      garbageBytesText: '60.0 MiB', garbageRatio: 0.47, pruneFirst: true, pruneBytesText: '20.0 MiB',
      pruneSnapshots: [3], pruneSegments: 2, spaceNeededText: '28.0 MiB', freeSpaceText: '9.0 GiB',
      reclaimableBytesText: '80.0 MiB',
    },
  });
  assert.equal(file.recommended, true);
  assert.equal(file.headline, 'Compact recommended');
  assert.equal(file.lines[0], 'live 48.0 MiB · free space inside 60.0 MiB (47 %)');
  // The store's size is shown once, above the block (storeBytesText): never repeated here.
  for (const line of [file.reason, ...file.lines]) assert.doesNotMatch(line, /128\.0 MiB/);
  assert.match(file.lines[1], /A prune frees 20.0 MiB first \(1 snapshot\(s\), 2 WAL segment\(s\)\)/);
  assert.match(file.lines[2], /9.0 GiB are free/);
  assert.match(file.title, /rewrite the file/);
  assert.equal(file.confirm, null);
  assert.doesNotMatch(file.title, /unreachable/);
  const marks = compactionModel({
    kind: 'directory',
    compaction: {
      recommended: true, reason: 'a prune would give back 20.0 MiB', compactable: false,
      pruneFirst: true, pruneBytesText: '20.0 MiB', pruneSnapshots: [0, 3], pruneSegments: 2,
      pruneMarksUnreachable: ['release-1', 'before-import'], reclaimableBytesText: '20.0 MiB',
    },
  });
  assert.match(marks.lines[2], /mark\(s\) "release-1", "before-import": they can no longer be opened/);
  assert.match(marks.title, /The mark\(s\) release-1, before-import become unreachable \(Compact asks first\)\.$/);
  // Recommended, but marks become unreachable: Compact ALWAYS asks, naming them, and sends
  // dropMarks after the confirmation.
  assert.equal(
    marks.confirm,
    'Compacting makes the mark(s) "release-1", "before-import" unreachable: the history they need is removed, ' +
      'so they cannot be opened, forked or rolled back to afterwards (only from a backup that holds older snapshots). ' +
      'It frees about 20.0 MiB. Compact anyway?',
  );
  assert.equal(marks.dropMarks, true);
  // Recommended, no mark affected: no question (unchanged), nothing to drop.
  assert.equal(file.dropMarks, false);
  const memory = compactionModel({
    memory: true,
    compaction: { recommended: false, reason: 'the store is smaller than the minimum of 64.0 MiB', compactable: false, pruneFirst: false },
  });
  assert.equal(memory.recommended, false);
  assert.equal(memory.headline, 'No compaction needed');
  assert.match(memory.lines[0], /In memory: Compact is a prune/);
  assert.match(memory.title, /^Not recommended now: the store is smaller/);
  assert.equal(
    memory.confirm,
    'Compacting frees about 0 B; not recommended: the store is smaller than the minimum of 64.0 MiB. Compact anyway?',
  );
  const notWorth = compactionModel({
    kind: 'file',
    compaction: {
      recommended: false, reason: 'a compaction would give back 3.0 MiB (5 % of the store)', compactable: true,
      totalBytesText: '64.0 MiB', liveBytesText: '61.0 MiB', garbageBytesText: '3.0 MiB', garbageRatio: 0.05,
      pruneFirst: false, spaceNeededText: '61.0 MiB', reclaimableBytesText: '3.0 MiB',
      pruneMarksUnreachable: ['m1'],
    },
  });
  assert.equal(
    notWorth.confirm,
    'Compacting makes the mark(s) "m1" unreachable: the history they need is removed, so they cannot be opened, ' +
      'forked or rolled back to afterwards (only from a backup that holds older snapshots). ' +
      'It frees about 3.0 MiB; not recommended: a compaction would give back 3.0 MiB (5 % of the store). Compact anyway?',
  );
  assert.equal(notWorth.dropMarks, true);
  assert.equal(memory.dropMarks, false);
});

// ---- a running store action (P15): the item's running form, the Store button, the tooltips

/** `state.storeTask` of an action started 42 s ago. */
const running = (id, key = null) => ({ id, key, startedAt: 1_000_000, now: 1_042_400 });

test('elapsedText is a clock', () => {
  assert.equal(elapsedText(0), '0:00');
  assert.equal(elapsedText(7_900), '0:07');
  assert.equal(elapsedText(42_000), '0:42');
  assert.equal(elapsedText(725_000), '12:05');
  assert.equal(elapsedText(3_723_000), '1:02:03');
  assert.equal(elapsedText(-5), '0:00');
  assert.equal(elapsedText(undefined), '0:00');
});

test('every store action has an idle label, a running text and a short word', () => {
  for (const id of ['checkpoint', 'mark', 'verify', 'compact', 'backup', 'backupFromMemory', 'view', 'live',
    'fork', 'atticFork', 'rollback', 'restore', 'atticRemove', 'switch', 'close']) {
    const t = STORE_TASKS[id];
    assert.ok(t && t.label && t.running.endsWith('…') && t.short, id);
  }
  assert.equal(STORE_TASKS.verify.running, 'Verifying the store…');
  assert.equal(STORE_TASKS.compact.running, 'Compacting…');
  assert.equal(storeTask('nope').label, 'nope');
});

test('runningTask: the running text and the elapsed time per action; nothing when idle', () => {
  assert.equal(runningTask(null), null);
  assert.deepEqual(runningTask(running('verify')), {
    id: 'verify', label: 'Verify the store', running: 'Verifying the store…', short: 'verifying', elapsed: '0:42',
  });
  assert.equal(runningTask(running('backup')).running, 'Backing up…');
  assert.equal(runningTask(running('rollback')).short, 'rolling back');
  assert.equal(runningTask({ id: 'mark', startedAt: 5 }).elapsed, '0:00');
});

test('isRunning: only the item of the running action, and with a key only that item', () => {
  assert.equal(isRunning(null, 'verify'), false);
  assert.equal(isRunning(running('verify'), 'verify'), true);
  assert.equal(isRunning(running('verify'), 'compact'), false);
  assert.equal(isRunning(running('restore', 'a1'), 'restore', 'a1'), true);
  assert.equal(isRunning(running('restore', 'a1'), 'restore', 'a2'), false);
  assert.equal(isRunning(running('restore', 'a1'), 'restore'), true);
});

test('taskButton: the running item is marked busy; the others say to wait', () => {
  const idle = taskButton(null, 'verify', 'Check every checksum');
  assert.deepEqual(idle, { class: { 'store-running': false }, 'aria-busy': 'false', title: 'Check every checksum' });
  const self = taskButton(running('verify'), 'verify', 'Check every checksum');
  assert.deepEqual(self, { class: { 'store-running': true }, 'aria-busy': 'true', title: 'Verifying the store… 0:42' });
  const other = taskButton(running('verify'), 'compact', 'Compact the file');
  assert.equal(other['aria-busy'], 'false');
  assert.equal(other.title, 'Wait for "Verify the store" to finish (0:42).');
  assert.equal(taskButton(running('restore', 'a1'), 'restore', 'Undo', 'a2').title, 'Wait for "Restore" to finish (0:42).');
  assert.equal(waitTitle(null, 'x'), 'x');
  assert.equal(waitTitle(running('backup'), 'x'), 'Wait for "Back up" to finish (0:42).');
});

test('items that conflict are disabled while an action runs (the menu locks on state.storeTask)', () => {
  // The menu passes `locked` = a store action or a query runs; every action is disabled then.
  for (const id of ['view', 'fork', 'rollback', 'compact']) {
    assert.equal(actionDisabled({ ...STORE, commitSeq: 9 }, id, 1, true), true, id);
  }
  assert.equal(actionDisabled({ ...STORE, commitSeq: 9 }, 'view', 1, false), false);
});

test('storeButtonText: the position when idle, the running action with its clock otherwise', () => {
  assert.equal(storeButtonText(null, null, null), 'Store ▾');
  assert.equal(storeButtonText({ commitSeq: 12 }, null, null), 'Store · #12 ▾');
  assert.equal(storeButtonText({ commitSeq: 12 }, null, running('verify')), 'Store · verifying… 0:42 ▾');
  assert.equal(storeButtonText({ commitSeq: 12 }, null, running('compact')), 'Store · compacting… 0:42 ▾');
  assert.equal(storeButtonText({ commitSeq: 12 }, { commitSeq: 3 }, null), 'Store · #12 · viewing #3 ▾');
  assert.equal(
    storeButtonText({ commitSeq: 12, damageFound: [{}], backup: {}, stopped: 'x' }, null, null),
    'Store · #12 · DAMAGED · BACKUP · STOPPED ▾',
  );
  assert.equal(storeButtonText({ commitSeq: 12, maintenance: {} }, null, running('backupFromMemory')), 'Store · backing up… 0:42 · DAMAGED ▾');
});

test('the live region announces the start and the end', () => {
  assert.equal(taskStartText('verify'), 'Verifying the store started.');
  assert.equal(taskEndText('verify', 42_000, true), 'Verify the store finished after 0:42.');
  assert.equal(taskEndText('backup', 61_000, false), 'Back up failed after 1:01.');
});

// Embedding models kept in the store (format spec 4.5).
import { modelRows, offersWithoutModels, snapshotFileName } from '../js/store-model.js';

test('the menu lists the store models with their licences and the models the schema misses', () => {
  const store = {
    models: [
      { id: 'model2vec-0123456789ab', identity: 'model2vec/acme/tiny@0123456789ab', bytesText: '1.0 MiB', license: 'mit', licenseText: 'template', gated: false, usedBySchema: true, problem: null },
      { id: 'model2vec-aaaaaaaaaaaa', identity: 'model2vec/acme/gated@aaaaaaaaaaaa', bytesText: '2.0 MiB', license: 'other', licenseText: 'file', gated: true, usedBySchema: false, problem: null },
    ],
    missingModels: ['model2vec/acme/gone@bbbbbbbbbbbb'],
  };
  const rows = modelRows(store);
  assert.equal(rows.length, 3);
  assert.equal(rows[0].licence, 'mit');
  assert.match(rows[0].licenceText, /standard licence text/);
  assert.equal(rows[0].used, true);
  assert.equal(rows[1].gated, true);
  assert.match(rows[1].licenceText, /from the repository/);
  assert.equal(rows[2].missing, true);
  assert.match(rows[2].problem, /does not keep it/);
  assert.equal(offersWithoutModels(store), true);
  assert.equal(offersWithoutModels({ models: [] }), false);
  assert.deepEqual(modelRows({}), []);
  assert.match(EXPLAIN.model, /redistributes/);
  assert.equal(snapshotFileName(7), 'graph-7.gsnap');
  assert.equal(snapshotFileName(7, false), 'graph-7-without-models.gsnap');
});
