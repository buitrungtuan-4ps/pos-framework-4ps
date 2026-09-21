import { For, Show, createMemo, createResource, createSignal } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { formatMoney } from "../lib/money";
import { matches } from "../lib/search";
import type { LayoutButton, MenuItemResponse, ModifierGroup } from "../api/types";
import {
  addItem,
  chooseSeat,
  groupsFor,
  modifiersSatisfied,
  fireOrder,
  floorTables,
  linesForTable,
  loadCheck,
  modifierNames,
  openBill,
  reasonsFor,
  seatFor,
  seatsEnabled,
  state,
  tableState,
  unfiredLinesForTable,
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

  // What the table owes right now, asked of the edge — the same `billing::assemble` the settle runs,
  // so the guest is quoted the figure the bill will charge (roadmap-v3 E5).
  //
  // Until this screen showed it, the only way to answer "how much so far?" was to press Take
  // payment, and that **opens a bill**: a guest's question changed the order's state. The
  // fingerprint below is what the edge would price differently — a line appearing, disappearing, or
  // changing state — so the figure refreshes on every act without polling.
  //
  // Written below `params` deliberately: a `createMemo` runs when it is created, so one placed above
  // the `const`s it reads dies in their temporal dead zone and takes the screen down (`.jules/bolt.md`).
  const priceable = createMemo(() =>
    linesForTable(params.id)
      .map((line) => `${line.orderLineId}:${line.state}:${line.quantityMilli}`)
      .join(","),
  );
  const [check] = createResource(priceable, () => loadCheck(params.id));

  // The lines the kitchen has not been told about. The count rides on the Send button, because the
  // question an operator asks before pressing it is "what is about to go" — and the answer used to
  // be a row count they made themselves (`docs/ui-ux.md` §3).
  const unfired = () => unfiredLinesForTable(params.id);

  // How many the store said this table seats, from the published floor plan. Zero means the plan
  // recorded no capacity, and a picker with no seats in it is worse than none — so the control only
  // appears where the store both does seats and said how many this table has.
  const seatCount = () => floorTables().find((table) => table.id === params.id)?.seats ?? 0;
  const showSeats = () => seatsEnabled() && seatCount() > 0;

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

  // Pre-index menu items by ID using createMemo to allow O(1) lookups instead of O(N) linear scans.
  const menuItemMap = createMemo(
    () => new Map(state.menu.map((item) => [item.menu_item_id, item])),
  );

  // Layout names the item; the price book prices it; the two meet only at the id (ADR-0066).
  // Uses O(1) hash map lookup instead of O(N) state.menu.some(...).
  const priced = (button: LayoutButton) => menuItemMap().has(button.menu_item_id);

  // Only categories that still have something to sell. Memoized to prevent re-filtering layout
  // categories on every access/re-render, reducing overall layout grid assembly complexity from
  // O(M * N) to O(M + N) where M is layout buttons count and N is menu items count.
  const arranged = createMemo(() =>
    state.layout.filter(
      (category) =>
        category.buttons.some(priced) ||
        category.subcategories.some((subcategory) => subcategory.buttons.some(priced)),
    ),
  );

  // What the operator has typed into the menu box. Empty means the grid, which is what the screen
  // has always shown; a query replaces it with the matches, flat, because a category heading over a
  // result list describes where the item lives rather than why it is on screen.
  // The item a guest is being asked about, and the choices made so far (ADR-0127).
  //
  // Only an item that attaches a group opens this: tapping a plain item still sells it in one tap,
  // which is §6's headline case and the one most taps on this screen are. An item with choices is a
  // different act and is declared as its own task.
  const [choosing, setChoosing] = createSignal<MenuItemResponse | null>(null);
  const [chosen, setChosen] = createSignal<string[]>([]);

  const [query, setQuery] = createSignal("");
  const searching = () => query().trim() !== "";

  // Every caption an item can be found by: the price book's own name, plus whatever the console
  // wrote on each button pointing at it (ADR-0066). The two differ on purpose — a button inside a
  // "Pizza" category can say "Large" — and an operator who knows an item by the grid's word for it
  // would otherwise search for it and be told there is no such thing.
  //
  // Built once per menu-and-layout rather than per keystroke: the two gates this screen already
  // carries for that (`menuItemMap`, `arranged`) exist because a linear scan per render is what this
  // grid costs, and a scan per *letter typed* would be worse than either.
  const captions = createMemo(() => {
    const byItem = new Map<string, string[]>(
      state.menu.map((item) => [item.menu_item_id, [item.display_name]]),
    );
    const record = (button: LayoutButton) => {
      const known = byItem.get(button.menu_item_id);
      if (known !== undefined && !known.includes(button.label)) {
        known.push(button.label);
      }
    };
    for (const category of state.layout) {
      category.buttons.forEach(record);
      for (const subcategory of category.subcategories) {
        subcategory.buttons.forEach(record);
      }
    }
    return byItem;
  });

  // The matches, in the price book's own order. Drawn with `display_name` and not the caption that
  // matched: a button's caption is shorthand that means something inside its category and nothing
  // outside it, and a flat result list is outside it.
  const results = createMemo(() =>
    searching()
      ? state.menu.filter((item) =>
          matches(query(), captions().get(item.menu_item_id) ?? [item.display_name]),
        )
      : [],
  );

  // Tapping an item: sell it, or ask first. The question is the store's, not this screen's — an
  // item attaches groups or it does not, and the till has no opinion beyond obeying that.
  const onItem = (item: MenuItemResponse) => {
    if (groupsFor(item).length === 0) {
      void guard(() => addItem(params.id, item));
      return;
    }
    setError(null);
    setChosen([]);
    setChoosing(item);
  };

  // One choice, toggled. A group at its maximum replaces rather than refuses: on a single-choice
  // group — every "Size" — tapping another size is obviously a correction, and making the operator
  // untick the first one would cost a tap to undo a tap.
  const chooseModifier = (group: ModifierGroup, memberId: string) => {
    setChosen((current) => {
      if (current.includes(memberId)) {
        return current.filter((id) => id !== memberId);
      }
      const mine = current.filter((id) => group.member_menu_item_ids.includes(id));
      if (mine.length >= group.max_select) {
        const dropped = current.filter((id) => !group.member_menu_item_ids.includes(id));
        // Keep the choices made *before* the ones in this group, so the order a guest chose in
        // survives a correction.
        return [...dropped, ...mine.slice(1), memberId];
      }
      return [...current, memberId];
    });
  };

  const closeChoosing = () => {
    setChoosing(null);
    setChosen([]);
  };

  // The confirm. Disabled until every required group is satisfied — the same arithmetic the edge
  // runs, so the refusal never reaches the screen. The edge is still the authority; this only keeps
  // a `409` off a guest's eyeline.
  const confirmItem = (item: MenuItemResponse) =>
    guard(async () => {
      await addItem(params.id, item, chosen());
      closeChoosing();
    });

  const sellButton = (item: MenuItemResponse, caption: string) => (
    <button
      type="button"
      class="flex min-h-touch items-center justify-between rounded-token border border-line bg-surface px-3 py-2 text-left disabled:opacity-50"
      disabled={!item.available}
      data-step="onItem"
      onClick={() => onItem(item)}
    >
      <span>{caption}</span>
      <span class="tabular-nums text-ink-muted">
        {item.available ? formatMoney(item.unit_price) : t("order.unavailable")}
      </span>
    </button>
  );

  // What the aside draws while a query is in the box: the matches, or the sentence saying there are
  // none. The empty state names what was searched for, because "nothing found" and "nothing found
  // *for this*" are different amounts of help when the answer is a typo.
  const searchResults = () => (
    <div class="grid grid-cols-2 gap-2 terminal:grid-cols-1" data-outcome="menu-results">
      <For
        each={results()}
        fallback={
          <p class="text-ink-muted">{t("order.search_empty", { query: query().trim() })}</p>
        }
      >
        {(item) => sellButton(item, item.display_name)}
      </For>
    </div>
  );

  // An arranged button carries the caption the console wrote; the price comes from the price book, so
  // there is never a second price that can disagree with it. Uses O(1) hash map lookup instead of
  // O(N) state.menu.find(...).
  const arrangedButton = (button: LayoutButton) => {
    const item = menuItemMap().get(button.menu_item_id);
    return <Show when={item}>{(found) => sellButton(found(), button.label)}</Show>;
  };

  return (
    <section class="grid gap-4 p-4 terminal:grid-cols-[1fr_20rem]">
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
                <span class="flex flex-1 flex-col">
                  <span classList={{ "line-through text-ink-muted": voided(line) }}>
                    {line.name}
                  </span>
                  {/* What was chosen, under the item it was chosen for (ADR-0127). The price already
                      included it and the caption did not, so this row read "Margherita" whether the
                      server picked 25cm or 30cm — and a server checking an order back to a guest had
                      nothing to check against. Struck through with the line, because a voided
                      line's choices are cancelled with it. */}
                  <Show when={modifierNames(line).length > 0}>
                    <span
                      class="text-sm text-ink-muted"
                      classList={{ "line-through": voided(line) }}
                      data-outcome="line-modifiers"
                    >
                      {modifierNames(line).join(" · ")}
                    </span>
                  </Show>
                </span>
                {/* Which seat it was ordered for, on the line that carries it. A line with none is
                    the table's, and says nothing rather than saying "no seat". */}
                <Show when={line.seat !== undefined}>
                  <span class="rounded-token bg-surface-muted px-2 text-sm tabular-nums text-ink-muted" data-outcome="line-seat">
                    {t("order.seat_short", { seat: line.seat ?? 0 })}
                  </span>
                </Show>
                <span class="tabular-nums" classList={{ "text-ink-muted": voided(line) }}>
                  {formatMoney(line.lineTotal)}
                </span>
                {/* A line waiting to be sent says so and nothing more: the Send button below acts on
                    every one of them at once, so a control per row would be one tap out of six
                    doing what one tap now does for all. */}
                <Show when={line.state === "ORDER_LINE_STATE_ADDED"}>
                  <span class="text-sm text-ink-muted">{t("order.unsent")}</span>
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

        {/*
          The choices an item needs before it can be sold (ADR-0127, `docs/ui-ux.md` §3's "required
          modifier groups open immediately").

          It opens on the item tap and not behind a button of its own, which is what keeps the
          common case at one tap: an item attaching no group never opens this at all. An item that
          does costs one tap per choice plus the confirm, and is declared as its own task rather
          than folded into "Add an item" — the reason the seat and the tipped settle are separate.

          The confirm is disabled until every required group is satisfied, running the same
          arithmetic the edge runs. The edge is still the authority; this only keeps a refusal off a
          guest's eyeline.
        */}
        <Show when={choosing()}>
          {(item) => (
            <div class="mb-3 rounded-token border border-line bg-surface p-3">
              <h2 class="font-semibold">{t("order.choose_title", { item: item().display_name })}</h2>
              <For each={groupsFor(item())}>
                {(group) => (
                  <div class="mt-3">
                    <p class="text-sm text-ink-muted">
                      {group.min_select >= 1
                        ? t("order.choose_required", { group: group.display_name })
                        : t("order.choose_optional", { group: group.display_name })}
                    </p>
                    <div class="mt-2 grid grid-cols-2 gap-2">
                      <For each={group.member_menu_item_ids}>
                        {(memberId) => (
                          <Show when={menuItemMap().get(memberId)}>
                            {(member) => (
                              <button
                                type="button"
                                class="flex min-h-touch items-center justify-between rounded-token border border-line px-3 text-left"
                                classList={{
                                  "bg-primary text-primary-ink": chosen().includes(memberId),
                                  "bg-surface text-ink": !chosen().includes(memberId),
                                }}
                                aria-pressed={chosen().includes(memberId)}
                                data-step="chooseModifier"
                                onClick={() => chooseModifier(group, memberId)}
                              >
                                <span>{member().display_name}</span>
                                {/* A modifier is an ordinary item with its own price, which is how
                                    a large costs more than a small. A free choice says nothing
                                    rather than saying zero. */}
                                <Show when={member().unit_price.amount_minor > 0}>
                                  <span class="tabular-nums text-ink-muted">
                                    {"+ "}
                                    {formatMoney(member().unit_price)}
                                  </span>
                                </Show>
                              </button>
                            )}
                          </Show>
                        )}
                      </For>
                    </div>
                  </div>
                )}
              </For>
              <button
                type="button"
                class="mt-4 min-h-money w-full rounded-token bg-primary px-4 font-semibold text-primary-ink disabled:opacity-50"
                disabled={!modifiersSatisfied(item(), chosen())}
                data-step="confirmItem"
                onClick={() => void confirmItem(item())}
              >
                {t("order.choose_add")}
              </button>
              <button
                type="button"
                class="mt-2 min-h-touch w-full rounded-token border border-line px-3 text-sm"
                onClick={() => closeChoosing()}
              >
                {t("common.cancel")}
              </button>
            </div>
          )}
        </Show>

        {/* Whose dish the next items are. Chosen before the items, as `docs/ui-ux.md` §3 asks, and
            sticky: a server orders a whole seat's worth at once, so making it per-item would be one
            extra tap per dish. Tapping the chosen seat again clears it, which is how "this one is
            for the table" is said without a second control.

            Absent entirely unless the store assigns seats — the edge refuses a seat when the
            capability is off, and offering an act that can only be refused teaches an operator the
            till is unreliable. */}
        <Show when={showSeats()}>
          <div class="mb-3 flex flex-wrap items-center gap-2">
            <span class="text-sm text-ink-muted">{t("order.seat_for")}</span>
            <For each={Array.from({ length: seatCount() }, (_, index) => index + 1)}>
              {(seat) => (
                <button
                  type="button"
                  class="min-h-touch w-11 rounded-token border border-line tabular-nums"
                  classList={{
                    "bg-primary text-primary-ink": seatFor(params.id) === seat,
                    "text-ink": seatFor(params.id) !== seat,
                  }}
                  aria-pressed={seatFor(params.id) === seat}
                  data-step="chooseSeat"
                  onClick={() => chooseSeat(params.id, seat)}
                >
                  {seat}
                </button>
              )}
            </For>
          </div>
        </Show>

        {/*
          What the table owes, on the screen the order is taken on. `docs/ui-ux.md` §8 asks for the
          total to be the largest thing on a money screen; until now this screen carried no figure at
          all, and the only way to read one was to press Take payment — which opens a bill. Asking a
          guest's question should not change the order's state.

          The three lines are the edge's, not a sum this app made: one calculation, in the domain
          (ADR-0028). A read that has not landed shows a dash rather than a zero, because a zero is a
          number and "I do not know yet" is not.
        */}
        <div class="mt-4 rounded-token border border-line bg-surface p-3" data-outcome="check-total">
          <Show
            when={check()}
            fallback={<p class="text-right text-2xl font-semibold tabular-nums">{"—"}</p>}
          >
            {(totals) => (
              <>
                <div class="flex justify-between text-sm text-ink-muted">
                  <span>{t("order.subtotal")}</span>
                  <span class="tabular-nums">{formatMoney(totals().subtotal)}</span>
                </div>
                <div class="flex justify-between text-sm text-ink-muted">
                  <span>{t("order.tax")}</span>
                  <span class="tabular-nums">{formatMoney(totals().tax_total)}</span>
                </div>
                <div class="mt-1 flex items-baseline justify-between">
                  <span class="text-sm font-semibold">{t("order.total")}</span>
                  <span class="text-2xl font-semibold tabular-nums">
                    {formatMoney(totals().total_due)}
                  </span>
                </div>
              </>
            )}
          </Show>
        </div>

        {/*
          The two acts that end this screen, anchored to the bottom of a phone.

          `docs/ui-ux.md` §1 principle 9 asks for exactly this — *"Phone: single column, primary
          action anchored at the bottom within thumb reach"* — and until now they simply sat after
          the check total, which on a handheld puts them below however many lines the table has
          ordered. A server taking a large table's order had to scroll to send it.

          Sticky rather than fixed: fixed would take the buttons out of the flow and float them over
          the last line of the order, and the line under your thumb is the one you were reading. On
          a tablet and a terminal the whole column fits, so the anchor is released and they sit
          where they always did — which is why this is `tablet:static` rather than a media query
          asking the phone for something special.
        */}
        <div class="sticky bottom-0 -mx-4 mt-4 border-t border-line bg-canvas px-4 pb-4 pt-3 tablet:static tablet:mx-0 tablet:border-0 tablet:bg-transparent tablet:p-0">
          <button
            type="button"
            class="min-h-money w-full rounded-token bg-primary px-4 text-lg font-semibold text-primary-ink disabled:opacity-50"
            disabled={unfired().length === 0}
            data-step="fireOrder"
            onClick={() => void guard(() => fireOrder(params.id))}
          >
            {unfired().length === 0
              ? t("order.send")
              : t("order.send_count", { count: unfired().length })}
          </button>

          <button
            type="button"
            class="mt-3 min-h-money w-full rounded-token border border-primary px-4 text-lg font-semibold text-ink"
            data-step="takePayment"
            onClick={() => void takePayment()}
          >
            {t("order.take_payment")}
          </button>
        </div>
      </div>

      <aside>
        <h2 class="mb-2 text-sm font-semibold text-ink-muted">{t("order.menu")}</h2>
        {/*
          The box that makes a long menu usable. `type="search"` rather than `text` so the browser
          gives the operator its own clear affordance — one control fewer to draw, and the one every
          other search box on their phone already has.

          No `data-step`: typing is not a tap (`docs/ui-ux.md` §6), which is exactly why this is
          worth having — it finds an item in a two-hundred-line book without costing the flow the
          tap the grid costs. The id is how the browser gate reaches it in a precondition, the same
          way it reaches the shift float.
        */}
        <label class="mb-3 block">
          <span class="sr-only">{t("order.search")}</span>
          <input
            id="menu-search"
            type="search"
            class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-ink"
            placeholder={t("order.search")}
            value={query()}
            onInput={(event) => setQuery(event.currentTarget.value)}
          />
        </label>

        <Show when={!searching()} fallback={searchResults()}>
          {/*
            Two ways to draw the same price book. When the console has arranged buttons on the
            `layout` node (ADR-0066, C4) the till groups by the categories it authored, in the order
            it authored them; when it has arranged nothing, the flat list is the honest fallback and
            is what the till drew before that node had a reader.
          */}
          <Show
            when={arranged().length > 0}
            fallback={
              <div class="grid grid-cols-2 gap-2 terminal:grid-cols-1">
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
                  <div class="grid grid-cols-2 gap-2 terminal:grid-cols-1">
                    <For each={category.buttons}>{(button) => arrangedButton(button)}</For>
                  </div>
                  <For each={category.subcategories}>
                    {(subcategory) => (
                      <div class="mt-3">
                        <h4 class="mb-2 text-xs text-ink-muted">{subcategory.name}</h4>
                        <div class="grid grid-cols-2 gap-2 terminal:grid-cols-1">
                          <For each={subcategory.buttons}>{(button) => arrangedButton(button)}</For>
                        </div>
                      </div>
                    )}
                  </For>
                </section>
              )}
            </For>
          </Show>
        </Show>
      </aside>
    </section>
  );
}
