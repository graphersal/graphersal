// The dialog of a saved query, opened from the Catalog dropdown's Saved queries manager
// (js/components/catalog.js), with two modes:
// - EDIT (create or change): name, folder, description, the language (Rhai / GQL switch), the
//   parameter list (name, type, constraints, default, description; add, remove, reorder) and
//   the body in a code editor: a Rhai function body with the DSL highlighting and completion,
//   or one GQL query with GQL highlighting (js/editor/gql-language.js) and "Add $parameters"
//   (declares the `$name`s it reads). Save calls defineQuery and shows the engine's validation
//   errors next to their field; Delete asks first; moving a query = changing its folder.
//   "Save as query…" of the query tabs opens it with `draftFromScript` (a `g.gql(..)` tab: a
//   GQL draft).
// - RUN: a form built from the parameter schemas (number fields with min/max, a select for an
//   enum, a checkbox for a boolean, UUID text, one line per item of a list; defaults prefilled,
//   required fields marked, descriptions as hints). Run WRITES `g.query("name", #{..})` into a
//   tab and runs it there (visible, editable, in the history); the last values per query are
//   kept in localStorage. A query that writes (badge R/W) lists its inferred effects, and Run
//   first shows a preview of what the call would change (a dry run, rolled back) and asks.
// The models are js/saved-queries.js (tested under Node); the engine calls go through store.js.

import { ref, reactive, computed, watch, nextTick, onBeforeUnmount, markRaw } from 'vue';
import { EditorState } from '@codemirror/state';
import { EditorView, keymap, lineNumbers, highlightActiveLine, drawSelection } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { bracketMatching, indentOnInput } from '@codemirror/language';
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete';
import {
  state, toast, defineSavedQuery, dropSavedQuery, runSavedQuery, previewSavedQuery, gqlVocabulary,
} from '../store.js';
import { storage } from '../util.js';
import {
  PARAM_TYPES, ITEM_TYPES, newParam, newDraft, draftFromQuery, draftToSpec,
  errorsByField, formFields, initialValues, formArgs, rememberValues, formatExample,
  accessBadge, previewText, languageBadge, switchDraftLanguage, withDetectedParams,
} from '../saved-queries.js';
import { dslLanguage, dslHighlight } from '../editor/dsl-language.js';
import { gqlLanguage } from '../editor/gql-language.js';
import { dslCompletionSource } from '../editor/completion.js';
import { gqlCompletionSource } from '../editor/gql-completion.js';
import { declaredParams, rhaiParamSource } from '../editor/param-completion.js';
import { loadCompletionData } from '../editor/completion-data.js';
import { floatingTooltips } from '../editor/tooltips.js';
import { editorTheme } from './query-editor.js';

const VALUES_KEY = 'savedQueryValues';
let completionData = null; // the DSL completion data (catalogue and token values), loaded once

