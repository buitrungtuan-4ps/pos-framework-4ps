// Which acts paint first is no longer a list (ADR-0196 decision 1, which retired ADR-0173 decision
// 4's allow-list): every act the catalogue marks `ACTION_EFFECT_EDGE_WRITE` takes the one path in
// `state/edge-write.ts`. This reads the till's source and fails when an edge write is neither sent
// through that path nor exempt here with its reason, so a new catalogue act cannot slip past it.

import { describe, expect, it } from "vitest";

import { ACTIONS } from "../../packages/package-renderer/src/actions";

/** Edge writes the till does not send through `editWrite`, and why. */
const EXEMPT: Readonly<Record<string, string>> = {
  "sales.line.fire":
    "the till fires by the order or by the course (`sales.order.fire`, `sales.course.fire`), never one line",
  "admin.pairing_code.create": "store administration on Devices: its result, a new code, is the edge's to give",
  "admin.device.revoke": "store administration on Devices: the device list is read again from the edge",
  "admin.print_agent.claim": "store administration on Devices: which terminal is held is the edge's to say",
  "admin.print_agent.revoke": "store administration on Devices: which terminal is held is the edge's to say",
  "admin.printer.test": "store administration on Devices: the test page's outcome is the printer's to say",
  "data.custom_field.set": "no till screen sends it yet: fields on an order or a bill come with their own panel (ADR-0206)",
  "data.custom_record.create":
    "the records screen waits for the edge's answer: a personal value is never drawn before the edge has kept it (ADR-0206)",
  "data.custom_record.change":
    "the records screen waits for the edge's answer: a personal value is never drawn before the edge has kept it (ADR-0206)",
};

// The till's own source as text, the generated API types left out.
const SOURCES: Readonly<Record<string, string>> = import.meta.glob(
  ["../src/**/*.{ts,tsx}", "!../src/api/generated/**"],
  { query: "?raw", import: "default", eager: true },
);

/** Every catalogue id named in the first argument of a call to the edge-write path. */
function routed(): Set<string> {
  const ids = new Set<string>();
  const call = /\b(?:editWrite|printWrite|\.run)(?:<[^>()]*>)?\(\s*([^,]+),/g;
  for (const text of Object.values(SOURCES)) {
    for (const match of text.matchAll(call)) {
      for (const id of (match[1] ?? "").matchAll(/"([a-z_]+\.[a-z_]+\.[a-z_]+)"/g)) {
        ids.add(id[1] ?? "");
      }
    }
  }
  return ids;
}

const EDGE_WRITES = ACTIONS.filter((action) => action.effect === "ACTION_EFFECT_EDGE_WRITE").map(
  (action) => action.action_id as string,
);

describe("every edge write paints first", () => {
  it("is sent through the edge-write path, or is exempt with its reason", () => {
    expect(Object.keys(SOURCES).length).toBeGreaterThan(50);
    const through = routed();
    const loose = EDGE_WRITES.filter((id) => !through.has(id) && EXEMPT[id] === undefined);
    expect(loose).toEqual([]);
  });

  it("exempts only edge writes the till does not send that way", () => {
    const through = routed();
    for (const id of Object.keys(EXEMPT)) {
      expect(EDGE_WRITES, id).toContain(id);
      expect(through.has(id), id).toBe(false);
    }
  });
});
