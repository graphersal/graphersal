// Schema panel: the schema EDITOR (Tables: nested tables per label, edited in place; JSON: the
// same schema as text, synchronised both ways) and two drawings of the SAME schema (the draft,
// else the stored, else the inferred one): a graph (vertex labels as nodes, edge labels as
// edges) and a Mermaid diagram.
//
// The editor works on a draft (js/schema/draft.js) and reaches the engine ONLY through the seven
// schema methods (js/schema/api.js): get/infer to show, diff ("What changes"), validate_patch
// ("Check against data"), patch ("Apply"). The drawings come from the engine's `schema()` view
// (the stored schema unchanged) or `schemaViewOf(working)` (anything else).

import { ref, reactive, computed, watch, onBeforeUnmount, nextTick, markRaw } from 'vue';
import { state, isDark, toast, schemaChanged, openQueryTab } from '../store.js';
import { storage, cssVar } from '../util.js';
import { loadScript } from '../cdn-script.js';
import { engineSchemaApi } from '../schema/api.js';
import { SchemaFlow } from '../schema/flow.js';
import { SchemaPanelSync } from '../schema/panel-sync.js';
import { MODES, schemaText, parseSchemaText, violationQuery, backfillScript } from '../schema/draft.js';
import { SchemaTables, SchemaReview } from './schema-editor.js';
import { exportFileName, downloadBlob, prepareSvg, svgToPngBlob } from '../export.js';
import { attachPanZoom } from '../pan-zoom.js';
import { PanelFrame, PanelTabs, FitButton, CopyButton, ExportMenu } from './panel-frame.js';
import { GraphCanvas, LAYOUTS } from './graph-canvas.js';

const TABS = [
  { id: 'table', label: 'Tables' },
  { id: 'json', label: 'JSON' },
  { id: 'graph', label: 'Graph' },
  { id: 'mermaid', label: 'Mermaid' },
];

// Mermaid is large: it is loaded on first use of the Mermaid tab only, as cdnjs's single-file
// build (global `mermaid`) with the SRI hash cdnjs publishes. (The ESM build loads dozens of
// chunk files that an import map's integrity list would each have to pin.)
export const MERMAID_SCRIPT = Object.freeze({
  src: 'https://cdnjs.cloudflare.com/ajax/libs/mermaid/11.17.2/mermaid.min.js',
  integrity: 'sha512-vVGqEG+fvZZAmwA11LG1aLNuXQnraqZPVkL17zv1/0qDtAmoPl70hXI6oZZI4IL57LgjerTteCCpTs1pKIWLmw==',
});
let mermaidPromise = null;
function loadMermaid() {
  if (!mermaidPromise) {
    const loaded = window.mermaid ? Promise.resolve() : loadScript(MERMAID_SCRIPT);
    mermaidPromise = loaded.then(() => window.mermaid).catch((err) => {
      mermaidPromise = null;
      throw err;
    });
  }
  return mermaidPromise;
}

let renderCount = 0;

/** Above this many labels the JSON tab asks before it shows the schema's text (jsonHeld). */
const JSON_MAX_LABELS = 2000;

/**
 * The api with the stored and the inferred schema kept out of Vue's reactivity (`markRaw`): they
 * are replaced whole, never edited in place (a draft is a copy), and the schema inferred from a
 * graph with one label per vertex has 100 000 labels, which every read through a reactive proxy
 * (the label list, the JSON text, a deep compare) would walk key by key on the main thread.
 */
function rawSchemas(api) {
  const raw = (v) => (v && typeof v === 'object' ? markRaw(v) : v);
  return { ...api, get: async () => raw(await api.get()), infer: async () => raw(await api.infer()) };
}

