// Tap-to-paint and the edge round trip, as a browser sees them (ADR-0173 decision 6).
//
// # What one sample is
//
// The time from the tap's `pointerdown` to the first animation frame after the change that answers
// it is in the DOM. The page records the `pointerdown` timestamp itself, in the capture phase, so
// the figure is the page's own clock from start to end. Playwright's own overhead never enters it.
// A `MutationObserver` watches for the answering change, and the first `requestAnimationFrame`
// callback after it ends the sample. That frame is the one the change is painted in.
//
// A reload is measured from the navigation's start (`performance.timeOrigin`) to the first frame in
// which the order screen shows both the menu and the order's line.
//
// The edge round trip is the browser's resource timing for every `/api/*` request made meanwhile,
// from the request's start to the response's end. Ids in a path are folded to `:id`.
//
// # Budgets
//
// ADR-0173 decision 6 gives each act a 95th-percentile budget: 50 ms for an act the till answers
// on its own (switching screens, and the three acts it paints before the edge answers), 150 ms for
// an act that waits for the edge, and 300 ms from a reload to a drawn order screen. The figures and
// each budget's headroom are written as JSON to `POS_PERF_OUT`, or to
// `test-results/tap-to-paint.json` by default.
//
// Only with `POS_PERF_ENFORCE=1`, which the nightly job sets, does a p95 over its budget fail the
// run. Every commit's checks assert no timing (ADR-0109): a number from a shared runner on a debug
// build is a number about the runner.

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

import { expect, test } from "@playwright/test";

import { startEdge } from "../tests/edge.mjs";

/** How many rounds, each against a fresh edge, and what each round does. */
const ROUNDS = Number(process.env["POS_PERF_ROUNDS"] ?? 10);
const LINES_PER_ROUND = 10;
const QUANTITY_TAPS_PER_ROUND = 10;
const SWITCHES_PER_ROUND = 10;
const RELOADS_PER_ROUND = 5;

const OUT = process.env["POS_PERF_OUT"] ?? "test-results/tap-to-paint.json";
const ENFORCE = process.env["POS_PERF_ENFORCE"] === "1";

/** Each act's p95 budget in milliseconds (ADR-0173 decision 6). */
const ALONE_MS = 50;
const EDGE_MS = 150;
const RELOAD_MS = 300;
const BUDGETS = {
  "seat a table": EDGE_MS,
  "add a line": ALONE_MS,
  "change a quantity": ALONE_MS,
  "order screen to floor": ALONE_MS,
  "floor to order screen": ALONE_MS,
  "reload to a drawn order screen": RELOAD_MS,
  "open the move list": ALONE_MS,
  "move a table": ALONE_MS,
};

/** Waits, outside any sample, until the edge has confirmed whatever the till painted. */
async function confirmed(page) {
  await expect(page.locator('[data-outcome="line-sending"], [data-outcome="writes-pending"]')).toHaveCount(0);
}

/** Runs in the page before any of its own scripts: remembers each tap's `pointerdown` time. */
function installProbe() {
  const probe = { tap: null, done: null, reload: null };
  window.__ttp = probe;
  addEventListener(
    "pointerdown",
    (event) => {
      probe.tap = event.timeStamp;
    },
    { capture: true },
  );
  // A reload is measured from the navigation's start, so the probe watches for the drawn order
  // screen from the very first script.
  let wanted = null;
  try {
    wanted = sessionStorage.getItem("ttp-reload");
  } catch {
    wanted = null;
  }
  if (wanted === "order") {
    const drawn = () =>
      document.querySelector('[data-step="onItem"]') !== null &&
      document.querySelector('[data-outcome="line-added"]') !== null;
    probe.reload = new Promise((resolve) => {
      const observer = new MutationObserver(() => {
        if (drawn()) {
          observer.disconnect();
          requestAnimationFrame(() => resolve(performance.now()));
        }
      });
      observer.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    });
  }
}

