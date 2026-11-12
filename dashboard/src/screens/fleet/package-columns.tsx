// The fleet table's two package columns: how configuration reaches each store and how fast
// ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)), and the
// package version it runs, with a configuration it refused
// ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 6).
// Both from the store's own heartbeat; a store that never said shows that, never a guess. Under the
// version, how many of the store's paired tills last read another one (roadmap-v3 B7.3): a till that
// missed a `config_applied` still draws the version it read before, and the store's own report
// cannot show it.

import { Show } from "solid-js";

import type { FleetStore } from "../../api/schemas";
import { t } from "../../i18n";
import { formatCount } from "../../lib/format";
import { StatusBadge } from "../../components/ui";
import type { Column } from "../../components/kit";

/** A store on the poll alone sorts first: it is the one a published change reaches slowest. */
export function deliveryRank(row: FleetStore): number {
  switch (row.config_wake?.delivery) {
    case "CONFIG_DELIVERY_POLL":
      return 0;
    case "CONFIG_DELIVERY_WAKE":
      return 2;
    default:
      return 1;
  }
}

function DeliveryCell(props: { row: FleetStore }) {
  return (
    <div class="flex flex-wrap items-baseline gap-2" data-outcome="config-delivery">
      <Show
        when={props.row.config_wake}
        fallback={<span class="text-sm text-ink-muted">{t("fleet.notReported")}</span>}
      >
        {(wake) => (
          <>
            <StatusBadge
              tone={wake().delivery === "CONFIG_DELIVERY_WAKE" ? "active" : "neutral"}
              label={
                wake().delivery === "CONFIG_DELIVERY_WAKE"
                  ? t("fleet.deliveryWake")
                  : t("fleet.deliveryPoll")
              }
            />
            <Show when={wake().p95_ms !== undefined && wake().max_ms !== undefined}>
              <span class="text-xs text-ink-muted">
                {t("fleet.deliveryTimes", {
                  p95: formatCount(wake().p95_ms ?? 0),
                  max: formatCount(wake().max_ms ?? 0),
                  samples: wake().samples,
                })}
              </span>
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}

/** How many of the store's tills read another version than it runs, or `null` when it never said. */
export function tillsBehind(row: FleetStore): { behind: number; count: number } | null {
  const tills = row.devices?.tills;
  if (tills === undefined) {
    return null;
  }
  return { behind: tills.filter((till) => till.behind).length, count: tills.length };
}

/** A store with a refused configuration sorts first, then one with tills behind. */
export function packageRank(row: FleetStore): number {
  if (row.package?.refused) {
    return 0;
  }
  return (tillsBehind(row)?.behind ?? 0) > 0 ? 1 : 2;
}

function TillsLine(props: { row: FleetStore }) {
  return (
    <Show when={tillsBehind(props.row)}>
      {(tills) => (
        <Show
          when={tills().behind > 0}
          fallback={
            <Show when={tills().count > 0}>
              <span class="text-xs text-ink-muted">{t("fleet.tillsCurrent", { count: tills().count })}</span>
            </Show>
          }
        >
          <span data-outcome="tills-behind">
            <StatusBadge tone="danger" label={t("fleet.tillsBehind", tills())} />
          </span>
        </Show>
      )}
    </Show>
  );
}

function PackageCell(props: { row: FleetStore }) {
  return (
    <Show
      when={props.row.package}
      fallback={<span class="text-sm text-ink-muted">{t("fleet.notReported")}</span>}
    >
      {(reported) => (
        <div class="flex flex-col gap-1">
          <span class="text-sm text-ink">
            {reported().package_version_id ?? t("packages.defaultPackage")}
          </span>
          <Show when={reported().refused}>
            {(refused) => (
              <StatusBadge
                tone="danger"
                label={t("fleet.packageRefused", { reason: refused().reason })}
              />
            )}
          </Show>
          <TillsLine row={props.row} />
        </div>
      )}
    </Show>
  );
}

/** The two columns, in the order the table shows them. */
export function packageColumns(): Column<FleetStore>[] {
  return [
    {
      key: "delivery",
      header: t("fleet.delivery"),
      sortValue: deliveryRank,
      cell: (row) => <DeliveryCell row={row} />,
    },
    {
      key: "package",
      header: t("fleet.package"),
      sortValue: packageRank,
      cell: (row) => <PackageCell row={row} />,
    },
  ];
}
