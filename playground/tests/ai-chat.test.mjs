// Node tests of the AI Chat tab model (js/ai-chat.js) and its tab kind (js/tabs.js): create,
// persist and restore, event merging by `after`, the per-turn grouping of the transcript (C08), the per-tab controller against a fake dev server
// (two tabs stay separate, a restored conversation answering 403/404 starts a new one on Send,
// /clear clears only its tab and deletes, unknown /commands stay local, a closed tab stops its
// loop, a hidden page pauses), the WebAssembly stub and the HTTP engine's endpoints.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as tabs from '../js/tabs.js';
import {
  createChat, serializeChat, mergeEvents, parseCommand, pairToolCalls, toolCallQuery, ChatController,
  RESTARTED_NOTICE, MAX_SAVED_ITEMS, groupTurns, turnSummary,
} from '../js/ai-chat.js';
import { HttpEngine, SERVER_ID } from '../js/engine/http-engine.js';
import { WasmEngine } from '../js/engine/wasm-engine.js';
import { AI_CHAT_UNAVAILABLE } from '../js/engine/engine.js';

/** A fake dev server AI: conversations owned by clients, scripted turns. */
class FakeServer {
  constructor() {
    this.conversations = new Map();
    this.calls = [];
    this.n = 0;
  }

  error(status, message) {
    const err = new Error(message);
    err.status = status;
    return err;
  }

  get(id, client) {
    const c = this.conversations.get(id);
    if (!c) throw this.error(404, `unknown AI Chat conversation "${id}"`);
    if (c.owner !== client) throw this.error(403, 'this AI Chat conversation belongs to another page');
    return c;
  }

  push(c, ev) {
    c.events.push({ ...ev, seq: c.events.length + 1 });
  }

  /** The API of one page (`client`). */
  api(client) {
    const log = (call) => this.calls.push({ client, ...call });
    return {
      create: async () => {
        log({ op: 'create' });
        const id = `c${++this.n}`;
        this.conversations.set(id, { owner: client, events: [], running: false });
        return { id };
      },
      send: async (id, text) => {
        log({ op: 'send', id, text });
        const c = this.get(id, client);
        if (c.running) throw this.error(409, 'a turn of this conversation is running');
        c.running = true;
        this.push(c, { type: 'user', text });
        return { turn: 1 };
      },
      events: async (id, after) => {
        log({ op: 'events', id, after });
        const c = this.get(id, client);
        return { events: c.events.slice(after), running: c.running, last: c.events.length };
      },
      stop: async (id) => {
        log({ op: 'stop', id });
        const c = this.get(id, client);
        const was = c.running;
        if (was) {
          c.running = false;
          this.push(c, { type: 'done', stopped: true, toolCalls: null });
        }
        return { stopped: was };
      },
      remove: async (id) => {
        log({ op: 'remove', id });
        this.get(id, client);
        this.conversations.delete(id);
        return { deleted: true };
      },
    };
  }

  /** The model answers the running turn of `id`. */
  answer(id, text) {
    const c = this.conversations.get(id);
    this.push(c, { type: 'tool_call', id: 't1', name: 'query', args: { script: 'g.v().count()' } });
    this.push(c, { type: 'tool_result', id: 't1', name: 'query', ok: true, summary: '1 result', text: '[6]' });
    this.push(c, { type: 'assistant', text });
    this.push(c, { type: 'done', stopped: false, toolCalls: 1 });
    c.running = false;
  }
}

/** Manual timers: `run()` fires the queued callbacks (bounded). */
function manualTimers() {
  const queue = [];
  return {
    queue,
    setTimer: (fn) => {
      const t = { fn };
      queue.push(t);
      return t;
    },
    clearTimer: (t) => {
      const i = queue.indexOf(t);
      if (i >= 0) queue.splice(i, 1);
    },
    async run(max = 20) {
      for (let n = 0; n < max && queue.length; n++) await queue.shift().fn();
    },
  };
}

function controller(chat, server, client, extra = {}) {
  const timers = manualTimers();
  let changes = 0;
  const c = new ChatController(chat, {
    api: server.api(client),
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer,
    onChange: () => { changes++; },
    ...extra,
  });
  return { c, timers, changes: () => changes };
}