export const SchemaPanel = {
  components: { PanelFrame, PanelTabs, GraphCanvas, FitButton, CopyButton, ExportMenu, SchemaTables, SchemaReview },
  props: {
    /** The panel is visible and expanded: its first show reads (and infers) the schema. */
    active: { type: Boolean, default: true },
  },
  setup(props) {
    const tab = ref(storage.get('schema.tab', 'table'));
    const schema = ref(null);
    const error = ref(null);
    const layout = ref('cose');
    const mermaidHost = ref(null);
    const schemaCy = ref(null);
    const mermaidStage = ref(null);
    let panZoom = null;
    let panZoomHost = null;
    const mermaidError = ref(null);
    const mermaidBusy = ref(false);
    const showSource = ref(false);

    // The editor: a draft over the seven schema methods. Created once the engine exists.
    const flow = ref(null);
    const schemaFile = ref(null);
    const isDrawing = () => tab.value === 'graph' || tab.value === 'mermaid';
    // What the drawings were read for: the editor's working schema (its identity and the draft
    // version) and the graph version (the label counts).
    let viewsFor = null;

    /**
     * The drawings' view, only when a drawing tab shows it: always of the schema the Tables and
     * JSON tabs show (the editor's working schema: the draft, else the stored, else the
     * inferred one), so the tabs never disagree. The stored schema unchanged is the engine's own
     * view; anything else goes to the engine as `schemaViewOf(working)` (no inference: the
     * inferred schema is the one the editor holds). A change of the graph re-reads the label
     * counts, except for a schema inferred earlier, which waits for refresh ("may be
     * outdated"). `force` without a drawing tab drops the view, so the next drawing reads it.
     */
    async function loadViews(force = false) {
      if (!state.engine || !state.graph) return;
      if (!isDrawing()) {
        if (force) dropViews();
        return;
      }
      const f = flow.value;
      // The editor loads first; the watch on its working schema draws once it has.
      if (!f || !f.loaded) return;
      const ed = f.ed;
      const key = { working: ed.working, version: ed.version, graphVersion: state.graphVersion };
      const same = viewsFor && viewsFor.working === key.working && viewsFor.version === key.version;
      const countsCurrent = viewsFor && (viewsFor.graphVersion === key.graphVersion || ed.source === 'inferred');
      if (!force && schema.value && same && countsCurrent) return;
      viewsFor = key;
      try {
        const view = ed.source === 'stored'
          ? await state.engine.schema()
          : await state.engine.schemaViewOf(key.working);
        if (viewsFor !== key) return; // a newer read is under way
        schema.value = markRaw(view);
        error.value = null;
      } catch (err) {
        if (viewsFor !== key) return;
        error.value = `The schema the editor shows cannot be drawn: ${err && err.message ? err.message : err}`;
      }
    }

    function dropViews() {
      schema.value = null;
      viewsFor = null;
    }

    // When the panel reads the schema again: first show, refresh, a change, a replaced graph
    // (js/schema/panel-sync.js). Created once the engine and a graph exist.
    let sync = null;
    function ensureSync() {
      if (sync) return sync;
      if (!state.engine || !state.graph) return null;
      flow.value = reactive(new SchemaFlow(rawSchemas(engineSchemaApi(state.engine))));
      sync = new SchemaPanelSync(flow.value, { load: loadViews, drop: dropViews }, state.graphEpoch);
      return sync;
    }

    const ed = computed(() => (flow.value ? flow.value.ed : null));

    // The JSON tab of a schema with very many labels (inferred from a graph with one label per
    // vertex: megabytes of text) is held until asked for: formatting it and filling the text
    // area blocks the page for about a second after every change of the graph.
    const jsonShowAll = ref(false);
    const jsonLabels = computed(() => {
      const w = ed.value ? ed.value.working : {};
      return Object.keys(w.vertices || {}).length + Object.keys(w.edges || {}).length;
    });
    const jsonHeld = computed(() => !jsonShowAll.value && jsonLabels.value > JSON_MAX_LABELS);

    function setMode(mode) {
      flow.value.ed.edit((d) => { d.mode = mode; });
    }

    async function apply() {
      if (await flow.value.apply()) {
        toast('Schema applied.', 'info');
        await schemaChanged();
      }
    }

    async function freeze(mode) {
      if (await flow.value.freeze(mode)) {
        toast(`Frozen: the structure the data has is now the stored ${mode} schema.`, 'info');
        await schemaChanged();
      }
    }

    async function inferDraft() {
      if (flow.value.ed.dirty && !window.confirm('Replace the current draft with the schema inferred from the data?')) return;
      await flow.value.inferDraft();
    }

    function discard() {
      if (flow.value.ed.dirty && !window.confirm('Discard the schema draft?')) return;
      flow.value.discard();
    }

    function downloadSchema() {
      const text = schemaText(flow.value.ed.working);
      downloadBlob(new Blob([text + '\n'], { type: 'application/json' }), 'schema.json');
    }

    async function onSchemaFile(e) {
      const file = e.target.files && e.target.files[0];
      e.target.value = '';
      if (!file) return;
      const parsed = parseSchemaText(await file.text());
      if (!parsed.ok) {
        toast(`"${file.name}": ${parsed.error}`, 'error', 9000);
        return;
      }
      if (flow.value.ed.dirty && !window.confirm(`Replace the current draft with "${file.name}"?`)) return;
      flow.value.ed.start(parsed.value, 'file');
      toast(`"${file.name}" is the draft now: review it, then Apply.`, 'info');
    }

    function showViolation(group) {
      const q = violationQuery(group);
      openQueryTab({ title: q.title, script: q.script, execute: true });
    }

    function openBackfill() {
      const q = backfillScript(flow.value.ed.draft, flow.value.freshCheck ? flow.value.freshCheck.violations.groups : []);
      if (q) openQueryTab({ title: q.title, script: q.script, execute: false });
    }

    const sourceBadge = computed(() => {
      if (!flow.value || !flow.value.loaded) return null;
      const src = flow.value.ed.source;
      return {
        draft: { text: 'draft', cls: 'warn', title: 'Unapplied edits: review them, then Apply' },
        stored: { text: 'stored', cls: 'accent', title: 'The schema the graph stores' },
        inferred: { text: 'inferred', cls: '', title: 'The graph stores no schema: derived from the data (edit it to start a draft)' },
        none: null,
      }[src];
    });
    const applyTitle = computed(() => {
      const f = flow.value;
      if (!f || !f.ed.draft) return 'No draft';
      if (!f.ed.dirty) return 'The draft equals the stored schema';
      if (f.ed.jsonError) return 'The JSON tab holds invalid JSON';
      if (f.blocked) return 'Blocked: the data violates the draft (see the check)';
      return f.ed.patch && f.ed.patch.exact
        ? 'Apply the draft (patch_schema; checks the data first, all or nothing)'
        : 'Apply the draft (set_schema: the draft sets a member to null, which a merge patch cannot express)';
    });

    async function renderMermaid() {
      if (tab.value !== 'mermaid' || !schema.value || showSource.value) return;
      mermaidBusy.value = true;
      mermaidError.value = null;
      try {
        const mermaid = await loadMermaid();
        mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: isDark() ? 'dark' : 'default' });
        const { svg } = await mermaid.render(`schema-mermaid-${++renderCount}`, schema.value.mermaid);
        await nextTick();
        const host = mermaidHost.value;
        const stage = mermaidStage.value;
        if (host && stage) {
          stage.innerHTML = svg;
          // Give the SVG its natural size (Mermaid sets width 100% + max-width) so pan/zoom owns scaling.
          const el = stage.querySelector('svg');
          const vb = el && el.viewBox && el.viewBox.baseVal;
          if (el && vb && vb.width && vb.height) {
            el.setAttribute('width', String(vb.width));
            el.setAttribute('height', String(vb.height));
            el.style.maxWidth = 'none';
          }
          if (panZoomHost !== host) {
            if (panZoom) panZoom.destroy();
            panZoom = attachPanZoom(host, stage);
            panZoomHost = host;
          }
          requestAnimationFrame(() => panZoom && panZoom.fit());
        }
      } catch (err) {
        mermaidError.value = `Mermaid could not render the diagram: ${err && err.message ? err.message : err}`;
      } finally {
        mermaidBusy.value = false;
      }
    }

    const exportBusy = ref(false);
    const exportError = ref(null);
    const GRAPH_FORMATS = [
      { id: 'png', label: 'PNG (whole drawing)' },
      { id: 'png-view', label: 'PNG (visible area)' },
    ];
    const MERMAID_FORMATS = [
      { id: 'png', label: 'PNG' },
      { id: 'svg', label: 'SVG' },
    ];

    /**
     * A dedicated export rendering of the Mermaid diagram: SVG text labels instead of HTML
     * <foreignObject> labels (those taint a canvas), same theme as the screen.
     */
    async function mermaidExportSvg() {
      const mermaid = await loadMermaid();
      const theme = isDark() ? 'dark' : 'default';
      mermaid.initialize({
        startOnLoad: false, securityLevel: 'strict', theme,
        htmlLabels: false, flowchart: { htmlLabels: false }, class: { htmlLabels: false },
      });
      try {
        const { svg } = await mermaid.render(`schema-mermaid-export-${++renderCount}`, schema.value.mermaid);
        return svg;
      } finally {
        mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme }); // back to the screen config
      }
    }

    async function onExport(kind, id, { transparent }) {
      exportError.value = null;
      exportBusy.value = true;
      try {
        if (kind === 'graph') {
          const blob = await schemaCy.value.exportPng({ full: id === 'png', transparent });
          downloadBlob(blob, exportFileName('schema', 'png'));
          return;
        }
        const shown = mermaidStage.value && mermaidStage.value.querySelector('svg');
        const fontFamily = shown ? getComputedStyle(shown).fontFamily : null;
        const background = transparent ? null : (cssVar('--surface') || '#ffffff');
        const prepared = prepareSvg(await mermaidExportSvg(), { background, fontFamily });
        if (id === 'svg') {
          downloadBlob(new Blob([prepared.text], { type: 'image/svg+xml' }), exportFileName('mermaid', 'svg'));
        } else {
          downloadBlob(await svgToPngBlob(prepared.text, prepared.width, prepared.height, 2), exportFileName('mermaid', 'png'));
        }
      } catch (err) {
        exportError.value = `Export failed: ${err && err.message ? err.message : err}`;
      } finally {
        exportBusy.value = false;
      }
    }

    onBeforeUnmount(() => { if (panZoom) panZoom.destroy(); panZoom = null; });
    // First show: visible and expanded (a collapsed or hidden panel reads nothing).
    watch(() => props.active, (active) => {
      const s = ensureSync();
      if (s) s.setActive(active);
    }, { immediate: true });
    watch(() => state.graphVersion, () => {
      const fresh = !sync;
      const s = ensureSync();
      if (!s) return;
      if (fresh) s.setActive(props.active);
      else s.graphChanged(state.graphEpoch);
    });
    watch(tab, (v) => { storage.set('schema.tab', v); loadViews().then(() => renderMermaid()); });
    // Every change of the editor's working schema (a draft edit, Infer from data, a loaded file,
    // Discard, an Apply) redraws, shortly after the last one.
    let redrawTimer = null;
    watch(() => (flow.value && flow.value.loaded ? [flow.value.ed.version, flow.value.ed.working] : null), (now) => {
      if (!now) return;
      clearTimeout(redrawTimer);
      redrawTimer = setTimeout(() => loadViews(), 250);
    });
    onBeforeUnmount(() => clearTimeout(redrawTimer));
    watch([schema, showSource, () => state.theme, () => state.systemDark], () => nextTick(renderMermaid));
    watch(() => state.focusPanel, (req) => {
      if (req && req.panel === 'schema' && req.tab && TABS.some((t) => t.id === req.tab)) tab.value = req.tab;
    });

    const graphElements = computed(() => (schema.value ? schema.value.graph : null));
    const isEmpty = computed(() => schema.value && !schema.value.table.rows.length);

    return {
      flow, ed, schemaFile, MODES, setMode, apply, freeze, inferDraft, discard, downloadSchema, onSchemaFile,
      jsonShowAll, jsonLabels, jsonHeld,
      showViolation, openBackfill, sourceBadge, applyTitle,
      pickSchemaFile: () => schemaFile.value && schemaFile.value.click(),
      state, TABS, LAYOUTS, tab, schema, error, layout, graphElements, isEmpty,
      mermaidHost, mermaidStage, mermaidError, mermaidBusy, showSource,
      fitMermaid: () => panZoom && panZoom.fit(),
      schemaCy,
      copyLabel: computed(() => ({ table: 'Copy the schema as JSON', json: 'Copy the schema as JSON', mermaid: 'Copy the Mermaid source' })[tab.value]),
      copySchema: () => {
        if (tab.value === 'mermaid') return schema.value ? schema.value.mermaid : '';
        return flow.value ? schemaText(flow.value.ed.working) : '';
      },
      fitGraph: () => schemaCy.value && schemaCy.value.fit(),
      exportBusy, exportError, GRAPH_FORMATS, MERMAID_FORMATS, onExport,
      // Header refresh: read the stored schema and infer again (the draft stays), clearing
      // "may be outdated"; the Mermaid view re-renders on change.
      refresh: () => { const s = ensureSync(); if (s) s.refresh(); },
      outdatedTitle: 'The graph changed after this schema was inferred (or its stored schema was dropped), so it may be outdated. Press refresh to infer it again: the panel does not re-infer on every change, which walks the whole graph.',
    };
  },
  template: `
  <panel-frame title="Schema" refresh-title="Reload the schema from the engine and infer it again (a draft stays)" @refresh="refresh">
    <template #tabs><panel-tabs :tabs="TABS" v-model="tab"></panel-tabs></template>
    <template #actions>
      <copy-button v-if="flow && tab !== 'graph'" :get-text="copySchema" :label="copyLabel"></copy-button>
      <span v-if="sourceBadge" class="badge secondary" :class="sourceBadge.cls" :title="sourceBadge.title">{{ sourceBadge.text }}</span>
      <span v-if="ed && ed.working.mode" class="badge secondary" title="The schema mode">{{ ed.working.mode }}</span>
      <span v-if="flow && flow.outdated" class="badge warn schema-outdated" role="status" :title="outdatedTitle" :aria-label="outdatedTitle">may be outdated - refresh</span>
    </template>
    <div class="fill">
      <div v-if="!flow || !flow.loaded" class="placeholder">Loading the schema…</div>
      <template v-else-if="tab === 'table' || tab === 'json'">
        <div class="schema-bar" role="toolbar" aria-label="Schema draft">
          <div class="schema-bar-group">
            <label>mode
              <select :value="ed.working.mode || 'none'" @change="setMode($event.target.value)" aria-label="Schema mode"
                      title="none: describe only; open: check declared labels; closed: also reject undeclared labels and properties, check edge topology">
                <option v-for="m in MODES" :key="m" :value="m">{{ m }}</option>
              </select>
            </label>
            <button class="btn small" :disabled="!!flow.busy" @click="inferDraft" title="Start a draft from the schema the data satisfies (infer_schema; stores nothing)">Infer from data</button>
            <button class="btn small" @click="pickSchemaFile" title="Load a schema.json file as the draft">Load…</button>
            <button class="btn small" @click="downloadSchema" title="Download what the editor shows as schema.json">Download</button>
            <input ref="schemaFile" type="file" accept=".json,application/json" hidden @change="onSchemaFile">
          </div>
          <div v-if="flow.canFreeze || flow.busy === 'freeze'" class="schema-bar-group">
            <button class="btn small" :disabled="!!flow.busy" @click="freeze('open')"
                    title="Store the schema the data satisfies in mode open: existing labels and properties keep their types; new ones stay allowed (set_schema(infer_schema(), SchemaMode.open))">Freeze as open</button>
            <button class="btn small" :disabled="!!flow.busy" @click="freeze('closed')"
                    title="Store the schema the data satisfies in mode closed: nothing else is admitted afterwards (set_schema(infer_schema(), SchemaMode.closed)). Fails, changing nothing, when vertices or edges have no label">{{ flow.busy === 'freeze' ? 'Freezing…' : 'Freeze as closed' }}</button>
          </div>
          <div v-if="ed.draft" class="schema-bar-group flow">
            <button class="btn small" :disabled="!!flow.busy || !!ed.jsonError" @click="flow.whatChanges()" title="List the changes from the stored schema (diff_schema)">
              {{ flow.busy === 'diff' ? 'Comparing…' : 'What changes' }}</button>
            <button class="btn small" :disabled="!!flow.busy || !!ed.jsonError || !ed.dirty" @click="flow.checkAgainstData()" title="Validate the existing data against the draft (validate_schema_patch; changes nothing)">
              {{ flow.busy === 'check' ? 'Checking…' : 'Check against data' }}</button>
            <button class="btn small primary" :disabled="!flow.canApply" @click="apply" :title="applyTitle">
              {{ flow.busy === 'apply' ? 'Applying…' : 'Apply' }}</button>
            <button class="btn small" :disabled="!!flow.busy" @click="discard" title="Throw the draft away; the stored schema stays">Discard</button>
          </div>
        </div>
        <div v-if="flow.loadError" class="notice" role="alert">{{ flow.loadError }}</div>
        <schema-review :flow="flow" @show-violation="showViolation" @backfill="openBackfill"></schema-review>
        <div v-if="tab === 'table'" class="schema-scroll">
          <schema-tables :flow="flow"></schema-tables>
        </div>
        <div v-else-if="jsonHeld" class="placeholder">
          <span>The schema has {{ jsonLabels }} labels: its JSON text is megabytes, and showing it here would block the
            page after every change of the graph.
            <a href="#" @click.prevent="jsonShowAll = true">Show the JSON anyway</a> or
            <a href="#" @click.prevent="downloadSchema">download it</a>.</span>
        </div>
        <div v-else class="schema-json">
          <div v-if="ed.jsonError" class="field-error" role="alert">{{ ed.jsonError }}. The tables keep the last valid draft.</div>
          <textarea class="code-input" spellcheck="false" aria-label="The schema as JSON (edits change the draft)"
                    :class="{ invalid: ed.jsonError }" :value="ed.jsonText" @input="ed.setJsonText($event.target.value)"></textarea>
        </div>
      </template>
      <template v-else>
        <div v-if="error" class="notice">{{ error }}</div>
        <div v-if="ed && ed.dirty" class="notice info">The drawing shows the draft, which is not applied yet.</div>
        <div v-if="schema && schema.truncated" class="notice info">
          The graph has {{ schema.total_labels }} labels; the views show the first ones only.
        </div>
        <div v-if="exportError" class="notice" role="alert">{{ exportError }} <a href="#" @click.prevent="exportError = null">Dismiss</a></div>
        <div v-if="!schema" class="placeholder">Loading the schema…</div>
        <div v-else-if="isEmpty" class="placeholder">
          <span>The graph has no labels or properties yet, so there is nothing to draw. Declare labels in the Tables tab.</span>
        </div>
        <div v-else-if="tab === 'graph'" class="graph-wrap" style="position:relative;flex:1">
          <div class="panel-toolbar">
            <div class="toolbar-main">
              <label>Layout <select v-model="layout"><option v-for="l in LAYOUTS" :key="l.id" :value="l.id">{{ l.label }}</option></select></label>
            </div>
            <div class="toolbar-end">
              <span class="info-tip" tabindex="0" role="note"
                    aria-label="Vertex labels as nodes, edge labels as edges, with element counts. Click for properties; double-click empty space to fit."
                    title="Vertex labels as nodes, edge labels as edges, with element counts. Click for properties; double-click empty space to fit.">ⓘ</span>
              <fit-button @fit="fitGraph"></fit-button>
              <export-menu :formats="GRAPH_FORMATS" :busy="exportBusy" @export="(id, o) => onExport('graph', id, o)"></export-menu>
            </div>
          </div>
          <graph-canvas ref="schemaCy" kind="schema" :elements="graphElements" :layout="layout" empty-text="No labels."></graph-canvas>
        </div>
        <div v-else-if="tab === 'mermaid'" style="flex:1;min-height:0;display:flex;flex-direction:column">
          <div class="panel-toolbar">
            <div class="toolbar-main">
              <label><input type="checkbox" v-model="showSource"> show source</label>
              <span v-if="mermaidBusy" class="faint">rendering…</span>
            </div>
            <div class="toolbar-end">
              <span v-if="!showSource" class="info-tip" tabindex="0" role="note"
                    aria-label="Pinch or Ctrl+wheel to zoom, drag or scroll to pan, double-click to fit."
                    title="Pinch or Ctrl+wheel to zoom, drag or scroll to pan, double-click to fit.">ⓘ</span>
              <fit-button v-if="!showSource" @fit="fitMermaid"></fit-button>
              <export-menu :formats="MERMAID_FORMATS" :busy="exportBusy" @export="(id, o) => onExport('mermaid', id, o)"></export-menu>
            </div>
          </div>
          <pre v-if="showSource" class="code-view" style="flex:1;overflow:auto">{{ schema.mermaid }}</pre>
          <template v-else>
            <div v-if="mermaidError" class="notice">{{ mermaidError }}</div>
            <div class="mermaid-view" ref="mermaidHost"><div class="pz-stage" ref="mermaidStage"></div></div>
          </template>
        </div>
      </template>
    </div>
  </panel-frame>
  `,
};
