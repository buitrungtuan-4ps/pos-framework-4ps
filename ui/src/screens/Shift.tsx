import { Show, createSignal } from "solid-js";

import { ApiError } from "../api/client";
import { formatMoney, money, parseWhole } from "../lib/money";
import { closeShift, countShift, openShift, state } from "../state/store";

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
      setError(caught instanceof ApiError ? caught.message : "The store did not respond.");
    }
  };

  const parsed = () => parseWhole(amount(), "VND");

  return (
    <section class="mx-auto max-w-md p-4">
      <h1 class="mb-4 text-lg font-semibold">Cash shift</h1>

      <Show when={error()}>
        {(message) => (
          <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <Show when={phase() === "NONE" || phase() === "SHIFT_STATE_CLOSED"}>
        <label class="block text-sm text-ink-muted" for="float">
          Opening float (đồng)
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
          onClick={() => {
            const value = parsed();
            if (value !== null) {
              void run(() => openShift(value));
            }
          }}
        >
          Open shift
        </button>
      </Show>

      <Show when={phase() === "SHIFT_STATE_OPEN"}>
        <p class="text-ink-muted">Shift open. Enter the counted cash to close it — you will see the expected amount only after.</p>
        <label class="mt-3 block text-sm text-ink-muted" for="count">
          Counted cash (đồng)
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
          onClick={() => {
            const value = parsed();
            const current = shift();
            if (value !== null && current) {
              void run(() => countShift(current.shiftId, value));
            }
          }}
        >
          Enter count
        </button>
      </Show>

      <Show when={phase() === "SHIFT_STATE_COUNTED"}>
        <p class="text-ink-muted">Counted. Close the shift to reveal the variance.</p>
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink"
          onClick={() => {
            const current = shift();
            if (current) {
              void run(() => closeShift(current.shiftId));
            }
          }}
        >
          Close &amp; reveal
        </button>
      </Show>

      <Show when={phase() === "SHIFT_STATE_CLOSED" && shift()?.variance}>
        {(variance) => (
          <div class="mt-4 rounded-token border border-line bg-surface p-4">
            <p class="font-semibold">Shift closed</p>
            <p class="mt-2 text-ink-muted">
              Expected <span class="tabular-nums">{formatMoney(shift()?.expected ?? money("VND", 0))}</span>
            </p>
            <p class="text-ink-muted">
              Counted <span class="tabular-nums">{formatMoney(shift()?.counted ?? money("VND", 0))}</span>
            </p>
            <p classList={{ "text-danger": variance().amount_minor !== 0, "text-ok": variance().amount_minor === 0 }}>
              Variance <span class="tabular-nums">{formatMoney(variance())}</span>
            </p>
          </div>
        )}
      </Show>
    </section>
  );
}