test('an AI Chat tab: created from "+", named Chat N, persisted and restored with its transcript', () => {
  const ts = tabs.restoreTabs(null);
  const chatTab = tabs.addTab(ts, { kind: 'ai' });
  assert.equal(chatTab.kind, 'ai');
  assert.equal(chatTab.name, 'Chat 1');
  assert.equal(chatTab.script, '');
  assert.equal(tabs.addTab(ts, { kind: 'ai' }).name, 'Chat 2');
  assert.equal(ts.tabs[0].kind, 'query');
  assert.equal(ts.tabs[0].chat, null);
  chatTab.chat.conversationId = 'abc';
  chatTab.chat.owned = true;
  chatTab.chat.after = 2;
  chatTab.chat.items.push({ type: 'user', text: 'hi', seq: 1 }, { type: 'assistant', text: 'hello', seq: 2 });
  chatTab.chat.draft = 'unsent';
  const saved = JSON.parse(JSON.stringify(tabs.serializeTabs(ts)));
  const back = tabs.restoreTabs(saved);
  const again = back.tabs.find((t) => t.id === chatTab.id);
  assert.equal(again.kind, 'ai');
  assert.equal(again.chat.conversationId, 'abc');
  assert.equal(again.chat.owned, false, 'a restored conversation is never known to be this page\'s');
  assert.equal(again.chat.after, 2);
  assert.deepEqual(again.chat.items.map((i) => i.text), ['hi', 'hello']);
  assert.equal(again.chat.draft, 'unsent');
  assert.equal(again.chat.running, false);
  // Query tabs keep their shape (no kind/chat keys).
  assert.equal(saved.tabs[0].kind, undefined);
  // An example never replaces a chat; closing the last tab when it is a chat leaves a query tab.
  tabs.activateTab(back, again.id);
  const { reused } = tabs.openExample(back, { title: 'Ex', script: 'g.v()' });
  assert.equal(reused, false);
  const one = tabs.restoreTabs({ tabs: [{ id: 1, name: 'Chat 1', kind: 'ai', script: '', chat: {} }], active: 1 });
  tabs.closeTab(one, 1);
  assert.equal(one.tabs[0].kind, 'query');
  assert.equal(one.tabs[0].name, 'Query 1');
  // A duplicate keeps the transcript as history, without the conversation.
  const dup = tabs.duplicateTab(back, again.id);
  assert.equal(dup.kind, 'ai');
  assert.equal(dup.chat.conversationId, null);
  assert.equal(dup.chat.items.length, 2);
});

test('persistence caps the transcript and long tool results; garbage restores as an empty chat', () => {
  const chat = createChat();
  for (let i = 1; i <= MAX_SAVED_ITEMS + 5; i++) chat.items.push({ type: 'assistant', text: `a${i}`, seq: i });
  chat.items.push({ type: 'tool_result', id: 'x', text: 'y'.repeat(10000), seq: 999 });
  const saved = serializeChat(chat);
  assert.equal(saved.items.length, MAX_SAVED_ITEMS);
  assert.ok(saved.items.at(-1).text.length < 2100);
  assert.equal(chat.items.at(-1).text.length, 10000, 'the live transcript is not cut');
  for (const bad of [null, 1, 'x', { items: 'no', after: -3, conversationId: 7 }, { items: [null, 1, { type: 2 }] }]) {
    const c = createChat(bad);
    assert.deepEqual([c.items.length, c.after, c.conversationId], [0, 0, null]);
  }
});

