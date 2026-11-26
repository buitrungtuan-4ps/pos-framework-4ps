---
kind: added
---
- **Settings history in words, and a rollback scoped by what and where** (ADR-0246). A new console
  screen, **Settings history**, says who changed which setting, from what to what, and where it
  applied. **Undo this change** puts one setting back; **Roll back to before this** puts back one
  setting, one group or every setting at that place only, leaving other places as they are. Each
  value goes back as an ordinary, audited setting write checked against today's rules, so a later
  publish keeps it, and a value the rules now refuse is refused, naming the setting.
