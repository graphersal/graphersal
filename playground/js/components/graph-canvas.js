// A graph drawing: a renderer (js/graph/renderer.js: the Detailed one, Cytoscape, or the Fast one,
// sigma.js on WebGL) driven by the graph controller (js/graph/controller.js), plus the legend and
// the inspector. It talks to the renderer interface only, never to a drawing engine.
//
// Data drawings (`kind: 'data'`) take the engine's compact graph view (js/graph-data.js): ids,
// labels and captions only; the inspector fetches the clicked element's properties from the
// engine (`engine.element(kind, id)`). The schema drawing (`kind: 'schema'`) takes the schema
// view's `{nodes, edges}` with their property types and is always Detailed.
//
// Tier: the `tier` prop ('detailed' | 'fast', chosen by the panel with js/graph/tiers.js). A
// change swaps the renderer under the same data and keeps the positions; the Fast renderer's
// libraries load on first use. When it cannot start (no WebGL, offline) `tier-failed` says why
// and the drawing stays Detailed. `renderer` reports the active renderer's capabilities.
//
// Editing: the inspector's pencil opens the property editor (components/element-editor.js) on a
// data element; it is disabled with the reason when the graph cannot be written (a read-only
// view, a backup, maintenance mode: `elementForEdit().readOnly`) or a query runs. After a save
// the inspector is read again, and a redrawn drawing re-inspects the element it showed.
//
// Gestures (every tier): click = inspector (and picks the vertex, `pick`), double click on a
// vertex = select it with its neighbours (again: one more hop), double click on empty space = fit,
// Esc on the focused drawing = clear the double-click selection and the panel's result highlight
// (`clear-highlight`; a click on the drawing gives it the focus).
//
// Edit mode (`editMode`, js/graph/edit-mode.js): double click on empty space = a new vertex there
// (the creation dialog of components/element-editor.js), a drag from a vertex to another = a new
// edge (the renderer draws the rubber band and reports the pair), double click on an element =
// its property editor, Delete (or the inspector's delete button) = delete the selected element
// after a confirmation that says how many edges go with a vertex. Adding needs a renderer with
// `capabilities.addElements` (Detailed); the Fast tier edits and deletes only. Each change is one
// unit in the engine; the canvas emits `edited` (the change, for the panel to patch its drawing:
// js/graph/edit-mode.js `applyEdit`) and then reloads the panels (`elementEdited`). The next
// redraws keep the positions (`carryEditPositions`): the new vertex appears where it was asked
// for, the view (zoom, pan) stays, no layout runs.
//
// Layout: small drawings use the renderer's built-in layouts; above the `cose` cap (or with the
// "Force (engine)" choice, and always in the Fast tier) the engine's Barnes-Hut layout runs in the
// worker / on the dev server, with progress (`layout-state`) and `cancelLayout()`; a running one
// is registered with the store's `layoutJobs`, so the toolbar's Stop cancels it too.

import { ref, watch, onMounted, onBeforeUnmount, computed, markRaw } from 'vue';
import { state, isDark, layoutJobs, elementEdited } from '../store.js';
import { formatValue, debounce } from '../util.js';
import { legendLabels, inspectorRows, neighbourhood, idsOfIndices } from '../graph-data.js';
import { rendererFactory } from '../graph/renderer.js';
import { GraphController } from '../graph/controller.js';
import { TIER_TUNING } from '../graph/tiers.js';
import { labelPalette, paletteLabels, OTHER_SLOT, PALETTE_SIZE } from '../graph/palette.js';
import { ElementEditor } from './element-editor.js';
import { ValueText } from './value-text.js';
import { EDIT_TUNING, carryEditPositions, deleteConfirmText } from '../graph/edit-mode.js';
import { isClearKey } from '../graph/highlight.js';

const pick4 = (e) => ({ key: e.key, altKey: e.altKey, ctrlKey: e.ctrlKey, metaKey: e.metaKey, shiftKey: e.shiftKey });

