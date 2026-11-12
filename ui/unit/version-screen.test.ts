// A screen the running version adds gets a route of its own (ADR-0190 decision 1): a package
// button or a reaction opening it lands on `/screen/:id`, where the version's tree is drawn. A
// screen of the release's own still opens at its own route, and an id neither declares opens
// nothing.

import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, createMemoryHistory } from "@solidjs/router";
import { render } from "solid-js/web";

import { ScreenReactions, screenRoute } from "../src/components/ScreenReactions";
import { VersionScreen } from "../src/screens/VersionScreen";
import { applyScreens } from "../src/state/package-screens";
import { openPackageScreen } from "../src/state/reactions";

const WELCOME = {
    screen_id: "welcome",
    root: {
        component_id: "screen_frame",
        children: [
            { component_id: "text", properties: { text_key: "floor.title" } },
            { component_id: "action_button", properties: { label_key: "floor.title", action: { action_id: "ui.screen.open", params: { screen_id: "kds" } } } },
        ],
    },
};

afterEach(() => {
    applyScreens({ running: null });
    document.body.innerHTML = "";
});

function mount(at: string) {
    const history = createMemoryHistory();
    history.set({ value: at });
    const target = document.createElement("div");
    document.body.append(target);
    const dispose = render(
        () =>
            MemoryRouter({
                history,
                root: (props) => [ScreenReactions(), props.children],
                children: [
                    { path: "/", component: () => null },
                    { path: "/kds", component: () => null },
                    { path: "/screen/:id", component: VersionScreen },
                ],
            }),
        target,
    );
    return { history, target, dispose };
}

describe("a screen the running version adds", () => {
    it("is routed at /screen/:id, and the release's own screens at theirs", () => {
        expect(screenRoute("welcome")).toBeUndefined();
        applyScreens({ running: { screens: [{ body: WELCOME }, { body: { ...WELCOME, screen_id: "kds" } }] } });
        expect(screenRoute("welcome")).toBe("/screen/welcome");
        expect(screenRoute("kds")).toBe("/kds");
        expect(screenRoute("nowhere")).toBeUndefined();
    });

    it("opens from a package button, drawn from the version, and its own button opens the next", async () => {
        applyScreens({ running: { screens: [{ body: WELCOME }] } });
        const { history, target, dispose } = mount("/");
        expect(openPackageScreen("nowhere")).toBe(false);
        expect(openPackageScreen("welcome")).toBe(true);
        await vi.waitFor(() => expect(target.querySelector('[data-action-id="ui.screen.open"]')).not.toBeNull());
        expect(history.get()).toBe("/screen/welcome");
        const button = target.querySelector<HTMLButtonElement>('[data-action-id="ui.screen.open"]');
        expect(button?.disabled).toBe(false);
        button?.click();
        await vi.waitFor(() => expect(history.get()).toBe("/kds"));
        dispose();
    });

    it("is not drawn for the release's own screen, nor once the version no longer declares it", () => {
        applyScreens({ running: { screens: [{ body: WELCOME }] } });
        const own = mount("/screen/kds");
        expect(own.target.querySelector("[data-package-fallback]")).not.toBeNull();
        own.dispose();
        document.body.innerHTML = "";
        const { target, dispose } = mount("/screen/welcome");
        expect(target.querySelector("section")).not.toBeNull();
        applyScreens({ running: null });
        expect(target.querySelector("section")).toBeNull();
        expect(target.querySelector("[data-package-fallback]")).not.toBeNull();
        dispose();
    });
});
