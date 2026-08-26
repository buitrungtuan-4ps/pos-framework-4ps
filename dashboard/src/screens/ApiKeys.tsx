// Per-tenant API-key provisioning (ADR-0037), on the F2 CRUD kit: list a tenant's keys in a sortable
// table, issue a new scoped key (the token is shown exactly once — only its hash is stored), and
// revoke one behind a confirmation. Deny-by-default: a key grants only the scopes ticked here.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { ApiKeySummary } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

const SCOPES: readonly { wire: string; key: MessageKey }[] = [
  { wire: "read_rollups", key: "scope.read_rollups" },
  { wire: "read_config", key: "scope.read_config" },
  { wire: "place_orders", key: "scope.place_orders" },
  { wire: "manage_devices", key: "scope.manage_devices" },
];

export function ApiKeys() {
  const [rows, setRows] = createSignal<ApiKeySummary[] | null>(null);
  const [chosen, setChosen] = createSignal<Set<string>>(new Set());
  const [token, setToken] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [pendingRevoke, setPendingRevoke] = createSignal<ApiKeySummary | null>(null);

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setRows(await api.listApiKeys(tenantId()));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const toggle = (wire: string) => {
    const next = new Set(chosen());
    if (next.has(wire)) {
      next.delete(wire);
    } else {
      next.add(wire);
    }
    setChosen(next);
  };

  const create = async () => {
    setError("");
    setToken("");
    setBusy(true);
    try {
      const created = await api.createApiKey(tenantId(), [...chosen()]);
      setToken(created.token);
      setChosen(new Set<string>());
      toast.ok(t("apiKeys.created"));
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const revoke = async () => {
    const key = pendingRevoke();
    if (!key) {
      return;
    }
    setBusy(true);
    try {
      await api.revokeApiKey(key.id);
      toast.ok(t("apiKeys.revokeDone"));
      setPendingRevoke(null);
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<ApiKeySummary>[] => [
    {
      key: "scopes",
      header: t("apiKeys.scopes"),
      cell: (row) => <span class="text-ink-muted">{row.scopes.join(", ")}</span>,
    },
    {
      key: "status",
      header: t("apiKeys.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.revoked ? "disabled" : "active"}
          label={row.revoked ? t("status.revoked") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("apiKeys.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("apiKeys.title")} description={t("apiKeys.description")} />
      <RequireContext need="tenant">
        <div class="grid gap-6 lg:grid-cols-2">
          <Card
            title={t("apiKeys.list")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show when={rows()}>
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => `${row.id} ${row.scopes.join(" ")}`}
                  pageSize={12}
                  empty={<EmptyState title={t("apiKeys.empty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <Show when={!row.revoked}>
                      <Button
                        variant="danger"
                        disabled={busy()}
                        onClick={() => setPendingRevoke(row)}
                      >
                        {t("action.revoke")}
                      </Button>
                    </Show>
                  )}
                />
              )}
            </Show>
          </Card>

          <Card title={t("apiKeys.create")}>
            <fieldset class="mb-4 flex flex-col gap-2">
              <legend class="mb-1 text-sm font-medium text-ink">{t("apiKeys.scopesLabel")}</legend>
              <For each={SCOPES}>
                {(scope) => (
                  <label class="flex items-center gap-2 text-sm text-ink">
                    <input
                      type="checkbox"
                      class="size-4"
                      checked={chosen().has(scope.wire)}
                      onChange={() => toggle(scope.wire)}
                    />
                    {t(scope.key)}
                  </label>
                )}
              </For>
            </fieldset>
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <Show when={token()}>
              {(value) => (
                <div class="my-2">
                  <Banner tone="ok" message={t("apiKeys.tokenOnce")} />
                  <code class="mt-2 block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
                    {value()}
                  </code>
                </div>
              )}
            </Show>
            <Button disabled={busy() || chosen().size === 0} onClick={() => void create()}>
              {t("action.create")}
            </Button>
          </Card>
        </div>

        <ConfirmDialog
          open={pendingRevoke() !== null}
          title={t("apiKeys.revokeTitle")}
          message={t("apiKeys.revokeMessage")}
          confirmLabel={t("action.revoke")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void revoke()}
          onCancel={() => setPendingRevoke(null)}
        />
      </RequireContext>
    </div>
  );
}
