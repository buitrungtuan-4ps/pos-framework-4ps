// Opening a table's bill for the pay screen, with the figure it owes (roadmap-v3 E5).
//
// Take payment used to open the bill and only then move to the pay screen, which then read the
// bill's figure: two answers in a row before the cashier saw anything, on a screen that looked
// untouched meanwhile. The till now moves at once, and the pay screen opens the bill and reads the
// table's check side by side. A bill opened here covers the whole table, so the table's figure is
// the bill's, and the tenders wait for the slower of the two answers rather than their sum.
//
// A table that already had a bill open — the open parts of a split one among them — keeps no such
// shortcut: its check is every part together, a figure that would settle nobody's bill, so `check`
// is then null and the screen waits for the bill's own read, as before.

import type { CheckResponse } from "../api/schemas";
import { openBill } from "./floor-writes";
import { loadCheck, loadLiveOrders, openBillFor } from "./store";

export async function openForPay(
  tableId: string,
): Promise<{ billId: string; check: CheckResponse | null }> {
  const figure = loadCheck(tableId).catch(() => null);
  // What is open is read first: asking for another bill is refused while one is open.
  await loadLiveOrders();
  const fresh = openBillFor(tableId) === undefined;
  const billId = await openBill(tableId);
  return { billId, check: fresh ? await figure : null };
}
