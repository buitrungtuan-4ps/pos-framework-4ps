import { For, Show, createSignal } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import { MENU } from "../lib/menu";
import { formatMoney } from "../lib/money";
import { addItem, fire, linesForTable, openBill, tableState } from "../state/store";

// A table's order: the running check on the left, the menu on the right (the tablet layout with a
// sliding bill). Tapping a menu item adds it optimistically; a fresh line can be fired to the
// kitchen. "Take payment" opens the bill and moves to the pay screen.
export function Order() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [error, setError] = createSignal<string | null>(null);

  const guard = async (run: () => Promise<void>) => {
    setError(null);
    try {
      await run();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : "The store did not respond.");
    }
  };

  const takePayment = () =>
    guard(async () => {
      await openBill(params.id);
      navigate(`/table/${params.id}/pay`);
    });

  return (
    <section class="grid gap-4 p-4 lg:grid-cols-[1fr_20rem]">
      <div>
        <div class="mb-3 flex items-center gap-3">
          <a href="/" class="text-sm text-ink-muted no-underline">
            ← Floor
          </a>
          <h1 class="text-lg font-semibold">Table {params.id.replace(/^0+/, "") || params.id}</h1>
          <span class="text-sm text-ink-muted">{tableState(params.id).replace("TABLE_STATE_", "").toLowerCase()}</span>
        </div>

        <Show when={error()}>
          {(message) => (
            <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
              {message()}
            </p>
          )}
        </Show>

        <ul class="flex flex-col gap-2">
          <For
            each={linesForTable(params.id)}
            fallback={<li class="text-ink-muted">No items yet. Add from the menu.</li>}
          >
            {(line) => (
              <li class="flex items-center gap-3 rounded-token border border-line bg-surface p-3">
                <span class="flex-1">{line.name}</span>
                <span class="tabular-nums">{formatMoney(line.lineTotal)}</span>
                <Show
                  when={line.state === "ORDER_LINE_STATE_ADDED"}
                  fallback={<span class="text-sm text-ok">Fired</span>}
                >
                  <button
                    type="button"
                    class="rounded-token bg-accent px-3 py-1 text-accent-ink"
                    onClick={() => void guard(() => fire(line.orderLineId))}
                  >
                    Fire
                  </button>
                </Show>
              </li>
            )}
          </For>
        </ul>

        <button
          type="button"
          class="mt-4 min-h-touch w-full rounded-token bg-accent px-4 text-lg font-semibold text-accent-ink"
          onClick={() => void takePayment()}
        >
          Take payment
        </button>
      </div>

      <aside>
        <h2 class="mb-2 text-sm font-semibold text-ink-muted">Menu</h2>
        <div class="grid grid-cols-2 gap-2 lg:grid-cols-1">
          <For each={MENU}>
            {(item) => (
              <button
                type="button"
                class="flex min-h-touch items-center justify-between rounded-token border border-line bg-surface px-3 py-2 text-left"
                onClick={() => void guard(() => addItem(params.id, item))}
              >
                <span>{item.name}</span>
                <span class="tabular-nums text-ink-muted">
                  {formatMoney({ currency_code: "VND", amount_minor: item.unitPriceMinor })}
                </span>
              </button>
            )}
          </For>
        </div>
      </aside>
    </section>
  );
}
