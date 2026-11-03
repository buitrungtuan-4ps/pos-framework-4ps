// Surplus cash taken from the drawer to the safe (ADR-0186 decision 5), on the Shift screen beside
// paid in and paid out. **Drop to safe** opens the pile being taken: counted note by note where the
// store counts so, else typed as one amount; then one tap records it, on a reason from the store's
// list for a drop (ADR-0186 amendment) or on the plain button, since a reason is not required.

import { For, Show, createSignal } from "solid-js";

import type { DrawerOutcome } from "../api/schemas";
import { t } from "../i18n";
import { money, type Money } from "../lib/money";
import { EMPTY_COUNT, fieldAmount } from "../state/cash-count";
import { recordSafeDrop } from "../state/safe-drop";
import { formatAmount, reasonsFor, storeCurrency } from "../state/store";
import { CountField } from "./CountField";

// The reasons a drop cites, as `/api/reason-codes` spells their act; never the paid-out list.
const CASH_DROPPED = "REASON_ACTION_CASH_DROPPED";

export function SafeDrop(props: {
  shiftId: string;
  /** The act on the drawer awaiting its answer, if any: every act waits for it. */
  busy: string | null;
  /** Runs the drop as the screen's act on its drawer, one at a time. */
  run: (key: string, act: () => Promise<unknown>) => Promise<void>;
  /** Told what was dropped and what the drawer did. */
  onDropped: (amount: Money, outcome: DrawerOutcome | undefined) => void;
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
  // Keyed by the reason tapped, or `drop` for none, so the button tapped is the one that says so.
  const drop = (reasonCodeId?: string) => {
    const value = dropped();
    if (value === null) {
      return;
    }
    void props.run(reasonCodeId ?? "drop", async () => {
      const outcome = await recordSafeDrop(props.shiftId, value.minor, value.breakdown, reasonCodeId);
      close();
      props.onDropped(money(storeCurrency(), value.minor), outcome);
    });
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
                  disabled={dropped() === null || props.busy !== null}
                  data-step="dropReason"
                  onClick={() => drop(reason.reason_code_id)}
                >
                  {props.busy === reason.reason_code_id ? t("common.sending") : reason.display_name}
                </button>
              )}
            </For>
          </div>
        </Show>
        <div class="mt-3 grid grid-cols-2 gap-2">
          <button
            type="button"
            class={`${button} border-primary`}
            disabled={dropped() === null || props.busy !== null}
            data-step="dropToSafe"
            onClick={() => drop()}
          >
            {props.busy === "drop"
              ? t("common.sending")
              : t("shift.drop_confirm", {
                  amount: formatAmount(money(storeCurrency(), dropped()?.minor ?? 0)),
                })}
          </button>
          <button type="button" class={button} disabled={props.busy !== null} onClick={close}>
            {t("common.cancel")}
          </button>
        </div>
      </div>
    </Show>
  );
}
