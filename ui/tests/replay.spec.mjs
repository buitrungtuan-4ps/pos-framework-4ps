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
//   not have. Two void tasks cannot run for a different reason: the manager's badge and PIN are
//   typed into fields that appear mid-flow, and this harness types only in a precondition — before
//   the first tap. All five carry `unreplayable` in the declaration, and the last test in this file
//   asserts the skipped set is exactly that set — so coverage cannot quietly shrink by one flow at
//   a time.

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
  await page.locator('[data-step="onItem"]').first().click();
  await expect(page.locator('[data-outcome="line-added"]').first()).toBeVisible();
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
  "Order an item for a particular seat": seatTable,
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
          if (root.scrollWidth <= root.clientWidth + 1) {
            return null;
          }
          // Name the widest thing that sticks out, so the failure says what to fix rather than
          // that something, somewhere, is too wide.
          let worst = null;
          for (const element of document.querySelectorAll("*")) {
            const box = element.getBoundingClientRect();
            if (box.width > 0 && box.right > root.clientWidth + 1 && (worst === null || box.right > worst.right)) {
              worst = { right: Math.round(box.right), tag: element.tagName.toLowerCase(), classes: String(element.className).slice(0, 70) };
            }
          }
          return { page: root.scrollWidth, viewport: root.clientWidth, worst };
        });
        expect(
          overflow,
          `${path} scrolls sideways on a ${device.name}: ${JSON.stringify(overflow)}`,
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
// declaration, where the reason is read by anyone looking at the map — silently dropping out of the
// browser gate is how coverage rots.
test("every flow is replayed except the ones that say why they cannot be", () => {
  expect(skipped.map((declared) => declared.task).sort()).toEqual(
    [
      "Charge a counter (takeaway) order in cash",
      "Charge a counter order by card",
      "Charge a counter order in cash, taking a tip",
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
