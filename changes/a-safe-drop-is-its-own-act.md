---
kind: added
upgrade_note: New event `cash.drawer.dropped` (`schema_version` 1; `PROTOCOL_VERSION` stays 1) and new till route `POST /api/shifts/{id}/safe-drop`, permitted by the existing `cash.movement.record`. The shift response gains `dropped_amount` and the cloud's daily cash and each drawer's figures gain `dropped`, all left out while zero. An older cloud skips the event in its cash rollup.
---
- **A drop to the safe is its own act**
  ([ADR-0186](../docs/adr/0186-a-drawer-is-counted-by-denomination.md) decision 5). The Shift screen
  has **Drop to safe** (*Nộp két*) under paid in and paid out: type the amount and tap it, with no
  reason asked. It lowers what the close expects as a paid out does, and is refused on a counted
  drawer as a paid out is, but the shift report, the close response and the console's drawer table
  show it as **Dropped to safe**, apart from what was paid out, so it is no longer counted as an
  expense. The request may carry the notes dropped. A reason for a drop, from its own list,
  follows in a later change.
