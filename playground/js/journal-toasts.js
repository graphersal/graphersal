// Journal notifications (toasts) and the shared journal follower. Pure logic, tested under Node
// (tests/journal-toasts.test.mjs); the view is js/components/journal-toasts.js, the System
// journal panel (js/components/journal.js) follows through the same follower.
//
// The follower is the one poller of the served graph's system journal: it starts at the
// journal's CURRENT end (a newest-first page of one record, so old records never pop up), then
// reads forward with `after` every `intervalMs` while the page is visible, never more than one
// request in flight; a failed poll is silent and retried later with a growing delay. It runs
// only while something subscribes (the toasts when the setting is not `off`, the open panel).
//
// The toast model keeps at most `TOAST_VISIBLE` toasts; the rest wait in a queue shown as one
// "+N more journal records" toast. A record of the same kind and message as a held toast merges
// into it with a counter. `warn` and `error` stay until closed; the others close after
// `TOAST_LIFE_MS`, their timer paused while the pointer is on them. Time comes from an
// injectable clock (`now()` in ms), so the model has no timers of its own: the view asks
// `nextDeadline()` and calls `expire()` then.

import { JOURNAL_REQUEST_LIMIT } from './journal.js';

/** Toasts shown at once; more collapse into the "+N more" toast. */
export const TOAST_VISIBLE = 3;
/** How long a non-sticky toast stays (ms), paused while hovered. */
export const TOAST_LIFE_MS = 30_000;
/** Records held in the queue behind the visible toasts; more are only counted. */
export const TOAST_QUEUE_MAX = 100;
/** Poll intervals (ms): the dev server, the WebAssembly playground. */
export const FOLLOW_INTERVAL_SERVER_MS = 5_000;
export const FOLLOW_INTERVAL_WASM_MS = 2_000;
/** The longest delay between polls after failures (ms). */
export const FOLLOW_MAX_BACKOFF_MS = 60_000;
/** Forward requests per poll at most; the next poll goes on from the cursor. */
export const FOLLOW_MAX_REQUESTS = 5;
/** How far back (ms) a rebase (graph replaced) still notifies about new records. */
export const REBASE_WINDOW_MS = 30_000;

/** The setting's choices (`settings.journalToasts`), the default first. */
export const TOAST_MIN_LEVELS = [
  { id: 'notice', label: 'notice and above' },
  { id: 'warn', label: 'warn and above' },
  { id: 'error', label: 'error only' },
  { id: 'off', label: 'Off' },
];
export const DEFAULT_TOAST_MIN_LEVEL = 'notice';

const RANKS = { error: 0, warn: 1, notice: 2, info: 3, debug: 4 };

/** A level's severity rank (0 = error); an unknown level ranks below `debug`. */
export function levelRank(level) {
  const rank = RANKS[String(level || '').toLowerCase()];
  return rank === undefined ? 5 : rank;
}

/** The setting's value, or the default for anything else. */
export function normalizeMinLevel(value) {
  return TOAST_MIN_LEVELS.some((l) => l.id === value) ? value : DEFAULT_TOAST_MIN_LEVEL;
}

/** Whether a record pops up under the setting `min` (`off`: never). */
export function passesMinLevel(record, min) {
  const level = normalizeMinLevel(min);
  if (level === 'off' || !record) return false;
  return levelRank(record.level) <= levelRank(level);
}

/** `warn` and `error` toasts stay until closed. */
export function isSticky(level) {
  return levelRank(level) <= 1;
}

/** The toast's colour class: `jt-error` red, `jt-warn` amber, `jt-notice` blue, `jt-info` grey, else neutral. */
export function toastClass(level) {
  switch (levelRank(level)) {
    case 0: return 'jt-error';
    case 1: return 'jt-warn';
    case 2: return 'jt-notice';
    case 3: return 'jt-info';
    default: return 'jt-debug';
  }
}

