import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { useSearchParams } from "@solidjs/router";

import type { MenuItemResponse } from "../api/types";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { chime, chimeReady, unlockChime } from "../lib/chime";
import { errorMessage } from "../lib/errors";
import { formatQuantity } from "../lib/money";
import { useDarkTakeover } from "../lib/screen";
import { fold, matches } from "../lib/search";
import {
  bump,
  kitchenTickets,
  loadLiveOrders,
  modifierNames,
  queueNumberFor,
  refreshQueueNumbers,
  setItemSoldOut,
  state,
  type KitchenTicket,
} from "../state/store";

// The kitchen display: fired lines gathered into tickets — one per order, station and course, the
// unit a cook makes together — large, on a dark panel, oldest first. Bumping a ticket records the
// durable `kitchen.ticket.bumped` event (#44) and fans it out, so a second KDS agrees the ticket is
// done rather than holding a private, divergent "done" flag — and a screen coming online after the
// bump reads the same prepared set. The bumped ticket drops off this screen at once; the store folds
// the returning event so it stays off.
//
// # Why the board keys its cards by string
//
// The board used to be one card per line, built from a fresh array of fresh objects on every change
// to any line in the store. `<For>` keys by identity, so every card was torn down and rebuilt each
// time: 600 waiting lines was 18,000 DOM nodes redrawn per bump, a quarter of a second on the box
// and a full second on a cheap tablet (finding F3) — and a cook's tap could land on a card in the
// middle of being replaced. Cards are keyed by the ticket's stable key now, and each one reads its
// lines out of the store, so a change updates the card it touches and nothing else.

/// How long a ticket may wait before the board says so, in seconds.
///
/// A default, and the wrong *kind* of value to hold here: how long is too long is a fact about a
/// kitchen, not about this screen, so it belongs in the store's configuration pack with the rest of
/// what a store sets (`AGENTS.md` §1). It is a constant until that field exists, rather than a
/// number buried in a class list.
const LATE_AFTER_SECONDS = 600;

/// The board's clock, ticking once a second.
///
/// One timer for the whole screen rather than one per ticket: the tickets all age together, and a
/// board with twenty timers redraws twenty times a second for no more information.
function useNow(): () => number {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(timer));
  return now;
}

/// Seconds between a fired time and now, or `null` for a line the edge did not date.
function waitedSeconds(firedTime: string | undefined, now: number): number | null {
  if (firedTime === undefined) {
    return null;
  }
  const fired = Date.parse(firedTime);
  if (Number.isNaN(fired)) {
    return null;
  }
  // A clock that disagrees with the edge's can read a fresh ticket as fired in the future. Clamp at
  // zero rather than draw "-0:03", which says nothing true and looks like a bug.
  return Math.max(0, Math.round((now - fired) / 1000));
}

/// `M:SS`, and `H:MM:SS` once a ticket has been waiting an hour — which should never happen, and is
/// exactly why it must not silently wrap back to `0:12`.
function formatWaited(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  const rest = String(seconds % 60).padStart(2, "0");
  if (minutes < 60) {
    return `${minutes}:${rest}`;
  }
  const hours = Math.floor(minutes / 60);
  return `${hours}:${String(minutes % 60).padStart(2, "0")}:${rest}`;
}

/// Two key lists are the same board.
function sameKeys(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((key, index) => key === b[index]);
}

// Whether this board rings when new food arrives, kept per device across reloads: the board over the
// grill may want it and the one beside the pass may not. Off until a cook turns it on, because a
// browser will not play sound until somebody has touched the page anyway (`lib/chime.ts`). Wrapped
// for the reason the language choice is: storage can be unavailable, and a chime is not worth
// failing a board over.
const SOUND = "pos.kds.sound";

function soundChosen(): boolean {
  try {
    return globalThis.localStorage?.getItem(SOUND) === "on";
  } catch {
    return false;
  }
}

function rememberSound(on: boolean): void {
  try {
    globalThis.localStorage?.setItem(SOUND, on ? "on" : "off");
  } catch {
    // Kept for this page; the next reload starts with the sound off again.
  }
}

