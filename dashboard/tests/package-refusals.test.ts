// A refusal the cloud names by its reason reads in the console's words (ADR-0190 decision 8).

import { describe, expect, it } from "vitest";

import en from "../src/i18n/en.json";
import { blankDraft } from "../src/lib/package-draft";
import { refusalText } from "../src/screens/packages/designer/refusals";

describe("a package refusal in words", () => {
  it("names the key that has no English text (ADR-0190 decision 6)", () => {
    const text = refusalText(
      {
        field: "time_slots.label_key",
        reason: "UNKNOWN_TRANSLATION_KEY",
        message: "time slot 0: the key `package.slot.dinner` has no English text in the tenant's translations",
        name: "package.slot.dinner",
      },
      blankDraft("May"),
    );
    expect(text).toBe(en["designer.refusal.unknownTranslationKey"].replace("{name}", "package.slot.dinner"));
  });
});
