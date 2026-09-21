import { For, Show, createSignal } from "solid-js";
import { A } from "@solidjs/router";

import { api } from "../api/client";
import { type MessageKey, locale, setLocale, t } from "../i18n";
import { state } from "../state/store";

const NAV: { href: string; key: MessageKey }[] = [
  { href: "/", key: "nav.floor" },
  { href: "/counter", key: "nav.counter" },
  // A guest order that nobody confirms never reaches the kitchen (ADR-0116), so the queue needs to
  // be one tap from every screen rather than somewhere a server has to remember to look.
  { href: "/guests", key: "nav.confirm" },
  { href: "/kds", key: "nav.kitchen" },
  { href: "/expo", key: "nav.pass" },
  { href: "/today", key: "nav.today" },
  { href: "/shift", key: "nav.shift" },
  { href: "/pair", key: "nav.pair" },
  // Retiring a lost till (ADR-0091, production-readiness O1). Beside pairing, because it is the same
  // job from the other end: this is where a device stops being admitted.
  { href: "/devices", key: "nav.devices" },
];

// The persistent status bar. It names the store link (to the edge on the LAN, not the cloud — a
// store is meant to trade with the cloud unreachable), the open shift, the language, and a theme
// toggle. Nothing here ever moves between states; only its text and colour change.
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
      <Show
        when={state.shift}
        fallback={<span class="text-ink-muted">{t("status.no_shift")}</span>}
      >
        {(shift) => (
          <span class="text-ink-muted">
            {t("status.shift", {
              state: shift().state.replace("SHIFT_STATE_", "").toLowerCase(),
            })}
          </span>
        )}
      </Show>
      <nav class="flex flex-wrap items-center gap-1 text-ink-muted">
        <For each={NAV}>
          {(item) => (
            <A
              href={item.href}
              class="inline-flex min-h-touch items-center rounded-token px-3 no-underline hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
              activeClass="text-ink"
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
    </header>
  );
}
