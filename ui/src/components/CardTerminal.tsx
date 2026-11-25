// A card through the card terminal (ADR-0230): the till asks the edge, the edge asks the terminal,
// and the till shows what the terminal concluded. A decline says so and charges nothing. A result the
// terminal could not give parks the bill amber with its three exits — ask the terminal again, confirm
// it from the terminal's own slip, or cancel it on the terminal — and the card button gives way to
// them until one answers. A till that reloads reads the park back from the edge. An edge with no
// terminal says so, and the card is recorded by hand as before (`onByHand`).

import { type Accessor, Show, createEffect, createSignal, on } from "solid-js";

import { ApiError, api } from "../api/client";
import type { CardResponse } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { money } from "../lib/money";
import { ApproverFields } from "./ApproverFields";

type Resolution = "LOOK_UP" | "CONFIRM" | "CANCEL";

export interface CardTerminalOptions {
  readonly billId: Accessor<string>;
  readonly currency: Accessor<string>;
  /** What the card applies to the bill, and the tip on it, in minor units. */
  readonly amount: Accessor<number>;
  readonly tip: Accessor<number>;
  readonly ways: Accessor<number | null>;
  /** The terminal took it: the edge's answer, which names the settled bill when it covered it. */
  readonly onCaptured: (answer: CardResponse) => void;
  /** The edge has no terminal: the card is recorded by hand. */
  readonly onByHand: () => void;
}

export interface CardTerminal {
  readonly busy: Accessor<boolean>;
  /** The attempt the bill is parked on, or null. */
  readonly parked: Accessor<string | null>;
  readonly note: Accessor<string | null>;
  readonly take: () => Promise<void>;
  readonly resolve: (action: Resolution) => void;
  /** Whether a confirm by hand waits for a manager's badge and PIN (ADR-0243). */
  readonly needsApprover: Accessor<boolean>;
  /** Confirms by hand with a manager's badge and PIN. */
  readonly confirmWith: (code: string, pin: string) => void;
}

/** The card terminal's half of a pay screen: taking a card, and the park it may leave. */
export function useCardTerminal(options: CardTerminalOptions): CardTerminal {
  const [busy, setBusy] = createSignal(false);
  const [parked, setParked] = createSignal<string | null>(null);
  const [note, setNote] = createSignal<string | null>(null);
  const [needsApprover, setNeedsApprover] = createSignal(false);

  // The park is the edge's: read it whenever the bill on screen changes, so a reload shows it.
  createEffect(
    on(options.billId, (billId) => {
      setParked(null);
      setNote(null);
      setNeedsApprover(false);
      if (billId !== "") {
        api.parkedCard(billId).then(
          (card) => setParked(card.payment_id),
          () => undefined,
        );
      }
    }),
  );

  const answered = (answer: CardResponse) => {
    setNeedsApprover(false);
    if (answer.outcome === "PAYMENT_OUTCOME_CAPTURED") {
      setParked(null);
      options.onCaptured(answer);
    } else if (answer.outcome === "PAYMENT_OUTCOME_DECLINED") {
      setParked(null);
      setNote(t("card.declined"));
    } else {
      setNote(parked() === answer.payment_id ? t("card.still_unknown") : null);
      setParked(answer.payment_id);
    }
  };

  // An act that went unanswered may have reached the terminal: read the park back rather than guess.
  const failed = async (caught: unknown) => {
    if (caught instanceof ApiError) {
      if (caught.reason === "CARD_TERMINAL_NONE") {
        options.onByHand();
      } else if (caught.reason === "APPROVAL_REQUIRED") {
        // The store asks a manager to agree before a card is confirmed by hand (ADR-0243).
        setNeedsApprover(true);
        setNote(t("card.confirm_needs_manager"));
      } else {
        setNote(errorMessage(caught));
      }
      return;
    }
    try {
      setParked((await api.parkedCard(options.billId())).payment_id);
    } catch {
      setNote(t("card.unanswered"));
    }
  };

  const run = async (act: () => Promise<CardResponse>) => {
    setBusy(true);
    setNote(null);
    try {
      answered(await act());
    } catch (caught) {
      await failed(caught);
    } finally {
      setBusy(false);
    }
  };

  const take = () =>
    run(() =>
      api.takeCard(options.billId(), {
        amount: money(options.currency(), options.amount()),
        tip: money(options.currency(), options.tip()),
        split_ways: options.ways() ?? undefined,
      }),
    );

  const resolve = (action: Resolution) => {
    const paymentId = parked();
    if (paymentId !== null) {
      void run(() => api.resolveCard(options.billId(), paymentId, { action }));
    }
  };

  const confirmWith = (code: string, pin: string) => {
    const paymentId = parked();
    if (paymentId !== null) {
      void run(() =>
        api.resolveCard(options.billId(), paymentId, {
          action: "CONFIRM",
          approver_code: code,
          approver_pin: pin,
        }),
      );
    }
  };

  return { busy, parked, note, take, resolve, needsApprover, confirmWith };
}

/** The manager's badge and PIN a confirm by hand waits for (ADR-0243). */
function ConfirmApprover(props: { readonly card: CardTerminal }) {
  const [code, setCode] = createSignal("");
  const [pin, setPin] = createSignal("");
  return (
    <div class="mt-2" data-outcome="card-confirm-approver">
      <ApproverFields
        id="card-confirm-approver"
        code={code()}
        pin={pin()}
        onCode={setCode}
        onPin={setPin}
        codeLabel={t("card.approver_code")}
        pinLabel={t("card.approver_pin")}
      />
      <button
        type="button"
        class="mt-2 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
        disabled={props.card.busy() || code() === "" || pin() === ""}
        data-step="cardConfirmApproved"
        onClick={() => props.card.confirmWith(code(), pin())}
      >
        {t("card.confirm_approved")}
      </button>
    </div>
  );
}

const EXIT = "min-h-touch rounded-token border border-line bg-surface disabled:opacity-50";

/** The parked card's three exits, and what the last card came to. */
export function CardParked(props: { readonly card: CardTerminal }) {
  return (
    <>
      <Show when={props.card.parked() !== null}>
        <div class="rounded-token border-2 border-awaiting bg-surface p-3" role="alert" data-outcome="card-unknown">
          <p class="font-semibold">{t("card.unknown")}</p>
          <div class="mt-2 flex flex-col gap-2">
            <button type="button" class={EXIT} disabled={props.card.busy()} data-step="cardLookUp" onClick={() => props.card.resolve("LOOK_UP")}>
              {t("card.look_up")}
            </button>
            <button type="button" class={EXIT} disabled={props.card.busy()} data-step="cardConfirm" onClick={() => props.card.resolve("CONFIRM")}>
              {t("card.confirm")}
            </button>
            <button type="button" class={EXIT} disabled={props.card.busy()} data-step="cardCancel" onClick={() => props.card.resolve("CANCEL")}>
              {t("card.cancel")}
            </button>
          </div>
          <Show when={props.card.needsApprover()}>
            <ConfirmApprover card={props.card} />
          </Show>
        </div>
      </Show>
      <Show when={props.card.note()}>
        {(text) => (
          <p class="text-sm text-danger" role="status" data-outcome="card-note">
            {text()}
          </p>
        )}
      </Show>
    </>
  );
}
