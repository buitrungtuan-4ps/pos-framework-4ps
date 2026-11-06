// The package's flows on the till (ADR-0192 decision 3): which steps an act waits on, and which the
// till asks for. The flows are the running version's, from `GET /api/package` (`running.flows`), or
// the release's default package's when the store runs none. They are loaded into the rules module
// (`../rules/till-rules.ts`) whenever they change, which compiles their conditions; a tap only asks.
//
// # What the till does with a flow
//
// - **It asks a step's action of the operator.** A dine-in table's seat that a flow makes a step
//   asks the guest count first (`guestStep`). A required step offers no Skip.
// - **It names a missing prerequisite.** An act whose step waits on one that is not done is held,
//   with the missing step named (`missingFor`), as the edge would refuse it.
// - **It never decides alone.** The edge re-checks every guarded command (decision 4). Where the
//   till cannot answer — the module not loaded yet, or an order whose facts it does not hold — it
//   holds nothing back and lets the edge answer.

import { createEffect, createRoot, createSignal } from "solid-js";

import DEFAULT_PACKAGE from "../package/default-package.json";
import { resolveVariant, type Variant } from "../package/variant";
import { t } from "../i18n";
import { needsRules, startTillRules, tillRules, type Fact, type Facts, type Flow, type FlowStep } from "../rules/till-rules";
import { reactionsNeedRules } from "./reactions";

/**
 * What the till knows of each table's guest count: the count it seated it with, or `"skipped"`.
 * A table it did not seat is absent, and the till then holds nothing back for want of a count.
 */
const [guestsAt, setGuestsAt] = createSignal<Readonly<Record<string, number | "skipped">>>({});

/** Records how a table was seated: with a count, or with the count skipped. */
export function recordGuests(tableId: string, count: number | undefined): void {
  setGuestsAt((known) => ({ ...known, [tableId]: count ?? "skipped" }));
}

/** Forgets a table's count: its seat was refused. */
export function forgetGuests(tableId: string): void {
  setGuestsAt((known) => Object.fromEntries(Object.entries(known).filter(([table]) => table !== tableId)));
}

/** The guest count the till seated a table with, `"skipped"`, or `undefined` when it did not seat it. */
export function guestsOf(tableId: string): number | "skipped" | undefined {
  return guestsAt()[tableId];
}

/** The flows of the running version, `null` for the release's default package. */
const [published, setPublished] = createSignal<readonly Variant<Flow>[] | null>(null);
/** Whether the rules module holds the flows `activeFlows` returns. */
const [loaded, setLoaded] = createSignal(false);

/** The default package's flows. */
const DEFAULT_FLOWS: readonly Variant<Flow>[] =
  ((DEFAULT_PACKAGE as { flows?: Variant<Flow>[] }).flows ?? []) as Variant<Flow>[];

/** Every variant of every flow the till runs now, in the package's order. */
export function activeFlows(): readonly Variant<Flow>[] {
  return published() ?? DEFAULT_FLOWS;
}

/** What `GET /api/package` answered: the running version's flows, if it names any. */
export function applyPackage(body: unknown): void {
  const running = (body as { running?: { flows?: Variant<Flow>[] } | null } | null)?.running;
  setPublished(running === null || running === undefined ? null : (running.flows ?? []));
}

/**
 * Loads the flows into the rules module, and again whenever they change. The module is fetched and
 * built only once a package's flows need it (`needsRules`), or a reaction has a condition
 * (`./reactions.ts`): the default package has neither.
 */
export function keepRulesLoaded(): void {
  createRoot(() => {
    createEffect(() => {
      const flows = activeFlows().map((variant) => variant.body);
      if (!needsRules(flows) && !reactionsNeedRules()) {
        return;
      }
      void startTillRules().then((rules) => {
        if (rules !== undefined) {
          setLoaded(rules.load(flows).ok);
        }
      });
    });
  });
}

/** One step of a flow, with the variant it is in, as the rules module is asked about it. */
export interface StepAt {
  readonly flowId: string;
  /** Which of the flow's variants, counted among those with its `flow_id`, in order. */
  readonly variant: number;
  readonly step: FlowStep;
}

