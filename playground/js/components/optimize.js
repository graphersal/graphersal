// The Optimize panel (top bar "Optimize", `openOptimize()` in store.js): one dialog with three
// tabs over the engine's own figures, model js/optimize.js.
//
// - Recommendations: the storage advisor's list (create or drop a property index, add or drop
//   a compression rule), each with its evidence, cost, memory effect, confidence and the exact
//   command; "Apply" runs it through the catalog (one commit, reversible), "Copy" copies the
//   command. "Every candidate" lowers the thresholds (small graphs). Several at once: a checkbox
//   per recommendation, "Select all" per group (kind and codec: "dedup: 74 keys, total -400.1
//   MiB"), nothing selected by default; "Apply selected" asks for confirmation and applies
//   them in ONE commit (`applyRecommendations`), all or nothing. After a change of compression
//   rules on a store, a note recommends a snapshot ("Write a snapshot now") until one holds that
//   commit (`checkpointDue`): until then every open replays the change and re-encodes the data.
// - Workload: the workload statistics per list (queries, properties, locks, jobs) in a window
//   (hour, 2 days, 90 days) for a channel, a sparkline per row; a row opens a detail chart
//   (runs per bucket with the failed part, mean and max time) with the tier switch.
// - Memory: the process memory bar (the shared MemoryBar over `statistics()`, as in the Profile
//   panel's Statistics tab), then the memory report: totals, where the process memory goes
//   (its components and the unattributed rest), properties and labels, the most bytes first.

import { ref, reactive, computed, watch, nextTick } from 'vue';
import { state, toast, optimizeApplied, runStoreTask } from '../store.js';
import {
  WINDOWS, TIERS, LISTS, MIN_TIMES, SNOOZE_DAYS, sortsDescending, recommendationRows, dismissedRows, appliedRows, adviceSummary, channels, workloadRows, sparkPath,
  recommendationGroups, pruneSelection, groupSelected, toggleGroup, toggleRecommendation, selectionSummary,
  changesCompressionRules, checkpointDue,
  chartGeometry, formatMicros, memoryRows, labelRows, memoryTotals, memoryOmitted, memoryReportSpec,
  memoryComponents,
} from '../optimize.js';
import { formatBytes } from '../statistics.js';
import { MemoryBar } from './memory-bar.js';

const message = (err) => (err && err.message ? err.message : String(err));

const SPARK_W = 120;
const SPARK_H = 22;
const CHART_W = 640;
const CHART_H = 160;

