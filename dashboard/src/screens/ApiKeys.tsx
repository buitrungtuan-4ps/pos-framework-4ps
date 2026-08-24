// Per-tenant API-key provisioning (ADR-0037): list a tenant's keys, issue a new scoped key (the
// token is shown exactly once — only its hash is stored), and revoke one. Deny-by-default: a key
// grants only the scopes ticked here.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { ApiKeySummary } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";

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
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  const revoke = async (id: string) => {
    setBusy(true);
    try {
      await api.revokeApiKey(id);
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("apiKeys.title")} description={t("apiKeys.description")} />
      <Show
        when={tenantId()}
        fallback={<Banner tone="danger" message={t("context.tenantRequired")} />}
      >
        <div class="grid gap-6 lg:grid-cols-2">
          <Card
            title={t("apiKeys.list")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.load")}
              </Button>
            }
          >
            <Show when={rows()}>
              {(loaded) => (
                <Show
                  when={loaded().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("apiKeys.empty")}</p>}
                >
                  <div class="overflow-x-auto">
                    <table class="w-full text-left text-sm">
                      <thead>
                        <tr class="border-b border-line text-ink-muted">
                          <th class="py-2 pr-4 font-medium">{t("apiKeys.id")}</th>
                          <th class="py-2 pr-4 font-medium">{t("apiKeys.scopes")}</th>
                          <th class="py-2 font-medium">{t("apiKeys.status")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={loaded()}>
                          {(row) => (
                            <tr class="border-b border-line text-ink">
                              <td class="py-2 pr-4 font-mono text-xs">{row.id}</td>
                              <td class="py-2 pr-4 text-ink-muted">{row.scopes.join(", ")}</td>
                              <td class="py-2">
                                <Show
                                  when={row.revoked}
                                  fallback={
                                    <Button
                                      variant="danger"
                                      disabled={busy()}
                                      onClick={() => void revoke(row.id)}
                                    >
                                      {t("action.revoke")}
                                    </Button>
                                  }
                                >
                                  <span class="text-ink-muted">{t("apiKeys.revoked")}</span>
                                </Show>
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
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
      </Show>
    </div>
  );
}
