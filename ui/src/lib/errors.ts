// What a screen tells the operator when the edge refuses or fails a command (ADR-0137).
//
// Every screen used to show the edge's own sentence — `caught.message` — which is English written
// for a log: a Vietnamese cashier read "the store is unavailable" or "the command was refused:
// capability disabled: seats_enabled" with no idea what to do next. The edge now names each refusal
// with a stable token (`pos-error-reason`), and this is the one place the till turns a token into a
// sentence in the operator's language, with a next step where there is one.
//
// A token with no entry here, or an edge too old to send one, falls back to the edge's sentence:
// English is better than nothing, and the entry can be added without a change on the edge.

import { ApiError } from "../api/client";
import { type MessageKey, t } from "../i18n";

// Token → catalogue key. Several tokens share a sentence where the operator's next step is the same.
const REASONS: Readonly<Record<string, MessageKey>> = {
  STORE_UNAVAILABLE: "error.store_unavailable",
  INTERNAL: "error.internal",
  INVALID_ARGUMENT: "error.invalid_argument",
  SHIFT_ALREADY_OPEN: "error.shift_already_open",
  UNKNOWN_SHIFT: "error.unknown_shift",
  NO_OPEN_ORDER: "error.no_open_order",
  UNKNOWN_LINE: "error.unknown_record",
  UNKNOWN_ORDER: "error.unknown_record",
  UNKNOWN_BILL: "error.unknown_record",
  BILL_ALREADY_OPEN: "error.bill_already_open",
  BILLS_ON_DIFFERENT_TABLES: "error.bills_on_different_tables",
  UNROUTABLE_LINE: "error.unroutable_line",
  AWAITING_STAFF_CONFIRMATION: "error.awaiting_staff_confirmation",
  NOT_AWAITING_STAFF_CONFIRMATION: "error.not_awaiting_staff_confirmation",
  ORDER_REJECTED: "error.order_rejected",
  ALREADY_FIRED: "error.already_fired",
  REASON_CODE_NOT_VALID: "error.reason_not_valid",
  VOID_REASON_NOT_VALID: "error.reason_not_valid",
  MODIFIER_SELECTION_INVALID: "error.modifier_selection_invalid",
  APPROVAL_REQUIRED: "error.approval_required",
  APPROVAL_REFUSED: "error.approval_refused",
  SUPERSEDED: "error.superseded",
  PERMISSION_DENIED: "error.permission_denied",
  CAPABILITY_DISABLED: "error.capability_disabled",
  PAYMENTS_DO_NOT_SUM_TO_TOTAL: "error.payments_do_not_sum_to_total",
  NEGATIVE_CHANGE: "error.negative_change",
  REDUCTION_EXCEEDS_BILL: "error.reduction_exceeds_bill",
  SPLIT_NOT_A_PARTITION: "error.split_not_a_partition",
  TAX_RATE_NOT_CONFIGURED: "error.tax_rate_not_configured",
  TRANSITION_REFUSED: "error.transition_refused",
  EMPTY: "error.empty",
  // `/setup`: the first screen a technician meets, and the one a store with no internet fails on.
  ACTIVATION_REFUSED: "error.activation_refused",
  ACTIVATION_CODE_MALFORMED: "error.activation_code_malformed",
  ALREADY_ACTIVATED: "error.already_activated",
  ACTIVATION_WRONG_STATE: "error.activation_wrong_state",
  ACTIVATION_UNAVAILABLE: "error.activation_unavailable",
};

// The sentence for anything a command threw: a translated refusal, the edge's own words for a
// refusal the till has no sentence for, or "the store did not respond" when there was no answer.
export function errorMessage(caught: unknown): string {
  if (!(caught instanceof ApiError)) {
    return t("common.store_error");
  }
  const key = caught.reason === null ? undefined : REASONS[caught.reason];
  return key === undefined ? caught.message : t(key);
}
