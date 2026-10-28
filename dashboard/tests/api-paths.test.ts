// The `/admin` route types (ADR-0177 decision 3): `tsc` refuses a call the document does not
// declare. The `@ts-expect-error` lines are the test — if one of them stops being an error, `tsc`
// fails on the unused directive — and the runtime assertions only keep Vitest from calling the
// file empty.

import { describe, expect, it } from "vitest";

import type { AdminCall, AdminPath, AdminUrl, AdminUrlFor } from "../src/api/paths";

describe("admin route types", () => {
  it("accept a declared route, filled in, with or without a query", () => {
    const route: AdminPath = "/admin/stores/{store_id}/config";
    const url: AdminUrl<"/admin/stores/{store_id}/config"> = `/admin/stores/${"s1"}/config`;
    const read: AdminUrlFor<"GET"> = `/admin/fees?tenant_id=${"t1"}`;
    const call: AdminCall = ["POST", "/admin/fees/publish"];
    expect([route, url, read, call[0]]).toHaveLength(4);
  });

  it("refuse a route the document does not declare", () => {
    // @ts-expect-error -- no such route
    const typo: AdminUrlFor<"GET"> = "/admin/feez";
    // @ts-expect-error -- `/admin` routes only
    const elsewhere: AdminUrlFor<"GET"> = "/api/orders";
    // @ts-expect-error -- a route name, not a URL: no placeholders
    const unfilled: AdminPath = "/admin/stores/s1/config";
    expect([typo, elsewhere, unfilled]).toHaveLength(3);
  });

  it("refuse a method the route does not answer", () => {
    // @ts-expect-error -- `/admin/fees/publish` takes POST only
    const read: AdminUrlFor<"GET"> = "/admin/fees/publish";
    // @ts-expect-error -- the tuple pairs the method with the route
    const call: AdminCall = ["DELETE", "/admin/fees/publish"];
    expect([read, call]).toHaveLength(2);
  });
});