test('events merge by the after cursor: repeated and overlapping answers add nothing twice', () => {
  const chat = createChat();
  const ev = (seq, type = 'assistant') => ({ type, seq, text: `e${seq}` });
  assert.deepEqual(mergeEvents(chat, { events: [ev(1, 'user'), ev(2)], running: true, last: 2 }), { added: 2, done: false });
  assert.equal(chat.after, 2);
  assert.equal(chat.running, true);
  assert.deepEqual(mergeEvents(chat, { events: [ev(2), ev(3)], running: true, last: 3 }), { added: 1, done: false });
  assert.deepEqual(mergeEvents(chat, { events: [ev(1), ev(2), ev(3)], running: false, last: 3 }), { added: 0, done: true });
  assert.deepEqual(chat.items.map((i) => i.seq), [1, 2, 3]);
  assert.equal(chat.running, false);
  mergeEvents(chat, { events: [{ type: 'assistant' }, { seq: 4 }, null], running: false });
  assert.equal(chat.items.length, 3, 'events without seq or type are ignored');
});

test('tool calls pair with their results; the query of a tool call', () => {
  const items = [
    { type: 'user', text: 'q' },
    { type: 'tool_call', id: 'a', name: 'query', args: { script: 'g.v()' } },
    { type: 'tool_call', id: 'b', name: 'statistics', args: {} },
    { type: 'tool_result', id: 'a', name: 'query', ok: true, summary: 's' },
    { type: 'tool_result', id: 'z', name: 'orphan', ok: false, summary: 'o' },
  ];
  const out = pairToolCalls(items);
  assert.deepEqual(out.map((i) => i.type), ['user', 'tool_call', 'tool_call', 'tool_result']);
  assert.equal(out[1].result.summary, 's');
  assert.equal(out[2].result, null);
  assert.equal(toolCallQuery(out[1]), 'g.v()');
  assert.equal(toolCallQuery(out[2]), null);
});

test('a tool call id repeated in a later turn pairs with its own result', () => {
  const items = [
    { type: 'tool_call', id: 'call_1', name: 'query', args: { script: 'g.v().count()' } },
    { type: 'tool_result', id: 'call_1', name: 'query', ok: true, summary: 'first' },
    { type: 'assistant', text: 'one' },
    { type: 'tool_call', id: 'call_1', name: 'query', args: { script: 'g.e().count()' } },
    { type: 'tool_result', id: 'call_1', name: 'query', ok: false, summary: 'second' },
  ];
  const out = pairToolCalls(items);
  assert.deepEqual(out.map((i) => i.type), ['tool_call', 'assistant', 'tool_call']);
  assert.equal(out[0].result.summary, 'first');
  assert.equal(out[2].result.summary, 'second');
});

// The events of one turn as the dev server emits them (server/ai.rs run_turn).
function seqd(events, from = 1) {
  return events.map((ev, i) => ({ ...ev, seq: from + i }));
}

test('a turn with interim texts and two tool calls: one group, the final answer outside it', () => {
  const items = seqd([
    { type: 'user', text: 'who knows whom?' },
    { type: 'assistant', text: 'I will check the saved queries.' },
    { type: 'tool_call', id: 'a', name: 'list_saved_queries', args: {} },
    { type: 'tool_result', id: 'a', name: 'list_saved_queries', ok: true, summary: 'none', text: '[]' },
    { type: 'assistant', text: 'Now a query.' },
    { type: 'tool_call', id: 'b', name: 'query', args: { script: 'g.v().out("knows")' } },
    { type: 'tool_result', id: 'b', name: 'query', ok: true, summary: '2 rows', text: '...' },
    { type: 'assistant', text: '**marko** knows vadas and josh.' },
    { type: 'done', stopped: false, toolCalls: 2 },
  ]);
  const entries = groupTurns(items, { running: false });
  assert.equal(entries.length, 1);
  const [turn] = entries;
  assert.equal(turn.kind, 'turn');
  assert.equal(turn.user.text, 'who knows whom?');
  assert.deepEqual(turn.steps.map((s) => s.type), ['assistant', 'tool_call', 'assistant', 'tool_call']);
  assert.equal(turn.steps[1].result.summary, 'none');
  assert.equal(turn.answer.text, '**marko** knows vadas and josh.');
  assert.deepEqual(turn.tail, []);
  assert.equal(turn.state, 'done');
  assert.equal(turn.ran, 2);
  assert.equal(turnSummary(turn), 'Worked: 2 tool calls (list_saved_queries, query)');
  // Many distinct names are cut after three.
  assert.equal(turnSummary({ ...turn, ran: 4, names: ['a', 'b', 'c', 'd'] }), 'Worked: 4 tool calls (a, b, c, …)');
});

