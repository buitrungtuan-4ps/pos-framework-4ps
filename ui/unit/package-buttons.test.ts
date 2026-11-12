// A package's button on a till screen runs what a reaction may run (`state/reactions.ts`): opening
// a screen and the prints, for the order or table its screen is for, when the signed-in person
// holds the act outright. An act that moves money, or one the person lacks, draws disabled.

import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "solid-js/web";

import { ApiError, api } from "../src/api/client";
import { IMPLEMENTATIONS } from "../src/package/registry";
import { PackageTree, type ScreenHost } from "../src/package/render";
import { NO_ACTS, orderHost } from "../src/package/screen";
import { adoptSession } from "../src/state/permissions";
import { setScreenOpener } from "../src/state/reactions";

function button(actionId: string, params: Record<string, string> = {}) {
    return { component_id: "action_button", properties: { label_key: "floor.title", action: { action_id: actionId, params } } };
}

function draw(host: ScreenHost, ...buttons: object[]): HTMLElement {
    const target = document.createElement("div");
    document.body.append(target);
    render(() => PackageTree({ root: { component_id: "stack", children: buttons as never }, host, implementations: IMPLEMENTATIONS, fallback: null }), target);
    return target;
}

function tapped(target: HTMLElement, actionId: string): HTMLButtonElement {
    const found = target.querySelector<HTMLButtonElement>(`[data-action-id="${actionId}"]`);
    if (found === null) {
        throw new Error(`no button for ${actionId}`);
    }
    return found;
}

afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = "";
});

describe("a package button on a till screen", () => {
    it("is offered the acts a reaction runs, only those the person holds outright, and never money", () => {
        adoptSession({ permissions_enforced: true, permissions: ["billing.bill.open", "billing.payment.take"] } as never);
        expect(NO_ACTS.offers("ui.screen.open")).toBe(true);
        expect(NO_ACTS.offers("billing.check.print")).toBe(true);
        expect(NO_ACTS.offers("reports.shift.print_x")).toBe(false);
        expect(NO_ACTS.offers("billing.bill.settle")).toBe(false);
        expect(NO_ACTS.offers("sales.order.fire")).toBe(false);
    });

    it("prints the check of the table its screen is for, one tap at a time", async () => {
        adoptSession({ permissions_enforced: false } as never);
        let answer: (value: unknown) => void = () => undefined;
        const print = vi.spyOn(api, "printTableCheck").mockReturnValue(new Promise((resolve) => (answer = resolve)) as never);
        const target = draw(orderHost(() => false, { id: "T7" }), button("billing.check.print"));
        const check = tapped(target, "billing.check.print");
        check.click();
        expect(check.disabled).toBe(true);
        expect(check.getAttribute("aria-busy")).toBe("true");
        check.click();
        await vi.waitFor(() => expect(print).toHaveBeenCalledWith("T7"));
        answer({});
        await vi.waitFor(() => expect(check.disabled).toBe(false));
        expect(print).toHaveBeenCalledTimes(1);
        expect(target.querySelector('[role="alert"]')).toBeNull();
    });

    it("says what a refused print was, under the button", async () => {
        adoptSession({ permissions_enforced: false } as never);
        vi.spyOn(api, "printOrderCheck").mockRejectedValue(new ApiError(409, "No printer is set for checks.", "PRINTER_NOT_SET"));
        const target = draw(orderHost(() => true, { id: "O9" }), button("billing.check.print"));
        tapped(target, "billing.check.print").click();
        await vi.waitFor(() => expect(target.querySelector('[role="alert"]')).not.toBeNull());
    });

    it("opens a screen for its table, and draws a money act disabled", () => {
        adoptSession({ permissions_enforced: false } as never);
        const opened: [string, string | undefined][] = [];
        setScreenOpener((screenId, fired) => opened.push([screenId, fired.tableId]) > 0);
        const target = draw(orderHost(() => false, { id: "T7" }), button("ui.screen.open", { screen_id: "pay" }), button("billing.bill.settle"));
        tapped(target, "ui.screen.open").click();
        expect(opened).toEqual([["pay", "T7"]]);
        expect(tapped(target, "billing.bill.settle").disabled).toBe(true);
    });
});
