// The till's words for the package renderer (`packages/package-renderer`, ADR-0209): a package
// label in the till's language, what a node of a component this release lacks says, and how a
// button says its act failed.

import type { JSX } from "solid-js";

import { NeedsUpdateSlot, type Words } from "../../../packages/package-renderer/src/render";
import { t, type MessageKey } from "../i18n";
import en from "../i18n/en.json";
import { tenantLabel } from "../state/labels";
import { OfflineSkip } from "../state/lookup";
import { printFailure } from "../state/print-writes";

/**
 * A package label in the till's language. A key the till's own catalogue has is shown from it; any
 * other from the tenant's translation grid the store holds (ADR-0043), in the till's language and
 * then English; and a key in neither draws nothing rather than the key itself.
 */
export function packageLabel(key: string | undefined): string {
  if (key === undefined) {
    return "";
  }
  return Object.hasOwn(en, key) ? t(key as MessageKey) : (tenantLabel(key) ?? "");
}

/** What a node of a component this release lacks draws in its place. */
export function NeedsUpdate(): JSX.Element {
  return <NeedsUpdateSlot text={t("package.needs_update")} />;
}

/**
 * The words the till draws a package tree with. A failed act says what the till says of a print; a
 * lookup its flow step skipped offline says the step's note (ADR-0214 decision 5).
 */
export const TILL_WORDS: Words = {
  label: (key) => packageLabel(key),
  needsUpdate: () => t("package.needs_update"),
  failure: (caught) =>
    caught instanceof OfflineSkip ? packageLabel(caught.noteKey) || t("error.gateway_offline") : printFailure(caught),
};
