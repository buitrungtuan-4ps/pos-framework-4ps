// A screen's own regions as catalogued components (ADR-0190, ADR-0203 decision 1; ADR-0209 decision 3).
//
// A till screen that moves into the default package keeps its state and its taps in
// `screens/<Name>.tsx`, where the step gate resolves them, and hands each region it draws to the
// renderer through `RegionsContext`, under the region's component id. The catalogue's entry for
// that id is `region(id)`: it draws what the screen on view handed over, and nothing on a screen
// that has no such region. A region is given its node, so a frame region draws its children.

import { createContext, useContext, type JSX } from "solid-js";

import type { ComponentId } from "./components";
import type { Implementation, NodeProps } from "./render";

/** One region a screen draws. */
export type Region = (props: NodeProps) => JSX.Element;

/** The regions a screen hands over, by component id. */
export type Regions = Readonly<Partial<Record<ComponentId, Region>>>;

export const RegionsContext = createContext<Regions>({});

/** The implementation of the catalogued component `id`: the region the screen on view drew for it. */
export function region(id: ComponentId): Implementation {
  return (props) => useContext(RegionsContext)[id]?.(props) ?? null;
}
