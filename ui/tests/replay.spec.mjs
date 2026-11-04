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
// * **A skipped flow says why, and the set is checked.** Two void tasks and the discount cannot run:
//   the manager's badge and PIN are typed into fields that appear mid-flow, and this harness types
//   only in a precondition — before the first tap. Each carries `unreplayable` in the declaration,
//   and the last test in this file asserts the skipped set is exactly that set — so coverage cannot
//   quietly shrink by one flow at a time. (The three counter charges used to be skipped too, because
//   a counter order could only arrive over a relay the example does not run; since the counter
//   starts its own orders (ADR-0146), each sets one up as a cashier would.)

import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

import { TASKS } from "../scripts/step-tasks.mjs";
import { startEdge } from "./edge.mjs";

/**
 * The edge's whole answer to an intercepted request, for a stub that rewrites it. The till names
 * the tag of the copy it keeps (ADR-0173), and the edge answers a tag it recognises with a `304`
 * and no body to rewrite, so the stub asks without one.
 */
async function fetchWhole(route) {
  const headers = { ...route.request().headers() };
  delete headers["if-none-match"];
  return route.fetch({ headers });
}

/**
 * Holds `pattern`'s POST until the test calls `release()`, so "while the request is out" is a moment
 * the test acts in rather than a race against a timer it has to win on a slow machine.
 *
 * With `reachEdge`, the request goes to the edge at once and only its answer is held: the edge has
 * committed, and its live-link event is on its way, before the till hears the answer. Without it,
 * the request itself waits, so neither the answer nor the event exists yet. `sent` resolves once
 * the request has reached that point.
 */
async function holdPost(page, pattern, { reachEdge = false } = {}) {
  let release = () => {};
  const released = new Promise((resolve) => {
    release = resolve;
  });
  let reached = () => {};
  const sent = new Promise((resolve) => {
    reached = resolve;
  });
  await page.route(pattern, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    if (reachEdge) {
      const response = await fetchWhole(route);
      reached();
      await released;
      await route.fulfill({ response });
    } else {
      reached();
      await released;
      await route.continue();
    }
  });
  return { release, sent };
}

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

/** Starts a walk-in at the counter and lands on its order screen (ADR-0146). */
async function startWalkIn(page) {
  await navigateTo(page, "/counter");
  await page.locator('[data-step="newOrder"]').click();
  await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
}

/** A walk-in with one line sent to the kitchen, back on the counter list waiting to be charged. */
async function aWalkInToCharge(page) {
  await startWalkIn(page);
  await addItem(page);
  await sendOrder(page);
  await page.locator('a[href="/counter"]').first().click();
  await page.waitForURL((url) => url.pathname === "/counter");
  await expect(page.locator('[data-step="charge"]').first()).toBeVisible();
}

/**
 * Waits for a line to be on the order. On a tablet the bill waits at the bottom of the screen until
 * it is tapped (`docs/ui-ux.md` §1 principle 9), so the line is on the page without being on screen
 * and the bar that counts it is what shows; everywhere else the line itself shows.
 */
async function expectLineAdded(page) {
  await expect(page.locator('[data-outcome="line-added"]').first()).toBeAttached();
  await expect(
    page
      .locator('[data-outcome="line-added"], [data-outcome="bill-bar"]')
      .filter({ visible: true })
      .first(),
  ).toBeVisible();
}

/** Adds the first item on the order screen's menu. */
async function addItem(page) {
  await page.locator('[data-step="onItem"]').first().click();
  await expectLineAdded(page);
}

/**
 * Adds the one item the store attaches a modifier group to, choosing a size on the way.
 *
 * Reached by typing rather than by position, for the same reason the declared flow does: the item
 * that asks a question is deliberately not first on the grid. The search is cleared afterwards so
 * the screen a flow continues on is the one it would be on anyway.
 */
async function addItemWithAChoice(page) {
  await page.locator("#menu-search").fill("margherita");
  await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
  await page.locator('[data-step="onItem"]').click();
  await page.locator('[data-step="chooseModifier"]').first().click();
  await page.locator('[data-step="confirmItem"]').click();
  await expect(page.locator('[data-outcome="line-modifiers"]').first()).toBeVisible();
  await page.locator("#menu-search").fill("");
}

/**
 * Adds the one item a search for `query` leaves on the grid, and clears the search again. By name
 * rather than by position, so a flow that needs two different lines gets two different lines.
 */
async function addByName(page, query) {
  await page.locator("#menu-search").fill(query);
  await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
  await page.locator('[data-step="onItem"]').click();
  await expectLineAdded(page);
  await page.locator("#menu-search").fill("");
}

/** Adds the demo store's starter (the salad), so a course has something waiting. */
async function addStarter(page) {
  await addByName(page, "salad");
}

/**
 * A table with a salad (97,900₫ with its tax) and an iced tea (43,450₫), on the pay screen with the
 * iced tea split off into a bill of its own — the guest who only had a drink.
 */
async function aTableWithTheDrinkSplitOff(page) {
  await seatTable(page);
  await addByName(page, "salad");
  await addByName(page, "iced");
  await sendThenTakePayment(page);
  await expect(page.getByText("141,350₫", { exact: true })).toBeVisible();
  await page.locator('[data-step="splitByItem"]').click();
  await expect(page.locator('[data-step="splitOff"]')).toBeDisabled();
  await page.locator('[data-step="pickLine"]', { hasText: "Iced tea" }).click();
  await page.locator('[data-step="splitOff"]').click();
  await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Iced tea");
  await expect(page.getByText("43,450₫", { exact: true })).toBeVisible();
}

/** Sends the order's unsent lines to the kitchen — one button, whatever the line count. */
async function sendOrder(page) {
  await page.locator('[data-step="fireOrder"]').click();
  await expect(page.locator('[data-outcome="line-fired"]').first()).toBeVisible();
}

/**
 * Takes payment the way a server does once the kitchen has the food: **Send** first when anything
 * is unsent, then **Take payment**. The till asks before it takes money for food the kitchen has not
 * been sent (pilot U1), and a test about paying is not a test about that question, so it sends
 * rather than answering it. The question has its own test.
 */
async function sendThenTakePayment(page) {
  await expect(page.locator('[data-outcome="writes-pending"]')).toHaveCount(0);
  const send = page.locator('[data-step="fireOrder"]');
  if ((await send.count()) > 0 && (await send.isEnabled())) {
    await sendOrder(page);
  }
  await page.locator('[data-step="takePayment"]').click();
}

/**
 * A table paid in cash, with the pay screen still showing what it settled for: the bill a guest
 * asks for a copy of.
 */
async function aSettledTable(page) {
  await seatTable(page);
  await addItem(page);
  await sendThenTakePayment(page);
  await page.locator('[data-step="setTender"]').first().click();
  await page.locator('[data-step="payCash"]').click();
  await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
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
// floor, which is where signing in leaves the device. A flow that takes payment starts from an order
// the kitchen already has, because the till asks before taking money for unsent food (pilot U1).
const PRECONDITIONS = {
  "Add an item to an open order": seatTable,
  // A counter order used to arrive only over the relay, which the on-fakes example does not run, so
  // the three counter charges were skipped. The counter now starts its own (ADR-0146), so each one
  // sets its order up the way a cashier would.
  "Start a counter order for a walk-in guest": (page) => navigateTo(page, "/counter"),
  "Add an item to a counter order": startWalkIn,
  "Charge a counter (takeaway) order in cash": aWalkInToCharge,
  "Charge a counter order by card": aWalkInToCharge,
  "Charge a counter order by QR transfer": aWalkInToCharge,
  "Charge a counter order in cash, taking a tip": aWalkInToCharge,
  "Change how many of a line": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Order an item for a particular seat": seatTable,
  "Mark an item sold out on every till": seatTable,
  "Move a table's guests to another table": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Release a table seated by mistake": seatTable,
  "Print a pre-bill for a table": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  // Today's list reads the edge when the screen opens, so the bill is settled first and the screen
  // opened after, by its link, as an operator does.
  "Reprint a receipt from today's bills": async (page) => {
    await aSettledTable(page);
    await navigateTo(page, "/today");
    await expect(page.locator('[data-step="chooseBill"]').first()).toBeVisible();
  },
  "Print a guest's receipt again right after they pay": aSettledTable,
  "Mark a dish sold out from the kitchen board": async (page) => {
    await navigateTo(page, "/kds");
  },
  // The item that asks a question is deliberately *not* first on the grid — every other flow's
  // precondition taps the first item and wants a line rather than a conversation. So this one types
  // the name to bring it up, exactly as "Find an item by name" does, and typing is not a tap. Every
  // declared step here is still a tap, which is what makes this replayable where the two voids are
  // not.
  "Add an item that needs a choice": async (page) => {
    await seatTable(page);
    await page.locator("#menu-search").fill("margherita");
    await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
  },
  // Typing, then the assertion that makes this flow worth declaring at all.
  //
  // `dac` is chosen and not `pho`, which was the obvious query and proves nothing: NFD leaves the
  // marks *after* the letter they sit on, so `pho` is already a substring of an unfolded `Phở` and
  // the test passes with the folding deleted. `đặc` is the opposite shape — `đ` has no
  // decomposition and the marks on `ặ` land between the `a` and the `c` — so `dac` reaches it only
  // if both halves of `fold` ran. Delete either and this goes red.
  //
  // It is also not the first item in the book, so a search that silently stopped filtering would
  // leave the harness tapping Margherita and still finding a line on the order. The count is what
  // forbids that: after typing, exactly one sell button may remain on the screen.
  "Find an item by name and add it": async (page) => {
    await seatTable(page);
    await page.locator("#menu-search").fill("dac");
    await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
  },
  // The note first, then a dish that asks no question, so the declared tap adds it straight away
  // and the note has exactly one line to land on.
  "Add a dish with a note for the kitchen": async (page) => {
    await seatTable(page);
    await page.locator("#line-note").fill("No ice, please");
    await page.locator("#menu-search").fill("iced tea");
    await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
  },
  "Fire the open lines to the kitchen": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  // A line on a course, so a course has something waiting and the row exists to tap. The salad is
  // the demo store's starter; reached by name rather than by position for the reason the modifier
  // flow is, since the first item on the grid is not it.
  "Fire one course to the kitchen": async (page) => {
    await seatTable(page);
    await addStarter(page);
  },
  "Settle a dine-in table in cash": async (page) => {
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
  },
  "Settle a dine-in table in cash, taking a tip": async (page) => {
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
  },
  "Settle a dine-in table by card": async (page) => {
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
  },
  "Settle a dine-in table by QR transfer": async (page) => {
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
  },
  "Split a dine-in bill evenly between two guests, each paying by QR": async (page) => {
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
  },
  // Two different things, so there are two lines to split between two guests.
  "Split one guest's items off a dine-in bill, then settle each part by QR": async (page) => {
    await seatTable(page);
    await addByName(page, "salad");
    await addByName(page, "iced");
    await sendOrder(page);
  },
  // One thing for seat 1 and one for seat 2, so there are two seats to split between.
  "Split a dine-in bill by seat, then settle each seat's part by QR": async (page) => {
    await seatTable(page);
    await page.locator('[data-step="chooseSeat"]').nth(0).click();
    await addByName(page, "salad");
    await page.locator('[data-step="chooseSeat"]').nth(1).click();
    await addByName(page, "iced");
    await sendOrder(page);
  },
  // A line to void. Unfired on purpose: that is the flow this task declares, and the fired one is
  // skipped for a reason the declaration states.
  "Void an unfired line": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  // The board and the pass both send a line that carries a choice, and both assert the board names
  // it before the declared tap happens. That assertion is the claim: a fired line consumes the base
  // recipe **plus one recipe per modifier** (§8), so a screen that shows only "Margherita" is asking
  // a cook to make something it will not name — and until this flow said so, both screens did.
  "Bump a ticket on the kitchen display": async (page) => {
    await seatTable(page);
    await addItemWithAChoice(page);
    await sendOrder(page);
    await navigateTo(page, "/kds");
    await expect(page.locator('[data-outcome="ticket-modifiers"]').first()).toBeVisible();
  },
  "Run away a course from the expo screen": async (page) => {
    await seatTable(page);
    await addItemWithAChoice(page);
    await sendOrder(page);
    await navigateTo(page, "/expo");
    await expect(page.locator('[data-outcome="pass-modifiers"]').first()).toBeVisible();
  },
  "Open the cash shift with a float": async (page) => {
    await navigateTo(page, "/shift");
    await page.locator("#float").fill("100000");
  },
  "Enter the blind cash count": async (page) => {
    await openShift(page);
    await page.locator("#count").fill("100000");
  },
  "Pay cash out of the drawer, for a supplier": async (page) => {
    await openShift(page);
    await page.locator("#movement-amount").fill("50000");
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

// Not a flow: what the declared "Fire one course" cannot say.
//
// That flow asserts the tap exists and that *something* reaches the kitchen — which a
// fire-by-course that ignored the course entirely would satisfy just as well, because firing
// everything also fires the starter. The claim worth gating is the **narrowing**: the starters go
// and nothing else does.
//
// So this orders two things the demo store deliberately separates — a salad on the "Starters"
// course and an iced tea on **no** course, which is what most lines in most stores are — sends the
// starters, and asserts one line fired and one is still waiting. Send the whole order from the
// course button and this goes red; drop the course filter on the edge and it goes red; stamp no
// course on the line as it is added and the button is not there to tap in the first place.
test("firing one course sends that course and leaves the rest of the order waiting", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);

    // On no course at all: a drink goes when it is poured. A course fire must not sweep it up.
    await page.locator("#menu-search").fill("iced tea");
    await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
    await page.locator('[data-step="onItem"]').click();
    await page.locator("#menu-search").fill("");
    await expect(page.locator('[data-outcome="line-unsent"]')).toHaveCount(2);

    // The store publishes two courses but only one of them has food waiting, so only one button is
    // drawn — a "Send mains" control over nothing is a control that does nothing.
    await expect(
      page.locator('[data-step="fireCourse"]'),
      "the course row offers a button per course with food waiting, and this order has starters only",
    ).toHaveCount(1);
    await page.locator('[data-step="fireCourse"]').click();

    await expect(
      page.locator('[data-outcome="line-fired"]'),
      "firing the starters sent more than the starters — the course filter is not narrowing anything",
    ).toHaveCount(1);
    await expect(
      page.locator('[data-outcome="line-unsent"]'),
      "the drink was on no course and should still be waiting",
    ).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// Sending to the kitchen paints first (ADR-0196 decision 1): the line shows fired at once, before
// the edge answers, and the Send button says it is sending. A refusal puts it back unsent with the refusal's sentence;
// the refusal is substituted, since the real edge sends any unsent line it is asked to.
test("a send paints the lines fired at once, and a refusal puts them back unsent", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    const held = await holdPost(page, "**/api/orders/*/fire");
    await page.locator('[data-step="fireOrder"]').click();
    await held.sent;
    await expect(page.locator('[data-outcome="line-fired"]')).toHaveCount(1);
    await expect(page.locator('[data-step="fireOrder"]')).toHaveText("Sending…");
    held.release();
    await expect(page.locator('[data-step="fireOrder"]')).not.toHaveText("Sending…");
    await expect(page.locator('[data-outcome="line-fired"]')).toHaveCount(1);

    await addByName(page, "iced");
    await refusePost(page, "**/api/orders/*/fire", "TRANSITION_REFUSED");
    await page.locator('[data-step="fireOrder"]').click();
    await expect(page.getByRole("alert")).toContainText("That is not possible right now");
    await expect(page.locator('[data-outcome="line-unsent"]')).toHaveCount(1);
    await expect(page.locator('[data-outcome="line-fired"]')).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// No answer to a send puts the lines back and reads the open orders again, which find them sent:
// the edge made it. It is sent once, never again by the till. A course's send takes the same path.
test("a send whose answer is lost is read again, and the lines show fired, sent once", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    let fires = 0;
    await page.route("**/api/orders/*/fire/*", async (route) => {
      fires += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="fireCourse"]').click();
    await expect(page.locator('[data-outcome="line-fired"]')).toHaveCount(1);
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveCount(0);
    await expect(page.locator('[data-outcome="line-unsent"]')).toHaveCount(0);
    expect(fires).toBe(1);
  } finally {
    await edge.stop();
  }
});

// A void paints first too: the line shows voided at once. A void that never reached the edge is put
// back, read again and found not made; a refusal (substituted: this unsent line needs no manager, so
// the real edge would not refuse it) puts it back with the refusal's sentence.
test("a void paints at once, a refusal and a void never made put the line back", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    await page.route("**/api/lines/*/void", (route) => route.abort("connectionreset"));
    await page.locator('[data-step="askVoid"]').first().click();
    await page.locator('[data-step="voidReason"]').first().click();
    await expect(page.locator('[data-outcome="line-voided"]')).toHaveCount(0);
    await expect(page.getByRole("alert").first()).toBeVisible();
    await expect(page.locator('[data-outcome="line-unsent"]')).toHaveCount(1);
    await page.unroute("**/api/lines/*/void");

    await refusePost(page, "**/api/lines/*/void", "APPROVAL_REQUIRED");
    await page.locator('[data-step="voidReason"]').first().click();
    await expect(page.getByRole("alert").first()).toBeVisible();
    await expect(page.locator('[data-outcome="line-voided"]')).toHaveCount(0);
    await page.unroute("**/api/lines/*/void");

    const held = await holdPost(page, "**/api/lines/*/void");
    await page.locator('[data-step="voidReason"]').first().click();
    await held.sent;
    await expect(page.locator('[data-outcome="line-voided"]')).toHaveCount(1);
    held.release();
    await expect(page.locator('[data-outcome="line-voided"]')).toHaveCount(1);
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// The till does not take money for food the kitchen was never sent (pilot U1).
//
// Take payment and the counter's charge used to open a bill whatever the lines' state, and settling
// fires nothing: a walk-in paid, the ticket never printed, and no screen said so. Now either asks
// first, in place. Cancel leaves the order as it was, still unsent and unbilled; Send and pay sends
// the lines in one go and carries on to payment, so the answer costs one tap and the food is with
// the kitchen before any money is.
test("taking payment for unsent food asks to send it first, at the table and at the counter", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await addByName(page, "iced");
    const prompt = page.locator('[data-outcome="send-first"]');

    await page.locator('[data-step="takePayment"]').click();
    await expect(prompt).toContainText("Send 2 items to the kitchen first?");
    await page.getByRole("alertdialog").getByRole("button", { name: "Cancel" }).click();
    await expect(prompt).toHaveCount(0);
    await expect(page.locator('[data-outcome="line-unsent"]')).toHaveCount(2);
    await expect(page).not.toHaveURL(/\/pay$/);

    await page.locator('[data-step="takePayment"]').click();
    await page.locator('[data-step="sendAndPay"]').click();
    await page.waitForURL(/\/pay$/);
    // The kitchen has both before any money is taken, which is the point of asking.
    await navigateTo(page, "/kds");
    await expect(page.getByText("Iced tea").first()).toBeVisible();
    await navigateTo(page, "/");
    await page.locator('[data-step="onCard"]').first().click();
    await expect(page.locator('[data-outcome="line-fired"]')).toHaveCount(2);
    await page.locator('[data-step="takePayment"]').click();
    await expect(prompt, "nothing is left unsent, so nothing is asked").toHaveCount(0);
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();

    // The counter's charge asks the same of a walk-in nobody sent.
    await startWalkIn(page);
    await addItem(page);
    await page.locator('a[href="/counter"]').first().click();
    await page.waitForURL((url) => url.pathname === "/counter");
    await page.locator('[data-step="charge"]').first().click();
    await expect(prompt).toContainText("Send 1 item to the kitchen first?");
    await page.locator('[data-step="sendAndPay"]').click();
    await expect(prompt).toHaveCount(0);
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// Not a flow: the regression for a mistyped PIN, which is the most ordinary event on a shop floor
// and used to unpair the tablet.
//
// The edge answers `401` for a refused sign-in *and* for an unpaired device, and the client read the
// status alone — so one wrong digit dropped the device token and bounced the operator back to the
// pairing screen, needing a manager and a fresh six-digit code mid-service. Every declared flow signs
// in with the right credentials, so no flow above could ever have caught it.
//
// Both halves are asserted, because fixing only the visible one would leave the damage: the screen
// has to *say* the PIN was wrong, and the device has to still be paired afterwards — proven by
// signing in properly with no second pairing in between.
test("a wrong PIN is refused on the sign-in screen and leaves the device paired", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);

    await page.locator("#signin-code").fill(edge.staffCode);
    // Not `edge.staffPin`, and longer than it, so it cannot collide with the demo one.
    await page.locator("#signin-pin").fill("999999");
    await page.locator('[data-step="submit"]').click();

    // The refusal is shown, on the screen the operator is already standing on.
    await expect(
      page.getByRole("alert"),
      "a wrong PIN showed no refusal — the client threw on the `401` instead of reading the body, which sends the till back to pairing and leaves the screen's three refusal messages unreachable",
    ).toBeVisible();

    // And the device is still paired, proven the only way that counts: the right PIN goes straight
    // in, with no pairing step in between.
    await signIn(page, edge);
  } finally {
    await edge.stop();
  }
});

test("a refusal does not wipe the PIN the operator has already started retyping", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);

    // Hold the answer back, so "while the request is in flight" is a moment the test can act in
    // rather than a race it has to win. This is the moment a busy store server gives an operator
    // for free.
    const held = await holdPost(page, "**/api/session/sign-in");

    await page.locator("#signin-code").fill(edge.staffCode);
    await page.locator("#signin-pin").fill("999999");
    await page.locator('[data-step="submit"]').click();

    // The operator does not wait: the button is disabled while the request is out, the field is
    // not, and they have already started the next attempt.
    await held.sent;
    await page.locator("#signin-pin").fill("4321");
    held.release();

    await expect(page.getByRole("alert")).toBeVisible();

    // What they typed is still there. It used to be cleared out from under them when the refusal
    // landed — digits vanishing mid-typing, so they typed again, and every confused attempt counts
    // toward the lockout (ADR-0030): a badge locked in the middle of service by the screen rather
    // than by the person.
    await expect(
      page.locator("#signin-pin"),
      "the refusal cleared a PIN the operator had already retyped",
    ).toHaveValue("4321");

    // And the cursor is where the next attempt is typed, so recovering costs no tap.
    await expect(page.locator("#signin-pin")).toBeFocused();
  } finally {
    await edge.stop();
  }
});

// A pairing code is single-use and lives five minutes, so the one an operator is holding is often
// spent: a second tablet given the same code, or a device sent back to pairing after it lost its
// token. The screen used to show the edge's English sentence and nothing about where the next code
// comes from, and the operator retyped the dead one. Asserted by text both languages carry, so
// the test does not depend on which one the browser picks.
test("a spent pairing code says where the next one comes from", async ({ page, browser }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);

    const other = await browser.newPage();
    try {
      await other.goto(`${edge.baseURL}/pair?code=${edge.pairingCode}`);
      await other.locator("#pair-submit").click();
      const refusal = other.getByRole("alert");
      await expect(refusal).toBeVisible();
      await expect(
        refusal,
        "a refused code should say how to get a new one, not show the edge's own sentence",
      ).toContainText("pairing-url.txt");
      await expect(refusal).not.toContainText("unknown or expired");
    } finally {
      await other.close();
    }
  } finally {
    await edge.stop();
  }
});

