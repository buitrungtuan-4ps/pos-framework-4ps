// Device activation codes (ADR-0050): issue a one-time code that a store device exchanges once for
// its credentials. Names the tenant + store in context and the device slot; the code is shown once.

import { createSignal, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import { t } from "../i18n";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

export function Activation() {
  const [deviceId, setDeviceId] = createSignal("");
  const [code, setCode] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const issue = async () => {
    setError("");
    setCode("");
    setBusy(true);
    try {
      const issued = await api.issueActivation(tenantId(), storeId(), deviceId());
      setCode(issued.activation_code);
      setDeviceId("");
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("activation.title")} description={t("activation.description")} />
      <Show
        when={tenantId() && storeId()}
        fallback={<Banner tone="danger" message={t("context.required")} />}
      >
        <Card title={t("activation.issue")}>
          <div class="flex max-w-md flex-col gap-4">
            <TextField
              label={t("activation.deviceId")}
              placeholder={t("activation.deviceIdPlaceholder")}
              value={deviceId()}
              onInput={setDeviceId}
            />
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <Show when={code()}>
              {(value) => (
                <div>
                  <Banner tone="ok" message={t("activation.codeOnce")} />
                  <code class="mt-2 block break-all rounded-token border border-line bg-surface-raised p-2 text-sm text-ink">
                    {value()}
                  </code>
                </div>
              )}
            </Show>
            <Button disabled={busy() || !deviceId()} onClick={() => void issue()}>
              {t("action.issue")}
            </Button>
          </div>
        </Card>
      </Show>
    </div>
  );
}
