import { For, Show, createSignal } from "solid-js";
import { A } from "@solidjs/router";

import { api } from "../api/client";
import { type MessageKey, locale, setLocale, t } from "../i18n";
import { state } from "../state/store";

const NAV: { href: string; key: MessageKey }[] = [
  { href: "/", key: "nav.floor" },
  { href: "/counter", key: "nav.counter" },
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
    <header class="flex flex-wrap items-center gap-3 border-b border-line bg-surface px-4 py-2 text-sm">
      <A href="/" class="font-semibold no-underline text-ink">
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
      <nav class="flex items-center gap-3 text-ink-muted">
        <For each={NAV}>
          {(item) => (
            <A href={item.href} class="no-underline hover:text-ink" activeClass="text-ink" end>
              {t(item.key)}
            </A>
          )}
        </For>
      </nav>
      <button
        type="button"
        class="ml-auto rounded-token border border-line px-3 py-1 text-ink"
        aria-label={t("status.language")}
        onClick={() => setLocale(locale() === "vi" ? "en" : "vi")}
      >
        {t(locale() === "vi" ? "status.english" : "status.vietnamese")}
      </button>
      <button
        type="button"
        class="rounded-token border border-line px-3 py-1 text-ink"
        onClick={cycleTheme}
      >
        {theme() === "dark" ? t("status.theme_light") : t("status.theme_dark")}
      </button>
      <button
        type="button"
        class="rounded-token border border-line px-3 py-1 text-ink"
        onClick={() => void signOut()}
      >
        {t("nav.signout")}
      </button>
    </header>
  );
}
