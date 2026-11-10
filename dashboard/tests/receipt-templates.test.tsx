// The receipt template editor (ADR-0207): the base template and each channel's are edited as
// structure, keys come from the tenant's translations with English, and a section a country's law
// requires is never offered off.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Json } from "../src/api/types";
import en from "../src/i18n/en.json";
import {
  isTemplates,
  templateFault,
  withKeys,
  withSection,
  withTemplate,
} from "../src/lib/receipt-templates";
import { ReceiptTemplatesEditor } from "../src/screens/settings/receipt/ReceiptTemplatesEditor";

const messages = en as Record<string, string>;
const TENANT = "01M22190WCY5PS7KCA7ET7H679";

vi.mock("../src/api/client", () => ({
  api: {
    getTranslations: () =>
      Promise.resolve({
        value: {
          "receipt.footer.thanks": { en: "Thank you", vi: "Cảm ơn" },
          "receipt.footer.untranslated": { vi: "Hẹn gặp lại" },
        },
        etag: "tg1",
      }),
    listCountries: () =>
      Promise.resolve([
        { code: "VN", receipt_required_sections: ["RECEIPT_SECTION_MODIFIERS"] },
        { code: "ZZ", receipt_required_sections: [] },
      ]),
  },
}));

describe("receipt templates as data", () => {
  it("writes a channel's template beside the base and removes it again, leaving defaults off the wire", () => {
    const base = withKeys({}, "footer_keys", ["receipt.footer.thanks"]);
    const both = withTemplate(withTemplate({}, "base", base), "SALES_CHANNEL_TAKEAWAY", withSection({}, "payments", false));
    expect(both).toEqual({
      base: { footer_keys: ["receipt.footer.thanks"] },
      channels: { SALES_CHANNEL_TAKEAWAY: { sections: { payments: false } } },
    });
    const takeaway = both.channels?.SALES_CHANNEL_TAKEAWAY ?? {};
    expect(withSection(takeaway, "payments", true)).toEqual({});
    expect(withTemplate(both, "SALES_CHANNEL_TAKEAWAY", undefined)).toEqual({ base: both.base });
  });

  it("finds a blank key, one with no English, too many lines, and a required section turned off", () => {
    const english = (key: string) => key === "receipt.footer.thanks";
    const none = new Set<string>();
    expect(templateFault({ footer_keys: [" "] }, english, none)).toEqual({ kind: "blank_key", part: "footer_keys" });
    expect(templateFault({ footer_keys: ["receipt.footer.untranslated"] }, english, none)).toEqual({
      kind: "key_without_english",
      key: "receipt.footer.untranslated",
    });
    expect(templateFault({ header_keys: ["a", "b", "c", "d", "e"] }, null, none)).toEqual({
      kind: "too_many_keys",
      part: "header_keys",
    });
    expect(
      templateFault({ sections: { modifiers: false } }, english, new Set(["RECEIPT_SECTION_MODIFIERS"])),
    ).toEqual({ kind: "section_required", field: "modifiers" });
    expect(isTemplates({ base: { footer_keys: [""] } })).toBe(false);
    expect(isTemplates({ base: { footer_keys: ["receipt.footer.thanks"] } })).toBe(true);
    expect(isTemplates("nope")).toBe(false);
  });
});

describe("the receipt template editor", () => {
  afterEach(cleanup);

  function mount(initial: Json | undefined) {
    const [value, setValue] = createSignal<Json | undefined>(initial);
    render(() => (
      <ReceiptTemplatesEditor tenantId={TENANT} value={value()} onChange={setValue} disabled={false} />
    ));
    return value;
  }

  it("adds a base template, a footer line from translations, and says a key with no English", async () => {
    const value = mount(undefined);
    fireEvent.click(screen.getByRole("button", { name: messages["settings.receipt.add"]! }));
    fireEvent.click(screen.getAllByRole("button", { name: messages["settings.receipt.addLine"]! })[1]!);
    const line = screen.getByLabelText(/^Line 1/) as HTMLInputElement;
    fireEvent.input(line, { target: { value: "receipt.footer.untranslated" } });
    expect(value()).toEqual({ base: { footer_keys: ["receipt.footer.untranslated"] } });
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("receipt.footer.untranslated"));
    fireEvent.input(line, { target: { value: "receipt.footer.thanks" } });
    await waitFor(() => expect(screen.queryByRole("alert")?.textContent ?? null).toBeNull());
  });

  it("draws a section the law requires as printing and disabled, and turns another off", async () => {
    const value = mount({ base: {} });
    const modifiers = () =>
      screen.getByRole("switch", { name: messages["settings.receipt.section.modifiers"]! }) as HTMLButtonElement;
    await waitFor(() => expect(modifiers().disabled).toBe(true));
    expect(modifiers().getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByRole("switch", { name: messages["settings.receipt.section.payments"]! }));
    expect(value()).toEqual({ base: { sections: { payments: false } } });
  });

  it("edits a channel's own template and leaves the base alone", () => {
    const value = mount({ base: { footer_keys: ["receipt.footer.thanks"] } });
    fireEvent.change(document.querySelector("select")!, {
      target: { value: "SALES_CHANNEL_TAKEAWAY" },
    });
    expect(screen.getByText(messages["settings.receipt.noChannel"]!)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: messages["settings.receipt.add"]! }));
    expect(value()).toEqual({
      base: { footer_keys: ["receipt.footer.thanks"] },
      channels: { SALES_CHANNEL_TAKEAWAY: {} },
    });
  });
});
