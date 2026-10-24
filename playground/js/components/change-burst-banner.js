// The change feed's burst banner (dev server only): while the graph changes fast elsewhere (a bulk
// load over MCP, a script, another tab), the page reloads at most every 30 s instead of after
// every change (js/change-burst.js, state `state.changeBurst`). The banner says so and offers
// Refresh now and Pause auto refresh / Resume. Its kind changes are announced in a polite live
// region (`state.changeBurstAnnounce`); the counts that change every poll are not.

import { ref } from 'vue';
import { state, refreshFromFeedNow, pauseFeedRefresh, resumeFeedRefresh } from '../store.js';

export const ChangeBurstBanner = {
  setup() {
    const busy = ref(false);
    async function guarded(fn) {
      if (busy.value) return;
      busy.value = true;
      try {
        await fn();
      } finally {
        busy.value = false;
      }
    }
    return {
      state,
      busy,
      refreshNow: () => guarded(refreshFromFeedNow),
      pause: pauseFeedRefresh,
      resume: () => guarded(resumeFeedRefresh),
    };
  },
  template: `
    <div v-if="state.changeBurst" class="notice info change-burst-banner" data-testid="change-burst">
      <b>{{ state.changeBurst.text }}.</b>
      <button class="btn small" :disabled="busy" :aria-busy="String(busy)" @click="refreshNow">{{ busy ? 'Refreshing…' : 'Refresh now' }}</button>
      <button v-if="state.changeBurst.kind === 'burst'" class="btn small" @click="pause" title="No automatic reloads until Resume">Pause auto refresh</button>
      <button v-else class="btn small" :disabled="busy" @click="resume">Resume</button>
    </div>
    <div class="sr-only" role="status" aria-live="polite">{{ state.changeBurstAnnounce }}</div>`,
};
