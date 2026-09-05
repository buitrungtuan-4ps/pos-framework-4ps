import { Show, createSignal } from "solid-js";

import { ApiError } from "../api/client";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { formatMoney, money, parseWhole } from "../lib/money";
import { closeShift, countShift, openShift, state, storeCurrency } from "../state/store";

// The cash shift: open with a float, enter the blind count (the screen shows nothing about what is
// expected), then close to reveal the variance. The blindness is the control — counting before the
// expectation is shown (§11.1) — so the count field never sits beside an expected figure.
export function Shift() {
  const [amount, setAmount] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);

  const shift = () => state.shift;
  const phase = () => shift()?.state ?? "NONE";

  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
      setAmount("");
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  // Parsed in the store's own currency, not a literal (roadmap E5): the minor-unit scale differs
  // between currencies, so parsing a typed figure as VND on a two-decimal currency would be out by
  // a factor of a hundred on the store's own cash count.
  const parsed = () => parseWhole(amount(), storeCurrency());

  return (
    <section class="mx-auto max-w-md p-4">
      <PageHeader title={t("shift.title")} />

      <Show when={error()}>
        {(message) => (
          <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <Show when={phase() === "NONE" || phase() === "SHIFT_STATE_CLOSED"}>
        <label class="block text-sm text-ink-muted" for="float">
          {t("shift.float_label")}
        </label>
        <input
          id="float"
          inputmode="numeric"
          class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
          value={amount()}
          onInput={(event) => setAmount(event.currentTarget.value)}
        />
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink disabled:opacity-50"
          disabled={parsed() === null}
          data-step="openShift"
          onClick={() => {
            const value = parsed();
            if (value !== null) {
              void run(() => openShift(value));
            }
          }}
        >
          {t("shift.open")}
        </button>
      </Show>

      <Show when={phase() === "SHIFT_STATE_OPEN"}>
        <p class="text-ink-muted" data-outcome="shift-open">
          {t("shift.open_hint")}
        </p>
        <label class="mt-3 block text-sm text-ink-muted" for="count">
          {t("shift.count_label")}
        </label>
        <input
          id="count"
          inputmode="numeric"
          class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
          value={amount()}
          onInput={(event) => setAmount(event.currentTarget.value)}
        />
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink disabled:opacity-50"
          disabled={parsed() === null}
          data-step="countShift"
          onClick={() => {
            const value = parsed();
            const current = shift();
            if (value !== null && current) {
              void run(() => countShift(current.shiftId, value));
            }
          }}
        >
          {t("shift.enter_count")}
        </button>
      </Show>

      <Show when={phase() === "SHIFT_STATE_COUNTED"}>
        <p class="text-ink-muted" data-outcome="shift-counted">
          {t("shift.counted_hint")}
        </p>
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink"
          data-step="closeShift"
          onClick={() => {
            const current = shift();
            if (current) {
              void run(() => closeShift(current.shiftId));
            }
          }}
        >
          {t("shift.close")}
        </button>
      </Show>

      <Show when={phase() === "SHIFT_STATE_CLOSED" && shift()?.variance}>
        {(variance) => (
          <div class="mt-4 rounded-token border border-line bg-surface p-4" data-outcome="shift-closed">
            <p class="font-semibold">{t("shift.closed")}</p>
            <p class="mt-2 text-ink-muted">
              {t("shift.expected")}{" "}
              <span class="tabular-nums">{formatMoney(shift()?.expected ?? money(storeCurrency(), 0))}</span>
            </p>
            <p class="text-ink-muted">
              {t("shift.counted")}{" "}
              <span class="tabular-nums">{formatMoney(shift()?.counted ?? money(storeCurrency(), 0))}</span>
            </p>
            <p
              classList={{
                "text-danger": variance().amount_minor !== 0,
                "text-ok": variance().amount_minor === 0,
              }}
            >
              {t("shift.variance")} <span class="tabular-nums">{formatMoney(variance())}</span>
            </p>
          </div>
        )}
      </Show>
    </section>
  );
}
