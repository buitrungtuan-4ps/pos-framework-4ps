// The browser half of the console's step gate: walk each declared flow in a real browser, against a
// real cloud.
//
// [ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md) decided **replay, not
// search**: this does not look for the cheapest path through the console. It clicks exactly the
// steps `scripts/step-tasks.mjs` names, in order, and asserts the flow reaches its stated outcome.
//
// That shape is what catches the hole the static gate cannot see. Insert a required picker into the
// cohort publish and the declared four clicks no longer end in a report — the fourth lands on a
// screen that refuses — so this goes red **with the declaration untouched**.
//
// # The rules it follows
//
// * **A step's element is `[data-step="<action>"]`, first match.** The static gate has already
//   proved that element calls the action it names, so "first" is a rule rather than a discovery.
// * **A picker is one step.** The budget counts choosing in a combobox as a single click, so the
//   harness presses the trigger and takes the first option rather than counting two. Picking by
//   position keeps the harness from encoding a second opinion about what the operator would choose.
// * **A field is typed into, not clicked.** `setChannelAmount` sits on a money input; the operator's
//   act there is entering a number, so the harness fills it.
// * **A nav or link step is `[data-nav="<screen>"]`.** The sidebar entry and the one in-app link a
//   flow follows carry the same attribute, so both kinds of step click the same way.
// * **A closed nav group is opened first, and that is a finding rather than a fixture.** The sidebar
//   is an accordion: one group open at a time, the rest `hidden`. So an operator sitting on the
//   store overview reaches Stores, Catalog or Config in *two* clicks — open the group, then the
//   entry — while the declaration counts one. The harness opens the group so the flow can run, and
//   this note is the record of what it had to do. Whether D7's ceilings should count that click, or
//   the nav should stop hiding entries, is a decision for `docs/cloud-admin-ux-plan.md`, not for
//   this file to make quietly.
// * **A precondition is not a click.** Signing in, choosing the working context, and typing a name
//   into the wizard's first field are how the harness reaches the starting line — the budget counts
//   clicks from the screen with the flow already available. They live in `PRECONDITIONS`, not in the
//   declaration.
// * **A skipped flow says why, and the set is checked.** The last test asserts the skipped set is
//   exactly the declared one, so coverage cannot shrink one quiet flow at a time.

import { expect, test } from "@playwright/test";

import { TASKS } from "../scripts/step-tasks.mjs";
import { seedFixtures, startCloud, stopCloud, totp } from "./cloud.mjs";

/** The cloud, the signed-in browser state and the fixtures every test shares. */
let cloud;
let fixtures;
let session;

test.beforeAll(async ({ browser }) => {
  cloud = await startCloud();

  // The run's one sign-in, through the login screen, because a TOTP code is spent when it is
  // accepted (ADR-0034) — a second sign-in inside the same thirty seconds is refused, and rightly.
  // The session it produces is what every test reuses and what writes the fixtures.
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`${cloud.baseURL}/login`);
  await page.locator('input[autocomplete="current-password"]').fill(cloud.password);
  await page.locator('input[autocomplete="one-time-code"]').fill(totp(cloud.secret));
  await page.getByRole("button", { name: /sign in/i }).click();
  // Attached, not visible: the sidebar is an accordion and most entries start inside a closed
  // group. What this asserts is that the shell rendered at all, which is what a refused sign-in
  // would not produce.
  await expect(
    page.locator('[data-nav="stores"]'),
    "the console never came up after signing in — the login screen refused, or the shell did not render",
  ).toBeAttached();

  session = await context.storageState();
  const [cookie] = await context.cookies();
  cloud.useSession(`${cookie.name}=${cookie.value}`);
  await context.close();

  fixtures = await seedFixtures(cloud);
});

test.afterAll(async () => {
  await stopCloud();
});

/** A browser already signed in, opened on the seeded working context (ADR-0120 carries it in the URL). */
async function openConsole(browser) {
  const context = await browser.newContext({ storageState: session });
  const page = await context.newPage();
  await page.goto(
    `${cloud.baseURL}/t/${fixtures.tenant.tenant_id}?store=${fixtures.store.store_id}`,
  );
  await expect(page.locator('[data-nav="stores"]')).toBeAttached();
  return { context, page };
}

