// The `demo` profile's own tab (js/ui-profiles.js): a module tab as a profile brings it, using
// only the profile API (js/profile-api.js): a saved query through `api.query`, Markdown through
// `api.markdown`, cards through `api.card` (the profile's `person` template in
// js/ui-profiles.js: a compact card per row, a click opens the full card beside the list, its
// "Say hello" action calls back into this module), a toast and a navigation. It lists the
// people of the modern sample through the saved query `demo_people`; when the graph has none,
// it shows the script that defines it (run it in the Playground tab: a profile tab has no way
// to run a script of its own, only saved queries).

import { ref, h, onMounted, watch } from 'vue';

/** The script that defines the saved query this tab calls. */
export const DEFINE_SCRIPT = 'g.define_query(#{name: "demo_people", folder: "demo", params: #{min_age: #{schema: #{type: "integer"}, default: 0}}, body: `fn demo_people(min_age) { g.v().has_label("person").has("age", P.gte(min_age)).value_map() }`})';

const INTRO = `**Demo profile.** This tab is an ES module of the profile (\`js/profiles/demo/hello.js\`):
it calls the saved query \`demo_people\` through the profile API and shows each row as a card
(the profile's \`person\` template). Click a card to open it.
The *Playground* tab is the built-in workspace.`;

/**
 * A row of `demo_people` (a `value_map()`: every property a one-item list) with two fields of
 * the tab's own for the card template: `senior` (a badge) and `about` (the Markdown body).
 */
export function person(row) {
  const name = Array.isArray(row.name) ? row.name[0] : row.name;
  const age = Array.isArray(row.age) ? row.age[0] : row.age;
  const about = typeof age === 'number'
    ? `**${name}** is ${age} years old. This text is the card's *Markdown body*, rendered by the safe renderer; <b>raw HTML</b> stays text.`
    : `**${name}** has no age.`;
  return { ...row, name, senior: typeof age === 'number' && age >= 30, about };
}

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const rows = ref([]);
    const openName = ref(null);
    const error = ref(null);
    const busy = ref(false);
    async function load() {
      busy.value = true;
      error.value = null;
      try {
        rows.value = (await props.api.query('demo_people', { min_age: Number(props.params.min || 0) })).map(person);
      } catch (err) {
        error.value = err;
      } finally {
        busy.value = false;
      }
    }
    // The card's action: the module decides what a click does.
    function onAction(id, row) {
      if (id === 'hello') props.api.toast('success', `Hello, ${row.name}!`);
    }
    function cards() {
      const opened = rows.value.find((r) => r.name === openName.value) || null;
      return h('div', { class: 'demo-cards' }, [
        h('div', { class: 'demo-cards-list' }, rows.value.map((row) => h('div', { key: row.name }, [
          props.api.card('person', row, {
            variant: 'compact',
            selected: row === opened,
            onOpen: (r) => { openName.value = r.name; },
          }),
        ]))),
        h('div', { class: 'demo-cards-open' }, opened
          ? [props.api.card('person', opened, { onAction })]
          : rows.value.length ? [h('p', { class: 'muted' }, 'Click a card to open it.')] : []),
      ]);
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
        : cards(),
    ]);
  },
};
