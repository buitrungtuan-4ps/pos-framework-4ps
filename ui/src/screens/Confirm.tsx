import { For, Show, createSignal, onCleanup, onMount } from "solid-js";

import { api } from "../api/client";
import type { RejectReason, WaitingOrder } from "../api/types";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { formatAmount, tableLabel } from "../state/store";
import { errorMessage } from "../lib/errors";

// The staff-confirmation queue: the guest orders waiting, and the two ways one leaves this screen
// (ADR-0116).
//
// ADR-0012 calls staff confirmation the protection on a printed QR code — a code is world-readable
// and never expires, so somebody walking past can submit to a table they are not sitting at. The
// hold that stops them was reported to the cloud and written to the store's ledger, and no screen
// ever showed it: there was nothing for a member of staff to press, and nothing on the fire path
// read it either. This screen and the fire gate are the two halves of making it real.
//
// The table is the heading of each card, because that is what a server walks to — by the label the
// floor shows, not the table's id, which read as a ULID on the card until the till review. The
// order's ULID is never shown either: an operator cannot read one.
//
// # It refreshes itself
//
// A guest's order arrives from the cloud, not from this till, so nothing on the screen would
// otherwise say a new one is waiting: the queue loaded once, when the screen opened, and an order
// placed a minute later sat unseen until somebody navigated away and back. It now reloads every
// `REFRESH_MS` while it is open, which is well inside how long a guest waits before asking a server.
const REFRESH_MS = 15_000;

export function Confirm() {
  const [orders, setOrders] = createSignal<WaitingOrder[] | null>(null);
  const [reasons, setReasons] = createSignal<RejectReason[]>([]);
  const [refusing, setRefusing] = createSignal<WaitingOrder | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const explain = (caught: unknown) =>
    setError(errorMessage(caught));

  const refresh = () =>
    api
      .awaitingConfirmation()
      .then((waiting) => {
        setOrders(waiting.orders);
        setReasons(waiting.reject_reasons);
        setError(null);
      })
      .catch((caught: unknown) => {
        // A failed reload keeps what is on screen: a queue that blanked on one dropped request every
        // fifteen seconds would be worse than one that is briefly stale. Only a first load that
        // fails shows the empty state.
        setOrders((current) => current ?? []);
        explain(caught);
      });

  onMount(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), REFRESH_MS);
    onCleanup(() => clearInterval(timer));
  });

  const confirm = (order: WaitingOrder) => {
    setBusy(true);
    void api
      .confirmOrder(order.order_id)
      .then(() => refresh())
      .catch(explain)
      .finally(() => setBusy(false));
  };

  const reject = (order: WaitingOrder, reason: RejectReason) => {
    setBusy(true);
    void api
      .rejectOrder(order.order_id, reason.reason_code_id)
      .then(() => {
        setRefusing(null);
        return refresh();
      })
      .catch(explain)
      .finally(() => setBusy(false));
  };

  return (
    <section class="mx-auto max-w-xl p-4">
      <PageHeader title={t("confirm.title")} />
      <p class="text-ink-muted">{t("confirm.subtitle")}</p>

      <Show when={error()}>
        {(message) => (
          <p class="mt-3 rounded-token border border-awaiting px-3 py-2 text-ink" role="status">
            {message()}
          </p>
        )}
      </Show>

      <Show
        when={(orders() ?? []).length > 0}
        fallback={
          <Show when={orders() !== null}>
            <p class="mt-6 text-ink-muted">{t("confirm.empty")}</p>
          </Show>
        }
      >
        <ul class="mt-4 flex list-none flex-col gap-3 p-0">
          <For each={orders() ?? []}>
            {(order) => (
              <li class="rounded-token border border-line bg-surface p-4" data-outcome="guest-order">
                <h3 class="text-lg font-semibold">
                  {t("confirm.table", { table: tableLabel(order.table_id) })}
                </h3>
                <ul class="mt-2 flex list-none flex-col gap-1 p-0 text-sm">
                  <For each={order.items}>
                    {(line) => (
                      <li class="flex items-baseline justify-between gap-3">
                        <span>{line.display_name}</span>
                        <span class="flex gap-3 tabular-nums text-ink-muted">
                          <span>{`×${String(line.quantity.milli / 1000)}`}</span>
                          <span>{formatAmount(line.line_total)}</span>
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
                <p class="mt-2 text-right text-lg font-semibold tabular-nums">
                  {formatAmount(order.total)}
                </p>
                <div class="mt-3 grid grid-cols-2 gap-2">
                  <button
                    type="button"
                    class="min-h-touch rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => confirm(order)}
                  >
                    {t("confirm.accept")}
                  </button>
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line bg-surface disabled:opacity-50"
                    disabled={busy() || reasons().length === 0}
                    onClick={() => setRefusing(order)}
                  >
                    {t("confirm.refuse")}
                  </button>
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>

      {/* The reason picker. A refusal must cite a reason from the store's managed list (ADR-0115),
          so the button that opens this is disabled when the list is empty rather than opening a
          dialog with nothing to choose. */}
      <Show when={refusing()}>
        {(order) => (
          <div class="mt-4 rounded-token border border-line bg-surface p-4">
            <h3 class="font-semibold">{t("confirm.reason_title")}</h3>
            <div class="mt-2 flex flex-col gap-2">
              <For each={reasons()}>
                {(reason) => (
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => reject(order(), reason)}
                  >
                    {reason.display_name}
                  </button>
                )}
              </For>
            </div>
            <button
              type="button"
              class="mt-2 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted"
              onClick={() => setRefusing(null)}
            >
              {t("common.cancel")}
            </button>
          </div>
        )}
      </Show>
    </section>
  );
}
