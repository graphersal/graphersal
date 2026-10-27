// The data model of the Store menu (dev server on a Store directory, `graphersal --graph <dir>
// --server`, or the WebAssembly playground's store in memory): targets (a snapshot, a mark, a
// commit number, a time), what each action does to them, the texts of the confirmation dialogs,
// the attic rows, the pages of an attic entry's changes, and the databases of the session.
// Pure functions, no Vue, no DOM (tested under Node: tests/store-model.test.mjs).
//
// A target is the JSON the server takes: `{commit: N}`, `{mark: "name"}` or `{time: "RFC 3339"}`
// (crates/graphersal-cli/src/server/store.rs). Every commit can be a target, not only the
// snapshots and marks; a snapshot only makes loading that state fast.

/** The one-line explanations shown in the menu. */
export const EXPLAIN = Object.freeze({
  snapshot: 'Snapshot: a full copy of the graph at a commit (fast start).',
  mark: 'Mark: a name for a position in the history; any commit can be restored, not only snapshots.',
  attic: 'Attic: the history a rollback moved aside. Restore it while nothing new was committed or marked; fork it any time.',
  database: 'Database: the store you loaded and the forks made from it, each in memory with its own history. Switch to one to query it.',
  model: 'Embedding models kept in the store with their licences: every copy carries them, so handing the store or a snapshot on redistributes them.',
});

/** The line that says where the store lives (memory: lost on reload; disk: durable). */
export function storeWhere(store) {
  if (store && store.backup) return 'A backup on disk: read-only, nothing here changes it.';
  if (store && store.memory) return 'In memory: everything is lost on page reload. Download a .gsnap to keep it.';
  if (store && store.kind === 'file') {
    return 'One file on disk (a single-file store): every commit is in it before its query answers; it survives a restart.';
  }
  return 'Every commit is on disk before its query answers; it survives a restart.';
}

const percent = (ratio) => `${Math.round((Number(ratio) || 0) * 100)} %`;

/**
 * The "Disk space" block of the Store menu, from the store's compaction advice
 * (`store.compaction`, see `GET /api/store/compaction`): a headline, the reason, the numbers,
 * and whether the Compact button is enabled (only when a compaction is recommended). `null`
 * without an advice.
 */
export function compactionModel(store) {
  const a = store && store.compaction;
  if (!a) return null;
  const lines = [];
  if (a.compactable) {
    // The store's size (the whole file) is shown once, in the block above (`storeBytesText`,
    // the same figure as the advice's total): here only how it splits.
    lines.push(`live ${a.liveBytesText} · free space inside ${a.garbageBytesText} (${percent(a.garbageRatio)})`);
  } else {
    lines.push(`${store.memory ? 'In memory' : 'A directory'}: Compact is a prune (a removal frees its space at once).`);
  }
  if (a.pruneFirst) {
    const snapshots = (a.pruneSnapshots || []).length;
    lines.push(`A prune frees ${a.pruneBytesText} first (${snapshots} snapshot(s), ${a.pruneSegments} WAL segment(s)).`);
  }
  const lost = a.pruneMarksUnreachable || [];
  if (lost.length) {
    lines.push(`Its history is gone afterwards for the mark(s) ${lost.map((m) => `"${m}"`).join(', ')}: they can no longer be opened, forked or rolled back to.`);
  }
  if (a.compactable) {
    lines.push(`A compaction needs ${a.spaceNeededText} of free disk space${a.freeSpaceText ? `; ${a.freeSpaceText} are free` : ''}.`);
  }
  return {
    recommended: !!a.recommended,
    headline: a.recommended ? 'Compact recommended' : 'No compaction needed',
    reason: a.reason,
    lines,
    title: a.recommended
      ? `Prune up to the current commit${a.compactable ? ', then rewrite the file without its free space (queries wait meanwhile)' : ''}: gives back about ${a.reclaimableBytesText}.${lost.length ? ` The mark(s) ${lost.join(', ')} become unreachable (Compact asks first).` : ''}`
      : `Not recommended now: ${a.reason}. Compact anyway asks first.`,
    // The question Compact asks before running (null: run at once): always when marks become
    // unreachable (recommended or not; a mark is never lost without a question), and when the
    // advice does not recommend it.
    confirm: compactQuestion(a, lost),
    // Marks the confirmed Compact drops: the request carries `dropMarks` then (the session
    // refuses with 409 and the list without it).
    dropMarks: lost.length > 0,
  };
}

