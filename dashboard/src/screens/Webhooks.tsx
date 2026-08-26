// Webhook endpoints (ADR-0032): list a tenant's endpoints, register a new HTTPS destination for the
// store in context (the URL is vetted server-side by the SSRF guard; the signing secret is shown
// once), and delete one. The cursor shows how far delivery has reached; `disabled` marks an
// auto-disabled endpoint.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { WebhookSummary } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

export function Webhooks() {
  const [rows, setRows] = createSignal<WebhookSummary[] | null>(null);
  const [url, setUrl] = createSignal("");
  const [secret, setSecret] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

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
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: string) => {
    setBusy(true);
    try {
      await api.deleteWebhook(tenantId(), id);
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
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
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

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
                <Show
                  when={loaded().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("webhooks.empty")}</p>}
                >
                  <ul class="flex flex-col gap-3">
                    <For each={loaded()}>
                      {(row) => (
                        <li class="flex items-start justify-between gap-3 border-b border-line pb-3">
                          <div class="min-w-0">
                            <p class="truncate text-sm text-ink">{row.url}</p>
                            <p class="text-xs text-ink-muted">
                              {row.disabled ? t("webhooks.disabled") : t("webhooks.active")}
                            </p>
                          </div>
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
                              onClick={() => void remove(row.id)}
                            >
                              {t("action.delete")}
                            </Button>
                          </div>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
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
      </RequireContext>
    </div>
  );
}