test('a turn without tool calls has no group; leading notices stay ungrouped', () => {
  const items = [
    { type: 'notice', text: 'hello' },
    ...seqd([
      { type: 'user', text: 'hi' },
      { type: 'assistant', text: 'Hello!' },
      { type: 'done', stopped: false, toolCalls: 0 },
    ]),
  ];
  const entries = groupTurns(items);
  assert.deepEqual(entries.map((e) => e.kind), ['item', 'turn']);
  assert.equal(entries[0].item.text, 'hello');
  const turn = entries[1];
  assert.deepEqual(turn.steps, []);
  assert.equal(turn.answer.text, 'Hello!');
  assert.equal(turn.toolCalls, 0);
  assert.equal(turnSummary(turn), '');
});

test('an error turn: the group stays, the error below it, the summary says how far it got', () => {
  const items = seqd([
    { type: 'user', text: 'q' },
    { type: 'assistant', text: 'Checking.' },
    { type: 'tool_call', id: 'a', name: 'list_saved_queries', args: {} },
    { type: 'tool_result', id: 'a', name: 'list_saved_queries', ok: true, summary: 'none', text: '[]' },
    { type: 'error', message: 'HTTP 500 (the turn stopped after 1 tool call; your next message continues the chat)', help: 'Try again.' },
    { type: 'done', stopped: false, toolCalls: 1 },
  ]);
  const [turn] = groupTurns(items);
  assert.equal(turn.state, 'error');
  assert.equal(turn.answer, null);
  assert.deepEqual(turn.steps.map((s) => s.type), ['assistant', 'tool_call']);
  assert.deepEqual(turn.tail.map((t) => t.type), ['error']);
  assert.equal(turnSummary(turn), 'Stopped after 1 tool call: list_saved_queries');

  // The tool-call limit: calls that were not run have no result and are not counted.
  const limit = seqd([
    { type: 'user', text: 'q' },
    { type: 'tool_call', id: 'a', name: 'query', args: { script: 'g.v()' } },
    { type: 'tool_call', id: 'b', name: 'query', args: { script: 'g.e()' } },
    { type: 'tool_result', id: 'a', name: 'query', ok: true, summary: '6 rows', text: '' },
    { type: 'error', message: 'the turn reached its limit of 1 tool calls', help: 'Send a message to continue' },
    { type: 'done', stopped: false, toolCalls: 1 },
  ]);
  const [t2] = groupTurns(limit);
  assert.equal(t2.toolCalls, 2);
  assert.equal(t2.ran, 1);
  assert.equal(turnSummary(t2), 'Stopped after 1 tool call: query');

  // Stop by the person.
  const stopped = seqd([
    { type: 'user', text: 'q' },
    { type: 'tool_call', id: 'a', name: 'query', args: { script: 'g.v()' } },
    { type: 'done', stopped: true, toolCalls: null },
  ]);
  const [t3] = groupTurns(stopped);
  assert.equal(t3.state, 'stopped');
  assert.deepEqual(t3.tail.map((t) => t.type), ['done']);
  assert.equal(turnSummary(t3), 'Stopped by you during query (no tool call finished)');
});

test('a running turn shows its current step; only the last turn runs', () => {
  const items = seqd([
    { type: 'user', text: 'first' },
    { type: 'assistant', text: 'Done.' },
    { type: 'done', stopped: false, toolCalls: 0 },
    { type: 'user', text: 'second' },
    { type: 'assistant', text: 'Looking.' },
    { type: 'tool_call', id: 'a', name: 'list_saved_queries', args: {} },
    { type: 'tool_result', id: 'a', name: 'list_saved_queries', ok: true, summary: 'none', text: '' },
    { type: 'tool_call', id: 'b', name: 'query', args: { script: 'g.v()' } },
  ]);
  const entries = groupTurns(items, { running: true });
  assert.equal(entries[0].state, 'done');
  const turn = entries[1];
  assert.equal(turn.state, 'running');
  assert.equal(turn.current, 'query');
  assert.equal(turn.answer, null, 'an interim text is never the answer');
  assert.equal(turnSummary(turn), 'Working: running query (1 tool call done)');
  // Between rounds (all results in): waiting for the model.
  const between = groupTurns(items.slice(0, 7), { running: true })[1];
  assert.equal(turnSummary(between), 'Working: waiting for the model (1 tool call done)');
  // The same items when nothing runs (the conversation is gone): interrupted.
  assert.equal(groupTurns(items, { running: false })[1].state, 'interrupted');
});