// A store the console has not staffed yet: the device pairs, and every code it tries is refused as
// a wrong one, because there is nobody to sign in as. The screen says so before anyone types.
test("a store nobody can sign in to says so on the sign-in screen", async ({ page }) => {
  const edge = await startEdge("unstaffed");
  try {
    const answered = page.waitForResponse((response) => response.url().endsWith("/api/session"));
    await pair(page, edge);
    expect((await (await answered).json()).sign_in_ready).toBe(false);
    await expect(
      page.locator("#signin-no-staff"),
      "a store with no staff published showed a sign-in form that refuses every code, and no reason",
    ).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("a staffed store's sign-in screen carries no such notice", async ({ page }) => {
  const edge = await startEdge();
  try {
    const answered = page.waitForResponse((response) => response.url().endsWith("/api/session"));
    await pair(page, edge);
    expect((await (await answered).json()).sign_in_ready).toBe(true);
    await signIn(page, edge);
  } finally {
    await edge.stop();
  }
});

// A device that was not running when the order was taken still learns what was chosen, and for whom.
//
// `GET /api/orders/live` exists for exactly this — its own doc calls it *"what a device has instead
// of the events it was not running to hear"* — and it carried the item, the quantity, the money and
// the state, and neither the choice nor the seat. So a kitchen display switched on mid-service, a
// second till joining a table, or any device that reloads, rebuilt every ticket as a bare
// "Margherita" belonging to nobody.
//
// This is the one case the rest of this file cannot reach: the harness is a single browser session,
// so every other flow sees both through the fan-out event it was there for. A real reload is what
// throws that away and makes the read answer — which is why this navigates with `page.goto` rather
// than by clicking, the opposite of the rule `navigateTo` follows for every other flow.
test("a screen that reloads mid-service still knows what was chosen, and for whom", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    const table = new URL(page.url()).pathname;
    await page.locator('[data-step="chooseSeat"]').first().click();
    await addItemWithAChoice(page);
    await sendOrder(page);

    // Everything this session knew is now gone; what comes back came from the edge.
    await page.goto(`${edge.baseURL}${table}`);
    await expect(page.locator('[data-outcome="line-modifiers"]').first()).toBeVisible();
    await expect(page.locator('[data-outcome="line-seat"]').first()).toBeVisible();

    await page.goto(`${edge.baseURL}/kds`);
    await expect(page.locator('[data-outcome="ticket-modifiers"]').first()).toBeVisible();
    await expect(page.getByText("+ Size — 25cm")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("the floor is drawn as the store published it: areas, seats, and the walkway gap", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);

    // The areas the example publishes, as headings. Before the screen read them, every table in the
    // building landed in one undifferentiated grid in publication order.
    await expect(page.getByRole("heading", { name: "Main hall" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Terrace" })).toBeVisible();

    // The seat count the console recorded, on the card. "Which free table seats six?" is a question
    // a host asks constantly and the screen could not answer at all.
    await expect(page.getByText("Seats 6 guests")).toBeVisible();

    // And the layout is the editor's, not a reflow. The example's hall leaves column 1 of row 1 empty
    // for the walkway, so table 5 — at zero-based (2, 1) — must land on CSS grid line 3, not on
    // line 2 where a list that merely flowed in order would put it.
    const fifth = page.locator('[data-step="onCard"]').nth(4);
    await expect(fifth).toHaveCSS("grid-column-start", "3");
    await expect(fifth).toHaveCSS("grid-row-start", "2");
  } finally {
    await edge.stop();
  }
});

// The device classes, in a browser, at the width each one names (`docs/ui-ux.md` §1 principle 9).
//
// Two claims, and both were false when this was written. `app.css`'s own header promises to "keep
// the page from scrolling sideways"; **every route** scrolled sideways on a phone, because the
// status bar's ten destinations sat in a `flex` that could not wrap and came to 488 px. And §1
// principle 2 asks for 48 px targets; those same destinations were bare text, 20 px high, at every
// size — the one control on the till a finger could not hit was the navigation.
//
// A sideways scroll is the right thing to assert rather than a screenshot: it is the one layout
// failure that is unambiguous. Text reflowing is a judgement call, a table needing a scroll of its
// own is sometimes correct, but content wider than the screen on a device with no mouse means an
// operator cannot reach it at all.
const DEVICE_CLASSES = [
  { name: "phone", width: 390, height: 844 },
  { name: "tablet", width: 768, height: 1024 },
  { name: "terminal", width: 1280, height: 800 },
];

for (const device of DEVICE_CLASSES) {
  test(`the till fits a ${device.name} on every screen, with targets a finger can hit`, async ({
    page,
  }) => {
    const edge = await startEdge();
    try {
      await page.setViewportSize({ width: device.width, height: device.height });
      await pair(page, edge);
      await signIn(page, edge);

      // A seated table with a line on it, so the order and pay screens are not measured empty —
      // an empty screen fits anything.
      await seatTable(page);
      await addItem(page);
      const table = new URL(page.url()).pathname.split("/")[2];

      for (const path of [
        "/",
        `/table/${table}`,
        `/table/${table}/pay`,
        "/kds",
        "/expo",
        "/shift",
        "/today",
        "/counter",
      ]) {
        await page.goto(`${edge.baseURL}${path}`);
        await expect(page.locator("header")).toBeVisible();

        const overflow = await page.evaluate(() => {
          const root = document.documentElement;
          // Name the widest thing that sticks out, so the failure says what to fix rather than
          // that something, somewhere, is too wide.
          const widest = (limit) => {
            let worst = null;
            for (const element of document.querySelectorAll("*")) {
              const box = element.getBoundingClientRect();
              if (box.width > 0 && box.right > limit + 1 && (worst === null || box.right > worst.right)) {
                worst = { right: Math.round(box.right), tag: element.tagName.toLowerCase(), classes: String(element.className).slice(0, 70) };
              }
            }
            return worst;
          };

          if (root.scrollWidth > root.clientWidth + 1) {
            return { kind: "document", page: root.scrollWidth, viewport: root.clientWidth, worst: widest(root.clientWidth) };
          }

          // The document not scrolling sideways is not the same as nothing being cut off, and for a
          // year it was the only thing asked. A box whose `overflow` is not `visible` on either axis
          // computes to `auto` on the other, so `overflow-y-auto` quietly makes an element a
          // *horizontal* scroller too — its content then overflows **inside it**, the document stays
          // exactly the viewport's width, and this gate saw nothing.
          //
          // That is not theoretical: the order screen ran 55px wider than a phone this way. The
          // content was reachable only by scrolling a container with no scrollbar and no affordance,
          // and one ordinary tap auto-scrolled it, cutting "← Floor", "Subtotal" and "Tax" off the
          // left with nothing to say they were there.
          // An author who writes `overflow-x-auto` asked for a horizontal scroller and gets one: the
          // placed floor plan is a room you pan around. What this catches is the container that
          // became one *without being asked*.
          const askedToScroll = /(^|\s)(overflow-x-auto|overflow-x-scroll|overflow-auto|overflow-scroll)(\s|$)/;
          for (const element of document.querySelectorAll("*")) {
            if (element.scrollWidth <= element.clientWidth + 1) {
              continue;
            }
            const style = getComputedStyle(element);
            if (style.overflowX === "visible") {
              continue;
            }
            // The `sr-only` pattern is a 1px box with hidden overflow. Its content always overflows,
            // by design, and none of it is on screen to be cut off.
            if (element.clientWidth <= 1 || element.clientHeight <= 1) {
              continue;
            }
            if (askedToScroll.test(String(element.className))) {
              continue;
            }
            return {
              kind: "container",
              tag: element.tagName.toLowerCase(),
              classes: String(element.className).slice(0, 70),
              content: element.scrollWidth,
              box: element.clientWidth,
              hiddenPx: element.scrollWidth - element.clientWidth,
              overflowX: style.overflowX,
              worst: widest(element.clientWidth),
            };
          }
          return null;
        });
        expect(
          overflow,
          `${path} is cut off sideways on a ${device.name}: ${JSON.stringify(overflow)}`,
        ).toBeNull();
      }

      // The status bar is on every screen, so its targets are the ones an operator meets most.
      await page.goto(`${edge.baseURL}/`);
      const shrunk = await page.evaluate(() => {
        const small = [];
        for (const control of document.querySelectorAll("header a, header button")) {
          const box = control.getBoundingClientRect();
          if (box.height > 0 && box.height < 48) {
            small.push(`${(control.textContent ?? "").trim()} is ${Math.round(box.height)}px`);
          }
        }
        return small;
      });
      expect(shrunk, "a status-bar control is under the 48px §1 principle 2 asks for").toEqual([]);

      // The same principle on the order and pay screens, where the till review measured the line's
      // Void button and the back links at about 30px: the controls a server meets on every table.
      for (const path of [`/table/${table}`, `/table/${table}/pay`]) {
        await page.goto(`${edge.baseURL}${path}`);
        await expect(page.locator("main a").first()).toBeVisible();
        const small = await page.evaluate(() => {
          const found = [];
          for (const control of document.querySelectorAll("main a, main button")) {
            const box = control.getBoundingClientRect();
            if (box.height > 0 && box.height < 48) {
              found.push(`${(control.textContent ?? "").trim()} is ${Math.round(box.height)}px`);
            }
          }
          return found;
        });
        expect(small, `a control on ${path} is under the 48px §1 principle 2 asks for`).toEqual([]);
      }
    } finally {
      await edge.stop();
    }
  });
}

// The store profile decides what home is (`docs/ui-ux.md` §3, §10).
//
// §10's capability model has carried a counter preset since it was written, and the till implemented
// exactly one profile: it landed every store on a floor plan and offered every store a kitchen
// board, including the ones with neither. This drives a **real counter store** — the same example
// binary, publishing `tables_enabled: false` — because a capability with no reader is the failure
// this tree keeps finding, and a reader with no gate is the next one.
test("a counter store lands on the counter, and is not offered a floor or a kitchen", async ({
  page,
}) => {
  const edge = await startEdge("counter");
  try {
    await pair(page, edge);
    await page.locator("#signin-code").fill(edge.staffCode);
    await page.locator("#signin-pin").fill(edge.staffPin);
    await page.locator('[data-step="submit"]').click();

    // Home is the counter list, at `/` — the address does not change with the shop, only what it
    // draws. The floor's own outcome mark must be absent, which is the half that would fail if the
    // profile were read but ignored.
    await expect(page).toHaveURL(/\/$/);
    await expect(page.locator('[data-outcome="counter"]')).toBeVisible();
    await expect(page.locator('[data-outcome="floor"]')).toHaveCount(0);

    // And the status bar stops offering what this shop cannot do. A destination that leads to a
    // board nobody watches is the same failure `tips_enabled` was published to stop: an action the
    // store cannot honour, presented as though it could.
    // Scoped to the nav, not the whole header: the brand is a link to `/` too, and on this store
    // that is the counter. "Home" is still offered — it is the *floor* that is not.
    await expect(page.locator('header nav a[href="/"]')).toHaveCount(0);
    await expect(page.locator('header nav a[href="/kds"]')).toHaveCount(0);
    await expect(page.locator('header nav a[href="/expo"]')).toHaveCount(0);
    // The counter itself is still a destination, and so is the shift — neither depends on tables.
    await expect(page.locator('header nav a[href="/counter"]')).toHaveCount(1);
    await expect(page.locator('header nav a[href="/shift"]')).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// The same store with the default profile still gets the floor, so the test above is measuring the
// flag rather than a screen that broke. Cheap, and it is the assertion that would have caught a
// `tables_enabled` read inverted.
test("a table-service store still lands on the floor", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await expect(page).toHaveURL(/\/$/);
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
    await expect(page.locator('header nav a[href="/"]')).toHaveCount(1);
    await expect(page.locator('header nav a[href="/kds"]')).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// Not a flow: the guard on the list above. A task that stops being replayable has to say so in the
// A kitchen board says how long a ticket has been waiting, and says it louder once it is late.
//
// The board had no clock at all: every ticket looked the same whether it was fired ten seconds ago
// or twenty minutes ago, so a cook picking the next one had nothing to pick *by*. That is the one
// question the board exists to answer.
//
// Driven with Playwright's clock rather than by waiting, because the interesting case is ten minutes
// in and a gate that takes ten minutes is a gate somebody turns off. The clock is installed before
// the page loads so the till's own `Date.now()` moves with it.
//
// Asserts the number as well as the state. The colour is the glance and the number is the fact, and
// a board that went red without saying how long would be conveying a state by hue alone — which is
// what `tokens.css` says the state colours exist to avoid.
test("the kitchen board counts how long a ticket has waited, and marks it late", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
    await navigateTo(page, "/kds");

    const waited = page.locator('[data-outcome="ticket-waited"]').first();
    await expect(waited).toHaveText("0:00");
    await expect(page.locator('[data-outcome="ticket-waiting"]').first()).toBeVisible();

    // Ninety seconds in: counting, still on time.
    await page.clock.fastForward(90_000);
    await expect(waited).toHaveText("1:30");
    await expect(page.locator('[data-outcome="ticket-waiting"]').first()).toBeVisible();
    await expect(page.locator('[data-outcome="ticket-late"]')).toHaveCount(0);

    // Past ten minutes: the board says so, and still says the number.
    await page.clock.fastForward(9 * 60 * 1000);
    await expect(waited).toHaveText("10:30");
    await expect(page.locator('[data-outcome="ticket-late"]').first()).toBeVisible();
    await expect(page.locator('[data-outcome="ticket-waiting"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// Each kitchen station says when its tickets are late (ADR-0160 decision 2).
//
// The `kitchen-stations` store publishes a kitchen whose tickets are late after two minutes and a
// bar that sets nothing, so is late after the ten every board used before. The iced tea goes to the
// bar and the salad to the kitchen, so one order puts a ticket on each, and each must turn late at
// its own station's threshold: on the board for every station, where both are drawn side by side,
// and on each station's own board.
test("each kitchen ticket turns late at its own station's threshold", async ({ page }) => {
  const edge = await startEdge("kitchen-stations");
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addByName(page, "salad");
    await addByName(page, "iced");
    await sendOrder(page);
    await navigateTo(page, "/kds");

    // On the board for every station, each ticket names its station.
    const at = (name) => page.locator('[data-step="onBump"]', { hasText: ` · ${name}` });
    await expect(at("Kitchen")).toHaveAttribute("data-outcome", "ticket-waiting");
    await expect(at("Bar")).toHaveAttribute("data-outcome", "ticket-waiting");

    // Ninety seconds in, both are on time.
    await page.clock.fastForward(90_000);
    await expect(at("Kitchen")).toHaveAttribute("data-outcome", "ticket-waiting");
    await expect(at("Bar")).toHaveAttribute("data-outcome", "ticket-waiting");

    // Past the kitchen's two minutes: its ticket is late, and the bar's is not.
    await page.clock.fastForward(60_000);
    await expect(at("Kitchen")).toHaveAttribute("data-outcome", "ticket-late");
    await expect(at("Bar")).toHaveAttribute("data-outcome", "ticket-waiting");

    // The same on each station's own board, where a ticket no longer names its station.
    const only = page.locator('[data-step="onBump"]');
    await page.getByRole("tab", { name: "Kitchen" }).click();
    await expect(only).toHaveCount(1);
    await expect(only).toHaveAttribute("data-outcome", "ticket-late");
    await page.getByRole("tab", { name: "Bar" }).click();
    await expect(only).toHaveCount(1);
    await expect(only).toHaveAttribute("data-outcome", "ticket-waiting");

    // Past ten minutes, the bar's ticket is late too, on its own board and on everybody's.
    await page.clock.fastForward(8 * 60 * 1000);
    await expect(only).toHaveAttribute("data-outcome", "ticket-late");
    await page.getByRole("tab", { name: "All stations" }).click();
    await expect(at("Bar")).toHaveAttribute("data-outcome", "ticket-late");
    await expect(at("Kitchen")).toHaveAttribute("data-outcome", "ticket-late");
  } finally {
    await edge.stop();
  }
});

// The kitchen board rings when new food reaches it, once a cook has turned its sound on — and not
// for what was already on the board when it came on.
//
// A board in a loud kitchen is looked at when something tells the cook to look. The test cannot
// hear, so a stand-in speaker counts the notes the board plays. Two pages share one device, as a
// till and a board do in a store: the till sends, the board rings.
//
// The reload at the end is the half that keeps the chime trustworthy. The choice survives, the
// food already on the board stays quiet, and the board asks for one tap before it can ring again —
// a browser will not start sound on a page nobody has touched.
test("the kitchen board rings when new food arrives, once its sound is on", async ({ context }) => {
  const edge = await startEdge();
  try {
    await context.addInitScript(() => {
      window.__notes = 0;
      class Speaker {
        constructor() {
          this.state = "running";
          this.currentTime = 0;
          this.destination = {};
        }
        resume() {
          this.state = "running";
          return Promise.resolve();
        }
        createOscillator() {
          window.__notes += 1;
          return {
            type: "sine",
            frequency: { setValueAtTime() {} },
            connect() {},
            start() {},
            stop() {},
          };
        }
        createGain() {
          return { gain: { setValueAtTime() {}, exponentialRampToValueAtTime() {} }, connect() {} };
        }
      }
      window.AudioContext = Speaker;
    });
    const till = await context.newPage();
    await pair(till, edge);
    await signIn(till, edge);
    await seatTable(till);
    await addItem(till);
    await sendOrder(till);

    const board = await context.newPage();
    await board.goto(`${edge.baseURL}/kds`);
    await expect(board.locator('[data-outcome="ticket-waiting"]').first()).toBeVisible();
    const notes = () => board.evaluate(() => window.__notes);
    const toggle = board.locator('[data-outcome="kds-sound"]');
    await expect(toggle).toHaveText("Sound off");
    await toggle.click();
    await expect(toggle).toHaveText("Sound on");
    // The toggle rings once, two notes, so the cook hears it working.
    await expect.poll(notes).toBe(2);

    await addByName(till, "iced");
    await till.locator('[data-step="fireOrder"]').click();
    await expect.poll(notes).toBe(4);

    // Away and back, with the sound still live: the board comes on over food it already had, which
    // is not news. Nothing to wait for but the absence of a note, so the wait is a fixed one.
    await navigateTo(board, "/");
    await navigateTo(board, "/kds");
    await expect(board.locator('[data-outcome="ticket-waiting"]').first()).toBeVisible();
    await board.waitForTimeout(700);
    expect(await notes()).toBe(4);

    await board.reload();
    await expect(toggle).toHaveText("Sound on");
    await expect(board.getByText("Tap anywhere on the board to let it ring for new food.")).toBeVisible();
    await expect(board.locator('[data-outcome="ticket-waiting"]').first()).toBeVisible();
    await board.locator("h1").click();
    await expect(board.getByText("Tap anywhere on the board to let it ring for new food.")).toHaveCount(0);
    expect(await notes()).toBe(0);

    await addByName(till, "water");
    await till.locator('[data-step="fireOrder"]').click();
    await expect.poll(notes).toBe(2);
  } finally {
    await edge.stop();
  }
});

// And the age survives a reload, which is the whole reason the edge records it.
//
// This is the assertion the backend change exists for. A board could have counted from the moment
// *it* first saw a ticket, and that works right up until the screen reloads — a crashed tab, a shift
// change, a second board brought online — at which point every ticket in the kitchen reads as brand
// new and the cook is told the opposite of the truth. `sales.order_line.fired` carries the time in
// its envelope, the projection keeps it, `/api/orders/live` returns it, so a board that has just
// started still counts from when the food was ordered.
//
// No fake clock here on purpose: the point is what the *edge* said, and a frozen page clock would
// be measuring against a time this test invented.
test("a kitchen board that reloads still knows when the food was ordered", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
    await navigateTo(page, "/kds");
    await expect(page.locator('[data-outcome="ticket-waited"]').first()).toBeVisible();

    // Everything the board held in memory is gone.
    await page.reload();
    await expect(page.locator('[data-outcome="ticket-waited"]').first()).toBeVisible();
    // Still a real elapsed reading rather than a blank: the time came back from the edge.
    await expect(page.locator('[data-outcome="ticket-waited"]').first()).toHaveText(/^\d+:\d{2}$/);
  } finally {
    await edge.stop();
  }
});

/**
 * Puts a board's live link where a test can cut it and hold it down, as a Wi-Fi blip does.
 *
 * Every `/ws` the page opens is routed through here and on to the real edge, and each is recorded
 * with the frames the edge sent on it (`heard[n]` for the n-th socket). Once `drop()` has cut the
 * socket that is open, the page's next one waits until `restore()`, so whatever the edge publishes
 * in between reaches this device on no socket at all. That leaves two ways for it to arrive: the
 * edge's replay after the position the board names, or a reload. `readsSinceRestore()` counts the
 * reloads, so a test can tell the two apart — from the restore on, because a till's actions while
 * the link is down take long enough that no boot read is still in flight by then.
 */
async function aLinkThatCanDrop(page) {
  const links = [];
  const heard = [];
  let reads = 0;
  let readsAtRestore = 0;
  let restore = () => {};
  let restored = null;
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/orders/live") {
      reads += 1;
    }
  });
  await page.routeWebSocket(
    (url) => url.pathname === "/ws",
    async (link) => {
      links.push(link);
      const frames = [];
      heard.push(frames);
      if (restored !== null) {
        await restored;
      }
      const edgeSide = link.connectToServer();
      edgeSide.onMessage((message) => {
        frames.push(message);
        link.send(message);
      });
    },
  );
  return {
    links,
    heard,
    reads: () => reads,
    readsSinceRestore: () => reads - readsAtRestore,
    async drop() {
      restored = new Promise((resolve) => {
        restore = resolve;
      });
      const count = links.length;
      await links[count - 1].close();
      // The board notices, and its next socket is waiting.
      await expect.poll(() => links.length).toBe(count + 1);
    },
    restore() {
      readsAtRestore = reads;
      restore();
    },
  };
}

// A kitchen board whose link drops for a moment still gets the food fired while it was gone, and it
// gets it from the edge's replay, not from a reload.
//
// The board hears the table open, which gives it a position. Its link drops, the till sends the
// order while it is down, and the board's next socket names that position. The fire reaches the
// board with no read of what is open, which is the difference between resuming and reloading.
test("a kitchen board that drops its link gets the food fired meanwhile, without reloading", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    await pair(till, edge);
    await signIn(till, edge);

    const board = await context.newPage();
    const link = await aLinkThatCanDrop(board);
    await board.goto(`${edge.baseURL}/kds`);
    await expect(board.getByText("Store connected")).toBeVisible();
    await seatTable(till);
    await addItem(till);
    // The board's first socket named no position, so the edge first said `resync`; the frames after
    // it are the stream's.
    const events = () =>
      link.heard[0].map((frame) => JSON.parse(frame)).filter((frame) => frame.type === "event");
    await expect.poll(() => events().length).toBeGreaterThan(0);
    const seen = events()[0];
    expect(typeof seen.stream_id, "every frame says which stream it is on").toBe("string");
    expect(typeof seen.sequence, "and where on it").toBe("number");
    await expect(board.locator('[data-outcome="ticket-waiting"]')).toHaveCount(0);

    await link.drop();
    await sendOrder(till);
    link.restore();

    await expect(board.locator('[data-outcome="ticket-waiting"]').first()).toBeVisible();
    const resumed = new URL(link.links[1].url()).searchParams;
    expect(resumed.get("stream_id")).toBe(seen.stream_id);
    expect(Number(resumed.get("after_sequence"))).toBeGreaterThanOrEqual(seen.sequence);
    const replayed = link.heard[1].map((frame) => JSON.parse(frame));
    expect(replayed.map((frame) => frame.event_type)).toContain("sales.order_line.fired");
    expect(replayed.some((frame) => frame.type === "resync"), "nothing was a gap").toBe(false);
    expect(link.readsSinceRestore(), "replayed, not reloaded").toBe(0);
  } finally {
    await edge.stop();
  }
});

// A board whose link drops before it has seen a single change has no position to name. It must not
// trust the live stream alone, so it reloads, and the food fired while it was gone is still there.
//
// The gap path through the real edge: the board asks to resume from nothing, the edge answers
// `resync` once the socket is subscribed, and the board makes the read a restarting device makes.
test("a kitchen board that drops its link before it heard anything reloads, and misses nothing", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    await pair(till, edge);
    await signIn(till, edge);

    const board = await context.newPage();
    const link = await aLinkThatCanDrop(board);
    await board.goto(`${edge.baseURL}/kds`);
    await expect(board.getByText("Store connected")).toBeVisible();
    await expect.poll(link.reads).toBeGreaterThan(0);
    const named = link.heard[0]
      .map((frame) => JSON.parse(frame))
      .filter((frame) => typeof frame.sequence === "number");
    expect(named, "the board has heard nothing it could name").toHaveLength(0);

    await link.drop();
    await seatTable(till);
    await addItem(till);
    await sendOrder(till);
    link.restore();

    await expect(board.locator('[data-outcome="ticket-waiting"]').first()).toBeVisible();
    const resumed = new URL(link.links[1].url()).searchParams;
    expect(resumed.get("stream_id"), "it has no stream to name").toBeNull();
    expect(resumed.get("after_sequence")).toBe("0");
    await expect.poll(() => link.heard[1].length).toBeGreaterThan(0);
    expect(JSON.parse(link.heard[1][0]).type, "the edge said it cannot resume the board").toBe(
      "resync",
    );
    expect(link.readsSinceRestore(), "so the board reloaded").toBeGreaterThan(0);
  } finally {
    await edge.stop();
  }
});

// A till that has just paired can be on the floor before its live link is up, and what happens
// meanwhile still reaches it.
//
// The link's first attempt is refused until the device pairs, and the retry that succeeds can come
// after the sign-in has read the store. Here that retry is held while a second screen seats a table
// and starts its order. The till names no position it does not have: the edge answers `resync` once
// the socket is subscribed, and the till reads again what is open, so its floor shows the table
// taken — with no reload of the page. Before, it asked for the live stream alone, and showed the
// table free until something reloaded it.
test("a till whose link opens after it reached the floor still hears what happened meanwhile", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    let open = () => {};
    const opened = new Promise((resolve) => {
      open = resolve;
    });
    const sockets = [];
    // Only a paired device's socket is held: the refusal before pairing reaches the edge as before.
    await till.routeWebSocket(
      (url) => url.pathname === "/ws",
      async (socket) => {
        if (socket.protocols().length > 1) {
          sockets.push(socket);
          await opened;
        }
        socket.connectToServer();
      },
    );
    let loads = 0;
    till.on("load", () => {
      loads += 1;
    });
    await pair(till, edge);
    await signIn(till, edge);
    await expect.poll(() => sockets.length).toBeGreaterThan(0);
    const firstDot = till.locator('[data-step="onCard"]').first().locator("span.rounded-full");
    await expect(firstDot).not.toHaveClass(/bg-occupied/);
    const loadsBefore = loads;

    // Another screen on the store takes the first table while the till's link is down.
    const other = await context.newPage();
    await other.goto(`${edge.baseURL}/`);
    await seatTable(other);
    await addItem(other);

    open();
    await expect(firstDot).toHaveClass(/bg-occupied/);
    const asked = new URL(sockets[0].url()).searchParams;
    expect(asked.get("stream_id"), "it has no position to name").toBeNull();
    expect(asked.get("after_sequence")).toBe("0");
    expect(loads, "read again, not reloaded").toBe(loadsBefore);
  } finally {
    await edge.stop();
  }
});

// What a till hears while it reads again after a `resync` is applied after the read, never under it.
//
// The read can be answered before a change the link then carries; applied in arrival order, the
// older answer would put the table back to free. Here the till's link opens late, as above, and the
// floor read its `resync` starts is answered at once but held back from the till while a second
// screen seats the first table. The till has heard the seating before the read lands, and still
// shows the table taken after it.
test("a till reading again after a resync does not undo what it heard meanwhile", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    let open = () => {};
    const opened = new Promise((resolve) => {
      open = resolve;
    });
    const carried = [];
    await till.routeWebSocket(
      (url) => url.pathname === "/ws",
      async (socket) => {
        if (socket.protocols().length > 1) {
          await opened;
        }
        socket.connectToServer().onMessage((message) => {
          carried.push(JSON.parse(String(message)).event_type);
          socket.send(message);
        });
      },
    );
    await pair(till, edge);
    await signIn(till, edge);
    const firstDot = till.locator('[data-step="onCard"]').first().locator("span.rounded-full");
    await expect(firstDot).not.toHaveClass(/bg-occupied/);

    // From here on, the floor read is answered by the edge at once and handed to the till later.
    let release = () => {};
    const released = new Promise((resolve) => {
      release = resolve;
    });
    let reached = () => {};
    const answered = new Promise((resolve) => {
      reached = resolve;
    });
    await till.route("**/api/floor", async (route) => {
      const response = await fetchWhole(route);
      reached();
      await released;
      await route.fulfill({ response });
    });
    open();
    await answered;

    const other = await context.newPage();
    await other.goto(`${edge.baseURL}/`);
    await seatTable(other);
    await expect.poll(() => carried.includes("sales.table.opened")).toBe(true);

    const landed = till.waitForResponse(
      (response) => new URL(response.url()).pathname === "/api/floor",
    );
    release();
    await landed;
    // One turn of the page's own loop, so the answer it was handed is applied before this looks.
    await till.evaluate(() => new Promise((resolve) => setTimeout(resolve, 0)));
    await expect(firstDot).toHaveClass(/bg-occupied/);
  } finally {
    await edge.stop();
  }
});

// A till that reloads still knows which tables have people at them.
//
// The floor is the home screen, and it drew every table free after a refresh. `GET /api/floor`
// served the published *plan* — areas, tables, stations — and no state, so a device learned
// occupancy only from the fan-out. A device that was not running when the guests sat down never saw
// those events, and a shift change, a crashed tab or a second till brought online all produce
// exactly that device.
//
// The consequence was not cosmetic: a server looking at the home screen would be told a table with
// people at it was free, and could seat guests on top of them.
//
// Asserts the *card*, not an API field, because what went wrong was what an operator saw.
test("a till that reloads still knows which tables are occupied", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await navigateTo(page, "/");

    const firstDot = page.locator('[data-step="onCard"]').first().locator("span.rounded-full");
    await expect(firstDot).toHaveClass(/bg-occupied/);

    // Everything this device folded from the fan-out is gone.
    await page.reload();
    await expect(page.locator('[data-outcome="floor"]').first()).toBeVisible();
    await expect(firstDot).toHaveClass(/bg-occupied/);
  } finally {
    await edge.stop();
  }
});

// The floor says how long each table has been seated, and a till that reloads still knows.
//
// A host reads it to know who is due a check and who is about to leave. The time is when the
// table's order opened, as the edge recorded it (`opened_time` on the live read), so a reload — or a
// second till switched on mid-service — shows the same figure rather than counting from its own
// start. Nothing shows for the first minute: "seated 0 min" says nothing.
test("the floor says how long a table has been seated, and a reload still knows", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await navigateTo(page, "/");

    const seated = page.locator('[data-step="onCard"]').first().locator('[data-outcome="table-seated"]');
    await expect(seated).toHaveCount(0);
    await page.clock.fastForward(25 * 60_000);
    await expect(seated).toHaveText("Seated 25 min");

    await page.reload();
    await expect(page.locator('[data-outcome="floor"]').first()).toBeVisible();
    await expect(seated).toHaveText("Seated 25 min");
  } finally {
    await edge.stop();
  }
});

// Staff mark an item sold out, and every till stops selling it at once — then bring it back.
//
// The kitchen runs out mid-service. One till marks the iced tea sold out, a second till showing the
// menu greys it out without reloading, the mark survives a reload (it is the edge's, not the
// screen's), and the same tap brings it back.
test("an item marked sold out on one till stops selling on every till, until it is brought back", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    await pair(till, edge);
    await signIn(till, edge);
    await seatTable(till);

    const other = await context.newPage();
    await other.goto(`${edge.baseURL}/`);
    await expect(other.locator('[data-outcome="floor"]').first()).toBeVisible();
    await other.locator('[data-step="onCard"]').nth(1).click();
    await expect(other.locator('[data-outcome="order-open"]')).toBeVisible();
    await other.locator("#menu-search").fill("iced");
    const otherTea = other.locator('[data-step="onItem"]');
    await expect(otherTea).toHaveCount(1);
    await expect(otherTea).toBeEnabled();

    await till.locator('[data-step="startMarking"]').click();
    await till.locator("#menu-search").fill("iced");
    await till.locator('[data-step="toggleSoldOut"]').click();
    await expect(till.locator('[data-outcome="item-sold-out"]')).toHaveText("Sold out");

    // The other till, without reloading.
    await expect(otherTea).toBeDisabled();
    await expect(other.locator('[data-outcome="item-sold-out"]')).toHaveText("Sold out");

    // The edge's fact, not the screen's: a reload still has it.
    await other.reload();
    await other.locator("#menu-search").fill("iced");
    await expect(other.locator('[data-step="onItem"]')).toBeDisabled();

    // The same tap brings it back, and it sells again.
    await till.locator('[data-step="toggleSoldOut"]').click();
    await expect(till.locator('[data-outcome="item-sold-out"]')).toHaveCount(0);
    await till.getByRole("button", { name: "Done" }).click();
    await expect(other.locator('[data-step="onItem"]')).toBeEnabled();
    await till.locator('[data-step="onItem"]').click();
    await expect(till.locator('[data-outcome="line-added"]').first()).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A choice the kitchen has run out of cannot be picked, and the dish still sells without it.
//
// A modifier is an item in the price book, so the search finds the extra cheese in marking mode and
// staff can mark it sold out. The pizza's picker then greys that choice out and says why, rather than
// offering a choice the edge refuses once the whole pizza has been built.
test("a modifier marked sold out cannot be chosen, and the dish still sells without it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);

    await page.locator('[data-step="startMarking"]').click();
    await page.locator("#menu-search").fill("cheese");
    await page.locator('[data-step="toggleSoldOut"]').click();
    await expect(page.locator('[data-outcome="item-sold-out"]')).toHaveText("Sold out");
    await page.getByRole("button", { name: "Done" }).click();

    await page.locator("#menu-search").fill("margherita");
    await page.locator('[data-step="onItem"]').click();
    const cheese = page.locator('[data-step="chooseModifier"]', { hasText: "Extra cheese" });
    await expect(cheese).toBeDisabled();
    await expect(cheese).toContainText("Sold out");

    await page.locator('[data-step="chooseModifier"]').first().click();
    await page.locator('[data-step="confirmItem"]').click();
    await expect(page.locator('[data-outcome="line-modifiers"]').first()).toBeVisible();
    await expect(page.locator('[data-outcome="line-modifiers"]').first()).not.toContainText(
      "Extra cheese",
    );
  } finally {
    await edge.stop();
  }
});

// A choice that costs more is charged: the picker says "+ 40,000₫", and the line, the check and
// everything after them carry it (pilot run 2, N2). A table's line is priced by the device from the
// menu it holds (ADR-0063), so a till that sent the dish's own price alone charged a large pizza at
// the small one's price.
test("a chosen modifier's price is charged on the line and the check", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);

    await page.locator("#menu-search").fill("margherita");
    await page.locator('[data-step="onItem"]').click();
    const large = page.locator('[data-step="chooseModifier"]', { hasText: "Size — 30cm" });
    await expect(large).toContainText("+ 40,000₫");
    await large.click();
    await page.locator('[data-step="confirmItem"]').click();

    // 149,000₫ for the pizza and 40,000₫ for the size.
    const line = page.locator('[data-outcome="line-added"]').first();
    await expect(line.locator('[data-outcome="line-modifiers"]')).toHaveText("Size — 30cm");
    await expect(line).toContainText("189,000₫");
  } finally {
    await edge.stop();
  }
});

// The kitchen marks a dish sold out, every till stops selling it, and the kitchen brings it back.
//
// The cook is usually the first to know something has run out. The board's own panel makes the
// same mark a till makes, so a till showing the menu greys the dish out without reloading, and the
// panel's "sold out now" list is where the kitchen brings it back from.
test("the kitchen marks a dish sold out, a till stops selling it, and the kitchen brings it back", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    await pair(till, edge);
    await signIn(till, edge);
    // The till hears the board's mark on its live link, which retries on a back-off after its
    // refusal before pairing and can still be down here; a mark made then never reaches it.
    await expect(till.getByText("Store connected")).toBeVisible();
    await seatTable(till);
    await till.locator("#menu-search").fill("iced");
    const tea = till.locator('[data-step="onItem"]');
    await expect(tea).toBeEnabled();

    const board = await context.newPage();
    await board.goto(`${edge.baseURL}/kds`);
    await board.locator('[data-step="openSoldOut"]').click();
    await expect(board.locator('[data-outcome="kds-sold-out"]')).toHaveCount(0);
    await board.locator("#kds-sold-out-search").fill("iced");
    await expect(board.locator('[data-step="markSoldOut"]')).toHaveCount(1);
    await board.locator('[data-step="markSoldOut"]').click();
    await expect(board.locator('[data-outcome="kds-sold-out"]')).toHaveText("Iced tea");
    // Off the list of dishes to mark once it is marked.
    await expect(board.locator('[data-step="markSoldOut"]')).toHaveCount(0);

    // The till, without reloading.
    await expect(tea).toBeDisabled();
    await expect(till.locator('[data-outcome="item-sold-out"]')).toHaveText("Sold out");

    await board.locator('[data-step="bringBack"]').click();
    await expect(board.locator('[data-outcome="kds-sold-out"]')).toHaveCount(0);
    await expect(tea).toBeEnabled();
  } finally {
    await edge.stop();
  }
});

// Guests move to another table and take their order with them.
//
// A till, the floor on a second device, and the kitchen board share one edge. The server moves
// table 1's guests to table 2 with a drink already sent. The till lands on table 2, says where the
// guests came from, and still has the drink; the floor shows table 1 waiting to be cleared and
// table 2 seated; the kitchen's ticket for the drink names table 2. The other two devices follow
// without reloading. Moving them back is refused until table 1 is cleared, as the floor tells it.
test("guests move to another table with their order, and every screen follows them", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    await pair(till, edge);
    await signIn(till, edge);
    const floor = await context.newPage();
    await floor.goto(`${edge.baseURL}/`);
    await expect(floor.locator('[data-outcome="floor"]')).toBeVisible();
    const board = await context.newPage();
    await board.goto(`${edge.baseURL}/kds`);

    await seatTable(till);
    await expect(till.locator('[data-outcome="order-open"]')).toHaveText("Table 1");
    await addByName(till, "iced");
    await sendOrder(till);
    const ticket = board.locator('[data-step="onBump"]');
    await expect(ticket).toContainText("Table 1");

    await till.locator('[data-step="openMove"]').click();
    const offered = till.locator('[data-step="moveTo"]');
    await expect(offered.first()).toBeVisible();
    // Only the free tables: never the table they are at.
    await expect(offered.filter({ hasText: "Table 1" })).toHaveCount(0);
    await offered.filter({ hasText: "Table 2" }).click();

    await expect(till.locator('[data-outcome="order-open"]')).toHaveText("Table 2");
    await expect(till.locator('[data-outcome="table-moved"]')).toHaveText("Moved from table 1");
    await expect(till.locator('[data-outcome="line-fired"]')).toHaveCount(1);

    const card = (label) => floor.locator('[data-step="onCard"]', { hasText: `Table ${label}` });
    await expect(card(1)).toContainText("Needs cleaning");
    await expect(card(2)).toContainText("Occupied");
    await expect(ticket).toContainText("Table 2");

    // Table 1 is not free until somebody clears it, so it is not offered as a way back.
    await till.locator('[data-step="openMove"]').click();
    await expect(offered.first()).toBeVisible();
    await expect(offered.filter({ hasText: "Table 1" })).toHaveCount(0);
    await card(1).click();
    await expect(card(1)).toContainText("Free");
    await expect(offered.filter({ hasText: "Table 1" })).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// Marking a dish sold out paints first (ADR-0196 decision 1): it shows sold out before the edge
// answers. A refusal puts it back with its sentence (substituted: the real edge marks any dish it
// knows); a mark whose answer is lost is put back and the menu is read again, which finds it marked.
test("a dish marked sold out shows at once, a refusal puts it back, a lost answer reads the menu", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await page.locator('[data-step="startMarking"]').click();
    await page.locator("#menu-search").fill("iced");
    const mark = page.locator('[data-step="toggleSoldOut"]');

    await refusePost(page, "**/api/menu/*/sold-out", "TRANSITION_REFUSED");
    await mark.click();
    await expect(page.getByRole("alert").first()).toContainText("That is not possible right now");
    await expect(page.locator('[data-outcome="item-sold-out"]')).toHaveCount(0);
    await page.unroute("**/api/menu/*/sold-out");

    const held = await holdPost(page, "**/api/menu/*/sold-out");
    await mark.click();
    await held.sent;
    await expect(page.locator('[data-outcome="item-sold-out"]')).toHaveText("Sold out");
    const answered = page.waitForResponse((response) => response.url().endsWith("/sold-out"));
    held.release();
    await answered;

    let restores = 0;
    await page.route("**/api/menu/*/restore", async (route) => {
      restores += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await mark.click();
    await expect(page.locator('[data-outcome="item-sold-out"]')).toHaveCount(0);
    expect(restores).toBe(1);
  } finally {
    await edge.stop();
  }
});

// Clearing a table paints first: the card shows free at once. A clean whose answer never reached the
// edge is put back and the floor is read again, which still needs cleaning; a lost answer is read
// again and found made.
test("a table cleared shows free at once, and a lost or unsent clean is read again", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aSettledTable(page);
    await navigateTo(page, "/");
    const card = page.locator('[data-step="onCard"]', { hasText: "Table 1" });
    await expect(card).toContainText("Needs cleaning");

    await page.route("**/api/tables/*/clean", (route) => route.abort("connectionreset"));
    await card.click();
    await expect(page.getByRole("alert").first()).toBeVisible();
    await expect(card).toContainText("Needs cleaning");
    await page.unroute("**/api/tables/*/clean");

    let cleans = 0;
    await page.route("**/api/tables/*/clean", async (route) => {
      cleans += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await card.click();
    await expect(card).toContainText("Free");
    expect(cleans).toBe(1);
  } finally {
    await edge.stop();
  }
});

// Giving a table back paints first: the floor holds it free at once. A refusal puts it back seated
// with its sentence (substituted: the real edge releases any table with nothing sold); a release
// whose answer is lost is read again and found made.
test("a table given back shows free at once, a refusal keeps it seated, a lost answer is read again", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    const release = page.locator('[data-step="releaseTable"]');
    await refusePost(page, "**/api/tables/*/release", "TRANSITION_REFUSED");
    await release.click();
    await expect(page.getByRole("alert").first()).toContainText("That is not possible right now");
    await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
    await page.unroute("**/api/tables/*/release");

    let releases = 0;
    await page.route("**/api/tables/*/release", async (route) => {
      releases += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await release.click();
    await navigateTo(page, "/");
    await expect(page.locator('[data-step="onCard"]', { hasText: "Table 1" })).toContainText("Free");
    expect(releases).toBe(1);
  } finally {
    await edge.stop();
  }
});

// Opening a bill paints first: the table waits for payment at once. An opening whose answer is lost
// is read again and found made, so the next Take payment finds that bill rather than asking again.
test("a bill opened whose answer is lost is read again, and is not opened twice", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
    let opens = 0;
    await page.route("**/api/tables/*/bill", async (route) => {
      if (route.request().method() !== "POST") {
        await route.fallback();
        return;
      }
      opens += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="takePayment"]').click();
    await expect(page.getByRole("alert").first()).toBeVisible();
    await navigateTo(page, "/");
    await expect(page.locator('[data-step="onCard"]', { hasText: "Table 1" })).toContainText("Awaiting payment");
    await page.locator('[data-step="onCard"]', { hasText: "Table 1" }).click();
    await page.locator('[data-step="takePayment"]').click();
    await expect(page.locator('[data-step="payCash"]')).toBeEnabled();
    expect(opens).toBe(1);
  } finally {
    await edge.stop();
  }
});

