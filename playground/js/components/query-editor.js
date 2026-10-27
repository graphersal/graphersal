// Query editor panel: CodeMirror 6 with DSL highlighting, step completion, run on Mod-Enter,
// query history (Alt-Up / Alt-Down, and the History menu) and example queries. The query tabs
// (query-tabs.js) share this one editor view: each tab keeps its own CodeMirror state (text,
// selection, undo history), swapped in when the tab becomes active. Mod-Shift-Enter runs all tabs.
// Run (button, Mod-Enter) executes the main selection when it holds more than whitespace, else the
// whole tab (js/editor/run-selection.js).
// The Examples button opens the Examples panel (examples-panel.js: an overlay drawer with search and
// collapsible groups). Its GQL group starts with "Wrap as GQL": the selection (or the whole tab)
// holding a GQL query becomes the script g.gql(`...`) that runs it (js/editor/gql-wrap.js).
// An AI Chat tab (tab kind 'ai') shows the chat (ai-chat.js) in place of the editor; the editor
// view stays mounted (hidden) so the query tabs keep their editor states.
// A GQL tab (tab language 'gql', js/gql-tab.js) swaps the language compartment: GQL highlighting
// (editor/gql-language.js) and the GQL completion (editor/gql-completion.js: keywords by
// position, functions, labels and keys of the graph, variables, $parameters); a Parameters field
// (a JSON map) sits under its editor, and an error's GQL span is underlined in the text.

import { ref, computed, watch, onMounted, onBeforeUnmount, markRaw } from 'vue';
import { EditorState, Prec, Compartment, StateField, StateEffect } from '@codemirror/state';
import {
  EditorView, Decoration, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, placeholder,
} from '@codemirror/view';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { bracketMatching, indentOnInput, language as languageFacet } from '@codemirror/language';
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete';
import { searchKeymap, highlightSelectionMatches } from '@codemirror/search';
import {
  state, run, runAllTabs, stopRun, saveScript, openScript, clearHistory, setRunOption, loadGraph,
  setTabLanguage, saveParams, addQueryParams, gqlVocabulary, toast, isServerMode,
} from '../store.js';
import { formatMs, modKey } from '../util.js';
import { EXAMPLES } from '../config.js';
import { dslLanguage, dslHighlight } from '../editor/dsl-language.js';
import { dslCompletionSource } from '../editor/completion.js';
import { loadCompletionData } from '../editor/completion-data.js';
import { floatingTooltips } from '../editor/tooltips.js';
import { runStatusText, cursorTexts } from '../editor/editor-status.js';
import { runText } from '../editor/run-selection.js';
import { wrapGql } from '../editor/gql-wrap.js';
import { gqlLanguage } from '../editor/gql-language.js';
import { gqlCompletionSource } from '../editor/gql-completion.js';
import { parseParams, paramsErrorText, spanRange } from '../gql-tab.js';
import { gqlParamNames } from '../editor/gql-tokens.js';
import { PanelFrame } from './panel-frame.js';
import { DropMenu } from './drop-menu.js';
import { ExamplesPanel } from './examples-panel.js';
import { QueryTabs } from './query-tabs.js';
import { AiChat } from './ai-chat.js';

export const editorTheme = EditorView.theme({
  '&': { height: '100%' },
  '.cm-content': { caretColor: 'var(--text)', padding: '8px 0' },
  '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--text)' },
  '.cm-gutters': {
    backgroundColor: 'var(--surface-2)',
    color: 'var(--text-faint)',
    border: 'none',
    borderRight: '1px solid var(--border)',
  },
  '.cm-activeLine': { backgroundColor: 'rgba(127, 127, 127, 0.07)' },
  '.cm-activeLineGutter': { backgroundColor: 'var(--surface-3)', color: 'var(--text-muted)' },
  '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': {
    backgroundColor: 'var(--selection) !important',
  },
  '.cm-matchingBracket': { backgroundColor: 'var(--accent-soft)', outline: '1px solid var(--accent)' },
  '.cm-selectionMatch': { backgroundColor: 'var(--accent-soft)' },
  '.cm-placeholder': { color: 'var(--text-faint)' },
  '.cm-panels': { backgroundColor: 'var(--surface-2)', color: 'var(--text)' },
  '.cm-panels input, .cm-panels button': { color: 'var(--text)' },
});

