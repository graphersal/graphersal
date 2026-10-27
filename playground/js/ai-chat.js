// The AI Chat tab's model: pure state logic plus a controller per chat tab with an injectable
// API and timers (no Vue, no DOM), so tests/ai-chat.test.mjs runs it under Node. store.js keeps
// one controller per chat tab; components/ai-chat.js renders the transcript.
//
// The agent runs in the dev server (`graphersal --server --ai FILE`, book page
// persistence/dev_server.md): a page creates a conversation (`POST /api/ai/conversations`, owned
// by the page's `X-Graphersal-Client`), sends a message (a turn runs on the server) and polls the
// conversation's events (`GET .../events?after=N`) until the turn is done. The events are
// `user {text}`, `assistant {text}`, `tool_call {id, name, args}`, `tool_result {id, name, ok,
// summary, text}`, `error {message, help}` and `done {stopped, toolCalls, elapsedMs, toolMs,
// llmCalls, inputTokens, outputTokens, cachedTokens, limitReached}`, each with `seq`.
//
// Isolation: every chat tab owns its own conversation, cursor (`after`), poll loop, Stop and
// error. The transcript and the conversation id persist with the tab (localStorage, shared by
// every browser tab of the playground); the conversation itself belongs to the page that created
// it (one client id per page load), so a restored conversation answers 403 (another page, or this
// page before a reload) or 404 (deleted, server restarted): the transcript stays as history and
// the next Send starts a new conversation and says so.

import { createPoller } from './change-feed.js';
import { exportFileName } from './export.js';

/** Poll interval while a turn runs. */
export const CHAT_POLL_MS = 700;
/** Most transcript items kept with a tab in storage (the newest). */
export const MAX_SAVED_ITEMS = 300;
/** Most characters of a tool result's text kept in storage. */
export const MAX_SAVED_TEXT = 2000;
/** Most characters of an unsent draft kept in storage. */
export const MAX_SAVED_DRAFT = 20000;

/** The chat commands (typed as the whole message; never sent to the model). */
export const COMMANDS = Object.freeze([
  { name: 'clear', text: 'deletes this conversation on the server and clears this tab' },
  { name: 'help', text: 'lists the chat commands' },
]);

export const COMMAND_HINT = '/clear starts over · /help lists the commands';

/** The text of a newly started conversation that replaces a restored one. */
export const RESTARTED_NOTICE =
  'The earlier conversation is not available to this page (it belongs to another page or ended ' +
  'with a reload or a server restart): this message starts a new conversation, the assistant does ' +
  'not see the history above.';

/** An empty chat (runtime fields included). */
export function createChat(saved = null) {
  const s = saved && typeof saved === 'object' ? saved : {};
  const items = Array.isArray(s.items)
    ? s.items.filter((it) => it && typeof it === 'object' && typeof it.type === 'string')
    : [];
  return {
    // The server conversation this tab uses (null: none yet; created by the next Send).
    conversationId: typeof s.conversationId === 'string' && s.conversationId ? s.conversationId : null,
    // Whether THIS page load created that conversation (a restored one is never known to be ours).
    owned: false,
    after: Number.isInteger(s.after) && s.after > 0 ? s.after : 0, // the last event seq merged
    items, // the transcript: server events and local notices/errors, oldest first
    draft: typeof s.draft === 'string' ? s.draft : '',
    running: false, // a turn of this tab's conversation runs
    sending: false, // a Send or /clear is in progress
    stopping: false,
    // Runtime only (not persisted): the turn keys (`groupTurns`) whose step group the person opened.
    openSteps: {},
  };
}

/** What is persisted of a chat: the conversation id, the cursor, the (capped) transcript. */
export function serializeChat(chat) {
  const items = chat.items.slice(-MAX_SAVED_ITEMS).map((it) => {
    if (it.type === 'tool_result' && typeof it.text === 'string' && it.text.length > MAX_SAVED_TEXT) {
      return { ...it, text: `${it.text.slice(0, MAX_SAVED_TEXT)}…` };
    }
    return { ...it };
  });
  return {
    conversationId: chat.conversationId,
    after: chat.after,
    items,
    draft: String(chat.draft || '').slice(0, MAX_SAVED_DRAFT),
  };
}

