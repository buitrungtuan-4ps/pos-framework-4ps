// A subject held at stores (ADR-0206 decision 4): each store reads as holding it, waiting on an
// erasure, or masked, and an erasure is offered only while a store holds it unasked.

import { describe, expect, it } from "vitest";

import { canEraseAtStores, pendingCount, storeStanding } from "../src/lib/subject-stores";

const row = (masked: boolean, erasurePending: boolean) => ({
    store_id: "01J0000000000000000000ST01",
    masked,
    erasure_pending: erasurePending,
});

describe("subject stores", () => {
    it("reads each store's standing", () => {
        expect(storeStanding(row(false, false))).toBe("holds");
        expect(storeStanding(row(false, true))).toBe("pending");
        expect(storeStanding(row(true, false))).toBe("masked");
    });

    it("counts the stores still to confirm and offers an erasure only where one is unasked", () => {
        const sent = {
            subject_id: "S",
            stores: [row(false, true), row(true, false)],
        };
        expect(pendingCount(sent)).toBe(1);
        expect(canEraseAtStores(sent)).toBe(false);
        const held = { subject_id: "S", stores: [row(false, false)] };
        expect(canEraseAtStores(held)).toBe(true);
    });
});
