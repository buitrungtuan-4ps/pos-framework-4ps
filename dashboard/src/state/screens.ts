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
import type { IconName } from "../components/icons";

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
  /**
   * The glyph the nav and the palette draw beside its name.
   *
   * Required, so a screen added without one is a compile error rather than the single gap in a
   * column of thirty icons. `IconName` is a type-only import: the union is checked here and the
   * geometry stays in the component that draws it.
   */
  readonly icon: IconName;
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
    icon: "gauge",
  },
  reports: {
    path: "/reports",
    key: "nav.reports",
    scope: "store",
    tenantScoped: true,
    inPalette: true,
    icon: "chart-column",
  },
  fleet: { path: "/fleet", key: "nav.fleet", scope: "tenant", tenantScoped: true, icon: "server" },
  ota: {
    path: "/ota",
    key: "nav.ota",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "arrow-down-to-line",
  },
  reconcile: {
    path: "/reconcile",
    key: "nav.reconcile",
    scope: "tenant",
    tenantScoped: true,
    icon: "refresh-cw",
  },
  // Console-level: alerts and the audit trail span every tenant, including server-wide conditions
  // that belong to none (ADR-0073), so neither takes a tenant in its URL.
  alerts: { path: "/alerts", key: "nav.alerts", tenantScoped: false, icon: "triangle-alert" },
  audit: { path: "/audit", key: "nav.audit", tenantScoped: false, icon: "history" },

  stores: {
    path: "/stores",
    key: "nav.stores",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "store",
  },
  newStore: {
    path: "/stores/new",
    key: "wizard.title",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "plus-circle",
  },
  // Store groups (ADR-0122): the named cohorts a tenant publishes to as one. Owner/admin only,
  // matching the server — organising the estate and pushing configuration to a slice of it are
  // both estate-level acts, not day-to-day authoring.
  storeGroups: {
    path: "/store-groups",
    key: "nav.storeGroups",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    inPalette: true,
    icon: "layers",
  },
  catalog: {
    path: "/catalog",
    key: "nav.catalog",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "book-open",
  },
  campaigns: {
    path: "/campaigns",
    key: "nav.campaigns",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "megaphone",
  },
  inventory: {
    path: "/inventory",
    key: "nav.inventory",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "package",
  },
  // Reason codes (ADR-0115): the managed list a void, discount, comp, refund, drawer opening, staff
  // rejection, cash movement or stock correction must cite. Owner/admin only, matching the server's
  // console.reason_codes.manage — authoring the list is what upholds or defeats the fraud control.
  reasonCodes: {
    path: "/reason-codes",
    key: "nav.reasonCodes",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "clipboard-list",
  },
  channels: {
    path: "/channels",
    key: "nav.channels",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "credit-card",
  },
  media: {
    path: "/media",
    key: "nav.media",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "image",
  },
  layout: {
    path: "/layout",
    key: "nav.layout",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "layout-grid",
  },
  floor: {
    path: "/floor",
    key: "nav.floor",
    scope: "store",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "armchair",
  },
  stations: {
    path: "/stations",
    key: "nav.stations",
    scope: "store",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "chef-hat",
  },
  people: {
    path: "/people",
    key: "nav.people",
    scope: "tenant",
    roles: ADMIN_MANAGERS,
    tenantScoped: true,
    icon: "users",
  },

  config: {
    path: "/config",
    key: "nav.config",
    scope: "store",
    tenantScoped: true,
    inPalette: true,
    icon: "file-cog",
  },
  storeSettings: {
    path: "/store-settings",
    key: "nav.storeSettings",
    scope: "store",
    tenantScoped: true,
    icon: "settings",
  },
  taxRates: { path: "/tax-rates", key: "nav.taxRates", scope: "tenant", tenantScoped: true, icon: "percent" },
  translations: {
    path: "/translations",
    key: "nav.translations",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "languages",
  },
  subjects: {
    path: "/subjects",
    key: "nav.subjects",
    scope: "tenant",
    roles: ["owner"],
    tenantScoped: true,
    icon: "shield-user",
  },

  apiKeys: {
    path: "/api-keys",
    key: "nav.apiKeys",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "key",
  },
  devices: {
    path: "/devices",
    key: "nav.devices",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "monitor-smartphone",
  },
  activation: {
    path: "/activation",
    key: "nav.activation",
    scope: "store",
    tenantScoped: true,
    inPalette: true,
    icon: "plug-zap",
  },
  // Console-level: the admin roster is the console's own users, not a tenant's.
  admins: { path: "/admins", key: "nav.admins", roles: ADMIN_MANAGERS, tenantScoped: false, icon: "user-cog" },

  webhooks: {
    path: "/webhooks",
    key: "nav.webhooks",
    scope: "tenant",
    tenantScoped: true,
    inPalette: true,
    icon: "webhook",
  },

  // Console-level: the signed-in admin's own sessions and security, which no tenant owns.
  mySessions: { path: "/my-sessions", key: "nav.mySessions", tenantScoped: false, icon: "monitor-check" },
  mySecurity: { path: "/my-security", key: "nav.mySecurity", tenantScoped: false, icon: "shield-check" },
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

