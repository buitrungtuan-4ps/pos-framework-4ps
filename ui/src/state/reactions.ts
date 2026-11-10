// The package's reactions on the till (ADR-0193 decision 5): *when* a trigger fires here, *if* its
// condition holds, *then* one catalogue act. The reactions are the running version's, from
// `GET /api/package` (`running.reactions`); the release's default package has none.
//
// # What the till holds itself to
//
// - **Never on a tap.** A trigger fires once the edge has confirmed the act it follows, and the
//   reactions run after the next frame (`afterPaint`), so a tap's frame carries only the tap.
// - **At most 16 per trigger**, as the edge checked on apply, in the package's order.
// - **No chaining.** A reaction's act does not fire a trigger: triggers fired while reactions run,
//   and the screen a reaction opens, are passed over.
// - **The signed-in person's permissions.** An act whose every permission the person does not hold
//   directly is not run: a reaction never asks for an approver. The edge checks the command as any.
// - **A condition the till cannot answer does not run** (the module not loaded, or an error).
// - **Only acts this till runs.** A UI act opening a screen, and the prints; any other act a
//   package names is passed over here, and the edge never sees it. A package screen's button runs
//   the same acts through the same runners (`buttonRuns`, `runButton`), on its own tap.

import { createSignal } from "solid-js";

import { api } from "../api/client";
import { afterPaint } from "../lib/after-paint";
import { ACTIONS } from "../package/actions";
import { resolveVariant, type Variant } from "../package/variant";
import { tillRules, type Facts } from "../rules/till-rules";
import { can, needsApprover, type PermissionId } from "./permissions";
import { printWrite } from "./print-writes";

/** One reaction, as `pos_proto::reaction::Reaction` writes it. */
export interface Reaction {
  readonly reaction_id: string;
  readonly when: string;
  readonly screen_id?: string;
  readonly flow_id?: string;
  readonly step_id?: string;
  readonly if?: string;
  readonly then: { readonly action_id: string; readonly params?: Readonly<Record<string, string>> };
}

/** What a trigger fired for: its qualifier, the ids an act may take from it, and its facts. */
export interface Fired {
  readonly screenId?: string;
  readonly flowId?: string;
  readonly stepId?: string;
  readonly tableId?: string;
  readonly orderId?: string;
  readonly billId?: string;
  readonly channel: string;
  readonly facts: Facts;
}

/** The most reactions one trigger runs (`pos_proto::reaction::REACTIONS_PER_TRIGGER_MAX`). */
const PER_TRIGGER_MAX = 16;

const [published, setPublished] = createSignal<readonly Variant<Reaction>[]>([]);

/** What `GET /api/package` answered: the running version's reactions, none for the default package. */
export function applyReactions(body: unknown): void {
  const running = (body as { running?: { reactions?: Variant<Reaction>[] } | null } | null)?.running;
  setPublished(running?.reactions ?? []);
}

/** Whether the running version has any reaction: a trigger costs nothing more when it has none. */
export function hasReactions(): boolean {
  return published().length > 0;
}

/** Whether a reaction has a condition, so the rules module is needed to answer it. */
export function reactionsNeedRules(): boolean {
  return published().some((variant) => variant.body.if !== undefined);
}

/** Opens a screen of the package; set by the app's root, which holds the router. */
let openScreen: ((screenId: string, fired: Fired) => boolean) | undefined;

/** Lets reactions open screens, through the app's router. */
export function setScreenOpener(opener: (screenId: string, fired: Fired) => boolean): void {
  openScreen = opener;
}

/**
 * Opens the package screen `screenId` for a button a package screen draws, through the router a
 * reaction uses; `false` when the till cannot (no router yet, or a screen that needs a table).
 */
export function openPackageScreen(screenId: string): boolean {
  return openScreen?.(screenId, { channel: "", facts: {} }) ?? false;
}

/** Whether reactions are running now, or the screen being entered is one a reaction opened. */
let running = 0;
let openedByReaction: string | undefined;

/**
 * What an act run here answers: whether it ran, or, for a request to the edge, the request itself,
 * whose failure is the caller's to show or to drop.
 */
type Ran = boolean | Promise<unknown>;

/**
 * The acts this till runs for a reaction or a package screen's button: a UI act opening a screen,
 * and the prints. None of them moves money.
 */