export const OptimizePanel = {
  components: { MemoryBar },
  setup() {
    const dlg = ref(null);
    const tab = ref('advice');
    // `selected`: the ids ticked for "Apply selected" (none by default: a human picks).
    // `rulesCommit`: the commit of the last compression rule change applied here; `checkpointDue`:
    // no snapshot holds it yet.
    const advice = reactive({ data: null, loading: false, error: null, everything: false, applying: null, selected: [], rulesCommit: null, checkpointDue: false });
    const work = reactive({
      report: null, loading: false, error: null, window: 'hour', channel: 'all', list: 'queries', minMicros: 0, sort: 'total', desc: true,
      selected: null, detailTier: 'minutes', detail: null, detailLoading: false,
    });
    // `stats`: the `statistics()` answer for the process memory bar (MemoryBar).
    const memory = reactive({ report: null, stats: null, loading: false, error: null });

    const engine = () => state.engine;
    const spec = () => (advice.everything ? { everything: true } : {});

    async function loadAdvice() {
      if (!engine() || typeof engine().advise !== 'function') {
        advice.error = 'This engine has no storage advisor (rebuild the playground).';
        return;
      }
      advice.loading = true;
      advice.error = null;
      try {
        const answer = await engine().advise(spec());
        if (answer && answer.ok) {
          advice.data = answer.advice;
          advice.selected = pruneSelection(advice.data, advice.selected);
          await checkCheckpointDue();
        } else advice.error = (answer && answer.error) || 'The advice failed.';
      } catch (err) {
        advice.error = message(err);
      } finally {
        advice.loading = false;
      }
    }

    async function loadWorkload() {
      if (!engine() || typeof engine().workloadReport !== 'function') return;
      work.loading = true;
      work.error = null;
      try {
        const tier = (WINDOWS.find((w) => w.id === work.window) || WINDOWS[0]).tier;
        work.report = await engine().workloadReport(tier);
      } catch (err) {
        work.error = message(err);
      } finally {
        work.loading = false;
      }
    }

    async function loadMemory() {
      if (!engine() || typeof engine().memoryReport !== 'function') return;
      memory.loading = true;
      memory.error = null;
      try {
        // The process bar (statistics) and the graph's report, one after the other: the engine
        // serves one request at a time anyway. A failing statistics read leaves the bar out.
        if (typeof engine().statistics === 'function') {
          try {
            memory.stats = await engine().statistics();
          } catch {
            memory.stats = null;
          }
        }
        // Bounded: at most MEMORY_LIST_LIMIT rows per list (largest first), the rest counted.
        const answer = await engine().memoryReport(memoryReportSpec());
        if (answer && answer.ok) memory.report = answer.report;
        else memory.error = (answer && answer.error) || 'The memory report failed.';
      } catch (err) {
        memory.error = message(err);
      } finally {
        memory.loading = false;
      }
    }

    function refresh() {
      if (tab.value === 'advice') return loadAdvice();
      if (tab.value === 'workload') return loadWorkload();
      return loadMemory();
    }

    async function open() {
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
      await refresh();
    }
    function close() {
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    // While `open` switches to a requested tab, the tab watcher leaves the loading to `open`.
    let opening = false;
    watch(() => state.optimizeN, async () => {
      const wanted = state.optimizeTab;
      state.optimizeTab = null;
      opening = true;
      if (wanted) tab.value = wanted;
      await nextTick();
      opening = false;
      await open();
    });
    watch(tab, () => {
      if (opening) return;
      const loaded = { advice: advice.data, workload: work.report, memory: memory.report }[tab.value];
      if (!loaded) refresh();
    });
    watch(() => advice.everything, loadAdvice);
    watch(() => work.window, () => {
      work.selected = null;
      loadWorkload();
    });

    // Recommendations.
    const recRows = computed(() => recommendationRows(advice.data));
    const summary = computed(() => adviceSummary(advice.data));
    // The snapshot note: asks the store each time the advice loads, so a snapshot written
    // another way (the Store menu, an automatic checkpoint) ends it too.
    async function checkCheckpointDue() {
      if (advice.rulesCommit == null) return;
      const e = engine();
      try {
        const store = e.storeSummary ? await e.storeSummary() : e.storeInfo ? await e.storeInfo() : null;
        advice.checkpointDue = checkpointDue(store, advice.rulesCommit);
      } catch (err) {
        advice.checkpointDue = false; // no store, or it cannot answer: nothing to recommend
      }
    }
    async function rulesApplied(kinds) {
      if (!changesCompressionRules(kinds) || !advice.data) return;
      advice.rulesCommit = advice.data.commit_seq;
      await checkCheckpointDue();
    }
    async function writeSnapshot() {
      try {
        const res = await runStoreTask('checkpoint', () => engine().checkpoint(''));
        if (res.refused) {
          toast('Not written: a query or another store action is running.', 'error', 6000);
          return;
        }
        toast(`Snapshot written at commit ${res.value.commitSeq}: the next start loads the rules from it.`, 'info', 7000);
        await checkCheckpointDue();
      } catch (err) {
        toast(`Checkpoint failed: ${message(err)}`, 'error', 12000);
      }
    }
    // Another graph: its store has nothing to do with the rules applied here.
    watch(() => state.graphEpoch, () => {
      advice.rulesCommit = null;
      advice.checkpointDue = false;
    });
    async function apply(row) {
      advice.applying = row.id;
      try {
        const res = await engine().applyRecommendation(row.id, spec());
        if (res && res.ok) {
          toast(`Applied: ${row.title}.`, 'info', 5000);
          await optimizeApplied();
          await loadAdvice();
          await rulesApplied([res.kind]);
          memory.report = null;
        } else {
          toast(`Not applied: ${(res && res.error) || 'refused'}`, 'error', 9000);
          await loadAdvice();
        }
      } catch (err) {
        toast(`Not applied: ${message(err)}`, 'error', 9000);
      } finally {
        advice.applying = null;
      }
    }
    async function dismiss(row, days) {
      try {
        const res = await engine().dismissRecommendation(row.id, days);
        if (res && res.ok) await loadAdvice();
        else toast(`Not dismissed: ${(res && res.error) || 'refused'}`, 'error', 9000);
      } catch (err) {
        toast(`Not dismissed: ${message(err)}`, 'error', 9000);
      }
    }
    async function restore(row) {
      try {
        await engine().restoreRecommendation(row.id);
        await loadAdvice();
      } catch (err) {
        toast(`Not restored: ${message(err)}`, 'error', 9000);
      }
    }
    // Several at once.
    const groups = computed(() => recommendationGroups(advice.data));
    const selection = computed(() => selectionSummary(advice.data, advice.selected));
    const isSelected = (id) => advice.selected.includes(id);
    const isGroupSelected = (group) => groupSelected(group, advice.selected);
    const toggle = (id) => { advice.selected = toggleRecommendation(advice.data, advice.selected, id); };
    const toggleAll = (group) => { advice.selected = toggleGroup(advice.data, advice.selected, group); };
    const clearSelection = () => { advice.selected = []; };
    async function applySelected() {
      const chosen = selection.value;
      if (!chosen.count || !window.confirm(chosen.confirm)) return;
      advice.applying = '*selected*';
      try {
        const res = await engine().applyRecommendations([...advice.selected], spec());
        if (res && res.ok) {
          const n = (res.applied || []).length;
          toast(`Applied ${n} recommendation${n === 1 ? '' : 's'} in one commit.`, 'info', 6000);
          advice.selected = [];
          await optimizeApplied();
          await loadAdvice();
          await rulesApplied((res.applied || []).map((a) => a.kind));
          memory.report = null;
        } else {
          toast(`Nothing applied: ${(res && res.error) || 'refused'}`, 'error', 12000);
          await loadAdvice();
        }
      } catch (err) {
        toast(`Nothing applied: ${message(err)}`, 'error', 12000);
      } finally {
        advice.applying = null;
      }
    }
    const dismissed = computed(() => dismissedRows(advice.data));
    const applied = computed(() => appliedRows(advice.data));
    async function copy(row) {
      try {
        await navigator.clipboard.writeText(row.command);
        toast('Command copied.', 'info', 2500);
      } catch {
        toast(row.command, 'info', 9000);
      }
    }

    // Workload.
    const channelList = computed(() => channels(work.report));
    const options = () => ({ minMicros: work.minMicros, sort: work.sort, desc: work.desc });
    const rows = computed(() => workloadRows(work.report, work.list, work.window, work.channel, Date.now() * 1000, options()));
    const listCounts = computed(() => Object.fromEntries(
      LISTS.map((l) => [l.id, workloadRows(work.report, l.id, work.window, work.channel, Date.now() * 1000, options()).length]),
    ));
    // A click sorts by the column; a second click on it turns the direction around.
    const sortBy = (column) => {
      if (work.sort === column) work.desc = !work.desc;
      else Object.assign(work, { sort: column, desc: sortsDescending(column) });
    };
    const sortClass = (column) => (work.sort === column ? ['active', work.desc ? 'desc' : 'asc'] : []);
    const spark = (row, field) => (row.series ? sparkPath(row.series.map((b) => b[field]), SPARK_W, SPARK_H) : '');
    async function select(row) {
      if (work.selected && work.selected.key === row.key) {
        work.selected = null;
        return;
      }
      work.selected = row;
      work.detailTier = (WINDOWS.find((w) => w.id === work.window) || WINDOWS[0]).tier;
      await loadDetail();
    }
    async function loadDetail() {
      if (!work.selected) return;
      work.detailLoading = true;
      try {
        const report = await engine().workloadReport(work.detailTier);
        const window = WINDOWS.find((w) => w.tier === work.detailTier) || WINDOWS[0];
        const found = workloadRows(report, work.list, window.id, 'all').find((r) => r.key === work.selected.key);
        work.detail = found ? chartGeometry(found.series || [], CHART_W, CHART_H, work.detailTier) : null;
      } catch (err) {
        work.detail = null;
        toast(`The chart failed: ${message(err)}`, 'error', 6000);
      } finally {
        work.detailLoading = false;
      }
    }
    watch(() => work.detailTier, loadDetail);

    // Memory.
    const memRows = computed(() => memoryRows(memory.report));
    const memLabels = computed(() => labelRows(memory.report));
    const memTotals = computed(() => memoryTotals(memory.report));
    const memComponents = computed(() => memoryComponents(memory.report));
    const memRowsOmitted = computed(() => memoryOmitted(memory.report, 'properties', formatBytes));
    const memLabelsOmitted = computed(() => memoryOmitted(memory.report, 'labels', formatBytes));

    return {
      state, dlg, tab, advice, work, memory, open, close, refresh,
      recRows, summary, apply, writeSnapshot, copy, dismiss, restore, dismissed, applied, SNOOZE_DAYS,
      groups, selection, isSelected, isGroupSelected, toggle, toggleAll, clearSelection, applySelected,
      WINDOWS, TIERS, LISTS, MIN_TIMES, sortBy, sortClass, channelList, rows, listCounts, spark, select, formatMicros, formatBytes,
      memRows, memLabels, memTotals, memComponents, memRowsOmitted, memLabelsOmitted,
      SPARK_W, SPARK_H, CHART_W, CHART_H,
    };
  },
  template: `
  <dialog ref="dlg" class="catalog-dialog wide optimize-dialog" aria-labelledby="op-title">
    <div class="dialog-head">
      <h2 id="op-title">Optimize</h2>
      <div class="op-tabs" role="tablist">
        <button type="button" role="tab" :aria-selected="tab === 'advice'" :class="{ active: tab === 'advice' }" @click="tab = 'advice'">Recommendations</button>
        <button type="button" role="tab" :aria-selected="tab === 'workload'" :class="{ active: tab === 'workload' }" @click="tab = 'workload'">Workload</button>
        <button type="button" role="tab" :aria-selected="tab === 'memory'" :class="{ active: tab === 'memory' }" @click="tab = 'memory'">Memory</button>
      </div>
      <span class="spacer"></span>
      <button type="button" class="btn small" :disabled="advice.loading || work.loading || memory.loading" @click="refresh" title="Compute again from the current data and statistics">Refresh</button>
      <button type="button" class="icon-btn" @click="close" aria-label="Close">×</button>
    </div>
    <div class="dialog-body">

      <template v-if="tab === 'advice'">
        <p class="muted sq-note">The advisor reads what the graph holds (the memory report), how it is used (the workload statistics) and its catalog. Savings are measured on the data; nothing changes until you apply a recommendation, and every one can be undone (drop the index or the rule).</p>
        <div class="op-bar">
          <span class="muted">{{ advice.loading ? 'Computing…' : summary }}</span>
          <span class="spacer"></span>
          <label class="op-check" title="Lower every threshold: shows candidates on small graphs too"><input type="checkbox" v-model="advice.everything"> Every candidate</label>
        </div>
        <div v-if="advice.checkpointDue" class="op-note" role="status">
          <span><strong>Write a snapshot now.</strong> The new compression rules are only in the write-ahead log, so every start of this store re-encodes their labels, with a higher memory peak. A snapshot holds them: each row is then coded as it loads.</span>
          <button type="button" class="btn small primary" :disabled="!!state.storeTask || state.storeBusy || state.running" @click="writeSnapshot">{{ state.storeTask && state.storeTask.id === 'checkpoint' ? 'Writing…' : 'Write a snapshot now' }}</button>
        </div>
        <div v-if="advice.error" class="field-error">{{ advice.error }}</div>
        <p v-else-if="advice.data && !recRows.length" class="muted">Nothing to recommend now. Recommendations come from frequent full-scan filters on large labels, large strings that repeat or compress well, unused indexes and compressed keys read often. Tick "Every candidate" to look at a small graph.</p>
        <div v-if="recRows.length > 1" class="op-groups">
          <span class="muted">Select all:</span>
          <label v-for="g in groups" :key="g.key" class="op-check" :title="'Tick every recommendation of this group (' + g.count + ')'">
            <input type="checkbox" :checked="isGroupSelected(g)" :disabled="!!advice.applying" @change="toggleAll(g)"> {{ g.title }}
          </label>
        </div>
        <div v-if="recRows.length > 1" class="op-bar op-selection">
          <span :class="advice.selected.length ? '' : 'muted'">{{ selection.text }}</span>
          <span class="spacer"></span>
          <button type="button" class="btn small" :disabled="!advice.selected.length || !!advice.applying" @click="clearSelection">Clear</button>
          <button type="button" class="btn small primary" :disabled="!advice.selected.length || !!advice.applying || state.running" @click="applySelected" title="Apply every ticked recommendation in one commit (asks first; all or nothing)">{{ advice.applying === '*selected*' ? 'Applying…' : 'Apply selected' }}</button>
        </div>
        <div v-for="r in recRows" :key="r.id" class="op-rec">
          <div class="op-rec-head">
            <input v-if="recRows.length > 1" type="checkbox" :checked="isSelected(r.id)" :disabled="!!advice.applying" @change="toggle(r.id)" :aria-label="'Select: ' + r.title">
            <span class="badge">{{ r.badge }}</span>
            <strong>{{ r.title }}</strong>
            <span class="spacer"></span>
            <span :class="['op-effect', r.frees ? 'good' : 'cost']">{{ r.effect }}<span v-if="r.share" class="muted"> ({{ r.share }})</span></span>
            <span v-if="r.impact" :class="['badge', 'op-impact', r.impact]" title="Impact: what applying it is worth (for memory: its share of the graph's memory; high from 10 %, medium from 2 %)">{{ r.impact }} impact</span>
            <span class="badge muted" title="Confidence: how much data the figures rest on (high: measured on all the data), not how much the recommendation is worth">confidence {{ r.confidence }}</span>
          </div>
          <div class="op-rec-line"><span class="muted">Why:</span> {{ r.evidence }}</div>
          <div class="op-rec-line"><span class="muted">Cost:</span> {{ r.cost }}</div>
          <div class="op-rec-foot">
            <code class="op-command">{{ r.command }}</code>
            <span class="spacer"></span>
            <button type="button" class="btn small" :disabled="!!advice.applying" @click="dismiss(r, SNOOZE_DAYS)" :title="'Hide it for ' + SNOOZE_DAYS + ' days'">Snooze {{ SNOOZE_DAYS }} days</button>
            <button type="button" class="btn small" :disabled="!!advice.applying" @click="dismiss(r, null)" title="Hide it for good (Restore lists it again)">Dismiss</button>
            <button type="button" class="btn small" @click="copy(r)">Copy</button>
            <button type="button" class="btn small primary" :disabled="!!advice.applying || state.running" @click="apply(r)">{{ advice.applying === r.id ? 'Applying…' : 'Apply' }}</button>
          </div>
        </div>
        <template v-if="applied.length">
          <h3>Applied</h3>
          <div v-for="a in applied" :key="a.id" class="op-rec op-applied">
            <div class="op-rec-head"><strong>{{ a.title }}</strong><span class="spacer"></span><span class="muted">{{ a.when }}</span></div>
            <div class="op-rec-line"><span class="muted">Promised:</span> {{ a.promised }} · <span class="muted">measured since:</span> {{ a.measured }}<span v-if="a.lookups"> · {{ a.lookups }}</span></div>
          </div>
          <p class="muted sq-note">Measured on the graph's property and property index memory then and now: other data changes since count too. Shown for 30 days.</p>
        </template>
        <details v-if="dismissed.length" class="op-dismissed">
          <summary>Dismissed ({{ dismissed.length }})</summary>
          <div v-for="d in dismissed" :key="d.id" class="op-bar">
            <span>{{ d.title }}</span><span class="muted">{{ d.until }}</span><span class="spacer"></span>
            <button type="button" class="btn small" @click="restore(d)">Restore</button>
          </div>
        </details>
      </template>

      <template v-else-if="tab === 'workload'">
        <div class="op-bar">
          <select v-model="work.window" aria-label="Window">
            <option v-for="w in WINDOWS" :key="w.id" :value="w.id">{{ w.label }}</option>
          </select>
          <select v-model="work.channel" aria-label="Channel">
            <option value="all">All channels</option>
            <option v-for="c in channelList" :key="c" :value="c">{{ c }}</option>
          </select>
          <select v-model.number="work.minMicros" aria-label="Slowest run at least" title="Show only the rows whose longest run took at least this long">
            <option v-for="t in MIN_TIMES" :key="t.micros" :value="t.micros">{{ t.label }}</option>
          </select>
          <span class="spacer"></span>
          <span class="muted" v-if="work.report">level {{ work.report.level }}</span>
        </div>
        <div class="op-tabs small" role="tablist">
          <button v-for="l in LISTS" :key="l.id" type="button" role="tab" :aria-selected="work.list === l.id" :class="{ active: work.list === l.id }" @click="work.list = l.id; work.selected = null">{{ l.label }} <span class="badge">{{ listCounts[l.id] || 0 }}</span></button>
        </div>
        <div v-if="work.error" class="field-error">{{ work.error }}</div>
        <p v-else-if="work.report && work.report.level === 'off'" class="muted">No workload statistics: they are kept for a graph in a store.</p>
        <p v-else-if="work.report && !rows.length" class="muted">Nothing counted in this window. Queries you run (here, through MCP, the CLI or Python) are counted; the page's own drawing is not.</p>
        <div v-else-if="rows.length" class="catalog-table-wrap">
          <table class="data-table catalog-table op-table">
            <colgroup>
              <col><col class="op-col-channel"><col class="op-col-n"><col class="op-col-n"><col class="op-col-n">
              <col class="op-col-t"><col class="op-col-t"><col class="op-col-t"><col class="op-col-spark">
            </colgroup>
            <thead><tr>
              <th>{{ LISTS.find((l) => l.id === work.list).what }}</th>
              <th class="op-sort" :class="sortClass('channel')" @click="sortBy('channel')" title="Sort by channel">Channel</th>
              <th class="num op-sort" :class="sortClass('count')" @click="sortBy('count')" title="Sort by runs">Runs</th>
              <th class="num op-sort" :class="sortClass('failed')" @click="sortBy('failed')" title="Sort by failed runs">Failed</th>
              <th class="num">Items</th>
              <th class="num op-sort" :class="sortClass('mean')" @click="sortBy('mean')" title="Sort by mean time">Mean</th>
              <th class="num op-sort" :class="sortClass('max')" @click="sortBy('max')" title="Sort by the longest run">Max</th>
              <th class="num op-sort" :class="sortClass('total')" @click="sortBy('total')" title="Sort by total time">Total</th>
              <th>Runs over time</th>
            </tr></thead>
            <tbody>
              <template v-for="row in rows" :key="row.key">
                <tr :class="['op-row', { selected: work.selected && work.selected.key === row.key }]" @click="select(row)" title="Show the chart">
                  <td class="code op-name" :title="row.name">{{ row.name }}</td><td>{{ row.channel }}</td>
                  <td class="num">{{ row.count }}</td><td class="num">{{ row.failed || '' }}</td><td class="num">{{ row.items }}</td>
                  <td class="num">{{ formatMicros(row.mean) }}</td><td class="num">{{ formatMicros(row.max) }}</td><td class="num">{{ formatMicros(row.total) }}</td>
                  <td><svg v-if="row.series" class="op-spark" :width="SPARK_W" :height="SPARK_H" :viewBox="'0 0 ' + SPARK_W + ' ' + SPARK_H" aria-hidden="true"><path :d="spark(row, 'count')"></path></svg></td>
                </tr>
                <tr v-if="work.selected && work.selected.key === row.key" class="op-detail-row">
                  <td colspan="9">
                    <div v-if="row.name.length > 80" class="op-full code">{{ row.name }}</div>
                    <div class="op-bar">
                      <span class="spacer"></span>
                      <select v-model="work.detailTier" aria-label="Buckets">
                        <option v-for="(t, id) in TIERS" :key="id" :value="id">{{ t.label }}</option>
                      </select>
                    </div>
                    <p v-if="work.detailLoading" class="muted">Loading…</p>
                    <p v-else-if="!work.detail" class="muted">No buckets in this tier.</p>
                    <template v-else>
                      <svg class="op-chart" :viewBox="'0 0 ' + CHART_W + ' ' + CHART_H" preserveAspectRatio="none" role="img" :aria-label="'Runs and times of ' + row.name">
                        <g class="op-bars"><g v-for="(b, i) in work.detail.bars" :key="i"><title>{{ b.title }}</title>
                          <rect :x="b.x" :y="CHART_H - b.h" :width="b.w" :height="b.h"></rect>
                          <rect v-if="b.failedH" class="failed" :x="b.x" :y="CHART_H - b.h" :width="b.w" :height="b.failedH"></rect>
                        </g></g>
                        <path class="op-line mean" :d="work.detail.mean"></path>
                        <path class="op-line max" :d="work.detail.max"></path>
                      </svg>
                      <div class="op-legend muted">
                        <span><i class="sw bars"></i> runs (max {{ work.detail.countMax }} per bucket)</span>
                        <span><i class="sw failed"></i> failed</span>
                        <span><i class="sw mean"></i> mean time</span>
                        <span><i class="sw max"></i> max time (up to {{ formatMicros(work.detail.timeMax) }})</span>
                      </div>
                    </template>
                  </td>
                </tr>
              </template>
            </tbody>
          </table>
        </div>
      </template>

      <template v-else>
        <div v-if="memory.error" class="field-error">{{ memory.error }}</div>
        <p v-else-if="memory.loading && !memory.report" class="muted">Measuring…</p>
        <template v-else-if="memory.report">
          <memory-bar :stats="memory.stats"></memory-bar>
          <div class="op-totals">
            <div v-for="t in memTotals" :key="t.label" class="op-total"><span class="muted">{{ t.label }}</span><strong>{{ formatBytes(t.bytes) }}</strong></div>
          </div>
          <template v-if="memComponents.length">
            <h3>Process memory</h3>
            <div class="catalog-table-wrap">
              <table class="data-table catalog-table">
                <thead><tr><th>Component</th><th>Which</th><th class="num">Bytes</th><th class="num">Share</th><th></th></tr></thead>
                <tbody>
                  <tr v-for="c in memComponents" :key="c.key">
                    <td class="code">{{ c.name }}</td><td>{{ c.detail }}</td>
                    <td class="num">{{ c.bytes < 0 ? '-' + formatBytes(-c.bytes) : formatBytes(c.bytes) }}</td>
                    <td class="num">{{ c.share == null ? '' : (c.share * 100).toFixed(1) + ' %' }}</td>
                    <td><span class="badge">{{ c.name === 'unattributed' ? 'rest' : (c.exact ? 'exact' : 'estimated') }}</span></td>
                  </tr>
                </tbody>
              </table>
            </div>
          </template>
          <p v-if="memory.report.sampled" class="muted sq-note">Sampled: every {{ memory.report.sample_every }}th element of a large label was read and scaled.</p>
          <h3>Properties</h3>
          <div class="catalog-table-wrap">
            <table class="data-table catalog-table">
              <thead><tr><th>Property</th><th>Element</th><th class="num">Elements</th><th class="num">Bytes</th><th>Types</th><th class="num">Distinct</th><th class="num">Largest</th><th></th></tr></thead>
              <tbody>
                <tr v-for="r in memRows" :key="r.key">
                  <td class="code">{{ r.target }}</td><td>{{ r.element }}</td><td class="num">{{ r.elements }}</td>
                  <td class="num">{{ formatBytes(r.bytes) }}</td><td>{{ r.types }}</td>
                  <td class="num">{{ r.distinct == null ? '' : r.distinct }}</td><td class="num">{{ r.maxValue ? formatBytes(r.maxValue) : '' }}</td>
                  <td><span v-for="b in r.badges" :key="b" class="badge">{{ b }}</span></td>
                </tr>
              </tbody>
            </table>
          </div>
          <p v-if="memRowsOmitted" class="muted">{{ memRowsOmitted.text }}</p>
          <h3>Labels</h3>
          <div class="catalog-table-wrap">
            <table class="data-table catalog-table">
              <thead><tr><th>Label</th><th>Element</th><th class="num">Elements</th><th class="num">Slots</th><th class="num">Properties</th><th class="num">Values</th><th class="num">Max degree</th></tr></thead>
              <tbody>
                <tr v-for="r in memLabels" :key="r.key">
                  <td class="code">{{ r.label }}</td><td>{{ r.element }}</td><td class="num">{{ r.elements }}</td>
                  <td class="num">{{ formatBytes(r.slots) }}</td><td class="num">{{ formatBytes(r.properties) }}</td>
                  <td class="num">{{ r.values }}</td><td class="num">{{ r.maxDegree == null ? '' : r.maxDegree }}</td>
                </tr>
              </tbody>
            </table>
          </div>
          <p v-if="memLabelsOmitted" class="muted">{{ memLabelsOmitted.text }}</p>
        </template>
      </template>
    </div>
  </dialog>
  `,
};
