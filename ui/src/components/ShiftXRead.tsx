// The X at the till (ADR-0187 decisions 4 and 5): a read that resets nothing, for a person whose own
// role sees takings or reads an X, with its paper; and, once every drawer of the day has closed, the
// store's end of day, "All tills closed for the day", with the store's figures and their paper.
//
// The figures are the edge's. The expected cash is there only where the edge sent it: a blind-close
// store sends it only to a holder of `reports.shift.read_x`, and nothing here works it out.

import { For, Show, createEffect, createSignal, on } from "solid-js";

import { api } from "../api/client";
import type { StoreXResponse, XFigures } from "../api/schemas";
import { t, type MessageKey } from "../i18n";
import { errorMessage } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";
import { holdsOwn } from "../state/permissions";
import { formatAmount, state } from "../state/store";
import type { Money } from "../lib/money";
import { channelName, groupChannels, orderTypeName, tenderRows, type ChannelLine } from "../lib/x-channels";
import { createPrint } from "../state/print-writes";

/** Whether the person signed in reads an X: by their own role, as the edge asks. */
export function readsX(): boolean {
  return holdsOwn("reports.takings.view") || holdsOwn("reports.shift.read_x");
}

/** An X's figures, as the till lists them; a movement that moved nothing is left out. */
export function XFiguresList(props: { figures: XFigures }) {
  const rows = (): readonly (readonly [string, Money])[] => {
    const figures = props.figures;
    const moved: (readonly [string, Money])[] = [
      ...tenderRows(figures),
      [t("x.discounts"), figures.discount_amount],
      [t("x.voided"), figures.voided_cash_amount],
      [t("x.paid_in"), figures.paid_in_amount],
      [t("x.paid_out"), figures.paid_out_amount],
      [t("x.dropped"), figures.dropped_amount],
    ];
    return [[t("x.sales"), figures.sales_amount], ...moved.filter(([, amount]) => amount.amount_minor !== 0)];
  };
  const grouped = (heading: MessageKey, label: (line: ChannelLine) => string) => ({
    heading,
    rows: groupChannels(props.figures.channels, label),
  });
  const sections = () => [
    grouped(
      "x.channel_and_type",
      (line) => `${channelName(line)} · ${orderTypeName(line.order_type, line.order_type_names)}`,
    ),
    grouped("x.by_channel", channelName),
    grouped("x.by_order_type", (line) => orderTypeName(line.order_type, line.order_type_names)),
  ];
  return (
    <dl class="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm tabular-nums" data-outcome="x-figures">
      <dt class="text-ink-muted">{t("x.bills")}</dt>
      <dd>{props.figures.bill_count}</dd>
      <For each={rows()}>
        {([label, amount]) => (
          <>
            <dt class="text-ink-muted">{label}</dt>
            <dd>{formatAmount(amount)}</dd>
          </>
        )}
      </For>
      <Show when={props.figures.expected_amount}>
        {(expected) => (
          <>
            <dt class="font-semibold">{t("x.expected")}</dt>
            <dd class="font-semibold" data-outcome="x-expected">
              {formatAmount(expected())}
            </dd>
          </>
        )}
      </Show>
      <Show when={props.figures.channels.length > 0}>
        <For each={sections()}>
          {(section) => (
            <>
              <dt class="col-span-2 mt-2 font-semibold" data-outcome={section.heading}>
                {t(section.heading)}
              </dt>
              <For each={section.rows}>
                {([label, bills, sales]) => (
                  <>
                    <dt class="text-ink-muted">{`${label} (${String(bills)})`}</dt>
                    <dd>{formatAmount(sales)}</dd>
                  </>
                )}
              </For>
            </>
          )}
        </For>
      </Show>
    </dl>
  );
}

/** Prints an X, the shift's or the store's, and says what came of it. */
function PrintX(props: { shiftId?: string }) {
  const [outcome, setOutcome] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const printer = createPrint(setError);
  const busy = printer.printing;
  const print = () =>
    printer.run("reports.shift.print_x", () => api.printX(props.shiftId), (printed) => setOutcome(printed.x_print));
  return (
    <>
      <button
        type="button"
        class="mt-3 min-h-touch w-full rounded-token border border-line font-semibold disabled:opacity-50"
        disabled={busy()}
        data-step="printX"
        onClick={() => void print()}
      >
        {t(busy() ? "common.sending" : "x.print")}
      </button>
      <Show when={outcome()}>
        {(done) => (
          <p class="mt-1 text-sm text-ink-muted" role="status" data-outcome="x-printed">
            {t(printOutcomeKey(done(), "x.printed"))}
          </p>
        )}
      </Show>
      <Show when={error()}>{(message) => <p class="mt-1 text-sm text-danger">{message()}</p>}</Show>
    </>
  );
}

/** The "X" button: this till's shift's X while it is open, else the store's. */
export function ShiftXButton() {
  const [read, setRead] = createSignal<{ figures: XFigures; shiftId?: string } | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const open = async () => {
    setError(null);
    try {
      const shift = state.shift;
      if (shift !== null && shift.state !== "SHIFT_STATE_CLOSED") {
        const x = await api.shiftX(shift.shiftId);
        setRead({ figures: x.figures, shiftId: x.shift_id });
      } else {
        const store = await api.storeX();
        setRead(store.totals === undefined ? null : { figures: store.totals });
      }
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };
  return (
    <Show when={readsX()}>
      <div class="mt-4" data-outcome="x-read">
        <Show
          when={read()}
          fallback={
            <button
              type="button"
              class="min-h-touch w-full rounded-token border border-line font-semibold"
              data-step="readX"
              onClick={() => void open()}
            >
              {t("x.read")}
            </button>
          }
        >
          {(x) => (
            <div class="rounded-token border border-line bg-surface p-3">
              <h2 class="font-semibold">{t("x.title")}</h2>
              <XFiguresList figures={x().figures} />
              <PrintX shiftId={x().shiftId} />
              <button
                type="button"
                class="mt-2 min-h-touch rounded-token border border-line px-3 text-sm"
                onClick={() => setRead(null)}
              >
                {t("common.cancel")}
              </button>
            </div>
          )}
        </Show>
        <Show when={error()}>{(message) => <p class="mt-1 text-sm text-danger">{message()}</p>}</Show>
      </div>
    </Show>
  );
}

/**
 * "All tills closed for the day", with the store's figures and their paper, once the store's X says
 * every drawer of the business date has closed. Read again whenever this till's shift or the drawer
 * list changes. Nothing for a person who reads no X.
 */
export function DayClosed() {
  const [day, setDay] = createSignal<StoreXResponse | null>(null);
  createEffect(
    on(
      () => [state.shift?.state, state.drawers] as const,
      () => {
        if (!readsX()) {
          setDay(null);
          return;
        }
        api.storeX().then(setDay, () => setDay(null));
      },
    ),
  );
  return (
    <Show when={day()?.all_closed === true && day()?.totals}>
      {(totals) => (
        <div class="mt-4 rounded-token border border-line bg-surface p-3" data-outcome="all-tills-closed">
          <p class="font-semibold">{t("x.all_closed")}</p>
          <XFiguresList figures={totals()} />
          <PrintX />
        </div>
      )}
    </Show>
  );
}
