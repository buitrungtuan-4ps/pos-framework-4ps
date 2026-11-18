import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import type { Tab } from "../api/schemas";
import { ReadFailed } from "../components/StoreData";
import { t } from "../i18n";
import { clockTime } from "../lib/clock-time";
import { errorMessage } from "../lib/errors";
import { formatAmount, reasonsFor } from "../state/store";
import { createScreenRead } from "../state/screen-read";
import { closeEmptyTab, labelTab, openTab, tabLabelsEnabled } from "../state/tabs";
import { RegionsContext, type Regions } from "../../../packages/package-renderer/src/regions";
import { NO_ACTS, PackageScreen } from "../package/screen";

// A bar's open tabs (ADR-0221 decision 3), as every till lists them: by the day's number, with the
// guest's label where the store keeps labels, what each owes and when its last round went out. A
// tab is taken up on the counter's order screen, where it takes rounds, fires and is paid as any
// order. One nothing was sold on closes here with the store's reason; a label is written here.
//
// Other tills open and settle tabs too, so the list reads again every `REFRESH_MS` while it is open.
const REFRESH_MS = 15_000;

/** The reason action a close cites (`sales.order.close`). */
const CLOSE_ORDER = "REASON_ACTION_CLOSE_ORDER";

/** Whether a tab has nothing on it to pay: closing is offered only then; the edge decides. */
function empty(tab: Tab): boolean {
  return tab.total_due.amount_minor === 0 && tab.bill_id === undefined;
}

/** One open tab: its number, label, last round and what it owes, and its label and close controls. */
function TabCard(props: {
  readonly tab: Tab;
  readonly labelling: boolean;
  readonly closing: boolean;
  readonly draft: string;
  readonly setDraft: (text: string) => void;
  readonly onOpen: () => void;
  readonly onLabel: () => void;
  readonly onSaveLabel: () => void;
  readonly onAskClose: () => void;
  readonly onClose: (reasonCodeId: string) => void;
  readonly onCancelClose: () => void;
}) {
  return (
    <li class="rounded-token border border-line bg-surface p-4" data-outcome="tab">
      <button type="button" class="flex min-h-touch w-full items-baseline justify-between gap-3 text-left" onClick={() => props.onOpen()}>
        <span class="text-lg font-semibold">
          {t("tabs.number", { number: props.tab.tab_number })}
          <Show when={props.tab.label}>{(label) => <span class="ml-2 font-normal">{label()}</span>}</Show>
        </span>
        <span class="flex gap-3 tabular-nums">
          <Show when={props.tab.last_round_time}>
            {(time) => <span class="text-sm text-ink-muted">{t("tabs.last_round", { time: clockTime(time()) })}</span>}
          </Show>
          <span class="font-semibold">{formatAmount(props.tab.total_due)}</span>
        </span>
      </button>
      <div class="mt-2 flex gap-2">
        <Show when={tabLabelsEnabled()}>
          <button type="button" class="min-h-touch rounded-token border border-line px-3 text-sm" onClick={() => props.onLabel()}>
            {t("tabs.label")}
          </button>
        </Show>
        <Show when={empty(props.tab)}>
          <button type="button" class="min-h-touch rounded-token border border-line px-3 text-sm" data-outcome="tab-close" onClick={() => props.onAskClose()}>
            {t("tabs.close")}
          </button>
        </Show>
      </div>
      <Show when={props.labelling}>
        <form class="mt-2 flex gap-2" onSubmit={(event) => { event.preventDefault(); props.onSaveLabel(); }}>
          <input class="min-h-touch flex-1 rounded-token border border-line px-3" maxLength={32} value={props.draft} aria-label={t("tabs.label")} onInput={(event) => props.setDraft(event.currentTarget.value)} />
          <button type="submit" class="min-h-touch rounded-token bg-primary px-3 text-primary-ink">{t("tabs.label_save")}</button>
        </form>
      </Show>
      <Show when={props.closing}>
        <div class="mt-2 flex flex-col gap-2">
          <p class="font-semibold">{t("tabs.close_reason")}</p>
          <For each={reasonsFor(CLOSE_ORDER)}>
            {(reason) => (
              <button type="button" class="min-h-touch rounded-token border border-line px-3 text-left" onClick={() => props.onClose(reason.reason_code_id)}>
                {reason.display_name}
              </button>
            )}
          </For>
          <button type="button" class="min-h-touch rounded-token border border-line text-sm text-ink-muted" onClick={() => props.onCancelClose()}>
            {t("common.cancel")}
          </button>
        </div>
      </Show>
    </li>
  );
}

