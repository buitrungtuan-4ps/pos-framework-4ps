// A package screen's lookup (ADR-0214): its fields send what a person typed, and the screen's own
// id where a field names it; its button runs `integrations.lookup.run` through the edge, painting
// pending first; its results draw the answer, which goes with the screen; a refusal is said under
// the button in the till's words; and only a person who holds the act may run it.

import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "solid-js/web";

import { ApiError, api } from "../src/api/client";
import { IMPLEMENTATIONS } from "../src/package/registry";
import { PackageTree } from "../../packages/package-renderer/src/render";
import { NO_ACTS, orderHost } from "../src/package/screen";
import { TILL_WORDS } from "../src/package/words";
import { adoptSession } from "../src/state/permissions";
import { forgetLookup, lookupAnswer } from "../src/state/lookup";
import { applyPackage, passedLookupSteps } from "../src/state/flows";
import { setScreenOpener } from "../src/state/reactions";

const ENDPOINT = "01J0000000000000000000GW01";

const SCREEN = {
    component_id: "stack",
    children: [
        { component_id: "lookup_field", properties: { field: "card_code", label_key: "floor.title" } },
        { component_id: "lookup_field", properties: { field: "visits", label_key: "floor.title", kind: "number" } },
        { component_id: "lookup_field", properties: { field: "table_id", label_key: "floor.title" } },
        {
            component_id: "action_button",
            properties: { label_key: "floor.title", action: { action_id: "integrations.lookup.run", params: { gateway_endpoint_id: ENDPOINT } } },
        },
        { component_id: "lookup_result", properties: { field: "points", label_key: "floor.title" } },
    ],
};

function draw(): { target: HTMLElement; dispose: () => void } {
    const target = document.createElement("div");
    document.body.append(target);
    const dispose = render(
        () => PackageTree({ root: SCREEN as never, host: orderHost(() => false, { id: "T7" }), implementations: IMPLEMENTATIONS, words: TILL_WORDS, fallback: null }),
        target,
    );
    return { target, dispose };
}

function type(target: HTMLElement, field: string, text: string): void {
    const input = target.querySelector<HTMLInputElement>(`[data-lookup-field="${field}"]`);
    if (input === null) {
        throw new Error(`no field ${field}`);
    }
    input.value = text;
    input.dispatchEvent(new Event("input", { bubbles: true }));
}

function lookupButton(target: HTMLElement): HTMLButtonElement {
    const found = target.querySelector<HTMLButtonElement>('[data-action-id="integrations.lookup.run"]');
    if (found === null) {
        throw new Error("no lookup button");
    }
    return found;
}

afterEach(() => {
    vi.restoreAllMocks();
    forgetLookup();
    document.body.innerHTML = "";
});

