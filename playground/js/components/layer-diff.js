// A LAYER's views (only when the graph is a layer: `state.graph.layer`, profile key `layerDiff`):
// - `LayerBadge`: the top-bar badge "Layer over <base>" with the change counts; a click opens
//   the Layer changes dialog;
// - `LayerDiffDialog`: the layer's changes against its base from `engine.layerDiff(spec)` (dev
//   server `POST /api/layer/diff`): Added, Modified (base → layer) and Removed, vertices before
//   edges, with a kind and a label filter, a limit and the truncated notice. A click on an added
//   or modified entry draws its neighbourhood in the Graph panel and opens it in the inspector
//   (an edge: its out vertex's neighbourhood); removed elements are listed only (they are not in
//   the graph any more). The footer has "Discard all changes…" on every writable layer (both
//   engines: `resetLayer`, one commit, the layer stays and equals its base again), with a confirm;
//   in the WebAssembly playground also Drop layer (Start layer is in the Store menu,
//   components/store-menu.js). Reloaded when it opens and after every change of the graph while
//   it is open.
// Model: js/layer-diff.js.

import { ref, reactive, computed, watch } from 'vue';
import { state, can, isServerMode, openLayerDiff, showNeighbourhood, switchLayer, resetLayer } from '../store.js';
import { serverReadOnlyReason } from '../server-mode.js';
import { formatValue } from '../util.js';
import { formatBytes } from '../statistics.js';
import {
  DIFF_KINDS, DIFF_LIMITS, initialDiffState, diffSpec, diffModel, layerBadge, layerActions, dropConfirmText,
  resetAction, resetConfirmText,
} from '../layer-diff.js';

const message = (err) => (err && err.message ? err.message : String(err));

export const LayerBadge = {
  setup() {
    const badge = computed(() => (can('layerDiff') && state.graph ? layerBadge(state.graph.layer, formatBytes) : null));
    return { badge, open: openLayerDiff };
  },
  template: `
  <button v-if="badge" type="button" class="badge layer-badge" @click="open" :title="badge.title" :aria-label="badge.label">
    {{ badge.text }} <span class="layer-counts"><span class="layer-count added">+{{ badge.counts.added }}</span>
    <span class="layer-count modified">~{{ badge.counts.modified }}</span>
    <span class="layer-count removed">−{{ badge.counts.removed }}</span></span>
  </button>`,
};

