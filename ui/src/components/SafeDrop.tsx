// Surplus cash taken from the drawer to the safe (ADR-0186 decision 5), on the Shift screen beside
// paid in and paid out. **Drop to safe** opens the pile being taken: counted note by note where the
// store counts so, else typed as one amount; then one tap records it, on a reason from the store's
// list for a drop (ADR-0186 amendment) or on the plain button, since a reason is not required.

import { For, Show, createSignal, on, createEffect } from "solid-js";

import type { CountBreakdown } from "../api/schemas";
import { t } from "../i18n";
import { money } from "../lib/money";
import { EMPTY_COUNT, fieldAmount } from "../state/cash-count";
import { formatAmount, reasonsFor, storeCurrency } from "../state/store";
import { CountField } from "./CountField";

// The reasons a drop cites, as `/api/reason-codes` spells their act; never the paid-out list.
const CASH_DROPPED = "REASON_ACTION_CASH_DROPPED";

export function SafeDrop(props: {
  /** Whether an act on the drawer awaits its answer: every act waits for it. */
  busy: boolean;
  /** How many drops the edge has committed: the pile closes when one more is. */
  dropped: number;
  /** Sends the drop as the screen's money act (`../state/cash-acts`). */
  onDrop: (minor: number, breakdown: CountBreakdown | undefined, reasonCodeId?: string) => void;
}) {
  const [open, setOpen] = createSignal(false);
  const [text, setText] = createSignal("");
  const [count, setCount] = createSignal(EMPTY_COUNT);
  const dropped = () => {
    const value = fieldAmount(text(), count());
    return value !== null && value.minor > 0 ? value : null;
  };
  const close = () => {
    setOpen(false);
    setText("");
    setCount(EMPTY_COUNT);
  };
  // Kept open while the drop is out, so a refusal leaves the pile as it was typed; closed once the
  // edge commits it.
  createEffect(on(() => props.dropped, close, { defer: true }));
  const drop = (reasonCodeId?: string) => {
    const value = dropped();
    if (value !== null) {
      props.onDrop(value.minor, value.breakdown, reasonCodeId);
    }
  };
  const button = "min-h-touch rounded-token border border-line bg-surface font-semibold disabled:opacity-50";

  return (
    <Show
      when={open()}
      fallback={
        <button type="button" class={`${button} col-span-2`} onClick={() => setOpen(true)}>
          {t("shift.safe_drop")}
        </button>
      }
    >
      <div class="col-span-2 rounded-token border border-line p-3" data-outcome="safe-drop">
        <CountField
          id="drop-amount"
          label={t("shift.drop_amount_label", { currency: storeCurrency() })}
          text={text()}
          onText={setText}
          count={count()}
          onCount={setCount}
          step="dropKeypad"
        />
        <Show when={reasonsFor(CASH_DROPPED).length > 0}>
          <p class="mt-3 text-sm text-ink-muted">{t("shift.drop_reason")}</p>
          <div class="mt-2 grid grid-cols-2 gap-2">
            <For each={reasonsFor(CASH_DROPPED)}>
              {(reason) => (
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                  disabled={dropped() === null || props.busy}
                  data-step="dropReason"
                  onClick={() => drop(reason.reason_code_id)}
                >
                  {reason.display_name}
                </button>
              )}
            </For>
          </div>
        </Show>
        <div class="mt-3 grid grid-cols-2 gap-2">
          <button
            type="button"
            class={`${button} border-primary`}
            disabled={dropped() === null || props.busy}
            data-step="dropToSafe"
            onClick={() => drop()}
          >
            {t("shift.drop_confirm", {
              amount: formatAmount(money(storeCurrency(), dropped()?.minor ?? 0)),
            })}
          </button>
          <button type="button" class={button} disabled={props.busy} onClick={close}>
            {t("common.cancel")}
          </button>
        </div>
      </div>
    </Show>
  );
}
