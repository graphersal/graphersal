// Dropdown menu used by every popup of the playground (View, Examples, History, Export).
//
// The menu is teleported to <body> and positioned with `position: fixed` from the trigger's
// bounding rectangle, so no panel's overflow or stacking context can clip or cover it. It opens
// below the trigger, aligned to its start or end edge, flips upward when there is no room below,
// and is shifted (and if needed height-limited) to stay inside the viewport. It closes on an
// outside click, Esc, a click on a `button.menu-item` (unless it has `data-keep-open`), and when its trigger disappears (a panel
// collapsed, maximised elsewhere or hidden); after other layout changes it follows its trigger.

import { ref, nextTick, onBeforeUnmount } from 'vue';

const MARGIN = 8;
let openMenu = null; // only one menu is open at a time

/** Dispatched by the workspace whenever the panel layout changes. */
export const LAYOUT_EVENT = 'playground:layout';

/** Closes the open dropdown menu, if any (the Examples panel opens on hover). */
export function closeOpenMenu() {
  if (openMenu) openMenu();
}

export const DropMenu = {
  props: {
    /** Classes of the trigger button. */
    buttonClass: { type: String, default: 'btn small' },
    title: { type: String, default: '' },
    ariaLabel: { type: String, default: '' },
    /** 'end': the menu's right edge follows the trigger's right edge; 'start': left edges. */
    align: { type: String, default: 'end' },
    menuStyle: { type: [String, Object], default: '' },
    /** Sets `aria-busy` on the trigger (the Store button while a store action runs). */
    busy: { type: Boolean, default: false },
  },
  setup(props) {
    const open = ref(false);
    const trigger = ref(null);
    const pop = ref(null);
    const pos = ref({ top: '0px', left: '0px', visibility: 'hidden' });
    let observer = null;

    function place() {
      const t = trigger.value;
      const p = pop.value;
      if (!t || !p) return;
      const r = t.getBoundingClientRect();
      if (!t.isConnected || (!r.width && !r.height)) {
        close(); // the trigger was hidden (collapsed, maximised elsewhere, removed)
        return;
      }
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      // Measuring clears the height limit, which resets the menu's own scroll position: keep it.
      const scrollTop = p.scrollTop;
      p.style.maxHeight = '';
      const w = p.offsetWidth;
      let h = p.offsetHeight;
      let left = props.align === 'start' ? r.left : r.right - w;
      left = Math.min(Math.max(MARGIN, left), Math.max(MARGIN, vw - w - MARGIN));
      let top = r.bottom + 4;
      let maxHeight = null;
      if (top + h > vh - MARGIN) {
        const above = r.top - 4 - h;
        if (above >= MARGIN) {
          top = above; // flip upward
        } else {
          // Neither side fits: use the larger side and scroll inside the menu.
          const below = vh - MARGIN - (r.bottom + 4);
          const room = r.top - 4 - MARGIN;
          if (room > below) {
            maxHeight = room;
            top = MARGIN;
          } else {
            maxHeight = below;
          }
          h = maxHeight;
        }
      }
      pos.value = {
        top: `${Math.round(top)}px`,
        left: `${Math.round(left)}px`,
        visibility: 'visible',
        ...(maxHeight ? { maxHeight: `${Math.floor(maxHeight)}px`, overflowY: 'auto' } : {}),
      };
      nextTick(() => {
        if (pop.value) pop.value.scrollTop = scrollTop;
      });
    }

    function onPointerDown(e) {
      if (pop.value && pop.value.contains(e.target)) return;
      if (trigger.value && trigger.value.contains(e.target)) return;
      close();
    }
    function onKey(e) {
      if (e.key === 'Escape') {
        e.preventDefault(); // tells other Esc handlers (restore from maximise) it was used
        close();
        if (trigger.value) trigger.value.focus();
      }
    }
    const onReflow = () => place();
    // Scrolling the page or a panel moves the trigger; scrolling inside the menu itself does not.
    const onScroll = (e) => {
      if (pop.value && e.target instanceof Node && pop.value.contains(e.target)) return;
      place();
    };
    // After a layout change: follow the trigger, or close if it was hidden (collapse, maximise, hide).
    const onLayout = () => requestAnimationFrame(place);

    function listen(on) {
      const m = on ? 'addEventListener' : 'removeEventListener';
      document[m]('pointerdown', onPointerDown, true);
      window[m]('keydown', onKey, true);
      window[m]('resize', onReflow);
      window[m]('scroll', onScroll, true);
      window[m](LAYOUT_EVENT, onLayout);
    }

    async function show() {
      if (openMenu && openMenu !== close) openMenu();
      openMenu = close;
      pos.value = { top: '0px', left: '0px', visibility: 'hidden' };
      open.value = true;
      listen(true);
      observer = new ResizeObserver(() => place());
      if (trigger.value) observer.observe(trigger.value);
      await nextTick();
      place();
      const first = pop.value && pop.value.querySelector('button, input, [tabindex]');
      if (first) first.focus({ preventScroll: true });
    }

    function close() {
      if (!open.value) return;
      open.value = false;
      listen(false);
      if (observer) observer.disconnect();
      observer = null;
      if (openMenu === close) openMenu = null;
    }

    function toggle() {
      if (open.value) close();
      else show();
    }

    function onMenuClick(e) {
      const item = e.target.closest && e.target.closest('button.menu-item');
      if (item && !item.disabled && !item.hasAttribute('data-keep-open')) close();
    }

    onBeforeUnmount(close);

    return { open, trigger, pop, pos, toggle, show, close, onMenuClick };
  },
  template: `
    <button ref="trigger" type="button" :class="buttonClass" :title="title" :aria-label="ariaLabel || null"
            aria-haspopup="menu" :aria-expanded="String(open)" :aria-busy="busy ? 'true' : null" @click="toggle">
      <slot name="trigger"></slot>
    </button>
    <teleport to="body">
      <div v-if="open" ref="pop" class="menu-list popover" role="menu" :style="[pos, menuStyle]" @click="onMenuClick">
        <slot :close="close"></slot>
      </div>
    </teleport>
  `,
};
