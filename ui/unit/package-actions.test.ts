// `package/actions.ts` is the action catalogue as `pos-proto` renders it (ADR-0191 decision 3); a
// `pos-proto` test holds the committed file to that rendering. These check what the till relies on.

import { describe, expect, it } from "vitest";

import { ACTIONS, type ActionId } from "../../packages/package-renderer/src/actions";

describe("the till's action catalogue", () => {
    it("lists each act once, by a three-word id", () => {
        const ids = ACTIONS.map((action) => action.action_id);
        expect(new Set(ids).size).toBe(ids.length);
        for (const id of ids) expect(id).toMatch(/^[a-z][a-z_]*\.[a-z][a-z_]*\.[a-z][a-z_]*$/);
    });

    it("types an act's id and carries what the till pre-checks", () => {
        const settle: ActionId = "billing.bill.settle";
        const entry = ACTIONS.find((action) => action.action_id === settle);
        expect(entry?.effect).toBe("ACTION_EFFECT_MONEY");
        expect(entry?.routes).toEqual(["POST /api/bills/{id}/settle"]);
        expect(entry?.refusals).toContain("PAYMENTS_DO_NOT_SUM_TO_TOTAL");
    });

    it("keeps a UI act off the edge", () => {
        const ui = ACTIONS.filter((action) => action.effect === "ACTION_EFFECT_UI");
        expect(ui.length).toBeGreaterThan(0);
        for (const action of ui) {
            expect(action.routes, action.action_id).toEqual([]);
            expect(action.permissions, action.action_id).toEqual([]);
        }
    });

    it("lets only a money act take an amount", () => {
        for (const action of ACTIONS) {
            const amount = action.params.some((param) => param.kind === "ACTION_PARAM_KIND_AMOUNT");
            if (amount) expect(action.effect, action.action_id).toBe("ACTION_EFFECT_MONEY");
        }
    });
});