/**
 * Opens the sidebar group holding `screen`, when that group is closed.
 *
 * See the note at the top: this is the harness paying a click the declaration does not count, so the
 * flow can proceed. It is deliberately not silent in the record.
 */
async function openNavGroupFor(page, screen) {
  const entry = page.locator(`[data-nav="${screen}"]`).first();
  if (await entry.isVisible()) {
    return;
  }
  // The nearest ancestor that holds a list: the shell wraps each group as a container with the
  // toggle and the `<ul>` of entries side by side, so this is the group whatever element it is.
  const group = entry.locator("xpath=ancestor::*[ul][1]");
  await group.locator("button[aria-expanded]").first().click();
  await expect(entry).toBeVisible();
}

/**
 * Opens the first row's action menu, when the step's control lives behind one.
 *
 * The same class of finding as the nav accordion, and the same posture: a row with several verbs
 * keeps them behind a kebab (V19), so reaching one of them costs a click the declaration does not
 * count. The harness pays it and says so here rather than leaving the flow unrunnable.
 */
async function openRowMenu(page) {
  // The row's menu, not the screen's: a list screen also carries kebabs on its panels (the
  // organisation card has one), and the flows here act on a row.
  const closed = 'button[aria-haspopup="true"][aria-expanded="false"]';
  const inARow = page.locator(`tbody ${closed}`).first();
  const kebab = (await inARow.count()) > 0 ? inARow : page.locator(closed).first();
  if ((await kebab.count()) === 0) {
    return;
  }
  await kebab.click();
}

/** How long a screen has to render its own controls before the harness looks behind a row menu. */
const SETTLE_MS = 3_000;

/** Clicks the step's element, in the way that element is operated. */
async function press(page, action, description, value) {
  // `value` names which of several controls sharing an action (the catalog tabs); without one the
  // first match is the rule, because the controls are interchangeable.
  const selector =
    value === undefined ? `[data-step="${action}"]` : `[data-step="${action}"][data-step-value="${value}"]`;
  const target = page.locator(selector).first();
  // A lazily-routed screen paints a moment after the click that navigated to it, so wait for the
  // control before concluding it is hidden behind something.
  await target.waitFor({ state: "visible", timeout: SETTLE_MS }).catch(() => openRowMenu(page));
  await expect(
    target,
    `${description} clicks \`${action}\`${value === undefined ? "" : ` (${value})`}, and nothing on screen offers it — the flow grew a step, or the declaration is stale`,
  ).toBeVisible();
  const tag = await target.evaluate((node) => node.tagName.toLowerCase());
  if (tag === "input") {
    // A money field: the act is entering an amount, and 150 000 is simply a different price from
    // the seeded one, so the save has something to write.
    await target.fill("150000");
    return;
  }
  // Asked *before* the click, for two reasons. It is what the control says about itself, rather
  // than a guess from what appeared afterwards — and a locator read is not free: `getAttribute`
  // waits for its element, so asking after a click that navigated or re-rendered would block on an
  // element that no longer exists until the whole test times out.
  const opensAPicker = (await target.getAttribute("aria-haspopup")) === "listbox";
  await target.click();
  // A combobox trigger opens a listbox instead of doing the thing; choosing is the same step.
  //
  // Gated on the trigger's own `aria-haspopup` rather than on what appeared, because what appears
  // is a race.
  // `ComboboxField` paints its popup the instant it is clicked and fills it when the options land:
  // until then `shown()` is empty and the popup renders `emptyLabel` — a bare `<p>`, with no
  // `[role="listbox"]` anywhere in it. A count taken at that moment is zero on any runner slower
  // than the one this was written on, the helper concludes the control was not a combobox, and the
  // popup is left open over whatever the next step means to click.
  //
  // That is the failure this gate has been producing: the click after it retries against
  // `<p>Nothing matches that search</p>`, then against `<li role="option">Airport branches</li>`
  // once the data arrives, for the whole two-minute timeout, and the run blames a control that was
  // never broken.
  if (opensAPicker) {
    const options = page.locator('[role="listbox"] [role="option"]');
    // Wait for the rows rather than counting them. A combobox with nothing to offer is a real
    // state — a cohort with no shops in it — so this is best-effort and the branch below handles
    // the empty case rather than failing here, where the message would be about a timeout.
    await options
      .first()
      .waitFor({ state: "visible", timeout: SETTLE_MS })
      .catch(() => {});
    if ((await options.count()) === 0) {
      // Nothing to choose, and a popup that must not be left covering the next control. Escape is
      // what an operator would press, and `ComboboxField` handles it — but only from the search
      // box, which is focused on open, so this is the same path a person takes.
      await page.keyboard.press("Escape");
      await page
        .locator('[role="combobox"]')
        .waitFor({ state: "hidden", timeout: SETTLE_MS })
        .catch(() => {});
      return;
    }
    await options.first().click();
    // And then wait for it to close, which is the half this was missing.
    //
    // The listbox is painted over the rest of the form, so while it is still on screen it
    // intercepts pointer events for whatever sits under it — including the next step's button.
    // Playwright does not fail that click; it retries it, for the full two-minute test timeout,
    // and the run ends with `<li role="option">Airport branches</li> ... intercepts pointer
    // events` repeated 230 times. It reads as the *next* control being broken, which is the
    // wrong place to look entirely.
    //
    // Whether it bites depends on how quickly the runner repaints, so it failed in CI and not
    // on a developer's machine — the shape every intermittent failure in this harness has had.
    await page
      .locator('[role="listbox"]')
      .waitFor({ state: "hidden", timeout: SETTLE_MS })
      .catch(() => {
        // A listbox that stays open is a real fault, but not this helper's to diagnose: the
        // step that follows will fail on its own terms, and with a better message than a
        // timeout here.
      });
  }
}

