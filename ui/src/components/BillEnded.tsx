// The pay screen's bill was settled or voided on another till (`../state/closing`). Said at once,
// with the way back, rather than left on screen until the next tap fails against a closed bill.
//
// Paid elsewhere, the table is done and waits for cleaning, so the way back is the floor. Voided
// elsewhere, the order is still on the table to be charged again, so it is the order.

import { useNavigate } from "@solidjs/router";

import { t } from "../i18n";

export function BillEnded(props: { how: "settled" | "voided"; tableId: string }) {
  const navigate = useNavigate();
  const settled = () => props.how === "settled";
  return (
    <div class="mt-6 rounded-token border border-line bg-surface p-4" role="alert" data-outcome="bill-ended-elsewhere">
      <p class="text-lg font-semibold">{t(settled() ? "pay.settled_elsewhere" : "pay.voided_elsewhere")}</p>
      <p class="mt-1 text-sm text-ink-muted">
        {t(settled() ? "pay.settled_elsewhere_hint" : "pay.voided_elsewhere_hint")}
      </p>
      <button
        type="button"
        class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
        data-step="leaveEndedBill"
        onClick={() => navigate(settled() ? "/" : `/table/${props.tableId}`)}
      >
        {t(settled() ? "common.back_floor" : "common.back_order")}
      </button>
    </div>
  );
}
