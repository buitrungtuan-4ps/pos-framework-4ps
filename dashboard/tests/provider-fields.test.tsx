// The connection form is the provider's own schema, drawn (ADR-0153): each of the six kinds gets
// the input it implies, a value leaves as the JSON type the cloud checks it as, and a credential is
// write-only — the input never holds a stored value, says when one is kept, and offers removal only
// where the schema does not require one.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { ProviderField, SettingValue } from "../src/api/types";
import { ProviderFields } from "../src/components/ProviderFields";

afterEach(cleanup);

/**
 * The input labelled `name`. A kit field's `<label>` also holds its hint, so its text is the label
 * *then* the hint; matching the start is matching the label.
 */
function field(name: string): HTMLInputElement {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return screen.getByLabelText(new RegExp(`^${escaped}`)) as HTMLInputElement;
}

const FIELDS: readonly ProviderField[] = [
  { key: "base_url", label_key: "provider.field.base_url", kind: "SETTING_KIND_URL", required: true },
  {
    key: "timeout_seconds",
    label_key: "provider.field.timeout_seconds",
    kind: "SETTING_KIND_NUMBER",
    required: false,
    min: 1,
    max: 120,
  },
  {
    key: "environment",
    label_key: "provider.field.environment",
    kind: "SETTING_KIND_CHOICE",
    required: false,
    options: ["test", "live"],
  },
  { key: "auto_confirm", label_key: "provider.field.auto_confirm", kind: "SETTING_KIND_FLAG", required: false },
  { key: "api_key", label_key: "provider.field.api_key", kind: "SETTING_KIND_SECRET", required: true, max_length: 64 },
  { key: "webhook_secret", label_key: "provider.field.webhook_secret", kind: "SETTING_KIND_SECRET", required: false },
];

function renderFields(options: { secretsSet?: readonly string[] } = {}) {
  const values: Record<string, SettingValue | undefined> = {};
  const secrets: Record<string, string> = {};
  const cleared: string[] = [];
  render(() => (
    <ProviderFields
      fields={FIELDS}
      values={{}}
      onValue={(key, value) => {
        values[key] = value;
      }}
      secrets={{}}
      onSecret={(key, value) => {
        secrets[key] = value;
      }}
      secretsSet={options.secretsSet ?? []}
      clearing={[]}
      onClear={(key, clear) => {
        if (clear) cleared.push(key);
      }}
    />
  ));
  return { values, secrets, cleared };
}

describe("a provider's settings form", () => {
  it("draws each kind as its input, labelled from the catalogue or by the key it does not know", () => {
    renderFields();
    // A label this console ships is translated; one it does not falls back to the field's key.
    const url = field("API address (required)");
    expect(url.type).toBe("url");
    expect((field("Request timeout (seconds)")).type).toBe(
      "number",
    );
    expect(field("environment").tagName).toBe("SELECT");
    expect((field("auto_confirm")).type).toBe("checkbox");
    const key = field("api_key (required)");
    expect(key.type).toBe("password");
    expect(key.autocomplete).toBe("new-password");
  });

  it("hands each value back as the JSON type the cloud checks it as", () => {
    const { values, secrets } = renderFields();
    fireEvent.input(field("API address (required)"), {
      target: { value: "https://gateway.example" },
    });
    fireEvent.input(field("Request timeout (seconds)"), {
      target: { value: "30" },
    });
    fireEvent.change(field("environment"), { target: { value: "live" } });
    fireEvent.click(field("auto_confirm"));
    fireEvent.input(field("api_key (required)"), { target: { value: "k-123" } });

    expect(values).toEqual({
      base_url: "https://gateway.example",
      timeout_seconds: 30,
      environment: "live",
      auto_confirm: true,
    });
    // The secret never joins the settings: it travels in its own map, to be sealed.
    expect(secrets).toEqual({ api_key: "k-123" });

    // Emptying a field removes the setting rather than sending an empty string.
    fireEvent.input(field("API address (required)"), { target: { value: "  " } });
    expect(values.base_url).toBeUndefined();
  });

  it("never shows a stored credential, says one is kept, and offers removal only where it is optional", () => {
    const { cleared } = renderFields({ secretsSet: ["api_key", "webhook_secret"] });
    const key = field("api_key (required)");
    expect(key.value).toBe("");
    expect(screen.getAllByText(/A value is stored and hidden/)).toHaveLength(2);

    // One removal control, for the optional secret: a required one can be replaced, not removed.
    const removals = screen.getAllByLabelText("Remove the stored value");
    expect(removals).toHaveLength(1);
    fireEvent.click(removals[0] as HTMLElement);
    expect(cleared).toEqual(["webhook_secret"]);
  });
});