/**
 * The nav's grouping, referring to screens by id so a rename cannot silently orphan an entry.
 *
 * # Why these eight, and not the six that were here
 *
 * The six were sized by how the console was built rather than by what an operator does with it, and
 * two of them had become dumping grounds. "Master data" held eleven entries — the menu, the floor
 * plan, the kitchen stations, the staff roster, the reason codes, the stores — which is not a
 * category, it is everything that is not a report. And a category nobody can predict the contents
 * of is worse than no category: you cannot guess where a screen lives, so you read all thirty names
 * every time.
 *
 * Two of the six also filed screens under headings that invited the wrong conclusion, which is the
 * more expensive failure:
 *
 *   - **Subject requests** sat under Settings. It is the instrument that erases or exports a named
 *     customer's data under Decree 13 — nobody looks for that beside the store's opening hours, and
 *     a setting is exactly what it must not read as. It now sits under Compliance with the audit
 *     trail, which is the other thing an inspector asks for.
 *   - **People** sat beside **Admins** under a heading about access. People are shop staff with
 *     PINs on a till; admins are console users with a password and a role over the whole tenant.
 *     Filing them together invites the belief that adding a person grants console access. They are
 *     now two groups apart: People is operations, Admins is Access.
 *
 * The rest follows the same test — can somebody who has never seen this console guess the heading
 * from the task? The menu, the till layout, its pictures, the channels it is sold through, the
 * promotions on it and the tax on it are all one job (Menu & pricing). The floor, the stations, the
 * stock, the reason codes and the roster are the shift (Operations). Stores, their boxes, their
 * devices and the updates and reconciliation those boxes need are the estate.
 *
 * Six entries is the ceiling, so a group is always readable at a glance — the largest is now six,
 * where it was eleven.
 *
 * # Why a group has no icon
 *
 * It used to have one. With the accordion (`lib/nav-groups.ts`) the headings are the surface an
 * operator scans — eight of them, always visible — and the entries are the transient half, so an
 * icon on the heading put a second glyph column above and inset from the entries' own. Two columns
 * of glyphs at two indents is what the eye has to sort through before it can read anything, and the
 * entry icons are the ones earning their place. There is also no honest set of eight: the vendored
 * geometry (`components/icons.tsx`) holds one glyph per screen, so five of eight headings would
 * have had to wear a glyph already worn by one of their own entries.
 */
export const NAV_GROUPS: readonly {
  key: MessageKey;
  items: readonly ScreenId[];
}[] = [
  // Is the shop all right, what did it make, and what is on fire.
  { key: "nav.group.overview", items: ["storeHub", "reports", "alerts"] },
  // What is sold and for how much: the menu, how it is laid out on a till, its pictures, the
  // channels it is sold through, the promotions on it and the tax on it.
  {
    key: "nav.group.menu",
    items: ["catalog", "layout", "media", "channels", "campaigns", "taxRates"],
  },
  // Running the shift: the room, the kitchen, the stock behind it, the reasons a till may be
  // overridden and the people who do the overriding.
  {
    key: "nav.group.operations",
    items: ["floor", "stations", "inventory", "reasonCodes", "people"],
  },
  // The estate: the shops, the boxes running them, the terminals in them, and the two jobs those
  // boxes need done to them (an update, and a check that nothing was lost).
  {
    key: "nav.group.estate",
    items: ["stores", "fleet", "devices", "activation", "ota", "reconcile"],
  },
  // How a store behaves, in its own words rather than in JSON — and, since ADR-0122, how a *set*
  // of them is made to behave the same. Store groups sit here rather than under the estate for the
  // reason the record gives: a group holds no configuration and changes no store's identity, it is
  // the cohort a configuration change is delivered to. The estate group is about the boxes and
  // their lifecycle; this one is about what runs on them.
  {
    key: "nav.group.settings",
    items: ["config", "storeGroups", "storeSettings", "translations"],
  },
  // Who and what may reach this console: console users, machine keys, and the endpoints it calls
  // out to. All three answer "who is allowed in, or out".
  { key: "nav.group.access", items: ["admins", "apiKeys", "webhooks"] },
  // What an auditor or a regulator asks for: what was done, by whom, and what a named person's
  // data may be made to do (ADR-0076, Decree 13).
  { key: "nav.group.compliance", items: ["audit", "subjects"] },
  // The signed-in admin's own account, which belongs to no tenant.
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
  const id = screenIdAtPath(path);
  return id === undefined ? undefined : SCREENS[id];
}

/**
 * The id of the screen at `path`, or `undefined`.
 *
 * The id rather than the spec, for the callers that need to ask a question *about* the screen rather
 * than render it — the nav asks which group holds the open screen, and a group lists ids.
 */
export function screenIdAtPath(path: string): ScreenId | undefined {
  const ids = Object.keys(SCREENS) as ScreenId[];
  return ids.find((id) => SCREENS[id].path === path);
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