/**
 * Arms the page to end a sample at the first frame after `until` holds.
 *
 * - `{ count: selector }`: more elements match than now.
 * - `{ text: selector }`: the first match's text differs from now.
 * - `{ present: selector }`: an element matches, where none does now.
 */
async function arm(page, until) {
  await page.evaluate((spec) => {
    const probe = window.__ttp;
    const select = (selector) => document.querySelectorAll(selector);
    let holds;
    if (spec.count !== undefined) {
      const before = select(spec.count).length;
      holds = () => select(spec.count).length > before;
    } else if (spec.text !== undefined) {
      const before = select(spec.text)[0]?.textContent ?? null;
      holds = () => (select(spec.text)[0]?.textContent ?? null) !== before;
    } else {
      holds = () => select(spec.present).length > 0;
    }
    probe.tap = null;
    probe.done = new Promise((resolve) => {
      const observer = new MutationObserver(() => {
        if (holds()) {
          observer.disconnect();
          requestAnimationFrame(() => resolve(performance.now() - probe.tap));
        }
      });
      observer.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    });
  }, until);
}

/** How long one sample may take before the run says which act never answered. */
const SAMPLE_DEADLINE_MS = 10_000;

/** Taps `locator` and returns the sample, in milliseconds. */
async function measure(page, locator, until) {
  await arm(page, until);
  await locator.click();
  const sample = await page.evaluate(
    (deadline) =>
      Promise.race([window.__ttp.done, new Promise((resolve) => setTimeout(() => resolve(null), deadline))]),
    SAMPLE_DEADLINE_MS,
  );
  if (sample === null) {
    throw new Error(`no change matching ${JSON.stringify(until)} within ${SAMPLE_DEADLINE_MS} ms`);
  }
  return sample;
}

/** Every `/api/*` request since the last call, as `{ "<path>": [ms, …] }`, and clears the buffer. */
async function drainRequests(page, into) {
  const entries = await page.evaluate(() => {
    const seen = performance
      .getEntriesByType("resource")
      .filter((entry) => new URL(entry.name).pathname.startsWith("/api/"))
      .map((entry) => {
        const url = new URL(entry.name);
        return { path: url.pathname + url.search, ms: entry.responseEnd - entry.startTime };
      });
    performance.clearResourceTimings();
    return seen;
  });
  for (const { path, ms } of entries) {
    const key = path.replace(/[0-9A-HJKMNP-TV-Z]{26}/g, ":id");
    (into[key] ??= []).push(ms);
  }
}

/** The nearest-rank percentile of `samples` at `q` (0 to 1). */
function percentile(sorted, q) {
  return sorted[Math.max(0, Math.ceil(q * sorted.length) - 1)];
}

function summarise(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const round = (ms) => Math.round(ms * 10) / 10;
  return {
    n: sorted.length,
    p50: round(percentile(sorted, 0.5)),
    p95: round(percentile(sorted, 0.95)),
    max: round(sorted[sorted.length - 1]),
  };
}

async function pairAndSignIn(page, edge) {
  await page.goto(`${edge.baseURL}/pair?code=${edge.pairingCode}`);
  await page.locator("#pair-submit").click();
  await expect(page).toHaveURL(/\/signin$/);
  await page.locator("#signin-code").fill(edge.staffCode);
  await page.locator("#signin-pin").fill(edge.staffPin);
  await page.locator('[data-step="submit"]').click();
  await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
}

/**
 * Opens the list of tables to move to, which the till answers alone, then moves, which it paints
 * before the edge answers (ADR-0173 decision 4), until no table is free.
 */
async function moveUntilNoTableIsFree(page, acts) {
  for (;;) {
    const open = page.locator('[data-step="openMove"]');
    acts["open the move list"].push(await measure(page, open, { present: '[data-step="moveTo"]' }));
    const offered = page.locator('[data-step="moveTo"]');
    if ((await offered.count()) <= 1) {
      return;
    }
    acts["move a table"].push(
      await measure(page, offered.first(), { text: '[data-outcome="order-open"]' }),
    );
    await confirmed(page);
  }
}

