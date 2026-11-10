// The action catalogue (ADR-0191) as the console's designers read it: which act a flow may start
// with or run, and which a reaction may run. The catalogue itself is generated from `pos-proto`
// into `src/package/`; this module is the one place the designers reach it from.

import { ACTIONS, type Action } from "../package/actions";

export type { Action };

/** Every act, in the catalogue's order. */
export const CATALOGUE: readonly Action[] = ACTIONS;

/** The act `actionId` names, if the catalogue holds one. */
export const actionAt = (actionId: string): Action | undefined =>
  CATALOGUE.find((action) => action.action_id === actionId);

/**
 * The acts a reaction may run: a UI act or an edge write, never money or an external call
 * (ADR-0193 decision 5).
 */
export const reactionActs = (): readonly Action[] =>
  CATALOGUE.filter((action) => action.effect === "ACTION_EFFECT_UI" || action.effect === "ACTION_EFFECT_EDGE_WRITE");

/** The parameters a reaction fixes for `action`: every one a person does not enter at the till. */
export const fixedParams = (action: Action): Action["params"] =>
  action.params.filter((param) => param.kind !== "ACTION_PARAM_KIND_ENTERED");
