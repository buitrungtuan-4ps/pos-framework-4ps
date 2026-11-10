// Comparing two versions on the Packages screen before pinning or rolling back (ADR-0189): the
// newest against the one before it by default, part by part, and "the same" when nothing differs.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { PackageVersionRecord } from "../src/api/schemas";
import en from "../src/i18n/en.json";
import { CompareCard } from "../src/screens/packages/versions/CompareCard";

const messages = en as Record<string, string>;

afterEach(cleanup);

function version(id: string, label: string, flows: PackageVersionRecord["content"]["flows"]): PackageVersionRecord {
  return {
    content: {
      package_id: "01J00000000000000000000P01",
      package_version_id: id,
      version_label: label,
      min_edge_version: "0.14.1",
      time_slots: [],
      flows,
    },
    create_time: "2026-11-10T00:00:00Z",
  } as PackageVersionRecord;
}

const MARCH = version("01J00000000000000000000V01", "March", []);
const APRIL = version("01J00000000000000000000V02", "April", [
  { body: { flow_id: "seat", start_action_id: "sales.order.open", steps: [] } },
]);

describe("comparing versions", () => {
  it("compares the newest with the one before it, and the other way round on request", () => {
    render(() => <CompareCard versions={[MARCH, APRIL]} />);
    expect(screen.getByText("Flows: seat is added")).toBeTruthy();
    fireEvent.change(screen.getByLabelText(messages["packageDiff.from"]!), { target: { value: APRIL.content.package_version_id } });
    fireEvent.change(screen.getByLabelText(messages["packageDiff.to"]!), { target: { value: MARCH.content.package_version_id } });
    expect(screen.getByText("Flows: seat is removed")).toBeTruthy();
  });

  it("says so when two versions run the same thing", () => {
    render(() => <CompareCard versions={[MARCH, version("01J00000000000000000000V03", "May", [])]} />);
    expect(screen.getByText(messages["packageDiff.same"]!)).toBeTruthy();
  });
});
