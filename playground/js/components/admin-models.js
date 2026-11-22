// The models section of the admin panel (components/landing-page.js): the embedding model blobs
// the server keeps once in its models/ directory (`GET api/admin/models`: repository, licence,
// size, the databases holding it, the ones waiting for it with "Give to waiting" (`{repo}`), a
// warning when a database holds a copy instead of a hard link), the `needs-model` databases whose
// model models/ does not hold, add (`POST
// api/admin/models`: a model directory or .gsnap on the server, or a repository models/ already
// holds, which also resolves the waiting databases) and remove (`DELETE api/admin/models/<repo>`;
// refused with help while a database uses it). Rules and texts: js/admin-api.js.

import { ref, reactive, nextTick, onMounted, watch } from 'vue';
import {
  modelRows, missingRows, removeModelAction, addModelBody, confirmText, outcomeText, bytesText,
} from '../admin-api.js';

const errorNotice = (err) => ({
  tone: 'error',
  text: err && err.message ? err.message : String(err),
  help: err && typeof err.help === 'string' ? err.help : '',
});
const freshForm = () => ({ source: 'path', path: '', repo: '', license: '' });

export const AdminModels = {
  props: {
    admin: { type: Object, required: true },
    // Bumped by the panel after its list reloads: the models are read again.
    version: { type: Number, default: 0 },
  },
  emits: ['done'],
  setup(props, { emit, expose }) {
    const models = ref([]);
    const missing = ref([]);
    const error = ref(null);
    const loaded = ref(false);
    const busy = reactive({});
    const dlg = ref(null);
    const dialog = reactive({ form: freshForm(), problems: [], error: null, busy: false });

    async function load() {
      try {
        const answer = await props.admin.models();
        models.value = modelRows(answer);
        missing.value = missingRows(answer);
        error.value = null;
      } catch (err) {
        error.value = errorNotice(err);
      } finally {
        loaded.value = true;
      }
    }

    /** Reports a finished add: the resolved databases are loadable now; the still missing ones are named. */
    function addedNotice(answer) {
      const still = Array.isArray(answer.stillMissing) ? answer.stillMissing.filter((m) => m && m.database) : [];
      const notes = Array.isArray(answer.notes) ? answer.notes.filter((n) => typeof n === 'string') : [];
      const lines = [
        ...still.map((m) => `"${m.database}" still waits for ${m.model}.`),
        ...notes,
      ];
      return { tone: still.length ? 'warn' : 'ok', text: outcomeText('addModel', '', answer), help: lines.join('\n') };
    }

    async function remove(m) {
      if (!window.confirm(confirmText('removeModel', m.id))) return;
      busy[m.blob] = true;
      try {
        const answer = await props.admin.removeModel(m.id);
        emit('done', { tone: 'ok', text: outcomeText('removeModel', m.id, answer), help: '' });
      } catch (err) {
        emit('done', errorNotice(err));
      } finally {
        delete busy[m.blob];
        await load();
      }
    }
    /** `{repo}` alone: a model models/ already holds resolves the databases waiting for it. */
    async function addByRepo(repo) {
      busy[`repo:${repo}`] = true;
      try {
        emit('done', addedNotice(await props.admin.addModel({ repo })));
      } catch (err) {
        emit('done', errorNotice(err));
      } finally {
        delete busy[`repo:${repo}`];
        await load();
      }
    }

    async function openAdd(init = {}) {
      Object.assign(dialog, { form: { ...freshForm(), ...init }, problems: [], error: null, busy: false });
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function closeAdd() {
      if (dialog.busy) return;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    async function submitAdd() {
      if (dialog.busy) return;
      const built = addModelBody(dialog.form);
      dialog.problems = built.problems || [];
      if (!built.body) return;
      dialog.busy = true;
      dialog.error = null;
      try {
        const answer = await props.admin.addModel(built.body);
        dialog.busy = false;
        emit('done', addedNotice(answer));
        closeAdd();
      } catch (err) {
        dialog.error = errorNotice(err);
      } finally {
        dialog.busy = false;
        await load();
      }
    }

    onMounted(load);
    watch(() => props.version, load);
    expose({ load, openAdd });
    return {
      models, missing, error, loaded, busy, dlg, dialog, load, remove, addByRepo, openAdd, closeAdd, submitAdd,
      bytesText, removeAct: removeModelAction,
      held: (repo) => models.value.some((m) => m.repo === repo),
      shortHash: (h) => h.slice(0, 12),
    };
  },
  template: `
  <section class="admin-models">
    <div class="landing-head">
      <h2>Models</h2>
      <span class="spacer"></span>
      <button class="btn small" type="button" @click="load">Refresh</button>
      <button class="btn small" type="button" @click="openAdd()">Add model…</button>
    </div>
    <div v-if="error" class="notice admin-error" role="alert">
      <div class="admin-notice-text"><span>The models could not be read: {{ error.text }}</span>
        <p v-if="error.help" class="admin-help">{{ error.help }}</p></div>
    </div>
    <p v-else-if="loaded && !models.length" class="muted">No embedding models. The server keeps one copy of each model in its models/ directory and hard-links it into every database that uses it.</p>
    <div v-if="models.length" class="landing-table-wrap">
    <table class="data-table landing-table models-table">
      <thead><tr><th>Model</th><th>Size</th><th>Used by</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="m in models" :key="m.blob" :data-model="m.id">
          <td><span class="mono">{{ m.repo || '(not a model archive)' }}</span>
            <div class="muted landing-sub mono" :title="m.blob">{{ shortHash(m.blob) }}<template v-if="m.license"> · {{ m.license }}</template></div>
            <div v-if="m.copied" class="landing-sub admin-warn" :title="m.linkWarning">Copied, not linked, in {{ m.copiedIn.join(', ') }}<template v-if="m.linkWarning">: {{ m.linkWarning }}</template></div></td>
          <td class="num">{{ bytesText(m.bytes) }}</td>
          <td>{{ m.users.join(', ') || '—' }}
            <div v-if="m.waiting.length" class="muted landing-sub">waiting: {{ m.waiting.join(', ') }}</div></td>
          <td class="admin-actions">
            <button v-if="m.repo && m.waiting.length" type="button" class="btn small" :disabled="!!busy['repo:' + m.repo]" :title="'Give it to ' + m.waiting.join(', ') + ' (they become loadable)'" @click="addByRepo(m.repo)">Give to waiting</button>
            <button type="button" class="btn small danger" :disabled="!!busy[m.blob] || !removeAct(m).enabled" :title="removeAct(m).title" @click="remove(m)">Remove…</button>
          </td>
        </tr>
      </tbody>
    </table>
    </div>
    <template v-if="missing.length">
      <h3 class="admin-sub-head">Databases waiting for a model</h3>
      <div class="landing-table-wrap">
      <table class="data-table landing-table missing-table">
        <thead><tr><th>Database</th><th>Missing model</th><th><span class="sr-only">Actions</span></th></tr></thead>
        <tbody>
          <tr v-for="m in missing" :key="m.database" :data-missing="m.database">
            <td class="mono">{{ m.database }}</td>
            <td class="mono">{{ m.model }}</td>
            <td class="admin-actions">
              <button v-if="held(m.model)" type="button" class="btn small" :disabled="!!busy['repo:' + m.model]" title="models/ holds it: give it to the waiting databases" @click="addByRepo(m.model)">Use from models/</button>
              <button v-else type="button" class="btn small" title="Add it from a model directory or .gsnap on the server" @click="openAdd({ repo: m.model })">Add…</button>
            </td>
          </tr>
        </tbody>
      </table>
      </div>
    </template>

    <dialog ref="dlg" class="store-dialog admin-dialog" aria-labelledby="admin-model-title" @cancel.prevent="closeAdd" @click.self="closeAdd">
      <form @submit.prevent="submitAdd">
        <div class="dialog-head">
          <h2 id="admin-model-title">Add a model</h2><span class="spacer"></span>
          <button type="button" class="icon-btn" @click="closeAdd" aria-label="Close">×</button>
        </div>
        <div class="dialog-body admin-form">
          <fieldset>
            <legend>From</legend>
            <label class="inline"><input type="radio" value="path" v-model="dialog.form.source"> A Model2Vec model directory or .gsnap on the server's machine</label>
            <label class="inline"><input type="radio" value="repo" v-model="dialog.form.source"> A repository models/ already holds</label>
          </fieldset>
          <label v-if="dialog.form.source === 'path'">Absolute path
            <input v-model="dialog.form.path" name="path" class="mono" autocomplete="off" spellcheck="false" placeholder="/models/potion-base-8M"></label>
          <label>Repository (owner/name){{ dialog.form.source === 'path' ? ', when the directory does not name it' : '' }}
            <input v-model="dialog.form.repo" name="repo" class="mono" autocomplete="off" spellcheck="false"></label>
          <label v-if="dialog.form.source === 'path'">Licence (SPDX id, when the directory does not name it)
            <input v-model="dialog.form.license" name="license" class="mono" autocomplete="off" spellcheck="false"></label>
          <p class="muted landing-sub">Every database waiting for the model gets it and can be loaded.</p>
          <ul v-if="dialog.problems.length" class="admin-problems" role="alert"><li v-for="p in dialog.problems" :key="p">{{ p }}</li></ul>
          <div v-if="dialog.error" class="notice admin-error" role="alert">
            <div class="admin-notice-text"><span>{{ dialog.error.text }}</span>
              <p v-if="dialog.error.help" class="admin-help">{{ dialog.error.help }}</p></div>
          </div>
          <div class="store-dialog-buttons">
            <button type="submit" class="btn primary" :disabled="dialog.busy">{{ dialog.busy ? 'Adding…' : 'Add' }}</button>
            <button type="button" class="btn" :disabled="dialog.busy" @click="closeAdd">Cancel</button>
          </div>
        </div>
      </form>
    </dialog>
  </section>
  `,
};
