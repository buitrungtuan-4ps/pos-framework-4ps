// A chain break on the Alerts screen leads to Chain integrity (ADR-0073 amendment 2026-10-31).
//
// The alert says *that* a store's log disagrees with itself; the Chain integrity screen says *where*.
// The link is the whole of the console half of this change, so it is pinned: built in the alert's own
// tenant, and only for the one kind that has a screen to go to.

import { describe, expect, it } from "vitest";

import en from "../src/i18n/en.json";
import vi from "../src/i18n/vi.json";
import { chainIntegrityHref, scopeWords } from "../src/screens/Alerts";

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

describe("an alert's scope in words (O11)", () => {
  const names = new Map([
    ["01KSTORE0000000000000000BT", "Bến Thành"],
    ["01KTENANT000000000000000PF", "Pizza Demo"],
  ]);
  it("names the store an alert is about, by its name", () => {
    const words = scopeWords({ tenant_id: "01KTENANT000000000000000PF", detail: { store_id: "01kstore0000000000000000bt" } }, names);
    expect(words).toContain("Bến Thành");
    expect(words).not.toContain("01K");
  });
  it("names the tenant for a tenant-wide alert, and the fleet for none", () => {
    expect(scopeWords({ tenant_id: "01KTENANT000000000000000PF", detail: {} }, names)).toBe("Pizza Demo");
    expect(scopeWords({ tenant_id: null, detail: null }, names)).not.toBe("");
  });
  it("falls back to the id where no name is known", () => {
    expect(scopeWords({ tenant_id: "01KUNKNOWN", detail: null }, names)).toBe("01KUNKNOWN");
  });
});
