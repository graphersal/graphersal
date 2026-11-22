// The landing view of a multi-database server (shown at its root when `/api/info` announces
// `admin: true` without `mountPath`, js/server-mode.js `isLanding`): the list of its databases
// from `GET /api/admin/databases` with their states and a link to each one's playground
// (`db/<name>/`). No admin actions here yet. The list reloads while a database is loading.

import { ref, onMounted, onBeforeUnmount } from 'vue';
import { state } from '../store.js';
import { fetchDatabases, anyLoading, LANDING_POLL_MS } from '../server-mode.js';

/** The server root: the directory of index.html. */
const ROOT = new URL('../../', import.meta.url);

export const LandingPage = {
  setup() {
    const rows = ref([]);
    const skipped = ref(0);
    const error = ref('');
    const loading = ref(false);
    const loaded = ref(false);
    let timer = null;
    let alive = true;

    async function load() {
      if (loading.value) return;
      loading.value = true;
      try {
        const answer = await fetchDatabases({
          base: ROOT,
          fetch: globalThis.fetch.bind(globalThis),
          headers: (state.landing && state.landing.headers) || {},
        });
        rows.value = answer.rows;
        skipped.value = answer.skipped;
        error.value = '';
      } catch (err) {
        error.value = err && err.message ? err.message : String(err);
      } finally {
        loading.value = false;
        loaded.value = true;
      }
      clearTimeout(timer);
      if (alive && anyLoading(rows.value)) timer = setTimeout(load, LANDING_POLL_MS);
    }

    onMounted(load);
    onBeforeUnmount(() => {
      alive = false;
      clearTimeout(timer);
    });

    const info = (state.landing && state.landing.info) || {};
    return {
      rows, skipped, error, loading, loaded, load,
      version: typeof info.version === 'string' ? info.version : '',
      href: (row) => new URL(row.href, ROOT).href,
    };
  },
  template: `
  <div class="start landing">
    <header class="topbar">
      <span class="brand">Graphersal <small class="hide-narrow">Server</small></span>
      <span v-if="version" class="version-label hide-narrow">v{{ version }}</span>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="load" :disabled="loading">{{ loading ? 'Loading…' : 'Refresh' }}</button>
    </header>
    <div class="start-inner">
      <h1>Databases</h1>
      <p v-if="error" class="notice" role="alert">The database list could not be read: {{ error }}</p>
      <p v-else-if="loaded && !rows.length" class="muted">This server has no databases yet.</p>
      <table v-if="rows.length" class="data-table landing-table">
        <thead><tr><th>Name</th><th>State</th><th>Kind</th><th>Description</th></tr></thead>
        <tbody>
          <tr v-for="row in rows" :key="row.name" :data-db="row.name">
            <td><a :href="href(row)" class="mono">{{ row.name }}</a>
              <span v-if="row.aliasOf" class="muted"> → {{ row.aliasOf }}</span>
              <span v-if="row.readOnly" class="badge warn" title="Every write to this database is refused">read-only</span></td>
            <td><span class="badge" :class="row.tone" :title="row.reason || row.stateText">{{ row.stateText }}</span>
              <span v-if="row.reason" class="muted"> {{ row.reason }}</span></td>
            <td>{{ row.kind }}</td>
            <td>{{ row.description }}</td>
          </tr>
        </tbody>
      </table>
      <p v-if="skipped" class="muted">{{ skipped }} entr{{ skipped === 1 ? 'y' : 'ies' }} of the server's answer could not be read.</p>
    </div>
  </div>
  `,
};
