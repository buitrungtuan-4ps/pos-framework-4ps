// The stores this HQ reads in other cells (ADR-0179 C10): per store, the days read and the events
// counted, or why it could not be read.
//
// Counts only: only aggregates cross a cell boundary, and the answer never holds a key, so there is
// none to show. It sits on the fleet screen under this cell's own panel, because both describe the
// deployment rather than a tenant. The read reaches other cells and may take up to 30 seconds, so it
// is not repeated when the operator comes back to the tab.

import { For, Show } from "solid-js";

import { getHqRollups, type HqStore } from "../api/hq";
import { t } from "../i18n";
import { createAdminResource, failureOf } from "../lib/resource";
import { Banner, Card, Skeleton, StatusBadge } from "./ui";

/** Every event a store's days counted, in total. */
export function totalEvents(store: HqStore): number {
  return (store.rollups ?? []).reduce((sum, day) => sum + day.total_events, 0);
}

/** One store: where it is, and its counts or the reason there are none. */
function StoreRow(props: { store: HqStore }) {
  return (
    <li class="flex flex-col gap-1 border-b border-line py-2 last:border-b-0">
      <span class="break-all text-ink">
        {t("hq.storeIn", { cell: props.store.cell_id, store: props.store.store_id })}
      </span>
      <Show
        when={props.store.rollups}
        fallback={
          <span role="status" class="break-words text-danger">
            {t("hq.notRead", { reason: props.store.error ?? "" })}
          </span>
        }
      >
        {(days) => (
          <span class="text-ink-muted">
            {t("hq.counts", {
              days: String(days().length),
              events: String(totalEvents(props.store)),
            })}
          </span>
        )}
      </Show>
    </li>
  );
}

export function HqCellsPanel() {
  const hq = createAdminResource(() => getHqRollups());
  const unread = () => (hq.value()?.stores ?? []).filter((store) => !store.rollups).length;
  return (
    <Card
      title={t("hq.title")}
      actions={
        <Show when={unread() > 0}>
          <StatusBadge tone="danger" label={t("hq.unread", { count: String(unread()) })} />
        </Show>
      }
    >
      <Show when={failureOf(hq)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <Show when={hq.value()} fallback={<Skeleton label={t("common.loading")} rows={2} />}>
        {(answer) => (
          <Show
            when={answer().configured}
            fallback={<p class="text-sm text-ink-muted">{t("hq.notConfigured")}</p>}
          >
            <p class="text-sm text-ink-muted">{t("hq.aggregatesOnly")}</p>
            <ul class="text-sm">
              <For each={answer().stores}>{(store) => <StoreRow store={store} />}</For>
            </ul>
          </Show>
        )}
      </Show>
    </Card>
  );
}
