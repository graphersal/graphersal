// The export dialog of the admin panel (components/landing-page.js): downloads a packed snapshot
// (`.gsnap`) of a database's latest state from `GET .../export` (js/admin-api.js `exportPath`),
// with or without the embedding models' blobs and, for a layer, as a layer snapshot (only its
// changes) or materialized (a standalone full snapshot).
//
// Without host headers the download is a plain link (the browser streams the file to disk, so a
// large snapshot never sits in memory; a refusal shows as a failed download). A link cannot
// carry a header, so with host headers (`window.GRAPHERSAL_ENGINE.headers`) the file is fetched
// with them, kept as a Blob and saved; a refusal then shows the server's `error`/`help` here.

import { ref, reactive, computed, nextTick } from 'vue';
import { exportPath, parseAdminError, AdminError } from '../admin-api.js';
import { downloadBlob } from '../export.js';

export const AdminExport = {
  props: {
    base: { type: [Object, String], required: true },
    headers: { type: Object, default: () => ({}) },
  },
  emits: ['done'],
  setup(props, { emit, expose }) {
    const dlg = ref(null);
    const s = reactive({ name: '', layer: false, models: true, materialize: false, busy: false, error: null });
    const withHeaders = computed(() => Object.keys(props.headers || {}).length > 0);
    const target = computed(() => exportPath(s.name, { models: s.models, materialize: s.materialize, layer: s.layer }));
    const href = computed(() => new URL(target.value.path, props.base).href);
    // A layer snapshot never carries models: the box applies only to a full snapshot.
    const modelsApply = computed(() => !s.layer || s.materialize);

    async function open(row) {
      Object.assign(s, { name: row.name, layer: row.kind === 'layer', models: true, materialize: false, busy: false, error: null });
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function close() {
      if (s.busy) return;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    function linkClicked() {
      emit('done', { tone: 'ok', text: `The download of ${target.value.fileName} started.`, help: '' });
      close();
    }
    async function fetchAndSave() {
      s.busy = true;
      s.error = null;
      try {
        let r;
        try {
          r = await fetch(href.value, { cache: 'no-store', headers: { ...props.headers } });
        } catch (err) {
          throw new AdminError(`The server could not be reached: ${err && err.message ? err.message : err}`);
        }
        if (!r.ok) throw parseAdminError(r.status, await r.text());
        downloadBlob(await r.blob(), target.value.fileName);
        emit('done', { tone: 'ok', text: `Saved ${target.value.fileName}.`, help: '' });
        s.busy = false;
        close();
      } catch (err) {
        s.error = { text: err && err.message ? err.message : String(err), help: err && err.help ? err.help : '' };
      } finally {
        s.busy = false;
      }
    }

    expose({ open, close });
    return { dlg, s, withHeaders, target, href, modelsApply, close, linkClicked, fetchAndSave };
  },
  template: `
  <dialog ref="dlg" class="store-dialog admin-dialog" aria-labelledby="admin-export-title" @cancel.prevent="close" @click.self="close">
    <div class="dialog-head">
      <h2 id="admin-export-title">Export "{{ s.name }}"</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body admin-form">
      <p class="muted">A packed snapshot (.gsnap) of its latest state; another server imports it.</p>
      <fieldset v-if="s.layer">
        <legend>Layer</legend>
        <label class="inline"><input type="radio" :value="false" v-model="s.materialize"> Layer snapshot: only the layer's changes (loads only over the same base)</label>
        <label class="inline"><input type="radio" :value="true" v-model="s.materialize"> Materialized: a standalone snapshot of everything the layer shows</label>
      </fieldset>
      <label class="inline" :title="modelsApply ? '' : 'A layer snapshot never carries models'">
        <input type="checkbox" name="models" v-model="s.models" :disabled="!modelsApply"> With the embedding models' files
        <span class="muted">(without: the models stay named; the importing server needs them)</span></label>
      <div v-if="s.error" class="notice admin-error" role="alert">
        <div class="admin-notice-text"><span>{{ s.error.text }}</span>
          <p v-if="s.error.help" class="admin-help">{{ s.error.help }}</p></div>
      </div>
      <div class="store-dialog-buttons">
        <a v-if="!withHeaders" class="btn primary" :href="href" :download="target.fileName" @click="linkClicked">Download {{ target.fileName }}</a>
        <button v-else type="button" class="btn primary" :disabled="s.busy" @click="fetchAndSave">{{ s.busy ? 'Downloading…' : 'Download ' + target.fileName }}</button>
        <button type="button" class="btn" :disabled="s.busy" @click="close">Cancel</button>
      </div>
    </div>
  </dialog>
  `,
};
