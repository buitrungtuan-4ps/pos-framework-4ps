// Each drawer's cash under the store's on the X/Z report (ADR-0167 decision 13). A drawer handed
// over at the till (ADR-0187) is one row whose cash the cloud counts once, with its handover
// difference; a drawer whose shifts name no handover, as every one before ADR-0187, lists each shift
// on its own, because nothing says whether a later float was new cash or the last shift's carried on.

import { For, Show } from "solid-js";

import { STORE_DRAWER } from "../../api/types";
import { t } from "../../i18n";
import { formatAmount } from "../../state/money";
import type { CashTotals, ShiftCash, XzReport } from "../../api/schemas";

/** One row of the table: a drawer, or one of its shifts. */
interface DrawerRow {
  readonly key: string;
  /** The till's name, `null` for the store's drawer and for a till the devices no longer list. */
  readonly name: string | null;
  /** The shift's place in the drawer's day, from 1, on a row that is one shift. */
  readonly shift?: number;
  readonly figures: CashTotals;
}

/** A shift's own figures, as a drawer's are read. Its voided cash is the drawer's alone. */
function shiftFigures(shift: ShiftCash): CashTotals {
  return {
    opening_float: shift.opening_float,
    paid_in: shift.paid_in,
    paid_out: shift.paid_out,
    dropped: shift.dropped ?? 0,
    shifts_opened: 1,
    shifts_closed: shift.closed ? 1 : 0,
    expected: shift.expected,
    counted: shift.counted,
    variance: shift.variance,
    closed_float: shift.closed ? shift.opening_float : 0,
  };
}

/** The shifts of `key`'s drawer on the day, oldest first, where they are not one handover chain. */
function unlinkedShifts(report: XzReport, key: string): ShiftCash[] {
  const shifts = Object.entries(report.cash.by_shift ?? {})
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([, shift]) => shift)
    .filter((shift) => shift.drawer === key);
  const fresh = shifts.filter((shift) => shift.handed_over !== true).length;
  return fresh > 1 ? shifts : [];
}

/**
 * The day's rows: the store's drawer first, then each till by name, then each till no longer listed
 * by its id, a drawer of unlinked shifts as one row per shift. None where the day kept only the
 * store's drawer in one chain of shifts, so that day reads as it always did.
 */
export function drawerRows(report: XzReport): DrawerRow[] {
  const rank = (row: DrawerRow) => (row.key === STORE_DRAWER ? 0 : row.name === null ? 2 : 1);
  const drawers = Object.entries(report.cash.by_drawer)
    .map(([key, figures]) => ({ key, name: report.till_names[key] ?? null, figures }))
    .sort((a, b) => rank(a) - rank(b) || (a.name ?? a.key).localeCompare(b.name ?? b.key));
  const rows = drawers.flatMap((drawer): DrawerRow[] => {
    const shifts = unlinkedShifts(report, drawer.key);
    if (shifts.length === 0) {
      return [drawer];
    }
    return shifts.map((shift, index) => ({ ...drawer, shift: index + 1, figures: shiftFigures(shift) }));
  });
  return rows.every((row) => row.key === STORE_DRAWER && row.shift === undefined) ? [] : rows;
}

/** Whether the day had a handover the cloud can read, so the table shows its difference. */
function handedOver(report: XzReport): boolean {
  return Object.values(report.cash.by_shift ?? {}).some((shift) => shift.handed_over === true);
}

export function DrawerTable(props: { report: XzReport }) {
  const cash = (minor: number) =>
    formatAmount({ amount_minor: minor, currency_code: props.report.cash.currency_code || "" });
  const rows = () => drawerRows(props.report);
  const split = () => rows().some((row) => row.shift !== undefined);
  return (
    <Show when={rows().length > 0}>
      <div class="mt-4 overflow-x-auto">
        <table class="w-full text-left text-sm">
          <caption class="mb-2 text-left font-medium text-ink">{t("reports.drawersTitle")}</caption>
          <thead>
            <tr class="border-b border-line text-ink-muted">
              <th class="py-2 pr-4 font-medium">{t("reports.drawerTill")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerFloat")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerPaidIn")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerPaidOut")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerDropped")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerClosedFloat")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerExpected")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerCounted")}</th>
              <th class="py-2 pr-4 font-medium">{t("reports.drawerVoided")}</th>
              <Show when={handedOver(props.report)}>
                <th class="py-2 pr-4 font-medium">{t("reports.drawerHandover")}</th>
              </Show>
              <th class="py-2 font-medium">{t("reports.drawerOverShort")}</th>
            </tr>
          </thead>
          <tbody>
            <For each={rows()}>
              {(drawer) => {
                // Only a close says what a drawer should have held: with none closed there is
                // nothing to show, and nothing here works one out.
                const closed = (minor: number) =>
                  drawer.figures.shifts_closed > 0 ? cash(minor) : "—";
                return (
                  <tr class="border-b border-line text-ink">
                    <td class="py-2 pr-4">
                      <Show when={drawer.key !== STORE_DRAWER} fallback={t("reports.drawerStore")}>
                        {drawer.name ?? t("reports.drawerUnlisted")}
                        <Show when={drawer.name === null}>
                          <span class="block font-mono text-xs text-ink-muted">{drawer.key}</span>
                        </Show>
                      </Show>
                      <Show when={drawer.shift}>
                        {(shift) => (
                          <span class="block text-xs text-ink-muted">
                            {t("reports.drawerShift", { number: shift() })}
                          </span>
                        )}
                      </Show>
                    </td>
                    <td class="py-2 pr-4">{cash(drawer.figures.opening_float)}</td>
                    <td class="py-2 pr-4">{cash(drawer.figures.paid_in)}</td>
                    <td class="py-2 pr-4">{cash(drawer.figures.paid_out)}</td>
                    <td class="py-2 pr-4">{cash(drawer.figures.dropped ?? 0)}</td>
                    <td class="py-2 pr-4">{closed(drawer.figures.closed_float ?? 0)}</td>
                    <td class="py-2 pr-4">{closed(drawer.figures.expected)}</td>
                    <td class="py-2 pr-4">{closed(drawer.figures.counted)}</td>
                    <td class="py-2 pr-4">
                      {drawer.shift === undefined ? cash(drawer.figures.voided_cash ?? 0) : "—"}
                    </td>
                    <Show when={handedOver(props.report)}>
                      <td class="py-2 pr-4">{cash(drawer.figures.handover_difference ?? 0)}</td>
                    </Show>
                    <td class="py-2 font-medium">{closed(drawer.figures.variance)}</td>
                  </tr>
                );
              }}
            </For>
          </tbody>
        </table>
      </div>
      <p class="mt-2 text-sm text-ink-muted">{t("reports.drawersHint")}</p>
      <Show when={handedOver(props.report)}>
        <p class="mt-1 text-sm text-ink-muted">{t("reports.drawersHandoverHint")}</p>
      </Show>
      <Show when={split()}>
        <p class="mt-1 text-sm text-ink-muted">{t("reports.drawersShiftsHint")}</p>
      </Show>
    </Show>
  );
}
