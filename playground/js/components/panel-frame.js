// A workspace panel: a header (disclosure toggle, title, tabs slot, actions slot, and the shared
// refresh / maximise / hide buttons) and a body. Every panel gets the same header actions from
// here; a panel opts into "refresh" with `refresh-title` and an `@refresh` handler.

import { ref } from 'vue';
import { copyText, storage } from '../util.js';
import { DropMenu } from './drop-menu.js';

export const PanelFrame = {
  props: {
    title: { type: String, required: true },
    collapsed: { type: Boolean, default: false },
    maximized: { type: Boolean, default: false },
    shortcut: { type: String, default: '' },
    /** Tooltip of the refresh button; the button is shown only when this is set. */
    refreshTitle: { type: String, default: '' },
  },
  emits: ['toggle-collapse', 'toggle-maximize', 'hide', 'refresh'],
  template: `
  <section class="panel" :class="{ collapsed: collapsed && !maximized, maximized }" :aria-label="title">
    <header class="panel-header" @dblclick.self="maximized || $emit('toggle-collapse')">
      <!-- Disclosure convention (same as the Profile tree): collapsed = chevron right ("expand"),
           expanded = chevron down ("collapse"), whatever the split direction. -->
      <button v-if="!maximized" class="icon-btn disclosure" @click="$emit('toggle-collapse')"
              :title="(collapsed ? 'Expand panel' : 'Collapse panel') + ' (or double-click the header)'"
              :aria-label="(collapsed ? 'Expand panel ' : 'Collapse panel ') + title" :aria-expanded="String(!collapsed)">
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
          <path v-if="collapsed" d="M6 4l4 4-4 4"/><path v-else d="M4 6l4 4 4-4"/>
        </svg>
      </button>
      <!-- The title toggles too: people click the name, not the small chevron (the chevron stays the
           keyboard and screen-reader control). -->
      <span class="panel-title" :class="{ toggles: !maximized }" @click="maximized || $emit('toggle-collapse')"
            :title="(shortcut ? title + ' (' + shortcut + ')' : title) + (maximized ? '' : collapsed ? ': click to expand' : ': click to collapse')">{{ title }}</span>
      <div class="tabs" v-if="!collapsed || maximized"><slot name="tabs"></slot></div>
      <div class="panel-actions">
        <slot name="actions" v-if="!collapsed || maximized"></slot>
        <button v-if="refreshTitle" class="icon-btn" @click="$emit('refresh')" :title="refreshTitle" :aria-label="refreshTitle + ' (' + title + ')'">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
            <path d="M13 8a5 5 0 1 1-1.5-3.6"/><path d="M13 2.5v3h-3"/>
          </svg>
        </button>
        <button class="icon-btn" @click="$emit('toggle-maximize')"
                :title="maximized ? 'Restore layout (Esc or Alt+M)' : 'Maximise panel (Alt+M)'"
                :aria-label="(maximized ? 'Restore layout from ' : 'Maximise panel ') + title" :aria-pressed="String(maximized)">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
            <template v-if="maximized"><path d="M2.5 9.5h4v4M6.5 9.5L2 14M13.5 6.5h-4v-4M9.5 6.5L14 2"/></template>
            <template v-else><path d="M9.5 2.5h4v4M13.5 2.5L9 7M6.5 13.5h-4v-4M2.5 13.5L7 9"/></template>
          </svg>
        </button>
        <button v-if="!maximized" class="icon-btn" @click="$emit('hide')" title="Hide panel (the View menu brings it back)" :aria-label="'Hide panel ' + title">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"/></svg>
        </button>
      </div>
    </header>
    <div class="panel-body"><slot></slot></div>
  </section>
  `,
};

