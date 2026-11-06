// `state/kept-table.ts`: the order screen kept beside the floor shows a refusal or a result only on
// the table its act was for, and starts each table as a fresh screen would.

import { createRoot, createSignal } from "solid-js";
import { describe, expect, it } from "vitest";

import { createTableScoped, resetPerTable } from "../src/state/kept-table";

describe("a value kept with its table", () => {
    it("shows an act's result on the act's table only, even when it lands after a switch", () => {
        createRoot((dispose) => {
            const [key, setKey] = createSignal("t1");
            const error = createTableScoped<string>(key);
            error.set(null);
            setKey("t2");
            error.set("refused");
            expect(error.get()).toBeNull();
            setKey("t1");
            expect(error.get()).toBe("refused");
            dispose();
        });
    });

    it("names its table where it is said", () => {
        createRoot((dispose) => {
            const [key, setKey] = createSignal("t2");
            const error = createTableScoped<string>(key);
            error.setFor("t1", "move refused");
            expect(error.get()).toBeNull();
            setKey("t1");
            expect(error.get()).toBe("move refused");
            dispose();
        });
    });
});

describe("each table starts fresh", () => {
    it("closes the panels on a switch, and drops the results when the screen hides", () => {
        const [key, setKey] = createSignal("t1");
        const [shown, setShown] = createSignal(true);
        const [panel, setPanel] = createSignal(true);
        const { error, dispose } = createRoot((dispose) => {
            const error = createTableScoped<string>(key);
            resetPerTable(shown, key, { panels: () => setPanel(false), results: [error] });
            return { error, dispose };
        });
        error.set(null);
        error.set("refused");
        setKey("t2");
        expect(panel()).toBe(false);
        setKey("t1");
        expect(error.get()).toBe("refused");
        setPanel(true);
        setShown(false);
        expect(panel()).toBe(false);
        expect(error.get()).toBeNull();
        dispose();
    });
});
