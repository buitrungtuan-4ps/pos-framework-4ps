import { onCleanup, onMount } from "solid-js";

// The kitchen and expo displays run dark by default (docs/roadmap.md P6): legible at two metres, on
// a dedicated panel. Rather than a second theme system, such a screen takes over the root theme
// while it is mounted and restores whatever was there on the way out, so a POS terminal that happens
// to open the KDS route does not keep the dark theme afterward.
export function useDarkTakeover(): void {
  let previous: string | undefined;
  onMount(() => {
    previous = document.documentElement.dataset["theme"];
    document.documentElement.dataset["theme"] = "dark";
  });
  onCleanup(() => {
    if (previous === undefined) {
      delete document.documentElement.dataset["theme"];
    } else {
      document.documentElement.dataset["theme"] = previous;
    }
  });
}
