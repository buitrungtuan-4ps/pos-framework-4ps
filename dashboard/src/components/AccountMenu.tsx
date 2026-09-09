// The account menu: who is signed in, how they want the console to look, and the way out
// (Wave 3 · Stage 6).
//
// # The two gaps this closes
//
// The console fetched `GET /admin/whoami` on mount and used the answer for one thing: hiding the nav
// entries a role cannot reach. So the identity was known and never shown. On a console where four
// roles see four different navs, an operator who could not find a screen had no way to check whether
// that was the role or the screen — and an operator with two accounts had nothing at all to tell
// them which one this tab was.
//
// The theme was the same shape of gap from the other side: `tokens.css` has honoured
// `data-theme` since P6 and nothing ever set it (see `lib/theme.ts`).
//
// # Why one menu rather than two more buttons in the header
//
// The header already carried eight controls and wrapped to a second row under `md`. Identity,
// appearance, language and sign-out are the same subject — "this is me, and this is how I want the
// console" — and every console puts them in one place behind the avatar. Folding the locale switch
// and the sign-out button in makes this a net *reduction* of one header control while adding two
// capabilities. Both folded controls are set-once preferences that persist per browser, which is
// what makes a click acceptable; a control an operator uses during a task would not belong here.

import { createEffect, createSignal, For, Show } from "solid-js";
import { A, useLocation } from "@solidjs/router";

import { LOCALES, type Locale, locale, localeName, setLocale, t } from "../i18n";
import { useEscape } from "../lib/escape";
import { initials } from "../lib/format";
import { setTheme, type Theme, theme, THEMES } from "../lib/theme";
import { actingAdmin } from "../state/session";
import { screenHref, SCREENS } from "../state/screens";
import { StatusBadge } from "./ui";

/** The i18n key naming a theme choice. A map, so a new `Theme` fails to compile until it is named. */
const THEME_LABEL: Record<Theme, "account.theme.system" | "account.theme.light" | "account.theme.dark"> =
  {
    system: "account.theme.system",
    light: "account.theme.light",
    dark: "account.theme.dark",
  };

/** The i18n key naming an admin role. Reuses the keys the roster and the audit trail already show. */
const ROLE_LABEL = {
  owner: "role.owner",
  admin: "role.admin",
  ops: "role.ops",
  viewer: "role.viewer",
} as const;

export function AccountMenu(props: { onSignOut: () => void }) {
  const [open, setOpen] = createSignal(false);
  const location = useLocation();
  useEscape(open, () => setOpen(false));

  // Navigating closes the menu — the two links in it go somewhere, and a menu left hanging over the
  // screen the operator just asked for is a panel they have to dismiss before they can read it.
  createEffect(() => {
    void location.pathname;
    setOpen(false);
  });

  // `null` covers both "whoami has not answered yet" and "whoami failed", because the signal cannot
  // tell them apart. That is deliberate rather than unnoticed: both render the same thing here, so
  // widening it to a three-state panel would be modelling with no consequence. Everything else in
  // the menu works without an identity — the theme, the language and the way out do not depend on
  // knowing who you are, and the one moment an operator most needs to sign out is the moment
  // something has gone wrong.
  const admin = () => actingAdmin();
  const avatar = () => {
    const who = admin();
    return who === null ? "" : initials(who.name);
  };

  return (
    <div class="relative">
      <button
        type="button"
        aria-label={t("account.open")}
        title={t("account.open")}
        aria-expanded={open()}
        aria-haspopup="true"
        onClick={() => setOpen((value) => !value)}
        class="flex min-h-touch items-center gap-2 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink transition-colors hover:bg-surface"
      >
        <Show
          when={avatar()}
          fallback={
            <span aria-hidden="true" class="text-ink-muted">
              ◍
            </span>
          }
        >
          {(letters) => (
            <span
              aria-hidden="true"
              class="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-accent text-xs font-semibold text-accent-ink"
            >
              {letters()}
            </span>
          )}
        </Show>
        <span aria-hidden="true" class="text-ink-muted">
          ▾
        </span>
      </button>

      <Show when={open()}>
        <div class="absolute right-0 z-30 mt-1 w-72 rounded-token border border-line bg-surface shadow-overlay">
          <Show when={admin()}>
            {(who) => (
              <div class="flex flex-col gap-1 border-b border-line px-3 py-3">
                <span class="text-xs uppercase tracking-wide text-ink-muted">
                  {t("account.signedInAs")}
                </span>
                <span class="text-sm font-semibold text-ink">{who().name}</span>
                {/* The email is the identity the server knows this session by, so it is the one an
                    operator checks when two accounts are in play. */}
                <span class="break-all text-sm text-ink-muted">{who().email}</span>
                <span class="mt-1">
                  <StatusBadge tone="neutral" label={t(ROLE_LABEL[who().role])} />
                </span>
              </div>
            )}
          </Show>

          <div class="flex flex-col gap-2 border-b border-line px-3 py-3">
            <span class="text-xs uppercase tracking-wide text-ink-muted">
              {t("account.appearance")}
            </span>
            {/* A radio group rather than three buttons: the three choices are one setting with one
                answer, which is what `radiogroup` says and what lets an arrow key move between
                them. `aria-checked` carries the answer, so no option needs a second label. */}
            <div role="radiogroup" aria-label={t("account.themeLabel")} class="flex gap-1">
              <For each={THEMES}>
                {(choice) => (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={theme() === choice}
                    onClick={() => setTheme(choice)}
                    class={`min-h-touch flex-1 rounded-token border px-2 text-sm transition-colors ${
                      theme() === choice
                        ? "border-accent bg-surface-raised font-semibold text-ink"
                        : "border-line text-ink-muted hover:text-ink"
                    }`}
                  >
                    {t(THEME_LABEL[choice])}
                  </button>
                )}
              </For>
            </div>

            <label class="flex flex-col gap-1">
              <span class="text-xs uppercase tracking-wide text-ink-muted">{t("locale.label")}</span>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                value={locale()}
                onChange={(event) => setLocale(event.currentTarget.value as Locale)}
              >
                <For each={LOCALES}>
                  {(code) => <option value={code}>{localeName(code)}</option>}
                </For>
              </select>
            </label>
          </div>

          <ul class="flex flex-col p-2">
            <For each={["mySessions", "mySecurity"] as const}>
              {(screen) => (
                <li>
                  <A
                    href={screenHref(screen, "", "")}
                    class="flex min-h-touch items-center rounded-token px-2 text-sm text-ink transition-colors hover:bg-surface-raised"
                  >
                    {t(SCREENS[screen].key)}
                  </A>
                </li>
              )}
            </For>
            <li>
              <button
                type="button"
                onClick={props.onSignOut}
                class="flex min-h-touch w-full items-center rounded-token px-2 text-left text-sm text-danger transition-colors hover:bg-surface-raised"
              >
                {t("action.logout")}
              </button>
            </li>
          </ul>
        </div>
      </Show>
    </div>
  );
}
