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
// count, not the live 86 list: `inventory.item.sold_out` minus `inventory.item.restored` cannot name
// the dish, only count it. It also does not count anything *yet* — nothing in the tree emits either
// event (production-readiness **O5**), so the card says "not reported" rather than a confident zero.
// ADR-0099 claimed the number was exact; a number nobody produces is exactly zero, which is not the
// same thing. Both cards are recorded as follow-ups in that ADR rather than dressed up here.

import { createSignal, Show, type JSXElement } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Alert, DailyRevenue, DailyRollup, FleetStore } from "../api/types";
import { t } from "../i18n";
import { formatCount, formatMoney, formatRelativeAge } from "../lib/format";
import { LOADING, type Panel, panelOf } from "../lib/panel";
import {
  configVerdict,
  neverInstalled,
  onlineVerdict,
  toneClass,
  type Tone,
} from "../lib/posture";
import { contextReady, onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, storeId, tenantId } from "../state/session";
import { screenHref, type ScreenId } from "../state/screens";
import { GetStarted } from "../components/GetStarted";
import { Card, PageHeader, Skeleton } from "../components/ui";

/** Owner/Admin see money (revenue is T2); the server re-checks, so this only hides what would 403. */
function canReadRevenue(): boolean {
  const role = actingAdmin()?.role;
  return role === "owner" || role === "admin";
}

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

/** How many of `type` the day's activity rollup counted (absent means none happened). */
function counted(day: DailyRollup | undefined, type: string): number {
  return day?.by_type[type] ?? 0;
}