/** Sets (or clears, with null) the underlined error span `{from, to}` of the editor. */
const setErrorSpan = StateEffect.define();
const errorMark = Decoration.mark({ class: 'cm-gql-error', attributes: { title: 'The engine reported the error here' } });
/** The underline of a GQL error span: kept until the text changes. */
const errorSpanField = StateField.define({
  create: () => Decoration.none,
  update(marks, tr) {
    for (const e of tr.effects) {
      if (e.is(setErrorSpan)) {
        const r = e.value;
        return r && r.to > r.from && r.to <= tr.state.doc.length ? Decoration.set([errorMark.range(r.from, r.to)]) : Decoration.none;
      }
    }
    return tr.docChanged ? Decoration.none : marks;
  },
  provide: (f) => EditorView.decorations.from(f),
});

const RHAI_PLACEHOLDER = 'g.v().has_label("person").values("name")   or GQL:   g.gql("MATCH (p:person) RETURN p.name AS name")';
const GQL_PLACEHOLDER = 'MATCH (p:person) WHERE p.age > $min RETURN p.name AS name   (GQL; parameters below)';

export const QueryEditor = {
  components: { PanelFrame, DropMenu, ExamplesPanel, QueryTabs, AiChat },
  setup(props, { expose }) {
    const host = ref(null);
    const cursor = ref({ line: 1, col: 1 });
    // Whether the main selection holds more than whitespace (Run then runs only it).
    const hasSelection = ref(false);
    // The completion data (`loadCompletionData`): the function catalogue and the token values.
    const completionData = ref(null);
    let view = null;
    let historyIndex = -1;
    let draft = '';
    // One CodeMirror EditorState per tab (its text, selection and undo history); the view shows
    // the active tab's. Not reactive: CodeMirror states are immutable values.
    const editorStates = new Map();
    let shownTabId = null;
    let extensions = [];
    // The language of the shown tab: Rhai (DSL highlighting) or GQL, with its placeholder.
    const langConf = new Compartment();
    const langExtension = (lang) => (lang === 'gql'
      ? [gqlLanguage, placeholder(GQL_PLACEHOLDER)]
      : [dslLanguage, placeholder(RHAI_PLACEHOLDER)]);
    const isGqlState = (st) => st.facet(languageFacet) === gqlLanguage;
    const dslSource = dslCompletionSource(() => completionData.value, () => state.savedQueries);
    // The $parameters offered first: the keys of the shown tab's Parameters field.
    const gqlSource = gqlCompletionSource(gqlVocabulary, () => {
      const tab = state.tabs.find((t) => t.id === shownTabId);
      const parsed = tab ? parseParams(tab.params) : null;
      return parsed && parsed.ok ? parsed.names.map((name) => ({ name, detail: 'Parameters field' })) : [];
    }, () => state.savedQueries);
    const completionSource = (ctx) => (isGqlState(ctx.state) ? gqlSource(ctx) : dslSource(ctx));

    function setDoc(text, focus = true) {
      if (!view) return;
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text }, selection: { anchor: text.length } });
      if (focus) view.focus();
    }

    function historyStep(delta) {
      const list = state.history;
      if (!list.length) return false;
      if (historyIndex === -1) draft = view.state.doc.toString();
      const next = Math.min(list.length - 1, Math.max(-1, historyIndex + delta));
      if (next === historyIndex) return true;
      historyIndex = next;
      setDoc(next === -1 ? draft : list[next].script);
      return true;
    }

    function selectionOf(editorState) {
      const { from, to } = editorState.selection.main;
      return from !== to && editorState.sliceDoc(from, to).trim() !== '';
    }

    /** Runs the main selection when it holds more than whitespace, else the whole tab. */
    function runNow() {
      historyIndex = -1;
      if (!view || shownTabId !== state.tab.id) {
        run(state.tab.script, state.tab.id);
        return true;
      }
      const doc = view.state.doc.toString();
      saveScript(doc, shownTabId);
      const { from, to } = view.state.selection.main;
      const picked = runText(doc, from, to);
      if (picked.selection) run(picked.text, state.tab.id, { selection: picked });
      else run(doc, state.tab.id);
      return true;
    }

    function runAll() {
      historyIndex = -1;
      if (view) saveScript(view.state.doc.toString(), shownTabId);
      runAllTabs();
      return true;
    }

    function makeState(doc, lang = 'rhai') {
      return EditorState.create({ doc, extensions: [...extensions, langConf.of(langExtension(lang))] });
    }

    /** Makes the editor's language that of `lang` (a tab switched, or a state of the other language). */
    function syncLanguage(lang) {
      if (!view || isGqlState(view.state) === (lang === 'gql')) return;
      view.dispatch({ effects: langConf.reconfigure(langExtension(lang)) });
      if (lang === 'gql') gqlVocabulary();
    }

    /** Underlines the GQL span of the shown tab's last error (when its text still is what ran). */
    function showErrorSpan() {
      if (!view) return;
      const tab = state.tabs.find((t) => t.id === shownTabId);
      const err = tab && tab.language === 'gql' && tab.result && tab.result.execution && tab.result.execution.error;
      const span = err && err.span;
      const range = span ? spanRange(view.state.doc.toString(), span, span.offset) : null;
      view.dispatch({ effects: setErrorSpan.of(range) });
    }

    /** Shows the active tab: keeps the outgoing tab's editor state, restores (or creates) the incoming one's. */
    function showActiveTab() {
      if (!view) return;
      const tab = state.tab;
      if (tab.kind === 'ai') return; // a chat tab: the editor keeps the last query tab
      if (shownTabId !== tab.id) {
        if (shownTabId !== null) editorStates.set(shownTabId, view.state);
        const saved = editorStates.get(tab.id);
        view.setState(saved && saved.doc.toString() === tab.script ? saved : makeState(tab.script, tab.language));
        shownTabId = tab.id;
        syncLanguage(tab.language);
        showErrorSpan();
        hasSelection.value = selectionOf(view.state);
        historyIndex = -1;
        const pos = view.state.selection.main.head;
        const line = view.state.doc.lineAt(pos);
        cursor.value = { line: line.number, col: pos - line.from + 1 };
      } else {
        syncLanguage(tab.language);
        if (tab.script !== view.state.doc.toString()) {
          // The text changed outside the editor (an example, a history entry, the Rhai / GQL
          // switch): one undoable change.
          setDoc(tab.script, false);
        }
      }
    }

    onMounted(async () => {
      const runKeys = Prec.highest(keymap.of([
        { key: 'Mod-Enter', run: runNow, preventDefault: true },
        { key: 'Mod-Shift-Enter', run: runAll, preventDefault: true },
        { key: 'Alt-ArrowUp', run: () => historyStep(1), preventDefault: true },
        { key: 'Alt-ArrowDown', run: () => historyStep(-1), preventDefault: true },
      ]));
      extensions = [
            runKeys,
            lineNumbers(),
            highlightActiveLineGutter(),
            history(),
            drawSelection(),
            indentOnInput(),
            bracketMatching(),
            closeBrackets(),
            highlightActiveLine(),
            highlightSelectionMatches(),
            floatingTooltips(host.value),
            errorSpanField,
            autocompletion({
              override: [completionSource],
              activateOnTyping: true,
              icons: true,
            }),
            keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, ...completionKeymap]),
            dslHighlight,
            editorTheme,
            EditorView.lineWrapping,
            EditorView.contentAttributes.of({ 'aria-label': 'Query editor' }),
            EditorView.updateListener.of((u) => {
              if (u.docChanged && shownTabId !== null) saveScript(u.state.doc.toString(), shownTabId);
              if (u.selectionSet || u.docChanged) {
                hasSelection.value = selectionOf(u.state);
                const pos = u.state.selection.main.head;
                const line = u.state.doc.lineAt(pos);
                cursor.value = { line: line.number, col: pos - line.from + 1 };
              }
            }),
      ];
      const first = state.tab.kind === 'ai' ? state.tabs.find((t) => t.kind !== 'ai') : state.tab;
      shownTabId = first ? first.id : null;
      view = markRaw(new EditorView({ parent: host.value, state: makeState(first ? first.script : '', first ? first.language : 'rhai') }));
      if (first && first.language === 'gql') gqlVocabulary();
      if (!isChat.value) view.focus();
      completionData.value = markRaw(await loadCompletionData(state.engine));
    });

    onBeforeUnmount(() => {
      if (view) view.destroy();
      view = null;
    });

    // Switching tabs swaps the editor state; a text changed outside the editor (an example, a
    // history entry, the Profile panel's example) is put into the editor.
    watch(() => [state.activeId, state.tab.script, state.tab.language], showActiveTab);
    // A finished run of the shown tab: underline its GQL error span (or clear the old one).
    watch(() => state.tab.result, () => {
      if (state.tab.id === shownTabId) showErrorSpan();
    });
    // Forget the editor states of closed tabs.
    watch(() => state.tabs.map((t) => t.id).join(','), () => {
      const ids = new Set(state.tabs.map((t) => t.id));
      for (const id of [...editorStates.keys()]) if (!ids.has(id)) editorStates.delete(id);
    });

    watch(() => state.jumpTo, (req) => {
      if (!req || !view) return;
      const doc = view.state.doc;
      const lineNo = Math.min(Math.max(1, req.line || 1), doc.lines);
      const line = doc.line(lineNo);
      const pos = Math.min(line.to, line.from + Math.max(0, (req.column || 1) - 1));
      view.dispatch({ selection: { anchor: pos }, scrollIntoView: true });
      view.focus();
    });

    const isChat = computed(() => state.tab.kind === 'ai');
    const runningHere = computed(() => state.running && state.runningTabId === state.tab.id);
    const runningText = computed(() => {
      const time = state.runningElapsedMs > 0 ? ` · ${formatMs(state.runningElapsedMs)}` : '';
      return state.stopping ? `Stopping…${time}` : `Running…${time}`;
    });
    const status = computed(() => {
      if (runningHere.value) return null;
      const other = state.running && state.tabs.find((t) => t.id === state.runningTabId);
      return runStatusText({
        result: state.tab.result,
        stale: state.tab.stale,
        elsewhere: other ? other.name : null,
        elapsedMs: state.runningElapsedMs,
      });
    });
    const cursorText = computed(() => cursorTexts(cursor.value));

    const SAMPLE_TITLES = { empty: 'Empty graph', modern: 'Modern (TinkerPop)', large: 'Large generated graph', tree: 'File tree' };
    /**
     * An example goes into the active tab when that tab is empty or still holds an unmodified
     * example; otherwise into a new tab, so user text is never overwritten.
     */
    async function useExample(ex) {
      historyIndex = -1;
      const current = state.graph && state.graph.kind;
      if (ex.graph && ex.graph !== 'any' && ex.graph !== current) {
        const title = SAMPLE_TITLES[ex.graph] || ex.graph;
        if (window.confirm(`This example is written for the "${title}" sample graph. Load it now? This replaces the current graph.`)) {
          await loadGraph({ kind: ex.graph });
        }
      }
      // An example of the GQL group with `lang: "gql"` opens as a GQL tab, its `params` (a JSON
      // map) in the Parameters field.
      const gqlExample = ex.lang === 'gql';
      openScript({
        title: ex.title,
        script: ex.script,
        language: gqlExample ? 'gql' : 'rhai',
        params: gqlExample && ex.params ? JSON.stringify(ex.params, null, 2) : '',
      });
      if (view) view.focus();
    }
    /**
     * Wraps the selection (when it holds more than whitespace) or the whole tab into
     * g.gql(`...`): one undoable change; an empty tab gets a template query.
     */
    function wrapAsGql() {
      if (!view) return;
      if (state.tab.language === 'gql') {
        toast('This tab is a GQL tab: write the GQL query as it is (switch to Rhai to get g.gql(…)).', 'info', 5000);
        return;
      }
      const doc = view.state.doc.toString();
      const { from, to } = view.state.selection.main;
      const picked = to > from && doc.slice(from, to).trim() !== '';
      const [lo, hi] = picked ? [from, to] : [0, doc.length];
      const insert = wrapGql(doc.slice(lo, hi));
      view.dispatch({ changes: { from: lo, to: hi, insert }, selection: { anchor: lo + insert.length } });
      view.focus();
    }
    /** A history entry follows the same rule as an example (the reused tab keeps its name). */
    function useHistory(h) {
      historyIndex = -1;
      openScript({ script: h.script, language: h.language, params: h.params || '' });
      if (view) view.focus();
    }
    function oneLine(s) {
      return s.replace(/\s+/g, ' ');
    }

    const runTitle = computed(() => (hasSelection.value
      ? `Run the selection (${modKey}+Enter): only the selected text runs; with several selections the main one`
      : `Run the query (${modKey}+Enter); select a part of the script to run only that part`));

    const isGql = computed(() => state.tab.kind !== 'ai' && state.tab.language === 'gql');
    /** The Rhai / GQL switch: the text is rewritten (js/gql-tab.js `switchTabLanguage`). */
    function switchLanguage(lang) {
      if (state.tab.language === lang) return;
      if (view && shownTabId === state.tab.id) saveScript(view.state.doc.toString(), shownTabId);
      if (setTabLanguage(lang) && view) view.focus();
    }
    // The Parameters field: its validation and the $names the query reads that it lacks.
    const paramsCheck = computed(() => (isGql.value ? parseParams(state.tab.params) : { ok: true, names: [] }));
    const paramsError = computed(() => paramsErrorText(paramsCheck.value));
    const missingParams = computed(() => {
      if (!isGql.value) return [];
      const have = paramsCheck.value.ok ? paramsCheck.value.names : [];
      return gqlParamNames(state.tab.script).filter((n) => !have.includes(n));
    });
    function onParams(e) {
      saveParams(e.target.value, state.tab.id);
    }

    expose({ focus: () => view && view.focus() });

    return {
      state, host, cursorText, status, runningHere, isChat, runningText, examples: EXAMPLES, serverMode: isServerMode, modKey, setRunOption, stopRun,
      useExample, useHistory, wrapAsGql, oneLine, clearHistory, runNow, hasSelection, runTitle,
      isGql, switchLanguage, paramsError, missingParams, onParams, addQueryParams,
    };
  },
  template: `
  <panel-frame title="Query">
    <template #actions>
      <template v-if="!isChat">
      <span class="segmented lang-switch" role="group" aria-label="Query language of this tab">
        <button type="button" class="seg-btn" :class="{ active: !isGql }" :aria-pressed="String(!isGql)" @click="switchLanguage('rhai')"
                title="Rhai: the Gremlin DSL (g.v()…); a tab holding plain GQL becomes g.gql(…, #{…})">Rhai</button>
        <button type="button" class="seg-btn" :class="{ active: isGql }" :aria-pressed="String(isGql)" @click="switchLanguage('gql')"
                title="GQL: write the query itself (MATCH … RETURN …) with its parameters below; a tab holding one g.gql(…) call becomes its query">GQL</button>
      </span>
      <button class="btn primary small run-btn" @click="runNow()" :disabled="state.running"
              :title="runTitle" :aria-label="hasSelection ? 'Run the selection' : 'Run the query'">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4.5 2.8v10.4L13 8z" fill="currentColor"/></svg>
        <span class="hide-narrow">{{ state.running ? 'Running…' : hasSelection ? 'Run selection' : 'Run' }}</span>
      </button>
      <button class="btn small stop-btn" @click="stopRun()" :disabled="!(state.running || state.layoutsRunning) || state.stopping"
              :title="state.running || !state.layoutsRunning
                ? 'Stop the running query: restarts the engine and restores the graph as last loaded or saved (later changes are lost)'
                : 'Stop the layout of the graph drawing: it keeps the positions reached so far'"
              :aria-label="state.running || !state.layoutsRunning ? 'Stop the running query' : 'Stop the layout'">
        <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="4" y="4" width="8" height="8" rx="1" fill="currentColor"/></svg>
        <span class="hide-narrow">{{ state.stopping ? 'Stopping…' : 'Stop' }}</span>
      </button>
      <label class="toggle" title="Collect metrics on every run of a traversal without a terminal (shown in the Profile panel); set per tab">
        <input type="checkbox" :checked="state.tab.runOptions.profile" @change="setRunOption('profile', $event.target.checked)"> Profile
      </label>
      <label class="toggle" :class="{ off: !state.tab.runOptions.profile }"
             title="Add the Memory section (ProfileType Memory) to the metrics: exact allocator figures (tracked) in this build">
        <input type="checkbox" :checked="state.tab.runOptions.memory" :disabled="!state.tab.runOptions.profile"
               @change="setRunOption('memory', $event.target.checked)"> Memory
      </label>
      <!-- The examples are written for the sample graphs; the dev server serves its own graph. -->
      <examples-panel v-if="!serverMode()" :examples="examples" @use="useExample" @wrap-gql="wrapAsGql()"></examples-panel>
      <drop-menu title="Query history (Alt+Up / Alt+Down in the editor)" menu-style="width: min(460px, 90vw); max-height: 60vh; overflow-y: auto">
        <template #trigger>History ▾</template>
          <div class="menu-title">Recent queries</div>
          <div v-if="!state.history.length" class="menu-item muted" style="cursor:default">No queries yet.</div>
          <button v-for="h in state.history" :key="h.at + h.script" class="menu-item" @click="useHistory(h)" :title="h.script">
            <span :style="{ color: h.ok ? 'var(--ok)' : 'var(--danger)' }">{{ h.ok ? '✓' : '✗' }}</span>
            <span class="code">{{ oneLine(h.script) }}</span>
            <span v-if="h.tab" class="history-tab faint" :title="'Run in tab ' + h.tab">{{ h.tab }}</span>
          </button>
          <template v-if="state.history.length">
            <div class="menu-sep"></div>
            <button class="menu-item" @click="clearHistory()">Clear history</button>
          </template>
      </drop-menu>
      </template>
    </template>
    <div class="fill">
      <query-tabs></query-tabs>
      <ai-chat v-if="isChat" :key="state.tab.id" :tab="state.tab"></ai-chat>
      <div class="editor-host" ref="host" v-show="!isChat" style="flex:1;min-height:0"></div>
      <div v-if="isGql" class="gql-params">
        <label class="gql-params-label" for="gql-params-input" title="A JSON map: $name in the query reads the value of &quot;name&quot;">Parameters</label>
        <textarea id="gql-params-input" class="gql-params-input mono" rows="1" spellcheck="false"
                  :class="{ invalid: !!paramsError }" :value="state.tab.params" @input="onParams"
                  placeholder='{"min": 30}' aria-describedby="gql-params-note"></textarea>
        <button type="button" class="btn small gql-params-add" @click="addQueryParams()" :disabled="!missingParams.length"
                :title="missingParams.length ? 'Add ' + missingParams.map((n) => '$' + n).join(', ') + ' to the map (null, to be filled in)' : 'The map holds every $parameter of the query'"
                aria-label="Add the $parameters the query reads">+ <span class="hide-narrow">Add </span>$parameters</button>
        <div id="gql-params-note" class="gql-params-note" :class="{ error: !!paramsError }" role="status">{{ paramsError || (missingParams.length ? 'The query reads ' + missingParams.map((n) => '$' + n).join(', ') + ', missing from the map.' : '') }}</div>
      </div>
      <div class="editor-status" v-show="!isChat">
        <span class="status-run" :title="runningHere ? runningText : status">
          <span v-if="runningHere" class="running-dot" aria-hidden="true"></span>
          <span class="status-text">{{ runningHere ? runningText : status }}</span>
        </span>
        <span class="spacer"></span>
        <span class="status-hints">{{ modKey }}+Enter run{{ hasSelection ? ' selection' : '' }} · Ctrl+Space complete · Alt+↑/↓ history</span>
        <span class="status-cursor mono" :title="cursorText.title">
          <span class="cursor-long">{{ cursorText.long }}</span><span class="cursor-short">{{ cursorText.short }}</span>
        </span>
      </div>
    </div>
  </panel-frame>
  `,
};
