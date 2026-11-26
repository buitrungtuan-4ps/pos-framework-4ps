// A delivery order's badge: the vendor it came through, so staff tell an app order from a walk-in at
// a glance (ADR-0248 decision 5). Renders nothing for any other order.

import { Show } from "solid-js";
import { t } from "../i18n";

export function VendorBadge(props: { vendor: string | undefined }) {
  return (
    <Show when={props.vendor}>
      {(vendor) => (
        <span
          class="ml-2 inline-block rounded-token border border-accent px-2 text-sm font-semibold text-accent"
          data-outcome="vendor-badge"
        >
          {t("confirm.vendor", { vendor: vendor() })}
        </span>
      )}
    </Show>
  );
}
