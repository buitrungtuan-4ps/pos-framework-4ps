import { For, Show, createSignal, onMount } from "solid-js";

import { ApiError, api } from "../api/client";
import type { PairedDevice } from "../api/types";
import { PageHeader } from "../components/ui";
import { locale, t } from "../i18n";

// Retiring a till (ADR-0091, production-readiness O1). `POST /api/pair/revoke` and
// `GET /api/pair/devices` have been mounted since the durable-auth slice and nothing called either,
// so a store whose tablet walked out the door had no way to lock it out — pairings are durable by
// design, and nothing expires them.
//
// The edge does not know a device's *name*: that lives in the cloud's approved-device registry, and
// a store that has never synced has none. So a row is identified by when it paired, and the tablet
// in the operator's hand is marked — together enough to recognise the one that is missing without
// reaching for the break-glass.
//
// Behind the paired-device gate rather than an operator login: the edge has no operator identity
// offline (the console is a browser on the LAN), so this is as strong as pairing and no stronger,
// and every revoke is written to the store's log.

// The paired instant, in the reader's own language. A date and a time, because two tills paired on
// the same afternoon are told apart by the clock, not the day.
function pairedAt(ms: number): string {
  return new Date(ms).toLocaleString(locale() === "vi" ? "vi-VN" : "en-GB", {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

// What the operator must type to confirm the break-glass, so retiring the whole store cannot be a
// mistap. Deliberately a word from the interface rather than free text.
const CONFIRM_ALL = "ALL";

export function Devices() {
  const [devices, setDevices] = createSignal<readonly PairedDevice[]>([]);
  const [durable, setDurable] = createSignal(true);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [confirming, setConfirming] = createSignal<string | null>(null);
  const [confirmAll, setConfirmAll] = createSignal("");

  const load = async () => {
    try {
      const state = await api.pairedDevices();
      setDevices(state.paired);
      setDurable(state.durable);
      setError(null);
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  onMount(() => void load());

  // Retire one device, or every device when `deviceId` is null. A failure is shown rather than
  // swallowed: a `503` means the durable registry could not be written, so the device may still be
  // paired after a restart — an operator told a lost tablet is locked out when it is not is worse
  // than one told to try again.
  const retire = async (deviceId: string | null) => {
    setBusy(true);
    setError(null);
    try {
      await api.revokeDevice(deviceId);
      setConfirming(null);
      setConfirmAll("");
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="mx-auto max-w-2xl p-4">
      <PageHeader title={t("devices.title")} />
      <p class="text-ink-muted">{t("devices.hint")}</p>

      <Show when={!durable()}>
        <p class="mt-3 rounded-token border border-awaiting px-3 py-2 text-ink" role="status">
          {t("devices.not_durable")}
        </p>
      </Show>

      <Show when={error()}>
        {(message) => (
          <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <ul class="mt-4 flex flex-col gap-2">
        <For
          each={devices()}
          fallback={<li class="text-ink-muted">{t("devices.none")}</li>}
        >
          {(device) => (
            <li class="rounded-token border border-line bg-surface p-3">
              <div class="flex flex-wrap items-baseline justify-between gap-2">
                <span class="font-semibold text-ink">
                  {device.this_device ? t("devices.this_device") : t("devices.a_device")}
                </span>
                <span class="text-sm text-ink-muted">
                  {t("devices.paired_at", { moment: pairedAt(device.paired_at_ms) })}
                </span>
              </div>
              <p class="mt-1 break-all font-mono text-xs text-ink-muted">{device.device_id}</p>
              <Show
                when={confirming() === device.device_id}
                fallback={
                  <button
                    type="button"
                    class="mt-2 min-h-touch rounded-token border border-line px-3 text-ink disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => setConfirming(device.device_id)}
                  >
                    {t("devices.retire")}
                  </button>
                }
              >
                <div class="mt-2 flex flex-wrap items-center gap-2">
                  <span class="text-sm text-ink">
                    {device.this_device ? t("devices.confirm_self") : t("devices.confirm")}
                  </span>
                  <button
                    type="button"
                    class="min-h-touch rounded-token bg-danger px-3 font-semibold text-danger-ink disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => void retire(device.device_id)}
                  >
                    {t("devices.retire_confirm")}
                  </button>
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line px-3 text-ink"
                    onClick={() => setConfirming(null)}
                  >
                    {t("common.cancel")}
                  </button>
                </div>
              </Show>
            </li>
          )}
        </For>
      </ul>

      <div class="mt-6 rounded-token border border-danger p-3">
        <p class="font-semibold text-ink">{t("devices.all_title")}</p>
        <p class="mt-1 text-sm text-ink-muted">{t("devices.all_hint")}</p>
        <label class="mt-2 block text-sm text-ink-muted" for="retire-all-confirm">
          {t("devices.all_type", { word: CONFIRM_ALL })}
        </label>
        <input
          id="retire-all-confirm"
          class="mt-1 w-full rounded-token border border-line bg-surface p-2 text-ink"
          value={confirmAll()}
          onInput={(event) => setConfirmAll(event.currentTarget.value)}
        />
        <button
          type="button"
          class="mt-2 min-h-touch w-full rounded-token bg-danger font-semibold text-danger-ink disabled:opacity-50"
          disabled={busy() || confirmAll().trim().toUpperCase() !== CONFIRM_ALL}
          onClick={() => void retire(null)}
        >
          {t("devices.all_confirm")}
        </button>
      </div>
    </section>
  );
}
