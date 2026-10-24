// The Statistics tab of the Profile panel: the graph's data counts (`g.statistics()`) and memory
// footprint, and beside them the limits a query runs under (the host's presets, the engine's
// defaults). Loaded when the tab becomes active; while active it reloads after a run that changed
// the data (`changes.data`, state.dataVersion) or a graph load. The rows come from the pure model
// in js/statistics.js.

import { ref, computed, watch } from 'vue';
import { state, isServerMode } from '../store.js';
import { statisticsModel, formatCount, counted } from '../statistics.js';

export const StatisticsView = {
  props: { active: { type: Boolean, default: false } },
  setup(props) {
    const open = computed(() => props.active);
    const raw = ref(null);
    const error = ref(null);
    const loading = ref(false);
    let loadedFor = -1; // the dataVersion `raw` belongs to

    async function load() {
      if (!state.engine || !state.graph || loading.value) return;
      const version = state.dataVersion;
      loading.value = true;
      try {
        raw.value = await state.engine.statistics();
        error.value = null;
        loadedFor = version;
      } catch (err) {
        error.value = err && err.message ? err.message : String(err);
      } finally {
        loading.value = false;
      }
      // Data changed while this request ran: load again.
      if (open.value && loadedFor !== state.dataVersion) load();
    }

    watch(open, (now) => {
      if (now && loadedFor !== state.dataVersion) load();
    }, { immediate: true });

    watch(() => state.dataVersion, () => {
      if (open.value) load();
    });

    const model = computed(() => statisticsModel(raw.value));
    const server = computed(() => isServerMode());
    return { error, loading, model, server, load, formatCount, counted };
  },
  template: `
  <div class="stats-view">
    <div class="stats-body">
      <div v-if="loading && !model.counts" class="placeholder"><span class="muted">loading…</span></div>
      <div v-if="error" class="notice" role="alert">{{ error }}</div>
      <template v-if="model.counts">
        <section class="stats-section">
          <h4>Data <span class="muted">(g.statistics())</span></h4>
          <p>{{ counted(model.counts.vertices, 'vertex', 'vertices') }}, {{ counted(model.counts.edges, 'edge', 'edges') }},
            {{ counted(model.counts.vertexLabelTotal, 'vertex label', 'vertex labels') }}, {{ counted(model.counts.edgeLabelTotal, 'edge label', 'edge labels') }}</p>
          <div class="stats-tables">
            <table class="data-table" v-if="model.counts.vertexLabels.length">
              <thead><tr><th>Vertex label</th><th title="Vertices carrying the label anywhere in their label set">Vertices</th>
                <th title="Vertices whose first (primary) label it is">Primary</th></tr></thead>
              <tbody><tr v-for="l in model.counts.vertexLabels" :key="l.label">
                <td>{{ l.label }}</td><td class="num">{{ formatCount(l.count) }}</td><td class="num">{{ formatCount(l.primary) }}</td></tr></tbody>
            </table>
            <table class="data-table" v-if="model.counts.edgeLabels.length">
              <thead><tr><th>Edge label</th><th>Edges</th></tr></thead>
              <tbody><tr v-for="l in model.counts.edgeLabels" :key="l.label">
                <td>{{ l.label }}</td><td class="num">{{ formatCount(l.count) }}</td></tr></tbody>
            </table>
          </div>
          <p v-if="model.counts.truncated" class="muted">The largest labels are listed; the totals count all of them.</p>
        </section>
      </template>
      <section v-if="model.memory" class="stats-section">
        <h4>Memory <span class="muted">(g.memory_usage(), an estimate of the graph's own footprint)</span></h4>
        <p><span v-for="m in model.memory" :key="m.name" class="stats-pair" :title="m.hint || null">{{ m.name }} <b>{{ m.text }}</b></span></p>
      </section>
      <section v-if="model.process" class="stats-section">
        <h4>Process memory <span class="muted">(what the whole process holds: the operating system's resident set size, the live heap, loaded models and the AI Chat history; not part of the graph)</span></h4>
        <p><span v-for="m in model.process" :key="m.name" class="stats-pair">{{ m.name }} <b>{{ m.text }}</b></span></p>
      </section>
      <section v-if="model.limits.length" class="stats-section">
        <h4>Limits of a query</h4>
        <p v-if="model.noPresets" class="muted">{{ server
          ? 'The dev server applies no presets (start it with --timeout, --memory-limit, --max-* to bound queries); the engine defaults apply.'
          : 'The playground applies no presets; the engine defaults apply. A query can change them with g.with(..).' }}</p>
        <table class="data-table">
          <thead><tr><th>Option</th><th>Value</th><th>Source</th></tr></thead>
          <tbody><tr v-for="l in model.limits" :key="l.source + l.key">
            <td><code>{{ l.key }}</code></td><td class="num">{{ l.text }}</td>
            <td class="muted">{{ l.source === 'default' ? 'engine default' : l.source === 'script' ? 'preset (script)' : 'preset' }}</td></tr></tbody>
        </table>
      </section>
    </div>
  </div>
  `,
};
