// A manager asks HQ to correct a paid bill whose e-invoice was issued (ADR-0245 route b, ADR-0250).
// Offered where the bill's void was refused because of its invoice: the store's route may send it
// to HQ (`einvoice.invoiced_correction`), and the edge decides whether it does. Nothing is voided and
// no money moves; once HQ approves, the manager voids the bill on the same panel, once. The reason
// and the manager's code and PIN are the ones the void was tried with.

import { createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { ApproverRequest, CorrectionRequestResponse } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { editWrite } from "../state/edge-write";
import { formatAmount } from "../state/store";

/** Sends the request through the edge-write path. Nothing is painted: the answer is the change. */
export function askHqToCorrect(
  billId: string,
  reasonCodeId: string | null,
  approval: ApproverRequest | undefined,
): Promise<CorrectionRequestResponse> {
  return editWrite("fiscal.correction.request", {
    send: () => api.requestCorrection(billId, { reason_code_id: reasonCodeId, ...approval }),
    reread: () => undefined,
  });
}

export function AskHqCorrection(props: {
  billId: string;
  reasonCodeId: string | null;
  approval: () => ApproverRequest | undefined;
}) {
  const [asked, setAsked] = createSignal<CorrectionRequestResponse | null>(null);
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const ask = async () => {
    setBusy(true);
    setRefusal(null);
    try {
      setAsked(await askHqToCorrect(props.billId, props.reasonCodeId, props.approval()));
    } catch (caught) {
      setRefusal(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="mt-3 rounded-token border border-line p-3" data-outcome="ask-hq">
      <Show
        when={asked()}
        fallback={
          <>
            <p class="text-sm text-ink-muted">{t("today.ask_hq_explain")}</p>
            <button
              type="button"
              class="mt-2 min-h-touch w-full rounded-token border border-line bg-surface px-3 disabled:opacity-50"
              disabled={busy()}
              data-step="askHqToCorrect"
              onClick={() => void ask()}
            >
              {t("today.ask_hq")}
            </button>
            <Show when={refusal()}>
              {(message) => (
                <p class="mt-2 text-sm text-danger" role="alert" data-outcome="ask-hq-refused">
                  {message()}
                </p>
              )}
            </Show>
          </>
        }
      >
        {(request) => (
          <p class="text-sm" role="status" data-outcome="asked-hq">
            {t("today.asked_hq", { amount: formatAmount(request().amount) })}
          </p>
        )}
      </Show>
    </div>
  );
}
