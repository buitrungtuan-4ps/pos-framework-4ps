// Taking money off a bill before it settles (roadmap B2.2): the act and its panel, for any screen
// that takes payment on a bill.
//
// Three taps from the order screen — pay, discount, reason — which is §6's ceiling for a rare action
// and the same shape the bill void has. The amount and the manager's badge are **typed**, and typing
// is not a tap, exactly as the shift float and the void's PIN are not.
//
// The manager is not this screen's rule. `billing.discount.apply` is granted to a server and carries
// no PIN flag — but no store publishes the discount ceiling that permission's own description refers
// to, so the edge reads the ceiling as zero and answers `403` naming
// `billing.discount.override_ceiling`. The fields are here because that is the answer today; when a
// store publishes a ceiling, a small discount will start going through and this panel will not have
// to change. Where the store enforces each person's own set, the fields are offered only to a person
// who holds the override with approval, and the button only to one who holds
// `billing.discount.apply`, which every discount needs whatever its size.

import { For, Show, createSignal, type Accessor } from "solid-js";

import type { CheckResponse } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { money } from "../lib/money";
import { asksApprover, permissionsEnforced } from "../state/permissions";
import { useDiscountAct, discountRefusal } from "../state/reduction-acts";
import { reasonsFor } from "../state/store";
import { PendingSettle } from "./PendingSettle";
import { RefusalPanel } from "./RefusalPanel";
import { ApproverFields, type Approver } from "./ApproverFields";
import { Keypad } from "./Keypad";

// The action a discount cites. Its own action, like the void's: a store may publish "Goodwill" for
// knocking money off and not for cancelling a bill, and the edge refuses a reason published for the
// wrong one — so a picker that offered both would offer a refusal.
export const DISCOUNT = "REASON_ACTION_DISCOUNT";

/** A screen's discount: whether its panel is open, what is typed, and the two taps that make it. */
export function useDiscount(options: {
  billId: Accessor<string | null>;
  currency: Accessor<string>;
  approver: Approver;
  /** The screen's error line: cleared on each tap, and given a refusal's sentence. */
  report: (message: string | null) => void;
  /** The bill as the edge re-priced it. */
  onApplied: (totals: CheckResponse) => void;
  /** What the bill comes to, where the screen knows it, for the till's own check (ADR-0196). */
  due?: Accessor<number | null>;
}) {
  const [open, setOpen] = createSignal(false);
  // What comes off, as typed. A string, not a number, because a half-typed "1" must stay "1" rather
  // than becoming a figure the screen then reformats under the operator's fingers.
  const [text, setText] = createSignal("");

  // Minor units, or null while what has been typed is not a number the till can send. Integer the
  // whole way (ADR-0028): the field takes digits and nothing else, so there is no decimal to lose.
  const amount = () => {
    const typed = text().trim();
    if (typed === "" || !/^[0-9]+$/.test(typed)) {
      return null;
    }
    const value = Number(typed);
    return value > 0 ? value : null;
  };

  // The reason buttons stay disabled until there is an amount and a manager, for the reason the
  // void's are: a button that can only produce a refusal teaches an operator the till is unreliable.
  //
  // Where the store enforces each person's own set, a person holding the override with approval is
  // offered the manager's fields rather than made to fill them: the till does not know their
  // ceiling, so a discount within it goes through alone, and one above it is refused until a
  // manager approves (ADR-0158). One half-typed is waited for, never dropped.
  const asks = () => asksApprover("billing.discount.override_ceiling");
  const optional = () => asks() && permissionsEnforced();
  const ready = () =>
    amount() !== null &&
    (options.approver.typed() ||
      !asks() ||
      (optional() && options.approver.code().trim() === "" && options.approver.pin() === ""));

  const closeDiscount = () => {
    setOpen(false);
    setText("");
    options.approver.clear();
  };

  const askDiscount = () => {
    options.report(null);
    setOpen(true);
  };

  // The second tap: apply it, citing this reason and the manager, as a money act (ADR-0196,
  // ADR-0204): pending until the edge answers, refused at once where the till can tell. The edge
  // answers with the whole re-priced bill and the screen takes that figure — it never subtracts the
  // discount itself, because the tax follows the reduced base and a second opinion about it is a
  // second price. Above the ceiling with no manager typed, the edge names the override that was
  // missing: for a person who holds it with approval, that is a manager to fetch, not a refusal.
  let sentWithout = false;
  const act = useDiscountAct(
    (totals) => {
      options.onApplied(totals);
      closeDiscount();
    },
    (caught) =>
      caught.reason === "PERMISSION_DENIED" && optional() && sentWithout
        ? t("error.approval_required")
        : errorMessage(caught),
  );
  const discountReason = (reasonCodeId: string) => {
    const id = options.billId();
    const value = amount();
    if (id === null || value === null) {
      return;
    }
    options.report(null);
    const approver = asks() ? options.approver.approval() : undefined;
    sentWithout = approver === undefined;
    act.send(id, money(options.currency(), value), reasonCodeId, approver, () =>
      discountRefusal({
        amount: value,
        due: options.due?.() ?? null,
        needsApprover: asks() && !optional(),
        approval: approver,
      }),
    );
  };

  return { open, text, setText, asks, optional, ready, closeDiscount, askDiscount, discountReason, act };
}