/**
 * Merges an events answer `{events, running, last}` into the chat: only events with `seq` after
 * the chat's cursor are added (a repeated or overlapping answer adds nothing twice). Returns
 * `{added, done}`: how many were added and whether the turn is over (`running` false).
 */
export function mergeEvents(chat, answer) {
  const events = answer && Array.isArray(answer.events) ? answer.events : [];
  let added = 0;
  for (const ev of events) {
    const seq = Number(ev && ev.seq);
    if (!Number.isInteger(seq) || seq <= chat.after || typeof ev.type !== 'string') continue;
    chat.items.push({ ...ev });
    chat.after = seq;
    added++;
  }
  const running = !!(answer && answer.running);
  chat.running = running;
  return { added, done: !running };
}

/**
 * A chat command: the whole message is `/word` (surrounding blanks allowed). Returns
 * `{name, known}` or null for an ordinary message.
 */
export function parseCommand(text) {
  const m = /^\s*\/([A-Za-z][\w-]*)\s*$/.exec(String(text || ''));
  if (!m) return null;
  const name = m[1].toLowerCase();
  return { name, known: COMMANDS.some((c) => c.name === name) };
}

/** The `/help` text. */
export function helpText() {
  return `Chat commands (typed as the whole message, never sent to the assistant): ${
    COMMANDS.map((c) => `/${c.name} ${c.text}`).join('; ')}.`;
}

/**
 * The tool calls of a transcript with their results: `tool_call` items get `result` (the first
 * later `tool_result` with the same id that no earlier call took: a provider may
 * repeat ids across turns), and the paired results are not listed on their own. Other items
 * pass through. Pure; returns new item objects for tool calls.
 */
export function pairToolCalls(items) {
  const out = [];
  const waiting = new Map(); // id -> the unpaired tool calls in `out`, oldest first
  for (const it of items) {
    if (it.type === 'tool_call') {
      const call = { ...it, result: null };
      out.push(call);
      if (!waiting.has(it.id)) waiting.set(it.id, []);
      waiting.get(it.id).push(call);
    } else if (it.type === 'tool_result') {
      const calls = waiting.get(it.id);
      if (calls && calls.length) calls.shift().result = it;
      else out.push(it);
    } else {
      out.push(it);
    }
  }
  return out;
}

/** The query text of a tool call (the `query` tool's `script`), or null. */
export function toolCallQuery(call) {
  const args = call && call.args;
  if (!args || typeof args !== 'object') return null;
  return typeof args.script === 'string' && args.script.trim() ? args.script : null;
}

/** Most distinct tool names listed in a turn's group summary (then "…"). */
export const SUMMARY_NAMES = 3;

function plural(n, word) {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}

function namesText(names) {
  const shown = names.slice(0, SUMMARY_NAMES).join(', ');
  return names.length > SUMMARY_NAMES ? `${shown}, …` : shown;
}

/**
 * The transcript grouped by turn for display (pure; derived from the stored items, so a
 * restored transcript groups the same way). A turn starts at a `user` item and runs to the next
 * one. Returns a list of entries:
 * - `{kind: 'item', key, item}`: an item before the first turn (a notice, a local error);
 * - `{kind: 'turn', key, user, steps, answer, tail, state, toolCalls, ran, names, current}`:
 *   `steps` are the turn's internal steps in order (interim assistant texts that came with tool
 *   calls, the tool calls paired with their results by `pairToolCalls`, unpaired results);
 *   `answer` the FINAL assistant item (the last one no tool call follows) or null; `tail` what is
 *   shown below the answer (errors, notices, "Stopped."); `state` is `running` (the last turn of a
 *   chat whose turn runs), `done`, `error` (a server error event), `stopped` (Stop) or
 *   `interrupted` (no `done` event and nothing runs: the conversation is gone); `toolCalls` the
 *   number of tool call items, `ran` those with a result, `names` the distinct names of the tool
 *   calls in order, `current` the running step's name (a tool call without result) or null.
 *   A turn without tool calls has no steps (no group is shown for it).
 * @param {Array} items  The chat's transcript (chat.items).
 * @param {{running?: boolean}} [o]  Whether the chat's turn runs (chat.running).
 */