// A table seated by mistake goes back to the floor, and every till follows (ADR-0163).
//
// Before this a seated table left the floor only by a bill, and a bill on nothing could never be
// paid, so the table stayed taken. With nothing sold on it, the order screen offers *Release table*
// where *Take payment* would be; once a dish is on it, the button is *Take payment* again, and a
// line voided before it was sent is nothing sold. The floor on a second device follows without
// reloading, and a reload still has the table free.
test("a table seated by mistake is released, and every till follows", async ({ context }) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    await pair(till, edge);
    await signIn(till, edge);
    const floor = await context.newPage();
    await floor.goto(`${edge.baseURL}/`);
    await expect(floor.locator('[data-outcome="floor"]')).toBeVisible();
    const card = floor.locator('[data-step="onCard"]', { hasText: "Table 1" });

    await seatTable(till);
    await expect(card).toContainText("Occupied");
    const release = till.locator('[data-step="releaseTable"]');
    const takePayment = till.locator('[data-step="takePayment"]');
    await expect(release).toBeVisible();
    await expect(takePayment).toHaveCount(0);

    // A dish on the table is something sold: the table pays or voids it first.
    await addItem(till);
    await expect(release).toHaveCount(0);
    await expect(takePayment).toBeVisible();
    await till.locator('[data-step="askVoid"]').first().click();
    await till.locator('[data-step="voidReason"]').first().click();
    await expect(till.locator('[data-outcome="line-voided"]')).toHaveCount(1);
    await expect(release).toBeVisible();

    await release.click();
    await till.waitForURL((url) => url.pathname === "/");
    const mine = till.locator('[data-step="onCard"]', { hasText: "Table 1" });
    await expect(mine).toContainText("Free");
    await expect(card).toContainText("Free");
    await expect(card.locator('[data-outcome="table-seated"]')).toHaveCount(0);

    await floor.reload();
    await expect(floor.locator('[data-outcome="floor"]')).toBeVisible();
    await expect(card).toContainText("Free");
  } finally {
    await edge.stop();
  }
});

// Not a flow: what the two declared reprints cannot say. Those prove the taps reach a copy; this
// proves each copy is **counted**, and by the edge rather than by the screen (ADR-0164). Two presses
// on the pay screen and one from Today are three copies of the one receipt, Today names the bill by
// its receipt number and its table, and a reload still says three, because the count comes back
// from the log. The demo store has no printer, which is the point too: a copy is counted when it is
// asked for, whatever the paper does.
test("every copy of a receipt is counted, and the count is the edge's", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aSettledTable(page);
    const press = async (step) => {
      const answered = page.waitForResponse(
        (response) =>
          response.url().endsWith("/receipt/reprint") && response.request().method() === "POST",
      );
      await page.locator(`[data-step="${step}"]`).click();
      expect((await answered).status()).toBe(200);
    };
    await press("printAgain");
    await press("printAgain");
    await expect(page.locator('[data-outcome="receipt-reprinted"]')).toHaveText(
      "No printer is set up for this store",
    );

    await navigateTo(page, "/today");
    const bill = page.locator('[data-step="chooseBill"]');
    await expect(bill).toHaveCount(1);
    await expect(bill).toContainText("Receipt #1");
    await expect(bill).toContainText("Table 1");
    const copies = page.locator('[data-outcome="receipt-copies"]');
    await expect(copies).toHaveText("2 copies");
    await bill.click();
    await press("reprintReceipt");
    await expect(copies).toHaveText("3 copies");

    await page.reload();
    await expect(copies).toHaveText("3 copies");
  } finally {
    await edge.stop();
  }
});

// A print says it is on its way from the tap and takes no second one (ADR-0196 decision 1). A print
// the edge refuses says why; a print whose answer is lost cannot be read back, so it is said not
// confirmed, and never sent again by the till (ADR-0204 decision 5). The refusal is substituted:
// the demo store prints a pre-bill for any order with a line on it.
test("a pre-bill says it is printing at once, holds no payment, a refusal says why, and a lost answer is not confirmed", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    const button = page.locator('[data-step="printPreBill"]');

    await refusePost(page, "**/check/print", "NOTHING_TO_PRINT");
    await button.click();
    await expect(page.getByRole("alert").first()).toContainText("There is nothing on this order to print yet.");
    await page.unroute("**/check/print");

    const held = await holdPost(page, "**/check/print");
    await button.click();
    await held.sent;
    await expect(button).toHaveText("Sending…");
    await expect(button).toBeDisabled();
    // A print changes no money: Take payment does not wait for the printer.
    await expect(page.locator('[data-step="takePayment"]')).toBeEnabled();
    held.release();
    await expect(button).toHaveText("Print pre-bill");

    let prints = 0;
    await page.route("**/check/print", async (route) => {
      prints += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await button.click();
    await expect(page.getByRole("alert").first()).toContainText("The print is not confirmed");
    expect(prints).toBe(1);
  } finally {
    await edge.stop();
  }
});

// A copy of a receipt whose answer is lost is said not confirmed and not sent again; the edge, which
// made it, counts it, and Today shows the edge's count.
test("a receipt copy whose answer is lost is said not confirmed, and the edge's count shows it made once", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aSettledTable(page);
    let reprints = 0;
    await page.route("**/receipt/reprint", async (route) => {
      reprints += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="printAgain"]').click();
    await expect(page.getByRole("alert").first()).toContainText("The print is not confirmed");
    expect(reprints).toBe(1);
    await page.unroute("**/receipt/reprint");

    await navigateTo(page, "/today");
    await expect(page.locator('[data-outcome="receipt-copies"]')).toHaveText("1 copy");
  } finally {
    await edge.stop();
  }
});

// A paid bill voided whole from Today (ADR-0184), which the declared flow cannot replay because the
// approver is typed mid-flow. The demo employee holds every permission, so they approve their own
// void, as they do the bill void before payment. The panel says what to hand back, the row says
// Voided and offers no second void, and a reload still says so, because the edge lists it voided.
test("a paid bill is voided whole from today's bills, and stays voided", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aSettledTable(page);

    await navigateTo(page, "/today");
    const bill = page.locator('[data-step="chooseBill"]');
    await expect(bill).toHaveCount(1);
    await bill.click();
    await page.locator('[data-step="askVoidPaidBill"]').click();
    await page.locator("#void-paid-bill-approver-code").fill(edge.staffCode);
    await page.locator("#void-paid-bill-approver-pin").fill(edge.staffPin);
    const answered = page.waitForResponse(
      (response) =>
        response.url().endsWith("/void_settled") && response.request().method() === "POST",
    );
    await page.locator('[data-step="voidPaidBillReason"]').first().click();
    expect((await answered).status()).toBe(200);

    const outcome = page.locator('[data-outcome="paid-bill-voided"]');
    await expect(outcome).toContainText("Bill voided. Receipt #1 keeps its number.");
    await expect(outcome).toContainText("in cash.");
    await expect(bill).toContainText("Voided");
    await expect(page.locator('[data-step="askVoidPaidBill"]')).toHaveCount(0);

    await page.reload();
    await expect(bill).toContainText("Voided");
    await bill.click();
    await expect(page.locator('[data-step="askVoidPaidBill"]')).toHaveCount(0);
    await expect(page.locator('[data-step="reprintReceipt"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// On a phone or a tablet, a dish's choices open on screen, where the thumb that tapped it is.
//
// Below a terminal the menu is under the bill, and the picker was drawn in the bill's column, so it
// opened wherever the bill ended. A long menu, or a screen on its side, puts that out of sight: the
// server tapped the pizza and saw nothing happen. It is a sheet from the bottom of the screen there
// now. The test does not click its way to the choices, because a click scrolls to what it clicks;
// it asks whether they are on screen the moment the pizza is tapped. A phone on its side is the case
// the demo's five-dish menu can show failing. Upright, that menu is too short to push the end of
// the bill out of view, so the upright case guards the sheet itself.
// A phone on its side is as wide as a tablet, so it gets the tablet's layout, and its bill waits
// behind the bar at the bottom of the screen until it is tapped.
for (const device of [
  { name: "a phone", width: 390, height: 844, billBar: false },
  { name: "a phone on its side", width: 844, height: 390, billBar: true },
]) {
  test(`on ${device.name} a dish's choices open on screen, where the thumb is`, async ({ page }) => {
    const edge = await startEdge();
    try {
      await page.setViewportSize({ width: device.width, height: device.height });
      await pair(page, edge);
      await signIn(page, edge);
      await seatTable(page);
      // A table with a few dishes on it already, as it is mid-service.
      for (const query of ["salad", "iced", "water", "pho"]) {
        await addByName(page, query);
      }

      await page.locator("#menu-search").fill("margherita");
      await page.locator('[data-step="onItem"]').click();
      const confirm = page.locator('[data-step="confirmItem"]');
      await expect(confirm).toBeInViewport();
      await expect(page.locator('[data-step="chooseModifier"]').first()).toBeInViewport();

      await page.locator('[data-step="chooseModifier"]').first().click();
      await confirm.click();
      await expect(confirm).toHaveCount(0);
      if (device.billBar) {
        await page.locator('[data-outcome="bill-bar"]').click();
      }
      await expect(page.locator('[data-outcome="line-modifiers"]').first()).toBeVisible();
    } finally {
      await edge.stop();
    }
  });
}

// On a tablet the menu is the screen and the bill slides up from the bottom of it.
//
// `docs/ui-ux.md` §1 principle 9 asks a tablet for "large item grid, bill slides up; usable
// one-handed". The order screen drew a tablet like a phone instead: the bill first and the menu
// under it, so a server scrolled past everything already ordered to reach the next dish. Now the
// menu is what a tablet shows, and the bill waits in a bar at the bottom of the screen, saying what
// is on it and what it comes to, with Send and Take payment one tap away; a tap on the bar slides
// the whole bill up, and another closes it.
test("on a tablet the menu fills the screen, and the bill slides up from the bottom", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.setViewportSize({ width: 768, height: 1024 });
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);

    const bar = page.locator('[data-outcome="bill-bar"]');
    await expect(page.locator("#menu-search")).toBeInViewport();
    await expect(bar).toBeInViewport();
    await expect(bar).toHaveAttribute("aria-expanded", "false");
    await expect(bar).toContainText("nothing yet");

    // A dish goes on from the menu with the bill closed: the bar counts it and prices the bill.
    await addByName(page, "salad");
    await expect(bar).toContainText("1 item");
    await expect(bar).toContainText("97,900");
    await expect(page.locator('[data-outcome="line-added"]').first()).toBeHidden();
    await expect(page.locator('[data-step="fireOrder"]')).toBeInViewport();
    await expect(page.locator('[data-step="takePayment"]')).toBeInViewport();

    // A tap on the bar slides the bill up, and another closes it.
    await bar.click();
    await expect(bar).toHaveAttribute("aria-expanded", "true");
    await expect(page.locator('[data-outcome="line-added"]').first()).toBeVisible();
    await expect(page.locator('[data-outcome="check-total"]')).toBeVisible();
    await bar.click();
    await expect(page.locator('[data-outcome="line-added"]').first()).toBeHidden();

    // And the order goes to the kitchen from the closed bar.
    await page.locator('[data-step="fireOrder"]').click();
    await expect(page.locator('[data-step="fireOrder"]')).toBeDisabled();
  } finally {
    await edge.stop();
  }
});

// On a tablet a refusal is seen with the bill open.
//
// The till shows a refusal at the top of the screen, and on a short tablet screen the open bill
// covers the top of it. So a tablet shows the refusal in the bill's panel, under the bar. The test
// checks that nothing covers it (a trial tap at it would land on it), because a message under the
// panel is still "in the viewport" and would pass a check that only asked that.
test("on a tablet a refusal shows under the bill's bar, where the open bill does not cover it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.setViewportSize({ width: 800, height: 600 });
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    for (const query of ["salad", "iced", "water", "pho"]) {
      await addByName(page, query);
    }
    await page.locator('[data-outcome="bill-bar"]').click();

    // The store refuses the next change.
    await page.route("**/api/lines/*/quantity", (route) =>
      route.fulfill({ status: 503, contentType: "text/plain", body: "unavailable" }),
    );
    await page.locator('[data-step="setQuantity"]').first().click();

    const refusal = page.getByRole("alert");
    await expect(refusal).toBeVisible();
    await refusal.click({ trial: true, timeout: 3000 });
  } finally {
    await edge.stop();
  }
});

// A tip on a bill that is not a round number still settles.
//
// The pay screen computed its tip keys with `(total * percent) / 100` — a float division on a money
// path. For as long as every price in the fixture was a round thousand and the rate was ten percent
// exclusive, every total was a multiple of a hundred and every key came out whole, so neither the
// screen nor this gate had anything to show. Off that grid the 5% key of a 304,733₫ bill is
// 15,236.65; `Money.amount_minor` is an `i64` and the edge's deserializer refuses it outright.
//
// The cashier's experience is what makes it worth a test rather than a lint. The keys *look* right
// — the formatter truncates on the way to the screen, so 2,172.5 draws as "2,172₫". Nothing fails
// until the settle, and what fails there is the generic store error, with the guest's money already
// on the counter and no hint that the tip button was the cause.
//
// So this walks the whole tipped settle rather than reading the key: tap 5%, tender, pay, and the
// bill must be settled at the end. The iced tea is priced at 39,500₫ for exactly this — see the
// fixture note in `crates/pos-edge/src/demo.rs`.
test("a tip on a bill that is not a round number still settles", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);

    // The one demo item priced off a round thousand. 39,500₫ plus ten percent is 43,450₫, and five
    // percent of that is 2,172.5 — the half đồng this test exists for.
    await page.locator("#menu-search").fill("tea");
    await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
    await page.locator('[data-step="onItem"]').click();
    await expect(page.locator('[data-outcome="line-added"]').first()).toBeVisible();

    await sendThenTakePayment(page);
    // Asserted, not merely awaited: the tip keys are a share of this figure, so a test that tapped
    // before the check landed would take five percent of nothing and prove the opposite of the point.
    await expect(page.getByText("43,450₫", { exact: true })).toBeVisible();

    await page.locator('[data-step="setTip"]').first().click();
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();

    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// Where the country rounds its cash, the tip keys round with it.
//
// `cash_rounding_increment` has been in ADR-0105 since it was written, and no fixture ever published
// one — so the posture had no browser gate over it at all. `POS_DEMO_PROFILE=cash-rounding`
// publishes Vietnam's own increment, 1,000 đồng, which is the smallest note in circulation.
//
// The salad is 89,000₫; ten percent exclusive tax makes 97,900₫, which the edge rounds to 98,000₫.
// Five, ten and fifteen percent of that are 4,900, 9,800 and 14,700 — whole numbers, and still three
// amounts no guest can put on a table. Snapped they are 5,000, 10,000 and 15,000, which is what
// somebody actually leaves.
//
// The amounts are asserted on the buttons rather than the arithmetic being restated: the row carries
// the figure and nothing else, so what the button says is the whole of what the cashier has to go on.
//
// They are grouped `98.000₫` rather than `98,000₫` because this fixture publishes Vietnam's own
// marks ([ADR-0136](../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)) and the till
// now reads them. That is the assertion doing double duty: the figures below are the arithmetic, and
// their punctuation is the proof that the screen takes its typography from the store rather than
// from a compiled-in `en-US`.
test("a store whose country rounds its cash offers tips a guest can hand over", async ({ page }) => {
  const edge = await startEdge("cash-rounding");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);

    await sendThenTakePayment(page);
    // The rounded total, so the keys below are a share of a figure that has already been rounded
    // once — and so a check that has not landed cannot pass this test with three zeroes.
    await expect(page.getByText("98.000₫", { exact: true })).toBeVisible();

    const keys = page.locator('[data-step="setTip"]');
    await expect(keys).toHaveText(["5.000₫", "10.000₫", "15.000₫"]);

    await keys.first().click();
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// On a bill too small for the increment, the snap stands down.
//
// The other half of the rule above, and the reason it is a rule rather than a rounding call. The
// bottled water is 9,000₫, which taxes to 9,900₫ and rounds to 10,000₫. Five, ten and fifteen
// percent of that snap to 1,000, 1,000 and 2,000: two buttons reading the same amount on a row whose
// buttons carry an amount and nothing else, so a cashier cannot tell which is which — and a 5% key
// that is really 10%.
//
// So the till shows the exact shares instead: 500₫, 1.000₫ and 1.500₫. Less tidy, and the only set
// of three a person can choose between.
test("a tiny bill keeps its exact tip keys rather than collapsing them", async ({ page }) => {
  const edge = await startEdge("cash-rounding");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);

    await page.locator("#menu-search").fill("water");
    await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
    await page.locator('[data-step="onItem"]').click();
    await expect(page.locator('[data-outcome="line-added"]').first()).toBeVisible();

    await sendThenTakePayment(page);
    await expect(page.getByText("10.000₫", { exact: true })).toBeVisible();

    await expect(page.locator('[data-step="setTip"]')).toHaveText(["500₫", "1.000₫", "1.500₫"]);
  } finally {
    await edge.stop();
  }
});

// A store's own tip keys (ADR-0160 decision 2).
//
// `POS_DEMO_PROFILE=tender-keys` publishes keys of ten, nothing and twenty percent, so the row offers
// two keys in the store's order, without the one it hid: 9,790₫ and 19,580₫ of the salad's 97,900₫.
// Then the edge says the store hides every key, and the row is not drawn at all — with no amount to
// type, a lone "No tip" would offer nothing — while the cash tender still takes the money.
test("a store's own tip keys are offered in its order, and a store that hides them all has no tip row", async ({
  page,
}) => {
  const edge = await startEdge("tender-keys");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    const table = new URL(page.url()).pathname.split("/")[2];

    await sendThenTakePayment(page);
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveText(["9,790₫", "19,580₫"]);
    await expect(page.getByRole("heading", { name: "Tip", exact: true })).toBeVisible();

    let hidden = 0;
    await page.route("**/api/menu*", async (route) => {
      const response = await fetchWhole(route);
      const book = await response.json();
      book.tip_percents = [];
      hidden += 1;
      await route.fulfill({ response, json: book });
    });
    await page.goto(`${edge.baseURL}/table/${table}/pay`);
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await expect(page.locator('[data-step="setTender"]').first()).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveCount(0);
    await expect(page.getByRole("heading", { name: "Tip", exact: true })).toHaveCount(0);
    expect(hidden, "the price book the till read hid every key").toBeGreaterThan(0);

    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A store that sets no tip keys or split ways, and an edge too old to send them, both offer what the
// till always offered: tip keys of five, ten and fifteen percent, 4,895₫, 9,790₫ and 14,685₫ of the
// salad's 97,900₫, and an even split of two to six guests.
//
// The second half is the one only a browser can see. An edge from before the settings sends no
// `tip_percents` and no `split_ways_max`, and the till must fall back to its own rather than offer
// none. The price book is read from the real edge and the fields taken out on the way, which is what
// an older edge's answer is.
test("a store that sets nothing, and an edge too old to send the settings, offer today's keys and split", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    const table = new URL(page.url()).pathname.split("/")[2];
    const today = ["4,895₫", "9,790₫", "14,685₫"];
    const twoToSix = ["2", "3", "4", "5", "6"];

    await sendThenTakePayment(page);
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveText(today);
    await expect(page.locator('[data-step="splitEvenly"]')).toHaveText(twoToSix);

    let older = 0;
    await page.route("**/api/menu*", async (route) => {
      const response = await fetchWhole(route);
      const book = await response.json();
      delete book.tip_percents;
      delete book.split_ways_max;
      older += 1;
      await route.fulfill({ response, json: book });
    });
    await page.goto(`${edge.baseURL}/table/${table}/pay`);
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveText(today);
    await expect(page.locator('[data-step="splitEvenly"]')).toHaveText(twoToSix);
    expect(older, "the price book the till read came from the older edge").toBeGreaterThan(0);
  } finally {
    await edge.stop();
  }
});

// A store that splits a bill more ways keeps the split in one row (ADR-0160 decision 2).
//
// `POS_DEMO_PROFILE=tender-keys` lets an even split go up to twelve guests, the most a store may
// set: eleven keys, which a phone's width cannot hold. A second line of them would push the tenders
// down on every bill, split or not, so the row scrolls sideways instead, as the kitchen board's
// station tabs do: every key on one line, the last one reached by scrolling the row, and the first
// tender still on a phone's screen with the page where it opened. Then the edge says eight, and the
// row offers two to eight the same way.
test("a store that splits up to twelve ways keeps one row that scrolls, and the tenders on screen", async ({
  page,
}) => {
  const edge = await startEdge("tender-keys");
  try {
    await page.setViewportSize({ width: 390, height: 844 });
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    const table = new URL(page.url()).pathname.split("/")[2];
    await sendThenTakePayment(page);
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();

    const ways = page.locator('[data-step="splitEvenly"]');
    const offersInOneRow = async (most) => {
      await expect(ways).toHaveText(Array.from({ length: most - 1 }, (_, index) => `${index + 2}`));
      const lines = await ways.evaluateAll(
        (keys) => new Set(keys.map((key) => Math.round(key.getBoundingClientRect().top))).size,
      );
      expect(lines, "every guest count sits on one line").toBe(1);
      await expect(page.locator('[data-step="setTender"]').first()).toBeInViewport();
    };

    await offersInOneRow(12);
    const row = await ways
      .first()
      .evaluate((key) => ({ content: key.parentElement.scrollWidth, box: key.parentElement.clientWidth }));
    expect(row.content, "the row scrolls sideways rather than wrapping").toBeGreaterThan(row.box);
    await ways.last().click();
    await expect(ways.last()).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator('[data-outcome="share-due"]')).toBeVisible();

    let rewritten = 0;
    await page.route("**/api/menu*", async (route) => {
      const response = await fetchWhole(route);
      const book = await response.json();
      book.split_ways_max = 8;
      rewritten += 1;
      await route.fulfill({ response, json: book });
    });
    await page.goto(`${edge.baseURL}/table/${table}/pay`);
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await offersInOneRow(8);
    expect(rewritten, "the price book the till read said eight").toBeGreaterThan(0);
  } finally {
    await edge.stop();
  }
});

// A bill larger than the largest note takes any amount the guest hands over.
//
// The quick-cash keys were "every note at least as large as the bill". That is the same answer while
// the bill is smaller than the largest note, and no answer at all above it: Vietnam's largest note is
// 500,000₫, so a family dinner offered "Exact" and nothing else, and a cashier handed 1,300,000₫ could
// neither record it nor see the change. The keys are now the smallest pile of each note that covers
// the bill, and "Other amount" takes whatever pile no key names.
//
// The demo store publishes no notes, and an empty list means "the exact amount only", so this hands
// the till Vietnam's own six — the country pack's list — by changing that one field of
// `GET /api/locale`, as the exponent test below does with its field. Four pizzas are 596,000₫ plus
// ten percent: 655,600₫. Piles of 500,000₫, 200,000₫, 100,000₫ and 20,000₫ notes cover it at
// 1,000,000, 800,000, 700,000 and 660,000; before the change the row was "Exact" alone.
//
// Then the pad: a figure short of the bill says by how much and cannot be taken, and one that covers
// it settles with the change the screen showed.
test("a bill larger than the largest note takes any amount handed over", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      await route.fulfill({
        response,
        json: {
          ...body,
          cash_denominations: [10_000, 20_000, 50_000, 100_000, 200_000, 500_000],
        },
      });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    for (let pizza = 0; pizza < 4; pizza += 1) {
      await addItemWithAChoice(page);
    }

    await sendThenTakePayment(page);
    await expect(page.getByText("655,600₫", { exact: true })).toBeVisible();
    await expect(page.locator('[data-step="setTender"]')).toHaveText([
      "Exact",
      "660,000₫",
      "700,000₫",
      "800,000₫",
      "1,000,000₫",
    ]);

    await page.locator('[data-step="typeTender"]').click();
    const keypad = page.locator('[data-step="tenderKeypad"]');
    for (const digit of ["6", "0", "0", "0", "0", "0"]) {
      await keypad.getByRole("button", { name: digit, exact: true }).click();
    }
    await expect(page.locator('[data-outcome="typed-tender"]')).toHaveText("600,000₫");
    await expect(page.getByText("Short by 55,600₫")).toBeVisible();
    await expect(page.locator('[data-step="payCash"]')).toBeDisabled();

    await keypad.getByRole("button", { name: "Clear the amount" }).click();
    for (const digit of ["1", "3", "0", "0", "0", "0", "0"]) {
      await keypad.getByRole("button", { name: digit, exact: true }).click();
    }
    await expect(page.locator('[data-outcome="typed-tender"]')).toHaveText("1,300,000₫");
    await expect(page.getByText("644,400₫", { exact: true })).toBeVisible();

    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.getByText("644,400₫", { exact: true })).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// The store's language is read only after sign-in, so the screens before it used to be English on a
// Vietnamese store after every reload (pilot run 2, N8). The till now remembers what the store said,
// and a language the operator chose on the device still wins over it.
test("a till remembers the store's language for the screens before sign-in", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      await route.fulfill({ response, json: { ...body, display_language: "vi-VN" } });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await expect(page.locator("html")).toHaveAttribute("lang", "vi");

    await page.getByRole("button", { name: "Đăng xuất" }).click();
    await expect(page).toHaveURL(/\/signin$/);
    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("lang", "vi");
    await expect(page.getByText("Nhập mã nhân viên và mã PIN để bắt đầu ca làm.")).toBeVisible();

    await page.evaluate(() => localStorage.setItem("pos.locale", "en"));
    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("lang", "en");
    await expect(page.getByText("Enter your staff code and PIN to start your shift.")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A device that has never signed in learns the store's language from the session read, which needs
// no sign-in (pilot run 2, N8): until then a fresh till drew its sign-in screen in English on a
// Vietnamese store, because the language came only from `GET /api/locale`, behind the sign-in.
// The operator's own choice on the device still wins.
test("a fresh device that never signed in shows the store's language", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.route("**/api/session", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      await route.fulfill({ response, json: { ...body, display_language: "vi-VN" } });
    });
    await pair(page, edge);
    await expect(page.locator("html")).toHaveAttribute("lang", "vi");
    await expect(page.getByText("Nhập mã nhân viên và mã PIN để bắt đầu ca làm.")).toBeVisible();

    await page.evaluate(() => localStorage.setItem("pos.locale", "en"));
    await page.reload();
    await expect(page.getByText("Enter your staff code and PIN to start your shift.")).toBeVisible();
    await expect(page.locator("html")).toHaveAttribute("lang", "en");
  } finally {
    await edge.stop();
  }
});

// A bill split three ways settles with each guest's own tender, and the cash guest's change.
//
// The edge settles a bill in one step, with payments that add up to the total exactly, so the pay
// screen holds each guest's share until the last one lands. A share is what is left divided by the
// shares still to pay, rounded up: four pizzas are 655,600₫, and three ways that is 218,534₫, then
// 218,533₫ twice, which add back up to the bill to the đồng.
//
// The first guest's share is taken and then given back — "Undo" — to prove the shares are worked out
// again from what is left rather than remembered. Then QR, card, and the third guest pays cash with
// a 250,000₫ pile, which the quick keys offer for a 218,533₫ share once the till has Vietnam's notes
// (injected as in the test above). The settled panel's change is that guest's 31,467₫: the change
// is summed over every payment on the bill, and only the cash share has any.
test("a bill split three ways settles with each guest's own tender, and the cash guest's change", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      await route.fulfill({
        response,
        json: {
          ...body,
          cash_denominations: [10_000, 20_000, 50_000, 100_000, 200_000, 500_000],
        },
      });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    for (let pizza = 0; pizza < 4; pizza += 1) {
      await addItemWithAChoice(page);
    }

    await sendThenTakePayment(page);
    await expect(page.getByText("655,600₫", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Split between 3 guests" }).click();
    const share = page.locator('[data-outcome="share-due"]');
    await expect(share).toHaveText("218,534₫");

    await page.locator('[data-step="payQr"]').click();
    await expect(page.locator('[data-outcome="share-taken"]')).toHaveCount(1);
    await expect(share).toHaveText("218,533₫");
    await page.getByRole("button", { name: "Undo" }).click();
    await expect(page.locator('[data-outcome="share-taken"]')).toHaveCount(0);
    await expect(share).toHaveText("218,534₫");

    await page.locator('[data-step="payQr"]').click();
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="share-taken"]')).toHaveCount(2);
    await expect(share).toHaveText("218,533₫");
    await expect(page.locator('[data-step="setTender"]')).toHaveText([
      "Exact",
      "250,000₫",
      "300,000₫",
      "400,000₫",
      "500,000₫",
    ]);
    await page.locator('[data-step="setTender"]').nth(1).click();
    await page.locator('[data-step="payCash"]').click();

    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.getByText("31,467₫", { exact: true })).toBeVisible();
  } finally {
    await edge.stop();
  }
});

/** The rendered font size of the first element `locator` matches, in pixels. */
async function fontSize(locator) {
  return locator.first().evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize));
}