/** The question of `compactionModel(..).confirm`. */
function compactQuestion(a, lost) {
  if (!lost.length && a.recommended) return null;
  const parts = [];
  if (lost.length) {
    parts.push(
      `Compacting makes the mark(s) ${lost.map((m) => `"${m}"`).join(', ')} unreachable: the history they need is removed, ` +
      'so they cannot be opened, forked or rolled back to afterwards (only from a backup that holds older snapshots).',
    );
  }
  if (!a.recommended) {
    parts.push(`${lost.length ? 'It frees' : 'Compacting frees'} about ${a.reclaimableBytesText || '0 B'}; not recommended: ${a.reason}.`);
  } else {
    parts.push(`It frees about ${a.reclaimableBytesText || '0 B'}.`);
  }
  parts.push('Compact anyway?');
  return parts.join(' ');
}

/**
 * The actions on a target, in menu order; `memory`: forks are databases in memory; `readOnly`:
 * a backup directory, which offers nothing that writes (no rollback).
 */
export function actionsFor(memory, readOnly = false) {
  const actions = [
    { id: 'view', label: 'Open read-only here', short: 'View', title: 'Show the state at this point, read-only (queries run on it; writes are refused). The live graph is not changed.' },
    memory
      ? { id: 'fork', label: 'Fork into a new database…', short: 'Fork…', title: 'Copy the state at this point into a NEW database in memory (a branch, listed under Databases); this one is not changed.' }
      : { id: 'fork', label: 'Fork to a new directory…', short: 'Fork…', title: 'Copy the state at this point into a NEW store directory (a branch); this store is not changed.' },
    { id: 'rollback', label: 'Roll back here…', short: 'Roll back…', title: 'Take THIS store back to this point; what comes after moves to the attic (asks first).' },
  ];
  return readOnly ? actions.filter((a) => a.id !== 'rollback') : actions;
}

/**
 * Whether an action on the point at `commitSeq` is disabled: everything while `locked` (a store
 * action or a query runs), and Roll back in maintenance mode or at or after the store's commit
 * (nothing to roll back). The menu and the history calendar share it. `compact` (the Disk space
 * block's button, no point: `commitSeq` is ignored) is disabled in maintenance mode, on a backup,
 * on a store frozen by damage and while `viewing` a read-only view; never because the compaction
 * advice does not recommend it (the button asks first then, `compactionModel(..).confirm`).
 */
export function actionDisabled(store, actionId, commitSeq, locked, viewing = false) {
  if (locked) return true;
  if (actionId === 'compact') {
    return !store || !!viewing || !!store.maintenance || !!store.backup || !!store.frozenByDamage;
  }
  if (actionId !== 'rollback') return false;
  return !!(store && store.maintenance) || !store || Number(commitSeq) >= Number(store.commitSeq);
}

/**
 * The banner text of a backup directory served read-only (`backup` from the server's info or
 * store answer: `commitSeq`, `takenText`, `restore`).
 */
export function backupBanner(backup) {
  if (!backup) return '';
  return `A BACKUP, read-only: it holds the store up to commit ${backup.commitSeq} (taken ${backup.takenText}). ` +
    'Queries read it; writes, checkpoints, marks and rollbacks are refused. ' +
    `To continue from it as the live store, stop the server and run: ${backup.restore}`;
}

/** The actions of a store on disk (the dev server). */
export const ACTIONS = Object.freeze(actionsFor(false));

/** A commit target. */
export function commitTarget(commitSeq) {
  return { commit: Number(commitSeq) };
}