/** `alert` for warn/error (announced at once), `status` (polite) for the others. */
export function toastRole(level) {
  return isSticky(level) ? 'alert' : 'status';
}

const mergeKey = (record) => `${record.kind || ''}\u0000${record.message || ''}`;

/**
 * The toast model. `now()` answers the time in ms. Methods: `push(records, min)` (records
 * oldest first; answers whether anything changed), `close(id)`, `closeOverflow()`,
 * `hover(id, on)`, `expire()`, `nextDeadline()`, `view()`, `clear()`.
 */
export function createToasts({
  now = () => Date.now(), visible = TOAST_VISIBLE, lifeMs = TOAST_LIFE_MS, queueMax = TOAST_QUEUE_MAX,
} = {}) {
  let nextId = 1;
  let shown = []; // arrival order
  let queued = []; // arrival order
  let dropped = 0; // beyond the queue: only counted
  let droppedSticky = false;
  let droppedDeadline = null;

  function toastOf(record, t) {
    const sticky = isSticky(record.level);
    return {
      id: nextId++,
      seq: record.seq,
      level: record.level || '',
      source: record.source || '',
      kind: record.kind || '',
      message: record.message || '',
      count: 1,
      sticky,
      paused: false,
      remaining: lifeMs,
      deadline: sticky ? null : t + lifeMs,
    };
  }
  function restart(toast, t) {
    toast.sticky = isSticky(toast.level);
    toast.remaining = lifeMs;
    toast.deadline = toast.sticky || toast.paused ? null : t + lifeMs;
  }
  function merge(toast, record, t) {
    toast.count += 1;
    if (record.seq > toast.seq) toast.seq = record.seq;
    if (levelRank(record.level) < levelRank(toast.level)) toast.level = record.level || '';
    restart(toast, t);
  }
  /** Fills free slots from the queue; a promoted toast's timer starts now. */
  function promote(t) {
    while (shown.length < visible && queued.length) {
      const toast = queued.shift();
      restart(toast, t);
      shown.push(toast);
    }
  }

  function push(records, min) {
    const t = now();
    let changed = false;
    for (const record of records || []) {
      if (!record || typeof record.seq !== 'number' || !passesMinLevel(record, min)) continue;
      changed = true;
      const key = mergeKey(record);
      const held = shown.find((x) => mergeKey(x) === key) || queued.find((x) => mergeKey(x) === key);
      if (held) {
        merge(held, record, t);
      } else if (shown.length < visible) {
        shown.push(toastOf(record, t));
      } else if (queued.length < queueMax) {
        queued.push(toastOf(record, t));
      } else {
        dropped += 1;
        if (isSticky(record.level)) droppedSticky = true;
        droppedDeadline = droppedSticky ? null : t + lifeMs;
      }
    }
    return changed;
  }

  function close(id) {
    const before = shown.length + queued.length;
    shown = shown.filter((x) => x.id !== id);
    queued = queued.filter((x) => x.id !== id);
    promote(now());
    return shown.length + queued.length !== before;
  }

  function closeOverflow() {
    const had = queued.length + dropped > 0;
    queued = [];
    dropped = 0;
    droppedSticky = false;
    droppedDeadline = null;
    return had;
  }

  function hover(id, on) {
    const toast = shown.find((x) => x.id === id);
    if (!toast || toast.paused === !!on) return false;
    const t = now();
    toast.paused = !!on;
    if (toast.sticky) return true;
    if (on) {
      toast.remaining = Math.max(0, (toast.deadline ?? t) - t);
      toast.deadline = null;
    } else {
      toast.deadline = t + toast.remaining;
    }
    return true;
  }

  /** Removes the toasts (and queued records) whose time is up; answers whether any went. */
  function expire() {
    const t = now();
    const due = (x) => x.deadline != null && x.deadline <= t;
    const before = shown.length + queued.length + dropped;
    shown = shown.filter((x) => !due(x));
    queued = queued.filter((x) => !due(x));
    if (dropped && droppedDeadline != null && droppedDeadline <= t) {
      dropped = 0;
      droppedDeadline = null;
    }
    promote(t);
    return shown.length + queued.length + dropped !== before;
  }

  /** The earliest deadline (ms), or `null` when nothing closes by itself. */
  function nextDeadline() {
    let next = null;
    for (const x of shown.concat(queued)) {
      if (x.deadline != null && (next == null || x.deadline < next)) next = x.deadline;
    }
    if (dropped && droppedDeadline != null && (next == null || droppedDeadline < next)) next = droppedDeadline;
    return next;
  }

  /**
   * What to render: `toasts` newest first (`{id, seq, level, source, kind, message, count, cls,
   * role, sticky}`), `more` = records behind them, `moreRole` its role (`alert` when one of
   * them is warn/error).
   */
  function view() {
    const toasts = shown.slice().reverse().map((x) => ({
      id: x.id, seq: x.seq, level: x.level, source: x.source, kind: x.kind, message: x.message,
      count: x.count, sticky: x.sticky, cls: toastClass(x.level), role: toastRole(x.level),
    }));
    const more = queued.reduce((n, x) => n + x.count, 0) + dropped;
    const stickyBehind = droppedSticky || queued.some((x) => x.sticky);
    return { toasts, more, moreRole: stickyBehind ? 'alert' : 'status' };
  }

  function clear() {
    shown = [];
    closeOverflow();
  }

  return { push, close, closeOverflow, hover, expire, nextDeadline, view, clear };
}

