// Settings dialog: display limits (like the CLI's --max-rows/--max-items/--no-limit), when element
// views collapse long values (js/value-view.js), the step-name spelling (--spelling) and the graph
// drawing limits. Saved in localStorage.

import { ref, reactive } from 'vue';
import { state, saveSettings, DEFAULT_SETTINGS } from '../store.js';
import { TIER_CHOICES, TIER_TUNING } from '../graph/tiers.js';
import { VALUE_MAX_CHARS, VALUE_VIEW_TUNING, maxCharsError } from '../value-view.js';

export const SettingsDialog = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const form = reactive({ ...state.settings });
    const error = ref(null);

    function open() {
      Object.assign(form, state.settings);
      error.value = null;
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function close() {
      if (dlg.value) dlg.value.close();
    }
    function apply() {
      const ints = ['maxRows', 'maxItems', 'graphMaxNodes', 'graphMaxEdges', 'graphResultMax'];
      for (const k of ints) {
        const n = Number(form[k]);
        if (!Number.isInteger(n) || n < 1) {
          error.value = 'Every limit must be a whole number of at least 1 (use "No limit" for unlimited display).';
          return;
        }
        form[k] = n;
      }
      const charsError = maxCharsError(form.valueMaxChars);
      if (charsError) {
        error.value = charsError;
        return;
      }
      form.valueMaxChars = Number(form.valueMaxChars);
      if (form.graphResultMax > TIER_TUNING.fastMax) {
        error.value = `The result graph takes at most ${fmt(TIER_TUNING.fastMax)} vertices + edges (the end of the Fast tier's range).`;
        return;
      }
      saveSettings({ ...form });
      close();
    }
    function reset() {
      Object.assign(form, DEFAULT_SETTINGS);
    }
    expose({ open, close });
    const fmt = (n) => n.toLocaleString('en-US');
    return { dlg, form, error, close, apply, reset, TIER_CHOICES, TIER_TUNING, DEFAULT_SETTINGS, VALUE_MAX_CHARS, VALUE_VIEW_TUNING, fmt };
  },
  template: `
  <dialog ref="dlg" aria-labelledby="settings-title" @click.self="close">
    <form method="dialog" @submit.prevent="apply">
      <div class="dialog-head">
        <h2 id="settings-title">Settings</h2><span class="spacer"></span>
        <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
      </div>
      <div class="dialog-body">
        <h3>Result display</h3>
        <div class="form-grid">
          <label for="s-rows">Max rows</label>
          <input id="s-rows" type="number" min="1" v-model="form.maxRows" :disabled="form.noLimit">
          <span class="hint">Top-level results shown (rows, traversers, list items). Data from to_list() is never cut.</span>
          <label for="s-items">Max items</label>
          <input id="s-items" type="number" min="1" v-model="form.maxItems" :disabled="form.noLimit">
          <span class="hint">Items shown of every nested collection (a fold() list, a group() row).</span>
          <label for="s-nolimit">No limit</label>
          <input id="s-nolimit" type="checkbox" v-model="form.noLimit" style="justify-self:start">
          <label for="s-valuechars">Collapse values above</label>
          <input id="s-valuechars" type="number" min="0" :max="VALUE_MAX_CHARS.max" v-model="form.valueMaxChars" aria-describedby="s-valuechars-hint">
          <span class="hint" id="s-valuechars-hint">Characters (default {{ VALUE_MAX_CHARS.default }}; 0 shows every value in full). A longer text, or one with more than
            {{ VALUE_VIEW_TUNING.maxLines }} lines, and a vector (a list of {{ VALUE_VIEW_TUNING.vectorMin }} or more numbers) are collapsed
            in the graph's element details and the result table, with an expand button. Copy copies the full value.</span>
        </div>
        <h3>Step names</h3>
        <div class="form-grid">
          <label for="s-spelling">Spelling</label>
          <select id="s-spelling" v-model="form.spelling">
            <option value="snake">snake_case (has_label)</option>
            <option value="camel">camelCase (hasLabel)</option>
          </select>
          <span class="hint">How the profile and error messages spell the steps. A query can set it with g.with("render.spelling", "camel").</span>
        </div>
        <h3>Graph drawing</h3>
        <div class="form-grid">
          <label for="s-nodes">Max vertices</label>
          <input id="s-nodes" type="number" min="1" v-model="form.graphMaxNodes">
          <label for="s-edges">Max edges</label>
          <input id="s-edges" type="number" min="1" v-model="form.graphMaxEdges">
          <span class="hint">Defaults {{ fmt(DEFAULT_SETTINGS.graphMaxNodes) }} and {{ fmt(DEFAULT_SETTINGS.graphMaxEdges) }}: a larger graph
            shows its labels instead (the Labels view), and neighbourhoods and samples stop there too. Above 200 vertices the engine
            lays the drawing out (seconds for tens of thousands, with progress and Stop).</span>
          <label for="s-result">Result graph</label>
          <input id="s-result" type="number" min="1" :max="TIER_TUNING.fastMax" v-model="form.graphResultMax">
          <span class="hint">Most vertices + edges of a query result the Graph panel draws (default {{ fmt(DEFAULT_SETTINGS.graphResultMax) }},
            at most {{ fmt(TIER_TUNING.fastMax) }}). The result's data is never cut.</span>
          <label for="s-tier">Drawing tier</label>
          <select id="s-tier" v-model="form.graphTier">
            <option v-for="c in TIER_CHOICES" :key="c.id" :value="c.id">{{ c.label }}</option>
          </select>
          <span class="hint">Auto draws up to {{ fmt(TIER_TUNING.fastFrom - 1) }} vertices and edges in the Detailed tier (every style,
            edge labels, curved edges, every layout) and larger drawings in the Fast tier (WebGL: smooth at
            {{ fmt(TIER_TUNING.fastMax) }} elements, vertex labels as you zoom in, edge labels on hover). Forcing Detailed above
            {{ fmt(TIER_TUNING.detailedConfirm) }} elements asks before drawing. The Graph panel has the same switch.</span>
        </div>
        <p class="muted" style="margin-top:12px;font-size:12px">If the playground freezes, open <a href="reset/">reset/</a>: it clears these
        settings, the layout and the query tabs (not the history) and starts again with the Modern sample graph.</p>
        <p v-if="error" class="badge danger" style="height:auto;padding:6px 10px;margin-top:12px;white-space:normal">{{ error }}</p>
        <div style="display:flex;gap:8px;margin-top:18px">
          <button type="button" class="btn" @click="reset">Defaults</button>
          <span class="spacer"></span>
          <button type="button" class="btn" @click="close">Cancel</button>
          <button type="submit" class="btn primary">Save</button>
        </div>
      </div>
    </form>
  </dialog>
  `,
};