// The change is what the cashier does next, so it is the largest figure once a cash payment settles,
// at a table and at the counter (UX-15). It was a line of small grey text under the receipt number.
test("the change to hand back is the largest figure once cash settles", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
            // One note, larger than either sale: the key after "Exact" is that note whether or not the
      // amount has landed when it is tapped, so the change is never nothing.
      const notes = [500_000]; await route.fulfill({ response, json: { ...body, cash_denominations: notes } });
    });
    await pair(page, edge);
    await signIn(page, edge);
    for (const reach of [
      async () => {
        await seatTable(page);
        await addItem(page);
        await sendThenTakePayment(page);
      },
      async () => {
        await aWalkInToCharge(page);
        await page.locator('[data-step="charge"]').first().click();
      },
    ]) {
      await reach();
      await page.locator('[data-step="setTender"]').nth(1).click();
      await page.locator('[data-step="payCash"]').click();
      const settled = page.locator('[data-outcome="settled"]');
      await expect(settled).toBeVisible();
      const change = page.locator('[data-outcome="change-due"]');
      await expect(change).toBeVisible();
      expect(await fontSize(change)).toBeGreaterThan(await fontSize(settled));
      await page.locator('a[href="/"]').first().click();
    }
  } finally {
    await edge.stop();
  }
});

// An even split is kept for its bill (UX-5): a reload, or leaving and coming back, finds the share
// already taken still taken, and only the rest owed. It used to be forgotten, and the reopened bill
// was taken whole after a guest had paid their half. Leaving with a share taken asks first.
test("an even split survives a reload and leaving the screen, and leaving asks first", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await page.getByRole("button", { name: "Split between 2 guests" }).click();
    const share = page.locator('[data-outcome="share-due"]');
    await expect(share).toBeVisible();
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="share-taken"]')).toHaveCount(1);
    const rest = await share.textContent();

    await page.reload();
    await expect(page.locator('[data-outcome="share-taken"]')).toHaveCount(1);
    await expect(share).toHaveText(rest ?? "");

    // Leaving asks; staying keeps the screen, leaving keeps the split for when the bill is back.
    page.once("dialog", (dialog) => void dialog.dismiss());
    await page.getByRole("link", { name: "← Order" }).click();
    await expect(page).toHaveURL(/\/pay$/);
    page.once("dialog", (dialog) => void dialog.accept());
    await page.getByRole("link", { name: "← Order" }).click();
    await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
    await page.locator('[data-step="takePayment"]').click();
    await expect(page.locator('[data-outcome="share-taken"]')).toHaveCount(1);
    await expect(share).toHaveText(rest ?? "");

    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A discounted bill spells out its arithmetic (UX-11): the lines before tax, what came off, and the
// edge's tax on what is left, so the rows add up to the total. It used to show the discount and a
// total that was not the old one less the discount, with nothing to say the tax had changed.
test("a discounted bill shows its subtotal, the discount and the tax, and they add up", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await expect(page.locator('[data-outcome="bill-tax"]')).toHaveCount(0);

    await page.locator('[data-step="askDiscount"]').click();
    await page.locator("#discount-amount").fill("10000");
    const code = page.locator("#discount-approver-code");
    if ((await code.count()) > 0) {
      await code.fill(edge.staffCode);
      await page.locator("#discount-approver-pin").fill(edge.staffPin);
    }
    await page.locator('[data-step="discountReason"]').first().click();
    await expect(page.locator('[data-outcome="bill-discounted"]')).toBeVisible();

    const figure = async (selector) =>
      Number((await page.locator(selector).last().textContent()).replace(/[^0-9]/g, ""));
    const subtotal = await figure('[data-outcome="bill-subtotal"] span.tabular-nums');
    const discount = await figure('[data-outcome="bill-discounted"] span.tabular-nums');
    const tax = await figure('[data-outcome="bill-tax"] span.tabular-nums');
    const total = Number(
      (await page.locator("p.text-2xl.tabular-nums").first().textContent()).replace(/[^0-9]/g, ""),
    );
    expect(discount).toBe(10000);
    expect(subtotal - discount + tax).toBe(total);
  } finally {
    await edge.stop();
  }
});

// A table split by item charges each guest their own part, and waits for the last one (ADR-0128).
//
// The guest who only had a drink pays for the drink: the iced tea is split off, its part reads what
// it is for and its own 43,450₫, and it settles by QR. The salad's part is still open, so the table
// is still awaiting payment on the floor — it used to go to cleaning when the first guest paid, with
// half the bill never collected. The way back to the rest is the ordinary one, the table and then
// Take payment, which lands on the salad's part and its 97,900₫. Only once that settles does the
// table want cleaning.
test("a table split by item charges each guest their own part, and waits for the last", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aTableWithTheDrinkSplitOff(page);
    await page.locator('[data-step="payQr"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.locator('[data-step="nextBill"]')).toHaveText("Pay the next bill (1 left)");

    await page.getByRole("button", { name: "Back to floor" }).click();
    const firstDot = page.locator('[data-step="onCard"]').first().locator("span.rounded-full");
    await expect(firstDot).toHaveClass(/bg-awaiting/);

    await page.locator('[data-step="onCard"]').first().click();
    await sendThenTakePayment(page);
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Garden salad");
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.locator('[data-step="nextBill"]')).toHaveCount(0);

    await page.getByRole("button", { name: "Back to floor" }).click();
    await expect(firstDot).toHaveClass(/bg-cleaning/);
  } finally {
    await edge.stop();
  }
});

