// The console audit trail (ADR-0069, Track G2 slice 4). A fleet-wide, filterable record of who
// changed what across the console: every /admin mutation appends one entry, and this reads them back
// newest-first. Filters (entity type, action, acting admin) apply on the server; a row's before/after
// opens in a detail drawer. Read-only, behind console.data.read; global by design, so no tenant is
// required to open it.

import { createSignal, onMount, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { AuditEntry } from "../api/types";
import { locale, t } from "../i18n";
import { formatRelativeAge } from "../lib/format";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  DataTable,
  Drawer,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

/** The most rows one read returns; the server caps at 500. */
const AUDIT_LIMIT = 200;

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

/** The absolute instant, in the reader's locale, for the detail drawer. */
function absolute(atMs: number): string {
  return new Date(atMs).toLocaleString(locale());
}

/** A JSON value pretty-printed for the before/after panels, or a placeholder when there is none. */
function pretty(value: unknown): string {
  return value === null || value === undefined
    ? t("audit.noValue")
    : JSON.stringify(value, null, 2);
}

export function Audit() {
  const [entries, setEntries] = createSignal<AuditEntry[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [selected, setSelected] = createSignal<AuditEntry | null>(null);

  // The applied filters. Empty strings mean "no filter"; the client drops absent fields.
  const [entityType, setEntityType] = createSignal("");
  const [action, setAction] = createSignal("");
  const [actor, setActor] = createSignal("");

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const loaded = await api.listAudit({
        entityType: entityType().trim() || undefined,
        action: action().trim() || undefined,
        actorAdminId: actor().trim() || undefined,
        limit: AUDIT_LIMIT,
      });
      setEntries(loaded);
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  onMount(() => void load());

  const columns = (): Column<AuditEntry>[] => [
    {
      key: "when",
      header: t("audit.when"),
      sortValue: (row) => row.at_ms,
      cell: (row) => (
        <span class="text-sm text-ink-muted" title={absolute(row.at_ms)}>
          {formatRelativeAge(ageSeconds(row.at_ms))}
        </span>
      ),
    },
    {
      key: "actor",
      header: t("audit.actor"),
      sortValue: (row) => row.actor_email,
      cell: (row) => (
        <div class="flex flex-wrap items-center gap-2">
          <span class="text-ink">{row.actor_email}</span>
          <StatusBadge tone="neutral" label={row.actor_role} />
        </div>
      ),
    },
    {
      key: "action",
      header: t("audit.action"),
      sortValue: (row) => row.action,
      cell: (row) => <span class="font-medium text-ink">{row.action}</span>,
    },
    {
      key: "entity",
      header: t("audit.entity"),
      sortValue: (row) => row.entity_type,
      cell: (row) => <span class="text-sm text-ink">{row.entity_type}</span>,
    },
  ];

  const detailRow = (label: string, value: string) => (
    <div class="flex justify-between gap-4 border-b border-line py-2 text-sm last:border-0">
      <span class="text-ink-muted">{label}</span>
      <span class="text-right text-ink">{value}</span>
    </div>
  );

  const jsonPanel = (label: string, value: unknown) => (
    <div>
      <span class="mb-1 block text-sm font-medium text-ink">{label}</span>
      <pre class="max-h-64 overflow-auto rounded-token border border-line bg-surface-raised p-3 font-mono text-xs text-ink">
        {pretty(value)}
      </pre>
    </div>
  );

  return (
    <div>
      <PageHeader title={t("audit.title")} description={t("audit.description")} />
      <Card
        title={t("audit.recent")}
        actions={
          <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
            {t("action.refresh")}
          </Button>
        }
      >
        <div class="mb-4 grid gap-3 sm:grid-cols-3">
          <TextField
            label={t("audit.filter.entityType")}
            value={entityType()}
            onInput={setEntityType}
            placeholder={t("audit.filter.entityTypeHint")}
          />
          <TextField
            label={t("audit.filter.action")}
            value={action()}
            onInput={setAction}
            placeholder={t("audit.filter.actionHint")}
          />
          <TextField
            label={t("audit.filter.actor")}
            value={actor()}
            onInput={setActor}
            placeholder={t("audit.filter.actorHint")}
          />
        </div>
        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={entries()} fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}>
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              searchText={(row) => `${row.action} ${row.actor_email} ${row.entity_type}`}
              pageSize={20}
              empty={<EmptyState title={t("audit.empty")} description={t("audit.emptyHint")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <Button variant="secondary" onClick={() => setSelected(row)}>
                  {t("audit.details")}
                </Button>
              )}
            />
          )}
        </Show>
      </Card>

      <Drawer
        open={selected() !== null}
        title={selected()?.action ?? ""}
        closeLabel={t("action.close")}
        onClose={() => setSelected(null)}
      >
        <Show when={selected()}>
          {(entry) => (
            <div class="flex flex-col gap-4">
              <div>
                {detailRow(t("audit.actor"), entry().actor_email)}
                {detailRow(t("audit.role"), entry().actor_role)}
                {detailRow(t("audit.action"), entry().action)}
                {detailRow(t("audit.entity"), entry().entity_type)}
                {detailRow(t("audit.entityId"), entry().entity_id)}
                {detailRow(t("audit.when"), absolute(entry().at_ms))}
                {detailRow(t("audit.tenant"), entry().tenant_id ?? t("audit.global"))}
              </div>
              {jsonPanel(t("audit.before"), entry().before)}
              {jsonPanel(t("audit.after"), entry().after)}
              <TechnicalDetails label={t("common.technicalDetails")}>
                {entry().id}
              </TechnicalDetails>
            </div>
          )}
        </Show>
      </Drawer>
    </div>
  );
}
