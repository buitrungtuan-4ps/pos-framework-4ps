---
kind: added
---
- **A cell issues activation codes with its prefix, and says where another cell's prefix points.**
  A cloud with a `[cell]` section returns every new code from `POST /admin/activation-codes` with its
  cell id in front, `VN1-XXXX-XXXX-XXXX` (ADR-0181); a single cell still issues `XXXX-XXXX-XXXX`.
  Only the twelve symbols are stored and redeemed, so a code redeems with or without its prefix.
  When `/activate` refuses a code whose prefix names another cell, the `403` adds a `code` detail,
  `OTHER_CELL` with the signed directory's base URL for that cell in the message, or
  `UNKNOWN_CELL`; every other refusal is unchanged. The store's `/setup` field no longer folds or
  cuts what is typed, so a prefix such as `LON1-` or `JP-EAST-2-` survives, and the store server
  presents the code to its configured cloud as typed.
