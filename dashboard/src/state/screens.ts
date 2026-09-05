// The console's screens, in one table — the single source of truth for the router, the nav, the
// breadcrumb labels and the command palette (ADR-0060, Track F1).
//
// # Why one table
//
// These four surfaces used to be four hand-maintained lists that had to agree: 28 `<Route>`s in
// `App.tsx`, the nav groups here, a 28-entry `CRUMB_KEY`, and the palette's own 11-entry `TARGETS`.
// Nothing checked them against each other, and the dashboard has no test runner — so a screen added
// to three of the four, or a path typo'd in one, was a 404 or a missing breadcrumb that only a human
// clicking through would find. Every surface now derives from `SCREENS`, and a `ScreenId` is a union
// of its keys: a typo is a compile error, and a screen that exists is automatically routable,
// nameable and reachable from the palette.
//
// # Context in the URL
//
// A tenant-scoped screen lives at `/t/<tenant>/<path>`; the store, when one is chosen, rides along as
// `?store=<id>`. That is the shape the console URL takes so a link can be shared and two tabs can sit
// on different tenants (F1). The tenant is a path segment because every tenant-scoped screen requires
// one — that is what `RequireContext` gates on — while the store is a query parameter because it is
// genuinely optional: most of these screens render perfectly well before a store is picked, and
// forcing an "unset" sentinel into the path would put a placeholder where a real id goes.
//
// A screen with `tenantScoped: false` is console-level — the alert list, the audit trail, the admin
// roster, the account screens — and keeps a bare path, because it has no tenant to carry.

import type { MessageKey } from "../i18n";
import type { AdminRole } from "../api/types";

/** The working context a screen needs before it can do anything useful. */
export type Scope = "tenant" | "store";

/** One screen: where it lives, what it is called, and what it needs. */
export type Screen = {
  /** The path under the tenant prefix, or the absolute path for a console-level screen. */
  readonly path: string;
  /** The i18n key for its name, used by the nav, the breadcrumb and the palette alike. */
  readonly key: MessageKey;
  /**
   * The context the screen needs, shown in the nav so an operator can see at a glance whether it is
   * ready to open. Absent means it needs none.
   */
  readonly scope?: Scope;
  /** When set, only these admin roles see it. The server enforces the same gate (ADR-0067). */
  readonly roles?: readonly AdminRole[];
  /**
   * Whether the screen lives under `/t/<tenant>`. `false` for the console-level screens, which have
   * no tenant to encode.
   */
  readonly tenantScoped: boolean;
  /** When `true`, the command palette offers it. Not every screen is worth a quick-switch entry. */
  readonly inPalette?: boolean;
};

/** The roles that may reach the admin roster: owner and admin (the server gates *changes* to owner). */
const ADMIN_MANAGERS: readonly AdminRole[] = ["owner", "admin"];

export const SCREENS = {
  // The tenant-scoped index is the per-store hub (ADR-0099): the first screen after picking a shop
  // answers "is this shop all right", and Reports — which answers "how much did it make" — moved to
  // its own path. A bookmark of `/t/<tenant>?store=X` therefore lands on the hub now; nothing 404s,
  // and Reports keeps every capability it had.
  storeHub: {
    path: "/",
    key: "nav.storeHub",
    scope: "store",
    tenantScoped: true,
    inPalette: true,
  },
  reports: {
    path: "/reports",
    key: "nav.reports",
    scope: "store",
    tenantScoped: true,
    inPalette: true,
  },
  fleet: { path: "/fleet", key: "nav.fleet", scope: "tenant", tenantScoped: true },
  ota: {
    path: "/ota",
    key: "nav.ota",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },
  reconcile: {
    path: "/reconcile",
    key: "nav.reconcile",
    scope: "tenant",
    tenantScoped: true,
  },
  // Console-level: alerts and the audit trail span every tenant, including server-wide conditions
  // that belong to none (ADR-0073), so neither takes a tenant in its URL.
  alerts: { path: "/alerts", key: "nav.alerts", tenantScoped: false },
  audit: { path: "/audit", key: "nav.audit", tenantScoped: false },

  stores: {
    path: "/stores",
    key: "nav.stores",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },
  newStore: {
    path: "/stores/new",
    key: "wizard.title",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },
  catalog: {
    path: "/catalog",
    key: "nav.catalog",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },
  campaigns: {
    path: "/campaigns",
    key: "nav.campaigns",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },
  inventory: {
    path: "/inventory",
    key: "nav.inventory",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },
  channels: {
    path: "/channels",
    key: "nav.channels",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },
  media: {
    path: "/media",
    key: "nav.media",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },
  layout: {
    path: "/layout",
    key: "nav.layout",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },
  floor: {
    path: "/floor",
    key: "nav.floor",
    scope: "store",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },
  stations: {
    path: "/stations",
    key: "nav.stations",
    scope: "store",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },
  people: {
    path: "/people",
    key: "nav.people",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
  },

  config: {
    path: "/config",
    key: "nav.config",
    scope: "store",
    tenantScoped: true,
    inPalette: true,
  },
  storeSettings: {
    path: "/store-settings",
    key: "nav.storeSettings",
    scope: "store",
    tenantScoped: true,
  },
  taxRates: { path: "/tax-rates", key: "nav.taxRates", scope: "tenant", tenantScoped: true },
  translations: {
    path: "/translations",
    key: "nav.translations",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },
  subjects: {
    path: "/subjects",
    key: "nav.subjects",
    scope: "tenant",
    roles: ["owner"],
    tenantScoped: true,
  },

  apiKeys: {
    path: "/api-keys",
    key: "nav.apiKeys",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },
  devices: {
    path: "/devices",
    key: "nav.devices",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },
  activation: {
    path: "/activation",
    key: "nav.activation",
    scope: "store",
    tenantScoped: true,
    inPalette: true,
  },
  // Console-level: the admin roster is the console's own users, not a tenant's.
  admins: { path: "/admins", key: "nav.admins", roles: ADMIN_MANAGERS, tenantScoped: false },

  webhooks: {
    path: "/webhooks",
    key: "nav.webhooks",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
  },

  // Console-level: the signed-in admin's own sessions and security, which no tenant owns.
  mySessions: { path: "/my-sessions", key: "nav.mySessions", tenantScoped: false },
  mySecurity: { path: "/my-security", key: "nav.mySecurity", tenantScoped: false },
} as const satisfies Record<string, Screen>;

