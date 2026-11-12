// `package/variant.ts` (ADR-0189 decision 4) answers every shared vector exactly as `pos-core` does.

import { describe, expect, it } from "vitest";

import vectors from "../../crates/pos-proto/testdata/package_variants.json";
import {
    currentTimeSlot,
    resolveVariant,
    type TimeSlot,
    type Variant,
    type VariantContext,
} from "../../packages/package-renderer/src/variant";

interface ResolveCase {
    name: string;
    variants: Variant<string>[];
    context: VariantContext;
    expect: string;
}

interface SlotCase {
    name: string;
    time_slots: TimeSlot[];
    at: { weekday: number; minute_of_day: number };
    expect: string | null;
}

describe("resolveVariant", () => {
    for (const vector of vectors.resolve as ResolveCase[]) {
        it(vector.name, () => {
            expect(resolveVariant(vector.variants, vector.context)).toBe(
                vector.expect,
            );
        });
    }

    it("answers nothing for variants with no base outside their selectors", () => {
        const variants: Variant<string>[] = [
            {
                selector: { device_class: "DEVICE_CLASS_TABLET" },
                body: "tablet",
            },
        ];
        expect(resolveVariant(variants, {})).toBeUndefined();
    });
});

describe("currentTimeSlot", () => {
    for (const vector of vectors.current_time_slot as SlotCase[]) {
        it(vector.name, () => {
            expect(currentTimeSlot(vector.time_slots, vector.at) ?? null).toBe(
                vector.expect,
            );
        });
    }
});
