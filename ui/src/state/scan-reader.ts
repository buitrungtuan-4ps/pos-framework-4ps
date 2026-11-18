// Telling a keyboard-wedge scanner from typing (ADR-0222 decision 2). A scanner types a code's
// digits and Enter faster than a person can: digits that each follow the last within the store's
// window, ending in Enter, are a scan. Anything slower, or with a letter in it, is typing and is
// left alone. Pure, so it is tested without a keyboard; `../components/ScanListener` feeds it.

/** The fewest digits a scan carries: a PLU is one to five, a GTIN eight to fourteen. */
const SHORTEST = 1;
const LONGEST = 14;

export interface ScanReader {
  /** Feeds one keystroke at `time` ms; answers the code when it completes a scan. */
  readonly key: (key: string, time: number) => string | null;
}

export function createScanReader(windowMs: () => number): ScanReader {
  // The digits of the run under way, or null once a key that is not a digit spoiled it: the run
  // is typing until a slow key starts another.
  let digits: string | null = "";
  let last = Number.NEGATIVE_INFINITY;
  return {
    key: (key, time) => {
      const quick = time - last <= windowMs();
      last = time;
      const run = quick ? digits : "";
      if (/^\d$/.test(key)) {
        digits = run === null ? null : run + key;
        return null;
      }
      digits = key === "Enter" ? "" : null;
      return key === "Enter" && run !== null && run.length >= SHORTEST && run.length <= LONGEST ? run : null;
    },
  };
}
