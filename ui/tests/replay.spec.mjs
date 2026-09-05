// The browser half of the step gate: walk each declared flow in a real browser, against a real edge.
//
// [ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md) decided **replay, not
// search**: this does not try to discover the cheapest path through the UI. It clicks exactly the
// taps `scripts/step-tasks.mjs` names, in order, and asserts the flow reaches its stated outcome.
//
// That shape is what catches the hole the static gate cannot see. Insert a required confirmation
// into the pay flow and the declared three taps no longer end in a paid bill — the third tap lands
// on a dialog instead of on the money — so this goes red **with the declaration untouched**.
//
// # The rules it follows
//
// * **Each step's element is `[data-step="<action>"]`, first match.** `setTip` and `setTender` sit on
//   repeated elements (a row of tip keys, a row of note keys), so "first" is a rule rather than a
//   discovery: the first tip key is the smallest, and the first tender key is the exact amount.
//   Both are legitimate operator choices, and picking by position keeps the harness from encoding a
//   second opinion about what the operator would do.
// * **Between taps the URL must match the step's declared `route`.** A flow that quietly navigates
//   somewhere else is a flow the map describes wrongly, even if every tap still lands.
// * **A precondition is not a tap.** Seating a table before the "add an item" flow, or typing a
//   float before the shift opens, is setup — the budget counts taps from the role's home screen with
//   the flow already available. Preconditions live in `PRECONDITIONS` below, keyed by task, and are
//   deliberately not in the declaration: they are how the harness reaches the starting line, not
//   part of the map.
// * **A skipped flow says why, and the set is checked.** Three counter tasks cannot run against the
//   on-fakes example, because a counter order arrives over the relay from a cloud the example does
//   not have. They carry `unreplayable` in the declaration, and the last test in this file asserts
//   the skipped set is exactly that set — so coverage cannot quietly shrink by one flow at a time.

import { expect, test } from "@playwright/test";

import { TASKS } from "../scripts/step-tasks.mjs";
import { startEdge } from "./edge.mjs";

/** A declared route as a regular expression: `:param` matches one path segment. */
function routePattern(route) {
  const escaped = route.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`^${escaped.replace(/:[A-Za-z]+/g, "[^/]+")}$`);
}

/**
 * Waits until the browser is on `route`, and says which flow expected it if it never gets there.
 *
 * Waits rather than reads: a tap posts to the edge and navigates when the answer lands, so a bare
 * read of the URL races the flow it is measuring and would fail whichever tap happened to be slow.
 */
async function expectRoute(page, route, description) {
  const pattern = routePattern(route);
  try {
    await page.waitForURL((url) => pattern.test(url.pathname));
  } catch {
    throw new Error(
      `${description} is declared on ${route}, and the flow is on ${new URL(page.url()).pathname}`,
    );
  }
}

/** Redeems the pairing code the edge minted, exactly as a device does (ADR-0030, ADR-0084). */
async function pair(page, edge) {
  await page.goto(`${edge.baseURL}/pair?code=${edge.pairingCode}`);
  await page.locator("#pair-submit").click();
  await expect(page).toHaveURL(/\/signin$/);
}

/** Signs the demo employee in, leaving the browser on the floor. */
async function signIn(page, edge) {
  await page.locator("#signin-code").fill(edge.staffCode);
  await page.locator("#signin-pin").fill(edge.staffPin);
  await page.locator('[data-step="submit"]').click();
  await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
}

/**
 * Moves to another screen the way an operator does: the status bar's own link.
 *
 * Not `page.goto`, which is a full page load. The kitchen and pass screens draw the fired lines this
 * browser session knows about — a reload empties that projection, and the harness would be asserting
 * against a blank board rather than against the flow. Clicking the link keeps the session, which is
 * also what the operator's tap does.
 */
async function navigateTo(page, path) {
  await page.locator(`a[href="${path}"]`).first().click();
  await page.waitForURL((url) => url.pathname === path);
}

/** Seats the first table on the floor and lands on its order screen. */
async function seatTable(page) {
  await expect(page.locator('[data-step="onCard"]').first()).toBeVisible();
  await page.locator('[data-step="onCard"]').first().click();
  await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
}

/** Adds the first item on the order screen's menu. */
async function addItem(page) {
  await page.locator('[data-step="addItem"]').first().click();
  await expect(page.locator('[data-outcome="line-added"]').first()).toBeVisible();
}