/**
 * What a flow needs typed between its clicks, keyed by task and by the step it follows.
 *
 * Typing is not clicking, and the budget counts clicks: the wizard needs a name before it can
 * create a store, exactly as the till's shift flow needs a float in the box before Open. Kept here
 * rather than in the declaration for that reason — it is how the harness reaches the next click,
 * not part of the map.
 */
const AFTER_STEP = {
  "Provision a new store and get its installer": async (page, index) => {
    if (index === 1) {
      // Step 2 followed the link into the wizard; its first field is the shop's name.
      await page.locator('input[type="text"]').first().fill("Xuân Thuỷ");
    }
  },
};

const replayed = TASKS.filter((declared) => declared.unreplayable === undefined);
const skipped = TASKS.filter((declared) => declared.unreplayable !== undefined);

/**
 * Walks a declared flow's steps, in the order and by the means the declaration names.
 *
 * `upTo` stops after that many steps, for a test that needs the screen a flow reaches rather than
 * the flow's own outcome. It defaults to the whole declaration, which is what the replay wants.
 */
async function walk(page, declared, upTo = declared.steps.length) {
  for (const [index, step] of declared.steps.slice(0, upTo).entries()) {
    const description = `step ${index + 1} of "${declared.task}"`;
    if (step.nav !== undefined) {
      await openNavGroupFor(page, step.nav);
      await page.locator(`[data-nav="${step.nav}"]`).first().click();
    } else if (step.link !== undefined) {
      await page.locator(`[data-nav="${step.link.to}"]`).first().click();
    } else {
      await press(page, step.action, description, step.value);
    }
    await AFTER_STEP[declared.task]?.(page, index);
  }
}

for (const declared of replayed) {
  test(declared.task, async ({ browser }) => {
    const { context, page } = await openConsole(browser);

    await walk(page, declared);

    await expect(
      page.locator(`[data-outcome="${declared.outcome.mark}"]`).first(),
      `"${declared.task}" ran its ${declared.steps.length} declared clicks and never reached \`data-outcome="${declared.outcome.mark}"\` — the flow needs a click nobody declared, or it no longer works`,
    ).toBeVisible();
    await context.close();
  });
}

