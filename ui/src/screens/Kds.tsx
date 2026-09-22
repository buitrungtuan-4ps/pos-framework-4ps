import { For, Show, createSignal, onCleanup } from "solid-js";

import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { useDarkTakeover } from "../lib/screen";
import { bump, firedLines } from "../state/store";

// The kitchen display: every fired line, large, on a dark panel. Bumping a ticket records the durable
// `kitchen.ticket.bumped` event (#44) and fans it out, so a second KDS agrees the ticket is done
// rather than holding a private, divergent "done" flag — and a screen coming online after the bump
// reads the same prepared set. The bumped line drops off this screen at once; the store folds the
// returning event so it stays off.

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

export function Kds() {
  useDarkTakeover();
  const now = useNow();
  const visible = () => firedLines();
  const onBump = (orderId: string, orderLineId: string) => {
    void bump(orderId, [orderLineId]);
  };

  return (
    <section class="p-4">
      <PageHeader title={t("kds.title")} size="xl" />
      <div class="grid grid-cols-1 gap-3 tablet:grid-cols-2 terminal:grid-cols-4">
        <For
          each={visible()}
          fallback={
            <p class="text-ink-muted" data-outcome="board-clear">
              {t("kds.empty")}
            </p>
          }
        >
          {(line) => {
            const waited = () => waitedSeconds(line.firedTime, now());
            const late = () => {
              const seconds = waited();
              return seconds !== null && seconds >= LATE_AFTER_SECONDS;
            };
            return (
              <button
                type="button"
                class="flex min-h-money flex-col items-start gap-1 rounded-token border bg-surface-raised p-4 text-left"
                classList={{ "border-line": !late(), "border-danger": late() }}
                data-step="onBump"
                data-outcome={late() ? "ticket-late" : "ticket-waiting"}
                onClick={() => onBump(line.orderId, line.orderLineId)}
              >
                <span class="flex w-full items-baseline justify-between gap-3">
                  <span class="text-sm text-ink-muted">
                    {t("common.table", { label: line.tableLabel })}
                  </span>
                  {/* How long the kitchen has had it — the one question a board exists to answer,
                      and the reason a cook can pick the oldest of eight tickets without reading
                      every one. Always the number, never colour alone: a red card says "late" only
                      to somebody who knows the convention and can see red, while "11:48" says it to
                      everybody. The colour is the glance; the number is the fact. */}
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
                <span class="text-xl font-semibold">{line.name}</span>
                {/* What was chosen, under the dish (ADR-0127). A board showing only "Margherita" could
                    not tell a 25cm from a 30cm, and the cook is the person the choice was recorded
                    for: a fired line consumes the base recipe plus one recipe per modifier (§8), so
                    the kitchen was being asked to make something the screen would not name. Indented
                    and one size down, so a cook scanning the board still reads *what* before *how* —
                    the same order the printed ticket uses. */}
                <Show when={line.modifiers.length > 0}>
                  <ul class="text-base text-ink-muted" data-outcome="ticket-modifiers">
                    <For each={line.modifiers}>{(modifier) => <li>+ {modifier}</li>}</For>
                  </ul>
                </Show>
                <span class="mt-1 text-sm text-ink-muted">{t("kds.bump")}</span>
              </button>
            );
          }}
        </For>
      </div>
      <Show when={visible().length > 0}>
        <p class="mt-4 text-sm text-ink-muted">{t("kds.count", { count: visible().length })}</p>
      </Show>
    </section>

  );
}
