// MCP usage dialog (dev server with --mcp): how agents used the MCP endpoint since the server
// started, from GET /api/mcp/stats (kept in the server's memory only). Opened from the top bar's
// MCP badge; model js/mcp-stats.js.

import { ref, computed } from 'vue';
import { state } from '../store.js';
import { mcpStatsModel } from '../mcp-stats.js';

export const McpStatsDialog = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const stats = ref(null);
    const error = ref('');
    const loading = ref(false);
    const load = async () => {
      if (!state.engine || typeof state.engine.mcpStats !== 'function') return;
      loading.value = true;
      try {
        stats.value = await state.engine.mcpStats();
        error.value = '';
      } catch (err) {
        error.value = err && err.message ? err.message : String(err);
      } finally {
        loading.value = false;
      }
    };
    const open = () => {
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      load();
    };
    const close = () => { if (dlg.value) dlg.value.close(); };
    expose({ open, close });
    const model = computed(() => mcpStatsModel(stats.value));
    return { dlg, close, load, loading, error, model };
  },
  template: `
  <dialog ref="dlg" class="mcp-stats-dialog" aria-labelledby="mcp-stats-title" @click.self="close">
    <div class="dialog-head">
      <h2 id="mcp-stats-title">MCP usage</h2><span class="spacer"></span>
      <button class="btn" @click="load" :disabled="loading">{{ loading ? 'Loading…' : 'Refresh' }}</button>
      <button class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <p class="muted">How agents used this server's MCP endpoint since it started. Kept in the server's memory
      only: nothing is written, a restart starts from zero.</p>
      <p v-if="error" role="alert" style="color: var(--danger)">{{ error }}</p>
      <template v-if="model">
        <table class="data-table catalog-table">
          <tr v-for="r in model.summary" :key="r.name"><th scope="row">{{ r.name }}</th><td>{{ r.text }}</td></tr>
          <tr v-if="model.refusals.length"><th scope="row">Refused</th>
            <td>{{ model.refusals.map((r) => r.name + ' ' + r.count).join(' · ') }}</td></tr>
          <tr v-if="model.methods.length"><th scope="row">Methods</th>
            <td>{{ model.methods.map((r) => r.name + ' ' + r.count).join(' · ') }}</td></tr>
        </table>
        <template v-for="section in [['Tools', model.tools], ['Saved queries', model.savedQueries]]" :key="section[0]">
          <h3>{{ section[0] }}</h3>
          <p v-if="!section[1].length" class="muted">None called yet.</p>
          <table v-else class="data-table catalog-table">
            <thead><tr><th>Name</th><th>Calls</th><th>Errors</th><th>Mean</th><th>Max</th><th>Answer chars</th><th>Latency</th></tr></thead>
            <tbody>
              <tr v-for="r in section[1]" :key="r.name">
                <td>{{ r.name }}</td><td>{{ r.calls }}</td><td>{{ r.errors }}</td><td>{{ r.mean }}</td>
                <td>{{ r.max }}</td><td>{{ r.chars }}</td><td>{{ r.histogram }}</td>
              </tr>
            </tbody>
          </table>
        </template>
        <h3>Clients</h3>
        <p v-if="!model.clients.length" class="muted">No client initialized yet.</p>
        <table v-else class="data-table catalog-table">
          <thead><tr><th>Client</th><th>Initialized</th><th>Requests</th><th>Tool calls</th></tr></thead>
          <tbody>
            <tr v-for="c in model.clients" :key="c.name">
              <td>{{ c.name }}</td><td>{{ c.initialized }}</td><td>{{ c.requests }}</td><td>{{ c.toolCalls }}</td>
            </tr>
          </tbody>
        </table>
      </template>
    </div>
  </dialog>`,
};
