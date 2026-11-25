// The data designer (ADR-0194, ADR-0206): a field is added to an existing object, a list is built
// for a choice field, and an object type gets fields of its own. The personal question has no
// default: a field cannot be published until it is answered, and a personal one until its
// retention and consent purpose are given. Every id it mints is a ULID, as `pos-proto` types them,
// and the name a field, list, entry or object type was added under becomes its label key.

import { cleanup, fireEvent, render, screen, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import en from "../src/i18n/en.json";
import { blankDraft } from "../src/lib/package-draft";
import { ULID_PATTERN } from "../src/lib/ulid";
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
    const added = draft().data?.custom_fields?.[0];
    expect(added).toMatchObject({ label_key: "field.allergy_note", object: "DATA_OBJECT_ORDER" });
    expect(added?.custom_field_id).toMatch(ULID_PATTERN);
    const field = document.querySelector<HTMLElement>(`[data-custom-field="${added?.custom_field_id}"]`)!;
    expect(within(field).getByText("field.allergy_note")).toBeTruthy();
    expect(within(field).getByText(m("data.problem.personalUnanswered"))).toBeTruthy();
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
    const list = draft().data?.lists?.[0];
    expect(list?.custom_list_id).toMatch(ULID_PATTERN);
    const typeId = draft().data?.object_types?.[0]?.object_type_id;
    expect(typeId).toMatch(ULID_PATTERN);
    const type = document.querySelector<HTMLElement>(`[data-object-type="${typeId}"]`)!;
    expect(within(type).getByText("object.tasting")).toBeTruthy();
    fireEvent.input(within(type).getByLabelText(m("data.newField")), { target: { value: "Spice level" } });
    fireEvent.click(within(type).getByRole("button", { name: m("data.addField") }));
    fireEvent.change(within(type).getByLabelText(m("data.fieldType")), { target: { value: "CUSTOM_FIELD_TYPE_CHOICE" } });
    // The list is offered by its label key, and chosen by its id.
    const picker = within(type).getByLabelText(m("data.list"));
    expect(within(picker).getByRole("option", { name: "list.spice" })).toBeTruthy();
    fireEvent.change(picker, { target: { value: list?.custom_list_id } });

    const entries = draft().data?.lists?.[0]?.entries ?? [];
    expect(draft().data?.lists?.[0]).toMatchObject({ custom_list_id: list?.custom_list_id, label_key: "list.spice" });
    expect(entries).toHaveLength(1);
    expect(entries[0]?.label_key).toBe("list.spice.mild");
    expect(entries[0]?.list_entry_id).toMatch(ULID_PATTERN);
    const field = draft().data?.object_types?.[0]?.fields[0];
    expect(field).toMatchObject({ label_key: "field.spice_level", field_type: "CUSTOM_FIELD_TYPE_CHOICE", custom_list_id: list?.custom_list_id });
    expect(field?.custom_field_id).toMatch(ULID_PATTERN);
    expect(field?.object).toBeUndefined();
  });
});
