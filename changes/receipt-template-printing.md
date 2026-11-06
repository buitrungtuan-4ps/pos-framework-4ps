---
kind: added
---
- **Receipts print their template.** A receipt, its copy and a pre-bill print the template of
  their order's sales channel, or the base template: header and footer lines from the translation
  grid, the custom fields it lists, and without the modifiers or the second language where it
  leaves them out. A personal value prints only on the person's own copy, and a reprint prints it
  only for someone holding `data.personal.read` (ADR-0207).
