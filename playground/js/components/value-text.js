// One property value in an element view (the graph inspector, a result table cell): short values
// as they are; a long text or a vector collapsed to a summary with an "expand" button (and
// "collapse" back), rules and the setting `valueMaxChars` in js/value-view.js. With `copy`, a
// copy button copies the FULL value, collapsed or not.

import { ref, computed, watch } from 'vue';
import { state } from '../store.js';
import { valueView } from '../value-view.js';
import { CopyButton } from './panel-frame.js';

export const ValueText = {
  components: { CopyButton },
  props: {
    value: { default: null },
    /** What the value is (for the buttons' accessible names), e.g. the property key. */
    name: { type: String, default: 'value' },
    /** Offer a copy button (the full value) on a collapsible value. */
    copy: { type: Boolean, default: false },
  },
  setup(props) {
    const expanded = ref(false);
    const view = computed(() => valueView(props.value, { maxChars: state.settings.valueMaxChars }));
    watch(() => props.value, () => { expanded.value = false; });
    const toggleLabel = computed(() => `${expanded.value ? 'Collapse' : 'Expand'} ${props.name}${view.value.kind === 'vector' ? ' (vector)' : ''}`);
    return { expanded, view, toggleLabel, fullText: () => view.value.full };
  },
  template: `
  <span v-if="view.short === null" class="value-text">{{ view.full }}</span>
  <span v-else class="value-text collapsible" :class="{ expanded, vector: view.kind === 'vector' }">
    <span class="value-body">{{ expanded ? view.full : view.short }}</span>
    <span class="value-tools">
      <button type="button" class="value-toggle" :aria-expanded="String(expanded)" :aria-label="toggleLabel"
              :title="expanded ? 'Show the short form' : 'Show the full value (' + view.full.length.toLocaleString('en-US') + ' characters)'"
              @click.stop="expanded = !expanded">{{ expanded ? 'collapse' : 'expand' }}</button>
      <copy-button v-if="copy" class="value-copy" :get-text="fullText" :label="'Copy the full ' + name"></copy-button>
    </span>
  </span>
  `,
};
