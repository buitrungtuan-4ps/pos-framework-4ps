// The kiosk (ADR-0224 decisions 4 and 5): a till on a `DEVICE_CLASS_KIOSK` terminal. It signs in as
// the kiosk's own staff record when it starts, with no PIN, and draws nothing but self-order: the
// menu, the guest's order and the number it is called by. Leaving it is a person signing in with
// their code and PIN, which the edge allows only to someone holding `admin.kiosk.exit`.
//
// It stands apart from the till's shell on purpose: the shell reads the floor, the open orders and
// the shift, none of which the edge lets a kiosk read.

import { Match, Show, Switch, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { ApiError, api } from "../api/client";
import { adoptStoreLanguage, t } from "../i18n";
import { abandon, loadKioskReasons, phase, startOver } from "../state/kiosk";
import { applyLocale } from "../state/store";
import { KioskCart, KioskDone, KioskMenu, setKioskMenu } from "./Kiosk";

/** How long the number stays up after an order is sent, before the next guest's screen. */
const DONE_SECONDS = 20;
/** The permission that lets a person leave a kiosk. */
const EXIT = "admin.kiosk.exit";

function home(): void {
  window.location.replace("/");
}

/** The code-and-PIN form a member of staff leaves the kiosk with. */
function KioskExit(props: { onCancel: () => void }) {
  const [code, setCode] = createSignal("");
  const [pin, setPin] = createSignal("");
  const [said, setSaid] = createSignal<string | null>(null);
  const submit = async (event: SubmitEvent) => {
    event.preventDefault();
    const result = await api.signIn(code(), pin()).catch(() => null);
    if (result?.ok === true) return home();
    setPin("");
    setSaid(result?.ok === false && result.outcome === "kiosk_exit_not_allowed" ? t("kiosk.exit_not_allowed") : t("kiosk.exit_wrong"));
  };
  return (
    <form class="mx-auto max-w-sm space-y-3 p-6" data-outcome="kiosk-exit" onSubmit={(event) => void submit(event)}>
      <h2 class="text-lg font-semibold">{t("kiosk.exit_title")}</h2>
      <label class="block">
        <span class="block text-sm">{t("kiosk.exit_code")}</span>
        <input id="kiosk-exit-code" class="w-full rounded-token border border-line p-2" value={code()} onInput={(event) => setCode(event.currentTarget.value)} />
      </label>
      <label class="block">
        <span class="block text-sm">{t("kiosk.exit_pin")}</span>
        <input id="kiosk-exit-pin" type="password" inputmode="numeric" class="w-full rounded-token border border-line p-2" value={pin()} onInput={(event) => setPin(event.currentTarget.value)} />
      </label>
      <Show when={said()}>{(text) => <p role="alert" class="text-danger">{text()}</p>}</Show>
      <div class="flex gap-2">
        <button type="submit" class="min-h-11 flex-1 rounded-token bg-primary font-semibold text-primary-ink" data-step="kioskExitSubmit">{t("kiosk.exit_submit")}</button>
        <button type="button" class="min-h-11 flex-1 rounded-token border border-line" onClick={() => props.onCancel()}>{t("kiosk.exit_cancel")}</button>
      </div>
    </form>
  );
}

/** The kiosk's screen once it has signed in: the menu and the order, the number, or a closed sign. */
function KioskBody(props: { onExit: () => void }) {
  return (
    <>
      <div class="mb-3 flex items-center justify-between">
        <h1 class="text-2xl font-semibold">{t("kiosk.title")}</h1>
        <button type="button" class="min-h-11 rounded-token border border-line px-3 text-sm" data-step="kioskExit" onClick={() => props.onExit()}>
          {t("kiosk.staff")}
        </button>
      </div>
      <Switch>
        <Match when={phase() === "closed"}>
          <p class="p-6 text-center text-lg" data-outcome="kiosk-closed">{t("kiosk.not_taking_orders")}</p>
        </Match>
        <Match when={phase() === "done"}>
          <KioskDone />
        </Match>
        <Match when={true}>
          <div class="grid gap-4 md:grid-cols-[2fr_1fr]">
            <KioskMenu />
            <KioskCart />
          </div>
        </Match>
      </Switch>
    </>
  );
}

export function KioskApp() {
  const [ready, setReady] = createSignal<"waiting" | "ready" | "not_a_kiosk">("waiting");
  const [exiting, setExiting] = createSignal(false);
  let abandonSeconds = 120;
  let timer: ReturnType<typeof setTimeout> | undefined;
  // Every touch, and every change of screen, starts the clock again: an order is given up only
  // once nobody has touched the kiosk for the store's whole wait.
  const expire = () => {
    if (phase() === "ordering") void abandon();
    else if (phase() === "done") startOver();
  };
  const touch = () => {
    clearTimeout(timer);
    timer = setTimeout(expire, (phase() === "done" ? DONE_SECONDS : abandonSeconds) * 1000);
  };
  createEffect(on(phase, touch));
  const start = async () => {
    const session = await api.session();
    adoptStoreLanguage(session.display_language);
    if (session.signed_in && (session.permissions ?? []).includes(EXIT)) return home();
    if (!session.signed_in) await api.kioskSignIn();
    abandonSeconds = session.kiosk_abandon_seconds ?? 120;
    const [menu, locale] = await Promise.all([api.menu(), api.locale(), loadKioskReasons()]);
    applyLocale(locale);
    setKioskMenu(menu);
    setReady("ready");
  };
  onMount(() => {
    start().catch((caught) => setReady(caught instanceof ApiError && caught.status === 403 ? "not_a_kiosk" : "waiting"));
    onCleanup(() => clearTimeout(timer));
  });
  return (
    <main class="min-h-full p-4" data-outcome="kiosk" onPointerDown={touch} onKeyDown={touch}>
      <Switch>
        <Match when={ready() === "not_a_kiosk"}>
          <p class="p-6 text-center">{t("kiosk.not_a_kiosk")}</p>
        </Match>
        <Match when={exiting()}>
          <KioskExit onCancel={() => setExiting(false)} />
        </Match>
        <Match when={ready() === "ready"}>
          <KioskBody onExit={() => setExiting(true)} />
        </Match>
      </Switch>
    </main>
  );
}
