// The operator's theme choice (Wave 3 · Stage 6).
//
// # Why this exists
//
// `styles/tokens.css` has honoured `data-theme="light"` and `data-theme="dark"` on the root element
// since P6, and until now nothing in the console ever set the attribute — so an operator got
// whichever palette their operating system was in and had no way to say otherwise. The till has a
// toggle for exactly this, reading the same token file. Only the console lacked one.
//
// # Why three states and not a toggle
//
// "System" is a real answer, not the absence of one: an operator whose laptop switches at sunset
// wants the console to switch with it. It is also the only correct default, because a console that
// picked light or dark on first run would be overriding a preference the viewer has already
// expressed to their operating system.
//
// That makes the choice three-valued, and it makes "system" the state that *removes* the attribute
// rather than setting a third value — the CSS knows only "light" and "dark", so writing
// `data-theme="system"` would leave a stale explicit choice in place while reading as though it had
// been cleared. The till's toggle has this shape of gap in the other direction: it flips between
// dark and light and can never return to following the system.

import { createSignal } from "solid-js";

/** What the operator has asked for. `system` follows `prefers-color-scheme`. */
export type Theme = "system" | "light" | "dark";

/** Every choice, in the order the switcher offers them: the default first. */
export const THEMES: readonly Theme[] = ["system", "light", "dark"];

const STORAGE_KEY = "pos.dashboard.theme";

function isTheme(value: unknown): value is Theme {
  return value === "system" || value === "light" || value === "dark";
}

/**
 * The choice this browser holds, or `system`.
 *
 * Anything unreadable or unrecognised reads as `system`, which is the default rather than a
 * fallback: a corrupt key must leave the console following the operating system, not stuck in a
 * palette nobody chose. Private windows and blocked site data throw on access, so the read is
 * guarded.
 */
export function loadTheme(): Theme {
  try {
    const stored: unknown = localStorage.getItem(STORAGE_KEY);
    return isTheme(stored) ? stored : "system";
  } catch {
    return "system";
  }
}

/**
 * Puts `theme` on the root element.
 *
 * `system` deletes the attribute. Setting it to the string "system" would be the bug this signature
 * exists to prevent: the stylesheet matches `[data-theme="dark"]` and `:not([data-theme="light"])`,
 * so an unrecognised value silently means "not light", which is not what the operator asked for.
 */
export function applyTheme(theme: Theme, root: HTMLElement): void {
  if (theme === "system") {
    delete root.dataset["theme"];
    return;
  }
  root.dataset["theme"] = theme;
}

/** Records the choice. Persistence is a convenience; a failure to store is not an error. */
export function rememberTheme(theme: Theme): void {
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // The choice holds for this page; only the memory of it is lost.
  }
}

// The live choice, held here for the same reason the locale's is held in `i18n/index.ts`: one
// module owns the preference, its persistence and the DOM it touches, and every consumer reads the
// signal. Applied once at boot from `main.tsx` — before `render`, so an operator whose choice
// differs from their operating system does not see the other palette flash first.
const [theme, setThemeSignal] = createSignal<Theme>(loadTheme());
export { theme };

/** Records the operator's choice, applies it, and remembers it for next time. */
export function setTheme(next: Theme): void {
  setThemeSignal(next);
  applyTheme(next, document.documentElement);
  rememberTheme(next);
}
