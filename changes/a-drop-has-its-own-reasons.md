---
kind: added
upgrade_note: `ReasonAction` gains `REASON_ACTION_CASH_DROPPED` and `cash.drawer.dropped` gains an optional `reason_code_id` (both additive; `PROTOCOL_VERSION` stays 1). The framework's default reason list gains `SAFE_DROP` for a drop and `PURCHASE` and `STAFF_ADVANCE` for a paid out; a store that publishes its own list keeps it, and adds these in the console if it wants them.
---
- **A drop to the safe can cite a reason of its own, and the default list covers spending from
  the drawer** ([ADR-0186](../docs/adr/0186-a-drawer-is-counted-by-denomination.md) amendment).
  `POST /api/shifts/{id}/safe-drop` takes an optional `reason_code_id` from the store's reasons for
  a drop, never a paid-out reason; the console's reason codes can be tagged "Dropped to safe". A
  store that has published no list offers "Dropped to safe" (*Nộp két*) for a drop, and
  "Small purchase" (*Mua hàng*) and "Advance to staff" (*Tạm ứng*, naming no person) for a paid out.
