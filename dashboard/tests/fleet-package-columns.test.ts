// The fleet's config delivery column sorts the store a published change reaches slowest first
// ([ADR-0195](../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)): one on the poll
// alone, then one that never said, then one that hears the wake.

import { describe, expect, it } from "vitest";

import type { FleetStore } from "../src/api/schemas";
import { deliveryRank, packageRank, tillsBehind } from "../src/screens/fleet/package-columns";

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

// A till that missed a `config_applied` draws the version it read before (roadmap-v3 B7.3): the
// package column counts the tills behind, and a store with some sorts after a refusal and before
// the rest.
describe("tills behind", () => {
  const store = (devices: readonly boolean[] | null, refused = false) =>
    ({
      package: { package_version_id: "V2", ...(refused ? { refused: { reason: "EDGE_TOO_OLD" } } : {}) },
      devices:
        devices === null
          ? null
          : {
              reported_at_ms: 0,
              tills: devices.map((behind, n) => ({ device_id: `D${n}`, package_version_id: behind ? "V1" : "V2", read_at_ms: 0, behind })),
            },
    }) as unknown as FleetStore;

  it("counts the tills on another version, and says nothing for a store that never reported", () => {
    expect(tillsBehind(store([false, true, true]))).toEqual({ behind: 2, count: 3 });
    expect(tillsBehind(store([]))).toEqual({ behind: 0, count: 0 });
    expect(tillsBehind(store(null))).toBeNull();
  });

  it("sorts a refusal first, then a store with tills behind", () => {
    expect(packageRank(store([true], true))).toBe(0);
    expect(packageRank(store([false, true]))).toBe(1);
    expect(packageRank(store([false]))).toBe(2);
    expect(packageRank(store(null))).toBe(2);
  });
});
