// A HYBRID store's offline symbol (ADR-0251 decision 6): the symbol it numbers under while it cannot
// reach the cloud, which the cloud allocates from the tenant's ledger like any symbol, so it never
// shares a sequence with the vendor's. Shown only where `einvoice.number_source` is HYBRID, which the
// settings write only with a recorded confirmation that legal and the vendor allow it.

import { createSignal, Show } from "solid-js";

import { api } from "../../api/client";
import type { Json } from "../../api/types";
import { allocateEInvoiceSymbol } from "../../api/einvoices";
import { t } from "../../i18n";
import { apiMessage } from "../../lib/errors";
import { createAdminResource, failureOf } from "../../lib/resource";
import { storeId, tenantId } from "../../state/session";
import { Banner, Button, StatusBadge } from "../../components/ui";
import { toast } from "../../components/Toast";

/** What the offline symbol card reads of the store's `einvoice` node. */
export type HybridState = { readonly hybrid: boolean; readonly offlineSymbol: string | null };

/** Reads the number source and the offline symbol out of an effective tree. */
export function readHybrid(effective: Json | null): HybridState {
  const root = effective !== null && typeof effective === "object" && !Array.isArray(effective) ? effective : {};
  const node = root["einvoice"];
  const einvoice = node !== null && typeof node === "object" && !Array.isArray(node) ? node : {};
  const offline = einvoice["offline_symbol"];
  return {
    hybrid: einvoice["number_source"] === "EINVOICE_NUMBER_SOURCE_HYBRID",
    offlineSymbol: typeof offline === "string" && offline !== "" ? offline : null,
  };
}

export function OfflineSymbol() {
  const read = createAdminResource(
    async (tenant, store) => (store ? readHybrid(await api.effectiveConfig(tenant, store)) : null),
    { scope: "tenant" },
  );
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const allocate = async () => {
    setBusy(true);
    setError("");
    try {
      const made = await allocateEInvoiceSymbol(tenantId(), storeId(), true);
      toast.ok(t("einvoices.offlineSymbolPublished", { symbol: made.symbol }));
      await read.refetch();
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Show when={read.value()?.hybrid}>
      <div class="grid max-w-xl gap-3" data-testid="einvoice-offline-symbol">
        <Show when={failureOf(read)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <div class="flex flex-wrap items-center gap-3">
          <span class="text-sm text-ink-muted">{t("einvoices.offlineSymbolNow")}</span>
          <Show
            when={read.value()?.offlineSymbol}
            fallback={<StatusBadge tone="neutral" label={t("einvoices.offlineSymbolNone")} />}
          >
            {(symbol) => <span class="font-mono text-lg text-ink">{symbol()}</span>}
          </Show>
        </div>
        <p class="text-sm text-ink-muted">{t("einvoices.offlineSymbolHint")}</p>
        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <div>
          <Button variant="secondary" onClick={() => void allocate()} disabled={busy()}>
            {t("einvoices.offlineSymbolAllocate")}
          </Button>
        </div>
      </div>
    </Show>
  );
}
