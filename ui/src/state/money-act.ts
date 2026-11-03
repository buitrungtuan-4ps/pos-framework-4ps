// How the till runs a money act (ADR-0196, ADR-0204): it checks what it can predict, paints the
// act's pending state at once, and lets the edge decide.
//
// - **Predictable refusals first** (safeguard 2). A refusal the till can decide is shown before
//   anything is sent or painted.
// - **Pending, never derived.** While the act is out, the screen shows it pending with what the edge
//   already confirmed, and nothing the act would derive (change due, a receipt).
// - **The edge decides.** Its answer shows the act done; its refusal rolls the screen back with the
//   refusal's own sentence, which the operator must dismiss (safeguard 3).
// - **No answer is never a rollback** (ADR-0204). The act stays pending and its subject is read again
//   every 2 s for 30 s, then every 10 s with a sentence saying the store server cannot be reached,
//   until the edge says whether it committed. Nothing is re-sent, and while the answer is unknown
//   the subject is marked unconfirmed, so the act is not offered on it again.

import { createSignal, onCleanup } from "solid-js";

import { ApiError } from "../api/client";
import { errorMessage } from "../lib/errors";

export type MoneyPhase = "idle" | "sending" | "checking" | "unreachable";

/** What a read of the subject says about an act that went unanswered. */
export type Recheck<T> = { readonly committed: T } | { readonly refused: string } | null;

export const RECHECK_EVERY_MS = 2_000;
export const RECHECK_FAST_FOR_MS = 30_000;
export const RECHECK_SLOW_MS = 10_000;

const [unconfirmed, setUnconfirmed] = createSignal<ReadonlySet<string>>(new Set<string>());

/** Whether an act on this subject went unanswered and the edge has not yet said what came of it. */
export function isUnconfirmed(subject: string | null): boolean {
  return subject !== null && unconfirmed().has(subject);
}

function mark(subject: string, on: boolean): void {
  setUnconfirmed((current) => {
    const next = new Set(current);
    if (on) {
      next.add(subject);
    } else {
      next.delete(subject);
    }
    return next;
  });
}

/** A refusal the edge gave, as opposed to an answer that never came: a 5xx or no response at all. */
export function isRefusal(caught: unknown): caught is ApiError {
  return caught instanceof ApiError && caught.status < 500;
}

export interface MoneyAct<T> {
  /** Sends the act, unless `precheck` names a refusal. */
  readonly run: (subject: string, precheck: () => string | null, send: () => Promise<T>) => Promise<void>;
  /** Picks up an act left unconfirmed on this subject, as a screen opened on it again does. */
  readonly resume: (subject: string) => void;
  readonly phase: () => MoneyPhase;
  readonly refusal: () => string | null;
  readonly dismiss: () => void;
}

export function useMoneyAct<T>(options: {
  readonly recheck: (subject: string) => Promise<Recheck<T>>;
  readonly committed: (value: T) => void;
  readonly timers?: { set: (run: () => void, ms: number) => number; clear: (id: number) => void };
  readonly now?: () => number;
}): MoneyAct<T> {
  const timers = options.timers ?? {
    set: (run: () => void, ms: number) => window.setTimeout(run, ms),
    clear: (id: number) => window.clearTimeout(id),
  };
  const now = options.now ?? (() => performance.now());
  const [phase, setPhase] = createSignal<MoneyPhase>("idle");
  const [refusal, setRefusal] = createSignal<string | null>(null);
  let timer: number | null = null;
  onCleanup(() => {
    if (timer !== null) {
      timers.clear(timer);
    }
  });

  const settleWith = (subject: string, answer: NonNullable<Recheck<T>>) => {
    mark(subject, false);
    setPhase("idle");
    if ("committed" in answer) {
      options.committed(answer.committed);
    } else {
      setRefusal(answer.refused);
    }
  };

  const check = (subject: string, since: number) => {
    const slow = now() - since >= RECHECK_FAST_FOR_MS;
    if (slow) {
      setPhase("unreachable");
    }
    timer = timers.set(
      () => {
        timer = null;
        options
          .recheck(subject)
          .then((answer) => (answer === null ? check(subject, since) : settleWith(subject, answer)))
          .catch(() => check(subject, since));
      },
      slow ? RECHECK_SLOW_MS : RECHECK_EVERY_MS,
    );
  };

  const resume = (subject: string) => {
    if (isUnconfirmed(subject) && phase() === "idle") {
      setPhase("checking");
      check(subject, now());
    }
  };

  const run = async (subject: string, precheck: () => string | null, send: () => Promise<T>) => {
    if (phase() !== "idle" || refusal() !== null || isUnconfirmed(subject)) {
      return;
    }
    const predicted = precheck();
    if (predicted !== null) {
      setRefusal(predicted);
      return;
    }
    setPhase("sending");
    try {
      const value = await send();
      setPhase("idle");
      options.committed(value);
    } catch (caught) {
      if (isRefusal(caught)) {
        setPhase("idle");
        setRefusal(errorMessage(caught));
        return;
      }
      mark(subject, true);
      setPhase("checking");
      check(subject, now());
    }
  };

  return { run, resume, phase, refusal, dismiss: () => setRefusal(null) };
}
