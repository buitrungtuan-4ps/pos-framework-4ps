// The dev server's proxy must not swallow a screen the router serves (Wave 3 · D4).
//
// `vite.config.ts` forwards the API surface to a locally running pos_cloud so `pnpm dev` is a live
// console. Vite matches a plain proxy key as a **prefix**, and the console has a screen at
// `/admins` — so the key `"/admin"` sent that navigation to pos_cloud, which answered with the
// *built* `index.html` out of `dist/`, whose hashed asset paths do not exist on the dev server. The
// Admins screen was a white page with two 404s for everyone working in `pnpm dev`, and nothing said
// why.
//
// The point of testing it here rather than asserting the three keys directly is that the collision
// is structural, not specific: it recurs whenever a new screen path happens to start with an
// existing proxy prefix, or a new proxy prefix happens to start a screen path. This checks every
// route the router actually serves against every key the proxy actually has, so the next collision
// fails here instead of on someone's afternoon.

import { describe, expect, it } from "vitest";

import viteConfig from "../vite.config";
import { SCREENS, type ScreenId, TENANT_PREFIX } from "../src/state/screens";

/** The three public routes `App.tsx` serves outside the auth guard. */
const PUBLIC_ROUTES = ["/login", "/setup", "/invite"];

const TENANT = "01M22190WCY5PS7KCA7ET7H679";

/** Every path a browser can navigate to and expect the SPA, not the API. */
function spaRoutes(): string[] {
  const ids = Object.keys(SCREENS) as ScreenId[];
  const routes = [...PUBLIC_ROUTES, "/"];
  for (const id of ids) {
    const { path, tenantScoped } = SCREENS[id];
    // A console-level screen keeps its bare path. A tenant-scoped one lives under the tenant
    // prefix — and `App.tsx` *also* registers its bare path, redirecting old bookmarks through the
    // remembered tenant, so both shapes are navigable and both belong in this check.
    routes.push(path);
    if (tenantScoped) {
      routes.push(`${TENANT_PREFIX}/${TENANT}${path === "/" ? "" : path}`);
    }
  }
  return [...new Set(routes)];
}

/**
 * Vite's own rule: a key beginning with `^` is a regular expression tested against the request path;
 * anything else matches as a literal prefix.
 */
function proxyMatches(key: string, path: string): boolean {
  return key.startsWith("^") ? new RegExp(key).test(path) : path.startsWith(key);
}

function proxyKeys(): string[] {
  const proxy = viteConfig.server?.proxy;
  expect(proxy, "vite.config.ts should still declare a dev proxy").toBeTruthy();
  return Object.keys(proxy ?? {});
}

describe("the dev-server proxy", () => {
  it("declares keys for the API surface the console calls", () => {
    const keys = proxyKeys();
    expect(keys.some((key) => key.includes("/admin"))).toBe(true);
    expect(keys.some((key) => key.includes("/v1"))).toBe(true);
    expect(keys.some((key) => key.includes("/health"))).toBe(true);
  });

  it("forwards the API calls the console actually makes", () => {
    const keys = proxyKeys();
    const forwarded = (path: string) => keys.some((key) => proxyMatches(key, path));
    // One from each surface, taken from `src/api/client.ts`.
    expect(forwarded("/admin/session")).toBe(true);
    expect(forwarded("/admin/tenants")).toBe(true);
    expect(forwarded(`/admin/stores/${TENANT}/config`)).toBe(true);
    expect(forwarded("/v1/orders")).toBe(true);
    expect(forwarded("/health")).toBe(true);
  });

  it("leaves every screen the router serves to the SPA", () => {
    const keys = proxyKeys();
    const swallowed = spaRoutes().flatMap((route) =>
      keys.filter((key) => proxyMatches(key, route)).map((key) => `${route} → proxy key ${key}`),
    );
    expect(swallowed).toEqual([]);
  });
});
