// One module tab of a UI profile (js/ui-profiles.js `tabs: [{id, title, module}]`): loads the
// profile's ES module with `defineAsyncComponent(() => import(files + module))` and renders its
// default export with the props `api` (js/profile-api.js, EXPERIMENTAL) and `params` (the tab's
// address parameters). A module that cannot be loaded, has no default export or throws while
// rendering shows an error card in its tab; the rest of the page keeps working.

import { defineAsyncComponent, h, ref, shallowRef, onErrorCaptured, watch, markRaw } from 'vue';
import { state, isServerMode, isDark, toast, profileQuery } from '../store.js';
import { navigateTab } from '../router.js';
import { cssVar } from '../util.js';
import { createProfileApi } from '../profile-api.js';
import { MarkdownView } from './markdown-view.js';
import { ProfileCard } from './profile-card.js';
import { ProfileGraph } from './profile-graph.js';
import { checkCardOptions } from '../profile-cards.js';

/** Toast kinds of the profile API onto the page's toast classes. */
const TOAST_CLASS = { info: 'info', success: 'success', warn: 'warn', error: 'error' };

/** The async components by module URL: a tab shown again does not load its module again. */
const loaded = new Map();

/** The card of `row` by the profile's template for `label` (`options` already checked). */
function renderCard(label, row, options) {
  return h(ProfileCard, {
    label,
    row,
    template: Object.prototype.hasOwnProperty.call(state.ui.profile.cards, label) ? state.ui.profile.cards[label] : null,
    options,
  });
}

/** The profile API of one tab (wired to the store, the router and Vue). */
export function profileApiFor() {
  return createProfileApi({
    profile: state.ui.profile,
    engineInfo: state.engineInfo,
    server: isServerMode(),
    runQuery: profileQuery,
    renderMarkdown: (text) => h(MarkdownView, { text }),
    renderCard,
    // The graph view (components/profile-graph.js); its side card goes through the same card path.
    renderGraph: (data, options) => h(ProfileGraph, {
      data,
      options,
      spec: state.ui.profile.graph || {},
      renderCard: (label, row, cardOptions) => renderCard(String(label), row, checkCardOptions(cardOptions)),
    }),
    showToast: (text, kind) => toast(text, TOAST_CLASS[kind], kind === 'error' || kind === 'warn' ? 9000 : 5000),
    navigate: navigateTab,
    themeState: () => ({ mode: state.theme, dark: isDark() }),
    cssToken: (name) => cssVar(`--${name}`),
  });
}

/** The error card of a tab whose module failed. */
export const ProfileTabError = {
  props: { title: { type: String, default: '' }, error: { default: null }, url: { type: String, default: '' } },
  template: `
  <div class="profile-tab-error" role="alert">
    <p class="profile-tab-error-head"><span aria-hidden="true">✗</span> The tab "{{ title }}" could not be shown.</p>
    <p class="profile-tab-error-msg">{{ error && error.message ? error.message : String(error) }}</p>
    <p class="muted">Module: <code>{{ url }}</code>. The other tabs keep working; reload the page after fixing the module.</p>
  </div>
  `,
};

/** The default export of the profile module at `url` (a Vue component), or a rejection naming why not. */
export function importComponent(url) {
  return import(url).then((m) => {
    const comp = m && m.default;
    if (!comp || (typeof comp !== 'object' && typeof comp !== 'function')) {
      throw new Error('the module has no default export (a Vue component)');
    }
    return comp;
  });
}

function asyncTab(url, title) {
  if (!loaded.has(url)) {
    loaded.set(url, markRaw(defineAsyncComponent({
      loader: () => importComponent(url),
      loadingComponent: { template: '<p class="muted profile-tab-loading">Loading…</p>' },
      delay: 150,
      onError(error, retry, fail) {
        loaded.delete(url); // a reload of the tab tries again
        fail();
      },
      errorComponent: {
        props: { error: { default: null } },
        setup(props) {
          return () => h(ProfileTabError, { error: props.error, url, title });
        },
      },
    })));
  }
  return loaded.get(url);
}

export const ProfileTab = {
  props: {
    tab: { type: Object, required: true },
    params: { type: Object, default: () => ({}) },
  },
  setup(props) {
    const url = state.ui.moduleUrl(props.tab, document.baseURI);
    const comp = shallowRef(asyncTab(url, props.tab.title));
    const api = profileApiFor();
    const failure = ref(null);
    // A module that throws while rendering or in a hook: the error card instead of a blank tab.
    onErrorCaptured((err) => {
      failure.value = err;
      return false;
    });
    watch(() => props.tab, () => { failure.value = null; });
    return () => h('section', { class: 'profile-tab', 'data-profile-tab': props.tab.id, 'aria-label': props.tab.title },
      failure.value
        ? [h(ProfileTabError, { title: props.tab.title, error: failure.value, url })]
        : [h(comp.value, { api, params: props.params })]);
  },
};
