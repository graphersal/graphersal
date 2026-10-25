// The tab strip of the Query panel: one tab per query (click to switch, double-click to rename,
// middle-click or × to close, drag to reorder), "+" for a new tab and a menu with the tab actions.
// A tab shows a pulsing dot while its query runs and an amber dot when its result is possibly
// stale (another tab changed the graph after it ran). "+" opens a menu: a Query tab, a GQL query
// tab (a Query tab in GQL, marked "GQL"; js/gql-tab.js) or an AI Chat tab (its own accent
// colour; js/ai-chat.js). "Share as link…" opens the share dialog, like the
// top bar's Share button (components/share-dialog.js); both are hidden over the dev server (a link
// to the public playground cannot carry a server's graph). "Save as query…" opens the saved-query
// editor on the tab's text (a `g.gql(..)` tab: a query written in GQL). The state and the
// actions are in store.js.

import { ref, nextTick, watch } from 'vue';
import {
  state, switchTab, newTab, newGqlTab, newChatTab, closeTab, closeOtherTabs, duplicateTab, renameTab, moveTab, runAllTabs,
  openShareDialog, saveTabAsQuery, isServerMode,
} from '../store.js';
import { modKey } from '../util.js';
import { DropMenu } from './drop-menu.js';

export const QueryTabs = {
  components: { DropMenu },
  setup(props, { expose }) {
    const editing = ref(null); // the id of the tab being renamed
    const draftName = ref('');
    const strip = ref(null);
    let dragId = null;

    async function startRename(tab) {
      editing.value = tab.id;
      draftName.value = tab.name;
      await nextTick();
      const input = strip.value && strip.value.querySelector('.qtab-input');
      if (input) {
        input.focus();
        input.select();
      }
    }
    function commitRename() {
      if (editing.value === null) return;
      renameTab(editing.value, draftName.value);
      editing.value = null;
    }
    function cancelRename() {
      editing.value = null;
    }

    function onAux(e, tab) {
      if (e.button === 1) {
        e.preventDefault();
        closeTab(tab.id);
      }
    }

    function onDragStart(e, tab) {
      dragId = tab.id;
      if (e.dataTransfer) {
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('text/plain', tab.name);
      }
    }
    function onDrop(e, tab) {
      if (dragId === null || dragId === tab.id) return;
      moveTab(dragId, state.tabs.indexOf(tab));
      dragId = null;
    }

    // Keep the active tab in view when it changes (keyboard switching, a new tab at the end).
    watch(() => state.activeId, async () => {
      await nextTick();
      const el = strip.value && strip.value.querySelector('.qtab.active');
      if (el && el.scrollIntoView) el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    });

    function tabTitle(tab) {
      const parts = [tab.name];
      if (tab.kind === 'ai') parts.push(tab.chat && tab.chat.running ? 'AI Chat, working' : 'AI Chat');
      else if (tab.language === 'gql') parts.push('GQL query');
      if (tab.id === state.runningTabId) parts.push('running');
      if (tab.stale) parts.push('result possibly stale: another tab changed the graph after this tab ran');
      parts.push('double-click to rename, middle-click to close');
      return parts.join(' · ');
    }

    expose({ rename: () => startRename(state.tab) });

    return {
      state, editing, draftName, strip, modKey, tabTitle, openShare: openShareDialog, saveTabAsQuery,
      serverMode: isServerMode,
      switchTab, closeTab, closeOtherTabs, duplicateTab, runAllTabs,
      newTab: () => newTab(), newGqlTab: () => newGqlTab(), newChatTab: () => newChatTab(), startRename, commitRename, cancelRename, onAux, onDragStart, onDrop,
    };
  },
  template: `
  <div class="qtabs" ref="strip">
    <div class="qtab-list" role="tablist" aria-label="Query tabs">
      <div v-for="t in state.tabs" :key="t.id" class="qtab"
           :class="{ active: t.id === state.activeId, stale: t.stale, ai: t.kind === 'ai' }"
           role="tab" :aria-selected="String(t.id === state.activeId)" :tabindex="t.id === state.activeId ? 0 : -1"
           :title="tabTitle(t)" draggable="true"
           @click="switchTab(t.id)" @dblclick="startRename(t)" @auxclick="onAux($event, t)" @mousedown.middle.prevent
           @keydown.enter.prevent="switchTab(t.id)" @keydown.f2.prevent="startRename(t)"
           @dragstart="onDragStart($event, t)" @dragover.prevent @drop.prevent="onDrop($event, t)">
        <span v-if="t.id === state.runningTabId || (t.kind === 'ai' && t.chat.running)" class="running-dot" aria-label="running"></span>
        <span v-else-if="t.kind === 'ai'" class="qtab-ai-mark" aria-hidden="true">✦</span>
        <span v-if="t.kind !== 'ai' && t.language === 'gql'" class="qtab-lang" aria-label="GQL">GQL</span>
        <input v-if="editing === t.id" class="qtab-input" v-model="draftName" maxlength="40" aria-label="Tab name"
               @click.stop @dblclick.stop @keydown.enter.prevent="commitRename" @keydown.esc.stop.prevent="cancelRename" @blur="commitRename">
        <span v-else class="qtab-name">{{ t.name }}</span>
        <span v-if="t.stale" class="qtab-stale" aria-label="result possibly stale">●</span>
        <button class="qtab-close" tabindex="-1" @click.stop="closeTab(t.id)" :title="'Close tab (Alt+W)'" :aria-label="'Close tab ' + t.name">×</button>
      </div>
    </div>
    <drop-menu button-class="icon-btn qtab-add" title="New tab: a query (Alt+N) or an AI Chat" aria-label="New tab" align="start" menu-style="min-width: 200px">
      <template #trigger>+</template>
      <button class="menu-item" @click="newTab">Query <span class="spacer"></span><kbd>Alt+N</kbd></button>
      <button class="menu-item" @click="newGqlTab" title="A query tab in GQL: MATCH … RETURN … with a Parameters field"><span class="qtab-lang" aria-hidden="true">GQL</span> GQL query</button>
      <button class="menu-item" @click="newChatTab"><span class="qtab-ai-mark" aria-hidden="true">✦</span> AI Chat</button>
    </drop-menu>
    <drop-menu button-class="icon-btn" title="Tab actions" aria-label="Tab actions" menu-style="min-width: 240px">
      <template #trigger>⋯</template>
      <button class="menu-item" @click="newTab">New tab <span class="spacer"></span><kbd>Alt+N</kbd></button>
      <button class="menu-item" @click="newGqlTab">New GQL query tab</button>
      <button class="menu-item" @click="newChatTab">New AI Chat tab</button>
      <button class="menu-item" @click="duplicateTab()">Duplicate tab</button>
      <button class="menu-item" @click="startRename(state.tab)">Rename tab</button>
      <div class="menu-sep"></div>
      <button v-if="!serverMode()" class="menu-item" @click="openShare()" :disabled="state.running || !state.graph" title="A link that opens this tab's query with the graph (small graphs only)">Share as link…</button>
      <button class="menu-item" @click="saveTabAsQuery()" :disabled="!state.graph || state.tab.kind === 'ai' || !state.tab.script.trim()"
              title="Store this tab's query in the graph as a saved query: a GQL tab (or a tab holding one g.gql(…) call) is saved as a GQL query with its $parameters">Save as query…</button>
      <div class="menu-sep"></div>
      <button class="menu-item" @click="runAllTabs()" :disabled="state.running">Run all tabs <span class="spacer"></span><kbd>{{ modKey }}+Shift+Enter</kbd></button>
      <div class="menu-sep"></div>
      <button class="menu-item" @click="closeTab()">Close tab <span class="spacer"></span><kbd>Alt+W</kbd></button>
      <button class="menu-item" @click="closeOtherTabs()" :disabled="state.tabs.length < 2">Close other tabs</button>
      <div class="menu-sep"></div>
      <div class="menu-title">Switch: Alt+[ / Alt+] · Alt+Shift+1…9</div>
    </drop-menu>
  </div>
  `,
};