describe("a package screen's lookup", () => {
    it("is offered only to a person who holds integrations.lookup.run outright", () => {
        adoptSession({ permissions_enforced: true, permissions: ["sales.line.add"] } as never);
        expect(NO_ACTS.offers("integrations.lookup.run")).toBe(false);
        adoptSession({ permissions_enforced: true, permissions: ["integrations.lookup.run"] } as never);
        expect(NO_ACTS.offers("integrations.lookup.run")).toBe(true);
    });

    it("sends what was typed and the table, paints pending, draws the answer and forgets it with the screen", async () => {
        adoptSession({ permissions_enforced: false } as never);
        let answer: (value: unknown) => void = () => undefined;
        const call = vi.spyOn(api, "lookup").mockReturnValue(new Promise((resolve) => (answer = resolve)) as never);
        const { target, dispose } = draw();
        expect(target.querySelector('[data-lookup-field="table_id"]')).toBeNull();
        type(target, "card_code", " 00123 ");
        type(target, "visits", "4");
        lookupButton(target).click();
        expect(lookupAnswer().state).toBe("pending");
        expect(target.querySelector('[data-lookup-result="points"]')).not.toBeNull();
        await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(1));
        const sent = call.mock.calls[0]?.[0];
        expect(sent?.gateway_endpoint_id).toBe(ENDPOINT);
        expect(sent?.values).toEqual({ card_code: "00123", visits: 4, table_id: "T7" });
        expect(sent?.idempotency_key).toMatch(/^[0-9a-f]{32}$/);
        answer({ call_id: "c", idempotency_key: sent?.idempotency_key, result: { points: 120 } });
        await vi.waitFor(() => expect(target.querySelector('[data-lookup-result="points"]')?.textContent).toContain("120"));
        dispose();
        expect(lookupAnswer().state).toBe("idle");
    });

    it("says a refusal under the button, and repeats its key only after a timeout", async () => {
        adoptSession({ permissions_enforced: false } as never);
        const call = vi
            .spyOn(api, "lookup")
            .mockRejectedValueOnce(new ApiError(504, "no answer", "GATEWAY_TIMEOUT"))
            .mockRejectedValueOnce(new ApiError(503, "offline", "GATEWAY_OFFLINE"))
            .mockRejectedValue(new ApiError(503, "offline", "GATEWAY_OFFLINE"));
        const { target } = draw();
        type(target, "card_code", "7");
        const button = lookupButton(target);
        button.click();
        await vi.waitFor(() => expect(target.querySelector('[role="alert"]')?.textContent).toBe(TILL_WORDS.failure(new ApiError(504, "", "GATEWAY_TIMEOUT"))));
        expect(lookupAnswer().state).toBe("refused");
        await vi.waitFor(() => expect(button.disabled).toBe(false));
        button.click();
        await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(2));
        await vi.waitFor(() => expect(button.disabled).toBe(false));
        button.click();
        await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(3));
        const keys = call.mock.calls.map((args) => args[0].idempotency_key);
        expect(keys[1]).toBe(keys[0]);
        expect(keys[2]).not.toBe(keys[1]);
        expect(target.querySelector('[role="alert"]')?.textContent).not.toContain("offline");
    });

    it("honours a flow step's offline branch: skip with its note, or open the screen of the step it goes to", async () => {
        adoptSession({ permissions_enforced: false } as never);
        const step = (step_id: string, offline: object) => ({ step_id, action_id: "integrations.lookup.run", kind: "STEP_KIND_REQUIRED", offline });
        applyPackage({
            running: {
                flows: [
                    { body: { flow_id: "points", start_action_id: "sales.table.seat", steps: [step("look", { skip_note_key: "floor.title" })] } },
                    {
                        body: {
                            flow_id: "card",
                            start_action_id: "sales.table.seat",
                            steps: [step("ask", { step_id: "by_hand" }), { step_id: "by_hand", screen_id: "card_by_hand", kind: "STEP_KIND_OPTIONAL" }],
                        },
                    },
                ],
            },
        });
        const opened: string[] = [];
        setScreenOpener((screenId) => opened.push(screenId) > 0);
        vi.spyOn(api, "lookup").mockRejectedValue(new ApiError(503, "offline", "GATEWAY_OFFLINE"));
        const { target } = draw();
        lookupButton(target).click();
        await vi.waitFor(() => expect(target.querySelector('[role="alert"]')?.textContent).toBe(TILL_WORDS.label("floor.title")));
        expect(passedLookupSteps("T7")).toEqual(["points/look", "card/ask"]);
        expect(opened).toEqual(["card_by_hand"]);
        applyPackage({ running: null });
    });

    it("counts a lookup step done once answered, and passes nothing for a refusal that is not offline", async () => {
        adoptSession({ permissions_enforced: false } as never);
        applyPackage({
            running: {
                flows: [
                    {
                        body: {
                            flow_id: "tier",
                            start_action_id: "sales.table.seat",
                            steps: [{ step_id: "look", action_id: "integrations.lookup.run", kind: "STEP_KIND_REQUIRED", offline: { skip_note_key: "floor.title" } }],
                        },
                    },
                ],
            },
        });
        const call = vi.spyOn(api, "lookup").mockRejectedValueOnce(new ApiError(502, "no", "GATEWAY_REFUSED"));
        const { target } = draw();
        lookupButton(target).click();
        await vi.waitFor(() => expect(lookupAnswer().state).toBe("refused"));
        expect(passedLookupSteps("T7").includes("tier/look")).toBe(false);
        call.mockResolvedValueOnce({ call_id: "c", idempotency_key: "k", result: {} } as never);
        await vi.waitFor(() => expect(lookupButton(target).disabled).toBe(false));
        lookupButton(target).click();
        await vi.waitFor(() => expect(passedLookupSteps("T7")).toContain("tier/look"));
        applyPackage({ running: null });
    });
});
