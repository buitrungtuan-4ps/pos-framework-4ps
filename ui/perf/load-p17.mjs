// The P17 load driver: one edge at peak (`docs/real-hardware-gates.md`, roadmap-v3 PF2).
//
// It pairs `DEVICES` devices through the edge's own pairing (the boot code for the first, then
// codes a signed-in manager mints), holds a `/ws` socket open on each, and has each device sell
// counter orders in a loop for `DURATION_S` seconds: open an order, add a line, fire it, bill it and
// settle it in cash. It records every call's latency and outcome, the frames each socket received,
// and at the end asks the edge's own `/api/sync` for its outbox depth, so the count of sales can be set
// against what the outbox holds.
//
// A Vietnamese store (or any store that asks each walk-in whether it eats in or takes away) refuses a
// counter order that names no channel, and taxes each line by its channel: the driver opens each order
// at `CHANNEL` (default: none, falling back to `SALES_CHANNEL_TAKEAWAY` when the store refuses one
// without), lets the edge price the line at that channel, and pays the check's total, tax included.
// It counts the e-invoices the settles issued (numbered or "number pending"), so a store that issues
// at settle is measured with that work in it.
//
// What it does not drive: the guest QR page and delivery intake, which reach a store through the
// cloud (`/v1/qr/*`, the gateway). Running those at the same time needs a cloud and its own load
// tool; on the NUC run this beside them.
//
// Usage:
//   POS_EDGE_BIN=<minimal-edge> node perf/load-p17.mjs            # starts its own edge
//   EDGE_URL=http://127.0.0.1:8787 PAIRING_CODE=123456 STAFF_CODE=.. STAFF_PIN=.. node perf/load-p17.mjs
// Knobs: DEVICES (default 30), DURATION_S (60), THINK_MS (pause between a device's sales, default
// 0: as fast as the edge answers, the worst case), PROFILE (the demo store, default
// `kitchen-stations`, whose stations take a counter order's lines), REPORT (a JSON report's path).
//
// Pass (P17): p99 of add-line, fire and settle at most 30, 40 and 60 ms (PF1), no error, and no
// edge restart. It exits 1 when a threshold is missed, so a gate can run it.

import { writeFileSync } from "node:fs";

import { startEdge } from "../tests/edge.mjs";

const DEVICES = Number(process.env["DEVICES"] ?? 30);
const DURATION_S = Number(process.env["DURATION_S"] ?? 60);
const THINK_MS = Number(process.env["THINK_MS"] ?? 0);
const BUDGET_MS = { add_line: 30, fire: 40, settle: 60 };
/** The channel each counter order opens at: the knob, or what the first order the store took needed. */
let channel = process.env["CHANNEL"] || undefined;
/** The e-invoices the settles issued: numbered by the store, or "number pending" for the vendor. */
const invoices = { numbered: 0, pending: 0 };

/** Latency samples per act, in milliseconds, and the failures. */
const samples = {};
const failures = {};
/** The first refusal of each act, so a failing run says why. */
const firstRefusal = {};

function record(act, ms, ok) {
  (samples[act] ??= []).push(ms);
  if (!ok) {
    failures[act] = (failures[act] ?? 0) + 1;
  }
}

function percentile(values, p) {
  if (values.length === 0) {
    return null;
  }
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)];
}