/** Every screen's id. A `ScreenId` that is not in [`SCREENS`] does not compile. */
export type ScreenId = keyof typeof SCREENS;

/**
 * One screen's spec, widened from the `as const` literal.
 *
 * `as const satisfies` is what makes `ScreenId` a union of real keys, but it also gives each entry a
 * literal type that omits the optional fields it does not set — so `SCREENS[id].roles` is a type
 * error on an entry without roles. This reads the entry back as the declared shape.
 */
export function specOf(screen: ScreenId): Screen {
  return SCREENS[screen];
}

/** The nav's grouping, referring to screens by id so a rename cannot silently orphan an entry. */
export const NAV_GROUPS: readonly { key: MessageKey; items: readonly ScreenId[] }[] = [
  {
    key: "nav.group.overview",
    items: ["storeHub", "reports", "fleet", "ota", "reconcile", "alerts", "audit"],
  },
  {
    key: "nav.group.masterData",
    items: [
      "stores",
      "catalog",
      "campaigns",
      "inventory",
      "channels",
      "media",
      "layout",
      "floor",
      "stations",
      "people",
    ],
  },
  {
    key: "nav.group.settings",
    items: ["config", "storeSettings", "taxRates", "translations", "subjects"],
  },
  { key: "nav.group.access", items: ["apiKeys", "devices", "activation", "admins"] },
  { key: "nav.group.integrations", items: ["webhooks"] },
  { key: "nav.group.account", items: ["mySessions", "mySecurity"] },
];

/** The tenant path prefix. One constant so the router and the link builder cannot drift apart. */
export const TENANT_PREFIX = "/t";

/** The query parameter the chosen store rides in. */
export const STORE_PARAM = "store";

/**
 * The URL for `screen`, carrying the working context.
 *
 * A tenant-scoped screen becomes `/t/<tenant><path>`, plus `?store=<store>` when a store is chosen.
 * Without a tenant it falls back to the bare path, which the router redirects to the context picker —
 * that is the pre-context state a fresh install starts in, not an error.
 *
 * A console-level screen ignores both: it has no tenant to carry, and adding one would imply a
 * scoping the screen does not have.
 */
export function screenHref(screen: ScreenId, tenant: string, store: string): string {
  const spec = specOf(screen);
  if (!spec.tenantScoped) {
    return spec.path;
  }
  if (!tenant) {
    return spec.path;
  }
  // The index screen is `/t/<tenant>`, not `/t/<tenant>/`, so the two forms do not both exist.
  const base =
    spec.path === "/"
      ? `${TENANT_PREFIX}/${encodeURIComponent(tenant)}`
      : `${TENANT_PREFIX}/${encodeURIComponent(tenant)}${spec.path}`;
  // A store is only carried where it means something. Appending `?store=` to a screen that never
  // reads one would put a parameter in a shared link that the recipient's screen silently ignores.
  if (store && spec.scope === "store") {
    return `${base}?${STORE_PARAM}=${encodeURIComponent(store)}`;
  }
  return base;
}

/** The screen whose path matches `path`, or `undefined` — used to label the breadcrumb. */
export function screenAtPath(path: string): Screen | undefined {
  return Object.values(SCREENS).find((screen) => screen.path === path);
}

/**
 * The screen path inside `location.pathname` — the part after `/t/<tenant>`, or the whole path for a
 * console-level screen.
 *
 * `/t/01ABC/people` is the `people` screen; so is a bare `/people` arriving from an old bookmark.
 * Both must resolve to the same entry, or the breadcrumb reads "Pizza 4P's" with no page name on
 * exactly the URLs this change introduced.
 */
export function screenPathOf(pathname: string): string {
  if (!pathname.startsWith(`${TENANT_PREFIX}/`)) {
    return pathname;
  }
  const afterPrefix = pathname.slice(TENANT_PREFIX.length + 1);
  const slash = afterPrefix.indexOf("/");
  // `/t/<tenant>` with nothing after it is the index screen.
  return slash === -1 ? "/" : afterPrefix.slice(slash);
}
