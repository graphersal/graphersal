// The AI Chat tab of the Query panel (EXPERIMENTAL, dev server with `--ai FILE` only): the
// transcript grouped by turn (`groupTurns`): the person's message, ONE collapsed group with the
// turn's internal steps (interim assistant texts, the tool calls collapsible with the query, a
// result summary and "Open in a query tab"; a live "Working" state while the turn runs), the
// final answer expanded as Markdown via markdown-view.js, then errors with their help, the provider notice, and an input with Send (Enter) and Stop. The model and
// the per-tab controller are in js/ai-chat.js, the actions in store.js. Without `--ai` the tab
// says how to start it; in the WebAssembly playground it is read-only (decision 6).

import { ref, computed, watch, nextTick, onMounted } from 'vue';
import {
  state, aiChatMode, chatController, chatSubmit, chatStop, setChatDraft, openQueryTab,
} from '../store.js';
import {
  groupTurns, turnSummary, timingText, toolCallQuery, COMMAND_HINT, chatToMarkdown, chatToJson, chatExportFileName,
} from '../ai-chat.js';
import { downloadBlob } from '../export.js';
import { AI_CHAT_UNAVAILABLE } from '../engine/engine.js';
import { LINKS } from '../config.js';
import { MarkdownView } from './markdown-view.js';
import { DropMenu } from './drop-menu.js';

/** The book section that explains the AI Chat (relative to the book root). */
const BOOK_PAGE = 'persistence/dev_server.html#ai-chat';

const API_NAMES = Object.freeze({ openai: 'OpenAI-compatible API', anthropic: 'Anthropic API' });

