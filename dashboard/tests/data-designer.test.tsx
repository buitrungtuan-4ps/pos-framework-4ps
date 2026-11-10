// The data designer (ADR-0194, ADR-0206): a field is added to an existing object, a list is built
// for a choice field, and an object type gets fields of its own. The personal question has no
// default: a field cannot be published until it is answered, and a personal one until its
// retention and consent purpose are given.

import { cleanup, fireEvent, render, screen, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import en from "../src/i18n/en.json";
import { blankDraft } from "../src/lib/package-draft";
import { DataPanel } from "../src/screens/packages/data/DataPanel";
import type { DraftChange } from "../src/screens/packages/designer/use-draft";

const m = (key: string): string => (en as Record<string, string>)[key] ?? key;
const starts = (key: string) => new RegExp(`^${m(key).replace(/[()?]/gu, "\\$&")}`);

afterEach(cleanup);

function mount() {
  const [draft, setDraft] = createSignal(blankDraft("March"));
  render(() => <DataPanel draft={draft} change={(next: DraftChange) => setDraft(next(draft()))} refusals={[]} />);
  return draft;
}

describe("the data designer", () => {
  it("asks whether a field is personal, and asks a personal one for its retention and consent", () => {
    const draft = mount();
    fireEvent.input(screen.getByLabelText(m("data.newField")), { target: { value: "Allergy note" } });
    fireEvent.click(screen.getAllByRole("button", { name: m("data.addField") })[0]!);
    const field = document.querySelector<HTMLElement>('[data-custom-field="allergy_note"]')!;
    expect(within(field).getByText(m("data.problem.personalUnanswered"))).toBeTruthy();
    expect(draft().data?.custom_fields?.[0]).toMatchObject({ custom_field_id: "allergy_note", object: "DATA_OBJECT_ORDER" });
    expect(draft().data?.custom_fields?.[0]?.personal).toBeUndefined();

    fireEvent.change(within(field).getByLabelText(starts("data.personal")), { target: { value: "yes" } });
    expect(within(field).getByText(m("data.problem.retention"))).toBeTruthy();
    fireEvent.input(within(field).getByLabelText(starts("data.retentionDays")), { target: { value: "30" } });
    fireEvent.input(within(field).getByLabelText(starts("data.consentPurposeKey")), { target: { value: "consent.allergy" } });
    expect(within(field).queryByText(m("data.problem.consent"))).toBeNull();
    expect(draft().data?.custom_fields?.[0]).toMatchObject({ personal: true, retention_days: 30, consent_purpose_key: "consent.allergy" });

    // Answering no drops what only a personal field keeps.
    fireEvent.change(within(field).getByLabelText(starts("data.personal")), { target: { value: "no" } });
    expect(draft().data?.custom_fields?.[0]?.retention_days).toBeUndefined();
    expect(draft().data?.custom_fields?.[0]?.consent_purpose_key).toBeUndefined();
  });

  it("builds a list and an object type whose choice field draws from it", () => {
    const draft = mount();
    fireEvent.input(screen.getByLabelText(m("data.newList")), { target: { value: "Spice" } });
    fireEvent.click(screen.getByRole("button", { name: m("data.addList") }));
    fireEvent.input(screen.getByLabelText(m("data.newEntry")), { target: { value: "Mild" } });
    fireEvent.click(screen.getByRole("button", { name: m("data.addEntry") }));
    fireEvent.input(screen.getByLabelText(m("data.newObjectType")), { target: { value: "Tasting" } });
    fireEvent.click(screen.getByRole("button", { name: m("data.addObjectType") }));
    const type = document.querySelector<HTMLElement>('[data-object-type="tasting"]')!;
    fireEvent.input(within(type).getByLabelText(m("data.newField")), { target: { value: "Spice level" } });
    fireEvent.click(within(type).getByRole("button", { name: m("data.addField") }));
    fireEvent.change(within(type).getByLabelText(m("data.fieldType")), { target: { value: "CUSTOM_FIELD_TYPE_CHOICE" } });
    fireEvent.change(within(type).getByLabelText(m("data.list")), { target: { value: "spice" } });

    expect(draft().data?.lists?.[0]).toEqual({
      custom_list_id: "spice",
      label_key: "list.spice",
      entries: [{ list_entry_id: "mild", label_key: "list.spice.mild" }],
    });
    const field = draft().data?.object_types?.[0]?.fields[0];
    expect(field).toMatchObject({ custom_field_id: "spice_level", field_type: "CUSTOM_FIELD_TYPE_CHOICE", custom_list_id: "spice" });
    expect(field?.object).toBeUndefined();
  });
});
