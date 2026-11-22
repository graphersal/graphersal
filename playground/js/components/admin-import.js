// The import dialog of the admin panel (components/landing-page.js): a new database from a file
// uploaded from this browser (`POST api/admin/import?name=...` with the file as the raw body,
// through XMLHttpRequest for the progress bar; the size is checked against the root
// `/api/info` `uploadBytes` before anything is sent) or from a path on the server's machine (the
// JSON form). Sources: a packed snapshot (full or layer), a Store, GraphSON or GraphML; the format
// is detected unless chosen. Request and rules: js/admin-api.js `importRequest`, `uploadImport`.
// The server's `error`/`help` (a `413` included) stay open in the dialog; Cancel aborts a running
// upload. "Import as a base" (P4) sends `role=base` (only a plain source becomes a base: the
// server refuses a layer source with `400` and a source missing its model with `409` + help).

import { ref, reactive, computed, nextTick } from 'vue';
import {
  importRequest, uploadImport, uploadLimit, uploadSizeProblem, progressText, suggestName,
  outcomeText, needsModelHint, bytesText, IMPORT_FORMATS, BASE_IS_FROZEN,
} from '../admin-api.js';

const freshForm = () => ({ source: 'upload', file: null, path: '', name: '', base: '', format: '', description: '', role: 'normal' });

