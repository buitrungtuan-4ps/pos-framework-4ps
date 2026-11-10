// The parts of a package a designer edits (ADR-0189 decision 1): screens, flows, rules and data.
// Each part is one entry here: its tab, the request field whose refusals it shows, and its panel.
// A part's designer joins the shell by replacing or adding its entry, in its own folder beside this
// one; the shell does not change.
//
// A panel is given the draft, a way to change it, and the cloud's refusals that name its field, so
// it can show each refusal beside what is refused.

import type { Accessor, Component } from "solid-js";

import type { PackageRefusal } from "../../../api/schemas";
import { t, type MessageKey } from "../../../i18n";
import type { PackageDraft } from "../../../lib/package-draft";
import { ScreensPart } from "../screens/ScreensPart";
import { RefusalList } from "./refusals";
import type { DraftChange } from "./use-draft";

/** What a part's panel is given. */
export type PartProps = {
  /** The draft as it stands. */
  readonly draft: Accessor<PackageDraft>;
  /** Changes the draft. */
  readonly change: (next: DraftChange) => void;
  /** The cloud's refusals that name this part's field, from the last check. */
  readonly refusals: readonly PackageRefusal[];
};

/** One part of a package, as the designer offers it. */
export type DesignerPart = {
  /** The tab's key. */
  readonly key: string;
  /** The tab's name. */
  readonly label: MessageKey;
  /** The request field the part writes: its refusals are those that name it or a field below it. */
  readonly field: string;
  /** The part's panel. */
  readonly Panel: Component<PartProps>;
};

/** What a part holds, in a line, and its refusals: the panel of a part whose designer is to come. */
function summary(count: (draft: PackageDraft) => number, key: MessageKey): Component<PartProps> {
  return (props) => (
    <div class="flex flex-col gap-3">
      <p class="text-sm text-ink">{t(key, { count: count(props.draft()) })}</p>
      <RefusalList refusals={props.refusals} draft={props.draft()} />
    </div>
  );
}

/** Every part, in the order the tabs show them. */
export const DESIGNER_PARTS: readonly DesignerPart[] = [
  {
    key: "screens",
    label: "designer.part.screens",
    field: "screens",
    Panel: ScreensPart,
  },
  {
    key: "data",
    label: "designer.part.data",
    field: "data",
    Panel: summary(
      (draft) =>
        (draft.data?.custom_fields?.length ?? 0) +
        (draft.data?.object_types?.length ?? 0) +
        (draft.data?.lists?.length ?? 0),
      "designer.dataSummary",
    ),
  },
];
