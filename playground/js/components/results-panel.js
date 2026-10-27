// Results panel: Table / JSON / Raw views of the active query tab's last result, the truncation notice,
// a "possibly stale" notice (another tab changed the graph after it ran), and errors. Graph statistics
// and query limits are the Statistics tab of the Profile panel (statistics-view.js).

import { ref, computed, watch } from 'vue';
import { state, reveal, run, showNeighbourhood, isServerMode } from '../store.js';
import { formatMs, toMarkdownTable } from '../util.js';
import { PanelFrame, PanelTabs, CopyButton } from './panel-frame.js';
import { DataTable } from './data-table.js';
import { ErrorView } from './error-view.js';
import { selectionText } from '../editor/run-selection.js';

const TABS = [
  { id: 'table', label: 'Table' },
  { id: 'json', label: 'JSON' },
  { id: 'raw', label: 'Raw' },
];

export const ResultsPanel = {
  components: { PanelFrame, PanelTabs, DataTable, ErrorView, CopyButton },
  setup() {
    const tab = ref('table');
    const result = computed(() => state.tab.result);

    watch(() => state.focusPanel, (req) => {
      if (req && req.panel === 'results' && req.tab && TABS.some((t) => t.id === req.tab)) tab.value = req.tab;
    });

    const meta = computed(() => {
      const r = result.value;
      if (!r) return '';
      if (!r.ok) return r.selection ? `ran the ${selectionText(r.selection.lines)}` : '';
      const parts = [`run #${r.run_id}`];
      const total = r.display ? r.display.total_rows : undefined;
      if (total !== undefined && total !== null) parts.push(`${total} result${total === 1 ? '' : 's'}`);
      parts.push(formatMs(r.elapsed_ms));
      if (r.selection) parts.push(`ran the ${selectionText(r.selection.lines)}`);
      if (r.mutated) {
        const summary = r.change_summary && r.change_summary.text;
        parts.push(summary ? `changed the graph (${summary})` : 'changed the graph');
      }
      return parts.join(' · ');
    });

    // Copy what the active tab shows (already cut to the display limits). The truncation notice is
    // appended to the Markdown table and the raw text; JSON stays valid JSON.
    const COPY_LABELS = { table: 'Copy as Markdown table', json: 'Copy as JSON', raw: 'Copy as raw text' };
    const copyLabel = computed(() => COPY_LABELS[tab.value]);
    function copyTextOfTab() {
      const r = result.value;
      if (!r || !r.ok || !r.display) return '';
      const d = r.display;
      if (tab.value === 'json') return d.json;
      if (tab.value === 'raw') return d.notice ? `${d.raw}\n${d.notice}` : d.raw;
      return toMarkdownTable(d.table, d.notice);
    }

    /**
     * Header refresh: run the last query again. Asks first when the last run changed the graph
     * (`mutated`).
     */
    function refresh() {
      const t = state.tab;
      if (!t.lastScript) return;
      if (t.result && t.result.mutated &&
          !window.confirm('The last query of this tab changed the graph. Run it again?')) return;
      run(t.lastScript, t.id, { selection: t.result && t.result.selection });
    }

    /** Vertex rows (an `id` and a `label` column, no edge `outV`): draw the vertex's neighbourhood. */
    const vertexAction = {
      label: '⌖',
      title: 'Draw this vertex and its neighbours in the Graph panel',
      applies(row, columns) {
        const id = columns.indexOf('id');
        if (id < 0 || !columns.includes('label') || columns.includes('outV')) return false;
        return typeof row[id] === 'string' || typeof row[id] === 'number';
      },
      run(row, columns) {
        showNeighbourhood(row[columns.indexOf('id')]);
      },
    };

    return { state, tab, TABS, result, meta, copyLabel, copyTextOfTab, refresh, vertexAction, showProfile: () => reveal('profile'), serverMode: isServerMode };
  },
  template: `
  <panel-frame title="Results" refresh-title="Run the last query again" @refresh="refresh">
    <template #tabs><panel-tabs :tabs="TABS" v-model="tab"></panel-tabs></template>
    <template #actions>
      <span class="result-meta secondary">{{ meta }}</span>
      <copy-button :get-text="copyTextOfTab" :label="copyLabel" :disabled="!result || !result.ok || !result.display"></copy-button>
    </template>
    <div class="fill">
      <template v-if="!result">
        <div class="placeholder">
          <span>Run a query to see its results here.</span>
          <span>Press <kbd>Ctrl</kbd>/<kbd>⌘</kbd> + <kbd>Enter</kbd> in the editor<template v-if="!serverMode()">, or pick one from <b>Examples</b></template>.</span>
        </div>
      </template>
      <template v-else-if="!result.ok">
        <div style="flex:1;min-height:0;overflow:auto">
          <error-view :error="result.execution.error"></error-view>
          <p v-if="result.execution.profile" class="muted" style="margin:0 10px 10px">
            The partial profile up to the failing step is in the <a href="#" @click.prevent="showProfile">Profile panel</a>.
          </p>
        </div>
      </template>
      <template v-else>
        <div v-if="state.tab.stale" class="notice" role="status">
          Possibly stale: another tab changed the graph after this result.
          <a href="#" @click.prevent="refresh">Run this tab's query again</a>
        </div>
        <div v-if="result.display.notice" class="notice" role="status">{{ result.display.notice }}</div>
        <div v-if="result.terminal === 'profile'" class="notice info">
          This query ended in <code>.profile()</code>: the metrics are in the
          <a href="#" @click.prevent="showProfile">Profile panel</a>.
        </div>
        <div style="flex:1;min-height:0;overflow:auto">
          <data-table v-if="tab === 'table'" :table="result.display.table" :row-action="vertexAction"></data-table>
          <pre v-else-if="tab === 'json'" class="code-view">{{ result.display.json }}</pre>
          <pre v-else class="code-view">{{ result.display.raw }}</pre>
        </div>
      </template>
    </div>
  </panel-frame>
  `,
};
