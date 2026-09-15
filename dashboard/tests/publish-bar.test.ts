// The publish bar's two rules, as rules rather than as pixels (Wave 4 · PR-5b, findings F13 and F4).
//
// Thirteen screens had written their own publish control and not one of them said whether the thing
// in front of the operator was already on the shop floor — the run found somebody pressing Publish
// twice because the screen gave them no way to tell. `PublishBar` is the shared rendering; what is
// worth pinning is the two pure functions underneath it, because they are where a wrong answer
// would be *convincing*: a bar that confidently says "on this store" about a node that is not.

import { describe, expect, it } from "vitest";

import { publishState } from "../src/components/kit";
import { staleNodes } from "../src/lib/published";
import type { ConfigNode } from "../src/api/types";

const node = (name: string, versionId: string | null, atMs: number | null): ConfigNode => ({
  node: name,
  version_id: versionId,
  at_ms: atMs,
});

describe("where a node stands", () => {
  it("is `never` when nothing has been published, whatever the screen has edited", () => {
    expect(publishState(null, Date.now())).toBe("never");
    expect(publishState(undefined, null)).toBe("never");
  });

  it("is `published` when the screen cannot tell when its own data changed", () => {
    // Half the screens author records with no `updated_at`. The honest answer there is the date of
    // the publish and no claim about staleness — not a guess in either direction.
    expect(publishState(1_000, null)).toBe("published");
    expect(publishState(1_000, undefined)).toBe("published");
  });

  it("is `stale` only when the edit is strictly newer than the publish", () => {
    expect(publishState(1_000, 2_000)).toBe("stale");
    expect(publishState(2_000, 1_000)).toBe("published");
    // Equal is not stale: a publish writes its own node, so two clocks agreeing means the publish
    // is the later event. Reading it the other way would leave every screen saying "publish again"
    // immediately after a successful publish.
    expect(publishState(1_000, 1_000)).toBe("published");
  });
});

describe("which nodes a store has not picked up", () => {
  const nodes = [
    node("locale", "01J0000000000000000000000A", 1),
    node("tax", "01J0000000000000000000000C", 3),
    node("menu", "01J0000000000000000000000B", 2),
  ];

  it("names the nodes published after the version the store holds", () => {
    // ULIDs sort lexicographically by time, which is why a string comparison is the right one.
    expect(staleNodes(nodes, "01J0000000000000000000000A")).toEqual(["tax", "menu"]);
  });

  it("names nothing when the store holds the newest version", () => {
    expect(staleNodes(nodes, "01J0000000000000000000000C")).toEqual([]);
  });

  it("names nothing when the store holds no version at all", () => {
    // Not "everything is stale": a store holding nothing has not been installed, which the hub's
    // own posture rules say better than a list of thirteen node names.
    expect(staleNodes(nodes, null)).toEqual([]);
  });

  it("does not call a node stale when its version was never recorded", () => {
    // The pre-field case from PR-5a. The node is published — it is in the effective document — and
    // this read cannot tell when, so a red line on it would be an invention.
    expect(staleNodes([node("permissions", null, null)], "01J0000000000000000000000A")).toEqual([]);
  });
});
