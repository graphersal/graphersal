// The backups dialog of the admin panel (components/landing-page.js): the backups of one
// database (`GET .../backups`, oldest first; also for the name of a deleted database), "Back up
// now" (`POST .../backup`) and restore of a backup over the database itself (unloaded only) or
// into a new database (`POST .../restore`). Rules and texts: js/admin-api.js. The server's
// `error`/`help` stay open in the dialog; a finished action is reported to the panel (`done`).

import { ref, reactive, computed, nextTick } from 'vue';
import {
  backupRows, restoreOverAction, nameProblem, confirmText, outcomeText, bytesText,
} from '../admin-api.js';

const errorOf = (err) => ({
  text: err && err.message ? err.message : String(err),
  help: err && typeof err.help === 'string' ? err.help : '',
});

export const AdminBackups = {
  props: {
    admin: { type: Object, required: true },
    // The panel's listing rows (to find the database's state for "restore over").
    rows: { type: Array, default: () => [] },
  },
  emits: ['done'],
  setup(props, { emit, expose }) {
    const dlg = ref(null);
    const s = reactive({
      name: '', typed: '', ask: false, list: [], listed: false, error: null, busy: '',
      restoring: null, target: 'over', newName: '',
    });

    const row = computed(() => props.rows.find((r) => r.name === s.name && !r.aliasOf) || null);
    const over = computed(() => restoreOverAction(row.value));

    async function refresh() {
      if (!s.name) return;
      s.error = null;
      try {
        s.list = backupRows(await props.admin.backups(s.name));
        s.listed = true;
      } catch (err) {
        s.list = [];
        s.listed = false;
        s.error = errorOf(err);
      }
    }

    /** Opens the dialog for `name`; without a name it asks for one (a deleted database's backups). */
    async function open(name) {
      Object.assign(s, { name: name || '', typed: '', ask: !name, list: [], listed: false, error: null, busy: '', restoring: null });
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      await refresh();
    }
    function close() {
      if (s.busy) return;
      if (dlg.value && dlg.value.open) dlg.value.close();
      s.name = '';
    }
    async function show() {
      const problem = nameProblem(s.typed, 'database name');
      if (problem) {
        s.error = { text: problem, help: '' };
        return;
      }
      s.name = s.typed.trim().toLowerCase();
      s.restoring = null;
      await refresh();
    }

    async function backupNow() {
      s.busy = 'backup';
      s.error = null;
      try {
        const answer = await props.admin.backup(s.name);
        emit('done', { tone: 'ok', text: outcomeText('backup', s.name, answer), help: '' });
        await refresh();
      } catch (err) {
        s.error = errorOf(err);
      } finally {
        s.busy = '';
      }
    }

    function startRestore(b) {
      s.restoring = b.backup;
      s.target = over.value.enabled ? 'over' : 'new';
      s.newName = `${s.name}-restored`;
      s.error = null;
    }
    async function restore() {
      const backup = s.restoring;
      let newName = '';
      if (s.target === 'new') {
        const problem = nameProblem(s.newName, 'name for the restored database');
        if (problem) {
          s.error = { text: problem, help: '' };
          return;
        }
        newName = s.newName.trim().toLowerCase();
      } else if (!window.confirm(confirmText('restoreOver', s.name, backup))) {
        return;
      }
      s.busy = 'restore';
      s.error = null;
      try {
        const answer = await props.admin.restore(s.name, backup, newName || undefined);
        emit('done', { tone: 'ok', text: outcomeText('restore', newName || s.name, answer), help: '' });
        s.busy = '';
        close();
      } catch (err) {
        s.error = errorOf(err);
      } finally {
        s.busy = '';
      }
    }

    expose({ open, close });
    return { dlg, s, row, over, close, show, backupNow, startRestore, restore, bytesText, refresh };
  },
  template: `
  <dialog ref="dlg" class="store-dialog admin-dialog" aria-labelledby="admin-backups-title" @cancel.prevent="close" @click.self="close">
    <div class="dialog-head">
      <h2 id="admin-backups-title">{{ s.name ? 'Backups of "' + s.name + '"' : 'Backups' }}</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body admin-form">
      <form v-if="s.ask" class="admin-inline-form" @submit.prevent="show">
        <label>Database (also the name of a deleted one)
          <input v-model="s.typed" name="database" class="mono" autocomplete="off" spellcheck="false"></label>
        <button type="submit" class="btn">Show</button>
      </form>
      <div v-if="s.name" class="admin-dialog-bar">
        <span class="muted">Verified ZIP backups in the server's backups/{{ s.name }}/ directory.</span>
        <span class="spacer"></span>
        <button v-if="row && row.kind" type="button" class="btn small primary" :disabled="!!s.busy" @click="backupNow">{{ s.busy === 'backup' ? 'Backing up…' : 'Back up now' }}</button>
      </div>
      <p v-if="s.listed && !s.list.length" class="muted">No backups yet.</p>
      <div v-if="s.list.length" class="landing-table-wrap">
      <table class="data-table landing-table backups-table">
        <thead><tr><th>Backup</th><th>Size</th><th>Kind</th><th><span class="sr-only">Actions</span></th></tr></thead>
        <tbody>
          <tr v-for="b in s.list" :key="b.backup" :data-backup="b.backup">
            <td class="mono">{{ b.backup }}<div v-if="b.file" class="muted landing-sub">{{ b.file }}</div></td>
            <td class="num">{{ bytesText(b.bytes) }}</td>
            <td>{{ [b.kind, b.role === 'base' ? 'base' : ''].filter(Boolean).join(' · ') }}<div v-if="b.base" class="muted landing-sub">over {{ b.base }}</div></td>
            <td class="admin-actions"><button type="button" class="btn small" :disabled="!!s.busy" @click="startRestore(b)">Restore…</button></td>
          </tr>
        </tbody>
      </table>
      </div>
      <form v-if="s.restoring" class="admin-restore" @submit.prevent="restore">
        <fieldset>
          <legend>Restore {{ s.restoring }}</legend>
          <label class="inline" :title="over.title"><input type="radio" value="over" v-model="s.target" :disabled="!over.enabled"> Over "{{ s.name }}" itself</label>
          <p v-if="!over.enabled" class="muted landing-sub">{{ over.title }}</p>
          <label class="inline"><input type="radio" value="new" v-model="s.target"> Into a new database</label>
          <label v-if="s.target === 'new'">Name <input v-model="s.newName" name="newName" class="mono" autocomplete="off" spellcheck="false"></label>
        </fieldset>
        <div class="store-dialog-buttons">
          <button type="submit" class="btn primary" :disabled="!!s.busy">{{ s.busy === 'restore' ? 'Restoring…' : 'Restore' }}</button>
          <button type="button" class="btn" :disabled="!!s.busy" @click="s.restoring = null">Cancel</button>
        </div>
      </form>
      <div v-if="s.error" class="notice admin-error" role="alert">
        <div class="admin-notice-text"><span>{{ s.error.text }}</span>
          <p v-if="s.error.help" class="admin-help">{{ s.error.help }}</p></div>
      </div>
    </div>
  </dialog>
  `,
};
