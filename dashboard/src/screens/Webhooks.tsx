// Webhook endpoints (ADR-0032): list a tenant's endpoints, register a new HTTPS destination for the
// store in context (the URL is vetted server-side by the SSRF guard; the signing secret is shown
// once), and delete one. The cursor shows how far delivery has reached; `disabled` marks an
// auto-disabled endpoint. On the F2 CRUD kit: the endpoints render in a DataTable and a delete is
// gated behind a type-to-confirm ConfirmDialog.

import { createSignal, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { WebhookSummary } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

export function Webhooks() {
  const [rows, setRows] = createSignal<WebhookSummary[] | null>(null);
  const [url, setUrl] = createSignal("");
  const [secret, setSecret] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [pendingDelete, setPendingDelete] = createSignal<WebhookSummary | null>(null);

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setRows(await api.listWebhooks(tenantId()));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const register = async () => {
    setError("");
    setSecret("");
    setBusy(true);
    try {
      const created = await api.registerWebhook(tenantId(), storeId(), url());
      setSecret(created.signing_secret);
      setUrl("");
      toast.ok(t("webhooks.registered"));
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    const endpoint = pendingDelete();
    if (!endpoint) {
      return;
    }
    setBusy(true);
    try {
      await api.deleteWebhook(tenantId(), endpoint.id);
      toast.ok(t("webhooks.deleted"));
      setPendingDelete(null);
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  // Re-enable an endpoint the delivery task auto-disabled after a day of failures; delivery then
  // resumes from the endpoint's stored cursor, so nothing in the backlog is skipped (ADR-0032).
  const reenable = async (id: string) => {
    setError("");
    setBusy(true);
    try {
      await api.enableWebhook(tenantId(), id);
      toast.ok(t("webhooks.reenabled"));
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<WebhookSummary>[] => [
    {
      key: "url",
      header: t("webhooks.urlLabel"),
      cell: (row) => row.url,
      class: "break-all",
    },
    {
      key: "status",
      header: t("webhooks.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.disabled ? "disabled" : "active"}
          label={row.disabled ? t("webhooks.disabled") : t("webhooks.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("webhooks.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("webhooks.title")} description={t("webhooks.description")} />
      <RequireContext need="tenant">
        <div class="grid gap-6 lg:grid-cols-2">
          <Card
            title={t("webhooks.list")}
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
                  empty={<EmptyState title={t("webhooks.empty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <div class="flex shrink-0 gap-2">
                      <Show when={row.disabled}>
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => void reenable(row.id)}
                        >
                          {t("webhooks.reenable")}
                        </Button>
                      </Show>
                      <Button
                        variant="danger"
                        disabled={busy()}
                        onClick={() => setPendingDelete(row)}
                      >
                        {t("action.delete")}
                      </Button>
                    </div>
                  )}
                />
              )}
            </Show>
          </Card>

          <Card title={t("webhooks.register")}>
            <Show
              when={storeId()}
              fallback={<Banner tone="danger" message={t("context.storeRequired")} />}
            >
              <div class="flex flex-col gap-4">
                <TextField
                  label={t("webhooks.urlLabel")}
                  type="url"
                  placeholder={t("webhooks.urlPlaceholder")}
                  value={url()}
                  onInput={setUrl}
                />
                <Show when={error()}>
                  {(message) => <Banner tone="danger" message={message()} />}
                </Show>
                <Show when={secret()}>
                  {(value) => (
                    <div>
                      <Banner tone="ok" message={t("webhooks.secretOnce")} />
                      <code class="mt-2 block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
                        {value()}
                      </code>
                    </div>
                  )}
                </Show>
                <Button disabled={busy() || !url()} onClick={() => void register()}>
                  {t("action.register")}
                </Button>
              </div>
            </Show>
          </Card>
        </div>

        <ConfirmDialog
          open={pendingDelete() !== null}
          title={t("webhooks.deleteTitle")}
          message={t("webhooks.deleteMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          typeToConfirm={pendingDelete()!.url}
          typePrompt={t("webhooks.deleteTypePrompt")}
          onConfirm={() => void remove()}
          onCancel={() => setPendingDelete(null)}
        />
      </RequireContext>
    </div>
  );
}