export const AiChat = {
  components: { MarkdownView, DropMenu },
  props: { tab: { type: Object, required: true } },
  setup(props) {
    const list = ref(null);
    const input = ref(null);
    const mode = computed(() => aiChatMode());
    const ai = computed(() => (state.engineInfo && state.engineInfo.ai) || null);
    const chat = computed(() => props.tab.chat);
    const entries = computed(() => groupTurns(chat.value.items, { running: chat.value.running }));
    // The groups the person opened (by turn key, kept with the chat while the page lives); a
    // group starts collapsed, also while its turn runs.
    const opened = computed(() => chat.value.openSteps || {});
    function toggle(turn) {
      if (!chat.value.openSteps) chat.value.openSteps = {};
      chat.value.openSteps[turn.key] = !chat.value.openSteps[turn.key];
    }
    // The bottom "Working…" line only when no group shows the running turn's state.
    const thinking = computed(() => {
      const c = chat.value;
      if (c.stopping || c.sending) return true;
      if (!c.running) return false;
      const list = entries.value;
      const last = list[list.length - 1];
      return !(last && last.kind === 'turn' && last.state === 'running' && last.toolCalls);
    });
    const busy = computed(() => chat.value.running || chat.value.sending);
    const bookUrl = LINKS.book ? new URL(BOOK_PAGE, LINKS.book).href : null;

    // The controller exists while the tab is shown in 'on' mode (Send creates it anyway).
    onMounted(() => {
      chatController(props.tab.id);
      scrollToEnd(true);
      if (input.value && mode.value === 'on') input.value.focus();
    });

    function nearEnd() {
      const el = list.value;
      return !el || el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    }
    async function scrollToEnd(force = false) {
      const stick = force || nearEnd();
      await nextTick();
      if (stick && list.value) list.value.scrollTop = list.value.scrollHeight;
    }
    watch(() => [chat.value.items.length, chat.value.running], () => scrollToEnd());

    const draft = computed({
      get: () => chat.value.draft,
      set: (v) => setChatDraft(props.tab.id, v),
    });

    async function send() {
      const text = chat.value.draft;
      if (!text.trim() || chat.value.sending) return;
      if (chat.value.running && !/^\s*\/\w/.test(text)) return;
      const keep = text;
      setChatDraft(props.tab.id, '');
      await chatSubmit(props.tab.id, keep);
      scrollToEnd(true);
    }
    function onKey(e) {
      if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
        e.preventDefault();
        send();
      }
    }

    const notice = computed(() => {
      const a = ai.value;
      if (!a) return '';
      // `provider` names the API format (`openai` is also what Gemini, Ollama, vLLM speak), not who runs it.
      const api = API_NAMES[a.provider] || `${a.provider} API`;
      const where = a.sendsDataTo ? `Data you query is sent to ${a.sendsDataTo} (${api}).` : `The model runs on this machine (${api}).`;
      return where;
    });

    function argsText(call) {
      try {
        return JSON.stringify(call.args, null, 2);
      } catch (e) {
        return String(call.args);
      }
    }
    function openQuery(call, execute) {
      const script = toolCallQuery(call);
      if (script) openQueryTab({ title: 'From chat', script, execute });
    }

    // Export (issue #18): the transcript this page holds, as Markdown or JSON (js/ai-chat.js).
    function exportChat(kind) {
      const items = chat.value.items;
      if (!items.length) return;
      const now = new Date();
      const a = ai.value || {};
      const meta = {
        chat: props.tab.name,
        provider: a.provider,
        model: a.model,
        readOnly: !!a.readOnly,
        server: state.engineInfo && state.engineInfo.server ? window.location.host : undefined,
        graph: state.graph ? state.graph.name : undefined,
        exportedAt: now,
      };
      const md = kind === 'md';
      const text = md ? chatToMarkdown(items, meta) : chatToJson(items, meta);
      const type = md ? 'text/markdown;charset=utf-8' : 'application/json';
      downloadBlob(new Blob([text], { type }), chatExportFileName(props.tab.name, md ? 'md' : 'json', now));
    }

    return {
      exportChat, state, mode, ai, chat, entries, opened, toggle, turnSummary, timingText, thinking, busy, draft, notice, list, input, bookUrl, COMMAND_HINT,
      AI_CHAT_UNAVAILABLE, send, onKey, stop: () => chatStop(props.tab.id), argsText, toolCallQuery, openQuery,
    };
  },
  template: `
  <div class="ai-chat">
    <div class="ai-chat-head">
      <span class="ai-chat-badge">AI Chat</span>
      <template v-if="mode === 'on' && ai">
        <span class="ai-chat-model mono" :title="'Provider ' + ai.provider + ', model ' + ai.model">{{ ai.model }}</span>
        <span v-if="ai.readOnly" class="badge" title="The agent may only read: the --ai file sets read_only">read-only</span>
        <span class="ai-chat-notice" :class="{ remote: !!ai.sendsDataTo }">{{ notice }}</span>
      </template>
      <span v-else-if="mode === 'off'" class="faint">not enabled on this server</span>
      <span v-else-if="mode === 'wasm'" class="faint">read-only here</span>
      <span class="spacer"></span>
      <drop-menu v-if="mode !== 'off' || chat.items.length" button-class="btn small" title="Download this chat as a file (what the transcript in this page holds)">
        <template #trigger>Export ▾</template>
        <div class="menu-title">This chat as a file</div>
        <button class="menu-item" :disabled="!chat.items.length" @click="exportChat('md')"
                title="The questions, the steps with tool calls and results, the answers, errors and timing as Markdown">Markdown (.md)</button>
        <button class="menu-item" :disabled="!chat.items.length" @click="exportChat('json')"
                title="The same transcript as JSON (format graphersal-ai-chat, version 1)">JSON (.json)</button>
      </drop-menu>
    </div>

    <div class="ai-chat-list" ref="list" role="log" aria-live="polite" aria-label="Chat transcript">
      <div v-if="mode === 'wasm'" class="ai-chat-off">
        <p>{{ AI_CHAT_UNAVAILABLE }}</p>
        <p v-if="bookUrl">The book section <a :href="bookUrl" target="_blank" rel="noopener noreferrer">AI Chat</a> explains the --ai file.</p>
        <p v-else class="faint">The section "AI Chat" of the book page "Dev Server and MCP" explains the --ai file (the online book is coming soon).</p>
      </div>
      <div v-else-if="mode === 'off'" class="ai-chat-off">
        <p>This dev server runs without the AI Chat. Start it with an <span class="mono">--ai</span> file to chat with an agent that works on this graph:</p>
        <pre class="mono">graphersal --graph &lt;source&gt; --server --ai ai.json</pre>
        <p class="faint">ai.json names the provider and the model, e.g. <span class="mono">{"provider": "anthropic", "model": "&lt;id&gt;", "api_key_env": "ANTHROPIC_API_KEY"}</span>; see the section "AI Chat" of the book page "Dev Server and MCP"<template v-if="bookUrl"> (<a :href="bookUrl" target="_blank" rel="noopener noreferrer">open</a>)</template>.</p>
      </div>
      <div v-else-if="!chat.items.length && !busy" class="ai-chat-empty faint">
        Ask about the graph, e.g. "Which people know each other?". The assistant runs queries on this graph; the steps it took are collected above each answer.
      </div>

      <template v-for="en in entries" :key="en.key">
        <template v-if="en.kind === 'item'">
          <div v-if="en.item.type === 'error'" class="chat-error" role="alert">
            <div class="chat-error-message">{{ en.item.message }}</div>
            <div v-if="en.item.help" class="chat-error-help">{{ en.item.help }}</div>
          </div>
          <div v-else-if="en.item.type === 'notice'" class="chat-notice">{{ en.item.text }}</div>
          <div v-else-if="en.item.type === 'assistant'" class="chat-msg assistant"><markdown-view :text="en.item.text"></markdown-view></div>
        </template>
        <div v-else class="chat-turn" :class="'turn-' + en.state">
          <div class="chat-msg user">{{ en.user.text }}</div>
          <div v-if="en.steps.length" class="chat-steps" :class="{ open: opened[en.key] }">
            <button type="button" class="chat-steps-toggle" :aria-expanded="opened[en.key] ? 'true' : 'false'"
                    :aria-controls="'steps-' + tab.id + '-' + en.key" @click="toggle(en)"
                    :title="opened[en.key] ? 'Hide the steps of this turn' : 'Show the steps of this turn: interim texts, tool calls and their results'">
              <span class="chat-steps-chevron" aria-hidden="true">▸</span>
              <span v-if="en.state === 'running'" class="running-dot" aria-hidden="true"></span>
              <span class="chat-steps-summary">{{ turnSummary(en) }}</span>
            </button>
            <div v-if="opened[en.key]" class="chat-steps-body" :id="'steps-' + tab.id + '-' + en.key">
              <template v-for="(it, i) in en.steps" :key="(it.seq || 'l') + ':' + i">
                <div v-if="it.type === 'assistant'" class="chat-msg assistant interim"><markdown-view :text="it.text"></markdown-view></div>
                <details v-else-if="it.type === 'tool_call'" class="chat-tool" :class="{ failed: it.result && !it.result.ok }">
                  <summary>
                    <span class="chat-tool-name mono">{{ it.name }}</span>
                    <span v-if="!it.result" class="faint">{{ en.state === 'running' ? 'running…' : 'no result' }}</span>
                    <span v-else class="chat-tool-summary" :class="it.result.ok ? 'ok' : 'err'">{{ it.result.ok ? '✓' : '✗' }} {{ it.result.summary }}</span>
                  </summary>
                  <div class="chat-tool-body">
                    <template v-if="toolCallQuery(it)">
                      <pre class="mono chat-tool-query">{{ toolCallQuery(it) }}</pre>
                      <div class="chat-tool-actions">
                        <button class="btn small" type="button" @click="openQuery(it, false)">Open in a query tab</button>
                        <button class="btn small primary" type="button" @click="openQuery(it, true)">Run</button>
                      </div>
                    </template>
                    <pre v-else class="mono chat-tool-args">{{ argsText(it) }}</pre>
                    <pre v-if="it.result && it.result.text" class="mono chat-tool-result">{{ it.result.text }}</pre>
                  </div>
                </details>
                <div v-else-if="it.type === 'tool_result'" class="chat-notice faint">{{ it.name }}: {{ it.summary }}</div>
              </template>
            </div>
          </div>
          <div v-if="en.answer" class="chat-msg assistant chat-answer"><markdown-view :text="en.answer.text"></markdown-view></div>
          <template v-for="(it, i) in en.tail" :key="'tail:' + (it.seq || 'l') + ':' + i">
            <div v-if="it.type === 'error'" class="chat-error" role="alert">
              <div class="chat-error-message">{{ it.message }}</div>
              <div v-if="it.help" class="chat-error-help">{{ it.help }}</div>
            </div>
            <div v-else-if="it.type === 'notice'" class="chat-notice">{{ it.text }}</div>
            <div v-else-if="it.type === 'tool_result'" class="chat-notice faint">{{ it.name }}: {{ it.summary }}</div>
            <div v-else-if="it.type === 'assistant'" class="chat-msg assistant"><markdown-view :text="it.text"></markdown-view></div>
            <div v-else-if="it.type === 'done' && it.stopped" class="chat-notice faint">Stopped.</div>
          </template>
          <div v-if="timingText(en)" class="chat-timing faint" title="Time the server spent on this turn, the part of it spent in tool calls, and the tokens the model read (cached ones included) and wrote for this answer">{{ timingText(en) }}</div>
        </div>
      </template>

      <div v-if="thinking" class="chat-thinking faint">
        <span class="running-dot" aria-hidden="true"></span>
        {{ chat.stopping ? 'Stopping…' : chat.sending ? 'Sending…' : 'Working…' }}
      </div>
    </div>

    <div class="ai-chat-input">
      <textarea ref="input" v-model="draft" rows="2" :disabled="mode !== 'on'"
                :placeholder="mode === 'on' ? 'Message the assistant (Enter sends, Shift+Enter: new line)' : ''"
                aria-label="Message to the assistant" @keydown="onKey"></textarea>
      <div class="ai-chat-buttons">
        <button class="btn small primary" type="button" @click="send" :disabled="mode !== 'on' || !draft.trim() || chat.sending"
                title="Send (Enter)">Send</button>
        <button class="btn small stop-btn" type="button" @click="stop" :disabled="mode !== 'on' || !chat.running || chat.stopping"
                title="Stop this chat's turn: the pending model call is abandoned and its running query cancelled">Stop</button>
      </div>
    </div>
    <div class="ai-chat-hint faint">{{ COMMAND_HINT }}</div>
  </div>
  `,
};
