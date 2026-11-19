// The guest page's replay (ADR-0224 decision 2): a guest at a table reads the menu, chooses a
// modifier, and sends an order, in a real browser under the page's own content security policy.
//
// The page is `guest/`, served here as the cloud serves it, and the cloud's two guest routes answer
// from fakes: the replay is of the page, not of the cloud, whose routes have their own tests
// (`crates/pos-cloud/tests/cloud/qr_menu.rs`). No personal data appears anywhere in it.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

const GUEST = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "guest");
const ORIGIN = "https://guest.test";
const TOKEN = "01J00000000000000000000T01.01J00000000000000000000S01.01J00000000000000000000B01.abcdef";
const POLICY =
  "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; " +
  "base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

const FILES = {
  "/qr/": ["index.html", "text/html"],
  "/qr/app.js": ["app.js", "text/javascript"],
  "/qr/style.css": ["style.css", "text/css"],
  "/qr/i18n/en.json": ["i18n/en.json", "application/json"],
  "/qr/i18n/vi.json": ["i18n/vi.json", "application/json"],
};

const PIZZA = "01J000000000000000000000P1";
const CHEESE = "01J000000000000000000000C1";
const TEA = "01J000000000000000000000T1";
const TOPPINGS = "01J000000000000000000000G1";

function menu(language) {
  const vi = language === "vi";
  return {
    language,
    languages: ["en", "vi"],
    items: [
      { menu_item_id: PIZZA, name: vi ? "Bánh Demo A" : "Demo Pizza A", unit_price: { currency_code: "VND", amount_minor: 150000 }, available: true, modifier_group_ids: [TOPPINGS] },
      { menu_item_id: CHEESE, name: vi ? "Thêm phô mai" : "Extra cheese", unit_price: { currency_code: "VND", amount_minor: 20000 }, available: true, modifier_group_ids: [] },
      { menu_item_id: TEA, name: vi ? "Trà Demo B" : "Demo Tea B", unit_price: { currency_code: "VND", amount_minor: 15000 }, available: false, modifier_group_ids: [] },
    ],
    modifier_groups: [
      { modifier_group_id: TOPPINGS, name: vi ? "Thêm" : "Toppings", min_select: 0, max_select: 1, member_menu_item_ids: [CHEESE] },
    ],
  };
}

async function serve(page, orders) {
  await page.route(`${ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    const file = FILES[url.pathname];
    if (file) {
      return route.fulfill({
        status: 200,
        headers: { "content-type": file[1], "content-security-policy": POLICY },
        body: readFileSync(join(GUEST, file[0])),
      });
    }
    if (url.pathname === "/v1/qr/menu") {
      expect(url.searchParams.get("table_token")).toBe(TOKEN);
      return route.fulfill({ json: menu(url.searchParams.get("lang") ?? "en") });
    }
    if (url.pathname === "/v1/qr/orders" && route.request().method() === "POST") {
      orders.push(route.request().postDataJSON());
      return route.fulfill({ status: 201, json: { order_id: "01J000000000000000000000O1", created: true } });
    }
    return route.fulfill({ status: 404, body: "" });
  });
}

test.describe("guest page", () => {
  test.use({ locale: "en-US" });

  test("a guest reads the menu, chooses a topping and sends an order that names nobody", async ({ page }) => {
    const orders = [];
    const violations = [];
    page.on("console", (entry) => {
      if (/Content Security Policy/i.test(entry.text())) violations.push(entry.text());
    });
    await serve(page, orders);
    await page.goto(`${ORIGIN}/qr/#t=${TOKEN}`);

    await expect(page.getByRole("heading", { name: "Menu" })).toBeVisible();
    await expect(page.getByText("Sold out")).toBeVisible();
    const pizza = page.locator(".item", { hasText: "Demo Pizza A" });
    await pizza.getByRole("button", { name: "Add" }).click();
    await page.getByLabel(/Extra cheese/).check();
    await page.locator(".choices").getByRole("button", { name: "Add" }).click();
    await expect(page.getByText("1 × Demo Pizza A + Extra cheese")).toBeVisible();
    await page.getByRole("button", { name: "One more Demo Pizza A" }).click();
    await expect(page.getByText("2 × Demo Pizza A + Extra cheese")).toBeVisible();

    await page.getByRole("button", { name: "Send order" }).click();
    await expect(page.getByText("Your order is sent.")).toBeVisible();
    expect(orders).toHaveLength(1);
    const [sent] = orders;
    expect(sent.table_token).toBe(TOKEN);
    expect(sent.lines).toEqual([{ menu_item_id: PIZZA, quantity_milli: 2000, modifier_menu_item_ids: [CHEESE] }]);
    expect(Object.keys(sent).sort()).toEqual(["external_reference", "lines", "placed_at_ms", "table_token"]);
    expect(violations).toEqual([]);
  });

  test("a guest switches the menu's language, and a page with no table code says so", async ({ page }) => {
    await serve(page, []);
    await page.goto(`${ORIGIN}/qr/#t=${TOKEN}`);
    await page.getByRole("button", { name: "vi", exact: true }).click();
    await expect(page.getByText("Bánh Demo A")).toBeVisible();

    await page.goto(`${ORIGIN}/qr/`);
    await page.reload();
    await expect(page.getByText("This table code is not recognised.")).toBeVisible();
  });
});
