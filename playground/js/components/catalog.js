// The Catalog: everything stored in the graph besides its data and schema (the catalog of
// definitions: saved queries, compression rules, property indexes).
//
// - `CatalogMenu`: the top bar's "Catalog ▾" dropdown. One item per kind with its count, each
//   opening that kind's manager, and the catalog file: "Save catalog (.json)" downloads every
//   kind as one file (`graphersal-catalog-<stamp>.json`), "Load catalog…" stores a file's
//   definitions (a compression rule re-encodes its values).
// - `CatalogDialogs`: the managers, hosted once by the workspace and opened through
//   `openCatalog(id)` (store.js).
//   * Saved queries: the queries by folder with their access and language badges and Run,
//     Edit, Delete and New (the editor and run form are js/components/saved-queries.js's
//     dialog); `openCatalog('save-as-query')` opens the editor on the active tab's text (the
//     query tabs' "Save as query…", `state.catalogDialogScript`).
//   * Compression: the rules with their stats (values, plain -> stored size, ratio, saving,
//     dictionary) with Edit, Recompress and Drop, and the form (name, element, label with the
//     graph's labels as suggestions, path, minimum size, dictionary) with the engine's field
//     errors next to their inputs. Its Estimate button measures the candidate rules on the
//     current data (the graph is not changed): a table with a saving bar per candidate, the
//     best one highlighted, and "Use" copying a candidate's codec, threshold and dictionary
//     into the form.
//   * Scheduled jobs: the system jobs and the jobs of users (catalog kind `job`) with their
//     schedule, next and last run (its error), runs in two days and a switch; Run now, Edit
//     and Drop for a job of a user, a schedule of its own (or the built-in one again) for a
//     system job; the form (name, action: a saved query with JSON arguments, a checkpoint or a
//     backup directory, a schedule with presets, enabled, timeout, description) with the
//     engine's field errors next to their inputs. Model js/jobs.js.
//   * Property indexes: the indexes with their stats (entries, distinct values, elements per
//     value, memory; "not served" on a storage without indexes) with Drop (confirmed), and the
//     create form (element, label with suggestions, key, optional name with the default name
//     as placeholder) with the engine's field errors next to their inputs.
// The models are js/compression.js, js/indexes.js and js/saved-queries.js (tested under Node).

import { ref, reactive, computed, watch, nextTick } from 'vue';
import {
  state, toast, openCatalog, saveCatalogFile, loadCatalogFile, refreshSavedQueries, refreshCompressions,
  defineCompressionRule, dropCompressionRule, recompressRule, estimateCompressionRule, dropSavedQuery,
  refreshIndexes, createPropertyIndex, dropPropertyIndex,
  refreshJobs, defineUserJob, dropUserJob, switchJob, scheduleSystemJob, runJobNow, isServerMode,
} from '../store.js';
import {
  jobsMenuItem, jobRows, newJobDraft, draftFromJob, jobDraftToSpec, queryChoices, argsSkeleton,
  JOB_ACTIONS, SCHEDULE_PRESETS, scheduleText, formErrors,
} from '../jobs.js';
import {
  ruleRow, totals, newDraft, draftFromRule, draftToSpec, labelSuggestions,
  canEstimate, draftToEstimateSpec, estimateView, estimateSummary, histogramRows, applyCandidate,
  CODECS, ESTIMATE_CODECS, withCodec,
} from '../compression.js';
import {
  queryTree, signatureText, errorsByField, accessBadge, languageBadge, draftFromScript, draftFromGql,
} from '../saved-queries.js';
import { paramArgs } from '../gql-tab.js';
import {
  catalogMenuItems, indexRow, indexTotals, newTransformIndexDraft, indexDraftToSpec, defaultIndexName,
  INDEX_TYPES, INDEX_TRANSFORMS, indexTypeOf, indexTransformOf, indexNotServedReason,
} from '../indexes.js';
import { formatBytes } from '../statistics.js';
import { DropMenu } from './drop-menu.js';
import { SavedQueryDialog } from './saved-queries.js';

const message = (err) => (err && err.message ? err.message : String(err));

/** The top bar's "Catalog ▾" dropdown. */
export const CatalogMenu = {
  components: { DropMenu },
  setup() {
    const fileInput = ref(null);
    const items = computed(() => [...catalogMenuItems(state.graph), jobsMenuItem(state.graph, state.jobs)]);
    // The jobs' count (active/all) follows the graph: a commit may define, drop or switch one.
    watch(() => state.graph, () => refreshJobs(), { immediate: true });
    function pickFile(close) {
      close();
      if (fileInput.value) fileInput.value.click();
    }
    async function onFile(e) {
      const file = e.target.files && e.target.files[0];
      e.target.value = '';
      if (file) await loadCatalogFile(file);
    }
    return { state, items, fileInput, pickFile, onFile, openCatalog, saveCatalogFile };
  },
  template: `
  <drop-menu button-class="btn" title="The catalog: saved queries, compression rules and property indexes stored in the graph" menu-style="min-width: 280px">
    <template #trigger>Catalog ▾</template>
    <template #default="{ close }">
      <div class="menu-title">Stored in the graph</div>
      <button v-for="item in items" :key="item.id" class="menu-item" :title="item.hint" @click="openCatalog(item.id)">
        {{ item.title }}<span class="spacer"></span><span class="badge">{{ item.count }}</span>
      </button>
      <div class="menu-sep"></div>
      <div class="menu-title">Catalog file (no data inside)</div>
      <button class="menu-item" :disabled="state.saving || state.running" @click="saveCatalogFile()"
              title="Every definition of the catalog (saved queries, compression rules, property indexes) as one JSON file (graphersal-catalog-YYYYMMDD-HHMMSS.json)">Save catalog (.json)</button>
      <button class="menu-item" :disabled="state.loadingGraph || state.running" @click.stop="pickFile(close)"
              title="Store the definitions of a catalog file in this graph: one of the same kind and name is replaced; a compression rule compresses its values at once">Load catalog…</button>
    </template>
  </drop-menu>
  <input ref="fileInput" type="file" accept=".json,application/json" hidden @change="onFile">
  `,
};

