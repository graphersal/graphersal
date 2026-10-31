// The System journal panel (Catalog ▸ System journal…): the served graph's journal of
// operational events (book "System Journal"), newest first, on both engines (the WebAssembly
// playground's graph or store in memory, the dev server's `GET /api/journal`). Filters by level
// (that level and the more severe ones) and source (a prefix). Opening reads the NEWEST page
// (`newest_first`: a long stored journal is read from its end); while open it reads new records
// through the shared journal follower (js/journal-toasts.js, the poller the journal
// notifications use too: 5 s on the dev server, 2 s in the WebAssembly playground) from its
// cursor (`after`), never the whole journal again; "Show more" renders the held records, then
// reads the next older page (`before`). At most a few hundred rows are rendered. `open(record)`
// (a notification's click) opens at that record: it is scrolled into view and highlighted.
// Model js/journal.js.

import { ref, reactive, computed, nextTick } from 'vue';
import { journalPage, journalFollower } from '../store.js';
import { batchAsPage, matchesJournalFilters } from '../journal-toasts.js';
import {
  emptyJournal, openJournal, readJournal, showMoreJournal, journalRows, journalFooter,
  sourceChoices, addJournalPage, JOURNAL_LEVELS, JOURNAL_PAGE_ROWS,
} from '../journal.js';

/** "Show more" pages read at most to reach a record a notification opened. */
const SEEK_PAGES = 10;

const message = (err) => (err && err.message ? err.message : String(err));