// A split table's food stays on the kitchen board until its last part is paid.
//
// The board draws the orders still owing money, and a settle took the order off it. With one bill per
// table that was right; with a split table it took the salad off the board when the guest with the
// drink paid, while the salad was still to be made. The order leaves the board with its last part.
test("a split table's food stays on the kitchen board until its last part is paid", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addByName(page, "salad");
    await addByName(page, "iced");
    await sendOrder(page);
    await sendThenTakePayment(page);
    await page.locator('[data-step="splitByItem"]').click();
    await page.locator('[data-step="pickLine"]', { hasText: "Iced tea" }).click();
    await page.locator('[data-step="splitOff"]').click();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Iced tea");
    await page.locator('[data-step="payQr"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();

    await navigateTo(page, "/kds");
    await expect(page.getByText("Garden salad").first()).toBeVisible();

    await navigateTo(page, "/");
    await page.locator('[data-step="onCard"]').first().click();
    await sendThenTakePayment(page);
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Garden salad");
    await page.locator('[data-step="payQr"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await navigateTo(page, "/kds");
    await expect(page.getByText("Garden salad")).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// A table split by seat pays seat by seat, and the table's own dishes are a bill of their own.
//
// The seat each dish was ordered for is on the line, so the split is one tap with nothing to pick.
// Seat 1 had the salad and seat 2 the iced tea; the bottled water was ordered for the table, with no
// seat, and a line cannot be halved — so it becomes a third bill rather than landing on whichever
// seat the code happened to sort last. Each part says whose it is.
test("a table split by seat pays seat by seat, with the table's own dishes as a bill of their own", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    const seats = page.locator('[data-step="chooseSeat"]');
    await seats.nth(0).click();
    await addByName(page, "salad");
    await seats.nth(1).click();
    await addByName(page, "iced");
    // Tapping the chosen seat again clears it: the water is the table's.
    await seats.nth(1).click();
    await addByName(page, "water");

    await sendThenTakePayment(page);
    await expect(page.locator('[data-step="splitBySeat"]')).toHaveText("Split by seat (3 bills)");
    await page.locator('[data-step="splitBySeat"]').click();

    const part = page.locator('[data-outcome="bill-part"]');
    await expect(part).toHaveText("Seat 1 · For: 1 × Garden salad");
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await page.locator('[data-step="payQr"]').click();
    await expect(page.locator('[data-step="nextBill"]')).toHaveText("Pay the next bill (2 left)");
    await page.locator('[data-step="nextBill"]').click();

    await expect(part).toHaveText("Seat 2 · For: 1 × Iced tea");
    await expect(page.getByText("43,450₫", { exact: true })).toBeVisible();
    await page.locator('[data-step="payQr"]').click();
    await page.locator('[data-step="nextBill"]').click();

    await expect(part).toHaveText("For: 1 × Bottled water");
    await expect(page.getByText("9,900₫", { exact: true })).toBeVisible();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.locator('[data-step="nextBill"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// A split made by mistake is put back together: a merge into the bill on screen, which then owes the
// whole table again and settles as one.
test("a split made by mistake is put back together", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aTableWithTheDrinkSplitOff(page);
    await page.getByRole("button", { name: "Put the 2 bills back together" }).click();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveCount(0);
    await expect(page.getByText("141,350₫", { exact: true })).toBeVisible();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.locator('[data-step="nextBill"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for a split: a split the edge committed but whose answer never reached the till is not
// rolled back. The till reads the table's bills again and puts the part it split off on screen.
test("a split whose answer is lost is checked, not rolled back, and shows the part", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addByName(page, "salad");
    await addByName(page, "iced");
    await sendThenTakePayment(page);
    await page.route("**/api/bills/*/split", async (route) => {
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="splitByItem"]').click();
    await page.locator('[data-step="pickLine"]', { hasText: "Iced tea" }).click();
    await page.locator('[data-step="splitOff"]').click();
    await expect(page.locator('[data-outcome="split-pending"]')).toContainText("Checking with the store server");
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Iced tea");
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for a split that never reached the edge: found not made, and only then rolled back.
test("a split that never reached the store server is found not made, and the bill is as it was", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addByName(page, "salad");
    await addByName(page, "iced");
    await sendThenTakePayment(page);
    await page.route("**/api/bills/*/split", (route) => route.abort("connectionreset"));
    await page.locator('[data-step="splitByItem"]').click();
    await page.locator('[data-step="pickLine"]', { hasText: "Iced tea" }).click();
    await page.locator('[data-step="splitOff"]').click();
    await expect(page.locator('[data-outcome="split-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("The bill was not split");
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveCount(0);
    await expect(page.getByText("141,350₫", { exact: true })).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// Safeguard 3 for a split (ADR-0196): the edge's refusal rolls the screen back with its own
// sentence, which the cashier dismisses; nothing is split.
test("a split the store server refuses says why and leaves the bill whole", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addByName(page, "salad");
    await addByName(page, "iced");
    await sendThenTakePayment(page);
    await refusePost(page, "**/api/bills/*/split", "SPLIT_NOT_A_PARTITION");
    await page.locator('[data-step="splitByItem"]').click();
    await page.locator('[data-step="pickLine"]', { hasText: "Iced tea" }).click();
    await page.locator('[data-step="splitOff"]').click();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText(
      "Each item must be on exactly one part of the split.",
    );
    await expect(page.getByRole("alertdialog")).toBeVisible();
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for a merge: committed with its answer lost, the till finds the absorbed bill merged and
// shows the bill whole again.
test("a merge whose answer is lost is checked, and the bill is whole again", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aTableWithTheDrinkSplitOff(page);
    await page.route("**/api/bills/*/merge", async (route) => {
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.getByRole("button", { name: "Put the 2 bills back together" }).click();
    await expect(page.locator('[data-outcome="merge-pending"]')).toBeVisible();
    await expect(page.getByText("141,350₫", { exact: true })).toBeVisible();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveCount(0);
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// Safeguard 3 for a merge: the edge refuses bills of different orders with its own sentence.
test("a merge the store server refuses says why and keeps the parts", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aTableWithTheDrinkSplitOff(page);
    await refusePost(page, "**/api/bills/*/merge", "BILLS_ON_DIFFERENT_ORDERS");
    await page.getByRole("button", { name: "Put the 2 bills back together" }).click();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("Bills on different orders cannot be merged");
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Iced tea");
  } finally {
    await edge.stop();
  }
});

// A till that reloads mid-split still has every part to settle.
//
// The live read named one bill per order, the newest open one. A till that reloaded with both parts
// still owing learned only the salad's; it could settle that and then had no id for the drink, and
// asking for a bill again is refused while one is open — so the rest of the table could not be
// charged from that till. Every open part is on the read now. The reload lands on the pay screen
// itself, which reads what is open before it asks for anything, and offers the parts oldest first.
test("a till that reloads mid-split still has every part to settle", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aTableWithTheDrinkSplitOff(page);

    await page.reload();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Iced tea");
    await expect(page.getByRole("button", { name: "Put the 2 bills back together" })).toBeVisible();
    await page.locator('[data-step="payQr"]').click();
    await expect(page.locator('[data-step="nextBill"]')).toHaveText("Pay the next bill (1 left)");
    await page.locator('[data-step="nextBill"]').click();
    await expect(page.locator('[data-outcome="bill-part"]')).toHaveText("For: 1 × Garden salad");
    await expect(page.getByText("97,900₫", { exact: true })).toBeVisible();
    await page.locator('[data-step="payQr"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.locator('[data-step="nextBill"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// Before anyone signs in, the status bar offers no destinations and no sign-out.
//
// Every destination only bounced back to sign-in, and a till that offers the kitchen before anyone
// has signed in looks as if it has forgotten who you are. The language and theme stay, because the
// first person to sign in may want them.
test("before anyone signs in, the status bar offers no destinations and no sign-out", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await expect(page.locator("header nav")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Sign out" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Language" })).toHaveCount(1);

    await signIn(page, edge);
    await expect(page.locator("header nav")).toHaveCount(1);
    await expect(page.getByRole("button", { name: "Sign out" })).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// On a terminal, the sign-in pad's digits are on screen without scrolling.
//
// Stacked under the fields, the pad's digit row sat below the fold of a 1366x768 till — the
// commonest Windows POS screen — so a PIN needed a scroll before its first digit. From a terminal
// up the fields and the pad sit side by side.
// On a 1024px till the status bar is one row in either language (UX-13). It wrapped to two or three
// rows, pushing every screen down. What does not fit of the destinations scrolls sideways, and the
// screen being shown is scrolled into view, so the cashier on the shift screen sees "Shift" lit.
test("on a 1024px till the status bar is one row, and the screen shown is in view", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.setViewportSize({ width: 1024, height: 768 });
    await pair(page, edge);
    await signIn(page, edge);
    const header = page.locator("header");
    for (const language of ["en", "vi"]) {
      for (const path of ["/", "/shift"]) {
        await navigateTo(page, path);
        const box = await header.boundingBox();
        expect(box?.height ?? 0, `${language} ${path}`).toBeLessThan(96);
        const shown = header.locator(`nav a[href="${path}"]`);
        await expect(shown).toBeInViewport({ ratio: 0.9 });
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(1024);
      await page.getByRole("button", { name: language === "en" ? "Language" : "Ngôn ngữ" }).click();
    }
  } finally {
    await edge.stop();
  }
});

test("on a 1366x768 terminal the sign-in pad's digits are on screen without scrolling", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.setViewportSize({ width: 1366, height: 768 });
    await pair(page, edge);
    const zero = page.locator("#signin-pad").getByRole("button", { name: "0", exact: true });
    await expect(zero).toBeVisible();
    const box = await zero.boundingBox();
    expect(box, "the pad's 0 key has a box").not.toBeNull();
    expect((box?.y ?? 0) + (box?.height ?? 0)).toBeLessThanOrEqual(768);
  } finally {
    await edge.stop();
  }
});

// The Today screen names the shift in the till's language.
//
// The tile lower-cased the wire token and let CSS capitalise it, so a Vietnamese till read "Open"
// in English. It now uses the status bar's own sentences.
test("the Today screen names the shift in the till's language", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/today");
    await expect(page.locator('[data-outcome="today-shift"]')).toHaveText("none open");

    await openShift(page);
    await navigateTo(page, "/today");
    await expect(page.locator('[data-outcome="today-shift"]')).toHaveText("Shift open");
    await page.getByRole("button", { name: "Language" }).click();
    await expect(page.locator('[data-outcome="today-shift"]')).toHaveText("Đang mở ca");
  } finally {
    await edge.stop();
  }
});

// A guest's QR order shows its table by the floor's label, and the queue refreshes itself.
//
// The screen's classes were defined nowhere, so it rendered unstyled; it headed each card with the
// table's id rather than the label on the floor; and it loaded once, so an order placed after the
// screen opened sat unseen. The demo store takes no QR orders (they arrive from the cloud), so the
// queue's one route is answered here: empty on the first read, one order on the next. The page's
// clock is driven so the fifteen-second refresh happens now rather than in fifteen seconds.
test("a guest's QR order shows its table by the floor's label, and the queue refreshes itself", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    const table = new URL(page.url()).pathname.split("/")[2];
    await page.locator('a[href="/"]').first().click();

    let reads = 0;
    await page.route("**/api/orders/awaiting-confirmation", async (route) => {
      reads += 1;
      const money = (amount) => ({ amount_minor: amount, currency_code: "VND" });
      await route.fulfill({
        json: {
          orders:
            reads === 1
              ? []
              : [
                  {
                    order_id: "01J0000000000000000000GUES",
                    table_id: table,
                    items: [
                      {
                        display_name: "Iced tea",
                        quantity: { milli: 2000 },
                        line_total: money(79_000),
                      },
                    ],
                    total: money(86_900),
                  },
                ],
          reject_reasons: [],
        },
      });
    });

    await navigateTo(page, "/guests");
    await expect(page.getByText("No guest orders are waiting.")).toBeVisible();
    await page.clock.fastForward(16_000);

    const card = page.locator('[data-outcome="guest-order"]');
    await expect(card).toHaveCount(1);
    await expect(card.locator("h3")).toHaveText("Table 1");
    const accept = card.getByRole("button", { name: "Send to kitchen" });
    const box = await accept.boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(48);
  } finally {
    await edge.stop();
  }
});

// A guest's QR order that joins the table's order (ADR-0160 item 2, `docs/pos-spec.md` §13). The
// edge writes the guest's line on the guest's own order and then `sales.table.merged`, which puts
// it on the table's, and the till folds the two as they come. The frames are given the next
// positions on the stream, so they are heard as themselves, and the edge is asked nothing about
// them: what is on screen is the till's own fold of the merge, which is what this measures.
test("a guest's line that joins the table's order is on the table's order as it arrives", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    let link = null;
    let last = null;
    const carried = [];
    await page.routeWebSocket(
      (url) => url.pathname === "/ws",
      (socket) => {
        link = socket;
        socket.connectToServer().onMessage((message) => {
          const frame = JSON.parse(String(message));
          const { stream_id: streamId, sequence } = frame;
          last = typeof sequence === "number" ? { streamId, sequence } : last;
          carried.push(frame.event_type);
          socket.send(message);
        });
      },
    );
    await pair(page, edge);
    await signIn(page, edge);
    // The link's first attempt was refused before the device paired, and it retries on a back-off,
    // so it can still be down when the till reaches the floor. A line added then is announced to no
    // one and never carried. Seated and added once the link is open, the line's event is.
    await expect(page.getByText("Store connected")).toBeVisible();
    await seatTable(page);
    await addItem(page);
    const lines = page.locator('[data-outcome="line-added"]');
    await expect(lines).toHaveCount(1);
    // The link has carried the line's own event, so the frames below are the next ones after
    // everything the edge has said, rather than ones that race the line's.
    await expect.poll(() => carried.includes("sales.order_line.added")).toBe(true);
    const live = await page.evaluate(() =>
      fetch("/api/orders/live", {
        headers: { authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}` },
      }).then((response) => response.json()),
    );
    const tables = live[0].order_id;
    const guests = "01J0000000000000000000GUES";
    const next = (step) =>
      last === null ? {} : { stream_id: last.streamId, sequence: last.sequence + step };

    link.send(
      JSON.stringify({
        type: "event",
        event_type: "sales.order_line.added",
        payload: {
          order_id: guests,
          order_line_id: "01J0000000000000000000LINE",
          display_name: "Guest's iced tea",
          quantity: { milli: 1000 },
          line_total: { currency_code: "VND", amount_minor: 39_500 },
          note_present: false,
        },
        ...next(1),
      }),
    );
    // On the guest's own order, which this table does not show.
    await expect(page.getByText("Guest's iced tea")).toHaveCount(0);

    link.send(
      JSON.stringify({
        type: "event",
        event_type: "sales.table.merged",
        payload: { target_order_id: tables, merged_order_id: guests },
        ...next(2),
      }),
    );
    await expect(lines).toHaveCount(2);
    await expect(page.getByText("Guest's iced tea")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A manager voids a bill on a till with no keyboard at all.
//
// The void, the fired-line void and the discount each asked for the manager's badge and PIN in
// plain inputs, which a fixed terminal with its on-screen keyboard switched off cannot fill — so on a
// POS Station or Terminal touch screen none of the three could be approved. The panels now share
// `ApproverFields`, whose credential pad appears when a field takes focus and follows it: letters
// and digits for the badge, digits for the PIN.
//
// Driven with no `fill` after sign-in, like the keyboardless sign-in test, because `fill` is exactly
// what this device class lacks. The demo employee holds every permission, so they approve their own
// void. This is also the first browser run of the bill void, which the declared-flow harness skips
// because its credentials are typed mid-flow.
test("a manager voids a bill on a till with no keyboard, typing on the on-screen pad", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await page.locator('[data-step="askVoidBill"]').click();

    const pad = page.locator("#void-bill-approver-pad");
    await expect(pad).toHaveCount(0);
    await page.locator("#void-bill-approver-code").click();
    for (const key of edge.staffCode.split("")) {
      await pad.getByRole("button", { name: key, exact: true }).click();
    }
    await expect(page.locator("#void-bill-approver-code")).toHaveValue(edge.staffCode);

    await page.locator("#void-bill-approver-pin").click();
    await expect(pad.getByRole("button", { name: "A", exact: true })).toHaveCount(0);
    for (const key of edge.staffPin.split("")) {
      await pad.getByRole("button", { name: key, exact: true }).click();
    }
    await expect(page.locator("#void-bill-approver-pin")).toHaveValue(edge.staffPin);

    await page.locator('[data-step="voidBillReason"]').first().click();
    await expect(page.locator('[data-outcome="bill-voided"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A drawer opened without a sale needs a manager's code and PIN and a reason (ADR-0165), which the
// declared flow cannot type between its taps. So this types them, and reads what the till says:
// the demo store has no drawer marked, so the opening is recorded and the till says to use the key.
test("a manager opens the drawer without a sale, and the till says what came of it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");
    await page.locator('[data-step="askOpenDrawer"]').click();
    await expect(page.locator('[data-step="drawerReason"]').first()).toBeDisabled();
    await page.locator("#no-sale-approver-code").fill(edge.staffCode);
    await page.locator("#no-sale-approver-pin").fill(edge.staffPin);
    await page.locator('[data-step="drawerReason"]').first().click();
    await expect(page.locator('[data-outcome="drawer-opened"]')).toBeVisible();
    await expect(page.locator('[data-outcome="drawer-opened"]')).toContainText("key");
  } finally {
    await edge.stop();
  }
});

// Cash paid in and out is the edge's to add up (ADR-0165): the shift shows the totals, a reload
// keeps them, and the close expects the float plus what came in less what went out.
test("cash paid in and out moves what the close expects, and a reload keeps it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);

    await page.locator("#movement-amount").fill("20000");
    await page.locator('[data-step="askPaidIn"]').click();
    await page.locator('[data-step="movementReason"]').first().click();
    await expect(page.locator('[data-outcome="cash-moved"]')).toBeVisible();

    await page.locator("#movement-amount").fill("5000");
    await page.locator('[data-step="askPaidOut"]').click();
    await page.locator('[data-step="movementReason"]').first().click();
    await expect(page.locator('[data-outcome="cash-totals"]')).toContainText("20");
    await expect(page.locator('[data-outcome="cash-totals"]')).toContainText("5");

    await page.reload();
    await expect(page.locator('[data-outcome="cash-totals"]')).toBeVisible();

    // 100,000 float + 20,000 in - 5,000 out, and no sales.
    await page.locator("#count").fill("115000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await page.locator('[data-step="closeShift"]').click();
    const closed = page.locator('[data-outcome="shift-closed"]');
    await expect(closed).toBeVisible();
    await expect(closed.locator(".text-ok")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A cash bill voided after payment leaves the drawer of the till that voided it (ADR-0184): the
// count is the float alone and balances, and the close card says what the voids handed back.
test("a shift whose cash bill was voided closes balanced on its float and says so", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await navigateTo(page, "/");
    await aSettledTable(page);

    await navigateTo(page, "/today");
    await page.locator('[data-step="chooseBill"]').click();
    await page.locator('[data-step="askVoidPaidBill"]').click();
    await page.locator("#void-paid-bill-approver-code").fill(edge.staffCode);
    await page.locator("#void-paid-bill-approver-pin").fill(edge.staffPin);
    await page.locator('[data-step="voidPaidBillReason"]').first().click();
    await expect(page.locator('[data-outcome="paid-bill-voided"]')).toBeVisible();

    await navigateTo(page, "/shift");
    await page.locator("#count").fill("100000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await page.locator('[data-step="closeShift"]').click();
    const closed = page.locator('[data-outcome="shift-closed"]');
    await expect(closed.locator(".text-ok")).toBeVisible();
    await expect(closed.locator('[data-outcome="shift-voided-after-payment"]')).toContainText(
      "Voided after payment: 1 bill, cash handed back",
    );
  } finally {
    await edge.stop();
  }
});

// A store's shift settings (ADR-0160 decision 2). The `shift-settings` store fills in a float of
// 500,000₫ and shows what the drawer should hold at the count; every other store fills in nothing
// and counts blind, as before the settings.
test("a store's float is filled in, the cashier changes it, and the count shows what the drawer should hold", async ({
  page,
}) => {
  const edge = await startEdge("shift-settings");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");
    await expect(page.locator("#float")).toHaveValue("500000");

    // The drawer holds something else today, so the cashier types what is there.
    await page.locator("#float").fill("450000");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await expect(page.locator('[data-outcome="shift-expected"]')).toHaveText(
      "The drawer should hold 450,000₫.",
    );

    // A salad paid in cash (97,900₫ with its tax) moves it, and the Shift screen reads it again
    // when it opens.
    await navigateTo(page, "/");
    await seatTable(page);
    await addByName(page, "salad");
    await sendThenTakePayment(page);
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await navigateTo(page, "/shift");
    await expect(page.locator('[data-outcome="shift-expected"]')).toHaveText(
      "The drawer should hold 547,900₫.",
    );

    // The variance is still the close's alone.
    await page.locator("#count").fill("547900");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await page.locator('[data-step="closeShift"]').click();
    const closed = page.locator('[data-outcome="shift-closed"]');
    await expect(closed.locator(".text-ok")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// The till that opened a shift can hear its own `cash.shift.opened` after the edge's answer. The
// event carries no figures, so it must move the shift it already holds to open and keep what the
// answer said the drawer should hold, rather than replace it with a shift that says nothing. The
// live link holds the event, and every frame after it, until the answer is on screen.
test("a till that hears its own opened shift after the answer still shows what the drawer should hold", async ({
  page,
}) => {
  const edge = await startEdge("shift-settings");
  try {
    let link = null;
    let holding = false;
    let heard = false;
    const held = [];
    await page.routeWebSocket(
      (url) => url.pathname === "/ws",
      (socket) => {
        link = socket;
        socket.connectToServer().onMessage((message) => {
          const frame = JSON.parse(String(message));
          if (!heard && frame.event_type === "cash.shift.opened") {
            heard = true;
            holding = true;
          }
          if (holding) {
            held.push(message);
          } else {
            socket.send(message);
          }
        });
      },
    );
    await pair(page, edge);
    await signIn(page, edge);
    // The event reaches the till only on a link that is open when the shift opens.
    await expect(page.getByText("Store connected")).toBeVisible();
    await navigateTo(page, "/shift");
    await page.locator("#float").fill("450000");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await expect(page.locator('[data-outcome="shift-expected"]')).toHaveText(
      "The drawer should hold 450,000₫.",
    );
    await expect.poll(() => heard).toBe(true);

    // The event lands after the answer. Hearing it reads the drawers again, so that read is the
    // moment the event has been applied.
    const applied = page.waitForRequest(
      (request) => request.method() === "GET" && new URL(request.url()).pathname === "/api/shifts",
    );
    holding = false;
    for (const message of held.splice(0)) {
      link.send(message);
    }
    await applied;
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await expect(page.locator('[data-outcome="shift-expected"]')).toHaveText(
      "The drawer should hold 450,000₫.",
    );
  } finally {
    await edge.stop();
  }
});

test("a store that sets no float fills in nothing, and its count stays blind", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");
    await expect(page.locator("#float")).toHaveValue("");
    await page.locator("#float").fill("100000");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await expect(page.locator("#count")).toBeVisible();
    await expect(page.locator('[data-outcome="shift-expected"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// Once the bill is open the menu sells nothing, and says how to order more.
//
// A bill names the dishes it covers when it opens, so a dish rung onto the table afterwards was on no
// bill: the guest paid the old total and the dish left with the table, unpaid. The edge refuses it
// now (`BILL_ALREADY_OPEN`); the till does not offer it, and says what to do instead. Voiding the
// bill (the demo employee holds every permission, so approves their own void) gives the menu back.
test("once the bill is open the menu is locked, and voiding the bill gives it back", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await page.getByRole("link", { name: "← Order" }).click();

    await expect(page.locator('[data-outcome="bill-open-locked"]')).toBeVisible();
    const items = page.locator('[data-step="onItem"]');
    await expect(items.first()).toBeVisible();
    for (const item of await items.all()) {
      await expect(item).toBeDisabled();
    }

    await sendThenTakePayment(page);
    await page.locator('[data-step="askVoidBill"]').click();
    await page.locator("#void-bill-approver-code").fill(edge.staffCode);
    await page.locator("#void-bill-approver-pin").fill(edge.staffPin);
    await page.locator('[data-step="voidBillReason"]').first().click();
    await expect(page.locator('[data-outcome="bill-voided"]')).toBeVisible();
    await page.getByRole("button", { name: "← Order" }).click();

    await expect(page.locator('[data-outcome="bill-open-locked"]')).toHaveCount(0);
    await addItem(page);
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(2);
  } finally {
    await edge.stop();
  }
});

// A pay screen whose bill is settled or voided on another till says so at once, with the way back
// (`docs/ui-ux.md` §3), rather than failing on the next tap against a bill that is closed. A till of
// its own does not see the notice for its own settle or void.
test("a pay screen says when another till paid or voided its bill, and offers the way back", async ({
  context,
}) => {
  const edge = await startEdge();
  try {
    const here = await context.newPage();
    await pair(here, edge);
    await signIn(here, edge);
    await seatTable(here);
    await addItem(here);
    await sendThenTakePayment(here);
    await expect(here.locator('[data-step="payCash"]')).toBeEnabled();
    const payUrl = here.url();
    const ended = here.locator('[data-outcome="bill-ended-elsewhere"]');

    // Voided on another till: the order is still on the table, so the way back is the order.
    const other = await context.newPage();
    await other.goto(payUrl);
    await other.locator('[data-step="askVoidBill"]').click();
    await other.locator("#void-bill-approver-code").fill(edge.staffCode);
    await other.locator("#void-bill-approver-pin").fill(edge.staffPin);
    await other.locator('[data-step="voidBillReason"]').first().click();
    await expect(other.locator('[data-outcome="bill-voided"]')).toBeVisible();
    await expect(other.locator('[data-outcome="bill-ended-elsewhere"]')).toHaveCount(0);
    await expect(ended).toContainText("This bill was voided on another till");
    await here.locator('[data-step="leaveEndedBill"]').click();
    await expect(here.locator('[data-outcome="order-open"]')).toBeVisible();

    // Paid on another till: the table is done, so the way back is the floor.
    await sendThenTakePayment(here);
    await expect(here.locator('[data-step="payCash"]')).toBeEnabled();
    await other.goto(here.url());
    await other.locator('[data-step="payCash"]').click();
    await expect(other.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(other.locator('[data-outcome="bill-ended-elsewhere"]')).toHaveCount(0);
    await expect(ended).toContainText("This bill was paid on another till");
    await expect(here.locator('[data-step="payCash"]')).toHaveCount(0);
    await here.locator('[data-step="leaveEndedBill"]').click();
    await expect(here.locator('[data-outcome="floor"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A counter order on the kitchen board and the pass is called by the guest's number.
//
// Both screens labelled a counter ticket with the last four characters of the order's internal id
// ("Counter order …7K3Q"), which nobody at the counter can match to a guest holding a number. The
// number lives on the counter list, not on the live orders the boards are drawn from, so the boards
// read it from there for the counter orders they show. The first walk-in of a fresh demo day is
// number 1.
test("a counter order on the kitchen board and the pass is called by the guest's number", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await startWalkIn(page);
    await addItem(page);
    await sendOrder(page);

    await navigateTo(page, "/kds");
    await expect(page.locator('[data-step="onBump"]').first()).toContainText("No. 1");
    await navigateTo(page, "/expo");
    await expect(page.getByText("No. 1", { exact: true })).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A guest note reaches the kitchen board, and the next dish does not inherit it (ADR-0157).
//
// The note is typed before the tap and rides with that one add. The board reads it from the frame the
// add fanned out, and again from the live orders when it reloads, which is the path a board switched
// on mid-service takes. The second dish is added with the field empty, so a note that stuck to the
// screen instead of the line would show twice.
test("a note typed at the till is on the kitchen board, under that dish only", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await page.locator("#line-note").fill("No ice, please");
    await addByName(page, "iced tea");
    await expect(page.locator('[data-outcome="line-note"]')).toHaveCount(1);
    await expect(page.locator("#line-note")).toHaveValue("");
    await addItem(page);
    await expect(page.locator('[data-outcome="line-note"]')).toHaveCount(1);
    await sendOrder(page);

    await navigateTo(page, "/kds");
    await expect(page.locator('[data-outcome="ticket-note"]')).toHaveCount(1);
    await expect(page.locator('[data-outcome="ticket-note"]')).toContainText("No ice, please");
  } finally {
    await edge.stop();
  }
});

// A counter order paid before the kitchen made it stays on the board, note and all, until a station
// bumps it (ADR-0161).
//
// A counter takes the money first and cooks after. The board used to drop an order the moment its
// bill settled, so the ticket and the guest's allergy note vanished while the food was still to
// make. This walks the counter's own order of work: ring the dish with a note, send it, charge it,
// and only then look at the board, once live and once after a reload, the path a board switched on
// mid-service takes. The bump is what takes it off.
test("a counter order paid before the kitchen made it stays on the board until it is bumped", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await startWalkIn(page);
    await page.locator("#line-note").fill("No peanuts");
    await addItem(page);
    await sendOrder(page);
    await page.locator('a[href="/counter"]').first().click();
    await page.waitForURL((url) => url.pathname === "/counter");
    await page.locator('[data-step="charge"]').first().click();
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();

    await navigateTo(page, "/kds");
    await expect(page.locator('[data-outcome="ticket-note"]')).toContainText("No peanuts");
    await page.reload();
    await expect(page.locator('[data-outcome="ticket-note"]')).toContainText("No peanuts");

    await page.locator('[data-step="onBump"]').first().click();
    await expect(page.locator('[data-step="onBump"]')).toHaveCount(0);
    await page.reload();
    await expect(page.locator('[data-outcome="ticket-note"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// A counter tip on a bill that is not a round number still settles.
//
// The table pay screen's float-division tip was fixed (the test above it here says how it failed);
// the counter's copy of the same line was not, and survived one file over. The iced tea's 43,450₫ is
// the bill it fails on: five percent is 2,172.5, which the edge refuses as a money amount — so this
// walks the whole tipped charge and asks only that it ends settled.
test("a counter tip on a bill that is not a round number still settles", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await startWalkIn(page);
    await page.locator("#menu-search").fill("tea");
    await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
    await page.locator('[data-step="onItem"]').click();
    await expect(page.locator('[data-outcome="line-added"]').first()).toBeVisible();
    await sendOrder(page);
    await page.locator('a[href="/counter"]').first().click();
    await page.waitForURL((url) => url.pathname === "/counter");

    await page.locator('[data-step="charge"]').first().click();
    await expect(page.getByText("43,450₫", { exact: true }).first()).toBeVisible();
    await page.locator('[data-step="setTip"]').first().click();
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();

    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// The counter's pay pad offers the store's own tip keys, in its order, and no tip row where the store
// hides every key (ADR-0160 decision 2), as the table pay screen does.
//
// It offered five, ten and fifteen percent at every store after the table pay screen took the
// store's keys. `POS_DEMO_PROFILE=tender-keys` publishes keys of ten, nothing and twenty percent, so a
// walk-in's salad, 97,900₫, offers 9,790₫ and 19,580₫ and settles in cash with the first. Then the
// edge says the store hides every key: the next walk-in's pad draws no tip row, and the cash tender
// still takes the money.
test("the counter's pay pad offers the store's own tip keys, and no tip row where it hides them all", async ({
  page,
}) => {
  const edge = await startEdge("tender-keys");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aWalkInToCharge(page);
    await page.locator('[data-step="charge"]').first().click();
    await expect(page.getByText("97,900₫", { exact: true }).first()).toBeVisible();
    await expect(page.getByRole("heading", { name: "Tip", exact: true })).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveText(["9,790₫", "19,580₫"]);
    await page.locator('[data-step="setTip"]').first().click();
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();

    let hidden = 0;
    await page.route("**/api/menu*", async (route) => {
      const response = await fetchWhole(route);
      const book = await response.json();
      book.tip_percents = [];
      hidden += 1;
      await route.fulfill({ response, json: book });
    });
    await page.goto(`${edge.baseURL}/counter`);
    await aWalkInToCharge(page);
    await page.locator('[data-step="charge"]').first().click();
    await expect(page.getByText("97,900₫", { exact: true }).first()).toBeVisible();
    await expect(page.locator('[data-step="setTender"]').first()).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveCount(0);
    await expect(page.getByRole("heading", { name: "Tip", exact: true })).toHaveCount(0);
    expect(hidden, "the price book the till read hid every key").toBeGreaterThan(0);
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A store that sets no tip keys, and an edge too old to send them, offer the counter's guest what it
// always offered: five, ten and fifteen percent, 4,895₫, 9,790₫ and 14,685₫ of the salad's 97,900₫.
// The second half is the price book read with `tip_percents` taken out, which is an older edge's
// answer, and the same order's pad is opened again.
test("a store that sets no tip keys, and an edge too old to send them, offer today's keys at the counter", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aWalkInToCharge(page);
    const today = ["4,895₫", "9,790₫", "14,685₫"];
    await page.locator('[data-step="charge"]').first().click();
    await expect(page.getByText("97,900₫", { exact: true }).first()).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveText(today);

    let older = 0;
    await page.route("**/api/menu*", async (route) => {
      const response = await fetchWhole(route);
      const book = await response.json();
      delete book.tip_percents;
      older += 1;
      await route.fulfill({ response, json: book });
    });
    await page.goto(`${edge.baseURL}/counter`);
    await page.locator('[data-step="charge"]').first().click();
    await expect(page.getByText("97,900₫", { exact: true }).first()).toBeVisible();
    await expect(page.locator('[data-step="setTip"]')).toHaveText(today);
    expect(older, "the price book the till read came from the older edge").toBeGreaterThan(0);
  } finally {
    await edge.stop();
  }
});

/** Adds the garden salad to the walk-in on screen, after seeing the counter offer it at `price`. */
async function sellTheSalad(page, price) {
  await page.locator("#menu-search").fill("salad");
  await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
  await expect(page.locator('[data-step="onItem"]')).toContainText(price);
  await page.locator('[data-step="onItem"]').click();
  await expectLineAdded(page);
}

// A walk-in is opened on the edge by its first item (UX-16). New order used to open it at the tap,
// and a guest who left before ordering left an order behind that had a queue number, nothing on it,
// and no way to be listed or closed. Leaving before the first item now leaves nothing.
test("a new counter order is opened by its first item, and leaving before then leaves nothing", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    let opened = 0;
    page.on("request", (request) => {
      if (request.method() === "POST" && new URL(request.url()).pathname === "/api/orders") {
        opened += 1;
      }
    });

    await startWalkIn(page);
    await expect(page).toHaveURL(/\/order\/new$/);
    await page.locator('a[href="/counter"]').first().click();
    await expect(page.locator('[data-step="newOrder"]')).toBeVisible();
    expect(opened).toBe(0);

    await startWalkIn(page);
    await addItem(page);
    await expect(page).toHaveURL(/\/order\/[0-9A-Z]{26}$/);
    expect(opened).toBe(1);
    await addItem(page);
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(2);
    expect(opened).toBe(1);
    await sendOrder(page);
    await page.locator('a[href="/counter"]').first().click();
    await expect(page.locator('[data-step="charge"]')).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// A counter bill takes money off and is voided as a table's is (UX-10), with the same panels: the
// counter's pay pad offered neither, so a walk-in that needed either went to a manager elsewhere.
test("a counter bill takes a discount and is voided like a table's", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aWalkInToCharge(page);
    await page.locator('[data-step="charge"]').first().click();
    const total = page.locator("p.text-2xl.tabular-nums").first();
    await expect(total).toHaveText(/\d/);
    const before = Number((await total.textContent()).replace(/[^0-9]/g, ""));

    const manager = async (panel) => {
      const code = page.locator(`#${panel}-approver-code`);
      if ((await code.count()) > 0) {
        await code.fill(edge.staffCode);
        await page.locator(`#${panel}-approver-pin`).fill(edge.staffPin);
      }
    };
    await page.locator('[data-outcome="counter-discount"]').click();
    await page.locator("#discount-amount").fill("10000");
    await manager("discount");
    await page.locator('[data-step="discountReason"]').first().click();
    await expect(page.locator("#discount-amount")).toHaveCount(0);
    const after = Number((await total.textContent()).replace(/[^0-9]/g, ""));
    expect(after).toBeLessThan(before);

    await page.locator('[data-outcome="counter-void"]').click();
    await manager("void-bill");
    await page.locator('[data-step="voidBillReason"]').first().click();
    await expect(page.locator('[data-outcome="bill-voided"]')).toBeVisible();
    // The order is still there to charge again, on a new bill.
    await page.locator('[data-step="charge"]').first().click();
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A store whose walk-ins are eaten in sells them from the dining room's book (ADR-0160 decision 2).
//
// `POS_DEMO_PROFILE=walk-in-dine-in` prices the garden salad at 89,000₫ in the dining room and
// 79,000₫ to take away, and sets `counter.walk_in_channel` to dine-in. New order opens the walk-in at
// once, as on every store, and the counter offers the salad at the dining room's price: the book of
// the channel the edge opened it on, which charges 97,900₫ with its tax.
test("a store whose walk-ins are eaten in sells them from the dining room's book", async ({
  page,
}) => {
  const edge = await startEdge("walk-in-dine-in");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await startWalkIn(page);
    await expect(page.locator('[data-outcome="walk-in-channel"]')).toHaveCount(0);
    await sellTheSalad(page, "89,000₫");
    await expect(page.locator('[data-outcome="check-total"]')).toContainText("97,900₫");
  } finally {
    await edge.stop();
  }
});

// A store that sets no walk-in channel, and an edge too old to send one, sell a walk-in from the
// takeaway book, as the counter always has.
//
// `POS_DEMO_PROFILE=walk-in-takeaway` prices the salad at 79,000₫ to take away and publishes no
// `counter` node, so the edge opens a walk-in for takeaway and charges 86,900₫ for it. Then the price
// book is read with `walk_in_channel` taken out, which is an older edge's answer, and the counter
// still offers the takeaway book's price.
test("a store that sets no walk-in channel, and an edge too old to send one, sell from the takeaway book", async ({
  page,
}) => {
  const edge = await startEdge("walk-in-takeaway");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await startWalkIn(page);
    await expect(page.locator('[data-outcome="walk-in-channel"]')).toHaveCount(0);
    await sellTheSalad(page, "79,000₫");
    await expect(page.locator('[data-outcome="check-total"]')).toContainText("86,900₫");

    let older = 0;
    await page.route("**/api/menu*", async (route) => {
      const response = await fetchWhole(route);
      const book = await response.json();
      delete book.walk_in_channel;
      older += 1;
      await route.fulfill({ response, json: book });
    });
    await page.goto(`${edge.baseURL}/counter`);
    await startWalkIn(page);
    await sellTheSalad(page, "79,000₫");
    await expect(page.locator('[data-outcome="check-total"]')).toContainText("86,900₫");
    expect(older, "the price book the till read came from the older edge").toBeGreaterThan(0);
  } finally {
    await edge.stop();
  }
});

// A store that asks each walk-in guest opens the order on their answer (ADR-0160 decision 2).
//
// `POS_DEMO_PROFILE=walk-in-ask` prices the salad at 89,000₫ in the dining room and 79,000₫ to take
// away, and sets `counter.walk_in_channel` to ask. New order offers two large answers instead of
// opening the order. Eat in opens it on the dining room's channel: the header says so, the counter
// offers the dining room's price and the edge charges 97,900₫. It still says so after a reload, which
// the till learns from the live orders. Take away opens the next one on takeaway, at 79,000₫ and
// 86,900₫, with the next queue number.
//
// Then the price book is read with `walk_in_channel` taken out, which is an older edge's answer: New
// order opens the order at one tap, as it always did, with no answer on the header, from the
// takeaway book, and the edge opens it for takeaway because the till named no channel.
test("a store that asks each walk-in guest opens the order eaten in or taken away, as they answer", async ({
  page,
}) => {
  const edge = await startEdge("walk-in-ask");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    const answer = page.locator('[data-outcome="walk-in-channel"]');
    const opensOn = async (step, label, price, total) => {
      await navigateTo(page, "/counter");
      // Collapsed, rather than absent, once the till has read that the store asks.
      await expect(page.locator('[data-step="newOrder"]')).toHaveAttribute("aria-expanded", "false");
      await page.locator('[data-step="newOrder"]').click();
      await expect(page.locator('[data-step="eatIn"]')).toBeVisible();
      await expect(page.locator('[data-step="takeAway"]')).toBeVisible();
      await page.locator(`[data-step="${step}"]`).click();
      await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
      await expect(answer).toHaveText(label);
      await sellTheSalad(page, price);
      await expect(page.locator('[data-outcome="check-total"]')).toContainText(total);
    };

    await opensOn("eatIn", "Eat in", "89,000₫", "97,900₫");
    await page.reload();
    await expect(answer).toHaveText("Eat in");
    await page.locator("#menu-search").fill("salad");
    await expect(page.locator('[data-step="onItem"]')).toContainText("89,000₫");
    await page.locator("#menu-search").fill("");

    await opensOn("takeAway", "Take away", "79,000₫", "86,900₫");
    await navigateTo(page, "/counter");
    await expect(page.locator('[data-step="charge"]')).toHaveCount(2);
    await expect(page.getByText("No. 2", { exact: true })).toBeVisible();

    let older = 0;
    await page.route("**/api/menu*", async (route) => {
      const response = await fetchWhole(route);
      const book = await response.json();
      delete book.walk_in_channel;
      older += 1;
      await route.fulfill({ response, json: book });
    });
    await page.goto(`${edge.baseURL}/counter`);
    await startWalkIn(page);
    await expect(answer).toHaveCount(0);
    await sellTheSalad(page, "79,000₫");
    await expect(page.locator('[data-outcome="check-total"]')).toContainText("86,900₫");
    expect(older, "the price book the till read came from the older edge").toBeGreaterThan(0);
  } finally {
    await edge.stop();
  }
});

// The till draws money with the exponent the store published, not one it guessed.
//
// `ui/src/lib/money.ts` used to hold `MINOR_DIGITS[code] ?? 0` and that `?? 0` was the defect: it is
// right for the đồng and the yen — the two currencies this app started with — and silently wrong for
// the rupee, whose country pack has shipped since ADR-0105. ₹261.45 drew as `INR 26,145`, and the
// *input* path was out by a hundred the other way, so a cashier's typed discount sent a different
// amount of money than the one they meant.
//
// The exponent now arrives on `GET /api/locale`. This intercepts that one response and changes that
// one field, because the demo store is VND and VND's published exponent and the old table's guess
// agree by construction — which is exactly why the bug survived every run of this gate. Two decimals
// on the đồng is not a real store; it is the smallest change that makes "the till used the published
// value" observable at all.
test("the till draws money with the exponent the store published", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      await route.fulfill({ response, json: { ...body, currency_exponent: 2 } });
    });

    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    await sendThenTakePayment(page);

    // The salad is 89,000 plus ten percent, so the check is 97,900 minor units. Read with the
    // store's published exponent of two that is 979.00; read with the old table's guess for the
    // đồng it is 97,900. The symbol still comes from the amount's own currency code, which this
    // does not touch.
    await expect(page.getByText("979.00₫", { exact: true })).toBeVisible();
    await expect(page.getByText("97,900₫", { exact: true })).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// declaration, where the reason is read by anyone looking at the map — silently dropping out of the
// browser gate is how coverage rots.
// A cashier with no keyboard can still open the shift.
//
// `docs/ui-ux.md` has asked for "a large numeric keypad for cash" since it was written, and the
// quick-cash buttons beside it were built while this was not. On a phone or a tablet the gap hid
// itself: `inputmode="numeric"` summons the OS keypad and the field fills. On the 13"+ terminal a
// counter is run from it summons one only where the platform's on-screen keyboard is enabled, and
// where it is not, the float field cannot be filled and the shift cannot be opened.
//
// Driven entirely through the keys, with no `fill` anywhere: `fill` is what every other test here
// uses and it is exactly the affordance a keyboardless terminal does not have. The digits are
// pressed by their own faces, so the assertion is that a person pressing 1-0-0-0-0-0 gets 100000
// rather than that a component exists.
test("a cashier with no keyboard can enter a float on the keypad", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");

    // The label names the store's own currency rather than a word compiled into the catalogue.
    //
    // It read "Opening float (đồng)" in both catalogues until now, which is not an example but a
    // claim about what the field holds: a Japanese cashier counting yen was told to count đồng.
    //
    // What this pins is that the label is *derived* — the word is gone and `storeCurrency()` is what
    // fills the gap. It does not prove the six-market behaviour, because every demo store trades in
    // đồng and there is no fixture in another currency to read it on.
    await expect(page.getByText("Opening float (VND)")).toBeVisible();

    const keypad = page.locator('[data-step="floatKeypad"]');
    for (const digit of ["1", "0", "0", "0", "0", "0"]) {
      await keypad.getByRole("button", { name: digit, exact: true }).click();
    }
    await expect(page.locator("#float")).toHaveValue("100000");

    // The correction keys matter as much as the digits: a cashier who mis-taps on a terminal has no
    // other way back, and a keypad without them would make every slip a reload.
    //
    // Found by its accessible name rather than its face: the key draws `⌫`, and a glyph is not a
    // name a screen reader can say, so the button carries an `aria-label` that becomes its name.
    // Asking for the glyph here failed, which is the assertion working — a keypad whose correction
    // keys were unnamed would be unusable with a reader and this test would not have noticed.
    await keypad.getByRole("button", { name: "Delete the last digit" }).click();
    await expect(page.locator("#float")).toHaveValue("10000");

    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// Đồng has no minor unit, so every amount ends in zeros: the pad has a `000` key, and the float,
// the cash paid in or out and the count add the store's own notes with one tap each
// (`docs/ui-ux.md` §2).
test("a till trading in đồng types thousands with one key, and adds a note with one tap", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    // The demo store publishes no notes of its own, so this one is given Vietnam's larger notes.
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      const notes = [10_000, 20_000, 50_000, 100_000, 200_000, 500_000];
      await route.fulfill({ response, json: { ...body, cash_denominations: notes } });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");
    const keypad = page.locator('[data-step="floatKeypad"]');
    await keypad.getByRole("button", { name: "Clear the amount" }).click();
    await keypad.getByRole("button", { name: "000", exact: true }).click();
    await expect(page.locator("#float")).toHaveValue("0");
    for (const key of ["1", "5", "000"]) {
      await keypad.getByRole("button", { name: key, exact: true }).click();
    }
    await expect(page.locator("#float")).toHaveValue("15000");

    // Each note key adds one note to what is typed, in whole đồng: the largest four are offered.
    await keypad.getByRole("button", { name: "Clear the amount" }).click();
    const notes = keypad.locator('[data-outcome="keypad-notes"] button');
    await expect(notes).toHaveText(["+50,000₫", "+100,000₫", "+200,000₫", "+500,000₫"]);
    await notes.last().click();
    await notes.last().click();
    await notes.first().click();
    await expect(page.locator("#float")).toHaveValue("1050000");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();

    // The count's pad offers them too.
    await expect(page.locator('[data-step="countKeypad"] [data-outcome="keypad-notes"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A store that counts by note (ADR-0186 decision 3) gets a counter on the float, the count and a drop
// to the safe: − and + per note, the keypad into the row tapped, a loose amount, and the total,
// which is what is sent with its rows. The sum is the till's, so it always adds up; the close card
// lists the rows the edge kept. The demo store publishes no counting list, so this one is given
// Vietnam's.
test("a drawer is counted note by note, and the close card lists the notes", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      const counting = [1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000, 500_000];
      await route.fulfill({ response, json: { ...body, cash_count_denominations: counting } });
    });
    const sent = [];
    page.on("request", (request) => {
      if (request.method() === "POST" && /\/api\/shifts(\/[^/]+\/(count|safe-drop))?$/.test(request.url())) {
        sent.push(request.postDataJSON());
      }
    });
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");

    // The float: two 500,000₫ notes on +, three 100,000₫ on the keypad, into the row tapped.
    const counter = page.locator('[data-outcome="denomination-counter"]');
    await expect(page.locator("#float")).toHaveCount(0);
    await counter.getByRole("button", { name: "One more 500,000₫" }).click();
    await counter.getByRole("button", { name: "One more 500,000₫" }).click();
    await counter.getByRole("textbox", { name: "How many 100,000₫" }).click();
    await page.locator('[data-step="floatKeypad"]').getByRole("button", { name: "3", exact: true }).click();
    await expect(page.locator('[data-outcome="counter-total"]')).toHaveText("Total 1,300,000₫");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    expect(sent[0].opening_float.amount_minor).toBe(1_300_000);
    expect(sent[0].denomination_counts).toEqual([
      { denomination: { amount_minor: 100_000, currency_code: "VND" }, quantity: 3 },
      { denomination: { amount_minor: 500_000, currency_code: "VND" }, quantity: 2 },
    ]);

    // A drop to the safe is counted the same way, and carries its notes.
    await page.getByRole("button", { name: "Drop to safe" }).click();
    const drop = page.locator('[data-outcome="safe-drop"]');
    await drop.getByRole("button", { name: "One more 500,000₫" }).click();
    await drop.locator('[data-step="dropToSafe"]').click();
    await expect(page.locator('[data-outcome="cash-moved"]')).toContainText("500,000₫");
    expect(sent[1].amount_minor).toBe(500_000);
    expect(sent[1].denomination_counts).toEqual([
      { denomination: { amount_minor: 500_000, currency_code: "VND" }, quantity: 1 },
    ]);

    // The count: 800,000₫ left, one note on +, two typed, and 100,000₫ loose that the list does not name.
    await counter.getByRole("button", { name: "One more 500,000₫" }).click();
    await counter.getByRole("textbox", { name: "How many 100,000₫" }).fill("2");
    await counter.locator('[data-outcome="counter-uncounted"]').fill("100000");
    await expect(page.locator('[data-outcome="counter-total"]')).toHaveText("Total 800,000₫");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    expect(sent[2].counted_minor).toBe(800_000);
    expect(sent[2].uncounted_amount).toEqual({ amount_minor: 100_000, currency_code: "VND" });

    await page.locator('[data-step="closeShift"]').click();
    const closed = page.locator('[data-outcome="shift-closed"]');
    await expect(closed.locator(".text-ok")).toBeVisible();
    const rows = closed.locator('[data-outcome="counted-rows"] li');
    await expect(rows).toHaveText([
      "100,000₫ × 2 = 200,000₫",
      "500,000₫ × 1 = 500,000₫",
      "Other / loose 100,000₫",
    ]);
  } finally {
    await edge.stop();
  }
});

// A shift is handed over at the till (ADR-0187): the outgoing cashier counts and closes, the
// incoming one signs in on the same till and counts; a count that differs is counted once more and
// then confirmed, and the opening names the shift it took over. The X reads the shift without
// closing it, and once the day's only drawer is closed the till says every till is.
test("a shift is handed over at the till, the X reads it, and the day closes on the till", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    const opened = [];
    page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/api/shifts")) {
        opened.push(request.postDataJSON());
      }
    });
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);

    // The X, which closes nothing: the shift is still open after it.
    await page.locator('[data-step="readX"]').click();
    await expect(page.locator('[data-outcome="x-figures"]')).toBeVisible();
    await expect(page.locator('[data-outcome="x-expected"]')).toHaveText("100,000₫");
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();

    // The cash in and out keypad shows at once (UX-14).
    await expect(page.locator('[data-step="movementKeypad"]')).toBeVisible();

    await page.locator('[data-step="beginHandover"]').click();
    await page.locator("#handover-out").fill("100000");
    await page.locator('[data-step="handOut"]').click();
    await expect(page.locator('[data-outcome="handover-closed"]')).toContainText("100,000₫");
    // No other opening is offered in the middle of a handover.
    await expect(page.locator('[data-step="openShift"]')).toHaveCount(0);

    await page.locator("#handover-incoming-code").fill(edge.staffCode);
    await page.locator("#handover-incoming-pin").fill(edge.staffPin);
    await page.locator('[data-step="handoverSignIn"]').click();
    await page.locator("#handover-in").fill("90000");
    await page.locator('[data-step="handIn"]').click();
    await expect(page.locator('[data-outcome="handover-differs"]')).toContainText("Count the drawer once more");
    await page.locator("#handover-in").fill("90000");
    await page.locator('[data-step="handIn"]').click();
    await expect(page.locator('[data-outcome="handover-differs"]')).toContainText("Confirm");
    await page.locator('[data-step="confirmHandIn"]').click();
    await expect(page.locator('[data-outcome="handover-done"]')).toContainText("-10,000₫");
    expect(opened.at(-1).opening_float.amount_minor).toBe(90_000);
    expect(opened.at(-1).handed_over_from_shift_id).toBeTruthy();
    // The slip prints again from the Shift screen, for a person who reads an X's expected cash.
    await page.locator('[data-step="reprintSlip"]').click();
    await expect(page.locator('[data-outcome="handover-slip-again"]')).toBeVisible();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();

    // The day's last drawer closes: the till says every till is closed, with the store's figures.
    await page.locator("#count").fill("90000");
    await page.locator('[data-step="countShift"]').click();
    await page.locator('[data-step="closeShift"]').click();
    await expect(page.locator('[data-outcome="shift-closed"]')).toBeVisible();
    const day = page.locator('[data-outcome="all-tills-closed"]');
    await expect(day).toContainText("All tills closed for the day");
    // The close card comes first, above the next opening (UX-14).
    const closedBox = await page.locator('[data-outcome="shift-closed"]').boundingBox();
    const openBox = await page.locator('[data-step="openShift"]').boundingBox();
    expect(closedBox.y).toBeLessThan(openBox.y);
  } finally {
    await edge.stop();
  }
});

// A drop to the safe may say why, from the store's reasons for a drop (ADR-0186 amendment), in one
// tap like a paid out, or go without one; the Shift screen shows what the shift has dropped beside
// what it paid in and out, and so does the close card.
//
// The close card used to lose what was dropped when the shift's own `cash.shift.closed` reached the
// live link after the close's answer: the event replaced the shift with its four figures and dropped
// the rest. The event is held here until the answer has landed, which is the order that lost it.
test("a drop says why if the cashier picks a reason, and the shift shows what it dropped", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    let closedHeard = false;
    let answered = () => {};
    const closeAnswered = new Promise((resolve) => {
      answered = resolve;
    });
    await page.routeWebSocket(
      (url) => url.pathname === "/ws",
      (socket) => {
        socket.connectToServer().onMessage(async (message) => {
          const closing = String(message).includes("cash.shift.closed");
          if (closing) await closeAnswered;
          socket.send(message);
          if (closing) closedHeard = true;
        });
      },
    );
    page.on("response", (response) => {
      if (response.request().method() === "POST" && /\/api\/shifts\/[^/]+\/close$/.test(response.url())) {
        answered();
      }
    });
    const drops = [];
    page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/safe-drop")) {
        drops.push(request.postDataJSON());
      }
    });
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);

    await page.getByRole("button", { name: "Drop to safe" }).click();
    await page.locator("#drop-amount").fill("50000");
    await page.locator('[data-step="dropReason"]').first().click();
    await expect(page.locator('[data-outcome="cash-dropped"]')).toHaveText("Dropped to safe 50,000₫");
    expect(drops[0].reason_code_id).toBeTruthy();

    // Skipped: the plain button records it with no reason.
    await page.getByRole("button", { name: "Drop to safe" }).click();
    await page.locator("#drop-amount").fill("30000");
    await page.locator('[data-step="dropToSafe"]').click();
    await expect(page.locator('[data-outcome="cash-dropped"]')).toHaveText("Dropped to safe 80,000₫");
    expect(drops[1].reason_code_id ?? null).toBeNull();

    // 100,000 float - 80,000 dropped.
    await page.locator("#count").fill("20000");
    await page.locator('[data-step="countShift"]').click();
    await page.locator('[data-step="closeShift"]').click();
    const closed = page.locator('[data-outcome="shift-closed"]');
    await expect(closed.locator(".text-ok")).toBeVisible();
    await expect(closed.locator('[data-outcome="shift-dropped"]')).toContainText("80,000₫");
    // And once the held event has landed after it.
    await expect.poll(() => closedHeard).toBe(true);
    await expect(closed.locator('[data-outcome="shift-dropped"]')).toContainText("80,000₫");
  } finally {
    await edge.stop();
  }
});

