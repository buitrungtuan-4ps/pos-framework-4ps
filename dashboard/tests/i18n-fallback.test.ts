// A message missing from a language falls back to English, a key at a time (ADR-0020,
// docs/languages.md). Both catalogues are complete today, which `cargo xtask i18n` holds, so the
// fallback is what a language added before its catalogue is finished relies on.

import { describe, expect, it } from "vitest";

import en from "../src/i18n/en.json";
import vi from "../src/i18n/vi.json";
import { type MessageKey, messageIn } from "../src/i18n";

const KEY = "nav.settings" as MessageKey;

describe("messageIn", () => {
  it("is the catalogue's own message when it has one", () => {
    expect(messageIn(vi, KEY)).toBe(vi[KEY]);
  });

  it("is the English message when the catalogue lacks the key", () => {
    expect(messageIn({}, KEY)).toBe(en[KEY]);
    expect(messageIn({ "other.key": "x" }, KEY)).toBe(en[KEY]);
  });
});