/**
 * The open discount panel: the amount on its keypad, the manager's fields where asked, the store's
 * reasons, and Cancel. A reason calls `onSubmit` with its id; `data-step` marks the reason buttons
 * for the step gate.
 */
export function DiscountPanel(props: {
  state: ReturnType<typeof useDiscount>;
  approver: Approver;
  onSubmit: (reasonCodeId: string) => void;
  "data-step"?: string;
}) {
  return (
    <div class="mt-6 rounded-token border border-line bg-surface p-3">
      <h2 class="font-semibold">{t("pay.discount_title")}</h2>
      <label class="mt-2 block text-sm">
        {t("pay.discount_amount")}
        <input
          id="discount-amount"
          type="text"
          inputmode="numeric"
          class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2 tabular-nums"
          value={props.state.text()}
          onInput={(event) => props.state.setText(event.currentTarget.value)}
        />
      </label>
      <Keypad value={props.state.text()} onChange={props.state.setText} data-step="discountKeypad" />
      <Show when={props.state.asks()}>
        <p class="mt-2 text-sm text-ink-muted">
          {t(props.state.optional() ? "pay.discount_over_limit" : "pay.discount_manager")}
        </p>
        <ApproverFields
          id="discount-approver"
          code={props.approver.code()}
          pin={props.approver.pin()}
          onCode={props.approver.setCode}
          onPin={props.approver.setPin}
          codeLabel={t("pay.approver_code")}
          pinLabel={t("pay.approver_pin")}
        />
      </Show>
      <p class="mt-3 text-sm text-ink-muted">{t("pay.discount_reason")}</p>
      <div class="mt-2 grid grid-cols-2 gap-2">
        <For each={reasonsFor(DISCOUNT)}>
          {(reason) => (
            <button
              type="button"
              class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
              disabled={!props.state.ready() || props.state.act.phase() !== "idle" || props.state.act.refusal() !== null}
              data-step={props["data-step"]}
              onClick={() => props.onSubmit(reason.reason_code_id)}
            >
              {reason.display_name}
            </button>
          )}
        </For>
      </div>
      <Show when={props.state.act.phase() !== "idle"}>
        <PendingSettle act="discount" phase={props.state.act.phase()} amount="" onLeave={() => props.state.closeDiscount()} />
      </Show>
      <RefusalPanel message={props.state.act.refusal()} onDismiss={props.state.act.dismiss} />
      <button
        type="button"
        class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
        onClick={() => props.state.closeDiscount()}
      >
        {t("common.cancel")}
      </button>
    </div>
  );
}
