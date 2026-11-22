// Node tests of the dev server's change feed model (js/change-feed.js) and of the HTTP engine's
// client id and `changes()`.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readChanges, createPoller, newClientId, mcpBadgeTitle } from '../js/change-feed.js';
import { HttpEngine } from '../js/engine/http-engine.js';

const HOST = { protocol: 'graphersal/1', server: 'graphersal-dev' };

const feed = (version, commitSeq, recent) => ({ version, commitSeq, recent });

test('readChanges: the first answer is the baseline, never external', () => {
  assert.deepEqual(readChanges(feed(5, 9, [{ version: 5, by: 'mcp', commitSeq: 9 }]), null, 'me'),
    { version: 5, commitSeq: 9, changed: false, external: false });
  assert.deepEqual(readChanges(feed(5, 9, []), 5, 'me'), { version: 5, commitSeq: 9, changed: false, external: false });
});

test('readChanges: own changes are not external, others and gaps are', () => {
  const own = feed(3, 3, [{ version: 2, by: 'me', commitSeq: 2 }, { version: 3, by: 'me', commitSeq: 3 }]);
  assert.equal(readChanges(own, 1, 'me').external, false);
  assert.equal(readChanges(own, 1, 'me').changed, true);
  const mixed = feed(3, 3, [{ version: 2, by: 'mcp', commitSeq: 2 }, { version: 3, by: 'me', commitSeq: 3 }]);
  assert.equal(readChanges(mixed, 1, 'me').external, true, 'an agent changed it before my own change');
  assert.equal(readChanges(mixed, 2, 'me').external, false, 'only my change is new');
  const other = feed(4, 7, [{ version: 4, by: 'tab-2', commitSeq: 7 }]);
  assert.deepEqual(readChanges(other, 3, 'me'), { version: 4, commitSeq: 7, changed: true, external: true });
  // More changes than the feed lists: some are unknown, so reload.
  const gap = feed(40, 40, [{ version: 40, by: 'me', commitSeq: 40 }]);
  assert.equal(readChanges(gap, 1, 'me').external, true);
  // A restarted server counts from 0 again.
  assert.equal(readChanges(feed(0, 0, []), 12, 'me').external, true);
  assert.equal(readChanges(undefined, null, 'me').version, 0);
});

test('createPoller polls while visible, waits for each answer, and stops', async () => {
  const timers = [];
  const setTimer = (fn, ms) => { timers.push({ fn, ms }); return timers.length; };
  const cleared = [];
  let visible = true;
  let polls = 0;
  const answers = [];
  const poller = createPoller({
    fetchChanges: async () => { polls++; if (polls === 2) throw new Error('down'); return { version: polls }; },
    onAnswer: (a) => { answers.push(a.version); },
    isVisible: () => visible,
    intervalMs: 2000,
    setTimer,
    clearTimer: (t) => cleared.push(t),
  });
  poller.start();
  poller.start(); // idempotent
  assert.equal(timers.length, 1);
  assert.equal(timers[0].ms, 0);
  await timers[0].fn();
  assert.deepEqual(answers, [1]);
  assert.equal(timers[1].ms, 2000);
  await timers[1].fn(); // a failed poll is ignored, the next one is scheduled
  assert.deepEqual(answers, [1]);
  visible = false;
  await timers[2].fn(); // hidden: no request
  assert.equal(polls, 2);
  visible = true;
  poller.kick(); // visible again: poll now
  assert.ok(cleared.includes(4));
  assert.equal(timers[4].ms, 0);
  await timers[4].fn();
  assert.deepEqual(answers, [1, 3]);
  poller.stop();
  assert.equal(poller.running, false);
  const before = timers.length;
  await timers[timers.length - 1].fn(); // a timer that fires after stop does nothing
  assert.equal(timers.length, before);
});

test('client ids are distinct; the MCP badge names the agent', () => {
  assert.notEqual(newClientId(), newClientId());
  assert.match(newClientId(), /^tab-/);
  assert.equal(mcpBadgeTitle(null, 'h'), '');
  const t = mcpBadgeTitle({ endpoint: '/mcp', readOnly: true, client: { name: 'claude-code', version: '2.1' } }, '127.0.0.1:9000');
  assert.match(t, /http:\/\/127\.0\.0\.1:9000\/mcp/);
  assert.match(t, /read-only/);
  assert.match(t, /claude-code 2\.1/);
  assert.match(mcpBadgeTitle({ endpoint: '/mcp', readOnly: false, client: null }, 'h'), /no agent connected/);
});

test('the HTTP engine names its page on every request and reads the change feed', async () => {
  const calls = [];
  const fetch = async (url, init = {}) => {
    calls.push({ path: new URL(url).pathname, init });
    return { ok: true, status: 200, text: async () => JSON.stringify({ version: 2, commitSeq: 5, recent: [], view: null, mcp: null }) };
  };
  const info = { ...HOST, version: '0.1.0', mcp: { endpoint: '/mcp', readOnly: false, client: null } };
  const engine = new HttpEngine(info, { base: 'http://127.0.0.1:8080/', fetch, clientId: 'tab-x' });
  assert.equal((await engine.info()).mcp.endpoint, '/mcp');
  const answer = await engine.changes();
  assert.equal(answer.commitSeq, 5);
  assert.equal(calls[0].path, '/api/changes');
  assert.equal(calls[0].init.headers['X-Graphersal-Client'], 'tab-x');
  await engine.execute('1', {});
  assert.equal(calls[1].init.headers['X-Graphersal-Client'], 'tab-x');
  assert.match(new HttpEngine(info, { fetch }).clientId, /^tab-/);
  assert.equal((await new HttpEngine({ ...HOST }, { fetch }).info()).mcp, null);
});
