// Entry point: mounts the Vue app, global keyboard shortcuts, dialogs and toasts. Routing: router.js.

import { createApp, ref, onMounted, onBeforeUnmount } from 'vue';
import {
  state, init, cycleTheme, reveal, newTab, closeTab, switchTabNumber, switchTabRelative, can, onWorkspaceTab,
} from './store.js';
import { applyRoute } from './router.js';
import { StartPage } from './components/start-page.js';
import { Workspace } from './components/workspace.js';
import { HelpDialog } from './components/help-dialog.js';
import { SettingsDialog } from './components/settings-dialog.js';
import { BuildMissing } from './components/build-missing.js';

const PANEL_KEYS = { Digit1: 'editor', Digit2: 'results', Digit3: 'graph', Digit4: 'schema', Digit5: 'profile' };

const App = {
  components: { StartPage, Workspace, HelpDialog, SettingsDialog, BuildMissing },
  setup() {
    const help = ref(null);
    const settings = ref(null);
    const bootError = ref(null);

    function isEditable(target) {
      return target && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));
    }

    function onKey(e) {
      if (can('help') && (e.key === 'F1' || (e.key === '?' && !isEditable(e.target) && !e.ctrlKey && !e.metaKey))) {
        e.preventDefault();
        help.value && help.value.open();
        return;
      }
      // The query-tab and panel shortcuts belong to the workspace tab (a profile's module tab
      // has none); theme and settings follow the profile.
      if (e.altKey && !e.ctrlKey && !e.metaKey && state.route === 'play' && !onWorkspaceTab()) {
        if (e.code === 'KeyT' && can('theme')) {
          e.preventDefault();
          cycleTheme();
        } else if (e.code === 'Comma' && can('settings')) {
          e.preventDefault();
          settings.value && settings.value.open();
        }
        return;
      }
      if (e.altKey && !e.ctrlKey && !e.metaKey && state.route === 'play') {
        // Query tabs. Ctrl/Cmd+T, +W and +1..9 belong to the browser (most browsers never pass
        // them to a page), so the tab shortcuts use Alt like the panel shortcuts.
        if (e.shiftKey) {
          const digit = /^Digit([1-9])$/.exec(e.code);
          if (digit) {
            e.preventDefault();
            switchTabNumber(Number(digit[1]));
          }
          return;
        }
        if (e.code === 'KeyN') {
          e.preventDefault();
          if (!can('tab.query')) return;
          newTab();
          reveal('editor');
        } else if (e.code === 'KeyW') {
          e.preventDefault();
          closeTab();
        } else if (e.code === 'BracketLeft' || e.code === 'BracketRight') {
          e.preventDefault();
          switchTabRelative(e.code === 'BracketLeft' ? -1 : 1);
        } else if (PANEL_KEYS[e.code]) {
          e.preventDefault();
          reveal(PANEL_KEYS[e.code]);
        } else if (e.code === 'KeyT') {
          e.preventDefault();
          if (can('theme')) cycleTheme();
        } else if (e.code === 'Comma') {
          e.preventDefault();
          if (can('settings')) settings.value && settings.value.open();
        }
      }
    }

    onMounted(async () => {
      window.addEventListener('keydown', onKey, true);
      window.addEventListener('hashchange', applyRoute);
      try {
        await init();
        if (!state.engineMissing) await applyRoute();
      } catch (err) {
        bootError.value = err && err.message ? err.message : String(err);
      }
    });
    onBeforeUnmount(() => {
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('hashchange', applyRoute);
    });

    return {
      state,
      help,
      settings,
      bootError,
      openHelp: () => help.value && help.value.open(),
      openSettings: () => settings.value && settings.value.open(),
    };
  },
  template: `
    <div v-if="bootError" class="boot"><p>The playground failed to start: {{ bootError }}</p></div>
    <div v-else-if="!state.ready" class="boot"><div class="boot-logo"></div><p>Loading the playground and its engine…</p></div>
    <build-missing v-else-if="state.engineMissing"></build-missing>
    <div v-else-if="state.profileError" class="boot profile-error" role="alert"><p><b>The page cannot be shown.</b></p><p>{{ state.profileError }}</p></div>
    <start-page v-else-if="state.route === 'start' && !(state.engineInfo && state.engineInfo.server)" @help="openHelp"></start-page>
    <workspace v-else @help="openHelp" @settings="openSettings"></workspace>
    <help-dialog ref="help"></help-dialog>
    <settings-dialog ref="settings"></settings-dialog>
    <div class="toasts" aria-live="polite">
      <div v-for="t in state.toasts" :key="t.id" class="toast" :class="t.kind">{{ t.message }}<button v-if="t.action" type="button" class="btn small toast-action" @click="t.action.run()">{{ t.action.label }}</button></div>
    </div>
  `,
};

createApp(App).mount('#app');