/** Fires the first unfired line to the kitchen. */
async function fireLine(page) {
  await page.locator('[data-step="fire"]').first().click();
  await expect(page.locator('[data-outcome="line-fired"]').first()).toBeVisible();
}

/** Opens the cash shift with a float, so a count and a close have something to act on. */
async function openShift(page) {
  await navigateTo(page, "/shift");
  await page.locator("#float").fill("100000");
  await page.locator('[data-step="openShift"]').click();
  await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
}

// How the harness reaches each flow's starting line. Every entry ends with the browser on the
// route the task's first step declares, with that tap available. A task with no entry starts on the
// floor, which is where signing in leaves the device.
const PRECONDITIONS = {
  "Add an item to an open order": seatTable,
  "Fire the open lines to the kitchen": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Settle a dine-in table in cash": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Settle a dine-in table in cash, taking a tip": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Settle a dine-in table by card": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Bump a ticket on the kitchen display": async (page) => {
    await seatTable(page);
    await addItem(page);
    await fireLine(page);
    await navigateTo(page, "/kds");
  },
  "Run away a course from the expo screen": async (page) => {
    await seatTable(page);
    await addItem(page);
    await fireLine(page);
    await navigateTo(page, "/expo");
  },
  "Open the cash shift with a float": async (page) => {
    await navigateTo(page, "/shift");
    await page.locator("#float").fill("100000");
  },
  "Enter the blind cash count": async (page) => {
    await openShift(page);
    await page.locator("#count").fill("100000");
  },
  "Close the shift and reveal the variance": async (page) => {
    await openShift(page);
    await page.locator("#count").fill("100000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
  },
  // The one flow whose precondition is *not* signing in, because signing in is the flow.
  "Sign in on a paired device": async (page, edge) => {
    await page.locator("#signin-code").fill(edge.staffCode);
    await page.locator("#signin-pin").fill(edge.staffPin);
  },
};

const replayed = TASKS.filter((declared) => declared.unreplayable === undefined);
const skipped = TASKS.filter((declared) => declared.unreplayable !== undefined);

for (const declared of replayed) {
  test(declared.task, async ({ page }) => {
    const edge = await startEdge();
    try {
      await pair(page, edge);
      // Every flow but the sign-in itself starts from a signed-in device on the floor, which is
      // where a real shift begins.
      if (declared.task !== "Sign in on a paired device") {
        await signIn(page, edge);
      }
      const prepare = PRECONDITIONS[declared.task];
      if (prepare !== undefined) {
        await prepare(page, edge);
      }

      for (const [index, step] of declared.steps.entries()) {
        await expectRoute(page, step.route, `step ${index + 1} of "${declared.task}"`);
        const tap = page.locator(`[data-step="${step.action}"]`).first();
        await expect(
          tap,
          `step ${index + 1} of "${declared.task}" taps \`${step.action}\`, and nothing on ${step.route} offers it — the flow grew a step, or the declaration is stale`,
        ).toBeVisible();
        await tap.click();
      }

      await expectRoute(page, declared.outcome.route, `the end of "${declared.task}"`);
      await expect(
        page.locator(`[data-outcome="${declared.outcome.mark}"]`).first(),
        `"${declared.task}" ran its ${declared.steps.length} declared taps and did not reach \`${declared.outcome.mark}\` — either the flow now needs a tap nobody declared, or it no longer does what the map says`,
      ).toBeVisible();
    } finally {
      await edge.stop();
    }
  });
}

// Not a flow: the guard on the list above. A task that stops being replayable has to say so in the
// declaration, where the reason is read by anyone looking at the map — silently dropping out of the
// browser gate is how coverage rots.
test("every flow is replayed except the ones that say why they cannot be", () => {
  expect(skipped.map((declared) => declared.task).sort()).toEqual(
    [
      "Charge a counter (takeaway) order in cash",
      "Charge a counter order by card",
      "Charge a counter order in cash, taking a tip",
    ].sort(),
  );
  for (const declared of skipped) {
    expect(declared.unreplayable, `"${declared.task}" must say why it cannot be replayed`).toMatch(
      /\S/,
    );
  }
  expect(replayed.length + skipped.length).toBe(TASKS.length);
});
