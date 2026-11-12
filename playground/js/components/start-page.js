// Start page: choose a graph (sample, generator, or a GraphSON/GraphML file with an optional schema
// file), load a schema for the current graph, save the current one, short intro and links.

import { ref, reactive, onMounted } from 'vue';
import { state, loadGraph, loadSchema, cycleTheme, saveGraph, saveSchema, saveCatalogFile, loadCatalogFile } from '../store.js';
import { catalogTotal } from '../compression.js';
import { navigate, hashOf } from '../router.js';
import { LINKS } from '../config.js';
import { loadChoice, droppedFiles } from '../load-dialog.js';

export const StartPage = {
  emits: ['help'],
  setup() {
    const samples = ref([]);
    const fileInput = ref(null);
    const schemaInput = ref(null);
    const queriesInput = ref(null);
    const dragover = ref(false);
    const busyKind = ref(null);
    // The "Load from file" dialog: a graph file and an optional schema file.
    const loadDlg = ref(null);
    const dlgGraphInput = ref(null);
    const dlgSchemaInput = ref(null);
    const pick = reactive({ graphFile: null, schemaFile: null, error: null });

    onMounted(async () => {
      try {
        samples.value = await state.engine.listSampleGraphs();
      } catch (err) {
        state.loadError = err.message;
      }
    });

    async function choose(sample) {
      if (!sample.available || state.loadingGraph) return;
      busyKind.value = sample.kind;
      const ok = await loadGraph({ kind: sample.kind });
      busyKind.value = null;
      if (ok) navigate('play');
    }

    async function loadFile(request) {
      if (!request || state.loadingGraph) return;
      busyKind.value = 'file';
      const ok = await loadGraph(request);
      busyKind.value = null;
      if (ok) navigate('play');
    }

    function openLoadDialog() {
      if (state.loadingGraph) return;
      Object.assign(pick, { graphFile: null, schemaFile: null, error: null });
      if (loadDlg.value && !loadDlg.value.open) loadDlg.value.showModal();
    }

    function closeLoadDialog() {
      if (loadDlg.value) loadDlg.value.close();
    }

    function onDialogGraph(e) {
      const file = e.target.files && e.target.files[0];
      e.target.value = '';
      if (file) Object.assign(pick, { graphFile: file, error: null });
    }

    function onDialogSchema(e) {
      const file = e.target.files && e.target.files[0];
      e.target.value = '';
      if (file) Object.assign(pick, { schemaFile: file, error: null });
    }

    function submitLoad() {
      const { request, error } = loadChoice(pick);
      if (error) {
        pick.error = error;
        return;
      }
      closeLoadDialog();
      loadFile(request);
    }

    // A drop of ONE file loads it without a schema; a graph file and its .json schema pair up
    // when that is unambiguous (load-dialog.js `droppedFiles`).
    function onDrop(e) {
      dragover.value = false;
      if (state.loadingGraph) return;
      const { request, error } = droppedFiles(e.dataTransfer && e.dataTransfer.files);
      if (error) state.loadError = error;
      else loadFile(request);
    }

    async function onSchemaChange(e) {
      const file = e.target.files && e.target.files[0];
      e.target.value = '';
      if (!file) return;
      busyKind.value = 'schema';
      await loadSchema(file);
      busyKind.value = null;
    }

    async function onQueriesChange(e) {
      const file = e.target.files && e.target.files[0];
      e.target.value = '';
      if (!file) return;
      busyKind.value = 'catalog';
      await loadCatalogFile(file);
      busyKind.value = null;
    }

    const icons = { empty: '○', modern: '◉', large: '⬢' };

    return {
      state, samples, schemaInput, queriesInput, dragover, busyKind, icons, LINKS,
      loadDlg, dlgGraphInput, dlgSchemaInput, pick, openLoadDialog, closeLoadDialog, onDialogGraph, onDialogSchema, submitLoad,
      choose, onDrop, onSchemaChange, onQueriesChange, cycleTheme, saveGraph, saveSchema, saveCatalogFile, catalogTotal,
      pickQueries: () => queriesInput.value && queriesInput.value.click(),
      pickDialogGraph: () => dlgGraphInput.value && dlgGraphInput.value.click(),
      pickDialogSchema: () => dlgSchemaInput.value && dlgSchemaInput.value.click(),
      pickSchema: () => schemaInput.value && schemaInput.value.click(),
      resume: () => navigate('play'),
      startHash: () => hashOf('start'),
    };
  },
  template: `
  <div class="start">
    <header class="topbar">
      <a class="brand" :href="startHash()">Graphersal <small>Playground</small></a>
      <span v-if="state.versionLabel" class="version-label hide-narrow" :title="state.versionLabel.title">{{ state.versionLabel.text }}</span>
      <span class="spacer"></span>
      <button class="icon-btn" @click="cycleTheme" :title="'Theme: ' + state.theme + ' (Alt+T)'" aria-label="Toggle theme">
        <span v-if="state.theme === 'dark'">☾</span><span v-else-if="state.theme === 'light'">☀</span><span v-else>◐</span>
      </button>
      <button class="icon-btn" @click="$emit('help')" title="Help and keyboard shortcuts (F1)" aria-label="Help">?</button>
    </header>
    <div class="start-inner">
      <h1>Graphersal Playground</h1>
      <p class="lead">
        Write Gremlin-style traversals against an in-memory property graph, right in the browser:
        results as tables and JSON, the graph drawn, the schema inspected, the optimized plan profiled.
      </p>

      <section v-if="!(state.engineInfo && state.engineInfo.server)" class="privacy" aria-labelledby="privacy-title">
        <h2 id="privacy-title">🔒 Your data never leaves this browser</h2>
        <p>
          <b>Everything runs on your device.</b> The graph engine is compiled to WebAssembly and works
          inside this page: the graphs you load, the files you open, the queries you write and their
          results are processed locally and are <b>never sent to any server</b> — not to us, not to anyone.
        </p>
        <p>
          No account, no cookies, no analytics, no tracking. Your settings, query tabs and history are
          kept only in this browser's local storage, and <a href="reset/">reset/</a> clears them.
          The page loads nothing but its own code and its open-source libraries (from cdnjs and jsDelivr).
        </p>
      </section>

      <div v-if="state.graph" style="margin-bottom:8px;display:flex;gap:8px;flex-wrap:wrap">
        <button class="btn primary" @click="resume">Continue with "{{ state.graph.name }}" →</button>
        <button class="btn" @click="saveGraph('graphson')" :disabled="state.saving"
                title="Download the current graph's data as GraphSON 3.0 (TinkerPop's format, keeps every value type; no schema inside: save it with Save schema)">
          {{ state.saving ? 'Saving…' : 'Save data (GraphSON)' }}
        </button>
        <button class="btn" @click="saveGraph('graphml')" :disabled="state.saving"
                title="Download the current graph's data as GraphML (no schema inside)">
          Save data (GraphML)
        </button>
        <button class="btn" @click="saveSchema" :disabled="state.saving"
                title="Download the stored schema as JSON; to load both back, give it as the schema file in Load from file">
          Save schema
        </button>
        <button class="btn" @click="saveCatalogFile" :disabled="state.saving"
                title="Download the catalog (saved queries and compression rules) as one JSON file; load it back with Load catalog">
          Save catalog
        </button>
        <button v-if="state.engine && state.engine.supportsSnapshots" class="btn" @click="saveGraph('snapshot')" :disabled="state.saving"
                title="Download the current graph as one packed snapshot (.gsnap): lossless binary with every value type, the schema and the catalog; load it back with Load from file">
          Save as snapshot
        </button>
      </div>

      <h2>Choose a graph</h2>
      <div class="cards">
        <button v-for="s in samples" :key="s.kind" class="card" :class="{ disabled: !s.available }"
                :disabled="!s.available || state.loadingGraph" @click="choose(s)"
                :title="s.available ? 'Open the workspace with this graph' : s.reason">
          <span class="card-icon" aria-hidden="true">{{ icons[s.kind] || '◆' }}</span>
          <h3>{{ s.title }}</h3>
          <p>{{ s.description }}</p>
          <div class="card-foot">
            <span v-if="!s.available" class="badge warn">{{ s.reason }}</span>
            <span v-else-if="busyKind === s.kind" class="badge accent">loading…</span>
          </div>
        </button>

        <button class="card" :class="{ dragover }" :disabled="state.loadingGraph" @click="openLoadDialog"
                @dragover.prevent="dragover = true" @dragleave="dragover = false" @drop.prevent="onDrop"
                title="Load a snapshot (.gsnap), a GraphSON file (.json, .jsonl, .graphson) or a GraphML file (.graphml, .xml), optionally with a schema file (.json)">
          <span class="card-icon" aria-hidden="true">⇪</span>
          <h3>Load from file</h3>
          <p>Open a GraphSON file (<code>.json</code>, TinkerPop's <code>g.io()</code> format) or a GraphML
          file (<code>.graphml</code>, <code>.xml</code>), optionally with its schema file, or drop it here.
          It starts without the previous graph's schema and stays in your browser. A snapshot
          (<code>.gsnap</code>, Save as snapshot) brings its own schema.</p>
          <div class="card-foot">
            <span v-if="busyKind === 'file'" class="badge accent">loading…</span>
          </div>
        </button>

        <button class="card" :disabled="state.loadingGraph" @click="pickSchema"
                title="Apply a schema file (JSON in the schema format) to the current graph">
          <span class="card-icon" aria-hidden="true">⚙</span>
          <h3>Load schema</h3>
          <p>Apply a schema (<code>.json</code>, e.g. saved with "Save schema") to the current graph. To load
          a graph file with its schema (enforced, GraphML's typed values come back), give the schema
          file in <b>Load from file</b>.</p>
          <div class="card-foot">
            <span v-if="busyKind === 'schema'" class="badge accent">loading…</span>
            <span v-else-if="state.graph && state.graph.schemaMode" class="badge">schema: {{ state.graph.schemaMode }}</span>
          </div>
        </button>
        <input ref="schemaInput" type="file" accept=".json,application/json" hidden @change="onSchemaChange">

        <button class="card" :disabled="state.loadingGraph || !state.graph" @click="pickQueries"
                :title="state.graph ? 'Add the definitions of a catalog file (JSON, saved with Save catalog) to the current graph' : 'Choose a graph first'">
          <span class="card-icon" aria-hidden="true">★</span>
          <h3>Load catalog</h3>
          <p>The catalog (saved queries, compression rules and property indexes) travels in its own
          <code>.json</code> file, like the schema. Load it into the current graph; a graph file loaded
          afterwards keeps it. A snapshot (<code>.gsnap</code>) brings its own.</p>
          <div class="card-foot">
            <span v-if="busyKind === 'catalog'" class="badge accent">loading…</span>
            <span v-else-if="catalogTotal(state.graph)" class="badge">{{ catalogTotal(state.graph) }} definition{{ catalogTotal(state.graph) === 1 ? '' : 's' }}</span>
          </div>
        </button>
        <input ref="queriesInput" type="file" accept=".json,application/json" hidden @change="onQueriesChange">
      </div>

      <dialog ref="loadDlg" aria-labelledby="load-title" @click.self="closeLoadDialog">
        <form method="dialog" @submit.prevent="submitLoad">
          <div class="dialog-head">
            <h2 id="load-title">Load from file</h2><span class="spacer"></span>
            <button type="button" class="icon-btn" @click="closeLoadDialog" aria-label="Close">×</button>
          </div>
          <div class="dialog-body">
            <div class="form-grid">
              <label for="load-graph">Graph file</label>
              <span style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
                <button id="load-graph" type="button" class="btn small" @click="pickDialogGraph">Choose…</button>
                <span>{{ pick.graphFile ? pick.graphFile.name : 'none chosen' }}</span>
              </span>
              <span class="hint">A snapshot (.gsnap), GraphSON (.json, .jsonl, .graphson) or GraphML (.graphml, .xml).</span>
              <label for="load-schema">Schema file (JSON)</label>
              <span style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
                <button id="load-schema" type="button" class="btn small" @click="pickDialogSchema">Choose…</button>
                <span>{{ pick.schemaFile ? pick.schemaFile.name : 'optional' }}</span>
                <button v-if="pick.schemaFile" type="button" class="btn small" @click="pick.schemaFile = null; pick.error = null"
                        title="Load without a schema">Remove</button>
              </span>
              <span class="hint">Optional. Set on the new graph before the data is imported: it is enforced (open/closed
              violations fail the load, nothing changes) and GraphML's typed values (uuid, dates and times, array, object) come back.
              Without it the graph has no schema (the previous graph's schema is not kept).</span>
            </div>
            <div v-if="pick.error" class="error-view error-box" role="alert">
              <div class="error-head"><span class="msg">{{ pick.error }}</span></div>
            </div>
            <input ref="dlgGraphInput" type="file" accept=".gsnap,.json,.jsonl,.graphson,.graphml,.xml,application/json,application/xml,text/xml" hidden @change="onDialogGraph">
            <input ref="dlgSchemaInput" type="file" accept=".json,application/json" hidden @change="onDialogSchema">
            <div style="display:flex;gap:8px;margin-top:18px">
              <span class="spacer"></span>
              <button type="button" class="btn" @click="closeLoadDialog">Cancel</button>
              <button type="submit" class="btn primary" :disabled="!pick.graphFile">Load</button>
            </div>
          </div>
        </form>
      </dialog>

      <div v-if="state.loadError" class="error-view error-box" role="alert">
        <div class="error-head"><span class="msg">Could not load the graph: {{ state.loadError }}</span></div>
      </div>

      <h2>Good to know</h2>
      <div class="start-notes">
        <div class="note">
          <h4>Both spellings</h4>
          <p>Every step works in snake_case and Gremlin camelCase: <code>has_label</code> / <code>hasLabel</code>, <code>out_e</code> / <code>outE</code>.</p>
        </div>
        <div class="note">
          <h4>Run and complete</h4>
          <p><kbd>Ctrl</kbd>/<kbd>⌘</kbd> + <kbd>Enter</kbd> runs the query; type <code>.</code> for step completion with documentation.</p>
        </div>
        <div class="note">
          <h4>Display is bounded, data never</h4>
          <p>Results show at most 100 rows and 100 nested items, like the CLI. Change the limits in the settings.</p>
        </div>
        <div class="note">
          <h4>The real engine, in your browser</h4>
          <p>Queries run on the Graphersal library compiled to WebAssembly, in a background worker. <b>Stop</b> ends a long query; nothing is sent to a server.</p>
        </div>
        <div class="note">
          <h4>If the playground freezes</h4>
          <p>Open <a href="reset/">/reset</a>: it clears the saved settings, layout and tabs (your query history stays)
          and starts again with the Modern sample graph.</p>
        </div>
      </div>

      <h2>Learn more</h2>
      <div class="links">
        <a v-if="LINKS.book" :href="LINKS.book" target="_blank" rel="noopener">The Graphersal book</a>
        <span v-else class="disabled" title="Not published yet">The Graphersal book (link coming soon)</span>
        <a v-if="LINKS.repository" :href="LINKS.repository" target="_blank" rel="noopener">Source repository</a>
        <span v-else class="disabled" title="Not published yet">Source repository (link coming soon)</span>
        <a href="https://tinkerpop.apache.org/gremlin.html" target="_blank" rel="noopener">Apache TinkerPop Gremlin</a>
        <a href="#" @click.prevent="$emit('help')">Keyboard shortcuts</a>
      </div>
    </div>
  </div>
  `,
};
