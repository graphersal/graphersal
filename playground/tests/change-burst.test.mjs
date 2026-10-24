// Node tests of the change feed's burst state machine (js/change-burst.js): throttled reloads
// while the graph changes fast elsewhere.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createBurst, burstAnnouncement, BURST_POLLS, BURST_WINDOW_MS, BURST_REFRESH_MS, BURST_QUIET_MS,
} from '../js/change-burst.js';
import { POLL_MS } from '../js/change-feed.js';

function clocked() {
  const clock = { t: 1_000_000 };
  const burst = createBurst({ now: () => clock.t });
  // One poll: advance the clock, observe, and reload when told to (as store.js does).
  const poll = (external, commits = 1, ms = POLL_MS) => {
    clock.t += ms;
    const d = burst.observe(external, commits);
    if (d === 'refresh') burst.refreshed(burst.beginRefresh());
    return d;
  };
  return { clock, burst, poll };
}

test('the constants are the designed ones', () => {
  assert.equal(BURST_POLLS, 3);
  assert.equal(BURST_WINDOW_MS, 10000);
  assert.equal(BURST_REFRESH_MS, 30000);
  assert.equal(BURST_QUIET_MS, 10000);
});

test('normal mode: a single change elsewhere reloads at once, no banner', () => {
  const { burst, poll } = clocked();
  assert.equal(poll(false), 'none');
  assert.equal(poll(true), 'refresh');
  assert.equal(burst.mode, 'normal');
  assert.equal(burst.banner(), null);
  assert.equal(burst.pending, 0);
  // Two changes spaced wider than the window never start a burst.
  assert.equal(poll(true, 1, BURST_WINDOW_MS), 'refresh');
  assert.equal(poll(true, 1, BURST_WINDOW_MS), 'refresh');
  assert.equal(poll(true, 1, BURST_WINDOW_MS), 'refresh');
  assert.equal(burst.mode, 'normal');
});

test('burst: three polls with changes within the window start it; reloads coalesce to every 30 s', () => {
  const { clock, burst, poll } = clocked();
  assert.equal(poll(true, 5), 'refresh');
  assert.equal(poll(true, 5), 'refresh');
  assert.equal(poll(true, 5), 'defer', 'the third poll starts the burst and defers');
  assert.equal(burst.mode, 'burst');
  const b = burst.banner();
  assert.equal(b.kind, 'burst');
  assert.match(b.text, /^The graph is changing fast \(15 commits in the last 6 s\): auto refresh every 30 s$/);
  // Changes keep coming: no reload until 30 s after the last one.
  let refreshes = 0;
  const lastRefresh = clock.t - POLL_MS; // the second poll reloaded
  while (clock.t + POLL_MS - lastRefresh < BURST_REFRESH_MS) {
    assert.equal(poll(true, 2), 'defer');
  }
  assert.ok(burst.pending > 0);
  if (poll(true, 2) === 'refresh') refreshes++;
  assert.equal(refreshes, 1, 'one reload once 30 s passed');
  assert.equal(burst.pending, 0);
  assert.equal(poll(true, 2), 'defer', 'the interval starts again');
  // A poll without changes inside the burst does nothing.
  assert.equal(poll(false), 'defer', 'still pending, not quiet yet');
});

test('burst ends after the quiet period with one final reload, then normal mode again', () => {
  const { burst, poll } = clocked();
  poll(true); poll(true); poll(true);
  assert.equal(burst.mode, 'burst');
  assert.equal(poll(false), 'defer');
  assert.equal(poll(false), 'defer');
  assert.equal(poll(false), 'defer');
  assert.equal(poll(false), 'defer');
  assert.equal(poll(false), 'refresh', 'quiet for 10 s: final reload');
  assert.equal(burst.mode, 'normal');
  assert.equal(burst.banner(), null);
  assert.equal(poll(true), 'refresh', 'normal behaviour again');
});

test('burst ending with nothing pending needs no final reload', () => {
  const { clock, burst, poll } = clocked();
  poll(true); poll(true); poll(true);
  burst.refreshed(burst.beginRefresh()); // Refresh now
  clock.t += BURST_QUIET_MS;
  assert.equal(burst.observe(false), 'none');
  assert.equal(burst.mode, 'normal');
});

test('pause: no automatic reloads; resume reloads when due; Refresh now restarts the interval', () => {
  const { clock, burst, poll } = clocked();
  poll(true); poll(true); poll(true, 4);
  burst.pause();
  assert.equal(burst.banner().kind, 'paused');
  assert.equal(burst.banner().text, 'Auto refresh paused: 4 commits since the last refresh');
  for (let i = 0; i < 20; i++) assert.equal(poll(true, 1), 'defer', 'paused: never a reload');
  // The burst ends while paused: manual mode stays, the banner keeps counting.
  for (let i = 0; i < 6; i++) assert.notEqual(poll(false), 'refresh');
  assert.equal(burst.mode, 'normal');
  assert.equal(burst.paused, true);
  assert.equal(burst.banner().text, 'Auto refresh paused: 24 commits since the last refresh');
  assert.equal(poll(true), 'defer', 'paused in normal mode: still no reload');
  // Refresh now while paused.
  burst.refreshed(burst.beginRefresh());
  assert.equal(burst.banner().text, 'Auto refresh paused: no commits since the last refresh');
  poll(true);
  assert.equal(burst.resume(), 'refresh', 'resumed in normal mode with a change pending');
  assert.equal(burst.resume(), 'none', 'resume twice is harmless');
  // Resume during a burst before the interval elapsed defers.
  burst.refreshed(burst.beginRefresh());
  poll(true); poll(true); poll(true);
  assert.equal(burst.mode, 'burst');
  burst.pause();
  clock.t += 1000;
  assert.equal(burst.resume(), 'defer');
  clock.t += BURST_REFRESH_MS;
  burst.pause();
  assert.equal(burst.resume(), 'refresh');
});

test('a reload keeps the changes seen while it ran pending', () => {
  const { burst, poll } = clocked();
  poll(true); poll(true); poll(true, 3);
  const token = burst.beginRefresh();
  assert.equal(token, 3);
  burst.observe(true, 2); // a poll answered while the reload ran
  burst.refreshed(token);
  assert.equal(burst.pending, 2);
});

test('hidden page: the first poll after a long pause follows the rules (no reload inside the interval)', () => {
  const { burst, poll } = clocked();
  poll(true); poll(true); poll(true);
  // Visible again 8 s later with changes still coming: inside the 30 s interval, so it defers.
  assert.equal(poll(true, 10, 8000), 'defer');
  // Visible again after a long time: the interval elapsed, so it reloads.
  assert.equal(poll(true, 10, BURST_REFRESH_MS), 'refresh');
  // Quiet for long: the burst ends with the final reload when something is pending.
  poll(true);
  assert.equal(poll(false, 0, 60000), 'refresh');
  assert.equal(burst.mode, 'normal');
});

test('the live region announces the banner kinds', () => {
  assert.match(burstAnnouncement({ kind: 'burst', text: '' }), /changing fast: auto refresh every 30 s/);
  assert.equal(burstAnnouncement({ kind: 'paused', text: '' }), 'Auto refresh paused.');
  assert.equal(burstAnnouncement(null), 'Auto refresh is back to normal.');
});
