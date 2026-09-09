// The frame every authenticated screen sits in (ADR-0060, Track F1): a top bar with the working
// tenant/store, the locale switch and logout; a grouped, scope-aware left nav; a breadcrumb strip;
// and a version footer. The context inputs persist per browser (state/session.ts) and are a
// convenience, not an authorisation — the server's session cookie is what gates every call.

import { createEffect, createSignal, For, onMount, type ParentProps, Show } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import type { AdminRole } from "../api/types";
import { LOCALES, type Locale, locale, localeName, setLocale, t } from "../i18n";
import { groupOpen, loadRemembered, remember, type Remembered } from "../lib/nav-groups";
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
  screenIdAtPath,
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

// Which piece of context a nav entry is waiting on.
//
// Called only for an entry whose context is *not* ready, which is the change. The marker used to be
// drawn for every scoped entry and filled with `bg-accent` — the brand colour, which in this palette
// is red — when the context *was* ready, leaving it hollow and all but invisible when the screen was
// blocked. So a nav full of red meant everything was fine, the entries an operator could not use
// looked like ordinary entries, and the one label a screen reader announced twenty times was
// "Context ready". Every dashboard convention reads a coloured dot as a thing needing attention.
//
// Now a ready entry carries no marker at all — that is the unremarkable state, and marking it is
// noise — and a blocked one is muted and says what it needs. `nav.scopeReady` went with the
// inversion: a label nobody needs on twenty entries.
function scopeHint(scope: Scope): string {
  return scope === "store" ? t("nav.scopeNeedsStore") : t("nav.scopeNeedsTenant");
}

export function Shell(props: ParentProps) {
  const navigate = useNavigate();
  const location = useLocation();

  // The nav under `md`. A disclosure rather than a modal overlay: it stays in the document flow and
  // pushes the page down, so there is no focus trap to get wrong and no scroll to lock. Navigating
  // closes it — an operator who has picked a screen is done with the menu — and so does Escape.
  const [navOpen, setNavOpen] = createSignal(false);
  createEffect(() => {
    // Read the path so this re-runs on every navigation, including one from the command palette.
    void location.pathname;
    setNavOpen(false);
  });

  // Which groups the operator has explicitly opened or closed. A group not in here follows the
  // containment rule in `lib/nav-groups.ts`.
  const [remembered, setRemembered] = createSignal<Remembered>(loadRemembered());
  const openScreen = () => screenIdAtPath(screenPathOf(location.pathname));

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
    <div
      class="flex min-h-full flex-col"
      onKeyDown={(event) => {
        if (event.key === "Escape" && navOpen()) {
          setNavOpen(false);
        }
      }}
    >
      <header class="flex flex-wrap items-center gap-3 border-b border-line bg-surface px-4 py-3">
        <button
          type="button"
          aria-label={t("nav.menu")}
          title={t("nav.menu")}
          aria-expanded={navOpen()}
          aria-controls="console-nav"
          onClick={() => setNavOpen(!navOpen())}
          class="flex min-h-touch items-center rounded-token border border-line bg-surface-raised px-3 text-sm text-ink md:hidden"
        >
          <span aria-hidden="true">☰</span>
        </button>
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
        <nav
          id="console-nav"
          aria-label={t("nav.menu")}
          class={`${navOpen() ? "block" : "hidden"} border-b border-line bg-surface md:block md:w-60 md:border-b-0 md:border-r`}
        >
          <div class="flex flex-col gap-2 p-2">
            <For each={NAV_GROUPS}>
              {(group) => {
                const items = () => group.items.filter(navItemVisible);
                const open = () =>
                  groupOpen({
                    items: group.items,
                    remembered: remembered()[group.key],
                    current: openScreen(),
                  });
                return (
                  <Show when={items().length > 0}>
                    <div>
                      {/* The heading is the toggle, and its accessible name is the group's own name:
                          `aria-expanded` carries the state, so a second label saying "collapse"
                          would be the state announced twice and wrong half the time. */}
                      <button
                        type="button"
                        aria-expanded={open()}
                        aria-controls={`nav-group-${group.key}`}
                        onClick={() => setRemembered(remember(remembered(), group.key, !open()))}
                        class="flex min-h-touch w-full items-center justify-between gap-2 rounded-token px-3 py-1 text-xs font-medium uppercase tracking-wide text-ink-muted transition-colors hover:bg-surface-raised"
                      >
                        <span>{t(group.key)}</span>
                        <span aria-hidden="true">{open() ? "▾" : "▸"}</span>
                      </button>
                      <ul
                        id={`nav-group-${group.key}`}
                        class="flex flex-col gap-1"
                        hidden={!open()}
                      >
                        <For each={items()}>
                          {(id) => {
                            const scope = () => specOf(id).scope;
                            const blocked = () => {
                              const need = scope();
                              return need !== undefined && !contextReady(need);
                            };
                            return (
                              <li>
                                <A
                                  // The link carries the working context, so copying it out of the
                                  // address bar gives somebody else the same screen on the same
                                  // tenant.
                                  href={screenHref(id, tenantId(), storeId())}
                                  end={specOf(id).path === "/"}
                                  class={`flex items-center justify-between gap-2 rounded-token px-3 py-2 text-base transition-colors hover:bg-surface-raised ${
                                    blocked() ? "text-ink-muted" : "text-ink"
                                  }`}
                                  activeClass="bg-surface-raised font-semibold"
                                >
                                  <span>{t(specOf(id).key)}</span>
                                  {/* Only a screen that cannot be opened yet is marked. A screen
                                      that is ready is the unremarkable case and carries nothing. */}
                                  <Show when={blocked() ? scope() : undefined}>
                                    {(need) => (
                                      <span
                                        aria-label={scopeHint(need())}
                                        title={scopeHint(need())}
                                        class="h-2 w-2 shrink-0 rounded-full border border-ink-muted bg-transparent"
                                      />
                                    )}
                                  </Show>
                                </A>
                              </li>
                            );
                          }}
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
