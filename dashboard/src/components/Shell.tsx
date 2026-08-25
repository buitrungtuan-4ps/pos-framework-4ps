// The frame every authenticated screen sits in: a left nav to each admin area, a top bar holding
// the tenant/store the operator is working within (read by most screens), a locale switch, and
// logout. The context inputs persist per browser (state/session.ts); they are a convenience, not an
// authorisation — the server's session cookie is what actually gates every call.

import { For, type ParentProps } from "solid-js";
import { A, useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import { LOCALES, type Locale, locale, setLocale, t } from "../i18n";
import { setAuthed } from "../state/session";
import { ContextPicker } from "./ContextPicker";

const NAV: readonly { href: string; key: Parameters<typeof t>[0] }[] = [
  { href: "/", key: "nav.reports" },
  { href: "/config", key: "nav.config" },
  { href: "/api-keys", key: "nav.apiKeys" },
  { href: "/devices", key: "nav.devices" },
  { href: "/webhooks", key: "nav.webhooks" },
  { href: "/translations", key: "nav.translations" },
  { href: "/activation", key: "nav.activation" },
];

export function Shell(props: ParentProps) {
  const navigate = useNavigate();
  const logout = async () => {
    await api.logout().catch(() => undefined);
    setAuthed(false);
    navigate("/login", { replace: true });
  };
  return (
    <div class="flex min-h-full flex-col">
      <header class="flex flex-wrap items-center gap-3 border-b border-line bg-surface px-4 py-3">
        <span class="text-lg font-semibold text-ink">{t("app.title")}</span>
        <div class="flex flex-1 flex-wrap items-center gap-2">
          <ContextPicker />
        </div>
        <label class="text-sm text-ink-muted">
          <span class="sr-only">{t("locale.label")}</span>
          <select
            class="min-h-touch rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
            value={locale()}
            onChange={(event) => setLocale(event.currentTarget.value as Locale)}
          >
            <For each={LOCALES}>{(code) => <option value={code}>{code.toUpperCase()}</option>}</For>
          </select>
        </label>
        <button
          type="button"
          class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
          onClick={() => void logout()}
        >
          {t("action.logout")}
        </button>
      </header>
      <div class="flex flex-1 flex-col md:flex-row">
        <nav class="border-b border-line bg-surface md:w-56 md:border-b-0 md:border-r">
          <ul class="flex flex-wrap gap-1 p-2 md:flex-col">
            <For each={NAV}>
              {(item) => (
                <li>
                  <A
                    href={item.href}
                    end={item.href === "/"}
                    class="block rounded-token px-3 py-2 text-base text-ink hover:bg-surface-raised"
                    activeClass="bg-surface-raised font-semibold"
                  >
                    {t(item.key)}
                  </A>
                </li>
              )}
            </For>
          </ul>
        </nav>
        <main class="flex-1 overflow-y-auto p-4 md:p-6">{props.children}</main>
      </div>
    </div>
  );
}
