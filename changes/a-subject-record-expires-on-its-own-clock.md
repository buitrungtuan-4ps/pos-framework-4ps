---
kind: added
upgrade_note: Store-sqlite migration 0016 adds a nullable `retain_until` column and an index to the edge's `subjects` table. It is additive; every existing row reads back without an expiry and is swept by the store's default retention as before.
---
- **A personal record on a store can expire on its own clock.** A subject record may carry
  `retain_until`, and the store's retention sweep masks it then, whatever the store's default
  retention says; a record without one, such as a B2B buyer's, is swept as before (ADR-0206
  decision 1). Nothing writes one yet: it is what a personal custom value will be written with.