const RUNNERS: Readonly<Record<string, (params: Readonly<Record<string, string>>, fired: Fired) => Ran>> = {
  "ui.screen.open": (params, fired) => openScreen?.(params.screen_id ?? "", fired) ?? false,
  "billing.check.print": (params, fired) => {
    const billId = params.bill_id ?? fired.billId;
    const orderId = params.order_id ?? fired.orderId;
    const tableId = params.table_id ?? fired.tableId;
    const send =
      billId !== undefined
        ? () => api.printBillCheck(billId)
        : orderId !== undefined
          ? () => api.printOrderCheck(orderId)
          : tableId !== undefined
            ? () => api.printTableCheck(tableId)
            : undefined;
    return send !== undefined && printWrite("billing.check.print", send);
  },
  "reports.shift.print_x": (params) => printWrite("reports.shift.print_x", () => api.printX(params.shift_id)),
};

/** A reaction's request is its own: what came of it is the edge's, and nobody waits on it. */
function sent(ran: Ran): void {
  if (ran instanceof Promise) {
    ran.catch(() => undefined);
  }
}

/** Whether the signed-in person holds every permission the act needs, without an approver. */
function permitted(actionId: string): boolean {
  const action = ACTIONS.find((candidate) => candidate.action_id === actionId);
  const needed = (action?.permissions ?? []) as readonly PermissionId[];
  return action !== undefined && needed.every((id) => can(id) && !needsApprover(id));
}

/** Whether the reaction's condition holds over the trigger's facts; unanswerable does not. */
function holds(reaction: Reaction, facts: Facts): boolean {
  if (reaction.if === undefined) {
    return true;
  }
  const answer = tillRules()?.holds(reaction.if, facts);
  return answer !== undefined && answer.ok && answer.holds;
}

/** The reactions `trigger` fires for `fired`, each reaction's variant chosen for its channel. */
export function reactionsFor(trigger: string, fired: Fired): Reaction[] {
  const variants = published();
  const ids = [...new Set(variants.map((variant) => variant.body.reaction_id))];
  return ids
    .flatMap((id) => {
      const chosen = resolveVariant(
        variants.filter((variant) => variant.body.reaction_id === id),
        { sales_channel: fired.channel },
      );
      return chosen === undefined ? [] : [chosen];
    })
    .filter(
      (reaction) =>
        reaction.when === trigger &&
        reaction.screen_id === fired.screenId &&
        reaction.flow_id === fired.flowId &&
        reaction.step_id === fired.stepId,
    )
    .slice(0, PER_TRIGGER_MAX);
}

/**
 * Fires `trigger`: once the frame is drawn, runs each of its reactions whose condition holds and
 * whose act the signed-in person may do. Passed over while reactions run (no chaining).
 */
export function react(trigger: string, fired: Fired): void {
  if (running > 0) {
    return;
  }
  if (trigger === "REACTION_TRIGGER_SCREEN_ENTERED" && fired.screenId === openedByReaction) {
    openedByReaction = undefined;
    return;
  }
  const reactions = reactionsFor(trigger, fired);
  if (reactions.length === 0) {
    return;
  }
  void afterPaint().then(() => {
    running += 1;
    try {
      for (const reaction of reactions) {
        const runner = RUNNERS[reaction.then.action_id];
        if (runner !== undefined && permitted(reaction.then.action_id) && holds(reaction, fired.facts)) {
          if (reaction.then.action_id === "ui.screen.open") {
            openedByReaction = reaction.then.params?.screen_id ?? "";
          }
          sent(runner(reaction.then.params ?? {}, fired));
        }
      }
    } finally {
      running -= 1;
    }
  });
}

/**
 * Whether a package screen's button may run `actionId` on this till: an act a reaction may run
 * (`RUNNERS`), which the signed-in person holds outright. Any other draws the button disabled.
 */
export function buttonRuns(actionId: string): boolean {
  return Object.hasOwn(RUNNERS, actionId) && permitted(actionId);
}

/**
 * Runs a package button's act on its tap, through the runners reactions use: the parameters the
 * package fixes, over the ids the screen supplies (its `order_id` and `table_id`). Answers what
 * the runner answered, so the button can show a print on its way and what came of it; `false`
 * when the act is not one `buttonRuns` offers.
 */
export function runButton(
  binding: { readonly action_id: string; readonly params?: Readonly<Record<string, unknown>> },
  supplied: Readonly<Record<string, string>>,
): Ran {
  const runner = RUNNERS[binding.action_id];
  if (runner === undefined || !buttonRuns(binding.action_id)) {
    return false;
  }
  const fixed = Object.entries(binding.params ?? {}).filter((entry): entry is [string, string] => typeof entry[1] === "string");
  const params = { ...supplied, ...Object.fromEntries(fixed) };
  return runner(params, { tableId: params.table_id, orderId: params.order_id, billId: params.bill_id, channel: "", facts: {} });
}
