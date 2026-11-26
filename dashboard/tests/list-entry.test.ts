// A list entry can be removed from a draft, or retired once a version declares it (O9).

import { describe, expect, it } from "vitest";

import type { CustomList } from "../src/api/schemas";
import { withEntry } from "../src/screens/packages/data/DataPanel";

const list = {
  custom_list_id: "01KLIST",
  label_key: "list.reason",
  entries: [
    { list_entry_id: "01KA", label_key: "list.reason.late" },
    { list_entry_id: "01KB", label_key: "list.reason.cold" },
  ],
} as unknown as CustomList;

describe("a list entry in the data designer", () => {
  it("is removed, and the others kept in order", () => {
    expect(withEntry(list, list.entries[0]!, null).entries.map((entry) => entry.list_entry_id)).toEqual(["01KB"]);
  });
  it("is retired in place, kept for reading", () => {
    const retired = withEntry(list, list.entries[1]!, { ...list.entries[1]!, deprecated: true });
    expect(retired.entries).toHaveLength(2);
    expect(retired.entries[1]?.deprecated).toBe(true);
  });
});
