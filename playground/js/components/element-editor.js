// The property editor of the Graph panel: a modal dialog that edits the properties of one vertex
// or edge (the inspector's pencil button). The model is js/element-editor.js; values travel in the
// engine's typed form (`engine.elementForEdit`, `engine.updateElement`), so an int64 stays an
// int64 and a string is never turned into another type by its content.
//
// - Rows: one line per property in a grid shared by all rows (name | type | value | actions);
//   a multi-line text, raw JSON and the children of an array or object go below their line. The
//   add row at the bottom has the same columns (name, type, value; Enter adds). Remove (undo),
//   rename. Nested arrays and objects are a tree (expand/collapse, add/remove items and keys);
//   any value, and the whole element (footer: Edit as JSON), can be edited as raw JSON instead.
// - Schema: the stored schema of the element's label(s) drives the form: declared properties are
//   listed (also when not set), required ones marked, with type, constraints and description as
//   hints, an `enum` as a menu, new values starting with the declared type.
// - Compression rules of the element's kind and label(s): strings at their paths get a
//   textarea right away (as long and multi-line strings do).
// - Checking: every change is checked by the ENGINE with a dry run of the write (debounced);
//   its errors are shown at their keys and Save is refused while there are any. Save writes the
//   changed and removed top-level keys as ONE unit; a refusal (schema, a read-only store) keeps
//   the dialog open with the error. Ids and labels are shown, not edited.
// - Create mode (`openCreate`, the Graph panel's edit mode, js/graph/edit-mode.js): the same form
//   for a NEW vertex or edge. Its label is picked from the engine's choices (`labelChoices`: the
//   schema's declared labels, for an edge only those whose connections allow the two vertices;
//   the graph's own labels; typed by hand unless the schema is closed); the form follows the
//   label's schema (required properties already there); an id may be given (else automatic).
//   Every draft is checked with a dry run of `createElement`; Create writes it as ONE unit and
//   emits `created` with what the drawing needs (`drawn`); the caller reloads the panels.

import { ref, reactive, computed, watch, nextTick } from 'vue';
import { state, elementEdited } from '../store.js';
import { serverReadOnlyReason } from '../server-mode.js';
import { debounce } from '../util.js';
import {
  TYPES, CONTAINERS, nodeFromTyped, setType, addItem, addEntry, scalarError, wantsTextarea, nodeToJson,
  nodeFromJsonText, defaultType, schemaFields, childSchema, schemaHint, schemaDescription, enumOptions,
  sameScalar, setScalar, compressionPaths, compressedAt, formFromElement, newRow, diffForm, engineErrors,
  formAsNode, applyObjectToForm, summary, emptyNode, typesOf, formForNew, splitHeadErrors, nodeWithValue,
  rowChanged, TEXT_TYPES, placeholderOf,
} from '../element-editor.js';
import { labelOptions, labelProblem, noEdgeLabelText } from '../graph/edit-mode.js';

const message = (err) => (err && err.message ? err.message : String(err));

