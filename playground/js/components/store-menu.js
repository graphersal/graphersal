// The Store menu: the dev server on a Store directory (`graphersal --graph <dir> --server`, DEV
// mode, on disk) or the WebAssembly playground's store in memory (every loaded graph is one;
// lost on page reload). It shows the store's position (commit, last commit time), its sizes,
// its marks and snapshots (each a `.gsnap` download) with a one-line explanation of each: the
// newest five of each, all of them in the history calendar (store-calendar.js, "Calendar…"),
// Checkpoint, Mark and Verify, and the time travel on any point of the history (a snapshot, a
// mark, "Go to commit #..." or "Go to time..."): "Open read-only here" (a read-only view with a
// banner and "Back to the live graph"), "Fork..." (a new directory on the server, a new
// database in memory) and "Roll back here..." (the dialog of store-dialog.js asks and explains).
// The attic lists the history rollbacks moved aside: Show changes, Restore (only while
// allowed, otherwise the reason and Fork), Fork, Remove. In memory the databases of the session
// (the store and its forks) are listed with Switch to and Close. A damaged store (maintenance
// mode, read-only) shows its damage report instead of the write actions; a backup directory
// (read-only until `graphersal store restore`) shows its marker and offers views, forks and
// downloads only. On the dev server it backs the store up (Back up: full or an increment into a
// directory next to it); damage found on disk while the store is open (by a backup, a
// checkpoint, Verify) is shown with what the store's damage policy did and Back up from memory.
// The store's creation parameters (damage policy, created) are listed. Refreshed when it
// opens, after every run that changed the graph and after every store action.

import { ref, computed, watch, onMounted } from 'vue';
import { DropMenu } from './drop-menu.js';
import { StoreDialog } from './store-dialog.js';
import { StoreCalendar } from './store-calendar.js';
import { StoreTaskLabel } from './store-task-label.js';
import {
  state, toast, saveGraph, openStoreView, backToLiveGraph, restoreAtticEntry, switchDatabase,
  closeDatabase, downloadStoredSnapshot, runStoreTask,
} from '../store.js';
import {
  EXPLAIN, actionsFor, commitTarget, markTarget, parseCommit, parseTime, atticRows, viewBanner,
  storeWhere, databaseRows, backupBanner, compactionModel, actionDisabled, storeTask, taskButton,
  waitTitle, storeButtonText, modelRows, offersWithoutModels, snapshotFileName,
} from '../store-model.js';
import { recentHistory } from '../store-calendar.js';
import { formatBytes, formatCount } from '../statistics.js';
import { catalogCountsText } from '../compression.js';

