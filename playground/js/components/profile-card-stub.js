// The card of `api.card(label, row)` (js/profile-api.js) until the profile cards land: the
// row's label and its fields as a plain list, long values collapsed (components/value-text.js).
// `template` is the profile's card template for the label (`cards`), not read yet.

import { computed } from 'vue';
import { ValueText } from './value-text.js';

/** The fields of a row: an element's `properties` (or the row itself), as [key, value] pairs. */
export function cardFields(row) {
  if (row === null || typeof row !== 'object' || Array.isArray(row)) return [['value', row]];
  const source = row.properties && typeof row.properties === 'object' ? row.properties : row;
  return Object.entries(source).filter(([key]) => key !== 'properties');
}

export const ProfileCardStub = {
  components: { ValueText },
  props: { label: { type: String, default: '' }, row: { default: null }, template: { default: null } },
  setup(props) {
    return { fields: computed(() => cardFields(props.row)) };
  },
  template: `
  <article class="profile-card">
    <header class="profile-card-label">{{ label }}</header>
    <dl class="profile-card-fields">
      <template v-for="[key, value] in fields" :key="key">
        <dt>{{ key }}</dt>
        <dd><value-text :value="typeof value === 'string' ? value : JSON.stringify(value)" :name="key"></value-text></dd>
      </template>
    </dl>
  </article>
  `,
};
