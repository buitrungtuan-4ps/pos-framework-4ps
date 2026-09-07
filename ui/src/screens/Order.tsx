import { For, Show, createSignal } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { formatMoney } from "../lib/money";
import type { LayoutButton, MenuItemResponse } from "../api/types";
import {
  addItem,
  fire,
  linesForTable,
  openBill,
  reasonsFor,
  state,
  tableState,
  voidLine,
  type OrderLine,
} from "../state/store";

// The action a void of one line cites, so the picker offers what the store holds *for voiding* and
// nothing else (ADR-0115). A reason valid only for refusing a guest's order — `OUT_OF_STOCK` is one
// in the framework's own default set — never appears here, because the edge would refuse it.
const VOID_LINE = "REASON_ACTION_VOID_LINE";

// A table's order: the running check on the left, the menu on the right (the tablet layout with a
// sliding bill). Tapping a menu item adds it optimistically; a fresh line can be fired to the
// kitchen. "Take payment" opens the bill and moves to the pay screen.
export function Order() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [error, setError] = createSignal<string | null>(null);
  const label = () => params.id.replace(/^0+/, "") || params.id;

  const guard = async (run: () => Promise<void>) => {
    setError(null);
    try {
      await run();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  const takePayment = () =>
    guard(async () => {
      await openBill(params.id);
      navigate(`/table/${params.id}/pay`);
    });

  // The line whose void is being reasoned about, and the manager standing at the till for it. The
  // approver's badge and PIN live here for the length of one refusal and no longer — `closeVoid`
  // clears both, and nothing writes either to storage.
  const [voiding, setVoiding] = createSignal<OrderLine | null>(null);
  const [approverCode, setApproverCode] = createSignal("");
  const [approverPin, setApproverPin] = createSignal("");

  const voided = (line: OrderLine) => line.state === "ORDER_LINE_STATE_VOIDED";

  // A line the kitchen has already been told to make. §5 puts that behind `VoidFiredLine` and a
  // verified PIN — the food exists, and stock has moved. An unfired line is an ordinary cancel.
  const needsApproval = (line: OrderLine) => line.state === "ORDER_LINE_STATE_FIRED";

  // Whether the reason buttons can be tapped yet. A fired line with no manager typed in would be
  // refused by the edge, and a button that can only produce a refusal is worse than a disabled one:
  // it teaches an operator that the till is unreliable rather than that something is missing.
  const readyToVoid = (line: OrderLine) =>
    !needsApproval(line) || (approverCode().trim() !== "" && approverPin() !== "");

  const closeVoid = () => {
    setVoiding(null);
    setApproverCode("");
    setApproverPin("");
  };

  // The first tap: open the picker for this line. Not a void of its own — nothing is written until a
  // reason is chosen, which is what makes the reason mandatory rather than a follow-up question.
  const askVoid = (line: OrderLine) => {
    setError(null);
    setVoiding(line);
  };

  // The second tap: void the line, citing this reason. The panel stays open on a refusal so the
  // operator can read it and try another reason or fetch a manager, and closes only once the edge
  // has recorded the void.
  const voidReason = (line: OrderLine, reasonCodeId: string) =>
    guard(async () => {
      await voidLine(
        line.orderLineId,
        reasonCodeId,
        needsApproval(line)
          ? { approver_code: approverCode().trim(), approver_pin: approverPin() }
          : undefined,
      );
      closeVoid();
    });

  // Only categories that still have something to sell. A category whose every button names an item
  // the price book no longer carries would otherwise draw as an empty heading — the console arranged
  // it before the item was withdrawn, and a heading with nothing under it reads as a fault.
  const arranged = () =>
    state.layout.filter(
      (category) =>
        category.buttons.some(priced) ||
        category.subcategories.some((subcategory) => subcategory.buttons.some(priced)),
    );

  // Layout names the item; the price book prices it; the two meet only at the id (ADR-0066).
  const priced = (button: LayoutButton) =>
    state.menu.some((item) => item.menu_item_id === button.menu_item_id);

  const sellButton = (item: MenuItemResponse, caption: string) => (
    <button
      type="button"
      class="flex min-h-touch items-center justify-between rounded-token border border-line bg-surface px-3 py-2 text-left disabled:opacity-50"
      disabled={!item.available}
      data-step="addItem"
      onClick={() => void guard(() => addItem(params.id, item))}
    >
      <span>{caption}</span>
      <span class="tabular-nums text-ink-muted">
        {item.available ? formatMoney(item.unit_price) : t("order.unavailable")}
      </span>
    </button>
  );

  // An arranged button carries the caption the console wrote; the price comes from the price book, so
  // there is never a second price that can disagree with it. A button naming an item the price book
  // does not carry draws nothing rather than an unpriceable tap.
  const arrangedButton = (button: LayoutButton) => {
    const item = state.menu.find((entry) => entry.menu_item_id === button.menu_item_id);
    return <Show when={item}>{(found) => sellButton(found(), button.label)}</Show>;
  };

  return (
    <section class="grid gap-4 p-4 lg:grid-cols-[1fr_20rem]">
      <div>
        <div class="mb-3 flex items-center gap-3">
          <a href="/" class="text-sm text-ink-muted no-underline">
            {t("common.back_floor")}
          </a>
          {/* Seating a table ends here, on this table's order screen (ADR-0109). */}
          <h1 class="text-lg font-semibold" data-outcome="order-open">
            {t("common.table", { label: label() })}
          </h1>
          <span class="text-sm text-ink-muted">{t(tableStateKey(tableState(params.id)))}</span>
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
            fallback={<li class="text-ink-muted">{t("order.empty")}</li>}
          >
            {(line) => (
              <li
                class="flex items-center gap-3 rounded-token border border-line bg-surface p-3"
                data-outcome="line-added"
              >
                {/* A voided line stays on the order, struck through. Removing the row would make a
                    mis-tapped void invisible to the person who made it — the guest is not charged
                    either way, and seeing what was cancelled is how the mistake gets noticed. */}
                <span
                  class="flex-1"
                  classList={{ "line-through text-ink-muted": voided(line) }}
                >
                  {line.name}
                </span>
                <span class="tabular-nums" classList={{ "text-ink-muted": voided(line) }}>
                  {formatMoney(line.lineTotal)}
                </span>
                <Show when={line.state === "ORDER_LINE_STATE_ADDED"}>
                  <button
                    type="button"
                    class="rounded-token bg-accent px-3 py-1 text-accent-ink"
                    data-step="fire"
                    onClick={() => void guard(() => fire(line.orderLineId))}
                  >
                    {t("order.fire")}
                  </button>
                </Show>
                <Show when={line.state === "ORDER_LINE_STATE_FIRED"}>
                  <span class="text-sm text-ok" data-outcome="line-fired">
                    {t("order.fired")}
                  </span>
                </Show>
                {/* The void control (ADR-0115, §11 item 2). Disabled while the store's reason list
                    has not loaded: the edge falls back to the framework's own set when nothing is
                    published, so an empty list here means the read has not landed, never that the
                    store has no reasons — and a picker opening on nothing would be a dead end. */}
                <Show
                  when={!voided(line)}
                  fallback={
                    <span class="text-sm text-ink-muted" data-outcome="line-voided">
                      {t("order.voided")}
                    </span>
                  }
                >
                  <button
                    type="button"
                    class="rounded-token border border-line px-3 py-1 text-sm text-ink-muted disabled:opacity-50"
                    disabled={reasonsFor(VOID_LINE).length === 0}
                    data-step="askVoid"
                    onClick={() => askVoid(line)}
                  >
                    {t("order.void")}
                  </button>
                </Show>
              </li>
            )}
          </For>
        </ul>

        {/*
          The reason picker (ADR-0115). Every entry it offers declares `REASON_ACTION_VOID_LINE`, so
          what a member of staff can cite is what the store published for voiding — the same question
          the edge asks before it writes the event, which is why the list can never offer a reason
          that would be refused.

          A fired line adds the manager block above it. The badge and PIN are typed, not tapped, so
          the void still costs the two taps `docs/ui-ux.md` §6 allows a rare action; the second tap
          is disabled until both are filled rather than sending a request that can only come back
          `403`.
        */}
        <Show when={voiding()}>
          {(line) => (
            <div class="mt-4 rounded-token border border-line bg-surface p-3">
              <h2 class="font-semibold">{t("order.void_title", { item: line().name })}</h2>

              <Show when={needsApproval(line())}>
                <p class="mt-1 text-sm text-ink-muted">{t("order.void_manager")}</p>
                <label class="mt-2 block text-sm">
                  {t("order.approver_code")}
                  <input
                    type="text"
                    class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                    value={approverCode()}
                    onInput={(event) => setApproverCode(event.currentTarget.value)}
                  />
                </label>
                <label class="mt-2 block text-sm">
                  {t("order.approver_pin")}
                  <input
                    type="password"
                    inputmode="numeric"
                    class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                    value={approverPin()}
                    onInput={(event) => setApproverPin(event.currentTarget.value)}
                  />
                </label>
              </Show>

              <p class="mt-3 text-sm text-ink-muted">{t("order.void_reason")}</p>
              <div class="mt-2 grid grid-cols-2 gap-2">
                <For each={reasonsFor(VOID_LINE)}>
                  {(reason) => (
                    <button
                      type="button"
                      class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                      disabled={!readyToVoid(line())}
                      data-step="voidReason"
                      onClick={() => void voidReason(line(), reason.reason_code_id)}
                    >
                      {reason.display_name}
                    </button>
                  )}
                </For>
              </div>

              <button
                type="button"
                class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
                onClick={() => closeVoid()}
              >
                {t("common.cancel")}
              </button>
            </div>
          )}
        </Show>

        <button
          type="button"
          class="mt-4 min-h-touch w-full rounded-token bg-accent px-4 text-lg font-semibold text-accent-ink"
          data-step="takePayment"
          onClick={() => void takePayment()}
        >
          {t("order.take_payment")}
        </button>
      </div>

      <aside>
        <h2 class="mb-2 text-sm font-semibold text-ink-muted">{t("order.menu")}</h2>
        {/*
          Two ways to draw the same price book. When the console has arranged buttons on the `layout`
          node (ADR-0066, C4) the till groups by the categories it authored, in the order it authored
          them; when it has arranged nothing, the flat list is the honest fallback and is what the
          till drew before that node had a reader.
        */}
        <Show
          when={arranged().length > 0}
          fallback={
            <div class="grid grid-cols-2 gap-2 lg:grid-cols-1">
              <For
                each={state.menu}
                fallback={<p class="text-ink-muted">{t("order.menu_empty")}</p>}
              >
                {(item) => sellButton(item, item.display_name)}
              </For>
            </div>
          }
        >
          <For each={arranged()}>
            {(category) => (
              <section class="mb-4">
                <h3 class="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-muted">
                  {category.name}
                </h3>
                <div class="grid grid-cols-2 gap-2 lg:grid-cols-1">
                  <For each={category.buttons}>{(button) => arrangedButton(button)}</For>
                </div>
                <For each={category.subcategories}>
                  {(subcategory) => (
                    <div class="mt-3">
                      <h4 class="mb-2 text-xs text-ink-muted">{subcategory.name}</h4>
                      <div class="grid grid-cols-2 gap-2 lg:grid-cols-1">
                        <For each={subcategory.buttons}>{(button) => arrangedButton(button)}</For>
                      </div>
                    </div>
                  )}
                </For>
              </section>
            )}
          </For>
        </Show>
      </aside>
    </section>
  );
}
