// `rules/till-rules.ts`: the till asks the rules module the same questions the edge's re-check is
// built on, over JSON, and a bad request is an answer, never a trap.

import { beforeAll, describe, expect, it } from "vitest";

import { instantiateTillRules, needsRules, type Flow, type TillRules } from "../src/rules/till-rules";
// The module's bytes, inlined as a data URL, as `pnpm rules` built them.
import moduleData from "../src/rules/pos_till_rules.wasm?url&inline";

const DINE_IN: Flow = {
    flow_id: "dine_in",
    start_action_id: "sales.table.seat",
    steps: [
        {
            step_id: "guests",
            action_id: "sales.table.seat",
            kind: "STEP_KIND_REQUIRED",
            applies_when: "channel == 'SALES_CHANNEL_DINE_IN'",
            done_when: "has(order.guest_count) && order.guest_count > 0",
        },
        { step_id: "first_line", action_id: "sales.line.add", kind: "STEP_KIND_REQUIRED", prerequisites: ["guests"] },
    ],
};

let rules: TillRules;

beforeAll(async () => {
    const base64 = moduleData.slice(moduleData.indexOf(",") + 1);
    const bytes = Uint8Array.from(atob(base64), (char) => char.charCodeAt(0));
    rules = await instantiateTillRules(bytes);
    expect(rules.load([DINE_IN])).toEqual({ ok: true, flows: 1, refused: [] });
});

describe("the till's rules module", () => {
    it("names the guest count as the step the first line waits for", () => {
        const facts = { channel: "SALES_CHANNEL_DINE_IN", order: { line_count: 0 } };
        expect(rules.available("dine_in", {}, facts)).toEqual({ ok: true, steps: ["guests"] });
        expect(rules.missing("dine_in", "first_line", {}, facts)).toEqual({ ok: true, missing: "guests" });
    });

    it("sees a step done from the order's own facts, as the edge does", () => {
        const facts = { channel: "SALES_CHANNEL_DINE_IN", order: { guest_count: 3 } };
        expect(rules.missing("dine_in", "first_line", {}, facts)).toEqual({ ok: true, missing: null });
    });

    it("answers a bad request with its reason", () => {
        const fraction = rules.holds("order.total > 1", { order: { total: 1.5 } });
        expect(fraction.ok).toBe(false);
        expect(rules.available("no_such_flow", {}, {}).ok).toBe(false);
        expect(rules.ask({ op: "launch" }).ok).toBe(false);
    });
});

describe("when the till loads the module", () => {
    it("only for flows a condition or a prerequisite can hold back", () => {
        expect(needsRules([])).toBe(false);
        expect(
            needsRules([{ flow_id: "f", start_action_id: "a", steps: [{ step_id: "s", action_id: "a" }] }]),
        ).toBe(false);
        expect(needsRules([DINE_IN])).toBe(true);
    });
});
