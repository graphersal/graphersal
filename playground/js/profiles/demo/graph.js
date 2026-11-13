// The `demo` profile's graph tab (js/ui-profiles.js): a graph view through the profile API only
// (`api.graph`, `api.mergeGraph`, `api.query`, `api.card`). It draws the people of the modern
// sample and who knows whom (the saved query `demo_graph`); a double click on a person (or the
// side card's Expand button) merges in what that person created (`demo_neighbours`). Styles come
// from the profile's `graph` key (person and software per label, `knows` / `created` edges, a
// legend entry); the module adds one per-element style: people of 30 and older get the `bold`
// class (a thick border), everyone else is faded while a person is expanded. When the graph has
// no such queries, the tab shows the script that defines them (run it in the Playground tab).

import { ref, shallowRef, h, onMounted } from 'vue';

/** The script that defines the two saved queries this tab calls. */
export const DEFINE_SCRIPT = [
  'g.define_query(#{name: "demo_graph", folder: "demo", body: `fn demo_graph() { #{',
  '  nodes: g.v().has_label("person").project("id", "label", "data").by(T.id).by(T.label).by(__.value_map()).to_list(),',
  '  edges: g.e().has_label("knows").project("id", "source", "target", "label", "data").by(T.id).by(__.out_v().id()).by(__.in_v().id()).by(T.label).by(__.value_map()).to_list()',
  '} }`});',
  'g.define_query(#{name: "demo_neighbours", folder: "demo", params: #{id: #{schema: #{type: "integer"}}}, body: `fn demo_neighbours(id) { #{',
  '  nodes: g.v(id).union(__.identity(), __.out("created")).project("id", "label", "data").by(T.id).by(T.label).by(__.value_map()).to_list(),',
  '  edges: g.v(id).out_e("created").project("id", "source", "target", "label", "data").by(T.id).by(__.out_v().id()).by(__.in_v().id()).by(T.label).by(__.value_map()).to_list()',
  '} }`})',
].join('\n');

/** `value_map()` wraps every property in a one-item list: the plain values. */
function plain(map) {
  const out = {};
  for (const [k, v] of Object.entries(map || {})) out[k] = Array.isArray(v) && v.length === 1 ? v[0] : v;
  return out;
}

/**
 * The graph data of a `demo_graph` / `demo_neighbours` answer (one map `{nodes, edges}`):
 * ids as the engine sends them, the name as the caption, the properties as `data`.
 */
export function toGraph(answer) {
  const first = Array.isArray(answer) ? answer[0] : answer;
  const nodes = ((first && first.nodes) || []).map((n) => {
    const data = plain(n.data);
    return { id: n.id, label: n.label, caption: data.name === undefined ? null : String(data.name), data: { ...data, label: n.label } };
  });
  const edges = ((first && first.edges) || []).map((e) => ({
    id: e.id, source: e.source, target: e.target, label: e.label, data: { ...plain(e.data), label: e.label },
  }));
  return { nodes, edges };
}

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const graph = shallowRef({ nodes: [], edges: [] });
    const error = ref(null);
    const busy = ref(false);
    const focus = ref(null); // the person expanded last (others fade)
    const picked = ref(null);

    async function load() {
      busy.value = true;
      error.value = null;
      focus.value = null;
      try {
        graph.value = toGraph(await props.api.query('demo_graph', {}));
      } catch (err) {
        error.value = err;
      } finally {
        busy.value = false;
      }
    }

    async function expand(node) {
      if (node.label !== 'person') {
        props.api.toast('info', `${node.data.name || node.id} has no neighbours to add here.`);
        return;
      }
      try {
        const more = toGraph(await props.api.query('demo_neighbours', { id: Number(node.id) }));
        const { graph: merged, added } = props.api.mergeGraph(graph.value, more);
        graph.value = merged;
        focus.value = node.id;
        props.api.toast(added.nodes.length ? 'success' : 'info', added.nodes.length
          ? `Added ${added.nodes.length} node(s) and ${added.edges.length} edge(s).`
          : `Nothing new around ${node.data.name || node.id}.`);
      } catch (err) {
        props.api.toast('error', err.message);
      }
    }

    // Per-element styles of the module: people of 30 and older get a thick border; while a
    // person is expanded, the people that are neither it nor its neighbours fade.
    function styleOf(element, kind) {
      if (kind !== 'node') return null;
      const classes = [];
      if (element.label === 'person' && typeof element.data.age === 'number' && element.data.age >= 30) classes.push('bold');
      if (focus.value !== null && element.label === 'person' && element.id !== String(focus.value)) {
        const near = graph.value.edges.some((e) => (String(e.source) === element.id && String(e.target) === String(focus.value))
          || (String(e.target) === element.id && String(e.source) === String(focus.value)));
        if (!near) classes.push('faded');
      }
      return classes.length ? { classes } : null;
    }

    onMounted(load);

    return () => h('div', { class: 'demo-graph' }, [
      h('p', { class: 'demo-actions' }, [
        h('button', { class: 'btn', type: 'button', onClick: load, disabled: busy.value }, busy.value ? 'Loading…' : 'Reload'),
        ' ',
        h('span', { class: 'muted' }, picked.value
          ? `Selected: ${picked.value.kind} ${picked.value.data.name || picked.value.label || picked.value.id}`
          : 'Click a person to see its card, double click to add what it created.'),
      ]),
      error.value
        ? h('div', { class: 'demo-error', role: 'alert' }, [
          h('p', null, error.value.message),
          error.value.help ? h('p', { class: 'muted' }, error.value.help) : null,
          h('p', { class: 'muted' }, 'Define them in the Playground tab (on the modern sample), then Reload:'),
          h('pre', { class: 'code-view' }, DEFINE_SCRIPT),
        ])
        : props.api.graph(graph.value, {
          styleOf,
          onSelect: (sel) => { picked.value = sel; },
          onExpand: expand,
          card: {
            actions: (sel) => (sel.kind === 'node' ? [{ id: 'hello', text: 'Say hello' }] : []),
            onAction: (id, sel) => props.api.toast('success', `Hello, ${sel.data.name || sel.id}!`),
          },
          legend: [{ text: '30 and older', node: { border: { width: 4 } } }],
          height: '460px',
        }),
    ]);
  },
};
