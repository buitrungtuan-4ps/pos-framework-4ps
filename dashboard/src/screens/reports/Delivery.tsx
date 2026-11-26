// The delivery reconciliation card (ADR-0248 decision 3): for each day and delivery app, the orders
// it sent, what they took, the app's commission at its rate and the net the store expects to be
// paid out. The bills stay gross; the commission is shown here, apart.

import { For, Show } from "solid-js";

import { deliveryReconciliation, type DeliveryReconciliationRow } from "../../api/delivery";
import { t } from "../../i18n";
import { createAdminResource } from "../../lib/resource";
import { formatAmount } from "../../state/money";
import { Card } from "../../components/ui";

const amount = (row: DeliveryReconciliationRow, minor: number) =>
  formatAmount({ amount_minor: minor, currency_code: row.currency_code });

/** The rate in basis points as a percentage, "25%" or "12.5%". */
export function rateText(bps: number): string {
  return `${String(bps / 100)}%`;
}

export function DeliveryReconciliation(props: { window: () => { from?: string; to?: string } }) {
  const rows = createAdminResource(
    (tenant, store) => deliveryReconciliation(tenant, store, props.window()),
    { scope: "store" },
  );
  return (
    <Card title={t("reports.deliveryTitle")}>
      <Show
        when={(rows.value() ?? []).length > 0}
        fallback={<p class="text-sm text-ink-muted">{t("reports.deliveryEmpty")}</p>}
      >
        <table class="w-full text-sm" data-outcome="delivery-reconciliation">
          <thead>
            <tr class="text-left text-ink-muted">
              <th>{t("reports.date")}</th>
              <th>{t("reports.deliveryVendor")}</th>
              <th class="text-right">{t("reports.deliveryOrders")}</th>
              <th class="text-right">{t("reports.deliveryGross")}</th>
              <th class="text-right">{t("reports.deliveryCommission")}</th>
              <th class="text-right">{t("reports.deliveryNet")}</th>
            </tr>
          </thead>
          <tbody>
            <For each={rows.value() ?? []}>
              {(row) => (
                <tr>
                  <td class="font-mono">{row.business_date}</td>
                  <td>{row.vendor_name || row.vendor_id}</td>
                  <td class="text-right tabular-nums">{row.orders}</td>
                  <td class="text-right tabular-nums">{amount(row, row.gross)}</td>
                  <td class="text-right tabular-nums">
                    {`${amount(row, row.commission)} (${rateText(row.commission_bps)})`}
                  </td>
                  <td class="text-right tabular-nums">{amount(row, row.net)}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </Card>
  );
}