test('a restored transcript groups the same way as the live one', () => {
  const chat = createChat();
  mergeEvents(chat, {
    running: false,
    events: seqd([
      { type: 'user', text: 'q' },
      { type: 'assistant', text: 'Checking.' },
      { type: 'tool_call', id: 'a', name: 'query', args: { script: 'g.v().count()' } },
      { type: 'tool_result', id: 'a', name: 'query', ok: true, summary: '6', text: 'x'.repeat(5000) },
      { type: 'assistant', text: 'There are 6.' },
      { type: 'done', stopped: false, toolCalls: 1 },
    ]),
  });
  chat.openSteps.whatever = true;
  const restored = createChat(JSON.parse(JSON.stringify(serializeChat(chat))));
  assert.deepEqual(restored.openSteps, {}, 'which groups were open is not persisted');
  const live = groupTurns(chat.items);
  const again = groupTurns(restored.items);
  const shape = (es) => es.map((e) => [e.key, e.state, turnSummary(e), e.answer && e.answer.text,
    e.steps.map((s) => s.type)]);
  assert.deepEqual(shape(again), shape(live));
});

test('chat commands: whole-message /word only', () => {
  assert.deepEqual(parseCommand(' /clear '), { name: 'clear', known: true });
  assert.deepEqual(parseCommand('/HELP'), { name: 'help', known: true });
  assert.deepEqual(parseCommand('/frobnicate'), { name: 'frobnicate', known: false });
  assert.equal(parseCommand('/clear the graph'), null);
  assert.equal(parseCommand('what does /clear do?'), null);
  assert.equal(parseCommand('/'), null);
});

test('Send creates a conversation, polls until done and stops its loop', async () => {
  const server = new FakeServer();
  const chat = createChat();
  const { c, timers } = controller(chat, server, 'page-1');
  assert.equal(await c.submit('How many vertices?'), true);
  assert.equal(chat.conversationId, 'c1');
  assert.equal(chat.owned, true);
  assert.equal(chat.running, true);
  assert.ok(c.polling);
  await timers.run(1);
  assert.deepEqual(chat.items.map((i) => i.type), ['user']);
  server.answer('c1', 'There are **6**.');
  await timers.run();
  assert.deepEqual(chat.items.map((i) => i.type), ['user', 'tool_call', 'tool_result', 'assistant', 'done']);
  assert.equal(chat.after, 5);
  assert.equal(chat.running, false);
  assert.equal(c.polling, false, 'the loop ends with the turn');
  assert.equal(timers.queue.length, 0);
  // The second message reuses the conversation and polls from the cursor.
  await c.submit('and edges?');
  await timers.run(1);
  const polls = server.calls.filter((x) => x.op === 'events');
  assert.equal(polls.at(-1).after, 5);
  assert.equal(server.calls.filter((x) => x.op === 'create').length, 1);
});

