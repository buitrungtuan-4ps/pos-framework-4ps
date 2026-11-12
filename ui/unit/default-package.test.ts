// The default package (ADR-0190): every till screen is drawn from it, within the renderer's limits,
// from components the till implements. `pos-core` holds the same file to the checks a published
// package passes; this holds it to what the till itself draws.

import { describe, expect, it } from "vitest";

import type { ScreenNode } from "../../packages/package-renderer/src/components";
import DEFAULT_PACKAGE from "../src/package/default-package.json";
import { IMPLEMENTATIONS } from "../src/package/registry";
import { withinLimits } from "../../packages/package-renderer/src/render";
import { defaultScreen } from "../src/package/screen";
import { SCREENS, type ScreenId } from "../src/state/screens";

/**
 * The package screen each route draws; the order screen serves a table's order and a counter's.
 * `/screen/:id` draws a screen the running version adds, which the default package never holds.
 */
const DRAWN_FROM: Record<ScreenId, string | undefined> = {
    floor: "floor",
    tableOrder: "order",
    counterOrder: "order",
    tablePay: "pay",
    kds: "kds",
    expo: "expo",
    counter: "counter",
    guests: "guests",
    today: "today",
    records: "records",
    shift: "shift",
    pair: "pair",
    devices: "devices",
    setup: "setup",
    signIn: "sign_in",
    versionScreen: undefined,
};

function components(node: ScreenNode): string[] {
    return [node.component_id, ...(node.children ?? []).flatMap(components)];
}

describe("the default package", () => {
    it("holds a screen for every route of the till", () => {
        for (const id of Object.keys(SCREENS) as ScreenId[]) {
            const drawn = DRAWN_FROM[id];
            if (drawn !== undefined) {
                expect(defaultScreen(drawn), id).toBeDefined();
            }
        }
    });

    it("draws each screen within the limits, from components the till implements", () => {
        for (const variant of DEFAULT_PACKAGE.screens) {
            const root: ScreenNode = variant.body.root;
            expect(withinLimits(root), variant.body.screen_id).toBe(true);
            for (const id of components(root)) {
                expect(Object.hasOwn(IMPLEMENTATIONS, id), `${variant.body.screen_id}: ${id}`).toBe(true);
            }
        }
    });
});
