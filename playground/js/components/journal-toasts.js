// Journal notifications: a small non-blocking popup in the top-right corner for every new
// record of the served graph's system journal at or above the setting's level (Settings ▸
// Notifications, `settings.journalToasts`, default notice). Coloured by level; warn and error
// stay until closed, the others close after 30 s (paused while the pointer is on them). At most
// three at once, the rest as one "+N more journal records" toast; repeats of a kind and message
// merge with a counter. A click opens the System journal panel at the record. Records come from
// the shared journal follower (store.js `journalFollower`), which starts at the journal's end:
// records written before the page opened never pop up. Model js/journal-toasts.js.

import { reactive, watch, onMounted, onBeforeUnmount } from 'vue';
import { state, journalFollower, openJournalAt } from '../store.js';
import { createToasts, normalizeMinLevel, moreText } from '../journal-toasts.js';

export const JournalToasts = {
  setup() {
    const model = createToasts();
    const v = reactive({ toasts: [], more: 0, moreRole: 'status' });
    let unsubscribe = null;
    let timer = null;

    function render() {
      Object.assign(v, model.view());
      if (timer) clearTimeout(timer);
      timer = null;
      const next = model.nextDeadline();
      if (next != null) {
        timer = setTimeout(() => {
          timer = null;
          model.expire();
          render();
        }, Math.max(0, next - Date.now()) + 20);
      }
    }
    const minLevel = () => normalizeMinLevel(state.settings.journalToasts);
    function onBatch(batch) {
      if (model.push(batch.records, minLevel())) render();
    }
    function follow() {
      const on = minLevel() !== 'off' && !!state.engine;
      if (on && !unsubscribe) unsubscribe = journalFollower().subscribe(onBatch);
      if (!on && unsubscribe) {
        unsubscribe();
        unsubscribe = null;
      }
    }

    function close(id) {
      model.close(id);
      render();
    }
    function closeMore() {
      model.closeOverflow();
      render();
    }
    function hover(id, on) {
      if (model.hover(id, on)) render();
    }
    function openRecord(t) {
      openJournalAt(t);
      close(t.id);
    }
    function openMore() {
      openJournalAt(null);
      closeMore();
    }

    onMounted(follow);
    watch(() => [state.settings.journalToasts, state.engine], follow);
    // A replaced graph (a load, another store) may have another journal: take its end again
    // (records of the last moments still pop up).
    watch(() => state.graphEpoch, () => {
      if (unsubscribe) journalFollower().rebase();
    });
    onBeforeUnmount(() => {
      if (unsubscribe) unsubscribe();
      unsubscribe = null;
      if (timer) clearTimeout(timer);
    });
    return { v, close, closeMore, hover, openRecord, openMore, moreText };
  },
  template: `
  <div class="journal-toasts" aria-label="Journal notifications">
    <div v-for="t in v.toasts" :key="t.id" class="jt" :class="t.cls" :role="t.role"
         :aria-live="t.role === 'alert' ? 'assertive' : 'polite'"
         @mouseenter="hover(t.id, true)" @mouseleave="hover(t.id, false)"
         @focusin="hover(t.id, true)" @focusout="hover(t.id, false)">
      <button type="button" class="jt-body" @click="openRecord(t)"
              :title="'Open the system journal at record ' + t.seq">
        <span class="jt-head"><span class="jt-level">{{ t.level || 'record' }}</span><span class="jt-kind code">{{ t.kind }}</span><span v-if="t.count > 1" class="jt-count" :aria-label="t.count + ' times'">×{{ t.count }}</span></span>
        <span class="jt-message">{{ t.message }}</span>
      </button>
      <button type="button" class="jt-close" @click="close(t.id)" aria-label="Close notification">×</button>
    </div>
    <div v-if="v.more" class="jt jt-more" :role="v.moreRole" :aria-live="v.moreRole === 'alert' ? 'assertive' : 'polite'">
      <button type="button" class="jt-body" @click="openMore" title="Open the system journal">{{ moreText(v.more) }}</button>
      <button type="button" class="jt-close" @click="closeMore" aria-label="Dismiss the other notifications">×</button>
    </div>
  </div>
  `,
};
