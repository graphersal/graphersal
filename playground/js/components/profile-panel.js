// Profile panel: the TraversalMetrics of the active query tab's last run that returned one (an
// implicit execute() with the Profile toggle, `execute(#{profile: true})`, or TinkerPop's
// `profile()`), as a step
// tree with timings, traverser counts, share of time, the optional Memory section and the
// optimizer rules that fired. A failed run marks its failing row by joining
// `execution.error.step_id` with `Metrics.id`, and the ancestors along `error.step_path`.
//
// Known sections only: unknown keys of Metrics are ignored, and an optional section (`timing`,
// `loops`, `path_recording`, `memory`, future ones) is rendered only when present (see the
// extensibility rule in engine/engine.js): `timing` as the Time cell's min/avg/max tooltip,
// `loops` as a line under a `repeat()` step, `path_recording` as the ` [path: ..]` suffix of the
// step name (as in the CLI's table).

import { ref, computed, watch } from 'vue';
import { state, run, openScript, reveal } from '../store.js';
import { formatDuration, formatBytes, toMarkdownTable } from '../util.js';
import { PanelFrame, PanelTabs, CopyButton } from './panel-frame.js';
import { StatisticsView } from './statistics-view.js';
import { profileText } from '../profile-text.js';
import { queryPeakModel } from '../memory-bar.js';

const TABS = [
  { id: 'tree', label: 'Steps' },
  { id: 'text', label: 'Text' },
  { id: 'stats', label: 'Statistics' },
];

/** The Time cell's tooltip: min/avg/max per call when the step has a `timing` section. */
function timingTitle(m) {
  if (!m.timing) return '';
  const avg = m.calls ? m.duration_ns / m.calls : m.duration_ns;
  return `per call: min ${formatDuration(m.timing.min_duration_ns)} / avg ${formatDuration(avg)} / max ${formatDuration(m.timing.max_duration_ns)}`;
}

/** The `loops` section of a repeat() step as one line. */
function loopsText(m) {
  if (!m.loops) return '';
  const avg = Number(m.loops.avg_loops_per_traverser);
  return `loops: ${m.loops.total_loops} total, max depth ${m.loops.max_depth}, ${Number.isFinite(avg) ? avg.toFixed(2) : avg} per traverser`;
}

const MEMORY_SOURCE_TEXT = {
  estimated: 'Memory figures are estimates (size_of/capacity based); retained bytes and allocation counts need tracking.',
  tracked: 'Memory figures are exact allocator measurements.',
};

function anyMemory(list) {
  return list.some((m) => m.memory || anyMemory(m.nested || []));
}

function findMetric(list, id) {
  for (const m of list) {
    if (m.id === id) return m;
    const inner = findMetric(m.nested || [], id);
    if (inner) return inner;
  }
  return null;
}

