import { Show, createSignal } from "solid-js";
import { A } from "@solidjs/router";

import { state } from "../state/store";

// The persistent status bar. It names the store link (to the edge on the LAN, not the cloud — a
// store is meant to trade with the cloud unreachable), the open shift, and a theme toggle. Nothing
// here ever moves between states; only its text and colour change.
export function StatusBar() {
  const linkLabel = () => {
    switch (state.link) {
      case "open":
        return "Store connected";
      case "connecting":
        return "Connecting…";
      default:
        return "Reconnecting…";
    }
  };
  const linkColour = () => (state.link === "open" ? "bg-ok" : "bg-awaiting");

  const initial = document.documentElement.dataset["theme"] ?? "system";
  const [theme, setTheme] = createSignal(initial);
  const cycleTheme = () => {
    const next = theme() === "dark" ? "light" : "dark";
    document.documentElement.dataset["theme"] = next;
    setTheme(next);
  };

  return (
    <header class="flex items-center gap-3 border-b border-line bg-surface px-4 py-2 text-sm">
      <A href="/" class="font-semibold no-underline text-ink">
        Pizza 4P's
      </A>
      <span class="inline-flex items-center gap-2 text-ink-muted">
        <span class={`inline-block h-2.5 w-2.5 rounded-full ${linkColour()}`} aria-hidden="true" />
        {linkLabel()}
      </span>
      <Show when={state.shift} fallback={<span class="text-ink-muted">No shift open</span>}>
        {(shift) => <span class="text-ink-muted">Shift {shift().state.replace("SHIFT_STATE_", "").toLowerCase()}</span>}
      </Show>
      <button
        type="button"
        class="ml-auto rounded-token border border-line px-3 py-1 text-ink"
        onClick={cycleTheme}
      >
        {theme() === "dark" ? "Light" : "Dark"}
      </button>
    </header>
  );
}
