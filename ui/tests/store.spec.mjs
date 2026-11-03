// The till's store, folded in Node with no browser and no edge: what the store keeps, and for how
// long. Loaded through Vite, the same transform the build uses, so the module under test is the one
// the till runs. Each test gets a fresh copy, so no state leaks from one to the next.

import { expect, test } from "@playwright/test";
import { createServer } from "vite";

/** A fresh copy of `src/state/store.ts`, and a way to let it go. */
async function freshStore() {
  const server = await createServer({
    root: new URL("..", import.meta.url).pathname,
    server: { middlewareMode: true, hmr: false },
    appType: "custom",
    logLevel: "error",
  });
  const store = await server.ssrLoadModule("/src/state/store.ts");
  return { store, close: () => server.close() };
}

const VND = (amount_minor) => ({ amount_minor, currency_code: "VND" });

/** A line added to `order`, as the fan-out carries it. */
function lineAdded(order, line) {
  return {
    eventType: "sales.order_line.added",
    payload: {
      order_line_id: line,
      order_id: order,
      menu_item_id: "menu-1",
      display_name: "Margherita",
      line_total: VND(100_000),
      quantity: { milli: 1000 },
    },
  };
}

const ordersOf = (store) => new Set(Object.values(store.state.lines).map((line) => line.orderId));

// `state.lines` only ever grew: every dish of the day stayed in memory for as long as the till was
// open (AGENTS.md: every in-memory structure has a limit). An order off every screen is forgotten.
test("the lines of a settled table's bill are gone once the settle is folded", async () => {
  const { store, close } = await freshStore();
  try {
    store.fold({ eventType: "sales.table.opened", payload: { table_id: "t1", order_id: "o1" } });
    store.fold(lineAdded("o1", "l1"));
    store.fold(lineAdded("o1", "l2"));
    store.fold({ eventType: "sales.table.opened", payload: { table_id: "t2", order_id: "o2" } });
    store.fold(lineAdded("o2", "l3"));
    store.fold({ eventType: "billing.bill.opened", payload: { bill_id: "b1", order_id: "o1" } });
    expect(ordersOf(store)).toEqual(new Set(["o1", "o2"]));

    store.fold({ eventType: "billing.bill.settled", payload: { bill_id: "b1" } });
    expect(ordersOf(store)).toEqual(new Set(["o2"]));
    expect(store.linesForTable("t2")).toHaveLength(1);
  } finally {
    await close();
  }
});

test("a split table keeps its lines until its last part is paid", async () => {
  const { store, close } = await freshStore();
  try {
    store.fold({ eventType: "sales.table.opened", payload: { table_id: "t1", order_id: "o1" } });
    store.fold(lineAdded("o1", "l1"));
    store.fold({ eventType: "billing.bill.opened", payload: { bill_id: "b1", order_id: "o1" } });
    store.fold({ eventType: "billing.bill.opened", payload: { bill_id: "b2", order_id: "o1" } });
    store.fold({ eventType: "billing.bill.settled", payload: { bill_id: "b1" } });
    expect(ordersOf(store)).toEqual(new Set(["o1"]));
    store.fold({ eventType: "billing.bill.settled", payload: { bill_id: "b2" } });
    expect(ordersOf(store)).toEqual(new Set());
  } finally {
    await close();
  }
});

test("a read of the live orders forgets the lines of every order that has ended", async () => {
  const { store, close } = await freshStore();
  try {
    // A paid walk-in the kitchen has bumped, and a table still being served.
    store.fold(lineAdded("walk-in", "l1"));
    store.fold({ eventType: "sales.table.opened", payload: { table_id: "t1", order_id: "o1" } });
    store.fold(lineAdded("o1", "l2"));
    const served = {
      order_id: "o1",
      table_id: "t1",
      lines: [
        {
          order_line_id: "l2",
          display_name: "Margherita",
          quantity: { milli: 1000 },
          line_total: VND(100_000),
          state: "ORDER_LINE_STATE_ADDED",
        },
      ],
    };
    store.applyLiveOrders({ orders: [served], kitchen: [], opensBefore: 0 });
    expect(ordersOf(store)).toEqual(new Set(["o1"]));
  } finally {
    await close();
  }
});