/** The tab buttons used inside a panel header. */
export const PanelTabs = {
  props: { tabs: { type: Array, required: true }, modelValue: { type: String, required: true } },
  emits: ['update:modelValue'],
  template: `
    <button v-for="t in tabs" :key="t.id" class="tab" :class="{ active: t.id === modelValue }"
            role="tab" :title="t.title || null" :aria-selected="String(t.id === modelValue)" @click="$emit('update:modelValue', t.id)">
      {{ t.label }}
    </button>
  `,
};

/**
 * The "Fit" button of every drawing (graph, schema graph, Mermaid): same icon, text and tooltip.
 * `compact` shows the icon only (a narrow Graph toolbar).
 */
export const FitButton = {
  props: { compact: { type: Boolean, default: false } },
  emits: ['fit'],
  template: `
    <button class="btn small" @click="$emit('fit')" title="Fit the drawing to the panel (or double-click empty space)"
            aria-label="Fit the drawing to the panel">
      <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
        <path d="M2 5.5V2h3.5M10.5 2H14v3.5M14 10.5V14h-3.5M5.5 14H2v-3.5"/>
      </svg>
      <template v-if="!compact">Fit</template>
    </button>
  `,
};

/**
 * The copy button of the panels: copies what `getText()` returns (what the active view shows),
 * `label` says what ("Copy as Markdown table"), and a check mark confirms it.
 */
export const CopyButton = {
  props: {
    getText: { type: Function, required: true },
    label: { type: String, default: 'Copy' },
    disabled: { type: Boolean, default: false },
  },
  setup(props) {
    const copied = ref(false);
    const failed = ref(false);
    async function copy() {
      const ok = await copyText(props.getText() ?? '');
      copied.value = ok;
      failed.value = !ok;
      setTimeout(() => { copied.value = false; failed.value = false; }, 1400);
    }
    return { copied, failed, copy };
  },
  template: `
    <button class="icon-btn" @click="copy" :disabled="disabled"
            :title="copied ? 'Copied' : failed ? 'The browser blocked clipboard access' : label" :aria-label="label">
      <span v-if="copied" aria-hidden="true">✓</span>
      <span v-else-if="failed" aria-hidden="true">!</span>
      <svg v-else viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><rect x="5" y="5" width="8" height="9" rx="1.5"/><path d="M3 11V3.5A1.5 1.5 0 0 1 4.5 2H10"/></svg>
    </button>
  `,
};

/**
 * The "Export" menu of every drawing toolbar (same icon everywhere). `formats` lists what the
 * drawing offers, e.g. [{id: 'png', label: 'PNG (whole drawing)'}, {id: 'svg', label: 'SVG'}];
 * choosing one emits `export(id, {transparent})`. The background choice is remembered. `compact`
 * shows the icon and the arrow only (a narrow Graph toolbar).
 */
export const ExportMenu = {
  props: { formats: { type: Array, required: true }, busy: { type: Boolean, default: false }, compact: { type: Boolean, default: false } },
  emits: ['export'],
  components: { DropMenu },
  setup(props, { emit }) {
    const transparent = ref(storage.get('export.transparent', false));
    function choose(id) {
      storage.set('export.transparent', transparent.value);
      emit('export', id, { transparent: transparent.value });
    }
    return { transparent, choose };
  },
  template: `
    <drop-menu title="Export the drawing as an image" aria-label="Export the drawing" menu-style="min-width: 240px">
      <template #trigger>
        <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
          <path d="M8 2v8M4.5 6.5L8 10l3.5-3.5M2.5 11v2.5h11V11"/>
        </svg>
        {{ compact ? '▾' : (busy ? 'Exporting…' : 'Export') + ' ▾' }}
      </template>
      <div class="menu-title">Export as</div>
      <button v-for="f in formats" :key="f.id" class="menu-item" :disabled="busy" @click="choose(f.id)">{{ f.label }}</button>
      <div class="menu-sep"></div>
      <label class="menu-item"><input type="checkbox" v-model="transparent"> Transparent background</label>
      <div class="menu-item faint" style="cursor:default;font-size:12px">Otherwise the background of the current theme.</div>
    </drop-menu>
  `,
};
