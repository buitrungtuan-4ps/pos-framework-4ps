// The flow and reaction designers name each act in words rather than by its raw id (O9).

import { describe, expect, it } from "vitest";

import en from "../src/i18n/en.json";
import vi from "../src/i18n/vi.json";
import { CATALOGUE } from "../src/lib/action-catalogue";

describe("every act the designers offer has a name", () => {
  it.each([
    ["en", en as Record<string, string>],
    ["vi", vi as Record<string, string>],
  ])("in %s", (_lang, catalogue) => {
    const unnamed = CATALOGUE.map((action) => action.action_id).filter(
      (id) => !catalogue[`designer.action.${id.replaceAll(".", "_")}`],
    );
    expect(unnamed).toEqual([]);
  });
});