/** The dialog; `open()` (exposed) shows it and reads the journal. */
export const JournalPanel = {
  setup(props, { expose }) {
    const dlg = ref(null);
    const d = reactive({
      view: emptyJournal(), level: '', source: '', shown: JOURNAL_PAGE_ROWS,
      loading: false, error: null, auto: true, epoch: 0, opened: false, highlight: null,
    });
    const rows = computed(() => journalRows(d.view, d.shown));
    const footer = computed(() => journalFooter(d.view, d.shown));
    const sources = computed(() => sourceChoices(d.view));
    let unsubscribe = null;

    const filters = () => ({ level: d.level, source: d.source });

    /**
     * Runs one read of the current epoch (a filter change starts a new one: a read of the old
     * filters still running is ignored); `step(view)` answers the new view, or `{view, shown}`.
     */
    async function run(step) {
      if (d.loading) return;
      d.loading = true;
      const epoch = d.epoch;
      try {
        const answer = await step(d.view);
        if (epoch === d.epoch) {
          if (answer && answer.view) {
            d.view = answer.view;
            d.shown = answer.shown;
          } else {
            d.view = answer;
          }
          d.error = null;
          d.opened = true;
        }
      } catch (err) {
        if (epoch === d.epoch) d.error = message(err);
      } finally {
        if (epoch === d.epoch) d.loading = false;
      }
    }
    /** Reads what is new since the cursor (the newest page first, when nothing is open yet). */
    async function refresh() {
      if (!d.opened) {
        await run(() => openJournal(filters(), journalPage));
        return;
      }
      await run((view) => readJournal(view, filters(), journalPage));
    }
    async function restart() {
      d.epoch++;
      d.view = emptyJournal();
      d.shown = JOURNAL_PAGE_ROWS;
      d.opened = false;
      d.loading = false;
      await refresh();
    }
    /** A follower batch: applied in memory when it continues the view, else read by itself. */
    function onBatch(batch) {
      if (!dlg.value || !dlg.value.open) {
        stopFollowing();
        return;
      }
      if (!d.auto || d.loading || !d.opened) return;
      const page = batchAsPage(d.view, batch, filters());
      if (page) d.view = addJournalPage(d.view, page);
      else refresh();
    }
    function stopFollowing() {
      if (unsubscribe) unsubscribe();
      unsubscribe = null;
    }
    /** Shows more rows until the record `seq` is rendered (bounded), then scrolls to it. */
    async function seek(seq) {
      for (let i = 0; i < SEEK_PAGES; i++) {
        const index = d.view.records.findIndex((r) => r.seq === seq);
        if (index >= 0 && index < d.shown) break;
        if (index < 0 && !(d.view.older && d.view.before != null && d.view.before > seq)) break;
        await showMore();
        if (d.error) break;
      }
      await nextTick();
      const row = dlg.value && dlg.value.querySelector(`tr[data-seq="${seq}"]`);
      if (row && row.scrollIntoView) {
        const reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
        row.scrollIntoView({ block: 'center', behavior: reduce ? 'auto' : 'smooth' });
      }
    }
    /** Opens the dialog; with `target` (`{seq, level, source}`) at that record. */
    async function open(target) {
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      d.highlight = target && typeof target.seq === 'number' ? target.seq : null;
      // A filter that would hide the record is cleared.
      if (d.highlight != null && !matchesJournalFilters(target, filters())) {
        d.level = '';
        d.source = '';
      }
      await restart();
      if (!unsubscribe) unsubscribe = journalFollower().subscribe(onBatch);
      if (d.highlight != null) await seek(d.highlight);
    }
    function close() {
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    async function showMore() {
      await run((view) => showMoreJournal(view, d.shown, filters(), journalPage));
    }
    expose({ open, close });
    return {
      dlg, d, rows, footer, sources, levels: JOURNAL_LEVELS, refresh, restart, close, showMore, stopFollowing,
    };
  },
  template: `
  <dialog ref="dlg" class="catalog-dialog wide" aria-labelledby="jl-title" @close="stopFollowing">
    <div class="dialog-head">
      <h2 id="jl-title">System journal</h2><span class="spacer"></span>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">
      <p class="muted sq-note">What this instance did: the store opened and closed, checkpoints, backups, damage, denied permissions, model loads. Newest first. Records hold names, counts and ids, never data values or query text. A graph without a store keeps its newest records in memory.</p>
      <div class="jl-filters">
        <label for="jl-level">Level</label>
        <select id="jl-level" v-model="d.level" @change="restart">
          <option v-for="l in levels" :key="l.name" :value="l.name">{{ l.label }}</option>
        </select>
        <label for="jl-source">Source</label>
        <input id="jl-source" type="text" list="jl-sources" v-model="d.source" placeholder="every source (store, auth, ...)"
               autocomplete="off" spellcheck="false" @change="restart" @keydown.enter.prevent="restart">
        <datalist id="jl-sources"><option v-for="s in sources" :key="s" :value="s"></option></datalist>
        <span class="spacer"></span>
        <label class="muted" title="Show new records as they are written"><input type="checkbox" v-model="d.auto"> follow new records</label>
        <button type="button" class="btn small" :disabled="d.loading" @click="refresh" title="Read the records written since the last read">{{ d.loading ? 'Reading…' : 'Refresh' }}</button>
      </div>
      <div v-if="d.error" class="field-error" role="alert">{{ d.error }}</div>
      <div v-for="note in d.view.notes" :key="note" class="field-error">{{ note }}</div>
      <div v-if="rows.length" class="catalog-table-wrap">
        <table class="data-table catalog-table jl-table">
          <thead><tr>
            <th>Time</th><th>Level</th><th>Source</th><th>Kind</th><th>Message</th><th class="num">Seq</th>
          </tr></thead>
          <tbody>
            <tr v-for="row in rows" :key="row.seq" :data-seq="row.seq" :class="{ 'jl-highlight': row.seq === d.highlight }">
              <td class="jl-time" :title="row.iso + ' (UTC)'">{{ row.time }}</td>
              <td><span :class="row.levelClass">{{ row.level }}</span></td>
              <td class="code">{{ row.source }}</td>
              <td class="code">{{ row.kind }}</td>
              <td>{{ row.message }}
                <div v-if="row.fields" class="muted cc-sub code jl-fields">{{ row.fields }}</div>
                <div v-if="row.extra" class="muted cc-sub">{{ row.extra }}</div></td>
              <td class="num" :title="row.origin">{{ row.seq }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p v-else-if="!d.loading && !d.error" class="muted">No records{{ d.level || d.source ? ' match the filters' : ' yet' }}.</p>
      <div class="sq-buttons">
        <span class="muted">{{ footer.text }}</span>
        <button v-if="footer.more" type="button" class="btn small" :disabled="d.loading" @click="showMore" title="Show 300 more rows; older ones are read from the journal">Show more</button>
        <span class="spacer"></span>
        <button type="button" class="btn" @click="close">Close</button>
      </div>
    </div>
  </dialog>
  `,
};