export const ProfilePanel = {
  components: { PanelFrame, PanelTabs, CopyButton, StatisticsView },
  setup() {
    const tab = ref('tree');
    const collapsed = ref(new Set());

    // A new profile starts fully expanded, so a failing row is visible along its path.
    watch(() => state.tab.profile, () => { collapsed.value = new Set(); });
    watch(() => state.focusPanel, (req) => {
      if (req && req.panel === 'profile' && req.tab && TABS.some((t) => t.id === req.tab)) tab.value = req.tab;
    });

    const entry = computed(() => state.tab.profile); // {metrics, run, script, at, error, terminal}
    const profile = computed(() => (entry.value ? entry.value.metrics : null));
    const memory = computed(() => !!profile.value && anyMemory(profile.value.metrics));
    const memorySource = computed(() => {
      if (!memory.value) return null;
      const find = (list) => {
        for (const m of list) {
          if (m.memory) return m.memory.source;
          const s = find(m.nested || []);
          if (s) return s;
        }
        return null;
      };
      return find(profile.value.metrics);
    });
    const showRetained = computed(() => {
      const has = (list) => list.some((m) => (m.memory && m.memory.bytes_retained !== null && m.memory.bytes_retained !== undefined) || has(m.nested || []));
      return memory.value && has(profile.value.metrics);
    });
    const failedId = computed(() => (entry.value && entry.value.error && entry.value.error.step_id !== undefined
      ? entry.value.error.step_id : null));
    const failedPath = computed(() => new Set(entry.value && entry.value.error && Array.isArray(entry.value.error.step_path)
      ? entry.value.error.step_path.map((s) => s.id).filter((id) => id !== failedId.value) : []));
    const failedStep = computed(() => (profile.value && failedId.value !== null
      ? findMetric(profile.value.metrics, failedId.value) : null));
    const isLatest = computed(() => entry.value && state.tab.result && state.tab.result.run_id === entry.value.run);

    // Flatten the tree into rows with depth, honouring collapsed nodes.
    const rows = computed(() => {
      const out = [];
      const walk = (list, depth) => {
        list.forEach((m) => {
          const nested = m.nested || [];
          out.push({ m, depth, key: m.id, hasChildren: nested.length > 0 });
          // Parents on the failure path never hide the failing row.
          if (nested.length && (!collapsed.value.has(m.id) || failedPath.value.has(m.id))) walk(nested, depth + 1);
        });
      };
      if (profile.value) walk(profile.value.metrics, 0);
      return out;
    });

    function toggle(key) {
      const s = new Set(collapsed.value);
      if (s.has(key)) s.delete(key);
      else s.add(key);
      collapsed.value = s;
    }

    function tryExample() {
      // Same rule as the Examples menu: never overwrites the user's text (a new tab instead).
      const t = openScript({ title: 'Profile example', script: 'g.v().has_label("person").out("created").count().profile()' });
      if (t) run(t.script, t.id);
    }

    const stats = ref(null); // the Statistics tab's component instance
    /** Header refresh: on the Statistics tab reload the statistics, else run the profiled query again. */
    function refresh() {
      if (tab.value === 'stats') {
        if (stats.value) stats.value.load();
        return;
      }
      const t = state.tab;
      if (t.profileScript) run(t.profileScript, t.id, { selection: t.profile && t.profile.selection });
    }

    // The run's peak against the per-query limit (`query_memory`), never the process ceiling.
    const peak = computed(() => queryPeakModel(profile.value));

    const text = computed(() => {
      const p = profile.value;
      if (!p) return '';
      return profileText(p, entry.value.error);
    });

    /** Steps tab: the shown rows as a Markdown table (nesting as a "↳" prefix), rules as a note. */
    function copyProfile() {
      const p = profile.value;
      if (!p) return '';
      if (tab.value === 'text') return text.value;
      const columns = ['Step', 'Calls', 'In', 'Out', 'Time', ...(memory.value ? ['Mem'] : []), ...(showRetained.value ? ['Retained', 'Allocs'] : []), '% time'];
      const table = {
        columns,
        rows: rows.value.map((r) => [
          (r.depth ? '↳ '.repeat(r.depth) : '') + r.m.name + (r.m.path_recording ? ` [path: ${r.m.path_recording}]` : '') + (r.m.id === failedId.value ? ' (failed here)' : ''),
          r.m.calls, r.m.count_in, r.m.counts.traverser_count, formatDuration(r.m.duration_ns),
          ...(memory.value ? [r.m.memory ? formatBytes(r.m.memory.bytes_materialized) : ''] : []),
          ...(showRetained.value ? [r.m.memory ? formatBytes(r.m.memory.bytes_retained) : '', r.m.memory ? r.m.memory.alloc_count : ''] : []),
          r.m.percent_duration.toFixed(2),
        ]),
      };
      const rules = p.optimizer_rules_applied.length ? `Optimizer rules applied: ${p.optimizer_rules_applied.join(', ')}` : '';
      return toMarkdownTable(table, rules);
    }

    function oneLine(s) {
      return (s || '').replace(/\s+/g, ' ');
    }

    return {
      state, TABS, tab, entry, profile, peak, memory, memorySource, showRetained, failedStep, failedId, failedPath, isLatest, rows, text,
      toggle, collapsed, tryExample, refresh, copyProfile, oneLine, stats,
      refreshTitle: computed(() => (tab.value === 'stats' ? 'Reload the statistics' : 'Run the profiled query again')),
      copyLabel: computed(() => (tab.value === 'text' ? 'Copy as text' : 'Copy as Markdown table')),
      formatDuration, formatBytes, MEMORY_SOURCE_TEXT, timingTitle, loopsText,
      showResults: () => reveal('results'),
    };
  },
  template: `
  <panel-frame title="Profile" :refresh-title="refreshTitle" @refresh="refresh">
    <template #tabs><panel-tabs :tabs="TABS" v-model="tab"></panel-tabs></template>
    <template #actions>
      <copy-button v-if="profile && tab !== 'stats'" :get-text="copyProfile" :label="copyLabel"></copy-button>
    </template>
    <div class="fill">
      <statistics-view v-if="tab === 'stats'" ref="stats" :active="tab === 'stats'"></statistics-view>
      <div v-else-if="!profile" class="placeholder">
        <span>With <b>Profile</b> on in the Query toolbar, every run of a traversal collects its optimized plan,
        per-step timings and traverser counts here; <b>Memory</b> adds the memory section. Explicit
        <code>.profile()</code> and <code>.execute(#{profile: true})</code> work too.</span>
        <button class="btn small" @click="tryExample">Run an example</button>
      </div>
      <template v-else>
        <div class="profile-run" :class="{ stale: !isLatest }">
          <span class="badge" :class="isLatest ? 'accent' : ''">run #{{ entry.run }}</span>
          <span class="mono profile-script" :title="entry.script">{{ oneLine(entry.script) }}</span>
          <span v-if="!isLatest" class="faint">(a later run returned no metrics)</span>
          <span v-if="state.tab.stale" class="badge warn" title="Another tab changed the graph after this run">possibly stale</span>
        </div>
        <div v-if="failedStep" class="notice" role="alert" style="background:var(--danger-soft);color:var(--danger)">
          Failed at step {{ failedId }} <code>{{ failedStep.name }}</code><template v-if="entry.error">: {{ entry.error.message }}</template>.
          The profile is partial. <a href="#" @click.prevent="showResults">See the error</a>
        </div>
        <template v-if="tab === 'tree'">
          <div style="flex:1;min-height:0;overflow:auto">
            <table class="data-table profile-table">
              <thead>
                <tr>
                  <th>Step</th><th>Calls</th><th>In</th><th>Out</th><th>Time</th>
                  <th v-if="memory">Mem</th>
                  <th v-if="showRetained">Retained</th><th v-if="showRetained">Allocs</th>
                  <th>% time</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="r in rows" :key="r.key" :class="{ failed: r.m.id === failedId, 'failed-path': failedPath.has(r.m.id) }">
                  <td class="step" :title="r.m.name">
                    <span class="indent" :style="{ width: (r.depth * 16) + 'px' }"></span>
                    <span class="twisty" v-if="r.hasChildren" @click="toggle(r.key)">{{ collapsed.has(r.key) ? '▸' : '▾' }}</span>
                    <span class="twisty" v-else></span>{{ r.m.name }}<span v-if="r.m.path_recording" class="faint mono" title="What this step records into traverser paths"> [path: {{ r.m.path_recording }}]</span>
                    <div v-if="r.m.loops" class="faint loops-line" :style="{ paddingLeft: (r.depth * 16 + 16) + 'px' }">{{ loopsText(r.m) }}</div>
                    <span v-if="r.m.id === failedId" class="badge danger" style="margin-left:6px">failed here</span>
                    <span v-else-if="failedPath.has(r.m.id)" class="faint" style="margin-left:6px" title="The failing step is inside this step">▸ contains the failure</span>
                  </td>
                  <td class="num">{{ r.m.calls }}</td>
                  <td class="num">{{ r.m.count_in }}</td>
                  <td class="num" :title="r.m.counts.element_count !== r.m.counts.traverser_count ? r.m.counts.element_count + ' logical traversers (bulk)' : ''">{{ r.m.counts.traverser_count }}</td>
                  <td class="num" :title="timingTitle(r.m)">{{ formatDuration(r.m.duration_ns) }}</td>
                  <td v-if="memory" class="num">{{ r.m.memory ? formatBytes(r.m.memory.bytes_materialized) : '' }}</td>
                  <td v-if="showRetained" class="num">{{ r.m.memory ? formatBytes(r.m.memory.bytes_retained) : '' }}</td>
                  <td v-if="showRetained" class="num">{{ r.m.memory && r.m.memory.alloc_count !== null ? r.m.memory.alloc_count : '' }}</td>
                  <td class="pct-cell"><div class="pct-bar"><span :style="{ width: Math.min(100, r.m.percent_duration) + '%' }"></span><em>{{ r.m.percent_duration.toFixed(1) }}</em></div></td>
                </tr>
              </tbody>
            </table>
          </div>
          <div class="profile-totals">
            <span>total <b>{{ formatDuration(profile.duration_ns) }}</b></span>
            <span v-if="memory">memory <span class="badge" :class="memorySource === 'tracked' ? 'accent' : 'warn'">{{ memorySource }}</span></span>
            <span v-if="memory" class="faint">{{ MEMORY_SOURCE_TEXT[memorySource] || '' }}</span>
          </div>
          <div v-if="peak" :class="['profile-totals', 'profile-peak', 'mem-' + peak.level]"
            title="The most memory this run held at once, against the per-query limit traversal.max_materialized_bytes (not the process ceiling)"><b>{{ peak.text }}</b></div>
          <div class="rules">
            <b>Optimizer rules applied:</b>
            <template v-if="profile.optimizer_rules_applied.length">
              <span v-for="r in profile.optimizer_rules_applied" :key="r" class="badge accent mono">{{ r }}</span>
            </template>
            <span v-else class="muted">none (the plan was not rewritten)</span>
          </div>
        </template>
        <pre v-else class="code-view" style="flex:1;overflow:auto">{{ text }}</pre>
      </template>
    </div>
  </panel-frame>
  `,
};
