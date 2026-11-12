// The cloud's refusals of a draft, in the operator's words (ADR-0190 decision 8). The dry run names
// each fault by a stable `reason` and says where it is; the words are the console's, so a refusal
// reads in the console's language and never as the server's English sentence.

import { For, Show } from "solid-js";

import type { PackageRefusal } from "../../../api/schemas";
import { t, type MessageKey } from "../../../i18n";
import type { PackageDraft } from "../../../lib/package-draft";

const REASON_KEYS: Readonly<Record<string, MessageKey>> = {
  TOO_MANY_SCREENS: "designer.refusal.tooManyScreens",
  SCREEN_ID: "designer.refusal.screenId",
  VARIANTS: "designer.refusal.variants",
  TOO_MANY_NODES: "designer.refusal.tooManyNodes",
  TOO_DEEP: "designer.refusal.tooDeep",
  UNKNOWN_COMPONENT: "designer.refusal.unknownComponent",
  DEVICE_CLASS: "designer.refusal.deviceClass",
  CHILDREN: "designer.refusal.children",
  UNKNOWN_PROPERTY: "designer.refusal.unknownProperty",
  MISSING_PROPERTY: "designer.refusal.missingProperty",
  PROPERTY_VALUE: "designer.refusal.propertyValue",
  UNKNOWN_ACTION: "designer.refusal.unknownAction",
  FIXED_PARAM: "designer.refusal.fixedParam",
  MISSING_PARAM: "designer.refusal.missingParam",
  NOT_ADDITIVE: "designer.refusal.notAdditive",
  TOO_LARGE: "designer.refusal.tooLarge",
  UNKNOWN_TRANSLATION_KEY: "designer.refusal.unknownTranslationKey",
};

const FIELD_KEYS: Readonly<Record<string, MessageKey>> = {
  version_label: "designer.refusal.versionLabel",
};

/** A refusal in words: by its reason, else by its field, else the general one. */
export function refusalText(refusal: PackageRefusal, draft: PackageDraft): string {
  const screen =
    refusal.screen_index === undefined
      ? ""
      : (draft.screens?.[refusal.screen_index]?.body.screen_id ?? String(refusal.screen_index));
  const args = {
    screen,
    node: (refusal.node_index ?? 0) + 1,
    name: refusal.name ?? "",
    field: refusal.field,
  };
  const key = REASON_KEYS[refusal.reason] ?? FIELD_KEYS[refusal.field] ?? "designer.refusal.other";
  return t(key, args);
}

/** The refusals a part shows, beside what they refuse. Nothing when there are none. */
export function RefusalList(props: { refusals: readonly PackageRefusal[]; draft: PackageDraft }) {
  return (
    <Show when={props.refusals.length > 0}>
      <ul role="alert" class="flex flex-col gap-1 rounded-token border border-danger p-3 text-sm text-danger" data-outcome="draft-refused">
        <For each={props.refusals}>{(refusal) => <li>{refusalText(refusal, props.draft)}</li>}</For>
      </ul>
    </Show>
  );
}
