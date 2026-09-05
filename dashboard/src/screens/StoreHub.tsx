// The per-store hub — the console's landing page ([ADR-0099](../../../docs/adr/0099-store-hub.md),
// roadmap v3 **Q4**). Six cards, each one question an operator has about one shop, each linking to
// the screen that can act on the answer.
//
// # Why this is the index and Reports is not
//
// Q4's URL half shipped a year of slices ago — a tenant is a path segment, a store a `?store=` query
// — and the screen it was *for* was never built, so the slice read as done. The index rendered
// Reports: a windowed revenue and product-mix view. That is a good screen and the wrong first
// screen. It answers how much the shop made before it says whether the shop is online, whether it
// holds the configuration that was published to it, whether anyone has opened a till, whether the
// kitchen has run out of anything, or whether an alert is firing against it — five questions that
// today cost five navigations each.
//
// # Read-only, and assembled from reads that already exist
//
// No new route, projection, migration or permission. Three requests: the fleet row (online +
// config), the activity rollup (shifts + sold-out counts) and the alert list; plus a fourth,
// revenue, for the roles allowed to see money. A hub that could *write* would be a second copy of
// five editors, each free to drift from the real one, so every card links out instead.
//
// # Two cards are honest approximations and say so on the card
//
// **Working** is a count of shifts, not a list of names: the cloud projects no roster, and a roster
// would be T1 employee data needing a lawful basis, not a card. **Out of stock** is the day's net
// count, not the live 86 list: `inventory.item.sold_out` minus `inventory.item.restored` is exact as
// a number and cannot name the dish. Both are recorded as follow-ups in ADR-0099 rather than
// dressed up here.

import { createSignal, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Alert, DailyRevenue, DailyRollup, FleetStore } from "../api/types";
import { t } from "../i18n";
import { formatCount, formatMoney, formatRelativeAge } from "../lib/format";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, storeId, tenantId } from "../state/session";
import { screenHref, type ScreenId } from "../state/screens";
import { Card, PageHeader } from "../components/ui";

/** Owner/Admin see money (revenue is T2); the server re-checks, so this only hides what would 403. */
function canReadRevenue(): boolean {
  const role = actingAdmin()?.role;
  return role === "owner" || role === "admin";
}

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

/**
 * A card's own load outcome. A hub renders six independent answers, so one failed read must not
 * blank the page — and a page-level banner would make "the alert service is down" and "this shop has
 * no revenue yet" look identical, which is the mistake this type exists to prevent.
 */
type Panel<T> = { readonly state: "loading" } | { readonly state: "ready"; readonly value: T } | {
  readonly state: "failed";
  readonly message: string;
};

const LOADING = { state: "loading" } as const;

function panelOf<T>(promise: Promise<T>, set: (panel: Panel<T>) => void): Promise<void> {
  return promise.then(
    (value) => set({ state: "ready", value }),
    (caught: unknown) =>
      set({
        state: "failed",
        message: caught instanceof ApiError ? caught.message : String(caught),
      }),
  );
}

/** How many of `type` the day's activity rollup counted (absent means none happened). */
function counted(day: DailyRollup | undefined, type: string): number {
  return day?.by_type[type] ?? 0;
}

/**
 * How a card's headline reads at a glance. The **support line always carries the same fact in
 * words**, so the hue is a second channel and never the only one — meaning carried by colour alone
 * is the accessibility failure the contrast gate's palette exists to avoid. `plain` is for a figure
 * that is neither good nor bad on its own: money taken is not a fault when it is low.
 */
type Tone = "ok" | "attention" | "idle" | "plain";

function toneClass(tone: Tone): string {
  switch (tone) {
    case "ok":
      return "text-ok";
    case "attention":
      return "text-danger";
    case "idle":
      return "text-ink-muted";
    default:
      return "text-ink";
  }
}

/** One card: a headline figure, a supporting line, and a link to the screen that owns the subject. */
function HubCard<T>(props: {
  title: string;
  panel: Panel<T>;
  link: ScreenId;
  linkLabel: string;
  children: (value: T) => { headline: string; support: string; tone?: Tone };
}) {
  return (
    <Card title={props.title}>
      <Show
        when={props.panel.state === "ready" ? props.panel : null}
        fallback={
          <p class="text-sm text-ink-muted">
            {props.panel.state === "failed" ? props.panel.message : t("common.loading")}
          </p>
        }
      >
        {(ready) => {
          const rendered = props.children(ready().value);
          return (
            <div class="space-y-1">
              <p class={`text-2xl font-semibold ${toneClass(rendered.tone ?? "plain")}`}>
                {rendered.headline}
              </p>
              <p class="text-sm text-ink-muted">{rendered.support}</p>
            </div>
          );
        }}
      </Show>
      <p class="mt-3 text-sm">
        <a class="text-accent underline" href={screenHref(props.link, tenantId(), storeId())}>
          {props.linkLabel}
        </a>
      </p>
    </Card>
  );
}

