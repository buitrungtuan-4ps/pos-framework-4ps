// The fleet's config delivery column sorts the store a published change reaches slowest first
// ([ADR-0195](../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)): one on the poll
// alone, then one that never said, then one that hears the wake.

import { describe, expect, it } from "vitest";

import type { FleetStore } from "../src/api/schemas";
import { deliveryRank } from "../src/screens/fleet/package-columns";

const row = (delivery: string | null) =>
  ({
    config_wake: delivery === null ? null : { delivery, samples: 0 },
  }) as unknown as FleetStore;

describe("deliveryRank", () => {
  it("puts a store on the poll alone first and one hearing the wake last", () => {
    const ranked = [row("CONFIG_DELIVERY_WAKE"), row(null), row("CONFIG_DELIVERY_POLL")]
      .map((store) => deliveryRank(store))
      .sort((a, b) => a - b);
    expect(ranked).toEqual([0, 1, 2]);
    expect(deliveryRank(row("CONFIG_DELIVERY_POLL"))).toBeLessThan(deliveryRank(row(null)));
    expect(deliveryRank(row(null))).toBeLessThan(deliveryRank(row("CONFIG_DELIVERY_WAKE")));
  });
});
