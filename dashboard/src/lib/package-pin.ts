// The tenant's package pin, read from its written settings and turned back into writes
// ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 2).
//
// A pin is four settings of the `package` node, and the cloud republishes every store on each write.
// So the order of the writes is the safety: the version ids go first and the mode last, and a store
// never runs a mode whose versions have not been written yet. Clearing goes the other way: the mode
// goes back to "no pin" before anything else, so the default package returns at once.

import type { Json } from "../api/types";

/** The most versions a choice may allow, as `pos_proto::package::ALLOWED_VERSIONS_MAX` says. */
export const ALLOWED_VERSIONS_MAX = 32;

export type PackageMode = "PACKAGE_MODE_UNSPECIFIED" | "PACKAGE_MODE_LOCKED" | "PACKAGE_MODE_CHOOSE";

/** The tenant's pin as it stands. */
export type PackagePin = {
  readonly mode: PackageMode;
  readonly lockedVersionId: string | null;
  readonly allowedVersionIds: readonly string[];
  readonly defaultVersionId: string | null;
};

/** What the operator asks for. */
export type PinRequest =
  | { readonly mode: "PACKAGE_MODE_UNSPECIFIED" }
  | { readonly mode: "PACKAGE_MODE_LOCKED"; readonly versionId: string }
  | {
      readonly mode: "PACKAGE_MODE_CHOOSE";
      readonly allowedVersionIds: readonly string[];
      readonly defaultVersionId: string;
    };

/** One setting write, in the order it must be sent. */
export type PinWrite = { readonly settingKey: string; readonly value: Json };

/** A setting value, as far as this module reads it. */
type Valued = { readonly setting_key: string; readonly value: Json };

/** A written setting: a value at a scope. */
type Written = Valued & { readonly scope: string };

const TENANT = "SETTING_SCOPE_TENANT";

/** The tenant-scope pin among `values`. A setting never written reads as its absent value. */
export function tenantPin(values: readonly Written[]): PackagePin {
  return pinOf(values.filter((value) => value.scope === TENANT));
}

/** One store's pin, from its effective settings, and the version its people chose, if any. */
export function storePin(effective: readonly Valued[]): PackagePin & {
  readonly chosenVersionId: string | null;
} {
  const chosen = effective.find((value) => value.setting_key === "package.chosen_version_id");
  return {
    ...pinOf(effective),
    chosenVersionId: typeof chosen?.value === "string" && chosen.value !== "" ? chosen.value : null,
  };
}

function pinOf(values: readonly Valued[]): PackagePin {
  const at = (field: string): Json | undefined =>
    values.find((value) => value.setting_key === `package.${field}`)?.value;
  const text = (value: Json | undefined): string | null =>
    typeof value === "string" && value !== "" ? value : null;
  const mode = text(at("mode"));
  const allowed = at("allowed_version_ids");
  return {
    mode:
      mode === "PACKAGE_MODE_LOCKED" || mode === "PACKAGE_MODE_CHOOSE"
        ? mode
        : "PACKAGE_MODE_UNSPECIFIED",
    lockedVersionId: text(at("locked_version_id")),
    allowedVersionIds: Array.isArray(allowed)
      ? allowed.filter((id): id is string => typeof id === "string")
      : [],
    defaultVersionId: text(at("default_version_id")),
  };
}

/** Why a choice cannot be saved, as a message key, or `null` when it can. */
export function choiceGap(
  allowedVersionIds: readonly string[],
  defaultVersionId: string,
): "packages.choiceNone" | "packages.choiceTooMany" | "packages.choiceDefault" | null {
  if (allowedVersionIds.length === 0) {
    return "packages.choiceNone";
  }
  if (allowedVersionIds.length > ALLOWED_VERSIONS_MAX) {
    return "packages.choiceTooMany";
  }
  return allowedVersionIds.includes(defaultVersionId) ? null : "packages.choiceDefault";
}

/** The writes that make `request` the tenant's pin, in the order they must be sent. */
export function pinWrites(request: PinRequest): PinWrite[] {
  switch (request.mode) {
    case "PACKAGE_MODE_UNSPECIFIED":
      return [{ settingKey: "package.mode", value: request.mode }];
    case "PACKAGE_MODE_LOCKED":
      return [
        { settingKey: "package.locked_version_id", value: request.versionId },
        { settingKey: "package.mode", value: request.mode },
      ];
    case "PACKAGE_MODE_CHOOSE":
      return [
        { settingKey: "package.allowed_version_ids", value: [...request.allowedVersionIds] },
        { settingKey: "package.default_version_id", value: request.defaultVersionId },
        { settingKey: "package.mode", value: request.mode },
      ];
  }
}
