// `lib/after-paint.ts` (ADR-0208): work that can wait for the screen starts after the tap's frame,
// and never waits forever for a frame a hidden page will not produce.

import { afterEach, describe, expect, it, vi } from "vitest";

import { afterPaint } from "../src/lib/after-paint";

afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
});

describe("after the paint", () => {
    it("waits for the next frame and the task after it", async () => {
        let frame: FrameRequestCallback | undefined;
        vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
            frame = callback;
            return 1;
        });
        let done = false;
        const waiting = afterPaint().then(() => {
            done = true;
        });
        await Promise.resolve();
        expect(done).toBe(false);
        frame?.(0);
        await waiting;
        expect(done).toBe(true);
    });

    it("goes ahead without a frame on a page that draws none", async () => {
        vi.useFakeTimers();
        vi.stubGlobal("requestAnimationFrame", () => 1);
        let done = false;
        const waiting = afterPaint().then(() => {
            done = true;
        });
        await vi.advanceTimersByTimeAsync(49);
        expect(done).toBe(false);
        await vi.advanceTimersByTimeAsync(1);
        await waiting;
        expect(done).toBe(true);
    });
});
