// The till draws for the device class the store publishes for it (ADR-0201 decision 3): the class
// `GET /api/session` names, where its till names one, and the screen's breakpoints otherwise. A
// package screen's variant follows it.

import { afterEach, describe, expect, it } from "vitest";
import { render } from "solid-js/web";

import { deviceClass, publishDeviceClass } from "../src/lib/device-class";
import { NO_ACTS, PackageScreen } from "../src/package/screen";
import { applyScreens } from "../src/state/package-screens";
import { takeSession } from "../src/state/session";

const SESSION = { signed_in: false, lease_standing: "active", sign_in_ready: true, permissions_enforced: false, permissions: [], permissions_with_approval: [], idle_lock_seconds: 0, opening_float_minor: 0 };

afterEach(() => {
    publishDeviceClass(undefined);
    applyScreens({ running: null });
    document.body.innerHTML = "";
});

describe("the device class", () => {
    it("is the one the session names, and the screen's where it names none", () => {
        // jsdom has no `matchMedia`: the screen reads as a terminal.
        expect(deviceClass()).toBe("DEVICE_CLASS_TERMINAL");
        takeSession({ ...SESSION, device_class: "DEVICE_CLASS_KITCHEN_DISPLAY" } as never);
        expect(deviceClass()).toBe("DEVICE_CLASS_KITCHEN_DISPLAY");
        takeSession({ ...SESSION, device_class: "DEVICE_CLASS_UNSPECIFIED" } as never);
        expect(deviceClass()).toBe("DEVICE_CLASS_TERMINAL");
        takeSession({ ...SESSION, device_class: "DEVICE_CLASS_PHONE" } as never);
        takeSession(SESSION as never);
        expect(deviceClass()).toBe("DEVICE_CLASS_TERMINAL");
    });

    it("chooses the variant a package screen draws, and draws it again when it changes", () => {
        const base = { screen_id: "welcome", root: { component_id: "screen_frame" } };
        const phone = { screen_id: "welcome", root: { component_id: "stack" } };
        applyScreens({ running: { screens: [{ body: base }, { selector: { device_class: "DEVICE_CLASS_PHONE" }, body: phone }] } });
        const target = document.createElement("div");
        document.body.append(target);
        const dispose = render(() => PackageScreen({ screenId: "welcome", host: NO_ACTS }), target);
        expect(target.firstElementChild?.tagName).toBe("SECTION");
        publishDeviceClass("DEVICE_CLASS_PHONE");
        expect(target.firstElementChild?.tagName).toBe("DIV");
        dispose();
    });
});
