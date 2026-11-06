// The till's renderer (ADR-0190 decision 3, ADR-0203): a package tree mounts the catalogue's
// components, an unknown one draws the "needs an update" slot without failing the screen, and a tree
// over the limits is left to the host screen's own.

import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "solid-js/web";

import { COMPONENTS, DEPTH_MAX, NODES_MAX, type ActionBinding, type ScreenNode } from "../src/package/components";
import { IMPLEMENTATIONS } from "../src/package/registry";
import { PackageTree, withinLimits, type ScreenHost, type Supplied } from "../src/package/render";

let dispose: (() => void) | undefined;

afterEach(() => {
    dispose?.();
    dispose = undefined;
    document.body.innerHTML = "";
});

function draw(root: ScreenNode, host: ScreenHost): HTMLElement {
    const target = document.createElement("div");
    document.body.append(target);
    dispose = render(
        () =>
            PackageTree({
                root,
                host,
                implementations: IMPLEMENTATIONS,
                fallback: "own screen",
            }),
        target,
    );
    return target;
}

function host(offered: readonly string[], run: ScreenHost["run"] = () => undefined): ScreenHost {
    return {
        subject: () => ({ order_id: "01J0000000000000000000000O" }),
        offers: (actionId) => offered.includes(actionId),
        run,
    };
}

const SEND: ScreenNode = {
    component_id: "action_button",
    properties: { label_key: "order.send", variant: "primary", action: { action_id: "sales.order.fire" } },
};

describe("the package renderer", () => {
    it("has an implementation for every catalogued component", () => {
        for (const component of COMPONENTS) {
            expect(IMPLEMENTATIONS[component.component_id], component.component_id).toBeTypeOf("function");
        }
    });

    it("draws a tree of catalogued components from its literals", () => {
        const target = draw(
            {
                component_id: "stack",
                children: [{ component_id: "text", properties: { text_key: "order.send", variant: "title" } }, SEND],
            },
            host(["sales.order.fire"]),
        );
        expect(target.querySelector("p")?.textContent).not.toBe("");
        const button = target.querySelector<HTMLButtonElement>("[data-action-id='sales.order.fire']");
        expect(button?.disabled).toBe(false);
        expect(button?.textContent).toBe(target.querySelector("p")?.textContent);
    });

    it("runs a button's act with the package's parameters and the screen's subject", () => {
        const run = vi.fn<(binding: ActionBinding, supplied: Supplied) => void>();
        const target = draw(SEND, host(["sales.order.fire"], run));
        target.querySelector<HTMLButtonElement>("button")?.click();
        expect(run).toHaveBeenCalledWith({ action_id: "sales.order.fire" }, { order_id: "01J0000000000000000000000O" });
    });

    it("disables a button for an act the screen cannot run", () => {
        const target = draw(SEND, host([]));
        expect(target.querySelector<HTMLButtonElement>("button")?.disabled).toBe(true);
    });

    it("draws a slot for a component from a newer release, and the rest of the screen", () => {
        const target = draw({ component_id: "stack", children: [{ component_id: "hologram" }, SEND] }, host([]));
        expect(target.querySelectorAll("[data-package-fallback]")).toHaveLength(1);
        expect(target.querySelector("button")).not.toBeNull();
    });

    it("leaves a tree over the limits to the screen's own", () => {
        const leaf: ScreenNode = { component_id: "text", properties: { text_key: "order.send" } };
        const wide: ScreenNode = { component_id: "stack", children: Array.from({ length: NODES_MAX }, () => leaf) };
        let deep: ScreenNode = leaf;
        for (let level = 1; level <= DEPTH_MAX; level += 1) deep = { component_id: "stack", children: [deep] };
        expect(withinLimits({ component_id: "stack", children: [leaf] })).toBe(true);
        expect(withinLimits(wide)).toBe(false);
        expect(withinLimits(deep)).toBe(false);
        expect(draw(wide, host([])).textContent).toBe("own screen");
    });
});
