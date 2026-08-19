import { For, Show, createSignal, onMount } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import type { BillResponse, PaymentRequest } from "../api/types";
import { formatMoney, money } from "../lib/money";
import { billTotalMinor, openBill, openBillFor, settle } from "../state/store";

// The pay screen: the amount owed large, a cash pad with the VND quick-cash denominations and its
// change, or card for the exact amount. On settlement it shows the gapless receipt number and the
// change to hand back; the table is now the floor's to clean.
export function Pay() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [billId, setBillId] = createSignal<string | null>(openBillFor(params.id) ?? null);
  const [error, setError] = createSignal<string | null>(null);
  const [tender, setTender] = createSignal<number | null>(null);
  const [done, setDone] = createSignal<BillResponse | null>(null);

  onMount(() => {
    if (billId() === null) {
      openBill(params.id)
        .then((id) => setBillId(id))
        .catch((caught: unknown) =>
          setError(caught instanceof ApiError ? caught.message : "Could not open the bill."),
        );
    }
  });

  const total = () => billTotalMinor(params.id).total;
  const change = () => {
    const chosen = tender();
    return chosen !== null && chosen >= total() ? chosen - total() : 0;
  };

  // The archive's VND quick-cash: 50k / 100k / 200k, plus the exact amount.
  const quickCash = () => [total(), 50_000, 100_000, 200_000].filter((amount) => amount >= total());

  const pay = async (payments: PaymentRequest[]) => {
    const id = billId();
    if (id === null) {
      return;
    }
    setError(null);
    try {
      setDone(await settle(id, payments));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : "The store did not respond.");
    }
  };

  const payCash = () => {
    const chosen = tender() ?? total();
    void pay([
      {
        method: "PAYMENT_METHOD_CASH",
        tendered: money("VND", chosen),
        applied_to_bill: money("VND", total()),
      },
    ]);
  };

  const payCard = () =>
    void pay([
      {
        method: "PAYMENT_METHOD_CARD",
        tendered: money("VND", total()),
        applied_to_bill: money("VND", total()),
      },
    ]);

  return (
    <section class="mx-auto max-w-xl p-4">
      <a href={`/table/${params.id}`} class="text-sm text-ink-muted no-underline">
        ← Order
      </a>

      <Show
        when={done()}
        fallback={
          <>
            <p class="mt-4 text-sm text-ink-muted">Amount due</p>
            <p class="text-2xl font-semibold tabular-nums">
              {formatMoney(money("VND", total()))}
            </p>

            <Show when={error()}>
              {(message) => (
                <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                  {message()}
                </p>
              )}
            </Show>

            <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">Cash</h2>
            <div class="grid grid-cols-2 gap-2">
              <For each={quickCash()}>
                {(amount) => (
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line bg-surface tabular-nums"
                    classList={{ "border-accent": tender() === amount }}
                    onClick={() => setTender(amount)}
                  >
                    {amount === total() ? "Exact" : formatMoney(money("VND", amount))}
                  </button>
                )}
              </For>
            </div>
            <p class="mt-2 text-sm text-ink-muted">
              Change: <span class="tabular-nums">{formatMoney(money("VND", change()))}</span>
            </p>

            <div class="mt-4 flex flex-col gap-2">
              <button
                type="button"
                class="min-h-money rounded-token bg-accent text-lg font-semibold text-accent-ink"
                onClick={() => payCash()}
              >
                Take cash
              </button>
              <button
                type="button"
                class="min-h-touch rounded-token border border-line bg-surface"
                onClick={() => payCard()}
              >
                Card (exact)
              </button>
            </div>
          </>
        }
      >
        {(bill) => (
          <div class="mt-6 rounded-token border border-line bg-surface p-4">
            <p class="text-lg font-semibold text-ok">Settled</p>
            <p class="mt-2">
              Receipt <span class="font-semibold tabular-nums">#{bill().receipt_number ?? "—"}</span>
            </p>
            <p class="mt-1 text-ink-muted">
              Change: <span class="tabular-nums">{formatMoney(money("VND", change()))}</span>
            </p>
            <Show when={bill().print_receipt}>
              <p class="mt-1 text-sm text-ink-muted">Printing receipt…</p>
            </Show>
            <button
              type="button"
              class="mt-4 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink"
              onClick={() => navigate("/")}
            >
              Back to floor
            </button>
          </div>
        )}
      </Show>
    </section>
  );
}
