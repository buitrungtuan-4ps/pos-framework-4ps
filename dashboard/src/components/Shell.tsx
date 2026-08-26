// The frame every authenticated screen sits in (ADR-0060, Track F1): a top bar with the working
// tenant/store, the locale switch and logout; a grouped, scope-aware left nav; a breadcrumb strip;
// and a version footer. The context inputs persist per browser (state/session.ts) and are a
// convenience, not an authorisation — the server's session cookie is what gates every call.

import { For, type ParentProps, Show } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import { LOCALES, type Locale, type MessageKey, locale, localeName, setLocale, t } from "../i18n";
import { contextReady, type Scope } from "../lib/scoped";
import { APP_VERSION } from "../lib/version";
import { setAuthed, storeName, tenantName } from "../state/session";
import { ContextPicker } from "./ContextPicker";

type NavItem = { href: string; key: MessageKey; scope: Scope };
type NavGroup = { key: MessageKey; items: readonly NavItem[] };

// The admin areas, grouped and each tagged with the working context it needs — so the nav shows at a
// glance whether a screen is ready to open (its tenant, or tenant *and* store, is set) or is waiting
// on a choice in the top bar.
const NAV_GROUPS: readonly NavGroup[] = [
  { key: "nav.group.overview", items: [{ href: "/", key: "nav.reports", scope: "store" }] },
  {
    key: "nav.group.masterData",
    items: [
      { href: "/stores", key: "nav.stores", scope: "tenant" },
      { href: "/catalog", key: "nav.catalog", scope: "tenant" },
      { href: "/layout", key: "nav.layout", scope: "tenant" },
    ],
  },
  {
    key: "nav.group.settings",
    items: [
      { href: "/config", key: "nav.config", scope: "store" },
      { href: "/translations", key: "nav.translations", scope: "tenant" },
    ],
  },
  {
    key: "nav.group.access",
    items: [
      { href: "/api-keys", key: "nav.apiKeys", scope: "tenant" },
      { href: "/devices", key: "nav.devices", scope: "tenant" },
      { href: "/activation", key: "nav.activation", scope: "store" },
    ],
  },
  {
    key: "nav.group.integrations",
    items: [{ href: "/webhooks", key: "nav.webhooks", scope: "tenant" }],
  },
];

// The page label for the breadcrumb, by route. `/stores/new` is the one path without a nav entry.
const CRUMB_KEY: Record<string, MessageKey> = {
  "/": "nav.reports",
  "/stores": "nav.stores",
  "/stores/new": "wizard.title",
  "/catalog": "nav.catalog",
  "/layout": "nav.layout",
  "/config": "nav.config",
  "/api-keys": "nav.apiKeys",
  "/devices": "nav.devices",
  "/webhooks": "nav.webhooks",
  "/translations": "nav.translations",
  "/activation": "nav.activation",
};

// The nav dot's tooltip/aria: whether the item's context is ready, or which piece it is waiting on.
function scopeHint(scope: Scope): string {
  if (contextReady(scope)) {
    return t("nav.scopeReady");
  }
  return scope === "store" ? t("nav.scopeNeedsStore") : t("nav.scopeNeedsTenant");
}

export function Shell(props: ParentProps) {
  const navigate = useNavigate();
  const location = useLocation();

  const logout = async () => {
    await api.logout().catch(() => undefined);
    setAuthed(false);
    navigate("/login", { replace: true });
  };

  // The breadcrumb: the working context (tenant, then store, each shown once set) then the page.
  const crumbs = (): string[] => {
    const trail: string[] = [];
    if (tenantName()) {
      trail.push(tenantName());
    }
    if (storeName()) {
      trail.push(storeName());
    }
    const pageKey = CRUMB_KEY[location.pathname];
    trail.push(pageKey ? t(pageKey) : t("app.title"));
    return trail;
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
            <For each={LOCALES}>{(code) => <option value={code}>{localeName(code)}</option>}</For>
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
        <nav class="border-b border-line bg-surface md:w-60 md:border-b-0 md:border-r">
          <div class="flex flex-col gap-4 p-2">
            <For each={NAV_GROUPS}>
              {(group) => (
                <div>
                  <p class="px-3 py-1 text-xs font-medium uppercase tracking-wide text-ink-muted">
                    {t(group.key)}
                  </p>
                  <ul class="flex flex-wrap gap-1 md:flex-col">
                    <For each={group.items}>
                      {(item) => (
                        <li>
                          <A
                            href={item.href}
                            end={item.href === "/"}
                            class="flex items-center justify-between gap-2 rounded-token px-3 py-2 text-base text-ink hover:bg-surface-raised"
                            activeClass="bg-surface-raised font-semibold"
                          >
                            <span>{t(item.key)}</span>
                            <span
                              aria-label={scopeHint(item.scope)}
                              title={scopeHint(item.scope)}
                              class={`h-2 w-2 shrink-0 rounded-full border ${
                                contextReady(item.scope)
                                  ? "border-accent bg-accent"
                                  : "border-line bg-transparent"
                              }`}
                            />
                          </A>
                        </li>
                      )}
                    </For>
                  </ul>
                </div>
              )}
            </For>
          </div>
        </nav>
        <div class="flex flex-1 flex-col">
          <nav
            aria-label={t("shell.breadcrumb")}
            class="flex flex-wrap items-center gap-1 border-b border-line px-4 py-2 text-sm text-ink-muted md:px-6"
          >
            <For each={crumbs()}>
              {(crumb, index) => (
                <>
                  <Show when={index() > 0}>
                    <span aria-hidden="true">›</span>
                  </Show>
                  <span class={index() === crumbs().length - 1 ? "text-ink" : ""}>{crumb}</span>
                </>
              )}
            </For>
          </nav>
          <main class="flex-1 overflow-y-auto p-4 md:p-6">{props.children}</main>
          <footer class="border-t border-line px-4 py-2 text-xs text-ink-muted md:px-6">
            <span>{t("app.title")}</span>
            <span aria-hidden="true"> · </span>
            <span>{t("shell.version", { version: APP_VERSION })}</span>
          </footer>
        </div>
      </div>
    </div>
  );
}