/** One timed call. Resolves to the parsed body, or `null` on a refusal. */
async function call(base, token, act, method, path, body) {
  const started = performance.now();
  let ok = false;
  let parsed = null;
  try {
    const response = await fetch(base + path, {
      method,
      headers: {
        ...(body === undefined ? {} : { "content-type": "application/json" }),
        ...(token ? { authorization: `Bearer ${token}` } : {}),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    ok = response.ok;
    const text = await response.text();
    if (ok) {
      parsed = text ? JSON.parse(text) : {};
    } else if (act && firstRefusal[act] === undefined) {
      firstRefusal[act] = `${response.status} ${response.headers.get("pos-error-reason") ?? ""} ${text.slice(0, 200)}`;
    }
  } catch {
    ok = false;
  }
  if (act) {
    record(act, performance.now() - started, ok);
  }
  return parsed;
}

async function pair(base, code) {
  const paired = await call(base, null, null, "POST", "/api/pair", { code });
  if (!paired) {
    throw new Error(`pairing with ${code} was refused`);
  }
  return paired.device_token;
}

async function signIn(base, token, staffCode, staffPin) {
  const signed = await call(base, token, null, "POST", "/api/session/sign-in", {
    code: staffCode,
    pin: staffPin,
  });
  if (!signed) {
    throw new Error("sign-in was refused");
  }
}

/** A `/ws` socket that counts its frames. */
function socket(base, token) {
  const counter = { frames: 0, closed: false };
  const url = base.replace(/^http/, "ws") + "/ws";
  const ws = new WebSocket(url, ["pos-edge.v1", token]);
  ws.addEventListener("message", () => {
    counter.frames += 1;
  });
  ws.addEventListener("close", () => {
    counter.closed = true;
  });
  counter.close = () => ws.close();
  return counter;
}

/** A counter order's body: at the channel, where one is set. */
function orderBody() {
  return channel ? { channel } : {};
}

/** Counts the e-invoice a settle answered with: numbered by the store, or pending for the vendor. */
function countInvoice(settled) {
  if (settled.invoice_number) {
    invoices.numbered += 1;
  } else if (settled.invoice_number_pending) {
    invoices.pending += 1;
  }
}

/** Sells counter orders in cash until `deadline`. Returns how many settled. */
async function sell(base, token, item, price, currency, deadline) {
  let settled = 0;
  while (performance.now() < deadline) {
    const order = await call(base, token, "open_order", "POST", "/api/orders", orderBody());
    if (!order) {
      continue;
    }
    const orderId = order.order_id;
    const line = await call(base, token, "add_line", "POST", `/api/orders/${orderId}/lines`, {
      menu_item_id: item,
      quantity: { milli: 1000 },
    });
    if (!line) {
      continue;
    }
    await call(base, token, "fire", "POST", `/api/orders/${orderId}/fire`, {});
    const bill = await call(base, token, "open_bill", "POST", `/api/orders/${orderId}/bill`);
    if (!bill) {
      continue;
    }
    // The check is what a till reads before it takes money: the total with its fees and tax.
    const check = await call(base, token, "check", "GET", `/api/bills/${bill.bill_id}/check`);
    const due = check?.total_due ?? bill.total_due ?? { currency_code: currency, amount_minor: price };
    const done = await call(base, token, "settle", "POST", `/api/bills/${bill.bill_id}/settle`, {
      payments: [{ method: "PAYMENT_METHOD_CASH", tendered: due, applied_to_bill: due }],
    });
    if (done) {
      settled += 1;
      countInvoice(done);
    }
    if (THINK_MS > 0) {
      await new Promise((resolve) => setTimeout(resolve, THINK_MS));
    }
  }
  return settled;
}

async function main() {
  const own = process.env["EDGE_URL"] ? null : await startEdge(process.env["PROFILE"] ?? "kitchen-stations");
  const base = process.env["EDGE_URL"] ?? own.baseURL;
  const staffCode = process.env["STAFF_CODE"] ?? own.staffCode;
  const staffPin = process.env["STAFF_PIN"] ?? own.staffPin;
  try {
    const first = await pair(base, process.env["PAIRING_CODE"] ?? own.pairingCode);
    await signIn(base, first, staffCode, staffPin);
    const tokens = [first];
    while (tokens.length < DEVICES) {
      const minted = await call(base, first, null, "POST", "/api/pair/codes", {});
      if (!minted) {
        throw new Error("the manager could not mint a pairing code");
      }
      const token = await pair(base, minted.code);
      await signIn(base, token, staffCode, staffPin);
      tokens.push(token);
    }
    const menu = await call(base, first, null, "GET", "/api/menu");
    const candidates = (menu?.items ?? []).filter(
      (entry) =>
        entry.available &&
        !entry.sold_out &&
        !entry.sold_by_weight &&
        !entry.open_price &&
        entry.modifier_group_ids.length === 0,
    );
    // A store that asks each walk-in for its channel refuses an order without one: take away, then.
    if (!channel && !(await call(base, first, null, "POST", "/api/orders", {}))) {
      channel = "SALES_CHANNEL_TAKEAWAY";
    }
    // An item a kitchen station takes, so firing it is the real work: the first one that fires.
    let item;
    for (const candidate of candidates) {
      const order = await call(base, first, null, "POST", "/api/orders", orderBody());
      await call(base, first, null, "POST", `/api/orders/${order.order_id}/lines`, {
        menu_item_id: candidate.menu_item_id,
        quantity: { milli: 1000 },
      });
      if (await call(base, first, null, "POST", `/api/orders/${order.order_id}/fire`, {})) {
        item = candidate;
        break;
      }
    }
    if (!item) {
      throw new Error("no item on the store's menu fires to a station");
    }
    const price = item.unit_price.amount_minor;
    const currency = item.unit_price.currency_code;
    const sockets = tokens.map((token) => socket(base, token));
    const deadline = performance.now() + DURATION_S * 1000;
    const started = performance.now();
    const sold = await Promise.all(
      tokens.map((token) => sell(base, token, item.menu_item_id, price, currency, deadline)),
    );
    const elapsed = (performance.now() - started) / 1000;
    const status = await call(base, first, null, "GET", "/api/sync");
    sockets.forEach((counter) => counter.close());
    const settled = sold.reduce((sum, count) => sum + count, 0);
    const calls = Object.values(samples).reduce((sum, list) => sum + list.length, 0);
    const report = {
      devices: DEVICES,
      duration_s: DURATION_S,
      think_ms: THINK_MS,
      settled,
      settles_per_s: settled / elapsed,
      calls,
      calls_per_s: calls / elapsed,
      failures,
      first_refusal: firstRefusal,
      sockets_closed_early: sockets.filter((counter) => counter.closed).length,
      frames: sockets.reduce((sum, counter) => sum + counter.frames, 0),
      outbox_depth: status?.outbox_depth ?? null,
      channel: channel ?? null,
      einvoices: invoices,
      acts: Object.fromEntries(
        Object.entries(samples).map(([act, list]) => [
          act,
          { n: list.length, p50: percentile(list, 50), p95: percentile(list, 95), p99: percentile(list, 99) },
        ]),
      ),
    };
    const missed = Object.entries(BUDGET_MS).filter(
      ([act, budget]) => (report.acts[act]?.p99 ?? Infinity) > budget,
    );
    report.pass = missed.length === 0 && Object.keys(failures).length === 0 && report.sockets_closed_early === 0;
    report.missed = missed.map(([act]) => act);
    const text = JSON.stringify(report, null, 2);
    if (process.env["REPORT"]) {
      writeFileSync(process.env["REPORT"], text);
    }
    console.log(text);
    process.exitCode = report.pass ? 0 : 1;
  } finally {
    await own?.stop();
  }
}

await main();
