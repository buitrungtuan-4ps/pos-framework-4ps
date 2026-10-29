---
kind: changed
---
- **Outcome and token fields in the console's answers are enumerated.** A fee, settings or
  permissions publish's `outcome`, a batch result's `outcome`, a fetched artifact's `outcome` and a
  device proposal's `status` are real enums in the code and list their tokens in
  `docs/openapi-admin.json`; a fee's `scope`, a sample bill's and a tax rate's `sales_channel`, a
  setting's `kind` and a fleet store's `edge_placement` list theirs too. The tokens are unchanged.
