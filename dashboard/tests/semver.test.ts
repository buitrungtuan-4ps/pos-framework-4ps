// Which store honours a setting is a version comparison, and the wrong one hides a working switch
// (ADR-0160 decision 5, "honour or hide").
//
// Three ways it goes wrong quietly, each pinned below:
//
//   * **as strings.** `"0.9.0" > "0.14.1"` in a string comparison, so a store two minor releases
//     behind would be shown a setting it ignores, and one ahead hidden from one it honours;
//   * **unknown read as older.** A store that has not reported a release — a new store, the one an
//     operator is setting up — would lose every setting. Unknown is its own answer, and the screen
//     shows the setting with a note;
//   * **equal read as older.** The release a setting is honoured *from* honours it.
//
// And one way it says something untrue: a setting the cloud applies, honoured from `0.0.0`, noted as
// one a store that has not reported may not honour yet.

import { describe, expect, it } from "vitest";

import {
  compareReleases,
  honouredByEveryRelease,
  parseRelease,
  releaseStanding,
} from "../src/lib/semver";

describe("reading a release", () => {
  it("takes MAJOR.MINOR.PATCH as three numbers", () => {
    expect(parseRelease("0.14.1")).toEqual({ major: 0, minor: 14, patch: 1 });
    expect(parseRelease(" 1.2.30 ")).toEqual({ major: 1, minor: 2, patch: 30 });
  });

  it("takes the tag's leading v, which names the same release", () => {
    expect(parseRelease("v0.14.1")).toEqual({ major: 0, minor: 14, patch: 1 });
  });

  it("reads nothing else as a release", () => {
    for (const text of [null, undefined, "", "dev", "0.14", "0.14.1.2", "0.14.x", "0.14.1-rc.1", "V 0.14.1"]) {
      expect(parseRelease(text)).toBeNull();
    }
  });
});

describe("comparing two releases", () => {
  it("knows an older one", () => {
    expect(compareReleases("0.14.0", "0.14.1")).toBe(-1);
    expect(compareReleases("0.13.9", "0.14.1")).toBe(-1);
    // The case a string comparison gets backwards.
    expect(compareReleases("0.9.0", "0.14.1")).toBe(-1);
  });

  it("knows a newer one", () => {
    expect(compareReleases("0.14.2", "0.14.1")).toBe(1);
    expect(compareReleases("1.0.0", "0.14.1")).toBe(1);
    expect(compareReleases("0.14.10", "0.14.9")).toBe(1);
  });

  it("knows the same one, with or without its v", () => {
    expect(compareReleases("0.14.1", "0.14.1")).toBe(0);
    expect(compareReleases("v0.14.1", "0.14.1")).toBe(0);
  });

  it("answers nothing when either side cannot be read", () => {
    expect(compareReleases(null, "0.14.1")).toBeNull();
    expect(compareReleases("0.14.1", "soon")).toBeNull();
  });
});

describe("whether a store honours a setting", () => {
  it("honours it from the release that names it, and after", () => {
    expect(releaseStanding("0.14.1", "0.14.1")).toBe("honours");
    expect(releaseStanding("0.15.0", "0.14.1")).toBe("honours");
  });

  it("does not on an older release", () => {
    expect(releaseStanding("0.14.0", "0.14.1")).toBe("older");
    // A hand-built edge reports 0.0.0, which is honestly not a release that honours anything.
    expect(releaseStanding("0.0.0", "0.14.1")).toBe("older");
  });

  it("is unknown, never older, for a store that has not said", () => {
    expect(releaseStanding(null, "0.14.1")).toBe("unknown");
    expect(releaseStanding(undefined, "0.14.1")).toBe("unknown");
    expect(releaseStanding("a build nobody tagged", "0.14.1")).toBe("unknown");
  });

  it("honours a setting every release honours, whatever the store reports", () => {
    expect(honouredByEveryRelease("0.0.0")).toBe(true);
    expect(honouredByEveryRelease("v0.0.0")).toBe(true);
    expect(honouredByEveryRelease("0.14.1")).toBe(false);
    const reported = ["0.0.0", "0.14.0", "1.0.0", null, undefined, "a build nobody tagged"];
    for (const installed of reported) {
      expect(releaseStanding(installed, "0.0.0")).toBe("honours");
    }
  });
});
