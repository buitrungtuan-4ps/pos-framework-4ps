import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { A } from "@solidjs/router";

import { api, deviceToken } from "../api/client";
import { type MessageKey, locale, setLocale, t } from "../i18n";
import { kdsEnabled, loadSync, state, tablesEnabled } from "../state/store";

// What the bar says about the cash shift, per state (the wire tokens are the edge's `SHIFT_STATE_*`).
const SHIFT_STATE_LABELS: Readonly<Record<string, MessageKey>> = {
  SHIFT_STATE_OPEN: "status.shift_open",
  SHIFT_STATE_COUNTED: "status.shift_counted",
  SHIFT_STATE_CLOSED: "status.shift_closed",
};

// How often the bar asks the edge about the cloud (ADR-0137). The edge caches the depth for five
// seconds, so a floor of tablets polling at this rate costs it one count per window between them.
const SYNC_POLL_MS = 15_000;

// What the bar says about the cloud, or null when there is nothing worth a word: a connected store
// with a shallow outbox, or an edge that has not answered yet. "Offline" only when the drain has
// actually failed to reach the cloud — a demo with no cloud at all is not offline, it is unconnected,
// and it says so only once its backlog is deep enough to matter.
function cloudNotice(): { key: MessageKey; tone: "muted" | "warn" | "danger" } | null {
  const sync = state.sync;
  if (sync === null) {
    return null;
  }
  switch (sync.outbox_level) {
    case "OUTBOX_LEVEL_HIGH":
    case "OUTBOX_LEVEL_BEYOND":
      return { key: "status.outbox_high", tone: "danger" };
    case "OUTBOX_LEVEL_ELEVATED":
      return { key: "status.outbox_elevated", tone: "warn" };
    default:
      return sync.cloud_link === "CLOUD_LINK_OFFLINE"
        ? { key: "status.cloud_offline", tone: "muted" }
        : null;
  }
}

// Every destination, and what the store has to do for it to be one.
//
// `needs` is a predicate over the published capabilities, absent where a destination is unconditional.
// A counter cafe has no floor and no kitchen board; offering either is the failure `tips_enabled` was
// published to stop — an action the store cannot honour, presented as though it could.
//
// The floor link is dropped rather than relabelled on a counter store, because `/` is still home
// there: it draws the counter list instead (see `App.tsx`), and a link to the page you are on is not
// navigation. `nav.counter` below is the one that names it.
const NAV: { href: string; key: MessageKey; needs?: () => boolean }[] = [
  { href: "/", key: "nav.floor", needs: tablesEnabled },
  { href: "/counter", key: "nav.counter" },
  // A guest order that nobody confirms never reaches the kitchen (ADR-0116), so the queue needs to
  // be one tap from every screen rather than somewhere a server has to remember to look.
  { href: "/guests", key: "nav.confirm" },
  { href: "/kds", key: "nav.kitchen", needs: kdsEnabled },
  { href: "/expo", key: "nav.pass", needs: kdsEnabled },
  { href: "/today", key: "nav.today" },
  { href: "/shift", key: "nav.shift" },
  { href: "/pair", key: "nav.pair" },
  // Retiring a lost till (ADR-0091, production-readiness O1). Beside pairing, because it is the same
  // job from the other end: this is where a device stops being admitted.
  { href: "/devices", key: "nav.devices" },
];