export const StoreMenu = {
  components: { DropMenu, StoreDialog, StoreCalendar, StoreTaskLabel },
  setup() {
    const info = ref(null);
    const error = ref('');
    // One store action at a time (`runStoreTask`), none while a query runs.
    const locked = computed(() => !!state.storeTask || state.running || state.storeBusy);
    const checkpointName = ref('');
    const markName = ref('');
    const goCommit = ref('');
    const goTime = ref('');
    const goError = ref('');
    const dialog = ref(null);
    const calendar = ref(null);

    const refreshing = ref(false);
    async function refresh() {
      if (!state.engine || !state.engine.storeInfo || refreshing.value) return;
      refreshing.value = true;
      try {
        info.value = await state.engine.storeInfo();
        error.value = '';
      } catch (err) {
        error.value = err && err.message ? err.message : String(err);
      } finally {
        refreshing.value = false;
      }
    }

    const message = (err) => (err && err.message ? err.message : String(err));

    const menu = ref(null);
    const openMenu = () => menu.value && !menu.value.open && menu.value.show();
    const damaged = () => !!(info.value && (info.value.maintenance || (info.value.damageFound && info.value.damageFound.length)));

    /**
     * Runs the store action `id` (a key of `STORE_TASKS`) through `runStoreTask`: its item and
     * the Store button show it running, the toast at the end says the result; a failure that
     * found damage (or Verify's problems) gets a "Details" button that opens this menu.
     */
    async function act(id, fn, key = null) {
      if (locked.value) return;
      try {
        const res = await runStoreTask(id, fn, key);
        if (res.refused) return;
        if (res.value) toast(res.value, 'info', 7000);
        await refresh();
      } catch (err) {
        // A failure may have found damage (a backup, a checkpoint, Verify): show it.
        await refresh();
        const details = err && err.storeProblems ? true : damaged();
        const text = err && err.storeProblems ? message(err) : `${storeTask(id).label} failed: ${message(err)}`;
        toast(text, 'error', 15000, details ? { label: 'Details', run: openMenu } : null);
      }
    }

    // The last Verify's result, shown under its item (the toast's "Details" opens it).
    const lastVerify = ref(null);

    const checkpoint = () => act('checkpoint', async () => {
      const s = await state.engine.checkpoint(checkpointName.value.trim());
      checkpointName.value = '';
      return `Snapshot at commit ${s.commitSeq}${s.name ? ` ("${s.name}")` : ''}: ${formatCount(s.vertexCount)} V · ${formatCount(s.edgeCount)} E.`;
    });

    const mark = () => act('mark', async () => {
      const name = markName.value.trim();
      if (!name) throw new Error('give the mark a name');
      const m = await state.engine.mark(name);
      markName.value = '';
      return `Mark "${m.name}" at commit ${m.commitSeq}.`;
    });

    const backupDir = ref('');
    const backupFull = ref(false);
    const canBackup = computed(() => !!(state.engine && state.engine.backupStore));
    const backup = (fromMemory) => act(fromMemory ? 'backupFromMemory' : 'backup', async () => {
      const body = { full: backupFull.value, from_memory: !!fromMemory };
      if (backupDir.value.trim()) body.dir = backupDir.value.trim();
      const { backup: b } = await state.engine.backupStore(body);
      const kind = b.fromMemory ? 'from memory' : (b.incremental ? 'incremental' : 'full');
      const warn = b.warnings && b.warnings.length ? ` Warning: ${b.warnings.join('; ')}` : '';
      return `Backup (${kind}) up to commit ${b.commitSeq} in ${b.dir}: ${b.bytesText}, every checksum read back.${warn} Restore it with: ${b.restore}`;
    });

    const verify = () => act('verify', async () => {
      lastVerify.value = null;
      const { verify: v } = await state.engine.verifyStore();
      lastVerify.value = v;
      if (!v.ok) {
        const err = new Error(`Verify found ${v.problems.length} problem(s): ${v.problems.slice(0, 3).map((p) => `${p.path}: ${p.reason}`).join('; ')}${v.problems.length > 3 ? '; …' : ''}`);
        err.storeProblems = true;
        throw err;
      }
      return `Verified: no damage (${v.snapshotsChecked} snapshot(s), ${v.recordsChecked} WAL record(s) checked).`;
    });

    const compaction = computed(() => compactionModel(info.value));
    const compact = () => {
      if (locked.value) return;
      // Asks first (outside the running action, so its clock starts with the work) when marks
      // become unreachable (always) or when not recommended; a mark the advice did not know of
      // yet is refused by the session (409, the toast names it; ask again).
      const model = compaction.value;
      const ask = model && model.confirm;
      if (ask && !window.confirm(ask)) return;
      act('compact', () => compactNow(model));
    };
    const compactNow = async (model) => {
      const { compact: c } = await state.engine.compactStore({ dropMarks: !!(model && model.dropMarks) });
      const lost = c.marksUnreachable && c.marksUnreachable.length
        ? `; the mark(s) ${c.marksUnreachable.join(', ')} are no longer reachable`
        : '';
      const pruned = `pruned ${c.snapshotsRemoved.length} snapshot(s) and ${c.segmentsRemoved.length} WAL segment(s)${lost}`;
      return c.compacted
        ? `Compacted: ${pruned}; the file gave back ${c.bytesFreedText}.`
        : `Compacted: ${pruned}.`;
    };

    /** One of the three actions on a target; View runs at once, Fork/Roll back ask first. */
    function action(id, target) {
      if (id === 'view') {
        act('view', async () => {
          const res = await openStoreView(target);
          return res ? `${res.view.label}. Queries read this state; writes are refused.` : '';
        });
      } else if (dialog.value) {
        dialog.value.open(id, { target });
      }
    }

    function goToCommit() {
      const parsed = parseCommit(goCommit.value, info.value ? info.value.commitSeq : undefined);
      goError.value = parsed.error || '';
      if (parsed.target && dialog.value) dialog.value.open('target', { target: parsed.target });
    }

    function goToTime() {
      const value = goTime.value;
      const offset = value ? -new Date(value).getTimezoneOffset() : undefined;
      const parsed = parseTime(value, Number.isFinite(offset) ? offset : undefined);
      goError.value = parsed.error || '';
      if (parsed.target && dialog.value) dialog.value.open('target', { target: parsed.target });
    }

    const backToLive = () => act('live', async () => {
      await backToLiveGraph();
      return 'Back to the live graph.';
    });

    const attic = computed(() => atticRows(info.value));
    const restore = (row) => act('restore', async () => {
      const res = await restoreAtticEntry(row.id);
      return res ? `Restored: the store is at commit ${res.restored.commitSeq} again.` : '';
    }, row.id);
    const atticDialog = (mode, row) => dialog.value && dialog.value.open(mode, { entry: row });

    const databases = computed(() => databaseRows(info.value));
    const models = computed(() => modelRows(info.value));
    const withoutModels = computed(() => offersWithoutModels(info.value));
    const switchTo = (row) => act('switch', async () => {
      const res = await switchDatabase(row.id);
      return res ? `Switched to "${row.label}" (commit ${row.commit}).` : '';
    }, row.id);
    const closeDb = (row) => {
      if (locked.value) return;
      if (!window.confirm(`Close "${row.label}"? Its history is gone with it (download a .gsnap first to keep its state).`)) return;
      act('close', async () => {
        await closeDatabase(row.id);
        return `Closed "${row.label}".`;
      }, row.id);
    };

    // The menu lists the newest few; the calendar holds them all.
    const recent = computed(() => recentHistory(info.value));
    const openCalendar = () => calendar.value && calendar.value.open();

    onMounted(refresh);
    watch(() => state.graph, refresh);
    watch(() => state.storeVersion, refresh);

    const memory = computed(() => !!(info.value && info.value.memory));
    return {
      catalogCountsText, formatCount,
      state, info, error, refreshing, menu, lastVerify,
      task: (id, title, key) => taskButton(state.storeTask, id, title, key),
      wait: (title) => waitTitle(state.storeTask, title),
      buttonText: computed(() => storeButtonText(info.value, state.storeView, state.storeTask)), checkpointName, markName, goCommit, goTime, goError, dialog,
      EXPLAIN, attic, databases, refresh, checkpoint, mark, verify, compaction, compact, action, goToCommit, goToTime,
      backupDir, backupFull, canBackup, backup,
      backToLive, restore, atticDialog, switchTo, closeDb, viewBanner, commitTarget, markTarget, storeWhere,
      memory, calendar, recent, openCalendar, actionDisabled,
      actions: computed(() => actionsFor(memory.value, !!(info.value && info.value.backup))),
      backupBanner,
      bytesText: formatBytes,
      snapshotUrl: (commit, withModels = true) => (state.engine.snapshotUrl ? state.engine.snapshotUrl(commit, withModels) : null),
      downloadSnapshot: (commit, withModels = true) => downloadStoredSnapshot(commit, withModels),
      models, withoutModels, snapshotFileName,
      saveSnapshot: () => saveGraph('snapshot'),
      locked,
    };
  },
  template: `
  <drop-menu ref="menu" :button-class="'btn hide-narrow' + (state.storeTask ? ' store-busy' : '')" :busy="!!state.storeTask" :title="memory ? 'The store in memory (lost on page reload): position, snapshots, marks, read-only views, forks into databases, rollback, attic' : 'The Store directory of the dev server: position, snapshots, marks, read-only views, forks, rollback, attic'" menu-style="min-width: 380px; max-width: 520px">
    <template #trigger><span @click="refresh"><span v-if="state.storeTask" class="store-spinner" aria-hidden="true"></span>{{ buttonText }}</span></template>
    <div class="menu-title store-title">
      <span>{{ memory ? 'Store in memory' : 'Store' }} <small v-if="info && !memory" style="font-weight:normal;text-transform:none;letter-spacing:0">{{ info.dir }}</small></span>
      <span class="spacer"></span>
      <button type="button" class="icon-btn store-refresh" :class="{ busy: refreshing }" :disabled="refreshing"
              title="Refresh" aria-label="Refresh the store information" :aria-busy="String(refreshing)" @click.stop="refresh">↻</button>
    </div>
    <div v-if="error" class="menu-item" style="cursor:default;color:var(--danger)">{{ error }}</div>
    <template v-if="info">
      <div class="menu-item store-block">
        <div>{{ info.backup ? 'Backup at commit' : 'Live graph at commit' }} <b>{{ info.commitSeq }}</b> · last commit {{ info.lastCommitText }}</div>
        <div v-if="memory"><small>store {{ info.storeBytesText }} in memory · graph {{ info.graphBytesText || '?' }} · {{ info.snapshots.length }} snapshot(s), WAL {{ bytesText(info.walBytes) }}</small></div>
        <div v-else><small>WAL: {{ info.walSegments }} segment(s), {{ bytesText(info.walBytes) }} · store {{ info.storeBytesText }} on disk · graph {{ info.graphId }}</small></div>
        <div v-if="info.definitions !== undefined"><small>catalog: {{ catalogCountsText(info.definitionsByKind, info.definitions) }}</small></div>
        <div :class="{ 'store-volatile': memory }"><small>{{ storeWhere(info) }}</small></div>
        <div v-if="info.damagePolicy"><small title="Fixed when the store was created; forks, backups and repairs carry it">created {{ info.createdText }} · on damage: <b>{{ info.damagePolicy }}</b> ({{ info.damagePolicy === 'continue' ? 'keeps committing and reports it' : 'turns read-only' }}; fixed at creation)</small></div>
        <div v-if="info.lastAutoCheckpointError" style="color:var(--danger)"><small>The automatic checkpoint failed: {{ info.lastAutoCheckpointError }}</small></div>
      </div>
      <div v-if="state.storeView" class="menu-item store-block store-viewing">
        <div><b>{{ viewBanner(state.storeView) }}</b></div>
        <div><small>Queries read this past state; writes are refused.</small></div>
        <button class="btn small" v-bind="task('live', 'Serve the live graph again')" :disabled="locked" @click.stop="backToLive"><store-task-label task="live" label="Back to the live graph"></store-task-label></button>
      </div>
      <template v-if="info.stopped">
        <div class="menu-sep"></div>
        <div class="menu-title" style="color:var(--danger)">Store stopped: nothing more is written</div>
        <div class="menu-item store-block">
          <div><small>{{ info.stopped }}</small></div>
          <div style="margin-top:4px"><small>{{ memory
            ? 'A store in memory cannot be reopened: reload the page to start over.'
            : 'The files on disk are intact. Close and reopen the store (restart the server): the open completes or undoes the change.' }}</small></div>
        </div>
      </template>
      <template v-if="info.damageFound && info.damageFound.length && !info.maintenance">
        <div class="menu-sep"></div>
        <div class="menu-title" style="color:var(--danger)">{{ info.frozenByDamage ? 'Damage found: read-only' : 'Damage found on disk' }}</div>
        <div class="menu-item store-block">
          <div><small>{{ info.damageText }}</small></div>
          <template v-for="(f, i) in info.damageFound" :key="i">
            <div v-for="(p, j) in f.problems.slice(0, 5)" :key="i + '-' + j" style="margin-top:4px">
              <small style="color:var(--danger)"><b>{{ f.foundBy }}</b> {{ p.path }}{{ p.offset !== null && p.offset !== undefined ? ' at byte ' + p.offset : '' }}: {{ p.reason }}</small>
            </div>
          </template>
          <div v-if="info.backupFromMemory && canBackup && !memory" class="store-actions" style="margin-top:6px">
            <input class="input" v-model="backupDir" placeholder="new directory (default: next to the store)" style="flex:1;min-width:0" @click.stop aria-label="Backup directory">
            <button class="btn small" :disabled="locked" @click.stop="backup(true)"
                    v-bind="task('backupFromMemory', 'Write the intact graph in memory into a new backup directory; nothing is written to the damaged store')"><store-task-label task="backupFromMemory" label="Back up from memory"></store-task-label></button>
          </div>
          <div v-if="!memory" style="margin-top:6px"><small>Then repair: <code>graphersal store repair {{ info.dir }} --to &lt;new_dir&gt;</code></small></div>
        </div>
      </template>
      <template v-if="info.maintenance">
        <div class="menu-sep"></div>
        <div class="menu-title" style="color:var(--danger)">Maintenance mode: read-only</div>
        <div class="menu-item store-block">
          <div><small>{{ info.maintenance.recoverable ? 'Every damaged item has a donor: a repair loses nothing.' : 'Some damage has no donor: a repair loses data (restore a backup if you have one).' }}</small></div>
          <div v-if="info.maintenance.lineageNote" style="margin-top:4px"><small><b>lineage</b> {{ info.maintenance.lineageNote }}</small></div>
          <div v-for="(d, i) in info.maintenance.damage" :key="i" style="margin-top:4px">
            <small :style="d.lost ? 'color:var(--danger)' : ''"><b>{{ d.kind }}</b> {{ d.text }}</small>
          </div>
          <div v-if="!memory" style="margin-top:6px"><small>Repair: <code>graphersal store repair {{ info.dir }} --to &lt;new_dir&gt;</code></small></div>
        </div>
      </template>
      <template v-else-if="info.backup">
        <div class="menu-sep"></div>
        <div class="menu-title">Backup: read-only</div>
        <div class="menu-item store-block">
          <div><small>{{ backupBanner(info.backup) }}</small></div>
          <div style="margin-top:6px"><small>An independent writable copy instead: <code>{{ info.backup.fork }}</code> (or Fork… on any point below).</small></div>
        </div>
      </template>
      <template v-else-if="!state.storeView && !info.frozenByDamage">
        <div class="menu-sep"></div>
        <div class="menu-item" style="cursor:default;gap:6px">
          <input class="input" v-model="checkpointName" placeholder="snapshot name (optional)" style="flex:1;min-width:0"
                 @keydown.enter.prevent="checkpoint" @click.stop aria-label="Snapshot name">
          <button class="btn small" :disabled="locked" @click.stop="checkpoint"
                  v-bind="task('checkpoint', 'Write a snapshot of the current state (a full copy: a fast start and a fast view of this point)')"><store-task-label task="checkpoint" label="Checkpoint"></store-task-label></button>
        </div>
        <div class="menu-item" style="cursor:default;gap:6px">
          <input class="input" v-model="markName" placeholder="mark name" style="flex:1;min-width:0"
                 @keydown.enter.prevent="mark" @click.stop aria-label="Mark name">
          <button class="btn small" :disabled="locked || !markName.trim()" @click.stop="mark"
                  v-bind="task('mark', 'Name the current position (costs nothing; a target to view, fork or roll back to later)')"><store-task-label task="mark" label="Mark"></store-task-label></button>
        </div>
      </template>

      <template v-if="memory && databases.length">
        <div class="menu-sep"></div>
        <div class="menu-title">Databases ({{ databases.length }})</div>
        <div class="store-explain">{{ EXPLAIN.database }}</div>
        <div v-for="db in databases" :key="db.id" class="menu-item store-row" :class="{ 'store-active-db': db.active }">
          <div class="store-row-head">
            <span>{{ db.label }}</span><small>commit {{ db.commit }}{{ db.active ? ' · in use' : '' }}</small>
          </div>
          <div v-if="!db.active" class="store-actions">
            <button class="btn small" :disabled="locked" @click.stop="switchTo(db)" v-bind="task('switch', 'Serve this database (the results of the tabs may be stale afterwards)', db.id)"><store-task-label task="switch" :task-key="db.id" label="Switch to"></store-task-label></button>
            <button class="btn small" :disabled="locked" @click.stop="closeDb(db)" v-bind="task('close', 'Close it and free its memory', db.id)"><store-task-label task="close" :task-key="db.id" label="Close"></store-task-label></button>
          </div>
        </div>
      </template>

      <div class="menu-sep"></div>
      <div class="menu-title">Marks ({{ info.marks.length }})</div>
      <div class="store-explain">{{ EXPLAIN.mark }}</div>
      <div v-if="!info.marks.length" class="menu-item" style="cursor:default"><small>none yet</small></div>
      <div v-for="m in recent.marks" :key="m.name" class="menu-item store-row">
        <div class="store-row-head">
          <span>{{ m.name }}</span><small>commit {{ m.commitSeq }} · {{ m.timeText }}</small>
        </div>
        <div class="store-actions">
          <button v-for="a in actions" :key="a.id" class="btn small" :disabled="actionDisabled(info, a.id, m.commitSeq, locked)"
                  :title="wait(a.title)" @click.stop="action(a.id, markTarget(m.name))">{{ a.short }}</button>
        </div>
      </div>

      <div class="menu-sep"></div>
      <div class="menu-title">Snapshots ({{ info.snapshots.length }})</div>
      <div class="store-explain">{{ EXPLAIN.snapshot }}</div>
      <div v-for="s in recent.snapshots" :key="s.commitSeq" class="menu-item store-row">
        <div class="store-row-head">
          <span>#{{ s.commitSeq }}<b v-if="s.name">&nbsp;{{ s.name }}</b></span>
          <small>{{ s.lastCommitText }} · {{ formatCount(s.vertexCount) }} V · {{ formatCount(s.edgeCount) }} E</small>
        </div>
        <div class="store-actions">
          <button v-for="a in actions" :key="a.id" class="btn small" :disabled="actionDisabled(info, a.id, s.commitSeq, locked)"
                  :title="wait(a.title)" @click.stop="action(a.id, commitTarget(s.commitSeq))">{{ a.short }}</button>
          <a v-if="snapshotUrl(s.commitSeq)" class="btn small" :href="snapshotUrl(s.commitSeq)" :download="snapshotFileName(s.commitSeq)"
             :title="withoutModels ? 'Download this snapshot as one packed file (.gsnap) WITH the embedding models of the store and their licences' : 'Download this snapshot as one packed file (.gsnap)'">.gsnap</a>
          <button v-else class="btn small" @click.stop="downloadSnapshot(s.commitSeq)"
                  :title="withoutModels ? 'Download this snapshot as one packed file (.gsnap) WITH the embedding models of the store and their licences' : 'Download this snapshot as one packed file (.gsnap)'">.gsnap</button>
          <template v-if="withoutModels">
            <a v-if="snapshotUrl(s.commitSeq, false)" class="btn small" :href="snapshotUrl(s.commitSeq, false)" :download="snapshotFileName(s.commitSeq, false)"
               title="Download this snapshot WITHOUT the embedding models (to hand it on without redistributing them)">.gsnap without models</a>
            <button v-else class="btn small" @click.stop="downloadSnapshot(s.commitSeq, false)"
                    title="Download this snapshot WITHOUT the embedding models (to hand it on without redistributing them)">.gsnap without models</button>
          </template>
        </div>
      </div>
      <button v-if="info.marks.length || info.snapshots.length" class="menu-item store-more" @click="openCalendar"
              :title="'Every mark and snapshot on a month calendar (local time), with the same actions'">
        {{ recent.more ? 'See all (' + recent.markCount + ' marks, ' + recent.snapshotCount + ' snapshots) in the calendar…' : 'Calendar…' }}
      </button>

      <template v-if="models.length">
        <div class="menu-sep"></div>
        <div class="menu-title">Models ({{ models.filter((m) => !m.missing).length }})</div>
        <div class="store-explain">{{ EXPLAIN.model }}</div>
        <div v-for="m in models" :key="m.id" class="menu-item store-row store-model">
          <div class="store-row-head">
            <span :style="m.missing ? 'color:var(--danger)' : ''">{{ m.identity }}</span>
            <small v-if="!m.missing">{{ m.size }} · licence {{ m.licence }} ({{ m.licenceText }}){{ m.gated ? ' · gated repository' : '' }} · {{ m.used ? 'used by the schema' : 'not used by the schema' }}</small>
          </div>
          <div v-if="m.problem"><small style="color:var(--danger)">{{ m.problem }}</small></div>
        </div>
      </template>

      <div class="menu-sep"></div>
      <div class="menu-title">Go to any point</div>
      <div class="menu-item" style="cursor:default;gap:6px">
        <input class="input" v-model="goCommit" placeholder="commit #" inputmode="numeric" style="flex:1;min-width:0"
               @keydown.enter.prevent="goToCommit" @click.stop aria-label="Commit number">
        <button class="btn small" :disabled="locked" @click.stop="goToCommit" :title="wait('View, fork or roll back to this commit')">Go to commit…</button>
      </div>
      <div class="menu-item" style="cursor:default;gap:6px">
        <input class="input" type="datetime-local" step="1" v-model="goTime" style="flex:1;min-width:0"
               @keydown.enter.prevent="goToTime" @click.stop aria-label="Date and time (local)">
        <button class="btn small" :disabled="locked" @click.stop="goToTime" :title="wait('The last commit at or before this time (your local time zone)')">Go to time…</button>
      </div>
      <div v-if="goError" class="menu-item" style="cursor:default;color:var(--danger)"><small>{{ goError }}</small></div>

      <template v-if="attic.length">
        <div class="menu-sep"></div>
        <div class="menu-title">Attic ({{ attic.length }})</div>
        <div class="store-explain">{{ EXPLAIN.attic }}</div>
        <div v-for="a in attic" :key="a.id" class="menu-item store-row">
          <div class="store-row-head">
            <span>{{ a.range }}</span><small>rolled back {{ a.when }} · {{ a.size }}</small>
          </div>
          <div v-if="a.blocked" class="store-explain">Restore is not possible: {{ a.blocked }}</div>
          <div class="store-actions">
            <button class="btn small" :disabled="locked" @click.stop="atticDialog('changes', a)" :title="wait('What the rolled-back commits changed')">Show changes</button>
            <button class="btn small" :disabled="locked || !a.restorable" @click.stop="restore(a)"
                    v-bind="task('restore', a.restorable ? 'Undo the rollback: the store goes back to the end of this history' : 'Not possible: ' + a.blocked + ' (fork it instead)', a.id)"><store-task-label task="restore" :task-key="a.id" label="Restore"></store-task-label></button>
            <button class="btn small" :disabled="locked" @click.stop="atticDialog('atticFork', a)" :title="wait(memory ? 'The rolled-back history as a NEW database in memory' : 'The rolled-back history as a NEW store directory')">Fork…</button>
            <button class="btn small" :disabled="locked || !!info.maintenance" @click.stop="atticDialog('atticRemove', a)" :title="wait('Delete this entry for good')">Remove…</button>
          </div>
        </div>
      </template>
      <template v-if="compaction && !info.maintenance && !info.backup">
        <div class="menu-sep"></div>
        <div class="menu-title">Disk space</div>
        <div class="menu-item store-block store-compaction">
          <div><b :style="compaction.recommended ? 'color:var(--accent)' : ''">{{ compaction.headline }}</b> <small>· {{ compaction.reason }}</small></div>
          <div v-for="(line, i) in compaction.lines" :key="i"><small>{{ line }}</small></div>
          <div class="store-actions">
            <button class="btn small" :disabled="actionDisabled(info, 'compact', null, locked, !!state.storeView)" @click.stop="compact"
                    v-bind="task('compact', state.storeView ? 'Go back to the live graph first' : compaction.title)"><store-task-label task="compact" label="Compact"></store-task-label></button>
          </div>
        </div>
      </template>
      <template v-if="canBackup && !memory && !info.backup && !info.maintenance">
        <div class="menu-sep"></div>
        <div class="menu-title">Backup</div>
        <div class="store-explain">A copy on the server's disk: full into a new directory, then increments into it. Every checksum is read in the store and again in the copy; damage stops it.</div>
        <div class="menu-item" style="cursor:default;gap:6px">
          <input class="input" v-model="backupDir" placeholder="directory (default: &lt;store&gt;-backup next to it)" style="flex:1;min-width:0"
                 @keydown.enter.prevent="backup(false)" @click.stop aria-label="Backup directory">
          <label @click.stop title="Always a full backup (the directory must be empty or new)"><input type="checkbox" v-model="backupFull"> full</label>
          <button class="btn small" :disabled="locked" @click.stop="backup(false)" v-bind="task('backup', 'Back the store up (works while queries and commits go on)')"><store-task-label task="backup" label="Back up"></store-task-label></button>
        </div>
      </template>
      <div class="menu-sep"></div>
      <button class="menu-item" data-keep-open :disabled="locked" @click="verify"
              v-bind="task('verify', 'Check every checksum of the store: snapshots, WAL records, commit continuity, lineage')"><store-task-label task="verify" label="Verify the store"></store-task-label></button>
      <div v-if="lastVerify" class="menu-item store-block store-verify-result">
        <div v-if="lastVerify.ok"><small><b>Last verify: no damage</b> · {{ lastVerify.snapshotsChecked }} snapshot(s), {{ lastVerify.recordsChecked }} WAL record(s) checked</small></div>
        <template v-else>
          <div style="color:var(--danger)"><small><b>Last verify: {{ lastVerify.problems.length }} problem(s)</b></small></div>
          <div v-for="(p, i) in lastVerify.problems.slice(0, 20)" :key="i"><small style="color:var(--danger)">{{ p.path }}{{ p.offset !== null && p.offset !== undefined ? ' at byte ' + p.offset : '' }}: {{ p.reason }}</small></div>
          <div v-if="lastVerify.problems.length > 20"><small>… and {{ lastVerify.problems.length - 20 }} more</small></div>
        </template>
      </div>
      <button class="menu-item" :disabled="state.saving || state.running" @click="saveSnapshot"
              title="Download the state shown now (the live graph, or the read-only view) as one packed file (.gsnap)">Download current state (.gsnap)</button>
    </template>
  </drop-menu>
  <div class="sr-only" role="status" aria-live="polite">{{ state.storeAnnounce }}</div>
  <store-dialog ref="dialog" :store="info" @changed="refresh"></store-dialog>
  <store-calendar ref="calendar" :store="info" :actions="actions" :locked="locked" :snapshot-url="snapshotUrl"
                  @action="action" @download="downloadSnapshot"></store-calendar>`,
};
