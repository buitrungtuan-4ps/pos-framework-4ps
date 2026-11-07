---
kind: added
---
- **A custom record prints its slip.** The till's Records screen has *Print slip*, which prints a
  record's slip at the till's receipt printer: the store's receipt template's lines and fields,
  the record's type, its reference and the time (`POST /api/custom-records/{id}/slip`, act
  `data.custom_record.print`, under `data.record.write`). A personal value prints only where the
  template marks the slip as the person's copy, on the first slip after the save by the person who
  saved it, or on a reprint by someone holding `data.personal.read` (ADR-0207, ADR-0206).
