// The printer/KDS onboarding queue (ADR-0041): a store reports the devices it found on its network;
// the super-admin approves or rejects each pending proposal here. Tenant-scoped.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { DeviceProposalSummary } from "../api/types";
import { t } from "../i18n";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";

export function Devices() {
  const [rows, setRows] = createSignal<DeviceProposalSummary[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setRows(await api.listProposals(tenantId()));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  const decide = async (id: string, approve: boolean) => {
    setBusy(true);
    try {
      await (approve ? api.approveDevice(tenantId(), id) : api.rejectDevice(tenantId(), id));
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("devices.title")} description={t("devices.description")} />
      <Show
        when={tenantId()}
        fallback={<Banner tone="danger" message={t("context.tenantRequired")} />}
      >
        <Card
          title={t("devices.pending")}
          actions={
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.load")}
            </Button>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={rows()}>
            {(loaded) => (
              <Show
                when={loaded().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("devices.empty")}</p>}
              >
                <div class="overflow-x-auto">
                  <table class="w-full text-left text-sm">
                    <thead>
                      <tr class="border-b border-line text-ink-muted">
                        <th class="py-2 pr-4 font-medium">{t("devices.id")}</th>
                        <th class="py-2 pr-4 font-medium">{t("devices.store")}</th>
                        <th class="py-2 pr-4 font-medium">{t("devices.kind")}</th>
                        <th class="py-2 font-medium">{t("devices.decide")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={loaded()}>
                        {(row) => (
                          <tr class="border-b border-line text-ink">
                            <td class="py-2 pr-4 font-mono text-xs">{row.id}</td>
                            <td class="py-2 pr-4 font-mono text-xs">{row.store_id}</td>
                            <td class="py-2 pr-4">{row.kind}</td>
                            <td class="flex gap-2 py-2">
                              <Button disabled={busy()} onClick={() => void decide(row.id, true)}>
                                {t("action.approve")}
                              </Button>
                              <Button
                                variant="danger"
                                disabled={busy()}
                                onClick={() => void decide(row.id, false)}
                              >
                                {t("action.reject")}
                              </Button>
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
      </Show>
    </div>
  );
}
