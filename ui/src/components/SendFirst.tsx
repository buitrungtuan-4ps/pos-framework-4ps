// "Send N items to the kitchen first?" — asked in place, where Take payment or the counter's
// charge was tapped, when the order still has lines the kitchen has not been sent (pilot U1).
//
// In place rather than as a modal, as every permission-gated act on the till is: `docs/ui-ux.md`
// §1 allows nothing that covers the screen. Two answers. **Send and pay** sends the unsent lines in
// one transaction, as the Send button does, and then goes on to payment; **Cancel** leaves the order
// as it is. There is no "pay without sending": food a guest paid for and the kitchen never heard of
// is the failure this exists to stop. A person who may not send (`sales.line.fire`) is told to ask
// someone who can, and the payment waits.

import { createSignal, Show } from "solid-js";

import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { can } from "../state/permissions";
import type { SendFirstGate } from "../state/unsent";

export function SendFirst(props: { readonly gate: SendFirstGate }) {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const sendAndPay = async () => {
    setBusy(true);
    setError(null);
    try {
      await props.gate.sendAndPay();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={props.gate.asking()}>
      {(key) => (
        <div
          class="mt-3 rounded-token border border-line bg-surface p-3"
          role="alertdialog"
          aria-labelledby="send-first-title"
          data-outcome="send-first"
        >
          <h2 id="send-first-title" class="font-semibold">
            {t("order.send_first_title", { count: props.gate.unsent(key()) })}
          </h2>
          <p class="mt-1 text-sm text-ink-muted">
            {can("sales.line.fire") ? t("order.send_first_hint") : t("order.send_first_no_fire")}
          </p>
          <Show when={error()}>
            {(message) => (
              <p class="mt-2 text-sm text-danger" role="alert">
                {message()}
              </p>
            )}
          </Show>
          <div class="mt-3 grid grid-cols-2 gap-2">
            <button
              type="button"
              class="min-h-money rounded-token bg-primary px-4 text-lg font-semibold text-primary-ink disabled:opacity-50"
              disabled={busy() || !can("sales.line.fire")}
              data-step="sendAndPay"
              onClick={() => void sendAndPay()}
            >
              {t("order.send_and_pay")}
            </button>
            <button
              type="button"
              class="min-h-money rounded-token border border-line px-4 text-lg"
              disabled={busy()}
              onClick={() => props.gate.cancel()}
            >
              {t("common.cancel")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
