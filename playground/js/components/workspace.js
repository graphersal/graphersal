// The IDE-like workspace: a top bar and five panels in two resizable columns.
//
// Layout library: Split.js (global `Split`, loaded as a classic script). Each column is split
// vertically between its visible panels, and the two columns horizontally. Hidden panels are
// left out of the splits; a collapsed panel keeps only its header. Visibility, collapse state and
// sizes are saved in localStorage (best effort).

import { reactive, ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from 'vue';
import { state, reveal, cycleTheme, saveGraph, saveSchema, saveToSource, isServerMode, backToLiveGraph, runStoreTask, toast, refreshSavedQueries, refreshCompressions, refreshIndexes, registerShareDialog, openShareDialog, openOptimize } from '../store.js';
import { viewBanner, backupBanner } from '../store-model.js';
import { mcpBadgeTitle } from '../change-feed.js';
import { navigate } from '../router.js';
import { storage, debounce, splitGraphName } from '../util.js';
import { formatCount } from '../statistics.js';
import { QueryEditor } from './query-editor.js';
import { ResultsPanel } from './results-panel.js';
import { GraphPanel } from './graph-panel.js';
import { SchemaPanel } from './schema-panel.js';
import { ProfilePanel } from './profile-panel.js';
import { DropMenu, LAYOUT_EVENT } from './drop-menu.js';
import { StoreMenu } from './store-menu.js';
import { CatalogMenu, CatalogDialogs } from './catalog.js';
import { OptimizePanel } from './optimize.js';
import { ShareDialog } from './share-dialog.js';
import { ChangeBurstBanner } from './change-burst-banner.js';

export const PANELS = [
  { id: 'editor', title: 'Query', column: 'left', key: 'Alt+1' },
  { id: 'results', title: 'Results', column: 'left', key: 'Alt+2' },
  { id: 'graph', title: 'Graph', column: 'right', key: 'Alt+3' },
  { id: 'schema', title: 'Schema', column: 'right', key: 'Alt+4' },
  { id: 'profile', title: 'Profile', column: 'right', key: 'Alt+5' },
];

const HEADER_PX = 36;
const GUTTER_PX = 8;
const NARROW_PX = 760;
const MIN_OPEN_PX = 90; // smallest height of an expanded panel in a split
const EXPAND_SHARE = 45; // % of the column an expanded panel gets when it has no usable last size
const USABLE_SHARE = 25; // a last expanded share below this is not worth restoring
const EXPAND_MIN_PX = 240; // an expanded panel shows its header, toolbar and a few rows

function defaultLayout() {
  return {
    visible: { editor: true, results: true, graph: true, schema: true, profile: true },
    collapsed: { editor: false, results: false, graph: false, schema: true, profile: true },
    // Relative weights of the expanded panels of a column. Schema and Profile start collapsed;
    // their weights make the first expand a usable 45% of the right column (Graph keeps 55%).
    sizes: { editor: 35, results: 65, graph: 55, schema: 45, profile: 45 },
    colSizes: [52, 48],
  };
}

function loadLayout() {
  const saved = storage.get('layout', null);
  const base = defaultLayout();
  if (!saved || typeof saved !== 'object') return base;
  return {
    visible: { ...base.visible, ...(saved.visible || {}) },
    collapsed: { ...base.collapsed, ...(saved.collapsed || {}) },
    sizes: { ...base.sizes, ...(saved.sizes || {}) },
    colSizes: Array.isArray(saved.colSizes) && saved.colSizes.length === 2 ? saved.colSizes : base.colSizes,
  };
}

const elementStyle = (dimension, size, gutterSize) => ({ 'flex-basis': `calc(${size}% - ${gutterSize}px)` });
const gutterStyle = (dimension, gutterSize) => ({ 'flex-basis': `${gutterSize}px` });

export const Workspace = {
  components: { QueryEditor, ResultsPanel, GraphPanel, SchemaPanel, ProfilePanel, DropMenu, StoreMenu, CatalogMenu, CatalogDialogs, OptimizePanel, ShareDialog, ChangeBurstBanner },
  emits: ['help', 'settings'],
  setup(props, { emit }) {
    const layout = reactive(loadLayout());
    const shareDialog = ref(null);
    onMounted(() => registerShareDialog(() => shareDialog.value && shareDialog.value.open()));
    onBeforeUnmount(() => registerShareDialog(null));
    const main = ref(null);
    const left = ref(null);
    const right = ref(null);
    const panelRefs = { editor: ref(null), results: ref(null), graph: ref(null), schema: ref(null), profile: ref(null) };
    const narrow = ref(false);
    // The maximised panel id, or null. Deliberately NOT part of `layout`: it is never persisted,
    // and restoring brings back the normal layout untouched.
    const maximized = ref(null);
    let lastPanel = null; // the panel the user last clicked or focused (target of Alt+M)
    let splits = [];

    const persist = () => storage.set('layout', JSON.parse(JSON.stringify(layout)));
    const columnPanels = (col) => PANELS.filter((p) => p.column === col).map((p) => p.id);
    const isShown = (id) => (maximized.value ? maximized.value === id : layout.visible[id]);
    const isCollapsed = (id) => !maximized.value && layout.collapsed[id];
    const leftCount = computed(() => columnPanels('left').filter(isShown).length);
    const rightCount = computed(() => columnPanels('right').filter(isShown).length);

    const panelEl = (id) => {
      const r = panelRefs[id].value;
      return r ? r.$el : null;
    };

    function clearStyles(el) {
      if (!el) return;
      el.style.flexBasis = '';
      el.style.flex = '';
      el.style.flexGrow = '';
      el.style.flexShrink = '';
    }

    function rebuild() {
      if (typeof window.Split !== 'function') return;
      splits.forEach((s) => s.destroy());
      splits = [];
      [left.value, right.value, ...PANELS.map((p) => panelEl(p.id))].forEach(clearStyles);
      narrow.value = window.innerWidth < NARROW_PX;

      const cols = [
        { el: left.value, ids: columnPanels('left').filter(isShown) },
        { el: right.value, ids: columnPanels('right').filter(isShown) },
      ].filter((c) => c.el && c.ids.length);

      if (cols.length === 2) {
        cols.forEach((c) => { c.el.style.flexGrow = '0'; c.el.style.flexShrink = '0'; });
        splits.push(window.Split(cols.map((c) => c.el), {
          direction: narrow.value ? 'vertical' : 'horizontal',
          sizes: layout.colSizes,
          minSize: narrow.value ? 120 : 280,
          gutterSize: GUTTER_PX,
          snapOffset: 0,
          elementStyle,
          gutterStyle,
          onDragEnd: (sizes) => { layout.colSizes = sizes.map((s) => Math.round(s * 10) / 10); persist(); },
        }));
      } else if (cols.length === 1) {
        cols[0].el.style.flex = '1 1 auto';
      }

      for (const col of cols) {
        const els = col.ids.map(panelEl);
        const open = col.ids.filter((id) => !isCollapsed(id));
        if (col.ids.length === 1 || open.length === 0) {
          els.forEach((el, i) => {
            el.style.flex = isCollapsed(col.ids[i]) ? '0 0 auto' : '1 1 auto';
            el.style.marginBottom = col.ids.length > 1 ? '6px' : '';
          });
          continue;
        }
        els.forEach((el) => { el.style.marginBottom = ''; el.style.flexGrow = '0'; el.style.flexShrink = '0'; });
        const height = Math.max(col.el.clientHeight, 200);
        const headerPct = ((HEADER_PX + GUTTER_PX) / height) * 100;
        const collapsedCount = col.ids.length - open.length;
        const weightSum = open.reduce((s, id) => s + (layout.sizes[id] || 30), 0);
        // (collapsed panels get exactly their header; the open ones share the rest by weight)
        const free = 100 - headerPct * collapsedCount;
        const sizes = col.ids.map((id) => (isCollapsed(id) ? headerPct : (free * (layout.sizes[id] || 30)) / weightSum));
        splits.push(window.Split(els, {
          direction: 'vertical',
          sizes,
          minSize: col.ids.map((id) => (isCollapsed(id) ? HEADER_PX : MIN_OPEN_PX)),
          gutterSize: GUTTER_PX,
          snapOffset: 0,
          elementStyle,
          gutterStyle,
          onDragEnd: (s) => {
            col.ids.forEach((id, i) => { if (!isCollapsed(id)) layout.sizes[id] = Math.round(s[i] * 10) / 10; });
            persist();
          },
        }));
      }
      window.dispatchEvent(new Event(LAYOUT_EVENT)); // open menus follow or close
    }

    const scheduleRebuild = () => nextTick(rebuild);
    const onResize = debounce(rebuild, 150);

    /**
     * Gives a panel that is being expanded a usable share of its column: its last expanded size
     * when that is usable, otherwise EXPAND_SHARE (and at least EXPAND_MIN_PX), taken from the
     * other expanded panels of the column in proportion to their sizes, never below their minimum.
     * Works on the relative weights in `layout.sizes`, so persistence is unchanged.
     */
    function makeRoom(id) {
      const panel = PANELS.find((p) => p.id === id);
      const colEl = panel.column === 'left' ? left.value : right.value;
      const others = columnPanels(panel.column).filter((o) => o !== id && layout.visible[o] && !layout.collapsed[o]);
      if (!others.length) return; // alone in its column: it gets everything anyway
      const height = Math.max(colEl ? colEl.clientHeight : 0, 300);
      const minPct = (px) => (px / height) * 100;
      const sum = others.reduce((t, o) => t + (layout.sizes[o] || 30), 0) + (layout.sizes[id] || 0);
      const current = ((layout.sizes[id] || 0) / sum) * 100; // the share its stored weight would give
      let target = current >= USABLE_SHARE ? current : EXPAND_SHARE;
      target = Math.max(target, minPct(EXPAND_MIN_PX));
      target = Math.min(target, 100 - others.length * minPct(MIN_OPEN_PX + GUTTER_PX)); // others keep their minimum
      const otherSum = others.reduce((t, o) => t + (layout.sizes[o] || 30), 0);
      const rest = 100 - target;
      others.forEach((o) => { layout.sizes[o] = Math.round(((layout.sizes[o] || 30) / otherSum) * rest * 10) / 10; });
      layout.sizes[id] = Math.round(target * 10) / 10;
    }

    function toggleCollapse(id) {
      maximized.value = null; // any layout change from the menu restores the normal layout
      if (layout.collapsed[id]) makeRoom(id);
      layout.collapsed[id] = !layout.collapsed[id];
      persist();
    }
    function hide(id) {
      layout.visible[id] = false;
      persist();
    }
    function show(id) {
      if (!layout.visible[id] || layout.collapsed[id]) makeRoom(id);
      layout.visible[id] = true;
      layout.collapsed[id] = false;
      persist();
    }
    function toggleVisible(id) {
      maximized.value = null; // any layout change from the menu restores the normal layout
      if (layout.visible[id]) hide(id);
      else show(id);
    }
    function resetLayout() {
      maximized.value = null; // any layout change from the menu restores the normal layout
      Object.assign(layout, defaultLayout());
      persist();
      scheduleRebuild(); // sizes changed even when visibility/collapse did not
    }
    function showAll() {
      maximized.value = null; // any layout change from the menu restores the normal layout
      PANELS.forEach((p) => { layout.visible[p.id] = true; });
      persist();
    }

    watch(() => [JSON.stringify(layout.visible), JSON.stringify(layout.collapsed), maximized.value], scheduleRebuild);
    // The catalog follows the graph: a load, a run that changed it (a define_query or
    // define_compression or create_index in a script too), a change made elsewhere (change feed), a store swap.
    watch(() => state.graphVersion, () => {
      refreshSavedQueries();
      refreshCompressions();
      refreshIndexes();
    }, { immediate: true });
    watch(() => state.focusPanel, (req) => {
      if (!req) return;
      if (maximized.value && maximized.value !== req.panel) maximized.value = null;
      if (!layout.visible[req.panel] || layout.collapsed[req.panel]) show(req.panel);
      nextTick(() => {
        const el = panelEl(req.panel);
        if (el && req.panel !== 'editor') el.scrollIntoView({ block: 'nearest' });
        if (req.panel === 'editor' && panelRefs.editor.value) panelRefs.editor.value.focus();
      });
    });

    function toggleMaximize(id) {
      maximized.value = maximized.value === id ? null : id;
      lastPanel = id;
    }

    function trackPanel(e) {
      const el = e.target && e.target.closest && e.target.closest('[data-panel]');
      if (el) lastPanel = el.dataset.panel;
    }

    function onKey(e) {
      // Alt+M toggles maximise of the focused (last clicked) panel.
      if (e.altKey && !e.ctrlKey && !e.metaKey && e.code === 'KeyM') {
        e.preventDefault();
        const active = document.activeElement && document.activeElement.closest && document.activeElement.closest('[data-panel]');
        const id = maximized.value || (active && active.dataset.panel) || lastPanel || 'results';
        toggleMaximize(id);
        return;
      }
      // Esc restores, unless something else used it (completion list, dialog, open menu).
      if (e.key === 'Escape' && maximized.value && !e.defaultPrevented &&
          !document.querySelector('dialog[open], .popover')) {
        maximized.value = null;
      }
    }


    onMounted(() => {
      rebuild();
      window.addEventListener('resize', onResize);
      window.addEventListener('keydown', onKey);
      document.addEventListener('pointerdown', trackPanel, true);
      document.addEventListener('focusin', trackPanel, true);
    });
    onBeforeUnmount(() => {
      splits.forEach((s) => s.destroy());
      splits = [];
      window.removeEventListener('resize', onResize);
      window.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', trackPanel, true);
      document.removeEventListener('focusin', trackPanel, true);
    });

    return {
      formatCount, shareDialog, openShareDialog, openOptimize,
      graphName: computed(() => splitGraphName(state.graph ? state.graph.name : '')),
      state, layout, main, left, right, narrow, leftCount, rightCount, PANELS,
      editorRef: panelRefs.editor, resultsRef: panelRefs.results, graphRef: panelRefs.graph,
      schemaRef: panelRefs.schema, profileRef: panelRefs.profile,
      toggleCollapse, hide, show, toggleVisible, resetLayout, showAll, cycleTheme, maximized, isShown, isCollapsed, toggleMaximize, saveGraph, saveSchema, saveToSource,
      serverMode: computed(isServerMode),
      changeGraph: () => navigate('start'),
      location: window.location,
      openHelp: () => emit('help'),
      openSettings: () => emit('settings'),
      reveal,
      viewBanner,
      backupBanner,
      mcpBadgeTitle,
      leaveView: async () => {
        try {
          const res = await runStoreTask('live', backToLiveGraph);
          if (!res.refused) toast('Back to the live graph.', 'info', 4000);
        } catch (err) {
          toast(`Back to the live graph failed: ${err && err.message ? err.message : err}`, 'error', 9000);
        }
      },
    };
  },
  template: `
  <div class="workspace">
    <header class="topbar">
      <a class="brand" href="#/" title="Back to the start page">Graphersal <small class="hide-narrow">Playground</small></a>
      <span v-if="state.versionLabel" class="version-label hide-narrow" :title="state.versionLabel.title">{{ state.versionLabel.text }}</span>
      <span v-if="serverMode" class="badge danger dev-badge"
            :title="'Connected to the dev server (graphersal --server) on ' + location.host + ': DEV mode, unstable, local only, not for production. ' + (state.engineInfo.store ? 'Queries run in that process on the graph of the Store directory ' + state.engineInfo.store + ': every commit is persisted and survives a restart.' : 'Queries run in that process on its in-memory graph' + (state.engineInfo.source ? ' (from ' + state.engineInfo.source + ')' : '') + '; nothing is saved unless you save it.')">DEV SERVER · unstable</span>
      <span v-if="serverMode && state.engineInfo.mcp" class="badge accent mcp-badge"
            :title="mcpBadgeTitle(state.engineInfo.mcp, location.host)">MCP<template v-if="state.engineInfo.mcp.readOnly"> · read-only</template></span>
      <template v-if="state.graph">
        <span class="graph-name" :title="state.graph.name"><span v-if="graphName.dir" class="graph-name-dir"><bdi>{{ graphName.dir }}</bdi></span><span class="graph-name-base">{{ graphName.base }}</span></span>
        <span class="badge" :title="state.graph.vertexCount + ' vertices, ' + state.graph.edgeCount + ' edges'">
          {{ formatCount(state.graph.vertexCount) }} V · {{ formatCount(state.graph.edgeCount) }} E
        </span>
      </template>
      <span class="spacer"></span>
      <button v-if="state.graph && !serverMode" type="button" class="btn share-btn" @click="openShareDialog" :disabled="state.running"
              title="Share as link: a link that opens this tab's query with the graph (small graphs only)">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M6.5 9.5a3 3 0 0 0 4.2 0l2.6-2.6a3 3 0 0 0-4.2-4.2L8 3.8" /><path d="M9.5 6.5a3 3 0 0 0-4.2 0L2.7 9.1a3 3 0 0 0 4.2 4.2L8 12.2" /></svg>
        <span>Share<span class="hide-narrow"> link</span></span>
      </button>
      <drop-menu button-class="btn" title="Show, hide and collapse panels" menu-style="min-width: 260px">
        <template #trigger>View ▾</template>
          <div class="menu-title">Panels</div>
          <div v-for="p in PANELS" :key="p.id" class="menu-item" style="cursor:default">
            <input type="checkbox" :id="'view-' + p.id" :checked="layout.visible[p.id]" @change="toggleVisible(p.id)"
                   :aria-label="(layout.visible[p.id] ? 'Hide panel ' : 'Show panel ') + p.title">
            <label :for="'view-' + p.id" style="cursor:pointer">{{ p.title }}</label>
            <span class="spacer"></span>
            <!-- Same disclosure convention as the panel headers: right = collapsed (expand), down = expanded (collapse). -->
            <button class="icon-btn" :disabled="!layout.visible[p.id]" @click="toggleCollapse(p.id)"
                    :title="layout.collapsed[p.id] ? 'Expand panel' : 'Collapse panel'"
                    :aria-label="(layout.collapsed[p.id] ? 'Expand panel ' : 'Collapse panel ') + p.title"
                    :aria-expanded="String(!layout.collapsed[p.id])">
              <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
                <path v-if="layout.collapsed[p.id]" d="M6 4l4 4-4 4"/><path v-else d="M4 6l4 4 4-4"/>
              </svg>
            </button>
            <kbd>{{ p.key }}</kbd>
          </div>
          <div class="menu-sep"></div>
          <button class="menu-item" @click="showAll">Show all panels</button>
          <button class="menu-item" @click="resetLayout">Reset layout</button>
      </drop-menu>
      <catalog-menu v-if="state.graph"></catalog-menu>
      <button v-if="state.graph" type="button" class="btn hide-narrow" @click="openOptimize()"
              title="Optimize: the storage advisor's recommendations (indexes, compression) with Apply, the workload statistics with charts, the memory report">Optimize</button>
      <store-menu v-if="(serverMode && state.engineInfo.store) || (state.engineInfo && state.engineInfo.storeMemory && state.graph)"></store-menu>
      <drop-menu v-if="state.graph" class="hide-narrow" button-class="btn" title="Download the graph data, its schema or a snapshot: one file per entry" menu-style="min-width: 300px">
        <template #trigger>{{ state.saving ? 'Saving…' : 'Save ▾' }}</template>
          <div class="menu-title">Graph data (no schema inside)</div>
          <button class="menu-item" :disabled="state.saving || state.running" @click="saveGraph('graphson')"
                  title="GraphSON 3.0 (graphersal-graph-YYYYMMDD-HHMMSS.json): TinkerPop's format, keeps every value type; Stop restores this state">Data as GraphSON (.json)</button>
          <button class="menu-item" :disabled="state.saving || state.running" @click="saveGraph('graphml')"
                  title="GraphML (graphersal-graph-YYYYMMDD-HHMMSS.graphml)">Data as GraphML (.graphml)</button>
          <div class="menu-sep"></div>
          <div class="menu-title">Schema</div>
          <button class="menu-item" :disabled="state.saving || state.running" @click="saveSchema"
                  title="The stored schema as JSON (graphersal-schema-YYYYMMDD-HHMMSS.json); load it back as the schema file of Load from file, together with the graph data">Schema (.json)</button>
          <div class="menu-sep"></div>
          <div class="menu-item muted" style="cursor:default;white-space:normal;font-size:12px">Saved queries and compression rules: Catalog ▸ Save catalog.</div>
          <template v-if="state.engine && state.engine.supportsSnapshots">
            <div class="menu-sep"></div>
            <div class="menu-title">Everything in one file</div>
            <button class="menu-item" :disabled="state.saving || state.running" @click="saveGraph('snapshot')"
                    title="One packed snapshot (.gsnap): lossless binary with every value type, the schema and the catalog (saved queries, compression rules); load it back with Load from file">Snapshot (.gsnap): data + schema + catalog</button>
          </template>
      </drop-menu>
      <button v-if="serverMode && state.engineInfo.saveable" class="btn hide-narrow" @click="saveToSource" :disabled="!state.graph || state.saving || state.running"
              :title="'Write the graph back to ' + state.engineInfo.source + ' (on the server; atomic). Never automatic.'">Save to file</button>
      <button v-if="!serverMode" class="btn hide-narrow" @click="changeGraph" title="Choose another graph">Change graph</button>
      <button class="icon-btn" @click="openSettings" title="Settings: display limits (Alt+,)" aria-label="Settings">⚙</button>
      <button class="icon-btn" @click="cycleTheme" :title="'Theme: ' + state.theme + ' (Alt+T)'" aria-label="Toggle theme">
        <span v-if="state.theme === 'dark'">☾</span><span v-else-if="state.theme === 'light'">☀</span><span v-else>◐</span>
      </button>
      <button class="icon-btn" @click="openHelp" title="Help and keyboard shortcuts (F1)" aria-label="Help">?</button>
    </header>
    <div v-if="serverMode && state.engineInfo.maintenance" class="notice" role="alert">
      <b>Maintenance mode: read-only.</b> The store {{ state.engineInfo.store }} has damage
      ({{ state.engineInfo.maintenance.damage.length }} item(s){{ state.engineInfo.maintenance.recoverable ? ', every one with a donor' : ', DATA IS LOST without a backup' }}).
      Queries read the intact data at commit {{ state.engineInfo.maintenance.commitSeq }}; writes are refused.
      Repair into a new directory: <code>graphersal store repair {{ state.engineInfo.store }} --to &lt;new_dir&gt;</code>.
      The damage report is in the Store menu.
    </div>
    <div v-if="serverMode && state.engineInfo.damageFound" class="notice" role="alert">
      <b>{{ state.engineInfo.damageFound.frozen ? 'Damage found: read-only.' : 'Damage found on disk.' }}</b>
      {{ state.engineInfo.damageFound.text }} The Store menu has Back up from memory.
    </div>
    <div v-if="serverMode && state.engineInfo.backup" class="notice" role="status">
      <b>Backup: read-only.</b> {{ backupBanner(state.engineInfo.backup) }}
    </div>
    <div v-if="state.storeView" class="notice info store-view-banner" role="status">
      <b>{{ viewBanner(state.storeView) }}.</b>
      Queries read this past state of the store; writes are refused. The live graph is unchanged.
      <button class="btn small" :disabled="state.storeBusy || !!state.storeTask" :aria-busy="String(!!state.storeTask && state.storeTask.id === 'live')" @click="leaveView">{{ state.storeTask && state.storeTask.id === 'live' ? 'Going back…' : 'Back to the live graph' }}</button>
    </div>
    <change-burst-banner v-if="serverMode"></change-burst-banner>

    <div class="ws-main" :class="{ vertical: narrow }" ref="main" v-show="leftCount + rightCount > 0">
      <div class="ws-col" ref="left" v-show="leftCount > 0">
        <query-editor ref="editorRef" v-show="isShown('editor')" :collapsed="layout.collapsed.editor" :maximized="maximized === 'editor'" data-panel="editor" @toggle-maximize="toggleMaximize('editor')"
                      shortcut="Alt+1" @toggle-collapse="toggleCollapse('editor')" @hide="hide('editor')"></query-editor>
        <results-panel ref="resultsRef" v-show="isShown('results')" :collapsed="layout.collapsed.results" :maximized="maximized === 'results'" data-panel="results" @toggle-maximize="toggleMaximize('results')"
                       shortcut="Alt+2" @toggle-collapse="toggleCollapse('results')" @hide="hide('results')"></results-panel>
      </div>
      <div class="ws-col" ref="right" v-show="rightCount > 0">
        <graph-panel ref="graphRef" v-show="isShown('graph')" :collapsed="layout.collapsed.graph" :maximized="maximized === 'graph'" data-panel="graph" @toggle-maximize="toggleMaximize('graph')"
                     shortcut="Alt+3" @toggle-collapse="toggleCollapse('graph')" @hide="hide('graph')"></graph-panel>
        <schema-panel ref="schemaRef" v-show="isShown('schema')" :active="isShown('schema') && !isCollapsed('schema')" :collapsed="layout.collapsed.schema" :maximized="maximized === 'schema'" data-panel="schema" @toggle-maximize="toggleMaximize('schema')"
                      shortcut="Alt+4" @toggle-collapse="toggleCollapse('schema')" @hide="hide('schema')"></schema-panel>
        <profile-panel ref="profileRef" v-show="isShown('profile')" :collapsed="layout.collapsed.profile" :maximized="maximized === 'profile'" data-panel="profile" @toggle-maximize="toggleMaximize('profile')"
                       shortcut="Alt+5" @toggle-collapse="toggleCollapse('profile')" @hide="hide('profile')"></profile-panel>
      </div>
    </div>
    <div v-if="leftCount === 0 && rightCount === 0" class="ws-empty">
      <p>All panels are hidden.</p>
      <button class="btn" @click="showAll">Show all panels</button>
    </div>
    <catalog-dialogs></catalog-dialogs>
    <optimize-panel></optimize-panel>
    <share-dialog ref="shareDialog"></share-dialog>
  </div>
  `,
};
