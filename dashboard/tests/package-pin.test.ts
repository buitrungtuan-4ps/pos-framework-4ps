// What the Packages screen writes is the pin it means
// ([ADR-0189](../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 2).
//
// Each write republishes every store, so the order is the safety: ids before the mode, so no store
// is ever told to run a mode whose versions are not written yet; and "no pin" is one write, so the
// default package returns at once. The tenant's pin is read from its tenant-scope values only.

import { describe, expect, it } from "vitest";

import {
  ALLOWED_VERSIONS_MAX,
  choiceGap,
  pinWrites,
  storePin,
  tenantPin,
} from "../src/lib/package-pin";

const V1 = "01J00000000000000000000V01";
const V2 = "01J00000000000000000000V02";

const written = (setting_key: string, value: unknown, scope = "SETTING_SCOPE_TENANT") => ({
  setting_key,
  scope,
  value: value as never,
});

describe("tenantPin", () => {
  it("reads nothing written as no pin", () => {
    expect(tenantPin([])).toEqual({
      mode: "PACKAGE_MODE_UNSPECIFIED",
      lockedVersionId: null,
      allowedVersionIds: [],
      defaultVersionId: null,
    });
  });

  it("reads the tenant's own values and ignores a store's", () => {
    const pin = tenantPin([
      written("package.mode", "PACKAGE_MODE_LOCKED"),
      written("package.locked_version_id", V1),
      written("package.locked_version_id", V2, "SETTING_SCOPE_STORE"),
      written("package.allowed_version_ids", [V1, V2]),
    ]);
    expect(pin.mode).toBe("PACKAGE_MODE_LOCKED");
    expect(pin.lockedVersionId).toBe(V1);
    expect(pin.allowedVersionIds).toEqual([V1, V2]);
  });

  it("reads a mode it does not know as no pin", () => {
    expect(tenantPin([written("package.mode", "PACKAGE_MODE_LATER")]).mode).toBe(
      "PACKAGE_MODE_UNSPECIFIED",
    );
  });
});

describe("storePin", () => {
  it("reads a store's effective choice and what its people chose", () => {
    const pin = storePin([
      { setting_key: "package.mode", value: "PACKAGE_MODE_CHOOSE" },
      { setting_key: "package.allowed_version_ids", value: [V1, V2] },
      { setting_key: "package.default_version_id", value: V1 },
      { setting_key: "package.chosen_version_id", value: V2 },
    ]);
    expect(pin.mode).toBe("PACKAGE_MODE_CHOOSE");
    expect(pin.allowedVersionIds).toEqual([V1, V2]);
    expect(pin.chosenVersionId).toBe(V2);
    expect(storePin([]).chosenVersionId).toBeNull();
  });
});

describe("pinWrites", () => {
  it("writes a lock's version before its mode", () => {
    expect(pinWrites({ mode: "PACKAGE_MODE_LOCKED", versionId: V2 })).toEqual([
      { settingKey: "package.locked_version_id", value: V2 },
      { settingKey: "package.mode", value: "PACKAGE_MODE_LOCKED" },
    ]);
  });

  it("writes a choice's versions and default before its mode", () => {
    expect(
      pinWrites({
        mode: "PACKAGE_MODE_CHOOSE",
        allowedVersionIds: [V1, V2],
        defaultVersionId: V1,
      }).map((write) => write.settingKey),
    ).toEqual(["package.allowed_version_ids", "package.default_version_id", "package.mode"]);
  });

  it("removes a pin in one write", () => {
    expect(pinWrites({ mode: "PACKAGE_MODE_UNSPECIFIED" })).toEqual([
      { settingKey: "package.mode", value: "PACKAGE_MODE_UNSPECIFIED" },
    ]);
  });
});

describe("choiceGap", () => {
  it("refuses an empty choice, too many versions, and a default outside the choice", () => {
    expect(choiceGap([], V1)).toBe("packages.choiceNone");
    const many = Array.from({ length: ALLOWED_VERSIONS_MAX + 1 }, (_, n) => `V${n}`);
    expect(choiceGap(many, "V0")).toBe("packages.choiceTooMany");
    expect(choiceGap([V1], V2)).toBe("packages.choiceDefault");
    expect(choiceGap([V1, V2], V2)).toBeNull();
  });
});
