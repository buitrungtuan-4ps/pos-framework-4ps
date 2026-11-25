// E-invoices for one store (ADR-0225, ADR-0229): the symbol the store numbers its e-invoices under.
//
// A Vietnamese store that must issue an invoice refuses to settle until the cloud publishes its
// symbol, so this is the screen HQ opens before a store trades and again after its box is replaced.
// The symbol is not a setting: it names one issuing point, is written on the store's own `einvoice`
// node, is checked against the store's country by the cloud and is audited there. The screen shows
// what is published now, takes a new one, and shows the store's audit trail beside it.

import { createEffect, createSignal, Show } from "solid-js";

import { api } from "../api/client";
import { allocateEInvoiceSymbol, publishEInvoiceSymbol } from "../api/einvoices";
import type { Json } from "../api/types";
import { t } from "../i18n";
import { RequireContext } from "../lib/scoped";
import { createAdminResource, failureOf } from "../lib/resource";
import { apiMessage } from "../lib/errors";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, StatusBadge, TextField } from "../components/ui";
import { AuditTrail } from "../components/AuditTrail";
import { toast } from "../components/Toast";
import { InvoiceList } from "./einvoices/InvoiceList";

/** The `einvoice` node's published fields this screen reads, from the store's effective tree. */
export type PublishedInvoicing = {
  readonly symbol: string | null;
  readonly issueMoment: string | null;
  readonly country: string | null;
};

type JsonObject = Record<string, Json>;

function objectAt(from: Json | null | undefined): JsonObject {
  return from !== null && from !== undefined && typeof from === "object" && !Array.isArray(from)
    ? from
    : {};
}

function textAt(from: JsonObject, key: string): string | null {
  const value = from[key];
  return typeof value === "string" && value !== "" ? value : null;
}

/** Reads the symbol, the issue moment and the country out of an effective tree; absent is `null`. */
export function readInvoicing(effective: Json | null): PublishedInvoicing {
  const root = objectAt(effective);
  const einvoice = objectAt(root.einvoice);
  return {
    symbol: textAt(einvoice, "symbol"),
    issueMoment: textAt(einvoice, "issue_moment"),
    country: textAt(objectAt(root.locale), "country_code"),
  };
}

/** Whether a store with this issue moment must have a symbol before it can settle. */
export function needsSymbol(issueMoment: string | null): boolean {
  return issueMoment !== null && issueMoment !== "EINVOICE_ISSUE_MOMENT_OFF";
}

/** What is published now, and what it means for the store's payments. */
function SymbolState(props: { state: PublishedInvoicing }) {
  return (
    <>
      <div class="flex flex-wrap items-center gap-3" data-testid="einvoice-symbol-now">
        <span class="text-sm text-ink-muted">{t("einvoices.symbolNow")}</span>
        <Show
          when={props.state.symbol}
          fallback={<StatusBadge tone="danger" label={t("einvoices.symbolNone")} />}
        >
          {(symbol) => <span class="font-mono text-lg text-ink">{symbol()}</span>}
        </Show>
      </div>
      <Show when={props.state.country}>
        {(country) => <p class="text-sm text-ink-muted">{t("einvoices.country", { country: country() })}</p>}
      </Show>
      <Show when={!props.state.symbol && needsSymbol(props.state.issueMoment)}>
        <Banner tone="danger" message={t("einvoices.symbolBlocks")} />
      </Show>
      <Show when={!needsSymbol(props.state.issueMoment)}>
        <p class="text-sm text-ink-muted">{t("einvoices.issueOff")}</p>
      </Show>
    </>
  );
}

/** Takes a symbol by hand and publishes it; `onPublished` reloads what is published. */
function SymbolForm(props: { current: string | null; onPublished: () => Promise<void> }) {
  const [draft, setDraft] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // The field starts from what is published, so changing a symbol starts from the one in force.
  createEffect(() => setDraft(props.current ?? ""));

  const run = async (act: () => Promise<string>) => {
    setBusy(true);
    setError("");
    try {
      const symbol = await act();
      toast.ok(t("einvoices.symbolPublished", { symbol }));
      await props.onPublished();
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    const symbol = draft().trim().toUpperCase();
    if (symbol === "") {
      setError(t("einvoices.symbolRequired"));
      return;
    }
    await run(async () => {
      await publishEInvoiceSymbol(tenantId(), storeId(), symbol);
      return symbol;
    });
  };

  const allocateNext = () =>
    run(async () => (await allocateEInvoiceSymbol(tenantId(), storeId())).symbol);

  return (
    <>
      <TextField
        label={t("einvoices.symbolField")}
        value={draft()}
        onInput={(value) => setDraft(value.toUpperCase())}
        hint={t("einvoices.symbolHint")}
      />
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <div class="flex flex-wrap gap-2">
        <Button onClick={() => void save()} disabled={busy()}>
          {props.current ? t("einvoices.symbolChange") : t("einvoices.symbolSet")}
        </Button>
        <Button variant="secondary" onClick={() => void allocateNext()} disabled={busy()}>
          {t("einvoices.symbolAllocate")}
        </Button>
      </div>
      <p class="text-sm text-ink-muted">{t("einvoices.symbolAllocateHint")}</p>
    </>
  );
}

export function EInvoices() {
  const read = createAdminResource(
    async (tenant, store) => (store ? readInvoicing(await api.effectiveConfig(tenant, store)) : null),
    { scope: "tenant" },
  );

  return (
    <>
      <PageHeader title={t("einvoices.title")} description={t("einvoices.description")} />
      <RequireContext need="store">
        <div class="flex flex-col gap-6">
          <Card title={t("einvoices.symbolTitle")}>
            <Show when={failureOf(read)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <Show when={read.value()}>
              {(state) => (
                <div class="grid max-w-xl gap-4">
                  <SymbolState state={state()} />
                  <SymbolForm current={state().symbol} onPublished={read.refetch} />
                </div>
              )}
            </Show>
          </Card>
          <Card title={t("einvoices.listTitle")}>
            <InvoiceList />
          </Card>
          <Card title={t("einvoices.historyTitle")}>
            <Show when={storeId()}>{(store) => <AuditTrail entityType="store" entityId={store()} />}</Show>
          </Card>
        </div>
      </RequireContext>
    </>
  );
}