// A shift starts on a terminal with no keyboard of any kind.
//
// The float keypad above closed this for cash. It did not close it for the screen that comes first.
// `docs/ui-ux.md` §2 asks for "a shared component for numeric and text entry on touch devices
// without a physical keyboard"; only the numeric half existed, and sign-in needs the other one. The
// badge code is free text — the console's own placeholder for it reads `e.g. A01` — so on a fixed
// terminal whose on-screen keyboard is switched off, `inputmode` asked the platform for a keyboard
// that never came and the field could not be filled. Not one awkward field: a till nobody can sign
// in to, and therefore every field on every later screen.
//
// Driven with no `fill` anywhere, like the keypad flow, because `fill` is precisely the affordance
// the device class in question does not have. Run at phone size, since that is the tightest of the
// three and a pad that does not fit is a pad that is not there.
test("a shift starts with no keyboard at all — badge code and PIN typed on the on-screen pad", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.setViewportSize({ width: 390, height: 844 });
    await pair(page, edge);

    // The fixture has to carry a letter or this flow proves nothing it claims to. The demo badge
    // code was four digits for a year, which is exactly how a keyboardless terminal's real problem
    // stayed invisible behind a gate that was otherwise thorough.
    expect(
      edge.staffCode,
      "the demo badge code is all digits again — a numeric keypad would serve it, and this flow no longer proves a text pad is needed",
    ).toMatch(/[A-Z]/);

    const pad = page.locator("#signin-pad");
    const press = async (characters) => {
      for (const character of characters) {
        await pad.getByRole("button", { name: character, exact: true }).click();
      }
    };

    // Tapping the field is how an operator chooses it, and it is what tells the pad which alphabet
    // to draw.
    await page.locator("#signin-code").click();
    await press(edge.staffCode);
    await expect(page.locator("#signin-code")).toHaveValue(edge.staffCode);

    // The caret never left the field the keys were filling. A button takes focus on `pointerdown`,
    // so without refusing that the first key press moves the cursor off the input and the operator
    // watches the rest of their code go nowhere.
    await expect(page.locator("#signin-code")).toBeFocused();

    await page.locator("#signin-pin").click();

    // The letters are gone, because the PIN field strips anything that is not a digit. Offering a
    // key whose press does nothing is worse than offering no key: the operator presses it, nothing
    // appears, and they have no way to tell that from a dead screen.
    await expect(
      pad.getByRole("button", { name: "A", exact: true }),
      "the pad still offers letters on the PIN field, which strips them — a key that swallows a press",
    ).toHaveCount(0);

    await press(edge.staffPin);
    await expect(page.locator("#signin-pin")).toHaveValue(edge.staffPin);

    // Correction keys, for the same reason the float keypad has them: a mis-tap on a terminal with
    // no keyboard has no other way back, and without them every slip is a reload. Found by their
    // accessible names — `C` and `⌫` are glyphs, and a glyph is not a name a reader can say.
    await pad.getByRole("button", { name: "Delete the last character" }).click();
    await expect(page.locator("#signin-pin")).toHaveValue(edge.staffPin.slice(0, -1));
    await press(edge.staffPin.slice(-1));

    // Nothing is cut off sideways on the tightest device, with the letter grid drawn.
    await page.locator("#signin-code").click();
    const overflow = await page.evaluate(() => {
      const root = document.documentElement;
      return root.scrollWidth - root.clientWidth;
    });
    expect(overflow, "the sign-in pad runs wider than a phone").toBeLessThanOrEqual(1);

    await page.locator('[data-step="submit"]').click();
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A box being claimed (ADR-0148) shows its claim page and nothing of a till. `pos-edge claim` serves
// only that page and `/api/claim`, so a status bar, navigation or sign-out drawn around it offered a
// person reading a code off the box a row of places that all failed — which is what the round-two
// demo found. The example store answers `/api/claim` with nothing, and that is fine here: the
// assertion is about what surrounds the page, not what the claim is doing.
test("a box being claimed shows its claim page and no till around it", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.goto(`${edge.baseURL}/claim`);
    await expect(page.locator('[data-outcome="claim"]')).toBeVisible();
    await expect(page.locator("header")).toHaveCount(0);
    await expect(page.locator("nav")).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// What the signed-in person may do, as a store that enforces each person's own permissions reports
// it (ADR-0158 decision 6). The example store enforces nothing, so this rewrites the session read's
// three fields and nothing else; `granted()` is asked on every read, so a test can change it.
async function aSessionThatSays(page, enforced, granted, withApproval = []) {
  await page.route("**/api/session", async (route) => {
    const response = await fetchWhole(route);
    if (!response.ok()) {
      await route.fulfill({ response });
      return;
    }
    const body = await response.json();
    await route.fulfill({
      response,
      json: {
        ...body,
        permissions_enforced: enforced,
        permissions: granted(),
        permissions_with_approval: withApproval,
      },
    });
  });
}

/** The status bar's destinations that are drawn, by address. */
async function destinations(page) {
  return page.locator("nav a").evaluateAll((links) => links.map((link) => link.getAttribute("href")));
}

// A store that enforces hides what the person may not do. This person seats, rings, sends and opens
// the bill, and opens the drawer alone. With a manager's approval they void a bill, and they go above
// a discount ceiling, but they hold no discount itself. They also move guests "with approval", which
// no act takes, so it lets them do nothing.
test("a store that enforces each person's own permissions hides what they may not do", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    const granted = [
      "billing.bill.open",
      "cash.drawer.open_no_sale",
      "sales.line.add",
      "sales.line.fire",
      "sales.table.manage",
    ];
    const withApproval = [
      "billing.bill.void",
      "billing.discount.override_ceiling",
      "sales.order.transfer",
    ];
    await aSessionThatSays(page, true, () => granted, withApproval);
    await pair(page, edge);
    await signIn(page, edge);
    await expect.poll(() => destinations(page)).toEqual(["/", "/counter", "/today", "/shift", "/pair"]);
    await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();

    // Void follows the line: a cancel before the kitchen has it, `sales.line.void_fired` after.
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
    await expect(page.locator('[data-step="askVoid"]')).toHaveCount(0);
    await addItem(page);
    await expect(page.locator('[data-outcome="line-unsent"]')).toHaveCount(1);
    await expect(page.locator('[data-step="askVoid"]')).toHaveCount(1);
    for (const hidden of ["openMove", "startMarking"]) {
      await expect(page.locator(`[data-step="${hidden}"]`)).toHaveCount(0);
    }

    await sendThenTakePayment(page);
    await expect(page.locator('[data-step="askVoidBill"]')).toBeVisible();
    for (const hidden of ["setTender", "payCash", "payCard", "splitEvenly", "askDiscount"]) {
      await expect(page.locator(`[data-step="${hidden}"]`)).toHaveCount(0);
    }
    // With approval: the manager's fields are asked for before the void is sent.
    await page.locator('[data-step="askVoidBill"]').click();
    await expect(page.locator("#void-bill-approver-code")).toBeVisible();
    await expect(page.locator('[data-step="voidBillReason"]').first()).toBeDisabled();

    // Directly: the drawer opens with nobody's PIN, and the shift itself is not theirs to open.
    await navigateTo(page, "/shift");
    await expect(page.locator('[data-step="openShift"]')).toHaveCount(0);
    await page.locator('[data-step="askOpenDrawer"]').click();
    await expect(page.locator("#no-sale-approver-code")).toHaveCount(0);
    await expect(page.locator('[data-step="drawerReason"]').first()).toBeEnabled();
  } finally {
    await edge.stop();
  }
});

// A person who may not bump still sees the board, and is told why its tickets answer no tap
// (pilot run 2, N7: a cashier saw every card disabled and nothing saying why).
test("the kitchen board says why a person who may not bump cannot", async ({ page }) => {
  const edge = await startEdge();
  try {
    await aSessionThatSays(page, true, () => ["sales.line.add", "sales.line.fire", "sales.table.manage"]);
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
    await page.goto(`${edge.baseURL}/kds`);
    await expect(page.locator('[data-outcome="ticket-waiting"]').first()).toBeDisabled();
    await expect(page.locator('[data-outcome="kds-bump-not-allowed"]')).toHaveText(
      "You can see these tickets but not bump them: that needs the permission to mark a kitchen ticket done. Ask a manager.",
    );
  } finally {
    await edge.stop();
  }
});

