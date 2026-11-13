---
kind: added
upgrade_note: Migration 0101 adds nullable `store_id` and `retain_until` to the cloud's `subjects` table. The retention cron now always runs; without `retention_days` it masks only records kept with their own expiry. New store-facing route GET /sync/stores/{store_id}/subjects/{subject_id} (scope relay_orders). The demo delivery format gains an optional `deliver_to`, and `connector_router` takes a recipient store.
---
- **A delivery order's recipient is kept apart as a personal record.** A vendor's name, phone and
  address for the person go to the cloud's subject store under the store's own retention, never
  into the order, an event or a log; the order carries only the subject id, and only that store
  reads the record back (ADR-0215).