export function groupTurns(items, o = {}) {
  const out = [];
  let turn = null;
  let pending = []; // the current turn's items (raw)
  const close = (last) => {
    if (!turn) return;
    turn.raw = pending;
    finishTurn(turn, last && !!o.running);
    delete turn.raw;
    out.push(turn);
    turn = null;
    pending = [];
  };
  items.forEach((it, i) => {
    if (it.type === 'user') {
      close(false);
      turn = { kind: 'turn', key: `t${i}:${it.seq ?? 'l'}`, user: it };
      pending = [];
    } else if (turn) {
      pending.push(it);
    } else {
      out.push({ kind: 'item', key: `i${i}:${it.seq ?? 'l'}`, item: it });
    }
  });
  close(true);
  return out;
}

/** Splits a turn's raw items into steps, answer and tail and derives its state. */
function finishTurn(turn, running) {
  const paired = pairToolCalls(turn.raw);
  let lastTool = -1;
  paired.forEach((it, i) => {
    if (it.type === 'tool_call' || it.type === 'tool_result') lastTool = i;
  });
  let answerAt = -1;
  for (let i = paired.length - 1; i > lastTool; i--) {
    if (paired[i].type === 'assistant') {
      answerAt = i;
      break;
    }
  }
  const steps = [];
  const tail = [];
  let done = null;
  let serverError = false;
  paired.forEach((it, i) => {
    if (i === answerAt) return;
    if (it.type === 'done') {
      done = it;
      if (it.stopped) tail.push(it);
    } else if (it.type === 'tool_call' || it.type === 'tool_result'
      || (it.type === 'assistant' && i < lastTool)) {
      steps.push(it);
    } else {
      if (it.type === 'error' && !it.local) serverError = true;
      tail.push(it);
    }
  });
  const calls = steps.filter((s) => s.type === 'tool_call');
  const names = [];
  for (const c of calls) if (!names.includes(c.name)) names.push(c.name);
  let state;
  if (done) state = done.stopped ? 'stopped' : serverError ? 'error' : 'done';
  else if (running) state = 'running';
  else state = serverError ? 'error' : 'interrupted';
  const open = calls.filter((c) => !c.result);
  turn.steps = calls.length ? steps : [];
  // Without tool calls nothing is grouped: an interim text cannot exist, unpaired results stay.
  if (!calls.length) tail.unshift(...steps);
  turn.answer = answerAt >= 0 ? paired[answerAt] : null;
  turn.tail = tail;
  turn.state = state;
  turn.toolCalls = calls.length;
  turn.elapsedMs = done && Number.isFinite(done.elapsedMs) ? done.elapsedMs : null;
  turn.toolMs = done && Number.isFinite(done.toolMs) ? done.toolMs : 0;
  const count = (key) => (done && Number.isFinite(done[key]) ? done[key] : null);
  turn.inputTokens = count('inputTokens');
  turn.outputTokens = count('outputTokens');
  turn.cachedTokens = count('cachedTokens') || 0;
  turn.llmCalls = count('llmCalls') || 0;
  turn.limitReached = !!(done && done.limitReached);
  turn.ran = calls.length - open.length;
  turn.names = names;
  turn.current = state === 'running' && open.length ? open[open.length - 1].name : null;
}

/**
 * The one-line summary of a turn's collapsed group: how far it got, e.g. "Worked: 3 tool calls
 * (query, list_saved_queries)", "Working: running query (1 tool call done)", "Stopped after 1 tool
 * call: list_saved_queries". Empty for a turn without tool calls.
 */
export function turnSummary(turn) {
  if (!turn || !turn.toolCalls) return '';
  const names = namesText(turn.names);
  switch (turn.state) {
    case 'running': {
      const step = turn.current ? `running ${turn.current}` : 'waiting for the model';
      return `Working: ${step} (${plural(turn.ran, 'tool call')} done)`;
    }
    case 'done':
      return `Worked: ${plural(turn.ran, 'tool call')} (${names})`;
    default: {
      const verb = turn.state === 'stopped' ? 'Stopped by you'
        : turn.state === 'interrupted' ? 'Interrupted' : 'Stopped';
      // No call finished: the turn ended while its first call(s) ran or were refused.
      if (!turn.ran) return `${verb} during ${names} (no tool call finished)`;
      return `${verb} after ${plural(turn.ran, 'tool call')}: ${names}`;
    }
  }
}

