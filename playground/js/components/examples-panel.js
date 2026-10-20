// The Examples panel of the Query panel: an overlay drawer (teleported to <body>, position fixed)
// that slides in below the Examples button over the editor and results without moving the layout;
// full width on narrow screens. Model: js/examples-panel.js (tree, search, keyboard order, state).
//
// Opens on a click of the button (focus goes to the search field) and when the pointer rests on the
// button for OPEN_DELAY ms (the focus stays where it was, so a pointer passing over the button while
// typing in the editor steals nothing). Closes on Esc (a search text is cleared first), an outside
// click, choosing an example, and - when opened by hover and not used since (no click into it, no
// search text, the search field not focused) - when the pointer leaves button and panel for
// CLOSE_DELAY ms. Reopening restores the scroll position and the search text of this page view and
// marks the last opened example; collapsed groups, the last example and the browse scroll position
// are kept in localStorage (js/util.js `storage`, never throws).
//
// Keyboard: Up/Down walk the shown examples (Down from the search field starts after the last opened
// one), Enter on an example opens it, Enter in the search field opens the first match.

import { ref, computed, watch, nextTick, onBeforeUnmount } from 'vue';
import { storage } from '../util.js';
import {
  exampleId, groupExamples, searchExamples, visibleTree, flatVisible, stepSelection,
  loadPanelState, savePanelState,
} from '../examples-panel.js';
import { LAYOUT_EVENT, closeOpenMenu } from './drop-menu.js';

const OPEN_DELAY = 300;
const CLOSE_DELAY = 400;
const MARGIN = 8;
const NARROW = 640; // below this viewport width the panel takes the full width
const WIDTH = 520;

// Per page view: the search text and the scroll positions (the browse position is also stored).
const session = { query: '', searchTop: 0, browseTop: null };

const ExampleItem = {
  props: { ex: Object, hit: Object, last: Boolean },
  emits: ['choose'],
  setup(props) {
    return { id: computed(() => exampleId(props.ex)) };
  },
  template: `
    <button type="button" class="ex-item" :class="{ last }" :data-id="id" :aria-current="last ? 'true' : null"
            :title="ex.script" @click="$emit('choose', ex)">
      <span class="ex-title">
        <template v-if="hit && hit.title">{{ hit.title.before }}<mark>{{ hit.title.match }}</mark>{{ hit.title.after }}</template>
        <template v-else>{{ ex.title }}</template>
        <span v-if="last" class="ex-last">last opened</span>
      </span>
      <span v-if="hit && hit.snippet" class="ex-snippet mono">{{ hit.snippet.before }}<mark>{{ hit.snippet.match }}</mark>{{ hit.snippet.after }}</span>
    </button>
  `,
};

