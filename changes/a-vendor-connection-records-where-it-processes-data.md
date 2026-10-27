---
kind: added
---
- **A vendor connection records where its vendor processes data.** A connection takes an optional
  `processing_country`, an ISO 3166-1 alpha-2 code stored upper-case, and its reads answer it with
  `processing_outside_cell`, true when it is known and differs from the cell's home country
  (ADR-0179 C7). A difference is a warning and never a refusal; a malformed code is refused naming
  the field. Connections stored before this read as not recorded. The backup destination's country,
  the other half of C7, moved to C8, beside `deploy/cells/<cell>.env`.