export function Kds() {
  useDarkTakeover();
  const now = useNow();

  // Which station this screen is for, from `?station=` so a board mounted over the grill can be
  // bookmarked as the grill's. Absent means every station — the screen a single-kitchen store has,
  // and the route the step gate and the replay drive.
  const [params, setParams] = useSearchParams<{ station?: string }>();
  const station = () => params.station ?? "";
  const stationName = (id: string | undefined) =>
    state.stations.find((candidate) => candidate.id === id)?.name ?? "";

  const tickets = createMemo(() => kitchenTickets());
  // A counter ticket is called by the guest's number, which the board reads from the counter list
  // for any order it does not yet know. It used to show the last four characters of the order's
  // internal id, which nobody at the counter could match to a guest.
  // It reads the board's clock too, so an order the counter list could not number yet is asked
  // about again — no more often than `refreshQueueNumbers` allows — rather than left unnumbered
  // until some other ticket changes.
  createEffect(() => {
    now();
    const counter = tickets()
      .filter((ticket) => ticket.tableLabel === "")
      .map((ticket) => ticket.orderId);
    if (counter.some((orderId) => queueNumberFor(orderId) === undefined)) {
      void refreshQueueNumbers(counter);
    }
  });
  const counterLabel = (orderId: string) => {
    const number = queueNumberFor(orderId);
    return number === undefined
      ? t("kds.counter_order", { ref: orderId.slice(-4) })
      : t("counter.queue_number", { number });
  };
  const byKey = createMemo(() => new Map(tickets().map((ticket) => [ticket.key, ticket])));
  const shownKeys = createMemo(
    () =>
      tickets()
        .filter((ticket) => station() === "" || ticket.stationId === station())
        .map((ticket) => ticket.key),
    [],
    { equals: sameKeys },
  );
  const shownLineCount = () =>
    shownKeys().reduce((count, key) => count + (byKey().get(key)?.lineIds.length ?? 0), 0);

  // The chime: new food on this board rings it, when the cook has turned the sound on.
  //
  // "New" means a line that reached the board after the board's own first read of what is open.
  // The lines already there when it came on — a reload mid-service, a tablet waking — are not news,
  // and a board that rang for every ticket at start-up would teach a kitchen to ignore it. Kept as
  // the set on the board now, replaced every time, so it is never larger than the board.
  const [sound, setSound] = createSignal(soundChosen());
  const [soundLive, setSoundLive] = createSignal(chimeReady());
  const [armed, setArmed] = createSignal(false);
  let onBoard = new Set<string>();
  const shownLines = () => shownKeys().flatMap((key) => byKey().get(key)?.lineIds ?? []);
  onMount(() => {
    void loadLiveOrders().finally(() => {
      onBoard = new Set(shownLines());
      setArmed(true);
    });
  });
  createEffect(() => {
    const lines = shownLines();
    if (armed() && sound() && lines.some((lineId) => !onBoard.has(lineId))) {
      chime();
    }
    onBoard = new Set(lines);
  });
  // Any tap on the board wakes the sound once it is chosen, since a board that reloaded cannot
  // start it by itself. The toggle is a tap too, and rings once so the cook hears it working.
  const wake = () => {
    if (sound() && !soundLive()) {
      void unlockChime().then(setSoundLive);
    }
  };
  const toggleSound = () => {
    const next = !sound();
    setSound(next);
    rememberSound(next);
    if (next) {
      void unlockChime().then((live) => {
        setSoundLive(live);
        chime();
      });
    }
  };

  const onBump = (ticket: KitchenTicket) => {
    void bump(ticket.orderId, ticket.lineIds, ticket.stationId);
  };

  // Taking a dish off every till from the kitchen (86), and putting it back. The cook is usually
  // the first to know the mozzarella has run out, and the board is where the cook is. A panel over
  // the board rather than a control on each ticket: a ticket is one whole-card tap that bumps it,
  // and a second target inside it would be hit by mistake in a rush. The same marks the tills make
  // (`docs/pos-spec.md` §3), under the same permission, which a cook holds by default.
  const [marking, setMarking] = createSignal(false);
  const [soldOutQuery, setSoldOutQuery] = createSignal("");
  const [soldOutError, setSoldOutError] = createSignal<string | null>(null);
  const soldOutNeedle = createMemo(() => fold(soldOutQuery().trim()));
  const soldOutNow = createMemo(() => state.menu.filter((item) => item.sold_out === true));
  // Every dish that can be marked, or the ones the box names. The whole price book, choices
  // included: the extra cheese runs out as surely as the pizza does. An item the console withdrew is
  // not the kitchen's to mark.
  const markable = createMemo(() =>
    state.menu.filter(
      (item) =>
        item.sold_out !== true &&
        item.available &&
        (soldOutNeedle() === "" || matches(soldOutNeedle(), [fold(item.display_name)])),
    ),
  );
  const openSoldOut = () => {
    setSoldOutError(null);
    setSoldOutQuery("");
    setMarking(true);
  };
  const guardSoldOut = async (run: () => Promise<void>) => {
    setSoldOutError(null);
    try {
      await run();
    } catch (caught) {
      setSoldOutError(errorMessage(caught));
    }
  };
  const markSoldOut = (item: MenuItemResponse) =>
    void guardSoldOut(() => setItemSoldOut(item.menu_item_id, true));
  const bringBack = (item: MenuItemResponse) =>
    void guardSoldOut(() => setItemSoldOut(item.menu_item_id, false));

  return (
    <section class="p-4" onPointerDown={() => wake()}>
      <div class="flex items-start justify-between gap-3">
        <PageHeader title={t("kds.title")} size="xl" />
        <div class="flex shrink-0 flex-wrap justify-end gap-2">
          <button
            type="button"
            class="min-h-touch rounded-token border border-line bg-surface px-3 text-sm"
            aria-expanded={marking()}
            data-step="openSoldOut"
            onClick={() => openSoldOut()}
          >
            {t("kds.sold_out")}
          </button>
          <button
            type="button"
            class="min-h-touch rounded-token border border-line bg-surface px-3 text-sm"
            aria-pressed={sound()}
            data-outcome="kds-sound"
            onClick={() => toggleSound()}
          >
            {sound() ? t("kds.sound_on") : t("kds.sound_off")}
          </button>
        </div>
      </div>
      <Show when={marking()}>
        <section
          class="mb-4 rounded-token border border-line bg-surface p-4"
          aria-label={t("kds.sold_out_title")}
        >
          <div class="flex items-center justify-between gap-3">
            <h2 class="text-lg font-semibold">{t("kds.sold_out_title")}</h2>
            <button
              type="button"
              class="min-h-touch rounded-token border border-line px-3 text-sm font-semibold"
              onClick={() => setMarking(false)}
            >
              {t("kds.sold_out_done")}
            </button>
          </div>
          <Show when={soldOutError()}>
            {(message) => (
              <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                {message()}
              </p>
            )}
          </Show>
          {/* What is off now, first: the question a cook asks before marking anything is whether
              somebody already has, and the answer is also the list to bring back from. */}
          <h3 class="mt-3 text-sm font-semibold text-ink-muted">{t("kds.sold_out_now")}</h3>
          <div class="mt-2 flex flex-wrap gap-2">
            <For
              each={soldOutNow()}
              fallback={<p class="text-sm text-ink-muted">{t("kds.sold_out_none")}</p>}
            >
              {(item) => (
                <button
                  type="button"
                  class="flex min-h-touch items-center gap-2 rounded-token border-2 border-danger bg-surface-raised px-3 text-left"
                  data-step="bringBack"
                  onClick={() => bringBack(item)}
                >
                  <span class="font-semibold line-through" data-outcome="kds-sold-out">
                    {item.display_name}
                  </span>
                  <span class="text-sm text-ink-muted">{t("kds.bring_back")}</span>
                </button>
              )}
            </For>
          </div>
          <label class="mt-4 block">
            <span class="text-sm text-ink-muted">{t("kds.sold_out_find")}</span>
            <input
              id="kds-sold-out-search"
              type="search"
              class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-3"
              value={soldOutQuery()}
              onInput={(event) => setSoldOutQuery(event.currentTarget.value)}
            />
          </label>
          <div class="mt-3 grid grid-cols-2 gap-2 tablet:grid-cols-3 terminal:grid-cols-4">
            <For
              each={markable()}
              fallback={<p class="text-sm text-ink-muted">{t("kds.sold_out_no_match")}</p>}
            >
              {(item) => (
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-left"
                  data-step="markSoldOut"
                  onClick={() => markSoldOut(item)}
                >
                  {item.display_name}
                </button>
              )}
            </For>
          </div>
        </section>
      </Show>
      <Show when={sound() && !soundLive()}>
        <p class="mb-3 text-sm text-ink-muted">{t("kds.sound_tap")}</p>
      </Show>
      <Show when={state.stations.length > 1}>
        <div class="-mx-1 mb-3 flex gap-2 overflow-x-auto px-1 pb-1" role="tablist" aria-label={t("kds.stations")}>
          <button
            type="button"
            role="tab"
            aria-selected={station() === ""}
            class="min-h-touch shrink-0 rounded-token border px-3 text-sm"
            classList={{
              "border-primary bg-selected text-selected-ink font-semibold": station() === "",
              "border-line bg-surface text-ink": station() !== "",
            }}
            onClick={() => setParams({ station: undefined })}
          >
            {t("kds.all_stations")}
          </button>
          <For each={state.stations}>
            {(option) => (
              <button
                type="button"
                role="tab"
                aria-selected={station() === option.id}
                class="min-h-touch shrink-0 rounded-token border px-3 text-sm"
                classList={{
                  "border-primary bg-selected text-selected-ink font-semibold": station() === option.id,
                  "border-line bg-surface text-ink": station() !== option.id,
                }}
                onClick={() => setParams({ station: option.id })}
              >
                {option.name}
              </button>
            )}
          </For>
        </div>
      </Show>
      <div class="grid grid-cols-1 gap-3 tablet:grid-cols-2 terminal:grid-cols-4">
        <For
          each={shownKeys()}
          fallback={
            <p class="text-ink-muted" data-outcome="board-clear">
              {t("kds.empty")}
            </p>
          }
        >
          {(key) => {
            const ticket = () => byKey().get(key);
            const waited = () => waitedSeconds(ticket()?.firedTime, now());
            const late = () => {
              const seconds = waited();
              return seconds !== null && seconds >= LATE_AFTER_SECONDS;
            };
            return (
              <Show when={ticket()}>
                {(current) => (
                  <button
                    type="button"
                    class="flex min-h-money flex-col items-start gap-1 rounded-token border bg-surface-raised p-4 text-left"
                    classList={{ "border-line": !late(), "border-danger": late() }}
                    data-step="onBump"
                    data-outcome={late() ? "ticket-late" : "ticket-waiting"}
                    onClick={() => onBump(current())}
                  >
                    <span class="flex w-full items-baseline justify-between gap-3">
                      <span class="text-sm text-ink-muted">
                        {current().tableLabel === ""
                          ? counterLabel(current().orderId)
                          : t("common.table", { label: current().tableLabel })}
                        <Show when={station() === "" && stationName(current().stationId) !== ""}>
                          {" · "}
                          {stationName(current().stationId)}
                        </Show>
                      </span>
                      {/* How long the kitchen has had it — the one question a board exists to
                          answer, and the reason a cook can pick the oldest of eight tickets without
                          reading every one. Always the number, never colour alone: a red card says
                          "late" only to somebody who knows the convention and can see red, while
                          "11:48" says it to everybody. The colour is the glance; the number is the
                          fact. Counted from the ticket's oldest line. */}
                      <Show when={waited() !== null}>
                        <span
                          class="text-lg font-semibold tabular-nums"
                          classList={{ "text-ink-muted": !late(), "text-danger": late() }}
                          data-outcome="ticket-waited"
                        >
                          {formatWaited(waited() ?? 0)}
                        </span>
                      </Show>
                    </span>
                    <ul class="flex w-full flex-col gap-1">
                      <For each={current().lineIds}>
                        {(lineId) => {
                          const line = () => state.lines[lineId];
                          const modifiers = () => {
                            const held = line();
                            return held === undefined ? [] : modifierNames(held);
                          };
                          return (
                            <li>
                              <span class="text-xl font-semibold">
                                <Show when={(line()?.quantityMilli ?? 1000) !== 1000}>
                                  <span class="tabular-nums">
                                    {formatQuantity(line()?.quantityMilli ?? 1000)}
                                    {"× "}
                                  </span>
                                </Show>
                                {line()?.name ?? ""}
                              </span>
                              {/* What was chosen, under the dish (ADR-0127). A board showing only
                                  "Margherita" could not tell a 25cm from a 30cm, and the cook is the
                                  person the choice was recorded for. Indented and one size down, so
                                  a cook scanning the board still reads *what* before *how* — the
                                  same order the printed ticket uses. */}
                              <Show when={modifiers().length > 0}>
                                <ul class="text-base text-ink-muted" data-outcome="ticket-modifiers">
                                  <For each={modifiers()}>{(modifier) => <li>+ {modifier}</li>}</For>
                                </ul>
                              </Show>
                            </li>
                          );
                        }}
                      </For>
                    </ul>
                    <span class="mt-1 text-sm text-ink-muted">{t("kds.bump")}</span>
                  </button>
                )}
              </Show>
            );
          }}
        </For>
      </div>
      <Show when={shownLineCount() > 0}>
        <p class="mt-4 text-sm text-ink-muted">{t("kds.count", { count: shownLineCount() })}</p>
      </Show>
    </section>
  );
}
