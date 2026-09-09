// The account avatar's initials (Wave 3 · Stage 6).
//
// Two letters in a circle looks like nothing to get wrong, and it is where a console built for
// Vietnam breaks first. A Vietnamese name is family-name-first with a middle name between —
// "Nguyễn Thị Hương" — so taking the first two *words* gives NT, the family name and a middle name
// that is not the person. First and last is what reads as an identity. And the names here are the
// operator's own, in scripts where indexing a string is not the same as taking a character.
//
// The fixtures are placeholders in several scripts, not real people: an admin's name is T1 under
// the data-handling rules.

import { describe, expect, it } from "vitest";

import { initials } from "../src/lib/format";

describe("initials", () => {
  it("takes the first and last word, not the first two", () => {
    // The Vietnamese case, which is the reason this is a function and not `name.slice(0, 2)`.
    expect(initials("Nguyễn Thị Hương")).toBe("NH");
    expect(initials("Trần Văn Minh Khôi")).toBe("TK");
  });

  it("takes one letter from a single word rather than inventing a second", () => {
    // Two letters from one word would show an initial the person does not have.
    expect(initials("Hương")).toBe("H");
  });

  it("survives a character outside the basic plane", () => {
    // `name[0]` on an astral character is half a surrogate pair and renders as a replacement glyph,
    // so the avatar would show a black diamond where a letter belongs. `Array.from` is what avoids
    // that, and this is the test that says so.
    const astral = "𝒜lice Zhang";
    expect(Array.from(astral)[0]).not.toBe(astral[0]);
    expect(initials(astral)).toBe(`${Array.from(astral)[0] as string}Z`);
  });

  it("handles a name with no space, in any script", () => {
    expect(initials("李")).toBe("李");
    expect(initials("さくら")).toBe("さ");
  });

  it("gives nothing for a name that is empty or only spaces", () => {
    // The caller draws a neutral glyph instead. An avatar is not the place to guess at a person.
    expect(initials("")).toBe("");
    expect(initials("   ")).toBe("");
    expect(initials("\t\n")).toBe("");
  });

  it("collapses runs of whitespace rather than reading them as words", () => {
    expect(initials("  Placeholder   Person  ")).toBe("PP");
  });
});