/** One round against a fresh edge: every act, `per`-many times, into `acts` and `requests`. */
async function round(page, acts, requests) {
  const edge = await startEdge();
  try {
    await pairAndSignIn(page, edge);
    await drainRequests(page, {});

    // Seat the first free table: an act that waits for the edge.
    const seat = page.locator('[data-step="onCard"]').first();
    acts["seat a table"].push(await measure(page, seat, { present: '[data-outcome="order-open"]' }));
    const tableUrl = page.url();

    // Adding a line, which waits for the edge.
    for (let i = 0; i < LINES_PER_ROUND; i += 1) {
      const item = page.locator('[data-step="onItem"]').first();
      acts["add a line"].push(await measure(page, item, { count: '[data-outcome="line-added"]' }));
      await confirmed(page);
    }

    // One more on the first line, which waits for the edge.
    for (let i = 0; i < QUANTITY_TAPS_PER_ROUND; i += 1) {
      const plus = page.locator('[data-step="setQuantity"]').first();
      acts["change a quantity"].push(await measure(page, plus, { text: '[data-outcome="line-quantity"]' }));
      await confirmed(page);
    }
    await drainRequests(page, requests);

    // Switching screens over data already loaded: to the floor and back to the table.
    for (let i = 0; i < SWITCHES_PER_ROUND; i += 1) {
      const home = page.locator('header a[href="/"]').first();
      acts["order screen to floor"].push(await measure(page, home, { present: '[data-outcome="floor"]' }));
      const card = page.locator('[data-step="onCard"]').first();
      acts["floor to order screen"].push(await measure(page, card, { present: '[data-outcome="order-open"]' }));
    }
    await drainRequests(page, requests);
    expect(page.url()).toBe(tableUrl);

    // A reload of the order screen, to the frame that shows the menu and the order's line.
    for (let i = 0; i < RELOADS_PER_ROUND; i += 1) {
      await page.evaluate(() => sessionStorage.setItem("ttp-reload", "order"));
      await page.reload();
      acts["reload to a drawn order screen"].push(await page.evaluate(() => window.__ttp.reload));
      await page.evaluate(() => sessionStorage.removeItem("ttp-reload"));
      await drainRequests(page, requests);
    }

    await moveUntilNoTableIsFree(page, acts);
    await drainRequests(page, requests);
  } finally {
    await edge.stop();
  }
}

test("tap-to-paint and the edge round trip, recorded", async ({ browser }) => {
  const acts = {
    "seat a table": [],
    "add a line": [],
    "change a quantity": [],
    "order screen to floor": [],
    "floor to order screen": [],
    "reload to a drawn order screen": [],
    "open the move list": [],
    "move a table": [],
  };
  const requests = {};
  for (let i = 0; i < ROUNDS; i += 1) {
    const context = await browser.newContext();
    await context.addInitScript(installProbe);
    const page = await context.newPage();
    await round(page, acts, requests);
    await context.close();
  }

  const tapToPaint = Object.fromEntries(
    Object.entries(acts).map(([act, samples]) => {
      const summary = summarise(samples);
      const budget = BUDGETS[act];
      return [act, { ...summary, budget, headroom: Math.round((budget - summary.p95) * 10) / 10 }];
    }),
  );
  const result = {
    rounds: ROUNDS,
    tapToPaint,
    edgeRoundTrip: Object.fromEntries(
      Object.entries(requests)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([path, s]) => [path, summarise(s)]),
    ),
  };
  mkdirSync(dirname(OUT), { recursive: true });
  writeFileSync(OUT, JSON.stringify(result, null, 2) + "\n");
  console.log(JSON.stringify(result, null, 2));
  if (ENFORCE) {
    const over = Object.entries(tapToPaint)
      .filter(([, figures]) => figures.p95 > figures.budget)
      .map(([act, figures]) => `${act}: p95 ${figures.p95} ms over its ${figures.budget} ms budget`);
    expect(over, "every act within its tap-to-paint budget (ADR-0173)").toEqual([]);
  }
});
