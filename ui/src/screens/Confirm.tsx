import { For, Show, createSignal, onMount } from "solid-js";

import { ApiError, api } from "../api/client";
import type { RejectReason, WaitingOrder } from "../api/types";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { formatMoney } from "../lib/money";

// The staff-confirmation queue: the guest orders waiting, and the two ways one leaves this screen
// (ADR-0116).
//
// ADR-0012 calls staff confirmation the protection on a printed QR code — a code is world-readable
// and never expires, so somebody walking past can submit to a table they are not sitting at. The
// hold that stops them was reported to the cloud and written to the store's ledger, and no screen
// ever showed it: there was nothing for a member of staff to press, and nothing on the fire path
// read it either. This screen and the fire gate are the two halves of making it real.
//
// The table is the heading of each card, because that is what a server walks to. The order's ULID
// is never shown — an operator cannot read one, and the card is what turns the id into something
// tappable.
export function Confirm() {
  const [orders, setOrders] = createSignal<WaitingOrder[] | null>(null);
  const [reasons, setReasons] = createSignal<RejectReason[]>([]);
  const [refusing, setRefusing] = createSignal<WaitingOrder | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const explain = (caught: unknown) =>
    setError(caught instanceof ApiError ? caught.message : t("common.store_error"));

  const refresh = () =>
    api
      .awaitingConfirmation()
      .then((waiting) => {
        setOrders(waiting.orders);
        setReasons(waiting.reject_reasons);
        setError(null);
      })
      .catch((caught: unknown) => {
        setOrders([]);
        explain(caught);
      });

  onMount(() => void refresh());

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
    <section class="stack">
      <PageHeader title={t("confirm.title")} />
      <p class="hint">{t("confirm.subtitle")}</p>

      <Show when={error()}>
        {(message) => (
          <p class="notice notice-warn" role="status">
            {message()}
          </p>
        )}
      </Show>

      <Show
        when={(orders() ?? []).length > 0}
        fallback={
          <Show when={orders() !== null}>
            <p class="empty">{t("confirm.empty")}</p>
          </Show>
        }
      >
        <ul class="card-grid">
          <For each={orders() ?? []}>
            {(order) => (
              <li class="card stack">
                <h3 class="card-title">
                  {t("confirm.table", { table: order.table_id })}
                </h3>
                <ul class="line-list">
                  <For each={order.items}>
                    {(line) => (
                      <li>
                        <span>{line.display_name}</span>
                        <span>{`×${String(line.quantity.milli / 1000)}`}</span>
                        <span>{formatMoney(line.line_total)}</span>
                      </li>
                    )}
                  </For>
                </ul>
                <p class="card-total">{formatMoney(order.total)}</p>
                <div class="row">
                  <button
                    type="button"
                    class="btn btn-primary"
                    disabled={busy()}
                    onClick={() => confirm(order)}
                  >
                    {t("confirm.accept")}
                  </button>
                  <button
                    type="button"
                    class="btn"
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
          <div class="panel stack">
            <h3 class="card-title">{t("confirm.reason_title")}</h3>
            <ul class="line-list">
              <For each={reasons()}>
                {(reason) => (
                  <li>
                    <button
                      type="button"
                      class="btn"
                      disabled={busy()}
                      onClick={() => reject(order(), reason)}
                    >
                      {reason.display_name}
                    </button>
                  </li>
                )}
              </For>
            </ul>
            <button type="button" class="btn" onClick={() => setRefusing(null)}>
              {t("common.cancel")}
            </button>
          </div>
        )}
      </Show>
    </section>
  );
}
