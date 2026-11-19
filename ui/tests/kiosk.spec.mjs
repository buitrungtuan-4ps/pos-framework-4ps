// The kiosk's replay (ADR-0224 decisions 4 to 6), against `examples/minimal-edge` on the `kiosk`
// profile: a terminal named `Kiosk` of class `DEVICE_CLASS_KIOSK`, and the kiosk's own staff record.
// A manager binds this device to it and signs out; the till then signs in as the kiosk by itself,
// a guest orders and sends the order to be paid at the counter, and the manager leaves the kiosk
// with their code and PIN. Demo data only.

import { expect, test } from "@playwright/test";

import { startEdge } from "./edge.mjs";

async function signIn(page, edge) {
  await page.locator("#signin-code").fill(edge.staffCode);
  await page.locator("#signin-pin").fill(edge.staffPin);
  await page.locator('[data-step="submit"]').click();
  await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
}

/** Pairs, signs the demo manager in, opens a shift and binds this device to the kiosk terminal. */
async function aKiosk(page, edge, { shift = true } = {}) {
  await page.goto(`${edge.baseURL}/pair?code=${edge.pairingCode}`);
  await page.locator("#pair-submit").click();
  await expect(page).toHaveURL(/\/signin$/);
  await signIn(page, edge);
  if (shift) {
    await page.locator('a[href="/shift"]').first().click();
    await page.locator("#float").fill("100000");
    await page.locator('[data-step="openShift"]').click();
    await expect(page.locator('[data-outcome="shift-open"]')).toBeVisible();
  }
  await page.locator('a[href="/devices"]').first().click();
  const card = page.locator('[data-outcome="till"]');
  await card.locator("button[aria-pressed]").filter({ hasText: "Kiosk" }).click();
  await card.getByRole("button", { name: "Bind this device to Kiosk" }).click();
  await expect(card.locator('[data-outcome="till-status"]')).toHaveText("This device is Kiosk.");
  await page.getByRole("button", { name: "Sign out" }).click();
  // Signed out, a till on a kiosk goes to the kiosk's own screen by itself.
  await expect(page).toHaveURL(/\/kiosk$/);
  await expect(page.locator('[data-outcome="kiosk-menu"]')).toBeVisible();
}

test("a kiosk signs in as itself, sends a guest's order to the counter, and a manager leaves it with a PIN", async ({
  page,
}) => {
  const edge = await startEdge("kiosk");
  try {
    await aKiosk(page, edge);
    await expect(page).toHaveURL(/\/kiosk$/);

    // The guest rings two of one dish and one of another, and takes one back.
    const items = page.locator('[data-step="kioskItem"]');
    await items.first().click();
    await expect(page.locator('[data-outcome="kiosk-line"]')).toHaveCount(1);
    await items.first().click();
    await expect(page.locator('[data-outcome="kiosk-line"]').first()).toContainText("2 ×");
    await items.nth(1).click();
    await expect(page.locator('[data-outcome="kiosk-line"]')).toHaveCount(2);
    await page.getByRole("button", { name: /^One fewer / }).last().click();
    await expect(page.locator('[data-outcome="kiosk-line"]')).toHaveCount(1);

    await page.locator('[data-step="kioskSend"]').click();
    await expect(page.locator('[data-outcome="kiosk-done"]')).toBeVisible();
    await expect(page.locator('[data-outcome="kiosk-number"]')).toHaveText(/^\d+$/);
    await page.getByRole("button", { name: "New order" }).click();
    await expect(page.locator('[data-outcome="kiosk-menu"]')).toBeVisible();

    // A wrong PIN keeps the kiosk; the manager's own leaves it for the till.
    await page.locator('[data-step="kioskExit"]').click();
    await page.locator("#kiosk-exit-code").fill(edge.staffCode);
    await page.locator("#kiosk-exit-pin").fill("0000");
    await page.locator('[data-step="kioskExitSubmit"]').click();
    await expect(page.getByRole("alert")).toHaveText("That code or PIN is not right.");
    await page.locator("#kiosk-exit-pin").fill(edge.staffPin);
    await page.locator('[data-step="kioskExitSubmit"]').click();
    await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("a kiosk order nobody touches is given up with the store's own reason, and the kiosk starts over", async ({
  page,
}) => {
  const edge = await startEdge("kiosk");
  try {
    await page.clock.install();
    await aKiosk(page, edge);
    await page.locator('[data-step="kioskItem"]').first().click();
    await expect(page.locator('[data-outcome="kiosk-line"]')).toHaveCount(1);
    // Touched before the wait is up, the order stays; untouched for the store's two minutes, it goes.
    await page.clock.runFor(100_000);
    await page.getByRole("heading", { name: "Your order" }).click();
    await page.clock.runFor(100_000);
    await expect(page.locator('[data-outcome="kiosk-line"]')).toHaveCount(1);
    await page.clock.runFor(21_000);
    await expect(page.locator('[data-outcome="kiosk-line"]')).toHaveCount(0);
    await expect(page.getByText("Tap a dish to start your order.")).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("a kiosk with no shift open says it is not taking orders", async ({ page }) => {
  const edge = await startEdge("kiosk");
  try {
    await aKiosk(page, edge, { shift: false });
    await page.locator('[data-step="kioskItem"]').first().click();
    await expect(page.locator('[data-outcome="kiosk-closed"]')).toHaveText(
      "This kiosk is not taking orders right now. Please order at the counter.",
    );
  } finally {
    await edge.stop();
  }
});