/** One value: its type menu, its editor (scalar input or tree), its raw JSON mode. */
export const ValueEditor = {
  name: 'ValueEditor',
  props: {
    node: { type: Object, required: true },
    /** The schema node of this value (null: undeclared). */
    schema: { type: Object, default: null },
    /** Keys and indices from the element to this value. */
    path: { type: Array, default: () => [] },
    /** The compression rule paths of the element (js/element-editor.js `compressionPaths`). */
    compressed: { type: Array, default: () => [] },
    disabled: { type: Boolean, default: false },
    /** A property row of the dialog: the cells join the dialog's property grid. */
    inRow: { type: Boolean, default: false },
    /** What the value is, for the screen-reader labels ("name", "address.city"). */
    name: { type: String, default: '' },
  },
  setup(props) {
    const raw = ref(false);
    const rawText = ref('');
    const rawError = ref('');
    const multiline = ref(false);
    const rule = computed(() => compressedAt(props.compressed, props.path));
    const options = computed(() => (CONTAINERS.includes(props.node.type) ? null : enumOptions(props.schema)));
    const declared = computed(() => typesOf(props.schema));
    const wantsLong = computed(() => wantsTextarea(props.node, !!rule.value));
    const textarea = computed(() => multiline.value || wantsLong.value);
    const of = computed(() => (props.name ? ` of ${props.name}` : ''));
    const pathName = (k) => (typeof k === 'number' ? `${props.name}[${k}]` : `${props.name ? props.name + '.' : ''}${k || '(new key)'}`);
    const error = computed(() => scalarError(props.node.type, props.node.text));
    const isContainer = computed(() => CONTAINERS.includes(props.node.type));
    const typeTitle = (t) => (props.schema && !declared.value.includes(t) ? `${t} (the schema does not declare it)` : t);

    function openRaw() {
      rawText.value = nodeToJson(props.node);
      rawError.value = '';
      raw.value = true;
    }
    function applyRaw() {
      const r = nodeFromJsonText(rawText.value, props.node);
      if (r.error) {
        rawError.value = r.error;
        return;
      }
      Object.assign(props.node, { type: r.node.type, text: r.node.text, bool: r.node.bool, items: r.node.items, entries: r.node.entries, expanded: true });
      raw.value = false;
    }
    function changeType(t) {
      setType(props.node, t);
    }
    function pickOption(index) {
      const o = options.value && options.value[Number(index)];
      if (o) setScalar(props.node, o.typed);
    }
    const selectedOption = computed(() => (options.value ? options.value.findIndex((o) => sameScalar(props.node, o.typed)) : -1));
    function addChild() {
      if (props.node.type === 'array') addItem(props.node, defaultType(childSchema(props.schema, 0)));
      else addEntry(props.node, '', 'string');
    }
    function onEntryKey(entry) {
      // A key the schema declares starts with its declared type while the value is still empty.
      const sub = childSchema(props.schema, entry.key);
      if (sub && entry.node.type === 'string' && !entry.node.text) setType(entry.node, defaultType(sub));
    }
    return {
      TYPES, raw, rawText, rawError, multiline, rule, options, textarea, wantsLong, of, pathName, error, isContainer, typeTitle,
      openRaw, applyRaw, changeType, pickOption, selectedOption, addChild, onEntryKey, summary, childSchema,
      schemaHint, schemaDescription, placeholderOf,
    };
  },
  // One line: type | value | actions; what does not fit a line (a multi-line text, raw JSON, the
  // children of an array or object) goes below it. In a property row of the dialog (`inRow`) the
  // root takes no box (CSS `display: contents`), so these cells line up in the columns of the
  // dialog's property grid; nested values lay out the same cells in their own small grid. In a
  // row the value's own parse error is not repeated: the row lists it (`diffForm`) under the line.
  template: `
  <div class="ve" :class="{ 've-in-row': inRow }">
    <select class="ve-type" :value="node.type" @change="changeType($event.target.value)" :disabled="disabled || raw" :aria-label="'Type' + of">
      <option v-for="t in TYPES" :key="t" :value="t" :title="typeTitle(t)">{{ t }}</option>
    </select>
    <div class="ve-value">
      <span v-if="raw" class="faint ve-note">edited as JSON below</span>
      <select v-else-if="options && options.length" class="ve-input" :value="selectedOption" @change="pickOption($event.target.value)" :disabled="disabled" :aria-label="'Value' + of + ' (one of the allowed values)'">
        <option :value="-1" disabled>{{ selectedOption < 0 ? 'not one of the allowed values' : '' }}</option>
        <option v-for="(o, i) in options" :key="i" :value="i">{{ o.label }}</option>
      </select>
      <label v-else-if="node.type === 'boolean'" class="ve-bool"><input type="checkbox" v-model="node.bool" :disabled="disabled" :aria-label="'Value' + of"> {{ node.bool ? 'true' : 'false' }}</label>
      <input v-else-if="node.type === 'null'" class="ve-input ve-null mono" type="text" value="null" disabled :aria-label="'Value' + of + ' (null)'">
      <template v-else-if="node.type === 'string'">
        <span v-if="textarea" class="faint ve-note">text below · {{ node.text.length }} char{{ node.text.length === 1 ? '' : 's' }}</span>
        <input v-else class="ve-input" type="text" v-model="node.text" :disabled="disabled" :aria-label="'Value' + of">
      </template>
      <input v-else-if="!isContainer" class="ve-input mono" :class="{ invalid: error }" type="text" v-model="node.text" :disabled="disabled"
             :placeholder="placeholderOf(node.type)" :aria-label="'Value' + of">
      <a v-else href="#" class="ve-toggle" @click.prevent="node.expanded = !node.expanded" :aria-expanded="String(node.expanded)"
         :title="node.expanded ? 'Collapse' : 'Expand'">{{ node.expanded ? '▾' : '▸' }} {{ summary(node) }}</a>
      <span v-if="rule && node.type === 'string'" class="badge" :title="'Covered by the compression rule ' + rule">compressed</span>
    </div>
    <textarea v-if="textarea && !raw && node.type === 'string'" class="ve-input ve-text ve-below" v-model="node.text" :disabled="disabled" rows="4" spellcheck="false"
              :title="rule ? 'The compression rule ' + rule + ' keeps this text compressed in memory' : ''" :aria-label="'Value' + of"></textarea>
    <div v-if="error && !raw && !inRow" class="ve-error ve-under-value">{{ error }}</div>
    <div class="ve-actions">
      <button v-if="node.type === 'string' && !raw && (!textarea || (multiline && !wantsLong))" type="button" class="icon-btn ve-small"
              @click="multiline = !multiline" :disabled="disabled"
              :title="multiline ? 'Back to a one-line field' : 'Edit in a larger, multi-line box'"
              :aria-label="(multiline ? 'One-line field for the value' : 'Multi-line box for the value') + of">{{ multiline ? '⤡' : '⤢' }}</button>
      <button type="button" class="icon-btn ve-small" @click="raw ? (raw = false) : openRaw()" :disabled="disabled"
              :title="raw ? 'Back to the form (discards the JSON text)' : 'Edit this value as JSON'"
              :aria-label="(raw ? 'Back to the form' : 'Edit as JSON: the value') + of">{{ raw ? '×' : '{ }' }}</button>
      <slot name="actions"></slot>
    </div>
    <div v-if="raw" class="ve-raw ve-below">
      <textarea v-model="rawText" rows="6" spellcheck="false" class="mono" aria-label="Value as JSON"></textarea>
      <div class="ve-raw-actions">
        <button type="button" class="btn small" @click="applyRaw">Apply JSON</button>
        <span class="faint">Numbers without a fraction are int64, others float64; types of the values you keep stay as they were.</span>
      </div>
      <div v-if="rawError" class="ve-error">{{ rawError }}</div>
    </div>
    <div v-if="isContainer && node.expanded && !raw" class="ve-children ve-below">
      <template v-if="node.type === 'array'">
        <div v-for="(item, i) in node.items" :key="item.id" class="ve-child">
          <span class="ve-key faint mono">[{{ i }}]</span>
          <value-editor :node="item" :schema="childSchema(schema, i)" :path="[...path, i]" :compressed="compressed" :disabled="disabled" :name="pathName(i)">
            <template #actions><button type="button" class="icon-btn ve-small" @click="node.items.splice(i, 1)" :disabled="disabled" title="Remove this item" :aria-label="'Remove ' + pathName(i)">−</button></template>
          </value-editor>
        </div>
      </template>
      <template v-else>
        <div v-for="(e, i) in node.entries" :key="e.id" class="ve-child">
          <input class="ve-key mono" type="text" v-model="e.key" @change="onEntryKey(e)" :disabled="disabled" placeholder="key" aria-label="Key"
                 :title="schemaDescription(childSchema(schema, e.key)) || schemaHint(childSchema(schema, e.key))">
          <value-editor :node="e.node" :schema="childSchema(schema, e.key)" :path="[...path, e.key]" :compressed="compressed" :disabled="disabled" :name="pathName(e.key)">
            <template #actions><button type="button" class="icon-btn ve-small" @click="node.entries.splice(i, 1)" :disabled="disabled" title="Remove this key" :aria-label="'Remove ' + pathName(e.key)">−</button></template>
          </value-editor>
        </div>
      </template>
      <button type="button" class="btn small ve-add" @click="addChild" :disabled="disabled">+ {{ node.type === 'array' ? 'item' : 'key' }}</button>
    </div>
  </div>
  `,
};
ValueEditor.components = { ValueEditor };