/** A duration for the answer's timing line: "840 ms", "12.4 s", "2 min 5 s". */
export function durationText(ms) {
  if (!Number.isFinite(ms) || ms < 0) return '';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)} s`;
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)} min ${total % 60} s`;
}

/** A token count for the answer's timing line: "830", "15.2k", "1.25M". */
export function tokenCountText(n) {
  if (!Number.isFinite(n) || n < 0) return '';
  if (n < 1000) return String(Math.round(n));
  if (n < 1e6) return `${(n / 1000).toFixed(n < 1e4 ? 2 : 1).replace(/\.?0+$/, '')}k`;
  return `${(n / 1e6).toFixed(2).replace(/\.?0+$/, '')}M`;
}

/**
 * The muted line under a turn's answer: the time the server spent on the turn and the part of it
 * spent in tool calls, the request and answer tokens of its provider calls (the cached part of
 * the request named), and whether the turn reached its tool-call limit, e.g. "12.4 s · tools
 * 3.1 s · tokens 15.2k in (12k cached) / 830 out" (just "800 ms" for a turn without tool time
 * and token counts; "tokens not reported by the provider" when the model answered without
 * them). Empty when the `done` event carries no timing (an older server, an
 * interrupted turn).
 */
export function timingText(turn) {
  if (!turn || turn.elapsedMs == null) return '';
  const parts = [durationText(turn.elapsedMs)];
  if (turn.toolMs > 0) parts.push(`tools ${durationText(turn.toolMs)}`);
  if (turn.inputTokens || turn.outputTokens) {
    const cached = turn.cachedTokens > 0 ? ` (${tokenCountText(turn.cachedTokens)} cached)` : '';
    parts.push(`tokens ${tokenCountText(turn.inputTokens || 0)} in${cached} / ${tokenCountText(turn.outputTokens || 0)} out`);
  } else if (turn.llmCalls > 0) {
    // The model answered, but its provider sent no token counts (`usage`).
    parts.push('tokens not reported by the provider');
  }
  if (turn.limitReached) parts.push('tool-call limit reached');
  return parts.join(' · ');
}

// ---- Export (issue #18): the transcript the page holds as a Markdown or JSON file ----------
//
// Both formats are derived from `groupTurns`, so they show the turns as the tab does. They hold
// only what the page holds: a restored transcript keeps the newest `MAX_SAVED_ITEMS` items and
// tool result texts cut to `MAX_SAVED_TEXT` characters (marked `resultTruncated`).

/** The `format` member of an exported JSON chat. */
export const CHAT_EXPORT_FORMAT = 'graphersal-ai-chat';
/** The `version` member of an exported JSON chat (raised only on an incompatible change). */
export const CHAT_EXPORT_VERSION = 1;

/** Whether a stored tool result text was cut by `serializeChat`. */
function isCutText(text) {
  return typeof text === 'string' && text.length === MAX_SAVED_TEXT + 1 && text.endsWith('…');
}

/** A tail/preamble item as an export entry, or null for one that is not exported. */
function exportEntry(it) {
  switch (it.type) {
    case 'error': return { type: 'error', message: String(it.message ?? ''), help: it.help ? String(it.help) : null };
    case 'notice': return { type: 'notice', text: String(it.text ?? '') };
    case 'assistant': return { type: 'text', text: String(it.text ?? '') };
    case 'done': return it.stopped ? { type: 'stopped' } : null;
    case 'tool_result': return exportTool({ id: it.id, name: it.name, args: null, result: it });
    default: return null;
  }
}

