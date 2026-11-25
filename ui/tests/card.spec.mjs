// A card through the card terminal (ADR-0230), against `examples/minimal-edge`, whose terminal is
// the fake, scripted card by card with `POS_DEMO_CARD`. A capture settles the bill; a decline says so
// and the bill is paid another way; an unknown result parks the bill amber, holds it across a reload,
// and is resolved by asking the terminal again, or cancelled on it. Demo data only. Nothing here is
// a real terminal: B10.3's adapter waits on the owner's choice of vendor.

import { expect, test } from "@playwright/test";

import { startEdge } from "./edge.mjs";

/** Pairs, signs the demo employee in, seats a table, rings one dish, sends it and opens payment. */
async function aBillToPay(page, edge) {
  await page.goto(`${edge.baseURL}/pair?code=${edge.pairingCode}`);
  await page.locator("#pair-submit").click();
  await expect(page).toHaveURL(/\/signin$/);
  await page.locator("#signin-code").fill(edge.staffCode);
  await page.locator("#signin-pin").fill(edge.staffPin);
  await page.locator('[data-step="submit"]').click();
  await expect(page.locator('[data-outcome="floor"]')).toBeVisible();
  await page.locator('[data-step="onCard"]').first().click();
  await page.locator('[data-outcome="guest-pad"] [data-step="seatWithGuests"]').nth(1).click();
  await expect(page.locator('[data-outcome="order-open"]')).toBeVisible();
  await page.locator('[data-step="onItem"]').first().click();
  await expect(page.locator('[data-outcome="line-added"]').first()).toBeAttached();
  await expect(page.locator("[data-link]")).toHaveAttribute("data-link", "open");
  await page.locator('[data-step="fireOrder"]').click();
  await expect(page.locator('[data-outcome="line-fired"]').first()).toBeVisible();
  await expect(page.locator('[data-outcome="writes-pending"]')).toHaveCount(0);
  await page.locator('[data-step="takePayment"]').click();
  await expect(page).toHaveURL(/\/pay$/);
}

test("a card the terminal takes settles the bill", async ({ page }) => {
  const edge = await startEdge();
  try {
    await aBillToPay(page, edge);
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("a declined card charges nothing, and the bill is paid in cash", async ({ page }) => {
  const edge = await startEdge(undefined, { POS_DEMO_CARD: "DECLINED" });
  try {
    await aBillToPay(page, edge);
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="card-note"]')).toHaveText("The card was declined. Nothing was charged.");
    await expect(page.locator('[data-outcome="settled"]')).toHaveCount(0);
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("an unknown card parks the bill amber, across a reload, until the terminal is asked again", async ({ page }) => {
  const edge = await startEdge(undefined, { POS_DEMO_CARD: "UNKNOWN" });
  try {
    await aBillToPay(page, edge);
    await page.locator('[data-step="payCard"]').click();
    const parked = page.locator('[data-outcome="card-unknown"]');
    await expect(parked).toBeVisible();
    await expect(page.locator('[data-step="payCard"]')).toHaveCount(0);

    // The edge refuses another tender while the card is unknown, whatever the till offers.
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toHaveCount(0);

    // A reload reads the park back from the edge.
    await page.reload();
    await expect(parked).toBeVisible();

    // The acquirer has the answer by now: asking again settles the bill once.
    await page.locator('[data-step="cardLookUp"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("an unknown card cancelled on the terminal frees the bill for cash", async ({ page }) => {
  const edge = await startEdge(undefined, { POS_DEMO_CARD: "UNKNOWN" });
  try {
    await aBillToPay(page, edge);
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="card-unknown"]')).toBeVisible();
    await page.locator('[data-step="cardCancel"]').click();
    await expect(page.locator('[data-outcome="card-unknown"]')).toHaveCount(0);
    await page.locator('[data-step="payCash"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});

test("a card confirmed from the terminal's slip waits for a manager (ADR-0243)", async ({ page }) => {
  const edge = await startEdge(undefined, { POS_DEMO_CARD: "UNKNOWN" });
  try {
    await aBillToPay(page, edge);
    await page.locator('[data-step="payCard"]').click();
    await expect(page.locator('[data-outcome="card-unknown"]')).toBeVisible();
    await page.locator('[data-step="cardConfirm"]').click();
    // The edge asks for a manager; nothing is settled meanwhile.
    const approver = page.locator('[data-outcome="card-confirm-approver"]');
    await expect(approver).toBeVisible();
    await expect(page.locator('[data-outcome="settled"]')).toHaveCount(0);
    // The demo employee holds every permission, so they approve, as the void replays do.
    await page.locator("#card-confirm-approver-code").fill(edge.staffCode);
    await page.locator("#card-confirm-approver-pin").fill(edge.staffPin);
    await page.locator('[data-step="cardConfirmApproved"]').click();
    await expect(page.locator('[data-outcome="settled"]')).toBeVisible();
  } finally {
    await edge.stop();
  }
});
