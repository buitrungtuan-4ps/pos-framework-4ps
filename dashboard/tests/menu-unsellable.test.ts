// A menu publish the store cannot sell is refused naming each item (pilot finding M1). The cloud
// names each item in a detail, `items.<menu_item_id>`, with the fault's token; the console words it
// in the operator's language with the item's name, and leaves any other refusal to `errorMessage`.

import { describe, expect, it } from "vitest";

import { ApiError } from "../src/api/client";
import { unsellableMessage } from "../src/screens/catalog/shared";

const names: Record<string, string> = { A1: "Margherita", B2: "House wine" };
const itemName = (id: string) => names[id] ?? id;

describe("a refused menu publish", () => {
  it("names each item the store cannot sell, and why", () => {
    const refused = new ApiError(422, "the store cannot sell this menu", "INVALID_ARGUMENT", [
      { field: "items.A1", reason: "CURRENCY_MISMATCH" },
      { field: "items.B2", reason: "TAX_RATE_NOT_CONFIGURED" },
    ]);
    const message = unsellableMessage(refused, itemName) ?? "";
    expect(message).toContain("Margherita: its price is in another currency");
    expect(message).toContain("House wine: the store has no tax rate");
  });

  it("leaves any other refusal to the server's own words", () => {
    expect(unsellableMessage(new ApiError(422, "the menu does not compile"), itemName)).toBeNull();
    expect(unsellableMessage(new ApiError(503, "down"), itemName)).toBeNull();
    expect(unsellableMessage(new Error("no answer"), itemName)).toBeNull();
  });
});