function exportTool(call) {
  const r = call.result;
  const text = r && typeof r.text === 'string' ? r.text : null;
  return {
    type: 'tool',
    id: call.id ?? null,
    name: String(call.name ?? ''),
    args: call.args === undefined ? null : call.args,
    ok: r ? !!r.ok : null,
    summary: r && r.summary != null ? String(r.summary) : null,
    result: text,
    resultTruncated: isCutText(text),
  };
}

function finite(v) {
  return Number.isFinite(v) ? v : null;
}

/**
 * The export model of a transcript (pure): `{preamble, turns}`. `preamble` are the entries before
 * the first question; each turn is `{question, state, steps, answer, tail, done, timing}`:
 * `steps` the interim texts (`{type: 'text', text}`) and tool calls (`{type: 'tool', id, name,
 * args, ok, summary, result, resultTruncated}`; `ok`/`summary`/`result` null without a result),
 * `answer` the final assistant text or null, `tail` the entries below the answer (`error
 * {message, help}`, `notice {text}`, `text {text}`, `stopped`), `done` the figures of the turn's
 * `done` event (`elapsedMs`, `toolMs`, `llmCalls`, `inputTokens`, `outputTokens`,
 * `cachedTokens`, `toolCalls`, `limitReached`, `stopped`; null without one) and `timing` its
 * `timingText` line ('' without one).
 */
export function chatExportModel(items) {
  const list = Array.isArray(items) ? items : [];
  const preamble = [];
  const turns = [];
  groupTurns(list).forEach((en) => {
    if (en.kind === 'item') {
      const e = exportEntry(en.item);
      if (e) preamble.push(e);
      return;
    }
    const raw = list.slice(list.indexOf(en.user) + 1);
    const nextUser = raw.findIndex((it) => it.type === 'user');
    const own = nextUser >= 0 ? raw.slice(0, nextUser) : raw;
    const doneItem = own.find((it) => it.type === 'done') || null;
    turns.push({
      question: String(en.user.text ?? ''),
      state: en.state,
      steps: en.steps.map((s) => (s.type === 'tool_call' ? exportTool(s)
        : s.type === 'assistant' ? { type: 'text', text: String(s.text ?? '') } : exportEntry(s)))
        .filter(Boolean),
      answer: en.answer ? String(en.answer.text ?? '') : null,
      tail: en.tail.map(exportEntry).filter(Boolean),
      done: doneItem ? {
        elapsedMs: finite(doneItem.elapsedMs),
        toolMs: finite(doneItem.toolMs),
        llmCalls: finite(doneItem.llmCalls),
        inputTokens: finite(doneItem.inputTokens),
        outputTokens: finite(doneItem.outputTokens),
        cachedTokens: finite(doneItem.cachedTokens),
        toolCalls: finite(doneItem.toolCalls),
        limitReached: !!doneItem.limitReached,
        stopped: !!doneItem.stopped,
      } : null,
      timing: timingText(en),
    });
  });
  return { preamble, turns };
}

/** The export metadata: only known members, all strings (`chat`, `provider`, `model`, `server`, `graph`). */
function cleanMeta(meta) {
  const out = {};
  for (const key of ['chat', 'provider', 'model', 'server', 'graph']) {
    const v = meta && meta[key];
    if (typeof v === 'string' && v.trim()) out[key] = v.trim();
  }
  if (meta && meta.readOnly === true) out.readOnly = true;
  return out;
}

function exportedAt(meta) {
  const d = meta && meta.exportedAt instanceof Date ? meta.exportedAt : new Date();
  return d.toISOString();
}

/**
 * The transcript as JSON text (pure; stable shape, documented in the book page "Dev Server and
 * MCP", section "AI Chat"): `{format: "graphersal-ai-chat", version: 1, exportedAt, meta:
 * {chat?, provider?, model?, readOnly?, server?, graph?}, preamble, turns}` with `preamble` and
 * `turns` from `chatExportModel`.
 * @param {Array} items  The chat's transcript (chat.items).
 * @param {{chat?: string, provider?: string, model?: string, readOnly?: boolean, server?: string,
 *   graph?: string, exportedAt?: Date}} [meta]
 */