test('two chat tabs keep separate conversations, transcripts and cursors', async () => {
  const server = new FakeServer();
  const ts = tabs.restoreTabs(null);
  const a = tabs.addTab(ts, { kind: 'ai' });
  const b = tabs.addTab(ts, { kind: 'ai' });
  const ca = controller(a.chat, server, 'page-1');
  const cb = controller(b.chat, server, 'page-1');
  await ca.c.submit('first');
  await cb.c.submit('second');
  assert.notEqual(a.chat.conversationId, b.chat.conversationId);
  server.answer(a.chat.conversationId, 'answer A');
  await ca.timers.run();
  await cb.timers.run(1);
  assert.deepEqual(a.chat.items.filter((i) => i.type !== 'tool_call' && i.type !== 'tool_result').map((i) => i.text || i.type),
    ['first', 'answer A', 'done']);
  assert.deepEqual(b.chat.items.map((i) => i.text), ['second']);
  assert.equal(a.chat.after, 5);
  assert.equal(b.chat.after, 1);
  assert.equal(a.chat.running, false);
  assert.equal(b.chat.running, true, 'tab B still waits for its own turn');
  // Stop in tab B stops only B's conversation.
  await cb.c.stop();
  await cb.timers.run();
  assert.equal(b.chat.items.at(-1).type, 'done');
  assert.equal(b.chat.items.at(-1).stopped, true);
  assert.deepEqual(server.calls.filter((x) => x.op === 'stop').map((x) => x.id), [b.chat.conversationId]);
});

test('a restored conversation answering 403 or 404 stays history; the next Send starts a new one', async () => {
  for (const [label, prepare] of [
    ['403: another page owns it', (server) => server.conversations.set('old', { owner: 'page-0', events: [], running: false })],
    ['404: gone (server restarted)', () => {}],
  ]) {
    const server = new FakeServer();
    prepare(server);
    const ts = tabs.restoreTabs({
      tabs: [{ id: 1, kind: 'ai', name: 'Chat 1', script: '', chat: {
        conversationId: 'old', after: 2, items: [{ type: 'user', text: 'before', seq: 1 }, { type: 'assistant', text: 'old answer', seq: 2 }],
      } }],
      active: 1,
    });
    const chat = ts.tabs[0].chat;
    const { c, timers } = controller(chat, server, 'page-1');
    assert.equal(await c.submit('hello again'), true, label);
    assert.notEqual(chat.conversationId, 'old', label);
    assert.equal(chat.owned, true, label);
    assert.equal(chat.after, 0, `${label}: the cursor starts over for the new conversation`);
    assert.deepEqual(chat.items.map((i) => i.type), ['user', 'assistant', 'notice'], label);
    assert.equal(chat.items[2].text, RESTARTED_NOTICE);
    await timers.run(1);
    assert.deepEqual(chat.items.map((i) => i.text).slice(-1), ['hello again'], label);
    if (server.conversations.has('old')) {
      assert.deepEqual(server.conversations.get('old').events, [], `${label}: nothing was posted into the other page's conversation`);
    }
  }
});

test('a conversation that disappears while polling ends the turn with an error; the next Send starts anew', async () => {
  const server = new FakeServer();
  const chat = createChat();
  const { c, timers } = controller(chat, server, 'page-1');
  await c.submit('q');
  server.conversations.clear(); // the server restarted
  await timers.run();
  assert.equal(chat.running, false);
  assert.equal(chat.conversationId, null);
  assert.equal(chat.items.at(-1).type, 'error');
  assert.equal(c.polling, false);
  await c.submit('again');
  assert.equal(chat.conversationId, 'c2');
});

test('/clear deletes the conversation and clears only its tab; while a turn runs it stops it first', async () => {
  const server = new FakeServer();
  const a = createChat();
  const b = createChat();
  const ca = controller(a, server, 'page-1');
  const cb = controller(b, server, 'page-1');
  await ca.c.submit('A1');
  await cb.c.submit('B1');
  await ca.timers.run(1);
  await cb.timers.run(1);
  const idA = a.conversationId;
  assert.equal(await ca.c.submit('/clear'), true);
  const ops = server.calls.filter((x) => x.id === idA).map((x) => x.op);
  assert.deepEqual(ops.slice(-2), ['stop', 'remove']);
  assert.equal(server.conversations.has(idA), false);
  assert.deepEqual([a.items.length, a.after, a.conversationId, a.running], [0, 0, null, false]);
  assert.equal(ca.c.polling, false);
  assert.deepEqual(b.items.map((i) => i.text), ['B1'], 'the other tab keeps its transcript');
  assert.ok(server.conversations.has(b.conversationId));
  // The next message starts a new conversation.
  await ca.c.submit('A2');
  assert.notEqual(a.conversationId, idA);
  // /clear of a restored conversation that is not ours: DELETE is refused and ignored.
  const restored = createChat({ conversationId: 'foreign', items: [{ type: 'user', text: 'x', seq: 1 }] });
  server.conversations.set('foreign', { owner: 'page-0', events: [], running: false });
  await controller(restored, server, 'page-1').c.submit('/clear');
  assert.equal(restored.items.length, 0);
  assert.ok(server.conversations.has('foreign'), 'another page\'s conversation is not deleted');
});