/** The text of the overflow toast. */
export function moreText(n) {
  return `+${n} more journal record${n === 1 ? '' : 's'}`;
}

/** The cursor of a page: its `next_after`, else its newest record's seq, else `fallback`. */
export function cursorOf(page, fallback = 0) {
  if (page && typeof page.next_after === 'number') return page.next_after;
  const seqs = ((page && page.records) || []).map((r) => r && r.seq).filter((s) => typeof s === 'number');
  return seqs.length ? Math.max(...seqs) : fallback;
}

/**
 * The shared follower. `fetchPage(spec)` is the engine's `journal`; `isVisible()` answers
 * whether the page is visible; `setTimer`/`clearTimer` and `nowMs` are injectable for tests.
 * `subscribe(fn)` (answers the unsubscribe function) gets batches `{records, from, to}`:
 * records oldest first with `from < seq`, the cursor moved from `from` to `to`; a batch with
 * `from: null` does not continue the previous one (after a rebase). `poke()` polls soon (the
 * page became visible), `rebase()` takes the journal's end again (the graph was replaced).
 */
export function createJournalFollower({
  fetchPage,
  intervalMs = FOLLOW_INTERVAL_SERVER_MS,
  isVisible = () => true,
  setTimer = (fn, ms) => setTimeout(fn, ms),
  clearTimer = (id) => clearTimeout(id),
  nowMs = () => Date.now(),
  maxBackoffMs = FOLLOW_MAX_BACKOFF_MS,
  maxRequests = FOLLOW_MAX_REQUESTS,
  limit = JOURNAL_REQUEST_LIMIT,
  rebaseWindowMs = REBASE_WINDOW_MS,
} = {}) {
  const subscribers = new Set();
  let cursor = null; // null: take the journal's end first
  let rebaseSince = null; // µs: after a rebase, records from then on are still announced
  let rebaseFrom = null; // the cursor before the rebase: records up to it were announced already
  let timer = null;
  let inFlight = false;
  let failures = 0;
  let epoch = 0;

  function emit(batch) {
    for (const fn of [...subscribers]) {
      try {
        fn(batch);
      } catch (err) {
        /* a subscriber's failure never stops the others */
      }
    }
  }
  function schedule(ms) {
    if (timer != null) clearTimer(timer);
    timer = subscribers.size ? setTimer(tick, Math.max(0, ms)) : null;
  }
  const seqOk = (r) => r && typeof r.seq === 'number';

  async function poll(my) {
    if (cursor == null) {
      const since = rebaseSince;
      const page = await fetchPage(since == null ? { newest_first: true, limit: 1 } : { newest_first: true, limit: 50 });
      if (my !== epoch) return;
      const end = cursorOf(page, 0);
      const before = rebaseFrom;
      cursor = end;
      rebaseSince = null;
      rebaseFrom = null;
      if (since != null) {
        // The same journal (its end not below the old cursor): only records after the old
        // cursor are new; another (shorter) journal: every recent record is.
        const sameJournal = before != null && end >= before;
        const records = ((page && page.records) || [])
          .filter((r) => seqOk(r) && typeof r.time === 'number' && r.time >= since
            && (!sameJournal || r.seq > before))
          .reverse();
        if (records.length) emit({ records, from: null, to: end });
      }
      return;
    }
    for (let i = 0; i < maxRequests; i++) {
      const from = cursor;
      const page = await fetchPage({ after: from, limit });
      if (my !== epoch) return;
      const records = ((page && page.records) || []).filter((r) => seqOk(r) && r.seq > from);
      const to = Math.max(from, cursorOf(page, records.length ? records[records.length - 1].seq : from));
      cursor = to;
      if (records.length || to > from) emit({ records, from, to });
      if (!(page && page.more)) break;
    }
  }

  async function tick() {
    timer = null;
    if (!subscribers.size || inFlight) return; // the running poll schedules the next
    if (!isVisible()) return; // poke() resumes when the page shows again
    inFlight = true;
    const my = epoch;
    let delay = intervalMs;
    try {
      await poll(my);
      failures = 0;
    } catch (err) {
      failures += 1;
      delay = Math.min(maxBackoffMs, intervalMs * 2 ** failures);
    } finally {
      inFlight = false;
    }
    if (my !== epoch) delay = 0; // rebased meanwhile: start over at once
    if (subscribers.size && isVisible()) schedule(delay);
  }

  function subscribe(fn) {
    subscribers.add(fn);
    if (subscribers.size === 1 && !inFlight) schedule(0);
    return () => {
      subscribers.delete(fn);
      if (!subscribers.size) {
        if (timer != null) clearTimer(timer);
        timer = null;
        cursor = null; // the next start takes the journal's end again
        rebaseSince = null;
        rebaseFrom = null;
        epoch++;
      }
    };
  }

  function poke() {
    if (subscribers.size && !inFlight && isVisible()) schedule(0);
  }

  function rebase() {
    if (!subscribers.size) return;
    epoch++;
    if (cursor != null) rebaseFrom = cursor;
    cursor = null;
    rebaseSince = (nowMs() - rebaseWindowMs) * 1000;
    if (!inFlight) schedule(0);
  }

  return {
    subscribe,
    poke,
    rebase,
    /** For tests and diagnostics. */
    status: () => ({ cursor, inFlight, failures, subscribers: subscribers.size, scheduled: timer != null }),
  };
}

/** Whether `record` passes the journal panel's filters (level and above, source prefix). */
export function matchesJournalFilters(record, filters) {
  if (!record) return false;
  const level = filters && filters.level ? String(filters.level).trim() : '';
  const source = filters && filters.source ? String(filters.source).trim() : '';
  if (level && levelRank(record.level) > levelRank(level)) return false;
  if (source) {
    const s = String(record.source || '');
    if (s !== source && !s.startsWith(`${source}.`)) return false;
  }
  return true;
}

/**
 * A follower batch as a forward page for the panel's view (js/journal.js `addJournalPage`),
 * or `null` when it does not continue the view's cursor (the panel then reads by itself).
 */
export function batchAsPage(view, batch, filters) {
  if (!view || !batch || batch.from == null || batch.from > view.after) return null;
  const records = (batch.records || []).filter((r) => r.seq > view.after && matchesJournalFilters(r, filters));
  return { records, next_after: batch.to, truncated: false, more: false, notes: [] };
}
