// The parts of a package a designer edits (ADR-0189 decision 1): screens, flows, rules and data.
// Each part is one entry here: its tab, the request field whose refusals it shows, and its panel.
// A part's designer joins the shell by replacing or adding its entry, in its own folder beside this
// one; the shell does not change.
//
// A panel is given the draft, a way to change it, and the cloud's refusals that name its field, so
// it can show each refusal beside what is refused.

import type { Accessor, Component } from "solid-js";

import type { PackageRefusal } from "../../../api/schemas";
import type { MessageKey } from "../../../i18n";
import type { PackageDraft } from "../../../lib/package-draft";
import { DataPanel } from "../data/DataPanel";
import { FlowsPanel } from "../flows/FlowsPanel";
import { ReactionsPanel } from "../reactions/ReactionsPanel";
import { ScreensPart } from "../screens/ScreensPart";
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

/** Every part, in the order the tabs show them. */
export const DESIGNER_PARTS: readonly DesignerPart[] = [
  {
    key: "screens",
    label: "designer.part.screens",
    field: "screens",
    Panel: ScreensPart,
  },
  {
    key: "flows",
    label: "designer.part.flows",
    field: "flows",
    Panel: FlowsPanel,
  },
  {
    key: "reactions",
    label: "designer.part.reactions",
    field: "reactions",
    Panel: ReactionsPanel,
  },
  {
    key: "data",
    label: "designer.part.data",
    field: "data",
    Panel: DataPanel,
  },
];
