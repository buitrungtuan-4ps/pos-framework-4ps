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

/** Starts a walk-in at the counter and lands on its order screen (ADR-0146). */
async function startWalkIn(page) {
  await navigateTo(page, "/counter");
  await page.locator('[data-step="newOrder"]').click();
  await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
}

/** A walk-in with one line, back on the counter list where it waits to be charged. */
async function aWalkInToCharge(page) {
  await startWalkIn(page);
  await addItem(page);
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
  await page.locator('[data-step="takePayment"]').click();
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
  },
  "Settle a dine-in table in cash, taking a tip": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Settle a dine-in table by card": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Settle a dine-in table by QR transfer": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  "Split a dine-in bill evenly between two guests, each paying by QR": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
  // Two different things, so there are two lines to split between two guests.
  "Split one guest's items off a dine-in bill, then settle each part by QR": async (page) => {
    await seatTable(page);
    await addByName(page, "salad");
    await addByName(page, "iced");
  },
  // One thing for seat 1 and one for seat 2, so there are two seats to split between.
  "Split a dine-in bill by seat, then settle each seat's part by QR": async (page) => {
    await seatTable(page);
    await page.locator('[data-step="chooseSeat"]').nth(0).click();
    await addByName(page, "salad");
    await page.locator('[data-step="chooseSeat"]').nth(1).click();
    await addByName(page, "iced");
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
    await page.route("**/api/session/sign-in", async (route) => {
      await new Promise((resolve) => setTimeout(resolve, 600));
      await route.continue();
    });

    await page.locator("#signin-code").fill(edge.staffCode);
    await page.locator("#signin-pin").fill("999999");
    await page.locator('[data-step="submit"]').click();

    // The operator does not wait: the button is disabled while the request is out, the field is
    // not, and they have already started the next attempt.
    await page.locator("#signin-pin").fill("4321");

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

    await page.locator('[data-step="takePayment"]').click();
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

    await page.locator('[data-step="takePayment"]').click();
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

    await page.locator('[data-step="takePayment"]').click();
    await expect(page.getByText("10.000₫", { exact: true })).toBeVisible();

    await expect(page.locator('[data-step="setTip"]')).toHaveText(["500₫", "1.000₫", "1.500₫"]);
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
      const response = await route.fetch();
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

    await page.locator('[data-step="takePayment"]').click();
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
      const response = await route.fetch();
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

    await page.locator('[data-step="takePayment"]').click();
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
    await page.locator('[data-step="takePayment"]').click();
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
    await page.locator('[data-step="takePayment"]').click();
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
    await page.locator('[data-step="takePayment"]').click();
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

    await page.locator('[data-step="takePayment"]').click();
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
    await page.locator('[data-step="takePayment"]').click();
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
      const response = await route.fetch();
      const body = await response.json();
      await route.fulfill({ response, json: { ...body, currency_exponent: 2 } });
    });

    await pair(page, edge);
    await signIn(page, edge);
    await seatTable(page);
    await addStarter(page);
    await page.locator('[data-step="takePayment"]').click();

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

test("every flow is replayed except the ones that say why they cannot be", () => {
  expect(skipped.map((declared) => declared.task).sort()).toEqual(
    [
      "Settle a dine-in table in cash, typing the amount handed over",
      "Take money off a bill",
      "Void a bill before it settles",
      "Void a line the kitchen has already been given",
    ].sort(),
  );
  for (const declared of skipped) {
    expect(declared.unreplayable, `"${declared.task}" must say why it cannot be replayed`).toMatch(
      /\S/,
    );
  }
  expect(replayed.length + skipped.length).toBe(TASKS.length);
});
