// Run administration dialog (a server that turned it on: graphersal --server --admin-runs): every
// queued or running run of the served graph, whatever its client (pages, MCP agents, the AI
// Chat), from GET /api/admin/runs, with a Kill button per run (POST /api/admin/runs/cancel).
// Opened from the top bar's Runs badge, shown only when /api/info names `adminRuns`; reloads
// every ADMIN_RUNS_REFRESH_MS while open. The token (when the server needs one) stays in this
// page's memory only. Model js/admin-runs.js. Issue #32.

import { ref, computed, onBeforeUnmount } from 'vue';
import { state } from '../store.js';
import { adminRunsModel, ADMIN_RUNS_REFRESH_MS } from '../admin-runs.js';

export const AdminRunsDialog = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const answer = ref(null);
    const error = ref('');
    const loading = ref(false);
    const token = ref('');
    const killing = ref(null);
    let timer = null;
    const tokenRequired = computed(() => !!(state.engineInfo.adminRuns && state.engineInfo.adminRuns.tokenRequired));
    const needsToken = computed(() => tokenRequired.value && !token.value.trim());
    const load = async () => {
      if (!state.engine || typeof state.engine.adminRuns !== 'function' || needsToken.value) return;
      loading.value = true;
      try {
        answer.value = await state.engine.adminRuns(token.value);
        error.value = '';
      } catch (err) {
        error.value = err && err.message ? err.message : String(err);
        if (err && err.status === 403) answer.value = null;
      } finally {
        loading.value = false;
      }
    };
    const stop = () => { if (timer) { clearInterval(timer); timer = null; } };
    const open = () => {
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      load();
      stop();
      timer = setInterval(() => { if (dlg.value && dlg.value.open && !loading.value) load(); }, ADMIN_RUNS_REFRESH_MS);
    };
    const close = () => { stop(); if (dlg.value) dlg.value.close(); };
    const kill = async (row) => {
      if (!window.confirm(`Kill run ${row.id} of ${row.client}? Its query stops and its changes roll back.`)) return;
      killing.value = row.id;
      try {
        const done = await state.engine.adminKillRun(row.id, token.value);
        if (!done || !done.cancelled) error.value = `Run ${row.id} has already ended.`;
        await load();
      } catch (err) {
        error.value = err && err.message ? err.message : String(err);
      } finally {
        killing.value = null;
      }
    };
    onBeforeUnmount(stop);
    expose({ open, close });
    const model = computed(() => adminRunsModel(answer.value, state.engine && state.engine.clientId));
    return { dlg, close, load, loading, error, model, token, tokenRequired, needsToken, kill, killing };
  },
  template: `
  <dialog ref="dlg" class="admin-runs-dialog" aria-labelledby="admin-runs-title" @click.self="close" @close="close">
    <div class="dialog-head">
      <h2 id="admin-runs-title">Running queries</h2><span class="spacer"></span>
      <button class="btn" @click="load" :disabled="loading || needsToken">{{ loading ? 'Loading…' : 'Refresh' }}</button>
      <button class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <p class="muted">Run administration: every query of this server that waits or runs, from every client
      (browser pages, MCP agents, the AI Chat). Kill stops one; its changes roll back.</p>
      <label v-if="tokenRequired" class="admin-runs-token">Admin token
        <input type="password" v-model="token" autocomplete="off" @keydown.enter="load" placeholder="--admin-runs-token">
      </label>
      <p v-if="error" role="alert" style="color: var(--danger)">{{ error }}</p>
      <template v-if="model">
        <p v-if="!model.rows.length" class="muted">Nothing runs.</p>
        <table v-else class="data-table catalog-table">
          <thead><tr><th>Id</th><th>Client</th><th>Kind</th><th>State</th><th>Started</th><th>Duration</th><th>Query</th><th></th></tr></thead>
          <tbody>
            <tr v-for="r in model.rows" :key="r.id">
              <td>{{ r.id }}</td>
              <td>{{ r.client }}<template v-if="r.own"> (this page)</template><div v-if="r.identity" class="muted">{{ r.identity }}</div></td>
              <td>{{ r.kind }}</td><td>{{ r.state }}</td><td>{{ r.started }}</td><td>{{ r.duration }}</td>
              <td class="admin-runs-text"><code class="mono">{{ r.text }}<template v-if="r.truncated">…</template></code></td>
              <td><button class="btn danger" @click="kill(r)" :disabled="r.cancelled || killing === r.id">Kill</button></td>
            </tr>
          </tbody>
        </table>
      </template>
    </div>
  </dialog>`,
};
