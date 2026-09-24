import { For, Show, createSignal, onMount } from "solid-js";

import { api } from "../api/client";
import type { MintedCode, PairedDevice, PrinterEntry } from "../api/types";
import { QrCode } from "../components/QrCode";
import { PageHeader } from "../components/ui";
import { type MessageKey, locale, t } from "../i18n";
import { errorMessage } from "../lib/errors";

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

// Where the new device goes. Built from this browser's own address rather than asked of the server:
// this tablet reached the store somehow, and whatever address worked for it is the address that will
// work for the one standing beside it — on a shop LAN and behind an ADR-0111 public origin alike.
function pairingUrl(code: string): string {
  return `${window.location.origin}/pair?code=${code}`;
}

// Whether this screen was opened by a loopback address — the store PC's own browser at
// `localhost`. The link it builds then points a phone at the phone itself, so the QR would scan and
// go nowhere; the screen says to open it by the PC's LAN address instead.
function onLoopback(): boolean {
  const host = window.location.hostname;
  return host === "localhost" || host === "127.0.0.1" || host === "[::1]";
}

// What a test page's outcome tells a manager, in the words the pay screen uses for a receipt —
// except success, which is a test page rather than a receipt.
function testPrintKey(outcome: string): MessageKey {
  switch (outcome) {
    case "PRINTED":
      return "devices.test_printed";
    case "NO_PRINTER":
      return "pay.print_no_printer";
    case "UNPRINTABLE_TEXT":
      return "pay.print_unprintable";
    case "QUEUED_TO_AGENT":
      return "pay.print_queued";
    case "PRINT_AGENT_UNAVAILABLE":
      return "pay.print_agent_unavailable";
    case "PRINT_QUEUE_FULL":
      return "pay.print_queue_full";
    default:
      return "pay.print_unavailable";
  }
}

export function Devices() {
  const [devices, setDevices] = createSignal<readonly PairedDevice[]>([]);
  const [durable, setDurable] = createSignal(true);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [confirming, setConfirming] = createSignal<string | null>(null);
  const [confirmAll, setConfirmAll] = createSignal("");
  const [minted, setMinted] = createSignal<MintedCode | null>(null);
  const [printers, setPrinters] = createSignal<readonly PrinterEntry[]>([]);
  // The last test page's outcome, per printer.
  const [tested, setTested] = createSignal<Record<string, string>>({});

  const load = async () => {
    try {
      const state = await api.pairedDevices();
      setDevices(state.paired);
      setDurable(state.durable);
      setError(null);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  onMount(() => {
    void load();
    // Forgiving: an edge that predates the route, or a store with nothing published, lists none.
    void api
      .printers()
      .then(setPrinters)
      .catch(() => setPrinters([]));
  });

  // Print a page on one printer (manager only — the edge refuses anyone else, and says so).
  const testPrint = async (deviceId: string) => {
    setBusy(true);
    setError(null);
    try {
      const outcome = await api.testPrinter(deviceId);
      setTested({ ...tested(), [deviceId]: outcome.print });
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

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
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // Mint the code for the next device (ADR-0118). Before this, adding a till meant restarting the
  // store server — which drops every till's and kitchen display's live session — because a code was
  // minted once per start-up and nothing minted another.
  //
  // The reply is the only copy of the code, so it is held in state and shown until this screen is
  // left. A `403` means the signed-in person is not a manager, and the message says so rather than
  // sending them back to the sign-in screen.
  const mint = async () => {
    setBusy(true);
    setError(null);
    try {
      setMinted(await api.mintPairingCode());
    } catch (caught) {
      setMinted(null);
      setError(errorMessage(caught));
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

      <div class="mt-6 rounded-token border border-line p-3">
        <p class="font-semibold text-ink">{t("devices.add_title")}</p>
        <p class="mt-1 text-sm text-ink-muted">{t("devices.add_hint")}</p>
        <Show
          when={minted()}
          fallback={
            <button
              type="button"
              class="mt-2 min-h-touch w-full rounded-token border border-line font-semibold text-ink disabled:opacity-50"
              disabled={busy()}
              onClick={() => void mint()}
            >
              {t("devices.add_button")}
            </button>
          }
        >
          {(code) => (
            <div class="mt-2">
              {/* Tracked wide and large: this is read aloud across a counter, or typed by somebody
                  holding a second tablet. `select-all` so one tap copies it. */}
              <p class="select-all text-center font-mono text-3xl tracking-[0.35em] text-ink">
                {code().code}
              </p>
              <p class="mt-2 text-sm text-ink-muted">
                {t("devices.add_expires", { moment: pairedAt(code().expires_at_ms) })}
              </p>
              <p class="mt-1 text-sm text-ink-muted">{t("devices.add_open")}</p>
              {/* The link as a QR code, so the tablet being added scans it rather than typing an
                  address, a port and six digits against a five-minute clock (ADR-0139). */}
              <div class="mt-3">
                <QrCode text={pairingUrl(code().code)} label={t("devices.add_qr")} />
              </div>
              <Show when={onLoopback()}>
                <p class="mt-2 rounded-token border border-awaiting px-3 py-2 text-sm text-ink" role="status">
                  {t("devices.add_qr_loopback")}
                </p>
              </Show>
              <p class="mt-2 select-all break-all font-mono text-xs text-ink-muted">
                {pairingUrl(code().code)}
              </p>
              <p class="mt-2 text-sm text-ink-muted">{t("devices.add_replaced")}</p>
              <button
                type="button"
                class="mt-2 min-h-touch w-full rounded-token border border-line text-ink disabled:opacity-50"
                disabled={busy()}
                onClick={() => void mint()}
              >
                {t("devices.add_button")}
              </button>
            </div>
          )}
        </Show>
      </div>

      <div class="mt-6 rounded-token border border-line p-3">
        <p class="font-semibold text-ink">{t("devices.printers_title")}</p>
        <p class="mt-1 text-sm text-ink-muted">{t("devices.printers_hint")}</p>
        <ul class="mt-2 flex flex-col gap-2">
          <For
            each={printers()}
            fallback={<li class="text-sm text-ink-muted">{t("devices.printers_none")}</li>}
          >
            {(printer) => (
              <li class="flex flex-wrap items-center justify-between gap-2">
                <span class="text-ink">{printer.name}</span>
                <span class="flex flex-wrap items-center gap-2">
                  <Show when={tested()[printer.device_id]}>
                    {(outcome) => (
                      <span class="text-sm text-ink-muted" role="status" data-outcome="test-print">
                        {t(testPrintKey(outcome()))}
                      </span>
                    )}
                  </Show>
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line px-3 text-ink disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => void testPrint(printer.device_id)}
                  >
                    {t("devices.test_print")}
                  </button>
                </span>
              </li>
            )}
          </For>
        </ul>
      </div>

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
