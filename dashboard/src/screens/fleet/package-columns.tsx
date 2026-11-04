// The fleet table's two package columns: how configuration reaches each store and how fast
// ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)), and the
// package version it runs, with a configuration it refused
// ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 6).
// Both from the store's own heartbeat; a store that never said shows that, never a guess.

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
      sortValue: (row) => (row.package?.refused ? 0 : 1),
      cell: (row) => <PackageCell row={row} />,
    },
  ];
}