/** The Saved queries manager: the list by folder with Run, Edit, Delete and New. */
export const SavedQueriesManager = {
  components: { SavedQueryDialog },
  setup(props, { expose }) {
    const dlg = ref(null);
    const editor = ref(null);
    const tree = computed(() => queryTree(state.savedQueries || []));
    const count = computed(() => (state.savedQueries || []).length);
    async function open() {
      await refreshSavedQueries();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function close() {
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    // The editor and run form are a dialog of their own: this one closes first and reopens when
    // that one closes (after a Save, Cancel or Delete), except after a Run.
    function openNew() { close(); editor.value.openNew('', { back: open }); }
    function openEdit(q) { close(); editor.value.openEdit(q.name, { back: open }); }
    function openRun(q) { close(); editor.value.openRun(q.name, { back: open }); }
    /** "Save as query…" of a query tab: the editor on a draft of its text; the list after Save. */
    async function openFromScript(script) {
      await refreshSavedQueries();
      close();
      // A GQL query tab hands `{language: 'gql', query, params}` (store.js saveTabAsQuery).
      const draft = script && typeof script === 'object' && script.language === 'gql'
        ? draftFromGql(script.query, paramArgs(script.params))
        : draftFromScript(script);
      await editor.value.openDraft(draft, { back: open });
    }
    async function remove(q) {
      if (!window.confirm(`Delete the saved query "${q.name}"? Scripts that call it fail afterwards.`)) return;
      try {
        await dropSavedQuery(q.name);
        toast(`Saved query "${q.name}" deleted.`, 'info', 4000);
      } catch (err) {
        toast(`Delete failed: ${message(err)}`, 'error', 9000);
      }
    }
    const tooltip = (q) => [signatureText(q), q.description, q.status === 'error' ? `Fails: ${q.error}` : null]
      .filter(Boolean).join('\n');
    expose({ open, close, openFromScript });
    return {
      state, dlg, editor, tree, count, close, openNew, openEdit, openRun, remove, tooltip, accessBadge, languageBadge,
    };
  },
  template: `
  <dialog ref="dlg" class="catalog-dialog" aria-labelledby="cq-title">
    <div class="dialog-head">
      <h2 id="cq-title">Saved queries</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <p class="muted sq-note">Named, parameterized queries stored in the graph, written in Rhai or in GQL; call one with g.query("name", #{…}) or Run it here. Save as query… in a query tab's ⋯ menu starts one from the tab. R/O queries only read; an R/W query changes data, only what its body writes (inferred when it is saved), and Run asks first. They travel with the catalog file (Catalog ▸ Save catalog) and with a snapshot.</p>
      <div v-if="state.savedQueriesError" class="field-error">{{ state.savedQueriesError }}</div>
      <p v-else-if="!count" class="muted">No saved queries yet.</p>
      <template v-for="grp in tree" :key="grp.folder">
        <h3 v-if="grp.folder">{{ grp.folder }}</h3>
        <div v-for="q in grp.queries" :key="q.name" class="catalog-row">
          <span class="code" :title="tooltip(q)">{{ q.name }}</span>
          <span :class="'badge ' + accessBadge(q).kind" :title="accessBadge(q).title">{{ accessBadge(q).text }}</span>
          <span class="badge" :title="languageBadge(q).title">{{ languageBadge(q).text }}</span>
          <span v-if="q.description" class="faint sq-desc">{{ q.description }}</span>
          <span class="spacer"></span>
          <button type="button" class="btn small" @click="openRun(q)">Run…</button>
          <button type="button" class="btn small" @click="openEdit(q)">Edit…</button>
          <button type="button" class="btn small danger" @click="remove(q)" :aria-label="'Delete ' + q.name">Delete</button>
        </div>
      </template>
      <div class="sq-buttons">
        <button type="button" class="btn primary" @click="openNew">New saved query…</button>
        <span class="spacer"></span>
        <button type="button" class="btn" @click="close">Close</button>
      </div>
    </div>
  </dialog>
  <saved-query-dialog ref="editor"></saved-query-dialog>
  `,
};

/** The Compression manager: the rules with their stats, and the rule form. */
export const CompressionManager = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const d = reactive({
      draft: null, // the form's draft (null: the list)
      errors: {}, // field -> [messages]
      busy: false,
      labels: { vertex: [], edge: [] },
      estimate: null, // the engine's estimate for the form's element, label and path
      estimating: false,
      estimateErrors: [],
    });
    const rows = computed(() => (state.compressions || []).map((rule) => ({ rule, row: ruleRow(rule) })));
    const sum = computed(() => totals(state.compressions || []));
    const suggestions = computed(() => (d.draft ? d.labels[d.draft.element] || [] : []));
    const estimateTable = computed(() => estimateView(d.estimate));
    const estimateText = computed(() => estimateSummary(d.estimate));
    const histogram = computed(() => histogramRows(d.estimate));
    const estimable = computed(() => canEstimate(d.draft));
    // An estimate belongs to the element, label and path it measured: a change drops it.
    watch(
      () => (d.draft ? [d.draft.element, d.draft.label, d.draft.path].join('\u0000') : ''),
      () => clearEstimate(),
    );

    function clearEstimate() {
      Object.assign(d, { estimate: null, estimateErrors: [] });
    }
    async function estimate() {
      if (d.estimating || !d.draft) return;
      const local = draftToEstimateSpec(d.draft);
      if (local.errors) {
        d.errors = errorsByField(local.errors);
        return;
      }
      Object.assign(d, { estimating: true, estimateErrors: [] });
      try {
        // lz4 and dedup side by side (the form's codec is chosen from the table's "Use").
        const res = await estimateCompressionRule({ ...local.spec, codec: [...ESTIMATE_CODECS] });
        if (res && res.ok) {
          d.estimate = res.estimate;
        } else {
          const errors = (res && res.errors) || [{ field: 'general', message: 'The engine refused it.' }];
          d.estimate = null;
          d.estimateErrors = errors.map((e) => e.message);
        }
      } catch (err) {
        Object.assign(d, { estimate: null, estimateErrors: [message(err)] });
      } finally {
        d.estimating = false;
      }
    }
    function useCandidate(candidate) {
      if (d.draft) Object.assign(d.draft, applyCandidate(d.draft, candidate));
    }
    function setCodec(codec) {
      if (d.draft) Object.assign(d.draft, withCodec(d.draft, codec));
    }

    async function loadLabels() {
      try {
        const stats = await state.engine.statistics();
        d.labels = { vertex: labelSuggestions(stats, 'vertex'), edge: labelSuggestions(stats, 'edge') };
      } catch (err) {
        d.labels = { vertex: [], edge: [] };
      }
    }
    async function open() {
      Object.assign(d, { draft: null, errors: {}, busy: false });
      await refreshCompressions();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function close() {
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    async function startNew() {
      Object.assign(d, { draft: newDraft(), errors: {} });
      clearEstimate();
      await loadLabels();
      await nextTick();
    }
    async function startEdit(rule) {
      Object.assign(d, { draft: draftFromRule(rule), errors: {} });
      clearEstimate();
      await loadLabels();
    }
    function cancelForm() {
      Object.assign(d, { draft: null, errors: {} });
      clearEstimate();
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
        const res = await defineCompressionRule(local.spec);
        if (res && res.ok) {
          const r = res.rule && res.rule.stats;
          toast(`Compression rule "${local.spec.name}" saved${r ? `: ${r.compressedValues} value${r.compressedValues === 1 ? '' : 's'} compressed` : ''}.`, 'info', 5000);
          cancelForm();
        } else {
          d.errors = errorsByField((res && res.errors) || [{ field: 'general', message: 'The engine refused it.' }]);
        }
      } catch (err) {
        d.errors = { general: [message(err)] };
      } finally {
        d.busy = false;
      }
    }
    async function recompress(rule) {
      if (d.busy) return;
      d.busy = true;
      try {
        const entry = await recompressRule(rule.name);
        const s = entry && entry.stats;
        toast(`Recompressed "${rule.name}"${s ? `: ${s.compressedValues} values, ${formatBytes(s.storedBytes)} stored` : ''}.`, 'info', 5000);
      } catch (err) {
        toast(`Recompress failed: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
      }
    }
    async function drop(rule) {
      if (d.busy) return;
      if (!window.confirm(`Drop the compression rule "${rule.name}"? Its values are decompressed; no data changes.`)) return;
      d.busy = true;
      try {
        await dropCompressionRule(rule.name);
        toast(`Compression rule "${rule.name}" dropped.`, 'info', 4000);
      } catch (err) {
        toast(`Drop failed: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
      }
    }
    expose({ open, close });
    return {
      state, dlg, d, rows, sum, suggestions, formatBytes, close, startNew, startEdit, cancelForm, save, recompress, drop,
      estimateTable, estimateText, histogram, estimable, estimate, useCandidate, setCodec, codecs: CODECS,
    };
  },
  template: `
  <dialog ref="dlg" class="catalog-dialog wide" aria-labelledby="cc-title">
    <div class="dialog-head">
      <h2 id="cc-title">Compression</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <template v-if="!d.draft">
        <p class="muted sq-note">A rule keeps the strings of one vertex or edge label at one property path LZ4-compressed in memory (codec lz4: when at least the minimum size and at most 90 % after compression; a read or a has() on such a field decompresses it), or stores each distinct value once and lets every element point to it (codec dedup: long values repeated across many elements, such as paths, URLs or category texts; strings of up to 24 bytes are stored inline and never shared). Reads are always plain. Files hold plain text. Rules travel with the catalog file and with a snapshot.</p>
        <div v-if="state.compressionsError" class="field-error">{{ state.compressionsError }}</div>
        <p v-else-if="!rows.length" class="muted">No compression rules yet. Large text that queries rarely read (a CV, a description, a document body) is the use case.</p>
        <div v-else class="catalog-table-wrap">
          <table class="data-table catalog-table">
            <thead><tr>
              <th>Name</th><th>Codec</th><th>Applies to</th><th>Path</th><th>Min. size</th><th>Dictionary</th>
              <th class="num" title="dedup: also the distinct texts the shared values hold">Values</th><th>Plain → stored</th><th class="num">Ratio</th><th class="num">Saved</th><th></th>
            </tr></thead>
            <tbody>
              <tr v-for="{ rule, row } in rows" :key="rule.name">
                <td class="code">{{ row.name }} <span v-if="!row.served" class="badge" :title="row.reason">not applied</span></td><td>{{ row.codec }}<div v-if="!row.served && row.reason" class="muted cc-reason">{{ row.reason }}</div></td><td>{{ row.target }}</td><td class="code">{{ row.path }}</td>
                <td>{{ row.threshold }}</td><td>{{ row.dictionary }}</td><td class="num">{{ row.values }}<div v-if="row.distinct !== '—'" class="muted cc-sub">{{ row.distinct }} distinct</div></td>
                <td>{{ row.sizes }}</td><td class="num">{{ row.ratio }}</td><td class="num">{{ row.saved }}</td>
                <td class="catalog-actions">
                  <button type="button" class="btn small" :disabled="d.busy || state.running || !row.served" @click="startEdit(rule)">Edit…</button>
                  <button type="button" class="btn small" :disabled="d.busy || state.running || !row.served" @click="recompress(rule)"
                          title="Rebuild the dictionary from the current values and re-encode them (after much data was added; a dedup rule drops the texts no element uses any more); changes no data">Recompress</button>
                  <button type="button" class="btn small danger" :disabled="d.busy || state.running" @click="drop(rule)">Drop</button>
                </td>
              </tr>
            </tbody>
          </table>
          <p class="muted sq-note">Together: {{ sum.compressedValues }} values, {{ formatBytes(sum.plainBytes) }} plain stored as {{ formatBytes(sum.storedBytes) }}, dictionaries {{ formatBytes(sum.dictionaryBytes) }}.</p>
        </div>
        <div class="sq-buttons">
          <button type="button" class="btn primary" :disabled="state.running" @click="startNew">New rule…</button>
          <span class="spacer"></span>
          <button type="button" class="btn" @click="close">Close</button>
        </div>
      </template>

      <form v-else @submit.prevent="save">
        <h3>{{ d.draft.replaces ? 'Edit rule "' + d.draft.replaces + '"' : 'New rule' }}</h3>
        <div class="form-grid">
          <label for="cc-name">Name</label>
          <input id="cc-name" type="text" v-model="d.draft.name" :class="{ invalid: d.errors.name }" placeholder="customer_cv" autocomplete="off" spellcheck="false">
          <div v-for="m in d.errors.name || []" class="field-error sq-span">{{ m }}</div>
          <label for="cc-element">Element</label>
          <select id="cc-element" v-model="d.draft.element" :class="{ invalid: d.errors.element }" style="justify-self:start">
            <option value="vertex">vertex</option><option value="edge">edge</option>
          </select>
          <div v-for="m in d.errors.element || []" class="field-error sq-span">{{ m }}</div>
          <label for="cc-label">Label</label>
          <input id="cc-label" type="text" list="cc-labels" v-model="d.draft.label" :class="{ invalid: d.errors.label }" placeholder="customer" autocomplete="off" spellcheck="false">
          <datalist id="cc-labels"><option v-for="l in suggestions" :key="l" :value="l"></option></datalist>
          <div v-for="m in d.errors.label || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">The label need not exist yet; the graph's {{ d.draft.element }} labels are suggested.</span>
          <label for="cc-path">Path</label>
          <input id="cc-path" type="text" v-model="d.draft.path" :class="{ invalid: d.errors.path }" placeholder="details.career.cv" autocomplete="off" spellcheck="false">
          <div v-for="m in d.errors.path || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">A property name or a path of names (cv, details.career.cv); no index such as [0].</span>
          <label for="cc-codec">Codec</label>
          <select id="cc-codec" :value="d.draft.codec || 'lz4'" @change="setCodec($event.target.value)" :class="{ invalid: d.errors.codec }" style="justify-self:start">
            <option v-for="c in codecs" :key="c" :value="c">{{ c }}</option>
          </select>
          <span class="hint">{{ (d.draft.codec || 'lz4') === 'dedup' ? 'Each distinct value stored once, every element points to it: for long values repeated across many elements. Reads cost nothing extra.' : 'Each value LZ4-compressed: for large text that queries rarely read.' }}</span>
          <label for="cc-min">Minimum size</label>
          <input id="cc-min" type="text" inputmode="numeric" v-model="d.draft.minBytesText" :class="{ invalid: d.errors.minBytes }" style="width:14ch;justify-self:start">
          <div v-for="m in d.errors.minBytes || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">Bytes (UTF-8); shorter strings stay plain.</span>
          <label for="cc-dict">Dictionary</label>
          <span><input id="cc-dict" type="checkbox" v-model="d.draft.dictionary" :disabled="d.draft.codec === 'dedup'"> <label for="cc-dict" class="muted">{{ d.draft.codec === 'dedup' ? 'lz4 only (a dedup rule has none)' : "built from a sample of the rule's values (better ratio for many similar texts)" }}</label></span>
          <div v-for="m in d.errors.dictionary || []" class="field-error sq-span">{{ m }}</div>
        </div>
        <div class="cc-estimate">
          <div class="cc-estimate-head">
            <button type="button" class="btn" :disabled="!estimable || d.estimating || d.busy || state.running" @click="estimate"
                    title="Measure lz4 with and without a dictionary and dedup at several minimum sizes on the current values of this label and path; changes nothing">{{ d.estimating ? 'Estimating…' : 'Estimate' }}</button>
            <span class="muted">{{ estimable ? 'What would a rule here save? Measured on the current data; the graph is not changed.' : 'Fill in the label and the path to estimate what a rule would save.' }}</span>
          </div>
          <div v-for="m in d.estimateErrors" class="field-error">{{ m }}</div>
          <template v-if="d.estimate">
            <p class="muted sq-note">{{ estimateText.found }}</p>
            <p v-for="n in estimateText.notes" :key="n" class="muted sq-note">{{ n }}</p>
            <div v-for="w in estimateText.warnings" :key="w" class="field-error">{{ w }}</div>
            <div v-if="estimateTable.rows.length" class="catalog-table-wrap">
              <table class="data-table catalog-table cc-estimate-table">
                <thead><tr>
                  <th>Codec</th><th>Min. size</th><th class="num" title="dedup: also the distinct texts among the values">Values</th><th>Plain → stored</th><th>Dictionary</th>
                  <th class="num">Ratio</th><th>Saved</th><th title="Decompression speed: every read of a compressed value pays it (a shared dedup value reads as plain)">Reads</th><th></th>
                </tr></thead>
                <tbody>
                  <tr v-for="r in estimateTable.rows" :key="r.index" :class="{ 'cc-best': r.best }">
                    <td>{{ r.name }} <span v-if="r.best" class="cc-tag">best</span> <span v-if="r.current" class="cc-tag">current</span></td>
                    <td>{{ r.threshold }}</td><td class="num">{{ r.values }}<div v-if="r.distinct !== '—'" class="muted cc-sub">{{ r.distinct }} distinct</div></td><td>{{ r.sizes }}</td><td>{{ r.dictionary }}</td>
                    <td class="num">{{ r.ratio }}</td>
                    <td><span class="cc-bar" aria-hidden="true"><span class="cc-bar-fill" :style="{ width: (100 * r.share).toFixed(1) + '%' }"></span></span> {{ r.saved }}</td>
                    <td>{{ r.reads }}</td>
                    <td><button type="button" class="btn small" @click="useCandidate(r.candidate)"
                                :aria-label="'Use ' + r.name + ' at ' + r.threshold">Use</button></td>
                  </tr>
                </tbody>
              </table>
            </div>
            <p v-if="estimateTable.empty" class="muted sq-note">{{ estimateTable.empty }}</p>
            <p v-if="histogram.length" class="muted sq-note">String sizes: {{ histogram.map((h) => h.range + ': ' + h.values).join(' · ') }}</p>
          </template>
        </div>
        <div v-for="m in [...(d.errors.general || []), ...(d.errors.codec || [])]" class="field-error">{{ m }}</div>
        <p class="muted sq-note">Saving re-encodes every value of the label at once (one commit); no data changes.</p>
        <div class="sq-buttons">
          <span class="spacer"></span>
          <button type="button" class="btn" @click="cancelForm">Back</button>
          <button type="submit" class="btn primary" :disabled="d.busy || state.running">{{ d.busy ? 'Saving…' : 'Save' }}</button>
        </div>
      </form>
    </div>
  </dialog>
  `,
};

/** The Property indexes manager: the indexes with their stats, and the create form. */
export const IndexesManager = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const d = reactive({
      draft: null, // the form's draft (null: the list)
      errors: {}, // field -> [messages]
      busy: false,
      labels: { vertex: [], edge: [] },
    });
    const rows = computed(() => (state.indexes || []).map((index) => ({
      index, row: indexRow(index), type: indexTypeOf(index), transform: indexTransformOf(index),
      reason: indexNotServedReason(index),
    })));
    const transformHint = computed(() => {
      const t = d.draft && INDEX_TRANSFORMS.find((x) => x.name === (d.draft.transform || ''));
      return t ? t.description : '';
    });
    const typeHint = computed(() => {
      const t = d.draft && INDEX_TYPES.find((x) => x.name === d.draft.type);
      return t ? t.description : '';
    });
    const sum = computed(() => indexTotals(state.indexes || []));
    const suggestions = computed(() => (d.draft ? d.labels[d.draft.element] || [] : []));
    const namePlaceholder = computed(() => {
      const x = d.draft;
      return x && x.label.trim() && x.key.trim()
        ? defaultIndexName(x.element, x.label.trim(), x.key.trim(), x.transform)
        : defaultIndexName('vertex', 'person', 'age', x && x.transform);
    });

    async function loadLabels() {
      try {
        const stats = await state.engine.statistics();
        d.labels = { vertex: labelSuggestions(stats, 'vertex'), edge: labelSuggestions(stats, 'edge') };
      } catch (err) {
        d.labels = { vertex: [], edge: [] };
      }
    }
    async function open() {
      Object.assign(d, { draft: null, errors: {}, busy: false });
      await refreshIndexes();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function close() {
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    async function startNew() {
      Object.assign(d, { draft: newTransformIndexDraft(), errors: {} });
      await loadLabels();
      await nextTick();
    }
    function cancelForm() {
      Object.assign(d, { draft: null, errors: {} });
    }
    async function save() {
      if (d.busy || !d.draft) return;
      const local = indexDraftToSpec(d.draft);
      if (local.errors) {
        d.errors = errorsByField(local.errors);
        return;
      }
      d.busy = true;
      try {
        const res = await createPropertyIndex(local.spec);
        if (res && res.ok) {
          const i = res.index || {};
          const s = i.stats;
          toast(`Property index "${i.name}" created${s ? `: ${s.entries} entr${s.entries === 1 ? 'y' : 'ies'}, ${formatBytes(s.bytes)}` : ''}.`, 'info', 5000);
          cancelForm();
        } else {
          d.errors = errorsByField((res && res.errors) || [{ field: 'general', message: 'The engine refused it.' }]);
        }
      } catch (err) {
        d.errors = { general: [message(err)] };
      } finally {
        d.busy = false;
      }
    }
    async function drop(index) {
      if (d.busy) return;
      if (!window.confirm(`Drop the property index "${index.name}"? No data changes; queries on ${index.label}.${index.key} scan again.`)) return;
      d.busy = true;
      try {
        await dropPropertyIndex(index.name);
        toast(`Property index "${index.name}" dropped.`, 'info', 4000);
      } catch (err) {
        toast(`Drop failed: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
      }
    }
    expose({ open, close });
    return {
      state, dlg, d, rows, sum, suggestions, namePlaceholder, typeHint, transformHint,
      indexTypes: INDEX_TYPES, indexTransforms: INDEX_TRANSFORMS, formatBytes, close,
      startNew, cancelForm, save, drop,
    };
  },
  template: `
  <dialog ref="dlg" class="catalog-dialog wide" aria-labelledby="ci-title">
    <div class="dialog-head">
      <h2 id="ci-title">Property indexes</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <template v-if="!d.draft">
        <p class="muted sq-note">An index orders the vertices (or edges) of one label by one top-level property, so a query such as g.v().has_label("person").has("age", P.gt(30)) reads only the matching elements: equality, P.within and ranges are served. Results never change (only their order may); .profile() names the index it reads. An index costs memory and a little time on every write of its key. Indexes travel with the catalog file and with a snapshot; their entries are rebuilt from the data on every load.</p>
        <div v-if="state.indexesError" class="field-error">{{ state.indexesError }}</div>
        <p v-else-if="!rows.length" class="muted">No property indexes yet. A label with many elements filtered on one property is the use case.</p>
        <div v-else class="catalog-table-wrap">
          <table class="data-table catalog-table">
            <thead><tr>
              <th>Name</th><th>Applies to</th><th>Key</th><th>Type</th><th>Transform</th>
              <th class="num">Entries</th><th class="num">Distinct values</th><th class="num">Per value</th><th class="num">Memory</th><th></th>
            </tr></thead>
            <tbody>
              <tr v-for="{ index, row, type, transform, reason } in rows" :key="index.name">
                <td class="code">{{ row.name }} <span v-if="!row.served" class="badge" :title="reason">not served</span></td>
                <td>{{ row.target }}</td><td class="code">{{ row.key }}</td>
                <td class="code">{{ type }}<div v-if="!row.served && reason" class="muted" style="font-size: 12px">{{ reason }}</div></td>
                <td class="code">{{ transform || '—' }}</td>
                <td class="num">{{ row.entries }}</td><td class="num">{{ row.distinct }}</td>
                <td class="num" title="Elements an equality reads on average">{{ row.perValue }}</td><td class="num">{{ row.bytes }}</td>
                <td class="catalog-actions">
                  <button type="button" class="btn small danger" :disabled="d.busy || state.running" @click="drop(index)">Drop</button>
                </td>
              </tr>
            </tbody>
          </table>
          <p class="muted sq-note">Together: {{ sum.entries }} entries, {{ formatBytes(sum.bytes) }}<template v-if="sum.notServed">; {{ sum.notServed }} not served by this storage</template>.</p>
        </div>
        <div class="sq-buttons">
          <button type="button" class="btn primary" :disabled="state.running" @click="startNew">New index…</button>
          <span class="spacer"></span>
          <button type="button" class="btn" @click="close">Close</button>
        </div>
      </template>

      <form v-else @submit.prevent="save">
        <h3>New index</h3>
        <div class="form-grid">
          <label for="ci-element">Element</label>
          <select id="ci-element" v-model="d.draft.element" :class="{ invalid: d.errors.element }" style="justify-self:start">
            <option value="vertex">vertex</option><option value="edge">edge</option>
          </select>
          <div v-for="m in d.errors.element || []" class="field-error sq-span">{{ m }}</div>
          <label for="ci-label">Label</label>
          <input id="ci-label" type="text" list="ci-labels" v-model="d.draft.label" :class="{ invalid: d.errors.label }" placeholder="person" autocomplete="off" spellcheck="false">
          <datalist id="ci-labels"><option v-for="l in suggestions" :key="l" :value="l"></option></datalist>
          <div v-for="m in d.errors.label || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">The label need not exist yet; the graph's {{ d.draft.element }} labels are suggested. Queries use the index after has_label() of this label.</span>
          <label for="ci-key">Key</label>
          <input id="ci-key" type="text" v-model="d.draft.key" :class="{ invalid: d.errors.key }" placeholder="age" autocomplete="off" spellcheck="false">
          <div v-for="m in d.errors.key || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">A top-level property name (not a jpath); lists and maps as values are not indexed. A key the schema declares as a vector, a list or a map is refused.</span>
          <label for="ci-type">Type</label>
          <select id="ci-type" v-model="d.draft.type" :class="{ invalid: d.errors.type }" :title="typeHint" style="justify-self:start">
            <option v-for="t in indexTypes" :key="t.name" :value="t.name" :title="t.description">{{ t.name }}</option>
          </select>
          <div v-for="m in d.errors.type || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">{{ typeHint }}</span>
          <label for="ci-transform">Transform</label>
          <select id="ci-transform" v-model="d.draft.transform" :class="{ invalid: d.errors.transform }" :title="transformHint" style="justify-self:start">
            <option v-for="t in indexTransforms" :key="t.name" :value="t.name" :title="t.description">{{ t.label }}</option>
          </select>
          <div v-for="m in d.errors.transform || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">{{ transformHint }}</span>
          <label for="ci-name">Name</label>
          <input id="ci-name" type="text" v-model="d.draft.name" :class="{ invalid: d.errors.name }" :placeholder="namePlaceholder" autocomplete="off" spellcheck="false">
          <div v-for="m in d.errors.name || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">Optional; without one the index is called {{ namePlaceholder }}.</span>
        </div>
        <div v-for="m in d.errors.general || []" class="field-error">{{ m }}</div>
        <p class="muted sq-note">Creating builds the index from the data at once (one commit); no data changes.</p>
        <div class="sq-buttons">
          <span class="spacer"></span>
          <button type="button" class="btn" @click="cancelForm">Back</button>
          <button type="submit" class="btn primary" :disabled="d.busy || state.running">{{ d.busy ? 'Creating…' : 'Create' }}</button>
        </div>
      </form>
    </div>
  </dialog>
  `,
};

/** The Scheduled jobs manager: every job with its runs and switch, and the define form. */
export const JobsManager = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const d = reactive({ draft: null, errors: {}, busy: false, now: Date.now() * 1000 });
    const rows = computed(() => {
      const view = state.jobs;
      return ((view && view.jobs) || []).map((job, i) => ({ job, row: jobRows({ jobs: [job] }, d.now)[0], i }));
    });
    const view = computed(() => state.jobs || { jobs: [] });
    const queries = computed(() => queryChoices(state.savedQueries));
    const serverMode = computed(isServerMode);
    let timer = null;

    async function open() {
      Object.assign(d, { draft: null, errors: {}, busy: false, now: Date.now() * 1000 });
      await refreshJobs();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      if (!timer) {
        timer = setInterval(async () => {
          if (!dlg.value || !dlg.value.open) {
            clearInterval(timer);
            timer = null;
            return;
          }
          d.now = Date.now() * 1000;
          if (!d.draft && !d.busy) await refreshJobs();
        }, 5000);
      }
    }
    function close() {
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    async function startNew() {
      await refreshSavedQueries();
      Object.assign(d, { draft: newJobDraft(), errors: {} });
      await nextTick();
    }
    async function startEdit(job) {
      await refreshSavedQueries();
      Object.assign(d, { draft: draftFromJob(job), errors: {} });
    }
    function cancelForm() {
      Object.assign(d, { draft: null, errors: {} });
    }
    function pickQuery() {
      const q = queries.value.find((x) => x.name === d.draft.query);
      const text = d.draft.argsText.trim();
      if (q && (!text || text === '{}')) d.draft.argsText = q.params.length ? argsSkeleton(q.params) : '{}';
    }
    async function save() {
      if (d.busy || !d.draft) return;
      const local = jobDraftToSpec(d.draft);
      if (local.errors) {
        d.errors = errorsByField(local.errors);
        return;
      }
      d.busy = true;
      try {
        const replaces = d.draft.replaces;
        const res = await defineUserJob(local.spec);
        if (res && res.ok) {
          // A rename stores the new job and drops the old one.
          if (replaces && replaces !== local.spec.name) await dropUserJob(replaces);
          toast(`Job "${local.spec.name}" saved: ${scheduleText(local.spec.schedule)}.`, 'info', 5000);
          cancelForm();
        } else {
          d.errors = errorsByField(formErrors((res && res.errors) || [{ field: 'general', message: 'The engine refused it.' }]));
        }
      } catch (err) {
        d.errors = { general: [message(err)] };
      } finally {
        d.busy = false;
      }
    }
    async function toggle(job) {
      if (d.busy) return;
      d.busy = true;
      try {
        await switchJob(job.name, !job.enabled);
      } catch (err) {
        toast(`Switch failed: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
      }
    }
    async function run(job) {
      if (d.busy) return;
      d.busy = true;
      try {
        const entry = await runJobNow(job.name);
        const error = entry && entry.last && entry.last.error;
        if (error) toast(`Job "${job.name}" failed: ${error}`, 'error', 9000);
        else toast(`Job "${job.name}" ran.`, 'info', 4000);
      } catch (err) {
        toast(`Run failed: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
        d.now = Date.now() * 1000;
      }
    }
    async function drop(job) {
      if (d.busy) return;
      if (!window.confirm(`Drop the job "${job.name}"? It stops running; no data changes.`)) return;
      d.busy = true;
      try {
        await dropUserJob(job.name);
        toast(`Job "${job.name}" dropped.`, 'info', 4000);
      } catch (err) {
        toast(`Drop failed: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
      }
    }
    async function reschedule(job) {
      if (d.busy) return;
      const text = window.prompt(`Schedule of the system job "${job.name}" (5-field cron, e.g. 0 3 * * *). Empty: its built-in schedule.`, job.schedule);
      if (text === null) return;
      d.busy = true;
      try {
        await scheduleSystemJob(job.name, text.trim() || null);
      } catch (err) {
        toast(`Schedule refused: ${message(err)}`, 'error', 9000);
      } finally {
        d.busy = false;
      }
    }
    expose({ open, close });
    return {
      state, dlg, d, rows, view, queries, serverMode, close, startNew, startEdit, cancelForm, save,
      toggle, run, drop, reschedule, pickQuery, actions: JOB_ACTIONS, presets: SCHEDULE_PRESETS, scheduleText,
    };
  },
  template: `
  <dialog ref="dlg" class="catalog-dialog wide" aria-labelledby="cj-title">
    <div class="dialog-head">
      <h2 id="cj-title">Scheduled jobs</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <template v-if="!d.draft">
        <p class="muted sq-note">Jobs run on a cron schedule ({{ view.timeZone || 'UTC' }}) in the store: the system jobs, and jobs you define (a saved query with fixed arguments, a checkpoint, a backup). A job runs with the rights it was defined with; a job of yours is part of the catalog and travels with the catalog file and backups.</p>
        <p v-if="state.jobs && !view.store" class="field-error">The graph is not in a store: jobs are kept but do not run.</p>
        <div v-if="state.jobsError" class="field-error">{{ state.jobsError }}</div>
        <div v-else-if="rows.length" class="catalog-table-wrap">
          <table class="data-table catalog-table">
            <thead><tr>
              <th>Name</th><th>Does</th><th>Schedule</th><th>On</th><th>Next</th><th>Last run</th>
              <th class="num" title="Runs in the last two days (failed)">Runs (2 days)</th><th></th>
            </tr></thead>
            <tbody>
              <tr v-for="{ job, row } in rows" :key="row.name">
                <td class="code">{{ row.name }} <span v-if="row.system" class="badge" title="Comes with the store; can be switched and rescheduled, not dropped">system</span>
                  <span v-if="!row.scheduled" class="badge" :title="row.reason">not running</span>
                  <div v-if="row.description" class="muted cc-sub">{{ row.description }}</div></td>
                <td>{{ row.action }}<div v-if="row.effects.length" class="muted cc-sub" title="What each run may write: the rights stored when the job was defined">may: {{ row.effects.join('; ') }}</div></td>
                <td :title="row.schedule"><span class="code">{{ row.schedule }}</span><div class="muted cc-sub">{{ row.scheduleText !== row.schedule ? row.scheduleText : '' }}{{ row.changed ? ' (changed)' : '' }}</div></td>
                <td><input type="checkbox" :checked="row.enabled" :disabled="d.busy || state.running" :aria-label="(row.enabled ? 'Switch off ' : 'Switch on ') + row.name" @change="toggle(job)"></td>
                <td>{{ row.next }}</td>
                <td>{{ row.last }}<span v-if="row.lastMs != null" class="muted"> · {{ row.lastMs }} ms</span>
                  <div v-if="row.lastError" class="field-error cc-sub">{{ row.lastError }}</div></td>
                <td class="num">{{ row.runs }}<span v-if="row.failed" class="field-error"> ({{ row.failed }})</span></td>
                <td class="catalog-actions">
                  <button type="button" class="btn small" :disabled="d.busy || state.running || !row.scheduled" @click="run(job)" title="Run it now, whatever its schedule">Run now</button>
                  <button v-if="row.system" type="button" class="btn small" :disabled="d.busy || state.running" @click="reschedule(job)">Schedule…</button>
                  <button v-if="!row.system" type="button" class="btn small" :disabled="d.busy || state.running" @click="startEdit(job)">Edit…</button>
                  <button v-if="!row.system" type="button" class="btn small danger" :disabled="d.busy || state.running" @click="drop(job)">Drop</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-else class="muted">No jobs.</p>
        <div class="sq-buttons">
          <button type="button" class="btn primary" :disabled="state.running" @click="startNew">New job…</button>
          <span class="spacer"></span>
          <button type="button" class="btn" @click="close">Close</button>
        </div>
      </template>

      <form v-else @submit.prevent="save">
        <h3>{{ d.draft.replaces ? 'Edit job "' + d.draft.replaces + '"' : 'New job' }}</h3>
        <div class="form-grid">
          <label for="cj-name">Name</label>
          <input id="cj-name" type="text" v-model="d.draft.name" :class="{ invalid: d.errors.name }" placeholder="nightly_cleanup" autocomplete="off" spellcheck="false">
          <div v-for="m in d.errors.name || []" class="field-error sq-span">{{ m }}</div>
          <label for="cj-action">Does</label>
          <select id="cj-action" v-model="d.draft.action" :class="{ invalid: d.errors.action }" style="justify-self:start">
            <option v-for="a in actions" :key="a.name" :value="a.name" :title="a.hint">{{ a.label }}</option>
          </select>
          <div v-for="m in d.errors.action || []" class="field-error sq-span">{{ m }}</div>
          <template v-if="d.draft.action === 'query'">
            <label for="cj-query">Saved query</label>
            <select id="cj-query" v-model="d.draft.query" :class="{ invalid: d.errors.query }" style="justify-self:start" @change="pickQuery">
              <option value="" disabled>Choose a saved query</option>
              <option v-for="q in queries" :key="q.name" :value="q.name">{{ q.name }}{{ q.params.length ? '(' + q.params.join(', ') + ')' : '()' }}</option>
            </select>
            <div v-for="m in d.errors.query || []" class="field-error sq-span">{{ m }}</div>
            <span v-if="!queries.length" class="hint">No saved queries yet: save one in Catalog ▸ Saved queries first.</span>
            <label for="cj-args">Arguments</label>
            <textarea id="cj-args" v-model="d.draft.argsText" rows="3" :class="{ invalid: d.errors.args }" spellcheck="false" class="code"></textarea>
            <div v-for="m in d.errors.args || []" class="field-error sq-span">{{ m }}</div>
            <span class="hint">A JSON object, parameter name -> value; checked against the query's parameters.</span>
          </template>
          <template v-if="d.draft.action === 'backup'">
            <label for="cj-path">Directory</label>
            <input id="cj-path" type="text" v-model="d.draft.path" :class="{ invalid: d.errors.path }" placeholder="/backups/graph" autocomplete="off" spellcheck="false">
            <div v-for="m in d.errors.path || []" class="field-error sq-span">{{ m }}</div>
            <span class="hint">{{ serverMode ? 'A directory on the machine of the dev server; the first run is a full backup, the next ones increments.' : 'The browser playground has no files: backup jobs work with the dev server (graphersal --server).' }}</span>
          </template>
          <label for="cj-schedule">Schedule</label>
          <span class="cj-schedule">
            <input id="cj-schedule" type="text" v-model="d.draft.schedule" :class="{ invalid: d.errors.schedule }" placeholder="0 3 * * *" autocomplete="off" spellcheck="false" style="width:16ch">
            <select aria-label="Schedule presets" @change="d.draft.schedule = $event.target.value; $event.target.value = ''">
              <option value="">Presets…</option>
              <option v-for="p in presets" :key="p.cron" :value="p.cron">{{ p.label }} ({{ p.cron }})</option>
            </select>
          </span>
          <div v-for="m in d.errors.schedule || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">5-field cron: minute hour day-of-month month day-of-week, in {{ view.timeZone || 'UTC' }}. {{ scheduleText(d.draft.schedule.trim()) !== d.draft.schedule.trim() ? scheduleText(d.draft.schedule.trim()) + '.' : '' }}</span>
          <label for="cj-enabled">On</label>
          <span><input id="cj-enabled" type="checkbox" v-model="d.draft.enabled"> <label for="cj-enabled" class="muted">runs on its schedule (off: kept, not run)</label></span>
          <label for="cj-timeout">Timeout</label>
          <input id="cj-timeout" type="text" inputmode="numeric" v-model="d.draft.timeoutText" :class="{ invalid: d.errors.timeout_ms }" placeholder="no limit" style="width:14ch;justify-self:start">
          <div v-for="m in d.errors.timeout_ms || []" class="field-error sq-span">{{ m }}</div>
          <span class="hint">Milliseconds a run may take; a longer run is stopped and recorded as failed.</span>
          <label for="cj-desc">Description</label>
          <input id="cj-desc" type="text" v-model="d.draft.description" :class="{ invalid: d.errors.description }" autocomplete="off">
        </div>
        <div v-for="m in d.errors.general || []" class="field-error">{{ m }}</div>
        <p class="muted sq-note">Saving checks your rights for everything the job will do and stores them with it (one commit); nothing runs until its time or Run now.</p>
        <div class="sq-buttons">
          <span class="spacer"></span>
          <button type="button" class="btn" @click="cancelForm">Back</button>
          <button type="submit" class="btn primary" :disabled="d.busy || state.running">{{ d.busy ? 'Saving…' : 'Save' }}</button>
        </div>
      </form>
    </div>
  </dialog>
  `,
};

/** The managers of the Catalog dropdown, opened through `openCatalog(id)`. */
export const CatalogDialogs = {
  components: { SavedQueriesManager, CompressionManager, IndexesManager, JobsManager },
  setup() {
    const queries = ref(null);
    const compression = ref(null);
    const indexes = ref(null);
    const jobs = ref(null);
    watch(() => state.catalogDialogN, async () => {
      const id = state.catalogDialog;
      state.catalogDialog = null;
      await nextTick();
      if (id === 'queries' && queries.value) await queries.value.open();
      if (id === 'save-as-query' && queries.value) {
        const script = state.catalogDialogScript;
        state.catalogDialogScript = null;
        await queries.value.openFromScript(script || '');
      }
      if (id === 'compression' && compression.value) await compression.value.open();
      if (id === 'indexes' && indexes.value) await indexes.value.open();
      if (id === 'jobs' && jobs.value) await jobs.value.open();
    });
    return { queries, compression, indexes, jobs };
  },
  template: `
  <saved-queries-manager ref="queries"></saved-queries-manager>
  <compression-manager ref="compression"></compression-manager>
  <indexes-manager ref="indexes"></indexes-manager>
  <jobs-manager ref="jobs"></jobs-manager>
  `,
};