export const ElementEditor = {
  components: { ValueEditor },
  emits: ['saved', 'created', 'committing'],
  setup(props, { expose, emit }) {
    const dlg = ref(null);
    const d = reactive({
      mode: 'edit', // 'edit' | 'create'
      kind: null,
      id: null,
      element: null, // the edit view
      form: null, // {rows, original}
      fields: null, // schemaFields(...)
      compressed: [],
      loading: false,
      loadError: '',
      saving: false,
      checking: false,
      checked: false, // the engine accepted the current draft
      engine: { byRow: new Map(), general: [] },
      saveError: '',
      raw: false, // the whole element as JSON
      rawText: '',
      rawError: '',
      newKey: '', newType: 'string', newText: '', newBool: false, // the add row
      // Create mode:
      choices: null, // labelChoices(...)
      schema: null, // the stored schema
      rules: [], // compression rules
      label: '',
      newId: '', // the id asked for ('' = automatic)
      ends: null, // an edge's endpoints: {from: {id, label}, to: {id, label}}
      head: {}, // engine errors of label / id / from / to
    });

    async function open(kind, id) {
      Object.assign(d, {
        mode: 'edit', choices: null, schema: null, rules: [], label: '', newId: '', ends: null, head: {},
        kind, id, element: null, form: null, fields: null, compressed: [], loading: true, loadError: '',
        saving: false, checking: false, checked: false, engine: { byRow: new Map(), general: [] },
        saveError: '', raw: false, rawText: '', rawError: '', newKey: '', newType: 'string', newText: '', newBool: false,
      });
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      try {
        const [element, schema, rules] = await Promise.all([
          state.engine.elementForEdit(kind, id),
          state.engine.getSchema().catch(() => null),
          typeof state.engine.listCompressions === 'function' ? state.engine.listCompressions().catch(() => []) : [],
        ]);
        if (!element) {
          d.loadError = `This ${kind} no longer exists (reload the drawing).`;
          return;
        }
        d.element = element;
        d.fields = schemaFields(schema, kind, element.labels || [element.label]);
        d.compressed = compressionPaths(rules, kind, element.labels || [element.label]);
        d.form = formFromElement(element, d.fields.declared ? d.fields : null);
      } catch (err) {
        d.loadError = message(err);
      } finally {
        d.loading = false;
      }
    }
    function close() {
      if (dlg.value) dlg.value.close();
    }

    /**
     * Opens the dialog for a NEW element: a vertex (`ends` null) or an edge between the vertices
     * `ends.from` and `ends.to` (`{id, label}` each).
     */
    async function openCreate(kind, ends = null) {
      Object.assign(d, {
        mode: 'create', kind, id: null, ends,
        element: { kind, id: '', label: '', labels: [], properties: [], readOnly: null, source: ends && ends.from.id, target: ends && ends.to.id },
        form: null, fields: null, compressed: [], loading: true, loadError: '', saving: false, checking: false, checked: false,
        engine: { byRow: new Map(), general: [] }, saveError: '', raw: false, rawText: '', rawError: '', newKey: '', newType: 'string', newText: '', newBool: false,
        choices: null, schema: null, rules: [], label: '', newId: '', head: {},
      });
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      try {
        const request = kind === 'edge' ? { kind, from: ends.from.id, to: ends.to.id } : { kind };
        const [choices, schema, rules] = await Promise.all([
          state.engine.labelChoices(request),
          state.engine.getSchema().catch(() => null),
          typeof state.engine.listCompressions === 'function' ? state.engine.listCompressions().catch(() => []) : [],
        ]);
        d.choices = choices;
        d.schema = schema;
        d.rules = rules || [];
        d.element.readOnly = choices.readOnly || null;
        const first = (choices.labels || [])[0];
        d.label = first ? first.label : '';
        applyLabel();
      } catch (err) {
        d.loadError = message(err);
      } finally {
        d.loading = false;
      }
    }

    /** The form for the chosen label (create mode); values already typed are kept. */
    function applyLabel() {
      const label = String(d.label || '').trim();
      const labels = label ? [label] : [];
      d.fields = schemaFields(d.schema, d.kind, labels);
      d.compressed = compressionPaths(d.rules, d.kind, labels);
      d.form = formForNew(d.fields, d.form);
      d.element.label = label;
      d.element.labels = labels;
    }
    watch(() => d.label, () => { if (d.mode === 'create' && d.choices) applyLabel(); });
    const options = computed(() => labelOptions(d.choices));
    const labelError = computed(() => (d.mode === 'create' && d.choices ? labelProblem(d.choices, d.label) : null));
    const noLabels = computed(() => (d.mode === 'create' && d.kind === 'edge' && d.ends ? noEdgeLabelText(d.choices, d.ends.from.label, d.ends.to.label) : null));
    const optionNote = computed(() => {
      const o = options.value.find((x) => x.label === String(d.label || '').trim());
      return o ? o.note : (d.label && d.choices && d.choices.free ? 'a new label' : '');
    });

    const readOnly = computed(() => (d.element && (serverReadOnlyReason(state.engineInfo) || d.element.readOnly)) || null);
    const diff = computed(() => (d.form ? diffForm(d.form, d.fields) : null));
    const fieldOf = (row) => (d.fields ? d.fields.fields.get(row.key) || null : null);
    const rowSchema = (row) => { const f = fieldOf(row); return f ? f.schema : null; };
    const unsetDeclared = computed(() => (d.form ? d.form.rows.filter((r) => r.declared && !r.node) : []));
    const liveRows = computed(() => (d.form ? d.form.rows.filter((r) => r.node || r.removed) : []));
    const keySuggestions = computed(() => unsetDeclared.value.map((r) => r.key));
    const blocked = computed(() => !!(diff.value && diff.value.errors.size) || d.engine.general.length > 0 || [...d.engine.byRow.values()].some((l) => l.length));
    const headBlocked = computed(() => !!labelError.value || Object.values(d.head).some((l) => l.length));
    const canSave = computed(() => !!(d.form && diff.value && (diff.value.changed || d.mode === 'create') && !blocked.value
      && !headBlocked.value && !readOnly.value && !d.saving && !d.checking && !d.raw));

    function request(validateOnly) {
      if (d.mode === 'create') {
        const label = String(d.label || '').trim();
        const out = d.kind === 'edge'
          ? { kind: 'edge', label, from: d.ends.from.id, to: d.ends.to.id }
          : { kind: 'vertex', labels: label ? [label] : [] };
        const id = String(d.newId || '').trim();
        if (id) out.id = id;
        out.properties = diff.value.set;
        out.validateOnly = validateOnly;
        return out;
      }
      return { kind: d.kind, id: d.id, set: diff.value.set, remove: diff.value.remove, validateOnly };
    }
    /** The engine's answer to a draft: errors at the head fields and at the rows. */
    function takeErrors(errors) {
      if (d.mode !== 'create') {
        d.engine = engineErrors(d.form, errors);
        return;
      }
      const { head, rest } = splitHeadErrors(errors);
      d.head = head;
      d.engine = engineErrors(d.form, rest);
    }
    const write = (req) => (d.mode === 'create' ? state.engine.createElement(req) : state.engine.updateElement(req));

    // The engine checks every draft (a dry run of the write), shortly after the last change.
    let checkSeq = 0;
    async function check() {
      const seq = ++checkSeq;
      d.checked = false;
      if (!d.form || readOnly.value) return;
      const df = diff.value;
      if (df.errors.size || (!df.changed && d.mode !== 'create') || labelError.value) {
        d.engine = { byRow: new Map(), general: [] };
        d.head = {};
        d.checking = false;
        return;
      }
      d.checking = true;
      try {
        const res = await write(request(true));
        if (seq !== checkSeq) return;
        if (res.ok) {
          d.engine = { byRow: new Map(), general: [] };
          d.head = {};
        } else takeErrors(res.errors);
        d.checked = !!res.ok;
      } catch (err) {
        if (seq !== checkSeq) return;
        d.engine = { byRow: new Map(), general: [`The check failed: ${message(err)}`] };
      } finally {
        if (seq === checkSeq) d.checking = false;
      }
    }
    const checkSoon = debounce(check, 350);
    watch(() => (d.form ? JSON.stringify([diff.value.set, diff.value.remove, d.mode === 'create' ? [d.label, d.newId] : null]) : ''), () => {
      d.saveError = '';
      d.checking = !!d.form;
      checkSoon();
    });

    async function save() {
      if (!canSave.value) return;
      d.saving = true;
      d.saveError = '';
      try {
        emit('committing', { mode: d.mode, kind: d.kind, id: d.id }); // the caller keeps the drawing's positions
        const res = await write(request(false));
        if (!res.ok) {
          // Nothing was applied: show why and stay open.
          takeErrors(res.errors);
          d.saveError = d.mode === 'create'
            ? 'Not created: the engine refused it (nothing was written).'
            : 'Not saved: the engine refused the change (nothing was applied).';
          return;
        }
        if (d.mode === 'create') {
          emit('created', { kind: d.kind, drawn: res.drawn, element: res.element });
          close();
          return;
        }
        if (res.changed) await elementEdited();
        emit('saved', { kind: d.kind, id: d.id });
        close();
      } catch (err) {
        d.saveError = `Not saved: ${message(err)}`;
      } finally {
        d.saving = false;
      }
    }

    function errorsOf(row) {
      const out = [];
      if (diff.value && diff.value.errors.has(row.id)) out.push(diff.value.errors.get(row.id));
      for (const e of d.engine.byRow.get(row.id) || []) out.push(e);
      return out;
    }
    function hintOf(row) {
      const f = fieldOf(row);
      if (!f) return d.fields && d.fields.declared && !d.fields.additional ? 'not declared by the schema (it admits no other keys)' : '';
      return [schemaHint(f.schema), schemaDescription(f.schema)].filter(Boolean).join(' — ');
    }
    function setDeclared(row) {
      row.node = emptyNode(defaultType(rowSchema(row)));
    }
    // The add row: a key the schema declares switches the type to the declared one (still
    // changeable); the value typed there becomes the new row's value.
    watch(() => d.newKey, (key) => {
      const f = d.fields && key ? d.fields.fields.get(key.trim()) : null;
      if (f) d.newType = defaultType(f.schema);
    });
    const newIsText = computed(() => TEXT_TYPES.includes(d.newType));
    function addProperty() {
      if (!d.form || readOnly.value) return;
      const key = d.newKey.trim();
      const init = { text: d.newText, bool: d.newBool };
      const declared = key && d.form.rows.find((r) => r.declared && !r.node && r.key === key);
      if (declared) declared.node = nodeWithValue(d.newType, init);
      else d.form.rows.push(newRow(key, d.newType, init));
      Object.assign(d, { newKey: '', newType: 'string', newText: '', newBool: false });
      nextTick(() => {
        if (!dlg.value) return;
        // Without a name the new row's name field takes the focus, else the add row's again.
        const inputs = dlg.value.querySelectorAll('.ee-prop .ee-key');
        if (!key && inputs.length) inputs[inputs.length - 1].focus();
        else { const add = dlg.value.querySelector('.ee-new-key'); if (add) add.focus(); }
      });
    }
    function removeRow(row) {
      if (row.originalKey === null && !row.declared) d.form.rows.splice(d.form.rows.indexOf(row), 1);
      else if (row.declared && row.originalKey === null) row.node = null;
      else row.removed = true;
    }
    function onRowKey(row) {
      const f = fieldOf(row);
      if (f && row.node && row.node.type === 'string' && !row.node.text && row.originalKey === null) setType(row.node, defaultType(f.schema));
    }

    function openRaw() {
      d.rawText = nodeToJson(formAsNode(d.form));
      d.rawError = '';
      d.raw = true;
    }
    function applyRaw() {
      const r = nodeFromJsonText(d.rawText, formAsNode(d.form));
      if (r.error || r.node.type !== 'object') {
        d.rawError = r.error || 'The element\'s properties are one JSON object.';
        return;
      }
      applyObjectToForm(d.form, r.node);
      d.raw = false;
    }

    const title = computed(() => {
      if (d.mode === 'create') {
        if (d.kind === 'edge' && d.ends) return `New edge ${d.ends.from.id} → ${d.ends.to.id}`;
        return 'New vertex';
      }
      if (!d.kind) return 'Edit properties';
      const el = d.element;
      return `Edit ${d.kind} ${d.id}${el && el.label ? ` · ${el.label}` : ''}`;
    });
    const modeNote = computed(() => {
      const f = d.fields;
      if (!f || !f.declared) {
        return f && f.mode !== 'none' && f.mode ? `The schema (mode ${f.mode}) declares none of this element's labels.` : '';
      }
      return f.mode === 'none'
        ? 'The schema (mode none) is shown as a guide; the engine does not enforce it.'
        : `The schema (mode ${f.mode}) is enforced: the engine checks every change before it is saved.`;
    });

    expose({ open, openCreate, close });
    return {
      options, labelError, noLabels, optionNote, headBlocked,
      dlg, d, title, readOnly, diff, liveRows, unsetDeclared, keySuggestions, canSave, blocked, save, close,
      errorsOf, hintOf, fieldOf, rowSchema, setDeclared, addProperty, removeRow, onRowKey, openRaw, applyRaw, modeNote,
      TYPES, newIsText, placeholderOf, changed: (row) => rowChanged(row, diff.value),
    };
  },
  template: `
  <dialog ref="dlg" class="element-editor" aria-labelledby="ee-title" @close="d.kind = null">
    <div class="dialog-head">
      <h2 id="ee-title">{{ title }}</h2>
      <span class="spacer"></span>
      <button class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <div v-if="d.loading" class="faint">Loading…</div>
      <div v-else-if="d.loadError" class="notice" role="alert">{{ d.loadError }}</div>
      <template v-else-if="d.form && d.mode === 'create'">
        <div v-if="readOnly" class="notice" role="status">Read-only: {{ readOnly }}</div>
        <div v-if="noLabels" class="notice" role="alert">{{ noLabels }}</div>
        <div class="ee-new">
          <label class="ee-new-field">Label
            <select v-if="!d.choices.free" v-model="d.label" :disabled="!!readOnly || !options.length" aria-label="Label">
              <option v-for="o in options" :key="o.label" :value="o.label">{{ o.label }}{{ o.note ? ' — ' + o.note : '' }}</option>
            </select>
            <template v-else>
              <input type="text" class="mono" v-model="d.label" list="ee-labels" :disabled="!!readOnly" placeholder="label" aria-label="Label" autocomplete="off">
              <datalist id="ee-labels"><option v-for="o in options" :key="o.label" :value="o.label">{{ o.note }}</option></datalist>
            </template>
          </label>
          <span v-if="d.choices.free" class="faint ee-label-note">{{ optionNote }}</span>
          <label class="ee-new-field">Id
            <input type="text" class="mono" v-model="d.newId" :disabled="!!readOnly" placeholder="automatic" aria-label="Id (empty: automatic)" size="12">
          </label>
        </div>
        <div v-if="labelError && d.label" class="ve-error" role="alert">{{ labelError }}</div>
        <div v-for="(e, i) in (d.head.label || [])" :key="'l' + i" class="ve-error" role="alert">{{ e }}</div>
        <div v-for="(e, i) in (d.head.id || [])" :key="'i' + i" class="ve-error" role="alert">Id: {{ e }}</div>
        <div v-for="(e, i) in [...(d.head.from || []), ...(d.head.to || [])]" :key="'e' + i" class="ve-error" role="alert">{{ e }}</div>
        <div v-if="d.kind === 'edge' && d.ends" class="ee-ident">
          <span><span class="faint">from → to</span> <span class="mono">{{ d.ends.from.id }}<template v-if="d.ends.from.label"> ({{ d.ends.from.label }})</template> → {{ d.ends.to.id }}<template v-if="d.ends.to.label"> ({{ d.ends.to.label }})</template></span></span>
        </div>
        <div v-if="modeNote" class="faint ee-note">{{ modeNote }}</div>
      </template>
      <template v-if="d.form && d.mode === 'edit' && !d.loading && !d.loadError">
        <div class="ee-ident" title="Ids and labels are not edited here.">
          <span><span class="faint">id</span> <span class="mono">{{ d.element.id }}</span></span>
          <span><span class="faint">{{ d.kind === 'vertex' && d.element.labels.length > 1 ? 'labels' : 'label' }}</span> <span class="mono">{{ d.kind === 'vertex' ? (d.element.labels.join(', ') || '(none)') : d.element.label }}</span></span>
          <span v-if="d.kind === 'edge'"><span class="faint">from → to</span> <span class="mono">{{ d.element.source }} → {{ d.element.target }}</span></span>
          <span class="faint ee-ident-note">Ids and labels are not edited here.</span>
        </div>
        <div v-if="readOnly" class="notice" role="status">Read-only: {{ readOnly }}</div>
        <div v-if="modeNote" class="faint ee-note">{{ modeNote }}</div>
      </template>
      <template v-if="d.form && !d.loading && !d.loadError">
        <div v-if="d.compressed.length" class="faint ee-note">Compression rules cover {{ d.compressed.map((p) => p.names.join('.')).join(', ') }}: those strings get a large text box.</div>

        <div v-if="d.raw" class="ee-raw">
          <textarea v-model="d.rawText" rows="16" spellcheck="false" class="mono" aria-label="All properties as JSON"></textarea>
          <div class="ve-raw-actions">
            <button type="button" class="btn small primary" @click="applyRaw">Apply JSON</button>
            <button type="button" class="btn small" @click="d.raw = false">Back to the form</button>
            <span class="faint">Numbers without a fraction are int64, others float64; the types of the values you keep stay as they were (a uuid or a date stays one).</span>
          </div>
          <div v-if="d.rawError" class="ve-error" role="alert">{{ d.rawError }}</div>
        </div>
        <template v-else>
          <!-- One grid for every row (CSS subgrid): name | type | value | actions line up across rows. -->
          <div class="ee-grid">
            <div v-if="!liveRows.length" class="faint ee-empty">No properties yet.</div>
            <div v-for="row in liveRows" :key="row.id" class="ee-row ee-prop"
                 :class="{ removed: row.removed, invalid: errorsOf(row).length, changed: changed(row) }">
              <div class="ee-name">
                <input class="ee-key mono" type="text" v-model="row.key" :disabled="row.removed || !!readOnly" @change="onRowKey(row)" placeholder="name" aria-label="Property name">
                <span v-if="fieldOf(row) && fieldOf(row).required" class="sq-required" title="Required by the schema">*</span>
                <span v-if="row.originalKey === null && !row.removed" class="badge">new</span>
                <span v-else-if="row.originalKey !== null && row.key !== row.originalKey && !row.removed" class="badge" :title="'Renamed from ' + row.originalKey">renamed</span>
              </div>
              <template v-if="row.removed">
                <span class="faint ee-removed-note">removed (saved without it)</span>
                <div class="ve-actions">
                  <button type="button" class="icon-btn ve-small" @click="row.removed = false" title="Undo remove" :aria-label="'Undo remove of ' + row.key">↶</button>
                </div>
              </template>
              <value-editor v-else in-row :node="row.node" :schema="rowSchema(row)" :path="[row.key]" :compressed="d.compressed" :disabled="!!readOnly" :name="row.key || 'the new property'">
                <template #actions>
                  <button type="button" class="icon-btn ve-small" @click="removeRow(row)" :disabled="!!readOnly" title="Remove this property" :aria-label="'Remove ' + (row.key || 'the new property')">−</button>
                </template>
              </value-editor>
              <div v-if="hintOf(row) && !row.removed" class="ee-hint faint ee-under">{{ hintOf(row) }}</div>
              <div v-for="(e, i) in errorsOf(row)" :key="i" class="ve-error ee-under" role="alert">{{ e }}</div>
            </div>
            <div class="ee-row ee-add-row">
              <div class="ee-name">
                <input type="text" class="mono ee-new-key" v-model="d.newKey" list="ee-keys" placeholder="new property name" aria-label="New property name"
                       :disabled="!!readOnly" @keydown.enter.prevent="addProperty">
                <datalist id="ee-keys"><option v-for="k in keySuggestions" :key="k" :value="k"></option></datalist>
              </div>
              <select class="ve-type" v-model="d.newType" :disabled="!!readOnly" aria-label="Type of the new property" @keydown.enter.prevent="addProperty">
                <option v-for="t in TYPES" :key="t" :value="t">{{ t }}</option>
              </select>
              <div class="ve-value">
                <input v-if="newIsText" type="text" class="ve-input" :class="{ mono: d.newType !== 'string' }" v-model="d.newText" :disabled="!!readOnly"
                       :placeholder="placeholderOf(d.newType)"
                       aria-label="Value of the new property" @keydown.enter.prevent="addProperty">
                <label v-else-if="d.newType === 'boolean'" class="ve-bool"><input type="checkbox" v-model="d.newBool" :disabled="!!readOnly" aria-label="Value of the new property"
                       @keydown.enter.prevent="addProperty"> {{ d.newBool ? 'true' : 'false' }}</label>
                <input v-else-if="d.newType === 'null'" class="ve-input ve-null mono" type="text" value="null" disabled aria-label="Value of the new property (null)">
                <span v-else class="faint ve-note">filled in after adding</span>
              </div>
              <div class="ve-actions">
                <button type="button" class="btn small" @click="addProperty" :disabled="!!readOnly" title="Add the property (Enter)">+ Add</button>
              </div>
            </div>
          </div>
          <div v-if="unsetDeclared.length" class="ee-declared">
            <span class="faint">Declared by the schema, not set:</span>
            <button v-for="row in unsetDeclared" :key="row.id" type="button" class="btn small" @click="setDeclared(row)" :disabled="!!readOnly"
                    :title="hintOf(row) || 'Set this property'">{{ row.key }}<span v-if="fieldOf(row) && fieldOf(row).required" class="sq-required">*</span></button>
          </div>
        </template>
        <div v-for="(e, i) in d.engine.general" :key="i" class="notice" role="alert">{{ e }}</div>
        <div v-if="d.saveError" class="notice" role="alert">{{ d.saveError }}</div>
        <div class="sq-buttons">
          <button type="button" class="btn primary" @click="save" :disabled="!canSave">{{ d.saving ? (d.mode === 'create' ? 'Creating…' : 'Saving…') : (d.mode === 'create' ? 'Create' : 'Save') }}</button>
          <button type="button" class="btn" @click="close">Cancel</button>
          <span class="faint ee-status" role="status">
            <template v-if="readOnly"></template>
            <template v-else-if="d.mode === 'create' && labelError">{{ labelError }}</template>
            <template v-else-if="d.mode === 'create' && headBlocked && !diff.errors.size">The engine refuses this {{ d.kind }} (see above).</template>
            <template v-else-if="d.mode === 'create' && !diff.errors.size && !d.checking && !blocked && d.checked">Checked by the engine: a new {{ d.kind }} with {{ diff.set.length }} propert{{ diff.set.length === 1 ? 'y' : 'ies' }}, created as one commit.</template>
            <template v-else-if="!diff.changed && d.mode !== 'create'">No changes.</template>
            <template v-else-if="diff.errors.size">Fix the marked fields first.</template>
            <template v-else-if="d.checking">Checking with the engine…</template>
            <template v-else-if="blocked">The engine refuses this change (see the marked fields).</template>
            <template v-else-if="d.checked">Checked by the engine: {{ diff.set.length }} to set, {{ diff.remove.length }} to remove, saved as one commit.</template>
          </span>
          <span class="spacer"></span>
          <button v-if="!d.raw" type="button" class="btn small ee-json" @click="openRaw" :disabled="!!readOnly" title="Edit all properties as one JSON object">Edit as JSON</button>
        </div>
      </template>
    </div>
  </dialog>
  `,
};
