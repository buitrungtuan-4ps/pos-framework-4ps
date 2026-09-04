// The frame every authenticated screen sits in (ADR-0060, Track F1): a top bar with the working
// tenant/store, the locale switch and logout; a grouped, scope-aware left nav; a breadcrumb strip;
// and a version footer. The context inputs persist per browser (state/session.ts) and are a
// convenience, not an authorisation — the server's session cookie is what gates every call.

import { For, onMount, type ParentProps, Show } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import type { AdminRole } from "../api/types";
import { LOCALES, type Locale, locale, localeName, setLocale, t } from "../i18n";
import { contextReady, type Scope } from "../lib/scoped";
import { APP_VERSION } from "../lib/version";
import {
  actingAdmin,
  setActingAdmin,
  setAuthed,
  storeId,
  storeName,
  tenantId,
  tenantName,
} from "../state/session";
import {
  NAV_GROUPS,
  type ScreenId,
  screenAtPath,
  screenHref,
  screenPathOf,
  specOf,
} from "../state/screens";
import { CommandPalette, openPalette } from "./CommandPalette";
import { ContextPicker } from "./ContextPicker";
import { NotificationBell, ToastHost } from "./Toast";

// A nav entry. `scope` (when set) tags the working context the screen needs, so the nav shows at a
// glance whether it is ready to open; a console-level screen omits it (no context, always openable).
// `roles` (when set) limits the entry to those admin roles — the server enforces the same gate, so
// this only hides what a role cannot use (ADR-0067).
// The nav's entries, their labels and their scopes all come from `state/screens`, which the router
// reads too — see that module for why these were merged. What stays here is only how they are
// *rendered*: the grouping headings, the role gate, and the context dot.

// Whether the signed-in admin's role clears a nav entry's role gate. An entry with no `roles` is open
// to all; a role-gated entry stays hidden until whoami loads (a brief, safe absence rather than a
// flash of an area the role cannot use).
function navItemVisible(id: ScreenId): boolean {
  const roles = specOf(id).roles;
  if (!roles) {
    return true;
  }
  const role = actingAdmin()?.role;
  return role !== undefined && (roles as readonly AdminRole[]).includes(role);
}

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

  // Learn who is signed in once the authenticated frame mounts, so the nav can gate the roster to
  // the roles that may reach it and the screens can greet the operator. A failure here is not fatal:
  // the session is already proven live (the Shell only renders when authed), so we simply leave the
  // role-gated entries hidden rather than blocking the console.
  onMount(() => {
    void api
      .whoami()
      .then(setActingAdmin)
      .catch(() => setActingAdmin(null));
  });

  const logout = async () => {
    await api.logout().catch(() => undefined);
    setAuthed(false);
    setActingAdmin(null);
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
    const screen = screenAtPath(screenPathOf(location.pathname));
    trail.push(screen ? t(screen.key) : t("app.title"));
    return trail;
  };

  return (
    <div class="flex min-h-full flex-col">
      <header class="flex flex-wrap items-center gap-3 border-b border-line bg-surface px-4 py-3">
        <span class="text-lg font-semibold text-ink">{t("app.title")}</span>
        <div class="flex flex-1 flex-wrap items-center gap-2">
          <ContextPicker />
        </div>
        <button
          type="button"
          aria-label={t("palette.open")}
          title={t("palette.open")}
          onClick={openPalette}
          class="flex min-h-touch items-center rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
        >
          <span aria-hidden="true">🔎</span>
        </button>
        <NotificationBell />
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
              {(group) => {
                const items = () => group.items.filter(navItemVisible);
                return (
                  <Show when={items().length > 0}>
                    <div>
                      <p class="px-3 py-1 text-xs font-medium uppercase tracking-wide text-ink-muted">
                        {t(group.key)}
                      </p>
                      <ul class="flex flex-wrap gap-1 md:flex-col">
                        <For each={items()}>
                          {(id) => (
                            <li>
                              <A
                                // The link carries the working context, so copying it out of the
                                // address bar gives somebody else the same screen on the same tenant.
                                href={screenHref(id, tenantId(), storeId())}
                                end={specOf(id).path === "/"}
                                class="flex items-center justify-between gap-2 rounded-token px-3 py-2 text-base text-ink hover:bg-surface-raised"
                                activeClass="bg-surface-raised font-semibold"
                              >
                                <span>{t(specOf(id).key)}</span>
                                <Show when={specOf(id).scope}>
                                  {(scope) => (
                                    <span
                                      aria-label={scopeHint(scope())}
                                      title={scopeHint(scope())}
                                      class={`h-2 w-2 shrink-0 rounded-full border ${
                                        contextReady(scope())
                                          ? "border-accent bg-accent"
                                          : "border-line bg-transparent"
                                      }`}
                                    />
                                  )}
                                </Show>
                              </A>
                            </li>
                          )}
                        </For>
                      </ul>
                    </div>
                  </Show>
                );
              }}
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
      <ToastHost />
      <CommandPalette />
    </div>
  );
}
