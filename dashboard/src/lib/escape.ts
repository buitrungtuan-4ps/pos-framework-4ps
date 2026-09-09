// Escape closes what is open (Wave 3 · Stage 6).
//
// This was a private helper inside `kit.tsx`, where the modal and the drawer could see it and
// nothing else could. So the three dropdowns in the header — the org switcher, the notification
// bell, and now the account menu — had no way out but clicking the same button again, which for a
// keyboard user means tabbing back through whatever the panel contains. The same shape of problem as
// `lib/errors.ts`: a helper visible to five files, so everyone else went without.
//
// It lives in `lib/` rather than being exported from `kit.tsx` for a mundane reason with a real
// cost. `kit.tsx` is a lazily-loaded chunk; `Toast.tsx` is in the shell bundle every visit loads.
// Importing the CRUD kit to reach five lines would move ten kilobytes into the first paint.

import { onCleanup, onMount } from "solid-js";

/**
 * Closes `isOpen` on Escape, for as long as the component is mounted.
 *
 * A window listener rather than a handler on the panel, because focus may be anywhere: on the
 * trigger, inside the panel, or — after a click that moved it — on the document body. A panel a
 * keyboard user cannot dismiss from where they are standing is not dismissible.
 */
export function useEscape(isOpen: () => boolean, close: () => void): void {
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape" && isOpen()) {
      close();
    }
  };
  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));
}
