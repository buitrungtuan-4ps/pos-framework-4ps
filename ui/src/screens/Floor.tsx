import { For, Show, createSignal } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { ApiError } from "../api/client";
import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { FLOOR, clean, seat, tableState } from "../state/store";

const DOT: Record<string, string> = {
  TABLE_STATE_FREE: "bg-free",
  TABLE_STATE_OCCUPIED: "bg-occupied",
  TABLE_STATE_AWAITING_PAYMENT: "bg-awaiting",
  TABLE_STATE_NEEDS_CLEANING: "bg-cleaning",
};

// The floor plan: one card per table, its state shown by a labelled colour (never colour alone). A
// free table seats and opens; an occupied or paying table opens its order; a table needing cleaning
// offers the one action that clears it. This is the two-column POS layout — a grid that reflows for
// tablet and phone rather than a fixed board.
export function Floor() {
  const navigate = useNavigate();
  const [error, setError] = createSignal<string | null>(null);

  const onCard = async (tableId: string) => {
    setError(null);
    const current = tableState(tableId);
    try {
      if (current === "TABLE_STATE_FREE") {
        await seat(tableId);
        navigate(`/table/${tableId}`);
      } else if (current === "TABLE_STATE_NEEDS_CLEANING") {
        await clean(tableId);
      } else {
        navigate(`/table/${tableId}`);
      }
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  return (
    <section class="p-4">
      <h1 class="mb-4 text-lg font-semibold">{t("floor.title")}</h1>
      <Show when={error()}>
        {(message) => (
          <p class="mb-4 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>
      <div class="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
        <For each={FLOOR}>
          {(table) => {
            const currentState = () => tableState(table.id);
            return (
              <button
                type="button"
                class="flex min-h-touch flex-col items-start gap-2 rounded-token border border-line bg-surface p-4 text-left"
                onClick={() => void onCard(table.id)}
              >
                <span class="text-xl font-semibold">{t("common.table", { label: table.label })}</span>
                <span class="inline-flex items-center gap-2 text-sm text-ink-muted">
                  <span
                    class={`inline-block h-2.5 w-2.5 rounded-full ${DOT[currentState()] ?? "bg-free"}`}
                    aria-hidden="true"
                  />
                  {t(tableStateKey(currentState()))}
                </span>
              </button>
            );
          }}
        </For>
      </div>
    </section>
  );
}
