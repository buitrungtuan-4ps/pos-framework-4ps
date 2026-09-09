// A store nobody has installed yet must not open the hub in alarms (Wave 3 · Stage 2).
//
// Provision a store in the console and open its hub: **Not reporting** in red, **Behind** in red.
// Both statements are true of the data and both are wrong as advice — no machine exists in that shop
// yet, so there is nothing to report and nothing to be behind. Every new store began life looking
// broken, which teaches an operator that red on this screen means nothing.
//
// The fix is a judgement about meaning, not a type: `online: false` and `config_current: false`
// compile identically whether the store is a till running a stale menu or a row somebody created two
// minutes ago. So the rules that tell those apart are pure functions, and this is what pins them —
// the compiler cannot, and the build cannot, because both outcomes render perfectly.

import { describe, expect, it } from "vitest";

import { configVerdict, neverInstalled, onlineVerdict, type StoreFacts } from "../src/lib/posture";

/** A store that has never once checked in: created in the console, not yet installed. */
const PROVISIONED: StoreFacts = {
  online: false,
  last_seen_at_ms: null,
  config_current: false,
  config_version_held: null,
  config_version_published: null,
};

/** A store that has reported at least once. */
function reporting(overrides: Partial<StoreFacts> = {}): StoreFacts {
  return {
    online: true,
    last_seen_at_ms: 1_757_000_000_000,
    config_current: true,
    config_version_held: "7",
    config_version_published: "7",
    ...overrides,
  };
}

describe("neverInstalled", () => {
  it("reads the one field that can tell a new store from a silent one", () => {
    expect(neverInstalled(PROVISIONED)).toBe(true);
    expect(neverInstalled(reporting())).toBe(false);
    // A store that reported once and went quiet has still been installed. The cloud never clears
    // `last_seen_at_ms`, which is what makes it the field that answers this.
    expect(neverInstalled(reporting({ online: false }))).toBe(false);
  });
});

describe("onlineVerdict", () => {
  it("does not alarm on a store that has never been installed", () => {
    const verdict = onlineVerdict(PROVISIONED);
    expect(verdict.tone).toBe("idle");
    expect(verdict.headline).toBe("hub.online.notInstalled");
  });

  it("does alarm on a store that reported and then went silent", () => {
    const verdict = onlineVerdict(reporting({ online: false }));
    expect(verdict.tone).toBe("attention");
    expect(verdict.headline).toBe("hub.online.no");
  });

  it("reads a reporting store as online", () => {
    expect(onlineVerdict(reporting())).toEqual({ headline: "hub.online.yes", tone: "ok" });
  });
});

describe("configVerdict", () => {
  it("does not alarm when nothing has ever been published", () => {
    // The server reports `config_current: false` when either side is missing — one flag for three
    // different gaps. Nobody has published is the console's own gap, not the store's fault.
    const verdict = configVerdict(PROVISIONED);
    expect(verdict.tone).toBe("idle");
    expect(verdict.headline).toBe("hub.config.notPublished");
  });

  it("does not alarm when a published version has not reached a store that is not installed", () => {
    const verdict = configVerdict({
      ...PROVISIONED,
      config_version_published: "3",
    });
    expect(verdict.tone).toBe("idle");
    expect(verdict.headline).toBe("hub.config.notDelivered");
  });

  it("does alarm when a reporting store is holding an older version", () => {
    const verdict = configVerdict(
      reporting({ config_current: false, config_version_held: "6", config_version_published: "7" }),
    );
    expect(verdict.tone).toBe("attention");
    expect(verdict.headline).toBe("hub.config.behind");
  });

  it("does alarm when a reporting store holds nothing and something is published", () => {
    const verdict = configVerdict(
      reporting({ config_current: false, config_version_held: null, config_version_published: "7" }),
    );
    expect(verdict.tone).toBe("attention");
    expect(verdict.headline).toBe("hub.config.behind");
  });

  it("reads a matching store as up to date", () => {
    expect(configVerdict(reporting())).toEqual({ headline: "hub.config.current", tone: "ok" });
  });
});
