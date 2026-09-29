import { For, Show, createMemo, createResource, createSignal } from "solid-js";
import { useLocation, useNavigate, useParams, useSearchParams } from "@solidjs/router";

import { ApproverFields } from "../components/ApproverFields";
import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { formatQuantity } from "../lib/money";
import { fold, matches } from "../lib/search";
import type { LayoutButton, LayoutCategory, MenuItemResponse, ModifierGroup } from "../api/types";
import {
  addItem,
  chooseSeat,
  groupsFor,
  holdWalkIn,
  modifiersSatisfied,
  fireCourse,
  fireOrder,
  floorTables,
  linesForTable,
  setQuantity,
  loadCheck,
  modifierNames,
  moveTable,
  openBill,
  openBillFor,
  reasonsFor,
  seatFor,
  seatsEnabled,
  setItemSoldOut,
  state,
  tableLabel,
  tableState,
  unfiredLinesForTable,
  unsentCoursesForTable,
  voidLine,
  walkInKey,
  type OrderLine,
  formatAmount,
} from "../state/store";
import { errorMessage } from "../lib/errors";

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
  const location = useLocation();
  const [error, setError] = createSignal<string | null>(null);
  // A walk-in order the counter started (ADR-0146), at `/order/:id`, rather than a table's. It sits
  // on no table, so it is held under a key of its own and every table-keyed read below works on it
  // unchanged; only the header, the way back and the way to pay differ.
  const walkIn = () => location.pathname.startsWith("/order/");
  const key = () => (walkIn() ? walkInKey(params.id) : params.id);
  if (walkIn()) {
    holdWalkIn(params.id);
  }
  // The table's published label — the number the floor, the kitchen board and the pass all show
  // (F6). Cutting leading zeros off the id is what this used to do, which was right for the
  // bootstrap floor's `01`…`12` and wrong for every published table: a ULID lost its zeros and the
  // header read "Table 69" over Table 1. Only a table the floor does not know yet falls back to it.
  const labelOf = (tableId: string) => {
    const published = tableLabel(tableId);
    return published !== tableId ? published : tableId.replace(/^0+/, "") || tableId;
  };
  const label = () => labelOf(params.id);

  const guard = async (run: () => Promise<void>) => {
    setError(null);
    try {
      await run();
    } catch (caught) {
      setError(errorMessage(caught));
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
    linesForTable(key())
      .map((line) => `${line.orderLineId}:${line.state}:${line.quantityMilli}`)
      .join(","),
  );
  const [check] = createResource(priceable, () => loadCheck(key()));

  // The lines the kitchen has not been told about. The count rides on the Send button, because the
  // question an operator asks before pressing it is "what is about to go" — and the answer used to
  // be a row count they made themselves (`docs/ui-ux.md` §3).
  // Wrapped in `createMemo` to avoid re-filtering `linesForTable(key())` on every access or re-render.
  const unfired = createMemo(() => unfiredLinesForTable(key()));

  // Pre-aggregate waiting line counts per course in a single O(N) pass to avoid O(C * N) filtering
  // during rendering in `<For each={unsentCourses()}>`.
  const courseWaitingCounts = createMemo(() => {
    const counts = new Map<string, number>();
    for (const line of unfired()) {
      if (line.courseId) {
        counts.set(line.courseId, (counts.get(line.courseId) ?? 0) + 1);
      }
    }
    return counts;
  });

  // The courses with food still waiting, in the store's service order. Empty on a store with courses
  // off, which is what keeps the row off those tills entirely rather than drawing an empty heading.
  const unsentCourses = () => unsentCoursesForTable(key());

  // How many the store said this table seats, from the published floor plan. Zero means the plan
  // recorded no capacity, and a picker with no seats in it is worse than none — so the control only
  // appears where the store both does seats and said how many this table has.
  const seatCount = () => floorTables().find((table) => table.id === params.id)?.seats ?? 0;
  const showSeats = () => seatsEnabled() && seatCount() > 0;

  // Moving the guests to another table: whether the list of free tables is open, and a refusal,
  // shown in that list rather than under a bill it may be drawn over. The table they came from rides
  // on the new table's address, so the screen they land on can say what just happened.
  const [moving, setMoving] = createSignal(false);
  const [moveError, setMoveError] = createSignal<string | null>(null);
  const [search] = useSearchParams<{ moved_from?: string }>();
  // Guests move before the bill only: once they have asked for it they pay where they sit, and the
  // edge refuses the move (`docs/pos-spec.md` §2).
  const movable = () => !walkIn() && tableState(params.id) === "TABLE_STATE_OCCUPIED";
  // Whether a bill is open on this table. A bill names the dishes it covers when it opens, so one
  // rung after it would be on no bill and leave with the table unpaid; the edge refuses it
  // (`BILL_ALREADY_OPEN`), and the menu says so before the tap rather than after it.
  const billed = () => openBillFor(key()) !== undefined;
  // Every free table on the published floor, in the floor's own order.
  const freeTables = createMemo(() =>
    floorTables().filter(
      (table) => table.id !== params.id && tableState(table.id) === "TABLE_STATE_FREE",
    ),
  );
  const openMove = () => {
    setMoveError(null);
    setMoving(true);
  };
  const moveTo = async (to: string) => {
    const from = params.id;
    setMoveError(null);
    try {
      await moveTable(from, to);
    } catch (caught) {
      setMoveError(errorMessage(caught));
      return;
    }
    setMoving(false);
    navigate(`/table/${to}?moved_from=${from}`);
  };

  // A walk-in is paid on the counter screen, whose pad charges any counter order; it opens this
  // order's bill there, so a bill is never left open on an order nobody went on to pay.
  const takePayment = () =>
    guard(async () => {
      if (walkIn()) {
        navigate(`/counter?charge=${params.id}`);
        return;
      }
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

  // Whether a tablet shows the whole bill or only the bar under the menu. `docs/ui-ux.md` §1
  // principle 9 asks a tablet for "large item grid, bill slides up", so the menu fills the screen and
  // the bill waits at the bottom until it is tapped. Only a tablet reads this: a phone and a terminal
  // always show the whole bill, and every class it drives is undone on the terminal.
  const [billOpen, setBillOpen] = createSignal(false);
  // What the bar says of the bill: the dishes on it, not counting the ones voided off it, and what
  // it comes to. The total is read in the markup, not handed back from a `<Show>` child, which runs
  // untracked and would print the first figure it saw for good. A dash until the edge has priced it.
  const billCount = createMemo(() => linesForTable(key()).filter((line) => !voided(line)).length);
  const billTotal = () => {
    const totals = check();
    return totals ? formatAmount(totals.total_due) : "—";
  };

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

  // The flat fallback's items: the price book without the ones that are only ever a choice inside a
  // modifier group (F11). With no layout published, "Size — 30cm" and "Extra cheese" drew as buttons
  // of their own beside the pizzas, and a topping could be sold alone with nothing to go on. Search
  // still finds them — an operator who types a name gets what the price book holds.
  const headlineItems = createMemo(() => {
    const choices = new Set(state.modifierGroups.flatMap((group) => group.member_menu_item_ids));
    return state.menu.filter((item) => !choices.has(item.menu_item_id));
  });

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

  // Which category the grid shows, when the console arranged more than one (F15). A large store's
  // plan drew every category at once — 17 categories and 386 items in one column 23,000px tall and
  // ten thousand DOM nodes — so a new server had to scroll a long way or know the name to type. One
  // category at a time, picked from a row of tabs, is what a cashier scans; the first is shown until
  // another is picked, and a pick that no longer exists (the plan was republished) falls back to it.
  const [pickedCategory, setPickedCategory] = createSignal<string | null>(null);
  const shownCategories = createMemo(() => {
    const all = arranged();
    if (all.length <= 1) {
      return all;
    }
    const picked = all.find((category) => category.display_category_id === pickedCategory());
    return [picked ?? all[0]].filter((category) => category !== undefined);
  });
  const isShown = (category: LayoutCategory) =>
    shownCategories().some((shown) => shown.display_category_id === category.display_category_id);

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
  //
  // Folded here rather than at the comparison, so a caption is normalized when the menu changes
  // instead of once per item per keystroke. Deduplicated on the folded form too: two buttons whose
  // captions differ only in case or tone marks are one string to search, and keeping both would
  // scan the same text twice.
  const captions = createMemo(() => {
    const byItem = new Map<string, string[]>(
      state.menu.map((item) => [item.menu_item_id, [fold(item.display_name)]]),
    );
    const record = (button: LayoutButton) => {
      const known = byItem.get(button.menu_item_id);
      const folded = fold(button.label);
      if (known !== undefined && !known.includes(folded)) {
        known.push(folded);
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
  //
  // The needle is its own memo for the same reason the captions are: folded once per keystroke
  // rather than once per item scanned.
  const needle = createMemo(() => fold(query().trim()));
  const results = createMemo(() =>
    searching()
      ? state.menu.filter((item) =>
          matches(needle(), captions().get(item.menu_item_id) ?? [fold(item.display_name)]),
        )
      : [],
  );

  // Tapping an item: sell it, or ask first. The question is the store's, not this screen's — an
  // item attaches groups or it does not, and the till has no opinion beyond obeying that.
  const onItem = (item: MenuItemResponse) => {
    if (groupsFor(item).length === 0) {
      void guard(() => addItem(key(), item));
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
      await addItem(key(), item, chosen());
      closeChoosing();
    });

  // Marking items sold out (86): while it is on, a tap on an item marks it sold out on every till,
  // or brings it back, rather than selling it. A mode rather than a control on every button, so a
  // tap meant to sell during a rush can never take a dish off the menu by accident.
  const [marking, setMarking] = createSignal(false);
  const startMarking = () => {
    setError(null);
    setMarking(true);
  };
  const toggleSoldOut = (item: MenuItemResponse) =>
    guard(() => setItemSoldOut(item.menu_item_id, item.sold_out !== true));

  // What an item's button says on its right: the price, or why the dish will not sell. A sold-out
  // item says so in words and not only by the strike-through, so a till read at arm's length still
  // says why.
  const itemState = (item: MenuItemResponse) => (
    <Show
      when={item.sold_out === true}
      fallback={
        <span class="tabular-nums text-ink-muted">
          {item.available ? formatAmount(item.unit_price) : t("order.unavailable")}
        </span>
      }
    >
      <span class="text-sm text-danger" data-outcome="item-sold-out">
        {t("order.sold_out")}
      </span>
    </Show>
  );

  const saleButton = (item: MenuItemResponse, caption: string) => (
    <button
      type="button"
      class="flex min-h-touch items-center justify-between rounded-token border border-line bg-surface px-3 py-2 text-left disabled:opacity-50"
      disabled={!item.available || billed()}
      data-step="onItem"
      onClick={() => onItem(item)}
    >
      <span classList={{ "line-through": item.sold_out === true }}>{caption}</span>
      {itemState(item)}
    </button>
  );

  const markButton = (item: MenuItemResponse, caption: string) => (
    <button
      type="button"
      class="flex min-h-touch items-center justify-between rounded-token border bg-surface px-3 py-2 text-left disabled:opacity-50"
      classList={{
        "border-line": item.sold_out !== true,
        "border-2 border-danger": item.sold_out === true,
      }}
      // An item the console withdrew, or cannot price, is not the till's to bring back.
      disabled={!item.available && item.sold_out !== true}
      aria-pressed={item.sold_out === true}
      data-step="toggleSoldOut"
      onClick={() => void toggleSoldOut(item)}
    >
      <span classList={{ "line-through": item.sold_out === true }}>{caption}</span>
      {itemState(item)}
    </button>
  );

  // Which of the two an item draws. A `<Show>` rather than a ternary: the buttons are drawn inside
  // `<For>` and `<Show>`, whose children run untracked, so a ternary would be read once and a button
  // already on screen would never change when the mode does.
  const sellButton = (item: MenuItemResponse, caption: string) => (
    <Show when={marking()} fallback={saleButton(item, caption)}>
      {markButton(item, caption)}
    </Show>
  );

  // What the aside draws while a query is in the box: the matches, or the sentence saying there are
  // none. The empty state names what was searched for, because "nothing found" and "nothing found
  // *for this*" are different amounts of help when the answer is a typo.
  const searchResults = () => (
    <div
      class="grid grid-cols-2 gap-2 tablet:grid-cols-3 terminal:grid-cols-1"
      data-outcome="menu-results"
    >
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
      {/* `min-w-0` on both grid items, or neither can shrink below its content and the column runs
          wider than a phone: a grid item's `min-width` is `auto`, not `0`.

          On a tablet this column's own box is dissolved (`contents`): its header becomes a row of
          the screen above the menu, and its body becomes the panel at the bottom. */}
      <div class="min-w-0 tablet:contents terminal:block">
        {/* Wraps rather than squeezes: on a narrow phone, in a longer language, the move button goes
            to a line of its own instead of breaking every word in the row onto two. */}
        <div class="mb-3 flex flex-wrap items-center gap-3">
          <a
            href={walkIn() ? "/counter" : "/"}
            class="inline-flex min-h-touch items-center text-sm text-ink-muted no-underline"
          >
            {walkIn() ? t("common.back_counter") : t("common.back_floor")}
          </a>
          {/* Seating a table, or starting a walk-in, ends here (ADR-0109, ADR-0146). */}
          <h1 class="text-lg font-semibold" data-outcome="order-open">
            {walkIn() ? t("order.walk_in") : t("common.table", { label: label() })}
          </h1>
          <Show when={!walkIn()}>
            <span class="text-sm text-ink-muted">{t(tableStateKey(tableState(params.id)))}</span>
          </Show>
          <Show when={movable()}>
            <button
              type="button"
              class="ml-auto min-h-touch whitespace-nowrap rounded-token border border-line px-3 text-sm text-ink-muted"
              aria-expanded={moving()}
              data-step="openMove"
              onClick={() => openMove()}
            >
              {t("order.move_table")}
            </button>
          </Show>
        </div>

        {/* The guests' new table says where they came from, once, on the screen they land on. */}
        <Show when={!walkIn() && search.moved_from}>
          {(from) => (
            <p class="mb-3 text-sm text-ok" role="status" data-outcome="table-moved">
              {t("order.moved_from", { label: labelOf(from()) })}
            </p>
          )}
        </Show>

        {/* The free tables the guests can move to. A tap moves them, with their order, and opens
            the new table; the table they leave waits to be cleared. */}
        <Show when={moving() && movable()}>
          <div class="mb-3 rounded-token border border-line bg-surface p-3">
            <div class="mb-1 flex items-center justify-between gap-2">
              <h2 class="font-semibold">{t("order.move_title", { label: label() })}</h2>
              <button
                type="button"
                class="min-h-touch rounded-token border border-line px-3 text-sm"
                onClick={() => setMoving(false)}
              >
                {t("common.cancel")}
              </button>
            </div>
            <p class="mb-2 text-sm text-ink-muted">{t("order.move_hint")}</p>
            <Show when={moveError()}>
              {(message) => (
                <p class="mb-2 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                  {message()}
                </p>
              )}
            </Show>
            <Show
              when={freeTables().length > 0}
              fallback={<p class="text-sm text-ink-muted">{t("order.move_none_free")}</p>}
            >
              <div class="grid grid-cols-3 gap-2 tablet:grid-cols-6 terminal:grid-cols-4">
                <For each={freeTables()}>
                  {(table) => (
                    <button
                      type="button"
                      class="min-h-touch rounded-token border border-line px-2 py-2 font-semibold"
                      data-step="moveTo"
                      onClick={() => void moveTo(table.id)}
                    >
                      {t("common.table", { label: table.label })}
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </div>
        </Show>

        {/*
          The bill. A phone and a terminal draw it in the column, as they always did. A tablet slides
          it up from the bottom of the screen: closed, it shows the bar saying what is on it, the
          seat row, and the two buttons that end the screen, so sending and paying stay one tap
          away; open, it is the whole bill, over the menu, with the bar at its top to close it.
        */}
        <div class="tablet:fixed tablet:inset-x-0 tablet:bottom-0 tablet:z-10 tablet:max-h-[85dvh] tablet:overflow-y-auto tablet:rounded-t-token tablet:border-t tablet:border-line tablet:bg-canvas tablet:px-4 tablet:pb-4 tablet:shadow-overlay terminal:static terminal:z-auto terminal:max-h-none terminal:overflow-visible terminal:rounded-none terminal:border-0 terminal:bg-transparent terminal:p-0 terminal:shadow-none">
          <button
            type="button"
            class="mb-3 hidden min-h-touch w-full items-center justify-between gap-3 bg-canvas py-2 text-left tablet:sticky tablet:top-0 tablet:flex terminal:hidden"
            aria-expanded={billOpen()}
            data-outcome="bill-bar"
            onClick={() => setBillOpen((open) => !open)}
          >
            {/* One string, the total inside it: the bar is on the page on every device, hidden
                off a tablet, and a figure of its own would be a second copy of the bill's total. */}
            <span class="text-lg font-semibold tabular-nums">
              {t("order.bill_summary", { count: billCount(), total: billTotal() })}
            </span>
            <span aria-hidden="true">{billOpen() ? "▾" : "▴"}</span>
          </button>

          {/* A refusal, directly under the table's header on a phone and a terminal, where the bar
              above it is not drawn. A tablet draws it under the bar, so it is seen whether the bill
              is open or closed: the open bill covers the top of a tablet's screen. While a dish's
              choices are open it shows in the picker instead, beside the button that drew it. */}
          <Show when={choosing() ? null : error()}>
            {(message) => (
              <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                {message()}
              </p>
            )}
          </Show>
          <ul class="flex flex-col gap-2" classList={{ "tablet:hidden terminal:flex": !billOpen() }}>
            <For
              each={linesForTable(key())}
              fallback={<li class="text-ink-muted">{t("order.empty")}</li>}
            >
              {(line) => (
                <li
                  class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-token border border-line bg-surface p-3 tablet:flex-nowrap"
                  data-outcome="line-added"
                >
                  {/* A voided line stays on the order, struck through. Removing the row would make a
                      mis-tapped void invisible to the person who made it — the guest is not charged
                      either way, and seeing what was cancelled is how the mistake gets noticed. */}
                  {/* The name takes the whole first row on a phone and the controls wrap under it
                      (F7): squeezed into one row at 390px the name broke a word per line, the seat
                      chip sat on top of it and the stepper ran off the right edge. */}
                  <span class="flex min-w-0 basis-full flex-col tablet:basis-auto tablet:flex-1">
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
                  {/* How many, and the two taps that change it. Only while the line is still
                      editable: once the kitchen has the ticket the number is settled, and the edge
                      refuses an amend from its state machine rather than from a permission.

                      Minus stops at one rather than reaching zero. Zero is not a smaller order, it is
                      no order — that is a void, which carries a reason and, after a fire, a manager.
                      Letting the stepper walk into it would give that act a second, quieter spelling. */}
                  <Show when={line.state === "ORDER_LINE_STATE_ADDED"}>
                    <span class="inline-flex items-center gap-1">
                      <button
                        type="button"
                        class="min-h-touch w-11 rounded-token border border-line text-lg text-ink disabled:opacity-40"
                        disabled={line.quantityMilli <= 1000}
                        aria-label={t("order.fewer", { item: line.name })}
                        onClick={() =>
                          void guard(() => setQuantity(line.orderLineId, line.quantityMilli - 1000))
                        }
                      >
                        {"−"}
                      </button>
                      <span class="w-8 text-center tabular-nums" data-outcome="line-quantity">
                        {formatQuantity(line.quantityMilli)}
                      </span>
                      <button
                        type="button"
                        class="min-h-touch w-11 rounded-token border border-line text-lg text-ink"
                        aria-label={t("order.more", { item: line.name })}
                        data-step="setQuantity"
                        onClick={() =>
                          void guard(() => setQuantity(line.orderLineId, line.quantityMilli + 1000))
                        }
                      >
                        {"+"}
                      </button>
                    </span>
                  </Show>
                  {/* A line already with the kitchen shows its count without the controls. */}
                  <Show when={line.state !== "ORDER_LINE_STATE_ADDED" && line.quantityMilli !== 1000}>
                    <span class="tabular-nums text-ink-muted">
                      {formatQuantity(line.quantityMilli)}
                    </span>
                  </Show>
                  {/* Which seat it was ordered for, on the line that carries it. A line with none is
                      the table's, and says nothing rather than saying "no seat". */}
                  <Show when={line.seat !== undefined}>
                    <span class="rounded-token bg-surface-muted px-2 text-sm tabular-nums text-ink-muted" data-outcome="line-seat">
                      {t("order.seat_short", { seat: line.seat ?? 0 })}
                    </span>
                  </Show>
                  <span class="tabular-nums" classList={{ "text-ink-muted": voided(line) }}>
                    {formatAmount(line.lineTotal)}
                  </span>
                  {/* A line waiting to be sent says so and nothing more: the Send button below acts on
                      every one of them at once, so a control per row would be one tap out of six
                      doing what one tap now does for all. */}
                  <Show when={line.state === "ORDER_LINE_STATE_ADDED"}>
                    {/* Marked, because "still waiting" is a state a gate has to be able to see. The
                        row's own `line-added` marks every line whatever its state, so it cannot say
                        which of them the kitchen has not been told about — and a fire-by-course that
                        sent the whole order would look identical through it. */}
                    <span class="text-sm text-ink-muted" data-outcome="line-unsent">
                      {t("order.unsent")}
                    </span>
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
                      class="min-h-touch rounded-token border border-line px-3 text-sm text-ink-muted disabled:opacity-50"
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
                  <ApproverFields
                    id="void-line-approver"
                    code={approverCode()}
                    pin={approverPin()}
                    onCode={setApproverCode}
                    onPin={setApproverPin}
                    codeLabel={t("order.approver_code")}
                    pinLabel={t("order.approver_pin")}
                  />
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
              <>
                {/*
                  Where the picker opens depends on where the menu is. On a phone or a tablet the menu
                  is below the bill, and the picker, drawn in the bill's column, opened off-screen: a
                  server tapped the pizza and saw nothing happen. There it is a sheet from the bottom
                  of the screen, where the thumb that tapped is, over the menu washed out behind it,
                  and a tap on the washed-out menu cancels, as Cancel does. A terminal draws the bill
                  beside the menu, so there the picker stays in the bill's column, where it always was.
                */}
                <div
                  class="fixed inset-0 z-20 bg-canvas/70 terminal:hidden"
                  aria-hidden="true"
                  onClick={() => closeChoosing()}
                />
                <div
                  class="fixed inset-x-0 bottom-0 z-30 flex max-h-[85dvh] flex-col rounded-t-token border-t border-line bg-surface shadow-overlay terminal:static terminal:z-auto terminal:mb-3 terminal:block terminal:max-h-none terminal:rounded-token terminal:border terminal:shadow-none"
                  role="dialog"
                  aria-label={t("order.choose_title", { item: item().display_name })}
                >
                  {/* The choices scroll inside the sheet and the two buttons under them do not, so a
                      phone on its side, a screen shorter than the choices, still shows Add. */}
                  <div class="min-h-0 overflow-y-auto p-4 pb-0 terminal:overflow-visible terminal:p-3 terminal:pb-0">
                    <h2 class="font-semibold">
                      {t("order.choose_title", { item: item().display_name })}
                    </h2>
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
                                      class="flex min-h-touch items-center justify-between rounded-token border border-line px-3 text-left disabled:opacity-50"
                                      classList={{
                                        "bg-primary text-primary-ink": chosen().includes(memberId),
                                        "bg-surface text-ink": !chosen().includes(memberId),
                                      }}
                                      // A choice the kitchen has run out of, or the console withdrew,
                                      // cannot be made: the edge refuses a line that asks for it. One
                                      // already chosen stays tappable, so it can still be taken off.
                                      disabled={!member().available && !chosen().includes(memberId)}
                                      aria-pressed={chosen().includes(memberId)}
                                      data-step="chooseModifier"
                                      onClick={() => chooseModifier(group, memberId)}
                                    >
                                      <span classList={{ "line-through": member().sold_out === true }}>
                                        {member().display_name}
                                      </span>
                                      <Show
                                        when={member().available}
                                        fallback={
                                          <span class="text-sm text-ink-muted">
                                            {member().sold_out === true
                                              ? t("order.sold_out")
                                              : t("order.unavailable")}
                                          </span>
                                        }
                                      >
                                        {/* A modifier is an ordinary item with its own price, which is
                                            how a large costs more than a small. A free choice says
                                            nothing rather than saying zero. */}
                                        <Show when={member().unit_price.amount_minor > 0}>
                                          <span class="tabular-nums text-ink-muted">
                                            {"+ "}
                                            {formatAmount(member().unit_price)}
                                          </span>
                                        </Show>
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
                    <Show when={error()}>
                      {(message) => (
                        <p
                          class="mt-3 rounded-token border border-danger px-3 py-2 text-danger"
                          role="alert"
                        >
                          {message()}
                        </p>
                      )}
                    </Show>
                  </div>
                  <div class="p-4 pt-0 terminal:p-3 terminal:pt-0">
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
                </div>
              </>
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
          <div
            class="mt-4 rounded-token border border-line bg-surface p-3"
            classList={{ "tablet:hidden terminal:block": !billOpen() }}
            data-outcome="check-total"
          >
            <Show
              when={check()}
              fallback={<p class="text-right text-2xl font-semibold tabular-nums">{"—"}</p>}
            >
              {(totals) => (
                <>
                  <div class="flex justify-between text-sm text-ink-muted">
                    <span>{t("order.subtotal")}</span>
                    <span class="tabular-nums">{formatAmount(totals().subtotal)}</span>
                  </div>
                  <div class="flex justify-between text-sm text-ink-muted">
                    <span>{t("order.tax")}</span>
                    <span class="tabular-nums">{formatAmount(totals().tax_total)}</span>
                  </div>
                  <div class="mt-1 flex items-baseline justify-between">
                    <span class="text-sm font-semibold">{t("order.total")}</span>
                    <span class="text-2xl font-semibold tabular-nums">
                      {formatAmount(totals().total_due)}
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
          <div class="sticky bottom-0 -mx-4 mt-4 border-t border-line bg-canvas px-4 pb-4 pt-3 tablet:static tablet:mx-0 tablet:grid tablet:grid-cols-2 tablet:gap-3 tablet:border-0 tablet:bg-transparent tablet:p-0 terminal:block">
            {/*
              Send one course at a time, above the button that sends everything (ADR-0130).

              Only the courses that still have food waiting: a "Send desserts" button on a table whose
              desserts have already gone is a control that does nothing, and an operator who presses a
              few of those stops trusting the row. Drawn in the store's published service order and
              never re-sorted here — the sequence is the whole meaning of a course, and a screen that
              rebuilt it from what it happened to be showing is how one till ends up disagreeing with
              another about what comes first.

              Absent entirely on a store with courses off, because the edge refuses a fire-by-course
              there: offering it would be an act the store will not honour, which is the rule
              `tips_enabled` and `seats_enabled` already follow.
            */}
            <Show when={unsentCourses().length > 0}>
              <div
                class="mb-3 tablet:col-span-2"
                classList={{ "tablet:hidden terminal:block": !billOpen() }}
              >
                <h3 class="mb-2 text-sm font-semibold text-ink-muted">{t("order.courses")}</h3>
                <div class="flex flex-wrap gap-2">
                  <For each={unsentCourses()}>
                    {(course) => {
                      const waiting = () => courseWaitingCounts().get(course.course_id) ?? 0;
                      return (
                        <button
                          type="button"
                          class="min-h-touch flex-1 rounded-token border border-primary px-3 text-base font-semibold text-ink"
                          data-step="fireCourse"
                          onClick={() => void guard(() => fireCourse(key(), course.course_id))}
                        >
                          {t("order.send_course_count", {
                            course: course.display_name,
                            count: waiting(),
                          })}
                        </button>
                      );
                    }}
                  </For>
                </div>
              </div>
            </Show>

            <button
              type="button"
              class="min-h-money w-full rounded-token bg-primary px-4 text-lg font-semibold text-primary-ink disabled:opacity-50"
              disabled={unfired().length === 0}
              data-step="fireOrder"
              onClick={() => void guard(() => fireOrder(key()))}
            >
              {unfired().length === 0
                ? t("order.send")
                : t("order.send_count", { count: unfired().length })}
            </button>

            <button
              type="button"
              class="mt-3 min-h-money w-full rounded-token border border-primary px-4 text-lg font-semibold text-ink tablet:mt-0 terminal:mt-3"
              data-step="takePayment"
              onClick={() => void takePayment()}
            >
              {t("order.take_payment")}
            </button>
          </div>
        </div>
      </div>

      {/* On a tablet the menu is the screen, with room at its foot for the closed panel. */}
      <aside class="min-w-0 tablet:pb-56 terminal:pb-0">
        <div class="mb-2 flex items-center justify-between gap-2">
          <h2 class="text-sm font-semibold text-ink-muted">{t("order.menu")}</h2>
          <Show
            when={marking()}
            fallback={
              <button
                type="button"
                class="min-h-touch rounded-token border border-line px-3 text-sm text-ink-muted"
                data-step="startMarking"
                onClick={() => startMarking()}
              >
                {t("order.mark_sold_out")}
              </button>
            }
          >
            <button
              type="button"
              class="min-h-touch rounded-token border border-line px-3 text-sm font-semibold"
              onClick={() => setMarking(false)}
            >
              {t("order.mark_sold_out_done")}
            </button>
          </Show>
        </div>
        <Show when={marking()}>
          <p class="mb-3 text-sm text-ink-muted" role="status">
            {t("order.mark_sold_out_hint")}
          </p>
        </Show>
        {/* The bill is open, so the menu sells nothing: said here, where the tap would have gone,
            with the way to order more. Marking a dish sold out has nothing to do with the bill and
            stays available. */}
        <Show when={billed() && !marking()}>
          <p
            class="mb-3 rounded-token border border-line bg-surface px-3 py-2 text-sm"
            role="status"
            data-outcome="bill-open-locked"
          >
            {t("order.bill_open_locked")}
          </p>
        </Show>
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
              <div class="grid grid-cols-2 gap-2 tablet:grid-cols-3 terminal:grid-cols-1">
                <For
                  each={headlineItems()}
                  fallback={<p class="text-ink-muted">{t("order.menu_empty")}</p>}
                >
                  {(item) => sellButton(item, item.display_name)}
                </For>
              </div>
            }
          >
            <Show when={arranged().length > 1}>
              <div
                class="-mx-1 mb-3 flex gap-2 overflow-x-auto px-1 pb-1"
                role="tablist"
                aria-label={t("order.categories")}
              >
                <For each={arranged()}>
                  {(category) => (
                    <button
                      type="button"
                      role="tab"
                      aria-selected={isShown(category)}
                      class="min-h-touch shrink-0 rounded-token border px-3 text-sm"
                      classList={{
                        "border-primary bg-selected text-selected-ink font-semibold": isShown(category),
                        "border-line bg-surface text-ink": !isShown(category),
                      }}
                      onClick={() => setPickedCategory(category.display_category_id)}
                    >
                      {category.name}
                    </button>
                  )}
                </For>
              </div>
            </Show>
            <For each={shownCategories()}>
              {(category) => (
                <section class="mb-4">
                  <h3 class="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-muted">
                    {category.name}
                  </h3>
                  <div class="grid grid-cols-2 gap-2 tablet:grid-cols-3 terminal:grid-cols-1">
                    <For each={category.buttons}>{(button) => arrangedButton(button)}</For>
                  </div>
                  <For each={category.subcategories}>
                    {(subcategory) => (
                      <div class="mt-3">
                        <h4 class="mb-2 text-xs text-ink-muted">{subcategory.name}</h4>
                        <div class="grid grid-cols-2 gap-2 tablet:grid-cols-3 terminal:grid-cols-1">
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
