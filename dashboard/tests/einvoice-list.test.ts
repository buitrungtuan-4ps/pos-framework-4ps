import { describe, expect, it } from "vitest";

import { stateTone } from "../src/screens/einvoices/InvoiceList";

describe("the invoice list's submission badge", () => {
  it("reads submitted as done, refused as a fault, and the rest as waiting", () => {
    expect(stateTone("SUBMITTED")).toBe("active");
    expect(stateTone("REJECTED")).toBe("danger");
    expect(stateTone("PENDING")).toBe("neutral");
    expect(stateTone("RETRYING")).toBe("neutral");
  });
});
