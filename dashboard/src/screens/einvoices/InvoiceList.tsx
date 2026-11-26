// The store's e-invoices and where each one's submission stands (ADR-0241), with a correction by an
// adjustment or a replacement (ADR-0225 decision 7). A paid bill with an issued invoice is corrected
// here, never voided at the store. No buyer is shown: the list names none.

import { createSignal, Show } from "solid-js";

import { correctEInvoice, type EInvoiceView, listEInvoices } from "../../api/einvoices";
import { t } from "../../i18n";
import { apiMessage } from "../../lib/errors";
import { formatMinor } from "../../lib/format";
import { createAdminResource, failureOf } from "../../lib/resource";
import { tenantId } from "../../state/session";
import { Banner, Button, NumberField, SelectField, StatusBadge } from "../../components/ui";
import { DataTable, Drawer } from "../../components/kit";
import { toast } from "../../components/Toast";

/** The badge tone a submission state reads as. */
export function stateTone(state: string): "active" | "danger" | "neutral" {
  if (state === "SUBMITTED") {
    return "active";
  }
  return state === "REJECTED" ? "danger" : "neutral";
}

/**
 * Whether the invoice waits for the vendor's number: the store issued it "number pending" and the
 * cloud holds a placeholder until the vendor numbers it (ADR-0251). It cannot be corrected yet.
 */
export function numberPending(number: string): boolean {
  return number.startsWith("PENDING/");
}

/** The number column's text: the number, or that the vendor has not yet given one. */
export function numberText(number: string): string {
  return numberPending(number) ? t("einvoices.numberPending") : number;
}

/** The money exponent of the currencies the pilot sells in; VND and JPY have none. */
function exponentOf(currency: string): number {
  return currency === "VND" || currency === "JPY" ? 0 : 2;
}

function CorrectionForm(props: { invoice: EInvoiceView; onDone: () => Promise<void> }) {
  const [kind, setKind] = createSignal<"ADJUSTMENT" | "REPLACEMENT">("ADJUSTMENT");
  const [total, setTotal] = createSignal<number | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const send = async () => {
    const amount = total();
    if (amount === null) {
      setError(t("einvoices.correctAmountRequired"));
      return;
    }
    setBusy(true);
    setError("");
    try {
      const made = await correctEInvoice(tenantId(), props.invoice.invoice_id, kind(), amount);
      toast.ok(t("einvoices.corrected", { number: made.invoice_number }));
      await props.onDone();
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="mt-3 grid max-w-xl gap-3 rounded-token border border-line p-3" data-testid="einvoice-correct">
      <p class="text-sm text-ink-muted">{t("einvoices.correctHint", { number: props.invoice.invoice_number })}</p>
      <SelectField
        label={t("einvoices.correctKind")}
        value={kind()}
        options={[
          { value: "ADJUSTMENT", label: t("einvoices.kind.ADJUSTMENT") },
          { value: "REPLACEMENT", label: t("einvoices.kind.REPLACEMENT") },
        ]}
        onChange={(value) => setKind(value === "REPLACEMENT" ? "REPLACEMENT" : "ADJUSTMENT")}
      />
      <NumberField
        label={kind() === "REPLACEMENT" ? t("einvoices.correctTotal") : t("einvoices.correctChange")}
        value={total()}
        onChange={setTotal}
        step={1}
      />
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <div>
        <Button onClick={() => void send()} disabled={busy()}>
          {t("einvoices.correctSend")}
        </Button>
      </div>
    </div>
  );
}

export function InvoiceList() {
  const read = createAdminResource((tenant, store) => (store ? listEInvoices(tenant, store) : Promise.resolve([])), {
    scope: "tenant",
  });
  const [open, setOpen] = createSignal<EInvoiceView | null>(null);
  return (
    <>
      <Show when={failureOf(read)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <DataTable
        rows={read.value() ?? []}
        empty={<p class="text-sm text-ink-muted">{t("einvoices.listEmpty")}</p>}
        pageSize={25}
        searchText={(row) => `${row.invoice_number} ${row.state}`}
        columns={[
          { key: "number", header: t("einvoices.colNumber"), cell: (row) => <span class="font-mono">{numberText(row.invoice_number)}</span>, sortValue: (row) => row.invoice_number },
          { key: "date", header: t("einvoices.colDate"), cell: (row) => <>{row.issued_on}</>, sortValue: (row) => row.issued_on },
          { key: "kind", header: t("einvoices.colKind"), cell: (row) => <>{t(`einvoices.kind.${row.kind}` as never)}</> },
          { key: "total", header: t("einvoices.colTotal"), cell: (row) => <>{formatMinor(row.total_minor, exponentOf(row.currency))} {row.currency}</> },
          { key: "state", header: t("einvoices.colState"), cell: (row) => <StatusBadge tone={stateTone(row.state)} label={t(`einvoices.state.${row.state}` as never)} /> },
          { key: "detail", header: t("einvoices.colDetail"), cell: (row) => <span class="text-sm text-ink-muted">{row.refusal ?? row.authority_reference ?? row.corrects_invoice_number ?? ""}</span> },
        ]}
        actions={(row) => (
          <Show when={!numberPending(row.invoice_number)}>
            <Button variant="ghost" onClick={() => setOpen(row)}>{t("einvoices.correct")}</Button>
          </Show>
        )}
        actionsHeader={t("einvoices.colActions")}
      />
      <Drawer
        open={open() !== null}
        title={t("einvoices.correctTitle")}
        closeLabel={t("action.close")}
        onClose={() => setOpen(null)}
      >
        <Show when={open()}>
          {(invoice) => (
            <CorrectionForm
              invoice={invoice()}
              onDone={async () => {
                setOpen(null);
                await read.refetch();
              }}
            />
          )}
        </Show>
      </Drawer>
    </>
  );
}
