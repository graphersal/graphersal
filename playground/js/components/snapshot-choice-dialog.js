// "Download current state" of a store that keeps embedding models: with them or without them, the
// models listed with their sizes. `ask(choice)` (`snapshotModelChoice` in js/store-model.js) answers
// 'with', 'without', or null when the dialog is closed; nothing is downloaded then.

import { ref } from 'vue';

export const SnapshotChoiceDialog = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const choice = ref(null);
    let settle = null;

    function answer(value) {
      if (settle) settle(value);
      settle = null;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    function ask(next) {
      answer(null);
      choice.value = next;
      if (dlg.value) dlg.value.showModal();
      return new Promise((resolve) => { settle = resolve; });
    }

    expose({ ask });
    return { dlg, choice, answer };
  },
  template: `
  <dialog ref="dlg" aria-labelledby="snapshot-choice-title" @click.self="answer(null)" @close="answer(null)" style="max-width:520px">
    <div class="dialog-head">
      <h2 id="snapshot-choice-title">Download current state</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="answer(null)" aria-label="Close">×</button>
    </div>
    <div class="dialog-body" v-if="choice">
      <p>The store keeps embedding models. Should they go into the file?</p>
      <ul>
        <li v-for="row in choice.rows" :key="row.id"><b>{{ row.repo }}</b> <span class="muted">{{ row.size }}</span>
          <span v-if="row.problem" class="badge danger">{{ row.problem }}</span></li>
      </ul>
      <p class="muted">Without them the file stays small, and a store made from it cannot embed new text until the
        model is added again. With them, handing the file on redistributes the models under their licences.</p>
      <div style="display:flex;gap:8px;margin-top:18px">
        <span class="spacer"></span>
        <button type="button" class="btn" @click="answer(null)">Cancel</button>
        <button type="button" class="btn" @click="answer('without')">Without models</button>
        <button type="button" class="btn primary" :disabled="!choice.canInclude" @click="answer('with')">With models</button>
      </div>
    </div>
  </dialog>`,
};
