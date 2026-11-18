---
kind: added
upgrade_note: A new edge route, `POST /api/bills/{id}/comp`, and catalogue act, `billing.line.comp`, now produce `billing.comp.applied`, an event defined since the schema was written that nothing emitted before; a cloud reader already knows it. A new refusal, `ALREADY_COMPED`. No default, permission or setting changes: `billing.comp.apply` stays PIN-flagged and manager-only by default.
---
- **A line can be comped.** `POST /api/bills/{id}/comp` gives one line of an open bill away,
  whole, for a reason the store publishes for a comp and with a manager's approval where the
  person acting needs one (ADR-0219). The edge takes the line's whole total off, the tax follows
  the lower base, the receipt prints the comp, and the approval is recorded with it. A line is
  comped at most once. The till's button for it is a later change.
