// Throttled reloads while the served graph changes fast (a bulk load from an AI agent over MCP, a
// script, another tab). The change feed (js/change-feed.js) is polled every 2 s; a single change
// made elsewhere reloads the page's graph at once (normal mode). When changes made elsewhere show
// up in several polls in a row (BURST_POLLS within BURST_WINDOW_MS), the page enters burst mode:
// it coalesces the changes and reloads at most every BURST_REFRESH_MS, and a banner offers
// Refresh now and Pause auto refresh (manual mode: no automatic reload until Resume). After
// BURST_QUIET_MS without a change made elsewhere it reloads once more (when something changed
// since the last reload) and returns to normal mode.
//
// Pure logic with an injectable clock, driven by the poll answers (no timers of its own), tested
// under Node (playground/tests/change-burst.test.mjs). `observe` and `resume` return what the page
// should do: 'refresh' (reload now), 'defer' (a change is pending, not reloaded yet) or 'none'.

import { POLL_MS } from './change-feed.js';

/** Polls with changes made elsewhere that start burst mode ... */
export const BURST_POLLS = 3;
/** ... when they all fall within this window. */
export const BURST_WINDOW_MS = 10000;
/** In burst mode, the page reloads at most this often. */
export const BURST_REFRESH_MS = 30000;
/** Burst mode ends after this long without a change made elsewhere. */
export const BURST_QUIET_MS = 10000;

const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

/**
 * The burst state machine. States: `normal` and `burst` (`mode`), each either automatic or paused
 * (`paused`, set by Pause auto refresh, cleared only by Resume).
 * @param {{now?: () => number}} [o]
 */
export function createBurst(o = {}) {
  const now = o.now || (() => Date.now());
  let mode = 'normal';
  let paused = false;
  let pending = 0; // commits made elsewhere since the last reload
  let externalPolls = []; // times of the recent polls with changes made elsewhere
  let commitLog = []; // [{t, n}] of the last BURST_REFRESH_MS (the banner's rate)
  let lastExternalAt = -Infinity;
  let lastRefreshAt = -Infinity;

  function prune(t) {
    externalPolls = externalPolls.filter((x) => t - x < BURST_WINDOW_MS);
    commitLog = commitLog.filter((c) => t - c.t < BURST_REFRESH_MS);
  }

  function dueInBurst(t) {
    return pending > 0 && !paused && t - lastRefreshAt >= BURST_REFRESH_MS;
  }

  return {
    /**
     * One poll answer. `external`: it saw changes this page did not make; `commits`: how many
     * (at least 1 when external).
     * @returns {'refresh'|'defer'|'none'}
     */
    observe(external, commits = 1) {
      const t = now();
      if (external) {
        const n = Math.max(1, Number(commits) || 1);
        externalPolls.push(t);
        commitLog.push({ t, n });
        pending += n;
        lastExternalAt = t;
      }
      prune(t);
      if (mode === 'normal') {
        if (!external) return 'none';
        if (externalPolls.length >= BURST_POLLS) {
          mode = 'burst';
          return dueInBurst(t) ? 'refresh' : 'defer';
        }
        return paused ? 'defer' : 'refresh';
      }
      // burst
      if (!external && t - lastExternalAt >= BURST_QUIET_MS) {
        mode = 'normal';
        externalPolls = [];
        return pending > 0 && !paused ? 'refresh' : 'none';
      }
      if (dueInBurst(t)) return 'refresh';
      return pending > 0 ? 'defer' : 'none';
    },
    /** A reload starts: returns its token for `refreshed` (the commits it covers). */
    beginRefresh() {
      return pending;
    },
    /** A reload that started with `token` finished (changes seen meanwhile stay pending). */
    refreshed(token = pending) {
      pending = Math.max(0, pending - token);
      lastRefreshAt = now();
    },
    /** Pause auto refresh: no automatic reload until `resume`. */
    pause() {
      paused = true;
    },
    /** Resume auto refresh; 'refresh' when a reload is due now. */
    resume() {
      if (!paused) return 'none';
      paused = false;
      if (pending === 0) return 'none';
      if (mode === 'normal') return 'refresh';
      return dueInBurst(now()) ? 'refresh' : 'defer';
    },
    get mode() {
      return mode;
    },
    get paused() {
      return paused;
    },
    get pending() {
      return pending;
    },
    /**
     * The banner: null in normal automatic mode, else `{kind: 'burst'|'paused', text}`.
     */
    banner() {
      if (paused) {
        const what = pending ? plural(pending, 'commit') : 'no commits';
        return { kind: 'paused', text: `Auto refresh paused: ${what} since the last refresh` };
      }
      if (mode !== 'burst') return null;
      const t = now();
      prune(t);
      const n = commitLog.reduce((s, c) => s + c.n, 0);
      // The oldest logged poll covers the changes of the poll interval before it.
      const span = commitLog.length ? t - commitLog[0].t + POLL_MS : POLL_MS;
      const secs = Math.round(Math.min(BURST_REFRESH_MS, span) / 1000);
      return {
        kind: 'burst',
        text: `The graph is changing fast (${plural(n, 'commit')} in the last ${secs} s): auto refresh every ${BURST_REFRESH_MS / 1000} s`,
      };
    },
  };
}

/** The live region's text for a banner (announced only when its kind changes). */
export function burstAnnouncement(banner) {
  if (!banner) return 'Auto refresh is back to normal.';
  if (banner.kind === 'paused') return 'Auto refresh paused.';
  return `The graph is changing fast: auto refresh every ${BURST_REFRESH_MS / 1000} s.`;
}