// A store that does not enforce hides nothing, whatever the two lists say: every store today.
test("a store that does not enforce each person's own permissions hides nothing", async ({ page }) => {
  const edge = await startEdge();
  try {
    await aSessionThatSays(page, false, () => []);
    await pair(page, edge);
    await signIn(page, edge);
    await expect.poll(() => destinations(page)).toEqual([
      "/",
      "/counter",
      "/guests",
      "/kds",
      "/expo",
      "/today",
      "/shift",
      "/pair",
      "/devices",
    ]);

    await seatTable(page);
    await addItem(page);
    for (const shown of ["fireOrder", "openMove", "startMarking", "askVoid"]) {
      await expect(page.locator(`[data-step="${shown}"]`).first()).toBeVisible();
    }
    await sendThenTakePayment(page);
    for (const shown of ["setTender", "payCash", "askDiscount", "askVoidBill"]) {
      await expect(page.locator(`[data-step="${shown}"]`).first()).toBeVisible();
    }
    // A manager approves the void, as on every store today.
    await page.locator('[data-step="askVoidBill"]').click();
    await expect(page.locator("#void-bill-approver-code")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// What the store has taken today (ADR-0160 decision 2): one figure for the store and how many bills,
// on the Today screen, to a person whose own role grants `reports.takings.view` — which the demo
// employee's does, holding every permission — and to nobody else, even where the store does not
// enforce each person's own permissions, as no store here does.
test("the Today screen shows the day's takings to a person whose role grants them", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    // A salad paid in cash: 97,900₫ with its tax.
    await seatTable(page);
    await addByName(page, "salad");
    await sendThenTakePayment(page);
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();

    await navigateTo(page, "/today");
    const tile = page.locator('[data-outcome="today-takings"]');
    await expect(tile).toContainText("97,900₫");
    await expect(tile).toContainText("Takings today");
    await expect(tile).toContainText("1 bill");
  } finally {
    await edge.stop();
  }
});

test("the Today screen shows no takings to a person whose role does not grant them", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await aSessionThatSays(page, false, () => []);
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/today");
    await expect(page.locator('[data-outcome="today-shift"]')).toBeVisible();
    await expect(page.locator('[data-outcome="today-takings"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// A manager makes this device one of the store's tills on its Devices screen (ADR-0112, ADR-0160
// decision 4). The `tills` store publishes two terminals and the edge decides every answer: the
// bind, this device refused a second till, a second device refused the till this one is, and a
// release that asks first and then frees the till for the other device. The second device is a
// second browser, paired with a code this manager gets on the same screen.
test("a manager binds this device to a till, and releases it for another device", async ({
  page,
  browser,
}) => {
  const edge = await startEdge("tills");
  const other = await browser.newPage();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/devices");
    const card = page.locator('[data-outcome="till"]');
    const status = card.locator('[data-outcome="till-status"]');
    const outcome = card.locator('[data-outcome="till-outcome"]');
    const till = (name) => card.locator("button[aria-pressed]").filter({ hasText: name });
    await expect(status).toHaveText("This device is not one of the store's tills yet.");

    await till("Bar till").click();
    await card.getByRole("button", { name: "Bind this device to Bar till" }).click();
    await expect(outcome).toHaveText("Bound.");
    await expect(status).toHaveText("This device is Bar till.");
    await expect(till("Bar till")).toContainText("This device");

    await till("Counter till").click();
    await card.getByRole("button", { name: "Bind this device to Counter till" }).click();
    await expect(outcome).toHaveText("This device is already another till. Release that one first.");
    await expect(status).toHaveText("This device is Bar till.");

    await page.getByRole("button", { name: "Get a pairing code" }).click();
    const link = page.getByText(/\/pair\?code=\d{6}$/);
    const code = /code=(\d{6})/.exec((await link.textContent()) ?? "")?.[1];
    await other.goto(`${edge.baseURL}/pair?code=${code}`);
    await other.locator("#pair-submit").click();
    await expect(other).toHaveURL(/\/signin$/);
    await signIn(other, edge);
    await navigateTo(other, "/devices");
    const otherCard = other.locator('[data-outcome="till"]');
    const otherTill = (name) => otherCard.locator("button[aria-pressed]").filter({ hasText: name });
    const otherOutcome = otherCard.locator('[data-outcome="till-outcome"]');
    await expect(otherTill("Bar till")).toContainText("Another device is this till");
    await otherTill("Bar till").click();
    await otherCard.getByRole("button", { name: "Bind this device to Bar till" }).click();
    await expect(otherOutcome).toHaveText(
      "Another device is already that till. Release it on that device first.",
    );

    // Release asks first, and nothing is released until the answer.
    const releases = [];
    page.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/print/agent/revoke") {
        releases.push(request.postDataJSON());
      }
    });
    await card.getByRole("button", { name: "Release" }).click();
    await expect(card.getByText("Release Bar till? This device stops being that till.")).toBeVisible();
    expect(releases).toHaveLength(0);
    await card.getByRole("button", { name: "Release" }).click();
    await expect(outcome).toHaveText("Released.");
    await expect(status).toHaveText("This device is not one of the store's tills yet.");
    expect(releases).toHaveLength(1);
    expect(releases[0]).toEqual({ agent_device_id: expect.any(String) });

    await otherCard.getByRole("button", { name: "Bind this device to Bar till" }).click();
    await expect(otherOutcome).toHaveText("Bound.");
    await expect(otherCard.locator('[data-outcome="till-status"]')).toHaveText(
      "This device is Bar till.",
    );
  } finally {
    await other.close();
    await edge.stop();
  }
});

// A device bound by hand, through the store server, to a till the store does not list was stuck: the
// card said it was none of the store's tills, Bind answered that it was already another, and nothing
// offered to release it. The store server names that till to the device that holds it, and the card
// says so and releases it, asking first as it does for one of the store's tills.
test("a device bound to a till the store no longer has is told so, and releases it", async ({
  page,
}) => {
  const edge = await startEdge("tills");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    // A till the `tills` store does not publish, claimed the way a call made by hand claims one.
    const gone = "0000000000000000000000000Z";
    const claimed = await page.evaluate(
      (agent) =>
        fetch("/api/print/agent", {
          method: "POST",
          headers: {
            authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}`,
            "content-type": "application/json",
          },
          body: JSON.stringify({ agent_device_id: agent }),
        }).then((response) => response.json()),
      gone,
    );
    expect(claimed).toEqual({ outcome: "BOUND" });

    await navigateTo(page, "/devices");
    const card = page.locator('[data-outcome="till"]');
    const status = card.locator('[data-outcome="till-status"]');
    const outcome = card.locator('[data-outcome="till-outcome"]');
    const till = (name) => card.locator("button[aria-pressed]").filter({ hasText: name });
    await expect(status).toHaveText("This device is bound to a till the store no longer has.");
    await expect(till("Counter till")).not.toContainText("This device");
    await expect(till("Bar till")).not.toContainText("This device");

    // Bind still refuses, and the card now shows what to release.
    await till("Bar till").click();
    await card.getByRole("button", { name: "Bind this device to Bar till" }).click();
    await expect(outcome).toHaveText("This device is already another till. Release that one first.");
    await expect(status).toHaveText("This device is bound to a till the store no longer has.");

    // Release asks first, and nothing is released until the answer.
    const releases = [];
    page.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/print/agent/revoke") {
        releases.push(request.postDataJSON());
      }
    });
    await card.getByRole("button", { name: "Release" }).click();
    await expect(
      card.getByText(
        "Release the till the store no longer has? This device stops being that till.",
      ),
    ).toBeVisible();
    expect(releases).toHaveLength(0);
    await card.getByRole("button", { name: "Release" }).click();
    await expect(outcome).toHaveText("Released.");
    await expect(status).toHaveText("This device is not one of the store's tills yet.");
    expect(releases).toEqual([{ agent_device_id: gone }]);

    await card.getByRole("button", { name: "Bind this device to Bar till" }).click();
    await expect(outcome).toHaveText("Bound.");
    await expect(status).toHaveText("This device is Bar till.");
  } finally {
    await edge.stop();
  }
});

// A store that publishes no till lists nothing to release from, so the till the store no longer has
// is said, and released, above the line that says where tills are created.
test("a device bound to a till in a store that lists none releases it all the same", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    const claimed = await page.evaluate(
      (agent) =>
        fetch("/api/print/agent", {
          method: "POST",
          headers: {
            authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}`,
            "content-type": "application/json",
          },
          body: JSON.stringify({ agent_device_id: agent }),
        }).then((response) => response.json()),
      "0000000000000000000000000Z",
    );
    expect(claimed).toEqual({ outcome: "BOUND" });

    await navigateTo(page, "/devices");
    const card = page.locator('[data-outcome="till"]');
    const status = card.locator('[data-outcome="till-status"]');
    await expect(status).toHaveText("This device is bound to a till the store no longer has.");
    await expect(card.locator('[data-outcome="till-empty"]')).toBeVisible();

    await card.getByRole("button", { name: "Release" }).click();
    await card.getByRole("button", { name: "Release" }).click();
    await expect(card.locator('[data-outcome="till-outcome"]')).toHaveText("Released.");
    await expect(status).toHaveCount(0);
    await expect(card.locator("button")).toHaveCount(0);
    await expect(card.locator('[data-outcome="till-empty"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// The card is drawn only for a person who may manage devices, as retiring one is (ADR-0158
// decision 8). Where the store enforces each person's own permissions, somebody whose role does not
// grant `admin.device.manage` is offered no Devices destination, and the screen reached by its
// address draws without the card.
test("the This device card is not drawn for a person who may not manage devices", async ({
  page,
}) => {
  const edge = await startEdge("tills");
  try {
    await aSessionThatSays(page, true, () => ["sales.line.add", "sales.table.manage"]);
    await pair(page, edge);
    await signIn(page, edge);
    await page.goto(`${edge.baseURL}/devices`);
    // The destinations are drawn once the session read has landed, so this waits for what the
    // person may do rather than passing on a bar not drawn yet.
    await expect
      .poll(async () => {
        const drawn = await destinations(page);
        return drawn.includes("/") && !drawn.includes("/devices");
      })
      .toBe(true);
    await expect(page.locator('[data-outcome="integrations"]')).toBeVisible();
    await expect(page.locator('[data-outcome="till"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// The store server decides who may see the tills from the person's own role in every store, so in
// a store that does not enforce each person's own set it refuses somebody the till offers the card
// to. The card is then not drawn, and the screen carries no error for it. The refusal is the
// store server's own words, because the demo's one employee holds every permission.
test("a person the store server refuses the tills to sees no card and no error", async ({ page }) => {
  const edge = await startEdge("tills");
  try {
    await page.route("**/api/print/agent", async (route) => {
      if (route.request().method() !== "GET") {
        await route.fallback();
        return;
      }
      await route.fulfill({
        status: 403,
        headers: { "pos-error-reason": "PERMISSION_DENIED" },
        body: "binding a print agent needs a manager signed in on this device",
      });
    });
    await pair(page, edge);
    await signIn(page, edge);
    const read = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname === "/api/print/agent" &&
        response.request().method() === "GET",
    );
    await navigateTo(page, "/devices");
    await read;
    await expect(page.locator('[data-outcome="integrations"]')).toBeVisible();
    await expect(page.locator('[data-outcome="till"]')).toHaveCount(0);
    await expect(page.getByRole("alert")).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// A store that publishes no terminal has no till to bind to, and the card says where tills come
// from in one line.
test("a store that publishes no till says where tills are created", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/devices");
    const card = page.locator('[data-outcome="till"]');
    await expect(card.locator('[data-outcome="till-empty"]')).toHaveText(
      "The store has no tills yet: they are created in the console under Devices → Terminals.",
    );
    await expect(card.locator("button")).toHaveCount(0);
    await expect(card.locator('[data-outcome="till-status"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// A drawer per till (ADR-0167). The `drawers` store keeps one for each of its two tills, the
// counter's on the store's float of 500,000₫ and the bar's on its own 200,000₫. A device becomes a
// till the way a manager makes it one, on the Devices screen, and the demo's one employee approves
// another till's drawer with their own badge and PIN, as a manager would.

/** Makes this device one of the store's tills on its Devices screen, then opens the Shift screen. */
async function bindTill(page, name) {
  await navigateTo(page, "/devices");
  const card = page.locator('[data-outcome="till"]');
  await card.locator("button[aria-pressed]").filter({ hasText: name }).click();
  await card.getByRole("button", { name: `Bind this device to ${name}` }).click();
  await expect(card.locator('[data-outcome="till-status"]')).toHaveText(`This device is ${name}.`);
  await navigateTo(page, "/shift");
}

/** A till's drawer in the Shift screen's list of every till's drawer. */
const drawerRow = (page, name) => page.locator('[data-step="pickDrawer"]').filter({ hasText: name });

/** Types the demo manager's badge and PIN where another till's drawer asks for them. */
async function approve(page, edge, pin = edge.staffPin) {
  await page.locator("#drawer-approver-code").fill(edge.staffCode);
  await page.locator("#drawer-approver-pin").fill(pin);
}

/** Every start this page asks the store server for, as it sent them. */
function startsSent(page) {
  const starts = [];
  page.on("request", (request) => {
    if (request.method() === "POST" && new URL(request.url()).pathname === "/api/shifts") {
      starts.push(request.postDataJSON());
    }
  });
  return starts;
}

// The second device pairs with a code this one gets on its Devices screen, as in the tills test.
test("a till's own drawer comes first on its own float, and another till's shift leaves it be", async ({
  page,
  browser,
}) => {
  const edge = await startEdge("drawers");
  const other = await browser.newPage();
  const heardOnLink = [];
  page.on("websocket", (socket) =>
    socket.on("framereceived", (frame) => {
      const text = String(frame.payload);
      if (text.includes("cash.shift.opened")) heardOnLink.push(text);
    }),
  );
  try {
    await pair(page, edge);
    await signIn(page, edge);
    // This till hears the counter's drawer open on its live link, which retries on a back-off
    // after its refusal before pairing and can still be down here; an event sent then never
    // reaches it.
    await expect(page.getByText("Store connected")).toBeVisible();
    await bindTill(page, "Bar till");
    await expect(page.locator('[data-outcome="own-drawer"]')).toHaveText(
      "Bar till — this till's drawer",
    );
    await expect(page.locator("#float")).toHaveValue("200000");

    await navigateTo(page, "/devices");
    await page.getByRole("button", { name: "Get a pairing code" }).click();
    const link = page.getByText(/\/pair\?code=\d{6}$/);
    const code = /code=(\d{6})/.exec((await link.textContent()) ?? "")?.[1];
    await other.goto(`${edge.baseURL}/pair?code=${code}`);
    await other.locator("#pair-submit").click();
    await expect(other).toHaveURL(/\/signin$/);
    await signIn(other, edge);
    await bindTill(other, "Counter till");
    await expect(other.locator("#float")).toHaveValue("500000");

    // The counter opens its own drawer. This till hears it, reads the drawers again — the read that
    // shows the counter's open is the one the event set off — and still shows its own closed.
    //
    // The Shift screen reads the drawers as it opens. That read is held here, answered by the edge
    // before the counter opens but delivered after the event, which is the race that made this test
    // flaky under load: the event's read used to join the read already out, whose answer predates
    // the open, and the till never read again.
    let deliver = () => {};
    const delivered = new Promise((resolve) => {
      deliver = resolve;
    });
    let heldOne = false;
    await page.route("**/api/shifts", async (route) => {
      if (heldOne || route.request().method() !== "GET") {
        await route.continue();
        return;
      }
      heldOne = true;
      const response = await fetchWhole(route);
      await delivered;
      await route.fulfill({ response });
    });
    await navigateTo(page, "/shift");
    await expect.poll(() => heldOne).toBe(true);
    const heard = page.waitForResponse(
      async (response) =>
        new URL(response.url()).pathname === "/api/shifts" &&
        response.request().method() === "GET" &&
        (await response.json()).drawers.some((drawer) => drawer.shift !== null),
    );
    await other.locator('[data-step="openShift"]').click();
    await expect(other.locator('[data-outcome="shift-open"]')).toBeVisible();
    // The held read lands only once this till's live link has heard the open.
    await expect.poll(() => heardOnLink.length).toBeGreaterThan(0);
    deliver();
    await expect(page.locator("#float")).toHaveValue("200000");
    await heard;
    await expect(page.locator('[data-outcome="shift-open"]')).toHaveCount(0);
    await expect(page.locator("header").getByText("No shift open")).toBeVisible();
    await expect(page.locator("#float")).toHaveValue("200000");
  } finally {
    await other.close();
    await edge.stop();
  }
});

test("a device that is no till says so, offers no drawer of its own, and is refused cash", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");
    await expect(page.locator('[data-outcome="no-till"]')).toHaveText(
      "This device is not one of the store's tills, so it has no drawer of its own: cash is taken at a till. A manager binds a device to a till on the Devices screen, under This device. A store with no tills yet creates them in the console, under Devices → Terminals, and publishes them to the store.",
    );
    await expect(page.locator('[data-outcome="own-drawer"]')).toHaveCount(0);
    await expect(page.locator('[data-step="openShift"]')).toHaveCount(0);
    await expect(page.locator('[data-step="askOpenDrawer"]')).toHaveCount(0);

    // A manager's tablet in the back office still starts a till's drawer, by naming it.
    await drawerRow(page, "Counter till").click();
    await approve(page, edge);
    await page.locator('[data-step="startDrawers"]').click();
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveText(
      "Started: Counter till.",
    );

    // Cash is refused here in the till's own words; a card still works.
    await navigateTo(page, "/");
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(
      page.getByText(
        "Cash is taken at a till, and this device is not one of the store's tills: a manager binds it to a till on the Devices screen. Cards and other ways to pay still work here.",
      ),
    ).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("a manager starts another till's drawer on that till's float, with a manager's PIN", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    // This till's own drawer comes first, on the store's float, since the counter sets none.
    await expect(page.locator('[data-outcome="own-drawer"]')).toHaveText(
      "Counter till — this till's drawer",
    );
    await expect(page.locator("#float")).toHaveValue("500000");
    await expect(drawerRow(page, "Counter till")).toContainText("This till");
    const bar = drawerRow(page, "Bar till");
    await expect(bar).toContainText("Closed");
    await expect(bar).toContainText("Float 200,000₫");

    const starts = startsSent(page);
    await bar.click();
    await expect(page.locator("#drawer-float")).toHaveValue("200000");
    const start = page.locator('[data-step="startDrawers"]');
    await expect(start).toHaveText("Start this drawer");
    await expect(start).toBeDisabled();
    await approve(page, edge);
    await start.click();
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveText("Started: Bar till.");
    expect(starts).toEqual([
      {
        opening_float: { currency_code: "VND", amount_minor: 200_000 },
        terminal_device_id: expect.any(String),
        approver_code: edge.staffCode,
        approver_pin: edge.staffPin,
      },
    ]);
    await expect(bar).toContainText(/Open since \d\d:\d\d/);
    // It was the bar's drawer that started: this till's own is still closed.
    await expect(drawerRow(page, "Counter till")).toContainText("Closed");
    await expect(page.locator('[data-step="openShift"]')).toBeVisible();
    // The manager's code and PIN went with that act alone.
    await bar.click();
    await expect(page.locator("#drawer-approver-code")).toHaveValue("");
    await expect(page.locator("#drawer-approver-pin")).toHaveValue("");
  } finally {
    await edge.stop();
  }
});

test("starting several drawers stops at the first refusal and says which did not start", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Bar till");
    const starts = startsSent(page);
    await drawerRow(page, "Counter till").click();
    await drawerRow(page, "Bar till").click();
    const start = page.locator('[data-step="startDrawers"]');
    await expect(start).toHaveText("Start 2 drawers");
    await expect(page.locator("#drawer-float")).toHaveCount(0);

    // A wrong PIN refuses the counter's drawer, and the bar's is not tried after it.
    await approve(page, edge, edge.staffPin === "0000" ? "1111" : "0000");
    await start.click();
    await expect(page.locator('[data-outcome="drawers-not-started"]')).toHaveText(
      "Not started: Counter till, Bar till.",
    );
    await expect(page.getByText("That manager PIN cannot authorise this.")).toBeVisible();
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveCount(0);
    expect(starts).toHaveLength(1);
    await expect(page.locator("#drawer-approver-code")).toHaveValue("");
    await expect(page.locator("#drawer-approver-pin")).toHaveValue("");
    // The refusal stands in place until it is dismissed (ADR-0196 safeguard 3).
    await page.locator('[data-step="dismissRefusal"]').click();

    // Both are still picked, and each starts on its own till's float.
    await approve(page, edge);
    await start.click();
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveText(
      "Started: Counter till, Bar till.",
    );
    expect(starts.slice(1).map((sent) => sent.opening_float.amount_minor)).toEqual([500_000, 200_000]);
    // This till's own drawer took no approver, and shows open at the top of the screen.
    expect(starts[1]?.approver_pin).toBe(edge.staffPin);
    expect(starts[2]?.approver_pin).toBeUndefined();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("two counted drawers close together, and each shows its figures", async ({ page }) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    // This till's drawer from the top of the screen, as every store opens one; the bar's from the
    // list, with a manager's PIN.
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await drawerRow(page, "Bar till").click();
    await approve(page, edge);
    await page.locator('[data-step="startDrawers"]').click();
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveText("Started: Bar till.");

    // Counted blind, drawer by drawer: this till's at the top, the bar's from the list.
    await page.locator("#count").fill("500000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await drawerRow(page, "Bar till").click();
    await expect(page.getByText("should hold")).toHaveCount(0);
    await page.locator("#drawer-count").fill("190000");
    await approve(page, edge);
    await page.locator('[data-step="countDrawer"]').click();
    await expect(page.locator('[data-outcome="drawer-counted"]')).toHaveText(
      "Bar till counted. Close it to reveal the variance.",
    );
    await expect(drawerRow(page, "Bar till")).toContainText("Counted, waiting to close");

    const closes = [];
    page.on("request", (request) => {
      if (request.method() === "POST" && new URL(request.url()).pathname.startsWith("/api/shifts:")) {
        closes.push(request.postDataJSON());
      }
    });
    await drawerRow(page, "Counter till").click();
    await drawerRow(page, "Bar till").click();
    await approve(page, edge);
    await page.locator('[data-step="closeDrawers"]').click();
    const closed = page.locator('[data-outcome="drawers-closed"]');
    await expect(closed).toContainText("2 drawers closed");
    const counter = closed.locator('[data-outcome="drawer-closed"]').nth(0);
    const bar = closed.locator('[data-outcome="drawer-closed"]').nth(1);
    await expect(counter).toContainText("Counter till");
    await expect(counter).toContainText("Expected 500,000₫");
    await expect(counter).toContainText("Variance 0₫");
    await expect(bar).toContainText("Bar till");
    await expect(bar).toContainText("Expected 200,000₫");
    await expect(bar).toContainText("counted 190,000₫");
    await expect(bar).toContainText("Variance -10,000₫");
    // Each drawer's report prints where it is closed, and this store has no printer to print it.
    await expect(counter).toContainText("No printer is set up for this store");
    await expect(bar).toContainText("No printer is set up for this store");
    expect(closes).toEqual([
      {
        shift_ids: [expect.any(String), expect.any(String)],
        approver_code: edge.staffCode,
        approver_pin: edge.staffPin,
      },
    ]);
    // This till's own drawer shows its close at the top of the screen too.
    await expect(page.locator('[data-outcome="shift-closed"]')).toBeVisible();
    await expect(drawerRow(page, "Bar till")).toContainText("Closed");
  } finally {
    await edge.stop();
  }
});

// A close over or short by more than the store allows gives a reason (ADR-0167 decision 12). The
// `drawers` store asks one beyond 20,000₫, and the framework's own reasons offer two for it: Staff
// error, `00000000000000000000000004`, and Counting difference, `00000000000000000000000009`.
test("a close beyond the store's limit shows the variance and closes with the reason picked", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await page.locator("#count").fill("450000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();

    const closes = [];
    page.on("request", (request) => {
      if (/^\/api\/shifts\/[^/]+\/close$/.test(new URL(request.url()).pathname)) {
        closes.push(request.postDataJSON());
      }
    });
    await page.locator('[data-step="closeShift"]').click();
    const owed = page.locator('[data-outcome="variance-owed"]');
    await expect(owed).toContainText("Over or short by more than the store allows. Why?");
    await expect(owed).toContainText("Expected 500,000₫ · counted 450,000₫");
    await expect(owed).toContainText("Variance -50,000₫");
    await expect(page.locator('[data-outcome="shift-closed"]')).toHaveCount(0);
    await owed.getByRole("button", { name: "Counting difference" }).click();
    await expect(page.locator('[data-outcome="shift-closed"]')).toContainText("-50,000₫");
    await expect(owed).toHaveCount(0);
    expect(closes).toEqual([null, { reason_code_id: "00000000000000000000000009" }]);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for the drawer list: a start whose answer is lost is read again from the list of drawers,
// and the drawer shows started; a batch close whose answer is lost is found made because none of its
// shifts is listed any more, and its figures, which only the close's answer gives, are left to the
// shift's report rather than worked out here.
test("a start and a batch close from the list whose answers are lost are checked, not sent again", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    const starts = startsSent(page);
    const lose = async (route) => {
      if (route.request().method() !== "POST") {
        await route.fallback();
        return;
      }
      await route.fetch();
      await route.abort("connectionreset");
    };
    await page.route("**/api/shifts", lose);
    await drawerRow(page, "Bar till").click();
    await approve(page, edge);
    await page.locator('[data-step="startDrawers"]').click();
    await expect(page.locator('[data-outcome="open-pending"]')).toContainText("Checking with the store server");
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveText("Started: Bar till.");
    await expect(drawerRow(page, "Bar till")).toContainText("Open");
    expect(starts).toHaveLength(1);
    await page.unroute("**/api/shifts", lose);

    await page.locator("#count").fill("500000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await drawerRow(page, "Bar till").click();
    await page.locator("#drawer-count").fill("200000");
    await approve(page, edge);
    await page.locator('[data-step="countDrawer"]').click();
    await expect(page.locator('[data-outcome="drawer-counted"]')).toBeVisible();

    let closes = 0;
    await page.route("**/api/shifts:batch_close", async (route) => {
      closes += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await drawerRow(page, "Counter till").click();
    await drawerRow(page, "Bar till").click();
    await approve(page, edge);
    await page.locator('[data-step="closeDrawers"]').click();
    await expect(page.locator('[data-outcome="close-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="drawers-closed-unread"]')).toContainText("read it on the shift's report");
    await expect(drawerRow(page, "Bar till")).toContainText("Closed");
    expect(closes).toBe(1);
  } finally {
    await edge.stop();
  }
});

// ADR-0204: a count from the list that never reached the edge is found not made, and says so.
test("a count from the list that never reached the store server is found not made", async ({ page }) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    await drawerRow(page, "Bar till").click();
    await approve(page, edge);
    await page.locator('[data-step="startDrawers"]').click();
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveText("Started: Bar till.");
    await page.route("**/api/shifts/*/count", (route) => route.abort("connectionreset"));
    await drawerRow(page, "Bar till").click();
    await page.locator("#drawer-count").fill("200000");
    await approve(page, edge);
    await page.locator('[data-step="countDrawer"]').click();
    await expect(page.locator('[data-outcome="count-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("The count was not recorded");
    await expect(drawerRow(page, "Bar till")).not.toContainText("Counted");
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for a handover: the outgoing count and close whose answers are lost are read again, and
// the incoming cashier is shown the count the edge recorded (90,000₫ for one typed as 100,000₫, as
// when another device's count reached it first), never the one typed; the incoming opening whose
// answer is lost is found made as the shift that took the outgoing one over.
test("a handover whose answers are lost goes on with the edge's count, never re-sent", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    let sent = 0;
    await page.route("**/api/shifts/*/count", async (route) => {
      sent += 1;
      const body = { ...route.request().postDataJSON(), counted_minor: 90000 };
      await route.fetch({ postData: JSON.stringify(body) });
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="beginHandover"]').click();
    await page.locator("#handover-out").fill("100000");
    await page.locator('[data-step="handOut"]').click();
    await expect(page.locator('[data-outcome="close-pending"]')).toBeVisible();
    // The count's answer was lost, so the close was never sent: the re-read finds the shift counted.
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("The shift was not closed");
    await page.locator('[data-step="dismissRefusal"]').click();
    await page.route("**/api/shifts/*/close", async (route) => {
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="handOut"]').click();
    await expect(page.locator('[data-outcome="handover-closed"]')).toContainText("90,000₫");
    expect(sent).toBe(1);

    let opens = 0;
    await page.route("**/api/shifts", async (route) => {
      if (route.request().method() !== "POST") {
        await route.fallback();
        return;
      }
      opens += 1;
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator("#handover-incoming-code").fill(edge.staffCode);
    await page.locator("#handover-incoming-pin").fill(edge.staffPin);
    await page.locator('[data-step="handoverSignIn"]').click();
    await page.locator("#handover-in").fill("90000");
    await page.locator('[data-step="handIn"]').click();
    await expect(page.locator('[data-outcome="open-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="handover-done"]')).toContainText("0₫");
    expect(opens).toBe(1);
  } finally {
    await edge.stop();
  }
});

test("drawers closed together each take the reason the store asks, under the one approver", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await drawerRow(page, "Bar till").click();
    await approve(page, edge);
    await page.locator('[data-step="startDrawers"]').click();
    await expect(page.locator('[data-outcome="drawers-started"]')).toHaveText("Started: Bar till.");

    // The counter's 30,000₫ short and the bar's 50,000₫ short, each beyond the limit.
    await page.locator("#count").fill("470000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await drawerRow(page, "Bar till").click();
    await page.locator("#drawer-count").fill("150000");
    await approve(page, edge);
    await page.locator('[data-step="countDrawer"]').click();
    await expect(page.locator('[data-outcome="drawer-counted"]')).toBeVisible();

    const closes = [];
    page.on("request", (request) => {
      if (request.method() === "POST" && new URL(request.url()).pathname.startsWith("/api/shifts:")) {
        closes.push(request.postDataJSON());
      }
    });
    await drawerRow(page, "Counter till").click();
    await drawerRow(page, "Bar till").click();
    await approve(page, edge);
    await page.locator('[data-step="closeDrawers"]').click();
    const owed = page.locator('[data-outcome="drawer-variance-owed"]');
    await expect(owed).toHaveCount(2);
    await expect(owed.nth(0)).toContainText("Counter till");
    await expect(owed.nth(0)).toContainText("Variance -30,000₫");
    await expect(owed.nth(1)).toContainText("Bar till");
    await expect(owed.nth(1)).toContainText("Expected 200,000₫ · counted 150,000₫");
    // A reason for each drawer, and the close goes again once the last one has its own.
    await owed.nth(0).getByRole("button", { name: "Staff error" }).click();
    await expect(page.locator('[data-outcome="drawers-closed"]')).toHaveCount(0);
    await owed.nth(1).getByRole("button", { name: "Counting difference" }).click();
    await expect(page.locator('[data-outcome="drawers-closed"]')).toContainText("2 drawers closed");
    const batch = {
      shift_ids: [expect.any(String), expect.any(String)],
      approver_code: edge.staffCode,
      approver_pin: edge.staffPin,
    };
    expect(closes).toEqual([
      batch,
      {
        ...batch,
        variance_reasons: [
          { shift_id: expect.any(String), reason_code_id: "00000000000000000000000004" },
          { shift_id: expect.any(String), reason_code_id: "00000000000000000000000009" },
        ],
      },
    ]);
  } finally {
    await edge.stop();
  }
});

test("every till's drawer is listed only for a person who may act on another till's", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    const granted = ["cash.shift.open", "cash.shift.close", "admin.device.manage"];
    const withApproval = [];
    await aSessionThatSays(page, true, () => granted, withApproval);
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    // A cashier keeps to their own till's drawer.
    await expect(page.locator('[data-outcome="own-drawer"]')).toBeVisible();
    await expect(page.locator('[data-outcome="drawers"]')).toHaveCount(0);

    // With a manager's approval they may act on another till's, which asks for the manager there.
    withApproval.push("cash.shift.manage_other_till");
    await page.goto(`${edge.baseURL}/shift`);
    await drawerRow(page, "Bar till").click();
    await expect(page.locator("#drawer-approver-code")).toBeVisible();
    // Their own till's drawer asks for nobody.
    await drawerRow(page, "Counter till").click();
    await drawerRow(page, "Bar till").click();
    await expect(page.locator('[data-step="startDrawers"]')).toHaveText("Start this drawer");
    await expect(page.locator("#drawer-approver-code")).toHaveCount(0);

    // Held directly, another till's asks for nobody either.
    granted.push("cash.shift.manage_other_till");
    await page.goto(`${edge.baseURL}/shift`);
    await drawerRow(page, "Bar till").click();
    await expect(page.locator('[data-step="startDrawers"]')).toBeVisible();
    await expect(page.locator("#drawer-approver-code")).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// The example keeps the machine's clock, and its business day is fixed when it boots, so no drawer
// passes its day's end within a test. The two shift reads are rewritten to say one has, as
// `aSessionThatSays` rewrites a session, and the store as waiting to change back to one drawer.
test("a drawer open past its business day is flagged, and a change of model waits in words", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    const ended = (shift) => (shift === null ? null : { ...shift, day_ended: true });
    await page.route("**/api/shifts/current", async (route) => {
      const response = await fetchWhole(route);
      await route.fulfill({ response, json: ended(await response.json()) });
    });
    await page.route("**/api/shifts", async (route) => {
      if (route.request().method() !== "GET") {
        await route.fallback();
        return;
      }
      const response = await fetchWhole(route);
      const read = await response.json();
      await route.fulfill({
        response,
        json: {
          ...read,
          waiting_drawer_model: "DRAWER_MODEL_PER_STORE",
          drawers: read.drawers.map((drawer) => ({ ...drawer, shift: ended(drawer.shift) })),
        },
      });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    // Read again when the screen opens, as a till does on a shift still trading.
    await navigateTo(page, "/");
    await navigateTo(page, "/shift");
    await expect(page.locator('[data-outcome="day-ended"]')).toHaveText(
      "This drawer's shift began on an earlier business day. Count and close it.",
    );
    await expect(
      drawerRow(page, "Counter till").locator('[data-outcome="drawer-day-ended"]'),
    ).toHaveText("Past its business day: count and close it");
    await expect(drawerRow(page, "Bar till").locator('[data-outcome="drawer-day-ended"]')).toHaveCount(0);
    await expect(page.locator('[data-outcome="model-waiting"]')).toHaveText(
      "The store changes to one drawer for the whole store once every till's drawer is closed.",
    );
  } finally {
    await edge.stop();
  }
});

// Where a store assigns drawers (ADR-0167 decision 9), the till says whose a drawer is by id alone,
// asks for the override approver on another person's, and asks whom another till's drawer is for by
// staff code. The example shares its drawers, so the two shift reads are rewritten to say the store
// assigns them and that the counter's drawer is another person's.
test("an assigned drawer says whose it is and asks for the override on another person's", async ({
  page,
}) => {
  const edge = await startEdge("drawers");
  try {
    const another = (shift) =>
      shift === null ? null : { ...shift, assigned_employee_id: "01ANOTHERPERS0NAAAAAAAAAAA" };
    await page.route("**/api/shifts/current", async (route) => {
      const response = await fetchWhole(route);
      await route.fulfill({ response, json: another(await response.json()) });
    });
    await page.route("**/api/shifts", async (route) => {
      if (route.request().method() !== "GET") {
        await route.fallback();
        return;
      }
      const response = await fetchWhole(route);
      const read = await response.json();
      await route.fulfill({
        response,
        json: {
          ...read,
          drawer_access: "DRAWER_ACCESS_ASSIGNED",
          drawers: read.drawers.map((drawer) => ({ ...drawer, shift: another(drawer.shift) })),
        },
      });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await bindTill(page, "Counter till");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await navigateTo(page, "/");
    await navigateTo(page, "/shift");
    await expect(page.locator('[data-drawer-owner="other"]').first()).toHaveText(
      "Another person's drawer for this shift.",
    );
    await expect(page.locator("#override-approver-code")).toBeVisible();
    await drawerRow(page, "Bar till").click();
    await expect(page.locator("#drawer-assignee")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// A role changed in the console reaches an open till with the configuration, without a new sign-in:
// the edge says `config_applied` on the live link (ADR-0160 decision 7) and the till reads the
// session again. The frame is given the next position on the stream, so it is heard as itself.
test("a till hears a new configuration and reads again what the person may do", async ({ page }) => {
  const edge = await startEdge();
  try {
    const granted = ["sales.line.add", "sales.table.manage"];
    await aSessionThatSays(page, true, () => granted);
    let link = null;
    let last = null;
    const carried = [];
    await page.routeWebSocket(
      (url) => url.pathname === "/ws",
      (socket) => {
        link = socket;
        socket.connectToServer().onMessage((message) => {
          const frame = JSON.parse(String(message));
          const { stream_id: streamId, sequence } = frame;
          last = typeof sequence === "number" ? { streamId, sequence } : last;
          carried.push(frame.event_type);
          socket.send(message);
        });
      },
    );
    await pair(page, edge);
    await signIn(page, edge);
    // The link's first attempt was refused before the device paired, and it retries on a back-off,
    // so it can still be down when the till reaches the floor. A line added then is announced to no
    // one and never carried. Seated and added once the link is open, the line's event is.
    await expect(page.getByText("Store connected")).toBeVisible();
    await seatTable(page);
    await addItem(page);
    await expect(page.locator('[data-step="fireOrder"]')).toHaveCount(0);
    // The link has carried the line's own event, so the frame below is the next one after
    // everything the edge has said, rather than one that races the line's.
    await expect.poll(() => carried.includes("sales.order_line.added")).toBe(true);

    granted.push("sales.line.fire");
    const position = last === null ? {} : { stream_id: last.streamId, sequence: last.sequence + 1 };
    link.send(JSON.stringify({ type: "config_applied", config_version_id: "replay", ...position }));
    await expect(page.locator('[data-step="fireOrder"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// The idle lock (ADR-0160 decision 2, `docs/pos-spec.md` §16). The `idle-lock` store publishes
// `session.idle_lock_seconds` = 120, the value a new store is given, and the page's clock is the
// test's, so two minutes pass in one call rather than in two minutes.

/** Who the store server says is signed in on this device, read the way the till reads it. */
const sessionRead = () =>
  fetch("/api/session", {
    headers: { authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}` },
  }).then((response) => response.json());

test("a till left untouched locks, signs its person out, and opens again with their PIN where they were", async ({
  page,
}) => {
  const edge = await startEdge("idle-lock");
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    const where = new URL(page.url()).pathname;

    await page.clock.fastForward(119_000);
    await expect(page.locator('[data-outcome="locked"]')).toHaveCount(0);
    await page.clock.fastForward(2_000);
    await expect(page.locator('[data-outcome="locked"]')).toBeVisible();

    // The store server has let the sign-in go, so the covered screen can do nothing.
    await expect.poll(async () => (await page.evaluate(sessionRead)).signed_in).toBe(false);

    // The person comes back. Their PIN alone opens the till, on the screen they left.
    await expect(page.locator("#lock-code")).toHaveCount(0);
    await page.locator("#lock-pin").fill(edge.staffPin);
    await page.locator("#lock-submit").click();
    await expect(page.locator('[data-outcome="locked"]')).toHaveCount(0);
    expect(new URL(page.url()).pathname).toBe(where);
    await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
    expect((await page.evaluate(sessionRead)).signed_in).toBe(true);
  } finally {
    await edge.stop();
  }
});

test("a touch or a key puts the idle lock off", async ({ page }) => {
  const edge = await startEdge("idle-lock");
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);

    await page.clock.fastForward(100_000);
    await page.keyboard.press("Shift");
    await page.clock.fastForward(100_000);
    await expect(page.locator('[data-outcome="locked"]')).toHaveCount(0);
    await page.clock.fastForward(21_000);
    await expect(page.locator('[data-outcome="locked"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("the kitchen board and the pass never lock, however long they are left", async ({ page }) => {
  const edge = await startEdge("idle-lock");
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);

    for (const path of ["/kds", "/expo"]) {
      await navigateTo(page, path);
      await page.clock.fastForward(10 * 60_000);
      await expect(page.locator('[data-outcome="locked"]')).toHaveCount(0);
      expect((await page.evaluate(sessionRead)).signed_in, `${path} stays signed in`).toBe(true);
    }

    // It is the screen that never locks, not the device: back on the floor, the lock applies.
    await navigateTo(page, "/");
    await page.clock.fastForward(121_000);
    await expect(page.locator('[data-outcome="locked"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("a store that publishes no idle lock never locks", async ({ page }) => {
  const edge = await startEdge();
  try {
    await page.clock.install();
    await pair(page, edge);
    await signIn(page, edge);

    await page.clock.fastForward(10 * 60_000);
    await expect(page.locator('[data-outcome="locked"]')).toHaveCount(0);
    expect((await page.evaluate(sessionRead)).signed_in).toBe(true);
  } finally {
    await edge.stop();
  }
});

// The store's data loads through one engine (ADR-0173): a required piece that will not load holds
// the till behind a Retry, a failed read is retried on its own, and an optional piece that will
// not load leaves the till working and says what is missing and why a button waits.

/** Types the demo badge and PIN, without waiting for the floor: a gated till does not draw it. */
async function signInWithoutWaiting(page, edge) {
  await page.locator("#signin-code").fill(edge.staffCode);
  await page.locator("#signin-pin").fill(edge.staffPin);
  await page.locator('[data-step="submit"]').click();
}

/** Answers `pattern` with a 503 while `failing()` holds, and counts how often it did. */
async function refuseWhile(page, pattern, failing) {
  const refused = { count: 0 };
  await page.route(pattern, async (route) => {
    if (failing()) {
      refused.count += 1;
      await route.fulfill({ status: 503, body: "busy" });
    } else {
      await route.continue();
    }
  });
  return refused;
}

/**
 * Records, in the page, that someone has tapped a Retry. The engine retries a failed read on its
 * own as well (ADR-0173), so a stub that started answering before the tap would let that backoff
 * succeed first: the Retry would unmount under the click, and the flow would wait on a detached
 * button. Answering only once the tap is recorded leaves the tap the one way through.
 */
async function watchRetryTaps(page) {
  const watch = () => {
    if (window.__retryWatched === true) {
      return;
    }
    window.__retryWatched = true;
    addEventListener(
      "click",
      (event) => {
        if (
          event.target instanceof Element &&
          event.target.closest('[data-step="retryLoad"], [data-step="retryRead"]') !== null
        ) {
          window.__retryTapped = true;
        }
      },
      { capture: true },
    );
  };
  // Every document from now on, and the one already open: a flow that moves on by a link click
  // never loads another.
  await page.addInitScript(watch);
  await page.evaluate(watch).catch(() => undefined);
}

/** Whether a Retry has been tapped in the page as it is now; a page between documents has not. */
async function retryTapped(page) {
  try {
    return await page.evaluate(() => window.__retryTapped === true);
  } catch {
    return false;
  }
}

/** Answers `pattern` with a 503 until a Retry is tapped, and counts how often it did. */
async function refuseUntilRetried(page, pattern) {
  await watchRetryTaps(page);
  const refused = { count: 0 };
  await page.route(pattern, async (route) => {
    if (await retryTapped(page)) {
      await route.continue();
      return;
    }
    refused.count += 1;
    await route.fulfill({ status: 503, body: "busy" });
  });
  return refused;
}

test("a price book that will not load holds the till behind a Retry, and the Retry draws it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    const refused = await refuseUntilRetried(page, "**/api/menu");
    await pair(page, edge);
    await signInWithoutWaiting(page, edge);

    const gate = page.locator('[data-outcome="store-loading"]');
    await expect(gate).toContainText("Could not load: Price book");
    await expect(page.locator('[data-outcome="floor"]')).toHaveCount(0);
    expect(refused.count).toBeGreaterThan(0);

    await page.locator('[data-step="retryLoad"]').click();
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
    await expect(gate).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

test("a read that fails is tried again on its own, and the till is drawn without a tap", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    let answered = 0;
    const refused = await refuseWhile(page, "**/api/locale", () => {
      answered += 1;
      return answered <= 2;
    });
    await pair(page, edge);
    await signInWithoutWaiting(page, edge);
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
    expect(refused.count, "two refusals, then the read that drew the till").toBe(2);
  } finally {
    await edge.stop();
  }
});

test("reason codes that will not load leave the till selling, say so, and say why a void waits", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await refuseUntilRetried(page, "**/api/reason-codes");
    await pair(page, edge);
    await signIn(page, edge);

    const banner = page.locator('[data-outcome="store-data-missing"]');
    await expect(banner).toContainText("Reason codes");
    await seatTable(page);
    await addItem(page);
    const voidLine = page.locator('[data-step="askVoid"]').first();
    await expect(voidLine).toBeDisabled();
    await expect(page.locator("#why-reasons")).toHaveText("Could not load: Reason codes");
    await expect(voidLine).toHaveAttribute("aria-describedby", "why-reasons");

    await banner.locator('[data-step="retryLoad"]').click();
    await expect(banner).toHaveCount(0);
    await expect(voidLine).toBeEnabled();
    await expect(page.locator("#why-reasons")).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

test("open orders that will not load hold the till behind a Retry, and the Retry draws them", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    const table = new URL(page.url()).pathname;

    await refuseUntilRetried(page, "**/api/orders/live");
    await page.goto(`${edge.baseURL}${table}`);
    const gate = page.locator('[data-outcome="store-loading"]');
    await expect(gate).toContainText("Could not load: Open orders");
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(0);

    await page.locator('[data-step="retryLoad"]').click();
    await expect(gate).toHaveCount(0);
    await expect(page.locator('[data-outcome="line-added"]').first()).toBeAttached();
  } finally {
    await edge.stop();
  }
});

test("a signed-in till that reloads reads its session once", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    let reads = 0;
    page.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/session" && request.method() === "GET") {
        reads += 1;
      }
    });
    await page.reload();
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
    // Every read the store's load makes has answered once the floor is drawn and the open orders
    // are in; a second read of the session would have gone out with them.
    await page.waitForLoadState("networkidle");
    expect(reads, "the boot gate's read, applied, and not read again by the store's load").toBe(1);
  } finally {
    await edge.stop();
  }
});

test("a screen's own read that fails says so in place, and its Retry reads again", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    const refused = await refuseUntilRetried(page, "**/api/bills/settled");
    await navigateTo(page, "/today");

    const failed = page.locator('[data-outcome="read-failed"]');
    await expect(failed).toBeVisible();
    expect(refused.count).toBeGreaterThan(0);

    await failed.locator('[data-step="retryRead"]').click();
    await expect(failed).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

test("a store server that does not answer is not told it has no cloud", async ({ page }) => {
  const edge = await startEdge();
  try {
    // The app's own boot check answers first, "not activated", which keeps the browser on the
    // setup screen; the screen's reads after it are refused until its Retry is tapped.
    await watchRetryTaps(page);
    let asked = 0;
    await page.route("**/api/activation", async (route) => {
      asked += 1;
      if (asked === 1 || (await retryTapped(page))) {
        await route.fulfill({ json: { activated: false } });
      } else {
        await route.fulfill({ status: 503, body: "busy" });
      }
    });
    await page.goto(`${edge.baseURL}/setup`);
    await expect(page.getByText("Checking whether this store is activated…")).toBeVisible();
    await expect(page.locator('[data-outcome="read-failed"]')).toBeVisible();
    await expect(page.getByText(/runs without a cloud/)).toHaveCount(0);

    await page.locator('[data-step="retryRead"]').click();
    await expect(page.locator('[data-outcome="read-failed"]')).toHaveCount(0);
    await expect(page.getByText("Checking whether this store is activated…")).toHaveCount(0);
    await expect(page.locator("input").first()).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// The till keeps a snapshot of the store's configuration, and nothing else (ADR-0173 decision 3).

/** Every record in the till's snapshot, as `{ key, etag, ... }`, or `[]` where there is none. */
async function snapshotRecords(page) {
  return page.evaluate(
    () =>
      new Promise((resolve) => {
        const request = indexedDB.open("pos-till-snapshot", 1);
        request.onupgradeneeded = () => request.result.createObjectStore("entries", { keyPath: "key" });
        request.onerror = () => resolve([]);
        request.onsuccess = () => {
          const read = request.result.transaction("entries", "readonly").objectStore("entries").getAll();
          read.onsuccess = () => {
            request.result.close();
            resolve(read.result);
          };
          read.onerror = () => resolve([]);
        };
      }),
  );
}

/** The configuration entries the snapshot may hold on the demo store, whose walk-ins are takeaway. */
const KEPT = ["layout", "locale", "menu", "menu_takeaway", "reason_codes"];

test("the snapshot holds the store's configuration and nothing about anyone", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await expect
      .poll(async () => (await snapshotRecords(page)).map((record) => record.key).sort())
      .toEqual(KEPT);
    const records = await snapshotRecords(page);
    const held = JSON.stringify(records);
    // Never the session, the person, the token, a drawer, a shift, an order or the floor: none of
    // the fields that carry them appears anywhere in it.
    for (const field of [
      "signed_in",
      "employee_id",
      "permissions",
      "assigned_employee_id",
      "drawer_model",
      "shift_id",
      "order_id",
      "order_line_id",
      "table_states",
      "seated_times",
    ]) {
      expect(held, field).not.toContain(`"${field}"`);
    }
    expect(held).not.toContain(await page.evaluate(() => localStorage.getItem("pos-edge.device-token")));
    for (const record of records) {
      expect(record.schema).toBe(1);
      expect(record.etag).toMatch(/^W\/"/);
    }
  } finally {
    await edge.stop();
  }
});

test("a reload confirms the snapshot's price book before anything is sold against it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await expect.poll(async () => (await snapshotRecords(page)).length).toBe(KEPT.length);

    // The edge cannot be reached for the price book: the snapshot's copy is never drawn, and the
    // till waits for the edge rather than sell against it.
    await refuseUntilRetried(page, "**/api/menu");
    await page.reload();
    await expect(page.locator('[data-outcome="store-loading"]')).toContainText("Could not load: Price book");
    await expect(page.locator('[data-step="onCard"]')).toHaveCount(0);

    // Once it answers, the snapshot's tag is confirmed with a `304`, and the till sells. The
    // refusals before the tap are not that answer.
    const confirmed = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname === "/api/menu" &&
        new URL(response.url()).search === "" &&
        response.status() !== 503,
    );
    await page.locator('[data-step="retryLoad"]').click();
    const answer = await confirmed;
    expect(answer.status()).toBe(304);
    expect(answer.request().headers()["if-none-match"]).toMatch(/^W\/"/);
    await seatTable(page);
    await addItem(page);
  } finally {
    await edge.stop();
  }
});

test("signing out clears the snapshot, and a new pairing never reads the old one", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await expect.poll(async () => (await snapshotRecords(page)).length).toBe(KEPT.length);

    // Another pairing: whatever the snapshot holds is not this one's, and no tag of it is sent.
    await page.evaluate(() => localStorage.setItem("pos-edge.pairing-id", "a-later-pairing"));
    const tags = [];
    page.on("request", (request) => {
      if (new URL(request.url()).pathname.startsWith("/api/")) {
        tags.push(request.headers()["if-none-match"]);
      }
    });
    await page.reload();
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
    expect(tags.filter((tag) => tag !== undefined)).toEqual([]);

    await page.getByRole("button", { name: "Sign out" }).click();
    await expect(page).toHaveURL(/\/signin$/);
    await expect.poll(async () => (await snapshotRecords(page)).length).toBe(0);
  } finally {
    await edge.stop();
  }
});

test("a till with no IndexedDB, as in a private window, works and says nothing about it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.addInitScript(() => {
      Object.defineProperty(window, "indexedDB", {
        get() {
          throw new Error("blocked");
        },
      });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await expect(page.locator('[data-outcome="store-data-missing"]')).toHaveCount(0);
    await seatTable(page);
    await addItem(page);
    await page.reload();
    await expect(page.locator('[data-outcome="line-added"], [data-outcome="floor"]').first()).toBeAttached();
    await expect(page.locator('[data-outcome="store-loading"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// Three acts are painted before the edge answers (ADR-0173 decision 4): adding a line, changing a
// quantity and moving a table. Each rolls back on a refusal, with the refusal's own sentence.

/** Answers `pattern`'s POST with a refusal carrying `reason`, as the edge does. */
async function refusePost(page, pattern, reason) {
  await page.route(pattern, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    await route.fulfill({ status: 409, headers: { "pos-error-reason": reason }, body: reason });
  });
}

test("an added line shows at once, sending, and payment waits until the edge confirms it", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    // The answer, and the live link's event with it, held back: only the till's own paint is left.
    const held = await holdPost(page, "**/api/tables/*/lines");
    await page.locator('[data-step="onItem"]').first().click();
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveText("Sending…");
    await expect(page.locator('[data-step="takePayment"]')).toBeDisabled();
    await expect(page.locator("#why-pending")).toHaveText(
      "Waiting for the store to confirm the last change.",
    );
    held.release();

    await expect(page.locator('[data-outcome="line-sending"]')).toHaveCount(0);
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(1);
    await expect(page.locator('[data-step="takePayment"]')).toBeEnabled();
  } finally {
    await edge.stop();
  }
});

// Seating a free table is painted at once too (ADR-0188): the Order screen opens before the edge
// answers, lines added meanwhile wait for the seat, and payment waits until it is confirmed.
test("a free table opens its order at once, and its lines and payment wait for the seat", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    const held = await holdPost(page, "**/api/tables/*/seat");
    await page.locator('[data-step="onCard"]').first().click();
    await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
    await page.locator('[data-step="onItem"]').first().click();
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveText("Sending…");
    await expect(page.locator('[data-step="takePayment"]')).toBeDisabled();
    held.release();

    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(1);
    await expect(page.locator('[data-step="takePayment"]')).toBeEnabled();
  } finally {
    await edge.stop();
  }
});

test("a seat the edge refuses goes back to the floor, saying why and keeping what was typed", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    let release = () => {};
    const released = new Promise((resolve) => {
      release = resolve;
    });
    await page.route("**/api/tables/*/seat", async (route) => {
      await released;
      await route.fulfill({
        status: 409,
        headers: { "pos-error-reason": "BILL_ALREADY_OPEN" },
        body: "BILL_ALREADY_OPEN",
      });
    });
    const lines = [];
    page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/lines")) {
        lines.push(request.url());
      }
    });
    await page.locator('[data-step="onCard"]').first().click();
    await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
    const item = await page.locator('[data-step="onItem"]').first().innerText();
    await page.locator('[data-step="onItem"]').first().click();
    release();

    const refused = page.locator('[data-outcome="seat-refused"]');
    await expect(refused).toContainText("A bill is already open for this order.");
    await expect(refused).toContainText(item.split("\n")[0]);
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
    expect(lines).toEqual([]);
  } finally {
    await edge.stop();
  }
});

/**
 * Double-taps `step` while its POST to `pattern` is held, and checks the button says `pending` and
 * takes no second tap until the answer. A money act paints its pending state instead of wording the
 * button (ADR-0196): `{ painted }` names that state's outcome. Resolves to how many POSTs reached `pattern` once `outcome`
 * shows; the route is taken off again, so the next act is not held.
 */
async function doubleTap(page, step, pattern, pending, outcome) {
  let posts = 0;
  let release = () => {};
  const released = new Promise((resolve) => {
    release = resolve;
  });
  const hold = async (route) => {
    if (route.request().method() !== "POST") {
      await route.fallback();
      return;
    }
    posts += 1;
    await released;
    await route.continue();
  };
  await page.route(pattern, hold);
  const button = page.locator(`[data-step="${step}"]`).first();
  await button.dblclick();
  if (typeof pending === "string") {
    await expect(button).toHaveText(pending);
  } else {
    await expect(page.locator(`[data-outcome="${pending.painted}"]`)).toBeVisible();
  }
  await expect(button).toBeDisabled();
  release();
  await expect(page.locator(`[data-outcome="${outcome}"]`)).toBeVisible();
  await page.unroute(pattern, hold);
  return posts;
}

// A tap that writes says so at once and takes no second tap until the edge answers (`docs/ui-ux.md`
// §4). The button used to look untouched while the request was out, so a cashier tapped again and
// a second send or settle went out behind the first.
test("a double tap on Send or on a tender sends one request, and the button says it is working", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    expect(await doubleTap(page, "fireOrder", "**/api/orders/*/fire", "Sending…", "line-fired")).toBe(1);
    await page.locator('[data-step="takePayment"]').click();
    // A settle paints its pending screen at once (ADR-0196), so the second tap finds no tender.
    const settle = await holdPost(page, "**/api/bills/*/settle");
    let settles = 0;
    page.on("request", (request) => {
      if (request.method() === "POST" && /\/api\/bills\/[^/]+\/settle$/.test(request.url())) {
        settles += 1;
      }
    });
    await page.locator('[data-step="payCash"]').dblclick();
    await expect(page.locator('[data-outcome="settle-pending"]')).toContainText("Taking payment…");
    settle.release();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    expect(settles).toBe(1);
  } finally {
    await edge.stop();
  }
});

test("a double tap on any act on the till's own drawer sends one request", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");
    await page.locator("#float").fill("100000");
    expect(await doubleTap(page, "openShift", "**/api/shifts", { painted: "open-pending" }, "shift-open")).toBe(1);

    await page.locator("#movement-amount").fill("20000");
    await page.locator('[data-step="askPaidIn"]').click();
    const paidIn = "**/api/shifts/*/paid-in";
    expect(await doubleTap(page, "movementReason", paidIn, { painted: "paid-in-pending" }, "cash-moved")).toBe(1);

    await page.locator('[data-step="askOpenDrawer"]').click();
    await page.locator("#no-sale-approver-code").fill(edge.staffCode);
    await page.locator("#no-sale-approver-pin").fill(edge.staffPin);
    const noSale = "**/api/drawer/open";
    expect(await doubleTap(page, "drawerReason", noSale, { painted: "no-sale-pending" }, "drawer-opened")).toBe(1);

    await page.locator("#count").fill("120000");
    const count = "**/api/shifts/*/count";
    expect(await doubleTap(page, "countShift", count, { painted: "count-pending" }, "shift-counted")).toBe(1);
    const close = "**/api/shifts/*/close";
    expect(await doubleTap(page, "closeShift", close, { painted: "close-pending" }, "shift-closed")).toBe(1);
  } finally {
    await edge.stop();
  }
});

// Take payment moves to the pay screen at once (`docs/ui-ux.md` §3). It used to open the bill
// first and read its figure after that, so the cashier waited on two answers in a row on a screen
// that looked untouched. The bill is now opened on the pay screen, beside a read of the table's
// figure, and the tenders wait until the amount is known.
test("take payment moves at once, and reads the amount while the bill is opened", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);

    const held = await holdPost(page, "**/api/tables/*/bill");
    const checks = [];
    page.on("request", (request) => {
      if (/\/api\/tables\/[^/]+\/check$/.test(new URL(request.url()).pathname)) {
        checks.push(request.url());
      }
    });
    await page.locator('[data-step="takePayment"]').click();
    await held.sent;
    await expect(page).toHaveURL(/\/table\/[^/]+\/pay$/);
    await expect(page.locator('[data-outcome="amount-loading"]')).toBeVisible();
    await expect(page.locator('[data-step="payCash"]')).toBeDisabled();
    expect(checks).toHaveLength(1);

    held.release();
    await expect(page.locator('[data-outcome="amount-loading"]')).toHaveCount(0);
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// The answer to a send lands after the lines were painted fired, and must not make the order read
// the table's figure again: the send changed no price. The CI shard met this ordering by chance, as
// two reads of the check behind one Take payment; here it is made certain. The send's answer is
// held until the lines show fired and the read the paint caused has landed, the reads are counted
// from then, and Take payment, which waits for the send, is tapped as soon as the answer lets it.
// A send that marked its lines as sending took them out of the figure's key at the paint and put
// them back at the answer, so the answer read the check a second time.
test("a send's answer, landing after its paint, reads the table's figure no second time", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    const fire = await holdPost(page, "**/api/orders/*/fire", { reachEdge: true });
    const isCheck = (url) => /\/api\/tables\/[^/]+\/check$/.test(new URL(url).pathname);
    const painted = page.waitForResponse((response) => isCheck(response.url()));
    await page.locator('[data-step="fireOrder"]').click();
    await fire.sent;
    await expect(page.locator('[data-outcome="line-fired"]')).toHaveCount(1);
    await painted;
    const checks = [];
    page.on("request", (request) => {
      if (isCheck(request.url())) {
        checks.push(request.url());
      }
    });
    const bill = await holdPost(page, "**/api/tables/*/bill");
    fire.release();
    await page.locator('[data-step="takePayment"]').click();
    await bill.sent;
    await expect(page.locator('[data-outcome="amount-loading"]')).toBeVisible();
    expect(checks, "only the pay screen's own read of the figure").toHaveLength(1);
    bill.release();
  } finally {
    await edge.stop();
  }
});

/** Holds every GET of a check until the test calls the returned function. */
async function holdChecks(page) {
  let release = () => {};
  const released = new Promise((resolve) => {
    release = resolve;
  });
  for (const pattern of ["**/api/tables/*/check", "**/api/bills/*/check"]) {
    await page.route(pattern, async (route) => {
      await released;
      await route.continue();
    });
  }
  return release;
}

// The cash keys wait for the amount, as the tenders do. The pay screen opens before the amount is
// known (`docs/ui-ux.md` §3), and "Exact" until then is nothing: a tap on it chose 0, the tender
// enabled once the figure landed, and the settle went out for 0 and was refused with the guest's
// money on the counter.
test("the cash keys wait for the amount, and a tap before it sends nothing", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
    let settles = 0;
    page.on("request", (request) => {
      if (request.method() === "POST" && /\/api\/bills\/[^/]+\/settle$/.test(request.url())) {
        settles += 1;
      }
    });

    const release = await holdChecks(page);
    await page.locator('[data-step="takePayment"]').click();
    await expect(page.locator('[data-outcome="amount-loading"]')).toBeVisible();
    await expect(page.locator('[data-step="setTender"]').first()).toBeDisabled();
    await expect(page.locator('[data-step="typeTender"]')).toBeDisabled();
    await page.locator('[data-step="setTender"]').first().click({ force: true });
    await expect(page.locator('[data-step="payCash"]')).toBeDisabled();

    release();
    await expect(page.locator('[data-outcome="amount-loading"]')).toHaveCount(0);
    await expect(page.locator('[data-step="setTender"]').first()).toBeEnabled();
    expect(settles).toBe(0);
    await page.locator('[data-step="setTender"]').first().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    expect(settles).toBe(1);
  } finally {
    await edge.stop();
  }
});

// One failed read of the order's figure does not leave it blank for good. The order screen's check
// read is a resource, and a resource whose read threw stayed in error: every later read answered,
// and the total still read "—" until the screen was left.
test("the order's total recovers after one read of it fails", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    let failures = 1;
    await page.route("**/api/tables/*/check", async (route) => {
      if (failures > 0) {
        failures -= 1;
        await route.fulfill({ status: 500, json: { code: "INTERNAL", message: "a blip" } });
        return;
      }
      await route.continue();
    });
    await seatTable(page);
    await addItem(page);
    expect(failures).toBe(0);
    const total = page.locator('[data-outcome="check-total"] .text-2xl');
    await expect(total).toHaveText(/\d/);
  } finally {
    await edge.stop();
  }
});

test("a sold-out refusal takes the painted line back off, with its sentence and no ghost", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await refusePost(page, "**/api/tables/*/lines", "ITEM_NOT_SELLABLE");
    await page.locator('[data-step="onItem"]').first().click();
    await expect(page.getByText("That item cannot be sold right now", { exact: false })).toBeVisible();
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(0);
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveCount(0);
    await expect(page.locator("#why-pending")).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// The till greys the menu once it knows a bill is open, so the edge refuses a line only when
// another device opened one in the instant before the tap. The edge's answer is stubbed with its
// own status and token for that instant.
test("a line refused because a bill is already open is taken back off, with no ghost", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await refusePost(page, "**/api/tables/*/lines", "BILL_ALREADY_OPEN");
    await page.locator('[data-step="onItem"]').first().click();
    await expect(page.getByText("A bill is already open for this order.")).toBeVisible();
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(1);
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveCount(0);
    await expect(page.locator('[data-step="takePayment"]')).toBeEnabled();
  } finally {
    await edge.stop();
  }
});

test("a refused quantity goes back to what it was, with the refusal's sentence", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await refusePost(page, "**/api/lines/*/quantity", "ALREADY_FIRED");
    await page.locator('[data-step="setQuantity"]').first().click();
    await expect(page.getByText("Already sent to the kitchen", { exact: false })).toBeVisible();
    await expect(page.locator('[data-outcome="line-quantity"]').first()).toHaveText("1");
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

test("a refused move brings the guests back to their table, with the refusal's sentence", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    const from = new URL(page.url()).pathname;
    await refusePost(page, "**/api/tables/*/transfer", "UNKNOWN_ORDER");
    await page.locator('[data-step="openMove"]').click();
    await page.locator('[data-step="moveTo"]').first().click();
    await page.waitForURL((url) => url.pathname === from);
    await expect(page.locator('[data-outcome="order-open"]')).toHaveText("Table 1");
    await expect(page.getByText("That item, order or bill has changed on another device", { exact: false })).toBeVisible();
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

test("the edge's event arriving before its answer confirms the line once, never twice", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    // The event can confirm the line only on a link that is open when the table is seated and the
    // line added: the link retries on a back-off after its refusal before pairing, and what the
    // edge announces while it is down is never carried.
    await expect(page.getByText("Store connected")).toBeVisible();
    await seatTable(page);
    // The edge makes the line and the live link carries its event at once; the answer is held
    // until the event has confirmed the line.
    const held = await holdPost(page, "**/api/tables/*/lines", { reachEdge: true });
    await page.locator('[data-step="onItem"]').first().click();
    await held.sent;
    // Confirmed by the event, with the edge's figure, while the answer is still held.
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(1);
    await expect(page.locator('[data-outcome="line-sending"]')).toHaveCount(0);
    held.release();
    // And once the answer lands, still the one line.
    await expect(page.locator('[data-step="takePayment"]')).toBeEnabled();
    await expect(page.locator('[data-outcome="line-added"]')).toHaveCount(1);
  } finally {
    await edge.stop();
  }
});

// The store server says whether other accounts on its computer can read the store's data
// (ADR-0174), and the setup screen warns where they can. The example store has no cloud, so the
// standing it would answer is stubbed with each value.
test("the setup screen warns when other accounts on the computer can read the store's data", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    let access = "STATE_ACCESS_TOO_OPEN";
    await page.route("**/api/activation", (route) =>
      route.fulfill({ json: { activated: false, state_access: access } }),
    );
    await page.goto(`${edge.baseURL}/setup`);
    const warning = page.locator('[data-outcome="state-too-open"]');
    await expect(warning).toHaveText(
      "Other accounts on this computer can read this store's data. Run the store server's installer again on this computer, as an administrator, to close it.",
    );
    await expect(page.locator("input").first()).toBeVisible();

    access = "STATE_ACCESS_OK";
    await page.reload();
    await expect(page.locator("input").first()).toBeVisible();
    await expect(warning).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

test("every flow is replayed except the ones that say why they cannot be", () => {
  expect(skipped.map((declared) => declared.task).sort()).toEqual(
    [
      "Close another till's drawer over or short, giving a reason",
      "Close the shift over or short, giving a reason",
      "Close two counted drawers together",
      "Count another till's drawer, blind",
      "Open the cash drawer without a sale",
      "Settle a dine-in table in cash, typing the amount handed over",
      "Start another till's drawer",
      "Start two tills' drawers at once",
      "Take money off a bill",
      "Void a bill before it settles",
      "Void a line the kitchen has already been given",
      "Void a paid bill from today's bills",
      "Waive a fee on a bill",
    ].sort(),
  );
  for (const declared of skipped) {
    expect(declared.unreplayable, `"${declared.task}" must say why it cannot be replayed`).toMatch(
      /\S/,
    );
  }
  expect(replayed.length + skipped.length).toBe(TASKS.length);
});

// Every route the till declares opens its own screen, and a path it does not declare opens none.
// Each screen is recognised by something only it draws on a fresh store, so a route pointed at the
// wrong screen fails here as surely as a route that went missing.
const SCREEN_TITLES = JSON.parse(
  readFileSync(new URL("../src/i18n/en.json", import.meta.url), "utf8"),
);
const ROUTED_SCREENS = [
  { path: "/", shows: '[data-outcome="floor"]' },
  {
    path: "/table/t1",
    shows: '[data-outcome="order-open"] >> nth=0',
    also: '[data-step="releaseTable"]',
  },
  { path: "/table/t1/pay", shows: '[data-step="splitEvenly"] >> nth=0' },
  { path: "/kds", shows: '[data-outcome="board-clear"]' },
  { path: "/expo", shows: '[data-outcome="pass-clear"]' },
  { path: "/counter", shows: '[data-outcome="counter"]' },
  {
    path: "/order/o1",
    shows: '[data-outcome="order-open"] >> nth=0',
    also: '[data-step="takePayment"]',
  },
  { path: "/guests", title: "confirm.title" },
  { path: "/today", shows: '[data-outcome="today-takings"]' },
  { path: "/shift", shows: '[data-step="openShift"]' },
  { path: "/devices", shows: '[data-outcome="till"]' },
  { path: "/setup", title: "setup.title" },
];

test("every route the till declares opens its own screen, and an undeclared one opens none", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await page.goto(`${edge.baseURL}/pair`);
    await expect(page.locator("#pair-submit")).toBeVisible();
    await pair(page, edge);
    await expect(page.locator('[data-step="submit"]')).toBeVisible();
    await signIn(page, edge);
    for (const screen of ROUTED_SCREENS) {
      await page.goto(`${edge.baseURL}${screen.path}`);
      await expect(page, `${screen.path} stays where it is`).toHaveURL(
        (url) => url.pathname === screen.path,
      );
      const main = page.locator("main");
      if (screen.title !== undefined) {
        await expect(main.locator("h1"), screen.path).toHaveText(SCREEN_TITLES[screen.title]);
      } else {
        await expect(main.locator(screen.shows), screen.path).toBeVisible();
      }
      if (screen.also !== undefined) {
        await expect(main.locator(screen.also), screen.path).toBeVisible();
      }
    }
    await page.goto(`${edge.baseURL}/no-such-screen`);
    await expect(page.locator("main")).toBeEmpty();
  } finally {
    await edge.stop();
  }
});

// Paint first for money (ADR-0196, ADR-0204). A settle shows its pending screen at once, with the
// amount the edge confirmed and no change, no receipt and no way to pay again; the edge decides.
test("a settle shows its pending screen at once, with no change until the edge commits", async ({ page }) => {
  const edge = await startEdge();
  try {
    // One note larger than the sale, so the settle owes change.
    await page.route("**/api/locale", async (route) => {
      const response = await fetchWhole(route);
      const body = await response.json();
      await route.fulfill({ response, json: { ...body, cash_denominations: [500_000] } });
    });
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    const settle = await holdPost(page, "**/api/bills/*/settle", { reachEdge: true });
    await page.locator('[data-step="setTender"]').nth(1).click();
    await page.locator('[data-step="payCash"]').click();
    const pending = page.locator('[data-outcome="settle-pending"]');
    await expect(pending).toBeVisible();
    await expect(page.locator('[data-outcome="change-due"]')).toHaveCount(0);
    await expect(page.locator('[data-step="printAgain"]')).toHaveCount(0);
    await expect(page.locator('[data-step="payCash"]')).toHaveCount(0);
    await settle.sent;
    settle.release();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.locator('[data-outcome="change-due"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// Safeguard 3: a settle the edge refuses rolls the screen back to the pad, says why in place, and
// takes no tap on a tender until the cashier dismisses it. The tender chosen is still chosen.
test("a refused settle rolls back to the pad with the refusal, which must be dismissed", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await refusePost(page, "**/api/bills/*/settle", "PAYMENTS_DO_NOT_SUM_TO_TOTAL");
    await page.locator('[data-step="setTender"]').last().click();
    await page.locator('[data-step="payCash"]').click();
    const refusal = page.locator('[data-outcome="money-refused"]');
    await expect(refusal).toBeVisible();
    await expect(page.getByRole("alertdialog")).toBeVisible();
    await expect(page.locator('[data-step="payCash"]')).toBeDisabled();
    await expect(page.locator('[data-outcome="settled"]')).toHaveCount(0);
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(refusal).toHaveCount(0);
    await expect(page.locator('[data-step="payCash"]')).toBeEnabled();
  } finally {
    await edge.stop();
  }
});

// ADR-0204: a settle the edge committed but whose answer never reached the till is not rolled
// back. The till checks with the store server and shows the bill settled.
test("a settle whose answer is lost is checked, not rolled back, and shows settled", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await page.route("**/api/bills/*/settle", async (route) => {
      if (route.request().method() !== "POST") {
        await route.continue();
        return;
      }
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="setTender"]').last().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settle-pending"]')).toContainText("Checking with the store server");
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for the drawer's own shift: a count whose answer is lost is read again, and the till
// shows the count the edge recorded, never the one it typed. The edge here records 90,000₫ for a
// count typed as 100,000₫, as when another device's count reached it first. The close's answer is
// lost too, and the closed card shows the figures the close's event brought: the edge's.
test("a count and a close whose answers are lost show the edge's count, not the till's", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await page.route("**/api/shifts/*/count", async (route) => {
      const body = { ...route.request().postDataJSON(), counted_minor: 90000 };
      await route.fetch({ postData: JSON.stringify(body) });
      await route.abort("connectionreset");
    });
    await page.locator("#count").fill("100000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="count-pending"]')).toContainText("Checking with the store server");
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();

    await page.route("**/api/shifts/*/close", async (route) => {
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="closeShift"]').click();
    const closed = page.locator('[data-outcome="shift-closed"]');
    await expect(closed).toContainText("Counted 90,000₫");
    await expect(closed).toContainText("-10,000₫");
    await expect(closed).not.toContainText("Counted 100,000₫");
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204: a close that never reached the edge is found not made, and the shift stays counted.
test("a close that never reached the store server is found not made, and the shift stays counted", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await page.locator("#count").fill("100000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await page.route("**/api/shifts/*/close", (route) => route.abort("connectionreset"));
    await page.locator('[data-step="closeShift"]').click();
    await expect(page.locator('[data-outcome="close-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("The shift was not closed");
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await expect(page.locator('[data-outcome="shift-closed"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for an opening: committed with its answer lost, the till finds the shift open.
test("an opening whose answer is lost is checked, and the shift shows open", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await navigateTo(page, "/shift");
    await page.route("**/api/shifts", async (route) => {
      if (route.request().method() !== "POST") {
        await route.continue();
        return;
      }
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator("#float").fill("100000");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="open-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// SHIFT_ALREADY_OPEN at the edge (ADR-0196 safeguard 3), from the real edge: another screen opens
// the drawer's shift while this till's link is held, so this till still offers **Open shift**, and
// the edge refuses it with its own sentence. The till's own check, which refuses an open while it
// holds a shift, is `unit/shift-acts.test.ts`'s: the screen hides **Open shift** once it does.
test("a shift opened elsewhere first is refused by the store server, saying why", async ({ context }) => {
  const edge = await startEdge();
  try {
    const till = await context.newPage();
    let release = () => {};
    const held = new Promise((resolve) => {
      release = resolve;
    });
    await pair(till, edge);
    await signIn(till, edge);
    await navigateTo(till, "/shift");
    await expect(till.locator('[data-step="openShift"]')).toBeVisible();
    await till.routeWebSocket(
      (url) => url.pathname === "/ws",
      async (socket) => {
        await held;
        socket.connectToServer();
      },
    );
    await till.reload();
    await expect(till.locator('[data-step="openShift"]')).toBeVisible();

    const other = await context.newPage();
    await other.goto(`${edge.baseURL}/`);
    await openShift(other);

    await till.locator("#float").fill("100000");
    await till.locator('[data-step="openShift"]').click();
    await expect(till.locator('[data-outcome="money-refused"]')).toContainText(
      "A shift is already open. Count and close it first.",
    );
    release();
  } finally {
    await edge.stop();
  }
});

// COUNT_DOES_NOT_ADD_UP at the edge. The real edge cannot be made to refuse it from this screen:
// the count field always sends notes that come to the total. So the edge's refusal is substituted
// here, to replay how the till shows it; the till's own check is `unit/shift-acts.test.ts`'s.
test("a count the store server refuses as not adding up says why and stays open", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await refusePost(page, "**/api/shifts/*/count", "COUNT_DOES_NOT_ADD_UP");
    await page.locator("#count").fill("100000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText(
      "The notes counted do not add up to the amount.",
    );
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for cash out of the drawer: a paid out whose answer is lost is read again, and counts as
// made because the shift's paid-out total grew by its amount since just before it was sent.
test("a paid out whose answer is lost is checked by the shift's total, and shows recorded", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await page.route("**/api/shifts/*/paid-out", async (route) => {
      await route.fetch();
      await route.abort("connectionreset");
    });
    await page.locator("#movement-amount").fill("20000");
    await page.locator('[data-step="askPaidOut"]').click();
    await page.locator('[data-step="movementReason"]').first().click();
    await expect(page.locator('[data-outcome="paid-out-pending"]')).toContainText("Checking with the store server");
    await expect(page.locator('[data-outcome="cash-moved"]')).toHaveText("Paid out 20,000₫.");
    await expect(page.locator('[data-outcome="cash-totals"]')).toContainText("20,000₫");
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204: a drop to the safe that never reached the edge is found not made, and the pile stays as
// it was typed for the cashier to send again or cancel.
test("a drop that never reached the store server is found not made, and the pile stays typed", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await page.route("**/api/shifts/*/safe-drop", (route) => route.abort("connectionreset"));
    await page.getByRole("button", { name: "Drop to safe" }).click();
    await page.locator("#drop-amount").fill("50000");
    await page.locator('[data-step="dropToSafe"]').click();
    await expect(page.locator('[data-outcome="safe-drop-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("The cash was not recorded");
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(page.locator("#drop-amount")).toHaveValue("50000");
    await expect(page.locator('[data-outcome="cash-dropped"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 decision 5: the drawer opened without a sale changes nothing a read can show. With no
// answer it is neither rolled back nor sent again: the till says it is unconfirmed, and the cashier,
// who can see the drawer, dismisses it.
test("a drawer opening with no answer is said unconfirmed, never sent again", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    let opens = 0;
    await page.route("**/api/drawer/open", async (route) => {
      opens += 1;
      await route.abort("connectionreset");
    });
    await page.locator('[data-step="askOpenDrawer"]').click();
    await page.locator("#no-sale-approver-code").fill(edge.staffCode);
    await page.locator("#no-sale-approver-pin").fill(edge.staffPin);
    await page.locator('[data-step="drawerReason"]').first().click();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText(
      "The drawer opening is not confirmed: check the drawer before opening it again.",
    );
    await page.locator('[data-step="dismissRefusal"]').click();
    expect(opens).toBe(1);
    await expect(page.locator('[data-outcome="drawer-opened"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// DRAWER_COUNTED for a paid bill's void, at the edge only: the cash would go back out of a drawer
// already counted, whose count is a snapshot. The till has no check of its own for it yet, because
// the action catalogue does not list this refusal for `billing.bill.void_settled` (a pos-proto
// change, waiting for the ADR-0191 follow-up).
test("a paid bill's void out of a counted drawer is refused by the store server, saying why", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await navigateTo(page, "/");
    await aSettledTable(page);
    await navigateTo(page, "/shift");
    await page.locator("#count").fill("100000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();

    await navigateTo(page, "/today");
    await page.locator('[data-step="chooseBill"]').click();
    await page.locator('[data-step="askVoidPaidBill"]').click();
    await page.locator("#void-paid-bill-approver-code").fill(edge.staffCode);
    await page.locator("#void-paid-bill-approver-pin").fill(edge.staffPin);
    const answered = page.waitForResponse(
      (response) => response.url().endsWith("/void_settled") && response.request().method() === "POST",
    );
    await page.locator('[data-step="voidPaidBillReason"]').first().click();
    const answer = await answered;
    expect(answer.status()).toBe(409);
    expect(answer.headers()["pos-error-reason"]).toBe("DRAWER_COUNTED");
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("This drawer is being closed.");
    await expect(page.locator('[data-outcome="paid-bill-voided"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204: a settle that never reached the edge is found not taken, and only then rolled back.
test("a settle that never reached the store server is found not taken, and the pad comes back", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await page.route("**/api/bills/*/settle", (route) =>
      route.request().method() === "POST" ? route.abort("connectionreset") : route.continue(),
    );
    await page.locator('[data-step="setTender"]').last().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settle-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("The payment was not taken");
    await expect(page.locator('[data-outcome="settled"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// Safeguard 2, replayed both ways (ADR-0196): cash into a drawer already counted is refused by the
// till before anything is sent, and the edge refuses the same settle with the same token.
test("cash into a counted drawer is refused before sending, and the edge refuses it the same way", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await openShift(page);
    await navigateTo(page, "/");
    await seatTable(page);
    await addItem(page);
    await sendOrder(page);
    await navigateTo(page, "/shift");
    await page.locator("#count").fill("100000");
    await page.locator('[data-step="countShift"]').click();
    await expect(page.locator('[data-outcome="shift-counted"]')).toBeVisible();
    await page.goBack();
    let billId = null;
    page.on("request", (request) => {
      const read = /\/api\/bills\/([^/]+)\/check$/.exec(new URL(request.url()).pathname);
      if (read !== null) {
        billId = read[1];
      }
    });
    await page.locator('[data-step="takePayment"]').click();
    let settles = 0;
    page.on("request", (request) => {
      if (request.method() === "POST" && /\/api\/bills\/[^/]+\/settle$/.test(request.url())) {
        settles += 1;
      }
    });
    await page.locator('[data-step="setTender"]').last().click();
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="money-refused"]')).toContainText("This drawer is being closed");
    await expect(page.locator('[data-outcome="settle-pending"]')).toHaveCount(0);
    expect(settles).toBe(0);

    // The same settle, straight at the edge: refused with the token the till predicted.
    await expect.poll(() => billId).not.toBeNull();
    const answer = await page.evaluate(async (id) => {
      const headers = {
        authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}`,
        "content-type": "application/json",
      };
      const check = await fetch(`/api/bills/${id}/check`, { headers }).then((response) => response.json());
      const due = check.total_due;
      const response = await fetch(`/api/bills/${id}/settle`, {
        method: "POST",
        headers,
        body: JSON.stringify({
          payments: [{ method: "PAYMENT_METHOD_CASH", tendered: due, applied_to_bill: due }],
        }),
      });
      return { status: response.status, reason: response.headers.get("pos-error-reason") };
    }, billId);
    expect(answer).toEqual({ status: 409, reason: "DRAWER_COUNTED" });
  } finally {
    await edge.stop();
  }
});

/** Lets `pattern`'s POST reach the edge, then drops its answer: committed there, unheard here. */
async function loseAnswer(page, pattern) {
  await page.route(pattern, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    await route.fetch();
    await route.abort("connectionreset");
  });
}

/** Opens the pay screen's void panel with the manager's badge and PIN typed. */
async function openVoidPanel(page, edge) {
  await page.locator('[data-step="askVoidBill"]').click();
  await page.locator("#void-bill-approver-code").fill(edge.staffCode);
  await page.locator("#void-bill-approver-pin").fill(edge.staffPin);
}

// ADR-0196 safeguard 3 for a bill's void: the edge's refusal keeps the panel and the manager's
// fields, says why in place, and takes no reason until it is dismissed.
test("a refused void keeps the panel and its refusal must be dismissed", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await refusePost(page, "**/api/bills/*/void", "VOID_REASON_NOT_VALID");
    await openVoidPanel(page, edge);
    await page.locator('[data-step="voidBillReason"]').first().click();
    await expect(page.locator('[data-outcome="money-refused"]')).toBeVisible();
    await expect(page.locator('[data-outcome="bill-voided"]')).toHaveCount(0);
    await expect(page.locator("#void-bill-approver-code")).toHaveValue(edge.staffCode);
    await page.locator('[data-step="dismissRefusal"]').click();
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for a bill's void: committed at the edge with its answer lost, it is checked and shown
// voided, never rolled back to an open bill.
test("a void whose answer is lost is checked, and shows the bill voided", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await loseAnswer(page, "**/api/bills/*/void");
    await openVoidPanel(page, edge);
    await page.locator('[data-step="voidBillReason"]').first().click();
    await expect(page.locator('[data-outcome="void-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="bill-voided"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for a paid bill's void: found voided by reading the day's bills again, it says so and
// names no sum to hand back, since the edge's figures never arrived.
test("a paid bill's void whose answer is lost is found voided, with no made-up figures", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await aSettledTable(page);
    await navigateTo(page, "/today");
    await page.locator('[data-step="chooseBill"]').click();
    await loseAnswer(page, "**/api/bills/*/void_settled");
    await page.locator('[data-step="askVoidPaidBill"]').click();
    await page.locator("#void-paid-bill-approver-code").fill(edge.staffCode);
    await page.locator("#void-paid-bill-approver-pin").fill(edge.staffPin);
    await page.locator('[data-step="voidPaidBillReason"]').first().click();
    const outcome = page.locator('[data-outcome="paid-bill-voided"]');
    await expect(outcome).toContainText("could not be read on this till");
    await expect(outcome).not.toContainText("in cash");
  } finally {
    await edge.stop();
  }
});

/** Opens the pay screen's discount panel with `amount` typed and a manager where one is asked. */
async function openDiscount(page, edge, amount) {
  await page.locator('[data-step="askDiscount"]').click();
  await page.locator("#discount-amount").fill(String(amount));
  const code = page.locator("#discount-approver-code");
  if ((await code.count()) > 0) {
    await code.fill(edge.staffCode);
    await page.locator("#discount-approver-pin").fill(edge.staffPin);
  }
}

/** Sends `body` to the edge as this device, and answers with the status and the refusal token. */
async function postAsTill(page, path, body) {
  return page.evaluate(
    async ([to, payload]) => {
      const response = await fetch(to, {
        method: "POST",
        headers: {
          authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}`,
          "content-type": "application/json",
        },
        body: JSON.stringify(payload),
      });
      return { status: response.status, reason: response.headers.get("pos-error-reason") };
    },
    [path, body],
  );
}

/** Records the id of the bill the pay screen reads. */
function watchBillId(page) {
  const seen = { id: null };
  page.on("request", (request) => {
    const read = /\/api\/bills\/([^/]+)\/check$/.exec(new URL(request.url()).pathname);
    if (read !== null) seen.id = read[1];
  });
  return seen;
}

// Safeguard 2 both ways (ADR-0196): a discount larger than the bill is refused by the till before
// anything is sent, and the edge refuses the same discount with the same token.
test("a discount larger than the bill is refused before sending, and the edge refuses it the same way", async ({
  page,
}) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    const bill = watchBillId(page);
    await sendThenTakePayment(page);
    let discounts = 0;
    page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/discount")) discounts += 1;
    });
    await openDiscount(page, edge, 99_000_000);
    await page.locator('[data-step="discountReason"]').first().click();
    await expect(page.locator('[data-outcome="money-refused"]')).toBeVisible();
    expect(discounts).toBe(0);

    await expect.poll(() => bill.id).not.toBeNull();
    const reasons = await page.evaluate(() =>
      fetch("/api/reason-codes", {
        headers: { authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}` },
      }).then((response) => response.json()),
    );
    const reason = reasons.reasons.find((entry) =>
      entry.applies_to.includes("REASON_ACTION_DISCOUNT"),
    );
    const answer = await postAsTill(page, `/api/bills/${bill.id}/discount`, {
      amount: { currency_code: "VND", amount_minor: 99_000_000 },
      reason_code_id: reason.reason_code_id,
      approver_code: edge.staffCode,
      approver_pin: edge.staffPin,
    });
    expect(answer).toEqual({ status: 409, reason: "REDUCTION_EXCEEDS_BILL" });
  } finally {
    await edge.stop();
  }
});

// ADR-0204 for a discount: committed at the edge with its answer lost, it is found by reading the
// bill again and shown, never rolled back.
test("a discount whose answer is lost is checked, and shows on the bill", async ({ page }) => {
  const edge = await startEdge();
  try {
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    await addItem(page);
    await sendThenTakePayment(page);
    await loseAnswer(page, "**/api/bills/*/discount");
    await openDiscount(page, edge, 10_000);
    await page.locator('[data-step="discountReason"]').first().click();
    await expect(page.locator('[data-outcome="discount-pending"]')).toBeVisible();
    await expect(page.locator('[data-outcome="bill-discounted"]')).toBeVisible();
    await expect(page.locator('[data-outcome="money-refused"]')).toHaveCount(0);
  } finally {
    await edge.stop();
  }
});

// APPROVAL_REQUIRED both ways (ADR-0196 safeguard 2, ADR-0158 decision 4), on a store that asks a
// manager for voids: the till takes no reason until a manager's badge and PIN are typed, the edge
// refuses the same void without them, and with the manager's it goes through.
test("a void the store asks a manager for waits for one at the till, and the edge refuses it without", async ({
  page,
}) => {
  const edge = await startEdge("approvals");
  try {
    expect(edge.managerCode).toBeTruthy();
    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addItem(page);
    const bill = watchBillId(page);
    await sendThenTakePayment(page);
    await page.locator('[data-step="askVoidBill"]').click();
    const reason = page.locator('[data-step="voidBillReason"]').first();
    await expect(page.locator("#void-bill-approver-code")).toBeVisible();
    await expect(reason).toBeDisabled();

    await expect.poll(() => bill.id).not.toBeNull();
    const reasons = await page.evaluate(() =>
      fetch("/api/reason-codes", {
        headers: { authorization: `Bearer ${localStorage.getItem("pos-edge.device-token")}` },
      }).then((response) => response.json()),
    );
    const voidReason = reasons.reasons.find((entry) =>
      entry.applies_to.includes("REASON_ACTION_VOID_BILL"),
    );
    const answer = await postAsTill(page, `/api/bills/${bill.id}/void`, {
      reason_code_id: voidReason.reason_code_id,
    });
    expect(answer).toEqual({ status: 403, reason: "APPROVAL_REQUIRED" });

    await page.locator("#void-bill-approver-code").fill(edge.managerCode);
    await page.locator("#void-bill-approver-pin").fill(edge.managerPin);
    await reason.click();
    await expect(page.locator('[data-outcome="bill-voided"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});
