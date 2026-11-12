// The `demo` profile's own tab (js/ui-profiles.js): a module tab as a profile brings it, using
// only the profile API (js/profile-api.js): a saved query through `api.query`, Markdown through
// `api.markdown`, a card per row through `api.card`, a toast and a navigation. It lists the
// people of the modern sample through the saved query `demo_people`; when the graph has none,
// it shows the script that defines it (run it in the Playground tab: a profile tab has no way
// to run a script of its own, only saved queries).

import { ref, h, onMounted, watch } from 'vue';

/** The script that defines the saved query this tab calls. */
export const DEFINE_SCRIPT = 'g.define_query(#{name: "demo_people", folder: "demo", params: #{min_age: #{schema: #{type: "integer"}, default: 0}}, body: `fn demo_people(min_age) { g.v().has_label("person").has("age", P.gte(min_age)).value_map() }`})';

const INTRO = `**Demo profile.** This tab is an ES module of the profile (\`js/profiles/demo/hello.js\`):
it calls the saved query \`demo_people\` through the profile API and shows each row as a card.
The *Playground* tab is the built-in workspace.`;

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const rows = ref([]);
    const error = ref(null);
    const busy = ref(false);
    async function load() {
      busy.value = true;
      error.value = null;
      try {
        rows.value = await props.api.query('demo_people', { min_age: Number(props.params.min || 0) });
      } catch (err) {
        error.value = err;
      } finally {
        busy.value = false;
      }
    }
    onMounted(load);
    // `params` follow the address: a navigate to this tab with other parameters loads again.
    watch(() => props.params.min, load);
    return () => h('div', { class: 'demo-hello' }, [
      h('h2', null, `Hello from the ${props.api.info.profile.title}`),
      props.api.markdown(INTRO),
      h('p', { class: 'demo-actions' }, [
        h('button', { class: 'btn', type: 'button', onClick: load, disabled: busy.value }, busy.value ? 'Loading…' : 'Reload'),
        ' ',
        h('button', { class: 'btn', type: 'button', onClick: () => props.api.navigate('hello', { min: '30' }) }, 'Only 30 and older'),
        ' ',
        h('button', { class: 'btn', type: 'button', onClick: () => props.api.toast('success', `${rows.value.length} people`) }, 'Count'),
        ' ',
        h('button', { class: 'btn', type: 'button', onClick: () => props.api.navigate('play') }, 'Open the playground'),
      ]),
      error.value
        ? h('div', { class: 'demo-error', role: 'alert' }, [
          h('p', null, error.value.message),
          error.value.help ? h('p', { class: 'muted' }, error.value.help) : null,
          h('p', { class: 'muted' }, 'Define it in the Playground tab (on the modern sample), then Reload:'),
          h('pre', { class: 'code-view' }, DEFINE_SCRIPT),
        ])
        : h('div', { class: 'demo-cards' }, rows.value.map((row, i) => h('div', { key: i }, [props.api.card('person', row)]))),
    ]);
  },
};
