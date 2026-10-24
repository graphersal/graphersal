// The Graph panel's filter popover: an overlay over the drawing (never a docked panel), opened by
// the toolbar's Filter button and anchored under it at the panel's left edge, below the toolbar
// (position and max-height: graph-panel.js `drawerStyle`, CSS `.filter-drawer`), so it never
// covers the inspector on the right except in a very narrow panel. The panel (graph-panel.js)
// owns the filter state, applies it and persists it; this component only shows it and emits the
// edited state. Model: js/graph/filter-model.js (rows, operator lists by key type, value
// conversion).
//
// Esc closes it (the panel returns the focus to the Filter button); on open the focus moves into
// the popover. It stays open while the drawing is used (clicks on the drawing do not close it), so
// the effect of a change can be watched.

import { ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from 'vue';
import {
  labelRows, setAllLabels, setLabel, setConditions, newCondition, withKey, opsForType, keyType, conditionErrorText,
} from '../graph/filter-model.js';

const TYPE_HINT = Object.freeze({ number: 'number', temporal: 'date/time', string: 'text', boolean: 'boolean', unknown: '' });
const fmt = (n) => (Number.isFinite(n) ? n.toLocaleString('en-US') : '');

export const GraphFilterDrawer = {
  props: {
    /** The filter state (filter-model.js). */
    model: { type: Object, required: true },
    /** `[{label, count}]` of the graph's vertex / edge labels (statistics). */
    vertexLabels: { type: Array, default: () => [] },
    edgeLabels: { type: Array, default: () => [] },
    /**
     * `{label: {color, assigned, other}}` (palette.js `labelSwatch`): the drawing colour of each
     * vertex label, shown as a decorative swatch before its name. Edges are drawn in one neutral
     * colour, so edge labels have none.
     */
    vertexSwatches: { type: Object, default: () => ({}) },
    /** How many vertex / edge labels the graph has (more than listed: statistics() lists the largest). */
    vertexLabelTotal: { type: Number, default: 0 },
    edgeLabelTotal: { type: Number, default: 0 },
    /** `keyTypesOf(..)`: property keys and their types per label. */
    types: { type: Object, required: true },
    /** `buildFilter(..)` of `model`: problems and the sent conditions. */
    built: { type: Object, required: true },
    /** The engine's error about the applied filter: `{message, target}` (target may be null). */
    engineError: { type: Object, default: null },
    /** 'auto' | 'manual' (filter-model.js `applyMode`). */
    mode: { type: String, default: 'auto' },
    /** Manual mode: the edited filter differs from the applied one. */
    dirty: { type: Boolean, default: false },
    /** A filtered drawing is being fetched. */
    busy: { type: Boolean, default: false },
    /** The labels / key types are loading (`labels`, `keys`), or null. */
    loading: { type: String, default: null },
    /** The vocabulary failed to load. */
    loadError: { type: String, default: null },
    /** The Graph panel's view ('graph' | 'labels' | 'neighbourhood' | 'result'). */
    source: { type: String, default: 'graph' },
  },
  emits: ['update', 'close', 'apply', 'open-query', 'reset', 'toggle'],
  setup(props, { emit }) {
    const root = ref(null);
    const error = computed(() => (props.engineError && props.engineError.target
      ? { ...props.engineError.target, message: props.engineError.message }
      : null));
    const rows = computed(() => ({
      vertices: labelRows(props.model, 'vertices', props.vertexLabels, props.types, { problems: props.built.problems, error: error.value }),
      edges: labelRows(props.model, 'edges', props.edgeLabels, props.types, { problems: props.built.problems, error: error.value }),
    }));
    /** An engine error that names no condition row: shown at the top. */
    const generalError = computed(() => (props.engineError && !props.engineError.target ? props.engineError.message : null));

    const update = (next) => emit('update', next);
    function all(field, on) {
      const labels = (field === 'vertices' ? props.vertexLabels : props.edgeLabels).map((l) => l.label);
      update(setAllLabels(props.model, field, labels, on));
    }
    function setOn(field, label, on) { update(setLabel(props.model, field, label, on)); }
    const condsOf = (field, label) => ((props.model[field].conds || {})[label] || []);
    function addCondition(field, label, keys) {
      const kind = field === 'vertices' ? 'vertex' : 'edge';
      const key = keys.length ? keys[0] : '';
      const type = key ? keyType(props.types, kind, label, { key }) : 'unknown';
      update(setConditions(props.model, field, label, [...condsOf(field, label), withKey(newCondition(), key, type)]));
      nextTick(() => {
        const inputs = root.value ? root.value.querySelectorAll(`[data-cond="${CSS.escape(field + '|' + label)}"]`) : [];
        const last = inputs[inputs.length - 1];
        if (last) last.focus();
      });
    }
    function changeCondition(field, label, index, patch) {
      const kind = field === 'vertices' ? 'vertex' : 'edge';
      const list = condsOf(field, label).map((c, i) => {
        if (i !== index) return c;
        if ('key' in patch) return withKey(c, patch.key, keyType(props.types, kind, label, { key: patch.key }));
        return { ...c, ...patch };
      });
      update(setConditions(props.model, field, label, list));
    }
    function removeCondition(field, label, index) {
      update(setConditions(props.model, field, label, condsOf(field, label).filter((c, i) => i !== index)));
    }
    /** The swatch: filled while the label is shown, hollow (its colour as a ring) while hidden,
     * a dashed neutral outline while it has no colour yet. */
    function swatchClass(row) {
      const sw = props.vertexSwatches[row.label];
      return { hollow: !row.on, unassigned: !sw || !sw.assigned };
    }
    function swatchTitle(row) {
      const sw = props.vertexSwatches[row.label];
      if (!sw || !sw.assigned) return 'Gets its colour when it is first drawn';
      return sw.other ? 'More labels than colours: drawn in the shared "other" colour' : null;
    }
    function setHideIsolated(on) { update({ ...props.model, hideIsolated: !!on }); }

    function onKey(e) {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopPropagation();
      emit('close');
    }
    onMounted(() => {
      nextTick(() => {
        const first = root.value && root.value.querySelector('input, button, select');
        if (first) first.focus({ preventScroll: true });
      });
    });
    const onWindowKey = (e) => {
      // Esc anywhere in the panel while the drawer is open (the focus may be on the drawing).
      if (e.key === 'Escape' && !e.defaultPrevented && root.value && !root.value.contains(e.target)) {
        const panel = root.value.closest('.graph-wrap');
        if (panel && panel.contains(e.target)) onKey(e);
      }
    };
    window.addEventListener('keydown', onWindowKey, true);
    onBeforeUnmount(() => window.removeEventListener('keydown', onWindowKey, true));
    watch(() => props.engineError, (err) => {
      if (!err || !err.target) return;
      nextTick(() => {
        const el = root.value && root.value.querySelector('.fd-cond.has-problem');
        if (el) el.scrollIntoView({ block: 'nearest' });
      });
    });

    return {
      root, rows, generalError, all, swatchClass, swatchTitle, setOn, addCondition, changeCondition, removeCondition, setHideIsolated, onKey,
      TYPE_HINT, fmt, opsForType, conditionErrorText,
    };
  },
  template: `
    <div ref="root" class="filter-drawer" role="dialog" aria-label="Filter the drawing" @keydown="onKey">
      <div class="fd-head">
        <span class="fd-title">Filter</span>
        <button type="button" class="btn small fd-switch" role="switch" :aria-checked="String(model.enabled)"
                :class="{ off: !model.enabled }" :title="model.enabled ? 'Switch the filter off (it is kept)' : 'Switch the filter on'"
                @click="$emit('toggle')"><span class="switch-track" aria-hidden="true"><span class="switch-knob"></span></span>
          {{ model.enabled ? 'On' : 'Off' }}</button>
        <span class="fd-mode faint" :title="mode === 'manual' ? 'This graph is large: every change reads the whole graph, so it applies on Apply' : 'Changes apply 300 ms after the last edit'">
          {{ mode === 'manual' ? 'Large graph: Apply to use' : 'Applies as you edit' }}<template v-if="busy"> · applying…</template>
        </span>
        <button v-if="mode === 'manual'" type="button" class="btn small primary fd-apply" :disabled="!dirty || busy" @click="$emit('apply')">Apply</button>
        <button type="button" class="ex-close fd-close" aria-label="Close the filter" title="Close (Esc)" @click="$emit('close')">×</button>
      </div>
      <div class="fd-body">
        <div v-if="!model.enabled" class="notice faint">The filter is off: the drawing shows everything. Its labels and conditions are kept.</div>
        <div v-if="source === 'result'" class="notice faint">The query result is filtered by label only: conditions apply to the whole graph and neighbourhood views.</div>
        <div v-else-if="source === 'labels'" class="notice faint">The label overview draws hidden labels faded; conditions apply to the whole graph and neighbourhood views.</div>
        <div v-if="generalError" class="notice" role="alert">{{ generalError }}</div>
        <div v-if="loadError" class="notice" role="alert">{{ loadError }}</div>
        <div v-if="loading" class="faint fd-loading">{{ loading === 'labels' ? 'Reading the labels…' : 'Reading the property keys (schema)…' }}</div>
        <section v-for="side in [{ field: 'vertices', title: 'Vertices' }, { field: 'edges', title: 'Edges' }]" :key="side.field" class="fd-side">
          <div class="fd-side-head">
            <span class="fd-side-title">{{ side.title }}</span>
            <span class="fd-all"><a href="#" @click.prevent="all(side.field, true)">all</a> · <a href="#" @click.prevent="all(side.field, false)">none</a></span>
          </div>
          <div v-if="!rows[side.field].length && !loading" class="faint fd-empty">No {{ side.field === 'vertices' ? 'vertex' : 'edge' }} labels.</div>
          <div v-if="(side.field === 'vertices' ? vertexLabelTotal : edgeLabelTotal) > rows[side.field].length && rows[side.field].length" class="faint fd-partial">
            The graph has {{ fmt(side.field === 'vertices' ? vertexLabelTotal : edgeLabelTotal) }} {{ side.field === 'vertices' ? 'vertex' : 'edge' }} labels;
            the {{ fmt(rows[side.field].length) }} largest are listed. The labels not listed stay visible.</div>
          <div v-for="row in rows[side.field]" :key="row.label" class="fd-label" :class="{ off: !row.on }">
            <label class="fd-label-line">
              <input type="checkbox" :checked="row.on" @change="setOn(side.field, row.label, $event.target.checked)">
              <span v-if="side.field === 'vertices'" class="fd-swatch" aria-hidden="true" :class="swatchClass(row)"
                    :style="vertexSwatches[row.label] && vertexSwatches[row.label].color ? { '--sw': vertexSwatches[row.label].color } : null"
                    :title="swatchTitle(row)"></span>
              <span class="fd-label-name">{{ row.label === '' ? '(no label)' : row.label }}</span>
              <span class="fd-count faint">({{ fmt(row.count) }})</span>
            </label>
            <template v-if="row.on">
              <div v-for="c in row.conds" :key="c.index" class="fd-cond" :class="{ 'has-problem': !!c.problem }">
                <div class="fd-cond-line">
                  <input class="fd-key" type="text" :value="c.key" :list="'fk-' + side.field + '-' + row.label" placeholder="key" spellcheck="false"
                         :data-cond="side.field + '|' + row.label" :aria-label="'Property key of condition ' + (c.index + 1) + ' on ' + row.label"
                         @change="changeCondition(side.field, row.label, c.index, { key: $event.target.value.trim() })">
                  <select class="fd-op" :value="c.op" :aria-label="'Operator of condition ' + (c.index + 1) + ' on ' + row.label"
                          @change="changeCondition(side.field, row.label, c.index, { op: $event.target.value })">
                    <option v-for="o in c.ops" :key="o.id" :value="o.id">{{ o.label }}</option>
                  </select>
                  <template v-if="c.arity === 1">
                    <select v-if="c.type === 'boolean'" class="fd-value" :value="c.value" aria-label="Value"
                            @change="changeCondition(side.field, row.label, c.index, { value: $event.target.value })">
                      <option value="">…</option><option value="true">true</option><option value="false">false</option>
                    </select>
                    <input v-else class="fd-value" :type="c.type === 'number' ? 'number' : 'text'" :value="c.value" spellcheck="false"
                           :placeholder="c.type === 'temporal' ? '2024-01-01' : 'value'" aria-label="Value"
                           @input="changeCondition(side.field, row.label, c.index, { value: $event.target.value })">
                  </template>
                  <template v-else-if="c.arity === 2">
                    <input class="fd-value half" :type="c.type === 'number' ? 'number' : 'text'" :value="c.value" placeholder="from" aria-label="From (inclusive)"
                           @input="changeCondition(side.field, row.label, c.index, { value: $event.target.value })">
                    <input class="fd-value half" :type="c.type === 'number' ? 'number' : 'text'" :value="c.value2" placeholder="to" aria-label="To (exclusive)"
                           @input="changeCondition(side.field, row.label, c.index, { value2: $event.target.value })">
                  </template>
                  <button type="button" class="fd-remove" :aria-label="'Remove condition ' + (c.index + 1) + ' on ' + row.label" title="Remove this condition"
                          @click="removeCondition(side.field, row.label, c.index)">✕</button>
                </div>
                <div v-if="c.key && TYPE_HINT[c.type]" class="fd-type faint">{{ TYPE_HINT[c.type] }}<template v-if="c.arity === 2"> · from inclusive, to exclusive</template></div>
                <div v-if="c.problem" class="fd-problem" role="alert" :title="conditionErrorText(c.problem).help">{{ conditionErrorText(c.problem).text }}</div>
              </div>
              <datalist :id="'fk-' + side.field + '-' + row.label"><option v-for="k in row.keys" :key="k" :value="k"></option></datalist>
              <button type="button" class="fd-add" @click="addCondition(side.field, row.label, row.keys)">+ condition</button>
            </template>
          </div>
        </section>
        <label class="fd-isolated"><input type="checkbox" :checked="model.hideIsolated" @change="setHideIsolated($event.target.checked)"> Hide vertices without visible edges</label>
      </div>
      <div class="fd-foot">
        <a href="#" @click.prevent="$emit('open-query')" title="Open the query that selects what this filter shows in a new Query tab">Open as query</a>
        · <a href="#" @click.prevent="$emit('reset')" title="Delete every condition and show every label">Reset</a>
      </div>
    </div>
  `,
};