test('/help answers locally; an unknown /command is a local error and never reaches the server', async () => {
  const server = new FakeServer();
  const chat = createChat();
  const { c } = controller(chat, server, 'page-1');
  await c.submit('/help');
  assert.equal(chat.items[0].type, 'notice');
  assert.match(chat.items[0].text, /\/clear/);
  await c.submit('/frobnicate');
  assert.equal(chat.items[1].type, 'error');
  assert.equal(chat.items[1].local, true);
  assert.match(chat.items[1].message, /Unknown command \/frobnicate/);
  assert.deepEqual(server.calls, []);
  assert.equal(chat.conversationId, null);
});

test('a closed tab stops its loop and deletes its own conversation; a hidden page pauses polling', async () => {
  const server = new FakeServer();
  const chat = createChat();
  let visible = false;
  const { c, timers } = controller(chat, server, 'page-1', { isVisible: () => visible });
  await c.submit('q');
  await timers.run(3);
  assert.equal(server.calls.filter((x) => x.op === 'events').length, 0, 'hidden: no poll');
  assert.ok(c.polling, 'the loop waits');
  visible = true;
  await timers.run(1);
  assert.equal(server.calls.filter((x) => x.op === 'events').length, 1);
  const id = chat.conversationId;
  c.dispose();
  assert.equal(c.polling, false);
  assert.equal(timers.queue.length, 0, 'no timer left behind');
  await Promise.resolve();
  assert.equal(server.conversations.has(id), false, 'the conversation this page created is deleted');
  assert.equal(await c.submit('more'), false, 'a disposed controller sends nothing');
  // A restored (not owned) conversation is left alone on close.
  server.conversations.set('foreign', { owner: 'page-0', events: [], running: false });
  const restored = createChat({ conversationId: 'foreign' });
  controller(restored, server, 'page-1').c.dispose();
  await Promise.resolve();
  assert.ok(server.conversations.has('foreign'));
});

test('a failing Send shows the error with help in the tab', async () => {
  const chat = createChat();
  const err = new Error('The dev server is not reachable');
  const c = new ChatController(chat, {
    api: { create: async () => { throw err; } },
    setTimer: () => 0,
    clearTimer: () => {},
  });
  assert.equal(await c.submit('q'), false);
  assert.equal(chat.items[0].type, 'error');
  assert.equal(chat.items[0].message, 'The dev server is not reachable');
  assert.ok(chat.items[0].help);
  assert.equal(chat.sending, false);
});

test('the WebAssembly engine is a stub: every AI method rejects with the local-server text', async () => {
  const engine = new WasmEngine();
  for (const call of [() => engine.aiCreateConversation(), () => engine.aiSend('x', 'y'), () => engine.aiEvents('x', 0),
    () => engine.aiStop('x'), () => engine.aiDelete('x')]) {
    await assert.rejects(call, (e) => e.message === AI_CHAT_UNAVAILABLE);
  }
  assert.match(AI_CHAT_UNAVAILABLE, /graphersal --server --ai ai\.json/);
});