// A picker whose options arrive late does not swallow the next step's click.
//
// This is the failure this gate kept producing, and the reason it is a test rather than a retry.
// `ComboboxField` paints its popup the moment the trigger is clicked and fills it when the options
// land; until then it renders `emptyLabel` — a bare `<p>`, with no `[role="listbox"]` in it. The
// harness counted options at that instant, got zero, concluded the control was not a picker, and
// walked on. The popup stayed open, absolutely positioned over the rest of the form, and the *next*
// step's click hit it instead of its own button. Playwright does not fail an intercepted click; it
// retries, for the full test timeout, and the run blames a control that was never broken.
//
// The cohort picker's options come from `GET /admin/store-groups`, so delaying that response
// reproduces on any machine what a loaded CI runner produced by itself. The delay is under the
// harness's own settle budget, because the point is that waiting is enough — not that the flow
// survives an outage.
test("a picker whose options arrive late does not swallow the next click", async ({ browser }) => {
  const declared = replayed.find((task) => task.task === "Publish one menu to a whole cohort of shops");
  expect(declared, "the cohort publish flow is the one with a served picker in it").toBeDefined();

  const { context, page } = await openConsole(browser);
  await page.route(/\/admin\/store-groups\?/, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 1_200));
    await route.continue();
  });

  await walk(page, declared);

  await expect(
    page.locator(`[data-outcome="${declared.outcome.mark}"]`).first(),
    "the flow reached its outcome with the cohort list served late, so no popup was left over the next control",
  ).toBeVisible();
  await context.close();
});

// A price in a two-decimal currency is typed as the price, not as its minor units.
//
// [ADR-0135](../../docs/adr/0135-the-console-reads-money-the-way-the-till-does.md): `MoneyField`
// used to edit `amount_minor` directly, so ₹261.45 was authored by typing `26145` — every decimal
// point the operator pressed was dropped on the way in. The field now reads the exponent the
// platform publishes.
//
// The unit tests in `money.test.tsx` pin the arithmetic with the exponent handed straight in. What
// only a browser against a real cloud can show is the chain between: `Shell` mounts, reads
// `GET /admin/countries`, and the field on the far side of four clicks knows what the rupee is. So
// this asserts the one thing that was impossible before — that a typed decimal survives — rather
// than re-testing the formatter.
//
// It stops at the field and does not save. Whether the cloud accepts a placement priced in a
// currency other than the store's own is a separate question from whether the operator can type the
// price, and answering it here would make a money test fail for a reason that is not about money.
test("a price in a two-decimal currency is typed as the price", async ({ browser }) => {
  const declared = replayed.find((task) => task.task.startsWith("Change an item's price"));
  expect(declared, "the price-change flow is the one that reaches a money field").toBeDefined();

  const { context, page } = await openConsole(browser);
  // The first four declared clicks end on the placement editor, which is where the money is.
  await walk(page, declared, 4);

  // Scoped to the open drawer, and found by the option it carries rather than by its label, so the
  // test depends on neither the language the console booted in nor what else is on the screen.
  const editor = page.locator('[role="dialog"]').first();
  await expect(editor, "the fourth click opens the placement editor").toBeVisible();
  const currency = editor
    .locator("select")
    .filter({ has: page.locator('option[value="INR"]') })
    .first();
  await expect(
    currency,
    "the currency picker offers INR, which it takes from the compiled country list",
  ).toBeVisible();
  await currency.selectOption("INR");

  const amount = editor.locator('[data-step="setChannelAmount"]').first();
  await amount.fill("261.45");
  // Blur, because that is when the field drops the draft and redraws from what it actually emitted.
  // Asserting before it would only prove the input kept the characters typed into it.
  await amount.blur();
  await expect(
    amount,
    "₹261.45 was typed and the field settled on it — before ADR-0135 the decimal point was stripped and the field read 26,145",
  ).toHaveValue("261.45");

  await expect(amount, "a currency with decimals asks for a keypad that has one").toHaveAttribute(
    "inputmode",
    "decimal",
  );
  await context.close();
});

test("the flows this harness skips are exactly the ones that say so", () => {
  // A flow drops out of the browser gate only by declaring why, in `scripts/step-tasks.mjs`. This
  // asserts the count rather than the reasons: the reasons are prose for a reader, and what must not
  // happen quietly is the set growing.
  expect(skipped.map((declared) => declared.task)).toEqual(["Acknowledge a firing alert"]);
  expect(replayed.length).toBe(TASKS.length - skipped.length);
});
