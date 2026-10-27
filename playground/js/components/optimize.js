// The Optimize panel (top bar "Optimize", `openOptimize()` in store.js): one dialog with three
// tabs over the engine's own figures, model js/optimize.js.
//
// - Recommendations: the storage advisor's list (create or drop a property index, add or drop
//   a compression rule), each with its evidence, cost, memory effect, confidence and the exact
//   command; "Apply" runs it through the catalog (one commit, reversible), "Copy" copies the
//   command. "Every candidate" lowers the thresholds (small graphs).
// - Workload: the workload statistics per list (queries, properties, locks, jobs) in a window
//   (hour, 2 days, 90 days) for a channel, a sparkline per row; a row opens a detail chart
//   (runs per bucket with the failed part, mean and max time) with the tier switch.
// - Memory: the memory report: totals, properties and labels, the most bytes first.

import { ref, reactive, computed, watch, nextTick } from 'vue';
import { state, toast, optimizeApplied } from '../store.js';
import {
  WINDOWS, TIERS, LISTS, MIN_TIMES, SNOOZE_DAYS, sortsDescending, recommendationRows, dismissedRows, appliedRows, adviceSummary, channels, workloadRows, sparkPath,
  chartGeometry, formatMicros, memoryRows, labelRows, memoryTotals,
} from '../optimize.js';
import { formatBytes } from '../statistics.js';

const message = (err) => (err && err.message ? err.message : String(err));

const SPARK_W = 120;
const SPARK_H = 22;
const CHART_W = 640;
const CHART_H = 160;

export const OptimizePanel = {
  setup() {
    const dlg = ref(null);
    const tab = ref('advice');
    const advice = reactive({ data: null, loading: false, error: null, everything: false, applying: null });
    const work = reactive({
      report: null, loading: false, error: null, window: 'hour', channel: 'all', list: 'queries', minMicros: 0, sort: 'total', desc: true,
      selected: null, detailTier: 'minutes', detail: null, detailLoading: false,
    });
    const memory = reactive({ report: null, loading: false, error: null });

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
        if (answer && answer.ok) advice.data = answer.advice;
        else advice.error = (answer && answer.error) || 'The advice failed.';
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
        const answer = await engine().memoryReport({});
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
    watch(() => state.optimizeN, async () => {
      await nextTick();
      await open();
    });
    watch(tab, () => {
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
    async function apply(row) {
      advice.applying = row.id;
      try {
        const res = await engine().applyRecommendation(row.id, spec());
        if (res && res.ok) {
          toast(`Applied: ${row.title}.`, 'info', 5000);
          await optimizeApplied();
          await loadAdvice();
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

    return {
      state, dlg, tab, advice, work, memory, open, close, refresh,
      recRows, summary, apply, copy, dismiss, restore, dismissed, applied, SNOOZE_DAYS,
      WINDOWS, TIERS, LISTS, MIN_TIMES, sortBy, sortClass, channelList, rows, listCounts, spark, select, formatMicros, formatBytes,
      memRows, memLabels, memTotals,
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
        <div v-if="advice.error" class="field-error">{{ advice.error }}</div>
        <p v-else-if="advice.data && !recRows.length" class="muted">Nothing to recommend now. Recommendations come from frequent full-scan filters on large labels, large strings that repeat or compress well, unused indexes and compressed keys read often. Tick "Every candidate" to look at a small graph.</p>
        <div v-for="r in recRows" :key="r.id" class="op-rec">
          <div class="op-rec-head">
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
          <div class="op-totals">
            <div v-for="t in memTotals" :key="t.label" class="op-total"><span class="muted">{{ t.label }}</span><strong>{{ formatBytes(t.bytes) }}</strong></div>
          </div>
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
          <p v-if="memory.report.rows_omitted" class="muted">{{ memory.report.rows_omitted }} smaller rows not shown.</p>
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
        </template>
      </template>
    </div>
  </dialog>
  `,
};
