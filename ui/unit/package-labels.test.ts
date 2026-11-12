// A package's label shows from the till's catalogue, then from the tenant's grid the store holds
// (ADR-0043) in the till's language and then English, and never as the raw key.

import { afterEach, describe, expect, it } from "vitest";

import { setLocale } from "../src/i18n";
import { packageLabel } from "../src/package/words";
import { applyLabels } from "../src/state/labels";

describe("a package label", () => {
  afterEach(() => {
    applyLabels({ running: null });
    setLocale("en");
  });

  it("comes from the tenant's grid in the till's language, then English", () => {
    applyLabels({
      running: { labels: { "data.reservation": { en: "Reservation", vi: "Đặt bàn" }, "data.party": { en: "Party" } } },
    });
    expect(packageLabel("data.reservation")).toBe("Reservation");
    setLocale("vi");
    expect(packageLabel("data.reservation")).toBe("Đặt bàn");
    expect(packageLabel("data.party")).toBe("Party");
  });

  it("draws nothing for a key the grid lacks, never the key", () => {
    applyLabels({ running: { labels: {} } });
    expect(packageLabel("data.unknown")).toBe("");
    expect(packageLabel(undefined)).toBe("");
  });
});
