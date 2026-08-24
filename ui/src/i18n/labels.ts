import type { MessageKey } from "./index";

// The message key for a table state's label — shared so the floor, order and Today screens name a
// state the same way, and so the wire enum is never shown to a guest-facing operator raw.
export function tableStateKey(state: string): MessageKey {
  switch (state) {
    case "TABLE_STATE_OCCUPIED":
      return "table.occupied";
    case "TABLE_STATE_AWAITING_PAYMENT":
      return "table.awaiting";
    case "TABLE_STATE_NEEDS_CLEANING":
      return "table.cleaning";
    default:
      return "table.free";
  }
}
