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

/** Adds the demo store's starter (the salad), so a course has something waiting. */
async function addStarter(page) {
  await page.locator("#menu-search").fill("salad");
  await expect(page.locator('[data-step="onItem"]')).toHaveCount(1);
  await page.locator('[data-step="onItem"]').click();
  await expect(page.locator('[data-outcome="line-added"]').first()).toBeVisible();
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
  "Change how many of a line": async (page) => {
    await seatTable(page);
    await addItem(page);
  },
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
    await expect(page.getByText("98,000₫", { exact: true })).toBeVisible();

    const keys = page.locator('[data-step="setTip"]');
    await expect(keys).toHaveText(["5,000₫", "10,000₫", "15,000₫"]);

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
// So the till shows the exact shares instead: 500₫, 1,000₫ and 1,500₫. Less tidy, and the only set
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
    await expect(page.getByText("10,000₫", { exact: true })).toBeVisible();

    await expect(page.locator('[data-step="setTip"]')).toHaveText(["500₫", "1,000₫", "1,500₫"]);
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
test("every flow is replayed except the ones that say why they cannot be", () => {
  expect(skipped.map((declared) => declared.task).sort()).toEqual(
    [
      "Charge a counter (takeaway) order in cash",
      "Charge a counter order by card",
      "Charge a counter order in cash, taking a tip",
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