export function chatToJson(items, meta = {}) {
  const model = chatExportModel(items);
  const doc = {
    format: CHAT_EXPORT_FORMAT,
    version: CHAT_EXPORT_VERSION,
    exportedAt: exportedAt(meta),
    meta: cleanMeta(meta),
    preamble: model.preamble,
    turns: model.turns,
  };
  return `${JSON.stringify(doc, null, 2)}\n`;
}

/** A code fence longer than any backtick run in `text` (at least three backticks). */
export function fenceFor(text) {
  let longest = 0;
  for (const m of String(text).matchAll(/`+/g)) longest = Math.max(longest, m[0].length);
  return '`'.repeat(Math.max(3, longest + 1));
}

function fenced(text, lang = '') {
  const body = String(text);
  const fence = fenceFor(body);
  return `${fence}${lang}\n${body}${body.endsWith('\n') ? '' : '\n'}${fence}`;
}

/** Inline code that survives backticks in its text. */
function inlineCode(text) {
  const s = String(text);
  let longest = 0;
  for (const m of s.matchAll(/`+/g)) longest = Math.max(longest, m[0].length);
  const ticks = '`'.repeat(longest + 1);
  const pad = s.startsWith('`') || s.endsWith('`') ? ' ' : '';
  return `${ticks}${pad}${s}${pad}${ticks}`;
}

/** Plain text as a Markdown block quote (every line, empty ones included). */
function quote(text) {
  return String(text).split('\n').map((l) => (l ? `> ${l}` : '>')).join('\n');
}

function argsJson(args) {
  try {
    return JSON.stringify(args, null, 2) ?? String(args);
  } catch (e) {
    return String(args);
  }
}

function entryMarkdown(e) {
  switch (e.type) {
    case 'error': return quote(`**Error:** ${e.message}${e.help ? `\n\n${e.help}` : ''}`);
    case 'notice': return quote(`Note: ${e.text}`);
    case 'text': return e.text;
    case 'stopped': return '*Stopped.*';
    case 'tool': return toolMarkdown(e);
    default: return '';
  }
}

function toolMarkdown(t) {
  const status = t.ok == null ? 'no result' : `${t.ok ? '✓' : '✗'} ${t.summary ?? ''}`.trim();
  const parts = [`**Tool call ${inlineCode(t.name)}**: ${status}`];
  if (t.args != null) parts.push(`Arguments:\n\n${fenced(argsJson(t.args), 'json')}`);
  if (t.result) {
    const label = t.resultTruncated ? 'Result (shortened in the stored transcript):' : 'Result:';
    parts.push(`${label}\n\n${fenced(t.result)}`);
  }
  return parts.join('\n\n');
}

const STATE_NOTES = Object.freeze({
  running: '*Still running when exported.*',
  interrupted: '*Interrupted: the turn has no end (the conversation ended).*',
});

/**
 * The transcript as Markdown text (pure): a title, the metadata, then per turn the question (a
 * block quote), the steps (interim texts; tool calls with their arguments as a fenced JSON block,
 * the result summary and the stored result text, marked when the stored text was shortened), the
 * answer exactly as the model wrote it (it is Markdown already), errors with their help, notices,
 * and the timing line. Code fences are longer than any backtick run they enclose.
 * @param {Array} items  The chat's transcript (chat.items).
 * @param {object} [meta]  As for `chatToJson`.
 */
export function chatToMarkdown(items, meta = {}) {
  const m = cleanMeta(meta);
  const model = chatExportModel(items);
  const out = [`# AI Chat${m.chat ? `: ${m.chat}` : ''}`];
  const facts = [`- Exported: ${exportedAt(meta)}`];
  if (m.model) facts.push(`- Model: ${m.model}${m.provider ? ` (${m.provider} API)` : ''}${m.readOnly ? ', read-only' : ''}`);
  else if (m.provider) facts.push(`- Provider: ${m.provider} API`);
  if (m.server) facts.push(`- Server: ${m.server}`);
  if (m.graph) facts.push(`- Graph: ${m.graph}`);
  out.push(facts.join('\n'));
  for (const e of model.preamble) out.push(entryMarkdown(e));
  if (!model.turns.length && !model.preamble.length) out.push('*The transcript is empty.*');
  model.turns.forEach((t, i) => {
    out.push('---', `## Turn ${i + 1}`, `**You:**\n\n${quote(t.question)}`);
    if (t.steps.length) {
      out.push('### Steps');
      for (const s of t.steps) out.push(entryMarkdown(s));
    }
    if (t.answer != null) out.push('### Answer', t.answer);
    for (const e of t.tail) out.push(entryMarkdown(e));
    if (STATE_NOTES[t.state]) out.push(STATE_NOTES[t.state]);
    if (t.timing) out.push(`*${t.timing}*`);
  });
  return `${out.filter((s) => s !== '').join('\n\n')}\n`;
}

/**
 * The export file name: `graphersal-<chat tab name>-YYYYMMDD-HHMMSS.<ext>`, the name lower case
 * with every run of other characters than a-z and 0-9 as one `-` (at most 40 characters;
 * `chat-` put in front when it does not start with "chat").
 */
export function chatExportFileName(name, ext, d = new Date()) {
  let slug = String(name || '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '')
    .slice(0, 40).replace(/-+$/, '');
  if (!slug.startsWith('chat')) slug = slug ? `chat-${slug}` : 'chat';
  return exportFileName(slug, ext, d);
}

/** The status of an HTTP error thrown by the engine (`err.status`), or 0. */
function statusOf(err) {
  return (err && Number(err.status)) || 0;
}

function messageOf(err) {
  return err && err.message ? err.message : String(err);
}

/**
 * The controller of one chat tab.
 * @param {object} chat  The tab's chat state (createChat; reactive in the page).
 * @param {{api: {create: () => Promise<{id:string}>, send: (id:string, text:string) => Promise<any>,
 *   events: (id:string, after:number) => Promise<{events:Array, running:boolean, last:number}>,
 *   stop: (id:string) => Promise<any>, remove: (id:string) => Promise<any>},
 *   onChange?: () => void, isVisible?: () => boolean, setTimer?: Function, clearTimer?: Function,
 *   intervalMs?: number}} o
 *   `api` errors carry the HTTP status as `status`; `onChange` is called after every change that
 *   should be persisted.
 */
export class ChatController {
  constructor(chat, o) {
    this.chat = chat;
    this.api = o.api;
    this.onChange = o.onChange || (() => {});
    this.timers = { isVisible: o.isVisible, setTimer: o.setTimer, clearTimer: o.clearTimer };
    this.intervalMs = o.intervalMs || CHAT_POLL_MS;
    this.poller = null;
    this.generation = 0; // bumped by /clear and dispose: answers of an older loop are dropped
    this.disposed = false;
  }

  /** Whether a poll loop runs (tests). */
  get polling() {
    return !!(this.poller && this.poller.running);
  }

  notice(text) {
    this.chat.items.push({ type: 'notice', text });
  }

  localError(message, help = '') {
    this.chat.items.push({ type: 'error', local: true, message, help });
  }

  /**
   * The input's Send: a chat command runs locally; any other text goes to this tab's
   * conversation (a new one when there is none, or when the restored one is gone).
   */
  async submit(text) {
    const message = String(text || '').trim();
    if (!message || this.disposed) return false;
    const command = parseCommand(message);
    if (command) {
      if (command.name === 'clear') await this.clear();
      else if (command.name === 'help') this.notice(helpText());
      else this.localError(`Unknown command /${command.name}`, `${helpText()} Anything else is sent to the assistant.`);
      this.onChange();
      return true;
    }
    if (this.chat.running || this.chat.sending) {
      this.localError('A turn is running', 'Wait for the answer, or press Stop.');
      this.onChange();
      return false;
    }
    this.chat.sending = true;
    try {
      let id = this.chat.conversationId;
      let created = false;
      if (!id) {
        id = await this.newConversation();
        created = true;
      }
      try {
        await this.api.send(id, message);
      } catch (err) {
        const status = statusOf(err);
        if (created || (status !== 403 && status !== 404)) throw err;
        this.notice(RESTARTED_NOTICE);
        id = await this.newConversation();
        await this.api.send(id, message);
      }
      this.chat.running = true;
      this.chat.draft = '';
      this.startPolling();
      return true;
    } catch (err) {
      this.localError(messageOf(err), statusOf(err) === 409
        ? 'Wait for the answer, or press Stop.'
        : 'Check that graphersal --server --ai is still running, then send again.');
      return false;
    } finally {
      this.chat.sending = false;
      this.onChange();
    }
  }

  /** Creates a conversation for this tab; the cursor starts over (the transcript stays). */
  async newConversation() {
    const res = await this.api.create();
    if (!res || typeof res.id !== 'string') throw new Error('The dev server did not create a conversation');
    this.chat.conversationId = res.id;
    this.chat.owned = true;
    this.chat.after = 0;
    this.onChange();
    return res.id;
  }

  /** Starts (or restarts) this tab's poll loop on its current conversation. */
  startPolling() {
    this.stopPolling();
    const id = this.chat.conversationId;
    if (!id || this.disposed) return;
    const generation = ++this.generation;
    const poller = createPoller({
      intervalMs: this.intervalMs,
      isVisible: this.timers.isVisible,
      setTimer: this.timers.setTimer,
      clearTimer: this.timers.clearTimer,
      fetchChanges: async () => {
        try {
          return await this.api.events(id, this.chat.after);
        } catch (err) {
          const status = statusOf(err);
          if (status === 403 || status === 404) return { gone: true, message: messageOf(err) };
          throw err; // not reachable right now: the poller tries again
        }
      },
      onAnswer: (answer) => {
        if (generation !== this.generation || this.chat.conversationId !== id) {
          poller.stop();
          return;
        }
        if (answer && answer.gone) {
          poller.stop();
          this.chat.running = false;
          this.chat.conversationId = null;
          this.chat.owned = false;
          this.localError(answer.message, 'The next message starts a new conversation.');
          this.onChange();
          return;
        }
        const { added, done } = mergeEvents(this.chat, answer);
        if (done) {
          poller.stop();
          this.chat.stopping = false;
        }
        if (added || done) this.onChange();
      },
    });
    this.poller = poller;
    poller.start();
  }

  stopPolling() {
    if (this.poller) this.poller.stop();
    this.poller = null;
  }

  /** Polls now (the page became visible again). */
  kick() {
    if (this.poller) this.poller.kick();
  }

  /** Stop: ends this tab's running turn on the server (its query cancelled, the call abandoned). */
  async stop() {
    const id = this.chat.conversationId;
    if (!id || !this.chat.running) return false;
    this.chat.stopping = true;
    try {
      await this.api.stop(id);
    } catch (err) {
      this.chat.stopping = false;
      const status = statusOf(err);
      if (status === 403 || status === 404) {
        this.stopPolling();
        this.chat.running = false;
        this.chat.conversationId = null;
      }
      this.localError(messageOf(err), 'The next message starts a new conversation if this one is gone.');
      this.onChange();
      return false;
    }
    this.kick(); // fetch the `done` event now
    return true;
  }

  /**
   * `/clear`: stops a running turn, deletes the server conversation and empties THIS tab's
   * transcript; the next Send starts a new conversation.
   */
  async clear() {
    const id = this.chat.conversationId;
    this.chat.sending = true;
    this.generation++;
    this.stopPolling();
    try {
      if (id) {
        if (this.chat.running) {
          try {
            await this.api.stop(id);
          } catch (err) {
            /* gone or not ours: nothing to stop */
          }
        }
        try {
          await this.api.remove(id);
        } catch (err) {
          /* gone or not ours: nothing to delete */
        }
      }
    } finally {
      this.chat.conversationId = null;
      this.chat.owned = false;
      this.chat.after = 0;
      this.chat.items.splice(0, this.chat.items.length);
      this.chat.openSteps = {};
      this.chat.running = false;
      this.chat.stopping = false;
      this.chat.sending = false;
      this.onChange();
    }
  }

  /**
   * The tab closes: its poll loop ends; a conversation this page created is deleted on the
   * server (its running turn stopped there); a restored one is left alone.
   */
  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.generation++;
    this.stopPolling();
    const id = this.chat.conversationId;
    if (id && this.chat.owned) this.api.remove(id).catch(() => {});
  }
}
