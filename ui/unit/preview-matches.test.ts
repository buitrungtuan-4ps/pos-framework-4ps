// The till's half of "the preview matches the till" (ADR-0209 decision 4): the fixture drawn by the
// till's own table, every region handed its children as a screen hands them, on the till's router, has the committed
// shape the console's preview must also have. `pnpm exec vitest run -u unit/preview-matches.test.ts`
// rewrites it.

import { afterEach, describe, expect, it } from "vitest";
import { MemoryRouter, createMemoryHistory } from "@solidjs/router";
import { render } from "solid-js/web";

import { COMPONENTS, type ComponentId, type ScreenNode } from "../../packages/package-renderer/src/components";
import { FIXTURE_HOST, FIXTURE_SCREENS, FIXTURE_WORDS, SHAPE_FILE, shapes } from "../../packages/package-renderer/fixtures/preview";
import { RegionsContext, type Regions } from "../../packages/package-renderer/src/regions";
import { Children, PackageTree } from "../../packages/package-renderer/src/render";
import { marked } from "../../packages/package-renderer/src/shape";
import DEFAULT_PACKAGE from "../src/package/default-package.json";
import { IMPLEMENTATIONS } from "../src/package/registry";

/** Each region as a screen hands it over: its contents left out, its children drawn. */
const REGIONS: Regions = Object.fromEntries(COMPONENTS.map(({ component_id }) => [component_id as ComponentId, Children]));

const disposals: (() => void)[] = [];

afterEach(() => {
    disposals.splice(0).forEach((dispose) => dispose());
    document.body.innerHTML = "";
});

function draw(root: ScreenNode): Element {
    const target = document.createElement("div");
    document.body.append(target);
    disposals.push(
        render(
            () =>
                MemoryRouter({
                    history: createMemoryHistory(),
                    children: {
                        path: "/",
                        component: () =>
                            RegionsContext.Provider({
                                value: REGIONS,
                                get children() {
                                    return PackageTree({ root, host: FIXTURE_HOST, implementations: marked(IMPLEMENTATIONS), words: FIXTURE_WORDS, fallback: null });
                                },
                            }),
                    },
                }),
            target,
        ),
    );
    return target;
}

describe("the preview matches the till", () => {
    it("draws its fixture from the default package this release ships", () => {
        expect(FIXTURE_SCREENS.slice(0, DEFAULT_PACKAGE.screens.length)).toEqual(DEFAULT_PACKAGE.screens);
    });

    it("draws the fixture in the committed shape", async () => {
        await expect(shapes(draw)).toMatchFileSnapshot(SHAPE_FILE);
    });
});