export const LayerDiffDialog = {
  setup() {
    const dlg = ref(null);
    const filter = reactive(initialDiffState());
    const answer = ref(null);
    const loading = ref(false);
    const error = ref('');
    const askDrop = ref(false);
    const askReset = ref(false);
    let seq = 0;

    const layer = computed(() => (state.graph ? state.graph.layer || null : null));
    const actions = computed(() => layerActions({
      server: isServerMode(),
      engineLayers: !!(state.engineInfo && state.engineInfo.layers),
      graph: state.graph,
      storeView: state.storeView,
      allowed: can('layerDiff'),
      running: state.running,
    }));
    const reset = computed(() => resetAction({
      engineReset: !!(state.engineInfo && state.engineInfo.layerReset),
      graph: state.graph,
      storeView: state.storeView,
      readOnly: serverReadOnlyReason(state.engineInfo),
      allowed: can('layerDiff'),
      running: state.running,
    }));

    async function load() {
      if (!state.engine || typeof state.engine.layerDiff !== 'function' || !layer.value) return;
      const mine = ++seq;
      loading.value = true;
      error.value = '';
      try {
        const res = await state.engine.layerDiff(diffSpec(filter));
        if (mine === seq) answer.value = res;
      } catch (err) {
        if (mine === seq) error.value = message(err);
      } finally {
        if (mine === seq) loading.value = false;
      }
    }

    const isOpen = () => !!(dlg.value && dlg.value.open);
    function open() {
      askDrop.value = false;
      askReset.value = false;
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      load();
    }
    function close() {
      if (dlg.value && dlg.value.open) dlg.value.close();
    }

    watch(() => state.layerDiffN, (n) => { if (n) open(); });
    // The graph changed (a run, an edit, the change feed): the open dialog reads the diff again.
    watch(() => state.graphVersion, () => { if (isOpen()) load(); });
    // The layer is gone (dropped, another graph loaded): nothing to show.
    watch(layer, (l) => { if (!l) { answer.value = null; close(); } });

    const model = computed(() => diffModel(answer.value, { format: formatValue, limit: filter.limit }));

    function focus(entry) {
      if (!entry.focus) return;
      close();
      showNeighbourhood(entry.focus.vertex, entry.focus.element);
    }

    async function discard() {
      const res = await resetLayer();
      askReset.value = false;
      if (res && res.ok) load();
    }

    async function drop() {
      const res = await switchLayer('drop');
      if (res && res.ok) close();
      askDrop.value = false;
    }

    return {
      state, dlg, filter, loading, error, model, layer, actions, askDrop, DIFF_KINDS, DIFF_LIMITS,
      reset, askReset, discard, resetText: computed(() => resetConfirmText(layer.value)),
      load, close, focus, drop, dropText: computed(() => dropConfirmText(layer.value)),
    };
  },
  template: `
  <dialog ref="dlg" class="catalog-dialog layer-dialog" aria-labelledby="layer-diff-title" @click.self="close">
    <div class="dialog-head">
      <h2 id="layer-diff-title">Layer changes</h2><span class="spacer"></span>
      <button class="btn" @click="load" :disabled="loading">{{ loading ? 'Loading…' : 'Refresh' }}</button>
      <button class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <p v-if="layer" class="muted">What this layer changed against its base <b>{{ layer.base }}</b>. The base stays
        unchanged; added and modified elements are outlined in the Graph panel (solid: added, dashed: modified).</p>
      <form class="layer-filters" @submit.prevent="load">
        <label>Show
          <select v-model="filter.kind" @change="load">
            <option v-for="k in DIFF_KINDS" :key="k.id" :value="k.id">{{ k.text }}</option>
          </select></label>
        <label>Labels <input type="text" v-model="filter.labels" placeholder="any (e.g. person, knows)" @change="load"
          title="Only elements with one of these labels (comma separated); empty: every label"></label>
        <label>Limit
          <select v-model.number="filter.limit" @change="load">
            <option v-for="n in DIFF_LIMITS" :key="n" :value="n">{{ n }}</option>
          </select></label>
      </form>
      <p v-if="error" role="alert" class="field-error">{{ error }}</p>
      <p v-else-if="model && model.error" role="alert" class="field-error">{{ model.error }}</p>
      <p v-else-if="loading && !model" class="muted">Reading the changes…</p>
      <template v-else-if="model">
        <p v-if="model.empty" class="muted">No changes match{{ filter.labels || filter.kind !== 'all' ? ' this filter' : ': the layer is still the same as its base' }}.</p>
        <p v-if="model.truncatedText" class="notice" role="status">{{ model.truncatedText }}</p>
        <section v-for="g in model.groups" :key="g.id" v-show="g.count" class="layer-group">
          <h3><span :class="['layer-mark', g.id]" aria-hidden="true"></span>{{ g.title }} ({{ g.count }})
            <span class="muted" style="font-weight:400;font-size:12px">{{ g.note }}</span></h3>
          <ul class="layer-entries">
            <li v-for="e in g.entries" :key="e.key" class="layer-entry">
              <div class="layer-entry-head">
                <button v-if="e.focus" type="button" class="link" @click="focus(e)"
                  :title="'Show it in the Graph panel' + (e.kind === 'edge' ? ' (the neighbourhood of its out vertex)' : '')">{{ e.title }}</button>
                <span v-else>{{ e.title }}</span>
              </div>
              <dl v-if="e.rows.length" class="layer-props">
                <template v-for="r in e.rows" :key="r.name">
                  <dt>{{ r.name }}</dt>
                  <dd v-if="g.id === 'modified'"><span :class="{ was: r.change !== 'set' }">{{ r.base }}</span> → <span>{{ r.layer }}</span></dd>
                  <dd v-else>{{ r.value }}</dd>
                </template>
              </dl>
            </li>
          </ul>
        </section>
      </template>
      <div v-if="reset.reset" class="layer-foot">
        <template v-if="!askReset">
          <button type="button" class="btn danger" :disabled="state.layerBusy || askDrop" @click="askReset = true"
            title="Discard every change of this layer (data, schema, catalog) in one commit; the layer stays and shows its base again, the base is not changed">Discard all changes…</button>
        </template>
        <template v-else>
          <span>{{ resetText }}</span>
          <button type="button" class="btn danger" :disabled="state.layerBusy" @click="discard">{{ state.layerBusy ? 'Discarding…' : 'Discard all changes' }}</button>
          <button type="button" class="btn" @click="askReset = false">Cancel</button>
        </template>
      </div>
      <div v-if="actions.drop" class="layer-foot">
        <template v-if="!askDrop">
          <button type="button" class="btn danger" :disabled="state.layerBusy || askReset" @click="askDrop = true"
            title="Discard this layer's changes and continue on its base">Drop layer…</button>
        </template>
        <template v-else>
          <span>{{ dropText }}</span>
          <button type="button" class="btn danger" :disabled="state.layerBusy" @click="drop">{{ state.layerBusy ? 'Dropping…' : 'Drop layer' }}</button>
          <button type="button" class="btn" @click="askDrop = false">Cancel</button>
        </template>
      </div>
    </div>
  </dialog>`,
};
