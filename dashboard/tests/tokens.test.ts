// Open wire tokens ([ADR-0177](../../docs/adr/0177-openapi-is-the-route-map.md), `Open<E>` option
// b): the document types a channel as `string`, the console labels the ones it knows, shows the
// rest raw, and keeps them through an edit.

import { describe, expect, it } from "vitest";

import { channelLabel, isKnownChannel, SALES_CHANNELS } from "../src/lib/channels";
import { draftFromRule, newDraft, ruleFromDraft } from "../src/lib/fees";
import { isKnown, tokenLabel } from "../src/lib/tokens";

const NEWER = "SALES_CHANNEL_KIOSK";

describe("open tokens", () => {
  it("narrow a known token and leave an unknown one alone", () => {
    expect(isKnown(SALES_CHANNELS, "SALES_CHANNEL_QR")).toBe(true);
    expect(isKnown(SALES_CHANNELS, NEWER)).toBe(false);
    expect(isKnownChannel(NEWER)).toBe(false);
  });

  it("label a known token and show an unknown one as it arrived", () => {
    expect(channelLabel("SALES_CHANNEL_DINE_IN")).toBe("Dine-in");
    expect(channelLabel(NEWER)).toBe(NEWER);
    expect(tokenLabel({ SALES_CHANNEL_QR: "channel.qr" }, "SALES_CHANNEL_API")).toBe(
      "SALES_CHANNEL_API",
    );
  });

  it("keep a channel a newer cloud sent through a fee edit", () => {
    const written = ruleFromDraft({
      ...newDraft("VND"),
      code: "SERVICE",
      displayName: "Service",
      channels: ["SALES_CHANNEL_DINE_IN", NEWER],
    });
    const draft = draftFromRule(written, "VND");
    expect(draft.channels).toEqual(["SALES_CHANNEL_DINE_IN", NEWER]);
    expect(ruleFromDraft(draft).channels).toEqual(["SALES_CHANNEL_DINE_IN", NEWER]);
  });
});
