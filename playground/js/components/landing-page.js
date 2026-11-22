// The landing view of a multi-database server (shown at its root when `/api/info` announces
// `admin: true` without `mountPath`, js/server-mode.js `isLanding`) with its admin panel: the
// database list from `GET /api/admin/databases` (state, kind and role, read-only and auto-load
// toggles, size and memory when the server sends them, a link to each one's playground
// `db/<name>/`), the row actions (load with a poll until it ends, unload, clone, delete), create
// (plain, base, or a layer over a base; optional description and schema) and the aliases (set,
// flip, remove). Requests and rules: the pure model js/admin-api.js. Errors show the server's
// `error` and `help` verbatim. The list reloads while a database is loading.

import { ref, reactive, computed, nextTick, onMounted, onBeforeUnmount } from 'vue';
import { state } from '../store.js';
import { fetchDatabases, anyLoading, databaseRows, LANDING_POLL_MS } from '../server-mode.js';
import {
  createAdmin, createBody, nameProblem, rowActions, confirmText, outcomeText, loadAndWatch,
  baseNames, realRows, bytesText,
} from '../admin-api.js';

/** The server root: the directory of index.html. */
const ROOT = new URL('../../', import.meta.url);

const errorNotice = (err) => ({
  tone: 'error',
  text: err && err.message ? err.message : String(err),
  help: err && typeof err.help === 'string' ? err.help : '',
});

const emptyCreate = () => ({ name: '', kind: 'plain', role: 'normal', base: '', description: '', schema: '', readOnly: false, autoLoad: false });
const freshDialog = () => ({ mode: null, source: '', form: emptyCreate(), alias: '', target: '', existing: false, problems: [], error: null, busy: false });

