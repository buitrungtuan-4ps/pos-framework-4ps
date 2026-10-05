import { For, Show, createSignal, onMount } from "solid-js";

import { ApiError, api } from "../api/client";
import type {
  IntegrationEntry,
  MintedCode,
  PairedDevice,
  PrinterEntry,
  TerminalEntry,
} from "../api/types";
import { QrCode } from "../components/QrCode";
import { PageHeader } from "../components/ui";
import { type MessageKey, locale, t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";
import { can } from "../state/permissions";
import { loadSync, state } from "../state/store";

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
// Retiring needs a signed-in person whose role grants `admin.device.manage`, as minting a code does
// (ADR-0158 decision 8): the published staff roster is the operator identity the edge checks
// offline. The list stays behind the paired-device gate alone, because POS Station reads it as its
// token probe with nobody signed in. Every revoke is written to the store's log.

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

// A connection's family in words. A family this edge does not know is still a connection the cloud
// sent, so it is listed as one rather than dropped.
function familyKey(family: string): MessageKey {
  switch (family) {
    case "INTEGRATION_FAMILY_E_INVOICE":
      return "devices.family_einvoice";
    case "INTEGRATION_FAMILY_QR_PAYMENT":
      return "devices.family_qr";
    case "INTEGRATION_FAMILY_CARD_TERMINAL":
      return "devices.family_card";
    case "INTEGRATION_FAMILY_DELIVERY":
      return "devices.family_delivery";
    case "INTEGRATION_FAMILY_COURIER":
      return "devices.family_courier";
    case "INTEGRATION_FAMILY_ERP":
      return "devices.family_erp";
    default:
      return "devices.family_other";
  }
}

// Which way the store server's clock is off, in words. The figure is shown unsigned beside it: "340 ms
// behind" reads, "-340 ms" makes a manager work out which of the two clocks the sign is about.
function clockOffsetKey(offsetMs: number): MessageKey {
  if (offsetMs > 0) {
    return "devices.clock_ahead";
  }
  return offsetMs < 0 ? "devices.clock_behind" : "devices.clock_exact";
}

// What a bind came to, in words. An outcome this till does not know is a bind that did not happen.
function bindOutcomeKey(outcome: string): MessageKey {
  switch (outcome) {
    case "BOUND":
      return "devices.till_bound";
    case "HELD_BY_ANOTHER_DEVICE":
      return "devices.till_held_by_another";
    case "DEVICE_HOLDS_ANOTHER_AGENT":
      return "devices.till_holds_another";
    default:
      return "devices.till_not_bound";
  }
}

// Who is a till, in words, or nothing where nobody is. A token this till does not know reads as
// nobody: the bind is offered, and the store server answers it.
function heldKey(held: string): MessageKey | null {
  switch (held) {
    case "THIS_DEVICE":
      return "devices.till_held_here";
    case "ANOTHER_DEVICE":
      return "devices.till_held_elsewhere";
    default:
      return null;
  }
}

// Which of the store's tills this device is (ADR-0112, ADR-0160 decision 4). A paired device
// becomes one through the print-agent binding: its guests' receipts then print where the console
// says for that till, and printers the console points at the till print through this device's
// print agent. Nothing on a screen made the binding before this card, so a manager called the store
// server by hand.
//
// Drawn for whoever may manage devices, as retiring one is. The store server decides from the
// person's own role in every store, so where the store does not enforce each person's own set it
// refuses the read to somebody `can` lets through. The card is then not drawn, as it would not be
// where the store enforces, rather than an error on a screen they opened to look at.
//
// The bundle is the one its own store server serves, POS Station's window included (ADR-0147), so a
// store server too old for this read never serves a till that asks it. A page left open while its
// store server rolls back is the version banner's case, and shows the failed read as any other.
function ThisDevice() {
  const [terminals, setTerminals] = createSignal<readonly TerminalEntry[] | null>(null);
  const [refused, setRefused] = createSignal(false);
  const [chosen, setChosen] = createSignal<string | null>(null);
  const [outcome, setOutcome] = createSignal<MessageKey | null>(null);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [releasing, setReleasing] = createSignal(false);
  const [busy, setBusy] = createSignal(false);

  const load = async () => {
    try {
      setTerminals((await api.terminals()).terminals);
    } catch (caught) {
      if (caught instanceof ApiError && caught.status === 403) {
        setRefused(true);
        return;
      }
      setFailure(errorMessage(caught));
    }
  };
  onMount(() => void load());

  // The till this device is, if it is one of the store's.
  const mine = () => terminals()?.find((terminal) => terminal.held === "THIS_DEVICE") ?? null;

  // A bind or a release, then the tills read again: the card says what the store server holds, not
  // what this screen expects it to.
  const act = async (send: () => Promise<MessageKey>) => {
    setBusy(true);
    setOutcome(null);
    setFailure(null);
    try {
      setOutcome(await send());
      await load();
    } catch (caught) {
      setFailure(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };
  const bind = (agentDeviceId: string) =>
    act(async () => bindOutcomeKey((await api.bindTerminal(agentDeviceId)).outcome));
  const release = (agentDeviceId: string) =>
    act(async () => {
      await api.releaseTerminal(agentDeviceId);
      setReleasing(false);
      return "devices.till_released";
    });

  return (
    <Show when={!refused()}>
      <div class="mt-6 rounded-token border border-line p-3" data-outcome="till">
        <p class="font-semibold text-ink">{t("devices.till_title")}</p>
        <Show when={terminals()}>
          {(listed) => (
            <Show
              when={listed().length > 0}
              fallback={
                <p class="mt-1 text-sm text-ink-muted" data-outcome="till-empty">
                  {t("devices.till_empty")}
                </p>
              }
            >
              <Show
                when={mine()}
                fallback={
                  <p class="mt-1 text-ink" data-outcome="till-status">
                    {t("devices.till_none")}
                  </p>
                }
              >
                {(till) => (
                  <>
                    <p class="mt-1 font-semibold text-ink" data-outcome="till-status">
                      {t("devices.till_is", { name: till().name })}
                    </p>
                    <Show
                      when={releasing()}
                      fallback={
                        <button
                          type="button"
                          class="mt-2 min-h-touch rounded-token border border-line px-3 text-ink disabled:opacity-50"
                          disabled={busy()}
                          onClick={() => setReleasing(true)}
                        >
                          {t("devices.till_release")}
                        </button>
                      }
                    >
                      <div class="mt-2 flex flex-wrap items-center gap-2">
                        <span class="text-sm text-ink">
                          {t("devices.till_release_confirm", { name: till().name })}
                        </span>
                        <button
                          type="button"
                          class="min-h-touch rounded-token bg-danger px-3 font-semibold text-danger-ink disabled:opacity-50"
                          disabled={busy()}
                          onClick={() => void release(till().agent_device_id)}
                        >
                          {t("devices.till_release")}
                        </button>
                        <button
                          type="button"
                          class="min-h-touch rounded-token border border-line px-3 text-ink"
                          onClick={() => setReleasing(false)}
                        >
                          {t("common.cancel")}
                        </button>
                      </div>
                    </Show>
                  </>
                )}
              </Show>
              <p class="mt-2 text-sm text-ink-muted">{t("devices.till_hint")}</p>
              {/* Choosing first, then Bind under the choice, as the Today screen reprints: a tap on
                  the wrong row of a list must not move where a till's paper prints. */}
              <ul class="mt-2 flex flex-col gap-2">
                <For each={listed()}>
                  {(terminal) => (
                    <li>
                      <button
                        type="button"
                        class="flex min-h-touch w-full items-center gap-3 rounded-token border border-line bg-surface px-3 text-left"
                        classList={{
                          "border-2 border-accent font-semibold":
                            chosen() === terminal.agent_device_id,
                        }}
                        aria-pressed={chosen() === terminal.agent_device_id}
                        onClick={() => setChosen(terminal.agent_device_id)}
                      >
                        <span class="min-w-0 flex-1 text-ink">{terminal.name}</span>
                        <Show when={heldKey(terminal.held)}>
                          {(key) => <span class="text-sm text-ink-muted">{t(key())}</span>}
                        </Show>
                      </button>
                      <Show
                        when={
                          chosen() === terminal.agent_device_id && terminal.held !== "THIS_DEVICE"
                        }
                      >
                        <button
                          type="button"
                          class="mt-2 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
                          disabled={busy()}
                          onClick={() => void bind(terminal.agent_device_id)}
                        >
                          {t("devices.till_bind", { name: terminal.name })}
                        </button>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          )}
        </Show>
        <Show when={outcome()}>
          {(key) => (
            <p class="mt-2 text-sm text-ink" role="status" data-outcome="till-outcome">
              {t(key())}
            </p>
          )}
        </Show>
        <Show when={failure()}>
          {(message) => (
            <p class="mt-2 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
              {message()}
            </p>
          )}
        </Show>
      </div>
    </Show>
  );
}

export function Devices() {
  // Retiring, adding, testing and binding this device to a till are managing devices
  // (`admin.device.manage`; retiring under ADR-0158 decision 8). Without it the screen still says
  // what is paired, published and measured.
  const manages = () => can("admin.device.manage");
  const [devices, setDevices] = createSignal<readonly PairedDevice[]>([]);
  const [durable, setDurable] = createSignal(true);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [confirming, setConfirming] = createSignal<string | null>(null);
  const [confirmAll, setConfirmAll] = createSignal("");
  const [minted, setMinted] = createSignal<MintedCode | null>(null);
  const [printers, setPrinters] = createSignal<readonly PrinterEntry[]>([]);
  const [integrations, setIntegrations] = createSignal<readonly IntegrationEntry[]>([]);
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

  // The store server's clock, as it last measured it against a time server (roadmap-v3 PF4). It
  // rides `GET /api/sync`, which the status bar already polls, so this reads the bar's answer rather
  // than asking a second time; the read on mount only saves waiting for the bar's next poll. Null
  // until a measurement has succeeded, which is not the same as a clock in step.
  const clock = () => {
    const sync = state.sync;
    if (sync?.clock_offset_ms === undefined || sync.clock_measure_time === undefined) {
      return null;
    }
    return {
      offsetMs: sync.clock_offset_ms,
      measuredMs: Date.parse(sync.clock_measure_time),
      alarm: sync.clock_drift === "CLOCK_DRIFT_ALARM",
      limitMs: sync.clock_drift_alarm_ms ?? 0,
    };
  };

  onMount(() => {
    void load();
    void loadSync();
    // Forgiving: an edge that predates the route, or a store with nothing published, lists none.
    void api
      .printers()
      .then(setPrinters)
      .catch(() => setPrinters([]));
    // The same forgiveness: an edge that predates the route lists none, which reads as what it is —
    // every family on its offline path.
    void api
      .integrations()
      .then(setIntegrations)
      .catch(() => setIntegrations([]));
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
  // than one told to try again. A `403` means the signed-in person may not manage devices, as for
  // minting below.
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
              <Show when={manages()}>
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
              </Show>
            </li>
          )}
        </For>
      </ul>

      <Show when={manages()}>
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
      </Show>

      <Show when={manages()}>
        <ThisDevice />
      </Show>

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
                        {t(printOutcomeKey(outcome(), "devices.test_printed"))}
                      </span>
                    )}
                  </Show>
                  <Show when={manages()}>
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line px-3 text-ink disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => void testPrint(printer.device_id)}
                  >
                    {t("devices.test_print")}
                  </button>
                  </Show>
                </span>
              </li>
            )}
          </For>
        </ul>
      </div>

      <div class="mt-6 rounded-token border border-line p-3" data-outcome="integrations">
        <p class="font-semibold text-ink">{t("devices.integrations_title")}</p>
        <p class="mt-1 text-sm text-ink-muted">{t("devices.integrations_hint")}</p>
        <ul class="mt-2 flex flex-col gap-1">
          <For
            each={integrations()}
            fallback={<li class="text-sm text-ink-muted">{t("devices.integrations_none")}</li>}
          >
            {(integration) => (
              <li class="flex flex-wrap items-baseline justify-between gap-2">
                <span class="text-ink">{integration.display_name}</span>
                <span class="text-sm text-ink-muted">{t(familyKey(integration.family))}</span>
              </li>
            )}
          </For>
        </ul>
      </div>

      <div class="mt-6 rounded-token border border-line p-3">
        <p class="font-semibold text-ink">{t("devices.clock_title")}</p>
        <p class="mt-1 text-sm text-ink-muted">{t("devices.clock_hint")}</p>
        <Show
          when={clock()}
          fallback={<p class="mt-2 text-sm text-ink-muted">{t("devices.clock_unmeasured")}</p>}
        >
          {(reading) => (
            <>
              <p class="mt-2 tabular-nums text-ink">
                {t(clockOffsetKey(reading().offsetMs), { offset: Math.abs(reading().offsetMs) })}
              </p>
              <p class="text-sm text-ink-muted">
                {t("devices.clock_checked", { moment: pairedAt(reading().measuredMs) })}
              </p>
              <Show when={reading().alarm}>
                <p class="mt-2 rounded-token border border-danger px-3 py-2 text-danger" role="status">
                  {t("devices.clock_alarm", { limit: reading().limitMs })}
                </p>
              </Show>
            </>
          )}
        </Show>
      </div>

      <Show when={manages()}>
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
      </Show>
    </section>
  );
}
