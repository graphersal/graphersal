// The top-bar module of a UI profile (js/ui-profiles.js `header: {module}`): loads the
// profile's ES module like a module tab (components/profile-tab.js `importComponent`) and
// renders its default export in the top bar, right of the spacer and left of the bar's buttons,
// with the prop `api` (js/profile-api.js, the same API a module tab gets; no `params`). It is
// shown on every tab: the place for a choice that affects all of them. A module that cannot be
// loaded, has no default export or throws while rendering shows a small error marker in the bar
// (its tooltip names the module and the error); the rest of the page keeps working.

import { defineAsyncComponent, h, ref, onErrorCaptured, markRaw } from 'vue';
import { state } from '../store.js';
import { importComponent, profileApiFor } from './profile-tab.js';

/** The text of an error (an Error's message, else the value as text). */
const errorText = (error) => (error && error.message ? error.message : String(error));

/** The compact error marker of a header module that failed. */
export const ProfileHeaderError = {
  props: { error: { default: null }, url: { type: String, default: '' } },
  setup(props) {
    return () => h('span', {
      class: 'profile-header-error',
      role: 'alert',
      title: `The top-bar module ${props.url} could not be shown: ${errorText(props.error)}. The tabs keep working; reload the page after fixing the module.`,
    }, [h('span', { 'aria-hidden': 'true' }, '✗ '), 'Header module failed']);
  },
};

export const ProfileHeader = {
  props: { header: { type: Object, required: true } },
  setup(props) {
    const url = state.ui.moduleUrl(props.header, document.baseURI);
    const comp = markRaw(defineAsyncComponent({
      loader: () => importComponent(url),
      errorComponent: {
        props: { error: { default: null } },
        setup(p) {
          return () => h(ProfileHeaderError, { error: p.error, url });
        },
      },
    }));
    const api = profileApiFor();
    const failure = ref(null);
    // A module that throws while rendering or in a hook: the marker instead of a broken bar.
    onErrorCaptured((err) => {
      failure.value = err;
      return false;
    });
    return () => h('div', { class: 'profile-header', 'data-profile-header': '' },
      failure.value ? [h(ProfileHeaderError, { error: failure.value, url })] : [h(comp, { api })]);
  },
};
