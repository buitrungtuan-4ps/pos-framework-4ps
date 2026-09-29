// What a print outcome tells the operator (ADR-0100, ADR-0112).
//
// One reading for every document the edge prints — a receipt, a test page, a pre-bill, a shift
// report — because a printer fails the same ways whichever piece of paper it was asked for. Only
// two answers name the document: the success, which says what came out, and the printer that did
// not answer, which says what is safe (a receipt's sale is saved; a pre-bill can simply be asked
// for again).

import type { MessageKey } from "../i18n";

export function printOutcomeKey(
  outcome: string,
  printed: MessageKey,
  unanswered: MessageKey = "print.unanswered",
): MessageKey {
  switch (outcome) {
    case "PRINTED":
      return printed;
    case "NO_PRINTER":
      return "pay.print_no_printer";
    case "UNPRINTABLE_TEXT":
      return "pay.print_unprintable";
    // A printer whose transport belongs to another device (ADR-0112). Three answers rather than
    // one, because they send an operator to three different places: wait, go and look at the
    // terminal, or go and look at the printer.
    case "QUEUED_TO_AGENT":
      return "pay.print_queued";
    case "PRINT_AGENT_UNAVAILABLE":
      return "pay.print_agent_unavailable";
    case "PRINT_QUEUE_FULL":
      return "pay.print_queue_full";
    // `PRINTER_UNAVAILABLE`, and any token newer than this app: the paper did not come out.
    default:
      return unanswered;
  }
}
