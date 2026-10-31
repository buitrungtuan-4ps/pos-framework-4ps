// A chain break on the Alerts screen leads to Chain integrity (ADR-0073 amendment 2026-10-31).
//
// The alert says *that* a store's log disagrees with itself; the Chain integrity screen says *where*.
// The link is the whole of the console half of this change, so it is pinned: built in the alert's own
// tenant, and only for the one kind that has a screen to go to.

import { describe, expect, it } from "vitest";

import en from "../src/i18n/en.json";
import vi from "../src/i18n/vi.json";
import { chainIntegrityHref } from "../src/screens/Alerts";

const TENANT = "01JTENANT0000000000000000A";

describe("the chain-break alert", () => {
  it("links to Chain integrity in its own tenant", () => {
    expect(chainIntegrityHref({ kind: "chain_broken", tenant_id: TENANT })).toBe(
      `/t/${TENANT}/chain-findings`,
    );
  });

  it("is the only kind that links there", () => {
    expect(chainIntegrityHref({ kind: "store_offline", tenant_id: TENANT })).toBeNull();
    expect(chainIntegrityHref({ kind: "chain_broken", tenant_id: null })).toBeNull();
  });

  it("has a label and a link text in both languages", () => {
    for (const messages of [en, vi] as Record<string, string>[]) {
      expect(messages["alerts.kind.chainBroken"]).toBeTruthy();
      expect(messages["alerts.openChainIntegrity"]).toBeTruthy();
    }
  });
});