/** A mark target. */
export function markTarget(name) {
  return { mark: String(name) };
}

/** How a target reads in texts: `commit 5`, `mark "before-import"`, `2026-10-08T02:43:00Z`. */
export function targetText(target) {
  if (!target) return '';
  if ('commit' in target) return `commit ${target.commit}`;
  if ('mark' in target) return `mark "${target.mark}"`;
  if ('time' in target) return `the last commit at or before ${target.time}`;
  return JSON.stringify(target);
}

/** A short, file-name-safe label (the server's default fork directory uses the same). */
export function targetLabel(target) {
  if ('commit' in target) return `commit-${target.commit}`;
  if ('mark' in target) return `mark-${target.mark}`;
  if ('time' in target) return `time-${String(target.time).replace(/[-:]/g, '')}`;
  return 'target';
}

/** Letters, digits, `.`, `_`, `-` stay; anything else becomes `-` (as on the server). */
export function fileSafe(text) {
  return String(text).replace(/[^A-Za-z0-9._-]/g, '-');
}

/** The last part of a path (`/a/b/data` -> `data`). */
export function baseName(path) {
  const parts = String(path).split(/[\\/]+/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : 'store';
}

/**
 * The directory the server forks into when none is given: `<store>-fork-<target>` next to the
 * store (the server adds `-2`, `-3`, ... when it exists).
 */
export function defaultForkDir(storeDir, label) {
  return `${baseName(storeDir)}-fork-${fileSafe(label)}`;
}

/**
 * "Go to commit #...": the target of a typed commit number, or `{error}`. `current` is the
 * store's commit (a later one does not exist).
 */
export function parseCommit(text, current) {
  const raw = String(text ?? '').trim().replace(/^#/, '');
  if (!/^\d+$/.test(raw)) return { error: 'Type a commit number (0, 1, 2, ...).' };
  const n = Number(raw);
  if (!Number.isSafeInteger(n)) return { error: 'That commit number is too large.' };
  if (typeof current === 'number' && n > current) return { error: `The store is at commit ${current}; commit ${n} does not exist yet.` };
  return { target: commitTarget(n) };
}

/** `-120` -> `-02:00`, `330` -> `+05:30`. */
function offsetText(minutes) {
  const sign = minutes < 0 ? '-' : '+';
  const abs = Math.abs(minutes);
  const pad = (n) => String(n).padStart(2, '0');
  return `${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}

/**
 * "Go to time...": the target of a time typed in a `datetime-local` input (`2026-10-08T04:43`
 * or `...T04:43:10`, the browser's local time) or as RFC 3339 text with a zone. The local time
 * gets this machine's offset at that date (`offsetMinutes`, east of UTC, from
 * `-new Date(value).getTimezoneOffset()`), and keeps the precision typed: the server takes a
 * minute (second) as the whole minute (second), so a time copied from a listing includes its
 * commit. Returns `{target}` or `{error}`.
 */
export function parseTime(text, offsetMinutes) {
  const raw = String(text ?? '').trim();
  if (!raw) return { error: 'Choose a date and time.' };
  if (/^\d+$/.test(raw)) return { target: { time: Number(raw) } };
  const m = /^(\d{4}-\d{2}-\d{2})[T ](\d{2}:\d{2}(?::\d{2}(?:[.,]\d+)?)?)(Z|z|[+-]\d{2}(?::?\d{2})?)?$/.exec(raw);
  if (!m) return { error: `"${raw}" is not a date-time like 2026-10-08T02:43:00Z.` };
  const [, date, time, zone] = m;
  if (zone) return { target: { time: `${date}T${time}${zone.toUpperCase() === 'Z' ? 'Z' : zone}` } };
  if (typeof offsetMinutes !== 'number' || !Number.isFinite(offsetMinutes)) {
    return { error: 'Add a zone: Z for UTC or an offset like +02:00.' };
  }
  return { target: { time: `${date}T${time}${offsetText(offsetMinutes)}` } };
}

/**
 * What a rollback to `target` will do, for the confirmation dialog: `{title, lines, commit}`.
 * `store` is the `GET /api/store` answer (its position, marks, snapshots); `deleteHistory` the
 * dialog's "delete instead of keeping in the attic" option.
 */
export function rollbackPlan(store, target, deleteHistory = false) {
  const current = store ? store.commitSeq : null;
  let commit = null;
  if ('commit' in target) commit = target.commit;
  else if ('mark' in target && store) {
    const mark = (store.marks || []).find((m) => m.name === target.mark);
    if (mark) commit = mark.commitSeq;
  }
  const where = commit === null ? targetText(target) : `commit ${commit}`;
  const after = commit === null ? `everything after ${where}` : `everything after commit ${commit}`;
  const range = commit !== null && current !== null && current > commit
    ? ` (commits ${commit + 1}..${current})`
    : '';
  const lines = [
    deleteHistory
      ? `${after[0].toUpperCase()}${after.slice(1)}${range} is DELETED for good. This cannot be undone.`
      : `${after[0].toUpperCase()}${after.slice(1)}${range} moves to the attic. You can restore it as long as nothing new is committed or marked; after that you can still fork it into a new ${store && store.memory ? 'database' : 'directory'}.`,
    `The live graph switches to ${where}; the query that runs now is cancelled, and every tab's result may be stale.`,
    'The store continues as a new lineage from there (its marks before that point stay).',
  ];
  return {
    title: `Roll back the store to ${where}?`,
    lines,
    commit,
    nothingToDo: commit !== null && current !== null && commit >= current,
  };
}

/**
 * The attic rows of the menu: `{id, range, when, size, reason, restorable, blocked}` newest
 * first. `blocked` says why Restore is disabled (then the row offers Fork instead).
 */
export function atticRows(store) {
  const entries = (store && store.attic) || [];
  return entries.slice().reverse().map((a) => ({
    id: a.id,
    range: a.firstCommitSeq === a.lastCommitSeq ? `commit ${a.lastCommitSeq}` : `commits ${a.firstCommitSeq ?? a.target + 1}..${a.lastCommitSeq}`,
    when: a.timeText,
    size: a.bytesText || '',
    reason: a.reason,
    restorable: !!a.restorable,
    blocked: a.restorable ? null : (a.restoreBlocked || 'not possible now'),
  }));
}

const KIND_TEXT = {
  addVertex: ['vertex added', 'vertices added'],
  dropVertex: ['vertex dropped', 'vertices dropped'],
  addEdge: ['edge added', 'edges added'],
  dropEdge: ['edge dropped', 'edges dropped'],
  setProperty: ['property written', 'properties written'],
  removeProperty: ['property removed', 'properties removed'],
  addLabel: ['label added', 'labels added'],
  removeLabel: ['label removed', 'labels removed'],
  setSchema: ['schema change', 'schema changes'],
  other: ['other change', 'other changes'],
};

/** `{addVertex: 2, setProperty: 1}` -> `2 vertices added, 1 property written`. */
export function countsText(counts) {
  return Object.entries(counts || {})
    .filter(([, n]) => n > 0)
    .map(([kind, n]) => {
      const [one, many] = KIND_TEXT[kind] || [kind, kind];
      return `${n} ${n === 1 ? one : many}`;
    })
    .join(', ') || 'no changes';
}

/** The commits one page of "Show changes" asks for. */
export const CHANGES_PAGE = 100;

/**
 * The "Show changes" view of one page of an attic entry (`GET /api/store/attic/changes?id=
 * &offset=&limit=`): a headline over ALL its commits (the totals the engine streams), the page
 * (`commits 19..118 of 250`), one row per commit `{commit, when, by, what, samples}`, and the
 * offsets of the previous and next page (`null`: none).
 */
export function changesModel(summary) {
  const commits = (summary && summary.commits) || [];
  const n = summary ? summary.commitCount : 0;
  const offset = (summary && summary.offset) || 0;
  const limit = (summary && summary.limit) || CHANGES_PAGE;
  const page = commits.length
    ? `commits ${commits[0].commitSeq}..${commits[commits.length - 1].commitSeq} of ${n}`
    : (n ? `no commits from #${offset + 1} on (${n} in all)` : 'no commits');
  return {
    headline: `${n} commit${n === 1 ? '' : 's'}, ${summary ? summary.mutationCount : 0} change${summary && summary.mutationCount === 1 ? '' : 's'}: ${countsText(summary && summary.totals)}`,
    page,
    rows: commits.map((c) => ({
      commit: c.commitSeq,
      when: c.timeText,
      by: c.principal || '',
      what: countsText(c.counts),
      samples: c.samples || [],
    })),
    prevOffset: offset > 0 ? Math.max(0, offset - limit) : null,
    nextOffset: summary && summary.hasMore ? offset + commits.length : null,
  };
}

/**
 * The embedding models of the store (`store.models`, format spec 4.5) as the Store menu lists
 * them: `{id, identity, size, licence, licenceText, gated, used, problem}`; then one row per model
 * the schema names that the store does not keep (`missing: true`). Empty without models.
 */
export function modelRows(store) {
  const rows = ((store && store.models) || []).map((m) => ({
    id: m.id,
    identity: m.identity || m.id,
    size: m.bytesText || '',
    licence: m.license || 'unknown',
    licenceText: m.licenseText === 'file' ? 'licence text from the repository'
      : m.licenseText === 'template' ? 'standard licence text, written by Graphersal'
        : 'no licence text',
    gated: !!m.gated,
    used: !!m.usedBySchema,
    problem: m.problem || null,
    missing: false,
  }));
  for (const identity of (store && store.missingModels) || []) {
    rows.push({
      id: identity, identity, size: '', licence: '', licenceText: '', gated: false, used: true,
      problem: 'the schema names it, the store does not keep it', missing: true,
    });
  }
  return rows;
}

/** Whether the snapshot downloads offer a copy without the models (the store keeps some). */
export function offersWithoutModels(store) {
  return !!(store && store.models && store.models.length);
}

/** The file name of a snapshot download (`graph-N.gsnap`, `graph-N-without-models.gsnap`). */
export function snapshotFileName(commitSeq, withModels = true) {
  return withModels ? `graph-${commitSeq}.gsnap` : `graph-${commitSeq}-without-models.gsnap`;
}

/**
 * The databases of the session (the store in memory and its forks): `{id, label, commit, active,
 * canClose}` in the order they were made; the one in use cannot be closed.
 */
export function databaseRows(store) {
  return ((store && store.databases) || []).map((db) => ({
    id: db.id,
    label: db.label,
    commit: db.commitSeq,
    active: !!db.active,
    canClose: !db.active,
  }));
}

/** The banner text of a read-only view (`GET /api/info` `view`). */
export function viewBanner(view) {
  if (!view) return '';
  const when = view.timeText && view.timeText !== '-' ? ` (last commit ${view.timeText})` : '';
  return `${view.label || `Read-only view of commit ${view.commitSeq}`}${when}`;
}

/**
 * The store actions whose run the menu shows (the item's running form, the Store button's busy
 * state, the live region): `label` the idle text of the item, `running` its text while it runs,
 * `short` the Store button's word ("Store · verifying… 0:42").
 */
export const STORE_TASKS = Object.freeze({
  checkpoint: { label: 'Checkpoint', running: 'Writing a snapshot…', short: 'checkpointing' },
  mark: { label: 'Mark', running: 'Marking…', short: 'marking' },
  verify: { label: 'Verify the store', running: 'Verifying the store…', short: 'verifying' },
  compact: { label: 'Compact', running: 'Compacting…', short: 'compacting' },
  backup: { label: 'Back up', running: 'Backing up…', short: 'backing up' },
  backupFromMemory: { label: 'Back up from memory', running: 'Backing up from memory…', short: 'backing up' },
  view: { label: 'Open read-only here', running: 'Opening the view…', short: 'opening a view' },
  live: { label: 'Back to the live graph', running: 'Going back…', short: 'going back' },
  fork: { label: 'Fork', running: 'Forking…', short: 'forking' },
  atticFork: { label: 'Fork', running: 'Forking…', short: 'forking' },
  rollback: { label: 'Roll back', running: 'Rolling back…', short: 'rolling back' },
  restore: { label: 'Restore', running: 'Restoring…', short: 'restoring' },
  atticRemove: { label: 'Remove', running: 'Removing…', short: 'removing' },
  switch: { label: 'Switch to', running: 'Switching…', short: 'switching' },
  close: { label: 'Close', running: 'Closing…', short: 'closing' },
});

/** The entry of `STORE_TASKS` for `id` (an unknown id reads as itself). */
export function storeTask(id) {
  return STORE_TASKS[id] || { label: String(id), running: `${id}…`, short: String(id) };
}

/** Elapsed time as a clock: `0:07`, `0:42`, `12:05`, `1:02:03`. */
export function elapsedText(ms) {
  const total = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`;
}

/**
 * The running store action as the menu shows it, or `null`: `task` is `state.storeTask`
 * (`{id, key, startedAt, now}`, milliseconds).
 */
export function runningTask(task) {
  if (!task || !task.id) return null;
  const t = storeTask(task.id);
  const elapsed = elapsedText((task.now ?? task.startedAt) - task.startedAt);
  return { id: task.id, label: t.label, running: t.running, short: t.short, elapsed };
}

/**
 * Whether the item of action `id` shows its running form; `key` tells the items of one action
 * apart (the Restore of one attic entry among several): with a key, only the item whose key the
 * running action carries (`state.storeTask.key`).
 */
export function isRunning(task, id, key = undefined) {
  return !!(task && task.id === id && (key === undefined || task.key === key));
}

/**
 * The tooltip of an item: its own `title`, or, while a store action runs (every item that could
 * start another one is disabled then), `Wait for "Verify the store" to finish (0:42).`
 */
export function waitTitle(task, title) {
  const r = runningTask(task);
  return r ? `Wait for "${r.label}" to finish (${r.elapsed}).` : title;
}

/**
 * The attributes of the button of action `id` (bound with `v-bind`): the class `store-running`
 * and `aria-busy` while it runs, the tooltip (its running text with the elapsed time; while
 * another action runs, `waitTitle`; else `title`); `key` as in `isRunning`.
 */
export function taskButton(task, id, title, key = undefined) {
  const running = isRunning(task, id, key);
  const r = runningTask(task);
  return {
    class: { 'store-running': running },
    'aria-busy': String(running),
    title: running ? `${r.running} ${r.elapsed}` : waitTitle(task, title),
  };
}

/**
 * The text of the Store button in the top bar (`info` the `GET /api/store` answer, `view` the
 * read-only view, `task` the running action): `Store · #12 ▾`, `Store · verifying… 0:42 ▾`.
 */
export function storeButtonText(info, view, task) {
  const r = runningTask(task);
  const parts = ['Store'];
  if (r) parts.push(`${r.short}… ${r.elapsed}`);
  else if (info) parts.push(`#${info.commitSeq}`);
  if (view) parts.push(`viewing #${view.commitSeq}`);
  if (info && (info.maintenance || (info.damageFound && info.damageFound.length))) parts.push('DAMAGED');
  if (info && info.backup) parts.push('BACKUP');
  if (info && info.stopped) parts.push('STOPPED');
  return `${parts.join(' · ')} ▾`;
}

/** What the live region says when action `id` starts. */
export function taskStartText(id) {
  return `${storeTask(id).running.replace(/…$/, '')} started.`;
}

/** What the live region says when action `id` ended after `ms` (`ok`: without an error). */
export function taskEndText(id, ms, ok) {
  return `${storeTask(id).label} ${ok ? 'finished' : 'failed'} after ${elapsedText(ms)}.`;
}
