// A seated table's missing guest count, given in place (ADR-0254). Where the store's flow asks for
// the count before the first dish and the table was seated without it, the order screen asks here
// rather than telling the cashier to give the table back and seat it again: one tap records the
// count on the table's order, and the dishes the flow held back can be added.

import { createSignal, For, Show } from "solid-js";

import { errorMessage } from "../lib/errors";
import { countGuests } from "../state/floor-writes";

/** The counts the guest pad offers, one tap each. */
export const GUEST_COUNTS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] as const;

export function GuestCountPrompt(props: { tableId: string; sentence: string }) {
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const record = async (count: number) => {
    setBusy(true);
    setRefusal(null);
    try {
      await countGuests(props.tableId, count);
    } catch (caught) {
      setRefusal(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="mb-3 rounded-token border border-line px-3 py-2" role="group" aria-label={props.sentence} data-outcome="guests-asked">
      <p class="mb-2" role="status">{props.sentence}</p>
      <div class="grid grid-cols-6 gap-2">
        <For each={GUEST_COUNTS}>
          {(count) => (
            <button
              type="button"
              class="min-h-touch rounded-token border border-line bg-surface text-lg font-semibold tabular-nums disabled:opacity-50"
              disabled={busy()}
              data-step="countGuests"
              onClick={() => void record(count)}
            >
              {count}
            </button>
          )}
        </For>
      </div>
      <Show when={refusal()}>
        {(message) => (
          <p class="mt-2 text-sm text-danger" role="alert" data-outcome="guests-refused">
            {message()}
          </p>
        )}
      </Show>
    </div>
  );
}
