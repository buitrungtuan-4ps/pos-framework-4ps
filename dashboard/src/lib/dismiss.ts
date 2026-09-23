// Pressing somewhere else closes what is open.
//
// The sibling of `lib/escape.ts`, and here for the reason that one gives about itself: this was
// written inside `ComboboxField`, where the picker could see it and nothing else could. So the three
// dropdowns in the header — the org switcher, the notification bell, the account menu — closed only
// by pressing their own button again or by Escape, and a mouse user who opened one and changed their
// mind had to find the button they came from. That is the same shape of problem `useEscape` was
// extracted to fix, one input device over.
//
// `lib/` rather than an export from `ui.tsx` for the same mundane reason `escape.ts` gives: the
// header components are in the shell bundle every visit loads, and reaching into the kit for a
// dozen lines would move it there too.

import { createEffect, onCleanup } from "solid-js";

/**
 * Calls `close` when a pointer goes down outside `container`, for as long as `isOpen` is true.
 *
 * `pointerdown` rather than `click`, so a press that begins outside and ends on the panel does not
 * act through a popover that is already closing — the panel is gone before the release lands. One
 * listener covers mouse, touch and pen.
 *
 * Bound only while open. A document-level listener that outlives the panel it closes is a listener
 * every pointer press in the console pays for, and there are four of these.
 *
 * `container` arrives as a function rather than an element because a caller's `ref` is assigned
 * during render: read eagerly it would be `undefined`, and the panel would never close.
 */
export function useClickOutside(
  isOpen: () => boolean,
  container: () => HTMLElement | undefined,
  close: () => void,
): void {
  createEffect(() => {
    if (!isOpen()) {
      return;
    }
    const onPointerDown = (event: PointerEvent) => {
      const root = container();
      if (root && !root.contains(event.target as Node)) {
        close();
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    onCleanup(() => document.removeEventListener("pointerdown", onPointerDown));
  });
}