test('the HTTP engine: ai info, the conversation endpoints and the status of a refused request', async () => {
  const calls = [];
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    calls.push({ method: init.method || 'GET', path: u.pathname + u.search, body: init.body, client: init.headers['X-Graphersal-Client'] });
    if (u.pathname.endsWith('/gone/events')) return { ok: false, status: 404, text: async () => '{"error":"unknown AI Chat conversation"}' };
    if (u.pathname.endsWith('/other/messages')) return { ok: false, status: 403, text: async () => '{"error":"belongs to another page"}' };
    const body = u.pathname.endsWith('/conversations') ? { id: 'abc' } : u.pathname.endsWith('/events') ? { events: [], running: false, last: 0 } : {};
    return { ok: true, status: 200, text: async () => JSON.stringify(body) };
  };
  const ai = { provider: 'anthropic', model: 'm', readOnly: false, sendsDataTo: 'api.anthropic.com', maxToolCallsPerTurn: 20 };
  const engine = new HttpEngine({ server: SERVER_ID, version: '0.1.0', ai }, { base: 'http://127.0.0.1:8080/', fetch, clientId: 'tab-1' });
  assert.deepEqual((await engine.info()).ai, ai);
  assert.equal((await new HttpEngine({ server: SERVER_ID }, { base: 'http://127.0.0.1:8080/', fetch }).info()).ai, null);
  assert.deepEqual(await engine.aiCreateConversation(), { id: 'abc' });
  await engine.aiSend('abc', 'hello "x"');
  await engine.aiEvents('abc', 7);
  await engine.aiStop('abc');
  await engine.aiDelete('abc');
  assert.deepEqual(calls.map((c) => `${c.method} ${c.path}`), [
    'POST /api/ai/conversations',
    'POST /api/ai/conversations/abc/messages',
    'GET /api/ai/conversations/abc/events?after=7',
    'POST /api/ai/conversations/abc/stop',
    'DELETE /api/ai/conversations/abc',
  ]);
  assert.deepEqual(JSON.parse(calls[1].body), { text: 'hello "x"' });
  assert.ok(calls.every((c) => c.client === 'tab-1'), 'every request names this page');
  await assert.rejects(() => engine.aiEvents('gone', 0), (e) => e.status === 404);
  await assert.rejects(() => engine.aiSend('other', 'x'), (e) => e.status === 403 && /another page/.test(e.message));
});

test('timingText: the turn time and the tool time of the done event', async () => {
  const { durationText, timingText, groupTurns } = await import('../js/ai-chat.js');
  assert.equal(durationText(840), '840 ms');
  assert.equal(durationText(12400), '12.4 s');
  assert.equal(durationText(125000), '2 min 5 s');
  assert.equal(timingText({ elapsedMs: 12400, toolMs: 3100 }), '12.4 s · tools 3.1 s');
  assert.equal(timingText({ elapsedMs: 800, toolMs: 0 }), '800 ms');
  assert.equal(timingText({ elapsedMs: null, toolMs: 0 }), '');
  const [turn] = groupTurns([
    { type: 'user', text: 'q', seq: 1 },
    { type: 'assistant', text: 'a', seq: 2 },
    { type: 'done', stopped: false, toolCalls: 0, elapsedMs: 1500, toolMs: 0, seq: 3 },
  ]);
  assert.equal(timingText(turn), '1.5 s');
});

test('timingText: the tokens of the turn and the tool-call limit', async () => {
  const { tokenCountText, timingText, groupTurns } = await import('../js/ai-chat.js');
  assert.equal(tokenCountText(830), '830');
  assert.equal(tokenCountText(1250), '1.25k');
  assert.equal(tokenCountText(12000), '12k');
  assert.equal(tokenCountText(15234), '15.2k');
  assert.equal(tokenCountText(1000000), '1M');
  const [turn] = groupTurns([
    { type: 'user', text: 'q', seq: 1 },
    { type: 'assistant', text: 'a', seq: 2 },
    { type: 'done', stopped: false, toolCalls: 20, elapsedMs: 12400, toolMs: 3100, llmCalls: 21,
      inputTokens: 15234, outputTokens: 830, cachedTokens: 12000, limitReached: true, seq: 3 },
  ]);
  assert.equal(timingText(turn), '12.4 s · tools 3.1 s · tokens 15.2k in (12k cached) / 830 out · tool-call limit reached');
  assert.equal(timingText({ elapsedMs: 800, toolMs: 0, inputTokens: 40, outputTokens: 5 }), '800 ms · tokens 40 in / 5 out');
});