export function Tabs() {
  const navigate = useNavigate();
  const [tabs, setTabs] = createSignal<Tab[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [labelling, setLabelling] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal("");
  const [closing, setClosing] = createSignal<string | null>(null);

  const tabsRead = createScreenRead(() => api.openTabs(), setTabs);
  const refresh = () => tabsRead.run();
  onMount(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), REFRESH_MS);
    onCleanup(() => clearInterval(timer));
  });

  const list = { get: tabs, set: setTabs };
  const act = (write: Promise<unknown>) => {
    setError(null);
    void write.catch((caught: unknown) => setError(errorMessage(caught)));
  };
  const open = () => {
    setError(null);
    void openTab(refresh)
      .then((opened) => navigate(`/order/${opened.order_id}`))
      .catch((caught: unknown) => setError(errorMessage(caught)));
  };
  const saveLabel = (orderId: string) => {
    setLabelling(null);
    act(labelTab(list, orderId, draft(), refresh));
  };
  const close = (orderId: string, reasonCodeId?: string) => {
    setClosing(null);
    act(closeEmptyTab(list, orderId, reasonCodeId, refresh));
  };
  // A store whose list gives no reason for closing closes without one; otherwise the picker asks.
  const askClose = (orderId: string) =>
    reasonsFor(CLOSE_ORDER).length === 0 ? close(orderId) : setClosing(orderId);

  const regions: Regions = {
    tabs_list: () => <>
      <div class="flex items-center justify-between gap-3">
        <p class="text-ink-muted">{t(tabLabelsEnabled() ? "tabs.subtitle_labels" : "tabs.subtitle")}</p>
        <button type="button" class="min-h-touch rounded-token bg-primary px-4 font-semibold text-primary-ink" data-outcome="tab-open" onClick={open}>
          {t("tabs.open")}
        </button>
      </div>
      <Show when={error()}>
        {(message) => <p class="mt-3 rounded-token border border-awaiting px-3 py-2 text-ink" role="status">{message()}</p>}
      </Show>
      <ReadFailed read={tabsRead} />
      <Show when={tabs().length > 0} fallback={<p class="mt-6 text-ink-muted">{t("tabs.empty")}</p>}>
        <ul class="mt-4 flex list-none flex-col gap-3 p-0">
          <For each={tabs()}>
            {(tab) => (
              <TabCard
                tab={tab}
                labelling={labelling() === tab.order_id}
                closing={closing() === tab.order_id}
                draft={draft()}
                setDraft={setDraft}
                onOpen={() => navigate(`/order/${tab.order_id}`)}
                onLabel={() => { setDraft(tab.label ?? ""); setLabelling(tab.order_id); }}
                onSaveLabel={() => saveLabel(tab.order_id)}
                onAskClose={() => askClose(tab.order_id)}
                onClose={(reasonCodeId) => close(tab.order_id, reasonCodeId)}
                onCancelClose={() => setClosing(null)}
              />
            )}
          </For>
        </ul>
      </Show>
    </>,
  };
  return <RegionsContext.Provider value={regions}><PackageScreen screenId="tabs" host={NO_ACTS} /></RegionsContext.Provider>;
}