export const AdminImport = {
  props: {
    admin: { type: Object, required: true },
    base: { type: [Object, String], required: true },
    headers: { type: Object, default: () => ({}) },
    // The root `/api/info` answer (for `uploadBytes`).
    info: { type: Object, default: () => ({}) },
    // The base databases a layer snapshot or Store may name.
    bases: { type: Array, default: () => [] },
  },
  emits: ['done'],
  setup(props, { emit, expose }) {
    const dlg = ref(null);
    const form = reactive(freshForm());
    const s = reactive({ busy: false, problems: [], error: null, loaded: 0, total: 0, sent: false });
    let running = null;
    let nameTouched = false;

    const limit = computed(() => uploadLimit(props.info));
    const fileProblem = computed(() => (form.source === 'upload' && form.file ? uploadSizeProblem(form.file.size, limit.value) : null));
    const percent = computed(() => (s.total > 0 ? Math.min(100, (s.loaded / s.total) * 100) : 0));

    async function open() {
      Object.assign(form, freshForm());
      Object.assign(s, { busy: false, problems: [], error: null, loaded: 0, total: 0, sent: false });
      nameTouched = false;
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function close() {
      if (s.busy) return;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    function pickFile(event) {
      const file = event.target.files && event.target.files[0];
      form.file = file || null;
      if (file && !nameTouched) form.name = suggestName(file.name);
    }
    function pathChanged() {
      if (!nameTouched) form.name = suggestName(form.path);
    }

    function finished(answer, subject) {
      const imp = answer.import && typeof answer.import === 'object' ? answer.import : {};
      const notes = Array.isArray(imp.notes) ? imp.notes.filter((n) => typeof n === 'string') : [];
      const hint = needsModelHint(answer);
      emit('done', {
        tone: answer.state === 'needs-model' ? 'warn' : 'ok',
        text: outcomeText('import', subject, answer),
        help: [hint, ...notes].filter(Boolean).join('\n'),
      });
    }

    async function submit() {
      if (s.busy) return;
      const built = importRequest(form, limit.value);
      s.problems = built.problems || [];
      s.error = null;
      if (built.problems) return;
      const subject = form.name.trim().toLowerCase();
      s.busy = true;
      try {
        let answer;
        if (built.upload) {
          Object.assign(s, { loaded: 0, total: built.upload.size, sent: false });
          running = uploadImport({ base: props.base, headers: props.headers, limit: limit.value }, built.upload, form.file, {
            onProgress: (loaded, total) => { s.loaded = loaded; s.total = total; },
            onSent: () => { s.sent = true; },
          });
          answer = await running.promise;
        } else {
          answer = await props.admin.importPath(built.request.body);
        }
        s.busy = false;
        finished(answer || {}, subject);
        close();
      } catch (err) {
        s.error = { text: err && err.message ? err.message : String(err), help: err && err.help ? err.help : '' };
      } finally {
        s.busy = false;
        running = null;
      }
    }
    function cancel() {
      if (running) {
        running.abort();
        return;
      }
      close();
    }

    expose({ open, close });
    return {
      dlg, form, s, limit, fileProblem, percent, close, cancel, submit, pickFile, pathChanged,
      progressText, bytesText, formats: IMPORT_FORMATS, frozen: BASE_IS_FROZEN,
      touchName: () => { nameTouched = true; },
    };
  },
  template: `
  <dialog ref="dlg" class="store-dialog admin-dialog" aria-labelledby="admin-import-title" @cancel.prevent="close" @click.self="close">
    <form @submit.prevent="submit">
      <div class="dialog-head">
        <h2 id="admin-import-title">Import a database</h2><span class="spacer"></span>
        <button type="button" class="icon-btn" @click="close" aria-label="Close" :disabled="s.busy">×</button>
      </div>
      <div class="dialog-body admin-form">
        <fieldset :disabled="s.busy">
          <legend>Source</legend>
          <label class="inline"><input type="radio" value="upload" v-model="form.source"> A file from this computer (uploaded)</label>
          <label class="inline"><input type="radio" value="path" v-model="form.source"> A file or Store directory on the server's machine</label>
        </fieldset>
        <label v-if="form.source === 'upload'">File (.gsnap, .gstore, GraphSON, GraphML)
          <input type="file" name="file" @change="pickFile" :disabled="s.busy">
          <span class="muted landing-sub">{{ limit ? 'Up to ' + bytesText(limit) + ' per upload.' : '' }}<template v-if="form.file"> This file: {{ bytesText(form.file.size) }}.</template></span></label>
        <p v-if="fileProblem" class="admin-problems" role="alert">{{ fileProblem }}</p>
        <label v-if="form.source === 'path'">Absolute path on the server
          <input v-model="form.path" name="path" class="mono" autocomplete="off" spellcheck="false" placeholder="/data/acme.gsnap" @change="pathChanged" :disabled="s.busy"></label>
        <label>Name of the new database
          <input v-model="form.name" name="name" class="mono" autocomplete="off" spellcheck="false" @input="touchName" :disabled="s.busy"></label>
        <label>Format
          <select v-model="form.format" name="format" :disabled="s.busy">
            <option v-for="f in formats" :key="f" :value="f">{{ f || 'detected from the content' }}</option>
          </select></label>
        <label class="inline"><input type="checkbox" name="role" :checked="form.role === 'base'" :disabled="s.busy"
            @change="form.role = $event.target.checked ? 'base' : 'normal'">
          Import as a base (from a full snapshot, a plain Store, GraphSON or GraphML; layers are built over it)</label>
        <p v-if="form.role === 'base'" class="muted landing-sub">{{ frozen }}</p>
        <label v-if="bases.length && form.role !== 'base'">Base (a layer snapshot or layer Store; empty: the base with its identity)
          <select v-model="form.base" name="base" :disabled="s.busy">
            <option value="">(found by the snapshot's base identity)</option>
            <option v-for="b in bases" :key="b" :value="b">{{ b }}</option>
          </select></label>
        <label>Description <input v-model="form.description" name="description" autocomplete="off" :disabled="s.busy"></label>
        <div v-if="s.busy && form.source === 'upload'" class="admin-progress" role="status">
          <progress :value="s.sent ? 100 : percent" max="100"></progress>
          <span class="muted">{{ s.sent ? 'Uploaded; the server is importing it…' : 'Uploading ' + progressText(s.loaded, s.total) }}</span>
        </div>
        <p v-else-if="s.busy" class="muted" role="status">The server is importing it…</p>
        <ul v-if="s.problems.length" class="admin-problems" role="alert"><li v-for="p in s.problems" :key="p">{{ p }}</li></ul>
        <div v-if="s.error" class="notice admin-error" role="alert">
          <div class="admin-notice-text"><span>{{ s.error.text }}</span>
            <p v-if="s.error.help" class="admin-help">{{ s.error.help }}</p></div>
        </div>
        <div class="store-dialog-buttons">
          <button type="submit" class="btn primary" :disabled="s.busy || !!fileProblem">{{ s.busy ? 'Importing…' : 'Import' }}</button>
          <button type="button" class="btn" :disabled="s.busy && (form.source !== 'upload' || s.sent)" @click="cancel">{{ s.busy && form.source === 'upload' && !s.sent ? 'Cancel upload' : 'Cancel' }}</button>
        </div>
      </div>
    </form>
  </dialog>
  `,
};
