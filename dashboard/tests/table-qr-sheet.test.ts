// The table QR sheet: each code holds the table's guest address, drawn module by module (ADR-0253).

import { describe, expect, it } from "vitest";

import { modulePath, tableAddress } from "../src/screens/floor/QrSheet";

describe("the table QR sheet", () => {
  it("addresses the guest page at the console's own origin, with the table's token", () => {
    expect(tableAddress("https://cloud.example", "TOKEN.SIG")).toBe("https://cloud.example/qr/#t=TOKEN.SIG");
    expect(tableAddress("https://cloud.example/", "T")).toBe("https://cloud.example/qr/#t=T");
  });

  it("draws one unit square per dark module, inside a quiet zone of four", () => {
    expect(modulePath([[true, false], [false, true]])).toBe("M4 4h1v1h-1zM5 5h1v1h-1z");
  });

  it("encodes an address with the till's encoder", async () => {
    const { default: qrcode } = await import("qrcode-generator");
    const code = qrcode(0, "M");
    code.addData(tableAddress("https://cloud.example", "T"));
    code.make();
    expect(code.getModuleCount()).toBeGreaterThanOrEqual(21);
  });
});
