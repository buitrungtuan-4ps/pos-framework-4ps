// The till's prints, as edge writes with nothing to read back (ADR-0196 decision 1, ADR-0204
// decision 5): a pre-bill, a copy of a receipt, an X and a handover slip again. The button says it is
// printing from the frame the tap lands in, and takes no second tap until the edge answers, while
// every other act, payment included, goes on taking taps. The edge's answer says what came of the
// paper; a refusal says why with its own sentence.
//
// No answer is neither rolled back nor sent again: no read can show whether the paper came out, so
// the till says the print is not confirmed, and the operator, who can see the printer, decides
// whether to ask for it again.

import { createSignal, type Accessor } from "solid-js";

import { ApiError } from "../api/client";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { editWrite, type EdgeWriteId } from "./edge-write";

/** The catalogue's prints. */
export type PrintId = Extract<
  EdgeWriteId,
  "billing.check.print" | "billing.receipt.reprint" | "reports.shift.print_x" | "reports.shift.print_handover"
>;

/** What the operator is told of a print that failed: the refusal, or that it is not confirmed. */
export function printFailure(caught: unknown): string {
  return caught instanceof ApiError ? errorMessage(caught) : t("print.unconfirmed");
}

/**
 * Sends one print through the edge-write path: nothing to paint, nothing to read again, and payment
 * never waits for it.
 */
export function printWrite<T>(action: PrintId, send: () => Promise<T>): Promise<T> {
  return editWrite(action, { send, reread: () => undefined, holdsPayment: false });
}

export interface Print {
  /** Whether a print is on its way, for the button's *Sending…*. */
  readonly printing: Accessor<boolean>;
  /** Sends `send` as `action`, unless a print is already out, and hands the answer to `answered`. */
  readonly run: <T>(action: PrintId, send: () => Promise<T>, answered: (answer: T) => void) => Promise<void>;
}

/** One print at a time on a screen; `report` is its error line, cleared on the tap. */
export function createPrint(report: (message: string | null) => void): Print {
  const [printing, setPrinting] = createSignal(false);
  return {
    printing,
    run: async (action, send, answered) => {
      if (printing()) {
        return;
      }
      setPrinting(true);
      report(null);
      try {
        answered(await printWrite(action, send));
      } catch (caught) {
        report(printFailure(caught));
      } finally {
        setPrinting(false);
      }
    },
  };
}