/** The layout menu: the renderer's built-in layouts and the engine's force layout. */
export const LAYOUTS = [
  { id: 'cose', label: 'Force (cose)' },
  { id: 'engine', label: 'Force (engine, Barnes-Hut)' },
  { id: 'circle', label: 'Circle' },
  { id: 'concentric', label: 'Concentric' },
  { id: 'breadthfirst', label: 'Tree (breadth-first)' },
  { id: 'grid', label: 'Grid' },
];

/** The inspector rows of a schema element (a label with its property types). */
function schemaRows(d) {
  if (!d) return [];
  const out = [['id', d.id], ['label', d.label || '(none)']];
  if (d.count !== undefined) out.push(['elements', d.count]);
  for (const [k, v] of Object.entries(d.properties || {})) out.push([k, formatValue(v)]);
  return out;
}

/** The labels a drawing colours, in the order they get palette slots. */
const message = (err) => (err && err.message ? err.message : String(err));

export const GraphCanvas = {
  components: { ElementEditor, ValueText },
  props: {
    /**
     * `kind: 'data'`: the engine's compact graph view (graph-data.js); `kind: 'schema'`:
     * `{nodes:[{id,label,properties,count?}], edges:[{id,label,source,target,properties}]}`.
     */
    elements: { type: Object, default: null },
    layout: { type: String, default: 'cose' },
    kind: { type: String, default: 'data' },
    /** The renderer tier: 'detailed' | 'fast' (the schema drawing is always detailed). */
    tier: { type: String, default: 'detailed' },
    edgeLabels: { type: Boolean, default: true },
    /** Ids of nodes/edges to highlight; when set, everything else is faded. */
    highlight: { type: Object, default: null },
    emptyText: { type: String, default: 'Nothing to draw.' },
    /** Show each vertex id on its node ("1 · marko"). */
    showIds: { type: Boolean, default: false },
    /** Show each edge id on its edge ("7 · knows"). */
    edgeIds: { type: Boolean, default: false },
    /**
     * Double click on a vertex asks the panel to expand it from the engine (`expand`, with the
     * vertex index) instead of selecting its neighbours within the drawing.
     */
    expandFromEngine: { type: Boolean, default: false },
    /** Positions (`x, y` per vertex) to draw the next `elements` at, without a layout. */
    initialPositions: { type: Object, default: null },
    /** Edit mode (the panel decides whether it is allowed: js/graph/edit-mode.js `editModeState`). */
    editMode: { type: Boolean, default: false },
    /** Vertex size by degree: a scale in [0, 1] per vertex of `elements` (js/graph/size-by.js), or null. */
    nodeScale: { type: Object, default: null },
  },
  emits: ['layout-note', 'layout-state', 'renderer', 'tier-failed', 'pick', 'activate', 'expand', 'edited', 'clear-highlight'],
  setup(props, { expose, emit }) {
    const host = ref(null);
    const editor = ref(null);
    // The inspector: {kind, id, title, rows, loading, error, readOnly}; `readOnly`: why the
    // element cannot be edited (null: it can; undefined: not known yet).
    const selected = ref(null);
    const failed = ref(null); // no renderer could be created
    let renderer = null;
    let tierNow = null; // the tier of `renderer`
    let controller = null;
    let observer = null;
    let userMoved = false; // the user zoomed/panned since the last automatic fit
    let inspectSeq = 0; // the latest click: an older property answer is dropped
    let shown; // the elements last handed to the controller
    let focus = null; // the double-click selection: {seed, hops, indices}
    let layoutToken = null; // registration with the store's layoutJobs while an engine layout runs
    let queue = Promise.resolve(); // tier switches and redraws, one after the other
    // Positions kept across the redraws that follow a change made here: {from, positions, place,
    // until}; `place` maps the id of a vertex added here to where it was asked for.
    let carry = null;
    let pendingPoint = null; // where the vertex being created was asked for
    let nextInspect = null; // {kind, id}: inspect this after the next redraw (a new element)
    const editNote = ref(null); // a short message of the edit mode (a refused gesture)
    const confirmDel = ref(null); // {kind, id, title, text, busy, error}: the delete confirmation

    const isHidden = () => !host.value || !host.value.clientWidth || !host.value.clientHeight;
    const isSchema = () => props.kind === 'schema';
    const wantedTier = () => (isSchema() ? 'detailed' : props.tier);

    const legend = computed(() => {
      if (!props.elements || isSchema()) return [];
      // Re-read when the theme changes.
      const dark = isDark();
      const labels = legendLabels(props.elements);
      labelPalette.assign(paletteLabels(props.elements, props.kind));
      const out = labels.map((l) => ({ label: l || '(no label)', color: labelPalette.color(l, dark), other: labelPalette.slot(l) === OTHER_SLOT }));
      const others = out.filter((l) => l.other);
      if (others.length) {
        return [...out.filter((l) => !l.other), { label: `others (${others.length}: more than ${PALETTE_SIZE} labels share this colour)`, color: others[0].color }];
      }
      return out;
    });

    function counts() {
      const els = props.elements;
      if (!els) return [0, 0];
      if (isSchema()) return [els.nodes.length, els.edges.length];
      return [els.vertexIds.length, els.edgeIds.length];
    }

    /** The highlight to show: the double-click selection, else the panel's. */
    function currentHighlight() {
      if (focus && props.elements && !isSchema()) return idsOfIndices(props.elements, focus.indices);
      return props.highlight;
    }

    function setFocus(next) {
      focus = next;
      if (renderer) renderer.select(currentHighlight());
      emit('pick', focus ? { index: focus.seed, hops: focus.hops, count: focus.indices.length } : null);
    }

    /** Positions for `next` kept from the drawing before a change made here, or null. */
    function carried(next) {
      if (!carry || isSchema() || Date.now() > carry.until) {
        carry = null;
        return null;
      }
      const xy = carryEditPositions(carry.from, carry.positions, next, carry.place);
      if (xy) {
        carry.from = next; // a reload that follows starts from this drawing
        carry.positions = xy;
      }
      return xy;
    }

    function show() {
      if (!controller) return;
      const was = nextInspect || (selected.value && selected.value.id !== undefined && !isSchema() ? selected.value : null);
      nextInspect = null;
      confirmDel.value = null;
      const kept = carried(props.elements);
      shown = props.elements;
      selected.value = null;
      if (!kept) userMoved = false;
      focus = null;
      emit('pick', null);
      labelPalette.assign(paletteLabels(props.elements, props.kind));
      controller.show(props.elements, {
        kind: props.kind,
        layout: props.layout,
        showIds: props.showIds,
        edgeIds: props.edgeIds,
        edgeLabels: props.edgeLabels,
        nodeScale: isSchema() ? null : props.nodeScale,
        palette: labelPalette,
        positions: kept || props.initialPositions,
        keepView: !!kept,
      });
      renderer.select(currentHighlight());
      if (was) reinspect(was.kind, was.id);
    }

    // ------------------------------------------------------------------ edit mode
    const editing = () => props.editMode && !isSchema();
    const canAdd = () => editing() && !!(renderer && renderer.capabilities.addElements);

    /** A change is about to be written: the next redraws keep the drawing's positions. */
    function beginCommit() {
      const positions = renderer ? renderer.getPositions() : null;
      carry = {
        from: shown,
        positions,
        place: carry && Date.now() <= carry.until ? carry.place : new Map(),
        until: Date.now() + EDIT_TUNING.carryMs,
      };
      if (!positions || !shown) carry.from = shown || null;
    }

    /** A change was written: the panel patches its drawing, then every panel reloads. */
    async function afterCommit(change, inspectNext = null) {
      if (carry) carry.until = Date.now() + EDIT_TUNING.carryMs;
      nextInspect = inspectNext;
      emit('edited', change);
      await elementEdited();
    }

    function refuseWhileRunning() {
      if (!state.running) return false;
      editNote.value = 'A query is running: change the graph when it has finished.';
      return true;
    }

    function vertexAt(index) {
      const els = props.elements;
      return { id: String(els.vertexIds[index]), label: els.labels[els.vertexLabel[index]] || '' };
    }

    function createVertexAt(point) {
      if (!editor.value || refuseWhileRunning()) return;
      editNote.value = null;
      pendingPoint = point ? [point.x, point.y] : null;
      editor.value.openCreate('vertex');
    }

    function onConnect(from, to) {
      if (!canAdd() || !props.elements || !editor.value || refuseWhileRunning()) return;
      editNote.value = null;
      pendingPoint = null;
      editor.value.openCreate('edge', { from: vertexAt(from.index), to: vertexAt(to.index) });
    }

    function onCommitting() {
      beginCommit();
    }

    async function onCreated({ kind, drawn }) {
      if (!drawn) return;
      if (kind === 'vertex' && pendingPoint && carry) carry.place.set(String(drawn.id), pendingPoint);
      pendingPoint = null;
      await afterCommit({ type: kind === 'vertex' ? 'add-vertex' : 'add-edge', drawn }, { kind, id: String(drawn.id) });
    }

    async function onSaved({ kind, id }) {
      reinspect(kind, id);
    }

    /** Asks to delete the inspected element: the engine counts what goes with it first. */
    async function askDelete() {
      const s = selected.value;
      if (!editing() || !s || s.id === undefined || isSchema()) return;
      if (s.readOnly) {
        editNote.value = `Read-only: ${s.readOnly}`;
        return;
      }
      if (refuseWhileRunning()) return;
      confirmDel.value = { kind: s.kind, id: s.id, title: `Delete ${s.kind} ${s.id}?`, text: 'Counting its edges…', busy: true, error: null };
      try {
        const preview = await state.engine.deleteElement({ kind: s.kind, id: s.id, validateOnly: true });
        if (!confirmDel.value || confirmDel.value.id !== s.id) return;
        if (!preview.ok) {
          confirmDel.value = { ...confirmDel.value, busy: false, text: '', error: preview.errors.map((e) => e.message).join(' ') };
          return;
        }
        const t = deleteConfirmText(s.kind, s.id, s.title !== s.id ? s.title : '', preview.edges);
        confirmDel.value = { ...confirmDel.value, ...t, busy: false };
      } catch (err) {
        if (confirmDel.value) confirmDel.value = { ...confirmDel.value, busy: false, text: '', error: message(err) };
      }
    }

    async function confirmDelete() {
      const c = confirmDel.value;
      if (!c || c.busy || c.error) return;
      if (refuseWhileRunning()) return;
      confirmDel.value = { ...c, busy: true };
      beginCommit();
      try {
        const res = await state.engine.deleteElement({ kind: c.kind, id: c.id });
        if (!res.ok) {
          confirmDel.value = { ...c, busy: false, error: res.errors.map((e) => e.message).join(' ') };
          return;
        }
        confirmDel.value = null;
        selected.value = null;
        inspectSeq++;
        if (res.deleted) await afterCommit({ type: 'remove', kind: c.kind, id: String(c.id) });
      } catch (err) {
        confirmDel.value = { ...c, busy: false, error: message(err) };
      }
    }

    /**
     * Delete / Escape in edit mode, listened on the document: when the focus is on the page
     * itself (a click on the canvas leaves it there) or inside this drawing, never in a field or
     * a dialog.
     */
    function onKey(event) {
      if (!host.value) return;
      const t = event.target;
      const inField = !!(t && t.closest && t.closest('input, textarea, select, dialog, [contenteditable]'));
      const box = host.value.parentElement;
      // Esc on the drawing (it has the focus) clears the double-click selection and asks the
      // panel to clear its result highlight (js/graph/highlight.js); a delete confirmation is
      // closed first.
      if (isClearKey({ ...pick4(event), inField }) && t === host.value && !confirmDel.value && (focus || props.highlight)) {
        event.preventDefault();
        if (focus) setFocus(null);
        emit('clear-highlight');
        return;
      }
      if (!editing() || inField) return;
      if (t && t !== document.body && !(box && box.contains(t))) return;
      if ((event.key === 'Delete' || event.key === 'Backspace') && selected.value && selected.value.id !== undefined) {
        event.preventDefault();
        if (confirmDel.value && !confirmDel.value.busy && !confirmDel.value.error) confirmDelete();
        else askDelete();
      } else if (event.key === 'Escape' && confirmDel.value) {
        confirmDel.value = null;
      }
    }
    /**
     * The canvas takes the keyboard focus on a press, so Delete and Esc reach it (again on the
     * release: Cytoscape blurs the active element on its mousedown).
     */
    function focusCanvas() {
      if (host.value && !isSchema() && document.activeElement !== host.value) host.value.focus({ preventScroll: true });
    }

    const deleteState = computed(() => {
      const s = selected.value;
      if (!s || s.id === undefined) return { disabled: true, title: '' };
      if (state.running) return { disabled: true, title: 'A query is running: delete when it has finished' };
      if (s.readOnly === undefined) return { disabled: true, title: 'Checking whether the graph can be written…' };
      if (s.readOnly) return { disabled: true, title: `Read-only: ${s.readOnly}` };
      return { disabled: false, title: s.kind === 'vertex' ? 'Delete this vertex and its edges (Delete key)' : 'Delete this edge (Delete key)' };
    });

    /** Inspects the element `kind`/`id` again when the drawing has it (after a redraw or a save). */
    function reinspect(kind, id) {
      const els = props.elements;
      if (!els || isSchema()) return;
      const ids = kind === 'vertex' ? els.vertexIds : els.edgeIds;
      const index = Array.prototype.findIndex.call(ids, (x) => String(x) === String(id));
      if (index >= 0) inspect({ group: kind === 'vertex' ? 'node' : 'edge', index });
    }

    /** Why the element cannot be edited (the engine's answer), without delaying the inspector. */
    async function loadEditable(seq, kind, id) {
      if (typeof state.engine.elementForEdit !== 'function') {
        if (seq === inspectSeq && selected.value) selected.value = { ...selected.value, readOnly: 'this engine cannot edit properties (reload the page)' };
        return;
      }
      try {
        const view = await state.engine.elementForEdit(kind, id);
        if (seq !== inspectSeq || !selected.value) return;
        selected.value = { ...selected.value, readOnly: view ? view.readOnly : 'the element no longer exists' };
      } catch (err) {
        if (seq === inspectSeq && selected.value) selected.value = { ...selected.value, readOnly: message(err) };
      }
    }

    function edit() {
      const s = selected.value;
      if (!s || s.id === undefined || !editor.value) return;
      editor.value.open(s.kind, s.id);
    }
    /** The edit button's state: `{disabled, title}`. */
    const editState = computed(() => {
      const s = selected.value;
      if (!s || s.id === undefined) return { disabled: true, title: '' };
      if (state.running) return { disabled: true, title: 'A query is running: edit when it has finished' };
      if (s.readOnly === undefined) return { disabled: true, title: 'Checking whether the graph can be written…' };
      if (s.readOnly) return { disabled: true, title: `Read-only: ${s.readOnly}` };
      return { disabled: false, title: `Edit the properties of this ${s.kind}` };
    });

    /** The inspector for a clicked element: schema data at once, data properties from the engine. */
    async function inspect(target) {
      const els = props.elements;
      if (!target || !els) {
        selected.value = null;
        return;
      }
      const isNode = target.group === 'node';
      if (isSchema()) {
        const d = isNode ? els.nodes[target.index] : els.edges[target.index];
        if (!d) return;
        selected.value = { kind: isNode ? 'vertex label' : 'edge label', title: d.label || d.id, rows: schemaRows(d), loading: false, error: null };
        return;
      }
      const kind = isNode ? 'vertex' : 'edge';
      const id = isNode ? els.vertexIds[target.index] : els.edgeIds[target.index];
      const label = els.labels[isNode ? els.vertexLabel[target.index] : els.edgeLabel[target.index]];
      const seq = ++inspectSeq;
      selected.value = { kind, id, title: label || id, rows: [['id', id]], loading: true, error: null, readOnly: undefined };
      loadEditable(seq, kind, id);
      try {
        const element = await state.engine.element(kind, id);
        if (seq !== inspectSeq || !selected.value) return;
        selected.value = element
          ? { ...selected.value, title: element.label || id, rows: inspectorRows(element, (v) => v), loading: false, error: null }
          : { ...selected.value, title: id, rows: [['id', id]], loading: false, error: `This ${kind} no longer exists (reload the drawing).` };
      } catch (err) {
        if (seq !== inspectSeq || !selected.value) return;
        selected.value = { ...selected.value, loading: false, error: message(err) };
      }
    }

    function onClick(target) {
      inspect(target);
      if (isSchema()) return;
      if (!target) {
        if (focus) setFocus(null);
        else emit('pick', null);
        return;
      }
      if (target.group !== 'node') return;
      // A click inside the double-click selection keeps it; elsewhere it picks that vertex.
      if (focus && focus.indices.includes(target.index)) return;
      if (focus) setFocus(null);
      emit('pick', { index: target.index, hops: 1, count: 0 });
    }

    function onDoubleClick(target, point = null) {
      if (editing() && !target && canAdd() && point) {
        createVertexAt(point); // edit mode: a new vertex where the user double-clicked
        return;
      }
      if (editing() && target && props.elements) {
        // Edit mode: the element's property editor.
        inspect(target);
        const kind = target.group === 'node' ? 'vertex' : 'edge';
        const id = kind === 'vertex' ? props.elements.vertexIds[target.index] : props.elements.edgeIds[target.index];
        if (editor.value && !refuseWhileRunning()) editor.value.open(kind, id);
        return;
      }
      if (!target) {
        if (editing() && !canAdd()) editNote.value = 'The Fast tier cannot add vertices: switch to Detailed to add them.';
        fitView(); // double click on empty canvas fits, like the Fit button and the Mermaid view
        return;
      }
      if (isSchema()) {
        emit('activate', target); // a label (or label pair) of the meta-graph: sample it
        return;
      }
      if (target.group !== 'node' || !props.elements) return;
      if (props.expandFromEngine) {
        emit('expand', { index: target.index });
        return;
      }
      const hops = focus && focus.seed === target.index ? focus.hops + 1 : 1;
      const indices = neighbourhood(props.elements, [target.index], hops, TIER_TUNING.drillMaxNodes);
      setFocus({ seed: target.index, hops, indices });
    }

    function fitView() {
      if (!renderer) return;
      renderer.fit();
      if (!isHidden()) userMoved = false;
    }

    function bind(r) {
      r.onClick(onClick);
      r.onDoubleClick(onDoubleClick);
      r.onUserMove(() => { userMoved = true; });
      r.onConnect(onConnect);
      r.setEditMode(editing());
    }

    function onControllerState(s) {
      emit('layout-note', s.note);
      emit('layout-state', s.layout);
      const running = !!(s.layout && s.layout.running);
      if (running && layoutToken === null) layoutToken = layoutJobs.add(() => { if (controller) controller.cancelLayout(); });
      if (!running && layoutToken !== null) {
        layoutJobs.remove(layoutToken);
        layoutToken = null;
      }
    }

    /**
     * Creates the renderer of `tier` in the host (and makes it the current one). When the Fast
     * renderer fails to start (no WebGL context), `fallback` (the Detailed constructor) draws
     * instead and `tier-failed` says why.
     */
    function construct(make, tier, fallback) {
      let next;
      try {
        next = make(host.value, { dark: isDark() });
        tierNow = tier;
      } catch (err) {
        if (!fallback) throw err;
        emit('tier-failed', message(err));
        next = fallback(host.value, { dark: isDark() });
        tierNow = 'detailed';
      }
      renderer = markRaw(next);
      bind(renderer);
      emit('renderer', renderer.capabilities);
      return renderer;
    }

    /**
     * Brings the drawing up to date: the wanted tier's renderer (loaded and swapped in when it
     * changed, keeping the positions when the data is the same) and the current data.
     */
    async function sync() {
      if (!controller) return;
      const want = wantedTier();
      if (want !== tierNow) {
        let make;
        try {
          make = await rendererFactory(want);
        } catch (err) {
          if (want === 'fast') emit('tier-failed', message(err));
          else failed.value = message(err);
          return;
        }
        const fallback = want === 'fast' ? await rendererFactory('detailed').catch(() => null) : null;
        if (!controller || !host.value) return;
        if (wantedTier() !== want) return sync(); // changed again while loading
        const sameData = props.elements === shown;
        controller.replaceRenderer(() => construct(make, want, fallback), { redraw: sameData });
        if (sameData) {
          renderer.select(currentHighlight());
          return;
        }
      }
      if (props.elements !== shown) show();
    }

    const schedule = () => { queue = queue.then(sync, sync); return queue; };

    onMounted(async () => {
      let want = wantedTier();
      let make;
      try {
        make = await rendererFactory(want);
      } catch (err) {
        if (want !== 'fast') {
          failed.value = message(err);
          return;
        }
        emit('tier-failed', message(err));
        want = 'detailed';
        try {
          make = await rendererFactory(want);
        } catch (err2) {
          failed.value = message(err2);
          return;
        }
      }
      const fallback = want === 'fast' ? await rendererFactory('detailed').catch(() => null) : null;
      if (!host.value) return; // unmounted meanwhile
      try {
        construct(make, want, fallback);
      } catch (err) {
        failed.value = message(err);
        return;
      }
      controller = markRaw(new GraphController({ renderer, engine: state.engine, isHidden, onState: onControllerState }));
      let wasHidden = isHidden();
      const onResize = debounce(() => {
        if (!renderer) return;
        if (isHidden()) {
          wasHidden = true;
          return;
        }
        renderer.resize();
        if (controller.pending !== null) controller.resume(); // skipped while the canvas had no size
        else if (wasHidden || !userMoved) fitView(); // keep a view the user zoomed/panned by hand
        wasHidden = false;
      }, 80);
      observer = new ResizeObserver(onResize);
      observer.observe(host.value);
      show();
      schedule(); // the tier may have changed while the first renderer loaded
    });

    document.addEventListener('keydown', onKey);
    onBeforeUnmount(() => {
      document.removeEventListener('keydown', onKey);
      if (observer) observer.disconnect();
      if (layoutToken !== null) layoutJobs.remove(layoutToken);
      layoutToken = null;
      if (controller) controller.destroy();
      controller = null;
      renderer = null;
    });

    watch(() => [props.elements, wantedTier()], schedule);
    watch(() => props.layout, (name) => { if (controller) { userMoved = false; controller.applyLayout(name); } });
    watch(() => props.highlight, () => { if (renderer) renderer.select(currentHighlight()); });
    watch(() => [props.editMode, props.kind], () => {
      if (renderer) renderer.setEditMode(editing());
      if (!editing()) confirmDel.value = null;
      editNote.value = null;
    });
    watch(() => [props.edgeLabels, props.showIds, props.edgeIds], () => {
      if (controller) controller.setOptions({ edgeLabels: props.edgeLabels, showIds: props.showIds, edgeIds: props.edgeIds });
    });
    watch(() => props.nodeScale, (scale) => {
      // A new drawing brings its own scales with show(); this is a change of the same drawing.
      if (controller && props.elements === shown && !isSchema()) controller.setOptions({ nodeScale: scale });
    });
    watch(() => [state.theme, state.systemDark], () => {
      // Wait for the theme attribute to apply before reading the CSS tokens.
      requestAnimationFrame(() => { if (renderer) renderer.setTheme(isDark()); });
    });

    async function exportPng(options) {
      if (!renderer) throw new Error('there is nothing to export');
      return renderer.exportPng(options);
    }

    expose({
      fit: fitView,
      relayout: () => { if (controller) { userMoved = false; return controller.applyLayout(props.layout); } return null; },
      cancelLayout: () => { if (controller) controller.cancelLayout(); },
      exportPng,
      /**
       * The next drawings (within a few seconds) keep the current positions of the vertices they
       * share with this one: the same view read again with more columns (vertex size by degree).
       */
      keepPositions: () => beginCommit(),
      /** The drawing's positions (`x, y` per vertex), or null before it is laid out. */
      positions: () => (renderer ? renderer.getPositions() : null),
      /** The vertex indices of the double-click selection (null: none). */
      focusIndices: () => (focus ? focus.indices.slice() : null),
      /** Clears the double-click selection (the drawing shows the panel's highlight, or none). */
      clearFocus: () => { if (focus) setFocus(null); },
    });

    const isEmpty = computed(() => counts()[0] === 0);

    return {
      host, editor, selected, legend, failed, isEmpty, close: () => { selected.value = null; confirmDel.value = null; inspectSeq++; }, isDark,
      edit, editState, onSaved, onCreated, onCommitting, deleteState, askDelete, confirmDelete, confirmDel, editNote, onKey, focusCanvas,
      editing: computed(() => props.editMode && props.kind !== 'schema'), isSchemaKind: computed(() => props.kind === 'schema'),
    };
  },
  template: `
  <div class="cy-host">
    <div class="cy-canvas" ref="host" role="img" :tabindex="isSchemaKind ? -1 : 0" @pointerdown.capture="focusCanvas" @pointerup.capture="focusCanvas"
         :aria-label="(editing ? 'Graph drawing, edit mode' : 'Graph drawing') + (highlight ? ' (result highlighted: Esc shows the graph normally)' : '')"></div>
    <div v-if="failed" class="graph-empty">{{ failed }}</div>
    <div v-else-if="isEmpty" class="graph-empty">{{ emptyText }}</div>
    <div class="graph-legend" v-if="legend.length && !isEmpty">
      <span v-for="l in legend" :key="l.label" class="legend-item"><span class="legend-swatch" :style="{ background: l.color }"></span>{{ l.label }}</span>
    </div>
    <div v-if="selected" class="inspector" @keydown.esc="close">
      <div class="inspector-head">
        <span class="badge">{{ selected.kind }}</span>
        <span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{{ selected.title }}</span>
        <span class="spacer"></span>
        <span v-if="selected.id !== undefined" :title="editState.title" class="inspector-edit"><button class="icon-btn" @click="edit" :disabled="editState.disabled"
                :aria-label="'Edit properties' + (editState.disabled ? ' (' + editState.title + ')' : '')">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
            <path d="M11 2.5l2.5 2.5L6 12.5H3.5V10z" stroke-linejoin="round"/><path d="M9.5 4l2.5 2.5"/>
          </svg>
        </button></span>
        <span v-if="editing && selected.id !== undefined" :title="deleteState.title" class="inspector-edit"><button class="icon-btn danger" @click="askDelete" :disabled="deleteState.disabled"
                :aria-label="'Delete this ' + selected.kind + (deleteState.disabled ? ' (' + deleteState.title + ')' : '')">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
            <path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.7 8.5h5.6l.7-8.5" stroke-linejoin="round"/>
          </svg>
        </button></span>
        <button class="icon-btn" @click="close" aria-label="Close">×</button>
      </div>
      <div v-if="confirmDel" class="inspector-confirm" role="alertdialog" aria-labelledby="del-title">
        <b id="del-title">{{ confirmDel.title }}</b>
        <div v-if="confirmDel.text" class="faint">{{ confirmDel.text }}</div>
        <div v-if="confirmDel.error" class="ve-error" role="alert">{{ confirmDel.error }}</div>
        <div class="inspector-confirm-buttons">
          <button type="button" class="btn small danger" @click="confirmDelete" :disabled="confirmDel.busy || !!confirmDel.error">{{ confirmDel.busy ? 'Working…' : 'Delete' }}</button>
          <button type="button" class="btn small" @click="confirmDel = null">Cancel</button>
        </div>
      </div>
      <dl>
        <template v-for="r in selected.rows" :key="selected.kind + ':' + selected.id + ':' + r[0]"><dt>{{ r[0] }}</dt><dd><value-text :value="r[1]" :name="r[0]" copy></value-text></dd></template>
      </dl>
      <div v-if="selected.loading" class="faint inspector-note">Loading properties…</div>
      <div v-if="selected.error" class="inspector-note" role="alert">{{ selected.error }}</div>
      <slot name="inspector-actions" :element="selected"></slot>
    </div>
    <div v-if="editNote" class="graph-edit-note notice" role="status">{{ editNote }} <a href="#" @click.prevent="editNote = null">Dismiss</a></div>
    <element-editor ref="editor" @saved="onSaved" @created="onCreated" @committing="onCommitting"></element-editor>
  </div>
  `,
};
