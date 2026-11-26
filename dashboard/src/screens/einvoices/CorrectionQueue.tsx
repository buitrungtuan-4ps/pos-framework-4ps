// HQ's queue of the correction requests store managers sent from the till (ADR-0245 route b,
// ADR-0250). A request asks HQ to let the store void a paid bill whose e-invoice was issued; nothing
// is issued and no money moves until HQ approves. Approving or declining records who decided, is
// published on the store's own `einvoice` node and audited with the requester. The queue is the
// tenant's, so each row names its store. People are shown by id only.

import { createSignal, Show } from "solid-js";

import { api } from "../../api/client";
import {
  type CorrectionRequestView,
  decideCorrectionRequest,
  listCorrectionRequests,
} from "../../api/einvoices";
import { t } from "../../i18n";
import { apiMessage } from "../../lib/errors";
import { formatInstant, formatMinor } from "../../lib/format";
import { createAdminResource, failureOf } from "../../lib/resource";
import { tenantId } from "../../state/session";
import { Banner, Button, StatusBadge } from "../../components/ui";
import { DataTable, Tabs } from "../../components/kit";
import { toast } from "../../components/Toast";

type Status = "PENDING" | "APPROVED" | "DECLINED";

/** The badge tone a request's status reads as. */
export function requestTone(status: string): "active" | "danger" | "neutral" {
  if (status === "APPROVED") {
    return "active";
  }
  return status === "DECLINED" ? "danger" : "neutral";
}

/** The last eight characters of an id: enough to match a bill or a badge at the store. */
export function shortId(id: string | null | undefined): string {
  return id ? id.slice(-8) : "—";
}

/** The status filter's choice as the route reads it; `ALL` sends none. */
export function statusFilter(choice: string): Status | undefined {
  return choice === "PENDING" || choice === "APPROVED" || choice === "DECLINED" ? choice : undefined;
}

function exponentOf(currency: string): number {
  return currency === "VND" || currency === "JPY" ? 0 : 2;
}

export function CorrectionQueue() {
  const [choice, setChoice] = createSignal<Status | "ALL">("PENDING");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal<string | null>(null);
  const read = createAdminResource(
    async (tenant) => {
      const [rows, stores, reasons] = await Promise.all([
        listCorrectionRequests(tenant, statusFilter(choice())),
        api.listStores(tenant),
        api.listReasonCodes(tenant),
      ]);
      return {
        rows,
        stores: new Map(stores.map((store) => [store.store_id, store.name])),
        reasons: new Map(reasons.map((reason) => [reason.id, reason.display_name])),
      };
    },
    { scope: "tenant" },
  );

  const decide = async (row: CorrectionRequestView, verb: "approve" | "decline") => {
    setBusy(row.request_id);
    setError("");
    try {
      await decideCorrectionRequest(tenantId(), row.request_id, verb);
      toast.ok(t(verb === "approve" ? "einvoices.queueApproved" : "einvoices.queueDeclined", { bill: shortId(row.bill_id) }));
      await read.refetch();
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div class="grid gap-3" data-testid="einvoice-queue">
      <p class="text-sm text-ink-muted">{t("einvoices.queueHint")}</p>
      <Tabs
        label={t("einvoices.queueFilter")}
        active={choice()}
        tabs={[
          { key: "PENDING", label: t("einvoices.request.PENDING") },
          { key: "APPROVED", label: t("einvoices.request.APPROVED") },
          { key: "DECLINED", label: t("einvoices.request.DECLINED") },
          { key: "ALL", label: t("einvoices.queueAll") },
        ]}
        onSelect={(key) => {
          setChoice(key);
          void read.refetch();
        }}
      />
      <Show when={failureOf(read)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <DataTable
        rows={read.value()?.rows ?? []}
        empty={<p class="text-sm text-ink-muted">{t("einvoices.queueEmpty")}</p>}
        pageSize={25}
        searchText={(row) => `${read.value()?.stores.get(row.store_id) ?? row.store_id} ${row.bill_id}`}
        columns={[
          { key: "store", header: t("einvoices.colStore"), cell: (row) => <>{read.value()?.stores.get(row.store_id) ?? row.store_id}</> },
          { key: "bill", header: t("einvoices.colBill"), cell: (row) => <span class="font-mono">{shortId(row.bill_id)}</span> },
          { key: "amount", header: t("einvoices.colTotal"), cell: (row) => <>{formatMinor(row.amount_minor, exponentOf(row.currency_code))} {row.currency_code}</> },
          { key: "kind", header: t("einvoices.colKind"), cell: (row) => <>{t(`einvoices.kind.${row.kind}` as never)}</> },
          { key: "reason", header: t("einvoices.colReason"), cell: (row) => <>{row.reason_code_id ? (read.value()?.reasons.get(row.reason_code_id) ?? shortId(row.reason_code_id)) : "—"}</> },
          { key: "by", header: t("einvoices.colRequestedBy"), cell: (row) => <span class="font-mono">{shortId(row.requested_by)}</span> },
          { key: "time", header: t("einvoices.colRequestedAt"), cell: (row) => <>{formatInstant(row.requested_time_ms)}</>, sortValue: (row) => row.requested_time_ms },
          {
            key: "status",
            header: t("einvoices.colDecision"),
            cell: (row) => (
              <span class="flex flex-col gap-1">
                <StatusBadge tone={requestTone(row.status)} label={t(`einvoices.request.${row.status}` as never)} />
                <Show when={row.decided_by_user_id}>
                  {(user) => <span class="text-xs text-ink-muted">{t("einvoices.decidedBy", { user: shortId(user()) })}</span>}
                </Show>
              </span>
            ),
          },
        ]}
        actions={(row) => (
          <Show when={row.status === "PENDING"}>
            <div class="flex gap-2">
              <Button onClick={() => void decide(row, "approve")} disabled={busy() !== null}>
                {t("einvoices.queueApprove")}
              </Button>
              <Button variant="secondary" onClick={() => void decide(row, "decline")} disabled={busy() !== null}>
                {t("einvoices.queueDecline")}
              </Button>
            </div>
          </Show>
        )}
        actionsHeader={t("einvoices.colActions")}
      />
    </div>
  );
}
