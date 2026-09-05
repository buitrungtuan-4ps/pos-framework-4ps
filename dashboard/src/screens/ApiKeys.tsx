// Per-tenant API-key provisioning (ADR-0037), on the F2 CRUD kit: list a tenant's keys in a sortable
// table, issue a new scoped key (the token is shown exactly once — only its hash is stored), and
// revoke one behind a confirmation. Deny-by-default: a key grants only the scopes ticked here.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { ApiKeySummary, Store } from "../api/types";
import { locale, type MessageKey, t } from "../i18n";
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

// The scopes that gate a live route. `relay_orders` belongs here because a store key without it
// leaves the order relay answering 403 on every poll while config-pull works fine — a half-connected
// store whose only symptom is a log line (roadmap-v3 E6).
//
// This list is now the whole vocabulary. `read_events` and `manage_webhooks` used to be absent from
// it while still existing in the cloud's `Scope` enum — so the picker was right and the API was not:
// `POST /admin/api-keys` accepted either name and issued a key whose scope list promised an
// authority no route consulted. Roadmap **Q5** removed both variants, so the API refuses them too.
const SCOPES: readonly { wire: string; key: MessageKey }[] = [
  { wire: "read_rollups", key: "scope.read_rollups" },
  { wire: "read_config", key: "scope.read_config" },
  { wire: "relay_orders", key: "scope.relay_orders" },
  { wire: "place_orders", key: "scope.place_orders" },
  { wire: "manage_devices", key: "scope.manage_devices" },
];

export function ApiKeys() {
  const [rows, setRows] = createSignal<ApiKeySummary[] | null>(null);
  const [stores, setStores] = createSignal<Store[]>([]);
  // Which store this key belongs to; "" is a tenant-wide integration key. A store's own credential
  // must name its store, because `/sync/stores/{id}/…` refuses a key that does not (S1).
  const [forStore, setForStore] = createSignal("");
  const [chosen, setChosen] = createSignal<Set<string>>(new Set());
  const [token, setToken] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [pendingRevoke, setPendingRevoke] = createSignal<ApiKeySummary | null>(null);

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [keys, registered] = await Promise.all([
        api.listApiKeys(tenantId()),
        api.listStores(tenantId()),
      ]);
      setStores(registered);
      setRows(keys);
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
      const created = await api.createApiKey(tenantId(), [...chosen()], forStore() || undefined);
      setToken(created.token);
      setChosen(new Set<string>());
      setForStore("");
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

  // A key's registered store name, the raw ULID if the registry has no row for it, or the
  // tenant-wide label when it is bound to no store at all.
  const storeLabel = (row: ApiKeySummary) => {
    if (row.store_id === null) {
      return t("apiKeys.tenantWide");
    }
    return stores().find((store) => store.store_id === row.store_id)?.name ?? row.store_id;
  };

  const formatMoment = (ms: number) =>
    new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeStyle: "short" }).format(
      new Date(ms),
    );

  // A key past its expiry stops working, and the console said "Active" (production-readiness O4):
  // `expires_at_ms` was served from the day the key store was written and this screen never read it.
  // Revoked outranks expired — a revoked key was deliberately killed, which is the more useful thing
  // to know about it.
  const isExpired = (row: ApiKeySummary) =>
    row.expires_at_ms !== null && row.expires_at_ms <= Date.now();

  const statusTone = (row: ApiKeySummary) =>
    row.revoked || isExpired(row) ? ("disabled" as const) : ("active" as const);

  const statusLabel = (row: ApiKeySummary): MessageKey => {
    if (row.revoked) {
      return "status.revoked";
    }
    return isExpired(row) ? "status.expired" : "status.active";
  };

  const columns = (): Column<ApiKeySummary>[] => [
    {
      key: "store",
      header: t("apiKeys.store"),
      cell: (row) => <span class="text-ink">{storeLabel(row)}</span>,
      sortValue: (row) => storeLabel(row),
    },
    {
      key: "scopes",
      header: t("apiKeys.scopes"),
      cell: (row) => <span class="text-ink-muted">{row.scopes.join(", ")}</span>,
    },
    {
      key: "status",
      header: t("apiKeys.status"),
      cell: (row) => <StatusBadge tone={statusTone(row)} label={t(statusLabel(row))} />,
      sortValue: (row) => statusLabel(row),
    },
    {
      key: "expires",
      header: t("apiKeys.expires"),
      cell: (row) => (
        <span class="text-ink-muted">
          {row.expires_at_ms === null ? t("apiKeys.neverExpires") : formatMoment(row.expires_at_ms)}
        </span>
      ),
      // Never-expiring keys sort last: a key with an end date is the one an operator is looking for.
      sortValue: (row) => row.expires_at_ms ?? Number.MAX_SAFE_INTEGER,
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
                  searchText={(row) => `${row.id} ${storeLabel(row)} ${row.scopes.join(" ")}`}
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
            <label class="mb-4 block">
              <span class="mb-1 block text-sm font-medium text-ink">{t("apiKeys.storeLabel")}</span>
              <select
                class="w-full rounded-token border border-line bg-surface px-2 py-1.5 text-sm text-ink"
                value={forStore()}
                onChange={(event) => setForStore(event.currentTarget.value)}
              >
                <option value="">{t("apiKeys.tenantWide")}</option>
                <For each={stores()}>
                  {(store) => <option value={store.store_id}>{store.name}</option>}
                </For>
              </select>
              <span class="mt-1 block text-xs text-ink-muted">{t("apiKeys.storeHint")}</span>
            </label>
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
