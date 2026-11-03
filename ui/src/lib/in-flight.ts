import { createSignal, type Accessor } from "solid-js";

import { errorMessage } from "./errors";

/** One act at a time on a button that writes, and which one is out (`docs/ui-ux.md` §4). */
export interface InFlight<K> {
  /** The act whose answer is awaited, or null when none is. */
  busy: Accessor<K | null>;
  /** Runs `act` as `key`, unless an act is already out, when the tap is ignored. */
  run: (key: K, act: () => Promise<unknown>) => Promise<void>;
}

// A tap that writes waits for the edge's answer, and on a store's network that can be a second or
// more. Until now the button looked untouched for all of it, so a cashier tapped again, and a second
// settle, send or drawer act went out behind the first. The key is set before anything is awaited,
// so the button says it is working in the frame the tap lands in, and a second tap in that window
// finds it set and does nothing. `report` is the screen's error line: cleared on the tap, and given
// the refusal's sentence if the act fails.
export function useInFlight<K>(report: (message: string | null) => void): InFlight<K> {
  const [busy, setBusy] = createSignal<K | null>(null);
  const run = async (key: K, act: () => Promise<unknown>) => {
    if (busy() !== null) {
      return;
    }
    setBusy(() => key);
    report(null);
    try {
      await act();
    } catch (caught) {
      report(errorMessage(caught));
    } finally {
      setBusy(null);
    }
  };
  return { busy, run };
}
