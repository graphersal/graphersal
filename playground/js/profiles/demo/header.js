// The `demo` profile's top-bar module (js/ui-profiles.js `header`): rendered right of the top
// bar's spacer on every tab, with the same profile API as a module tab. It names the engine
// (`api.info`) and opens the Graph tab (`api.navigate`); the engine text hides on narrow screens.

import { h } from 'vue';

export default {
  props: { api: { type: Object, required: true } },
  setup(props) {
    return () => h('span', { class: 'demo-header' }, [
      h('span', { class: 'muted hide-narrow' }, props.api.info.server ? 'dev server' : 'browser engine'),
      ' ',
      h('button', { type: 'button', class: 'btn small', onClick: () => props.api.navigate('graph') }, 'Graph'),
    ]);
  },
};