export const LandingPage = {
  setup() {
    const rows = ref([]);
    const aliases = ref([]);
    const problems = ref([]);
    const skipped = ref(0);
    const error = ref(null);
    const loading = ref(false);
    const loaded = ref(false);
    const busy = reactive({});
    const notice = ref(null);
    const dlg = ref(null);
    const dialog = reactive(freshDialog());
    let timer = null;
    let alive = true;

    const headers = (state.landing && state.landing.headers) || {};
    const fetchFn = globalThis.fetch.bind(globalThis);
    const admin = createAdmin({ base: ROOT, fetch: fetchFn, headers });

    async function load() {
      if (loading.value) return;
      loading.value = true;
      try {
        const answer = await fetchDatabases({ base: ROOT, fetch: fetchFn, headers });
        rows.value = answer.rows;
        aliases.value = answer.aliases;
        problems.value = answer.problems;
        skipped.value = answer.skipped;
        error.value = null;
      } catch (err) {
        error.value = errorNotice(err);
      } finally {
        loading.value = false;
        loaded.value = true;
      }
      clearTimeout(timer);
      if (alive && anyLoading(rows.value)) timer = setTimeout(load, LANDING_POLL_MS);
    }

    /** Puts an entry answered by an action into the list at once (the reload follows). */
    function patchRow(entry) {
      if (!entry || typeof entry.name !== 'string') return;
      const [row] = databaseRows({ databases: [entry] }).rows;
      if (!row) return;
      rows.value = rows.value.map((r) => (r.name === row.name && !r.aliasOf ? row : r));
    }

    /** Runs one action of `subject`: busy marker, outcome or error notice, list reload. */
    async function run(subject, action, fn) {
      busy[subject] = action;
      notice.value = null;
      try {
        const answer = (await fn()) || {};
        const failed = action === 'load' && (answer.state === 'failed' || answer.state === 'needs-model');
        notice.value = { tone: failed ? 'error' : 'ok', text: outcomeText(action, subject, answer), help: '' };
        return true;
      } catch (err) {
        notice.value = errorNotice(err);
        return false;
      } finally {
        delete busy[subject];
        await load();
      }
    }

    const aliasTargets = computed(() => aliases.value.map((a) => a.target));
    const acts = (row) => rowActions(row, { busy: !!busy[row.name], aliasTargets: aliasTargets.value });
    const databaseNames = computed(() => realRows(rows.value).map((r) => r.name));
    const bases = computed(() => baseNames(rows.value));
    const showSize = computed(() => rows.value.some((r) => r.sizeBytes !== null));
    const showMemory = computed(() => rows.value.some((r) => r.memoryBytes !== null));

    const loadDb = (row) => run(row.name, 'load', () => loadAndWatch(admin, row.name, { onState: patchRow, alive: () => alive }));
    function unloadDb(row) {
      if (!window.confirm(confirmText('unload', row.name))) return;
      run(row.name, 'unload', () => admin.unload(row.name));
    }
    function deleteDb(row) {
      if (!window.confirm(confirmText('delete', row.name))) return;
      run(row.name, 'delete', () => admin.delete(row.name));
    }
    async function setFlag(row, flag, event) {
      const box = event.target;
      const value = box.checked;
      const ok = await run(row.name, flag, () => (flag === 'readOnly' ? admin.readOnly(row.name, value) : admin.autoLoad(row.name, value)));
      // A refusal keeps the server's value: the checkbox shows it again.
      if (!ok) box.checked = !value;
    }

    async function openDialog(mode, init = {}) {
      Object.assign(dialog, freshDialog(), { mode }, init);
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function closeDialog() {
      if (dialog.busy) return;
      dialog.mode = null;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    const openCreate = () => openDialog('create', { form: { ...emptyCreate(), base: bases.value[0] || '' } });
    const openClone = (row) => openDialog('clone', { source: row.name, form: { ...emptyCreate(), name: `${row.name}-copy` } });
    const openAlias = (alias) => openDialog('alias', alias
      ? { alias: alias.alias, target: alias.target, existing: true }
      : { target: databaseNames.value[0] || '' });

    /** The request of the open dialog, or `{problems}` (shown in the dialog, nothing sent). */
    function dialogRequest() {
      if (dialog.mode === 'create') {
        const built = createBody(dialog.form);
        if (built.problems) return { problems: built.problems };
        return { subject: built.body.name, action: 'create', send: () => admin.create(built.body) };
      }
      if (dialog.mode === 'clone') {
        const problem = nameProblem(dialog.form.name, 'name for the clone');
        if (problem) return { problems: [problem] };
        const subject = dialog.form.name.trim().toLowerCase();
        const source = dialog.source;
        return { subject, action: 'clone', send: () => admin.clone(source, subject) };
      }
      if (dialog.mode === 'alias') {
        const problem = nameProblem(dialog.alias, 'alias name') || (dialog.target ? null : 'Choose the database the alias names.');
        if (problem) return { problems: [problem] };
        const subject = dialog.alias.trim().toLowerCase();
        const target = dialog.target;
        const was = aliases.value.find((a) => a.alias === subject);
        if (was && was.target !== target && !window.confirm(confirmText('flipAlias', subject, target))) return { cancelled: true };
        return { subject, action: 'setAlias', send: () => admin.setAlias(subject, target) };
      }
      return { cancelled: true };
    }

    async function submitDialog() {
      if (dialog.busy) return;
      const req = dialogRequest();
      dialog.problems = req.problems || [];
      if (!req.send) return;
      dialog.busy = true;
      dialog.error = null;
      notice.value = null;
      try {
        const answer = (await req.send()) || {};
        notice.value = { tone: 'ok', text: outcomeText(req.action, req.subject, answer), help: '' };
      } catch (err) {
        // The dialog stays open with the server's words, so the input can be fixed.
        dialog.error = errorNotice(err);
        return;
      } finally {
        dialog.busy = false;
      }
      closeDialog();
      await load();
    }
    function removeAlias(a) {
      if (!window.confirm(confirmText('removeAlias', a.alias))) return;
      run(a.alias, 'removeAlias', () => admin.removeAlias(a.alias));
    }

    onMounted(load);
    onBeforeUnmount(() => {
      alive = false;
      clearTimeout(timer);
    });

    const info = (state.landing && state.landing.info) || {};
    return {
      rows, aliases, problems, skipped, error, loading, loaded, load, busy, notice, dlg, dialog,
      acts, bases, databaseNames, showSize, showMemory, bytesText,
      loadDb, unloadDb, deleteDb, setFlag, openCreate, openClone, openAlias, closeDialog, submitDialog, removeAlias,
      version: typeof info.version === 'string' ? info.version : '',
      href: (row) => new URL(row.href, ROOT).href,
      kindText: (row) => [row.kind, row.role === 'base' ? 'base' : ''].filter(Boolean).join(' · '),
      dialogTitle: computed(() => ({
        create: 'New database',
        clone: `Clone "${dialog.source}"`,
        alias: dialog.existing ? `Alias "${dialog.alias}"` : 'New alias',
      })[dialog.mode] || ''),
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
      <div class="landing-head">
        <h1>Databases</h1>
        <span class="spacer"></span>
        <button class="btn primary" type="button" @click="openCreate" :disabled="!loaded || !!error">New database…</button>
      </div>
      <div v-if="notice" class="notice admin-notice" :class="notice.tone === 'ok' ? 'info' : 'admin-error'" :role="notice.tone === 'ok' ? 'status' : 'alert'">
        <div class="admin-notice-text"><span>{{ notice.text }}</span>
          <p v-if="notice.help" class="admin-help">{{ notice.help }}</p></div>
        <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
      </div>
      <div v-if="error" class="notice admin-error" role="alert">
        <div class="admin-notice-text"><span>The database list could not be read: {{ error.text }}</span>
          <p v-if="error.help" class="admin-help">{{ error.help }}</p></div>
      </div>
      <p v-else-if="loaded && !rows.length" class="muted">This server has no databases yet.</p>
      <ul v-if="problems.length" class="admin-problems">
        <li v-for="p in problems" :key="p">{{ p }}</li>
      </ul>
      <div v-if="rows.length" class="landing-table-wrap">
      <table class="data-table landing-table">
        <thead><tr>
          <th>Name</th><th>State</th><th>Kind</th>
          <th title="Every write to the database is refused">Read-only</th>
          <th title="Loaded when the server starts">Auto-load</th>
          <th v-if="showSize">Size</th><th v-if="showMemory">Memory</th>
          <th><span class="sr-only">Actions</span></th>
        </tr></thead>
        <tbody>
          <tr v-for="row in rows" :key="(row.aliasOf ? '@' : '') + row.name" :data-db="row.name" :class="{ 'alias-row': row.aliasOf }">
            <td><a :href="href(row)" class="mono">{{ row.name }}</a>
              <span v-if="row.aliasOf" class="muted"> → {{ row.aliasOf }}</span>
              <div v-if="row.description" class="muted landing-sub">{{ row.description }}</div></td>
            <td><span class="badge" :class="row.tone" :title="row.reason || row.stateText">{{ busy[row.name] === 'unload' ? 'unloading…' : row.stateText }}</span>
              <div v-if="row.reason" class="muted landing-sub">{{ row.reason }}</div>
              <div v-if="row.model" class="muted landing-sub mono">{{ row.model }}</div></td>
            <td>{{ kindText(row) }}<div v-if="row.base" class="muted landing-sub">over {{ row.base }}</div></td>
            <td><input type="checkbox" :checked="row.readOnly" :disabled="!acts(row).readOnly.enabled" :title="acts(row).readOnly.title"
                       :aria-label="'Read-only: ' + row.name" @change="setFlag(row, 'readOnly', $event)"></td>
            <td><input type="checkbox" :checked="row.autoLoad" :disabled="!acts(row).autoLoad.enabled" :title="acts(row).autoLoad.title"
                       :aria-label="'Auto-load: ' + row.name" @change="setFlag(row, 'autoLoad', $event)"></td>
            <td v-if="showSize" class="num">{{ bytesText(row.sizeBytes) }}</td>
            <td v-if="showMemory" class="num">{{ row.state === 'loaded' ? bytesText(row.memoryBytes) : '' }}</td>
            <td class="admin-actions">
              <template v-if="!row.aliasOf">
                <button v-if="row.state === 'loaded'" type="button" class="btn small" :disabled="!acts(row).unload.enabled" :title="acts(row).unload.title" @click="unloadDb(row)">Unload…</button>
                <button v-else type="button" class="btn small" :disabled="!acts(row).load.enabled" :title="acts(row).load.title" @click="loadDb(row)">{{ busy[row.name] === 'load' || row.state === 'loading' ? 'Loading…' : 'Load' }}</button>
                <button type="button" class="btn small" :disabled="!acts(row).clone.enabled" :title="acts(row).clone.title" @click="openClone(row)">Clone…</button>
                <button type="button" class="btn small danger" :disabled="!acts(row).delete.enabled" :title="acts(row).delete.title" @click="deleteDb(row)">Delete…</button>
              </template>
            </td>
          </tr>
        </tbody>
      </table>
      </div>
      <p v-if="skipped" class="muted">{{ skipped }} entr{{ skipped === 1 ? 'y' : 'ies' }} of the server's answer could not be read.</p>

      <div class="landing-head">
        <h2>Aliases</h2>
        <span class="spacer"></span>
        <button class="btn small" type="button" @click="openAlias(null)" :disabled="!databaseNames.length">New alias…</button>
      </div>
      <p v-if="!aliases.length" class="muted">No aliases. An alias is a second name for a database (its pages are served under db/&lt;alias&gt;/ too); point it to another database to switch every link at once.</p>
      <div v-else class="landing-table-wrap">
      <table class="data-table landing-table aliases-table">
        <thead><tr><th>Alias</th><th>Database</th><th><span class="sr-only">Actions</span></th></tr></thead>
        <tbody>
          <tr v-for="a in aliases" :key="a.alias" :data-alias="a.alias">
            <td class="mono">{{ a.alias }}</td>
            <td class="mono">{{ a.target }}</td>
            <td class="admin-actions">
              <button type="button" class="btn small" :disabled="!!busy[a.alias]" @click="openAlias(a)">Point to…</button>
              <button type="button" class="btn small danger" :disabled="!!busy[a.alias]" @click="removeAlias(a)">Remove…</button>
            </td>
          </tr>
        </tbody>
      </table>
      </div>
    </div>

    <dialog ref="dlg" class="store-dialog admin-dialog" aria-labelledby="admin-dialog-title" @cancel.prevent="closeDialog" @click.self="closeDialog">
      <form v-if="dialog.mode" @submit.prevent="submitDialog">
        <div class="dialog-head">
          <h2 id="admin-dialog-title">{{ dialogTitle }}</h2><span class="spacer"></span>
          <button type="button" class="icon-btn" @click="closeDialog" aria-label="Close">×</button>
        </div>
        <div class="dialog-body admin-form">
          <template v-if="dialog.mode === 'create'">
            <label>Name <input v-model="dialog.form.name" class="mono" name="name" autocomplete="off" spellcheck="false"></label>
            <fieldset>
              <legend>Kind</legend>
              <label class="inline"><input type="radio" value="plain" v-model="dialog.form.kind"> Plain</label>
              <label class="inline" :title="bases.length ? '' : 'Create a base database first'"><input type="radio" value="layer" v-model="dialog.form.kind" :disabled="!bases.length"> Layer over a base</label>
            </fieldset>
            <label v-if="dialog.form.kind === 'layer'">Base
              <select v-model="dialog.form.base" name="base"><option v-for="b in bases" :key="b" :value="b">{{ b }}</option></select></label>
            <label v-else class="inline"><input type="checkbox" name="role" :checked="dialog.form.role === 'base'" @change="dialog.form.role = $event.target.checked ? 'base' : 'normal'">
              A base database (always read-only; layers can be created over it)</label>
            <label>Description <input v-model="dialog.form.description" name="description" autocomplete="off"></label>
            <label>Schema (optional, JSON)
              <textarea v-model="dialog.form.schema" name="schema" class="mono" rows="5" spellcheck="false" placeholder='{"mode": "open", ...}'></textarea></label>
            <label class="inline" v-if="dialog.form.role !== 'base' || dialog.form.kind === 'layer'"><input type="checkbox" name="readOnly" v-model="dialog.form.readOnly"> Read-only</label>
            <label class="inline"><input type="checkbox" name="autoLoad" v-model="dialog.form.autoLoad"> Load when the server starts</label>
          </template>
          <template v-else-if="dialog.mode === 'clone'">
            <p>A copy of the latest state of "{{ dialog.source }}" with the same kind, base, description and read-only flag; auto-load off. It is independent afterwards.</p>
            <label>Name of the clone <input v-model="dialog.form.name" name="name" class="mono" autocomplete="off" spellcheck="false"></label>
          </template>
          <template v-else-if="dialog.mode === 'alias'">
            <label>Alias <input v-model="dialog.alias" name="alias" class="mono" autocomplete="off" spellcheck="false" :readonly="dialog.existing"></label>
            <label>Database
              <select v-model="dialog.target" name="target"><option v-for="n in databaseNames" :key="n" :value="n">{{ n }}</option></select></label>
          </template>
          <ul v-if="dialog.problems.length" class="admin-problems" role="alert"><li v-for="p in dialog.problems" :key="p">{{ p }}</li></ul>
          <div v-if="dialog.error" class="notice admin-error" role="alert">
            <div class="admin-notice-text"><span>{{ dialog.error.text }}</span>
              <p v-if="dialog.error.help" class="admin-help">{{ dialog.error.help }}</p></div>
          </div>
          <div class="store-dialog-buttons">
            <button type="submit" class="btn primary" :disabled="dialog.busy">{{ dialog.busy ? 'Working…' : dialog.mode === 'create' ? 'Create' : dialog.mode === 'clone' ? 'Clone' : 'Save' }}</button>
            <button type="button" class="btn" :disabled="dialog.busy" @click="closeDialog">Cancel</button>
          </div>
        </div>
      </form>
    </dialog>
  </div>
  `,
};