/** The dialog of a saved query: edit (new or existing) and run. */
export const SavedQueryDialog = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const host = ref(null);
    const d = reactive({
      mode: null, // 'edit' | 'run'
      draft: null,
      entry: null, // the run mode's query
      fields: [],
      values: {},
      errors: {}, // field -> [messages] (edit) or name -> message (run)
      preview: null, // a query that writes: {text} or {error} of the call preview, before Run
      busy: false,
      loading: false,
    });
    let view = null;
    // A changed argument needs a new preview before a query that writes runs.
    watch(() => d.values, () => { d.preview = null; }, { deep: true });

    const message = (err) => (err && err.message ? err.message : String(err));

    function destroyEditor() {
      if (view) view.destroy();
      view = null;
    }

    async function mountEditor() {
      await nextTick();
      destroyEditor();
      if (!host.value || !d.draft) return;
      const gql = d.draft.language === 'gql';
      if (!gql && !completionData) completionData = markRaw(await loadCompletionData(state.engine));
      // The language: Rhai with the DSL completion, or GQL with the GQL completion. Both offer
      // the declared parameters (read from the table on every request, so they follow its edits):
      // a Rhai body their names (the function's arguments), a GQL body `$name` after `$`.
      const params = () => declaredParams(d.draft ? d.draft.params : []);
      if (gql) gqlVocabulary();
      const language = gql
        ? [
          gqlLanguage,
          floatingTooltips(host.value),
          autocompletion({
            override: [gqlCompletionSource(gqlVocabulary, params, () => state.savedQueries)],
            activateOnTyping: true,
            icons: true,
          }),
        ]
        : [
          floatingTooltips(host.value),
          autocompletion({
            override: [rhaiParamSource(() => (d.draft ? d.draft.params : [])), dslCompletionSource(() => completionData, () => state.savedQueries)],
            activateOnTyping: true,
            icons: true,
          }),
          dslLanguage,
        ];
      view = markRaw(new EditorView({
        parent: host.value,
        state: EditorState.create({
          doc: d.draft.body,
          extensions: [
            lineNumbers(), history(), drawSelection(), indentOnInput(), bracketMatching(), closeBrackets(),
            highlightActiveLine(),
            ...language,
            keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...completionKeymap]),
            dslHighlight, editorTheme, EditorView.lineWrapping,
            EditorView.contentAttributes.of({ 'aria-label': gql ? 'GQL query of the saved query' : 'Body of the saved query' }),
            EditorView.updateListener.of((u) => {
              if (u.docChanged && d.draft) d.draft.body = u.state.doc.toString();
            }),
          ],
        }),
      }));
    }

    function show() {
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }

    // Called when this dialog closes (Save, Cancel, Delete, Esc): the Saved queries manager
    // that opened it reopens, so the next query can be run or edited from the list. A Run
    // clears it (the call goes to a query tab).
    let back = null;

    function close() {
      destroyEditor();
      if (dlg.value && dlg.value.open) dlg.value.close();
      d.mode = null;
    }

    /** The dialog closed (by `close()` or Esc): return to where it was opened from. */
    function onClosed() {
      d.mode = null;
      const reopen = back;
      back = null;
      if (reopen) reopen();
    }

    /** "New saved query" (in `folder`); `options.back` is called when the dialog closes. */
    async function openNew(folder = '', options = {}) {
      await openDraft(newDraft(folder), options);
    }

    /** The editor on a prepared draft (Save as query: `draftFromScript`); `options.back` as for `openNew`. */
    async function openDraft(draft, options = {}) {
      back = options.back || null;
      Object.assign(d, { mode: 'edit', draft, entry: null, errors: {}, busy: false });
      show();
      await mountEditor();
    }

    async function loadEntry(name) {
      d.loading = true;
      try {
        const entry = await state.engine.getQuery(name);
        if (!entry) throw new Error(`the saved query "${name}" no longer exists`);
        return entry;
      } finally {
        d.loading = false;
      }
    }

    /** "Edit" of a stored query; `options.back` as for `openNew`. */
    async function openEdit(name, options = {}) {
      back = options.back || null;
      try {
        const entry = await loadEntry(name);
        Object.assign(d, { mode: 'edit', draft: draftFromQuery(entry), entry, errors: {}, busy: false });
        show();
        await mountEditor();
      } catch (err) {
        toast(`Cannot edit: ${message(err)}`, 'error', 8000);
      }
    }

    /** "Run…": the parameter form of a stored query; `options.back` as for `openNew` (not called
     * after a Run). */
    async function openRun(name, options = {}) {
      back = options.back || null;
      try {
        const entry = await loadEntry(name);
        destroyEditor();
        const fields = formFields(entry);
        const saved = (storage.get(VALUES_KEY, {}) || {})[name] || {};
        Object.assign(d, { mode: 'run', entry, draft: null, fields, values: initialValues(fields, saved), errors: {}, preview: null, busy: false });
        show();
        if (!fields.length) return; // nothing to ask: the form shows Run only
      } catch (err) {
        toast(`Cannot run: ${message(err)}`, 'error', 8000);
      }
    }

    expose({ openNew, openDraft, openEdit, openRun, close });

    // ---------------------------------------------------------------- edit
    const fieldErrors = computed(() => (d.mode === 'edit' ? d.errors : {}));
    const signature = computed(() => {
      if (!d.draft) return '';
      const params = d.draft.params.map((p) => p.name.trim() || '?').join(', ');
      return `fn ${d.draft.name.trim() || 'name'}(${params}) {`;
    });

    /** The Rhai / GQL switch: the body is converted (switchDraftLanguage) and the editor remounted. */
    async function setLanguage(language) {
      if (!d.draft || d.draft.language === language) return;
      d.draft = switchDraftLanguage(d.draft, language);
      d.errors = {};
      await mountEditor();
    }
    /** "Add $parameters": declares the `$name`s the GQL query reads that the list lacks. */
    function addDetectedParams() {
      const before = d.draft.params.length;
      d.draft.params = withDetectedParams(d.draft);
      if (d.draft.params.length === before) toast('Every $parameter of the query is declared already.', 'info', 4000);
    }
    const isGql = computed(() => !!d.draft && d.draft.language === 'gql');

    function addParam() {
      d.draft.params.push(newParam(`p${d.draft.params.length + 1}`));
    }
    function removeParam(i) {
      d.draft.params.splice(i, 1);
    }
    function moveParam(i, delta) {
      const j = i + delta;
      if (j < 0 || j >= d.draft.params.length) return;
      const [p] = d.draft.params.splice(i, 1);
      d.draft.params.splice(j, 0, p);
    }
    function paramErrors(param, index) {
      const e = fieldErrors.value;
      return [...(e[`params.${param.name.trim()}`] || []), ...(e[`params.${index}`] || [])];
    }

    async function save() {
      if (d.busy || !d.draft) return;
      const local = draftToSpec(d.draft);
      if (local.errors) {
        d.errors = errorsByField(local.errors);
        return;
      }
      d.busy = true;
      try {
        const res = await defineSavedQuery(local.spec);
        if (res && res.ok) {
          toast(`Saved query "${local.spec.name}" saved.`, 'info', 4000);
          close();
        } else {
          d.errors = errorsByField((res && res.errors) || [{ field: 'general', message: 'The engine refused it.' }]);
        }
      } catch (err) {
        d.errors = { general: [message(err)] };
      } finally {
        d.busy = false;
      }
    }

    async function remove() {
      const name = d.draft && d.draft.replaces;
      if (!name || d.busy) return;
      if (!window.confirm(`Delete the saved query "${name}"? Scripts that call it fail afterwards.`)) return;
      d.busy = true;
      try {
        await dropSavedQuery(name);
        toast(`Saved query "${name}" deleted.`, 'info', 4000);
        close();
      } catch (err) {
        d.errors = { general: [message(err)] };
      } finally {
        d.busy = false;
      }
    }

    // ---------------------------------------------------------------- run
    async function runNow() {
      if (d.busy || !d.entry) return;
      const { args, errors } = formArgs(d.fields, d.values);
      d.errors = errors;
      if (Object.keys(errors).length) return;
      // A query that writes: the first Run shows what the call would change and asks; Run
      // again (the confirm button) runs it. Changing an argument asks again.
      if (d.entry.writes && !(d.preview && !d.preview.error)) {
        d.busy = true;
        try {
          const res = await previewSavedQuery(d.entry.name, args);
          d.preview = res && res.ok
            ? { text: previewText(res.changes) }
            : { error: (res && res.error && (res.error.message || res.error.diagnostic)) || 'The preview failed.' };
        } catch (err) {
          d.preview = { error: message(err) };
        } finally {
          d.busy = false;
        }
        return;
      }
      const kept = Object.fromEntries(Object.entries(d.values).map(([k, v]) => [k, typeof v === "boolean" ? v : String(v ?? "")]));
      storage.set(VALUES_KEY, rememberValues(storage.get(VALUES_KEY, {}), d.entry.name, kept));
      d.busy = true;
      try {
        const name = d.entry.name;
        back = null;
        close();
        await runSavedQuery(name, args);
      } catch (err) {
        toast(`Run failed: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
      }
    }

    function editFromRun() {
      const name = d.entry && d.entry.name;
      if (name) openEdit(name, { back });
    }

    onBeforeUnmount(destroyEditor);

    return {
      state, dlg, host, d, PARAM_TYPES, ITEM_TYPES, fieldErrors, signature, close, formatExample, accessBadge,
      languageBadge, setLanguage, addDetectedParams, isGql, addParam, removeParam, moveParam, paramErrors, save, remove, runNow, editFromRun, onClosed,
    };
  },
  template: `
  <dialog ref="dlg" class="saved-query-dialog" aria-labelledby="sq-title" @close="onClosed">
    <form v-if="d.mode === 'edit' && d.draft" method="dialog" @submit.prevent="save">
      <div class="dialog-head">
        <h2 id="sq-title">{{ d.draft.replaces ? 'Edit saved query' : 'New saved query' }}</h2><span class="spacer"></span>
        <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
      </div>
      <div class="dialog-body">
        <div class="form-grid">
          <label for="sq-name">Name</label>
          <input id="sq-name" type="text" v-model="d.draft.name" :class="{ invalid: fieldErrors.name }" placeholder="top_customers" autocomplete="off" spellcheck="false">
          <div v-for="m in fieldErrors.name || []" class="field-error sq-span">{{ m }}</div>
          <label for="sq-folder">Folder</label>
          <input id="sq-folder" type="text" v-model="d.draft.folder" :class="{ invalid: fieldErrors.folder }" placeholder="reports/sales (empty: the root)" autocomplete="off" spellcheck="false">
          <div v-for="m in fieldErrors.folder || []" class="field-error sq-span">{{ m }}</div>
          <label for="sq-description">Description</label>
          <input id="sq-description" type="text" v-model="d.draft.description" :class="{ invalid: fieldErrors.description }" placeholder="What it answers, in one sentence">
          <div v-for="m in fieldErrors.description || []" class="field-error sq-span">{{ m }}</div>
          <span id="sq-language-label">Language</span>
          <span class="segmented sq-language" role="group" aria-labelledby="sq-language-label">
            <button v-for="l in [['rhai', 'Rhai'], ['gql', 'GQL']]" :key="l[0]" type="button" class="seg-btn"
                    :aria-pressed="String(d.draft.language === l[0])" :class="{ active: d.draft.language === l[0] }"
                    :title="l[0] === 'gql' ? 'One GQL query; its $name parameters are declared below' : 'A Rhai function body (Gremlin steps)'"
                    @click="setLanguage(l[0])">{{ l[1] }}</button>
          </span>
          <div v-for="m in fieldErrors.language || []" class="field-error sq-span">{{ m }}</div>
        </div>

        <h3>Parameters</h3>
        <p v-if="!d.draft.params.length" class="muted sq-note">No parameters. Callers pass named values, for example g.query("name", #{limit: 5}){{ isGql ? '; the query reads one as $limit' : '' }}.</p>
        <div v-for="(p, i) in d.draft.params" :key="i" class="sq-param" :class="{ invalid: paramErrors(p, i).length }">
          <div class="sq-param-row">
            <input type="text" v-model="p.name" class="sq-param-name" :aria-label="'Name of parameter ' + (i + 1)" placeholder="name" spellcheck="false">
            <select v-model="p.type" :aria-label="'Type of ' + p.name">
              <option v-for="t in PARAM_TYPES" :key="t" :value="t">{{ t === 'custom' ? 'schema (JSON)' : t }}</option>
            </select>
            <select v-if="p.type === 'array'" v-model="p.itemsType" :aria-label="'Item type of ' + p.name">
              <option v-for="t in ITEM_TYPES" :key="t" :value="t">of {{ t }}</option>
            </select>
            <input type="text" v-model="p.defaultText" class="sq-default" :aria-label="'Default of ' + p.name" :placeholder="p.type === 'boolean' ? 'default: true / false' : 'default (empty: required)'">
            <span class="spacer"></span>
            <button type="button" class="icon-btn" :disabled="i === 0" @click="moveParam(i, -1)" :aria-label="'Move ' + p.name + ' up'" title="Move up">↑</button>
            <button type="button" class="icon-btn" :disabled="i === d.draft.params.length - 1" @click="moveParam(i, 1)" :aria-label="'Move ' + p.name + ' down'" title="Move down">↓</button>
            <button type="button" class="icon-btn" @click="removeParam(i)" :aria-label="'Remove ' + p.name" title="Remove">✕</button>
          </div>
          <div class="sq-param-row">
            <template v-if="p.type === 'integer' || p.type === 'number'">
              <input type="text" v-model="p.minimum" placeholder="minimum" :aria-label="'Minimum of ' + p.name" class="sq-small">
              <input type="text" v-model="p.maximum" placeholder="maximum" :aria-label="'Maximum of ' + p.name" class="sq-small">
            </template>
            <template v-if="p.type === 'string'">
              <input type="text" v-model="p.minLength" placeholder="min length" :aria-label="'Minimum length of ' + p.name" class="sq-small">
              <input type="text" v-model="p.maxLength" placeholder="max length" :aria-label="'Maximum length of ' + p.name" class="sq-small">
              <input type="text" v-model="p.pattern" placeholder="pattern (regex)" :aria-label="'Pattern of ' + p.name" spellcheck="false">
            </template>
            <input v-if="['string', 'integer', 'number'].includes(p.type)" type="text" v-model="p.enumText" placeholder="allowed values: a, b, c" :aria-label="'Allowed values of ' + p.name">
            <input v-if="p.type !== 'custom'" type="text" v-model="p.description" placeholder="description (a hint in the run form)" :aria-label="'Description of ' + p.name" class="sq-grow">
          </div>
          <textarea v-if="p.type === 'custom'" v-model="p.schemaText" class="code-input sq-schema" rows="3" spellcheck="false"
                    :aria-label="'Schema of ' + p.name" placeholder='{"type": ["integer", "null"], "minimum": 0}'></textarea>
          <div v-for="m in paramErrors(p, i)" class="field-error">{{ m }}</div>
        </div>
        <div v-for="m in fieldErrors.params || []" class="field-error">{{ m }}</div>
        <button type="button" class="btn small" @click="addParam" style="margin-top:6px">+ Add parameter</button>
        <button v-if="isGql" type="button" class="btn small" @click="addDetectedParams" style="margin-top:6px"
                title="Declare every $name the query reads that the list above lacks (as a string; change the type)">+ Add $parameters of the query</button>

        <h3>{{ isGql ? 'GQL query' : 'Body' }}</h3>
        <p v-if="isGql" class="muted sq-note">One GQL query, stored as written (no g.gql(…) around it). Every $name it reads is a parameter declared above, and every declared parameter is read.</p>
        <p v-else-if="d.draft.raw" class="muted sq-note">The whole function text (its stored layout cannot be split): keep <code>fn {{ d.draft.name || 'name' }}(…)</code> and its parameters in step with the list above.</p>
        <div v-else class="sq-signature mono">{{ signature }}</div>
        <div ref="host" class="sq-editor" :class="{ invalid: fieldErrors.body }"></div>
        <div v-if="!d.draft.raw && !isGql" class="sq-signature mono">}</div>
        <div v-for="m in fieldErrors.body || []" class="field-error">{{ m }}</div>
        <p v-if="isGql" class="muted sq-note">A call returns the query's rows like g.gql(…), and callers may continue it with Gremlin steps. A query with INSERT or CREATE is saved as R/W: it writes when it is called.</p>
        <p v-else class="muted sq-note">Return a traversal (callers can continue it, read-only) or a value. A body that writes data is saved as R/W: a returned writing traversal such as <code>g.add_v("person").property("name", name)</code> runs when the query is called, and callers continue over what it wrote.</p>
        <div v-for="m in [...(fieldErrors.general || []), ...(fieldErrors.meta || [])]" class="field-error">{{ m }}</div>

        <div class="sq-buttons">
          <button v-if="d.draft.replaces" type="button" class="btn danger" :disabled="d.busy" @click="remove">Delete…</button>
          <span class="spacer"></span>
          <button type="button" class="btn" @click="close">Cancel</button>
          <button type="submit" class="btn primary" :disabled="d.busy">{{ d.busy ? 'Saving…' : 'Save' }}</button>
        </div>
      </div>
    </form>

    <form v-else-if="d.mode === 'run' && d.entry" method="dialog" @submit.prevent="runNow">
      <div class="dialog-head">
        <h2 id="sq-title">Run "{{ d.entry.name }}"</h2>
        <span :class="'badge ' + accessBadge(d.entry).kind" :title="accessBadge(d.entry).title">{{ accessBadge(d.entry).text }}</span>
        <span class="badge" :title="languageBadge(d.entry).title">{{ languageBadge(d.entry).text }}</span>
        <span class="spacer"></span>
        <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
      </div>
      <div class="dialog-body">
        <p v-if="d.entry.description" class="sq-description">{{ d.entry.description }}</p>
        <p v-if="d.entry.status === 'error'" class="field-error">It cannot run: {{ d.entry.error }}</p>
        <p v-if="d.entry.writes" class="sq-note">This query changes data: <code>{{ (d.entry.effects || []).map((e) => e.text).join(', ') }}</code>. Run shows what it would change first.</p>
        <p v-else-if="(d.entry.denied_writes || []).length" class="muted sq-note">Read-only: the writes of its body are denied. Save the query again (Edit… then Save) to let it write.</p>
        <p v-if="!d.fields.length" class="muted sq-note">No parameters.</p>
        <div v-else class="form-grid">
          <template v-for="f in d.fields" :key="f.name">
            <label :for="'sq-run-' + f.name">{{ f.name }}<span v-if="f.required" class="sq-required" title="required">*</span></label>
            <select v-if="f.kind === 'select'" :id="'sq-run-' + f.name" v-model="d.values[f.name]" :class="{ invalid: d.errors[f.name] }">
              <option v-if="!f.required" value="">{{ f.hasDefault ? '(default)' : '(none)' }}</option>
              <option v-for="o in f.options" :key="JSON.stringify(o)" :value="JSON.stringify(o)">{{ typeof o === 'string' ? o : JSON.stringify(o) }}</option>
            </select>
            <input v-else-if="f.kind === 'checkbox'" :id="'sq-run-' + f.name" type="checkbox" v-model="d.values[f.name]" style="justify-self:start">
            <input v-else-if="f.kind === 'integer' || f.kind === 'number'" :id="'sq-run-' + f.name" type="number"
                   :step="f.kind === 'integer' ? 1 : 'any'" :min="f.min" :max="f.max" v-model="d.values[f.name]"
                   :class="{ invalid: d.errors[f.name] }" :placeholder="f.hasDefault ? 'default ' + f.defaultValue : ''" style="width:160px">
            <textarea v-else-if="f.kind === 'list' || f.kind === 'json'" :id="'sq-run-' + f.name" v-model="d.values[f.name]" rows="3"
                      class="code-input" :class="{ invalid: d.errors[f.name] }" spellcheck="false"
                      :placeholder="f.kind === 'list' ? 'one ' + f.itemsKind + ' per line' : 'JSON'"></textarea>
            <input v-else :id="'sq-run-' + f.name" type="text" v-model="d.values[f.name]" :class="{ invalid: d.errors[f.name] }"
                   :placeholder="f.hasDefault ? 'default: ' + f.defaultValue : formatExample(f.kind)" spellcheck="false">
            <span v-if="d.errors[f.name]" class="hint sq-error">{{ d.errors[f.name] }}</span>
            <span v-if="f.hint" class="hint">{{ f.hint }}</span>
          </template>
        </div>
        <p class="muted sq-note">Run writes <code>g.query("{{ d.entry.name }}", #{…})</code> into a query tab and runs it there: visible, editable and in the history. An empty field takes the parameter's default.</p>
        <div v-if="d.preview" role="status" :class="d.preview.error ? 'field-error' : 'sq-preview'">
          <template v-if="d.preview.error">The preview failed: {{ d.preview.error }}</template>
          <template v-else>{{ d.preview.text }} Run it?</template>
        </div>
        <div class="sq-buttons">
          <button type="button" class="btn" @click="editFromRun">Edit…</button>
          <span class="spacer"></span>
          <button type="button" class="btn" @click="close">Cancel</button>
          <button type="submit" class="btn primary" :disabled="d.busy || state.running">{{ d.entry.writes ? (d.preview && !d.preview.error ? 'Run and change data' : 'Preview…') : 'Run' }}</button>
        </div>
      </div>
    </form>
  </dialog>
  `,
};
