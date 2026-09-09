// The frame every authenticated screen sits in (ADR-0060, Track F1): a top bar with the working
// tenant/store and the account menu; a grouped, scope-aware left nav; a breadcrumb strip; and a
// version footer. The locale switch and sign-out live in the account menu (Stage 6) — see
// `AccountMenu.tsx` for why they were folded in rather than standing beside it. The context inputs persist per browser (state/session.ts) and are a
// convenience, not an authorisation — the server's session cookie is what gates every call.

import { createEffect, createSignal, For, onMount, type ParentProps, Show } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import type { AdminRole } from "../api/types";
import { t } from "../i18n";
import { groupOpen, holdsCurrent, loadOpened, type Opened, rememberOpened } from "../lib/nav-groups";
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
import { AccountMenu } from "./AccountMenu";
import { CommandPalette, openPalette } from "./CommandPalette";
import { Icon } from "./icons";
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

  // Which group the operator opened, `null` if they closed the one that was open, and `undefined`
  // if they have never said — which is when the containment rule in `lib/nav-groups.ts` answers.
  const [opened, setOpened] = createSignal<Opened | undefined>(loadOpened());
  // The screen the operator is standing on, by id. Matched on the path alone, so it is exact: the
  // wizard at `/stores/new` is `newStore` and not a second, dimmer version of `stores`.
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
      // From `md` up the shell is the viewport: the header stays, and the nav and the page scroll
      // in their own boxes. That is what stops a long nav from making the whole page tall and
      // pushing the page's own content off the bottom. Below `md` nothing is bounded, because
      // there the nav is a disclosure that pushes the page down — squeezing the page into a
      // shrinking box instead would be a different, worse control. The header, the breadcrumb and
      // the footer are `shrink-0` for the same reason the boxes exist: with the shell bounded, a
      // short viewport would otherwise squeeze the fixed furniture instead of the scrolling page.
      class="flex min-h-full flex-col md:h-full md:overflow-hidden"
      onKeyDown={(event) => {
        if (event.key === "Escape" && navOpen()) {
          setNavOpen(false);
        }
      }}
    >
      <header class="flex shrink-0 flex-wrap items-center gap-3 border-b border-line bg-surface px-4 py-3">
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
        <AccountMenu onSignOut={() => void logout()} />
      </header>
      <div class="flex flex-1 flex-col md:min-h-0 md:flex-row">
        <nav
          id="console-nav"
          aria-label={t("nav.menu")}
          class={`${navOpen() ? "block" : "hidden"} border-b border-line bg-surface md:block md:w-60 md:shrink-0 md:overflow-y-auto md:border-b-0 md:border-r`}
        >
          {/* One click of vertical rhythm between groups: with the accordion the headings sit
              together as a list, and a wide gap between them reads as eight separate things
              rather than one index. */}
          <div class="flex flex-col gap-0.5 p-2">
            <For each={NAV_GROUPS}>
              {(group) => {
                const items = () => group.items.filter(navItemVisible);
                const open = () =>
                  groupOpen({
                    key: group.key,
                    items: group.items,
                    opened: opened(),
                    current: openScreen(),
                  });
                // A closed group holding the open screen says so, because the entry that would
                // have said it is `hidden`. Without this the accordion can put you somewhere the
                // nav does not admit to — the one way collapsing groups can lose an operator.
                const holdsIt = () => !open() && holdsCurrent(group.items, openScreen());
                return (
                  <Show when={items().length > 0}>
                    <div>
                      {/* The heading is the toggle, and its accessible name is the group's own name:
                          `aria-expanded` carries the state, so a second label saying "collapse"
                          would be the state announced twice and wrong half the time.

                          Open and closed are told apart three ways at once — the ink goes from
                          muted to full, the weight from medium to semibold, and the marker turns —
                          because one of the three was all the old heading had and it was the
                          smallest one. `md:min-h-8` is the density: 48px is the touch minimum and
                          it stays below `md`, where a heading is tapped; with a mouse a 32px row
                          is the difference between eight headings that fit and eight that scroll. */}
                      <button
                        type="button"
                        aria-expanded={open()}
                        aria-controls={`nav-group-${group.key}`}
                        onClick={() => setOpened(rememberOpened(open() ? null : group.key))}
                        class={`flex min-h-touch w-full items-center justify-between gap-2 rounded-token px-3 text-xs uppercase tracking-wide transition-colors hover:bg-canvas md:min-h-8 ${
                          open() ? "font-semibold text-ink" : "font-medium text-ink-muted"
                        }`}
                      >
                        <span class="flex items-center gap-2">
                          {t(group.key)}
                          <Show when={holdsIt()}>
                            <span
                              aria-label={t("nav.groupHoldsCurrent")}
                              title={t("nav.groupHoldsCurrent")}
                              class="h-1.5 w-1.5 shrink-0 rounded-full bg-selected-ink"
                            />
                          </Show>
                        </span>
                        {/* One glyph turned rather than two glyphs swapped: the turn is the same
                            information and it shows the two states are one control. The house
                            reduced-motion rule in `app.css` flattens the transition to nothing,
                            so the rotation still lands — it just does not travel. */}
                        <span
                          aria-hidden="true"
                          class={`shrink-0 transition-transform ${open() ? "rotate-90" : ""}`}
                        >
                          ▸
                        </span>
                      </button>
                      <ul
                        id={`nav-group-${group.key}`}
                        class="flex flex-col gap-0.5"
                        hidden={!open()}
                      >
                        <For each={items()}>
                          {(id) => {
                            const scope = () => specOf(id).scope;
                            const blocked = () => {
                              const need = scope();
                              return need !== undefined && !contextReady(need);
                            };
                            // Read from the same table the router reads, rather than from the
                            // link's own `activeClass`: that matches on a path *prefix*, so the
                            // wizard at `/stores/new` used to light Stores up as well. `<A>` still
                            // sets `aria-current` itself, on an exact match, which is the half a
                            // screen reader needs and the half this cannot get wrong.
                            const here = () => openScreen() === id;
                            return (
                              <li>
                                <A
                                  // The link carries the working context, so copying it out of the
                                  // address bar gives somebody else the same screen on the same
                                  // tenant.
                                  href={screenHref(id, tenantId(), storeId())}
                                  end={specOf(id).path === "/"}
                                  // One class per state rather than a base plus an `activeClass`:
                                  // the two would set the same properties under different
                                  // variants, and which of `hover:bg-canvas` and an active
                                  // `bg-selected` wins is decided by their order in the generated
                                  // stylesheet — which a class list cannot state. The row you are
                                  // on is an accent tint, not the `surface-raised` that every
                                  // hovered row also wore.
                                  class={`flex min-h-touch items-center justify-between gap-2 rounded-token px-3 py-2 text-base transition-colors md:min-h-0 md:py-1.5 ${
                                    here()
                                      ? "bg-selected font-semibold text-selected-ink"
                                      : `hover:bg-canvas ${blocked() ? "text-ink-muted" : "text-ink"}`
                                  }`}
                                >
                                  <span class="flex items-center gap-2">
                                    <Icon name={specOf(id).icon} />
                                    {t(specOf(id).key)}
                                  </span>
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
        <div class="flex flex-1 flex-col md:min-h-0 md:min-w-0">
          <nav
            aria-label={t("shell.breadcrumb")}
            class="flex shrink-0 flex-wrap items-center gap-1 border-b border-line px-4 py-2 text-sm text-ink-muted md:px-6"
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
          <footer class="shrink-0 border-t border-line px-4 py-2 text-xs text-ink-muted md:px-6">
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
