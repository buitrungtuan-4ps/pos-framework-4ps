// A pinned version's screens are drawn in place of the default package's (ADR-0190 decision 3):
// the version's variant for this device's class where it declares the screen, the default
// package's otherwise, and a refetch that brings the same screens back keeps the drawn tree.

import { afterEach, describe, expect, it } from "vitest";
import { render } from "solid-js/web";

import { NO_ACTS, PackageScreen, defaultScreen, packageScreen } from "../src/package/screen";
import { applyScreens } from "../src/state/package-screens";

const FLOOR = { screen_id: "floor", root: { component_id: "screen_frame", children: [{ component_id: "text", properties: { text_key: "floor.title" } }] } };
const PHONE_FLOOR = { screen_id: "floor", root: { component_id: "stack" } };
const WELCOME = { running: { screens: [{ body: { ...FLOOR, screen_id: "welcome" } }] } };
const answer = { running: { screens: [{ body: FLOOR }, { selector: { device_class: "DEVICE_CLASS_PHONE" }, body: PHONE_FLOOR }] } };

afterEach(() => {
    applyScreens({ running: null });
    document.body.innerHTML = "";
});

describe("a pinned version's screens", () => {
    it("are drawn for the screens the version declares, by device class, and the default's otherwise", () => {
        applyScreens(answer);
        expect(packageScreen("floor", { device_class: "DEVICE_CLASS_TERMINAL" })).toEqual(FLOOR);
        expect(packageScreen("floor", { device_class: "DEVICE_CLASS_PHONE" })).toEqual(PHONE_FLOOR);
        expect(packageScreen("kds")).toBe(defaultScreen("kds"));
        applyScreens({ running: null });
        expect(packageScreen("floor")).toBe(defaultScreen("floor"));
    });

    it("replace the drawn tree when the version changes, and only then", () => {
        const target = document.createElement("div");
        document.body.append(target);
        const dispose = render(() => PackageScreen({ screenId: "welcome", host: NO_ACTS }), target);
        // The default package has no such screen: the slot that says the till needs an update.
        expect(target.querySelector("[data-package-fallback]")).not.toBeNull();
        applyScreens(WELCOME);
        const drawn = target.firstElementChild;
        expect(drawn?.tagName).toBe("SECTION");
        applyScreens(JSON.parse(JSON.stringify(WELCOME)));
        expect(target.firstElementChild).toBe(drawn);
        dispose();
    });

    it("let a package button open another screen, and no other act", () => {
        expect(NO_ACTS.offers("ui.screen.open")).toBe(true);
        expect(NO_ACTS.offers("billing.bill.settle")).toBe(false);
    });
});
