// A screenshot walk of every edge screen, at the three device sizes the framework ships for.
//
// Not a gate — a **record**. It boots the same `examples/minimal-edge` the step gate drives, signs
// in as the demo employee, drives the store into each meaningful state, and photographs the screen
// at a Windows POS size, a 10" Android tablet size and a phone size. What comes out is what an
// operator actually sees, rather than what a component file suggests they would.
//
// Every step is best-effort and time-boxed: a screen that cannot be reached records the reason and
// the walk goes on. A walk that stops at the first unreachable screen photographs nothing, and a
// step with no timeout eats the whole test's budget when a modal is in the way.

import { test } from "@playwright/test";
import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { startEdge } from "./edge.mjs";

const OUT = fileURLToPath(new URL("../screenshots/", import.meta.url));

/** Every locator action gets this. Long enough for a slow boot, short enough to fail a stuck modal. */
const STEP_MS = 6_000;

/** The three sizes the framework is aimed at, named as the hardware rather than as pixels. */
const DEVICES = [
  { id: "pos", label: "Windows POS 1366x768", width: 1366, height: 768 },
  { id: "tablet", label: "Android tablet 10in landscape 1200x800", width: 1200, height: 800 },
  { id: "phone", label: "Phone portrait 390x844", width: 390, height: 844 },
];

function walker(page, device, notes) {
  const tap = (selector, nth = 0) =>
    page.locator(selector).nth(nth).click({ timeout: STEP_MS });
  const type = (selector, value) =>
    page.locator(selector).first().fill(value, { timeout: STEP_MS });
  const see = (selector) =>
    page.locator(selector).first().waitFor({ state: "visible", timeout: STEP_MS });

  return {
    tap,
    type,
    see,
    async shot(name, reach, note) {
      try {
        await reach();
        await page.waitForTimeout(200);
        const file = `${device.id}--${name}.png`;
        await page.screenshot({ path: `${OUT}${file}` });
        notes.push({ device: device.id, screen: name, file, note });
      } catch (error) {
        notes.push({
          device: device.id,
          screen: name,
          file: null,
          note: `UNREACHED: ${String(error.message).split("\n")[0].slice(0, 160)}`,
        });
      }
    },
  };
}

for (const device of DEVICES) {
  test(`screens — ${device.label}`, async ({ browser }) => {
    const notes = [];
    const edge = await startEdge();
    const context = await browser.newContext({
      viewport: { width: device.width, height: device.height },
      hasTouch: true,
    });
    const page = await context.newPage();
    const w = walker(page, device, notes);

    try {
      await w.shot(
        "01-pairing",
        async () => {
          await page.goto(`${edge.baseURL}/pair?code=${edge.pairingCode}`);
        },
        "First run: the device redeems the 6-digit code the edge printed at boot (ADR-0030/0084)",
      );

      await w.shot(
        "02-signin",
        async () => {
          await w.tap("#pair-submit");
          await page.waitForURL(/\/signin$/, { timeout: STEP_MS });
        },
        "Staff sign-in: badge code + PIN, works with no internet",
      );

      await w.shot(
        "03-floor",
        async () => {
          await w.type("#signin-code", edge.staffCode);
          await w.type("#signin-pin", edge.staffPin);
          await w.tap('[data-step="submit"]');
          await w.see('[data-outcome="floor"]');
        },
        "The room. One tap on a table seats it and opens its order",
      );

      await w.shot(
        "04-order-empty",
        async () => {
          await w.tap('[data-step="onCard"]');
          await w.see('[data-outcome="order-open"]');
        },
        "A seated table: menu on one side, the bill on the other",
      );

      await w.shot(
        "05-order-lines",
        async () => {
          await w.tap('[data-step="onItem"]');
          await w.see('[data-outcome="line-added"]');
          await w.tap('[data-step="onItem"]', 1);
          await page.waitForTimeout(300);
        },
        "Two lines on the bill, one tap each",
      );

      await w.shot(
        "06-order-modifier",
        async () => {
          await w.type("#menu-search", "margherita");
          await w.tap('[data-step="onItem"]');
          await w.see('[data-step="chooseModifier"]');
        },
        "An item that asks a question before it joins the bill (modifier groups)",
      );

      await w.shot(
        "07-order-search",
        async () => {
          // Close the modifier dialog the way an operator does, or the search below is blocked.
          await w.tap('[data-step="chooseModifier"]');
          await w.tap('[data-step="confirmItem"]');
          await page.waitForTimeout(300);
          await w.type("#menu-search", "sa");
          await page.waitForTimeout(300);
        },
        "Finding an item by name instead of hunting the grid",
      );

      await w.shot(
        "08-order-fired",
        async () => {
          await w.type("#menu-search", "");
          await w.tap('[data-step="fireOrder"]');
          await w.see('[data-outcome="line-fired"]');
        },
        "Lines sent to the kitchen",
      );

      await w.shot(
        "09-kds",
        async () => {
          await w.tap('a[href="/kds"]');
          await page.waitForURL((url) => url.pathname === "/kds", { timeout: STEP_MS });
        },
        "Kitchen display: the tickets the line is cooking",
      );

      await w.shot(
        "10-expo",
        async () => {
          await w.tap('a[href="/expo"]');
          await page.waitForURL((url) => url.pathname === "/expo", { timeout: STEP_MS });
        },
        "The pass: what is ready to run away to a table",
      );

      await w.shot(
        "11-pay",
        async () => {
          await w.tap('a[href="/"]');
          await page.waitForURL((url) => url.pathname === "/", { timeout: STEP_MS });
          await w.tap('[data-step="onCard"]');
          await w.see('[data-outcome="order-open"]');
          await w.tap('[data-step="takePayment"]');
          await page.waitForURL(/\/pay$/, { timeout: STEP_MS });
        },
        "Settling: tender keys, tip, cash or card",
      );

      await w.shot(
        "12-counter",
        async () => {
          await page.goto(`${edge.baseURL}/counter`);
        },
        "Takeaway / counter orders (empty on fakes: the relay needs a cloud)",
      );

      await w.shot(
        "13-guests",
        async () => {
          await page.goto(`${edge.baseURL}/guests`);
        },
        "Guest count confirmation",
      );

      await w.shot(
        "14-today",
        async () => {
          await page.goto(`${edge.baseURL}/today`);
        },
        "Today: what this store has taken so far",
      );

      await w.shot(
        "15-shift",
        async () => {
          await page.goto(`${edge.baseURL}/shift`);
        },
        "Cash shift: open with a float, count, close",
      );

      await w.shot(
        "16-devices",
        async () => {
          await page.goto(`${edge.baseURL}/devices`);
        },
        "Devices paired to this store",
      );

      await w.shot(
        "17-setup",
        async () => {
          await page.goto(`${edge.baseURL}/setup`);
        },
        "First-run setup / activation",
      );
    } finally {
      mkdirSync(OUT, { recursive: true });
      writeFileSync(`${OUT}index-${device.id}.json`, JSON.stringify(notes, null, 2));
      await context.close();
      await edge.stop();
    }
  });
}