export const ExamplesPanel = {
  components: { ExampleItem },
  props: {
    /** The examples (js/examples.json). */
    examples: { type: Array, required: true },
  },
  emits: ['use', 'wrap-gql'],
  setup(props, { emit }) {
    const open = ref(false);
    const trigger = ref(null);
    const panel = ref(null);
    const search = ref(null);
    const list = ref(null);
    const pos = ref({});
    const query = ref(session.query);
    const stored = loadPanelState(storage);
    const collapsed = ref(stored.collapsed);
    const lastId = ref(stored.lastId);
    if (session.browseTop === null) session.browseTop = stored.scrollTop;
    // Opened by hover and not used since: leaving closes it.
    let byHover = false;
    let openTimer = null;
    let closeTimer = null;

    const tree = computed(() => groupExamples(props.examples));
    const hits = computed(() => searchExamples(props.examples, query.value));
    const shown = computed(() => visibleTree(tree.value, collapsed.value, hits.value));
    const countText = computed(() => {
      if (!hits.value) return `${props.examples.length} examples`;
      const n = hits.value.size;
      return n === 1 ? '1 match' : `${n} matches`;
    });

    function persist() {
      savePanelState(storage, { collapsed: collapsed.value, lastId: lastId.value, scrollTop: session.browseTop || 0 });
    }

    function place() {
      const t = trigger.value;
      if (!t) return;
      const r = t.getBoundingClientRect();
      if (!t.isConnected || (!r.width && !r.height)) {
        close(); // the trigger was hidden (collapsed, maximised elsewhere)
        return;
      }
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const top = Math.round(r.bottom + 4);
      const height = Math.max(160, vh - top - MARGIN);
      if (vw < NARROW) {
        pos.value = { top: `${top}px`, left: '0px', width: '100vw', height: `${height}px` };
        return;
      }
      const width = Math.min(WIDTH, vw - 2 * MARGIN);
      const left = Math.min(Math.max(MARGIN, r.right - width), vw - width - MARGIN);
      pos.value = { top: `${top}px`, left: `${Math.round(left)}px`, width: `${width}px`, height: `${height}px` };
    }

    function itemEl(id) {
      if (!list.value || id === null) return null;
      for (const el of list.value.querySelectorAll('[data-id]')) if (el.dataset.id === id) return el;
      return null;
    }

    /** Restores the scroll position; the last opened example is brought into view if needed. */
    function restoreScroll() {
      const l = list.value;
      if (!l) return;
      l.scrollTop = hits.value ? session.searchTop : session.browseTop || 0;
      const el = itemEl(lastId.value);
      if (!el) return;
      const lr = l.getBoundingClientRect();
      const er = el.getBoundingClientRect();
      if (er.top < lr.top || er.bottom > lr.bottom) {
        // Room for the next examples below it.
        l.scrollTop += er.top - lr.top - Math.min(l.clientHeight / 3, 120);
      }
    }

    function onScroll() {
      if (!list.value) return;
      if (hits.value) session.searchTop = list.value.scrollTop;
      else session.browseTop = list.value.scrollTop;
    }

    function onPointerDown(e) {
      if (panel.value && panel.value.contains(e.target)) {
        byHover = false; // used: it stays open
        return;
      }
      if (trigger.value && trigger.value.contains(e.target)) return;
      close();
    }

    function onWindowKey(e) {
      if (e.key !== 'Escape' || !open.value) return;
      e.preventDefault(); // tells other Esc handlers (restore from maximise) it was used
      if (query.value) {
        query.value = '';
        if (search.value) search.value.focus();
        return;
      }
      close();
      if (trigger.value) trigger.value.focus();
    }

    const onReflow = () => place();
    const onLayout = () => requestAnimationFrame(place);

    function listen(on) {
      const m = on ? 'addEventListener' : 'removeEventListener';
      document[m]('pointerdown', onPointerDown, true);
      window[m]('keydown', onWindowKey, true);
      window[m]('resize', onReflow);
      window[m](LAYOUT_EVENT, onLayout);
    }

    async function show(focusSearch) {
      clearTimers();
      if (open.value) {
        if (focusSearch) focusField();
        return;
      }
      closeOpenMenu();
      open.value = true;
      listen(true);
      place();
      await nextTick();
      restoreScroll();
      if (focusSearch) focusField();
    }

    function focusField() {
      if (!search.value) return;
      search.value.focus({ preventScroll: true });
      search.value.select();
    }

    function close() {
      clearTimers();
      if (!open.value) return;
      onScroll();
      open.value = false;
      byHover = false;
      listen(false);
      persist();
    }

    function clearTimers() {
      clearTimeout(openTimer);
      clearTimeout(closeTimer);
      openTimer = null;
      closeTimer = null;
    }

    function onTriggerClick() {
      if (open.value && !byHover) {
        close();
        return;
      }
      byHover = false;
      show(true);
    }

    function onEnter() {
      clearTimeout(closeTimer);
      closeTimer = null;
      if (!open.value && !openTimer) {
        openTimer = setTimeout(() => {
          openTimer = null;
          byHover = true;
          show(false);
        }, OPEN_DELAY);
      }
    }

    function onLeave() {
      clearTimeout(openTimer);
      openTimer = null;
      if (!open.value || !byHover) return;
      clearTimeout(closeTimer);
      closeTimer = setTimeout(() => {
        closeTimer = null;
        const typing = search.value && document.activeElement === search.value;
        if (byHover && !typing && !query.value) close();
      }, CLOSE_DELAY);
    }

    function choose(ex) {
      lastId.value = exampleId(ex);
      close();
      emit('use', ex);
    }

    function wrap() {
      close();
      emit('wrap-gql');
    }

    function toggle(key) {
      const next = new Set(collapsed.value);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      collapsed.value = next;
      persist();
    }

    function focusItem(id) {
      const el = itemEl(id);
      if (!el) return;
      el.focus({ preventScroll: true });
      el.scrollIntoView({ block: 'nearest' });
    }

    function onPanelKey(e) {
      if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
      const ids = flatVisible(shown.value).map(exampleId);
      const active = document.activeElement;
      const current = active && active.dataset && active.dataset.id && ids.includes(active.dataset.id)
        ? active.dataset.id
        : null;
      if (current === null && active !== search.value) return; // a group header or button: keep the default
      e.preventDefault();
      byHover = false;
      const next = stepSelection(ids, current, e.key === 'ArrowDown' ? 1 : -1, hits.value ? null : lastId.value);
      if (next === null) focusField();
      else focusItem(next);
    }

    function openFirst() {
      const first = flatVisible(shown.value)[0];
      if (hits.value && first) choose(first);
    }

    watch(query, (q, before) => {
      session.query = q;
      byHover = false;
      if (!open.value) return;
      const wasSearch = !!(before && before.trim());
      const isSearch = !!q.trim();
      if (wasSearch !== isSearch || isSearch) {
        if (isSearch) session.searchTop = 0;
        nextTick(() => {
          if (list.value) list.value.scrollTop = isSearch ? 0 : session.browseTop || 0;
        });
      }
    });

    onBeforeUnmount(close);

    return {
      open, trigger, panel, search, list, pos, query, hits, shown, countText, lastId, exampleId,
      onTriggerClick, onEnter, onLeave, onScroll, onPanelKey, openFirst, choose, wrap, toggle, close,
    };
  },
  template: `
    <button ref="trigger" type="button" class="btn small" title="Example queries and scripts"
            aria-haspopup="dialog" :aria-expanded="String(open)" aria-controls="examples-panel"
            @click="onTriggerClick" @mouseenter="onEnter" @mouseleave="onLeave">Examples ▾</button>
    <teleport to="body">
      <div v-if="open" id="examples-panel" ref="panel" class="examples-panel" role="dialog" aria-label="Examples"
           :style="pos" @mouseenter="onEnter" @mouseleave="onLeave" @keydown="onPanelKey">
        <div class="ex-head">
          <input ref="search" v-model="query" type="search" class="ex-search" spellcheck="false" autocomplete="off"
                 placeholder="Search titles and scripts, e.g. .order(" aria-label="Search examples (titles and scripts)"
                 aria-controls="examples-list" @keydown.enter.prevent="openFirst">
          <span class="ex-count faint" aria-live="polite">{{ countText }}</span>
          <button type="button" class="ex-close" aria-label="Close examples" title="Close (Esc)" @click="close()">×</button>
        </div>
        <div id="examples-list" ref="list" class="ex-list" @scroll.passive="onScroll">
          <div v-if="hits && !shown.length" class="ex-empty faint">No example title or script contains “{{ query.trim() }}”.</div>
          <section v-for="g in shown" :key="g.key" class="ex-group">
            <button type="button" class="ex-toggle" :aria-expanded="String(!g.collapsed)" :disabled="!!hits"
                    :title="hits ? 'All matching groups are shown while searching' : (g.collapsed ? 'Expand' : 'Collapse') + ' ' + g.title"
                    @click="toggle(g.key)">
              <span class="ex-caret" aria-hidden="true">{{ g.collapsed ? '▸' : '▾' }}</span>
              <span class="ex-group-name">{{ g.title }}</span>
              <span class="ex-n">{{ g.count }}</span>
            </button>
            <div v-if="!g.collapsed" class="ex-body">
              <button v-if="g.key === 'GQL' && !hits" type="button" class="ex-item ex-action" @click="wrap()"
                      title="Turn the GQL query in the editor (the selection, or the whole tab) into the script g.gql(\`...\`) that runs it; an empty tab gets a template">
                <span class="ex-title">Wrap as GQL: g.gql(\`…\`)</span>
              </button>
              <example-item v-for="ex in g.items" :key="exampleId(ex)" :ex="ex" :hit="hits && hits.get(exampleId(ex))"
                            :last="exampleId(ex) === lastId" @choose="choose"></example-item>
              <section v-for="s in g.subgroups" :key="s.key" class="ex-sub">
                <button type="button" class="ex-toggle sub" :aria-expanded="String(!s.collapsed)" :disabled="!!hits"
                        :title="hits ? 'All matching groups are shown while searching' : (s.collapsed ? 'Expand' : 'Collapse') + ' ' + s.title"
                        @click="toggle(s.key)">
                  <span class="ex-caret" aria-hidden="true">{{ s.collapsed ? '▸' : '▾' }}</span>
                  <span class="ex-group-name">{{ s.title }}</span>
                  <span class="ex-n">{{ s.count }}</span>
                </button>
                <div v-if="!s.collapsed" class="ex-body">
                  <example-item v-for="ex in s.items" :key="exampleId(ex)" :ex="ex" :hit="hits && hits.get(exampleId(ex))"
                                :last="exampleId(ex) === lastId" @choose="choose"></example-item>
                </div>
              </section>
            </div>
          </section>
        </div>
      </div>
    </teleport>
  `,
};