/** One card: a headline figure, a supporting line, and a link to the screen that owns the subject. */
function HubCard<T>(props: {
  title: string;
  panel: Panel<T>;
  link: ScreenId;
  linkLabel: string;
  children: (value: T) => { headline: string; support: string; tone?: Tone };
  /**
   * An optional affordance under the answer, rendered only when the panel is ready.
   *
   * One card uses it — the region warning, which ADR-0114 requires be answerable in the place it is
   * drawn. Every other card stays read-only and links out, per ADR-0099: a hub that could edit five
   * things would be a second copy of five editors, free to drift from the real ones.
   */
  action?: (value: T) => JSXElement;
}) {
  return (
    <Card title={props.title}>
      <Show
        when={props.panel.state === "ready" ? props.panel : null}
        fallback={
          <Show
            when={props.panel.state === "failed" ? props.panel : null}
            // Two bars: a card answers with one figure and one supporting line, so that is the
            // shape the placeholder holds. A refusal stays words — there is nothing arriving for a
            // skeleton to stand in for.
            fallback={<Skeleton label={t("common.loading")} rows={2} />}
          >
            {(failed) => <p class="text-sm text-ink-muted">{failed().message}</p>}
          </Show>
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
              {props.action?.(ready().value)}
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

  // Whether this store has a region to show at all (ADR-0114). Narrowed on the panel's own state
  // rather than reaching past it: a store still loading, or one whose read failed, has nothing to
  // say here, and an in-store machine is in the shop and has no region by construction. Rendering a
  // card for it would invent a fact.
  const hasRegion = () => {
    const panel = fleet();
    return panel.state === "ready" && panel.value.region_country !== null;
  };

  // Whether the get-started checklist belongs on the screen.
  //
  // Six cards of absence do not tell an operator that this shop simply has not been installed yet,
  // and the checklist does — so it renders whenever the setup is unfinished, and fires none of its
  // reads when it is. A store that has checked in has been set up: every link of the chain happened
  // or it could not have reported. That keeps the landing screen's request count flat for the whole
  // life of a working store, which is the reason this decision is made here, on a read the hub
  // already does, rather than inside the checklist on a seventh request of its own.
  //
  // With no store chosen the fleet read never runs — `onScopedContext("store")` is what gates it —
  // so a permanently loading panel is not "we do not know yet", it is the fresh-install state, and
  // the state the checklist exists for.
  const showGetStarted = () => {
    const panel = fleet();
    if (panel.state === "ready") {
      return neverInstalled(panel.value);
    }
    return !contextReady("store") || panel.state === "failed";
  };

  // The answer an admin is typing, and whether one is in flight. Kept on the screen rather than in
  // the card, because the card is a pure render of a panel and this is the one place on the hub that
  // writes anything (ADR-0114; ADR-0099's read-only rule bends here for exactly one action, because
  // the warning it answers is drawn here and nowhere else).
  const [reason, setReason] = createSignal("");
  const [acknowledging, setAcknowledging] = createSignal(false);
  const [refusal, setRefusal] = createSignal<string | null>(null);

  // Records the answer, then re-reads the store so the card redraws from the server's verdict rather
  // than from an optimistic guess — the standing rule lives on the server and the console must not
  // second-guess it.
  const acknowledge = async (store: FleetStore) => {
    const tenant = tenantId();
    const id = storeId();
    if (tenant === null || id === null) {
      return;
    }
    setAcknowledging(true);
    setRefusal(null);
    try {
      await api.acknowledgeRegion(tenant, id, {
        profile_country: store.profile_country ?? "",
        region_country: store.region_country ?? "",
        reason: reason(),
      });
      setReason("");
      await panelOf(api.fleetStore(tenant, id), setFleet);
    } catch (error) {
      setRefusal(error instanceof ApiError ? error.message : String(error));
    } finally {
      setAcknowledging(false);
    }
  };

  // A store-scoped alert carries its store id in `dedup_key` (ADR-0073): the key scopes the alert
  // *within* its kind, and for the store-scoped kinds that scope is the store. A server-wide kind
  // keys on something else and simply will not match, which is the behaviour we want — the console
  // Alerts screen is where fleet-wide conditions belong.
  const storeAlerts = (all: Alert[]) =>
    all.filter((alert) => alert.dedup_key === storeId() && alert.resolved_at_ms === null);

  return (
    <div class="space-y-6">
      <PageHeader title={t("hub.title")} description={t("hub.description")} />
      <Show when={showGetStarted()}>
        <GetStarted />
      </Show>
      <RequireContext need="store">
        <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          <HubCard
            title={t("hub.online.title")}
            panel={fleet()}
            link="fleet"
            linkLabel={t("hub.online.link")}
          >
            {(store) => {
              const verdict = onlineVerdict(store);
              return {
                headline: t(verdict.headline),
                tone: verdict.tone,
                support:
                  store.last_seen_at_ms === null
                    ? t("hub.online.never")
                    : t("hub.online.lastSeen", {
                        when: formatRelativeAge(ageSeconds(store.last_seen_at_ms)),
                      }),
              };
            }}
          </HubCard>

          {/* Where this store's data rests, and whether that agrees with the country the store
              publishes (ADR-0114). It rides `api.fleetStore`, which this screen already calls, so
              ADR-0099's rule holds — each card reads an endpoint that already exists.

              A mismatch warns and never blocks. Whether a particular cross-border placement is
              lawful depends on facts the operator holds, under law that changes without a release;
              a block would refuse lawful placements, catch no unlawful ones, and get routed around
              by an admin picking whichever region passes — leaving the record saying something
              false. A dismissed warning is recorded. A dodged block is not. */}
          <Show when={hasRegion()}>
            <HubCard
              title={t("hub.region.title")}
              panel={fleet()}
              link="fleet"
              linkLabel={t("hub.region.link")}
              action={(store) => (
                // Only a live, unanswered difference gets the form. An agreeing store has nothing to
                // answer; a store with no published country has nothing to compare, and the fix
                // there is to publish one, not to explain a difference nobody has established.
                <Show
                  when={store.region_agreement === "differs"}
                  fallback={<></>}
                >
                  <Show
                    when={store.region_acknowledgement}
                    fallback={
                      <div class="mt-3 space-y-2 border-t border-line pt-3">
                        <label class="block text-sm" for="region-reason">
                          {t("hub.region.acknowledgeLabel")}
                        </label>
                        <textarea
                          id="region-reason"
                          class="w-full rounded border border-line bg-surface p-2 text-sm"
                          rows={2}
                          maxlength={280}
                          value={reason()}
                          onInput={(event) => setReason(event.currentTarget.value)}
                        />
                        <p class="text-xs text-ink-muted">{t("hub.region.acknowledgeHint")}</p>
                        <Show when={refusal()}>
                          {(message) => <p class="text-xs text-danger">{message()}</p>}
                        </Show>
                        <button
                          type="button"
                          class="rounded bg-accent px-3 py-1 text-sm text-on-accent disabled:opacity-50"
                          disabled={acknowledging() || reason().trim() === ""}
                          onClick={() => void acknowledge(store)}
                        >
                          {t("hub.region.acknowledgeAction")}
                        </button>
                      </div>
                    }
                  >
                    {(answer) => (
                      <div class="mt-3 space-y-1 border-t border-line pt-3">
                        <p class="text-sm font-medium">{t("hub.region.acknowledged")}</p>
                        <p class="text-sm text-ink-muted">{answer().reason}</p>
                        <p class="text-xs text-ink-muted">
                          {t("hub.region.acknowledgedWhen", {
                            when: formatRelativeAge(ageSeconds(answer().acknowledged_at_ms)),
                          })}
                        </p>
                      </div>
                    )}
                  </Show>
                </Show>
              )}
            >
              {(store) => ({
                headline: store.region_label
                  ? `${store.region_country} · ${store.region_label}`
                  : (store.region_country ?? ""),
                tone:
                  store.region_agreement === "agrees" || store.region_acknowledgement
                    ? "ok"
                    : "attention",
                support:
                  store.region_agreement === "agrees"
                    ? t("hub.region.agrees", { country: store.profile_country ?? "" })
                    : store.region_agreement === "differs"
                      ? t("hub.region.differs", {
                          region: store.region_country ?? "",
                          country: store.profile_country ?? "",
                        })
                      : t("hub.region.countryNotRecorded"),
              })}
            </HubCard>
          </Show>

          <HubCard
            title={t("hub.config.title")}
            panel={fleet()}
            link="config"
            linkLabel={t("hub.config.link")}
          >
            {(store) => {
              const verdict = configVerdict(store);
              return {
                headline: t(verdict.headline),
                tone: verdict.tone,
                support: t("hub.config.versions", {
                  held: store.config_version_held ?? t("hub.config.none"),
                  published: store.config_version_published ?? t("hub.config.none"),
                }),
              };
            }}
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
              // Nothing in the tree emits either event yet (production-readiness **O5**): the
              // auto-86 rule exists in `pos-core` §8 but the live stock projection that would fire
              // it is still a follow-up. So this arithmetic is always zero, and a confident `0`
              // beside "items marked out" reads as "nothing is 86'd today" — a measurement, when
              // there is no measurement. An em dash says what is true: not reported yet.
              if (out <= 0) {
                return {
                  headline: "—",
                  tone: "idle" as const,
                  support: t("hub.stock.notReported"),
                };
              }
              return {
                headline: formatCount(out),
                tone: "attention" as const,
                support: day
                  ? t("hub.stock.support", { date: day.business_date })
                  : t("hub.stock.noDay"),
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