// The persistent status bar. It names the store link (to the edge on the LAN), the cloud when there is
// something to say about it — "Offline — selling normally" and how many events are waiting, amber
// and then red as the backlog deepens, never a block (ADR-0137) — the open shift, the language, and
// a theme toggle. Nothing here ever moves between states; only its text and colour change.
//
// # Why every control here is `min-h-touch`
//
// `docs/ui-ux.md` §1 principle 2 asks for 48 px, and this bar was the one place in the till that
// ignored it: the ten destinations were bare text in a `flex` that could not wrap, so each was a
// 20 px-high target and together they were 488 px wide — which is what made **every route** scroll
// sideways on a phone, against `app.css`'s own promise not to. The bar wraps now and each
// destination is padded to the token, which costs vertical room on a phone and is the correct
// trade: a target a finger cannot hit during service is not navigation.
export function StatusBar() {
  const linkKey = (): MessageKey => {
    switch (state.link) {
      case "open":
        return "status.connected";
      case "connecting":
        return "status.connecting";
      default:
        return "status.reconnecting";
    }
  };
  const linkColour = () => (state.link === "open" ? "bg-ok" : "bg-awaiting");

  // Poll the cloud link while this device is paired. A paired device that nobody has signed in to
  // yet is refused the read, which costs nothing and leaves the bar as it was.
  onMount(() => {
    const poll = () => {
      if (deviceToken() !== null) {
        void loadSync();
      }
    };
    poll();
    const timer = setInterval(poll, SYNC_POLL_MS);
    onCleanup(() => clearInterval(timer));
  });

  // Whether the phone's folded menu is open. Irrelevant from a tablet up, where nothing folds.
  const [menuOpen, setMenuOpen] = createSignal(false);

  const initial = document.documentElement.dataset["theme"] ?? "system";
  const [theme, setTheme] = createSignal(initial);
  const cycleTheme = () => {
    const next = theme() === "dark" ? "light" : "dark";
    document.documentElement.dataset["theme"] = next;
    setTheme(next);
  };

  // End the shift on this device: sign out and return to the sign-in screen (S0b, ADR-0084). The
  // device stays paired, so the next person only signs in.
  const signOut = async () => {
    await api.signOut();
    window.location.replace("/signin");
  };

  return (
    <header class="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-line bg-surface px-4 py-2 text-sm">
      <A
        href="/"
        class="inline-flex min-h-touch items-center rounded-token font-semibold no-underline text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
      >
        {t("app.brand")}
      </A>
      <span class="inline-flex items-center gap-2 text-ink-muted">
        <span class={`inline-block h-2.5 w-2.5 rounded-full ${linkColour()}`} aria-hidden="true" />
        {t(linkKey())}
      </span>
      <Show when={cloudNotice()}>
        {(notice) => (
          <span
            class="inline-flex items-center gap-2"
            classList={{
              "text-ink-muted": notice().tone === "muted",
              "text-awaiting": notice().tone === "warn",
              "text-danger font-semibold": notice().tone === "danger",
            }}
            role="status"
            data-outcome="cloud-notice"
          >
            <span
              class="inline-block h-2.5 w-2.5 rounded-full"
              classList={{
                "bg-awaiting": notice().tone !== "danger",
                "bg-danger": notice().tone === "danger",
              }}
              aria-hidden="true"
            />
            {t(notice().key)}
            <Show when={(state.sync?.outbox_depth ?? 0) > 0}>
              <span class="tabular-nums">
                {"· "}
                {t("status.cloud_waiting", { count: state.sync?.outbox_depth ?? 0 })}
              </span>
            </Show>
          </span>
        )}
      </Show>
      <Show
        when={state.shift}
        fallback={<span class="text-ink-muted">{t("status.no_shift")}</span>}
      >
        {(shift) => (
          <span class="text-ink-muted">
            {/* A sentence per state, not the wire token spliced into one: "Ca open" is what a
                Vietnamese till read when the state was lower-cased into a translated template. */}
            {t(SHIFT_STATE_LABELS[shift().state] ?? "status.shift_open")}
          </span>
        )}
      </Show>
      {/*
        On a phone the destinations and the three settings fold behind one button (F7): wrapped
        in full they took four rows — about a third of a 390px screen — on every page, above the
        work. From a tablet up they sit in the bar as before; `tablet:flex` beats the phone's
        `hidden` because a variant is ordered after the base utility.
      */}
      <button
        type="button"
        class="ml-auto min-h-touch rounded-token border border-line px-3 text-ink tablet:hidden focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
        aria-expanded={menuOpen()}
        aria-controls="status-menu"
        onClick={() => setMenuOpen(!menuOpen())}
      >
        {t(menuOpen() ? "nav.menu_close" : "nav.menu")}
      </button>
      <div
        id="status-menu"
        class="w-full flex-col items-stretch gap-1 tablet:flex tablet:w-auto tablet:flex-1 tablet:flex-row tablet:flex-wrap tablet:items-center tablet:gap-x-3"
        classList={{ hidden: !menuOpen(), flex: menuOpen() }}
      >
        <nav class="flex flex-col gap-1 text-ink-muted tablet:flex-row tablet:flex-wrap tablet:items-center">
          <For each={NAV.filter((item) => item.needs === undefined || item.needs())}>
            {(item) => (
              <A
                href={item.href}
                class="inline-flex min-h-touch items-center rounded-token px-3 no-underline hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
                activeClass="text-ink"
                onClick={() => setMenuOpen(false)}
                end
              >
                {t(item.key)}
              </A>
            )}
          </For>
        </nav>
        <button
          type="button"
          class="min-h-touch rounded-token border border-line px-3 text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          aria-label={t("status.language")}
          onClick={() => setLocale(locale() === "vi" ? "en" : "vi")}
        >
          {t(locale() === "vi" ? "status.english" : "status.vietnamese")}
        </button>
        <button
          type="button"
          class="min-h-touch rounded-token border border-line px-3 text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          aria-label={theme() === "dark" ? t("status.theme_light") : t("status.theme_dark")}
          onClick={cycleTheme}
        >
          {theme() === "dark" ? t("status.theme_light") : t("status.theme_dark")}
        </button>
        <button
          type="button"
          class="min-h-touch rounded-token border border-line px-3 text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          onClick={() => void signOut()}
        >
          {t("nav.signout")}
        </button>
      </div>
    </header>
  );
}