/** The variant of each flow chosen for `channel`, and the steps that run `actionId` in it. */
function stepsFor(actionId: string, channel: string): StepAt[] {
  const variants = activeFlows();
  const ids = [...new Set(variants.map((variant) => variant.body.flow_id))];
  return ids.flatMap((flowId) => {
    const own = variants.filter((variant) => variant.body.flow_id === flowId);
    const chosen = resolveVariant(own, { sales_channel: channel });
    if (chosen === undefined) {
      return [];
    }
    const variant = own.findIndex((candidate) => candidate.body === chosen);
    return chosen.steps
      .filter((step) => step.action_id === actionId)
      .map((step) => ({ flowId, variant, step }));
  });
}

/** Whether a step applies over `facts`. Unanswerable (no module, or an error) applies: fail closed. */
function applies(step: FlowStep, facts: Facts): boolean {
  if (step.applies_when === undefined) {
    return true;
  }
  const rules = loaded() ? tillRules() : undefined;
  const answer = rules?.holds(step.applies_when, facts);
  return answer === undefined || !answer.ok || answer.holds;
}

/** The guest-count step a dine-in seat runs, and whether it is required, or `undefined`. */
export function guestStep(channel: string): { readonly required: boolean } | undefined {
  const facts: Facts = { channel, order: { channel } };
  const found = stepsFor("sales.table.seat", channel).find((at) => applies(at.step, facts));
  return found === undefined ? undefined : { required: found.step.kind === "STEP_KIND_REQUIRED" };
}

/**
 * The step `actionId` waits on that is not done, for an order whose facts are `facts`, or `null`.
 * `guestsSkipped` says the operator skipped the guest count when seating it. `null` too when the
 * till cannot answer: the edge re-checks.
 */
export function missingFor(actionId: string, channel: string, facts: Facts, guestsSkipped = false): string | null {
  const rules = loaded() ? tillRules() : undefined;
  if (rules === undefined) {
    return null;
  }
  const skipped = guestsSkipped ? stepsFor("sales.table.seat", channel).map((at) => at.step.step_id) : [];
  for (const at of stepsFor(actionId, channel)) {
    const answer = rules.missing(at.flowId, at.step.step_id, { skipped }, facts, at.variant);
    if (answer.ok && answer.missing !== null) {
      return answer.missing;
    }
  }
  return null;
}

/** A table's facts, as a condition reads them: its order's channel, line count and guest count. */
export function tableFacts(tableId: string, lineCount: number): Facts {
  const guests = guestsOf(tableId);
  const order: Record<string, Fact> =
    typeof guests === "number"
      ? { channel: DINE_IN_TOKEN, line_count: lineCount, guest_count: guests }
      : { channel: DINE_IN_TOKEN, line_count: lineCount };
  return { channel: DINE_IN_TOKEN, order };
}

/** The flow steps a seat runs, by flow and step: the ones a seat with a guest count completes. */
export function seatSteps(): readonly { readonly flowId: string; readonly stepId: string }[] {
  return stepsFor("sales.table.seat", DINE_IN_TOKEN).map((at) => ({ flowId: at.flowId, stepId: at.step.step_id }));
}

/**
 * The step a table's next line waits on, or `null`: from the guest count this till seated it with
 * and its lines. A table this till did not seat holds nothing back, since the till does not know
 * its count; the edge re-checks the line.
 */
export function lineWaits(tableId: string, lineCount: number): string | null {
  const guests = guestsOf(tableId);
  if (guests === undefined) {
    return null;
  }
  const order: Record<string, Fact> =
    guests === "skipped"
      ? { channel: DINE_IN_TOKEN, line_count: lineCount }
      : { channel: DINE_IN_TOKEN, line_count: lineCount, guest_count: guests };
  return missingFor("sales.line.add", DINE_IN_TOKEN, { channel: DINE_IN_TOKEN, order }, guests === "skipped");
}

/** What the till says of a step a line waits on. */
export function flowWaitSentence(step: string): string {
  return isGuestStep(step) ? t("order.flow_waits_guests") : t("order.flow_waits_step", { step });
}

/** Whether `step` is a step that seats a table, which is the one that asks the guest count. */
function isGuestStep(step: string): boolean {
  return activeFlows().some((variant) =>
    variant.body.steps.some((candidate) => candidate.step_id === step && candidate.action_id === "sales.table.seat"),
  );
}

/** The dine-in channel's wire token: a table's orders are dine-in. */
const DINE_IN_TOKEN = "SALES_CHANNEL_DINE_IN";
