// The one path every edge write takes (ADR-0196 decision 1, ADR-0173 decision 4): an act whose
// catalogue effect is `ACTION_EFFECT_EDGE_WRITE` paints its change at once, marked as sending, and
// the edge decides.
//
// - **The screen moves at once.** `paint` applies the provisional change and hands back its undo;
//   one request goes to the edge. Nothing is queued: the edge is the only writer (decision 5).
// - **The edge confirms.** Its answer, or its event, whichever lands first, makes the change real;
//   `confirmed` takes the answer.
// - **A refusal rolls back,** and the caller shows the refusal's own sentence.
// - **No answer rolls back and reads again,** because the edge may have made it: `reread` reads the
//   act's subject. The act is never sent again by the till.
//
// Payment waits while any write is on its way (`writesPending`).

import { createSignal } from "solid-js";

import { ApiError } from "../api/client";
import type { ACTIONS } from "../package/actions";
import { state } from "./store";

/** The catalogue's edge writes: the only acts this path takes, checked when the till is built. */
export type EdgeWriteId = Extract<(typeof ACTIONS)[number], { effect: "ACTION_EFFECT_EDGE_WRITE" }>["action_id"];

const [inFlight, setInFlight] = createSignal(0);

/** Whether a write is on its way to the edge, or a line is still waiting for the edge's figures. */
export function writesPending(): boolean {
  return inFlight() > 0 || Object.values(state.lines).some((line) => line.pending === true);
}

/** One edge write: its paint and undo, the request, and what to read again with no answer. */
export interface EdgeWrite<T> {
  /** Paints the change and returns its undo; left out where the caller painted before the request. */
  readonly paint?: () => (caught: unknown) => void;
  /** The undo of a change the caller painted itself. */
  readonly undo?: (caught: unknown) => void;
  readonly send: () => Promise<T>;
  readonly confirmed?: (answer: T) => void;
  /** Reads the act's subject again, when the edge did not answer. */
  readonly reread: () => void;
}

/**
 * Runs `write` for the catalogue's `action`. Answers what the edge answered; throws what it threw,
 * after the undo and, with no answer, the re-read.
 */
export async function editWrite<T>(action: EdgeWriteId, write: EdgeWrite<T>): Promise<T> {
  void action;
  const undo = write.paint?.() ?? write.undo ?? (() => undefined);
  setInFlight((count) => count + 1);
  try {
    const answer = await write.send();
    write.confirmed?.(answer);
    return answer;
  } catch (caught) {
    undo(caught);
    if (!(caught instanceof ApiError)) {
      write.reread();
    }
    throw caught;
  } finally {
    setInFlight((count) => count - 1);
  }
}