export function StoreHub() {
  const [fleet, setFleet] = createSignal<Panel<FleetStore>>(LOADING);
  const [activity, setActivity] = createSignal<Panel<DailyRollup[]>>(LOADING);
  const [revenue, setRevenue] = createSignal<Panel<DailyRevenue[]>>(LOADING);
  const [alerts, setAlerts] = createSignal<Panel<Alert[]>>(LOADING);

  // `limit: 1` returns the store's newest trading day (the window keeps the newest N, oldest first),
  // which is not necessarily *today* — a shop that has not traded yet reports yesterday. Every card
  // built on it prints the business date it is reporting rather than claiming "today", because "no
  // revenue today" and "the latest day we have is yesterday" are different facts.
  onScopedContext("store", (tenant, store) => {
    setFleet(LOADING);
    setActivity(LOADING);
    setRevenue(LOADING);
    setAlerts(LOADING);
    void panelOf(api.fleetStore(tenant, store), setFleet);
    void panelOf(api.dailyRollups(tenant, store, { limit: 1 }), setActivity);
    void panelOf(api.listAlerts(), setAlerts);
    if (canReadRevenue()) {
      void panelOf(api.dailyRevenue(tenant, store, { limit: 1 }), setRevenue);
    }
  });

  // A store-scoped alert carries its store id in `dedup_key` (ADR-0073): the key scopes the alert
  // *within* its kind, and for the store-scoped kinds that scope is the store. A server-wide kind
  // keys on something else and simply will not match, which is the behaviour we want — the console
  // Alerts screen is where fleet-wide conditions belong.
  const storeAlerts = (all: Alert[]) =>
    all.filter((alert) => alert.dedup_key === storeId() && alert.resolved_at_ms === null);

  return (
    <div class="space-y-6">
      <PageHeader title={t("hub.title")} description={t("hub.description")} />
      <RequireContext need="store">
        <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          <HubCard
            title={t("hub.online.title")}
            panel={fleet()}
            link="fleet"
            linkLabel={t("hub.online.link")}
          >
            {(store) => ({
              headline: store.online ? t("hub.online.yes") : t("hub.online.no"),
              tone: store.online ? "ok" : "attention",
              support:
                store.last_seen_at_ms === null
                  ? t("hub.online.never")
                  : t("hub.online.lastSeen", {
                      when: formatRelativeAge(ageSeconds(store.last_seen_at_ms)),
                    }),
            })}
          </HubCard>

          <HubCard
            title={t("hub.config.title")}
            panel={fleet()}
            link="config"
            linkLabel={t("hub.config.link")}
          >
            {(store) => ({
              headline: store.config_current ? t("hub.config.current") : t("hub.config.behind"),
              tone: store.config_current ? "ok" : "attention",
              support: t("hub.config.versions", {
                held: store.config_version_held ?? t("hub.config.none"),
                published: store.config_version_published ?? t("hub.config.none"),
              }),
            })}
          </HubCard>

          <Show
            when={canReadRevenue()}
            fallback={
              <Card title={t("hub.money.title")}>
                <p class="text-sm text-ink-muted">{t("hub.money.hidden")}</p>
              </Card>
            }
          >
            <HubCard
              title={t("hub.money.title")}
              panel={revenue()}
              link="reports"
              linkLabel={t("hub.money.link")}
            >
              {(days) => {
                const day = days.at(-1);
                return {
                  headline: day
                    ? formatMoney({ amount_minor: day.net, currency_code: day.currency_code })
                    : t("hub.money.none"),
                  support: day
                    ? t("hub.money.onDate", {
                        date: day.business_date,
                        bills: formatCount(day.bills),
                      })
                    : t("hub.money.noneSupport"),
                };
              }}
            </HubCard>
          </Show>

          <HubCard
            title={t("hub.working.title")}
            panel={activity()}
            link="people"
            linkLabel={t("hub.working.link")}
          >
            {(days) => {
              const day = days.at(-1);
              const open = counted(day, "cash.shift.opened") - counted(day, "cash.shift.closed");
              return {
                headline: formatCount(Math.max(0, open)),
                tone: open > 0 ? "ok" : "idle",
                support: day
                  ? t("hub.working.support", {
                      date: day.business_date,
                      opened: formatCount(counted(day, "cash.shift.opened")),
                    })
                  : t("hub.working.noDay"),
              };
            }}
          </HubCard>

          <HubCard
            title={t("hub.stock.title")}
            panel={activity()}
            link="inventory"
            linkLabel={t("hub.stock.link")}
          >
            {(days) => {
              const day = days.at(-1);
              const out =
                counted(day, "inventory.item.sold_out") - counted(day, "inventory.item.restored");
              return {
                headline: formatCount(Math.max(0, out)),
                tone: out > 0 ? "attention" : "ok",
                support: day ? t("hub.stock.support", { date: day.business_date }) : t("hub.stock.noDay"),
              };
            }}
          </HubCard>

          <HubCard
            title={t("hub.alerts.title")}
            panel={alerts()}
            link="alerts"
            linkLabel={t("hub.alerts.link")}
          >
            {(all) => {
              const firing = storeAlerts(all);
              return {
                headline: formatCount(firing.length),
                tone: firing.length > 0 ? "attention" : "ok",
                support:
                  firing[0] === undefined
                    ? t("hub.alerts.quiet")
                    : t("hub.alerts.newest", { summary: firing[0].summary }),
              };
            }}
          </HubCard>
        </div>
      </RequireContext>
    </div>
  );
}
