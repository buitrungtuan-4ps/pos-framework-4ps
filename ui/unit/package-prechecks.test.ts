// `package/prechecks.ts` names, per act, refusals its catalogue entry lists (ADR-0196 safeguard 2):
// the till checks a part of what the edge refuses, never something the edge does not.

import { describe, expect, it } from "vitest";

import { ACTIONS } from "../../packages/package-renderer/src/actions";
import { PRECHECKED, WRITES_PENDING } from "../src/package/prechecks";

describe("the till's pre-checks", () => {
  it("are each a refusal the act's catalogue entry lists, or the till's own pending write", () => {
    for (const [id, tokens] of Object.entries(PRECHECKED)) {
      const entry = ACTIONS.find((action) => action.action_id === id);
      expect(entry, id).toBeDefined();
      expect(entry?.effect, id).toBe("ACTION_EFFECT_MONEY");
      for (const token of tokens ?? []) {
        if (token !== WRITES_PENDING) expect(entry?.refusals, `${id} ${token}`).toContain(token);
      }
    }
  });
});
