// Device activation codes (ADR-0050, ADR-0065): issue a one-time code a store device exchanges once
// for its credentials. The device is now chosen (or created) by **name** from the registry — no more
// typing a raw device-slot ULID. Names the tenant + store in context; the code is shown once.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Device } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

// The device kinds offered when adding one, each mapped to a static i18n key. `kind` is free text on
// the wire, so this is a convenience list, not a closed set.
const KINDS: readonly { wire: string; key: MessageKey }[] = [
  { wire: "pos", key: "device.kind.pos" },
  { wire: "printer", key: "device.kind.printer" },
  { wire: "kds", key: "device.kind.kds" },
  { wire: "tablet", key: "device.kind.tablet" },
];

export function Activation() {
  const [devices, setDevices] = createSignal<Device[] | null>(null);
  const [selected, setSelected] = createSignal("");
  const [code, setCode] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [newName, setNewName] = createSignal("");
  const [newKind, setNewKind] = createSignal("pos");

  const fail = (caught: unknown) =>
    setError(caught instanceof ApiError ? caught.message : String(caught));

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setDevices(await api.listDevices(tenantId(), storeId()));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant/store changes — never with an empty context (F0).
  onScopedContext("store", () => void load());

  const createDevice = async () => {
    const name = newName().trim();
    if (!name) {
      setError(t("activation.deviceNameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      const device = await api.createDevice(tenantId(), storeId(), name, newKind());
      setNewName("");
      setSelected(device.device_id);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const issue = async () => {
    if (!selected()) {
      setError(t("activation.deviceRequired"));
      return;
    }
    setError("");
    setCode("");
    setBusy(true);
    try {
      const issued = await api.issueActivation(tenantId(), storeId(), selected());
      setCode(issued.activation_code);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("activation.title")} description={t("activation.description")} />
      <RequireContext need="store">
        <div class="flex flex-col gap-6">
          <Card
            title={t("activation.devices")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <div class="flex max-w-md flex-col gap-4">
              <Show
                when={devices()}
                fallback={<p class="text-sm text-ink-muted">{t("activation.loadHint")}</p>}
              >
                {(loaded) => (
                  <Show
                    when={loaded().length > 0}
                    fallback={<p class="text-sm text-ink-muted">{t("activation.noDevices")}</p>}
                  >
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("activation.deviceSelect")}
                      </span>
                      <select
                        class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        value={selected()}
                        onChange={(event) => setSelected(event.currentTarget.value)}
                      >
                        <option value="">{t("activation.chooseDevice")}</option>
                        <For each={loaded()}>
                          {(device) => (
                            <option value={device.device_id}>
                              {device.name} · {device.kind}
                            </option>
                          )}
                        </For>
                      </select>
                    </label>
                  </Show>
                )}
              </Show>

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

              <Button disabled={busy() || !selected()} onClick={() => void issue()}>
                {t("action.issue")}
              </Button>
            </div>
          </Card>

          <Card title={t("activation.createDevice")}>
            <div class="flex max-w-md flex-col gap-4">
              <TextField
                label={t("activation.deviceName")}
                placeholder={t("activation.deviceNamePlaceholder")}
                value={newName()}
                onInput={setNewName}
              />
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">
                  {t("activation.deviceKind")}
                </span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={newKind()}
                  onChange={(event) => setNewKind(event.currentTarget.value)}
                >
                  <For each={KINDS}>
                    {(kind) => <option value={kind.wire}>{t(kind.key)}</option>}
                  </For>
                </select>
              </label>
              <Button disabled={busy()} onClick={() => void createDevice()}>
                {t("action.add")}
              </Button>
            </div>
          </Card>
        </div>
      </RequireContext>
    </div>
  );
}
