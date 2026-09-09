// Per-tenant API-key provisioning (ADR-0037), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)): list a tenant's keys in a
// sortable table, issue a new scoped key, and revoke one behind a confirmation. Deny-by-default: a
// key grants only the scopes ticked here.
//
// The token is shown **exactly once** — only its hash is stored — and that is the one thing on this
// screen the panel could not carry. `FormPanel` closes on success, which is right for a form and
// wrong for a result an operator has to copy before it is gone forever. So the token is a second,
// separate `Modal`, opened as the panel closes: "here is your key, copy it now" is a different
// sentence from "fill this in", and giving it its own dialog says so. The alternative — a
// `keepOpen` flag on `FormPanel` for this one screen — is how a kit starts collecting special
// cases, which is what ADR-0121 exists to stop.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type { ApiKeySummary, Store } from "../api/types";
import { locale, type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  PageHeader,
  SelectField,
  StatusBadge,
} from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  FormPanel,
  Modal,
  TechnicalDetails,
} from "../components/kit";
import { useEntityCrud } from "../lib/entity-crud";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";

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
  // The issued token, held only until the operator dismisses it. Nothing persists it, here or
  // anywhere: the cloud stored the hash and cannot show this again.
  const [token, setToken] = createSignal("");
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  // Two lifecycles, so revoking one key does not disable the issue form (ADR-0121 §3).
  const issue = useEntityCrud<ApiKeySummary>();
  const revocation = useEntityCrud<ApiKeySummary>();

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      const [keys, registered] = await Promise.all([
        api.listApiKeys(tenantId()),
        api.listStores(tenantId()),
      ]);
      setStores(registered);
      setRows(keys);
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setLoading(false);
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

  const openIssue = () => {
    setForStore("");
    setChosen(new Set<string>());
    issue.create();
  };

  const submitIssue = () => {
    if (chosen().size === 0) {
      issue.refuse(t("apiKeys.scopeRequired"));
      return;
    }
    void issue
      .run(async () => {
        const created = await api.createApiKey(tenantId(), [...chosen()], forStore() || undefined);
        // Set inside the write, so the token exists before `run` closes the panel and the second
        // dialog can open in the same tick. It is the only copy that will ever be shown.
        setToken(created.token);
      })
      .then((issued) => {
        if (issued) {
          toast.ok(t("apiKeys.created"));
          void load();
        }
      });
  };

  const revoke = () => {
    const key = revocation.subject();
    if (!key) {
      return;
    }
    void revocation.run(() => api.revokeApiKey(key.id)).then((revoked) => {
      if (revoked) {
        toast.ok(t("apiKeys.revokeDone"));
        void load();
      } else {
        // The confirm closed itself only on success; a refusal leaves it open with the message, and
        // the page banner would duplicate it.
        toast.error(revocation.error());
      }
    });
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
        <div class="flex flex-col gap-6">
          <Card
            title={t("apiKeys.list")}
            actions={
              <div class="flex gap-2">
                <Button onClick={openIssue}>{t("apiKeys.create")}</Button>
                <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
                  {t("action.refresh")}
                </Button>
              </div>
            }
          >
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
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
                        disabled={revocation.saving()}
                        onClick={() => revocation.confirm(row)}
                      >
                        {t("action.revoke")}
                      </Button>
                    </Show>
                  )}
                />
              )}
            </Show>
          </Card>

        </div>

        <FormPanel
          crud={issue}
          createTitle={t("apiKeys.create")}
          editTitle={t("apiKeys.create")}
          submitLabel={t("action.create")}
          onSubmit={submitIssue}
          dirty={() => chosen().size > 0 || forStore() !== ""}
        >
          <SelectField
            label={t("apiKeys.storeLabel")}
            value={forStore()}
            options={stores().map((store) => ({ value: store.store_id, label: store.name }))}
            onChange={setForStore}
            placeholder={t("apiKeys.tenantWide")}
            hint={t("apiKeys.storeHint")}
          />
          <fieldset class="flex flex-col gap-2">
            <legend class="mb-1 text-sm font-medium text-ink">{t("apiKeys.scopesLabel")}</legend>
            <For each={SCOPES}>
              {(scope) => (
                <CheckboxField
                  label={t(scope.key)}
                  checked={chosen().has(scope.wire)}
                  onChange={() => toggle(scope.wire)}
                />
              )}
            </For>
          </fieldset>
        </FormPanel>

        {/* The token, once. A separate dialog from the form that made it, because it says a
            different thing: the form asked for input, this hands back something that cannot be
            recovered. Dismissing it drops the only copy the console will ever hold. */}
        <Modal
          open={token() !== ""}
          title={t("apiKeys.tokenTitle")}
          closeLabel={t("action.close")}
          onClose={() => setToken("")}
          footer={<Button onClick={() => setToken("")}>{t("action.close")}</Button>}
        >
          <Banner tone="ok" message={t("apiKeys.tokenOnce")} />
          <code class="mt-3 block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
            {token()}
          </code>
        </Modal>

        <ConfirmDialog
          open={revocation.mode() === "confirming"}
          title={t("apiKeys.revokeTitle")}
          message={t("apiKeys.revokeMessage")}
          confirmLabel={t("action.revoke")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={revocation.saving()}
          onConfirm={revoke}
          onCancel={revocation.close}
        />
      </RequireContext>
    </div>
  );
}
