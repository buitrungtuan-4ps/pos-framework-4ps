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
}

/** The card terminal's half of a pay screen: taking a card, and the park it may leave. */
export function useCardTerminal(options: CardTerminalOptions): CardTerminal {
  const [busy, setBusy] = createSignal(false);
  const [parked, setParked] = createSignal<string | null>(null);
  const [note, setNote] = createSignal<string | null>(null);

  // The park is the edge's: read it whenever the bill on screen changes, so a reload shows it.
  createEffect(
    on(options.billId, (billId) => {
      setParked(null);
      setNote(null);
      if (billId !== "") {
        api.parkedCard(billId).then(
          (card) => setParked(card.payment_id),
          () => undefined,
        );
      }
    }),
  );

  const answered = (answer: CardResponse) => {
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

  return { busy, parked, note, take, resolve };
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
