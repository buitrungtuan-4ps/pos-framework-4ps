// A package screen's lookup on the till (ADR-0214): what a person typed into the screen's
// `lookup_field`s, the act that sends them through the edge to the cloud gateway, and the answer the
// screen's `lookup_result`s draw.
//
// # What the till holds itself to
//
// - **The answer is drawn, never kept.** It lives in this module's memory while the screen is
//   shown, and `forgetLookup` drops it, with what was typed, when the screen goes. It is never
//   logged, written to storage or put in an event; a lookup moves no money.
// - **Paint first** (ADR-0196): the tap paints the act pending, and the request leaves after that
//   frame (`afterPaint`).
// - **The edge decides.** The person needs `integrations.lookup.run`; a button for it draws
//   disabled without it, and the edge refuses anyway. A refusal is the button's to say, in the
//   till's words for its token (`../lib/errors.ts`).
// - **A retry is the same act.** A tap again after a call that did not answer, with nothing typed
//   since, sends the same `idempotency_key`, so the cloud answers it once. An answer that comes
//   back after the screen has gone is dropped.
// - **A flow's offline branch is honoured** (ADR-0214 decision 5). An answer completes the lookup
//   steps of the table's or order's flows (`./flows.ts`). When the cloud is offline, each lookup step
//   with an `offline` branch is passed: one that skips says its note under the button in place of
//   the refusal, and one that goes to a step showing a screen opens that screen.

import { createSignal } from "solid-js";

import { api, ApiError } from "../api/client";
import { afterPaint } from "../lib/after-paint";
import { lookupAnswered, lookupOffline } from "./flows";
import { can, needsApprover } from "./permissions";
import { openPackageScreen } from "./reactions";

/** A lookup the cloud could not take, passed by its flow step's offline branch with this note. */
export class OfflineSkip extends Error {
  constructor(readonly noteKey: string) {
    super(noteKey);
  }
}

/** The act a screen's button runs a lookup with. */
export const LOOKUP_ACT = "integrations.lookup.run";

/** A table's channel, for choosing its flows' variants. */
const DINE_IN = "SALES_CHANNEL_DINE_IN";

/** The fields a lookup takes from the screen's own subject instead of from a person. */
export const SUBJECT_FIELDS: ReadonlySet<string> = new Set(["order_id", "table_id"]);

/** What the screen's lookup has come to. */
export type LookupAnswer =
  | { readonly state: "idle" }
  | { readonly state: "pending" }
  | { readonly state: "answered"; readonly result: Readonly<Record<string, unknown>> }
  | { readonly state: "refused" };

const IDLE: LookupAnswer = { state: "idle" };

const [typed, setTyped] = createSignal<Readonly<Record<string, string | number>>>({});
const [answer, setAnswer] = createSignal<LookupAnswer>(IDLE);
/** The subject fields the screen's lookup sends, by the `lookup_field`s that name them. */
const fromSubject = new Set<string>();
/** The key of the act a retry repeats: set on a tap, dropped once answered or when a value changes. */
let retryKey: string | undefined;
/** Counts the screens the lookup has served, so an answer for one that has gone is dropped. */
let generation = 0;

/** The screen's last answer, for its `lookup_result`s. */
export const lookupAnswer = answer;

/** Sets the value a person typed for `field`; an empty one is not sent. */
export function typeLookupValue(field: string, value: string | number | undefined): void {
  retryKey = undefined;
  setTyped((now) => {
    const { [field]: _dropped, ...rest } = now;
    return value === undefined ? rest : { ...rest, [field]: value };
  });
}

/** Says the screen's lookup sends its subject's `field` (`order_id` or `table_id`). */
export function sendSubjectField(field: string): void {
  fromSubject.add(field);
}

/** Drops what was typed and the answer: the screen is gone. */
export function forgetLookup(): void {
  setTyped({});
  setAnswer(IDLE);
  fromSubject.clear();
  retryKey = undefined;
  generation += 1;
}

/** Whether the signed-in person may run a lookup without an approver. */
export function lookupRuns(): boolean {
  return can(LOOKUP_ACT) && !needsApprover(LOOKUP_ACT);
}

/** A fresh key for one act: 32 hex digits, inside the edge's 1 to 64 letters, digits, `-` and `_`. */
function newKey(): string {
  return crypto.randomUUID().replaceAll("-", "");
}

/**
 * Runs the lookup the package bound: its endpoint, what the screen's fields hold, and the subject's
 * ids those fields name. Paints it pending first; answers the request, whose refusal the button
 * says.
 */
export async function runLookup(
  binding: { readonly params?: Readonly<Record<string, unknown>> },
  supplied: Readonly<Record<string, string>>,
): Promise<void> {
  const endpoint = binding.params?.["gateway_endpoint_id"];
  const values: Record<string, string | number> = { ...typed() };
  for (const field of fromSubject) {
    const id = supplied[field];
    if (id !== undefined) {
      values[field] = id;
    }
  }
  const subject = supplied["table_id"] ?? supplied["order_id"] ?? "";
  const channel = supplied["table_id"] === undefined ? "" : DINE_IN;
  retryKey ??= newKey();
  const key = retryKey;
  const screen = generation;
  setAnswer({ state: "pending" });
  await afterPaint();
  try {
    const answered = await api.lookup({
      gateway_endpoint_id: typeof endpoint === "string" ? endpoint : "",
      idempotency_key: key,
      values,
    });
    if (screen === generation) {
      retryKey = undefined;
      setAnswer({ state: "answered", result: answered.result });
    }
    lookupAnswered(subject, channel);
  } catch (caught) {
    if (screen === generation) {
      // Only a call nobody answered in time is repeated under its key; any other refusal is final.
      if (!(caught instanceof ApiError && caught.reason === "GATEWAY_TIMEOUT")) {
        retryKey = undefined;
      }
      setAnswer({ state: "refused" });
    }
    if (caught instanceof ApiError && caught.reason === "GATEWAY_OFFLINE") {
      const branch = lookupOffline(subject, channel);
      if (branch.screenId !== undefined) {
        openPackageScreen(branch.screenId);
      }
      if (branch.noteKey !== undefined) {
        throw new OfflineSkip(branch.noteKey);
      }
    }
    throw caught;
  }
}
