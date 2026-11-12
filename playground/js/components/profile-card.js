// The card of `api.card(label, row, options)` (js/profile-api.js, EXPERIMENTAL): one row shown
// by the profile's card template for its label. What it shows is computed by the pure model
// js/profile-cards.js (`cardModel`); this component only renders it: every value as a text node,
// the Markdown body and `format: "markdown"` fields through the safe renderer
// (components/markdown-view.js), long field values collapsed by components/value-text.js (the
// setting `valueMaxChars`). Colours come from the theme tokens (css/app.css `.profile-card`),
// so light and dark follow by themselves; the card wraps, it never scrolls sideways.

import { h, ref, computed } from 'vue';
import { toast } from '../store.js';
import { cardModel } from '../profile-cards.js';
import { ValueText } from './value-text.js';
import { MarkdownView } from './markdown-view.js';

const BUTTON_CLASS = { default: 'btn small', primary: 'btn small primary', danger: 'btn small danger' };

/** Whether a click started on something of its own (a button, a link, a form field). */
function interactive(event) {
  const target = event && event.target;
  return !!(target && target.closest && target.closest('button, a, input, textarea, select, summary'));
}

export const ProfileCard = {
  props: {
    label: { type: String, default: '' },
    row: { default: null },
    /** The profile's template for `label`, or null (the default card). */
    template: { default: null },
    /** Checked options (js/profile-cards.js `checkCardOptions`). */
    options: { type: Object, default: () => ({}) },
  },
  setup(props) {
    const model = computed(() => cardModel(props.label, props.row, props.template, props.options));
    const busy = ref(null); // the id of the action whose promise is pending

    function open() {
      if (props.options.onOpen) props.options.onOpen(props.row);
    }

    async function act(id) {
      if (busy.value || !props.options.onAction) return;
      try {
        const answer = props.options.onAction(id, props.row);
        if (answer && typeof answer.then === 'function') {
          busy.value = id;
          await answer;
        }
      } catch (err) {
        toast(`${err && err.message ? err.message : String(err)}`, 'error', 9000);
      } finally {
        busy.value = null;
      }
    }

    function fieldValue(f) {
      if (f.format === 'markdown') {
        return h(MarkdownView, { text: typeof f.value === 'string' ? f.value : JSON.stringify(f.value) });
      }
      return h(ValueText, { value: f.value, name: f.title });
    }

    return () => {
      const m = model.value;
      const headingText = m.heading.text;
      const headingClass = ['profile-card-title', m.heading.fromLabel ? 'from-label' : ''];
      const heading = m.openable
        ? h('h3', { class: headingClass }, [h('button', { type: 'button', class: 'profile-card-open', onClick: open }, headingText)])
        : h('h3', { class: headingClass }, headingText);
      const head = h('header', { class: 'profile-card-head' }, [
        heading,
        m.badges.length
          ? h('span', { class: 'profile-card-badges' }, m.badges.map((b, i) => h('span', { key: i, class: ['profile-card-badge', `tone-${b.tone}`] }, b.text)))
          : null,
      ]);
      const children = [head];
      if (m.subtitle) children.push(h('p', { class: 'profile-card-subtitle' }, m.subtitle));
      if (m.excerpt) children.push(h('p', { class: 'profile-card-excerpt' }, m.excerpt));
      if (m.body) children.push(h('div', { class: 'profile-card-body' }, [h(MarkdownView, { text: m.body })]));
      if (m.fields.length) {
        children.push(h('dl', { class: 'profile-card-fields' }, m.fields.flatMap((f) => [
          h('dt', { key: `k:${f.key}` }, f.title),
          h('dd', { key: `v:${f.key}` }, [fieldValue(f)]),
        ])));
      }
      if (m.actions.length) {
        children.push(h('footer', { class: 'profile-card-actions' }, m.actions.map((a) => h('button', {
          key: a.id,
          type: 'button',
          class: BUTTON_CLASS[a.kind] || BUTTON_CLASS.default,
          title: a.title || null,
          'data-action': a.id,
          disabled: a.disabled || busy.value !== null,
          'aria-busy': busy.value === a.id ? 'true' : null,
          onClick: () => act(a.id),
        }, a.text))));
      }
      return h('article', {
        class: ['profile-card', m.variant, { selected: m.selected, openable: m.openable }],
        'data-card-label': m.label,
        'aria-current': m.selected ? 'true' : null,
        onClick: m.openable ? (event) => { if (!interactive(event)) open(); } : null,
      }, children);
    };
  },
};
