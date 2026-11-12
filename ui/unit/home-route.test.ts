// A sign-in lands on the screen the running version names as this device's home (ADR-0210
// decision 1), and on `/` as before when it names none, names one the till cannot open, or names one
// that needs a table.

import { afterEach, describe, expect, it } from "vitest";

import { homeRoute } from "../src/components/ScreenReactions";
import { publishDeviceClass } from "../src/lib/device-class";
import { applyScreens } from "../src/state/package-screens";

const WELCOME = { screen_id: "welcome", root: { component_id: "screen_frame" } };

function running(homes: object[]) {
    applyScreens({ running: { screens: [{ body: WELCOME }], home_screens: homes } });
}

afterEach(() => {
    publishDeviceClass(undefined);
    applyScreens({ running: null });
});

describe("the home a sign-in lands on", () => {
    it("is `/` for a version that states no homes, and for the default package", () => {
        expect(homeRoute()).toBe("/");
        running([]);
        expect(homeRoute()).toBe("/");
    });

    it("is the version's home for this device's class, else its base", () => {
        running([{ body: { screen_id: "welcome" } }, { selector: { device_class: "DEVICE_CLASS_PHONE" }, body: { screen_id: "today" } }]);
        expect(homeRoute()).toBe("/screen/welcome");
        publishDeviceClass("DEVICE_CLASS_PHONE");
        expect(homeRoute()).toBe("/today");
    });

    it("is `/` for a home that needs a table, or one the till cannot open", () => {
        running([{ body: { screen_id: "order" } }]);
        expect(homeRoute()).toBe("/");
        running([{ body: { screen_id: "nowhere" } }]);
        expect(homeRoute()).toBe("/");
    });
});
