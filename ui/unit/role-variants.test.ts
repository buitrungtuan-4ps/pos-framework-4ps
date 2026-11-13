// The till chooses a package screen's role variant, and a role's home, by the roles the signed-in
// person holds, as `GET /api/session` answers them (ADR-0213 decisions 2 and 3). When several of
// their roles match, the variant listed first in the version wins; with none, the base.

import { afterEach, describe, expect, it } from "vitest";
import { render } from "solid-js/web";

import { homeRoute } from "../src/components/ScreenReactions";
import { NO_ACTS, PackageScreen } from "../src/package/screen";
import { applyScreens } from "../src/state/package-screens";
import { lockTill, roleIds, takeSession } from "../src/state/session";

const CASHIER = "01J00000000000000000000R01";
const MANAGER = "01J00000000000000000000R02";

const SESSION = { signed_in: true, employee_id: "01J00000000000000000000E01", lease_standing: "active", permissions: [], permissions_with_approval: [] };

function signIn(roles?: string[]) {
    takeSession({ ...SESSION, ...(roles === undefined ? {} : { role_ids: roles }) } as never);
}

const WELCOME = { screen_id: "welcome", root: { component_id: "screen_frame" } };

afterEach(() => {
    lockTill();
    applyScreens({ running: null });
    document.body.innerHTML = "";
});

describe("a person's roles", () => {
    it("are what the session answers, none from an edge too old to send them, and none once locked", () => {
        signIn([CASHIER, MANAGER]);
        expect(roleIds()).toEqual([CASHIER, MANAGER]);
        signIn();
        expect(roleIds()).toEqual([]);
        signIn([MANAGER]);
        lockTill();
        expect(roleIds()).toEqual([]);
    });

    it("choose the home a sign-in lands on, the one listed first when two of their roles have one", () => {
        applyScreens({
            running: {
                screens: [{ body: WELCOME }],
                home_screens: [
                    { body: { screen_id: "welcome" } },
                    { selector: { role_id: MANAGER }, body: { screen_id: "today" } },
                    { selector: { role_id: CASHIER }, body: { screen_id: "records" } },
                ],
            },
        });
        expect(homeRoute()).toBe("/screen/welcome");
        signIn([CASHIER]);
        expect(homeRoute()).toBe("/records");
        // Sorted ids put the cashier first; the version lists the manager's home first, and wins.
        signIn([CASHIER, MANAGER]);
        expect(homeRoute()).toBe("/today");
        signIn(["01J00000000000000000000R09"]);
        expect(homeRoute()).toBe("/screen/welcome");
    });

    it("choose the variant a package screen draws, and draw it again when they change", () => {
        const manager = { screen_id: "welcome", root: { component_id: "stack" } };
        applyScreens({ running: { screens: [{ body: WELCOME }, { selector: { role_id: MANAGER }, body: manager }] } });
        const target = document.createElement("div");
        document.body.append(target);
        const dispose = render(() => PackageScreen({ screenId: "welcome", host: NO_ACTS }), target);
        expect(target.firstElementChild?.tagName).toBe("SECTION");
        signIn([MANAGER]);
        expect(target.firstElementChild?.tagName).toBe("DIV");
        lockTill();
        expect(target.firstElementChild?.tagName).toBe("SECTION");
        dispose();
    });
});
